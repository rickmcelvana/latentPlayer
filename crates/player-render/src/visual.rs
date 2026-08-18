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
}

impl<'a> VisualFrame<'a> {
    pub fn new(samples: &'a [f32], channels: u16, sample_rate: u32) -> Self {
        Self {
            samples,
            channels,
            sample_rate,
        }
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
}

impl VisualKind {
    /// Every visual, in the order the gallery offers them.
    pub const ALL: &'static [VisualKind] = &[VisualKind::Vectorscope];

    /// What the gallery opens at for anyone who has never chosen.
    pub const DEFAULT: VisualKind = VisualKind::Vectorscope;

    pub fn id(self) -> &'static str {
        match self {
            VisualKind::Vectorscope => "vectorscope",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            VisualKind::Vectorscope => "3D vectorscope",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            VisualKind::Vectorscope => {
                "The stereo field extruded along time, on the goniometer's own axes."
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_frame_reads_its_length_in_time_rather_than_in_numbers() {
        let stereo = [0.0f32; 8];
        assert_eq!(VisualFrame::new(&stereo, 2, 48_000).frames(), 4);
        assert_eq!(VisualFrame::new(&stereo, 1, 48_000).frames(), 8);
        // A layout of zero channels is nonsense no caller should be able to
        // divide by.
        assert_eq!(VisualFrame::new(&stereo, 0, 48_000).frames(), 8);
        assert_eq!(VisualFrame::new(&[], 2, 48_000).frames(), 0);
    }
}
