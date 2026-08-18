//! The visuals themselves -- one module per entry in
//! [`VisualKind`](crate::VisualKind).
//!
//! Everything in here implements [`Visual`](crate::Visual) and nothing in here
//! knows which shell it is running in, how big the window is, whether it is
//! fullscreen, or that a gallery exists at all. That is the host's half of the
//! seam (see `crate::visual`), and the reason a new visual is a file here plus
//! one line of registry rather than a change to either shell.

mod vectorscope;

pub use vectorscope::Vectorscope;
