//! Custom waves and custom shapes (Phase 3 M7c).
//!
//! The built-in waveform (`wave.rs`) draws one of eight fixed modes. A preset
//! that wants its *own* figure declares up to four custom waves and four custom
//! shapes, each with EEL code of its own: a wave runs a `per_point` block once
//! per sample and the block writes each point's `x`/`y` and colour; a shape runs
//! a `per_frame` block once per *instance* and the block writes the polygon's
//! centre, radius, angle and colours.
//!
//! Each wave and each shape has its **own variable pool** (its own `Symbols`
//! and `Context`), because a user variable declared in one pool is not visible
//! in another (MilkDrop's three-pool rule). The bridges are the same two the
//! preset itself uses: `q1..q32` flow in from the preset's per-frame result,
//! and `t1..t8` flow from that wave/shape's own init, through its per-frame, to
//! its per-point (or per-instance). A wave's per-point block runs on *one*
//! context, so `x`, `y`, the colour and `q` all carry from point to point --
//! which is what a preset smoothing its own wave relies on.
//!
//! # The coordinate convention
//!
//! `x`/`y` are MilkDrop's screen space: `0..1`, `y` with 0 at the bottom and 1
//! at the top. A point is a *position*, not an amplitude, so unlike the built-in
//! wave no aspect is applied to it (the preset author uses `aspectx`/`aspecty`
//! if they want aspect-correct shapes) -- it maps straight to clip space as
//! `x*2-1`, `y*2-1`. A shape's *radius* is an amplitude, so it does take the
//! aspect pair, the same way the built-in wave's amplitudes do.
//!
//! The one thing a readback cannot settle and a live look must: whether the
//! vertical orientation matches MilkDrop's. The convention here is the
//! authoring guide's own ("y: 0=bottom, 1=top", read as `y*2-1` into y-up clip
//! space). If a preset shows up mirrored, the flip is one line at the point of
//! emission, not a change of model.

use std::collections::BTreeMap;

use crate::milk::eel::{self, Context, Program, Symbols};
use crate::milk::state::Inputs;
use crate::milk::warp::Aspect;
use crate::milk::wave::Wave;
use crate::milk::{CustomShape, CustomWave};

/// How many points a custom wave may draw.
pub const MAX_WAVE_SAMPLES: usize = 512;

/// How many sides a custom shape may have.
pub const MAX_SHAPE_SIDES: usize = 100;

const Q_COUNT: usize = 32;
const T_COUNT: usize = 8;

/// The read-only inputs every wave/shape pool sees, plus the two bridges.
struct Common {
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
    q: [usize; Q_COUNT],
    t: [usize; T_COUNT],
}

fn intern_common(s: &mut Symbols) -> Common {
    Common {
        time: s.slot("time"),
        fps: s.slot("fps"),
        frame: s.slot("frame"),
        progress: s.slot("progress"),
        bass: s.slot("bass"),
        mid: s.slot("mid"),
        treb: s.slot("treb"),
        bass_att: s.slot("bass_att"),
        mid_att: s.slot("mid_att"),
        treb_att: s.slot("treb_att"),
        q: std::array::from_fn(|i| s.slot(&format!("q{}", i + 1))),
        t: std::array::from_fn(|i| s.slot(&format!("t{}", i + 1))),
    }
}

/// Compiles a block, or records why it did not.
fn compile(symbols: &mut Symbols, source: &str, faults: &mut Vec<String>) -> Option<Program> {
    if source.trim().is_empty() {
        return None;
    }
    match eel::compile(source, symbols) {
        Ok(program) => Some(program),
        Err(why) => {
            faults.push(why.to_string());
            None
        }
    }
}

/// Writes the shared inputs into a pool's context.
fn write_common(ctx: &mut Context, common: &Common, inputs: &Inputs, q_seed: &[f64; Q_COUNT]) {
    let set = |ctx: &mut Context, slot: usize, v: f64| ctx.set(slot, v);
    set(ctx, common.time, inputs.time as f64);
    set(ctx, common.fps, inputs.fps as f64);
    set(ctx, common.frame, inputs.frame as f64);
    set(ctx, common.progress, inputs.progress as f64);
    set(ctx, common.bass, inputs.bass as f64);
    set(ctx, common.mid, inputs.mid as f64);
    set(ctx, common.treb, inputs.treb as f64);
    set(ctx, common.bass_att, inputs.bass_att as f64);
    set(ctx, common.mid_att, inputs.mid_att as f64);
    set(ctx, common.treb_att, inputs.treb_att as f64);
    for (slot, value) in common.q.iter().zip(q_seed) {
        set(ctx, *slot, *value);
    }
}

fn read_flag(base: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
    base.get(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| v >= 0.5)
        .unwrap_or(default)
}

fn read_num(base: &BTreeMap<String, String>, key: &str, default: f64) -> f64 {
    base.get(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// What a custom wave reads its samples from, once per frame.
pub struct WaveSource<'a> {
    /// Both channels of the newest audio, -1..1, display gain applied, and
    /// *not* scaled or smoothed -- a custom wave has its own `scaling` and
    /// `smoothing` and must not inherit the built-in wave's.
    pub left: &'a [f32],
    pub right: &'a [f32],
    /// The frame's spectrum, dBFS per band, for `bSpectrum` waves.
    pub spectrum: &'a [f32],
    /// The preset's `fWaveScale`, which MilkDrop applies to custom waves too.
    pub wave_scale: f32,
    pub aspect: Aspect,
}

/// How MilkDrop scales a waveform sample before a custom wave sees it:
/// `0.004` times its byte-ranged sample (+-128), i.e. 0.512 per unit of this
/// build's -1..1 audio.
const WAVEFORM_MULT: f64 = 0.004 * 128.0;

/// One custom wave, compiled and runnable.
pub struct WaveRig {
    _symbols: Symbols,
    ctx: Context,
    per_frame: Option<Program>,
    per_point: Option<Program>,
    common: Common,
    r: usize,
    g: usize,
    b: usize,
    a: usize,
    samples: usize,
    x: usize,
    y: usize,
    sample: usize,
    value1: usize,
    value2: usize,
    enabled: bool,
    spectrum: bool,
    scaling: f64,
    smoothing: f64,
    sep: i64,
    dots: bool,
    thick: bool,
    additive: bool,
    file_samples: usize,
    file_colour: [f64; 4],
    t_after_init: [f64; T_COUNT],
    /// What did not compile, for the preset report.
    pub faults: Vec<String>,
}

impl WaveRig {
    pub fn new(custom: &CustomWave) -> Self {
        let mut symbols = Symbols::new();
        let mut faults = Vec::new();
        let common = intern_common(&mut symbols);
        let (r, g, b, a) = (
            symbols.slot("r"),
            symbols.slot("g"),
            symbols.slot("b"),
            symbols.slot("a"),
        );
        let (samples, x, y, sample, value1, value2) = (
            symbols.slot("samples"),
            symbols.slot("x"),
            symbols.slot("y"),
            symbols.slot("sample"),
            symbols.slot("value1"),
            symbols.slot("value2"),
        );

        let init = compile(&mut symbols, &custom.init, &mut faults);
        let per_frame = compile(&mut symbols, &custom.per_frame, &mut faults);
        let per_point = compile(&mut symbols, &custom.per_point, &mut faults);

        let mut ctx = Context::new(&symbols);
        ctx.reseed(1000 + u64::from(custom.index));
        ctx.sync(&symbols);

        let mut t_after_init = [0.0f64; T_COUNT];
        if let Some(init) = init {
            for (slot, value) in common.t.iter().zip(t_after_init) {
                ctx.set(*slot, value);
            }
            if ctx.run(&init).is_err() {
                faults.push("init ran past its budget".into());
            }
            for (slot, out) in common.t.iter().zip(t_after_init.iter_mut()) {
                *out = ctx.get(*slot);
            }
        }

        let file_samples = read_num(&custom.base, "samples", 512.0) as usize;
        let file_colour = [
            read_num(&custom.base, "r", 1.0),
            read_num(&custom.base, "g", 1.0),
            read_num(&custom.base, "b", 1.0),
            read_num(&custom.base, "a", 1.0),
        ];
        Self {
            _symbols: symbols,
            ctx,
            per_frame,
            per_point,
            common,
            r,
            g,
            b,
            a,
            samples,
            x,
            y,
            sample,
            value1,
            value2,
            enabled: read_flag(&custom.base, "enabled", false),
            spectrum: read_flag(&custom.base, "bSpectrum", false),
            scaling: read_num(&custom.base, "scaling", 1.0),
            smoothing: read_num(&custom.base, "smoothing", 0.5),
            sep: read_num(&custom.base, "sep", 0.0) as i64,
            dots: read_flag(&custom.base, "bUseDots", false),
            thick: read_flag(&custom.base, "bDrawThick", false),
            additive: read_flag(&custom.base, "bAdditive", false),
            file_samples,
            file_colour,
            t_after_init,
            faults,
        }
    }

    /// Runs this wave for one frame, returning the points to draw.
    ///
    /// `left`/`right` are the prepared waveform samples (already scaled and
    /// smoothed the way the built-in wave's are); `spectrum` is the frame's
    /// spectrum in dBFS, used when the wave's `bSpectrum` flag is set.
    #[allow(clippy::too_many_arguments)]
    /// Runs the wave's per-frame and per-point code and returns what to draw.
    ///
    /// MilkDrop 2's `DrawCustomWaves`, line for line where it matters:
    ///
    /// - the samples are the middle `samples` of the waveform (offset by
    ///   `sep` between the channels), smoothed forward and back by the wave's
    ///   own `smoothing`, then scaled by `0.004 * scaling * fWaveScale`;
    /// - **every point starts fresh**: `x`/`y` at `0.5 + value1/value2` and
    ///   `r`/`g`/`b`/`a` at what the per-frame code left, so a point that does
    ///   not write its colour does not inherit the previous point's;
    /// - **every point keeps its own colour.** Fading a line's ends to
    ///   transparent (`a = a*sin(sample*pi)`) is one of the corpus's commonest
    ///   idioms, and taking the last point's colour for the whole strip drew
    ///   those waves at alpha zero -- which is to say not at all;
    /// - positions are multiplied by the inverse aspect, so a circle drawn in
    ///   0..1 stays a circle on a window that is not square;
    /// - a line (not dots) gets MilkDrop's one-pass tessellation.
    pub fn build(
        &mut self,
        inputs: &Inputs,
        q_seed: &[f64; Q_COUNT],
        source: &WaveSource,
    ) -> Option<Wave> {
        let per_point = self.per_point.as_ref()?;
        if !self.enabled {
            return None;
        }

        write_common(&mut self.ctx, &self.common, inputs, q_seed);
        for (slot, value) in self.common.t.iter().zip(self.t_after_init) {
            self.ctx.set(*slot, value);
        }
        // r/g/b/a and `samples` are per-frame variables a preset's code may
        // overwrite, so they start at their file defaults each frame, like the
        // preset's own scalars do.
        self.ctx.set(self.samples, self.file_samples as f64);
        self.ctx.set(self.r, self.file_colour[0]);
        self.ctx.set(self.g, self.file_colour[1]);
        self.ctx.set(self.b, self.file_colour[2]);
        self.ctx.set(self.a, self.file_colour[3]);

        if let Some(per_frame) = self.per_frame.take() {
            if self.ctx.run(&per_frame).is_err() {
                self.faults.push("per_frame ran past its budget".into());
            }
            self.per_frame = Some(per_frame);
        }

        let samples =
            (self.ctx.get(self.samples).round() as i64).clamp(0, MAX_WAVE_SAMPLES as i64) as usize;
        if samples < 2 && !(self.dots && samples >= 1) {
            return None;
        }
        let frame_colour = [
            self.ctx.get(self.r),
            self.ctx.get(self.g),
            self.ctx.get(self.b),
            self.ctx.get(self.a),
        ];

        let (mut d1, mut d2) = (vec![0.0f64; samples], vec![0.0f64; samples]);
        let last = (samples.max(2) - 1) as f64;
        if self.spectrum {
            for (j, (v1, v2)) in d1.iter_mut().zip(d2.iter_mut()).enumerate() {
                let value = spectrum_value(source.spectrum, j as f64 / last);
                (*v1, *v2) = (value, value);
            }
        } else {
            let len = source.left.len().min(source.right.len()) as i64;
            let centre = (len - samples as i64) / 2;
            let read = |data: &[f32], start: i64, j: usize| -> f64 {
                let i = (start + j as i64).clamp(0, (len - 1).max(0)) as usize;
                data.get(i).copied().unwrap_or(0.0) as f64
            };
            for j in 0..samples {
                d1[j] = read(source.left, centre - self.sep / 2, j);
                d2[j] = read(source.right, centre + self.sep / 2, j);
            }
        }
        // Forward and back, so the smoothing has no direction.
        let mix1 = (self.smoothing * 0.98).max(0.0).sqrt();
        let mix2 = 1.0 - mix1;
        for d in [&mut d1, &mut d2] {
            for j in 1..samples {
                d[j] = d[j] * mix2 + d[j - 1] * mix1;
            }
            for j in (0..samples.saturating_sub(1)).rev() {
                d[j] = d[j] * mix2 + d[j + 1] * mix1;
            }
        }
        // MilkDrop scales the spectrum by 0.15 of its own FFT's magnitudes,
        // which have no counterpart in a dBFS band; the spectrum keeps this
        // build's 0..1 amplitude, scaled only by the wave's own `scaling`.
        let mult = if self.spectrum {
            self.scaling
        } else {
            WAVEFORM_MULT * self.scaling * source.wave_scale as f64
        };

        let (inv_x, inv_y) = (source.aspect.inv_x, source.aspect.inv_y);
        let mut points = Vec::with_capacity(samples);
        let mut colours = Vec::with_capacity(samples);
        for j in 0..samples {
            let (v1, v2) = (d1[j] * mult, d2[j] * mult);
            self.ctx.set(self.sample, j as f64 / last);
            self.ctx.set(self.value1, v1);
            self.ctx.set(self.value2, v2);
            self.ctx.set(self.x, 0.5 + v1);
            self.ctx.set(self.y, 0.5 + v2);
            self.ctx.set(self.r, frame_colour[0]);
            self.ctx.set(self.g, frame_colour[1]);
            self.ctx.set(self.b, frame_colour[2]);
            self.ctx.set(self.a, frame_colour[3]);

            if self.ctx.run(per_point).is_err() {
                self.faults.push("per_point ran past its budget".into());
                break;
            }

            let x = self.ctx.get(self.x) as f32;
            let y = self.ctx.get(self.y) as f32;
            points.push([(x * 2.0 - 1.0) * inv_x, (y * 2.0 - 1.0) * inv_y]);
            colours.push([
                self.ctx.get(self.r).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.g).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.b).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.a).clamp(0.0, 1.0) as f32,
            ]);
        }

        if points.is_empty() || colours.iter().all(|c| c[3] < 0.004) {
            return None;
        }
        if !self.dots && points.len() >= 2 {
            (points, colours) = crate::milk::wave::smooth_coloured(&points, &colours);
        }
        let colour = colours[colours.len() / 2];
        Some(Wave {
            points,
            colours: Some(colours),
            break_at: None,
            colour,
            additive: self.additive,
            dots: self.dots,
            thick: self.thick,
        })
    }
}

/// The spectrum level at a given progress through the samples, as an amplitude.
///
/// The frame's spectrum is a dBFS array over log-spaced bands; a custom wave
/// maps its point index linearly across those bands. Monophonic -- the build
/// has no left/right spectrum, so `value1` and `value2` read the same figure.
fn spectrum_value(spectrum: &[f32], sample: f64) -> f64 {
    if spectrum.is_empty() {
        return 0.0;
    }
    let idx = (sample * (spectrum.len() - 1) as f64).round() as usize;
    let db = spectrum.get(idx).copied().unwrap_or(spectrum[0]);
    10f64.powf((db as f64) / 20.0)
}

/// A divisor a preset may have set to zero: MilkDrop divides by `tex_zoom`
/// unguarded. Here zero is "no zoom", the default, rather than an infinity
/// that blanks the shape.
fn non_zero(v: f64) -> f64 {
    if v.abs() < 1e-6 || !v.is_finite() {
        1.0
    } else {
        v
    }
}

/// One custom shape, compiled and runnable.
pub struct ShapeRig {
    _symbols: Symbols,
    ctx: Context,
    per_frame: Option<Program>,
    common: Common,
    instance: usize,
    num_inst: usize,
    sides: usize,
    thick: usize,
    additive: usize,
    x: usize,
    y: usize,
    rad: usize,
    ang: usize,
    tex_ang: usize,
    tex_zoom: usize,
    textured: usize,
    r: usize,
    g: usize,
    b: usize,
    a: usize,
    r2: usize,
    g2: usize,
    b2: usize,
    a2: usize,
    border_r: usize,
    border_g: usize,
    border_b: usize,
    border_a: usize,
    enabled: bool,
    file_num_inst: i64,
    file_sides: i64,
    file_additive: bool,
    file_thick: bool,
    file_x: f64,
    file_y: f64,
    file_rad: f64,
    file_ang: f64,
    file_tex_ang: f64,
    file_tex_zoom: f64,
    file_textured: bool,
    file_inner: [f64; 4],
    file_outer: [f64; 4],
    file_border: [f64; 4],
    t_after_init: [f64; T_COUNT],
    pub faults: Vec<String>,
}

/// The geometry of one shape instance.
pub struct ShapeGeom {
    /// The centre, in clip space.
    pub centre: [f32; 2],
    /// The rim vertices, in order, in clip space.
    pub rim: Vec<[f32; 2]>,
    /// Colour at the centre.
    pub inner: [f32; 4],
    /// Colour at the rim.
    pub outer: [f32; 4],
    /// Border colour; an alpha under 0.004 means no border.
    pub border_colour: [f32; 4],
    pub additive: bool,
    /// Whether the fill samples the previous frame (`textured=1`), in which
    /// case `inner`/`outer` *modulate* that picture rather than being the
    /// fill. MilkDrop's way of drawing a shrunken, rotated copy of the screen
    /// inside a shape -- the whole of the "nested" and "mirror" families.
    pub textured: bool,
    /// Where the centre and each rim vertex sample the previous frame, in
    /// texture space. Meaningful only when `textured`.
    pub centre_uv: [f32; 2],
    pub rim_uv: Vec<[f32; 2]>,
}

impl ShapeRig {
    pub fn new(custom: &CustomShape) -> Self {
        let mut symbols = Symbols::new();
        let mut faults = Vec::new();
        let common = intern_common(&mut symbols);
        let (instance, num_inst) = (symbols.slot("instance"), symbols.slot("num_inst"));
        let (sides, thick, additive) = (
            symbols.slot("sides"),
            symbols.slot("thick"),
            symbols.slot("additive"),
        );
        let (x, y, rad, ang, textured) = (
            symbols.slot("x"),
            symbols.slot("y"),
            symbols.slot("rad"),
            symbols.slot("ang"),
            symbols.slot("textured"),
        );
        let (tex_ang, tex_zoom) = (symbols.slot("tex_ang"), symbols.slot("tex_zoom"));
        let (r, g, b, a) = (
            symbols.slot("r"),
            symbols.slot("g"),
            symbols.slot("b"),
            symbols.slot("a"),
        );
        let (r2, g2, b2, a2) = (
            symbols.slot("r2"),
            symbols.slot("g2"),
            symbols.slot("b2"),
            symbols.slot("a2"),
        );
        let (border_r, border_g, border_b, border_a) = (
            symbols.slot("border_r"),
            symbols.slot("border_g"),
            symbols.slot("border_b"),
            symbols.slot("border_a"),
        );

        let init = compile(&mut symbols, &custom.init, &mut faults);
        let per_frame = compile(&mut symbols, &custom.per_frame, &mut faults);

        let mut ctx = Context::new(&symbols);
        ctx.reseed(2000 + u64::from(custom.index));
        ctx.sync(&symbols);

        let mut t_after_init = [0.0f64; T_COUNT];
        if let Some(init) = init {
            for (slot, value) in common.t.iter().zip(t_after_init) {
                ctx.set(*slot, value);
            }
            if ctx.run(&init).is_err() {
                faults.push("init ran past its budget".into());
            }
            for (slot, out) in common.t.iter().zip(t_after_init.iter_mut()) {
                *out = ctx.get(*slot);
            }
        }

        Self {
            _symbols: symbols,
            ctx,
            per_frame,
            common,
            instance,
            num_inst,
            sides,
            thick,
            additive,
            x,
            y,
            rad,
            ang,
            tex_ang,
            tex_zoom,
            textured,
            r,
            g,
            b,
            a,
            r2,
            g2,
            b2,
            a2,
            border_r,
            border_g,
            border_b,
            border_a,
            enabled: read_flag(&custom.base, "enabled", false),
            file_num_inst: read_num(&custom.base, "num_inst", 1.0) as i64,
            file_sides: read_num(&custom.base, "sides", 4.0) as i64,
            file_additive: read_flag(&custom.base, "additive", false),
            file_thick: read_flag(&custom.base, "thickOutline", false),
            file_x: read_num(&custom.base, "x", 0.5),
            file_y: read_num(&custom.base, "y", 0.5),
            // MilkDrop 2's own defaults (`state.cpp`), for the keys a preset
            // leaves out: a small red-to-transparent-green shape, not a large
            // white one. A preset that relies on a default draws what its
            // author saw.
            file_rad: read_num(&custom.base, "rad", 0.1),
            file_ang: read_num(&custom.base, "ang", 0.0),
            file_tex_ang: read_num(&custom.base, "tex_ang", 0.0),
            file_tex_zoom: read_num(&custom.base, "tex_zoom", 1.0),
            file_textured: read_flag(&custom.base, "textured", false),
            file_inner: [
                read_num(&custom.base, "r", 1.0),
                read_num(&custom.base, "g", 0.0),
                read_num(&custom.base, "b", 0.0),
                read_num(&custom.base, "a", 1.0),
            ],
            file_outer: [
                read_num(&custom.base, "r2", 0.0),
                read_num(&custom.base, "g2", 1.0),
                read_num(&custom.base, "b2", 0.0),
                read_num(&custom.base, "a2", 0.0),
            ],
            file_border: [
                read_num(&custom.base, "border_r", 1.0),
                read_num(&custom.base, "border_g", 1.0),
                read_num(&custom.base, "border_b", 1.0),
                read_num(&custom.base, "border_a", 0.1),
            ],
            t_after_init,
            faults,
        }
    }

    /// Runs every instance of this shape and returns the geometry to draw.
    ///
    /// The arithmetic is MilkDrop 2's `DrawCustomShapes`, with this crate's
    /// one standing difference: y is negated throughout (MilkDrop writes
    /// `y*-2+1`, this crate `y*2-1`, for waves and shapes alike), so the rim's
    /// `sin` term and the texture's `v` term are negated with it.
    pub fn build(
        &mut self,
        inputs: &Inputs,
        q_seed: &[f64; Q_COUNT],
        aspect: Aspect,
    ) -> Vec<ShapeGeom> {
        if !self.enabled {
            return Vec::new();
        }

        let mut out = Vec::new();
        // num_inst is writable by the per-frame code, so it is re-read as a
        // variable each instance rather than trusted from the file.
        let num_inst = self.file_num_inst.max(1) as usize;

        for inst in 0..num_inst {
            write_common(&mut self.ctx, &self.common, inputs, q_seed);
            for (slot, value) in self.common.t.iter().zip(self.t_after_init) {
                self.ctx.set(*slot, value);
            }
            self.ctx.set(self.instance, inst as f64);
            self.ctx.set(self.num_inst, num_inst as f64);
            self.ctx.set(self.sides, self.file_sides as f64);
            self.ctx
                .set(self.additive, if self.file_additive { 1.0 } else { 0.0 });
            self.ctx
                .set(self.thick, if self.file_thick { 1.0 } else { 0.0 });
            self.ctx
                .set(self.textured, if self.file_textured { 1.0 } else { 0.0 });
            self.ctx.set(self.x, self.file_x);
            self.ctx.set(self.y, self.file_y);
            self.ctx.set(self.rad, self.file_rad);
            self.ctx.set(self.ang, self.file_ang);
            self.ctx.set(self.tex_ang, self.file_tex_ang);
            self.ctx.set(self.tex_zoom, self.file_tex_zoom);
            self.ctx.set(self.r, self.file_inner[0]);
            self.ctx.set(self.g, self.file_inner[1]);
            self.ctx.set(self.b, self.file_inner[2]);
            self.ctx.set(self.a, self.file_inner[3]);
            self.ctx.set(self.r2, self.file_outer[0]);
            self.ctx.set(self.g2, self.file_outer[1]);
            self.ctx.set(self.b2, self.file_outer[2]);
            self.ctx.set(self.a2, self.file_outer[3]);
            self.ctx.set(self.border_r, self.file_border[0]);
            self.ctx.set(self.border_g, self.file_border[1]);
            self.ctx.set(self.border_b, self.file_border[2]);
            self.ctx.set(self.border_a, self.file_border[3]);

            // A shape with no per-frame code draws once, at the file's
            // defaults; the block is optional, unlike a wave's per-point.
            if let Some(per_frame) = self.per_frame.as_ref() {
                if self.ctx.run(per_frame).is_err() {
                    self.faults.push("per_frame ran past its budget".into());
                    continue;
                }
            }

            let x = self.ctx.get(self.x);
            let y = self.ctx.get(self.y);
            let rad = self.ctx.get(self.rad);
            let ang = self.ctx.get(self.ang);
            let sides =
                (self.ctx.get(self.sides).round() as i64).clamp(3, MAX_SHAPE_SIDES as i64) as usize;
            let inner = [
                self.ctx.get(self.r).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.g).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.b).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.a).clamp(0.0, 1.0) as f32,
            ];
            let outer = [
                self.ctx.get(self.r2).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.g2).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.b2).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.a2).clamp(0.0, 1.0) as f32,
            ];
            let border_colour = [
                self.ctx.get(self.border_r).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.border_g).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.border_b).clamp(0.0, 1.0) as f32,
                self.ctx.get(self.border_a).clamp(0.0, 1.0) as f32,
            ];

            if inner[3] < 0.004 && outer[3] < 0.004 && border_colour[3] < 0.004 {
                continue;
            }

            // `rad` is in *clip* units (-1..1), not in the 0..1 space `x` and
            // `y` are given in -- MilkDrop adds it after the conversion. Adding
            // it before and doubling drew every shape at twice its size, which
            // for an additive or textured shape nested a few deep is a white
            // screen. The quarter turn is MilkDrop's too: at `ang=0` a
            // four-sided shape is a square with its sides on the axes, not a
            // diamond.
            let tex_ang = self.ctx.get(self.tex_ang);
            let tex_zoom = non_zero(self.ctx.get(self.tex_zoom));
            let centre = [(x as f32) * 2.0 - 1.0, (y as f32) * 2.0 - 1.0];
            let mut rim = Vec::with_capacity(sides);
            let mut rim_uv = Vec::with_capacity(sides);
            for k in 0..sides {
                let t = std::f64::consts::TAU * k as f64 / sides as f64;
                let a = t + ang + std::f64::consts::FRAC_PI_4;
                rim.push([
                    centre[0] + (rad * a.cos() * aspect.y as f64) as f32,
                    centre[1] - (rad * a.sin()) as f32,
                ]);
                let ta = t + tex_ang + std::f64::consts::FRAC_PI_4;
                rim_uv.push([
                    (0.5 + 0.5 * ta.cos() / tex_zoom * aspect.y as f64) as f32,
                    (0.5 - 0.5 * ta.sin() / tex_zoom) as f32,
                ]);
            }

            out.push(ShapeGeom {
                centre,
                rim,
                inner,
                outer,
                border_colour,
                additive: self.ctx.get(self.additive) >= 0.5,
                textured: self.ctx.get(self.textured) >= 0.5,
                centre_uv: [0.5, 0.5],
                rim_uv,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Inputs {
        Inputs::default()
    }

    fn q_seed() -> [f64; Q_COUNT] {
        [0.0; Q_COUNT]
    }

    fn source<'a>(left: &'a [f32], right: &'a [f32]) -> WaveSource<'a> {
        WaveSource {
            left,
            right,
            spectrum: &[],
            wave_scale: 1.0,
            aspect: Aspect::default(),
        }
    }

    #[test]
    fn a_wave_with_no_per_point_draws_nothing() {
        let mut rig = WaveRig::new(&CustomWave {
            index: 0,
            base: BTreeMap::new(),
            init: String::new(),
            per_frame: String::new(),
            per_point: String::new(),
        });
        assert!(rig.build(&inputs(), &q_seed(), &source(&[], &[])).is_none());
    }

    #[test]
    fn a_disabled_wave_draws_nothing() {
        let mut base = BTreeMap::new();
        base.insert("enabled".to_string(), "0".to_string());
        let mut rig = WaveRig::new(&CustomWave {
            index: 0,
            base,
            init: String::new(),
            per_frame: String::new(),
            per_point: "x=0.5;y=0.5;".to_string(),
        });
        assert!(rig
            .build(&inputs(), &q_seed(), &source(&[0.0; 512], &[0.0; 512]))
            .is_none());
    }

    #[test]
    fn a_wave_maps_its_points_into_clip_space() {
        let mut base = BTreeMap::new();
        base.insert("enabled".to_string(), "1".to_string());
        base.insert("samples".to_string(), "3".to_string());
        let mut rig = WaveRig::new(&CustomWave {
            index: 0,
            base,
            init: String::new(),
            per_frame: String::new(),
            per_point: "x=sample;y=0.5;a=1;".to_string(),
        });
        let wave = rig
            .build(&inputs(), &q_seed(), &source(&[0.0; 512], &[0.0; 512]))
            .expect("draws");
        // Three points, tessellated to five: a new one between each pair.
        assert_eq!(wave.points.len(), 5);
        // sample 0 -> x=0 -> clip -1; sample 1 -> x=1 -> clip +1.
        assert!(
            (wave.points[0][0] + 1.0).abs() < 1e-5,
            "{:?}",
            wave.points[0]
        );
        assert!(
            (wave.points[4][0] - 1.0).abs() < 1e-5,
            "{:?}",
            wave.points[4]
        );
        assert!((wave.points[2][1]).abs() < 1e-5, "y=0.5 -> 0");
    }

    #[test]
    fn a_wave_reads_the_waveform_samples() {
        let mut base = BTreeMap::new();
        base.insert("enabled".to_string(), "1".to_string());
        base.insert("samples".to_string(), "2".to_string());
        let mut rig = WaveRig::new(&CustomWave {
            index: 0,
            base,
            init: String::new(),
            per_frame: String::new(),
            per_point: "x=value1+0.5;y=0.5;a=1;".to_string(),
        });
        // value1 = 0.25 * MilkDrop's 0.512 = 0.128 -> x = 0.628 -> clip 0.256.
        let left = [0.25f32; 512];
        let right = [0.0f32; 512];
        let wave = rig
            .build(&inputs(), &q_seed(), &source(&left, &right))
            .expect("draws");
        assert!(
            (wave.points[0][0] - 0.256).abs() < 1e-4,
            "{:?}",
            wave.points[0]
        );
    }

    #[test]
    fn a_wave_whose_ends_fade_out_still_draws_and_keeps_each_points_colour() {
        // The idiom that drew nothing when the strip took its last point's
        // colour: alpha follows sin(sample*pi), so the last point is 0.
        let mut base = BTreeMap::new();
        base.insert("enabled".to_string(), "1".to_string());
        base.insert("samples".to_string(), "5".to_string());
        base.insert("bUseDots".to_string(), "1".to_string());
        let mut rig = WaveRig::new(&CustomWave {
            index: 0,
            base,
            init: String::new(),
            per_frame: String::new(),
            per_point: "x=sample;y=0.5;a=sin(sample*3.14159265);".to_string(),
        });
        let wave = rig
            .build(&inputs(), &q_seed(), &source(&[0.0; 512], &[0.0; 512]))
            .expect("a wave with opaque middle points draws");
        let colours = wave.colours.expect("per-point colours");
        assert!(colours[0][3] < 1e-3 && colours[4][3] < 1e-3);
        assert!((colours[2][3] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn each_point_starts_from_the_frame_colour_not_the_previous_point() {
        let mut base = BTreeMap::new();
        base.insert("enabled".to_string(), "1".to_string());
        base.insert("samples".to_string(), "3".to_string());
        base.insert("bUseDots".to_string(), "1".to_string());
        base.insert("r".to_string(), "0.25".to_string());
        let mut rig = WaveRig::new(&CustomWave {
            index: 0,
            base,
            init: String::new(),
            per_frame: String::new(),
            // Only the first point writes r; the others must read 0.25 again.
            per_point: "r=if(equal(sample,0),1,r);a=1;".to_string(),
        });
        let wave = rig
            .build(&inputs(), &q_seed(), &source(&[0.0; 512], &[0.0; 512]))
            .expect("draws");
        let colours = wave.colours.expect("per-point colours");
        assert!((colours[0][0] - 1.0).abs() < 1e-6);
        assert!((colours[1][0] - 0.25).abs() < 1e-6, "{:?}", colours[1]);
    }

    #[test]
    fn a_shape_builds_a_fan_of_the_requested_sides() {
        let mut base = BTreeMap::new();
        base.insert("enabled".to_string(), "1".to_string());
        base.insert("sides".to_string(), "4".to_string());
        base.insert("num_inst".to_string(), "1".to_string());
        base.insert("a".to_string(), "1".to_string());
        let mut rig = ShapeRig::new(&CustomShape {
            index: 0,
            base,
            init: String::new(),
            per_frame: "rad=0.5;x=0.5;y=0.5;".to_string(),
        });
        let shapes = rig.build(&inputs(), &q_seed(), Aspect::default());
        assert_eq!(shapes.len(), 1);
        // A 4-sided polygon has a 4-vertex rim.
        assert_eq!(shapes[0].rim.len(), 4);
    }

    #[test]
    fn a_disabled_shape_draws_nothing() {
        let mut base = BTreeMap::new();
        base.insert("enabled".to_string(), "0".to_string());
        let mut rig = ShapeRig::new(&CustomShape {
            index: 0,
            base,
            init: String::new(),
            per_frame: "rad=0.5;".to_string(),
        });
        assert!(rig
            .build(&inputs(), &q_seed(), Aspect::default())
            .is_empty());
    }
}
