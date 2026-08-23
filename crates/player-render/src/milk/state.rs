//! A preset's state: the scalars it carries, and the equations that move them.
//!
//! MilkDrop's design here is worth stating plainly, because it decides this
//! module's shape. A `.milk` file is **not** a program with some settings
//! attached; it is a set of ~70 named scalars, most of which are also
//! *variables the preset's own code may overwrite every frame*. `fDecay=0.98`
//! in the file and `decay = 0.9 + 0.1*bass` in `per_frame` are the same
//! quantity reached two ways, and the renderer reads only the second.
//!
//! So the scalars are not a struct the renderer consults. They are the values
//! the per-frame variables are **reset to** before each frame's code runs --
//! which is exactly what MilkDrop's `LoadPerFrameEvallibVars` does -- and the
//! renderer reads the variables afterwards. Doing it the other way (a struct of
//! defaults, patched by whatever the code happened to write) is the same thing
//! said less clearly, and gets the precedence wrong the moment a preset assigns
//! a variable conditionally.
//!
//! Every default and every key/variable pairing below is read from MilkDrop 2's
//! own source (`state.cpp`'s `CState::Default` and its export table,
//! `milkdropfs.cpp`'s `LoadPerFrameEvallibVars`), per the milestone's rule that
//! this is the one place writing from memory is forbidden -- see
//! `crates/player-render/src/milk/mod.rs` for the attribution and the licence.

use std::collections::BTreeMap;

use crate::milk::eel::{self, Context, Program, Symbols};

/// How many `q` variables carry from a preset's init block into its frames.
pub const Q_COUNT: usize = 32;

/// A scalar that is also a per-frame variable.
///
/// [`key`](Self::key) is what a `.milk` file writes, [`var`](Self::var) is what
/// the equations call it, and the two are frequently *not* the same word --
/// `fDecay` is `decay`, `nMotionVectorsX` is `mv_x`, `fWaveParam` is
/// `wave_mystery`. A table rather than a naming rule, because there is no rule.
pub struct ScalarDef {
    pub key: &'static str,
    pub var: &'static str,
    pub default: f64,
}

const fn s(key: &'static str, var: &'static str, default: f64) -> ScalarDef {
    ScalarDef { key, var, default }
}

/// Every scalar the per-frame code can read or write, with MilkDrop's default.
///
/// The ten motion variables come first and are the ones `per_pixel` may
/// overwrite again per vertex; the rest are read once per frame.
pub const SCALARS: &[ScalarDef] = &[
    // Motion -- these ten are re-read per vertex after `per_pixel` runs.
    s("zoom", "zoom", 1.0),
    s("fZoomExponent", "zoomexp", 1.0),
    s("rot", "rot", 0.0),
    s("warp", "warp", 1.0),
    s("cx", "cx", 0.5),
    s("cy", "cy", 0.5),
    s("dx", "dx", 0.0),
    s("dy", "dy", 0.0),
    s("sx", "sx", 1.0),
    s("sy", "sy", 1.0),
    // The feedback loop and the composite.
    s("fDecay", "decay", 0.98),
    s("fGammaAdj", "gamma", 2.0),
    s("fVideoEchoZoom", "echo_zoom", 2.0),
    s("fVideoEchoAlpha", "echo_alpha", 0.0),
    s("nVideoEchoOrientation", "echo_orient", 0.0),
    s("bTexWrap", "wrap", 1.0),
    s("bDarkenCenter", "darken_center", 0.0),
    s("bBrighten", "brighten", 0.0),
    s("bDarken", "darken", 0.0),
    s("bSolarize", "solarize", 0.0),
    s("bInvert", "invert", 0.0),
    // The built-in waveform.
    s("nWaveMode", "wave_mode", 0.0),
    s("fWaveAlpha", "wave_a", 0.8),
    s("wave_r", "wave_r", 1.0),
    s("wave_g", "wave_g", 1.0),
    s("wave_b", "wave_b", 1.0),
    s("wave_x", "wave_x", 0.5),
    s("wave_y", "wave_y", 0.5),
    s("fWaveParam", "wave_mystery", 0.0),
    s("bWaveDots", "wave_usedots", 0.0),
    s("bWaveThick", "wave_thick", 0.0),
    s("bAdditiveWaves", "wave_additive", 0.0),
    s("bMaximizeWaveColor", "wave_brighten", 1.0),
    // Borders.
    s("ob_size", "ob_size", 0.01),
    s("ob_r", "ob_r", 0.0),
    s("ob_g", "ob_g", 0.0),
    s("ob_b", "ob_b", 0.0),
    s("ob_a", "ob_a", 0.0),
    s("ib_size", "ib_size", 0.01),
    s("ib_r", "ib_r", 0.25),
    s("ib_g", "ib_g", 0.25),
    s("ib_b", "ib_b", 0.25),
    s("ib_a", "ib_a", 0.0),
    // Motion vectors -- read here so a preset's code can drive them, drawn in
    // M7c. Present now because leaving them out would let a preset's
    // `mv_x = 40` silently create a *new* variable instead of setting a known
    // one, which is a different program.
    s("nMotionVectorsX", "mv_x", 12.0),
    s("nMotionVectorsY", "mv_y", 9.0),
    s("mv_dx", "mv_dx", 0.0),
    s("mv_dy", "mv_dy", 0.0),
    s("mv_l", "mv_l", 0.9),
    s("mv_r", "mv_r", 1.0),
    s("mv_g", "mv_g", 1.0),
    s("mv_b", "mv_b", 1.0),
    s("mv_a", "mv_a", 1.0),
    // The blur chain's ranges. Same reasoning: the variables exist so a
    // preset's arithmetic lands somewhere real. The passes themselves are M7b,
    // where the shaders that sample them live.
    s("b1n", "blur1min", 0.0),
    s("b2n", "blur2min", 0.0),
    s("b3n", "blur3min", 0.0),
    s("b1x", "blur1max", 1.0),
    s("b2x", "blur2max", 1.0),
    s("b3x", "blur3max", 1.0),
    s("b1ed", "blur1edge", 0.25),
];

/// The first entry in [`SCALARS`] that is not one of the ten motion variables.
const MOTION_COUNT: usize = 10;

/// The scalars a preset's code cannot reach.
///
/// MilkDrop never exposes these as `var_pf_*`, so they are read from the file
/// once and used directly. Keeping them separate is not tidiness: a preset that
/// writes `fWarpScale = 2` in `per_frame` is writing to a variable nothing
/// reads, and that has to stay true here or the two engines diverge on a
/// preset nobody would think to test.
#[derive(Clone, Copy, Debug)]
pub struct Fixed {
    /// Timescale for the warp animation's own clock.
    pub warp_anim_speed: f32,
    /// Spatial scale of the warp's sine terms, used as its reciprocal.
    pub warp_scale: f32,
    /// How much the composite's corner tint departs from white, 0..1.
    pub shader: f32,
    /// Amplitude the waveform samples are drawn at.
    pub wave_scale: f32,
    /// One-pole smoothing along the waveform, 0 (none) to 0.9 (heavy).
    pub wave_smoothing: f32,
    pub mod_wave_alpha_by_volume: bool,
    pub mod_wave_alpha_start: f32,
    pub mod_wave_alpha_end: f32,
}

impl Default for Fixed {
    fn default() -> Self {
        Self {
            warp_anim_speed: 1.0,
            warp_scale: 1.0,
            shader: 0.0,
            wave_scale: 1.0,
            wave_smoothing: 0.75,
            mod_wave_alpha_by_volume: false,
            mod_wave_alpha_start: 0.75,
            mod_wave_alpha_end: 0.95,
        }
    }
}

/// The file's scalars as numbers, keyed by the lowercase of their name.
///
/// Lowercased because `.milk` keys are read through Windows' profile API,
/// which is case-insensitive, and the corpus already proves the *variable*
/// names are too (one preset writes `MINBPM` and reads `minbpm`). A key-case
/// difference silently reverting a preset to a default is exactly the kind of
/// wrong answer a collection of a thousand files hides well.
fn numeric_base(base: &BTreeMap<String, String>) -> BTreeMap<String, f64> {
    base.iter()
        .filter_map(|(k, v)| {
            let n = v.trim().parse::<f64>().ok().filter(|n| n.is_finite())?;
            Some((k.to_ascii_lowercase(), n))
        })
        .collect()
}

impl Fixed {
    fn from_base(base: &BTreeMap<String, f64>) -> Self {
        let mut fixed = Fixed::default();
        let read = |key: &str, fallback: f32| -> f32 {
            base.get(&key.to_ascii_lowercase())
                .map(|v| *v as f32)
                .filter(|v| v.is_finite())
                .unwrap_or(fallback)
        };
        fixed.warp_anim_speed = read("fWarpAnimSpeed", 1.0);
        fixed.warp_scale = read("fWarpScale", 1.0);
        fixed.shader = read("fShader", 0.0);
        fixed.wave_scale = read("fWaveScale", 1.0);
        fixed.wave_smoothing = read("fWaveSmoothing", 0.75);
        fixed.mod_wave_alpha_by_volume = read("bModWaveAlphaByVolume", 0.0) >= 0.5;
        fixed.mod_wave_alpha_start = read("fModWaveAlphaStart", 0.75);
        fixed.mod_wave_alpha_end = read("fModWaveAlphaEnd", 0.95);
        fixed
    }
}

/// What the host tells a preset about the moment it is drawing.
///
/// Every figure here is either the shell's own clock or a summary of the
/// analysis already in the frame -- nothing in this module measures anything
/// (Phase 3 decision 2).
#[derive(Clone, Copy, Debug, Default)]
pub struct Inputs {
    /// Seconds since this preset was selected. A preset's own clock, not the
    /// track's: MilkDrop restarts it per preset and thousands of presets are
    /// written expecting a small number here.
    pub time: f32,
    pub fps: f32,
    pub frame: u32,
    /// How far through this preset's display time, 0..1. Meaningless without a
    /// preset rotation, which is M7c -- 0.0 until then, which is what MilkDrop
    /// itself reports at the start of a preset.
    pub progress: f32,
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
    pub bass_att: f32,
    pub mid_att: f32,
    pub treb_att: f32,
    pub mesh_x: u32,
    pub mesh_y: u32,
    pub pixels_x: u32,
    pub pixels_y: u32,
    /// MilkDrop publishes the **inverse** aspects under these names. Named as
    /// it does rather than corrected, because presets are written against what
    /// they are given.
    pub aspect_x: f32,
    pub aspect_y: f32,
}

/// The ten quantities that decide where a mesh vertex reads from.
///
/// Set per frame by `per_frame` and then, for a preset that has one,
/// overwritten per vertex by `per_pixel`.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    pub zoom: f32,
    pub zoomexp: f32,
    pub rot: f32,
    pub warp: f32,
    pub cx: f32,
    pub cy: f32,
    pub dx: f32,
    pub dy: f32,
    pub sx: f32,
    pub sy: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            zoomexp: 1.0,
            rot: 0.0,
            warp: 1.0,
            cx: 0.5,
            cy: 0.5,
            dx: 0.0,
            dy: 0.0,
            sx: 1.0,
            sy: 1.0,
        }
    }
}

/// The waveform's state for one frame.
#[derive(Clone, Copy, Debug)]
pub struct WaveState {
    pub mode: i32,
    pub alpha: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub x: f32,
    pub y: f32,
    pub mystery: f32,
    pub dots: bool,
    pub thick: bool,
    pub additive: bool,
    pub brighten: bool,
}

/// One border ring.
#[derive(Clone, Copy, Debug, Default)]
pub struct Border {
    pub size: f32,
    pub colour: [f32; 4],
}

/// Everything the renderer needs after `per_frame` has run.
#[derive(Clone, Copy, Debug)]
pub struct FrameState {
    pub motion: Motion,
    pub decay: f32,
    pub gamma: f32,
    pub echo_zoom: f32,
    pub echo_alpha: f32,
    pub echo_orientation: i32,
    pub wrap: bool,
    pub darken_center: bool,
    pub brighten: bool,
    pub darken: bool,
    pub solarize: bool,
    pub invert: bool,
    pub wave: WaveState,
    pub outer_border: Border,
    pub inner_border: Border,
    /// The range each blur level is normalised into, which the chain writes
    /// with and a preset's `GetBlur1..3` reads back through.
    pub blur_min: [f32; 3],
    pub blur_max: [f32; 3],
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            motion: Motion::default(),
            decay: 0.98,
            gamma: 2.0,
            echo_zoom: 2.0,
            echo_alpha: 0.0,
            echo_orientation: 0,
            wrap: true,
            darken_center: false,
            brighten: false,
            darken: false,
            solarize: false,
            invert: false,
            wave: WaveState {
                mode: 0,
                alpha: 0.8,
                r: 1.0,
                g: 1.0,
                b: 1.0,
                x: 0.5,
                y: 0.5,
                mystery: 0.0,
                dots: false,
                thick: false,
                additive: false,
                brighten: true,
            },
            blur_min: [0.0; 3],
            blur_max: [1.0; 3],
            outer_border: Border {
                size: 0.01,
                colour: [0.0, 0.0, 0.0, 0.0],
            },
            inner_border: Border {
                size: 0.01,
                colour: [0.25, 0.25, 0.25, 0.0],
            },
        }
    }
}

/// Where the equations' inputs and outputs live, resolved once at load.
///
/// Slot indices rather than names: `per_pixel` runs once per mesh vertex, and
/// a preset at MilkDrop's high mesh has 3185 of them, so a string lookup per
/// variable per vertex would be the most expensive thing in the frame by a
/// wide margin.
struct Slots {
    /// One per [`SCALARS`] entry, in the same order.
    scalars: Vec<usize>,
    q: [usize; Q_COUNT],
    monitor: usize,
    time: usize,
    fps: usize,
    frame: usize,
    progress: usize,
    bass: usize,
    mid: usize,
    treb: usize,
    bass_att: usize,
    mid_att: usize,
    treb_att: usize,
    meshx: usize,
    meshy: usize,
    pixelsx: usize,
    pixelsy: usize,
    aspectx: usize,
    aspecty: usize,
    /// `per_pixel`'s own inputs, which no other block is given.
    x: usize,
    y: usize,
    rad: usize,
    ang: usize,
}

/// Why a preset stopped being run.
///
/// A stranger's program is sandboxed by budget rather than by trust (decision
/// 51), and when the sandbox closes on one the app has to be able to *say so* --
/// a preset that silently draws nothing is a preset nobody can debug, and a
/// collection of a thousand will contain some.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A block would not compile. Named by block, since a preset with a broken
    /// `per_pixel` and a working `per_frame` still draws something.
    Compile { block: &'static str, why: String },
    /// A block ran past its evaluation budget.
    Budget { block: &'static str },
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Compile { block, why } => write!(f, "{block} did not compile: {why}"),
            Fault::Budget { block } => write!(f, "{block} ran past its evaluation budget"),
        }
    }
}

/// A preset's compiled equations and the state they run over.
pub struct Runtime {
    symbols: Symbols,
    ctx: Context,
    per_frame: Option<Program>,
    per_pixel: Option<Program>,
    slots: Slots,
    /// The value each [`SCALARS`] entry is reset to at the top of a frame:
    /// what the file said, or MilkDrop's default where it said nothing.
    scalar_defaults: Vec<f64>,
    /// `q1..q32` and `monitor` as the init block left them. MilkDrop carries
    /// these forward into every frame rather than letting a frame's writes
    /// accumulate, which is what makes `q` a *channel from init to frame*
    /// rather than a running total.
    q_after_init: [f64; Q_COUNT],
    monitor_after_init: f64,
    pub fixed: Fixed,
    faults: Vec<Fault>,
    /// Set when `per_pixel` exhausts its share of a frame's budget, so the
    /// remaining vertices are skipped rather than each paying the same price.
    pixel_budget_spent: bool,
}

impl Runtime {
    /// Compiles a preset's three equation blocks and runs its init.
    ///
    /// Never fails: a block that will not compile is recorded as a
    /// [`Fault`] and simply does not run, because a collection of a thousand
    /// presets will contain some that are wrong and the answer to those is a
    /// note beside the preset, not a refusal to open the folder (decision 50).
    pub fn new(preset: &crate::milk::Preset) -> Self {
        let mut symbols = Symbols::new();
        let mut faults = Vec::new();

        // Intern every known name before compiling anything, so the slot
        // layout does not depend on which names a particular preset happens to
        // mention first.
        let scalars: Vec<usize> = SCALARS.iter().map(|d| symbols.slot(d.var)).collect();
        let mut q = [0usize; Q_COUNT];
        for (i, slot) in q.iter_mut().enumerate() {
            *slot = symbols.slot(&format!("q{}", i + 1));
        }
        let slots = Slots {
            scalars,
            q,
            monitor: symbols.slot("monitor"),
            time: symbols.slot("time"),
            fps: symbols.slot("fps"),
            frame: symbols.slot("frame"),
            progress: symbols.slot("progress"),
            bass: symbols.slot("bass"),
            mid: symbols.slot("mid"),
            treb: symbols.slot("treb"),
            bass_att: symbols.slot("bass_att"),
            mid_att: symbols.slot("mid_att"),
            treb_att: symbols.slot("treb_att"),
            meshx: symbols.slot("meshx"),
            meshy: symbols.slot("meshy"),
            pixelsx: symbols.slot("pixelsx"),
            pixelsy: symbols.slot("pixelsy"),
            aspectx: symbols.slot("aspectx"),
            aspecty: symbols.slot("aspecty"),
            x: symbols.slot("x"),
            y: symbols.slot("y"),
            rad: symbols.slot("rad"),
            ang: symbols.slot("ang"),
        };

        let mut compile = |source: &str, block: &'static str| -> Option<Program> {
            if source.trim().is_empty() {
                return None;
            }
            match eel::compile(source, &mut symbols) {
                Ok(program) => Some(program),
                Err(why) => {
                    faults.push(Fault::Compile {
                        block,
                        why: why.to_string(),
                    });
                    None
                }
            }
        };
        let init = compile(&preset.init, "per_frame_init");
        let per_frame = compile(&preset.per_frame, "per_frame");
        let per_pixel = compile(&preset.per_pixel, "per_pixel");

        let base = numeric_base(&preset.base);
        let scalar_defaults = SCALARS
            .iter()
            .map(|d| {
                base.get(&d.key.to_ascii_lowercase())
                    .copied()
                    .unwrap_or(d.default)
            })
            .collect::<Vec<_>>();

        let mut ctx = Context::new(&symbols);
        ctx.sync(&symbols);

        let mut runtime = Self {
            symbols,
            ctx,
            per_frame,
            per_pixel,
            slots,
            scalar_defaults,
            q_after_init: [0.0; Q_COUNT],
            monitor_after_init: 0.0,
            fixed: Fixed::from_base(&base),
            faults,
            pixel_budget_spent: false,
        };

        // The init block sees the file's scalars and nothing else -- there is
        // no frame yet, so time, the audio and the mesh are all zero, which is
        // what MilkDrop gives it too.
        if let Some(init) = init {
            runtime.load_scalar_defaults();
            if runtime.ctx.run(&init).is_err() {
                runtime.faults.push(Fault::Budget {
                    block: "per_frame_init",
                });
            }
            for i in 0..Q_COUNT {
                runtime.q_after_init[i] = runtime.ctx.get(runtime.slots.q[i]);
            }
            runtime.monitor_after_init = runtime.ctx.get(runtime.slots.monitor);
        }

        runtime
    }

    /// What went wrong, in the order it was found. Empty for a preset that
    /// compiled and has stayed inside its budget.
    pub fn faults(&self) -> &[Fault] {
        &self.faults
    }

    fn load_scalar_defaults(&mut self) {
        for (slot, value) in self.slots.scalars.iter().zip(&self.scalar_defaults) {
            self.ctx.set(*slot, *value);
        }
    }

    /// Runs `per_frame` and reads the result.
    ///
    /// The order is MilkDrop's and matters: the scalars are reset from the
    /// file, then the read-only inputs are written, then `q` is restored from
    /// the init block, and only then does the preset's code run. A preset whose
    /// `per_frame` reads `q1` is reading what its *init* left there, not what
    /// its own previous frame did.
    pub fn run_frame(&mut self, inputs: &Inputs) -> FrameState {
        self.load_scalar_defaults();

        let set = |ctx: &mut Context, slot: usize, v: f64| ctx.set(slot, v);
        set(&mut self.ctx, self.slots.time, inputs.time as f64);
        set(&mut self.ctx, self.slots.fps, inputs.fps as f64);
        set(&mut self.ctx, self.slots.frame, inputs.frame as f64);
        set(&mut self.ctx, self.slots.progress, inputs.progress as f64);
        set(&mut self.ctx, self.slots.bass, inputs.bass as f64);
        set(&mut self.ctx, self.slots.mid, inputs.mid as f64);
        set(&mut self.ctx, self.slots.treb, inputs.treb as f64);
        set(&mut self.ctx, self.slots.bass_att, inputs.bass_att as f64);
        set(&mut self.ctx, self.slots.mid_att, inputs.mid_att as f64);
        set(&mut self.ctx, self.slots.treb_att, inputs.treb_att as f64);
        set(&mut self.ctx, self.slots.meshx, inputs.mesh_x as f64);
        set(&mut self.ctx, self.slots.meshy, inputs.mesh_y as f64);
        set(&mut self.ctx, self.slots.pixelsx, inputs.pixels_x as f64);
        set(&mut self.ctx, self.slots.pixelsy, inputs.pixels_y as f64);
        set(&mut self.ctx, self.slots.aspectx, inputs.aspect_x as f64);
        set(&mut self.ctx, self.slots.aspecty, inputs.aspect_y as f64);
        for i in 0..Q_COUNT {
            let (slot, value) = (self.slots.q[i], self.q_after_init[i]);
            self.ctx.set(slot, value);
        }
        let monitor = self.monitor_after_init;
        self.ctx.set(self.slots.monitor, monitor);

        if let Some(program) = self.per_frame.take() {
            if self.ctx.run(&program).is_err() {
                self.note_budget("per_frame");
            }
            self.per_frame = Some(program);
        }

        self.pixel_budget_spent = false;
        self.read_frame_state()
    }

    /// Runs `per_pixel` for one mesh vertex and reads the motion back.
    ///
    /// `base` is the frame's own motion, which every vertex starts from -- a
    /// preset's `per_pixel` that writes only `zoom` leaves the other nine at
    /// the per-frame values rather than at whatever the previous vertex made
    /// them, and getting that wrong makes the mesh accumulate down the grid.
    ///
    /// A preset with no `per_pixel` gets `base` back untouched, which is the
    /// common case and costs nothing.
    pub fn run_pixel(&mut self, base: &Motion, x: f32, y: f32, rad: f32, ang: f32) -> Motion {
        let Some(program) = self.per_pixel.take() else {
            return *base;
        };
        if self.pixel_budget_spent {
            self.per_pixel = Some(program);
            return *base;
        }

        self.write_motion(base);
        self.ctx.set(self.slots.x, x as f64);
        self.ctx.set(self.slots.y, y as f64);
        self.ctx.set(self.slots.rad, rad as f64);
        self.ctx.set(self.slots.ang, ang as f64);

        let ok = self.ctx.run(&program).is_ok();
        self.per_pixel = Some(program);
        if !ok {
            // One vertex's overrun ends the block for the whole frame: the
            // alternative is every one of a few thousand vertices paying the
            // full budget, which is a frame that takes seconds rather than a
            // preset that misbehaves.
            self.pixel_budget_spent = true;
            self.note_budget("per_pixel");
            return *base;
        }
        self.read_motion()
    }

    /// True once `per_pixel` has given up for this frame.
    pub fn pixel_budget_spent(&self) -> bool {
        self.pixel_budget_spent
    }

    fn note_budget(&mut self, block: &'static str) {
        let fault = Fault::Budget { block };
        if !self.faults.contains(&fault) {
            self.faults.push(fault);
        }
    }

    fn scalar(&self, index: usize) -> f64 {
        self.ctx.get(self.slots.scalars[index])
    }

    fn write_motion(&mut self, motion: &Motion) {
        let values = [
            motion.zoom,
            motion.zoomexp,
            motion.rot,
            motion.warp,
            motion.cx,
            motion.cy,
            motion.dx,
            motion.dy,
            motion.sx,
            motion.sy,
        ];
        debug_assert_eq!(values.len(), MOTION_COUNT);
        for (index, value) in values.iter().enumerate() {
            let slot = self.slots.scalars[index];
            self.ctx.set(slot, *value as f64);
        }
    }

    fn read_motion(&self) -> Motion {
        // A block that assigns a variable to something non-finite -- and
        // `log(0)` alone will do it -- must not reach the mesh, where one NaN
        // vertex is a triangle that vanishes.
        let f = |index: usize, fallback: f32| -> f32 {
            let v = self.scalar(index) as f32;
            if v.is_finite() {
                v
            } else {
                fallback
            }
        };
        Motion {
            zoom: f(0, 1.0),
            zoomexp: f(1, 1.0),
            rot: f(2, 0.0),
            warp: f(3, 1.0),
            cx: f(4, 0.5),
            cy: f(5, 0.5),
            dx: f(6, 0.0),
            dy: f(7, 0.0),
            sx: f(8, 1.0),
            sy: f(9, 1.0),
        }
    }

    /// `q1..q32` as they stand after this frame's `per_frame` block.
    ///
    /// The widest channel between a preset's equations and its shaders: the
    /// same numbers `env.rs` maps onto `_qa.._qh`.
    pub fn q(&self) -> [f32; 32] {
        let mut out = [0.0; 32];
        for (i, slot) in self.slots.q.iter().enumerate().take(32) {
            let v = self.ctx.get(*slot) as f32;
            out[i] = if v.is_finite() { v } else { 0.0 };
        }
        out
    }

    fn read_frame_state(&self) -> FrameState {
        let d = FrameState::default();
        // Index arithmetic against SCALARS' order, checked by the test below
        // rather than trusted: the table is the definition and this is a view
        // of it.
        let at = |name: &str| -> usize {
            SCALARS
                .iter()
                .position(|s| s.var == name)
                .expect("SCALARS is missing a variable this reader needs")
        };
        let num = |name: &str, fallback: f32| -> f32 {
            let v = self.scalar(at(name)) as f32;
            if v.is_finite() {
                v
            } else {
                fallback
            }
        };
        // MilkDrop's own threshold for a flag: `if (*var_pf_invert)` in C is
        // "not exactly zero", but every one of these is written by preset code
        // that may leave 0.9999 there, so half-way is the reading that matches
        // what a person sees.
        let flag = |name: &str, fallback: bool| -> bool {
            let v = self.scalar(at(name));
            if v.is_finite() {
                v >= 0.5
            } else {
                fallback
            }
        };

        FrameState {
            motion: self.read_motion(),
            decay: num("decay", d.decay),
            gamma: num("gamma", d.gamma),
            echo_zoom: num("echo_zoom", d.echo_zoom),
            echo_alpha: num("echo_alpha", d.echo_alpha),
            // MilkDrop takes this one modulo 4 rather than clamping, because a
            // preset animating it expects it to wrap.
            echo_orientation: (num("echo_orient", 0.0) as i32).rem_euclid(4),
            wrap: flag("wrap", d.wrap),
            darken_center: flag("darken_center", d.darken_center),
            brighten: flag("brighten", d.brighten),
            darken: flag("darken", d.darken),
            solarize: flag("solarize", d.solarize),
            invert: flag("invert", d.invert),
            blur_min: [
                num("blur1min", d.blur_min[0]),
                num("blur2min", d.blur_min[1]),
                num("blur3min", d.blur_min[2]),
            ],
            blur_max: [
                num("blur1max", d.blur_max[0]),
                num("blur2max", d.blur_max[1]),
                num("blur3max", d.blur_max[2]),
            ],
            wave: WaveState {
                mode: (num("wave_mode", 0.0) as i32).rem_euclid(NUM_WAVES),
                alpha: num("wave_a", d.wave.alpha),
                r: num("wave_r", 1.0),
                g: num("wave_g", 1.0),
                b: num("wave_b", 1.0),
                x: num("wave_x", 0.5),
                y: num("wave_y", 0.5),
                mystery: num("wave_mystery", 0.0),
                dots: flag("wave_usedots", false),
                thick: flag("wave_thick", false),
                additive: flag("wave_additive", false),
                brighten: flag("wave_brighten", true),
            },
            outer_border: Border {
                size: num("ob_size", 0.01),
                colour: [
                    num("ob_r", 0.0),
                    num("ob_g", 0.0),
                    num("ob_b", 0.0),
                    num("ob_a", 0.0),
                ],
            },
            inner_border: Border {
                size: num("ib_size", 0.01),
                colour: [
                    num("ib_r", 0.25),
                    num("ib_g", 0.25),
                    num("ib_b", 0.25),
                    num("ib_a", 0.0),
                ],
            },
        }
    }

    /// The symbol table, for a caller that wants to read a variable this
    /// module does not surface -- the smoke checks do.
    pub fn symbols(&self) -> &Symbols {
        &self.symbols
    }

    /// The evaluation context, on the same terms.
    pub fn context(&self) -> &Context {
        &self.ctx
    }
}

/// How many waveform modes MilkDrop cycles through.
///
/// Eight, not nine: mode 8 exists in the source and is marked unfinished
/// there, and `wave_mode` is taken modulo this number, so a preset that writes
/// 8 gets mode 0 exactly as MilkDrop gives it.
pub const NUM_WAVES: i32 = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milk::Preset;

    fn preset(text: &str) -> Preset {
        Preset::parse(text).expect("test preset should parse")
    }

    #[test]
    fn the_scalar_table_names_every_variable_the_reader_asks_for() {
        // `read_frame_state` looks entries up by variable name and panics if
        // one is missing; running it once over a default preset is what turns
        // that panic into a test failure rather than a crash on a user's
        // machine the first time they open a preset that sets nothing.
        let mut runtime = Runtime::new(&preset("[preset00]\n"));
        let state = runtime.run_frame(&Inputs::default());
        assert_eq!(state.decay, 0.98);
        assert_eq!(state.gamma, 2.0);
        assert!(state.wrap);
        assert!(state.wave.brighten);
    }

    #[test]
    fn the_motion_variables_are_the_first_ten_entries() {
        // `write_motion` and `read_motion` index the table directly, which is
        // only correct while this holds.
        let expected = [
            "zoom", "zoomexp", "rot", "warp", "cx", "cy", "dx", "dy", "sx", "sy",
        ];
        for (i, name) in expected.iter().enumerate() {
            assert_eq!(SCALARS[i].var, *name, "entry {i}");
        }
        assert_eq!(MOTION_COUNT, expected.len());
    }

    #[test]
    fn every_key_and_variable_in_the_table_is_unique() {
        // Two entries sharing a key means one of them silently never loads;
        // two sharing a variable means one silently overwrites the other.
        let mut keys: Vec<_> = SCALARS.iter().map(|s| s.key).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate .milk key in SCALARS");

        let mut vars: Vec<_> = SCALARS.iter().map(|s| s.var).collect();
        vars.sort_unstable();
        vars.dedup();
        assert_eq!(vars.len(), count, "duplicate variable name in SCALARS");
    }

    #[test]
    fn a_file_scalar_beats_the_default_and_per_frame_beats_both() {
        let mut runtime = Runtime::new(&preset("[preset00]\nfDecay=0.5\n"));
        assert_eq!(runtime.run_frame(&Inputs::default()).decay, 0.5);

        let mut runtime =
            Runtime::new(&preset("[preset00]\nfDecay=0.5\nper_frame_1=decay=0.25;\n"));
        assert_eq!(runtime.run_frame(&Inputs::default()).decay, 0.25);
    }

    #[test]
    fn a_frames_writes_do_not_carry_into_the_next_frame() {
        // MilkDrop resets the scalars every frame, so `decay = decay*0.9`
        // reaches the same value each time rather than decaying to nothing.
        let mut runtime = Runtime::new(&preset(
            "[preset00]\nfDecay=1.0\nper_frame_1=decay = decay*0.5;\n",
        ));
        let first = runtime.run_frame(&Inputs::default()).decay;
        let second = runtime.run_frame(&Inputs::default()).decay;
        assert_eq!(first, 0.5);
        assert_eq!(second, 0.5);
    }

    #[test]
    fn q_carries_from_init_into_every_frame_rather_than_accumulating() {
        let mut runtime = Runtime::new(&preset(
            "[preset00]\nper_frame_init_1=q1 = 7;\nper_frame_2=q1 = q1 + 1; zoom = q1;\n",
        ));
        let first = runtime.run_frame(&Inputs::default()).motion.zoom;
        let second = runtime.run_frame(&Inputs::default()).motion.zoom;
        assert_eq!(first, 8.0);
        assert_eq!(second, 8.0, "q1 must be restored from init, not carried");
    }

    #[test]
    fn per_pixel_starts_each_vertex_from_the_frames_own_motion() {
        // Only `zoom` is written, so the other nine must still read the
        // per-frame values at every vertex.
        let mut runtime = Runtime::new(&preset(
            "[preset00]\nper_frame_1=sx = 2;\nper_pixel_1=zoom = 1 + rad;\n",
        ));
        let frame = runtime.run_frame(&Inputs::default());
        assert_eq!(frame.motion.sx, 2.0);
        let a = runtime.run_pixel(&frame.motion, 0.0, 0.0, 0.25, 0.0);
        let b = runtime.run_pixel(&frame.motion, 1.0, 1.0, 0.75, 0.0);
        assert_eq!(a.zoom, 1.25);
        assert_eq!(b.zoom, 1.75);
        assert_eq!(a.sx, 2.0);
        assert_eq!(b.sx, 2.0);
    }

    #[test]
    fn a_preset_with_no_per_pixel_costs_nothing_and_stands_still() {
        let mut runtime = Runtime::new(&preset("[preset00]\n"));
        let frame = runtime.run_frame(&Inputs::default());
        let motion = runtime.run_pixel(&frame.motion, 0.3, 0.7, 0.5, 1.0);
        assert_eq!(motion.zoom, 1.0);
        assert_eq!(motion.cx, 0.5);
    }

    #[test]
    fn a_block_that_will_not_compile_is_a_note_rather_than_a_refusal() {
        let runtime = Runtime::new(&preset("[preset00]\nper_frame_1=zoom = ((;\n"));
        assert!(matches!(
            runtime.faults().first(),
            Some(Fault::Compile {
                block: "per_frame",
                ..
            })
        ));
    }

    #[test]
    fn a_runaway_per_pixel_gives_up_for_the_frame_rather_than_per_vertex() {
        // A stranger's program is bounded by budget (decision 51). The cost
        // that matters is not one vertex's -- it is the whole frame's, so the
        // second vertex must not pay again.
        let mut runtime = Runtime::new(&preset(
            "[preset00]\nper_pixel_1=loop(1000000, zoom = zoom + 1;);\n",
        ));
        runtime.ctx.budget = 10_000;
        let frame = runtime.run_frame(&Inputs::default());
        let first = runtime.run_pixel(&frame.motion, 0.0, 0.0, 0.0, 0.0);
        assert!(runtime.pixel_budget_spent());
        assert_eq!(
            first.zoom, 1.0,
            "an overrun falls back to the frame's motion"
        );

        let before = runtime.context().last_cost();
        let second = runtime.run_pixel(&frame.motion, 1.0, 1.0, 1.0, 0.0);
        assert_eq!(second.zoom, 1.0);
        assert_eq!(
            runtime.context().last_cost(),
            before,
            "the second vertex must not have run at all"
        );
        assert!(runtime
            .faults()
            .contains(&Fault::Budget { block: "per_pixel" }));
    }

    #[test]
    fn a_non_finite_result_falls_back_instead_of_reaching_the_mesh() {
        let mut runtime = Runtime::new(&preset("[preset00]\nper_frame_1=zoom = log(0);\n"));
        let frame = runtime.run_frame(&Inputs::default());
        assert_eq!(frame.motion.zoom, 1.0);
    }

    #[test]
    fn the_scalars_a_preset_cannot_reach_are_read_from_the_file_only() {
        let runtime = Runtime::new(&preset(
            "[preset00]\nfWarpScale=2.5\nfShader=0.8\nper_frame_1=fWarpScale = 9;\n",
        ));
        assert_eq!(runtime.fixed.warp_scale, 2.5);
        assert_eq!(runtime.fixed.shader, 0.8);
    }

    #[test]
    fn the_wave_mode_wraps_the_way_milkdrop_wraps_it() {
        let mut runtime = Runtime::new(&preset("[preset00]\nper_frame_1=wave_mode = 8;\n"));
        assert_eq!(runtime.run_frame(&Inputs::default()).wave.mode, 0);
        let mut runtime = Runtime::new(&preset("[preset00]\nper_frame_1=wave_mode = -1;\n"));
        assert_eq!(runtime.run_frame(&Inputs::default()).wave.mode, 7);
    }
}
