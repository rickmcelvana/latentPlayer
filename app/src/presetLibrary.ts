// Adapter for the *preset library* (Phase 3 M7c item 3) -- the shell-choosing
// module beside library.ts (music), playlist.ts (playback), analysis.ts
// (metering) and visualizer.ts (the host).
//
// The two shells host a preset collection very differently and cannot be made
// to agree on that: natively it is a folder Rust walks into a JSON index, on
// the web it is a `FileSystemDirectoryHandle` and an IndexedDB store. What they
// *can* agree on is everything above that -- what a row looks like, what its
// report means, and what selecting one does. This module is where the
// difference stops.
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { isTauri } from "./isTauri";
import { setMilkPreset as setMilkPresetNative } from "./nativePlayer";
import { setPresetWeb, setTexturesWeb } from "./webVisualizer";
import {
  allPresetFolders,
  allPresets,
  deletePresetFolder,
  replacePresetFolder,
  type StoredPreset,
} from "./presetStore";
import type { PresetDoneResponse, PresetProgressResponse, PresetScanFile } from "./presetWorker";
import { hashHex } from "./libraryStore";

/// One row, in the shape components render. The two stores fill this in from
/// very different places; nothing above this line knows which.
export interface PresetRow {
  id: string;
  fileName: string;
  /// What this preset asks for that this build does not do, and what went
  /// wrong compiling it -- one line each. Empty is a preset with nothing to
  /// report, not a failure.
  report: string[];
  /// Native only: the path selection reads.
  path: string | null;
  /// Web only: where the file is inside its folder, so selection reopens it.
  folderId: string | null;
  relativePath: string[];
}

export interface PresetFolder {
  id: string;
  name: string;
  /// False when the folder's files cannot be reopened without picking it
  /// again -- the `<input webkitdirectory>` fallback. Always true natively.
  available: boolean;
}

export interface PresetScanSummary {
  found: number;
  added: number;
  failed: number;
}

/// Every row. Read whole rather than paged, for the music library's reason: a
/// row is a couple of hundred bytes, so even a large collection is a few
/// megabytes read once.
export async function presetRows(): Promise<PresetRow[]> {
  if (isTauri()) {
    const rows = await invoke<NativePresetRow[]>("preset_rows");
    return rows.map((row) => ({
      id: row.id,
      fileName: row.fileName,
      report: row.report,
      path: row.path,
      folderId: null,
      relativePath: [],
    }));
  }
  return (await allPresets()).map(fromStored);
}

/// The native row's shape, which carries a path and no folder handle.
interface NativePresetRow {
  id: string;
  path: string;
  fileName: string;
  report: string[];
}

export async function presetFolders(): Promise<PresetFolder[]> {
  if (isTauri()) {
    const paths = await invoke<string[]>("preset_folders");
    return paths.map((path) => ({
      id: path,
      name: path.split(/[\\/]/).filter(Boolean).pop() ?? path,
      available: true,
    }));
  }
  return (await allPresetFolders()).map((folder) => ({
    id: folder.id,
    name: folder.name,
    available: folder.handle !== null,
  }));
}

/// Asks the user for a folder and scans it.
///
/// Must be called from a user gesture on the web, for the music library's
/// reason: `showDirectoryPicker` and the `<input webkitdirectory>` fallback
/// both require one.
export async function addPresetFolder(
  onProgress?: (done: number, total: number) => void,
): Promise<PresetScanSummary | null> {
  if (isTauri()) {
    const picked = await open({ directory: true, multiple: false, recursive: true });
    if (typeof picked !== "string") return null;
    return invoke<PresetScanSummary>("scan_preset_folder", { path: picked });
  }

  const picker = window.showDirectoryPicker;
  if (!picker) return scanViaInput(onProgress);

  let handle: FileSystemDirectoryHandle;
  try {
    handle = await picker({ id: "latentplayer-presets", mode: "read" });
  } catch {
    return null;
  }
  return scanHandle(handle, onProgress);
}

/// Forgets a folder and its rows. The files themselves are untouched.
export async function forgetPresetFolder(id: string): Promise<void> {
  if (isTauri()) return invoke("forget_preset_folder", { path: id });
  return deletePresetFolder(id);
}

/// Selects a preset: reads its text (and, on the web, its images) and hands it
/// to the visualizer. Answers with the report, so the caller can show it.
export async function openPreset(row: PresetRow): Promise<string[]> {
  if (isTauri()) {
    if (!row.path) throw new Error(`"${row.fileName}" has no path`);
    return setMilkPresetNative(row.path);
  }

  const stored = (await allPresets()).find((r) => r.id === row.id);
  if (!stored) throw new Error(`"${row.fileName}" is no longer in the library`);
  const folder = (await allPresetFolders()).find((f) => f.id === stored.folderId);
  if (!folder?.handle) {
    throw new Error(`the folder holding "${row.fileName}" needs to be added again`);
  }
  if (!(await ensurePermission(folder.handle))) {
    throw new Error(`permission to read "${folder.name}" was not granted`);
  }

  // Walk the folder handle down to the file, then read its text and the images
  // it references -- the item-2 gap this milestone closes: a browser's
  // single-file input could not resolve a texture-pack folder, and a scanned
  // folder's handle is exactly what can.
  let dir = folder.handle;
  for (const segment of stored.relativePath.slice(0, -1)) {
    dir = await dir.getDirectoryHandle(segment);
  }
  const last = stored.relativePath[stored.relativePath.length - 1];
  if (!last) throw new Error(`"${row.fileName}" has no path inside its folder`);
  const file = await (await dir.getFileHandle(last)).getFile();
  const text = await file.text();

  const textures: { name: string; bytes: Uint8Array }[] = [];
  for (const name of stored.textures) {
    const bytes = await readImage(folder.handle, name);
    if (bytes) textures.push({ name, bytes });
  }
  for (const name of stored.sprites) {
    const bytes = await readImage(folder.handle, name);
    if (bytes) textures.push({ name, bytes });
  }

  const report = setPresetWeb(stored.fileName, text);
  setTexturesWeb(textures);
  return report;
}

/// Reads an image file beside the preset, by texture-pack stem or sprite path.
///
/// The same walk the native shell's `find_image`/`find_sprite_image` do: a
/// `textures/` or `sprites/` subfolder, or a sibling of a `presets/` folder,
/// walking up to two ancestors. `name` is a stem (`clouds` finds
/// `textures/clouds.jpg`) or a path (`sprites/x1.png`).
async function readImage(
  folder: FileSystemDirectoryHandle,
  name: string,
): Promise<Uint8Array | null> {
  const exts = ["png", "jpg", "jpeg", "jfif"];
  const candidates: string[] = [];
  if (name.includes("/") || name.includes("\\")) {
    candidates.push(name.replace(/\\/g, "/"));
  } else {
    for (const dir of ["textures", "sprites"]) {
      for (const ext of exts) candidates.push(`${dir}/${name}.${ext}`);
    }
  }
  for (const candidate of candidates) {
    const bytes = await readByPath(folder, candidate);
    if (bytes) return bytes;
  }
  return null;
}

/// Reads a file at a slash-separated path inside a directory handle, or null.
async function readByPath(
  dir: FileSystemDirectoryHandle,
  path: string,
): Promise<Uint8Array | null> {
  const segments = path.split("/").filter(Boolean);
  let current = dir;
  for (const segment of segments.slice(0, -1)) {
    try {
      current = await current.getDirectoryHandle(segment);
    } catch {
      return null;
    }
  }
  const last = segments[segments.length - 1];
  if (!last) return null;
  try {
    const file = await (await current.getFileHandle(last)).getFile();
    return new Uint8Array(await file.arrayBuffer());
  } catch {
    return null;
  }
}

/// Re-grants read permission on a stored handle if it has lapsed.
async function ensurePermission(handle: FileSystemDirectoryHandle): Promise<boolean> {
  if ((await handle.queryPermission({ mode: "read" })) === "granted") return true;
  return (await handle.requestPermission({ mode: "read" })) === "granted";
}

/// Walks a directory handle, parses every `.milk` file in the preset worker,
/// and replaces the folder's rows with what it found.
async function scanHandle(
  handle: FileSystemDirectoryHandle,
  onProgress?: (done: number, total: number) => void,
): Promise<PresetScanSummary> {
  if (!(await ensurePermission(handle))) {
    throw new Error(`permission to read "${handle.name}" was not granted`);
  }

  const files: PresetScanFile[] = [];
  await collectFiles(handle, [], files);
  const folderId = hashHex(`presets:${handle.name}`);
  const summary = await runScan(folderId, files, onProgress);
  await replacePresetFolder({ id: folderId, name: handle.name, handle }, summary.presets);
  return { found: summary.found, added: summary.added, failed: summary.failed };
}

/// The fallback for browsers with no `showDirectoryPicker`.
async function scanViaInput(
  onProgress?: (done: number, total: number) => void,
): Promise<PresetScanSummary | null> {
  const input = document.createElement("input");
  input.type = "file";
  input.multiple = true;
  input.webkitdirectory = true;

  const picked = await new Promise<FileList | null>((resolve) => {
    input.onchange = () => resolve(input.files);
    input.oncancel = () => resolve(null);
    input.click();
  });
  if (!picked || picked.length === 0) return null;

  const files: PresetScanFile[] = [];
  let folderName = "Folder";
  for (const file of picked) {
    const parts = file.webkitRelativePath.split("/").filter(Boolean);
    if (parts.length > 1) folderName = parts[0] ?? folderName;
    const relativePath = parts.slice(1);
    if (relativePath.length === 0 || !file.name.toLowerCase().endsWith(".milk")) continue;
    files.push({ relativePath, file });
  }
  if (files.length === 0) return { found: 0, added: 0, failed: 0 };

  const folderId = hashHex(`presets:${folderName}`);
  const summary = await runScan(folderId, files, onProgress);
  await replacePresetFolder({ id: folderId, name: folderName, handle: null }, summary.presets);
  return { found: summary.found, added: summary.added, failed: summary.failed };
}

/// Runs one scan in the preset worker, off the main thread.
function runScan(
  folderId: string,
  files: PresetScanFile[],
  onProgress?: (done: number, total: number) => void,
): Promise<PresetDoneResponse> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./presetWorker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (
      e: MessageEvent<PresetDoneResponse | PresetProgressResponse | { type: "error"; message: string }>,
    ) => {
      if (e.data.type === "progress") {
        onProgress?.(e.data.done, e.data.total);
        return;
      }
      worker.terminate();
      if (e.data.type === "error") reject(new Error(e.data.message));
      else resolve(e.data);
    };
    worker.onerror = (event) => {
      worker.terminate();
      reject(new Error(event.message || "the preset worker failed"));
    };
    worker.postMessage({ type: "scan", folderId, files });
  });
}

/// Depth-first walk of a directory handle, collecting `.milk` files. Sorted at
/// each level so a rescan produces the same order twice running.
async function collectFiles(
  dir: FileSystemDirectoryHandle,
  prefix: string[],
  out: PresetScanFile[],
): Promise<void> {
  const files: FileSystemFileHandle[] = [];
  const dirs: FileSystemDirectoryHandle[] = [];
  for await (const entry of dir.values()) {
    if (entry.kind === "file") files.push(entry);
    else dirs.push(entry);
  }
  files.sort((a, b) => a.name.localeCompare(b.name));
  dirs.sort((a, b) => a.name.localeCompare(b.name));

  for (const handle of files) {
    if (!handle.name.toLowerCase().endsWith(".milk")) continue;
    out.push({ relativePath: [...prefix, handle.name], file: await handle.getFile() });
  }
  for (const child of dirs) {
    await collectFiles(child, [...prefix, child.name], out);
  }
}

function fromStored(row: StoredPreset): PresetRow {
  return {
    id: row.id,
    fileName: row.fileName,
    report: row.report,
    path: null,
    folderId: row.folderId,
    relativePath: row.relativePath,
  };
}

/// Where the selected preset is remembered between sessions.
///
/// localStorage, like the visual and the gain, and for the same reason: it is
/// one short string, not library data. The stored value is a path natively and
/// a `folderId/relativePath` on the web -- the two things `openPreset` needs to
/// reopen the file.
const SELECTION_KEY = "latentplayer.preset";

/// Remembers a selection, in whichever form this shell reopens it.
export function rememberPreset(row: PresetRow): void {
  try {
    if (isTauri()) {
      if (row.path) localStorage.setItem(SELECTION_KEY, row.path);
    } else if (row.folderId) {
      localStorage.setItem(SELECTION_KEY, `${row.folderId}/${row.relativePath.join("/")}`);
    }
  } catch {
    // Not remembering is a smaller problem than not selecting.
  }
}

/// Forgets the remembered selection.
export function forgetPreset(): void {
  try {
    localStorage.removeItem(SELECTION_KEY);
  } catch {
    // See above.
  }
}

/// Restores the selection from last session, if it still resolves.
///
/// Returns the report, or null when there is nothing to restore -- no stored
/// selection, a file that has moved, or (on the web) a folder whose handle or
/// permission has lapsed. A failed restore is silent rather than an error: the
/// honest state is "no preset selected", and the person can pick again.
export async function restorePreset(): Promise<string[] | null> {
  let stored: string | null = null;
  try {
    stored = localStorage.getItem(SELECTION_KEY);
  } catch {
    return null;
  }
  if (!stored) return null;

  if (isTauri()) {
    const rows = await presetRows();
    const row = rows.find((r) => r.path === stored);
    if (!row) {
      forgetPreset();
      return null;
    }
    return openPreset(row);
  }

  const [folderId, ...rest] = stored.split("/");
  const relativePath = rest;
  if (!folderId || relativePath.length === 0) {
    forgetPreset();
    return null;
  }
  const rows = await presetRows();
  const row = rows.find(
    (r) => r.folderId === folderId && JSON.stringify(r.relativePath) === JSON.stringify(relativePath),
  );
  if (!row) {
    forgetPreset();
    return null;
  }
  try {
    return await openPreset(row);
  } catch {
    // A folder whose permission has lapsed cannot be reopened without a user
    // gesture; the selection is left remembered so the next session can try
    // again once the folder is re-added.
    return null;
  }
}
