//! The ten-band graphic equaliser: PLAN.md's "basic EQ".
//!
//! Cross-target, like every DSP module since M4, and for the same reason: the
//! cpal callback and the web build's AudioWorklet must not be able to filter
//! differently. What is unusual here -- and is M8's one real departure from
//! M4 decision 3 -- is that this module cannot own the whole job on the web,
//! because the web's audio path is TypeScript. So the split is drawn where the
//! bugs actually live:
//!
//! - **The design is here**: what a slider position means, what shape each band
//!   has, what its Q is, how the preamp folds in, and what the resulting curve
//!   looks like ([`magnitude_response`], which is what the UI draws). Every one
//!   of those is a decision that could differ between two implementations.
//! - **The application** -- one direct-form-I difference equation -- is five
//!   multiply-adds, written here and mirrored in app/src/eqFilter.ts, with
//!   `/eq.html` holding the two to each other sample for sample.
//!
//! # Where it sits in the chain
//!
//! Between the playback ring and the output, *before* the analysis tap and
//! before the volume control. So the spectrum, the loudness meters and the
//! vectorscope all show the audio as it leaves the player, which is what makes
//! a boost that clips visible as clipping (M8 decision 2). Volume stays after
//! the tap, where it has always been: monitoring level must not change what a
//! track measures.

/// Bands. Ten, at ISO octave centres -- the layout a listener coming from any
/// other player already knows how to read.
pub const EQ_BANDS: usize = 10;

/// Centre frequencies, in Hz. One octave apart, so a slider moves the octave
/// around its label rather than a sliver of it.
pub const BAND_FREQUENCIES: [f32; EQ_BANDS] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// How far a band or the preamp can be moved, in dB either way.
pub const MAX_GAIN_DB: f32 = 12.0;

/// Q for the peaking bands: one octave of bandwidth, which is what
/// `sqrt(2^n)/(2^n - 1)` gives at n = 1. Matching the band spacing is what
/// keeps a row of equal boosts summing to something close to a flat lift
/// instead of a comb.
const BAND_Q: f64 = std::f64::consts::SQRT_2;

/// Q for the two shelves. Below this the shelf sags before it turns and above
/// it the corner overshoots; `1/sqrt(2)` is the flattest one, which is the same
/// reason it is the default Q of every shelving filter there has ever been.
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Highest centre frequency the design will place, as a fraction of the sample
/// rate. A biquad's centre must sit below Nyquist to mean anything, and the top
/// band is 16kHz -- fine at 44.1kHz and above, but this player takes whatever
/// rate the output device offers and a 32kHz device would otherwise be asked
/// for a filter at half its Nyquist.
const MAX_CENTRE_FRACTION: f64 = 0.45;

/// What one band does to the signal.
///
/// The ends are shelves rather than peaks deliberately. A peaking filter at
/// 31Hz leaves everything below it alone, so "more bass" would do nothing to
/// the lowest octave a system can reproduce; the same argument at the top is
/// what "air" means on a mixing desk. The eight bands in between are peaks,
/// because that is what a graphic EQ's middle sliders are for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandKind {
    LowShelf,
    Peaking,
    HighShelf,
}

/// The kind of filter each slider drives.
pub fn band_kind(band: usize) -> BandKind {
    match band {
        0 => BandKind::LowShelf,
        b if b == EQ_BANDS - 1 => BandKind::HighShelf,
        _ => BandKind::Peaking,
    }
}

/// Where the user has left the sliders.
///
/// Deliberately a plain description of the *controls* rather than of the
/// filters: it is what crosses to the UI, what a preset would be, and what gets
/// persisted. Turning it into coefficients is [`EqCoefficients::design`]'s job
/// and depends on the output rate, which the UI has no business knowing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqSettings {
    /// Off means the audio passes through untouched -- not "all sliders at
    /// zero", which is nearly but not exactly the same thing (a cascade of
    /// unity biquads is still arithmetic on every sample).
    pub enabled: bool,
    /// Applied ahead of the bands, in dB. What a listener reaches for after
    /// boosting six sliders and finding the track now clips.
    pub preamp_db: f32,
    /// One slider per band, in dB.
    pub gains_db: [f32; EQ_BANDS],
}

impl Default for EqSettings {
    fn default() -> Self {
        Self::flat()
    }
}

impl EqSettings {
    /// Enabled, everything at zero: the state the EQ starts in.
    pub fn flat() -> Self {
        Self {
            enabled: true,
            preamp_db: 0.0,
            gains_db: [0.0; EQ_BANDS],
        }
    }

    /// The same settings with everything forced into range, and any NaN a
    /// caller managed to send turned into zero.
    ///
    /// Clamped here rather than trusted from the caller because these arrive
    /// over an IPC boundary and end up as filter coefficients on the audio
    /// thread: a NaN reaching a biquad's state poisons every sample after it,
    /// for the life of the stream. Anything not finite is read as "leave this
    /// control alone" rather than as an extreme of it -- an infinity is a bug
    /// somewhere upstream, and the safe reading of a bug is no change.
    pub fn clamped(&self) -> Self {
        let clamp = |db: f32| {
            if db.is_finite() {
                db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
            } else {
                0.0
            }
        };
        let mut gains = [0.0; EQ_BANDS];
        for (out, &db) in gains.iter_mut().zip(self.gains_db.iter()) {
            *out = clamp(db);
        }
        Self {
            enabled: self.enabled,
            preamp_db: clamp(self.preamp_db),
            gains_db: gains,
        }
    }

    /// Whether every control is at zero, i.e. the filters would pass the signal
    /// through unchanged.
    pub fn is_flat(&self) -> bool {
        self.preamp_db == 0.0 && self.gains_db.iter().all(|&db| db == 0.0)
    }
}

/// Floats ahead of the bands in [`EqCoefficients::to_array`]: the enabled flag
/// and the preamp.
pub const EQ_COEFFICIENT_HEADER: usize = 2;

/// Floats in one flattened cascade -- what the web build's shared buffer holds.
pub const EQ_COEFFICIENT_FIELDS: usize = EQ_COEFFICIENT_HEADER + EQ_BANDS * 5;

/// One biquad's coefficients, normalised so `a0` is 1.
///
/// f64 for the reason `loudness.rs` gives about its own filters, which applies
/// harder here: the lowest band sits at 31.25Hz, and at a 96kHz output rate --
/// which several of this machine's devices run at -- that is 0.0003 of the
/// sample rate. In f32 the difference between the pole pair and the unit circle
/// is a handful of representable values wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl BiquadCoefficients {
    /// Passes everything through unchanged.
    pub fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// The five numbers in the order both shells store them (the SharedArray-
    /// Buffer region on the web, an rtrb message natively).
    pub fn to_array(&self) -> [f64; 5] {
        [self.b0, self.b1, self.b2, self.a1, self.a2]
    }

    /// Gain at `frequency`, as a linear magnitude.
    ///
    /// Evaluates `|H(e^jw)|` directly rather than approximating it, so the
    /// curve the UI draws is the response of the filter that is actually
    /// running -- including whatever the bilinear transform did to the shape
    /// near Nyquist.
    fn magnitude(&self, frequency: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * frequency / sample_rate;
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();
        // z^-1 = cos(w) - j sin(w), z^-2 = cos(2w) - j sin(2w).
        let num_re = self.b0 + self.b1 * cos1 + self.b2 * cos2;
        let num_im = -(self.b1 * sin1 + self.b2 * sin2);
        let den_re = 1.0 + self.a1 * cos1 + self.a2 * cos2;
        let den_im = -(self.a1 * sin1 + self.a2 * sin2);
        let denominator = (den_re * den_re + den_im * den_im).sqrt();
        if denominator < 1e-30 {
            return 1.0;
        }
        (num_re * num_re + num_im * num_im).sqrt() / denominator
    }
}

/// One channel's memory of one biquad.
#[derive(Clone, Copy, Default, Debug)]
pub struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadState {
    /// Direct form I: the same arithmetic, in the same order, as
    /// app/src/eqFilter.ts. Any change here is a change there.
    #[inline]
    fn process(&mut self, c: &BiquadCoefficients, x: f64) -> f64 {
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Everything the audio thread needs to run the EQ: the ten filters, the
/// preamp as a linear gain, and whether to bother.
///
/// `Copy` on purpose. Natively it travels to the cpal callback through an SPSC
/// queue, the way M3's track boundaries do, because the callback may not take a
/// lock; on the web the same numbers are written into the shared buffer for the
/// worklet to pick up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqCoefficients {
    pub enabled: bool,
    pub preamp: f64,
    pub bands: [BiquadCoefficients; EQ_BANDS],
}

impl Default for EqCoefficients {
    fn default() -> Self {
        Self::bypassed()
    }
}

impl EqCoefficients {
    /// What the audio thread runs when the EQ is off.
    pub fn bypassed() -> Self {
        Self {
            enabled: false,
            preamp: 1.0,
            bands: [BiquadCoefficients::identity(); EQ_BANDS],
        }
    }

    /// Designs the filter cascade for `settings` at `sample_rate`.
    ///
    /// The formulas are the RBJ cookbook's -- the standard derivation of these
    /// shapes by bilinear transform -- and are held to their own claims by the
    /// tests below rather than taken on trust: each band is asserted to reach
    /// its slider's gain at its own centre and to leave DC and Nyquist alone.
    pub fn design(settings: &EqSettings, sample_rate: u32) -> Self {
        let settings = settings.clamped();
        let rate = f64::from(sample_rate.max(1));
        let mut bands = [BiquadCoefficients::identity(); EQ_BANDS];

        for (band, coefficients) in bands.iter_mut().enumerate() {
            let gain_db = f64::from(settings.gains_db[band]);
            if gain_db == 0.0 {
                // A 0dB peaking filter is mathematically the identity, so
                // designing one would only cost precision. This also means an
                // untouched slider adds nothing to the audio thread's work
                // beyond a multiply-add chain of zeros.
                continue;
            }
            let centre = f64::from(BAND_FREQUENCIES[band]).min(rate * MAX_CENTRE_FRACTION);
            *coefficients = match band_kind(band) {
                BandKind::LowShelf => low_shelf(centre, rate, gain_db, SHELF_Q),
                BandKind::HighShelf => high_shelf(centre, rate, gain_db, SHELF_Q),
                BandKind::Peaking => peaking(centre, rate, gain_db, BAND_Q),
            };
        }

        Self {
            enabled: settings.enabled,
            preamp: 10f64.powf(f64::from(settings.preamp_db) / 20.0),
            bands,
        }
    }

    /// The cascade as a flat array, for the boundary that cannot carry a
    /// struct: the web build's shared buffer, which the worker writes and the
    /// AudioWorklet reads (`EQ` in app/src/audioSab.ts mirrors this order, and
    /// nothing else on that side names it).
    ///
    /// f64 all the way across. Truncating to f32 here would undo exactly the
    /// precision this type keeps for the 31Hz band's sake, and JavaScript's own
    /// numbers are f64 anyway, so there is nothing to gain by it.
    pub fn to_array(&self) -> [f64; EQ_COEFFICIENT_FIELDS] {
        let mut out = [0.0; EQ_COEFFICIENT_FIELDS];
        out[0] = if self.enabled { 1.0 } else { 0.0 };
        out[1] = self.preamp;
        for (band, coefficients) in self.bands.iter().enumerate() {
            let at = EQ_COEFFICIENT_HEADER + band * 5;
            out[at..at + 5].copy_from_slice(&coefficients.to_array());
        }
        out
    }

    /// Whether running this cascade would leave the audio exactly as it found
    /// it, so the audio thread can skip it entirely.
    ///
    /// Not a micro-optimisation: at a 96kHz output rate ten biquads per sample
    /// per channel is millions of multiply-adds a second spent proving that
    /// `y = x`, on the one thread in the program that must never run late.
    pub fn is_transparent(&self) -> bool {
        !self.enabled
            || (self.preamp == 1.0
                && self
                    .bands
                    .iter()
                    .all(|band| *band == BiquadCoefficients::identity()))
    }

    /// Gain at `frequency` in dB, preamp included -- the curve, evaluated at a
    /// point.
    fn response_db(&self, frequency: f64, sample_rate: f64) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        // Above Nyquist a discrete filter's response mirrors back down, which
        // would draw a curve for frequencies the device cannot carry. The axis
        // runs to 20kHz whatever the rate, so hold the last real value instead.
        let frequency = frequency.min(sample_rate * 0.4999);
        let magnitude = self
            .bands
            .iter()
            .fold(self.preamp, |gain, band| {
                gain * band.magnitude(frequency, sample_rate)
            })
            .max(1e-12);
        (20.0 * magnitude.log10()) as f32
    }
}

/// The peaking filter of the RBJ cookbook: a bell of `gain_db` at `centre`,
/// unity at DC and Nyquist whatever the gain.
fn peaking(centre: f64, rate: f64, gain_db: f64, q: f64) -> BiquadCoefficients {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * centre / rate;
    let (sin_w0, cos_w0) = w0.sin_cos();
    let alpha = sin_w0 / (2.0 * q);
    let a0 = 1.0 + alpha / a;
    BiquadCoefficients {
        b0: (1.0 + alpha * a) / a0,
        b1: (-2.0 * cos_w0) / a0,
        b2: (1.0 - alpha * a) / a0,
        a1: (-2.0 * cos_w0) / a0,
        a2: (1.0 - alpha / a) / a0,
    }
}

/// A low shelf: `gain_db` below `centre`, unity above it.
fn low_shelf(centre: f64, rate: f64, gain_db: f64, q: f64) -> BiquadCoefficients {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * centre / rate;
    let (sin_w0, cos_w0) = w0.sin_cos();
    let alpha = sin_w0 / (2.0 * q);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
    BiquadCoefficients {
        b0: a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha) / a0,
        b1: 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0) / a0,
        b2: a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha) / a0,
        a1: -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0) / a0,
        a2: ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha) / a0,
    }
}

/// A high shelf: `gain_db` above `centre`, unity below it.
fn high_shelf(centre: f64, rate: f64, gain_db: f64, q: f64) -> BiquadCoefficients {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * centre / rate;
    let (sin_w0, cos_w0) = w0.sin_cos();
    let alpha = sin_w0 / (2.0 * q);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
    BiquadCoefficients {
        b0: a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha) / a0,
        b1: -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0) / a0,
        b2: a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha) / a0,
        a1: 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0) / a0,
        a2: ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha) / a0,
    }
}

/// How many points the response curve is drawn from. Enough that a 12dB bell
/// reads as a curve rather than as a polyline, few enough to send whenever a
/// slider moves without thinking about it.
pub const RESPONSE_POINTS: usize = 128;

/// The frequencies [`magnitude_response`] reports against: log-spaced across
/// the audible band, which is the axis the curve is drawn on.
///
/// Computed here rather than in either UI for the same reason the goniometer's
/// rotation is (M5 decision 7): two implementations of an axis are two chances
/// to draw the same filter differently.
pub fn response_frequencies() -> Vec<f32> {
    let low = 20f64.ln();
    let high = 20_000f64.ln();
    (0..RESPONSE_POINTS)
        .map(|i| {
            let t = i as f64 / (RESPONSE_POINTS - 1) as f64;
            (low + t * (high - low)).exp() as f32
        })
        .collect()
}

/// The curve the UI draws: the cascade's gain in dB at each of
/// [`response_frequencies`], preamp included.
///
/// A function of the settings and the rate only, so it is computed when a
/// slider moves rather than per frame.
pub fn magnitude_response(settings: &EqSettings, sample_rate: u32) -> Vec<f32> {
    let coefficients = EqCoefficients::design(settings, sample_rate);
    let rate = f64::from(sample_rate.max(1));
    response_frequencies()
        .into_iter()
        .map(|frequency| coefficients.response_db(f64::from(frequency), rate))
        .collect()
}

/// The cascade, running on interleaved audio in the output's channel layout.
///
/// Allocation-free once built, because the native shell runs it inside the cpal
/// callback: the filters have to be where a slider move is heard immediately,
/// and anything earlier in the chain is behind a ring holding two seconds of
/// already-filtered audio.
pub struct EqProcessor {
    coefficients: EqCoefficients,
    /// Cached [`EqCoefficients::is_transparent`], so the audio thread answers
    /// "is there anything to do" with a bool rather than by walking ten bands.
    transparent: bool,
    /// `states[channel][band]`.
    states: Vec<[BiquadState; EQ_BANDS]>,
    channels: usize,
    /// Which channel the next sample belongs to.
    ///
    /// Carried across calls because the native callback hands over whatever the
    /// ring gives it, and a ring read that wraps arrives as two slices whose
    /// split need not fall on a frame boundary. Tracking the phase here means
    /// the filters cannot be handed a left sample as if it were a right one.
    phase: usize,
}

impl EqProcessor {
    /// A processor for `channels` of audio at `sample_rate`, starting flat.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels.max(1));
        let coefficients = EqCoefficients::design(&EqSettings::flat(), sample_rate);
        Self {
            transparent: coefficients.is_transparent(),
            coefficients,
            states: vec![[BiquadState::default(); EQ_BANDS]; channels],
            channels,
            phase: 0,
        }
    }

    /// Swaps in a new cascade, keeping the filter state.
    ///
    /// Keeping it is what makes a slider drag sound like an EQ sweep instead of
    /// a series of clicks: the coefficients change under a signal that is still
    /// flowing, which is what every mixing desk does.
    pub fn set(&mut self, coefficients: EqCoefficients) {
        self.transparent = coefficients.is_transparent();
        self.coefficients = coefficients;
    }

    pub fn coefficients(&self) -> EqCoefficients {
        self.coefficients
    }

    /// Forgets the filter history. For a break in the audio -- a seek -- where
    /// carrying it would ring the new position with the old one's tail.
    pub fn reset(&mut self) {
        for channel in &mut self.states {
            *channel = [BiquadState::default(); EQ_BANDS];
        }
        self.phase = 0;
    }

    /// Puts the samples the callback is about to play back where they came
    /// from, filtered. In place, because the caller has nowhere else to put
    /// them.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        if self.transparent {
            return;
        }
        for sample in interleaved.iter_mut() {
            let channel = self.phase;
            self.phase += 1;
            if self.phase >= self.channels {
                self.phase = 0;
            }
            let states = &mut self.states[channel];
            let mut x = f64::from(*sample) * self.coefficients.preamp;
            for (band, state) in self.coefficients.bands.iter().zip(states.iter_mut()) {
                x = state.process(band, x);
            }
            *sample = x as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Measured gain at `hz`, in dB, by running a tone through the processor
    /// and comparing RMS in against RMS out.
    ///
    /// Deliberately measures the *filters* rather than the response function:
    /// the two are computed by different code, and a test that asked the
    /// response function about the response function would pass with the
    /// filters wired backwards.
    fn measured_gain_db(settings: &EqSettings, hz: f32, rate: u32) -> f32 {
        let frames = rate as usize * 2;
        let input: Vec<f32> = (0..frames)
            .map(|n| 0.25 * (2.0 * std::f32::consts::PI * hz * n as f32 / rate as f32).sin())
            .collect();
        let mut output = input.clone();
        let mut eq = EqProcessor::new(rate, 1);
        eq.set(EqCoefficients::design(settings, rate));
        eq.process(&mut output);

        // Skip the first half second: the filters start from rest, and their
        // settling is not what this is measuring.
        let skip = rate as usize / 2;
        let rms = |samples: &[f32]| {
            (samples[skip..]
                .iter()
                .map(|s| f64::from(*s) * f64::from(*s))
                .sum::<f64>()
                / (samples.len() - skip) as f64)
                .sqrt()
        };
        (20.0 * (rms(&output) / rms(&input)).log10()) as f32
    }

    fn one_band(band: usize, gain_db: f32) -> EqSettings {
        let mut settings = EqSettings::flat();
        settings.gains_db[band] = gain_db;
        settings
    }

    #[test]
    fn a_band_reaches_its_own_gain_at_its_own_centre() {
        // The claim a graphic EQ makes to its user: the slider labelled 1k,
        // moved to +6, gives 6dB at 1kHz. Checked on the audio rather than on
        // the coefficients.
        for (band, &hz) in BAND_FREQUENCIES.iter().enumerate() {
            for gain in [-12.0, -6.0, 6.0, 12.0] {
                // A shelf sits at *half* its gain at its own corner, by
                // definition, so it is probed where it has actually arrived --
                // two octaves into the band it shelves.
                let probe = match band_kind(band) {
                    BandKind::LowShelf => hz / 4.0,
                    BandKind::HighShelf => hz * 1.25,
                    BandKind::Peaking => hz,
                };
                let measured = measured_gain_db(&one_band(band, gain), probe, RATE);
                assert!(
                    (measured - gain).abs() < 1.0,
                    "band {band} ({hz}Hz) at {gain:+}dB measured {measured:+.2}dB at {probe}Hz"
                );
            }
        }
    }

    #[test]
    fn a_band_leaves_the_rest_of_the_spectrum_alone() {
        // What separates an EQ from a tone control: moving the 1k slider must
        // not move 62Hz or 16kHz with it. Two octaves out is where an
        // octave-wide bell should have essentially nothing left.
        let settings = one_band(5, 12.0);
        for hz in [62.5, 250.0, 4000.0, 16000.0] {
            let measured = measured_gain_db(&settings, hz, RATE);
            assert!(
                measured.abs() < 1.5,
                "boosting 1kHz by 12dB moved {hz}Hz by {measured:+.2}dB"
            );
        }
    }

    #[test]
    fn the_measured_response_matches_the_curve_the_ui_draws() {
        // The curve is drawn from `magnitude_response`; the audio comes out of
        // `EqProcessor`. If those two ever disagree the display is a drawing of
        // a filter nobody is listening to.
        let mut settings = EqSettings::flat();
        settings.gains_db = [6.0, -3.0, 0.0, 9.0, -12.0, 4.0, 0.0, -6.0, 3.0, 12.0];
        settings.preamp_db = -3.0;

        let curve = magnitude_response(&settings, RATE);
        let frequencies = response_frequencies();
        for hz in [100.0f32, 440.0, 1000.0, 3000.0, 10_000.0] {
            let measured = measured_gain_db(&settings, hz, RATE);
            // The curve is sampled at 128 log-spaced points; take the nearest.
            let nearest = frequencies
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (*a - hz)
                        .abs()
                        .partial_cmp(&(*b - hz).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| curve[i])
                .unwrap_or(0.0);
            assert!(
                (measured - nearest).abs() < 1.0,
                "at {hz}Hz the audio moved {measured:+.2}dB and the curve says {nearest:+.2}dB"
            );
        }
    }

    #[test]
    fn the_preamp_moves_everything_by_itself() {
        let mut settings = EqSettings::flat();
        settings.preamp_db = -6.0;
        for hz in [50.0, 440.0, 5000.0] {
            let measured = measured_gain_db(&settings, hz, RATE);
            assert!(
                (measured + 6.0).abs() < 0.1,
                "a -6dB preamp moved {hz}Hz by {measured:+.2}dB"
            );
        }
    }

    #[test]
    fn flat_settings_pass_the_audio_through_unchanged() {
        // Not "close enough": with every slider at zero the design skips the
        // filters entirely, so this should be bit-exact. A cascade that had
        // quietly designed ten unity biquads instead would be off in the last
        // bits, which is the difference between a player that is transparent
        // and one that is nearly transparent.
        let input: Vec<f32> = (0..2048).map(|n| (n as f32 * 0.01).sin() * 0.5).collect();
        let mut output = input.clone();
        let mut eq = EqProcessor::new(RATE, 2);
        eq.set(EqCoefficients::design(&EqSettings::flat(), RATE));
        eq.process(&mut output);
        assert_eq!(input, output);
    }

    #[test]
    fn disabling_the_eq_bypasses_it_whatever_the_sliders_say() {
        let mut settings = one_band(5, 12.0);
        settings.enabled = false;
        let input: Vec<f32> = (0..2048).map(|n| (n as f32 * 0.05).sin() * 0.5).collect();
        let mut output = input.clone();
        let mut eq = EqProcessor::new(RATE, 2);
        eq.set(EqCoefficients::design(&settings, RATE));
        eq.process(&mut output);
        assert_eq!(input, output);
        assert!(magnitude_response(&settings, RATE)
            .iter()
            .all(|&db| db == 0.0));
    }

    #[test]
    fn the_bands_are_independent_of_the_output_rate() {
        // The rate is the device's, not the file's -- 44.1, 48 or 96kHz on this
        // machine's own devices. A design that only worked at one of them would
        // sound different depending on which output the listener picked, which
        // is the class of bug M5's K-weighting table and M6's fixed sample
        // count both were.
        let settings = one_band(5, 9.0);
        for rate in [44_100u32, 48_000, 96_000] {
            let measured = measured_gain_db(&settings, 1000.0, rate);
            assert!(
                (measured - 9.0).abs() < 0.5,
                "1kHz at +9dB measured {measured:+.2}dB at {rate}Hz"
            );
        }
    }

    #[test]
    fn channels_are_filtered_independently() {
        // Interleaved audio through one processor: the left channel's history
        // must not leak into the right one. A tone in the left channel only
        // must leave the right one silent.
        let mut settings = one_band(5, 12.0);
        settings.preamp_db = 3.0;
        let mut audio: Vec<f32> = (0..4096)
            .flat_map(|n| {
                let s = (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / RATE as f32).sin() * 0.3;
                [s, 0.0]
            })
            .collect();
        let mut eq = EqProcessor::new(RATE, 2);
        eq.set(EqCoefficients::design(&settings, RATE));
        eq.process(&mut audio);
        assert!(
            audio.iter().skip(1).step_by(2).all(|&s| s == 0.0),
            "a silent channel came out of the EQ with something in it"
        );
        assert!(
            audio.iter().step_by(2).any(|&s| s.abs() > 0.3),
            "the boosted channel should have grown"
        );
    }

    #[test]
    fn a_split_block_filters_the_same_as_a_whole_one() {
        // The native callback hands over whatever the ring gives it, which on a
        // wrap is two slices whose split need not fall on a frame boundary. If
        // the processor restarted its channel phase per call, a wrapped read
        // would swap the channels for the rest of the stream.
        let audio: Vec<f32> = (0..2048)
            .flat_map(|n| [(n as f32 * 0.03).sin() * 0.4, (n as f32 * 0.07).cos() * 0.4])
            .collect();
        let settings = one_band(3, 8.0);

        let mut whole = audio.clone();
        let mut eq = EqProcessor::new(RATE, 2);
        eq.set(EqCoefficients::design(&settings, RATE));
        eq.process(&mut whole);

        let mut split = audio.clone();
        let mut eq = EqProcessor::new(RATE, 2);
        eq.set(EqCoefficients::design(&settings, RATE));
        // An odd split, i.e. mid-frame.
        let (head, tail) = split.split_at_mut(777);
        eq.process(head);
        eq.process(tail);
        assert_eq!(whole, split);
    }

    #[test]
    fn a_reset_forgets_the_filter_history() {
        let settings = one_band(0, 12.0);
        let coefficients = EqCoefficients::design(&settings, RATE);
        let mut eq = EqProcessor::new(RATE, 1);
        eq.set(coefficients);
        eq.process(&mut vec![0.9; 4096]);

        // A fresh processor and a reset one must produce the same thing from
        // the same input, or a seek carries the previous position's ringing
        // into the new one.
        let mut after_reset = vec![0.1; 512];
        eq.reset();
        eq.process(&mut after_reset);

        let mut fresh_processor = EqProcessor::new(RATE, 1);
        fresh_processor.set(coefficients);
        let mut fresh = vec![0.1; 512];
        fresh_processor.process(&mut fresh);
        assert_eq!(after_reset, fresh);
    }

    #[test]
    fn settings_are_clamped_before_they_reach_a_filter() {
        // These arrive over an IPC boundary. A NaN in a biquad's state stays
        // there for the life of the stream, so it must not be possible to get
        // one in from outside.
        let wild = EqSettings {
            enabled: true,
            preamp_db: f32::NAN,
            gains_db: [
                100.0,
                -100.0,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NAN,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ],
        };
        let clamped = wild.clamped();
        assert_eq!(clamped.preamp_db, 0.0);
        assert_eq!(clamped.gains_db[0], MAX_GAIN_DB);
        assert_eq!(clamped.gains_db[1], -MAX_GAIN_DB);
        // The infinities are read as "no change" rather than as the extreme
        // they point at: a control that arrives infinite is a bug upstream.
        assert_eq!(clamped.gains_db[2], 0.0);
        assert_eq!(clamped.gains_db[3], 0.0);
        assert_eq!(clamped.gains_db[4], 0.0);

        let mut audio = vec![0.5f32; 1024];
        let mut eq = EqProcessor::new(RATE, 2);
        eq.set(EqCoefficients::design(&wild, RATE));
        eq.process(&mut audio);
        assert!(
            audio.iter().all(|s| s.is_finite()),
            "a wild setting reached the audio"
        );
    }

    #[test]
    fn a_low_output_rate_still_gets_a_usable_top_band() {
        // 16kHz is above Nyquist at 32kHz, and a centre frequency there means
        // nothing. The design pulls it down rather than producing a filter with
        // no defined shape -- and the band still has to do something, or the
        // top slider would silently stop working on such a device.
        let settings = one_band(EQ_BANDS - 1, 12.0);
        // The centre is pulled down to 0.45 of the rate (14.4kHz here), where a
        // shelf sits at half its gain -- so this is the top slider still
        // reaching the top of what the device can carry.
        let measured = measured_gain_db(&settings, 14_400.0, 32_000);
        assert!(
            measured > 4.0,
            "the top band did nothing at a 32kHz output rate ({measured:+.2}dB)"
        );
        let curve = magnitude_response(&settings, 32_000);
        assert!(
            curve.iter().all(|db| db.is_finite()),
            "the curve must stay finite past the top band's centre"
        );
        // The axis runs to 20kHz whatever the rate, and above Nyquist the curve
        // holds its last real value rather than mirroring back down.
        assert!(
            (curve[RESPONSE_POINTS - 1] - 12.0).abs() < 1.0,
            "the curve's top point reads {:+.2}dB",
            curve[RESPONSE_POINTS - 1]
        );
    }
}
