//! Loudness metering to ITU-R BS.1770-4: momentary, short-term, integrated,
//! and true peak.
//!
//! Cross-target and free of threads, like `spectrum.rs` and for the same reason
//! (M4 decision 3): the native analysis thread and the web worker both call
//! exactly this code, so the two shells cannot measure differently. What is a
//! display decision stays outside -- this reports LUFS and dBTP and nothing
//! about how they should look.
//!
//! # What the standard actually asks for
//!
//! - **K-weighting**: a high-shelf followed by a high-pass, applied per
//!   channel, standing in for how a listener weights what they hear.
//! - **Mean square per channel**, summed with per-channel weights (1.0 for
//!   front channels, 1.41 for surrounds), turned into a loudness with
//!   `-0.691 + 10*log10(sum)`. The offset is what makes the scale LUFS.
//! - **Momentary** is that over a 400ms window, **short-term** over 3s.
//! - **Integrated** is the gated mean over the whole measurement: 400ms blocks
//!   overlapping by 75%, keeping only those above an absolute gate (-70 LUFS)
//!   and then only those within 10 LU of the mean of what survived. The gating
//!   is the part that stops a quiet passage dragging a whole track's number
//!   down.
//!
//! # Two deliberate departures
//!
//! The coefficients are *derived* for the output's rate rather than taken from
//! the spec's 48kHz table -- see [`KWeighting`], and the test that holds the
//! derivation to that table. And silence reports [`FLOOR_LUFS`] rather than
//! negative infinity, which is a boundary constraint rather than a measurement
//! one (M5 decision 5).

use std::collections::VecDeque;

/// What a silent (or gated-out) measurement reports. BS.1770-4's own absolute
/// gate: below this the standard stops counting a block at all, so it is the
/// natural place to put "there is nothing here to measure".
///
/// The honest value is negative infinity. It is not used because these numbers
/// cross a JSON boundary on the desktop shell, where a non-finite float
/// silently becomes `null`, and both shells have to report the same thing.
pub const FLOOR_LUFS: f32 = -70.0;

/// Floor for true peak, in dBTP. Far below the noise floor of any real source,
/// and finite for the same reason `FLOOR_LUFS` is.
pub const FLOOR_DBTP: f32 = -120.0;

/// The scale offset in BS.1770-4's loudness equation. Not a fudge factor: it is
/// what puts a reference signal at the level the standard says it should read.
const LOUDNESS_OFFSET: f64 = -0.691;

/// BS.1770-4's absolute gate.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;

/// How far below the ungated mean the relative gate sits, in LU.
const RELATIVE_GATE_LU: f64 = 10.0;

/// Sub-blocks per second. A gating block is 400ms overlapping 75%, i.e. a new
/// block every 100ms, so 100ms is the unit everything here accumulates in.
const SUB_BLOCKS_PER_SECOND: usize = 10;

/// Sub-blocks in one gating block (400ms) and in a short-term window (3s).
const MOMENTARY_SUB_BLOCKS: usize = 4;
const SHORT_TERM_SUB_BLOCKS: usize = 30;

/// A biquad in direct form I, in f64.
///
/// f64 rather than f32 throughout the filter: the second K-weighting stage is a
/// high-pass with poles at 38Hz, which at 96kHz sits close enough to DC that
/// f32 coefficients lose meaningful precision in the difference between them.
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

/// One channel's filter state.
#[derive(Clone, Copy, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn process(&self, state: &mut BiquadState, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * state.x1 + self.b2 * state.x2
            - self.a1 * state.y1
            - self.a2 * state.y2;
        state.x2 = state.x1;
        state.x1 = x;
        state.y2 = state.y1;
        state.y1 = y;
        y
    }
}

/// BS.1770-4's K-weighting: a high-shelf, then a high-pass.
///
/// The spec publishes these as coefficient tables for 48kHz, and pasting that
/// table is the usual implementation. It is also wrong at any other rate, and
/// the rate here is the *output device's* -- 96kHz on several of this machine's
/// devices, 44.1 on others (see `playback.rs`'s `device_config`: the device's
/// mix format is the only one that opens). So the filters are built from the
/// analog prototypes the table was itself derived from, by bilinear transform
/// with `K = tan(pi*f0/rate)`, and `derivation_matches_the_published_table`
/// below holds that derivation to the published numbers at 48kHz. Getting the
/// same answer as the spec where the spec states one is the evidence that the
/// other rates are right too.
pub struct KWeighting {
    shelf: Biquad,
    high_pass: Biquad,
}

impl KWeighting {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));

        // Stage 1, the high-shelf. f0/Q/gain are the prototype's, as given in
        // BS.1770-4's derivation of its own table.
        let f0 = 1681.974450955533;
        let gain_db = 3.999843853973347;
        let q = 0.7071752369554196;
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let vh = 10f64.powf(gain_db / 20.0);
        // The shelf's mid-band gain: sqrt-ish of the high-band one, at the
        // exponent that makes the prototype's response what the table shows.
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        };

        // Stage 2, the RLB high-pass: a plain second-order high-pass, which is
        // why its zeros are exactly 1, -2, 1.
        let f0 = 38.13547087602444;
        let q = 0.5003270373238773;
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let high_pass = Biquad {
            b0: 1.0,
            b1: -2.0,
            b2: 1.0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        };

        Self { shelf, high_pass }
    }
}

/// Per-channel state for the two filter stages.
#[derive(Clone, Copy, Default)]
struct ChannelState {
    shelf: BiquadState,
    high_pass: BiquadState,
}

/// What the loudness meter currently reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessReading {
    /// 400ms window, in LUFS.
    pub momentary: f32,
    /// 3s window, in LUFS.
    pub short_term: f32,
    /// Gated mean over everything measured since the last reset, in LUFS.
    pub integrated: f32,
    /// Highest true (inter-sample) peak since the last reset, in dBTP.
    pub true_peak: f32,
}

impl LoudnessReading {
    /// What a meter that has measured nothing reports.
    pub fn silent() -> Self {
        Self {
            momentary: FLOOR_LUFS,
            short_term: FLOOR_LUFS,
            integrated: FLOOR_LUFS,
            true_peak: FLOOR_DBTP,
        }
    }
}

/// The loudness meter: audio in a channel at a time, LUFS out.
///
/// Fed by [`crate::analysis::Meters`] from the tap drain, so it sees every
/// sample that reached the output -- which is the difference between an
/// integrated number that means something and one that skipped whatever the
/// display happened to miss between frames (M4 decision 1).
pub struct LoudnessMeter {
    weighting: KWeighting,
    states: Vec<ChannelState>,
    weights: Vec<f64>,
    channels: usize,
    /// Frames in a 100ms sub-block, and how far into the current one we are.
    sub_block_frames: usize,
    frames_in_sub_block: usize,
    /// Sum of squares per channel for the sub-block being filled.
    partial: Vec<f64>,
    /// Completed sub-blocks: mean square per channel. Bounded by the longest
    /// window that reads it (3s), because nothing looks further back than that
    /// -- the integrated measurement keeps its own summary instead.
    history: VecDeque<Vec<f64>>,
    /// One weighted mean square per 400ms gating block, which is all the
    /// integrated measurement needs to keep: gating decides which blocks count
    /// only once every block's value is known, so they cannot be folded into a
    /// running sum as they arrive. ~10 f64 per second of playback.
    blocks: Vec<f64>,
    true_peak: TruePeak,
    peak: f64,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels.max(1));
        Self {
            weighting: KWeighting::new(sample_rate),
            states: vec![ChannelState::default(); channels],
            weights: channel_weights(channels),
            channels,
            sub_block_frames: (sample_rate as usize / SUB_BLOCKS_PER_SECOND).max(1),
            frames_in_sub_block: 0,
            partial: vec![0.0; channels],
            history: VecDeque::with_capacity(SHORT_TERM_SUB_BLOCKS),
            blocks: Vec::new(),
            true_peak: TruePeak::new(channels),
            peak: 0.0,
        }
    }

    /// Forgets everything measured so far.
    ///
    /// Called when the listen breaks rather than periodically (M5 decision 2):
    /// a new track, a seek, or a tap discontinuity. Integrating across one of
    /// those would report a number for a timeline nobody heard.
    pub fn reset(&mut self) {
        self.states
            .iter_mut()
            .for_each(|s| *s = ChannelState::default());
        self.partial.iter_mut().for_each(|p| *p = 0.0);
        self.frames_in_sub_block = 0;
        self.history.clear();
        self.blocks.clear();
        self.true_peak.reset();
        self.peak = 0.0;
    }

    /// Adds interleaved audio in the meter's channel layout.
    pub fn push(&mut self, interleaved: &[f32]) {
        if interleaved.is_empty() {
            return;
        }
        self.true_peak.push(interleaved, &mut self.peak);

        for frame in interleaved.chunks_exact(self.channels) {
            for (channel, &sample) in frame.iter().enumerate() {
                let shelved = self
                    .weighting
                    .shelf
                    .process(&mut self.states[channel].shelf, f64::from(sample));
                let weighted = self
                    .weighting
                    .high_pass
                    .process(&mut self.states[channel].high_pass, shelved);
                self.partial[channel] += weighted * weighted;
            }
            self.frames_in_sub_block += 1;
            if self.frames_in_sub_block >= self.sub_block_frames {
                self.close_sub_block();
            }
        }
    }

    /// Momentary, short-term, integrated and true peak as of the last `push`.
    pub fn reading(&self) -> LoudnessReading {
        LoudnessReading {
            momentary: self.window_loudness(MOMENTARY_SUB_BLOCKS),
            short_term: self.window_loudness(SHORT_TERM_SUB_BLOCKS),
            integrated: self.integrated(),
            true_peak: if self.peak > 0.0 {
                ((20.0 * self.peak.log10()) as f32).max(FLOOR_DBTP)
            } else {
                FLOOR_DBTP
            },
        }
    }

    /// Turns the finished sub-block into a mean square per channel, and closes
    /// a gating block with it if there are now four.
    fn close_sub_block(&mut self) {
        let frames = self.frames_in_sub_block.max(1) as f64;
        let mean_squares: Vec<f64> = self.partial.iter().map(|sum| sum / frames).collect();
        self.partial.iter_mut().for_each(|p| *p = 0.0);
        self.frames_in_sub_block = 0;

        if self.history.len() == SHORT_TERM_SUB_BLOCKS {
            self.history.pop_front();
        }
        self.history.push_back(mean_squares);

        // A gating block every sub-block once there are four to average: that
        // is what BS.1770-4's 75% overlap means in practice.
        if self.history.len() >= MOMENTARY_SUB_BLOCKS {
            if let Some(power) = self.window_power(MOMENTARY_SUB_BLOCKS) {
                self.blocks.push(power);
            }
        }
    }

    /// Weighted sum of the per-channel mean squares over the newest `count`
    /// sub-blocks -- the `sum(G * z)` in BS.1770-4's loudness equation. `None`
    /// until that many sub-blocks exist, since a window shorter than it claims
    /// to be would read low.
    fn window_power(&self, count: usize) -> Option<f64> {
        if self.history.len() < count {
            return None;
        }
        let mut total = 0.0;
        for channel in 0..self.channels {
            let sum: f64 = self
                .history
                .iter()
                .rev()
                .take(count)
                .map(|block| block[channel])
                .sum();
            total += self.weights[channel] * sum / count as f64;
        }
        Some(total)
    }

    fn window_loudness(&self, count: usize) -> f32 {
        match self.window_power(count) {
            Some(power) => loudness(power),
            None => FLOOR_LUFS,
        }
    }

    /// BS.1770-4's gated measurement: drop everything below the absolute gate,
    /// then everything more than 10 LU below the mean of what is left, and
    /// average what survives.
    fn integrated(&self) -> f32 {
        let above_absolute: Vec<f64> = self
            .blocks
            .iter()
            .copied()
            .filter(|&power| f64::from(loudness(power)) > ABSOLUTE_GATE_LUFS)
            .collect();
        if above_absolute.is_empty() {
            return FLOOR_LUFS;
        }

        let ungated_mean = above_absolute.iter().sum::<f64>() / above_absolute.len() as f64;
        let relative_gate = f64::from(loudness(ungated_mean)) - RELATIVE_GATE_LU;

        let (sum, count) = above_absolute
            .iter()
            .filter(|&&power| f64::from(loudness(power)) > relative_gate)
            .fold((0.0, 0usize), |(sum, count), &power| {
                (sum + power, count + 1)
            });
        if count == 0 {
            return FLOOR_LUFS;
        }
        loudness(sum / count as f64)
    }
}

/// BS.1770-4's loudness from a weighted mean square.
fn loudness(power: f64) -> f32 {
    if power <= 0.0 {
        return FLOOR_LUFS;
    }
    ((LOUDNESS_OFFSET + 10.0 * power.log10()) as f32).max(FLOOR_LUFS)
}

/// Per-channel weights, assuming the SMPTE/WAVE channel order the output layout
/// uses (L, R, C, LFE, Ls, Rs).
///
/// BS.1770-4 weights the surrounds 1.41 (+1.5dB) and everything else 1.0, and
/// excludes the LFE. For the stereo and mono outputs this player actually meets
/// this is simply "all ones" -- the rest is here so a surround device doesn't
/// silently get it wrong.
fn channel_weights(channels: usize) -> Vec<f64> {
    (0..channels)
        .map(|channel| match channel {
            0..=2 => 1.0,
            // Only a 5-channel-or-wider layout has an LFE at index 3; a
            // 4-channel one is quad, where index 3 is a surround.
            3 if channels >= 5 => 0.0,
            _ => 1.41,
        })
        .collect()
}

/// Taps per polyphase branch of the true-peak interpolator. 12 branches of 4
/// gives a 48-tap prototype -- the length BS.1770-4's own Annex 2 filter uses,
/// which is enough stopband rejection for the peak estimate to be worth
/// reporting to a tenth of a dB.
const TRUE_PEAK_TAPS: usize = 12;

/// The 4x oversampling factor BS.1770-4 Annex 2 specifies for rates up to
/// 48kHz. Higher rates are already oversampled relative to the audio band, so
/// the same factor only over-delivers.
const TRUE_PEAK_FACTOR: usize = 4;

/// Inter-sample peak detection by 4x oversampling.
///
/// The reason this exists rather than `max(|sample|)`: a signal whose samples
/// never exceed full scale can still reconstruct above it between them, and
/// that is what clips a DAC or an encoder. The classic case -- a sine at fs/4
/// sampled through its zero crossings -- has a sample peak of -3dBFS and a true
/// peak of 0dBTP, and is what `a_sine_between_samples_peaks_above_its_samples`
/// holds this to.
///
/// The interpolator is a windowed-sinc polyphase FIR: BS.1770-4's method, not
/// its exact published table, which the spec gives only for 48kHz input.
struct TruePeak {
    /// `phases[p][k]` multiplies the sample `k` back from the newest to produce
    /// output phase `p`.
    phases: [[f64; TRUE_PEAK_TAPS]; TRUE_PEAK_FACTOR],
    /// Newest-last ring of the last `TRUE_PEAK_TAPS` samples, per channel.
    history: Vec<Vec<f64>>,
    channels: usize,
}

impl TruePeak {
    fn new(channels: usize) -> Self {
        Self {
            phases: design_phases(),
            history: vec![vec![0.0; TRUE_PEAK_TAPS]; channels],
            channels,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.history {
            channel.iter_mut().for_each(|s| *s = 0.0);
        }
    }

    /// Runs `interleaved` through the interpolator, raising `peak` to the
    /// highest absolute value seen. Takes the peak by reference rather than
    /// returning one so the caller's running maximum survives resets of nothing
    /// else.
    fn push(&mut self, interleaved: &[f32], peak: &mut f64) {
        for frame in interleaved.chunks_exact(self.channels) {
            for (channel, &sample) in frame.iter().enumerate() {
                let history = &mut self.history[channel];
                history.rotate_right(1);
                history[0] = f64::from(sample);
                // The sample itself counts too: at 4x the interpolator can sit
                // either side of a true peak that lands on a sample.
                *peak = peak.max(history[0].abs());
                for phase in &self.phases {
                    let mut sum = 0.0;
                    for (coefficient, &past) in phase.iter().zip(history.iter()) {
                        sum += coefficient * past;
                    }
                    *peak = peak.max(sum.abs());
                }
            }
        }
    }
}

/// A 48-tap windowed-sinc low-pass at a quarter of the oversampled Nyquist,
/// split into its four polyphase branches.
fn design_phases() -> [[f64; TRUE_PEAK_TAPS]; TRUE_PEAK_FACTOR] {
    let length = TRUE_PEAK_TAPS * TRUE_PEAK_FACTOR;
    let centre = (length - 1) as f64 / 2.0;
    let mut prototype = vec![0.0; length];
    for (n, tap) in prototype.iter_mut().enumerate() {
        let x = n as f64 - centre;
        let sinc = if x.abs() < 1e-12 {
            1.0
        } else {
            let arg = std::f64::consts::PI * x / TRUE_PEAK_FACTOR as f64;
            arg.sin() / arg
        };
        // Hann, over the prototype's full length: the sidelobes it costs in
        // width it buys back in stopband, which is what keeps a peak estimate
        // from picking up an image of the signal instead of the signal.
        let window =
            0.5 * (1.0 - (2.0 * std::f64::consts::PI * n as f64 / (length - 1) as f64).cos());
        *tap = sinc * window;
    }

    let mut phases = [[0.0; TRUE_PEAK_TAPS]; TRUE_PEAK_FACTOR];
    for (p, phase) in phases.iter_mut().enumerate() {
        // Output sample 4n+p is sum over k of prototype[p + 4k] * x[n-k]:
        // the zero-stuffed convolution, with the zeros never materialised.
        let mut sum = 0.0;
        for (k, tap) in phase.iter_mut().enumerate() {
            *tap = prototype[p + k * TRUE_PEAK_FACTOR];
            sum += *tap;
        }
        // Each branch is a fractional-delay filter and so must have unity DC
        // gain; normalising per branch rather than scaling the prototype by 4
        // makes that exact rather than approximately true.
        if sum.abs() > 1e-12 {
            phase.iter_mut().for_each(|tap| *tap /= sum);
        }
    }
    phases
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Interleaved stereo sine.
    fn sine(hz: f32, amplitude: f32, seconds: f32, rate: u32) -> Vec<f32> {
        let frames = (seconds * rate as f32) as usize;
        (0..frames)
            .flat_map(|n| {
                let s =
                    amplitude * (2.0 * std::f32::consts::PI * hz * n as f32 / rate as f32).sin();
                [s, s]
            })
            .collect()
    }

    #[test]
    fn derivation_matches_the_published_table() {
        // BS.1770-4's own coefficients for 48kHz. If the bilinear derivation
        // above reproduces these, it can be trusted at 44.1 and 96 where the
        // spec publishes nothing -- which is the whole reason not to paste the
        // table (M5 decision 1).
        let k = KWeighting::new(48_000);
        let close = |got: f64, want: f64, what: &str| {
            assert!(
                (got - want).abs() < 1e-6,
                "{what}: derived {got:.14}, published {want:.14}"
            );
        };
        close(k.shelf.b0, 1.53512485958697, "shelf b0");
        close(k.shelf.b1, -2.69169618940638, "shelf b1");
        close(k.shelf.b2, 1.19839281085285, "shelf b2");
        close(k.shelf.a1, -1.69065929318241, "shelf a1");
        close(k.shelf.a2, 0.73248077421585, "shelf a2");

        close(k.high_pass.b0, 1.0, "high-pass b0");
        close(k.high_pass.b1, -2.0, "high-pass b1");
        close(k.high_pass.b2, 1.0, "high-pass b2");
        close(k.high_pass.a1, -1.99004745483398, "high-pass a1");
        close(k.high_pass.a2, 0.99007225036621, "high-pass a2");
    }

    #[test]
    fn a_1khz_tone_reads_its_own_level() {
        // The calibration the whole scale rests on. K-weighting has +0.691dB of
        // gain at 1kHz, which is precisely what the -0.691 offset cancels -- so
        // a 1kHz sine on *one* channel reads its own RMS level in dBFS, and a
        // meter that is out by that offset is out by it everywhere.
        //
        // One channel, because two identical ones are 3.01 LU louder by
        // definition (see `mono_and_stereo_of_the_same_signal_agree`). The
        // amplitude for -23dBFS RMS is sqrt(2)*10^(-23/20).
        let amplitude = 2f32.sqrt() * 10f32.powf(-23.0 / 20.0);
        let stereo = sine(1000.0, amplitude, 4.0, RATE);
        let mono: Vec<f32> = stereo.iter().step_by(2).copied().collect();
        let mut meter = LoudnessMeter::new(RATE, 1);
        meter.push(&mono);
        let reading = meter.reading();
        assert!(
            (reading.momentary - (-23.0)).abs() < 0.2,
            "momentary {:.2} LUFS, want -23",
            reading.momentary
        );
        assert!(
            (reading.short_term - (-23.0)).abs() < 0.2,
            "short-term {:.2} LUFS, want -23",
            reading.short_term
        );
        assert!(
            (reading.integrated - (-23.0)).abs() < 0.2,
            "integrated {:.2} LUFS, want -23",
            reading.integrated
        );
    }

    #[test]
    fn the_scale_is_decibels() {
        // Halving amplitude is -6dB, wherever the meter happens to sit.
        let mut loud = LoudnessMeter::new(RATE, 2);
        loud.push(&sine(1000.0, 0.5, 4.0, RATE));
        let mut quiet = LoudnessMeter::new(RATE, 2);
        quiet.push(&sine(1000.0, 0.25, 4.0, RATE));
        let difference = loud.reading().momentary - quiet.reading().momentary;
        assert!(
            (difference - 6.02).abs() < 0.1,
            "halving amplitude should cost 6.02 LU, cost {difference:.2}"
        );
    }

    #[test]
    fn the_measurement_is_rate_independent() {
        // The same tone at three rates must read the same loudness -- the
        // property that pasting the 48kHz table would break, and the one this
        // machine's 96kHz devices depend on.
        let readings: Vec<f32> = [44_100u32, 48_000, 96_000]
            .iter()
            .map(|&rate| {
                let mut meter = LoudnessMeter::new(rate, 2);
                meter.push(&sine(1000.0, 0.5, 4.0, rate));
                meter.reading().momentary
            })
            .collect();
        for reading in &readings {
            assert!(
                (reading - readings[0]).abs() < 0.1,
                "loudness varies with the output rate: {readings:?}"
            );
        }
    }

    #[test]
    fn silence_reads_the_floor_rather_than_negative_infinity() {
        let mut meter = LoudnessMeter::new(RATE, 2);
        meter.push(&vec![0.0; RATE as usize * 2]);
        let reading = meter.reading();
        assert_eq!(reading.momentary, FLOOR_LUFS);
        assert_eq!(reading.integrated, FLOOR_LUFS);
        assert_eq!(reading.true_peak, FLOOR_DBTP);
        assert!(
            reading.momentary.is_finite(),
            "must survive a JSON boundary"
        );
    }

    #[test]
    fn a_window_shorter_than_it_claims_reads_the_floor() {
        // 200ms into a track there is no 400ms window yet. Reporting whatever
        // half a window measured would read ~3dB low and look like a real
        // number.
        let mut meter = LoudnessMeter::new(RATE, 2);
        meter.push(&sine(1000.0, 0.5, 0.2, RATE));
        assert_eq!(meter.reading().momentary, FLOOR_LUFS);
        meter.push(&sine(1000.0, 0.5, 0.3, RATE));
        assert!(meter.reading().momentary > -20.0);
    }

    #[test]
    fn quiet_passages_are_gated_out_of_the_integrated_number() {
        // The point of gating: a track with silence in it measures as loud as
        // the music in it, not as the average of music and silence. Ungated,
        // 4s of tone and 4s of silence would come out ~3dB below the tone.
        let mut meter = LoudnessMeter::new(RATE, 2);
        meter.push(&sine(1000.0, 0.5, 4.0, RATE));
        let music_only = meter.reading().integrated;
        meter.push(&vec![0.0; RATE as usize * 2 * 4]);
        let with_silence = meter.reading().integrated;
        assert!(
            (music_only - with_silence).abs() < 0.2,
            "silence dragged the integrated number from {music_only:.2} to {with_silence:.2}"
        );
    }

    #[test]
    fn a_reset_forgets_what_came_before() {
        let mut meter = LoudnessMeter::new(RATE, 2);
        meter.push(&sine(1000.0, 0.9, 4.0, RATE));
        let loud = meter.reading().integrated;
        meter.reset();
        assert_eq!(meter.reading().integrated, FLOOR_LUFS);

        meter.push(&sine(1000.0, 0.09, 4.0, RATE));
        let quiet = meter.reading().integrated;
        assert!(
            quiet < loud - 15.0,
            "the quiet track measured {quiet:.2} against the loud one's {loud:.2}; \
             the reset did not take"
        );
    }

    #[test]
    fn a_sine_between_samples_peaks_above_its_samples() {
        // fs/4 sampled through its zero crossings: every sample sits at
        // +-1/sqrt(2), so a sample-peak meter reads -3dBFS while the waveform
        // between them reaches full scale. This is the entire reason true peak
        // is measured rather than assumed.
        let frames = RATE as usize;
        // The phase is taken modulo the period rather than accumulated: at
        // 48000 samples an f32 phase has lost enough precision to move the
        // samples off the exact +-1/sqrt(2) the test's premise depends on.
        let audio: Vec<f32> = (0..frames)
            .flat_map(|n| {
                let phase =
                    std::f64::consts::PI * (n % 4) as f64 / 2.0 + std::f64::consts::FRAC_PI_4;
                let s = phase.sin() as f32;
                [s, s]
            })
            .collect();

        let sample_peak = audio.iter().fold(0.0f32, |peak, &s| peak.max(s.abs()));
        assert!(
            (20.0 * sample_peak.log10() + 3.01).abs() < 0.01,
            "the test signal's samples should sit at -3dBFS"
        );

        let mut meter = LoudnessMeter::new(RATE, 2);
        meter.push(&audio);
        let true_peak = meter.reading().true_peak;
        assert!(
            // The interpolator reads -0.07 dBTP here: a tenth of a dB of the
            // analytic answer, which is the accuracy this filter length buys.
            (true_peak - 0.0).abs() < 0.15,
            "true peak read {true_peak:.2} dBTP for a signal that reaches full scale"
        );
    }

    #[test]
    fn true_peak_holds_its_maximum() {
        // A peak meter reports the loudest thing it has seen, not the loudest
        // thing in the last block -- otherwise a transient that clipped is gone
        // before anyone can read it.
        let mut meter = LoudnessMeter::new(RATE, 2);
        meter.push(&sine(1000.0, 0.5, 1.0, RATE));
        let after_loud = meter.reading().true_peak;
        meter.push(&sine(1000.0, 0.01, 1.0, RATE));
        assert_eq!(meter.reading().true_peak, after_loud);
    }

    #[test]
    fn mono_and_stereo_of_the_same_signal_agree() {
        // BS.1770-4 *sums* the weighted channels rather than averaging them, so
        // the same waveform in two channels is 3.01 LU louder than in one. That
        // is not a quirk to correct for -- it is what makes a hard-panned mix
        // and a centred one measure differently -- but it is the opposite of
        // what the spectrum does with its mono sum, so it is worth pinning
        // down rather than discovering later.
        let stereo = sine(1000.0, 0.5, 4.0, RATE);
        let mono: Vec<f32> = stereo.iter().step_by(2).copied().collect();

        let mut a = LoudnessMeter::new(RATE, 2);
        a.push(&stereo);
        let mut b = LoudnessMeter::new(RATE, 1);
        b.push(&mono);
        let difference = a.reading().momentary - b.reading().momentary;
        assert!(
            (difference - 3.01).abs() < 0.1,
            "two identical channels should sum to +3.01 LU, got {difference:.2}"
        );
    }
}
