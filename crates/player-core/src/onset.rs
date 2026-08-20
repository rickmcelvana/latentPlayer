//! The onset detection function, and the features measured on its grid:
//! transients, onsets and the spectral centroid.
//!
//! Cross-target and free of threads, like `spectrum.rs` and `loudness.rs`, and
//! fed from the same drain (`analysis.rs`). What makes this one different from
//! the spectrum next door is *when* it runs (Phase 3 M5 decision 32): the
//! spectrum is a picture of a window, computed when a display asks, while a
//! detection function is a measurement over time and has to run on a fixed grid
//! whatever the caller's cadence is. Spectral flux measured between two drains
//! would be a different measurement on every drain -- and the native analysis
//! thread drains every 16ms while the web worker drains on
//! `requestAnimationFrame`, which is two shells disagreeing out of one
//! implementation.
//!
//! So this keeps its own short-time transform: a hop derived from the output
//! rate so the detection function runs at a fixed rate in *time*, and an
//! internal accumulator that makes the caller's chunking irrelevant. Push a
//! drain of any size, in any pieces, and the hops that come out are identical.
//!
//! It also does not reuse [`crate::spectrum::SpectrumAnalyzer`], for three
//! reasons that are each fatal on their own (decision 33): that window is
//! display resolution, its levels are log bands with a 30dB/s release baked
//! into them, and it is computed when the display asks. Flux across smoothed,
//! log-binned, irregularly-timed levels is not an onset detection function.
//! The converse holds too -- these bins stay inside this module, because a
//! second published spectrum would be a second answer to the question the
//! single analysis tap exists to answer once.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// How often the detection function is evaluated, in Hz. The hop is rounded
/// from this and the real rate is derived back from the hop, so a 48kHz device
/// (hop 480) and a 44.1kHz one (hop 441) both land on exactly 100Hz -- which is
/// what lets the two report the same tempo to the same decimal.
///
/// 100Hz is 10ms of timing resolution on an onset, below the ~30ms at which two
/// hits stop being heard as separate, and it puts the tempo range this app
/// tracks (60-200 BPM) at lags of 30 to 100 -- long enough for the
/// autocorrelation peak to be interpolated rather than merely indexed.
pub const DETECTION_HZ: f32 = 100.0;

/// Magnitude compression, as `ln(1 + COMPRESSION * magnitude)`, over magnitudes
/// normalised so that a full-scale sine reads 1.0 in its own bin.
///
/// Flux on raw magnitudes is dominated by whatever is loudest, so a kick drum
/// hides every other onset in the mix and a quiet passage produces none at all.
/// Compressing first is what makes the detection function about *change*
/// rather than about level -- the standard trick, and the reason a hi-hat in a
/// loud chorus still registers.
const COMPRESSION: f32 = 1000.0;

/// The quietest magnitude that counts as content, in normalised full-scale
/// units: -80dBFS per bin.
///
/// Compression's cost is that it magnifies *relative* change, and the relative
/// change in a bin holding nothing but window leakage is large -- a held 440Hz
/// sine moves its own leakage tails every hop, because the hop is not a whole
/// number of cycles. Floored, those bins contribute a constant and so no flux.
/// Unfloored they contributed enough to fire fifteen onsets during four seconds
/// of one held note (measured 2026-08-19, and the reason this constant exists).
const MAGNITUDE_FLOOR: f32 = 1e-4;

/// How far back the adaptive threshold looks. Long enough to average over a
/// beat at any tempo this tracks, short enough to follow a track getting
/// busier without a run of false onsets while it catches up.
const MEAN_SECONDS: f32 = 0.35;

/// How far above the local mean the detection function has to rise to be an
/// onset.
const THRESHOLD_FACTOR: f32 = 1.5;

/// An absolute floor under the threshold, in the compressed units the flux is
/// summed in. Without it, near-silence has a tiny mean and *any* fluctuation is
/// 1.5x of tiny.
///
/// Sized by measurement rather than by taste: with `MAGNITUDE_FLOOR` in place a
/// held sine's residual flux peaks around 0.18 while a click's reaches 1005, so
/// half a unit sits three orders of magnitude under anything real and still
/// covers the residue.
const SILENCE_FLOOR: f32 = 0.5;

/// Shortest gap between two onsets. 50ms is 1200 BPM of sixteenth notes -- far
/// faster than anything this needs to resolve, and enough that one drum hit
/// with a rattly tail is one onset rather than three.
const REFRACTORY_SECONDS: f32 = 0.05;

/// How long the normalising peak takes to forget a loud passage. This is what
/// makes [`HopFeatures::transient`] a 0..1 figure rather than an unbounded one,
/// and the release is slow so that a quiet bar after a loud one does not
/// immediately re-scale every small transient in it up to 1.
const PEAK_RELEASE_SECONDS: f32 = 2.0;

/// What one hop of the detection function measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HopFeatures {
    /// The detection function itself: rectified spectral flux with the local
    /// mean removed, in compressed magnitude units. Unbounded, and what the
    /// tempo tracker correlates -- not a display quantity.
    pub detection: f32,
    /// The same thing normalised against a slowly-forgotten peak, 0..1: how
    /// percussive the music is right now. Published as measured, with any decay
    /// left to whatever draws it (decision 37).
    pub transient: f32,
    /// Strength of an onset picked in this hop, 0.0 when there was none.
    pub onset: f32,
    /// Spectral centroid of this hop, in Hz -- the "brightness" of the moment.
    /// 0.0 when there is no energy to take a centroid of, which is a value
    /// rather than a NaN for the reason every floor in this crate is
    /// (decision 36).
    pub centroid_hz: f32,
}

/// The detection function, its features, and the hop clock they run on.
pub struct OnsetDetector {
    fft: Arc<dyn RealToComplex<f32>>,
    /// Periodic Hann over the analysis window.
    window: Vec<f32>,
    /// `2 / sum(window)`: what puts a full-scale sine at 1.0, so
    /// `MAGNITUDE_FLOOR` means the same thing at any window length.
    scale: f32,
    /// Mono history, used as a circular buffer: the newest `window.len()`
    /// frames, which is what each hop transforms.
    ring: Vec<f32>,
    write: usize,
    /// Frames since the last hop boundary. The whole of decision 32's
    /// chunking-invariance lives in this counter.
    since_hop: usize,
    hop: usize,
    channels: usize,
    sample_rate: u32,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Compressed magnitudes of the previous hop, which flux is measured
    /// against. `None` until a hop has been analysed, so the first one after a
    /// reset reports no flux rather than reporting the whole spectrum as a
    /// change.
    previous: Vec<f32>,
    have_previous: bool,
    /// The last `MEAN_SECONDS` of detection values, for the adaptive threshold.
    /// Summed fresh each hop rather than kept as a running total: it is 35
    /// additions, and a running total in f32 drifts by an amount that depends
    /// on how the caller chunked its input -- which is exactly what this module
    /// promises it cannot.
    recent: Vec<f32>,
    recent_write: usize,
    recent_filled: usize,
    /// Hops since the last onset, against `REFRACTORY_SECONDS`.
    since_onset: usize,
    refractory_hops: usize,
    /// Slowly-released peak of the detection function, normalising
    /// [`HopFeatures::transient`].
    peak: f32,
    peak_decay: f32,
    /// Hops produced by the current `push`, handed back so the caller can feed
    /// each one to the tempo tracker. Cleared per call, never grown past a
    /// drain's worth.
    produced: Vec<HopFeatures>,
}

impl OnsetDetector {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let sample_rate = sample_rate.max(1);
        let channels = usize::from(channels.max(1));
        let hop = ((sample_rate as f32 / DETECTION_HZ).round() as usize).max(1);
        // Two hops, so consecutive windows overlap by half: a transient that
        // falls near a hop boundary is then whole in one of them rather than
        // split across two, and the flux sees its real height.
        let size = (hop * 2).next_power_of_two();

        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(size);
        let input = fft.make_input_vec();
        let output = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let bins = output.len();

        let hop_seconds = hop as f32 / sample_rate as f32;
        let mean_hops = ((MEAN_SECONDS / hop_seconds).round() as usize).max(1);

        let window: Vec<f32> = (0..size).map(|n| hann(n, size)).collect();
        let window_sum = window.iter().sum::<f32>().max(f32::MIN_POSITIVE);

        Self {
            fft,
            window,
            scale: 2.0 / window_sum,
            ring: vec![0.0; size],
            write: 0,
            since_hop: 0,
            hop,
            channels,
            sample_rate,
            input,
            output,
            scratch,
            previous: vec![0.0; bins],
            have_previous: false,
            recent: vec![0.0; mean_hops],
            recent_write: 0,
            recent_filled: 0,
            since_onset: usize::MAX / 2,
            refractory_hops: ((REFRACTORY_SECONDS / hop_seconds).round() as usize).max(1),
            peak: 0.0,
            peak_decay: (-hop_seconds / PEAK_RELEASE_SECONDS).exp(),
            produced: Vec::new(),
        }
    }

    /// Frames between two hops. The detection function's period, in samples.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// The rate the detection function actually runs at, which is the output
    /// rate divided by the hop rather than [`DETECTION_HZ`] itself. What the
    /// tempo tracker turns a lag into a tempo with.
    pub fn detection_hz(&self) -> f32 {
        self.sample_rate as f32 / self.hop as f32
    }

    /// Forgets everything: after a seek, a lap or a track hand-off, where
    /// carrying the previous spectrum across would report the discontinuity
    /// itself as the loudest onset in the track.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.since_hop = 0;
        self.previous.fill(0.0);
        self.have_previous = false;
        self.recent.fill(0.0);
        self.recent_write = 0;
        self.recent_filled = 0;
        self.since_onset = usize::MAX / 2;
        self.peak = 0.0;
        self.produced.clear();
    }

    /// Adds interleaved audio and returns the hops it completed.
    ///
    /// Zero, one or several, depending on how much arrived -- and *which* hops
    /// they are does not depend on that at all, which is the property decision
    /// 32 exists for and `chunking_cannot_change_the_answer` below pins.
    pub fn push(&mut self, interleaved: &[f32]) -> &[HopFeatures] {
        self.produced.clear();
        for frame in interleaved.chunks_exact(self.channels) {
            // Summed to mono, like the spectrum's own input (M4 decision 5):
            // an onset is an event in the music, not in one speaker.
            let mono = frame.iter().sum::<f32>() / self.channels as f32;
            self.ring[self.write] = mono;
            self.write = (self.write + 1) % self.ring.len();
            self.since_hop += 1;
            if self.since_hop >= self.hop {
                self.since_hop = 0;
                let features = self.analyse();
                self.produced.push(features);
            }
        }
        &self.produced
    }

    /// Transforms the newest window and measures everything on it.
    fn analyse(&mut self) -> HopFeatures {
        let size = self.ring.len();
        for (n, slot) in self.input.iter_mut().enumerate() {
            // The ring's oldest sample is where the next write will go, so
            // reading from there gives the window in time order with the newest
            // sample last.
            *slot = self.ring[(self.write + n) % size] * self.window[n];
        }

        // The only failure modes are wrong slice lengths, which are this
        // struct's own invariants -- a failure here would be a bug in this
        // file, not a condition the caller could act on.
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            return HopFeatures::default();
        }

        let bin_hz = self.sample_rate as f32 / size as f32;
        let mut flux = 0.0f32;
        let mut weighted = 0.0f64;
        let mut total = 0.0f64;
        for (bin, value) in self.output.iter().enumerate().skip(1) {
            let magnitude = value.norm() * self.scale;
            // The centroid is taken on the *linear* magnitudes: it is the
            // energy's centre of mass in Hz, and compressing first would move
            // it toward whatever is quietest.
            weighted += f64::from(magnitude) * f64::from(bin as f32 * bin_hz);
            total += f64::from(magnitude);

            let compressed = (1.0 + COMPRESSION * magnitude.max(MAGNITUDE_FLOOR)).ln();
            if self.have_previous {
                // Half-wave rectified: energy appearing is an onset, energy
                // going away is a note ending, and only one of those is an
                // event the music has.
                flux += (compressed - self.previous[bin]).max(0.0);
            }
            self.previous[bin] = compressed;
        }
        self.have_previous = true;

        let centroid_hz = if total > 1e-9 {
            (weighted / total) as f32
        } else {
            0.0
        };

        // The threshold looks at the hops *before* this one. Including the
        // current value would let a big transient raise its own bar.
        let filled = self.recent_filled.max(1);
        let mean: f32 = self.recent.iter().take(self.recent_filled).sum::<f32>() / filled as f32;
        let detection = (flux - mean).max(0.0);

        self.recent[self.recent_write] = flux;
        self.recent_write = (self.recent_write + 1) % self.recent.len();
        self.recent_filled = (self.recent_filled + 1).min(self.recent.len());

        self.peak = (self.peak * self.peak_decay).max(detection);
        let transient = if self.peak > f32::MIN_POSITIVE {
            (detection / self.peak).clamp(0.0, 1.0)
        } else {
            0.0
        };

        self.since_onset = self.since_onset.saturating_add(1);
        let over_threshold = flux > mean * THRESHOLD_FACTOR + SILENCE_FLOOR;
        let onset = if over_threshold && self.since_onset >= self.refractory_hops {
            self.since_onset = 0;
            transient.max(f32::MIN_POSITIVE)
        } else {
            0.0
        };

        HopFeatures {
            detection,
            transient,
            onset,
            centroid_hz,
        }
    }
}

/// Periodic Hann of length `len`, at position `n`. Same window `spectrum.rs`
/// uses, and periodic for the same reason: this is a continuous signal being
/// analysed, not a finite record.
fn hann(n: usize, len: usize) -> f32 {
    let phase = 2.0 * std::f32::consts::PI * n as f32 / len as f32;
    0.5 * (1.0 - phase.cos())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Interleaved stereo audio from a per-frame function.
    fn stereo(frames: usize, mut sample: impl FnMut(usize) -> f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = sample(n);
                [s, s]
            })
            .collect()
    }

    /// Silence with a short burst of noise every `period` frames, starting at
    /// `offset` -- a click track with a known tempo and known onset times.
    fn clicks(frames: usize, period: usize, offset: usize) -> Vec<f32> {
        let mut state = 12_345u32;
        stereo(frames, move |n| {
            if n < offset {
                return 0.0;
            }
            let into = (n - offset) % period;
            if into < 96 {
                // A deterministic pseudo-random burst: broadband, so it moves
                // every bin, which is what an onset detector is looking for.
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (state >> 8) as f32 / 8_388_608.0 - 1.0;
                let envelope = 1.0 - into as f32 / 96.0;
                noise * envelope * 0.7
            } else {
                0.0
            }
        })
    }

    fn hops_of(detector: &mut OnsetDetector, audio: &[f32]) -> Vec<HopFeatures> {
        detector.push(audio).to_vec()
    }

    #[test]
    fn the_hop_is_derived_so_the_rate_is_exact() {
        // 100Hz at both of the rates this app actually meets, which is what
        // makes a tempo the same figure on either device (decision 32).
        for rate in [44_100, 48_000] {
            let detector = OnsetDetector::new(rate, 2);
            assert_eq!(detector.hop(), rate as usize / 100);
            assert!((detector.detection_hz() - 100.0).abs() < 1e-6);
        }
        // And an odd rate still lands within a hop of the nominal.
        let odd = OnsetDetector::new(37_000, 2);
        assert!((odd.detection_hz() - DETECTION_HZ).abs() < 1.0);
    }

    #[test]
    fn chunking_cannot_change_the_answer() {
        // Decision 32's real content. The native thread hands over ~16ms at a
        // time and the web worker whatever a frame happened to be; if those
        // produced different hops, the two shells would report different
        // tempi from identical audio.
        let audio = clicks(48_000, 24_000, 1_000);

        let mut whole = OnsetDetector::new(RATE, 2);
        let all = hops_of(&mut whole, &audio);

        let mut piecemeal = OnsetDetector::new(RATE, 2);
        let mut collected = Vec::new();
        // Deliberately awkward sizes, none of them a multiple of the hop.
        let mut at = 0;
        for (i, size) in [2, 1_022, 7_778, 130, 65_536].iter().cycle().enumerate() {
            if at >= audio.len() {
                break;
            }
            let end = (at + size).min(audio.len());
            collected.extend_from_slice(piecemeal.push(&audio[at..end]));
            at = end;
            assert!(i < 10_000, "the loop must consume the audio");
        }

        assert_eq!(all.len(), collected.len());
        assert_eq!(all, collected);
    }

    #[test]
    fn an_onset_lands_where_it_was_synthesized() {
        // One burst, at a known frame. The detection function sees it in the
        // first hop whose window contains it, so the honest tolerance is one
        // hop -- and the check is that it is not two, or twelve.
        let mut detector = OnsetDetector::new(RATE, 2);
        let audio = clicks(48_000, 48_000, 12_000);
        let hops = hops_of(&mut detector, &audio);

        let fired: Vec<usize> = hops
            .iter()
            .enumerate()
            .filter(|(_, hop)| hop.onset > 0.0)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(fired.len(), 1, "one burst, one onset: {fired:?}");

        let expected = 12_000 / detector.hop();
        let seconds = |hop: usize| hop as f32 / detector.detection_hz();
        assert!(
            fired[0] >= expected && fired[0] <= expected + 1,
            "onset at {:.3}s, synthesized at {:.3}s",
            seconds(fired[0]),
            seconds(expected)
        );
    }

    #[test]
    fn every_click_of_a_click_track_is_one_onset() {
        // Ten clicks a second apart: ten onsets, not nine and not thirty.
        let mut detector = OnsetDetector::new(RATE, 2);
        let audio = clicks(RATE as usize * 10, RATE as usize, RATE as usize / 2);
        let hops = hops_of(&mut detector, &audio);
        let fired = hops.iter().filter(|hop| hop.onset > 0.0).count();
        assert_eq!(fired, 10, "one onset per click");
    }

    #[test]
    fn silence_has_no_onsets_and_no_centroid() {
        let mut detector = OnsetDetector::new(RATE, 2);
        let hops = hops_of(&mut detector, &vec![0.0; 48_000 * 2]);
        assert!(!hops.is_empty());
        assert!(hops.iter().all(|hop| hop.onset == 0.0));
        assert!(hops.iter().all(|hop| hop.transient == 0.0));
        assert!(hops.iter().all(|hop| hop.centroid_hz == 0.0));
    }

    #[test]
    fn a_steady_tone_is_not_a_stream_of_onsets() {
        // A held note changes nothing after its attack, so flux is ~0 and the
        // detector must stay quiet -- the failure that makes a "beat detector"
        // fire on every frame of a pad.
        let mut detector = OnsetDetector::new(RATE, 2);
        let audio = stereo(RATE as usize * 4, |n| {
            0.5 * (2.0 * std::f32::consts::PI * 440.0 * n as f32 / RATE as f32).sin()
        });
        let hops = hops_of(&mut detector, &audio);
        let fired = hops.iter().filter(|hop| hop.onset > 0.0).count();
        assert!(fired <= 1, "a held tone fired {fired} onsets");
    }

    #[test]
    fn the_centroid_has_an_analytic_answer() {
        // A tone reads its own frequency, within a bin.
        let mut detector = OnsetDetector::new(RATE, 2);
        let bin_hz = RATE as f32 / detector.ring.len() as f32;
        let audio = stereo(RATE as usize, |n| {
            (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / RATE as f32).sin()
        });
        let hops = hops_of(&mut detector, &audio);
        let centroid = hops[hops.len() - 1].centroid_hz;
        assert!(
            (centroid - 1000.0).abs() < bin_hz,
            "a 1kHz tone read {centroid:.1}Hz, bin is {bin_hz:.1}Hz"
        );

        // ...and white noise reads the middle of the band it fills, which for
        // a flat spectrum to Nyquist is Nyquist/2.
        let mut state = 99u32;
        let mut noisy = OnsetDetector::new(RATE, 2);
        let noise = stereo(RATE as usize, move |_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / 8_388_608.0 - 1.0
        });
        let hops = hops_of(&mut noisy, &noise);
        let centroid = hops[hops.len() - 1].centroid_hz;
        let expected = RATE as f32 / 4.0;
        assert!(
            (centroid - expected).abs() < expected * 0.1,
            "white noise read {centroid:.0}Hz, expected about {expected:.0}Hz"
        );
    }

    #[test]
    fn a_reset_forgets_the_spectrum_rather_than_reporting_it() {
        // The first hop after a break must not see the whole spectrum arriving
        // as one enormous change -- which is what a seek would look like to a
        // detector that kept its previous magnitudes.
        let mut detector = OnsetDetector::new(RATE, 2);
        let tone = stereo(RATE as usize, |n| {
            0.5 * (2.0 * std::f32::consts::PI * 440.0 * n as f32 / RATE as f32).sin()
        });
        hops_of(&mut detector, &tone);
        detector.reset();

        let hops = hops_of(&mut detector, &tone);
        assert_eq!(hops[0].detection, 0.0, "the first hop measures no change");
        assert_eq!(hops[0].onset, 0.0);
    }

    #[test]
    fn the_transient_figure_stays_in_range() {
        let mut detector = OnsetDetector::new(RATE, 2);
        let audio = clicks(RATE as usize * 4, RATE as usize / 2, 0);
        let hops = hops_of(&mut detector, &audio);
        assert!(hops.iter().all(|hop| (0.0..=1.0).contains(&hop.transient)));
        assert!(
            hops.iter().any(|hop| hop.transient > 0.9),
            "a click should reach the top of its own scale"
        );
    }

    #[test]
    fn a_quiet_mix_registers_as_much_as_a_loud_one() {
        // Compression's job (COMPRESSION above): the same music 20dB down is
        // the same music, and its onsets are the same onsets.
        let loud = clicks(RATE as usize * 4, RATE as usize / 2, 0);
        let quiet: Vec<f32> = loud.iter().map(|s| s * 0.1).collect();

        let count = |audio: &[f32]| {
            let mut detector = OnsetDetector::new(RATE, 2);
            hops_of(&mut detector, audio)
                .iter()
                .filter(|hop| hop.onset > 0.0)
                .count()
        };
        assert_eq!(count(&loud), count(&quiet));
    }
}
