//! Real-time spectrum analysis: a window of played audio in, a row of dBFS
//! levels out, one per log-spaced display band.
//!
//! Cross-target on purpose (M4 decision 3). Both shells call this same code --
//! natively from the analysis thread, on the web from the worker via
//! `player-rt-wasm` -- so the two cannot drift apart in what they measure or
//! how it looks. Phase 0's two worst bugs were both web-only; a second
//! implementation of this in TypeScript would be that mistake again, with a
//! slower feedback loop.
//!
//! What it deliberately does *not* do: decide a display floor, pick colours, or
//! smooth for a particular frame rate it assumes. Levels come out as dBFS and
//! the caller passes the real elapsed time, so the same analyser behaves the
//! same at 30fps and 144fps.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Level reported for a band with no energy in it, and the floor everything
/// else is clamped to. Below the noise floor of any real 16-bit source, so it
/// reads as "nothing here" rather than as a number worth drawing.
pub const FLOOR_DB: f32 = -120.0;

/// How fast a band is allowed to fall, in dB per second. Rises are instant:
/// a transient that peaks for one frame should be seen at its real height,
/// while an unsmoothed *fall* is what makes a raw FFT display flicker.
/// 30dB/s is the usual analyser feel -- a full-scale peak decays to the floor
/// in about four seconds.
const RELEASE_DB_PER_SECOND: f32 = 30.0;

/// Lowest band edge. Below this is subsonic content no monitoring chain
/// reproduces, and giving it screen width costs the octaves that matter.
const MIN_HZ: f32 = 20.0;

/// Highest band edge, subject to Nyquist -- kept just under it because the
/// last bins of any real FFT sit in the anti-alias filter's transition band.
const MAX_HZ: f32 = 20_000.0;

/// One display band: the FFT bins that fall inside it.
struct Band {
    lo: usize,
    hi: usize,
    centre_hz: f32,
}

pub struct SpectrumAnalyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    /// Hann window, and the sum of it -- the normalisation that makes a
    /// full-scale sine read 0dBFS rather than "whatever this FFT length and
    /// window happen to scale to".
    window: Vec<f32>,
    window_sum: f32,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    magnitudes: Vec<f32>,
    bands: Vec<Band>,
    levels: Vec<f32>,
    centres: Vec<f32>,
    sample_rate: u32,
}

impl SpectrumAnalyzer {
    /// `fft_size` is rounded up to a power of two. 2048 at 48kHz is ~23Hz of
    /// resolution over a 43ms window -- fine enough to separate bass notes,
    /// short enough to still look live.
    pub fn new(fft_size: usize, sample_rate: u32, band_count: usize) -> Self {
        let fft_size = fft_size.max(64).next_power_of_two();
        let band_count = band_count.max(1);
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);

        // Periodic Hann (n/N, not n/(N-1)): this window is analysing a
        // continuous signal, not a finite record, and the periodic form is the
        // one whose bins don't leak into their neighbours.
        let window: Vec<f32> = (0..fft_size).map(|n| hann(n, fft_size)).collect();
        let window_sum = window.iter().sum::<f32>().max(f32::MIN_POSITIVE);

        let input = fft.make_input_vec();
        let output = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let magnitudes = vec![0.0; output.len()];

        let nyquist = sample_rate as f32 / 2.0;
        let max_hz = MAX_HZ.min(nyquist * 0.98);
        let bands = build_bands(band_count, fft_size, sample_rate, max_hz);
        let centres = bands.iter().map(|b| b.centre_hz).collect();

        Self {
            fft,
            window,
            window_sum,
            input,
            output,
            scratch,
            magnitudes,
            bands,
            levels: vec![FLOOR_DB; band_count],
            centres,
            sample_rate,
        }
    }

    /// Frames one FFT covers.
    pub fn fft_size(&self) -> usize {
        self.window.len()
    }

    pub fn band_count(&self) -> usize {
        self.bands.len()
    }

    /// Centre frequency of each band, for axis labels. Fixed for the life of
    /// the analyser, so a caller can fetch it once.
    pub fn band_centres(&self) -> &[f32] {
        &self.centres
    }

    /// The most recent levels, in dBFS, one per band.
    pub fn levels(&self) -> &[f32] {
        &self.levels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Drops the display back to the floor -- after a seek, a track change, or
    /// a tap discontinuity, where holding the previous decay would draw a
    /// moment of audio that is no longer playing.
    pub fn reset(&mut self) {
        self.levels.fill(FLOOR_DB);
    }

    /// Analyses the newest `fft_size` frames of `interleaved` and returns the
    /// smoothed levels.
    ///
    /// `channels` is the interleaving width (the output's, since that is what
    /// the tap holds) and the channels are summed to mono -- M4 decision 5.
    /// `dt_seconds` is the real time since the last call, which is what makes
    /// the decay rate a property of the analyser rather than of how often the
    /// caller happens to run.
    ///
    /// Short input is analysed as-is, zero-padded: at the very start of a track
    /// there genuinely isn't a full window yet, and reporting the floor for the
    /// first 40ms looks like a bug even though it isn't.
    pub fn analyze(&mut self, interleaved: &[f32], channels: u16, dt_seconds: f32) -> &[f32] {
        let channels = usize::from(channels.max(1));
        let fft_size = self.window.len();
        let frames = interleaved.len() / channels;
        let take = frames.min(fft_size);
        let skip = (frames - take) * channels;

        self.input.fill(0.0);
        // Right-aligned, so a partial window is padded at the front and the
        // newest sample always sits at the end of the analysis window.
        //
        // A short record gets a Hann of its own length rather than a slice of
        // the full one, and is normalised by what was actually applied. Slicing
        // the full window would put the audio under its fade-out and read ~20dB
        // low -- which is exactly what the first frames of every track are.
        let pad = fft_size - take;
        let partial = take < fft_size;
        let mut applied_sum = 0.0f32;
        for frame in 0..take {
            let base = skip + frame * channels;
            let sum: f32 = interleaved[base..base + channels].iter().sum();
            let w = if partial {
                hann(frame, take)
            } else {
                self.window[frame]
            };
            applied_sum += w;
            self.input[pad + frame] = (sum / channels as f32) * w;
        }
        let applied_sum = if partial {
            applied_sum.max(f32::MIN_POSITIVE)
        } else {
            self.window_sum
        };

        // The only failure modes are wrong slice lengths, which are this
        // struct's own invariants -- so a failure here is a bug in this file,
        // not a condition a caller could act on.
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            return &self.levels;
        }

        // 2/window_sum is the normalisation that puts a full-scale sine at
        // 0dBFS: a real signal's energy is split between its positive and
        // negative frequencies, and the window scales everything by its own
        // sum.
        let scale = 2.0 / applied_sum;
        for (magnitude, bin) in self.magnitudes.iter_mut().zip(self.output.iter()) {
            *magnitude = bin.norm() * scale;
        }

        let decay = RELEASE_DB_PER_SECOND * dt_seconds.max(0.0);
        for (band, level) in self.bands.iter().zip(self.levels.iter_mut()) {
            // The loudest bin in the band, not the average: a band several bins
            // wide should show the tone that's in it, and averaging buries a
            // sharp peak under its silent neighbours.
            let peak = self.magnitudes[band.lo..=band.hi]
                .iter()
                .copied()
                .fold(0.0f32, f32::max);
            let db = if peak > 0.0 {
                (20.0 * peak.log10()).max(FLOOR_DB)
            } else {
                FLOOR_DB
            };
            *level = if db >= *level {
                db
            } else {
                (*level - decay).max(db)
            };
        }
        &self.levels
    }
}

/// The band centre frequencies an analyser of this shape would report, without
/// building one (which plans an FFT). For a caller that only wants the axis.
pub fn band_centres(fft_size: usize, sample_rate: u32, band_count: usize) -> Vec<f32> {
    let fft_size = fft_size.max(64).next_power_of_two();
    let max_hz = MAX_HZ.min(sample_rate as f32 / 2.0 * 0.98);
    build_bands(band_count.max(1), fft_size, sample_rate, max_hz)
        .iter()
        .map(|b| b.centre_hz)
        .collect()
}

/// Periodic Hann of length `len`, at position `n`.
fn hann(n: usize, len: usize) -> f32 {
    let phase = 2.0 * std::f32::consts::PI * n as f32 / len as f32;
    0.5 * (1.0 - phase.cos())
}

/// Log-spaced bands from `MIN_HZ` to `max_hz`, each mapped to the FFT bins
/// whose centre frequency falls inside it -- a half-open `[lo_hz, hi_hz)`, so
/// **no bin belongs to two bands**.
///
/// That exclusivity is load-bearing above the bass, and getting it wrong is
/// invisible until something depends on which bar is tallest. Rounding the
/// edges outwards (floor the bottom, ceil the top) instead put the bin holding
/// a 440Hz tone in both band 28 and band 29, which then read exactly the same
/// level -- so "the loudest band" came down to how a max happened to break
/// ties, and the two shells broke it differently (found by the M4 smoke checks,
/// 2026-08-16: 433Hz on the web, 483Hz natively, same audio).
///
/// The low bands genuinely are narrower than one bin -- at 48kHz/2048 the bins
/// are 23Hz apart, so everything below ~200Hz has fewer bins than bands. Those
/// bands collapse onto their nearest bin and do overlap, unavoidably: a display
/// with gaps in the bass would be worse than one where neighbouring bass bars
/// agree.
fn build_bands(band_count: usize, fft_size: usize, sample_rate: u32, max_hz: f32) -> Vec<Band> {
    let bin_hz = sample_rate as f32 / fft_size as f32;
    let last_bin = fft_size / 2;
    let ratio = (max_hz / MIN_HZ).powf(1.0 / band_count as f32);

    (0..band_count)
        .map(|i| {
            let lo_hz = MIN_HZ * ratio.powi(i as i32);
            let hi_hz = lo_hz * ratio;
            let first = (lo_hz / bin_hz).ceil() as usize;
            let last = ((hi_hz / bin_hz).ceil() as usize).saturating_sub(1);
            let (lo, hi) = if last < first {
                // Narrower than the bin spacing: nearest bin, shared with a
                // neighbour rather than left blank.
                let nearest = ((lo_hz * hi_hz).sqrt() / bin_hz).round() as usize;
                (nearest, nearest)
            } else {
                (first, last)
            };
            Band {
                lo: lo.clamp(1, last_bin),
                hi: hi.clamp(lo.clamp(1, last_bin), last_bin),
                // Geometric centre: on a log axis that is the middle of the
                // bar, where the arithmetic mean would sit visibly right of it.
                centre_hz: (lo_hz * hi_hz).sqrt(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Interleaved stereo sine at `hz`, `amplitude` in linear full scale.
    fn sine(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s =
                    amplitude * (2.0 * std::f32::consts::PI * hz * n as f32 / RATE as f32).sin();
                [s, s]
            })
            .collect()
    }

    /// Index of the band whose range contains `hz`.
    fn band_at(analyzer: &SpectrumAnalyzer, hz: f32) -> usize {
        analyzer
            .band_centres()
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (a.log10() - hz.log10())
                    .abs()
                    .total_cmp(&(b.log10() - hz.log10()).abs())
            })
            .map(|(i, _)| i)
            .expect("analyser always has bands")
    }

    #[test]
    fn a_full_scale_sine_reads_about_zero_dbfs() {
        let mut analyzer = SpectrumAnalyzer::new(2048, RATE, 64);
        let audio = sine(1000.0, 1.0, 4096);
        let levels = analyzer.analyze(&audio, 2, 0.016).to_vec();

        let peak = band_at(&analyzer, 1000.0);
        assert!(
            (levels[peak] - 0.0).abs() < 1.5,
            "1kHz at full scale should read ~0dBFS, got {:.2}",
            levels[peak]
        );
    }

    #[test]
    fn amplitude_maps_to_decibels() {
        // Half amplitude is -6dB, whatever the window and FFT length are --
        // this is the assertion that catches a normalisation that happens to
        // look right at one level.
        let mut analyzer = SpectrumAnalyzer::new(2048, RATE, 64);
        let peak = band_at(&analyzer, 1000.0);

        let full = analyzer.analyze(&sine(1000.0, 1.0, 4096), 2, 0.016)[peak];
        analyzer.reset();
        let half = analyzer.analyze(&sine(1000.0, 0.5, 4096), 2, 0.016)[peak];

        assert!(
            (full - half - 6.02).abs() < 0.5,
            "halving amplitude should drop ~6dB: {full:.2} -> {half:.2}"
        );
    }

    #[test]
    fn energy_lands_in_the_band_that_holds_the_tone() {
        let mut analyzer = SpectrumAnalyzer::new(2048, RATE, 64);
        let levels = analyzer
            .analyze(&sine(1000.0, 1.0, 4096), 2, 0.016)
            .to_vec();

        let peak = band_at(&analyzer, 1000.0);
        let loudest = levels
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(i, _)| i)
            .expect("levels are non-empty");
        assert_eq!(loudest, peak, "the loudest band should be the one at 1kHz");

        // Two octaves away there should be essentially nothing -- this is what
        // the window is for, and a rectangular window would smear well past it.
        let far = band_at(&analyzer, 250.0);
        assert!(
            levels[far] < levels[peak] - 60.0,
            "leakage two octaves down: {:.1}dB against a {:.1}dB peak",
            levels[far],
            levels[peak]
        );
    }

    #[test]
    fn bands_wide_enough_to_hold_a_bin_do_not_share_one() {
        // Above the bass, where there are more bins than bands, a bin must
        // belong to exactly one band -- otherwise two bars read identically
        // and "the loudest band" is decided by tie-breaking rather than by the
        // audio.
        let analyzer = SpectrumAnalyzer::new(2048, RATE, 64);
        let mut previous: Option<&Band> = None;
        for band in &analyzer.bands {
            if let Some(previous) = previous {
                let multi_bin = previous.hi > previous.lo || band.hi > band.lo;
                if multi_bin {
                    assert!(
                        band.lo > previous.hi,
                        "bands sharing bin {}..={} and {}..={}",
                        previous.lo,
                        previous.hi,
                        band.lo,
                        band.hi
                    );
                }
            }
            previous = Some(band);
        }
    }

    #[test]
    fn silence_reads_the_floor() {
        let mut analyzer = SpectrumAnalyzer::new(1024, RATE, 32);
        let levels = analyzer.analyze(&vec![0.0; 4096], 2, 0.016);
        assert!(levels.iter().all(|&db| db == FLOOR_DB));
    }

    #[test]
    fn levels_fall_at_the_release_rate_and_rise_instantly() {
        let mut analyzer = SpectrumAnalyzer::new(1024, RATE, 32);
        let peak = band_at(&analyzer, 1000.0);

        let loud = analyzer.analyze(&sine(1000.0, 1.0, 2048), 2, 0.016)[peak];
        // One second of silence: exactly one release-rate's worth of decay.
        let after = analyzer.analyze(&vec![0.0; 2048], 2, 1.0)[peak];
        assert!(
            (loud - after - RELEASE_DB_PER_SECOND).abs() < 0.5,
            "one second of silence should drop {RELEASE_DB_PER_SECOND}dB: {loud:.1} -> {after:.1}"
        );

        // ...and the tone coming back is not smoothed at all.
        let back = analyzer.analyze(&sine(1000.0, 1.0, 2048), 2, 0.016)[peak];
        assert!(
            (back - loud).abs() < 0.5,
            "a returning tone should read its real level immediately: {back:.1} vs {loud:.1}"
        );
    }

    #[test]
    fn bands_ascend_and_stay_inside_the_audible_range() {
        let analyzer = SpectrumAnalyzer::new(2048, RATE, 64);
        let centres = analyzer.band_centres();
        assert_eq!(centres.len(), 64);
        assert!(centres[0] >= MIN_HZ);
        assert!(*centres.last().expect("64 bands") <= RATE as f32 / 2.0);
        assert!(
            centres.windows(2).all(|w| w[1] > w[0]),
            "band centres must ascend"
        );
    }

    #[test]
    fn a_partial_window_is_analysed_rather_than_refused() {
        // 512 frames into a 2048-point analyser: the start of every track.
        let mut analyzer = SpectrumAnalyzer::new(2048, RATE, 64);
        let levels = analyzer.analyze(&sine(1000.0, 1.0, 512), 2, 0.016).to_vec();
        assert!(
            levels[band_at(&analyzer, 1000.0)] > -20.0,
            "a quarter window should still show the tone"
        );
    }

    #[test]
    fn mono_and_stereo_of_the_same_signal_agree() {
        // The mono sum is an average, not a sum: the same tone in both channels
        // must not read 6dB louder than in one.
        let mut stereo = SpectrumAnalyzer::new(2048, RATE, 64);
        let mut mono = SpectrumAnalyzer::new(2048, RATE, 64);
        let peak = band_at(&stereo, 1000.0);

        let interleaved = sine(1000.0, 1.0, 4096);
        let single: Vec<f32> = interleaved.iter().step_by(2).copied().collect();

        let a = stereo.analyze(&interleaved, 2, 0.016)[peak];
        let b = mono.analyze(&single, 1, 0.016)[peak];
        assert!((a - b).abs() < 0.01, "{a:.3} vs {b:.3}");
    }
}
