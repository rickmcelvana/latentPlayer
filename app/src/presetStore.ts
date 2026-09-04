// Where the web build's preset library lives: IndexedDB (Phase 3 M7c item 3).
//
// The same shape as libraryStore.ts, for the same reason: a preset library is
// a folder of files plus the ability to reopen them next session, and a
// `FileSystemDirectoryHandle` is the only thing a browser can store that makes
// that possible. The rows and the handle both survive a reload.
//
// The database itself is libraryStore.ts's -- the preset library is a sibling
// of the music library, not a separate app, and one database means one upgrade
// path. This module only owns the two preset stores and their operations.

import { hashHex, promisify, withStore } from "./libraryStore";

/// One row of the preset library, as the web build stores it. Mirrors
/// `PresetRow` in crates/player-render/src/milk/library.rs field for field,
/// except for how the file is found again -- a path there, a handle and a
/// relative path here.
export interface StoredPreset {
  id: string;
  /// Which scanned folder this came from, and where inside it.
  folderId: string;
  relativePath: string[];
  fileName: string;
  /// What this preset asks for that this build does not do, and what went
  /// wrong compiling it -- one line each.
  report: string[];
  /// The texture-pack base names and sprite image paths the preset references,
  /// so selection can read those images beside the preset and hand them to
  /// `setTextures` (the item-2 gap this milestone closes).
  textures: string[];
  sprites: string[];
}

/// A folder the user added to the preset library.
export interface StoredPresetFolder {
  id: string;
  name: string;
  /// Present when the browser has the File System Access API, absent when the
  /// folder arrived through the `<input webkitdirectory>` fallback.
  handle: FileSystemDirectoryHandle | null;
}

const PRESETS = "presets";
const PRESET_FOLDERS = "presetFolders";

/// A preset row's id, derived the same way the native library derives it --
/// from the location, so a rescan updates rows rather than duplicating them.
export function presetId(folderId: string, relativePath: string[]): string {
  return hashHex(`${folderId}/${relativePath.join("/")}`);
}

export async function allPresets(): Promise<StoredPreset[]> {
  return withStore(PRESETS, "readonly", (tx) =>
    promisify(tx.objectStore(PRESETS).getAll() as IDBRequest<StoredPreset[]>),
  );
}

export async function allPresetFolders(): Promise<StoredPresetFolder[]> {
  return withStore(PRESET_FOLDERS, "readonly", (tx) =>
    promisify(tx.objectStore(PRESET_FOLDERS).getAll() as IDBRequest<StoredPresetFolder[]>),
  );
}

/// Replaces everything known about one folder: its record and its rows.
///
/// One transaction, so a scan either lands completely or not at all.
export async function replacePresetFolder(
  folder: StoredPresetFolder,
  presets: StoredPreset[],
): Promise<void> {
  await withStore([PRESET_FOLDERS, PRESETS], "readwrite", async (tx) => {
    tx.objectStore(PRESET_FOLDERS).put(folder);

    const index = tx.objectStore(PRESETS).index("folderId");
    const stale = await promisify(index.getAllKeys(folder.id) as IDBRequest<IDBValidKey[]>);
    for (const key of stale) tx.objectStore(PRESETS).delete(key);

    for (const preset of presets) tx.objectStore(PRESETS).put(preset);
  });
}

/// Forgets a folder and its rows.
export async function deletePresetFolder(folderId: string): Promise<void> {
  await withStore([PRESET_FOLDERS, PRESETS], "readwrite", async (tx) => {
    tx.objectStore(PRESET_FOLDERS).delete(folderId);
    const index = tx.objectStore(PRESETS).index("folderId");
    const keys = await promisify(index.getAllKeys(folderId) as IDBRequest<IDBValidKey[]>);
    for (const key of keys) tx.objectStore(PRESETS).delete(key);
  });
}

/// Empties the preset stores. Only the dev harness uses this.
export async function clearPresets(): Promise<void> {
  await withStore([PRESETS, PRESET_FOLDERS], "readwrite", (tx) => {
    tx.objectStore(PRESETS).clear();
    tx.objectStore(PRESET_FOLDERS).clear();
  });
}
