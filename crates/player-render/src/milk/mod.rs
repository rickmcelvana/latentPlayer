//! MilkDrop `.milk` preset support (Phase 3 M7).
//!
//! The milestone opened with a de-risk spike (decision 7) -- "parse one classic
//! preset, run its equations, render offscreen" -- because the engine's three
//! unknowns (the format's real surface, the EEL language's cost, the distance
//! from HLSL to WGSL) decide a build's shape rather than its details. It
//! reported, and M7 split three ways on what it found: **M7a** a preset draws,
//! **M7b** a preset draws as itself (the shader translator), **M7c** custom
//! waves and shapes and the preset library. This is M7a.
//!
//! `crates/player-render/examples/milk_smoke.rs` is the spike's grown-up form:
//! it runs a whole folder of presets and asserts rather than reports.
//!
//! # Where this comes from, and under what licence
//!
//! Two separate questions, and the answers differ.
//!
//! **The engine's route** is decision 7's: Butterchurn-guided (MIT), never
//! projectM (LGPL). Guided rather than ported -- Butterchurn transpiles EEL to
//! JavaScript and HLSL to GLSL through a fork of Unity's `hlslparser`, neither
//! of which is a move available to a Rust core that also runs natively. What is
//! taken from it is the *format reading*, which is the part where a decade of
//! real presets has already found the corner cases.
//!
//! **The arithmetic** -- the per-vertex warp, the waveform modes, the composite
//! and every scalar's default -- is read out of MilkDrop's own source, because
//! this is the one place in the milestone where writing from memory is both
//! specifically forbidden and specifically tempting: a transform that is subtly
//! wrong is invisible in a still frame and obvious in motion.
//!
//! That source is **MilkDrop 2, Copyright 2005-2013 Nullsoft, Inc., released
//! under the 3-clause BSD licence** (the header of every file in
//! `vis_milk2/`). Checked rather than assumed, and worth stating precisely,
//! because decision 57's licence check found the *MilkDrop3* repository -- the
//! one this project's test presets came from -- declaring no licence at all.
//! Those are different repositories with different answers: reading Nullsoft's
//! BSD-licensed original is clean, and no `.milk` file ships with this app.
//!
//! The files leaned on, so a reader can check any line against its source:
//! `milkdropfs.cpp` (the per-vertex warp loop, `DrawWave`, `SmoothWave`,
//! `DrawSprites`, the composite), `plugin.cpp` (the aspect pair and the mesh
//! grid), and `state.cpp` (`CState::Default` and the export table that names
//! every `.milk` key).
//!
//! One line of MilkDrop's transform is deliberately **not** copied -- its
//! half-texel offset, which is a Direct3D 9 artefact and a per-frame
//! displacement anywhere else. See [`warp`]'s module note.

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
