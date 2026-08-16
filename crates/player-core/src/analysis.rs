//! The meters, and the drain loop that feeds them.
//!
//! Cross-target, and deliberately free of threads, devices and buffers: the
//! native analysis thread (`playback.rs`) and the web worker
//! (`player-rt-wasm`'s `WasmAnalyzer`) both own their own scheduling and both
//! call exactly these two methods. That is the same split `StreamingDecoder`
//! made in M2 -- one description of what the work *is*, two hosts that decide
//! when it runs -- and for the same reason: the two shells' bugs should not be
//! able to differ.
//!
//! M4 had one meter in here. M5's loudness and phase meters joined it, and the
//! split between them is worth naming: the spectrum and the goniometer are
//! *pictures of a window* and read `window` below, while loudness and
//! correlation are *measurements over time* and are fed every sample as it
//! arrives. That is the same distinction the tap draws between `window` and
//! `TapCursor` (M4 decision 1), one layer up -- and it is why `ingest` takes
//! everything drained rather than only what the FFT needs.

use crate::loudness::{LoudnessMeter, LoudnessReading};
use crate::phase::{goniometer_points, CorrelationMeter};
use crate::spectrum::SpectrumAnalyzer;

/// Frames the spectrum transforms at once. 2048 at 48kHz is ~23Hz of
/// resolution over a 43ms window: fine enough to separate bass notes, short
/// enough that the display still tracks the music.
pub const FFT_SIZE: usize = 2048;

/// Display bands. 64 across 20Hz-20kHz is ~6 per octave, which reads as a
/// spectrum rather than as a picket fence, and stays cheap to send to a UI at
/// frame rate.
pub const BAND_COUNT: usize = 64;

/// Points the goniometer publishes per frame. 256 of a 2048-frame window is
/// every eighth frame -- enough for the figure to read as a shape rather than a
/// dot cloud, and bounded so the cost of sending it across an IPC boundary at
/// frame rate is a property of the display rather than of the sample rate.
pub const GONIOMETER_POINTS: usize = 256;

/// The frequency axis the levels are reported against, without building a set
/// of meters to ask. Fixed for a given output rate.
pub fn band_centres(sample_rate: u32) -> Vec<f32> {
    crate::spectrum::band_centres(FFT_SIZE, sample_rate, BAND_COUNT)
}

/// Everything the meters read at one instant, in the units they measure in.
///
/// One struct rather than five accessors for `TransportState`'s reason: these
/// describe a single moment, and a display that fetched them separately could
/// show a momentary loudness from one instant against an integrated from
/// another (M5 decision 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterSnapshot {
    /// 400ms loudness, LUFS.
    pub momentary_lufs: f32,
    /// 3s loudness, LUFS.
    pub short_term_lufs: f32,
    /// Gated loudness since the last reset, LUFS.
    pub integrated_lufs: f32,
    /// Highest inter-sample peak since the last reset, dBTP.
    pub true_peak_db: f32,
    /// Stereo correlation: +1 mono, 0 uncorrelated, -1 inverted.
    pub correlation: f32,
}

/// Floats in a snapshot. The web build stores them in its shared buffer in the
/// order [`MeterSnapshot::to_array`] gives (mirrored by `METER` in
/// app/src/audioSab.ts), which is why the count is named here rather than
/// written out at each end.
pub const METER_FIELDS: usize = 5;

impl MeterSnapshot {
    /// The snapshot as a flat array, for the boundaries that cannot carry a
    /// struct: the web build's shared buffer, and wasm-bindgen's return values.
    pub fn to_array(&self) -> [f32; METER_FIELDS] {
        [
            self.momentary_lufs,
            self.short_term_lufs,
            self.integrated_lufs,
            self.true_peak_db,
            self.correlation,
        ]
    }

    /// What a display shows before anything has been measured.
    pub fn silent() -> Self {
        let quiet = LoudnessReading::silent();
        Self {
            momentary_lufs: quiet.momentary,
            short_term_lufs: quiet.short_term,
            integrated_lufs: quiet.integrated,
            true_peak_db: quiet.true_peak,
            correlation: 0.0,
        }
    }
}

/// Every meter, fed from one drain of the analysis tap.
pub struct Meters {
    spectrum: SpectrumAnalyzer,
    loudness: LoudnessMeter,
    correlation: CorrelationMeter,
    channels: u16,
    /// The newest `FFT_SIZE` frames, interleaved -- the spectrum's window,
    /// carried across calls because a drain at 60Hz delivers ~800 frames while
    /// the transform wants 2048.
    window: Vec<f32>,
    capacity: usize,
}

impl Meters {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let channels = channels.max(1);
        let spectrum = SpectrumAnalyzer::new(FFT_SIZE, sample_rate, BAND_COUNT);
        let capacity = spectrum.fft_size() * usize::from(channels);
        Self {
            spectrum,
            loudness: LoudnessMeter::new(sample_rate, channels),
            correlation: CorrelationMeter::new(sample_rate, channels),
            channels,
            window: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds audio drained from the tap.
    ///
    /// `discontinuity` says the samples do not continue the previous ones (the
    /// reader was lapped, a seek cleared the tap, or a track handed over). The
    /// window is dropped rather than spliced: an FFT across a seek would show a
    /// moment of the track that never played, and an integrated loudness across
    /// one would report a number for a timeline nobody heard (M5 decision 2).
    pub fn ingest(&mut self, samples: &[f32], discontinuity: bool) {
        if discontinuity {
            self.window.clear();
            self.spectrum.reset();
            self.loudness.reset();
            self.correlation.reset();
            // And this drain is dropped rather than measured. A break is only
            // noticed once the drain containing it has already been read, so
            // these samples straddle it -- at a gapless hand-off, the first
            // milliseconds here are the *previous* track's. Measuring them
            // would leave the outgoing track's peak latched into the incoming
            // track's peak hold, which is what the M5 smoke check caught
            // (2026-08-16: a -33 LUFS track reporting the -23 dBTP of the one
            // before it). The cost is up to one drain -- 16ms natively, ~20ms
            // on the web -- of audio not measured at each break, which is
            // below a single sub-block of the loudness measurement.
            return;
        }
        // Every sample, not just the window's worth: an integrated measurement
        // that skipped whatever fell between two drains would be reporting on
        // audio it never saw.
        self.loudness.push(samples);
        self.correlation.push(samples);
        self.window.extend_from_slice(samples);
        if self.window.len() > self.capacity {
            // Keep the newest window's worth. Copying up to 4096 floats per
            // frame is nothing next to the FFT that follows it.
            let excess = self.window.len() - self.capacity;
            self.window.drain(..excess);
        }
    }

    /// Runs the meters over what has been ingested. `dt_seconds` is real
    /// elapsed time, so the display decays at the same rate whatever the
    /// caller's cadence is.
    pub fn update(&mut self, dt_seconds: f32) -> &[f32] {
        self.spectrum
            .analyze(&self.window, self.channels, dt_seconds)
    }

    /// Latest levels without recomputing.
    pub fn levels(&self) -> &[f32] {
        self.spectrum.levels()
    }

    /// What the loudness and phase meters read as of the last `ingest`.
    ///
    /// Not part of `update`, because these have no smoothing to advance: they
    /// are windows over the audio itself, so asking twice between drains gives
    /// the same answer rather than a decayed one.
    pub fn snapshot(&self) -> MeterSnapshot {
        let loudness = self.loudness.reading();
        MeterSnapshot {
            momentary_lufs: loudness.momentary,
            short_term_lufs: loudness.short_term,
            integrated_lufs: loudness.integrated,
            true_peak_db: loudness.true_peak,
            correlation: self.correlation.correlation(),
        }
    }

    /// The goniometer's points for the window currently held: interleaved
    /// side/mid pairs (see `phase::goniometer_points`).
    pub fn goniometer(&self, max_points: usize) -> Vec<f32> {
        goniometer_points(&self.window, self.channels, max_points)
    }

    /// Centre frequency of each band, fixed for the analyser's life.
    pub fn band_centres(&self) -> &[f32] {
        self.spectrum.band_centres()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_never_grows_past_one_fft() {
        let mut meters = Meters::new(48_000, 2);
        for _ in 0..10 {
            meters.ingest(&vec![0.1; 4096], false);
        }
        assert_eq!(meters.window.len(), FFT_SIZE * 2);
    }

    #[test]
    fn a_discontinuity_throws_the_window_and_the_drain_away() {
        // Both halves matter. The window has to go because audio from before
        // the break must not be analysed with what follows it -- and the drain
        // carrying the break has to go with it, because it holds audio from
        // both sides and nothing here can say where one ends.
        let mut meters = Meters::new(48_000, 2);
        meters.ingest(&vec![0.5; 1024], false);
        meters.ingest(&[0.5, 0.5], true);
        assert_eq!(meters.window.len(), 0);

        // ...and the next drain is measured normally.
        meters.ingest(&[0.5, 0.5], false);
        assert_eq!(meters.window.len(), 2);
    }

    /// One second of interleaved stereo sine at 1kHz.
    fn tone(amplitude: f32, right_gain: f32) -> Vec<f32> {
        (0..48_000)
            .flat_map(|n| {
                let s =
                    amplitude * (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin();
                [s, s * right_gain]
            })
            .collect()
    }

    #[test]
    fn a_discontinuity_resets_every_meter_that_holds_state() {
        // The spectrum's window is not the only thing that must not be spliced
        // across a seek or a hand-off: an integrated loudness carried over one
        // describes a timeline that was never played (M5 decision 2).
        let mut meters = Meters::new(48_000, 2);
        meters.ingest(&tone(0.5, 1.0), false);
        let measured = meters.snapshot();
        assert!(measured.integrated_lufs > -20.0, "{measured:?}");
        assert!(measured.true_peak_db > -10.0, "{measured:?}");

        meters.ingest(&[], true);
        let after = meters.snapshot();
        assert_eq!(after, MeterSnapshot::silent());
    }

    #[test]
    fn the_meters_read_the_same_audio_the_spectrum_does() {
        // One drain, several readers: the loudness, the correlation and the
        // goniometer all have to come out of the same `ingest` the spectrum
        // does, or a display could show four meters describing four moments.
        let mut meters = Meters::new(48_000, 2);
        meters.ingest(&tone(0.5, 1.0), false);
        meters.update(0.016);

        let measured = meters.snapshot();
        assert!(measured.momentary_lufs > -20.0, "{measured:?}");
        assert!((measured.correlation - 1.0).abs() < 0.01, "{measured:?}");
        assert!(
            meters.levels().iter().copied().fold(f32::MIN, f32::max) > -10.0,
            "the spectrum saw it too"
        );
        let points = meters.goniometer(GONIOMETER_POINTS);
        assert_eq!(points.len(), GONIOMETER_POINTS * 2);
        // Mono material has no side component, so the figure stands up.
        assert!(points.chunks_exact(2).all(|pair| pair[0].abs() < 1e-6));

        // The same audio with one channel inverted: the phase meters notice
        // and the loudness does not, which is the point of having both.
        let mut inverted = Meters::new(48_000, 2);
        inverted.ingest(&tone(0.5, -1.0), false);
        let flipped = inverted.snapshot();
        assert!(
            (flipped.correlation + 1.0).abs() < 0.01,
            "an inverted pair should read -1, read {}",
            flipped.correlation
        );
        assert!(
            (flipped.momentary_lufs - measured.momentary_lufs).abs() < 0.01,
            "inverting a channel changes no channel's loudness"
        );
        assert!(inverted
            .goniometer(GONIOMETER_POINTS)
            .chunks_exact(2)
            .all(|pair| pair[1].abs() < 1e-6));
    }

    #[test]
    fn levels_are_reported_per_band() {
        let mut meters = Meters::new(48_000, 2);
        let tone: Vec<f32> = (0..FFT_SIZE)
            .flat_map(|n| {
                let s = (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin();
                [s, s]
            })
            .collect();
        meters.ingest(&tone, false);
        let levels = meters.update(0.016).to_vec();
        assert_eq!(levels.len(), BAND_COUNT);
        assert_eq!(meters.band_centres().len(), BAND_COUNT);
        assert!(
            levels.iter().copied().fold(f32::MIN, f32::max) > -3.0,
            "a full-scale tone should show up somewhere"
        );
    }
}
