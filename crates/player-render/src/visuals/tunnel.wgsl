// Oscilloscope tunnel (Phase 3 M4). The waveform wrapped around a circle, one
// ring per clock tick, laid along a curving path the eye flies down.
// `shared.wgsl` is prepended by `attachments::shader_source`.
//
// Two things are deliberately *not* here. The path is not evaluated in this
// shader -- `scene.centres` arrives already computed, so the camera and the
// geometry are the same Rust function rather than two implementations that
// agree until one is edited (M4 decision 25). And there is no vertex buffer:
// `vertex_index` is the (ring, point) pair, and the waveform comes out of the
// history texture, the same arrangement the waterfall uses.

struct Scene {
    view_proj: mat4x4<f32>,
    // [base_radius, wave_depth, min_scale, max_scale]. `wave_depth` arrives with
    // display gain already in it: this visual draws samples, so gain is an
    // amplitude multiplier here rather than the decibel offset the two spectrum
    // displays use (decision 28).
    shape: vec4<f32>,
    // [srgb_target, points, filled, head]
    grid: vec4<f32>,
    // [background r, g, b, fade_rings]
    fog: vec4<f32>,
    // [twist, rings, 0, 0]
    extra: vec4<f32>,
    // Where each ring's centre is, *relative to the eye*, with `w` carrying how
    // many rings ahead it is. Relative because the tunnel is endless and world
    // coordinates that grow without bound lose their precision exactly where
    // the detail is.
    centres: array<vec4<f32>, 240>,
};

@group(0) @binding(0) var<uniform> scene: Scene;
// Two channels per texel: mid displaces the wall, side twists the point round
// the ring (decision 29). Read with `textureLoad` -- every vertex sits on
// exactly one texel, so there is no filtering question and no sampler.
@group(0) @binding(1) var history: texture_2d<f32>;

/// The family's light, from the side the other visuals are lit from.
const LIGHT: vec3<f32> = vec3<f32>(-0.45, 0.82, 0.36);
const TAU: f32 = 6.28318530718;

/// Which texture row holds the ring of age `age`, 0 being the newest.
fn ring_row(age: u32) -> i32 {
    let rings = u32(scene.extra.y);
    let head = u32(scene.grid.w);
    return i32((head + rings - min(age, rings - 1u)) % rings);
}

struct TubeOut {
    @builtin(position) clip_position: vec4<f32>,
    // How far this point pushes the wall out or in, before the clamp -- the
    // colour ramp's input, so colour reads as level.
    @location(0) push: f32,
    @location(1) shade: f32,
    // How far ahead this ring is, in rings: what the fog fades with.
    @location(2) ahead: f32,
};

@vertex
fn vs_tube(@builtin(vertex_index) vertex_index: u32) -> TubeOut {
    let points = max(u32(scene.grid.y), 1u);
    let age = vertex_index / points;
    let point = vertex_index % points;

    let texel = textureLoad(history, vec2<i32>(i32(point), ring_row(age)), 0);
    let mid = texel.r;
    let side = texel.g;

    // The waveform runs round the ring, and the stereo field twists it: a mono
    // signal draws an even circle, a wide one corkscrews. Per sample rather
    // than per ring, because a per-ring twist would be a *statistic* over the
    // window -- a measurement, which a visual does not make (decision 2).
    let angle = TAU * f32(point) / f32(points) + scene.extra.x * side;
    // Decision 28: the wall may never reach the axis. Full scale at x8 would
    // otherwise close the tube around the eye, which draws as a screenful of
    // flat colour and reads as a bug rather than as loud music.
    let scale = clamp(1.0 + scene.shape.y * mid, scene.shape.z, scene.shape.w);
    let radius = scene.shape.x * scale;

    let centre = scene.centres[age];
    let around = vec3<f32>(cos(angle), sin(angle), 0.0);
    let world = centre.xyz + around * radius;

    var out: TubeOut;
    out.clip_position = scene.view_proj * vec4<f32>(world, 1.0);
    out.push = scene.shape.y * mid;
    // The eye is *inside*, so the face that can be seen points at the axis.
    out.shade = 0.42 + 0.58 * max(dot(-around, normalize(LIGHT)), 0.0);
    out.ahead = centre.w;
    return out;
}

@fragment
fn fs_tube(in: TubeOut) -> @location(0) vec4<f32> {
    // The family's two colours, meaning here what they mean on the bars and the
    // waterfall: violet is quiet, green is loud.
    let quiet = vec3<f32>(0.55, 0.30, 0.95);
    let loud = vec3<f32>(0.15, 0.95, 0.55);
    let colour = mix(quiet, loud, clamp(abs(in.push) * 1.6, 0.0, 1.0));
    // Fading to the background rather than to black: the far end of a tunnel is
    // an opening, and an opening that faded to black would be a hole cut in a
    // page rather than distance.
    let distance = clamp(in.ahead / max(scene.fog.w, 1.0), 0.0, 1.0);
    let lit = mix(colour * in.shade, scene.fog.rgb, distance * distance);
    return vec4<f32>(to_target(lit, scene.grid.x), 1.0);
}
