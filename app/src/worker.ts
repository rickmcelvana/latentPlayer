// Web Worker: owns the decode step, off the main thread (same rule the
// sibling repos follow for anything CPU-heavy -- decode.rs's whole-file
// decode is not free for a multi-minute track). Also owns allocating the
// SharedArrayBuffer, since it already has the decoded frame count in hand
// right after decoding and there's no reason to round-trip that back to
// the main thread just to allocate there instead.
//
// SAB layout (shared with player-worklet.ts and playerEngine.ts -- all
// three must agree on this):
//   Int32Array,  offset 0,             length CONTROL_INTS (3):
//     [0] playing (0/1), [1] cursorFrames, [2] totalFrames
//   Float32Array, offset CONTROL_INTS*4, length totalFrames*channels:
//     interleaved decoded PCM
import init, { decode } from "./wasm/player_rt_wasm.js";

const CONTROL_INTS = 3;

interface DecodeRequest {
  type: "decode";
  bytes: Uint8Array;
  extensionHint: string | null;
}

interface DecodedResponse {
  type: "decoded";
  sab: SharedArrayBuffer;
  channels: number;
  sampleRate: number;
  frameCount: number;
}

interface ErrorResponse {
  type: "error";
  message: string;
}

let wasmReady: Promise<unknown> | null = null;

self.onmessage = async (e: MessageEvent<DecodeRequest>) => {
  if (e.data.type !== "decode") return;
  try {
    wasmReady ??= init();
    await wasmReady;

    const track = decode(e.data.bytes, e.data.extensionHint ?? undefined);
    const channels = track.channels;
    const frameCount = track.frame_count;

    const sab = new SharedArrayBuffer(CONTROL_INTS * 4 + frameCount * channels * 4);
    const control = new Int32Array(sab, 0, CONTROL_INTS);
    control[0] = 0; // paused
    control[1] = 0; // cursor
    control[2] = frameCount;

    const data = new Float32Array(sab, CONTROL_INTS * 4);
    track.copy_into(data, 0);
    const sampleRate = track.sample_rate;
    track.free(); // wasm-bindgen object -- must read fields before freeing

    const response: DecodedResponse = { type: "decoded", sab, channels, sampleRate, frameCount };
    self.postMessage(response);
  } catch (err) {
    const response: ErrorResponse = { type: "error", message: String(err) };
    self.postMessage(response);
  }
};
