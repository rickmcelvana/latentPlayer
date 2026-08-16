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
//
// Since M3 the ring holds audio in the *output's* shape -- the AudioContext's
// sample rate and channel count, converted in the worker -- rather than the
// file's. A queue has to be able to hand from one track to another without the
// ring's geometry changing underneath the samples already in it.

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
  /// The decoder has produced the last sample of the *queue*, with nothing
  /// behind it. Not the same as `finished` -- audio can still be queued.
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
  /// Where to seek to, in output frames. Written before `seekSeq` is bumped.
  seekTargetFrames: 8,

  /// Frames played within the current track. The authoritative position: the
  /// worklet counts what it actually sent to the output, so it can't drift
  /// from what's audible. Int32, so it wraps after 2^31 frames -- 12 hours in
  /// one track at 48kHz.
  positionFrames: 9,
  /// Current track's length in output frames, or 0 when the container doesn't
  /// state one. Republished by the worklet at each hand-off.
  totalFrames: 10,
  /// End of the queue *and* an empty ring, i.e. the music is really over. An
  /// empty ring alone is an underrun, which must not stop the transport.
  finished: 11,

  /// Ring capacity in frames, and the output channel count. Kept here so the
  /// worklet can size its views from the buffer itself rather than from
  /// processorOptions that could disagree with it.
  ringFrames: 12,
  channels: 13,

  /// Scope ring write position in samples, `0..SCOPE_SAMPLES`, and how much of
  /// it is valid (it stops growing once the ring has wrapped once).
  scopeIndex: 14,
  scopeFilled: 15,

  // --- the gapless hand-off (mirrors `Boundary` in playback.rs) ---
  //
  // Track changes travel with the audio rather than being announced out of
  // band, because the only honest answer to "which track is playing" is
  // "whichever one the frame the output just took came from". The worker runs
  // up to a ring ahead, so anything it published directly would be up to two
  // seconds early -- the title would change while the previous song was still
  // playing.
  //
  /// Output frames the worker has ever written, and the worklet has ever
  /// taken (including frames a seek threw away, or every boundary after one
  /// would land late). Both wrap at 2^31; see `framesUntil` below for why
  /// that is fine.
  writtenFrames: 16,
  consumedFrames: 17,
  /// Hand-offs that have reached the output. Not an index into any playlist --
  /// the engine only ever knows two tracks -- just a counter the UI can watch
  /// to know the music moved on.
  trackIndex: 18,
  /// Boundary queue read/write slots, `0..BOUNDARY_SLOTS`. Worklet owns the
  /// head, worker owns the tail; empty when they are equal.
  boundaryHead: 19,
  boundaryTail: 20,

  /// Total samples ever written to the scope/analysis ring, which is what
  /// makes that ring a *tap*: `scopeIndex` alone says where the writer is, not
  /// how much a reader has missed. The worker's analysis cursor compares
  /// against this to know when it has been lapped -- mirrors the `written`
  /// count on `AnalysisTap` natively (player-core/src/tap.rs). Int32 and so
  /// wrapping; compared through `framesUntil` for the same reason
  /// `writtenFrames` is.
  ///
  /// Reset to 0 by the worklet when a seek drains the ring, which is how a
  /// reader learns the audio it is about to get does not continue what it had.
  tapWritten: 21,

  /// Goniometer points currently valid, of `GONIOMETER_POINTS`. Fewer than the
  /// full count at the start of a track, where there isn't yet a window's worth
  /// of audio to plot -- drawing the whole buffer then would draw the zeros
  /// behind it as a dot in the centre.
  goniometerPoints: 22,

  /// Bumped by the worker after it writes a new set of EQ coefficients (M8).
  ///
  /// The worklet keeps its own copy of the cascade and refreshes it only when
  /// this changes, which is what keeps a fifty-double read off the audio path
  /// in the steady state. It is also the seqlock that makes the read safe: the
  /// worklet re-reads this after copying and tries again next block if it moved
  /// underneath, so a slider moved mid-copy can never leave half of one curve
  /// and half of another running.
  eqGeneration: 23,
} as const;

/// Where the boundary records start, and how many there can be at once. One
/// pending hand-off is the realistic maximum -- the UI only queues a single
/// track ahead -- so four is slack for tracks shorter than the ring.
export const BOUNDARY_BASE = 24;
export const BOUNDARY_SLOTS = 4;
/// Per record: the value of `writtenFrames` at the incoming track's first
/// frame, and that track's length in output frames.
export const BOUNDARY_FIELDS = 2;
export const BOUNDARY_AT = 0;
export const BOUNDARY_TOTAL_FRAMES = 1;

/// Rounded up past the last field so the float regions start 8-byte aligned.
export const CONTROL_INTS = BOUNDARY_BASE + BOUNDARY_SLOTS * BOUNDARY_FIELDS;

/// Signed 32-bit difference between two frame counters, correct across the
/// wrap. `writtenFrames`/`consumedFrames` count forever and so must wrap, but
/// they are only ever compared when within a ring of each other (~2s), which
/// is nowhere near 2^31 apart -- so forcing the subtraction back into int32 is
/// exact. `a` is at or past `b` when this is <= 0.
export function framesUntil(a: number, b: number): number {
  return (a - b) | 0;
}

/// How much audio the ring holds. Same reasoning as the native side: enough
/// that a decode hiccup doesn't reach the speakers, small enough that a seek
/// throws little away.
export const RING_SECONDS = 2;

/// Samples of playback history kept for the scope. Power of two so the wrap is
/// a mask; ~170ms of 48kHz stereo, far more than the 2048 a frame draws.
export const SCOPE_SAMPLES = 32_768;
export const SCOPE_MASK = SCOPE_SAMPLES - 1;

/// Display bands the spectrum publishes, mirroring `BAND_COUNT` in
/// player-core/src/analysis.rs. The count has to be known here to size the
/// buffer; the *values* are computed in Rust and only ever read from here.
export const SPECTRUM_BANDS = 64;

/// What an unmeasured band reads, mirroring `FLOOR_DB` in
/// player-core/src/spectrum.rs. Duplicated deliberately and minimally: the
/// buffer has to start at "nothing here" rather than at 0, which as a dBFS
/// value would mean full scale.
export const SPECTRUM_FLOOR_DB = -120;

/// Where each of the loudness/phase numbers sits in the meter region, mirroring
/// `MeterSnapshot::to_array` in player-core/src/analysis.rs. The worker copies
/// what Rust returns straight in, so this is the one place the order is named
/// on this side.
export const METER = {
  momentaryLufs: 0,
  shortTermLufs: 1,
  integratedLufs: 2,
  truePeakDb: 3,
  correlation: 4,
} as const;
export const METER_FIELDS = 5;

/// What a meter that has measured nothing reads, mirroring `FLOOR_LUFS` and
/// `FLOOR_DBTP` in player-core/src/loudness.rs. Finite rather than -Infinity
/// for the reason M5 decision 5 gives -- the desktop shell's JSON boundary
/// turns a non-finite float into `null`, and both shells report the same thing.
export const LOUDNESS_FLOOR_LUFS = -70;
export const TRUE_PEAK_FLOOR_DB = -120;

/// Goniometer points the analyser publishes per frame, mirroring
/// `GONIOMETER_POINTS` in player-core/src/analysis.rs. Two floats each.
export const GONIOMETER_POINTS = 256;

/// The EQ's cascade as the worker writes it and the worklet reads it (M8),
/// mirroring `EqCoefficients::to_array` in player-core/src/eq.rs -- which is
/// the one place the order is decided. Doubles, not floats: the 31Hz band's
/// poles sit close enough to the unit circle at a 96kHz output rate that f32
/// coefficients lose the difference between them.
export const EQ_BANDS = 10;
export const EQ_ENABLED = 0;
export const EQ_PREAMP = 1;
export const EQ_HEADER = 2;
/// Five coefficients per band, after the two header fields.
export const EQ_FIELDS = EQ_HEADER + EQ_BANDS * 5;

/// How far a slider may travel either way, mirroring `MAX_GAIN_DB`. Only used
/// to size a control before the engine has answered; the engine's own clamp is
/// what actually holds.
export const EQ_MAX_GAIN_DB = 12;

export interface AudioViews {
  control: Int32Array;
  /// Interleaved PCM waiting to be played, in the output's shape.
  ring: Float32Array;
  /// Recently played interleaved samples: the scope draws it, the worker's
  /// meters read it. One tap, two kinds of reader (see CONTROL.tapWritten).
  scope: Float32Array;
  /// Latest spectrum in dBFS, one per band, written by the worker's analyser.
  spectrum: Float32Array;
  /// Loudness and phase, in `METER`'s order.
  meters: Float32Array;
  /// Goniometer points, interleaved side/mid. Only the first
  /// `CONTROL.goniometerPoints` pairs are valid.
  goniometer: Float32Array;
  /// The EQ's cascade, designed in Rust by the worker and applied by the
  /// worklet. Read through `CONTROL.eqGeneration`, never directly.
  eq: Float64Array;
  ringFrames: number;
  channels: number;
}

/// Byte offset of the EQ region, given everything before it. Rounded up to
/// eight because a `Float64Array` view must start on an 8-byte boundary, and
/// `METER_FIELDS` is odd -- so without this the region's start depends on the
/// meter count, which is exactly the sort of thing that works until someone
/// adds a meter.
function eqOffset(after: number): number {
  return Math.ceil(after / 8) * 8;
}

/// Allocates the buffer for a session. `channels` and `sampleRate` are the
/// *output's* -- fixed for as long as the AudioContext lives, so the same
/// buffer geometry serves every track in the queue.
export function createAudioSab(channels: number, sampleRate: number): SharedArrayBuffer {
  const ringFrames = Math.ceil(sampleRate * RING_SECONDS);
  const bytes =
    eqOffset(
      CONTROL_INTS * 4 +
        ringFrames * channels * 4 +
        SCOPE_SAMPLES * 4 +
        SPECTRUM_BANDS * 4 +
        METER_FIELDS * 4 +
        GONIOMETER_POINTS * 2 * 4,
    ) +
    EQ_FIELDS * 8;
  const sab = new SharedArrayBuffer(bytes);
  const control = new Int32Array(sab, 0, CONTROL_INTS);
  control[CONTROL.ringFrames] = ringFrames;
  control[CONTROL.channels] = channels;
  // Zeroed, these would read as full scale on every band and 0 LUFS on every
  // meter until the first analysis frame lands.
  const views = audioViews(sab);
  views.spectrum.fill(SPECTRUM_FLOOR_DB);
  views.meters.fill(LOUDNESS_FLOOR_LUFS);
  views.meters[METER.truePeakDb] = TRUE_PEAK_FLOOR_DB;
  views.meters[METER.correlation] = 0;
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
  const ringBytes = CONTROL_INTS * 4 + ringSamples * 4;
  const ring = new Float32Array(sab, CONTROL_INTS * 4, ringSamples);
  const scope = new Float32Array(sab, ringBytes, SCOPE_SAMPLES);
  const spectrumBytes = ringBytes + SCOPE_SAMPLES * 4;
  const spectrum = new Float32Array(sab, spectrumBytes, SPECTRUM_BANDS);
  const meterBytes = spectrumBytes + SPECTRUM_BANDS * 4;
  const meters = new Float32Array(sab, meterBytes, METER_FIELDS);
  const goniometerBytes = meterBytes + METER_FIELDS * 4;
  const goniometer = new Float32Array(sab, goniometerBytes, GONIOMETER_POINTS * 2);
  const eq = new Float64Array(
    sab,
    eqOffset(goniometerBytes + GONIOMETER_POINTS * 2 * 4),
    EQ_FIELDS,
  );
  return { control, ring, scope, spectrum, meters, goniometer, eq, ringFrames, channels };
}
