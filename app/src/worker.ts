// Web Worker: owns the decoder and keeps the playback ring topped up, off the
// main thread (same rule the sibling repos follow for anything CPU-heavy).
//
// Phase 0/M1 decoded the entire file here in one call and copied all of it
// into a SharedArrayBuffer sized to the track. M2 replaces that with a pump: a
// `WasmDecoder` stays open for the life of the track and this loop refills a
// small ring as the worklet drains it. It is the web mirror of the native
// decoder thread (player-core/src/playback.rs) -- same handshake, same
// producer/consumer split; a `setTimeout` loop where the native side has an
// OS thread, because a Worker has no blocking sleep worth using.
//
// Buffer layout lives in audioSab.ts, shared with the worklet and the engine.
import init, { WasmDecoder } from "./wasm/player_rt_wasm.js";
import { CONTROL, audioViews, createAudioSab } from "./audioSab";

/// How often the pump runs when it has nothing else to do. The ring holds ~2s,
/// so this is two orders of magnitude more often than it needs to be -- the
/// margin is for a main thread that is busy rendering.
const PUMP_INTERVAL_MS = 20;

/// Ceiling on how much one pump decodes before yielding. Without it, filling a
/// two-second ring from cold is one long uninterruptible task, which delays
/// the seek this loop is also responsible for noticing.
const PUMP_BUDGET_SAMPLES = 96_000;

interface LoadRequest {
  type: "load";
  bytes: Uint8Array;
  extensionHint: string | null;
}

/// Nudges the pump to run now rather than at the next tick -- sent after a
/// seek so it isn't waiting out a timer before responding.
interface WakeRequest {
  type: "wake";
}

type Request = LoadRequest | WakeRequest;

interface LoadedResponse {
  type: "loaded";
  sab: SharedArrayBuffer;
  channels: number;
  sampleRate: number;
  durationSeconds: number;
}

interface ErrorResponse {
  type: "error";
  message: string;
}

let wasmReady: Promise<unknown> | null = null;
let decoder: WasmDecoder | null = null;
let views: ReturnType<typeof audioViews> | null = null;
let sampleRate = 44_100;
/// The seek this worker has already acted on.
let seq = 0;
let timer: ReturnType<typeof setTimeout> | null = null;

self.onmessage = async (e: MessageEvent<Request>) => {
  if (e.data.type === "wake") {
    pump();
    return;
  }
  if (e.data.type !== "load") return;

  try {
    wasmReady ??= init();
    await wasmReady;

    decoder?.free();
    decoder = WasmDecoder.create(e.data.bytes, e.data.extensionHint ?? undefined);
    const channels = decoder.channels;
    sampleRate = decoder.sample_rate;
    const durationSeconds = decoder.duration_seconds;

    const sab = createAudioSab(channels, sampleRate);
    views = audioViews(sab);
    // 0 means "the container didn't say" and travels all the way to the UI --
    // see StreamingDecoder::duration_seconds for when that happens.
    views.control[CONTROL.totalFrames] = Math.round(durationSeconds * sampleRate);
    seq = 0;

    const response: LoadedResponse = {
      type: "loaded",
      sab,
      channels,
      sampleRate,
      durationSeconds,
    };
    self.postMessage(response);
    pump();
  } catch (err) {
    const response: ErrorResponse = { type: "error", message: String(err) };
    self.postMessage(response);
  }
};

/// One turn of decode-into-the-ring, then reschedule.
function pump(): void {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  if (!decoder || !views) return;
  const { control, ring, ringFrames, channels } = views;

  try {
    // Step 2 of the handshake (see audioSab.ts): move the decoder, then say so
    // *before* writing anything. Until the worklet acknowledges, everything in
    // the ring is from before the seek and nothing new is on its way, which is
    // what makes it safe for the worklet to throw all of it away.
    const requested = Atomics.load(control, CONTROL.seekSeq);
    if (requested !== seq) {
      const target = Atomics.load(control, CONTROL.seekTargetFrames) / sampleRate;
      decoder.seek(target);
      Atomics.store(control, CONTROL.eos, 0);
      seq = requested;
      Atomics.store(control, CONTROL.decoderReadySeq, seq);
      schedule();
      return;
    }
    if (Atomics.load(control, CONTROL.drainedSeq) !== seq) {
      schedule();
      return;
    }

    let budget = PUMP_BUDGET_SAMPLES;
    while (budget > 0) {
      const freeFrames = ringFrames - Atomics.load(control, CONTROL.filledFrames);
      if (freeFrames <= 0) break;

      const writeIndex = Atomics.load(control, CONTROL.writeIndex);
      // Only up to the end of the ring: the wrap is handled by going round the
      // loop again, so the decoder never has to know the buffer is circular.
      const contiguousFrames = Math.min(freeFrames, ringFrames - writeIndex);
      const maxSamples = Math.min(contiguousFrames * channels, budget);
      const written = decoder.read(ring, writeIndex * channels, maxSamples);
      if (written === 0) {
        if (decoder.is_finished) Atomics.store(control, CONTROL.eos, 1);
        break;
      }

      const frames = written / channels;
      Atomics.store(control, CONTROL.writeIndex, (writeIndex + frames) % ringFrames);
      Atomics.add(control, CONTROL.filledFrames, frames);
      budget -= written;
    }
  } catch (err) {
    const response: ErrorResponse = { type: "error", message: String(err) };
    self.postMessage(response);
    return;
  }

  schedule();
}

function schedule(): void {
  timer = setTimeout(pump, PUMP_INTERVAL_MS);
}
