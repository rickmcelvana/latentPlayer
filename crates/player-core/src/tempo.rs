//! Tempo: how fast the music is, and where in the beat it currently is.
//!
//! Fed one value per hop by [`crate::onset`], and pure arithmetic over a ring
//! of floats -- no FFT, no device, and no clock but the hop count it is told
//! about. That is deliberate: everything here can be checked against an
//! analytic answer without a GPU, an audio device or a browser, which is what
//! Phase 3 M5's verification standard asks for (known answers, not
//! plausibility).
//!
//! # How the tempo is found
//!
//! Autocorrelation over a window of the detection function, scored over the
//! lags that correspond to [`MIN_BPM`]..[`MAX_BPM`] (decision 34). Two details
//! carry the milestone:
//!
//! - **Harmonic scoring plus a tempo prior**, because octave errors are the
//!   known failure mode of every tempo estimator: a 120 BPM track has an
//!   autocorrelation peak at 60 BPM too (every other beat) and at 240 (every
//!   eighth). Summing each candidate's own harmonics rewards the period that
//!   explains the *whole* pattern, and the prior breaks the remaining ties the
//!   way a listener does, toward the middle of the tempo range.
//! - **An interpolated peak**, because the lag grid cannot express most tempi.
//!   At a 100Hz detection function the lags either side of 120 BPM are 120.0
//!   and 117.6 -- so a tempo reported as an integer lag is a meter that can
//!   only ever be approximately right, and 90 BPM (lag 66.67) could not be
//!   reported at all.
//!
//! # Where the beat is
//!
//! The phase is *tracked*, not re-derived each hop (decision 35): it advances
//! on the hop clock at the estimated tempo and is pulled toward onsets as they
//! arrive, like a phase-locked loop. So it moves smoothly and keeps counting
//! through a bar with nothing in it, where taking the best alignment each hop
//! would jitter by a hop whenever a weak onset won.

/// Slowest tempo tracked. Below this the ear stops hearing a pulse and starts
/// hearing separate events, and the autocorrelation window would have to grow
/// to hold enough periods to be sure.
pub const MIN_BPM: f32 = 60.0;

/// Fastest tempo tracked. Above this is where a tracker starts reporting the
/// subdivision instead of the beat; a 200 BPM ceiling is what keeps a 120 BPM
/// track with sixteenth hats from reading 480.
pub const MAX_BPM: f32 = 200.0;

/// What a tempo that has not been established reads. Not a NaN, for
/// [`crate::loudness::FLOOR_LUFS`]'s reason: these numbers cross a JSON
/// boundary on the desktop shell, where a non-finite float silently becomes
/// `null`, and both shells have to report the same thing (decision 36).
pub const NO_TEMPO: f32 = 0.0;

/// How much of the detection function the autocorrelation sees. Eight seconds
/// holds eight beats at 60 BPM and twenty-six at 200 -- enough for a periodicity
/// to be a periodicity rather than a coincidence, and short enough that a tempo
/// change is followed within a few seconds rather than averaged away.
const WINDOW_SECONDS: f32 = 8.0;

/// How much has to be there before any tempo is reported at all. Four seconds
/// is four beats at 60 BPM, which is the least that can distinguish a pulse
/// from two events that happened to be evenly spaced.
const WARMUP_SECONDS: f32 = 4.0;

/// How often the estimate is recomputed. Not every hop: the answer cannot
/// meaningfully change in 10ms, and the autocorrelation is the only expensive
/// thing in this module.
const REFRESH_SECONDS: f32 = 0.25;

/// Harmonics summed into a candidate's score, and their weights. A true period
/// also correlates at twice and three times itself, so a candidate that
/// explains those as well is the one that explains the pattern.
const HARMONIC_WEIGHTS: [f32; 3] = [1.0, 0.5, 0.25];

/// Where the tempo prior is centred, and how wide it is in octaves. 120 BPM is
/// the usual perceptual centre; the width is deliberately loose, so the prior
/// breaks ties between octaves rather than choosing the tempo itself.
const PRIOR_CENTRE_BPM: f32 = 120.0;
const PRIOR_WIDTH_OCTAVES: f32 = 0.9;

/// How strongly the detection function has to correlate with itself at the
/// winning period for the tempo to be reported with full confidence, as a
/// fraction of its own energy.
///
/// This is the measure of *periodicity itself* rather than of how far the
/// winner beat the other candidates: a peaky pattern can leave every other
/// candidate deeply negative, which makes "how far above average" say more
/// about the pattern's shape than about how sure the answer is.
const CONFIDENT_CORRELATION: f32 = 0.3;

/// How many onsets the window has to contain before a tempo is reported at
/// all.
///
/// A tempo needs beats, and a beat is an event. Without this the
/// autocorrelation -- which is normalised by the detection function's own
/// energy, so that it can find a pulse in quiet music as well as loud -- will
/// report a *confident* tempo for a periodicity in numerical residue. That is
/// not hypothetical: a held 1kHz tone read 122.4 BPM at confidence 1.00 on the
/// web build while the detector reported no onsets at all and a peak transient
/// of 0.022 (measured 2026-08-20 by `/musical.html`). The native build read no
/// tempo for the same fixture, which was luck rather than correctness -- its
/// residue happened not to be periodic.
///
/// Four is lenient: the slowest tempo tracked puts eight beats in this window
/// and the fastest twenty-six, so half of them can be missed and a real pulse
/// still reports.
const MIN_ONSETS_IN_WINDOW: usize = 4;

/// How strong a picked onset has to be before it counts as one of those four,
/// or is allowed to pull the beat phase.
///
/// `MIN_ONSETS_IN_WINDOW` asks for beats and this says what a beat is. The
/// peak picker fires on any rise above its local mean, which is right for
/// finding *where* something happened and wrong for deciding whether the
/// something was a beat: a slow swell in a pad crosses that bar repeatedly,
/// and eighty-two such crossings in twelve seconds correlated into a confident
/// 142 BPM (reported live 2026-08-20 on sustained organ material, reproduced
/// with a synthesized pad).
///
/// A quarter of full strength, against `onset.rs`'s newly absolute scale: a
/// real attack reports 1.0 there, the pad's crossings report about 0.15, and
/// anything a listener would call a beat is far nearer the first than the
/// second.
const MIN_ONSET_STRENGTH: f32 = 0.25;

/// Below this, the tempo is reported as [`NO_TEMPO`] rather than as a number
/// nobody should act on -- decision 36's "material with no steady pulse says
/// so". The confidence itself is still published, so a consumer can tell a
/// borderline reading from silence.
const MIN_CONFIDENCE: f32 = 0.15;

/// How hard an onset pulls the phase toward it. Small, because this is a
/// correction and not a re-estimate: a run of onsets locks the phase within a
/// beat or two, while one stray hit moves it by a few percent.
const PHASE_CORRECTION: f32 = 0.2;

/// Confidence below which onsets stop pulling the phase at all. Locking to
/// onsets while the tempo is a guess would chase noise.
const MIN_LOCK_CONFIDENCE: f32 = 0.2;

/// The tempo as of the last hop pushed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoReading {
    /// Beats per minute, or [`NO_TEMPO`] when none is established.
    pub bpm: f32,
    /// How much to believe it, 0..1.
    pub confidence: f32,
    /// Where in the beat the newest analysed sample falls: 0.0 on the beat,
    /// rising to 1.0 just before the next one. Advances only at hop
    /// boundaries, so its resolution is one hop (10ms).
    pub beat_phase: f32,
}

impl TempoReading {
    /// What a tracker that has measured nothing reads.
    pub fn silent() -> Self {
        Self {
            bpm: NO_TEMPO,
            confidence: 0.0,
            beat_phase: 0.0,
        }
    }
}

/// The detection function's history, the tempo taken from it, and the beat
/// phase tracked against it.
pub struct TempoTracker {
    detection_hz: f32,
    /// Ring of detection values, newest at `write`.
    history: Vec<f32>,
    /// Whether each of those hops carried an onset, in the same order. Counted
    /// rather than correlated: see `MIN_ONSETS_IN_WINDOW`.
    onsets: Vec<bool>,
    write: usize,
    filled: usize,
    /// Time-ordered scratch copy of `history`, and the autocorrelation over it.
    /// Kept as fields so a recompute allocates nothing.
    series: Vec<f32>,
    correlation: Vec<f32>,
    scores: Vec<f32>,
    raw: Vec<f32>,
    min_lag: usize,
    max_lag: usize,
    warmup_hops: usize,
    refresh_hops: usize,
    since_estimate: usize,
    bpm: f32,
    confidence: f32,
    phase: f32,
}

impl TempoTracker {
    /// `detection_hz` is the rate the detection function actually runs at --
    /// [`crate::onset::OnsetDetector::detection_hz`], which is derived from the
    /// output rate rather than nominal, so a lag means the same duration on
    /// every device.
    pub fn new(detection_hz: f32) -> Self {
        let detection_hz = if detection_hz.is_finite() && detection_hz > 0.0 {
            detection_hz
        } else {
            1.0
        };
        let hops = |seconds: f32| ((seconds * detection_hz).round() as usize).max(1);
        let window = hops(WINDOW_SECONDS);
        let min_lag = ((60.0 * detection_hz / MAX_BPM).floor() as usize).max(1);
        let max_lag = ((60.0 * detection_hz / MIN_BPM).ceil() as usize).max(min_lag + 1);

        Self {
            detection_hz,
            history: vec![0.0; window],
            onsets: vec![false; window],
            write: 0,
            filled: 0,
            series: Vec::with_capacity(window),
            correlation: Vec::new(),
            scores: Vec::new(),
            raw: Vec::new(),
            min_lag,
            max_lag,
            warmup_hops: hops(WARMUP_SECONDS),
            refresh_hops: hops(REFRESH_SECONDS),
            since_estimate: 0,
            bpm: NO_TEMPO,
            confidence: 0.0,
            phase: 0.0,
        }
    }

    /// Forgets the tempo: after a seek, a lap or a track hand-off. A tempo
    /// measured across two tracks is a tempo nobody played.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.onsets.fill(false);
        self.write = 0;
        self.filled = 0;
        self.since_estimate = 0;
        self.bpm = NO_TEMPO;
        self.confidence = 0.0;
        self.phase = 0.0;
    }

    /// Adds one hop of the detection function, and the onset picked in it (0.0
    /// when there was none).
    pub fn push(&mut self, detection: f32, onset: f32) {
        let detection = if detection.is_finite() {
            detection.max(0.0)
        } else {
            0.0
        };
        self.history[self.write] = detection;
        let is_beat = onset >= MIN_ONSET_STRENGTH;
        self.onsets[self.write] = is_beat;
        self.write = (self.write + 1) % self.history.len();
        self.filled = (self.filled + 1).min(self.history.len());

        self.advance_phase();
        if is_beat {
            self.pull_phase(onset);
        }

        self.since_estimate += 1;
        if self.since_estimate >= self.refresh_hops && self.filled >= self.warmup_hops {
            self.since_estimate = 0;
            self.estimate();
        }
    }

    /// The tempo, its confidence and the beat phase as of the last hop.
    pub fn reading(&self) -> TempoReading {
        TempoReading {
            bpm: self.bpm,
            confidence: self.confidence,
            beat_phase: self.phase,
        }
    }

    /// One hop of elapsed time, at the tempo currently believed.
    fn advance_phase(&mut self) {
        if self.bpm <= 0.0 {
            return;
        }
        let hops_per_beat = 60.0 * self.detection_hz / self.bpm;
        if hops_per_beat > 0.0 {
            self.phase = (self.phase + 1.0 / hops_per_beat).fract();
        }
    }

    /// Nudges the phase toward an onset that just happened, in proportion to
    /// how strong it was.
    fn pull_phase(&mut self, onset: f32) {
        if self.bpm <= 0.0 || self.confidence < MIN_LOCK_CONFIDENCE {
            return;
        }
        // Signed distance to the nearest beat, in beats: an onset just after a
        // beat says the phase is running late, one just before says early.
        let error = if self.phase < 0.5 {
            self.phase
        } else {
            self.phase - 1.0
        };
        let corrected = self.phase - PHASE_CORRECTION * onset.clamp(0.0, 1.0) * error;
        self.phase = corrected.rem_euclid(1.0);
    }

    /// Recomputes the tempo from the window currently held.
    fn estimate(&mut self) {
        let count = self.filled;
        let capacity = self.history.len();

        // Beats first: without events there is nothing here that a listener
        // would call a tempo, however periodic the residue happens to be.
        if self.onsets.iter().filter(|seen| **seen).count() < MIN_ONSETS_IN_WINDOW {
            self.bpm = NO_TEMPO;
            self.confidence = 0.0;
            return;
        }

        // Oldest first, so lags mean what they say.
        self.series.clear();
        let start = (self.write + capacity - count) % capacity;
        for i in 0..count {
            self.series.push(self.history[(start + i) % capacity]);
        }
        let mean = self.series.iter().sum::<f32>() / count as f32;
        for value in self.series.iter_mut() {
            *value -= mean;
        }

        // Lags out to the longest harmonic any candidate will ask for, bounded
        // by what the window can actually support.
        let harmonics = HARMONIC_WEIGHTS.len();
        let longest = (self.max_lag * harmonics).min(count.saturating_sub(1));
        if longest <= self.min_lag {
            self.bpm = NO_TEMPO;
            self.confidence = 0.0;
            return;
        }

        self.correlation.clear();
        self.correlation.resize(longest + 1, 0.0);
        for lag in 0..=longest {
            let overlap = count - lag;
            let sum: f32 = (lag..count)
                .map(|n| self.series[n] * self.series[n - lag])
                .sum();
            // Divided by the overlap: without it every autocorrelation slopes
            // downward with lag, which biases every estimate fast.
            self.correlation[lag] = sum / overlap as f32;
        }

        let energy = self.correlation[0];
        if energy <= f32::MIN_POSITIVE {
            // Nothing happened -- silence, or a passage with no change in it.
            self.bpm = NO_TEMPO;
            self.confidence = 0.0;
            return;
        }

        let max_lag = self.max_lag.min(longest);
        if max_lag <= self.min_lag {
            self.bpm = NO_TEMPO;
            self.confidence = 0.0;
            return;
        }

        // Two scores per candidate. `raw` is how periodic the detection
        // function is at that period -- harmonics averaged, normalised by the
        // window's own energy -- and is what the confidence is taken from.
        // `weighted` is that under the tempo prior, and is what wins.
        self.scores.clear();
        self.scores.resize(max_lag + 1, 0.0);
        self.raw.clear();
        self.raw.resize(max_lag + 1, 0.0);
        for lag in self.min_lag..=max_lag {
            let mut score = 0.0;
            let mut weight_used = 0.0;
            for (harmonic, weight) in HARMONIC_WEIGHTS.iter().enumerate() {
                let at = lag * (harmonic + 1);
                if at > longest {
                    break;
                }
                score += weight * (self.correlation[at] / energy);
                weight_used += weight;
            }
            if weight_used > 0.0 {
                score /= weight_used;
            }
            self.raw[lag] = score;
            self.scores[lag] = score * prior(60.0 * self.detection_hz / lag as f32);
        }

        let candidates = &self.scores[self.min_lag..=max_lag];
        let (best, peak) = candidates.iter().enumerate().fold(
            (0usize, f32::MIN),
            |(best, peak), (index, &score)| {
                if score > peak {
                    (index, score)
                } else {
                    (best, peak)
                }
            },
        );
        let best_lag = self.min_lag + best;

        if peak <= 0.0 {
            self.bpm = NO_TEMPO;
            self.confidence = 0.0;
            return;
        }

        let confidence = (self.raw[best_lag] / CONFIDENT_CORRELATION).clamp(0.0, 1.0);

        // The lag grid is coarse where it matters most: interpolating the peak
        // is the difference between "reads 117.6" and "reads 120.0"
        // (decision 34).
        let lag = interpolate_peak(&self.scores, best_lag, self.min_lag, max_lag);
        self.confidence = confidence;
        self.bpm = if confidence >= MIN_CONFIDENCE && lag > 0.0 {
            60.0 * self.detection_hz / lag
        } else {
            NO_TEMPO
        };
    }
}

/// A log-normal preference for tempi near [`PRIOR_CENTRE_BPM`], which is how a
/// listener resolves the octave ambiguity a periodicity alone cannot.
fn prior(bpm: f32) -> f32 {
    if bpm <= 0.0 {
        return 0.0;
    }
    let octaves = (bpm / PRIOR_CENTRE_BPM).log2() / PRIOR_WIDTH_OCTAVES;
    (-0.5 * octaves * octaves).exp()
}

/// Sub-lag position of the peak at `best`, by fitting a parabola through it and
/// its neighbours. Returns `best` itself at the edges of the range, where there
/// is no pair to fit through.
fn interpolate_peak(scores: &[f32], best: usize, min_lag: usize, max_lag: usize) -> f32 {
    if best <= min_lag || best >= max_lag {
        return best as f32;
    }
    let (left, centre, right) = (scores[best - 1], scores[best], scores[best + 1]);
    let curvature = left - 2.0 * centre + right;
    if curvature.abs() < f32::MIN_POSITIVE {
        return best as f32;
    }
    let delta = 0.5 * (left - right) / curvature;
    // A parabola through three samples of a real peak puts the vertex inside
    // the middle sample; anything further out means the peak is not one.
    best as f32 + delta.clamp(-0.5, 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETECTION_HZ: f32 = 100.0;

    /// A detection function with an impulse every `period` hops -- a click
    /// track as the tempo tracker sees it, with no audio involved.
    fn pulses(hops: usize, period: f32) -> Vec<f32> {
        (0..hops)
            .map(|hop| {
                let into = hop as f32 % period;
                let distance = into.min(period - into);
                // A couple of hops wide, like a real transient's flux.
                if distance < 1.5 {
                    1.0 - distance / 1.5
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn track(values: &[f32]) -> TempoTracker {
        let mut tracker = TempoTracker::new(DETECTION_HZ);
        for &value in values {
            // Onsets do not matter to the tempo itself, only to the phase.
            tracker.push(value, if value > 0.9 { value } else { 0.0 });
        }
        tracker
    }

    fn bpm_of(period_bpm: f32, seconds: f32) -> TempoReading {
        let period = 60.0 * DETECTION_HZ / period_bpm;
        let hops = (seconds * DETECTION_HZ) as usize;
        track(&pulses(hops, period)).reading()
    }

    #[test]
    fn a_click_track_reads_its_own_tempo() {
        // The milestone's headline claim, at two tempi -- one at the prior's
        // centre and one well off it, because a tracker that reports its prior
        // instead of the audio passes the first check on its own.
        for expected in [120.0, 90.0, 140.0, 75.0] {
            let reading = bpm_of(expected, 12.0);
            assert!(
                (reading.bpm - expected).abs() < 0.5,
                "{expected} BPM read {:.2} (confidence {:.2})",
                reading.bpm,
                reading.confidence
            );
            assert!(
                reading.confidence > 0.5,
                "{expected} BPM read with confidence {:.2}",
                reading.confidence
            );
        }
    }

    #[test]
    fn the_peak_is_interpolated_rather_than_indexed() {
        // 90 BPM is lag 66.67 at a 100Hz detection function: unreachable by
        // any integer lag, and the two nearest are 89.55 and 90.91. A tracker
        // without interpolation cannot be within 0.5 of the answer here, so
        // this is decision 34's arithmetic half.
        let reading = bpm_of(90.0, 12.0);
        assert!(
            (reading.bpm - 90.0).abs() < 0.5,
            "read {:.3}, and the integer lags either side are 89.55 and 90.91",
            reading.bpm
        );
    }

    #[test]
    fn an_offbeat_does_not_double_the_tempo() {
        // 120 BPM with something on every eighth: the classic octave error,
        // and the reason for harmonic scoring. The eighths are weaker than the
        // beats, exactly as a hi-hat is under a snare.
        let period = 60.0 * DETECTION_HZ / 120.0;
        let beats = pulses(1_200, period);
        let eighths = pulses(1_200, period / 2.0);
        let mixed: Vec<f32> = beats
            .iter()
            .zip(eighths.iter())
            .map(|(beat, eighth)| beat.max(eighth * 0.5))
            .collect();

        let reading = track(&mixed).reading();
        assert!(
            (reading.bpm - 120.0).abs() < 1.0,
            "read {:.2}, which is {}",
            reading.bpm,
            if reading.bpm > 200.0 {
                "the subdivision"
            } else {
                "not the beat"
            }
        );
    }

    #[test]
    fn a_slow_track_does_not_read_double_either() {
        // The other direction: 70 BPM is far enough below the prior's centre
        // that 140 would score well on the prior alone.
        let reading = bpm_of(70.0, 14.0);
        assert!(
            (reading.bpm - 70.0).abs() < 1.0,
            "read {:.2} (confidence {:.2})",
            reading.bpm,
            reading.confidence
        );
    }

    #[test]
    fn nothing_steady_reports_no_tempo() {
        // Decision 36's honest failure. Pseudo-random activity with no pulse in
        // it must not produce a confident number.
        let mut state = 7u32;
        let noise: Vec<f32> = (0..1_200)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / 16_777_216.0
            })
            .collect();
        let reading = track(&noise).reading();
        assert!(
            reading.confidence < 0.5,
            "unpulsed material read confidence {:.2} at {:.1} BPM",
            reading.confidence,
            reading.bpm
        );
    }

    #[test]
    fn a_periodic_residue_with_no_onsets_in_it_is_not_a_tempo() {
        // The web build's held 1kHz tone, in miniature: a detection function
        // that is perfectly periodic and vanishingly small, with nothing in it
        // any detector would call an onset. The autocorrelation is normalised
        // by its own energy, so without `MIN_ONSETS_IN_WINDOW` this reads a
        // confident 120 BPM (measured 2026-08-20).
        let period = 60.0 * DETECTION_HZ / 120.0;
        let mut tracker = TempoTracker::new(DETECTION_HZ);
        for value in pulses(1_200, period) {
            tracker.push(value * 0.0005, 0.0);
        }
        let reading = tracker.reading();
        assert_eq!(reading.bpm, NO_TEMPO, "confidence {}", reading.confidence);
        assert_eq!(reading.confidence, 0.0);

        // ...and the identical shape *with* onsets in it is a tempo, which is
        // what keeps this rule from being a way of reporting nothing.
        let mut heard = TempoTracker::new(DETECTION_HZ);
        for value in pulses(1_200, period) {
            heard.push(value * 0.0005, if value > 0.9 { value } else { 0.0 });
        }
        assert!((heard.reading().bpm - 120.0).abs() < 0.5);
    }

    #[test]
    fn silence_reports_no_tempo_at_all() {
        let reading = track(&vec![0.0; 1_200]).reading();
        assert_eq!(reading.bpm, NO_TEMPO);
        assert_eq!(reading.confidence, 0.0);
        assert_eq!(reading.beat_phase, 0.0);
    }

    #[test]
    fn no_tempo_before_the_warmup() {
        // Two seconds of a perfect click still reads nothing: four evenly
        // spaced events are not yet a pulse (WARMUP_SECONDS).
        let reading = bpm_of(120.0, 2.0);
        assert_eq!(reading.bpm, NO_TEMPO);
    }

    #[test]
    fn the_phase_lands_on_the_beat() {
        // Decision 35: the phase is what a visual animates on, so it has to be
        // near zero *at* the onsets rather than merely advancing at the right
        // rate.
        let period = 60.0 * DETECTION_HZ / 120.0;
        let values = pulses(2_000, period);
        let mut tracker = TempoTracker::new(DETECTION_HZ);
        let mut errors = Vec::new();
        for (hop, &value) in values.iter().enumerate() {
            tracker.push(value, if value > 0.9 { value } else { 0.0 });
            // Only once the tempo has been established and the loop has had a
            // few beats to lock.
            if hop > 1_000 && value > 0.9 {
                let phase = tracker.reading().beat_phase;
                errors.push(phase.min(1.0 - phase));
            }
        }
        assert!(!errors.is_empty(), "the click track had onsets");
        let worst = errors.iter().copied().fold(0.0f32, f32::max);
        assert!(worst < 0.1, "the phase was {worst:.3} of a beat out");
    }

    #[test]
    fn the_phase_keeps_counting_through_a_gap() {
        // A bar with nothing in it must not stop the beat: the phase advances
        // on the clock, and only onsets *correct* it (decision 35).
        let period = 60.0 * DETECTION_HZ / 120.0;
        let mut values = pulses(2_000, period);
        let mut tracker = TempoTracker::new(DETECTION_HZ);
        for value in values.iter_mut().skip(1_500) {
            *value = 0.0;
        }
        for &value in &values {
            tracker.push(value, if value > 0.9 { value } else { 0.0 });
        }
        let reading = tracker.reading();
        assert!(reading.bpm > 0.0, "the tempo survives a silent passage");

        // 2000 hops is 40 beats at 120 BPM, so the phase should be back where
        // it started -- and the check is that it advanced at all rather than
        // freezing at whatever the last onset left it.
        let expected = (2_000.0 / period).fract();
        let error = (reading.beat_phase - expected).abs();
        assert!(
            error.min(1.0 - error) < 0.15,
            "phase {:.3}, expected about {expected:.3}",
            reading.beat_phase
        );
    }

    #[test]
    fn a_reset_forgets_the_tempo() {
        let mut tracker = TempoTracker::new(DETECTION_HZ);
        let period = 60.0 * DETECTION_HZ / 120.0;
        for value in pulses(1_200, period) {
            tracker.push(value, if value > 0.9 { value } else { 0.0 });
        }
        assert!(tracker.reading().bpm > 0.0);

        tracker.reset();
        assert_eq!(tracker.reading(), TempoReading::silent());

        // ...and it needs the whole warm-up again rather than answering from
        // what it used to know.
        for value in pulses(100, period) {
            tracker.push(value, 0.0);
        }
        assert_eq!(tracker.reading().bpm, NO_TEMPO);
    }

    #[test]
    fn the_output_rate_cannot_change_the_answer() {
        // The tracker is told the detection function's real rate, so a device
        // whose hop lands on a different number of samples still reports the
        // same tempo (decision 32's other half).
        let reading = |detection_hz: f32| {
            let period = 60.0 * detection_hz / 128.0;
            let hops = (12.0 * detection_hz) as usize;
            let mut tracker = TempoTracker::new(detection_hz);
            for value in pulses(hops, period) {
                tracker.push(value, if value > 0.9 { value } else { 0.0 });
            }
            tracker.reading().bpm
        };
        let (fast, slow) = (reading(100.0), reading(93.75));
        assert!((fast - 128.0).abs() < 0.6, "{fast:.2}");
        assert!((slow - 128.0).abs() < 0.6, "{slow:.2}");
    }

    #[test]
    fn the_prior_prefers_the_middle_without_choosing_the_tempo() {
        assert!(prior(120.0) > prior(60.0));
        assert!(prior(120.0) > prior(200.0));
        // ...but not so hard that a real 70 BPM track cannot outscore it: an
        // octave out costs about half the score, which a genuine periodicity
        // clears easily.
        assert!(prior(60.0) > 0.4, "{}", prior(60.0));
    }

    /// A click track as audio: broadband bursts at an exact tempo, stereo and
    /// interleaved the way the tap holds it.
    fn click_audio(rate: u32, bpm: f32, seconds: f32) -> Vec<f32> {
        let period = (60.0 / bpm * rate as f32).round() as usize;
        let frames = (seconds * rate as f32) as usize;
        let mut state = 2_463_534_242u32;
        (0..frames)
            .flat_map(|n| {
                let into = n % period;
                let s = if into < 128 {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let noise = (state >> 8) as f32 / 8_388_608.0 - 1.0;
                    noise * (1.0 - into as f32 / 128.0) * 0.7
                } else {
                    0.0
                };
                [s, s]
            })
            .collect()
    }

    /// The tempo of `audio`, measured the way the meters will: through the real
    /// detector, in drain-sized pieces.
    fn measured(rate: u32, audio: &[f32]) -> TempoReading {
        let mut detector = crate::onset::OnsetDetector::new(rate, 2);
        let mut tracker = TempoTracker::new(detector.detection_hz());
        // ~16ms at a time, in frames, like the native analysis thread.
        for chunk in audio.chunks(2 * (rate as usize / 60)) {
            for hop in detector.push(chunk) {
                tracker.push(hop.detection, hop.onset);
            }
        }
        tracker.reading()
    }

    #[test]
    fn a_click_track_reads_its_tempo_through_the_real_detector() {
        // The two modules together, on audio rather than on a synthetic
        // detection function: this is the figure `musical_smoke` will assert
        // through the whole playback path, checked here where it is cheap.
        for bpm in [120.0, 90.0] {
            let reading = measured(48_000, &click_audio(48_000, bpm, 14.0));
            assert!(
                (reading.bpm - bpm).abs() < 0.5,
                "{bpm} BPM read {:.2} (confidence {:.2})",
                reading.bpm,
                reading.confidence
            );
            assert!(reading.confidence > 0.5, "{:.2}", reading.confidence);
        }
    }

    #[test]
    fn the_device_rate_cannot_change_the_tempo() {
        // Decision 32 end to end: 44.1kHz hops every 441 frames and 48kHz every
        // 480, and the same music has to read the same tempo on either.
        let at = |rate: u32| measured(rate, &click_audio(rate, 128.0, 14.0)).bpm;
        let (forty_four, forty_eight) = (at(44_100), at(48_000));
        assert!((forty_four - 128.0).abs() < 0.5, "{forty_four:.2}");
        assert!((forty_eight - 128.0).abs() < 0.5, "{forty_eight:.2}");
        assert!(
            (forty_four - forty_eight).abs() < 0.2,
            "{forty_four:.2} against {forty_eight:.2}"
        );
    }
}
