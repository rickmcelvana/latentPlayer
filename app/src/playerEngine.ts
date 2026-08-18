// Adapter module for the web playback path -- mirrors latent-mixing's
// mixerEngine.ts. Components never touch the worklet/wasm/worker directly;
// everything about web audio playback goes through this module (see
// nativePlayer.ts for the Tauri-side equivalent, where all of this instead
// runs in Rust).
//
// Since Phase 1 M2 the audio arrives through a ring the worker keeps topped
// up rather than a buffer holding the whole decoded track (audioSab.ts).
// This module's job is unchanged from the outside: it still reports the same
// `TransportState` the native adapter does, so App.tsx never branches on which
// shell it's in.
import {
  CONTROL,
  EQ_BANDS,
  EQ_MAX_GAIN_DB,
  LOUDNESS_FLOOR_LUFS,
  METER,
  SCOPE_MASK,
  SCOPE_SAMPLES,
  SPECTRUM_FLOOR_DB,
  TRUE_PEAK_FLOOR_DB,
  audioViews,
  bufferingAfterFrames,
} from "./audioSab";

let audioCtx: AudioContext | null = null;
let workletNode: AudioWorkletNode | null = null;
let gainNode: GainNode | null = null;
let worker: Worker | null = null;
let views: ReturnType<typeof audioViews> | null = null;
let sampleRate = 44_100;
let volume = 1;
let bandCentres = new Float32Array(0);

/// Output channels for the session. Fixed rather than taken from the track,
/// for the same reason the rate is: the ring's geometry cannot change while a
/// queue is handing between tracks. Two because that is what Web Audio's
/// destination defaults to and what the XY scope wants to plot.
const OUTPUT_CHANNELS = 2;

/// Mirrors nativePlayer.ts's `TransportState` (and `TransportState` in
/// src-tauri/src/lib.rs) exactly, so components can drive the transport
/// without knowing which shell they're running in.
export interface TransportState {
  position: number;
  duration: number;
  playing: boolean;
  volume: number;
  finished: boolean;
  trackIndex: number;
  /// Waiting for audio that has not arrived (M5). The state both engines have
  /// always had -- an empty ring with the track not ended -- and neither could
  /// say until now, which over a network is the difference between "this is
  /// slow" and "this is broken".
  buffering: boolean;
}

/// The load in flight, if any. One at a time by construction -- a second load
/// replaces the first -- so this is a slot rather than a queue.
let pendingLoad: {
  resolve: (loaded: LoadedMessage) => void;
  reject: (error: Error) => void;
} | null = null;

interface LoadedMessage {
  sab: SharedArrayBuffer;
  channels: number;
  sampleRate: number;
  durationSeconds: number;
  bandCentres: Float32Array;
}

/// The worker, with one message handler for the life of the session.
///
/// It used to be set per load, which worked while "loaded" and "error" were the
/// only messages there were. The EQ's curve arrives whenever a slider moves,
/// including between loads, so the routing has to outlive any one of them.
function getWorker(): Worker {
  if (!worker) {
    worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (e: MessageEvent) => {
      const message = e.data;
      if (message.type === "loaded") {
        pendingLoad?.resolve(message as LoadedMessage);
        pendingLoad = null;
      } else if (message.type === "error") {
        const waiting = pendingLoad;
        pendingLoad = null;
        // An error with nothing waiting on it is a decode that failed after
        // the load returned -- a queued track that wouldn't open, say. It has
        // nowhere to be thrown, so it is reported rather than swallowed.
        if (waiting) waiting.reject(new Error(message.message));
        else console.error("player worker:", message.message);
      } else if (message.type === "eqResponse") {
        // The settings come back through Rust's clamp, so what is shown is what
        // the filters are running -- the desktop shell's `set_eq` answers the
        // same way.
        eqSettings = {
          enabled: eqSettings.enabled,
          preampDb: message.preampDb,
          gainsDb: Array.from(message.gainsDb as number[]),
        };
        eqResponseDb = new Float32Array(message.responseDb);
        eqResponseHz = new Float32Array(message.responseFrequencies);
        eqBandHz = new Float32Array(message.bandFrequencies);
        eqMaxGain = message.maxGainDb;
        eqCurveVersion += 1;
      }
    };
  }
  return worker;
}

/// One context, one rate, for the whole session -- whatever the browser's
/// output happens to run at -- with every track resampled to it in the worker.
///
/// M1 did the opposite: it built the context at the *file's* rate, because the
/// worklet wrote decoded samples straight out with no resampling and a
/// mismatch plays pitched (found live 2026-08-15 -- audibly high vocals on the
/// web build while native, which resamples to the device via cpal, sounded
/// right). That fix worked for one track at a time and cannot survive a queue:
/// Web Audio has no way to change a live context's rate, so per-track rates
/// mean tearing the context down between tracks, i.e. gapless except when the
/// rate changes. Resampling in the worker (M3) is the native path's answer,
/// and now the web's.
async function getAudioContext(): Promise<AudioContext> {
  if (!audioCtx) {
    audioCtx = new AudioContext();
    await audioCtx.audioWorklet.addModule("/worklet/player-worklet.js");
  }
  return audioCtx;
}

/// Opens `file` and wires up an AudioWorkletNode to play it. Replaces any
/// previously loaded track.
///
/// Returns once the format is known and the worker has started filling the
/// ring -- not once the file is decoded, which is the point of M2. The browser
/// still hands over the whole file as bytes (there is no path to stream from),
/// but the *decoded* audio is now bounded by the ring rather than by the
/// track's length.
export async function loadFile(file: File): Promise<TransportState> {
  const { bytes, extensionHint } = await readFile(file);
  return loadEncoded(bytes, extensionHint);
}

/// The same, for audio that did not come from a `File` -- a server track's
/// bytes, fetched (Phase 2 M2). Everything past this point is identical:
/// the worker cannot tell where a track came from, any more than the desktop
/// engine can tell a `RemoteSource` from a file on disk.
export async function loadEncoded(
  bytes: Uint8Array,
  extensionHint: string | null,
): Promise<TransportState> {
  const ctx = await getAudioContext();

  const loaded = await new Promise<LoadedMessage>((resolve, reject) => {
    const w = getWorker();
    pendingLoad = { resolve, reject };
    // Not transferred: the worker keeps the encoded bytes so it can reopen the
    // track, which a seek arriving mid-hand-off has to do.
    w.postMessage({
      type: "load",
      bytes,
      extensionHint,
      outRate: ctx.sampleRate,
      outChannels: OUTPUT_CHANNELS,
    });
  });

  workletNode?.disconnect();
  gainNode?.disconnect();
  workletNode = new AudioWorkletNode(ctx, "player-worklet", {
    outputChannelCount: [loaded.channels],
    processorOptions: { sab: loaded.sab },
  });
  // Volume rides on a GainNode rather than a multiply inside the worklet:
  // Web Audio already has to touch every sample here, and its own
  // parameter smoothing avoids the zipper noise a raw per-block gain
  // change would cause. The native path has no equivalent, so it does the
  // multiply itself (player-core/src/playback.rs).
  gainNode = ctx.createGain();
  gainNode.gain.value = volume;
  workletNode.connect(gainNode);
  gainNode.connect(ctx.destination);

  views = audioViews(loaded.sab);
  sampleRate = loaded.sampleRate;
  // Copied out of the message rather than kept: the worker builds it over
  // wasm memory, which is a SharedArrayBuffer under COOP/COEP and can move.
  bandCentres = new Float32Array(loaded.bandCentres);

  return transport();
}

/// Queues the track to play when the current one ends, replacing whatever was
/// queued before. Mirrors `Playback::set_next` on the native side, including
/// what it deliberately leaves out: the engine holds what is playing and what
/// is next, and nothing else. Which track that is, and what follows it, stay
/// the caller's business -- watch `trackIndex` for the hand-off and queue the
/// one after it.
export async function setNext(file: File): Promise<void> {
  const { bytes, extensionHint } = await readFile(file);
  setNextEncoded(bytes, extensionHint);
}

/// Queues already-fetched bytes -- the web shell's prefetch. The fetching
/// happens in the caller (playlist.ts) rather than here, and happens when the
/// track is queued rather than when the seam arrives, which is what makes a
/// gapless hand-off between two server tracks possible at all.
export function setNextEncoded(bytes: Uint8Array, extensionHint: string | null): void {
  getWorker().postMessage({ type: "queue", bytes, extensionHint });
}

/// Forgets any queued track, so the current one is the last.
export function clearNext(): void {
  getWorker().postMessage({ type: "queue", bytes: null, extensionHint: null });
}

/// How many gapless hand-offs have reached the output. Increments when the new
/// track's first frame is played, not when the worker opened it, so a caller
/// that redraws on a change redraws in time with the music.
export function trackIndex(): number {
  return views ? Atomics.load(views.control, CONTROL.trackIndex) : 0;
}

/// The rate every track is resampled to, fixed for the session. Mirrors
/// `Playback::output_sample_rate`.
export function outputSampleRate(): number {
  return sampleRate;
}

/// Channels the output runs at, which is also how `scopeWindow`'s samples are
/// interleaved. Mirrors `Playback::output_channels`.
export function outputChannels(): number {
  return OUTPUT_CHANNELS;
}

async function readFile(file: File): Promise<{ bytes: Uint8Array; extensionHint: string | null }> {
  return {
    bytes: new Uint8Array(await file.arrayBuffer()),
    extensionHint: file.name.includes(".") ? file.name.split(".").pop()! : null,
  };
}

export async function play(): Promise<void> {
  if (!views || !audioCtx) return;
  // Replaying a finished track should start it over rather than sit at the
  // end doing nothing.
  if (isFinished()) seek(0);
  await audioCtx.resume();
  Atomics.store(views.control, CONTROL.playing, 1);
}

export function pause(): void {
  if (!views) return;
  Atomics.store(views.control, CONTROL.playing, 0);
}

export function isPlaying(): boolean {
  return views ? Atomics.load(views.control, CONTROL.playing) === 1 : false;
}

function isFinished(): boolean {
  if (!views) return false;
  return Atomics.load(views.control, CONTROL.finished) === 1;
}

/// Jumps to `seconds`, clamped to the track.
///
/// M1 could do this with a single store, because the whole decoded track was
/// in memory and seeking was just choosing a different index into it. With
/// streaming the audio has to be re-decoded from the new position, so this
/// posts the request and returns; the sound follows a moment later, once the
/// worker has moved and the worklet has thrown away what was queued (see
/// audioSab.ts's handshake). The reported position updates immediately
/// regardless, so the seek bar doesn't snap back under the user's finger.
export function seek(seconds: number): TransportState {
  if (views) {
    const { control } = views;
    const totalFrames = Atomics.load(control, CONTROL.totalFrames);
    let frame = Math.max(0, Math.round(seconds * sampleRate));
    if (totalFrames > 0) frame = Math.min(frame, totalFrames);

    Atomics.store(control, CONTROL.seekTargetFrames, frame);
    Atomics.store(control, CONTROL.positionFrames, frame);
    Atomics.store(control, CONTROL.finished, 0);
    Atomics.add(control, CONTROL.seekSeq, 1);
    // The worker also polls, but a poll interval of latency on every seek is
    // needless when a message costs nothing.
    getWorker().postMessage({ type: "wake" });
  }
  return transport();
}

/// Linear gain, clamped to 0..1 -- same contract as the native side.
export function setVolume(next: number): void {
  volume = Math.min(1, Math.max(0, next));
  if (gainNode) gainNode.gain.value = volume;
}

/// Where the EQ's sliders are (M8). Mirrors `EqSettings` in
/// player-core/src/eq.rs and `EqState` in src-tauri/src/lib.rs.
export interface EqSettings {
  enabled: boolean;
  preampDb: number;
  gainsDb: number[];
}

/// The EQ as a display needs it: the sliders, their labels, and the curve they
/// add up to. One shape for both shells, like `TransportState`.
export interface EqState extends EqSettings {
  /// The sliders' centre frequencies.
  bandFrequencies: Float32Array;
  /// The curve: gain in dB at each of `responseFrequencies`.
  responseDb: Float32Array;
  responseFrequencies: Float32Array;
  maxGainDb: number;
}

let eqSettings: EqSettings = {
  enabled: true,
  preampDb: 0,
  gainsDb: new Array(EQ_BANDS).fill(0),
};
/// The curve and its axes, as the worker last computed them in Rust. Empty
/// until the first `setEq` or `eqState` -- the UI asks once on mount.
let eqResponseDb = new Float32Array(0);
let eqResponseHz = new Float32Array(0);
let eqBandHz = new Float32Array(0);
let eqMaxGain = EQ_MAX_GAIN_DB;
/// Curves the worker has sent, so a caller can tell its own answer from the
/// one before it.
let eqCurveVersion = 0;

/// Moves the sliders. The worker designs the coefficients and publishes them
/// into the shared buffer; the worklet picks them up on its next block, so this
/// is audible within one 128-frame block (2.7ms at 48kHz) rather than after the
/// ring drains.
export async function setEq(settings: EqSettings): Promise<EqState> {
  eqSettings = {
    enabled: settings.enabled,
    preampDb: settings.preampDb,
    gainsDb: settings.gainsDb.slice(0, EQ_BANDS),
  };
  const before = eqCurveVersion;
  getWorker().postMessage({ type: "eq", ...eqSettings });
  return waitForCurve(before);
}

/// The current setting and its curve.
export async function eqState(): Promise<EqState> {
  // Asking is what triggers the first curve, since the worker owns the Rust
  // that computes it and may not have been asked for one yet.
  const before = eqCurveVersion;
  getWorker().postMessage({ type: "eq", ...eqSettings });
  return waitForCurve(before);
}

/// Waits for the worker's answer to the setting just posted.
///
/// A version counter rather than a per-request id: the curve is a function of
/// the settings alone, so two answers in flight cannot disagree about anything
/// except which is newer, and the newer one always lands last. Waiting for the
/// *count* to move is what stops a slider drag drawing the previous position's
/// curve. A timeout returns the last curve there was, which is the honest
/// degradation -- a stale curve beats a blank panel.
async function waitForCurve(after: number): Promise<EqState> {
  for (let attempt = 0; attempt < 50 && eqCurveVersion === after; attempt++) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  return {
    ...eqSettings,
    gainsDb: eqSettings.gainsDb.slice(),
    bandFrequencies: eqBandHz,
    responseDb: eqResponseDb,
    responseFrequencies: eqResponseHz,
    maxGainDb: eqMaxGain,
  };
}

/// Current transport state, matching the native adapter's shape.
export function transport(): TransportState {
  if (!views) {
    return {
      position: 0,
      duration: 0,
      playing: false,
      volume,
      finished: false,
      trackIndex: 0,
      buffering: false,
    };
  }
  const { control } = views;
  // 0 means the container didn't state a length -- the same "unknown" the
  // native adapter reports. Phase 0 always knew, because it had decoded the
  // whole file and could measure it.
  const totalFrames = Atomics.load(control, CONTROL.totalFrames);
  const finished = isFinished();
  return {
    position: Atomics.load(control, CONTROL.positionFrames) / sampleRate,
    duration: totalFrames / sampleRate,
    playing: isPlaying(),
    volume,
    finished,
    trackIndex: trackIndex(),
    // The worklet counts the silence; how much of it is worth calling
    // "buffering" is one number, and it comes from the same Rust the native
    // shell applies (M5 decision 15) rather than being written again here.
    // False at the end of a track, where there is nothing left to wait for.
    buffering:
      !finished &&
      Atomics.load(control, CONTROL.starvedFrames) >= bufferingAfterFrames(sampleRate),
  };
}

/// The latest spectrum, one dBFS level per band, as the worker's meters last
/// measured it (~20ms). Mirrors `Playback::spectrum` natively -- and the
/// numbers come from the same Rust either way, so the two shells draw the same
/// picture of the same audio.
///
/// Copies rather than handing back the shared view: a caller holding a live
/// view would see values change under it mid-draw.
export function spectrum(): Float32Array {
  if (!views) return new Float32Array(0);
  return new Float32Array(views.spectrum);
}

/// Centre frequency of each band, for the axis. Empty until a track is loaded,
/// since the analyser is built with the output's rate.
export function spectrumBands(): Float32Array {
  return bandCentres;
}

/// The floor the spectrum reports for a band with nothing in it.
export function spectrumFloorDb(): number {
  return SPECTRUM_FLOOR_DB;
}

/// Loudness and phase at one instant. Mirrors `MeterState` in
/// src-tauri/src/lib.rs and nativePlayer.ts's copy, the same way
/// `TransportState` does -- one shape, so the meter components never branch on
/// which shell they're in.
export interface MeterSnapshot {
  /// 400ms loudness, LUFS.
  momentaryLufs: number;
  /// 3s loudness, LUFS.
  shortTermLufs: number;
  /// Gated loudness since the meters were last reset, LUFS.
  integratedLufs: number;
  /// Highest inter-sample peak since then, dBTP.
  truePeakDb: number;
  /// Stereo correlation: +1 mono, 0 uncorrelated, -1 inverted.
  correlation: number;
}

/// The meters as the worker's analyser last read them (~20ms).
///
/// Read out of the shared buffer in one go, so the five numbers are one
/// instant's -- which is the whole reason they are published together
/// (M5 decision 4).
export function meters(): MeterSnapshot {
  if (!views) return silentMeters();
  const m = views.meters;
  return {
    momentaryLufs: m[METER.momentaryLufs],
    shortTermLufs: m[METER.shortTermLufs],
    integratedLufs: m[METER.integratedLufs],
    truePeakDb: m[METER.truePeakDb],
    correlation: m[METER.correlation],
  };
}

/// What the meters read before anything has been measured.
export function silentMeters(): MeterSnapshot {
  return {
    momentaryLufs: LOUDNESS_FLOOR_LUFS,
    shortTermLufs: LOUDNESS_FLOOR_LUFS,
    integratedLufs: LOUDNESS_FLOOR_LUFS,
    truePeakDb: TRUE_PEAK_FLOOR_DB,
    correlation: 0,
  };
}

/// The goniometer's points -- interleaved side/mid pairs, computed in Rust
/// (M5 decision 7). Mirrors `Playback::goniometer` natively.
///
/// Copies rather than handing back the shared view, and takes only the pairs
/// the worker says are valid: early in a track the buffer's tail is still the
/// zeros it was created with, which would draw as a bright dot in the centre.
export function goniometer(): Float32Array {
  if (!views) return new Float32Array(0);
  const points = Atomics.load(views.control, CONTROL.goniometerPoints);
  return new Float32Array(views.goniometer.subarray(0, points * 2));
}

/// The floors the loudness meters report with nothing to measure.
export function loudnessFloorLufs(): number {
  return LOUDNESS_FLOOR_LUFS;
}

export function truePeakFloorDb(): number {
  return TRUE_PEAK_FLOOR_DB;
}

/// The most recently played `n` interleaved samples, for the scope render
/// loop to draw from -- mirrors player-core's `Playback::scope_window`
/// exactly (same idea, same job, native vs. web).
///
/// Copies rather than returning a view: the samples are a wrapped ring now,
/// not a contiguous slice of the track.
export function scopeWindow(n: number): Float32Array {
  if (!views) return new Float32Array(0);
  const { control, scope } = views;
  const filled = Atomics.load(control, CONTROL.scopeFilled);
  const index = Atomics.load(control, CONTROL.scopeIndex);
  const count = Math.min(n, filled, SCOPE_SAMPLES);
  const out = new Float32Array(count);
  const start = index - count;
  for (let i = 0; i < count; i++) out[i] = scope[(start + i) & SCOPE_MASK];
  return out;
}
