// Live check of the web library: tag reading through wasm, the IndexedDB
// store, the scan worker, and search (Phase 1 M7).
//
// The web counterpart of `cargo run -p player-core --example library_smoke`,
// running the same checks against the same claims so the two shells can be
// held to one standard -- the habit M4, M5 and M6 all follow. Served at
// /library.html by the dev server; not part of the production bundle.
//
// The files are synthesised here, in the same shape the native example builds
// them: an ID3v2.3 tag in front of a WAV. Symphonia's probe reads leading
// metadata before it looks for a container, so this exercises the real ID3v2
// reader without needing an MP3 to be synthesised.
//
// One thing this cannot reach: `showDirectoryPicker` itself, which needs a
// user gesture and a real folder on disk. Everything downstream of it -- the
// walk's output, the worker, the store, the rows -- is driven directly here,
// and the picker is what the live desktop/browser confirmation covers.
import { search, type LibraryTrack } from "./library";
import {
  allFolders,
  allPlaylists,
  allTracks,
  clearAll,
  coverBlob,
  deleteFolder,
  hashHex,
  putPlaylist,
  replaceFolder,
} from "./libraryStore";
import type { DoneResponse, ProgressResponse, ScanFile } from "./libraryWorker";

const out = document.getElementById("out") as HTMLPreElement;
let failures = 0;

function log(line: string): void {
  out.textContent += `${line}\n`;
}

function check(label: string, ok: boolean, detail: string): void {
  if (!ok) failures += 1;
  log(`  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}`);
}

/// A silent 16-bit stereo WAV. The audio is irrelevant -- what is scanned is
/// the container and its tags -- so it is as short as it can be.
function wav(frames: number): Uint8Array {
  const blockAlign = 4;
  const dataLen = frames * blockAlign;
  const buffer = new ArrayBuffer(44 + dataLen);
  const view = new DataView(buffer);
  const ascii = (offset: number, text: string) => {
    for (let i = 0; i < text.length; i++) view.setUint8(offset + i, text.charCodeAt(i));
  };
  ascii(0, "RIFF");
  view.setUint32(4, 36 + dataLen, true);
  ascii(8, "WAVEfmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 2, true);
  view.setUint32(24, 44_100, true);
  view.setUint32(28, 44_100 * blockAlign, true);
  view.setUint16(32, blockAlign, true);
  view.setUint16(34, 16, true);
  ascii(36, "data");
  view.setUint32(40, dataLen, true);
  return new Uint8Array(buffer);
}

function concat(parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

const ascii = (text: string) => new TextEncoder().encode(text);

/// One ID3v2.3 frame: four-character id, big-endian size, two flag bytes,
/// then the body.
function frame(id: string, body: Uint8Array): Uint8Array {
  const header = new Uint8Array(10);
  header.set(ascii(id), 0);
  new DataView(header.buffer).setUint32(4, body.length, false);
  return concat([header, body]);
}

/// A text frame's body: one encoding byte (0 = ISO-8859-1) then the text.
function textFrame(id: string, text: string): Uint8Array {
  return frame(id, concat([new Uint8Array([0]), ascii(text)]));
}

/// An APIC body: encoding, null-terminated media type, picture type,
/// null-terminated description, then the image.
function pictureFrame(mediaType: string, pictureType: number, image: Uint8Array): Uint8Array {
  return frame(
    "APIC",
    concat([
      new Uint8Array([0]),
      ascii(mediaType),
      new Uint8Array([0, pictureType, 0]),
      image,
    ]),
  );
}

/// Wraps frames in an ID3v2.3 tag. Sizes are syncsafe -- seven bits per byte.
function id3v2(frames: Uint8Array[]): Uint8Array {
  const body = concat(frames);
  const size = body.length;
  const header = new Uint8Array([
    ...ascii("ID3"),
    3,
    0,
    0,
    (size >> 21) & 0x7f,
    (size >> 14) & 0x7f,
    (size >> 7) & 0x7f,
    size & 0x7f,
  ]);
  return concat([header, body]);
}

function taggedFile(name: string, frames: Uint8Array[]): File {
  const bytes = concat([id3v2(frames), wav(4_410)]);
  return new File([bytes as BlobPart], name, { type: "audio/wav" });
}

/// Runs one scan through the real library worker -- the same code path
/// `library.ts` drives, off the main thread and off the decoder's worker.
function runWorker(folderId: string, files: ScanFile[]): Promise<DoneResponse> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./libraryWorker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (
      e: MessageEvent<DoneResponse | ProgressResponse | { type: "error"; message: string }>,
    ) => {
      if (e.data.type === "progress") return;
      worker.terminate();
      if (e.data.type === "error") reject(new Error(e.data.message));
      else resolve(e.data);
    };
    worker.onerror = (event) => {
      worker.terminate();
      reject(new Error(event.message || "the library worker failed"));
    };
    worker.postMessage({ type: "scan", folderId, files });
  });
}

async function run(): Promise<void> {
  out.textContent = "";
  failures = 0;
  // A previous run's library would make every count below meaningless.
  await clearAll();

  const cover = concat([new Uint8Array([0xff, 0xd8, 0xff, 0xe0]), new Uint8Array(512).fill(0x42)]);
  const back = new Uint8Array(32).fill(0xaa);

  // One album of three sharing a cover, a single by someone else, and an
  // untagged file -- the same four cases the native example builds.
  const files: ScanFile[] = [
    ["01 Opening.wav", "Opening", 1],
    ["02 Phosphor.wav", "Phosphor", 2],
    ["03 Decay.wav", "Decay", 3],
  ].map(([name, title, n]) => ({
    relativePath: ["Phosphor", name as string],
    file: taggedFile(name as string, [
      textFrame("TIT2", title as string),
      textFrame("TPE1", "The Vectorscopes"),
      textFrame("TALB", "Phosphor"),
      textFrame("TRCK", `${n}/3`),
      textFrame("TYER", "1998"),
      textFrame("TCON", "Electronic"),
      // A back cover first, so "the front cover wins" is actually tested
      // rather than being true by accident of ordering.
      pictureFrame("image/jpeg", 4, back),
      pictureFrame("image/jpeg", 3, cover),
    ]),
  }));
  files.push({
    relativePath: ["Singles", "lone.wav"],
    file: taggedFile("lone.wav", [
      textFrame("TIT2", "Lone Signal"),
      textFrame("TPE1", "Another Artist"),
    ]),
  });
  files.push({
    relativePath: ["Singles", "untagged.wav"],
    file: new File([wav(4_410) as BlobPart], "untagged.wav", { type: "audio/wav" }),
  });

  log(`library smoke -- scanning ${files.length} synthesised files`);

  const folderId = hashHex("handle:Music");
  const scanned = await runWorker(folderId, files);

  check(
    "the worker reads every file it is given",
    scanned.found === 5 && scanned.added === 5 && scanned.failed === 0,
    `found ${scanned.found}, added ${scanned.added}, failed ${scanned.failed}`,
  );

  const phosphor = scanned.tracks.filter((t) => t.album === "Phosphor");
  check(
    "tags come back off the real ID3v2 reader",
    phosphor.length === 3 &&
      phosphor.every(
        (t) => t.artist === "The Vectorscopes" && t.year === 1998 && t.genre === "Electronic",
      ),
    `${phosphor.length} tracks on the album, first is ${JSON.stringify(phosphor[0]?.title)} by ` +
      `${JSON.stringify(phosphor[0]?.artist)} (${phosphor[0]?.year})`,
  );

  const numbers = phosphor.map((t) => t.trackNumber);
  check(
    'track numbers survive the "7/12" spelling',
    JSON.stringify(numbers) === JSON.stringify([1, 2, 3]),
    JSON.stringify(numbers),
  );

  // The point of hashing the image rather than the track: an album's tracks
  // must name one stored blob between them, not one each.
  const coverIds = phosphor.map((t) => t.coverId).filter((id): id is string => id !== null);
  const distinct = new Set(coverIds);
  check(
    "one album's cover is stored once, not once per track",
    coverIds.length === 3 && distinct.size === 1 && scanned.covers.length === 1,
    `${coverIds.length} rows reference ${distinct.size} cover, ${scanned.covers.length} sent`,
  );

  check(
    "the front cover wins over the other picture in the file",
    scanned.covers[0]?.bytes.length === cover.length,
    `${scanned.covers[0]?.bytes.length} bytes stored, front is ${cover.length} and back is ${back.length}`,
  );

  const untagged = scanned.tracks.find((t) => t.fileName === "untagged.wav");
  check(
    "an untagged file is still a usable row",
    untagged?.title === null &&
      untagged?.sampleRate === 44_100 &&
      (untagged?.durationSeconds ?? 0) > 0,
    `title ${JSON.stringify(untagged?.title)}, ${untagged?.sampleRate}Hz, ` +
      `${untagged?.durationSeconds?.toFixed(3)}s -- the UI falls back to "${untagged?.fileName}"`,
  );

  // Into the store, which is where the web build's library actually lives.
  const covers = new Map(
    scanned.covers.map((c) => [c.id, new Blob([c.bytes as BlobPart], { type: c.mediaType })]),
  );
  await replaceFolder({ id: folderId, name: "Music", handle: null }, scanned.tracks, covers);

  const stored = await allTracks();
  check(
    "the rows persist in IndexedDB",
    stored.length === 5 && (await allFolders()).length === 1,
    `${stored.length} rows and ${(await allFolders()).length} folder(s) read back`,
  );

  const blob = await coverBlob(coverIds[0] ?? "");
  const storedCover = blob ? new Uint8Array(await blob.arrayBuffer()) : null;
  check(
    "the stored cover is the bytes that were embedded",
    storedCover?.length === cover.length && storedCover[0] === 0xff && storedCover[1] === 0xd8,
    storedCover
      ? `${storedCover.length} bytes stored, ${cover.length} embedded`
      : "no cover blob was stored",
  );

  // A rescan must be idempotent -- scan twice, do not get a library twice the
  // size. The same claim the native example makes.
  const rescanned = await runWorker(folderId, files);
  await replaceFolder({ id: folderId, name: "Music", handle: null }, rescanned.tracks, covers);
  const afterRescan = await allTracks();
  check(
    "rescanning updates rows rather than duplicating them",
    afterRescan.length === 5 &&
      JSON.stringify(afterRescan.map((t) => t.id).sort()) ===
        JSON.stringify(stored.map((t) => t.id).sort()),
    `${stored.length} rows before, ${afterRescan.length} after, same ids`,
  );

  // Search is one implementation for both shells (M7 decision 3): by this
  // point both hold the same array of rows, so the same function filters them.
  const rows: LibraryTrack[] = afterRescan.map((row) => ({ ...row, path: null }));
  const byArtist = search(rows, "vectorscopes");
  const byTwoWords = search(rows, "phosphor decay");
  check(
    "search matches across fields and requires every word",
    byArtist.length === 3 && byTwoWords.length === 1 && byTwoWords[0]?.title === "Decay",
    `"vectorscopes" -> ${byArtist.length}, "phosphor decay" -> ${byTwoWords.length} ` +
      `(${JSON.stringify(byTwoWords[0]?.title)})`,
  );

  await putPlaylist({ id: "p1", name: "Evening", trackIds: phosphor.map((t) => t.id) });
  const playlists = await allPlaylists();
  const resolves = playlists[0]?.trackIds.every((id) => rows.some((row) => row.id === id));
  check(
    "a saved playlist persists and its references still resolve",
    playlists.length === 1 && playlists[0]?.trackIds.length === 3 && resolves === true,
    playlists[0]
      ? `"${playlists[0].name}" with ${playlists[0].trackIds.length} tracks, all still in the library`
      : "no playlist was read back",
  );

  await deleteFolder(folderId);
  check(
    "forgetting a folder drops its rows",
    (await allTracks()).length === 0 && (await allFolders()).length === 0,
    `${(await allTracks()).length} rows left`,
  );

  log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => log(`ERROR: ${e}`));
});
