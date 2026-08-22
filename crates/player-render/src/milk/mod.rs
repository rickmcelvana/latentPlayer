//! MilkDrop `.milk` preset support -- Phase 3 M7's de-risk spike (decision 7).
//!
//! Nothing here is wired into a [`crate::Visual`] yet, on purpose. The
//! milestone was planned to open with a spike -- "parse one classic preset, run
//! its equations, render offscreen" -- because the engine's three unknowns
//! (the format's real surface, the EEL language's cost, the distance from HLSL
//! to WGSL) are the kind that decide a build's shape rather than its details.
//! `crates/player-render/examples/milk_spike.rs` is the spike's report.
//!
//! Route, per decision 7: Butterchurn-guided (MIT), never projectM (LGPL).
//! Guided rather than ported -- Butterchurn transpiles EEL to JavaScript and
//! HLSL to GLSL through a fork of Unity's `hlslparser`, neither of which is a
//! move available here. What is taken from it is the *format reading*, which is
//! the part where a decade of real presets has already found the corner cases.

pub mod eel;
pub mod frame;
pub mod parse;
pub mod state;
pub mod warp;
pub mod wave;

pub use frame::{CompositeSettings, MilkFrame, WarpVertex, DEFAULT_MESH};
pub use parse::{CustomShape, CustomWave, ParseError, Preset, Unsupported};
pub use state::{Fault, FrameState, Inputs, Motion, Runtime};
pub use warp::{Aspect, Grid, GridPoint, Warp};
pub use wave::{Wave, WaveInput};
