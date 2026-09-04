// Web Worker: parses a folder full of `.milk` files into preset rows.
//
// A *second* worker, deliberately not worker.ts or libraryWorker.ts, for the
// same reason the library worker is separate from the decoder: a
// several-thousand-preset parse sweep must not delay ring refills or the music
// library's own scan. Nothing the preset library does may reach the speakers or
// the renderer.
//
// The parsing itself is `player_render::milk::Preset` via wasm, the same code
// the desktop shell calls directly, so the two shells cannot disagree about
// what a preset needs (M7c item 3).
import init, { parsePreset } from "./wasm/player_rt_wasm.js";
import { presetId, type StoredPreset } from "./presetStore";

/// How often to report progress. Posting per file would be thousands of
/// messages for a large collection.
const PROGRESS_EVERY = 25;

export interface PresetScanFile {
  /// Path inside the scanned folder, one segment per level.
  relativePath: string[];
  file: File;
}

export interface PresetScanRequest {
  type: "scan";
  folderId: string;
  files: PresetScanFile[];
}

export interface PresetProgressResponse {
  type: "progress";
  done: number;
  total: number;
}

export interface PresetDoneResponse {
  type: "done";
  presets: StoredPreset[];
  found: number;
  added: number;
  failed: number;
}

export interface PresetErrorResponse {
  type: "error";
  message: string;
}

type Response = PresetProgressResponse | PresetDoneResponse | PresetErrorResponse;

let wasmReady: Promise<unknown> | null = null;

self.onmessage = async (e: MessageEvent<PresetScanRequest>) => {
  if (e.data.type !== "scan") return;
  const { folderId, files } = e.data;

  try {
    wasmReady ??= init();
    await wasmReady;

    const presets: StoredPreset[] = [];
    let failed = 0;

    for (const [index, entry] of files.entries()) {
      try {
        presets.push(await readPreset(folderId, entry));
      } catch {
        // One unreadable file skips rather than aborting the scan.
        failed += 1;
      }
      if ((index + 1) % PROGRESS_EVERY === 0) {
        post({ type: "progress", done: index + 1, total: files.length });
      }
    }

    post({
      type: "done",
      presets,
      found: files.length,
      added: presets.length,
      failed,
    });
  } catch (err) {
    post({ type: "error", message: String(err) });
  }
};

/// Reads one file's text and parses it into a row. A file that is not a preset
/// is still a row -- listed as failing, with the reason, rather than hidden.
async function readPreset(folderId: string, entry: PresetScanFile): Promise<StoredPreset> {
  const text = await entry.file.text();
  const parsed = JSON.parse(parsePreset(text)) as {
    ok: boolean;
    report: string[];
    textures: string[];
    sprites: string[];
  };
  return {
    id: presetId(folderId, entry.relativePath),
    folderId,
    relativePath: entry.relativePath,
    fileName: entry.file.name,
    report: parsed.report,
    textures: parsed.textures,
    sprites: parsed.sprites,
  };
}

function post(response: Response): void {
  self.postMessage(response);
}
