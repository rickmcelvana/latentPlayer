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

const AUDIO_FILTERS = [
  { name: "Audio", extensions: ["mp3", "flac", "wav", "aac", "m4a", "ogg"] },
];

/// Opens the native file picker, allowing a whole selection at once so a
/// playlist can be built in one go. Returns an empty array if the user
/// cancelled.
export async function pickAudioFiles(): Promise<string[]> {
  const picked = await open({ multiple: true, filters: AUDIO_FILTERS });
  if (Array.isArray(picked)) return picked;
  return typeof picked === "string" ? [picked] : [];
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
  /// Gapless hand-offs that have reached the output since the last
  /// `loadFile`. Not a playlist index -- the engine only ever knows two
  /// tracks -- so the UI adds it to the index it last loaded.
  trackIndex: number;
}

/// Decodes and starts playing the file at `path`.
export async function loadFile(path: string): Promise<TransportState> {
  return invoke<TransportState>("load_file", { path });
}

/// Queues the track to play when the current one ends, replacing whatever was
/// queued before. Mirrors `setNext` in playerEngine.ts.
export async function setNext(path: string): Promise<void> {
  return invoke("set_next", { path });
}

/// Forgets any queued track, so the current one is the last.
export async function clearNext(): Promise<void> {
  return invoke("clear_next");
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

/// The latest spectrum, one dBFS level per band, as the analysis thread last
/// measured it. Mirrors `spectrum()` in playerEngine.ts; the levels are
/// computed by the same Rust on both shells.
export async function spectrum(): Promise<Float32Array> {
  return new Float32Array(await invoke<number[]>("spectrum"));
}

/// Centre frequency of each band, for the axis. Fixed once a track is open.
export async function spectrumBands(): Promise<Float32Array> {
  return new Float32Array(await invoke<number[]>("spectrum_bands"));
}

/// Loudness and phase at one instant. Mirrors `MeterState` in
/// src-tauri/src/lib.rs and playerEngine.ts's copy, so the meter components
/// never branch on which shell they're in.
export interface MeterSnapshot {
  momentaryLufs: number;
  shortTermLufs: number;
  integratedLufs: number;
  truePeakDb: number;
  correlation: number;
}

/// The meters as the analysis thread last read them. One round trip, because
/// the five numbers describe one instant.
export async function meters(): Promise<MeterSnapshot> {
  return invoke<MeterSnapshot>("meters");
}

/// The goniometer's points, interleaved side/mid. Decimated in Rust to a fixed
/// count, so what crosses the IPC boundary each frame is bounded.
export async function goniometer(): Promise<Float32Array> {
  return new Float32Array(await invoke<number[]>("goniometer"));
}
