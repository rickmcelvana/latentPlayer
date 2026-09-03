//! MilkDrop 3 sprites (Phase 3 M7c): an external image drawn as a textured
//! quad over a preset, with EEL code of its own.
//!
//! A `[SPRITE_BEGIN]` block carries the image's name, a colour key, a blend
//! mode and the sprite's starting transform, plus `code_N` lines that run once
//! a frame and may overwrite any of the transform. The transform variables are
//! MilkDrop 2's own (`x`, `y`, `sx`, `sy`, `rot`, `r`, `g`, `b`, `a`,
//! `repeatx`, `repeaty`, `blendmode`), seeded from the file's `Sprite*` scalars
//! and then handed to the code -- which is how a preset saved in the MilkDrop 3
//! editor keeps its GUI values as defaults while its own code animates them.
//!
//! A sprite has **its own variable pool** (its own `Symbols`/`Context`, the
//! three-pool rule), and -- unlike a wave or a shape -- **no `q`/`t` bridges**:
//! MilkDrop 2's sprite expressions have none, so `megabuf`/`gmegabuf`/`reg` are
//! per-sprite and the only things flowing in are the ten frame inputs.
//!
//! # The transform, read from MilkDrop 2 rather than from memory
//!
//! `milkdropfs.cpp`'s `DrawUserSprites` draws a triangle strip whose four
//! corners are `(±sx, ±sy)`, then, in order: corrects for the image's own
//! aspect (`img_h/img_w`), rotates by `rot`, translates to `(x, y)` where
//! `x = var_x*2-1` (so `var_x` is 0..1, 0.5 at the centre), and finally
//! corrects for the screen's aspect so a square image stays square on a wide
//! frame. `sx`/`sy` are in clip units, so 1.0 spans the whole frame. The
//! scalar-to-variable seeding is MilkDrop 3's own editor convention -- `SpriteX`
//! is an offset from centre, `SpriteSX` an offset from full size -- and is
//! overwritten by the code in almost every preset in the corpus, so it matters
//! only for the rare sprite with no code.

use std::collections::BTreeMap;

use crate::milk::eel::{self, Context, Program, Symbols};
use crate::milk::state::Inputs;
use crate::milk::Sprite;

/// The read-only inputs a sprite's code sees, the same ten as a wave or shape.
struct InputSlots {
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
}

/// The transform a sprite's code may overwrite.
struct OutputSlots {
    x: usize,
    y: usize,
    sx: usize,
    sy: usize,
    rot: usize,
    r: usize,
    g: usize,
    b: usize,
    a: usize,
    repeatx: usize,
    repeaty: usize,
    blendmode: usize,
    done: usize,
    burn: usize,
    flipx: usize,
    flipy: usize,
}

/// One compiled sprite: its code, its pool, and its seeded defaults.
pub struct SpriteRig {
    _symbols: Symbols,
    ctx: Context,
    code: Option<Program>,
    inputs: InputSlots,
    out: OutputSlots,
    /// The image file's name, verbatim from `SpriteName` -- the host reads the
    /// file and hands back the decoded image by this name.
    pub name: String,
    /// The colour key the image is matched against, and whether one is set.
    pub colorkey: [f32; 3],
    pub has_colorkey: bool,
    /// The image's pixel size, filled in by the host once decoded -- the aspect
    /// correction needs it.
    pub img_size: [f32; 2],
    /// The file's starting transform, re-seeded every frame before the code.
    seed: [f64; 6],
    seed_colour: [f64; 4],
    seed_repeat: [f64; 2],
    seed_blendmode: f64,
    seed_flipx: f64,
    seed_flipy: f64,
    /// What did not compile, for the preset report.
    pub faults: Vec<String>,
}

/// The result of running one sprite for one frame: everything the draw pass
/// needs, already in clip space.
pub struct SpriteDraw {
    /// Four corners in clip space, triangle-strip order.
    pub corners: [[f32; 2]; 4],
    /// The matching texture coordinates, with the repeat applied.
    pub uvs: [[f32; 2]; 4],
    /// `r`, `g`, `b`, `a`, each 0..1.
    pub modulate: [f32; 4],
    /// The blend mode, 0..10 (MilkDrop 3 extends MilkDrop 2's 0..4).
    pub blendmode: u32,
    /// The colour key, and whether one is set.
    pub colorkey: [f32; 3],
    pub has_colorkey: bool,
    /// `true` means the sprite asked to be removed; the caller may ignore it.
    pub done: bool,
}

fn intern_inputs(s: &mut Symbols) -> InputSlots {
    InputSlots {
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
    }
}

fn intern_outputs(s: &mut Symbols) -> OutputSlots {
    OutputSlots {
        x: s.slot("x"),
        y: s.slot("y"),
        sx: s.slot("sx"),
        sy: s.slot("sy"),
        rot: s.slot("rot"),
        r: s.slot("r"),
        g: s.slot("g"),
        b: s.slot("b"),
        a: s.slot("a"),
        repeatx: s.slot("repeatx"),
        repeaty: s.slot("repeaty"),
        blendmode: s.slot("blendmode"),
        done: s.slot("done"),
        burn: s.slot("burn"),
        flipx: s.slot("flipx"),
        flipy: s.slot("flipy"),
    }
}

fn read_num(base: &BTreeMap<String, String>, key: &str, default: f64) -> f64 {
    base.get(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// `SpriteColorKey=0xRRGGBB` (or decimal) to `[r, g, b]`; `None` when absent.
fn parse_colorkey(base: &BTreeMap<String, String>) -> Option<[f32; 3]> {
    let raw = base.get("ColorKey")?;
    let raw = raw.trim();
    let value = if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        raw.parse::<u32>().ok()?
    };
    Some([
        ((value >> 16) & 0xff) as f32 / 255.0,
        ((value >> 8) & 0xff) as f32 / 255.0,
        (value & 0xff) as f32 / 255.0,
    ])
}

impl SpriteRig {
    pub fn new(sprite: &Sprite) -> Self {
        let mut symbols = Symbols::new();
        let mut faults = Vec::new();
        let inputs = intern_inputs(&mut symbols);
        let out = intern_outputs(&mut symbols);

        let init = compile(&mut symbols, &sprite.init, &mut faults);
        let code = compile(&mut symbols, &sprite.code, &mut faults);

        let mut ctx = Context::new(&symbols);
        ctx.sync(&symbols);
        if let Some(init) = &init {
            if ctx.run(init).is_err() {
                faults.push("init ran past its budget".into());
            }
        }

        Self {
            _symbols: symbols,
            ctx,
            code,
            inputs,
            out,
            name: sprite
                .base
                .get("Name")
                .map(String::as_str)
                .unwrap_or("")
                .replace('\\', "/"),
            colorkey: parse_colorkey(&sprite.base).unwrap_or([0.0; 3]),
            has_colorkey: parse_colorkey(&sprite.base).is_some(),
            img_size: [1.0, 1.0],
            // MilkDrop 3's editor stores an offset from centre / from full size;
            // the classic variables are centre = 0.5 and full = 1.0.
            seed: [
                0.5 + read_num(&sprite.base, "X", 0.0),
                0.5 + read_num(&sprite.base, "Y", 0.0),
                1.0 + read_num(&sprite.base, "SX", 0.0),
                1.0 + read_num(&sprite.base, "SY", 0.0),
                read_num(&sprite.base, "Rot", 0.0),
                0.0,
            ],
            seed_colour: [1.0, 1.0, 1.0, read_num(&sprite.base, "Alpha", 1.0)],
            seed_repeat: [
                read_num(&sprite.base, "RepeatX", 1.0),
                read_num(&sprite.base, "RepeatY", 1.0),
            ],
            seed_blendmode: read_num(&sprite.base, "Blend", 0.0),
            seed_flipx: 0.0,
            seed_flipy: 0.0,
            faults,
        }
    }

    /// Runs the sprite's code for one frame and returns the quad to draw.
    ///
    /// `None` when the sprite asked to be removed (`done`) or its alpha has gone
    /// to nothing; `screen_aspect` is the feedback buffer's width/height.
    pub fn build(&mut self, inputs: &Inputs, screen_aspect: f32) -> Option<SpriteDraw> {
        self.ctx.set(self.inputs.time, inputs.time as f64);
        self.ctx.set(self.inputs.fps, inputs.fps as f64);
        self.ctx.set(self.inputs.frame, inputs.frame as f64);
        self.ctx.set(self.inputs.progress, inputs.progress as f64);
        self.ctx.set(self.inputs.bass, inputs.bass as f64);
        self.ctx.set(self.inputs.mid, inputs.mid as f64);
        self.ctx.set(self.inputs.treb, inputs.treb as f64);
        self.ctx.set(self.inputs.bass_att, inputs.bass_att as f64);
        self.ctx.set(self.inputs.mid_att, inputs.mid_att as f64);
        self.ctx.set(self.inputs.treb_att, inputs.treb_att as f64);

        // The code may overwrite any of these, so they start at the file's own
        // defaults every frame, like the preset's scalars do.
        self.ctx.set(self.out.x, self.seed[0]);
        self.ctx.set(self.out.y, self.seed[1]);
        self.ctx.set(self.out.sx, self.seed[2]);
        self.ctx.set(self.out.sy, self.seed[3]);
        self.ctx.set(self.out.rot, self.seed[4]);
        self.ctx.set(self.out.r, self.seed_colour[0]);
        self.ctx.set(self.out.g, self.seed_colour[1]);
        self.ctx.set(self.out.b, self.seed_colour[2]);
        self.ctx.set(self.out.a, self.seed_colour[3]);
        self.ctx.set(self.out.repeatx, self.seed_repeat[0]);
        self.ctx.set(self.out.repeaty, self.seed_repeat[1]);
        self.ctx.set(self.out.blendmode, self.seed_blendmode);
        self.ctx.set(self.out.done, 0.0);
        self.ctx.set(self.out.burn, 1.0);
        self.ctx.set(self.out.flipx, self.seed_flipx);
        self.ctx.set(self.out.flipy, self.seed_flipy);

        if let Some(code) = self.code.as_ref() {
            if self.ctx.run(code).is_err() {
                self.faults.push("code ran past its budget".into());
            }
        }

        let done = self.ctx.get(self.out.done) != 0.0;
        let x = self.ctx.get(self.out.x);
        let y = self.ctx.get(self.out.y);
        let sx = self.ctx.get(self.out.sx).clamp(-1000.0, 1000.0);
        let sy = self.ctx.get(self.out.sy).clamp(-1000.0, 1000.0);
        let rot = self.ctx.get(self.out.rot);
        let flipx = self.ctx.get(self.out.flipx) != 0.0;
        let flipy = self.ctx.get(self.out.flipy) != 0.0;
        let repeatx = self.ctx.get(self.out.repeatx).clamp(0.01, 100.0);
        let repeaty = self.ctx.get(self.out.repeaty).clamp(0.01, 100.0);
        let modulate = [
            self.ctx.get(self.out.r).clamp(0.0, 1.0) as f32,
            self.ctx.get(self.out.g).clamp(0.0, 1.0) as f32,
            self.ctx.get(self.out.b).clamp(0.0, 1.0) as f32,
            self.ctx.get(self.out.a).clamp(0.0, 1.0) as f32,
        ];
        let blendmode = (self.ctx.get(self.out.blendmode).round() as i64).clamp(0, 10) as u32;

        // The alpha that actually matters: for a sprite drawn into the loop,
        // `a` under a hair is invisible, and `done` removes it entirely.
        if done || modulate[3] < 0.004 {
            return None;
        }

        // The four corners, triangle-strip order (0,1,2,3), in MilkDrop 2's
        // own arrangement before any aspect or rotation.
        let mut corners = [[-sx, -sy], [sx, -sy], [-sx, sy], [sx, sy]];
        if flipx {
            corners.swap(0, 1);
            corners.swap(2, 3);
        }
        if flipy {
            corners.swap(0, 2);
            corners.swap(1, 3);
        }

        // Image aspect: a wide image's quad is shorter than it is wide, and a
        // tall image's is narrower -- so `sx`/`sy` describe the *long* side.
        let img_aspect = (self.img_size[1] / self.img_size[0].max(1.0)) as f64;
        for c in &mut corners {
            if img_aspect < 1.0 {
                c[1] *= img_aspect;
            } else {
                c[0] /= img_aspect;
            }
        }

        let (cos, sin) = (rot.cos(), rot.sin());
        for c in &mut corners {
            let (cx, cy) = (c[0], c[1]);
            c[0] = cx * cos - cy * sin;
            c[1] = cx * sin + cy * cos;
        }

        // Translation into clip space: `x`/`y` are 0..1, centre 0.5.
        let tx = x * 2.0 - 1.0;
        let ty = y * 2.0 - 1.0;
        for c in &mut corners {
            c[0] += tx;
            c[1] += ty;
        }

        // Screen aspect: on a wide frame the quad is widened, so a square image
        // stays square. `screen_aspect` is width/height.
        let sa = screen_aspect.max(1e-3) as f64;
        for c in &mut corners {
            if sa > 1.0 {
                c[1] *= sa;
            } else {
                c[0] /= sa;
            }
        }

        // Texture coordinates: the corners' `±0.5` centred on 0.5, scaled by the
        // repeat. With a wrapping sampler, `repeatx` copies tile across the quad.
        let uv = |cx: f32, cy: f32| [cx * repeatx as f32 + 0.5, cy * repeaty as f32 + 0.5];
        let uvs = [uv(-0.5, -0.5), uv(0.5, -0.5), uv(-0.5, 0.5), uv(0.5, 0.5)];

        Some(SpriteDraw {
            corners: corners.map(|c| [c[0] as f32, c[1] as f32]),
            uvs,
            modulate,
            blendmode,
            colorkey: self.colorkey,
            has_colorkey: self.has_colorkey,
            done,
        })
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn sprite(base: BTreeMap<String, String>, code: &str) -> Sprite {
        Sprite {
            index: 1,
            base,
            init: String::new(),
            code: code.to_string(),
        }
    }

    #[test]
    fn a_sprite_with_no_code_draws_at_the_file_defaults() {
        let mut base = BTreeMap::new();
        base.insert("SX".to_string(), "-0.5".to_string());
        base.insert("SY".to_string(), "-0.5".to_string());
        let mut rig = SpriteRig::new(&sprite(base, ""));
        let draw = rig.build(&Inputs::default(), 1.0).expect("draws");
        // sx = 1 + (-0.5) = 0.5 -> corners at ±0.5 in clip space.
        assert!(
            (draw.corners[0][0] + 0.5).abs() < 1e-4,
            "{:?}",
            draw.corners
        );
        assert!(
            (draw.corners[3][0] - 0.5).abs() < 1e-4,
            "{:?}",
            draw.corners
        );
        assert_eq!(draw.blendmode, 0);
    }

    #[test]
    fn the_code_overrides_the_file_seed() {
        let mut rig = SpriteRig::new(&sprite(BTreeMap::new(), "x=0.25;y=0.75;sx=1;sy=1;"));
        let draw = rig.build(&Inputs::default(), 1.0).expect("draws");
        // x=0.25 -> tx = -0.5; the sprite spans -1.5..-0.5.
        assert!(
            (draw.corners[0][0] + 1.5).abs() < 1e-4,
            "{:?}",
            draw.corners
        );
        // y=0.75 -> ty = 0.5; the top of the sprite is above centre.
        assert!(draw.corners[3][1] > 0.5, "{:?}", draw.corners);
    }

    #[test]
    fn a_done_sprite_draws_nothing() {
        let mut rig = SpriteRig::new(&sprite(BTreeMap::new(), "done=1;"));
        assert!(rig.build(&Inputs::default(), 1.0).is_none());
    }

    #[test]
    fn the_colour_key_is_parsed_from_hex() {
        let mut base = BTreeMap::new();
        base.insert("ColorKey".to_string(), "0xFF0000".to_string());
        let rig = SpriteRig::new(&sprite(base, ""));
        assert!(rig.has_colorkey);
        assert!((rig.colorkey[0] - 1.0).abs() < 1e-5);
        assert!(rig.colorkey[1] < 1e-5);
    }

    #[test]
    fn repeat_scales_the_texture_coordinates() {
        let mut base = BTreeMap::new();
        base.insert("RepeatX".to_string(), "2.0".to_string());
        let mut rig = SpriteRig::new(&sprite(base, ""));
        let draw = rig.build(&Inputs::default(), 1.0).expect("draws");
        // repeatx=2 -> uv spans 0.5 - 1.0 .. 0.5 + 1.0 = -0.5 .. 1.5.
        assert!((draw.uvs[0][0] + 0.5).abs() < 1e-4, "{:?}", draw.uvs);
        assert!((draw.uvs[1][0] - 1.5).abs() < 1e-4, "{:?}", draw.uvs);
    }
}
