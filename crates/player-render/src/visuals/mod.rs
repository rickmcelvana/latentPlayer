//! The visuals themselves -- one module per entry in
//! [`VisualKind`](crate::VisualKind).
//!
//! Everything in here implements [`Visual`](crate::Visual) and nothing in here
//! knows which shell it is running in, how big the window is, whether it is
//! fullscreen, or that a gallery exists at all. That is the host's half of the
//! seam (see `crate::visual`), and the reason a new visual is a file here plus
//! one line of registry rather than a change to either shell.

mod bars;
mod history;
mod particles;
mod scale;
mod tunnel;
mod vectorscope;
mod waterfall;

pub use bars::SpectrumBars;
pub use particles::ParticleField;
pub use tunnel::{OscilloscopeTunnel, TUNNEL_SECONDS};
pub use vectorscope::Vectorscope;
pub use waterfall::{SpectrogramWaterfall, HISTORY_SECONDS};
