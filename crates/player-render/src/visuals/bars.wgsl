// Spectrum bars (Phase 3 M2). One instanced box per display band, plus a
// second instance per band for the peak cap, plus a line per octave on the
// floor. `shared.wgsl` is prepended by `attachments::shader_source`.

struct Scene {
    view_proj: mat4x4<f32>,
    // [span, height, depth, bar_width] in world units -- see bars.rs. The
    // camera there is *fitted* to the first three, so these arrive from Rust
    // rather than being repeated here: a shader constant and a Rust constant
    // drifting apart would frame a different scene than this shader draws.
    // `bar_width` follows the band count, which is the frame's to say.
    dims: vec4<f32>,
    // [srgb_target, 0, 0, 0] -- the surface's colour space, see shared.wgsl.
    flags: vec4<f32>,
};

@group(0) @binding(0) var<uniform> scene: Scene;

/// Where the light comes from, in world space. Fixed and slightly to the left,
/// which is the side the camera is on: the point of shading here is to separate
/// a bar's top from its front from its side, and a light straight down the view
/// axis separates nothing.
const LIGHT: vec3<f32> = vec3<f32>(-0.45, 0.82, 0.36);

struct BarOut {
    @builtin(position) clip_position: vec4<f32>,
    // How far up the *scene* this fragment is, 0 at the floor and 1 at full
    // scale -- the gradient's input, so a tall bar shows the whole ramp and a
    // short one only its bottom. Colour therefore reads as level, which is the
    // one thing a spectrum display has to get right.
    @location(0) height: f32,
    @location(1) shade: f32,
    // 0 for a bar, 1 for a peak cap.
    @location(2) cap: f32,
};

/// A box, instanced. `instance` is [x, base, size, cap]: where the band sits
/// across the display (-0.5 to 0.5), where this box starts and how tall it is
/// (both 0 to 1 of full scale), and which of the two kinds it is.
@vertex
fn vs_bar(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) instance: vec4<f32>,
) -> BarOut {
    var out: BarOut;
    let height = instance.y + position.y * instance.z;
    // Centred on the origin, because that is the scene the camera was fitted
    // to: the floor is at -height/2 rather than at 0.
    let world = vec3<f32>(
        instance.x * scene.dims.x + position.x * scene.dims.w,
        height * scene.dims.y - scene.dims.y * 0.5,
        position.z * scene.dims.z,
    );
    out.clip_position = scene.view_proj * vec4<f32>(world, 1.0);
    out.height = height;
    out.shade = 0.42 + 0.58 * max(dot(normalize(normal), normalize(LIGHT)), 0.0);
    out.cap = instance.w;
    return out;
}

@fragment
fn fs_bar(in: BarOut) -> @location(0) vec4<f32> {
    // The vectorscope's own two colours, used for the same thing they mean
    // there: violet is far away or long ago, green is here and now. On a bar
    // that reads as quiet at the bottom and loud at the top.
    let quiet = vec3<f32>(0.55, 0.30, 0.95);
    let loud = vec3<f32>(0.15, 0.95, 0.55);
    let cap = vec3<f32>(0.93, 0.96, 1.0);
    let colour = mix(mix(quiet, loud, clamp(in.height, 0.0, 1.0)), cap, in.cap);
    return vec4<f32>(to_target(colour * in.shade, scene.flags.x), 1.0);
}

struct LineOut {
    @builtin(position) clip_position: vec4<f32>,
};

/// The octave marks, in the same normalised scene coordinates the instances
/// use, so one uniform describes both.
@vertex
fn vs_line(@location(0) position: vec3<f32>) -> LineOut {
    var out: LineOut;
    let world = vec3<f32>(
        position.x * scene.dims.x,
        position.y * scene.dims.y - scene.dims.y * 0.5,
        position.z * scene.dims.z,
    );
    out.clip_position = scene.view_proj * vec4<f32>(world, 1.0);
    return out;
}

@fragment
fn fs_line() -> @location(0) vec4<f32> {
    // Dim enough to be a reference rather than a feature: these are read out of
    // the corner of the eye while the bars are what is being watched.
    return vec4<f32>(to_target(vec3<f32>(0.30, 0.26, 0.44), scene.flags.x), 1.0);
}
