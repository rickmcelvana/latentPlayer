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
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  BrowseRequest,
  BrowseResult,
  MarkRequest,
  PlaylistRequest,
  PlaylistResult,
  ServerTrack,
} from "./serverTypes";

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
  /// Waiting for audio that has not arrived (M5). Over a network this is the
  /// difference between "slow" and "broken", and until M5 the two drew the
  /// same picture: playing, with the position held and nothing said.
  buffering: boolean;
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

/// Where the EQ's sliders are (M8). Mirrors `EqSettings` in
/// player-core/src/eq.rs and playerEngine.ts's copy.
export interface EqSettings {
  enabled: boolean;
  preampDb: number;
  gainsDb: number[];
}

/// The EQ as a display needs it: the sliders, their labels, and the curve they
/// add up to -- read in one round trip, because a UI that fetched them
/// separately could draw a curve for a setting it isn't showing. Mirrors
/// `EqState` in src-tauri/src/lib.rs.
export interface EqState extends EqSettings {
  bandFrequencies: Float32Array;
  responseDb: Float32Array;
  responseFrequencies: Float32Array;
  maxGainDb: number;
}

/// The arrays cross the JSON boundary as plain number lists; the components
/// draw from typed arrays, as they do on the web side.
interface RawEqState {
  enabled: boolean;
  preampDb: number;
  gainsDb: number[];
  bandFrequencies: number[];
  responseDb: number[];
  responseFrequencies: number[];
  maxGainDb: number;
}

function eqStateFrom(raw: RawEqState): EqState {
  return {
    enabled: raw.enabled,
    preampDb: raw.preampDb,
    gainsDb: raw.gainsDb,
    bandFrequencies: new Float32Array(raw.bandFrequencies),
    responseDb: new Float32Array(raw.responseDb),
    responseFrequencies: new Float32Array(raw.responseFrequencies),
    maxGainDb: raw.maxGainDb,
  };
}

export async function eqState(): Promise<EqState> {
  return eqStateFrom(await invoke<RawEqState>("eq_state"));
}

/// Moves the sliders and returns what the engine settled on, clamping
/// included -- so the display shows what is being played rather than what was
/// asked for.
export async function setEq(settings: EqSettings): Promise<EqState> {
  return eqStateFrom(
    await invoke<RawEqState>("set_eq", {
      enabled: settings.enabled,
      preampDb: settings.preampDb,
      gainsDb: settings.gainsDb,
    }),
  );
}

/// Geometry of the scope's own window. Mirrors `ScopeState` in
/// src-tauri/src/lib.rs; see scope.ts for why this is an adapter rather than
/// something components call directly.
export interface ScopeState {
  width: number;
  height: number;
  visible: boolean;
}

export async function scopeState(): Promise<ScopeState> {
  return invoke<ScopeState>("scope_state");
}

/// Resizes the scope window. The renderer picks the new size up from its own
/// size poll, so nothing here has to tell it.
export async function setScopeSize(width: number, height: number): Promise<void> {
  return invoke("set_scope_size", { width, height });
}

/// Shows or hides the scope window (hidden, not closed -- the renderer's
/// surface is built from its handle).
export async function setScopeVisible(visible: boolean): Promise<void> {
  return invoke("set_scope_visible", { visible });
}

/// The music server's state (Phase 2 M1). Mirrors `ServerState` in
/// src-tauri/src/servers.rs, and `subsonicWeb.ts`'s copy on the web side -- one
/// shape, so `ServerPanel` never branches on which shell it is in.
export interface ServerState {
  configured: boolean;
  connected: boolean;
  baseUrl: string;
  user: string;
  auth: string;
  serverType: string;
  serverVersion: string;
  apiVersion: string;
  openSubsonic: boolean;
  supportsApiKey: boolean;
  error: string;
  errorIsCredentials: boolean;
  /// What kind of failure it was, in terms of what has to change (M5): one of
  /// "credentials", "certificate", "unreachable", "address", "server",
  /// "malformed" -- or empty when nothing failed. `error_kind` in
  /// src-tauri/src/servers.rs, out of `SubsonicError::kind` -- so both shells
  /// classify with the same Rust rather than by reading message text.
  errorKind: string;
}

export async function serverState(): Promise<ServerState> {
  return invoke<ServerState>("server_state");
}

/// Connects and, if the server accepts the secret, remembers it -- the address
/// in the app data directory, the secret in the OS credential store.
///
/// A rejected password comes back as a `ServerState` carrying the reason rather
/// than as a thrown error: it is an ordinary outcome the panel draws.
export async function connectServer(
  baseUrl: string,
  user: string,
  secret: string,
  auth: "password" | "apiKey",
): Promise<ServerState> {
  return invoke<ServerState>("connect_server", { baseUrl, user, secret, auth });
}

export async function reconnectServer(): Promise<ServerState> {
  return invoke<ServerState>("reconnect_server");
}

export async function forgetServer(): Promise<ServerState> {
  return invoke<ServerState>("forget_server");
}

/// A handful of tracks at random -- still the shortest path from a connected
/// server to sound, and what `remote_smoke` plays.
export async function serverRandomTracks(count: number): Promise<ServerTrack[]> {
  return invoke<ServerTrack[]>("server_random_tracks", { count });
}

/// One browse request against the server's library (Phase 2 M3).
///
/// One command for all five, because which endpoint a request is and what its
/// answer means are `player_core::subsonic::browse`'s business -- the same
/// module the web shell calls through wasm.
export async function serverBrowse(request: BrowseRequest): Promise<BrowseResult> {
  return invoke<BrowseResult>("server_browse", { request });
}

/// One playlist request against the server (Phase 2 M4).
///
/// One command for the two reads and the four writes, for the reason
/// `serverBrowse` is one command for five browses.
export async function serverPlaylists(request: PlaylistRequest): Promise<PlaylistResult> {
  return invoke<PlaylistResult>("server_playlists", { request });
}

/// One star, unstar or rating (Phase 2 M4).
export async function serverMark(request: MarkRequest): Promise<void> {
  return invoke("server_mark", { request });
}

/// One observation of where playback is, and whatever it earned (Phase 2 M4).
///
/// The tracker lives in this shell's Tauri state rather than here, because the
/// rule it applies is `player_core`'s and the web shell holds the very same code
/// in a wasm object (decision 10). The clock is passed in from here for the same
/// reason the auth salt is: time and randomness belong to the shell.
export async function serverScrobbleTick(
  trackId: string | null,
  positionSeconds: number,
  durationSeconds: number,
  playing: boolean,
): Promise<string[]> {
  return invoke<string[]>("server_scrobble_tick", {
    trackId,
    positionSeconds,
    durationSeconds,
    playing,
    nowMs: Date.now(),
  });
}

/// A URL for one piece of the server's cover art.
///
/// Synchronous, and an ordinary URL an `<img src>` takes: the `cover://` scheme
/// this shell already serves the local library's art through, with a prefix that
/// marks the id as the server's. The fetch happens inside that handler, off the
/// main thread, so a list of rows does not hold up the UI (M7 decision 5 --
/// art never travels through `invoke`).
export function serverCoverUrl(coverArt: string): string {
  return convertFileSrc(`server:${coverArt}`, "cover");
}

/// Opens a server track and starts playing it. The same shape as `loadFile`,
/// because on this shell the difference is one `PlaybackSource` deep: the bytes
/// arrive over HTTP as the decoder asks for them, and nothing downstream knows.
export async function loadServerTrack(id: string, suffix: string): Promise<TransportState> {
  return invoke<TransportState>("load_server_track", { id, suffix });
}

/// Queues a server track for the gapless hand-off. The decoder opens it when it
/// runs out of the current track, which is where the prefetch happens.
export async function setNextServerTrack(id: string, suffix: string): Promise<void> {
  return invoke("set_next_server_track", { id, suffix });
}
