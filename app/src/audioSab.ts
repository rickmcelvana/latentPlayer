// The one definition of the SharedArrayBuffer the web playback path is built
// around. Imported by all three parties -- worker.ts (producer),
// worklet/player-worklet.ts (consumer), playerEngine.ts (control and
// reporting) -- because Phase 0's version was three copies of the same
// constants with a comment asking them to stay in sync, and M2 roughly
// quadruples the number of fields.
//
// Layout:
//
//   Int32Array   at 0                        CONTROL_INTS ints  (the fields below)
//   Float32Array at CONTROL_INTS*4           ringFrames*channels  interleaved PCM
//   Float32Array after that                  SCOPE_SAMPLES        recently played audio
//
// The ring is a small fixed size -- about two seconds -- rather than the whole
// track. Phase 0 and M1 allocated `frameCount * channels * 4` bytes here, i.e.
// the entire decoded file, which is the thing M2 exists to stop doing.

/// Index of each field in the Int32 control block.
export const CONTROL = {
  /// 0/1. Written by playerEngine, read by the worklet.
  playing: 0,
  /// Frames currently in the ring. The producer adds, the consumer subtracts,
  /// both atomically -- so unlike a pair of monotonic counters it can never
  /// overflow, however long the track.
  filledFrames: 1,
  /// Ring write position in frames, `0..ringFrames`. Worker only.
  writeIndex: 2,
  /// Ring read position in frames, `0..ringFrames`. Worklet only.
  readIndex: 3,
  /// The decoder has produced the last sample of the track. Not the same as
  /// `finished` -- audio can still be queued.
  eos: 4,

  // --- the seek handshake (mirrors player-core/src/playback.rs's `Shared`) ---
  //
  // A seek has to discard the audio already in the ring without discarding any
  // decoded after the seek. The producer is the only one who knows which side
  // of the seek a sample came from, the consumer is the only one who can drop
  // it, so the job passes between them:
  //
  //   1. playerEngine bumps `seekSeq`.
  //   2. The worker seeks, then sets `decoderReadySeq` *before* writing
  //      anything new -- so when the worklet sees it, the ring is all stale.
  //   3. The worklet empties the ring and sets `drainedSeq`; the worker waits
  //      for that before writing again.
  //
  /// Bumped by playerEngine to request a seek.
  seekSeq: 5,
  /// Set by the worker once it has seeked and stopped producing old audio.
  decoderReadySeq: 6,
  /// Set by the worklet once the ring has been emptied.
  drainedSeq: 7,
  /// Where to seek to, in source frames. Written before `seekSeq` is bumped.
  seekTargetFrames: 8,

  /// Frames played. The authoritative position: the worklet counts what it
  /// actually sent to the output, so it can't drift from what's audible.
  /// Int32, so it wraps after 2^31 frames -- 12 hours in one track at 48kHz.
  positionFrames: 9,
  /// Track length in frames, or 0 when the container doesn't state one.
  totalFrames: 10,
  /// End of stream *and* an empty ring, i.e. the track is really over. An
  /// empty ring alone is an underrun, which must not stop the transport.
  finished: 11,

  /// Ring capacity in frames, and the track's channel count. Kept here so the
  /// worklet can size its views from the buffer itself rather than from
  /// processorOptions that could disagree with it.
  ringFrames: 12,
  channels: 13,

  /// Scope ring write position in samples, `0..SCOPE_SAMPLES`, and how much of
  /// it is valid (it stops growing once the ring has wrapped once).
  scopeIndex: 14,
  scopeFilled: 15,
} as const;

/// Rounded up past the last field so the float regions start 8-byte aligned.
export const CONTROL_INTS = 16;

/// How much audio the ring holds. Same reasoning as the native side: enough
/// that a decode hiccup doesn't reach the speakers, small enough that a seek
/// throws little away.
export const RING_SECONDS = 2;

/// Samples of playback history kept for the scope. Power of two so the wrap is
/// a mask; ~170ms of 48kHz stereo, far more than the 2048 a frame draws.
export const SCOPE_SAMPLES = 32_768;
export const SCOPE_MASK = SCOPE_SAMPLES - 1;

export interface AudioViews {
  control: Int32Array;
  /// Interleaved PCM waiting to be played.
  ring: Float32Array;
  /// Recently played interleaved samples, for the scope.
  scope: Float32Array;
  ringFrames: number;
  channels: number;
}

/// Allocates the buffer for one track. Called in the worker, which is where
/// the channel count and sample rate are first known.
export function createAudioSab(channels: number, sampleRate: number): SharedArrayBuffer {
  const ringFrames = Math.ceil(sampleRate * RING_SECONDS);
  const bytes = CONTROL_INTS * 4 + ringFrames * channels * 4 + SCOPE_SAMPLES * 4;
  const sab = new SharedArrayBuffer(bytes);
  const control = new Int32Array(sab, 0, CONTROL_INTS);
  control[CONTROL.ringFrames] = ringFrames;
  control[CONTROL.channels] = channels;
  return sab;
}

/// Reconstructs the typed-array views over an existing buffer. `ringFrames`
/// and `channels` come out of the control block, so every party sees the same
/// geometry by construction.
export function audioViews(sab: SharedArrayBuffer): AudioViews {
  const control = new Int32Array(sab, 0, CONTROL_INTS);
  const ringFrames = control[CONTROL.ringFrames];
  const channels = control[CONTROL.channels];
  const ringSamples = ringFrames * channels;
  const ring = new Float32Array(sab, CONTROL_INTS * 4, ringSamples);
  const scope = new Float32Array(sab, CONTROL_INTS * 4 + ringSamples * 4, SCOPE_SAMPLES);
  return { control, ring, scope, ringFrames, channels };
}
