// The library, and the fourth of the shell-choosing adapter modules -- beside
// playlist.ts (playback), analysis.ts (metering) and scope.ts (the scope's
// geometry).
//
// M7 decision 3: the *scan* belongs to the shell and the *index* belongs to
// here. Natively a library is a folder that Rust walks; on the web it is a
// `FileSystemDirectoryHandle` and an IndexedDB store (libraryStore.ts). Those
// two have no common implementation and pretending otherwise would mean the
// desktop build could only do what a browser can. Everything above them --
// what a row looks like, how search works, what a playlist is -- is shared,
// and this is where the difference stops.
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { isTauri } from "./isTauri";
import type { PlaylistTrack } from "./playlist";
import { trackFromFile, trackFromPath } from "./playlist";
import {
  allFolders,
  allPlaylists,
  allTracks,
  coverBlob,
  deleteFolder,
  deletePlaylist as deletePlaylistWeb,
  hashHex,
  putPlaylist,
  replaceFolder,
  type StoredFolder,
  type StoredTrack,
} from "./libraryStore";
import type { DoneResponse, ProgressResponse, ScanFile } from "./libraryWorker";

/// Extensions a scan will try. Kept in step with `AUDIO_EXTENSIONS` in
/// crates/player-core/src/library.rs, so a folder scanned on the desktop and
/// the same folder scanned in a browser find the same files.
const AUDIO_EXTENSIONS = [
  "mp3",
  "flac",
  "wav",
  "wave",
  "aac",
  "m4a",
  "m4b",
  "mp4",
  "ogg",
  "oga",
  "aiff",
  "aif",
  "alac",
  "mka",
];

/// One row, in the shape components render. The two stores fill this in from
/// very different places; nothing above this line knows which.
export interface LibraryTrack {
  id: string;
  fileName: string;
  title: string | null;
  artist: string | null;
  album: string | null;
  albumArtist: string | null;
  genre: string | null;
  trackNumber: number | null;
  discNumber: number | null;
  year: number | null;
  durationSeconds: number | null;
  sampleRate: number | null;
  channels: number | null;
  coverId: string | null;
  /// Desktop only: the path `LocalFile` opens.
  path: string | null;
}

export interface LibraryFolder {
  id: string;
  name: string;
  /// False when the folder's files cannot be reopened without picking it
  /// again -- a web folder added through the `<input webkitdirectory>`
  /// fallback, whose rows persist but whose handle does not. Always true
  /// natively, where a path is all that is needed.
  available: boolean;
}

export interface LibraryPlaylist {
  id: string;
  name: string;
  trackIds: string[];
}

export interface ScanSummary {
  found: number;
  added: number;
  failed: number;
}

/// Every row. Read whole rather than paged: a row is a couple of hundred bytes,
/// so even a large library is a few megabytes read once, and search then runs
/// over the same array on both shells -- which is why search below is one
/// function rather than two implementations that could disagree.
export async function tracks(): Promise<LibraryTrack[]> {
  if (isTauri()) return invoke<LibraryTrack[]>("library_tracks");
  return (await allTracks()).map(fromStored);
}

export async function folders(): Promise<LibraryFolder[]> {
  if (isTauri()) {
    const paths = await invoke<string[]>("library_folders");
    return paths.map((path) => ({
      id: path,
      name: path.split(/[\\/]/).filter(Boolean).pop() ?? path,
      available: true,
    }));
  }
  return (await allFolders()).map((folder) => ({
    id: folder.id,
    name: folder.name,
    available: folder.handle !== null,
  }));
}

/// Asks the user for a folder and scans it.
///
/// Must be called from a user gesture on the web: both `showDirectoryPicker`
/// and the `<input webkitdirectory>` fallback require one, and so does
/// re-granting permission on a folder from a previous session.
export async function addFolder(onProgress?: (done: number, total: number) => void): Promise<
  ScanSummary | null
> {
  if (isTauri()) {
    const picked = await open({ directory: true, multiple: false, recursive: true });
    if (typeof picked !== "string") return null;
    // The scan itself runs on a blocking thread in Rust -- see `scan_folder`
    // in src-tauri/src/lib.rs for why it must not run on the main one.
    return invoke<ScanSummary>("scan_folder", { path: picked });
  }

  const picker = window.showDirectoryPicker;
  if (!picker) return scanViaInput(onProgress);

  let handle: FileSystemDirectoryHandle;
  try {
    handle = await picker({ id: "latentplayer-library", mode: "read" });
  } catch {
    // The user dismissed the picker. Not an error, and not something to show
    // them a message about -- they know they cancelled.
    return null;
  }
  return scanHandle(handle, onProgress);
}

/// Forgets a folder and its rows. The files themselves are untouched.
export async function forgetFolder(id: string): Promise<void> {
  if (isTauri()) return invoke("forget_folder", { path: id });
  return deleteFolder(id);
}

export async function playlists(): Promise<LibraryPlaylist[]> {
  if (isTauri()) return invoke<LibraryPlaylist[]>("playlists");
  return allPlaylists();
}

export async function savePlaylist(playlist: LibraryPlaylist): Promise<void> {
  if (isTauri()) return invoke("save_playlist", { playlist });
  return putPlaylist(playlist);
}

export async function deletePlaylist(id: string): Promise<void> {
  if (isTauri()) return invoke("delete_playlist", { id });
  return deletePlaylistWeb(id);
}

/// A URL for a row's cover art, or null when it has none.
///
/// Both shells hand back an ordinary URL an `<img src>` can take, which is the
/// whole point of M7 decision 5: art never crosses the command boundary as
/// JSON. Natively that is the `cover://` protocol src-tauri registers; on the
/// web it is an object URL over the stored blob.
/// Keyed on the *promise*, not the resolved URL, and that distinction is
/// load-bearing: an album's rows all render at once and all ask for the same
/// cover in the same tick, so a cache written only after the blob resolves is
/// still empty when the second and third callers look. Every one of them then
/// creates its own object URL -- a live reference to the blob that nothing
/// will ever revoke. Measured in-browser during M7: three rows sharing one
/// cover produced three distinct blob URLs.
const objectUrls = new Map<string, Promise<string | null>>();

export function coverUrl(coverId: string | null): Promise<string | null> {
  if (!coverId) return Promise.resolve(null);
  if (isTauri()) return Promise.resolve(convertFileSrc(coverId, "cover"));

  const existing = objectUrls.get(coverId);
  if (existing) return existing;

  const pending = coverBlob(coverId).then((blob) => (blob ? URL.createObjectURL(blob) : null));
  objectUrls.set(coverId, pending);
  // A lookup that failed should not be cached as a permanent "no cover" --
  // the next scan may well store the blob this one was too early for.
  void pending.catch(() => objectUrls.delete(coverId));
  return pending;
}

/// Filters rows by a free-text query across the fields a listener would search
/// by. One implementation for both shells, because by this point both shells
/// hold the same array -- the difference between them is the store underneath,
/// not the rows.
export function search(rows: LibraryTrack[], query: string): LibraryTrack[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return rows;
  // Every word must match something, so "miles blue" finds Miles Davis's Blue
  // in Green rather than everything by either name.
  const words = needle.split(/\s+/);
  return rows.filter((row) => {
    const haystack = [row.title, row.artist, row.album, row.albumArtist, row.genre, row.fileName]
      .filter((v): v is string => typeof v === "string")
      .join(" ")
      .toLowerCase();
    return words.every((word) => haystack.includes(word));
  });
}

/// Turns a library row into something playlist.ts can play.
///
/// The one place the two shells' notion of "a file" has to be reconciled: a
/// desktop row already carries the path `LocalFile` opens, while a web row
/// carries a folder handle and a relative path that have to be walked to reach
/// a `File`. Async for that reason, and it can fail -- a handle whose
/// permission has lapsed, or a file that has been moved since the scan.
export async function toPlaylistTrack(track: LibraryTrack): Promise<PlaylistTrack> {
  if (isTauri()) {
    if (!track.path) throw new Error(`"${track.fileName}" has no path`);
    const entry = trackFromPath(track.path);
    return { ...entry, name: displayName(track), libraryId: track.id };
  }

  const stored = (await allTracks()).find((row) => row.id === track.id);
  if (!stored) throw new Error(`"${track.fileName}" is no longer in the library`);
  const folder = (await allFolders()).find((f) => f.id === stored.folderId);
  if (!folder?.handle) {
    throw new Error(`the folder holding "${track.fileName}" needs to be added again`);
  }
  if (!(await ensurePermission(folder.handle))) {
    throw new Error(`permission to read "${folder.name}" was not granted`);
  }

  let dir = folder.handle;
  for (const segment of stored.relativePath.slice(0, -1)) {
    dir = await dir.getDirectoryHandle(segment);
  }
  const last = stored.relativePath[stored.relativePath.length - 1];
  if (!last) throw new Error(`"${track.fileName}" has no path inside its folder`);
  const file = await (await dir.getFileHandle(last)).getFile();
  const entry = trackFromFile(file);
  return { ...entry, name: displayName(track), libraryId: track.id };
}

/// What to show for a row: its title, or the file name when it has none. The
/// fallback is why `fileName` is stored even for a fully tagged file.
export function displayName(track: LibraryTrack): string {
  return track.title ?? track.fileName;
}

/// Re-grants read permission on a stored handle if it has lapsed.
///
/// A `FileSystemDirectoryHandle` survives in IndexedDB across sessions but its
/// permission grant does not, so a restored handle reports "prompt" and has to
/// ask again -- from a user gesture, which is why every caller of this is
/// reached by a click.
async function ensurePermission(handle: FileSystemDirectoryHandle): Promise<boolean> {
  if ((await handle.queryPermission({ mode: "read" })) === "granted") return true;
  return (await handle.requestPermission({ mode: "read" })) === "granted";
}

/// Walks a directory handle, reads every audio file's tags in the library
/// worker, and replaces the folder's rows with what it found.
async function scanHandle(
  handle: FileSystemDirectoryHandle,
  onProgress?: (done: number, total: number) => void,
): Promise<ScanSummary> {
  if (!(await ensurePermission(handle))) {
    throw new Error(`permission to read "${handle.name}" was not granted`);
  }

  const files: ScanFile[] = [];
  await collectFiles(handle, [], files);
  // Keyed by name rather than by the handle itself: two handles to the same
  // directory are different objects, so keying by identity would add the same
  // folder twice.
  const folderId = hashHex(`handle:${handle.name}`);
  const summary = await runScan(folderId, files, onProgress);
  await persist({ id: folderId, name: handle.name, handle }, summary);
  return summary.counts;
}

/// The fallback for browsers with no `showDirectoryPicker` (Firefox, Safari).
///
/// `<input webkitdirectory>` gives the files but no handle, so the rows persist
/// and the ability to reopen them does not -- the folder is marked unavailable
/// and the UI asks for it again rather than failing at the moment someone
/// presses play (M7 decision 4).
async function scanViaInput(
  onProgress?: (done: number, total: number) => void,
): Promise<ScanSummary | null> {
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

  const files: ScanFile[] = [];
  let folderName = "Folder";
  for (const file of picked) {
    // webkitRelativePath is "<folder>/<subdir>/<name>"; the first segment names
    // the folder that was picked and the rest is the path inside it.
    const parts = file.webkitRelativePath.split("/").filter(Boolean);
    if (parts.length > 1) folderName = parts[0] ?? folderName;
    const relativePath = parts.slice(1);
    if (relativePath.length === 0 || !isAudioFile(file.name)) continue;
    files.push({ relativePath, file });
  }
  if (files.length === 0) return { found: 0, added: 0, failed: 0 };

  const folderId = hashHex(`input:${folderName}`);
  const summary = await runScan(folderId, files, onProgress);
  await persist({ id: folderId, name: folderName, handle: null }, summary);
  return summary.counts;
}

interface ScanResult {
  counts: ScanSummary;
  done: DoneResponse;
}

/// Runs one scan in the library worker, off the main thread and off the
/// decoder's worker (M7 decision 6).
function runScan(
  folderId: string,
  files: ScanFile[],
  onProgress?: (done: number, total: number) => void,
): Promise<ScanResult> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./libraryWorker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (e: MessageEvent<DoneResponse | ProgressResponse | { type: "error"; message: string }>) => {
      if (e.data.type === "progress") {
        onProgress?.(e.data.done, e.data.total);
        return;
      }
      // A worker per scan rather than one kept alive: a scan is a rare,
      // bounded piece of work, and terminating it is also how a scan is
      // cancelled if that is ever wanted.
      worker.terminate();
      if (e.data.type === "error") {
        reject(new Error(e.data.message));
        return;
      }
      const done = e.data;
      resolve({
        counts: { found: done.found, added: done.added, failed: done.failed },
        done,
      });
    };
    worker.onerror = (event) => {
      worker.terminate();
      reject(new Error(event.message || "the library worker failed"));
    };
    worker.postMessage({ type: "scan", folderId, files });
  });
}

async function persist(folder: StoredFolder, result: ScanResult): Promise<void> {
  const covers = new Map<string, Blob>();
  for (const cover of result.done.covers) {
    // `bytes` is a copy owned by this thread, so wrapping it in a Blob does
    // not hold anything the worker still needs.
    covers.set(cover.id, new Blob([cover.bytes as BlobPart], { type: cover.mediaType }));
  }
  await replaceFolder(folder, result.done.tracks, covers);
}

/// Depth-first walk of a directory handle. Sorted at each level so a rescan
/// produces the same order twice running, matching the native scanner.
async function collectFiles(
  dir: FileSystemDirectoryHandle,
  prefix: string[],
  out: ScanFile[],
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
    if (!isAudioFile(handle.name)) continue;
    out.push({ relativePath: [...prefix, handle.name], file: await handle.getFile() });
  }
  for (const child of dirs) {
    await collectFiles(child, [...prefix, child.name], out);
  }
}

function isAudioFile(name: string): boolean {
  const extension = name.split(".").pop()?.toLowerCase();
  return extension !== undefined && AUDIO_EXTENSIONS.includes(extension);
}

function fromStored(row: StoredTrack): LibraryTrack {
  return { ...row, path: null };
}
