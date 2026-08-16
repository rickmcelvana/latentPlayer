// Web Worker: reads tags for a folder full of files.
//
// A *second* worker, deliberately not worker.ts (M7 decision 6). That one owns
// the decoder and keeps the playback ring topped up; a five-thousand-file tag
// sweep sharing it would delay ring refills, and a delayed ring refill is a
// dropout. This is M4 decision 2's rule -- analysis never runs on the audio
// thread -- one layer further out: nothing the library does may reach the
// speakers.
//
// The parsing itself is `player_core::tags` via wasm, the same code the
// desktop shell calls directly, so the two shells cannot disagree about what a
// file is called (M7 decision 1).
import init, { WasmTags } from "./wasm/player_rt_wasm.js";
import { hashHex, type StoredTrack } from "./libraryStore";

/// How often to report progress. Posting per file would be thousands of
/// messages for a large folder, and a progress bar cannot show more than a
/// screen's worth of steps anyway.
const PROGRESS_EVERY = 25;

export interface ScanFile {
  /// Path inside the scanned folder, one segment per level. Kept so the file
  /// can be found again through the folder's handle next session.
  relativePath: string[];
  file: File;
}

export interface ScanRequest {
  type: "scan";
  folderId: string;
  files: ScanFile[];
}

export interface ProgressResponse {
  type: "progress";
  done: number;
  total: number;
}

/// A cover, sent once per distinct image rather than once per track -- the
/// worker already knows which ones it has seen, so an album's twelve tracks
/// send one image between them instead of twelve copies through structured
/// clone.
export interface ScannedCover {
  id: string;
  bytes: Uint8Array;
  mediaType: string;
}

export interface DoneResponse {
  type: "done";
  tracks: StoredTrack[];
  covers: ScannedCover[];
  found: number;
  added: number;
  failed: number;
}

export interface ErrorResponse {
  type: "error";
  message: string;
}

type Response = ProgressResponse | DoneResponse | ErrorResponse;

let wasmReady: Promise<unknown> | null = null;

self.onmessage = async (e: MessageEvent<ScanRequest>) => {
  if (e.data.type !== "scan") return;
  const { folderId, files } = e.data;

  try {
    wasmReady ??= init();
    await wasmReady;

    const tracks: StoredTrack[] = [];
    const covers = new Map<string, ScannedCover>();
    let failed = 0;

    for (const [index, entry] of files.entries()) {
      try {
        tracks.push(await readTrack(folderId, entry, covers));
      } catch {
        // One unreadable file skips rather than aborting the scan: a folder of
        // ten thousand tracks will contain a truncated download eventually,
        // and losing the other 9,999 to it would be absurd. Counted, though,
        // so the UI can say so.
        failed += 1;
      }
      if ((index + 1) % PROGRESS_EVERY === 0) {
        post({ type: "progress", done: index + 1, total: files.length });
      }
    }

    post({
      type: "done",
      tracks,
      covers: [...covers.values()],
      found: files.length,
      added: tracks.length,
      failed,
    });
  } catch (err) {
    post({ type: "error", message: String(err) });
  }
};

/// Reads one file's tags into a row, recording its cover if it has one.
async function readTrack(
  folderId: string,
  entry: ScanFile,
  covers: Map<string, ScannedCover>,
): Promise<StoredTrack> {
  // The whole file, because that is what a browser offers: there is no
  // random-access read to hand a decoder, and tags are not reliably at the
  // front (MP4 can carry its `moov` box at the end, ID3v1 lives in the last
  // 128 bytes). Read once, parsed, and released before the next file --
  // see ARCHITECTURE.md's known gaps for the cost this carries.
  const bytes = new Uint8Array(await entry.file.arrayBuffer());
  const extension = entry.file.name.split(".").pop()?.toLowerCase();
  const tags = WasmTags.read(bytes, extension);

  try {
    let coverId: string | null = null;
    const coverData = tags.coverData;
    if (coverData && coverData.length > 0) {
      coverId = hashHex(coverData);
      if (!covers.has(coverId)) {
        covers.set(coverId, {
          id: coverId,
          bytes: coverData,
          mediaType: tags.coverMediaType ?? "application/octet-stream",
        });
      }
    }

    return {
      id: hashHex(`${folderId}/${entry.relativePath.join("/")}`),
      folderId,
      relativePath: entry.relativePath,
      fileName: entry.file.name,
      title: tags.title ?? null,
      artist: tags.artist ?? null,
      album: tags.album ?? null,
      albumArtist: tags.albumArtist ?? null,
      genre: tags.genre ?? null,
      trackNumber: tags.trackNumber ?? null,
      discNumber: tags.discNumber ?? null,
      year: tags.year ?? null,
      durationSeconds: tags.durationSeconds ?? null,
      sampleRate: tags.sampleRate ?? null,
      channels: tags.channels ?? null,
      coverId,
    };
  } finally {
    // wasm-bindgen objects are not garbage collected on the Rust side; a scan
    // of several thousand files that forgot this would hold every cover it had
    // ever read until the worker was torn down.
    tags.free();
  }
}

function post(response: Response): void {
  self.postMessage(response);
}
