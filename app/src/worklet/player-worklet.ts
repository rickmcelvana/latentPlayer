/// <reference lib="dom" />
// NOTE: this file is NOT loaded directly -- an AudioWorklet can only load a
// plain URL, so playerEngine.ts addModule()s the esbuild bundle at
// public/worklet/player-worklet.js, which is gitignored and generated (see
// package.json's `worklet` script and predev/prebuild hooks). Editing this
// file therefore does nothing until `npm run worklet` re-runs -- same
// lesson latent-mixing already learned the hard way (see its
// mixer-worklet.ts's own comment), so predev/prebuild rebuild it
// automatically here from the start.
//
// Unlike mixer-worklet.ts, this processor makes no wasm calls at all. It reads
// already-decoded interleaved f32 PCM out of a ring in a SharedArrayBuffer that
// worker.ts keeps topped up, and copies it to the output. Since M3 that PCM is
// already in the output's rate and channel count, so it really is a copy. See
// audioSab.ts for the layout, which this shares with the worker and
// playerEngine.
//
// Since M8 there is one piece of DSP here: the EQ. Its *coefficients* are still
// designed in Rust, in the worker, and arrive through the shared buffer -- what
// happens here is the difference equation and nothing else (see eqFilter.ts for
// why the line is drawn there). It has to be here rather than in the worker
// because the worker writes a ring holding two seconds of audio, and an EQ a
// listener cannot hear move is not an EQ.
//
// Its responsibilities beyond copying samples: it is the consumer end of the
// seek handshake, it owns the authoritative play position, it applies track
// changes as they reach the output, and it writes the scope's history --
// because it is the only party that knows what actually reached the output.

import {
  BOUNDARY_AT,
  BOUNDARY_BASE,
  BOUNDARY_FIELDS,
  BOUNDARY_SLOTS,
  BOUNDARY_TOTAL_FRAMES,
  CONTROL,
  EQ_FIELDS,
  SCOPE_MASK,
  audioViews,
  framesUntil,
} from "../audioSab";
import { EqFilter } from "../eqFilter";

const BLOCK_FRAMES = 128;

class PlayerWorklet extends AudioWorkletProcessor {
  private control: Int32Array;
  private ring: Float32Array;
  private scope: Float32Array;
  private ringFrames: number;
  private channels: number;
  /// The EQ, and the coefficients the worker publishes for it.
  private eqCoefficients: Float64Array;
  private eq: EqFilter;
  /// The generation of the cascade currently loaded, against
  /// `CONTROL.eqGeneration`.
  private eqGeneration = 0;
  /// One frame and one cascade, both reused: the worklet may not allocate on
  /// the audio path any more than the native callback may.
  private frame: Float64Array;
  private eqSnapshot = new Float64Array(EQ_FIELDS);

  // `any` here matches the sibling repos' own worklet convention
  // (CONVENTIONS.md): this is a separate esbuild bundle with ambient-typed
  // globals, and `processorOptions` is untyped at the message boundary.
  constructor(options: any) {
    super();
    const { sab } = options.processorOptions as { sab: SharedArrayBuffer };
    const views = audioViews(sab);
    this.control = views.control;
    this.ring = views.ring;
    this.scope = views.scope;
    this.ringFrames = views.ringFrames;
    this.channels = views.channels;
    this.eqCoefficients = views.eq;
    this.eq = new EqFilter(views.channels);
    this.frame = new Float64Array(views.channels);
  }

  /// Picks up a new cascade if the worker has published one.
  ///
  /// The seqlock: read the generation, copy, read it again. If it moved during
  /// the copy the coefficients may be half of one curve and half of another, so
  /// the copy is abandoned and retried next block -- 2.7ms later, which nobody
  /// can hear, and far better than running a cascade that was never designed.
  private refreshEq(): void {
    const published = Atomics.load(this.control, CONTROL.eqGeneration);
    if (published === this.eqGeneration) return;
    this.eqSnapshot.set(this.eqCoefficients);
    if (Atomics.load(this.control, CONTROL.eqGeneration) !== published) return;
    this.eq.set(this.eqSnapshot);
    this.eqGeneration = published;
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const output = outputs[0];
    const c = this.control;

    // Before any of the early returns below, for the reason the native callback
    // drains its own queue there: a listener who moves a slider while paused,
    // or mid-seek, must have the EQ they asked for when the music comes back.
    this.refreshEq();

    // Step 3 of the handshake (see audioSab.ts). Once the worker says it has
    // moved, everything queued is from where the track no longer is.
    const requested = Atomics.load(c, CONTROL.seekSeq);
    if (requested !== Atomics.load(c, CONTROL.drainedSeq)) {
      if (Atomics.load(c, CONTROL.decoderReadySeq) === requested) {
        const stale = Atomics.load(c, CONTROL.filledFrames);
        Atomics.store(c, CONTROL.readIndex, Atomics.load(c, CONTROL.writeIndex));
        Atomics.store(c, CONTROL.filledFrames, 0);
        // Count what was discarded, or every boundary after this one lands
        // `stale` frames late.
        Atomics.add(c, CONTROL.consumedFrames, stale);
        // Any hand-off queued behind the drain describes audio that no longer
        // exists. The worker is blocked until `drainedSeq` below, so nothing
        // new can arrive here.
        Atomics.store(c, CONTROL.boundaryHead, Atomics.load(c, CONTROL.boundaryTail));
        Atomics.store(c, CONTROL.positionFrames, Atomics.load(c, CONTROL.seekTargetFrames));
        Atomics.store(c, CONTROL.finished, 0);
        // Otherwise the next scope frame is spliced from both sides of the
        // seek. Restarting the tap at zero -- index and count together, as
        // `AnalysisTap::clear` does natively -- is also what tells the worker's
        // meters that what follows is from somewhere else in the track.
        Atomics.store(c, CONTROL.scopeFilled, 0);
        Atomics.store(c, CONTROL.scopeIndex, 0);
        Atomics.store(c, CONTROL.tapWritten, 0);
        // The filters' memory belongs to where the track was, not where it is
        // going -- same reason, same place, as the native callback's reset.
        this.eq.reset();
        Atomics.store(c, CONTROL.drainedSeq, requested);
      }
      // Either way, play nothing this block.
      this.silence(output);
      return true;
    }

    if (Atomics.load(c, CONTROL.playing) !== 1) {
      this.silence(output);
      // Paused audio is still audio the output received, and the tap holds
      // what the output received -- same rule, same reason, as the native
      // callback. Without it the meters freeze on the last frame before the
      // pause and hold it forever.
      this.tapSilence();
      return true;
    }

    const available = Atomics.load(c, CONTROL.filledFrames);
    const frames = Math.min(BLOCK_FRAMES, available);
    const readIndex = Atomics.load(c, CONTROL.readIndex);
    let scopeIndex = Atomics.load(c, CONTROL.scopeIndex);

    for (let frame = 0; frame < BLOCK_FRAMES; frame++) {
      // Short of a full block means end of the queue, or the worker falling
      // behind. Silence either way; the position only counts real frames, so
      // an underrun stalls rather than skipping ahead.
      //
      // The silence goes into the scope as well as the output. Recording only
      // what *arrived* left the trace frozen on the last good audio through a
      // dropout -- hiding it at the one moment the scope would have shown it
      // (found on the native side, 2026-08-15; same bug lived here).
      if (frame >= frames) {
        for (let ch = 0; ch < output.length; ch++) output[ch][frame] = 0;
        for (let s = 0; s < this.channels; s++) {
          this.scope[(scopeIndex + s) & SCOPE_MASK] = 0;
        }
        scopeIndex += this.channels;
        continue;
      }
      const base = ((readIndex + frame) % this.ringFrames) * this.channels;
      // The ring is already in the output's channel layout, adapted in the
      // worker (M3), so these indices line up one to one. What happens between
      // the ring and the output is the EQ and nothing else -- volume rides the
      // GainNode after this node, which is why the tap below holds EQ'd audio
      // at the monitoring level it was measured at (M8 decision 2).
      if (this.eq.bypassed) {
        for (let ch = 0; ch < output.length; ch++) {
          output[ch][frame] = this.ring[base + ch] ?? 0;
        }
        for (let s = 0; s < this.channels; s++) {
          this.scope[(scopeIndex + s) & SCOPE_MASK] = this.ring[base + s];
        }
      } else {
        for (let s = 0; s < this.channels; s++) this.frame[s] = this.ring[base + s];
        this.eq.process(this.frame);
        for (let ch = 0; ch < output.length; ch++) {
          output[ch][frame] = this.frame[ch] ?? 0;
        }
        for (let s = 0; s < this.channels; s++) {
          this.scope[(scopeIndex + s) & SCOPE_MASK] = this.frame[s];
        }
      }
      scopeIndex += this.channels;
    }

    Atomics.store(c, CONTROL.scopeIndex, scopeIndex & SCOPE_MASK);
    Atomics.store(
      c,
      CONTROL.scopeFilled,
      Math.min(SCOPE_MASK + 1, Atomics.load(c, CONTROL.scopeFilled) + BLOCK_FRAMES * this.channels),
    );
    // Published last, and only after the samples above are in place: a reader
    // that sees this count must be able to read every sample it counts.
    Atomics.add(c, CONTROL.tapWritten, BLOCK_FRAMES * this.channels);

    if (frames > 0) {
      Atomics.store(c, CONTROL.readIndex, (readIndex + frames) % this.ringFrames);
      Atomics.add(c, CONTROL.filledFrames, -frames);
      Atomics.add(c, CONTROL.positionFrames, frames);
      Atomics.add(c, CONTROL.consumedFrames, frames);
    }

    this.applyBoundaries();

    // Finished only when the queue has run out *and* nothing is left to play.
    // Either alone is an ordinary mid-track state, and a track ending with
    // another queued behind it is a hand-off, which is not an ending at all.
    if (Atomics.load(c, CONTROL.eos) === 1 && Atomics.load(c, CONTROL.filledFrames) === 0) {
      Atomics.store(c, CONTROL.finished, 1);
    }

    return true;
  }

  /// Track changes land here rather than when the worker opened the file:
  /// this is the moment the new track's first frame reaches the output.
  /// Noticing it a block late costs nothing, because the position is derived
  /// from how far past the boundary we are rather than from when we looked. A
  /// loop, since a track shorter than one block would be crossed within it.
  private applyBoundaries(): void {
    const c = this.control;
    const consumed = Atomics.load(c, CONTROL.consumedFrames);
    for (let guard = 0; guard < BOUNDARY_SLOTS; guard++) {
      const head = Atomics.load(c, CONTROL.boundaryHead);
      if (head === Atomics.load(c, CONTROL.boundaryTail)) return;
      const base = BOUNDARY_BASE + head * BOUNDARY_FIELDS;
      const at = c[base + BOUNDARY_AT];
      if (framesUntil(at, consumed) > 0) return;

      Atomics.store(c, CONTROL.positionFrames, framesUntil(consumed, at));
      Atomics.store(c, CONTROL.totalFrames, c[base + BOUNDARY_TOTAL_FRAMES]);
      // A hand-off is the opposite of an ending, whatever may have been
      // latched while the ring ran dry across the seam.
      Atomics.store(c, CONTROL.finished, 0);
      Atomics.store(c, CONTROL.boundaryHead, (head + 1) % BOUNDARY_SLOTS);
      Atomics.add(c, CONTROL.trackIndex, 1);
    }
  }

  private silence(output: Float32Array[]): void {
    for (let ch = 0; ch < output.length; ch++) output[ch].fill(0);
  }

  /// One block of silence into the tap, for the paths that send the output
  /// nothing. `AnalysisTap::push_silence` natively.
  private tapSilence(): void {
    const c = this.control;
    const samples = BLOCK_FRAMES * this.channels;
    let index = Atomics.load(c, CONTROL.scopeIndex);
    for (let s = 0; s < samples; s++) this.scope[(index + s) & SCOPE_MASK] = 0;
    index = (index + samples) & SCOPE_MASK;
    Atomics.store(c, CONTROL.scopeIndex, index);
    Atomics.store(
      c,
      CONTROL.scopeFilled,
      Math.min(SCOPE_MASK + 1, Atomics.load(c, CONTROL.scopeFilled) + samples),
    );
    Atomics.add(c, CONTROL.tapWritten, samples);
  }
}

registerProcessor("player-worklet", PlayerWorklet);
