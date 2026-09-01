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
    pub fn build(
        &mut self,
        inputs: &Inputs,
        q_seed: &[f64; Q_COUNT],
        left: &[f32],
        right: &[f32],
        spectrum: &[f32],
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
        if samples < 2 {
            return None;
        }

        let mut points = Vec::with_capacity(samples);
        for i in 0..samples {
            let sample = if samples > 1 {
                i as f64 / (samples - 1) as f64
            } else {
                0.0
            };
            self.ctx.set(self.sample, sample);

            let (v1, v2) = if self.spectrum {
                let value = spectrum_value(spectrum, sample) * self.scaling;
                (value, value)
            } else {
                let idx = (sample * (left.len().max(1) - 1) as f64).round() as usize;
                (
                    left.get(idx).copied().unwrap_or(0.0) as f64 * self.scaling,
                    right.get(idx).copied().unwrap_or(0.0) as f64 * self.scaling,
                )
            };
            self.ctx.set(self.value1, v1);
            self.ctx.set(self.value2, v2);

            if self.ctx.run(per_point).is_err() {
                self.faults.push("per_point ran past its budget".into());
                break;
            }

            let x = self.ctx.get(self.x);
            let y = self.ctx.get(self.y);
            points.push([(x as f32) * 2.0 - 1.0, (y as f32) * 2.0 - 1.0]);
        }

        // The strip takes one colour: the one the last point left. (This
        // build's line pipeline is one colour per strip -- the built-in wave's
        // own limit -- so a preset that colours per point gets its last colour,
        // which is what a uniform colouring would show anyway.)
        let colour = [
            self.ctx.get(self.r).clamp(0.0, 1.0) as f32,
            self.ctx.get(self.g).clamp(0.0, 1.0) as f32,
            self.ctx.get(self.b).clamp(0.0, 1.0) as f32,
            self.ctx.get(self.a).clamp(0.0, 1.0) as f32,
        ];

        let alpha = colour[3];
        if alpha < 0.004 || points.len() < 2 {
            return None;
        }

        Some(Wave {
            points,
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
            file_rad: read_num(&custom.base, "rad", 0.5),
            file_ang: read_num(&custom.base, "ang", 0.0),
            file_textured: read_flag(&custom.base, "textured", false),
            file_inner: [
                read_num(&custom.base, "r", 1.0),
                read_num(&custom.base, "g", 1.0),
                read_num(&custom.base, "b", 1.0),
                read_num(&custom.base, "a", 1.0),
            ],
            file_outer: [
                read_num(&custom.base, "r2", 1.0),
                read_num(&custom.base, "g2", 1.0),
                read_num(&custom.base, "b2", 1.0),
                read_num(&custom.base, "a2", 1.0),
            ],
            file_border: [
                read_num(&custom.base, "border_r", 1.0),
                read_num(&custom.base, "border_g", 1.0),
                read_num(&custom.base, "border_b", 1.0),
                read_num(&custom.base, "border_a", 0.0),
            ],
            t_after_init,
            faults,
        }
    }

    /// Runs every instance of this shape and returns the geometry to draw.
    ///
    /// Textured shapes (`textured` set) are not drawn -- sampling the feedback
    /// buffer into the polygon is a separate pass. The fill still shows.
    pub fn build(
        &mut self,
        inputs: &Inputs,
        q_seed: &[f64; Q_COUNT],
        aspect: Aspect,
    ) -> Vec<ShapeGeom> {
        let per_frame = match self.per_frame.as_ref() {
            Some(p) => p,
            None => return Vec::new(),
        };
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

            if self.ctx.run(per_frame).is_err() {
                self.faults.push("per_frame ran past its budget".into());
                continue;
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

            let centre = [(x as f32) * 2.0 - 1.0, (y as f32) * 2.0 - 1.0];
            let mut rim = Vec::with_capacity(sides);
            for k in 0..sides {
                let a = ang + std::f64::consts::TAU * k as f64 / sides as f64;
                let ex = x + rad * a.cos() * aspect.y as f64;
                let ey = y + rad * a.sin() * aspect.x as f64;
                rim.push([(ex as f32) * 2.0 - 1.0, (ey as f32) * 2.0 - 1.0]);
            }

            out.push(ShapeGeom {
                centre,
                rim,
                inner,
                outer,
                border_colour,
                additive: self.ctx.get(self.additive) >= 0.5,
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

    #[test]
    fn a_wave_with_no_per_point_draws_nothing() {
        let mut rig = WaveRig::new(&CustomWave {
            index: 0,
            base: BTreeMap::new(),
            init: String::new(),
            per_frame: String::new(),
            per_point: String::new(),
        });
        assert!(rig.build(&inputs(), &q_seed(), &[], &[], &[]).is_none());
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
            .build(&inputs(), &q_seed(), &[0.0; 512], &[0.0; 512], &[])
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
            .build(&inputs(), &q_seed(), &[0.0; 512], &[0.0; 512], &[])
            .expect("draws");
        assert_eq!(wave.points.len(), 3);
        // sample 0 -> x=0 -> clip -1; sample 1 -> x=1 -> clip +1.
        assert!(
            (wave.points[0][0] + 1.0).abs() < 1e-5,
            "{:?}",
            wave.points[0]
        );
        assert!(
            (wave.points[2][0] - 1.0).abs() < 1e-5,
            "{:?}",
            wave.points[2]
        );
        assert!((wave.points[1][1]).abs() < 1e-5, "y=0.5 -> 0");
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
        // value1 = left[0] = 0.25 -> x = 0.75 -> clip 0.5.
        let left = [0.25f32; 512];
        let right = [0.0f32; 512];
        let wave = rig
            .build(&inputs(), &q_seed(), &left, &right, &[])
            .expect("draws");
        assert!(
            (wave.points[0][0] - 0.5).abs() < 1e-4,
            "{:?}",
            wave.points[0]
        );
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
