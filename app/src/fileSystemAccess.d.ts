// The parts of the File System Access API TypeScript's DOM library doesn't
// declare yet.
//
// `FileSystemDirectoryHandle` and its async iterators *are* in lib.dom.d.ts
// (checked against the typescript in node_modules, not assumed) -- what is
// missing is the entry point that hands one over and the permission methods
// that make a stored handle usable again in a later session. Those two are
// exactly what M7's web library is built on, so they are declared here rather
// than cast away with `any`, which CONVENTIONS.md forbids.
//
// Both are Chromium-only today. The library falls back to
// `<input webkitdirectory>` where they are missing, which is why every use of
// them is behind a feature test rather than assumed present.

/// Whether the page may read through a handle without prompting again.
type FileSystemPermissionState = "granted" | "denied" | "prompt";

interface FileSystemHandlePermissionDescriptor {
  mode?: "read" | "readwrite";
}

interface FileSystemHandle {
  /// Current permission, without prompting. Returns "prompt" for a handle
  /// restored from IndexedDB in a new session -- the handle survives, the
  /// grant does not.
  queryPermission(
    descriptor?: FileSystemHandlePermissionDescriptor,
  ): Promise<FileSystemPermissionState>;

  /// Asks for permission. Must be called from a user gesture, which is why the
  /// UI has a button for it rather than doing this on load.
  requestPermission(
    descriptor?: FileSystemHandlePermissionDescriptor,
  ): Promise<FileSystemPermissionState>;
}

interface DirectoryPickerOptions {
  id?: string;
  mode?: "read" | "readwrite";
  startIn?: FileSystemHandle | string;
}

interface Window {
  showDirectoryPicker?: (options?: DirectoryPickerOptions) => Promise<FileSystemDirectoryHandle>;
}
