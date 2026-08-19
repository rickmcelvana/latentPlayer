//! The host seam (Phase 3 M1): what a *visual* is, and what the host hands it.
//!
//! Before this, the renderer owned a surface and the one scope, and
//! `draw_frame(&[f32], channels)` was the vectorscope's signature rather than a
//! visual's. PLAN.md section 4 asks for a gallery instead, which means deciding
//! once where the line between "the host" and "a visual" falls rather than
//! discovering it per visual (tasks/phase-3.md decision 1):
//!
//! > **The host owns the surface, the analysis feed, selection and fullscreen.
//! > A visual owns only its scene.**
//!
//! So a [`Visual`] is handed a device, a target view and a [`VisualFrame`], and
//! gives back a command buffer. It never sees the surface, never learns which
//! shell it is in, and -- the rule that keeps the listener-facing visuals and
//! the studio meters honest (decision 2) -- **computes no DSP**. Everything in
//! the frame is measured once, off the audio thread, by the same `player-core`
//! analysis the meters draw.

/// The GPU handles a visual is allowed to touch.
///
/// Handed over per call rather than stored: the host owns the device and the
/// surface format, and a visual keeping its own copy would be a visual with an
/// opinion about the surface it draws into.
pub struct Gpu<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    /// The colour format of the view [`Visual::draw`] is given. Fixed for the
    /// life of a visual: it is the surface's, and swapping visuals never swaps
    /// the surface (decision 5).
    pub format: wgpu::TextureFormat,
}

/// One frame's worth of what the audio is doing, assembled by the host.
///
/// Deliberately a struct rather than a widening argument list: M2's spectrum
/// bars need the metering spectrum's own bands and M5's scenes need beat and
/// onset figures, and each of those is a field added here -- not a new
/// signature every visual has to be edited for.
///
/// Borrowed rather than owned because the host already holds this data (the
/// tap's window, the meters' results) and should copy it no more than once per
/// frame.
pub struct VisualFrame<'a> {
    /// Interleaved samples that reached the output, newest last, in the output
    /// device's channel layout -- exactly what the analysis tap holds. Empty
    /// before anything has played, which a visual draws as *nothing* rather
    /// than as a signal sitting at zero.
    pub samples: &'a [f32],
    /// The interleaving width of `samples`. Not assumed to be 2: the tap
    /// follows the output device, and a mono or 5.1 device would otherwise be
    /// de-interleaved wrongly.
    pub channels: u16,
    /// The rate `samples` were played at. What turns a sample count into a
    /// duration, so a visual means the same thing on a 44.1kHz device and a
    /// 96kHz one.
    pub sample_rate: u32,
    /// Seconds since the previous frame, as the shell measures it (its redraw
    /// interval natively, the gap between animation callbacks on the web).
    ///
    /// Anything that moves over time needs this, and it has to come from the
    /// shell: there is no clock in this crate that works on both targets
    /// (`Instant::now` does not exist on wasm). Used by [`GainSetting::Auto`]'s
    /// release below, and by every animated visual after M1.
    pub elapsed_seconds: f32,
    /// The spectrum meter's own levels, dBFS per display band, low frequency
    /// first -- **the same array the studio Spectrum panel draws**, published
    /// off the audio thread by `player-core::spectrum` on both shells.
    ///
    /// Empty when nothing has measured yet, which a visual draws as *nothing*
    /// rather than as sixty-four bars resting on the floor: the two are
    /// different pictures, and only one of them is true before a track loads.
    /// Its length is the band count and is not a constant a visual may assume
    /// (M2 decision 10) -- the web's rows are empty until the shared buffer
    /// exists, where the native side always answers with a full row.
    pub levels: &'a [f32],
    /// Centre frequency of each entry in [`Self::levels`], in Hz. The axis
    /// those levels are against, so a visual can place a landmark -- an octave,
    /// a crossover -- at the frequency it actually belongs to rather than at a
    /// band index that would move if the band count ever did.
    ///
    /// Fixed for a given output rate, and empty on the same terms as
    /// `levels`.
    pub band_centres: &'a [f32],
    /// How much to magnify the signal for display, 1.0 being unity.
    ///
    /// **Not analysis, and the one field a shell does not fill in**: the host
    /// resolves it per frame from the chosen [`GainSetting`] and overwrites
    /// whatever was here. It is in the frame rather than beside it so one
    /// control means the same thing to every visual -- amplitudes drawn this
    /// much larger -- and so a visual never has to ask the host anything.
    pub gain: f32,
}

impl<'a> VisualFrame<'a> {
    /// A frame at unity gain. The host replaces [`Self::gain`] before any visual
    /// sees it, so a shell assembling one never sets it.
    pub fn new(samples: &'a [f32], channels: u16, sample_rate: u32, elapsed_seconds: f32) -> Self {
        Self {
            samples,
            channels,
            sample_rate,
            elapsed_seconds,
            levels: &[],
            band_centres: &[],
            gain: 1.0,
        }
    }

    /// Adds the metering spectrum to the frame.
    ///
    /// Chained rather than folded into [`Self::new`] so that a caller with no
    /// spectrum to offer says nothing instead of passing two empty slices, and
    /// so that adding the *next* analysis field in M5 is again an addition
    /// rather than an edit to every construction site in the project.
    pub fn with_spectrum(mut self, levels: &'a [f32], band_centres: &'a [f32]) -> Self {
        self.levels = levels;
        self.band_centres = band_centres;
        self
    }

    /// Where `hz` falls along [`Self::levels`], as a fractional band index.
    ///
    /// `None` when there is no axis, or when the frequency is outside the one
    /// there is -- a landmark off the end of the display is not drawn at the
    /// end of it.
    ///
    /// Interpolated in **log** frequency, because the bands are log-spaced and
    /// a linear interpolation between two centres an octave apart would put the
    /// midpoint a long way from where a person reading the display expects it.
    /// Lives here rather than in a visual because every visual that draws a
    /// frequency landmark needs exactly this, and two of them now use it (the
    /// bars' octave marks, the waterfall's rails).
    pub fn band_position(&self, hz: f32) -> Option<f32> {
        let centres = self.band_centres;
        if centres.len() < 2 || !hz.is_finite() || hz <= 0.0 {
            return None;
        }
        let (first, last) = (centres[0], centres[centres.len() - 1]);
        if hz < first || hz > last {
            return None;
        }
        let target = hz.ln();
        let upper = centres.iter().position(|c| *c >= hz)?;
        if upper == 0 {
            return Some(0.0);
        }
        let (lo, hi) = (centres[upper - 1].ln(), centres[upper].ln());
        let span = hi - lo;
        let fraction = if span.abs() < f32::EPSILON {
            0.0
        } else {
            (target - lo) / span
        };
        Some(upper as f32 - 1.0 + fraction)
    }

    /// The loudest sample in the frame, as an amplitude.
    ///
    /// A summary of the data the frame already carries rather than a
    /// measurement of its own -- which is why the *host* may compute it (for
    /// auto gain) without breaking decision 2. It cannot disagree with the
    /// meters, because it describes exactly the samples being drawn.
    pub fn peak(&self) -> f32 {
        self.samples
            .iter()
            .fold(0.0f32, |loudest, sample| loudest.max(sample.abs()))
    }

    /// How much audio this frame holds, in frames rather than in numbers --
    /// which is what a visual actually reasons about.
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

/// A scene the host can draw. One implementor per entry in [`VisualKind`].
///
/// Held boxed by the renderer, so this is object-safe on purpose: `set_visual`
/// swaps the box and everything around it -- surface, window, canvas, redraw
/// loop -- carries on untouched (decision 5).
pub trait Visual {
    /// The viewport changed size. Whatever the visual keeps that depends on it
    /// (depth and multisample attachments, a projection) is rebuilt here; the
    /// host has already reconfigured the surface itself.
    fn resize(&mut self, gpu: &Gpu, width: u32, height: u32);

    /// Draws one frame into `target`, returning the commands to submit.
    ///
    /// Submitting and presenting are the host's: a visual that submitted its
    /// own work could not be composed with anything, and the offscreen path
    /// needs to add its readback copy to the same queue behind this.
    fn draw(
        &mut self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        frame: &VisualFrame,
    ) -> wgpu::CommandBuffer;
}

/// The boxed form the renderer holds.
///
/// Natively the whole renderer is moved onto its own redraw thread, so the box
/// has to be `Send`; on wasm nothing in wgpu is `Send` and requiring it would
/// simply not compile. The bound therefore differs by target -- the honest
/// shape of the constraint rather than a workaround for it.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxedVisual = Box<dyn Visual + Send>;
#[cfg(target_arch = "wasm32")]
pub type BoxedVisual = Box<dyn Visual>;

/// The gallery, as data (decision 3).
///
/// One registry, in Rust, cross-target: the gallery UI, the desktop commands,
/// the wasm bindings and both smoke checks all name visuals from here. The
/// alternative -- a list per shell -- is two lists that agree right up until
/// someone adds the third visual to one of them.
///
/// [`id`](Self::id) is the stable name that crosses process and storage
/// boundaries (a command argument, a stored selection); [`name`](Self::name)
/// and [`description`](Self::description) are what a person reads. An id never
/// changes once shipped, because a stored selection outlives a release.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VisualKind {
    /// PLAN.md section 4's hero, shipped in Phase 1 M6 and this host's first
    /// citizen.
    Vectorscope,
    /// PLAN.md section 4's second built-in (Phase 3 M2): the metering
    /// spectrum's own bands as instanced 3D bars.
    SpectrumBars,
    /// PLAN.md section 4's third built-in (Phase 3 M3): the same bands kept,
    /// as a scrolling terrain -- the first visual with history of its own.
    SpectrogramWaterfall,
    /// PLAN.md section 4's fourth built-in (Phase 3 M4): the waveform swept
    /// along a path -- the first visual whose camera moves.
    OscilloscopeTunnel,
}

impl VisualKind {
    /// Every visual, in the order the gallery offers them.
    pub const ALL: &'static [VisualKind] = &[
        VisualKind::Vectorscope,
        VisualKind::SpectrumBars,
        VisualKind::SpectrogramWaterfall,
        VisualKind::OscilloscopeTunnel,
    ];

    /// What the gallery opens at for anyone who has never chosen.
    pub const DEFAULT: VisualKind = VisualKind::Vectorscope;

    pub fn id(self) -> &'static str {
        match self {
            VisualKind::Vectorscope => "vectorscope",
            VisualKind::SpectrumBars => "spectrum-bars",
            VisualKind::SpectrogramWaterfall => "spectrogram-waterfall",
            VisualKind::OscilloscopeTunnel => "oscilloscope-tunnel",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            VisualKind::Vectorscope => "3D vectorscope",
            VisualKind::SpectrumBars => "Spectrum bars",
            VisualKind::SpectrogramWaterfall => "Spectrogram waterfall",
            VisualKind::OscilloscopeTunnel => "Oscilloscope tunnel",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            VisualKind::Vectorscope => {
                "The stereo field extruded along time, on the goniometer's own axes."
            }
            VisualKind::SpectrumBars => {
                "The analyser's own bands as 3D bars, with peaks that fall and the octaves marked."
            }
            VisualKind::SpectrogramWaterfall => {
                "The last six seconds of the spectrum as a terrain, newest at the front."
            }
            VisualKind::OscilloscopeTunnel => {
                "The waveform wrapped into rings and flown through, newest furthest ahead."
            }
        }
    }

    /// The inverse of [`id`](Self::id). `None` rather than a default, so a
    /// stored selection naming a visual this build does not have is something
    /// the caller decides about instead of silently becoming something else.
    pub fn from_id(id: &str) -> Option<Self> {
        VisualKind::ALL.iter().copied().find(|k| k.id() == id)
    }

    /// Builds the scene. Construction lives here, beside the registry, so no
    /// shell ever names a concrete visual type -- it names a kind.
    pub fn create(self, gpu: &Gpu, width: u32, height: u32) -> BoxedVisual {
        match self {
            VisualKind::Vectorscope => {
                Box::new(crate::visuals::Vectorscope::new(gpu, width, height))
            }
            VisualKind::SpectrumBars => {
                Box::new(crate::visuals::SpectrumBars::new(gpu, width, height))
            }
            VisualKind::SpectrogramWaterfall => Box::new(
                crate::visuals::SpectrogramWaterfall::new(gpu, width, height),
            ),
            VisualKind::OscilloscopeTunnel => {
                Box::new(crate::visuals::OscilloscopeTunnel::new(gpu, width, height))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 60Hz frame, which is what both shells aim for.
    const FRAME: f32 = 1.0 / 60.0;

    #[test]
    fn every_kind_round_trips_through_its_id() {
        for kind in VisualKind::ALL {
            assert_eq!(VisualKind::from_id(kind.id()), Some(*kind));
        }
    }

    #[test]
    fn ids_and_names_are_unique_and_present() {
        // Two visuals sharing an id is a stored selection that silently
        // becomes the other one; two sharing a name is a gallery nobody can
        // use. Cheap to assert, and only easier to break from here on.
        let mut ids: Vec<_> = VisualKind::ALL.iter().map(|k| k.id()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "visual ids must be unique");

        let mut names: Vec<_> = VisualKind::ALL.iter().map(|k| k.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "visual names must be unique");

        assert!(VisualKind::ALL
            .iter()
            .all(|k| !k.id().is_empty() && !k.name().is_empty() && !k.description().is_empty()));
    }

    #[test]
    fn an_unknown_id_is_not_quietly_substituted() {
        assert_eq!(VisualKind::from_id("milkdrop"), None);
        assert_eq!(VisualKind::from_id(""), None);
    }

    #[test]
    fn the_default_is_one_of_the_offered_visuals() {
        assert!(VisualKind::ALL.contains(&VisualKind::DEFAULT));
    }

    #[test]
    fn a_frame_starts_at_unity_gain_and_reports_its_own_peak() {
        // Unity because the *host* fills gain in (see `Renderer::draw_frame`);
        // a shell that set it would be a second opinion about a display setting.
        let quiet = [0.1f32, -0.25, 0.05];
        let frame = VisualFrame::new(&quiet, 1, 48_000, FRAME);
        assert_eq!(frame.gain, 1.0);
        assert_eq!(frame.peak(), 0.25);
        assert_eq!(VisualFrame::new(&[], 2, 48_000, FRAME).peak(), 0.0);
    }

    #[test]
    fn a_frame_reads_its_length_in_time_rather_than_in_numbers() {
        let stereo = [0.0f32; 8];
        assert_eq!(VisualFrame::new(&stereo, 2, 48_000, FRAME).frames(), 4);
        assert_eq!(VisualFrame::new(&stereo, 1, 48_000, FRAME).frames(), 8);
        // A layout of zero channels is nonsense no caller should be able to
        // divide by.
        assert_eq!(VisualFrame::new(&stereo, 0, 48_000, FRAME).frames(), 8);
        assert_eq!(VisualFrame::new(&[], 2, 48_000, FRAME).frames(), 0);
    }
}
