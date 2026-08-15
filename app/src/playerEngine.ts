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
import { CONTROL, SCOPE_MASK, SCOPE_SAMPLES, audioViews } from "./audioSab";

let audioCtx: AudioContext | null = null;
let workletNode: AudioWorkletNode | null = null;
let gainNode: GainNode | null = null;
let worker: Worker | null = null;
let views: ReturnType<typeof audioViews> | null = null;
let sampleRate = 44_100;
let volume = 1;

/// Mirrors nativePlayer.ts's `TransportState` (and `TransportState` in
/// src-tauri/src/lib.rs) exactly, so components can drive the transport
/// without knowing which shell they're running in.
export interface TransportState {
  position: number;
  duration: number;
  playing: boolean;
  volume: number;
  finished: boolean;
}

function getWorker(): Worker {
  worker ??= new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
  return worker;
}

/// `sampleRate` must match the track being loaded: the worklet writes raw
/// decoded samples straight to the output with no resampling, so an
/// AudioContext running at a different rate than the source (e.g. the
/// browser's 48kHz default output against a 44.1kHz MP3) plays back
/// pitched/sped up -- found live (2026-08-15): vocals audibly high-pitched
/// on the web build while the native build, which uses the file's own real
/// sample rate via cpal, sounded correct. If a context already exists at a
/// different rate (loading a second file with a different native rate),
/// it's closed and replaced -- Web Audio has no way to change an existing
/// context's rate.
async function getAudioContext(sampleRate: number): Promise<AudioContext> {
  if (audioCtx && audioCtx.sampleRate !== sampleRate) {
    await audioCtx.close();
    audioCtx = null;
  }
  if (!audioCtx) {
    audioCtx = new AudioContext({ sampleRate });
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
  const bytes = new Uint8Array(await file.arrayBuffer());
  const extensionHint = file.name.includes(".") ? file.name.split(".").pop()! : null;

  const loaded = await new Promise<{
    sab: SharedArrayBuffer;
    channels: number;
    sampleRate: number;
    durationSeconds: number;
  }>((resolve, reject) => {
    const w = getWorker();
    w.onmessage = (e: MessageEvent) => {
      if (e.data.type === "loaded") resolve(e.data);
      else if (e.data.type === "error") reject(new Error(e.data.message));
    };
    w.postMessage({ type: "load", bytes, extensionHint }, [bytes.buffer]);
  });

  const ctx = await getAudioContext(loaded.sampleRate);

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

  return transport();
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

/// Current transport state, matching the native adapter's shape.
export function transport(): TransportState {
  if (!views) {
    return { position: 0, duration: 0, playing: false, volume, finished: false };
  }
  const { control } = views;
  // 0 means the container didn't state a length -- the same "unknown" the
  // native adapter reports. Phase 0 always knew, because it had decoded the
  // whole file and could measure it.
  const totalFrames = Atomics.load(control, CONTROL.totalFrames);
  return {
    position: Atomics.load(control, CONTROL.positionFrames) / sampleRate,
    duration: totalFrames / sampleRate,
    playing: isPlaying(),
    volume,
    finished: isFinished(),
  };
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
