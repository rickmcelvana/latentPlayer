// The EQ's cascade, applied (M8). The web build's audio path is the
// AudioWorklet, and an AudioWorklet is TypeScript -- so this is the one piece
// of DSP in the player that exists in two languages.
//
// It is deliberately the *smallest* such piece. Everything that could be
// decided differently by two implementations -- what a slider position means,
// each band's shape and Q, where the centres are, how the preamp folds in, what
// curve the UI draws -- stays in player-core/src/eq.rs and arrives here as
// finished coefficients through the shared buffer. What is left is one direct
// form I difference equation, the same five multiply-adds `BiquadState::process`
// runs, and `/eq.html` holds the two to each other sample for sample on the same
// input.
//
// Doubles throughout, which JavaScript gives for free and which matches the f64
// the Rust side keeps for the 31Hz band's sake.
import { EQ_BANDS, EQ_ENABLED, EQ_FIELDS, EQ_HEADER, EQ_PREAMP } from "./audioSab";

/// Coefficients per band in the flat layout: b0, b1, b2, a1, a2.
const COEFFICIENTS_PER_BAND = 5;

/// State per band per channel: x1, x2, y1, y2.
const STATE_PER_BAND = 4;

/// One cascade running over interleaved audio -- the counterpart of
/// `EqProcessor` in player-core/src/eq.rs, including the channel phase it
/// carries and why.
export class EqFilter {
  private readonly channels: number;
  /// The cascade as `EqCoefficients::to_array` lays it out: enabled, preamp,
  /// then five coefficients per band.
  private readonly coefficients = new Float64Array(EQ_FIELDS);
  /// `state[(channel * EQ_BANDS + band) * 4 + n]`. One flat array rather than
  /// nested ones: this is indexed once per band per sample.
  private readonly state: Float64Array;
  /// Whether the current cascade would leave the audio exactly as it found it,
  /// so a flat EQ costs one branch per block rather than ten biquads per
  /// sample. Mirrors `EqCoefficients::is_transparent`.
  private transparent = true;

  constructor(channels: number) {
    this.channels = Math.max(1, channels);
    this.state = new Float64Array(this.channels * EQ_BANDS * STATE_PER_BAND);
    this.coefficients[EQ_PREAMP] = 1;
    for (let band = 0; band < EQ_BANDS; band++) {
      this.coefficients[EQ_HEADER + band * COEFFICIENTS_PER_BAND] = 1;
    }
  }

  /// Takes a new cascade from the shared buffer. Keeps the filter state, so a
  /// slider drag sweeps rather than clicking -- the same choice `EqProcessor`
  /// makes.
  set(source: Float64Array): void {
    this.coefficients.set(source);
    this.transparent = this.isTransparent();
  }

  /// Forgets the filter history, for a break in the audio (a seek). Carrying it
  /// across one rings the new position with the old one's tail.
  reset(): void {
    this.state.fill(0);
  }

  /// Whether `process` would do nothing, so the caller can skip the copy as
  /// well as the arithmetic.
  get bypassed(): boolean {
    return this.transparent;
  }

  /// Filters one frame in place. `frame` holds one sample per channel, in the
  /// output's layout.
  ///
  /// A frame at a time rather than a buffer, because the worklet builds its
  /// output frame by frame out of a ring that wraps -- and taking the frame
  /// means the channel each sample belongs to is known rather than inferred
  /// from a running phase.
  process(frame: Float64Array): void {
    if (this.transparent) return;
    const c = this.coefficients;
    const preamp = c[EQ_PREAMP];
    for (let channel = 0; channel < this.channels; channel++) {
      let x = frame[channel] * preamp;
      for (let band = 0; band < EQ_BANDS; band++) {
        const at = EQ_HEADER + band * COEFFICIENTS_PER_BAND;
        const s = (channel * EQ_BANDS + band) * STATE_PER_BAND;
        // Direct form I, in the order player-core/src/eq.rs writes it.
        const y =
          c[at] * x +
          c[at + 1] * this.state[s] +
          c[at + 2] * this.state[s + 1] -
          c[at + 3] * this.state[s + 2] -
          c[at + 4] * this.state[s + 3];
        this.state[s + 1] = this.state[s];
        this.state[s] = x;
        this.state[s + 3] = this.state[s + 2];
        this.state[s + 2] = y;
        x = y;
      }
      frame[channel] = x;
    }
  }

  private isTransparent(): boolean {
    const c = this.coefficients;
    if (c[EQ_ENABLED] !== 1) return true;
    if (c[EQ_PREAMP] !== 1) return false;
    for (let band = 0; band < EQ_BANDS; band++) {
      const at = EQ_HEADER + band * COEFFICIENTS_PER_BAND;
      if (c[at] !== 1 || c[at + 1] !== 0 || c[at + 2] !== 0 || c[at + 3] !== 0 || c[at + 4] !== 0) {
        return false;
      }
    }
    return true;
  }
}
