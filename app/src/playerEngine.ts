// Adapter module for the web playback path -- mirrors latent-mixing's
// mixerEngine.ts. Components never touch the worklet/wasm/worker directly;
// everything about web audio playback goes through this module (see
// nativePlayer.ts for the Tauri-side equivalent, where all of this instead
// runs in Rust).
const CONTROL_INTS = 3; // [playing, cursorFrames, totalFrames] -- see worker.ts

let audioCtx: AudioContext | null = null;
let workletNode: AudioWorkletNode | null = null;
let worker: Worker | null = null;
let sab: SharedArrayBuffer | null = null;
let control: Int32Array | null = null;
let data: Float32Array | null = null;
let channels = 2;

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
/// play it. Returns the track's duration in seconds. Replaces any
/// previously loaded track.
export async function loadFile(file: File): Promise<number> {
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
  workletNode = new AudioWorkletNode(ctx, "player-worklet", {
    outputChannelCount: [decoded.channels],
    processorOptions: { sab: decoded.sab, channels: decoded.channels },
  });
  workletNode.connect(ctx.destination);

  sab = decoded.sab;
  control = new Int32Array(sab, 0, CONTROL_INTS);
  data = new Float32Array(sab, CONTROL_INTS * 4);
  channels = decoded.channels;

  return decoded.frameCount / decoded.sampleRate;
}

export async function play(): Promise<void> {
  if (!control || !audioCtx) return;
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
