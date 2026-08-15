// Adapter module for the web playback path -- mirrors latent-mixing's
// mixerEngine.ts. Components never touch the worklet/wasm/worker directly;
// everything about web audio playback goes through this module (see
// nativePlayer.ts for the Tauri-side equivalent, where all of this instead
// runs in Rust).
const CONTROL_INTS = 3; // [playing, cursorFrames, totalFrames] -- see worker.ts

let audioCtx: AudioContext | null = null;
let workletNode: AudioWorkletNode | null = null;
let gainNode: GainNode | null = null;
let worker: Worker | null = null;
let sab: SharedArrayBuffer | null = null;
let control: Int32Array | null = null;
let data: Float32Array | null = null;
let channels = 2;
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

/// Decodes `file` (off the main thread) and wires up an AudioWorkletNode to
/// play it. Replaces any previously loaded track.
export async function loadFile(file: File): Promise<TransportState> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const extensionHint = file.name.includes(".") ? file.name.split(".").pop()! : null;

  const decoded = await new Promise<{
    sab: SharedArrayBuffer;
    channels: number;
    sampleRate: number;
    frameCount: number;
  }>((resolve, reject) => {
    const w = getWorker();
    w.onmessage = (e: MessageEvent) => {
      if (e.data.type === "decoded") resolve(e.data);
      else if (e.data.type === "error") reject(new Error(e.data.message));
    };
    w.postMessage({ type: "decode", bytes, extensionHint }, [bytes.buffer]);
  });

  const ctx = await getAudioContext(decoded.sampleRate);

  workletNode?.disconnect();
  gainNode?.disconnect();
  workletNode = new AudioWorkletNode(ctx, "player-worklet", {
    outputChannelCount: [decoded.channels],
    processorOptions: { sab: decoded.sab, channels: decoded.channels },
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

  sab = decoded.sab;
  control = new Int32Array(sab, 0, CONTROL_INTS);
  data = new Float32Array(sab, CONTROL_INTS * 4);
  channels = decoded.channels;
  sampleRate = decoded.sampleRate;

  return transport();
}

export async function play(): Promise<void> {
  if (!control || !audioCtx) return;
  // Replaying a finished track should start it over rather than sit at the
  // end doing nothing -- the worklet outputs silence once the cursor is
  // past totalFrames.
  if (isFinished()) Atomics.store(control, 1, 0);
  await audioCtx.resume();
  Atomics.store(control, 0, 1);
}

export function pause(): void {
  if (!control) return;
  Atomics.store(control, 0, 0);
}

export function isPlaying(): boolean {
  return control ? Atomics.load(control, 0) === 1 : false;
}

function isFinished(): boolean {
  if (!control) return false;
  return Atomics.load(control, 1) >= Atomics.load(control, 2);
}

/// Jumps to `seconds`, clamped to the track. The worklet reads its cursor
/// from the SAB every block, so storing a new frame index is the whole
/// seek -- no message round trip, and it takes effect on the next block.
export function seek(seconds: number): TransportState {
  if (control) {
    const totalFrames = Atomics.load(control, 2);
    const frame = Math.max(0, Math.round(seconds * sampleRate));
    Atomics.store(control, 1, Math.min(frame, totalFrames));
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
  if (!control) {
    return { position: 0, duration: 0, playing: false, volume, finished: false };
  }
  const totalFrames = Atomics.load(control, 2);
  return {
    position: Atomics.load(control, 1) / sampleRate,
    duration: totalFrames / sampleRate,
    playing: isPlaying(),
    volume,
    finished: isFinished(),
  };
}

/// The most recently played `n` interleaved samples, for the scope render
/// loop to draw from -- mirrors player-core's `Playback::scope_window`
/// exactly (same idea, same job, native vs. web).
export function scopeWindow(n: number): Float32Array {
  if (!control || !data) return new Float32Array(0);
  const cursorFrames = Atomics.load(control, 1);
  const end = cursorFrames * channels;
  const start = Math.max(0, end - n);
  return data.subarray(start, end);
}
