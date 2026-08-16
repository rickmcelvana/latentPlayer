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
//! M4 has one meter in here. M5's loudness and phase meters join it: they read
//! the same drained audio, which is why `ingest` takes every sample rather than
//! only what the spectrum's window needs.

use crate::spectrum::SpectrumAnalyzer;

/// Frames the spectrum transforms at once. 2048 at 48kHz is ~23Hz of
/// resolution over a 43ms window: fine enough to separate bass notes, short
/// enough that the display still tracks the music.
pub const FFT_SIZE: usize = 2048;

/// Display bands. 64 across 20Hz-20kHz is ~6 per octave, which reads as a
/// spectrum rather than as a picket fence, and stays cheap to send to a UI at
/// frame rate.
pub const BAND_COUNT: usize = 64;

/// The frequency axis the levels are reported against, without building a set
/// of meters to ask. Fixed for a given output rate.
pub fn band_centres(sample_rate: u32) -> Vec<f32> {
    crate::spectrum::band_centres(FFT_SIZE, sample_rate, BAND_COUNT)
}

/// Every meter, fed from one drain of the analysis tap.
pub struct Meters {
    spectrum: SpectrumAnalyzer,
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
            channels,
            window: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds audio drained from the tap.
    ///
    /// `discontinuity` says the samples do not continue the previous ones (the
    /// reader was lapped, or a seek cleared the tap). The window is dropped
    /// rather than spliced: an FFT across a seek would show a moment of the
    /// track that never played.
    pub fn ingest(&mut self, samples: &[f32], discontinuity: bool) {
        if discontinuity {
            self.window.clear();
            self.spectrum.reset();
        }
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
    fn a_discontinuity_throws_the_window_away() {
        let mut meters = Meters::new(48_000, 2);
        meters.ingest(&vec![0.5; 1024], false);
        meters.ingest(&[0.5, 0.5], true);
        assert_eq!(
            meters.window.len(),
            2,
            "audio from before the break must not be analysed with what follows it"
        );
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
