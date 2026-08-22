//! MilkDrop's shader environment, which decision 55 says is ours to provide
//! and is written down.
//!
//! A preset's shader body is not a whole program and never was. MilkDrop
//! pastes `data/include.fx` in front of it, wraps it in an entry point, and
//! compiles the result -- so `uv`, `time`, `bass`, `q1` and `GetBlur2` are not
//! HLSL, they are *this file*, and a translator without them translates
//! nothing.
//!
//! Read out of MilkDrop 2's own source rather than from memory, under the same
//! rule M7a's warp transform was: `plugin.cpp`'s `CPlugin::LoadShaderFromMemory`
//! for the wrapper, and `data/include.fx` for the environment. See
//! [`milk`](super::super)'s module note for the licence.
//!
//! # The surprise: it is macros, not uniforms
//!
//! The plan said "the `texsize`/`aspect` uniforms", and that is not the shape.
//! MilkDrop declares **fourteen `float4` constant registers** `_c0.._c13` and
//! **eight more** `_qa.._qh`, and every named input is a `#define` onto a lane
//! of one of them: `#define time _c2.x`, `#define q17 _qe.x`,
//! `#define aspect _c0`. Sixty-odd names over twenty-two vectors.
//!
//! That matters twice over. It is far less uniform plumbing than "one binding
//! per name" would be -- and it means two names can be the *same storage*,
//! which they are: `blur1_min` is `_c6.z` and `GetBlur3`'s bias is `_c6.y`, in
//! one register.
//!
//! # The second surprise: a blur texture is not a blurred picture
//!
//! ```text
//! #define GetBlur1(uv) (tex2D(sampler_blur1, uv).xyz * _c5.x + _c5.y)
//! ```
//!
//! The blur chain stores its result *remapped* into whatever range the
//! `blur1_min`/`blur1_max` settings give it, and `_c5`/`_c6` carry the inverse
//! mapping so the shader gets the original values back. A blur pass that
//! writes the blurred image straight out and a `GetBlur1` that just samples it
//! would agree at the default settings and diverge everywhere else -- which is
//! the kind of difference that reads as "the preset looks a bit off" rather
//! than as a bug.
//!
//! # `uv_orig` in a composite shader is `uv`, and MilkDrop says so
//!
//! `LoadShaderFromMemory` writes `"#define uv_orig _uv.xy\n" //[sic]` for the
//! composite shader, against `_uv.zw` for the warp shader. The `[sic]` is
//! Nullsoft's own comment. A composite shader has no un-warped coordinate to
//! offer, so the name resolves to the warped one rather than to nothing, and a
//! preset written against that behaviour depends on it.

use super::ty::Ty;

/// A name the preset's shader may use, and the storage it resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alias {
    /// What the preset writes.
    pub name: &'static str,
    /// What `include.fx` defines it as, verbatim.
    pub target: &'static str,
    pub ty: Ty,
}

const fn f(name: &'static str, target: &'static str) -> Alias {
    Alias {
        name,
        target,
        ty: Ty::FLOAT,
    }
}

const fn f4(name: &'static str, target: &'static str) -> Alias {
    Alias {
        name,
        target,
        ty: Ty::FLOAT4,
    }
}

/// Every `#define` in `include.fx` that names a value, in its order.
///
/// The rotation matrices (`rot_s1..4`, `rot_d*`, `rot_f*`, `rot_vf*`,
/// `rot_uf*`, `rot_rand*`) are declared directly as `float4x3` rather than
/// aliased, so they are in [`MATRICES`] instead.
pub const ALIASES: &[Alias] = &[
    // _c0 -- the aspect pair, as a whole register.
    f4("aspect", "_c0"),
    // _c2 -- time and the frame counters.
    f("time", "_c2.x"),
    f("fps", "_c2.y"),
    f("frame", "_c2.z"),
    f("progress", "_c2.w"),
    // _c3 -- the immediate audio levels.
    f("bass", "_c3.x"),
    f("mid", "_c3.y"),
    f("treb", "_c3.z"),
    f("vol", "_c3.w"),
    // _c4 -- the attenuated ones. `bass/bass_att > 1` means the bass is
    // spiking, which is how presets detect a hit.
    f("bass_att", "_c4.x"),
    f("mid_att", "_c4.y"),
    f("treb_att", "_c4.z"),
    f("vol_att", "_c4.w"),
    // _c6/_c13 -- the blur ranges. Note _c6 is shared with GetBlur3's own
    // scale and bias; see the module note.
    f("blur1_min", "_c6.z"),
    f("blur1_max", "_c6.w"),
    f("blur2_min", "_c13.x"),
    f("blur2_max", "_c13.y"),
    f("blur3_min", "_c13.z"),
    f("blur3_max", "_c13.w"),
    // _c7 -- .xy = (width, height), .zw = the reciprocals. The corpus reaches
    // for `texsize.zw` constantly, as the size of one pixel in UV space.
    f4("texsize", "_c7"),
    // _c8.._c11 -- the roaming oscillators, four rates each.
    f4("roam_cos", "_c8"),
    f4("roam_sin", "_c9"),
    f4("slow_roam_cos", "_c10"),
    f4("slow_roam_sin", "_c11"),
    // _qa.._qh -- q1..q32, four to a register, carried from the per-frame
    // equations. The single widest channel between a preset's code and its
    // picture.
    f("q1", "_qa.x"),
    f("q2", "_qa.y"),
    f("q3", "_qa.z"),
    f("q4", "_qa.w"),
    f("q5", "_qb.x"),
    f("q6", "_qb.y"),
    f("q7", "_qb.z"),
    f("q8", "_qb.w"),
    f("q9", "_qc.x"),
    f("q10", "_qc.y"),
    f("q11", "_qc.z"),
    f("q12", "_qc.w"),
    f("q13", "_qd.x"),
    f("q14", "_qd.y"),
    f("q15", "_qd.z"),
    f("q16", "_qd.w"),
    f("q17", "_qe.x"),
    f("q18", "_qe.y"),
    f("q19", "_qe.z"),
    f("q20", "_qe.w"),
    f("q21", "_qf.x"),
    f("q22", "_qf.y"),
    f("q23", "_qf.z"),
    f("q24", "_qf.w"),
    f("q25", "_qg.x"),
    f("q26", "_qg.y"),
    f("q27", "_qg.z"),
    f("q28", "_qg.w"),
    f("q29", "_qh.x"),
    f("q30", "_qh.y"),
    f("q31", "_qh.z"),
    f("q32", "_qh.w"),
    // Declared directly rather than aliased, and named here so the parser
    // resolves them like everything else.
    f4("rand_frame", "rand_frame"),
    f4("rand_preset", "rand_preset"),
];

/// The rotation matrices, all `float4x3`.
///
/// Six families of four: static, slow, fast, very fast, ultra fast, and one
/// re-rolled every frame. A preset uses them with `mul` to spin a coordinate.
pub const MATRICES: &[&str] = &[
    "rot_s1",
    "rot_s2",
    "rot_s3",
    "rot_s4",
    "rot_d1",
    "rot_d2",
    "rot_d3",
    "rot_d4",
    "rot_f1",
    "rot_f2",
    "rot_f3",
    "rot_f4",
    "rot_vf1",
    "rot_vf2",
    "rot_vf3",
    "rot_vf4",
    "rot_uf1",
    "rot_uf2",
    "rot_uf3",
    "rot_uf4",
    "rot_rand1",
    "rot_rand2",
    "rot_rand3",
    "rot_rand4",
];

/// Every sampler `include.fx` declares.
///
/// `sampler_main` is the previous frame. The four `sampler_?[cw]_main` names
/// are the same texture with the four filter/address combinations -- `f`/`p`
/// for bilinear or point, `c`/`w` for clamp or wrap. The noise textures are
/// generated by MilkDrop rather than loaded, so they are in scope
/// (decision 56); the two `sampler3D` ones are volume noise.
pub const SAMPLERS: &[(&str, bool)] = &[
    ("sampler_main", false),
    ("sampler_fc_main", false),
    ("sampler_pc_main", false),
    ("sampler_fw_main", false),
    ("sampler_pw_main", false),
    ("sampler_noise_lq", false),
    ("sampler_noise_lq_lite", false),
    ("sampler_noise_mq", false),
    ("sampler_noise_hq", false),
    ("sampler_noisevol_lq", true),
    ("sampler_noisevol_hq", true),
    ("sampler_blur1", false),
    ("sampler_blur2", false),
    ("sampler_blur3", false),
];

/// The helper macros, verbatim from `include.fx`, and what each returns.
///
/// All five are `#define`s rather than functions, which is why `GetPixel(uv)`
/// costs nothing beyond its `tex2D` and why none of them can be overloaded by
/// a preset that declares a function of the same name -- the macro wins.
pub const MACROS: &[(&str, &str, Ty)] = &[
    ("GetMain", "(tex2D(sampler_main, uv).xyz)", Ty::FLOAT3),
    ("GetPixel", "(tex2D(sampler_main, uv).xyz)", Ty::FLOAT3),
    (
        "GetBlur1",
        "(tex2D(sampler_blur1, uv).xyz * _c5.x + _c5.y)",
        Ty::FLOAT3,
    ),
    (
        "GetBlur2",
        "(tex2D(sampler_blur2, uv).xyz * _c5.z + _c5.w)",
        Ty::FLOAT3,
    ),
    (
        "GetBlur3",
        "(tex2D(sampler_blur3, uv).xyz * _c6.x + _c6.y)",
        Ty::FLOAT3,
    ),
    ("lum", "(dot(x, float3(0.32, 0.49, 0.29)))", Ty::FLOAT),
];

/// Which of the two shaders is being translated. They differ in four
/// `#define`s and in one parameter's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Warp,
    Comp,
}

impl Pass {
    /// The per-pass names, exactly as `LoadShaderFromMemory` defines them.
    ///
    /// The composite shader's `uv_orig` really is its `uv`; `plugin.cpp`
    /// carries a `//[sic]` on that line. See the module note.
    pub fn defines(self) -> &'static [Alias] {
        const WARP: &[Alias] = &[
            f("rad", "_rad_ang.x"),
            f("ang", "_rad_ang.y"),
            Alias {
                name: "uv",
                target: "_uv.xy",
                ty: Ty::FLOAT2,
            },
            Alias {
                name: "uv_orig",
                target: "_uv.zw",
                ty: Ty::FLOAT2,
            },
        ];
        const COMP: &[Alias] = &[
            f("rad", "_rad_ang.x"),
            f("ang", "_rad_ang.y"),
            Alias {
                name: "uv",
                target: "_uv.xy",
                ty: Ty::FLOAT2,
            },
            Alias {
                name: "uv_orig",
                target: "_uv.xy",
                ty: Ty::FLOAT2,
            },
            Alias {
                name: "hue_shader",
                target: "_vDiffuse.xyz",
                ty: Ty::FLOAT3,
            },
        ];
        match self {
            Pass::Warp => WARP,
            Pass::Comp => COMP,
        }
    }
}

/// The name of the wrapper macro a preset writes before its block.
///
/// `LoadShaderFromMemory` finds this word, blanks it, and writes an entry
/// point signature in its place -- so it is a marker in the source rather than
/// a macro `include.fx` defines, and a shader body without one is one MilkDrop
/// itself refuses to compile.
pub const BODY_MARKER: &str = "shader_body";

/// What MilkDrop inserts immediately inside the body's opening brace.
///
/// `ret` is the output, it is declared by the wrapper rather than by the
/// preset, and **it starts at zero**. A preset that writes nothing to it draws
/// black rather than drawing the previous frame.
pub const FIRST_LINE: &str = "float3 ret = 0;";

/// What MilkDrop appends before the body's closing brace.
///
/// The alpha comes from the *vertex* colour, not from the preset: a shader
/// controls three channels and the fourth is the pipeline's.
pub const LAST_LINE: &str = "_return_value = float4(ret.xyz, _vDiffuse.w);";

/// Look a name up in the environment for `pass`.
///
/// Per-pass names shadow the shared ones, which is the order
/// `LoadShaderFromMemory` pastes them in: `include.fx` first, then the pass
/// defines.
pub fn lookup(pass: Pass, name: &str) -> Option<Alias> {
    pass.defines()
        .iter()
        .chain(ALIASES)
        .find(|a| a.name == name)
        .copied()
}

/// The type of a name in the environment, whatever kind of thing it is.
pub fn type_of(pass: Pass, name: &str) -> Option<Ty> {
    if let Some(alias) = lookup(pass, name) {
        return Some(alias.ty);
    }
    if MATRICES.contains(&name) {
        return Some(Ty::Mat {
            base: super::ty::Base::Float,
            rows: 4,
            cols: 3,
        });
    }
    if SAMPLERS.iter().any(|(s, _)| *s == name) {
        return Some(Ty::Sampler);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn all_thirty_two_q_values_are_present_and_land_in_distinct_lanes() {
        let mut targets = BTreeSet::new();
        for i in 1..=32 {
            let name = format!("q{i}");
            let alias = lookup(Pass::Warp, &name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(alias.ty, Ty::FLOAT, "{name}");
            assert!(targets.insert(alias.target), "{name} shares a lane");
        }
        assert_eq!(targets.len(), 32);
    }

    #[test]
    fn the_registers_are_eight_q_blocks_and_fourteen_constants() {
        // Two names sharing one register is normal and expected -- _c6 holds
        // GetBlur3's scale and bias *and* blur1's range. What would not be
        // normal is a name pointing outside the declared set.
        for alias in ALIASES {
            let reg = alias.target.split('.').next().unwrap();
            let known = reg.starts_with("_c") && reg[2..].parse::<u32>().is_ok_and(|n| n <= 13)
                || matches!(
                    reg,
                    "_qa" | "_qb" | "_qc" | "_qd" | "_qe" | "_qf" | "_qg" | "_qh"
                )
                || matches!(reg, "rand_frame" | "rand_preset");
            assert!(known, "{} points at {reg}", alias.name);
        }
    }

    #[test]
    fn a_composite_shaders_uv_orig_is_its_uv_which_is_milkdrops_own_sic() {
        assert_eq!(lookup(Pass::Warp, "uv_orig").unwrap().target, "_uv.zw");
        assert_eq!(lookup(Pass::Comp, "uv_orig").unwrap().target, "_uv.xy");
        assert_eq!(lookup(Pass::Comp, "uv").unwrap().target, "_uv.xy");
    }

    #[test]
    fn hue_shader_exists_only_in_the_composite_pass() {
        assert!(lookup(Pass::Comp, "hue_shader").is_some());
        assert!(lookup(Pass::Warp, "hue_shader").is_none());
    }

    #[test]
    fn a_blur_read_is_scaled_and_biased_rather_than_sampled_raw() {
        // The single most forgettable fact in the environment: a blur texture
        // holds a remapped image, and the two constants that undo it are part
        // of the accessor. A GetBlur that only samples agrees at the default
        // settings and is wrong everywhere else.
        for name in ["GetBlur1", "GetBlur2", "GetBlur3"] {
            let (_, body, ty) = MACROS.iter().find(|(n, ..)| *n == name).unwrap();
            assert!(body.contains(" * _c"), "{name} has no scale");
            assert!(body.contains(" + _c"), "{name} has no bias");
            assert_eq!(*ty, Ty::FLOAT3);
        }
        // GetPixel, by contrast, is exactly a sample.
        let (_, pixel, _) = MACROS.iter().find(|(n, ..)| *n == "GetPixel").unwrap();
        assert!(!pixel.contains('*'), "GetPixel is a plain sample");
    }

    #[test]
    fn every_name_the_five_preset_sample_uses_resolves() {
        // Taken from `milk_survey`'s free-identifier list, minus the locals.
        for name in [
            "uv",
            "time",
            "aspect",
            "texsize",
            "rad",
            "ang",
            "bass",
            "q4",
            "q5",
            "q8",
            "q13",
            "q15",
            "q16",
            "q30",
            "sampler_main",
            "sampler_fw_main",
        ] {
            assert!(
                type_of(Pass::Warp, name).is_some(),
                "{name} does not resolve"
            );
        }
        // `ret` deliberately does not: it is declared by the wrapper's first
        // line, not by the environment, so the parser sees it as a local.
        assert!(type_of(Pass::Warp, "ret").is_none());
    }

    #[test]
    fn the_samplers_carry_their_dimensionality() {
        let volume: Vec<&str> = SAMPLERS
            .iter()
            .filter(|(_, is_3d)| *is_3d)
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(volume, vec!["sampler_noisevol_lq", "sampler_noisevol_hq"]);
        assert_eq!(type_of(Pass::Warp, "sampler_blur2"), Some(Ty::Sampler));
    }

    #[test]
    fn the_rotation_matrices_are_six_families_of_four() {
        assert_eq!(MATRICES.len(), 24);
        assert_eq!(
            type_of(Pass::Comp, "rot_uf3"),
            Some(Ty::Mat {
                base: super::super::ty::Base::Float,
                rows: 4,
                cols: 3
            })
        );
    }

    #[test]
    fn ret_starts_at_zero_rather_than_at_the_previous_frame() {
        // A preset whose body never assigns `ret` draws black. Worth a test
        // because "the warp shader starts from the previous frame" is the
        // intuitive reading and it is wrong -- reaching the previous frame is
        // what `GetPixel` is for.
        assert!(FIRST_LINE.contains("ret = 0"));
        assert!(LAST_LINE.contains("_vDiffuse.w"));
    }
}
