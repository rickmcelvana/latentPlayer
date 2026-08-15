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
// Unlike mixer-worklet.ts, this processor makes no wasm calls at all: a
// plain player has no per-block DSP to run, so there's nothing for wasm to
// do here. It reads already-decoded interleaved f32 PCM out of a ring in a
// SharedArrayBuffer that worker.ts keeps topped up, and copies it to the
// output. See audioSab.ts for the layout, which this shares with the worker
// and playerEngine.
//
// Its M2 responsibilities beyond copying samples: it is the consumer end of
// the seek handshake, it owns the authoritative play position, and it writes
// the scope's history -- because it is the only party that knows what actually
// reached the output.

import { CONTROL, SCOPE_MASK, audioViews } from "../audioSab";

const BLOCK_FRAMES = 128;

class PlayerWorklet extends AudioWorkletProcessor {
  private control: Int32Array;
  private ring: Float32Array;
  private scope: Float32Array;
  private ringFrames: number;
  private channels: number;

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
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const output = outputs[0];
    const c = this.control;

    // Step 3 of the handshake (see audioSab.ts). Once the worker says it has
    // moved, everything queued is from where the track no longer is.
    const requested = Atomics.load(c, CONTROL.seekSeq);
    if (requested !== Atomics.load(c, CONTROL.drainedSeq)) {
      if (Atomics.load(c, CONTROL.decoderReadySeq) === requested) {
        Atomics.store(c, CONTROL.readIndex, Atomics.load(c, CONTROL.writeIndex));
        Atomics.store(c, CONTROL.filledFrames, 0);
        Atomics.store(c, CONTROL.positionFrames, Atomics.load(c, CONTROL.seekTargetFrames));
        Atomics.store(c, CONTROL.finished, 0);
        // Otherwise the next scope frame is spliced from both sides of the seek.
        Atomics.store(c, CONTROL.scopeFilled, 0);
        Atomics.store(c, CONTROL.drainedSeq, requested);
      }
      // Either way, play nothing this block.
      this.silence(output);
      return true;
    }

    if (Atomics.load(c, CONTROL.playing) !== 1) {
      this.silence(output);
      return true;
    }

    const available = Atomics.load(c, CONTROL.filledFrames);
    const frames = Math.min(BLOCK_FRAMES, available);
    const readIndex = Atomics.load(c, CONTROL.readIndex);
    let scopeIndex = Atomics.load(c, CONTROL.scopeIndex);

    for (let frame = 0; frame < BLOCK_FRAMES; frame++) {
      // Short of a full block means end of track, or the worker falling
      // behind. Silence either way; the position only counts real frames, so
      // an underrun stalls rather than skipping ahead.
      if (frame >= frames) {
        for (let ch = 0; ch < output.length; ch++) output[ch][frame] = 0;
        continue;
      }
      const base = ((readIndex + frame) % this.ringFrames) * this.channels;
      for (let ch = 0; ch < output.length; ch++) {
        // Simple channel adaptation, same rule as the native path
        // (player-core/src/playback.rs): reuse source channels cyclically.
        output[ch][frame] = this.ring[base + (ch % this.channels)] ?? 0;
      }
      // The scope gets what was played, in the source's own channel layout, so
      // a stereo track gives it real L/R pairs to plot against each other.
      for (let s = 0; s < this.channels; s++) {
        this.scope[(scopeIndex + s) & SCOPE_MASK] = this.ring[base + s];
      }
      scopeIndex += this.channels;
    }

    if (frames > 0) {
      Atomics.store(c, CONTROL.readIndex, (readIndex + frames) % this.ringFrames);
      Atomics.add(c, CONTROL.filledFrames, -frames);
      Atomics.add(c, CONTROL.positionFrames, frames);
      const written = frames * this.channels;
      Atomics.store(c, CONTROL.scopeIndex, scopeIndex & SCOPE_MASK);
      Atomics.store(
        c,
        CONTROL.scopeFilled,
        Math.min(SCOPE_MASK + 1, Atomics.load(c, CONTROL.scopeFilled) + written),
      );
    }

    // Finished only when the decoder is done *and* nothing is left to play.
    // Either alone is an ordinary mid-track state.
    if (
      Atomics.load(c, CONTROL.eos) === 1 &&
      Atomics.load(c, CONTROL.filledFrames) === 0
    ) {
      Atomics.store(c, CONTROL.finished, 1);
    }

    return true;
  }

  private silence(output: Float32Array[]): void {
    for (let ch = 0; ch < output.length; ch++) output[ch].fill(0);
  }
}

registerProcessor("player-worklet", PlayerWorklet);
