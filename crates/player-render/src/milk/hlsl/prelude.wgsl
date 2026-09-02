// MilkDrop's shader environment, as WGSL (Phase 3 M7b, decision 55).
//
// `env.rs` records what `include.fx` *says*; this is the same thing in the
// language the translator emits. Every named input a preset can write resolves
// into this file: `time` is `u.c2.x`, `q17` is `u.qe.x`, `GetBlur2(uv)` is the
// function below.
//
// The register shape is MilkDrop's own, deliberately. Fourteen `float4`s plus
// eight more for `q1..q32`, rather than one binding per name, because that is
// how the presets' constants are laid out and inventing a flatter layout would
// mean maintaining a mapping in two places.

struct MilkUniforms {
    // _c0 -- aspect: .xy multiplies UVs to cover the frame, .zw is the inverse.
    c0: vec4<f32>,
    // _c1 -- unused by any name `include.fx` defines, kept so the indices are
    // MilkDrop's rather than ours.
    c1: vec4<f32>,
    // _c2 -- time, fps, frame, progress.
    c2: vec4<f32>,
    // _c3 -- bass, mid, treb, vol.
    c3: vec4<f32>,
    // _c4 -- the attenuated four.
    c4: vec4<f32>,
    // _c5 -- GetBlur1's scale and bias in .xy, GetBlur2's in .zw.
    c5: vec4<f32>,
    // _c6 -- GetBlur3's scale and bias in .xy, blur1's min and max in .zw.
    c6: vec4<f32>,
    // _c7 -- texsize: (w, h, 1/w, 1/h).
    c7: vec4<f32>,
    // _c8.._c11 -- the roaming oscillators.
    c8: vec4<f32>,
    c9: vec4<f32>,
    c10: vec4<f32>,
    c11: vec4<f32>,
    c12: vec4<f32>,
    // _c13 -- blur2's and blur3's ranges.
    c13: vec4<f32>,

    // q1..q32, four to a register.
    qa: vec4<f32>,
    qb: vec4<f32>,
    qc: vec4<f32>,
    qd: vec4<f32>,
    qe: vec4<f32>,
    qf: vec4<f32>,
    qg: vec4<f32>,
    qh: vec4<f32>,

    rand_frame: vec4<f32>,
    rand_preset: vec4<f32>,

    texsize_noise_lq: vec4<f32>,
    texsize_noise_lq_lite: vec4<f32>,
    texsize_noise_mq: vec4<f32>,
    texsize_noise_hq: vec4<f32>,
    texsize_noisevol_lq: vec4<f32>,
    texsize_noisevol_hq: vec4<f32>,

    // The rotation matrices. HLSL's `float4x3` is four rows of three, and
    // this mapping takes those rows to be WGSL's columns -- four columns of
    // three, `mat4x3`. The two numbers stay in the same order, which is not
    // the folklore; see `ty::Ty::wgsl` for why, and `emit::Emitter::mul` for
    // the one place it costs something.
    rot_s1: mat4x3<f32>, rot_s2: mat4x3<f32>, rot_s3: mat4x3<f32>, rot_s4: mat4x3<f32>,
    rot_d1: mat4x3<f32>, rot_d2: mat4x3<f32>, rot_d3: mat4x3<f32>, rot_d4: mat4x3<f32>,
    rot_f1: mat4x3<f32>, rot_f2: mat4x3<f32>, rot_f3: mat4x3<f32>, rot_f4: mat4x3<f32>,
    rot_vf1: mat4x3<f32>, rot_vf2: mat4x3<f32>, rot_vf3: mat4x3<f32>, rot_vf4: mat4x3<f32>,
    rot_uf1: mat4x3<f32>, rot_uf2: mat4x3<f32>, rot_uf3: mat4x3<f32>, rot_uf4: mat4x3<f32>,
    rot_rand1: mat4x3<f32>, rot_rand2: mat4x3<f32>, rot_rand3: mat4x3<f32>, rot_rand4: mat4x3<f32>,
}

@group(0) @binding(0) var<uniform> u: MilkUniforms;

// Four samplers rather than four textures: MilkDrop's `sampler_fw_main`,
// `_fc_`, `_pw_` and `_pc_` are one texture read four ways -- f/p for bilinear
// or point, w/c for wrap or clamp.
@group(0) @binding(1) var samp_fw: sampler;
@group(0) @binding(2) var samp_fc: sampler;
@group(0) @binding(3) var samp_pw: sampler;
@group(0) @binding(4) var samp_pc: sampler;

@group(0) @binding(5) var tex_main: texture_2d<f32>;
@group(0) @binding(6) var tex_blur1: texture_2d<f32>;
@group(0) @binding(7) var tex_blur2: texture_2d<f32>;
@group(0) @binding(8) var tex_blur3: texture_2d<f32>;
@group(0) @binding(9) var tex_noise_lq: texture_2d<f32>;
@group(0) @binding(10) var tex_noise_lq_lite: texture_2d<f32>;
@group(0) @binding(11) var tex_noise_mq: texture_2d<f32>;
@group(0) @binding(12) var tex_noise_hq: texture_2d<f32>;
@group(0) @binding(13) var tex_noisevol_lq: texture_3d<f32>;
@group(0) @binding(14) var tex_noisevol_hq: texture_3d<f32>;
// The user-texture array a preset's texture pack reads from (decision 59).
// Each distinct `sampler_<name>` the preset names maps to a layer here, filled
// by the host from the preset's own folder; a layer whose file is not found
// stays grey, so the preset still draws and the name is reported rather than
// silently dropped.
@group(0) @binding(15) var tex_user: texture_2d_array<f32>;

// `tex2D` becomes `textureSampleLevel`, not `textureSample`, everywhere.
//
// WGSL forbids implicit-derivative sampling under non-uniform control flow,
// and a quarter of the corpus branches -- a `tex2D` inside an `if` is
// completely ordinary in these presets and would make every such shader fail
// to compile. The feedback and blur targets have one mip level, so an explicit
// LOD of zero is exactly what a `tex2D` would have read there anyway.
fn milk_tex2D(t: texture_2d<f32>, s: sampler, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(t, s, uv, 0.0);
}

fn milk_tex3D(t: texture_3d<f32>, s: sampler, uvw: vec3<f32>) -> vec4<f32> {
    return textureSampleLevel(t, s, uvw, 0.0);
}

// A texture-pack read selects its image as the array's layer. The layer is a
// compile-time constant in the emitted shader, so there is never a dynamic,
// non-uniform index into the array.
fn milk_tex2D_array(t: texture_2d_array<f32>, s: sampler, uv: vec2<f32>, layer: i32) -> vec4<f32> {
    return textureSampleLevel(t, s, uv, layer, 0.0);
}

fn GetMain(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(tex_main, samp_fw, uv, 0.0).xyz;
}

fn GetPixel(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(tex_main, samp_fw, uv, 0.0).xyz;
}

// A blur texture holds a *remapped* image -- the chain normalises into the
// blur1_min/blur1_max range -- and _c5/_c6 carry the inverse. A GetBlur that
// only sampled would agree at the default settings and be wrong at every
// other. See `env.rs`.
fn GetBlur1(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(tex_blur1, samp_fc, uv, 0.0).xyz * u.c5.x + u.c5.y;
}

fn GetBlur2(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(tex_blur2, samp_fc, uv, 0.0).xyz * u.c5.z + u.c5.w;
}

fn GetBlur3(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(tex_blur3, samp_fc, uv, 0.0).xyz * u.c6.x + u.c6.y;
}

fn lum(x: vec3<f32>) -> f32 {
    return dot(x, vec3<f32>(0.32, 0.49, 0.29));
}
