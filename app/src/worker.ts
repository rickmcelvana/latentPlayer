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
// M3 adds the queue: this holds two tracks, the one playing and the one after
// it, and crosses between them without a hole. The `WasmDecoder` converts to
// the output's rate and channel count before handing anything over, so the
// ring's geometry is the session's and does not change at a track boundary.
//
// Buffer layout lives in audioSab.ts, shared with the worklet and the engine.
import init, { WasmAnalyzer, WasmDecoder } from "./wasm/player_rt_wasm.js";
import {
  BOUNDARY_AT,
  BOUNDARY_BASE,
  BOUNDARY_FIELDS,
  BOUNDARY_SLOTS,
  BOUNDARY_TOTAL_FRAMES,
  CONTROL,
  SCOPE_MASK,
  SCOPE_SAMPLES,
  audioViews,
  createAudioSab,
  framesUntil,
} from "./audioSab";

/// How often the pump runs when it has nothing else to do. The ring holds ~2s,
/// so this is two orders of magnitude more often than it needs to be -- the
/// margin is for a main thread that is busy rendering.
const PUMP_INTERVAL_MS = 20;

/// Ceiling on how much one pump decodes before yielding. Without it, filling a
/// two-second ring from cold is one long uninterruptible task, which delays
/// the seek this loop is also responsible for noticing.
const PUMP_BUDGET_SAMPLES = 96_000;

/// A track's encoded bytes, kept so it can be reopened -- which a seek
/// arriving during a hand-off has to do (see `pump`).
interface Source {
  bytes: Uint8Array;
  extensionHint: string | null;
}

interface LoadRequest {
  type: "load";
  bytes: Uint8Array;
  extensionHint: string | null;
  /// The output's shape, fixed for the session by the AudioContext.
  outRate: number;
  outChannels: number;
}

/// Queues the track to play after the current one, replacing whatever was
/// queued before. `bytes: null` clears the queue.
interface QueueRequest {
  type: "queue";
  bytes: Uint8Array | null;
  extensionHint: string | null;
}

/// Nudges the pump to run now rather than at the next tick -- sent after a
/// seek so it isn't waiting out a timer before responding.
interface WakeRequest {
  type: "wake";
}

type Request = LoadRequest | QueueRequest | WakeRequest;

interface LoadedResponse {
  type: "loaded";
  sab: SharedArrayBuffer;
  channels: number;
  sampleRate: number;
  durationSeconds: number;
  /// Centre frequency of each spectrum band, computed in Rust and fixed for
  /// the session -- sent once here rather than living in the shared buffer,
  /// since it never changes.
  bandCentres: Float32Array;
}

interface ErrorResponse {
  type: "error";
  message: string;
}

let wasmReady: Promise<unknown> | null = null;
let decoder: WasmDecoder | null = null;
let views: ReturnType<typeof audioViews> | null = null;
/// The meters, and this worker's read position in the tap the worklet writes.
/// Analysis lives here rather than in the worklet because the worklet is the
/// audio thread: a meter that overruns must go stale, not silent (M4).
let analyzer: WasmAnalyzer | null = null;
let tapCursor = 0;
let lastAnalysis = 0;
/// The output's rate -- what every track is resampled to and what all the
/// frame counts in the control block are measured in.
let outRate = 44_100;
let outChannels = 2;
/// The seek this worker has already acted on.
let seq = 0;
let timer: ReturnType<typeof setTimeout> | null = null;

/// The track being decoded, and the one queued behind it.
let current: Source | null = null;
let queued: Source | null = null;
/// Which track the decoder is on, against the worklet's `trackIndex`, which is
/// the one being heard. They differ while a hand-off sits undelivered in the
/// ring.
let decodingIndex = 0;
/// The track still being heard after the decoder has moved past it. Held only
/// until its successor's boundary lands, and only so a seek arriving in that
/// window can rewind to it.
let previous: { index: number; source: Source } | null = null;

self.onmessage = async (e: MessageEvent<Request>) => {
  if (e.data.type === "wake") {
    pump();
    return;
  }
  if (e.data.type === "queue") {
    queued = e.data.bytes
      ? { bytes: e.data.bytes, extensionHint: e.data.extensionHint }
      : null;
    // A track queued while the decoder is parked at the end of the last one
    // has to wake it, not wait for a seek.
    pump();
    return;
  }
  if (e.data.type !== "load") return;

  try {
    wasmReady ??= init();
    await wasmReady;

    outRate = e.data.outRate;
    outChannels = e.data.outChannels;
    current = { bytes: e.data.bytes, extensionHint: e.data.extensionHint };
    queued = null;
    previous = null;
    decodingIndex = 0;

    decoder?.free();
    decoder = openTrack(current);
    const durationSeconds = decoder.duration_seconds;

    const sab = createAudioSab(outChannels, outRate);
    views = audioViews(sab);
    analyzer?.free();
    analyzer = WasmAnalyzer.create(outRate, outChannels);
    tapCursor = 0;
    lastAnalysis = performance.now();
    // 0 means "the container didn't say" and travels all the way to the UI --
    // see StreamingDecoder::duration_seconds for when that happens.
    views.control[CONTROL.totalFrames] = Math.round(durationSeconds * outRate);
    seq = 0;

    const response: LoadedResponse = {
      type: "loaded",
      sab,
      channels: outChannels,
      sampleRate: outRate,
      durationSeconds,
      bandCentres: analyzer.bandCentres(),
    };
    self.postMessage(response);
    pump();
  } catch (err) {
    const response: ErrorResponse = { type: "error", message: String(err) };
    self.postMessage(response);
  }
};

function openTrack(source: Source): WasmDecoder {
  return WasmDecoder.create(
    source.bytes,
    source.extensionHint ?? undefined,
    outRate,
    outChannels,
  );
}

/// One turn of decode-into-the-ring, then reschedule.
function pump(): void {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  if (!decoder || !views) return;
  const { control, ring, ringFrames, channels } = views;

  // First, and outside the decode paths below: several of them return early
  // (mid-seek, waiting on the drain), and the meters should keep decaying
  // through a seek rather than freezing until decoding resumes.
  runAnalysis();

  try {
    // The listener has caught up; the old track can go.
    if (previous && Atomics.load(control, CONTROL.trackIndex) >= decodingIndex) {
      previous = null;
    }

    // Step 2 of the handshake (see audioSab.ts): move the decoder, then say so
    // *before* writing anything. Until the worklet acknowledges, everything in
    // the ring is from before the seek and nothing new is on its way, which is
    // what makes it safe for the worklet to throw all of it away.
    const requested = Atomics.load(control, CONTROL.seekSeq);
    if (requested !== seq) {
      // The decoder runs up to a ring ahead, so near the end of a track it has
      // already moved on to the queued one while the listener is still hearing
      // this one. Seeking what is being *decoded* would scrub a song the user
      // cannot hear yet -- and the seek bar they dragged belongs to the one
      // they can. Rewind, handing the upcoming track back to the queue so
      // gapless still works after the seek lands.
      if (previous && current) {
        // Open before freeing: a track that won't reopen should leave the
        // decoder we already have rather than nothing at all.
        const rewound = openTrack(previous.source);
        queued = current;
        current = previous.source;
        decodingIndex = previous.index;
        previous = null;
        decoder.free();
        decoder = rewound;
      }
      const target = Atomics.load(control, CONTROL.seekTargetFrames) / outRate;
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
        if (decoder.is_finished && !handOff(control)) {
          Atomics.store(control, CONTROL.eos, 1);
        }
        break;
      }

      const frames = written / channels;
      Atomics.store(control, CONTROL.writeIndex, (writeIndex + frames) % ringFrames);
      Atomics.add(control, CONTROL.filledFrames, frames);
      Atomics.add(control, CONTROL.writtenFrames, frames);
      budget -= written;
    }
  } catch (err) {
    const response: ErrorResponse = { type: "error", message: String(err) };
    self.postMessage(response);
    return;
  }

  schedule();
}

/// Drains the tap into the meters and publishes what they read.
///
/// Rides the decode pump rather than having a timer of its own: it runs at the
/// same ~20ms cadence, and an FFT of 2048 points costs tens of microseconds
/// against a pump budgeted for tens of milliseconds of decoding. The native
/// side gives this its own thread only because it has threads to give.
function runAnalysis(): void {
  if (!analyzer || !views) return;
  const { control, scope, spectrum } = views;

  const written = Atomics.load(control, CONTROL.tapWritten);
  let available = framesUntil(written, tapCursor);
  let discontinuity = false;

  if (available < 0) {
    // The worklet restarted the tap at zero: a seek drained the ring, so what
    // comes next is from somewhere else in the track.
    discontinuity = true;
    tapCursor = 0;
    available = written;
  }
  if (available > SCOPE_SAMPLES) {
    // Lapped -- the worklet wrote more than the ring holds since the last
    // drain. Take what survived and say so, exactly as `AnalysisTap::read`
    // does natively.
    tapCursor = written - SCOPE_SAMPLES;
    available = SCOPE_SAMPLES;
    discontinuity = true;
  }

  if (available > 0) {
    const drained = new Float32Array(available);
    for (let i = 0; i < available; i++) drained[i] = scope[(tapCursor + i) & SCOPE_MASK];
    tapCursor = written;
    analyzer.ingest(drained, discontinuity);
  } else if (discontinuity) {
    analyzer.ingest(new Float32Array(0), true);
  }

  const now = performance.now();
  const levels = analyzer.update((now - lastAnalysis) / 1000);
  lastAnalysis = now;
  spectrum.set(levels);
}

/// Crosses to the queued track, if there is one. Returns whether it did.
///
/// The outgoing track's last sample -- including the tail the resampler was
/// holding, which `WasmDecoder::read` flushes before reporting itself finished
/// -- is already in the ring by the time this runs. So the incoming track's
/// first frame goes in immediately behind it with nothing between them, and
/// `writtenFrames` right now is exactly the frame the boundary sits at.
function handOff(control: Int32Array): boolean {
  if (!queued || !current) return false;

  let opened: WasmDecoder;
  try {
    opened = openTrack(queued);
  } catch (err) {
    // A bad file ends the queue rather than stalling it: the UI sees
    // `finished` and can queue something else.
    self.postMessage({ type: "error", message: String(err) } satisfies ErrorResponse);
    queued = null;
    return false;
  }

  const at = Atomics.load(control, CONTROL.writtenFrames);
  const totalFrames = Math.round(opened.duration_seconds * outRate);
  if (!pushBoundary(control, at, totalFrames)) {
    // Only reachable with more tracks in flight than the ring holds audio
    // for. Publishing here reports the change early rather than losing it;
    // the audio is unaffected.
    Atomics.store(control, CONTROL.totalFrames, totalFrames);
    Atomics.add(control, CONTROL.trackIndex, 1);
  }

  previous = { index: decodingIndex, source: current };
  current = queued;
  queued = null;
  decodingIndex += 1;
  decoder?.free();
  decoder = opened;
  // Order matters: clearing `eos` first stops the worklet latching `finished`
  // in between.
  Atomics.store(control, CONTROL.eos, 0);
  Atomics.store(control, CONTROL.finished, 0);
  return true;
}

/// Appends a boundary record. False when the queue is full.
function pushBoundary(control: Int32Array, at: number, totalFrames: number): boolean {
  const tail = Atomics.load(control, CONTROL.boundaryTail);
  const next = (tail + 1) % BOUNDARY_SLOTS;
  if (next === Atomics.load(control, CONTROL.boundaryHead)) return false;
  const base = BOUNDARY_BASE + tail * BOUNDARY_FIELDS;
  control[base + BOUNDARY_AT] = at;
  control[base + BOUNDARY_TOTAL_FRAMES] = totalFrames;
  // Release the record only once its fields are written.
  Atomics.store(control, CONTROL.boundaryTail, next);
  return true;
}

function schedule(): void {
  timer = setTimeout(pump, PUMP_INTERVAL_MS);
}
