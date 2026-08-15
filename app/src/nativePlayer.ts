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

/// Everything the transport UI needs, read in one round trip so the seek
/// bar, clock and play/pause button can't disagree with each other. Mirrors
/// `TransportState` in src-tauri/src/lib.rs, and playerEngine.ts's
/// `TransportState` on the web side -- the two adapters deliberately expose
/// the same shape so App.tsx doesn't branch on which shell it's in.
export interface TransportState {
  position: number;
  duration: number;
  playing: boolean;
  volume: number;
  finished: boolean;
}

/// Decodes and starts playing the file at `path`.
export async function loadFile(path: string): Promise<TransportState> {
  return invoke<TransportState>("load_file", { path });
}

export async function setPlaying(playing: boolean): Promise<void> {
  return invoke("set_playing", { playing });
}

/// Current transport state. Polled by the UI to drive the seek bar/clock.
export async function transport(): Promise<TransportState> {
  return invoke<TransportState>("transport");
}

export async function seek(seconds: number): Promise<TransportState> {
  return invoke<TransportState>("seek", { seconds });
}

export async function setVolume(volume: number): Promise<void> {
  return invoke("set_volume", { volume });
}
