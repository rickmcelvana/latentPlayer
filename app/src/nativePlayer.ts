// Adapter module for the Tauri (desktop) playback path -- mirrors the
// sibling repos' rule that only one module touches a given backend
// directly (mixerEngine.ts / worker.ts there; this + playerEngine.ts here).
// Components call these functions, never `invoke`/`open` directly.
//
// On desktop, decode + output + the scope's sample feed all happen in Rust
// (player-core via cpal, player-render via a native wgpu window -- see
// src-tauri/src/lib.rs). The webview only sends transport commands and
// picks files; it never touches audio data itself. Compare playerEngine.ts,
// which is the equivalent adapter for the web build, where all of that runs
// in the browser instead (§5's "Web audio I/O" note).
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

/// Opens the native file picker. Returns null if the user cancelled.
export async function pickAudioFile(): Promise<string | null> {
  const path = await open({
    multiple: false,
    filters: [{ name: "Audio", extensions: ["mp3", "flac", "wav", "aac", "m4a", "ogg"] }],
  });
  return typeof path === "string" ? path : null;
}

/// Decodes and starts playing the file at `path`. Returns its duration in
/// seconds.
export async function loadFile(path: string): Promise<number> {
  return invoke<number>("load_file", { path });
}

export async function setPlaying(playing: boolean): Promise<void> {
  return invoke("set_playing", { playing });
}
