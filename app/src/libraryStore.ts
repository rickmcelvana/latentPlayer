// Where the web build's library lives: IndexedDB (M7 decision 4).
//
// The alternatives were considered and rejected for reasons worth keeping next
// to the code:
//   - localStorage is synchronous, capped near 5MB and stores only strings. A
//     library index is none of those things, and writing one on the main
//     thread would stutter the scope's own rAF loop.
//   - OPFS would mean copying the user's music into browser-private storage --
//     gigabytes duplicated, and an index that goes stale against the real
//     folder the moment anything changes on disk.
//   - IndexedDB is asynchronous, structured-clone capable, and can store a
//     `FileSystemDirectoryHandle`. That last property is the one that decides
//     it: it is what makes a *persisted* library possible on the web at all,
//     because the rows and the ability to reopen the real files both survive a
//     reload.
//
// Deliberately hand-rolled rather than pulling in `idb`: four object stores and
// six operations do not justify a dependency, and the sibling repos' policy is
// permissive-licence-only dependencies added on purpose, not by reflex.

/// One row of the library, as the web build stores it. Mirrors
/// `LibraryTrack` in crates/player-core/src/library.rs field for field, except
/// for how the file is found again -- a path there, a handle and a relative
/// path here (M7 decision 3).
export interface StoredTrack {
  id: string;
  /// Which scanned folder this came from, and where inside it. Together these
  /// are how the file is reopened next session: the folder's handle is walked
  /// down this path. A native row stores an absolute path instead.
  folderId: string;
  relativePath: string[];
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
  /// Content hash of the embedded image, naming a blob in the `covers` store.
  /// Content-keyed so an album's twelve tracks share one blob.
  coverId: string | null;
}

/// A folder the user added to the library.
export interface StoredFolder {
  id: string;
  name: string;
  /// Present when the browser has the File System Access API, absent when the
  /// folder arrived through the `<input webkitdirectory>` fallback. Absent
  /// means the rows survive a reload but the files cannot be reopened without
  /// picking the folder again -- which the UI says rather than failing at the
  /// moment someone presses play.
  handle: FileSystemDirectoryHandle | null;
}

export interface StoredPlaylist {
  id: string;
  name: string;
  trackIds: string[];
}

const DB_NAME = "latentplayer-library";
/// Bumped only when the schema changes; `onupgradeneeded` creates whatever is
/// missing, so an existing database gains new stores rather than being dropped.
/// Bumped to 2 in Phase 2 M1 for the `servers` store. The upgrade path is the
/// existing one: every store is created only if absent, so an existing library
/// gains the new store and keeps its rows.
const DB_VERSION = 2;

const TRACKS = "tracks";
const COVERS = "covers";
const FOLDERS = "folders";
const PLAYLISTS = "playlists";
const SERVERS = "servers";

let db: Promise<IDBDatabase> | null = null;

function open(): Promise<IDBDatabase> {
  db ??= new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(TRACKS)) {
        const tracks = database.createObjectStore(TRACKS, { keyPath: "id" });
        // Dropping a folder means deleting its rows, and doing that by index
        // rather than by scanning every row keeps "remove folder" fast on a
        // library that has several.
        tracks.createIndex("folderId", "folderId", { unique: false });
      }
      // Covers are raw blobs keyed by content hash, so they carry no key path
      // of their own.
      if (!database.objectStoreNames.contains(COVERS)) database.createObjectStore(COVERS);
      if (!database.objectStoreNames.contains(FOLDERS)) {
        database.createObjectStore(FOLDERS, { keyPath: "id" });
      }
      if (!database.objectStoreNames.contains(PLAYLISTS)) {
        database.createObjectStore(PLAYLISTS, { keyPath: "id" });
      }
      if (!database.objectStoreNames.contains(SERVERS)) {
        database.createObjectStore(SERVERS, { keyPath: "id" });
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("indexedDB.open failed"));
  });
  return db;
}

/// Wraps one request in a promise. Every operation below is one of these or a
/// transaction's worth of them.
function promisify<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB request failed"));
  });
}

async function withStore<T>(
  names: string | string[],
  mode: IDBTransactionMode,
  run: (tx: IDBTransaction) => Promise<T> | T,
): Promise<T> {
  const database = await open();
  const tx = database.transaction(names, mode);
  const result = await run(tx);
  // Resolve on the *transaction*, not on the last request: a write is only
  // durable once the transaction commits, and returning earlier would let a
  // caller read back its own write before it landed.
  await new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB transaction failed"));
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB transaction aborted"));
  });
  return result;
}

/// A music server the web build can reach (Phase 2 M1).
///
/// The secret is here in **plaintext**, and that is worth stating rather than
/// hiding: a browser has no keychain to put it in. The desktop shell stores its
/// secret in the OS credential store (src-tauri/src/servers.rs); the web build's
/// lives in this origin's IndexedDB, readable by anything with script access to
/// this origin. The panel says so where the user types it, because a weaker
/// guarantee the user does not know about is the worst of both (phase-2
/// decision 4).
///
/// One row, keyed `current`: M1 connects to one server. A list of them is a
/// later milestone's problem and this store is shaped to take it.
export interface StoredServer {
  id: string;
  baseUrl: string;
  user: string;
  auth: "password" | "apiKey";
  secret: string;
}

export const CURRENT_SERVER = "current";

export async function storedServer(): Promise<StoredServer | null> {
  return withStore(SERVERS, "readonly", async (tx) => {
    const found = await promisify<StoredServer | undefined>(
      tx.objectStore(SERVERS).get(CURRENT_SERVER),
    );
    return found ?? null;
  });
}

export async function putServer(server: StoredServer): Promise<void> {
  await withStore(SERVERS, "readwrite", (tx) => {
    tx.objectStore(SERVERS).put(server);
  });
}

export async function deleteServer(): Promise<void> {
  await withStore(SERVERS, "readwrite", (tx) => {
    tx.objectStore(SERVERS).delete(CURRENT_SERVER);
  });
}

export async function allTracks(): Promise<StoredTrack[]> {
  return withStore(TRACKS, "readonly", (tx) =>
    promisify(tx.objectStore(TRACKS).getAll() as IDBRequest<StoredTrack[]>),
  );
}

export async function allFolders(): Promise<StoredFolder[]> {
  return withStore(FOLDERS, "readonly", (tx) =>
    promisify(tx.objectStore(FOLDERS).getAll() as IDBRequest<StoredFolder[]>),
  );
}

export async function allPlaylists(): Promise<StoredPlaylist[]> {
  return withStore(PLAYLISTS, "readonly", (tx) =>
    promisify(tx.objectStore(PLAYLISTS).getAll() as IDBRequest<StoredPlaylist[]>),
  );
}

/// Replaces everything known about one folder: its record, its rows, and any
/// covers the scan produced.
///
/// One transaction, so a scan either lands completely or not at all. The
/// alternative -- writing rows as they arrive -- would leave a half-scanned
/// folder behind if the tab were closed mid-scan, and the user would have no
/// way to tell that from a folder that really did only have 40 tracks in it.
export async function replaceFolder(
  folder: StoredFolder,
  tracks: StoredTrack[],
  covers: Map<string, Blob>,
): Promise<void> {
  await withStore([FOLDERS, TRACKS, COVERS], "readwrite", async (tx) => {
    tx.objectStore(FOLDERS).put(folder);

    // Out with the folder's previous rows -- a rescan updates a folder, it
    // does not add a second copy of it.
    const index = tx.objectStore(TRACKS).index("folderId");
    const stale = await promisify(index.getAllKeys(folder.id) as IDBRequest<IDBValidKey[]>);
    for (const key of stale) tx.objectStore(TRACKS).delete(key);

    for (const track of tracks) tx.objectStore(TRACKS).put(track);
    // Content-keyed, so re-putting an image an album already stored is a
    // no-op rather than a duplicate.
    for (const [id, blob] of covers) tx.objectStore(COVERS).put(blob, id);
  });
}

/// Forgets a folder and its rows. Cover blobs are deliberately left behind:
/// they are shared by content, so another folder may well be using the same
/// image, and an orphaned thumbnail costs kilobytes.
export async function deleteFolder(folderId: string): Promise<void> {
  await withStore([FOLDERS, TRACKS], "readwrite", async (tx) => {
    tx.objectStore(FOLDERS).delete(folderId);
    const index = tx.objectStore(TRACKS).index("folderId");
    const keys = await promisify(index.getAllKeys(folderId) as IDBRequest<IDBValidKey[]>);
    for (const key of keys) tx.objectStore(TRACKS).delete(key);
  });
}

export async function coverBlob(coverId: string): Promise<Blob | null> {
  const blob = await withStore(COVERS, "readonly", (tx) =>
    promisify(tx.objectStore(COVERS).get(coverId) as IDBRequest<Blob | undefined>),
  );
  return blob ?? null;
}

export async function putPlaylist(playlist: StoredPlaylist): Promise<void> {
  await withStore(PLAYLISTS, "readwrite", (tx) => {
    tx.objectStore(PLAYLISTS).put(playlist);
  });
}

export async function deletePlaylist(id: string): Promise<void> {
  await withStore(PLAYLISTS, "readwrite", (tx) => {
    tx.objectStore(PLAYLISTS).delete(id);
  });
}

/// Empties every store. Only the dev harness uses this -- it needs to start
/// from a known state, and a test that inherited a previous run's library
/// would prove nothing.
export async function clearAll(): Promise<void> {
  await withStore([TRACKS, COVERS, FOLDERS, PLAYLISTS], "readwrite", (tx) => {
    for (const name of [TRACKS, COVERS, FOLDERS, PLAYLISTS]) tx.objectStore(name).clear();
  });
}

/// FNV-1a, hex. The same function `player-core`'s library uses for the same
/// two jobs -- naming a row from its location and a cover from its bytes -- so
/// the two shells derive ids the same way even though their stores don't
/// resemble each other.
export function hashHex(bytes: Uint8Array | string): string {
  const data = typeof bytes === "string" ? new TextEncoder().encode(bytes) : bytes;
  // BigInt rather than Number: FNV-1a is 64-bit, and JS numbers lose the low
  // bits above 2^53, which would turn distinct inputs into equal ids.
  let hash = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  const mask = 0xffffffffffffffffn;
  for (const byte of data) {
    hash = (hash ^ BigInt(byte)) & mask;
    hash = (hash * prime) & mask;
  }
  return hash.toString(16).padStart(16, "0");
}
