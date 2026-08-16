//! Stereo phase: the correlation meter and the goniometer's points.
//!
//! Two views of the same question -- how the left and right channels relate --
//! answered at two rates. Correlation is a number that has to be smoothed over
//! a window to mean anything, and so holds state; the goniometer is a picture of
//! one instant, and so is a pure function of a window of audio.
//!
//! Both live in Rust rather than in the display for M4 decision 3's reason: the
//! arithmetic is short enough that two implementations would look identical and
//! still disagree -- which is exactly what happened to the two shells' band
//! edges in M4.

/// How long the correlation meter averages over. 300ms is the usual broadcast
/// correlation-meter constant: long enough that a single bass cycle doesn't
/// swing the needle, short enough to follow a mix as it moves.
const CORRELATION_SECONDS: f64 = 0.3;

/// Below this the correlation of a channel pair is meaningless -- there is no
/// signal to correlate -- and the meter reads 0 rather than amplifying noise
/// into a confident answer.
const SILENCE_POWER: f64 = 1e-12;

/// Phase correlation between the first two channels.
///
/// +1 is mono (the two channels identical), 0 is uncorrelated (wide stereo), -1
/// is one channel inverted against the other -- the case that cancels when a
/// listener's setup sums to mono, and the reason this meter exists at all.
///
/// A one-pole average rather than a sliding window: it costs three accumulators
/// instead of a ring of them, and a correlation meter's needle is a smoothed
/// quantity by nature. The decay is computed from the number of frames actually
/// pushed, so the time constant is 300ms of *audio* however the caller happens
/// to chunk it.
pub struct CorrelationMeter {
    sample_rate: u32,
    channels: usize,
    left_right: f64,
    left_left: f64,
    right_right: f64,
}

impl CorrelationMeter {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            channels: usize::from(channels.max(1)),
            left_right: 0.0,
            left_left: 0.0,
            right_right: 0.0,
        }
    }

    /// Forgets what it has averaged -- after a seek or a track change, where
    /// carrying the old mix's correlation into the new one would report a
    /// relationship between two pieces of audio that were never played
    /// together.
    pub fn reset(&mut self) {
        self.left_right = 0.0;
        self.left_left = 0.0;
        self.right_right = 0.0;
    }

    /// Adds interleaved audio in the meter's channel layout.
    pub fn push(&mut self, interleaved: &[f32]) {
        if self.channels < 2 || interleaved.len() < self.channels {
            return;
        }
        let mut left_right = 0.0;
        let mut left_left = 0.0;
        let mut right_right = 0.0;
        let mut frames = 0usize;
        for frame in interleaved.chunks_exact(self.channels) {
            let left = f64::from(frame[0]);
            let right = f64::from(frame[1]);
            left_right += left * right;
            left_left += left * left;
            right_right += right * right;
            frames += 1;
        }
        if frames == 0 {
            return;
        }

        // One pole, applied for as many samples as arrived: the state keeps
        // `(1 - decay_per_sample)` of the incoming sums, so a chunk of any
        // length lands at the same place a sample-by-sample loop would.
        let per_sample = (-1.0 / (CORRELATION_SECONDS * f64::from(self.sample_rate))).exp();
        let decay = per_sample.powi(frames as i32);
        let weight = 1.0 - per_sample;
        self.left_right = self.left_right * decay + left_right * weight;
        self.left_left = self.left_left * decay + left_left * weight;
        self.right_right = self.right_right * decay + right_right * weight;
    }

    /// The current correlation, in `-1.0..=1.0`.
    ///
    /// A mono output reads +1: one channel is perfectly correlated with itself,
    /// which is the honest answer rather than "unknown".
    pub fn correlation(&self) -> f32 {
        if self.channels < 2 {
            return 1.0;
        }
        if self.left_left < SILENCE_POWER || self.right_right < SILENCE_POWER {
            return 0.0;
        }
        let denominator = (self.left_left * self.right_right).sqrt();
        ((self.left_right / denominator) as f32).clamp(-1.0, 1.0)
    }
}

/// The goniometer's points for a window of audio: interleaved `x, y` pairs,
/// oldest first.
///
/// The axes are the mix's, not the channels': `x` is the side signal
/// `(L - R)/sqrt(2)` and `y` the mid `(L + R)/sqrt(2)`. That is the 45-degree
/// rotation every goniometer draws, and it is what makes the display readable
/// -- mono material stands up as a vertical line, an out-of-phase pair lies flat
/// as a horizontal one, and a wide mix fills the middle.
///
/// The rotation is the orthonormal one, so the numbers are the real mid and
/// side amplitudes: a full-scale sample in one channel plots at 0.707, and
/// full-scale mono at 1.414. Scaling that onto a canvas is the display's
/// business, the same way the spectrum's floor is (M4 decision 4) -- rescaling
/// here would make the two channels' relationship depend on which of them was
/// louder.
///
/// Decimated to at most `max_points` frames by taking every nth, rather than by
/// averaging: averaging adjacent frames would pull a wide signal toward the
/// centre and quietly make every mix look more mono than it is.
pub fn goniometer_points(interleaved: &[f32], channels: u16, max_points: usize) -> Vec<f32> {
    let channels = usize::from(channels.max(1));
    let frames = interleaved.len() / channels;
    if frames == 0 || max_points == 0 {
        return Vec::new();
    }
    let stride = frames.div_ceil(max_points).max(1);
    let scale = std::f32::consts::FRAC_1_SQRT_2;

    let mut points = Vec::with_capacity((frames / stride + 1) * 2);
    let mut frame = 0;
    while frame < frames {
        let base = frame * channels;
        let left = interleaved[base];
        // A mono output has no stereo field to draw: everything sits on the
        // centre line, which is the truthful picture of it.
        let right = if channels > 1 {
            interleaved[base + 1]
        } else {
            left
        };
        points.push((left - right) * scale);
        points.push((left + right) * scale);
        frame += stride;
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Interleaved stereo, right channel scaled by `right_gain` (negative for
    /// an inverted channel).
    fn tone(hz: f32, right_gain: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = (2.0 * std::f32::consts::PI * hz * n as f32 / RATE as f32).sin();
                [s, s * right_gain]
            })
            .collect()
    }

    #[test]
    fn identical_channels_read_plus_one() {
        let mut meter = CorrelationMeter::new(RATE, 2);
        meter.push(&tone(440.0, 1.0, RATE as usize));
        assert!(
            (meter.correlation() - 1.0).abs() < 0.001,
            "mono material should read +1, read {}",
            meter.correlation()
        );
    }

    #[test]
    fn an_inverted_channel_reads_minus_one() {
        // The failure this meter exists to catch: audio that vanishes the
        // moment anything sums it to mono.
        let mut meter = CorrelationMeter::new(RATE, 2);
        meter.push(&tone(440.0, -1.0, RATE as usize));
        assert!(
            (meter.correlation() + 1.0).abs() < 0.001,
            "an inverted channel should read -1, read {}",
            meter.correlation()
        );
    }

    #[test]
    fn unrelated_channels_read_near_zero() {
        // Two tones an octave apart, over a whole number of cycles of both:
        // orthogonal, so their correlation is zero by construction rather than
        // by luck.
        let frames = RATE as usize;
        let audio: Vec<f32> = (0..frames)
            .flat_map(|n| {
                let t = n as f32 / RATE as f32;
                [
                    (2.0 * std::f32::consts::PI * 440.0 * t).sin(),
                    (2.0 * std::f32::consts::PI * 880.0 * t).sin(),
                ]
            })
            .collect();
        let mut meter = CorrelationMeter::new(RATE, 2);
        meter.push(&audio);
        assert!(
            meter.correlation().abs() < 0.1,
            "unrelated channels should read ~0, read {}",
            meter.correlation()
        );
    }

    #[test]
    fn silence_reads_zero_rather_than_a_confident_answer() {
        let mut meter = CorrelationMeter::new(RATE, 2);
        meter.push(&vec![0.0; RATE as usize * 2]);
        assert_eq!(meter.correlation(), 0.0);
    }

    #[test]
    fn a_mono_output_reads_plus_one() {
        let mut meter = CorrelationMeter::new(RATE, 1);
        meter.push(&vec![0.5; RATE as usize]);
        assert_eq!(meter.correlation(), 1.0);
    }

    #[test]
    fn chunking_does_not_change_the_answer() {
        // The meter is fed whatever a drain happens to hold -- 800 frames at
        // 60Hz natively, thousands after a slow frame on the web. The needle
        // must not depend on that.
        let audio = tone(440.0, 0.5, RATE as usize);
        let mut whole = CorrelationMeter::new(RATE, 2);
        whole.push(&audio);

        let mut chunked = CorrelationMeter::new(RATE, 2);
        for chunk in audio.chunks(997 * 2) {
            chunked.push(chunk);
        }
        assert!(
            (whole.correlation() - chunked.correlation()).abs() < 0.001,
            "{} in one push, {} in chunks",
            whole.correlation(),
            chunked.correlation()
        );
    }

    #[test]
    fn a_reset_forgets_the_previous_mix() {
        let mut meter = CorrelationMeter::new(RATE, 2);
        meter.push(&tone(440.0, -1.0, RATE as usize));
        meter.reset();
        assert_eq!(meter.correlation(), 0.0);
    }

    #[test]
    fn mono_material_stands_up_the_centre_line() {
        // The rotation's whole purpose: identical channels have no side
        // component at all, so every point sits on x = 0.
        let points = goniometer_points(&tone(440.0, 1.0, 1024), 2, 256);
        assert_eq!(points.len(), 256 * 2);
        for pair in points.chunks_exact(2) {
            assert!(
                pair[0].abs() < 1e-6,
                "side energy in a mono signal: {pair:?}"
            );
        }
        assert!(
            points.chunks_exact(2).any(|pair| pair[1].abs() > 0.5),
            "the mid axis should carry the signal"
        );
    }

    #[test]
    fn an_inverted_channel_lies_flat() {
        let points = goniometer_points(&tone(440.0, -1.0, 1024), 2, 256);
        for pair in points.chunks_exact(2) {
            assert!(
                pair[1].abs() < 1e-6,
                "mid energy in an inverted pair: {pair:?}"
            );
        }
        assert!(points.chunks_exact(2).any(|pair| pair[0].abs() > 0.5));
    }

    #[test]
    fn the_rotation_preserves_amplitude() {
        // Orthonormal, so the extremes are known and a display can be built
        // against them: full-scale mono reaches sqrt(2) on the mid axis, a
        // hard-panned full-scale sample 1/sqrt(2) on each.
        let root_two = 2f32.sqrt();
        let points = goniometer_points(&[1.0, 1.0, 1.0, -1.0, 1.0, 0.0], 2, 8);
        assert_eq!(points[0], 0.0);
        assert!(
            (points[1] - root_two).abs() < 1e-6,
            "mono peak at {}",
            points[1]
        );
        assert!(
            (points[2] - root_two).abs() < 1e-6,
            "inverted peak at {}",
            points[2]
        );
        assert_eq!(points[3], 0.0);
        assert!((points[4] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((points[5] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        for value in &points {
            assert!(
                value.abs() <= root_two + 1e-6,
                "{value} is beyond full scale"
            );
        }
    }

    #[test]
    fn decimation_never_exceeds_the_point_budget() {
        // Bounded output is what makes this affordable to send across an IPC
        // boundary every frame.
        for frames in [1usize, 7, 256, 1024, 4096] {
            let points = goniometer_points(&vec![0.5; frames * 2], 2, 256);
            assert!(
                points.len() <= 256 * 2,
                "{frames} frames produced {} points",
                points.len() / 2
            );
            assert!(!points.is_empty());
        }
    }
}
