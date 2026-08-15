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
// do here. All the decode work already happened once, up front, in
// worker.ts -- this just reads already-decoded interleaved f32 PCM
// straight out of a SharedArrayBuffer and copies it to the output. See
// playerEngine.ts for the buffer layout this shares.

const BLOCK_FRAMES = 128;
const CONTROL_INTS = 3; // [playing, cursorFrames, totalFrames]

class PlayerWorklet extends AudioWorkletProcessor {
  private control: Int32Array;
  private data: Float32Array;
  private channels: number;

  // `any` here matches the sibling repos' own worklet convention
  // (CONVENTIONS.md): this is a separate esbuild bundle with ambient-typed
  // globals, and `processorOptions` is untyped at the message boundary.
  constructor(options: any) {
    super();
    const { sab, channels } = options.processorOptions as { sab: SharedArrayBuffer; channels: number };
    this.channels = channels;
    this.control = new Int32Array(sab, 0, CONTROL_INTS);
    this.data = new Float32Array(sab, CONTROL_INTS * 4);
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const output = outputs[0];
    const playing = Atomics.load(this.control, 0) === 1;
    const totalFrames = Atomics.load(this.control, 2);
    let cursor = Atomics.load(this.control, 1);

    for (let frame = 0; frame < BLOCK_FRAMES; frame++) {
      const silent = !playing || cursor >= totalFrames;
      for (let ch = 0; ch < output.length; ch++) {
        const channel = output[ch];
        if (silent) {
          channel[frame] = 0;
          continue;
        }
        // Simple channel adaptation, same rule as the native path
        // (player-core/src/playback.rs): reuse source channels cyclically.
        const srcIdx = cursor * this.channels + (ch % this.channels);
        channel[frame] = this.data[srcIdx] ?? 0;
      }
      if (!silent) cursor++;
    }

    Atomics.store(this.control, 1, cursor);
    return true;
  }
}

registerProcessor("player-worklet", PlayerWorklet);
