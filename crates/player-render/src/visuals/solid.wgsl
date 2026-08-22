// Raymarched solid (Phase 3 M6): the analyser's own bands as the shape of a
// surface, pulsed on the beat and coloured by the brightness of the moment.
//
// Two pipelines: `fs_march` raymarches into an offscreen texture, `fs_upscale`
// puts that texture on the surface. The split is decision 47 -- a fullscreen
// raymarch is priced in fragments, so the number of fragments is this visual's
// own business rather than the window's.
//
// The distance field is a sphere whose radius depends on where you are on it:
//
//   radius(direction) = base * pulse + displace * level(latitude) + ripple
//
// `level(latitude)` is the spectrum -- low frequencies at the south pole, high
// at the north, placed by real frequency before they ever reach this shader
// (see `solid.rs`, which resamples the frame's own bands onto the axis). So the
// silhouette *is* the spectrum, read around the outline from bottom to top, and
// a bass-heavy passage is a shape a person can point at rather than a colour
// that happened to change.
//
// That radius is not a true distance field -- a displaced sphere never is -- so
// the march takes short steps (`STEP_SCALE`) rather than trusting the estimate
// it is given. The alternative is an overshoot that eats the surface at glancing
// angles, which looks like holes in the solid.

struct Scene {
    // The camera, as a ray generator: where the eye is, and the three axes a
    // pixel's direction is built from.
    eye: vec4<f32>,
    forward: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    // `[tan_half_fov, aspect, srgb_target, seconds]`.
    view: vec4<f32>,
    // `[base_radius, displace, pulse, ripple]` -- the last two already scaled by
    // confidence and by the onset envelope in `solid.rs`, so this shader
    // believes what it is given and decides nothing about the music.
    shape: vec4<f32>,
    // `[hue, bands, has_spectrum, glow]`.
    colour: vec4<f32>,
}

@group(0) @binding(0) var<uniform> scene: Scene;
// The spectrum, resampled onto a fixed log-frequency axis: 0 is the lowest
// frequency the display covers and `bands - 1` the highest, each already
// normalised to 0..1 across the display window.
@group(0) @binding(1) var levels: texture_2d<f32>;

const MAX_STEPS: i32 = 128;
// Well under one, because a displaced sphere's radius is not a distance: the
// estimate is only exact along a ray through the middle, and at the silhouette
// -- where the spectrum is read -- it overshoots straight through the surface.
// Measured cheap enough to be generous with (see `MAX_MARCH_PIXELS`).
const STEP_SCALE: f32 = 0.35;
const SURFACE: f32 = 0.002;
const MAX_DISTANCE: f32 = 12.0;

/// The level at a latitude, from the south pole (0) to the north (1),
/// interpolated between the two bands either side of it.
fn level_at(latitude: f32) -> f32 {
    let bands = scene.colour.y;
    if (bands < 2.0) {
        return 0.0;
    }
    let position = clamp(latitude, 0.0, 1.0) * (bands - 1.0);
    let low = floor(position);
    // Smoothstep rather than linear between two bands: a linear blend has a
    // corner at every band centre, and a corner in the *radius* is a crease the
    // normal reads as an edge all the way around the solid.
    let fraction = smoothstep(0.0, 1.0, position - low);
    let a = textureLoad(levels, vec2<i32>(i32(low), 0), 0).r;
    let b = textureLoad(levels, vec2<i32>(i32(min(low + 1.0, bands - 1.0)), 0), 0).r;
    return mix(a, b, fraction);
}

/// The radius of the solid in a given direction.
fn radius_towards(direction: vec3<f32>) -> f32 {
    // Latitude from the y component: the poles are the ends of the spectrum, so
    // the outline from bottom to top is the display from low to high.
    let latitude = clamp(direction.y * 0.5 + 0.5, 0.0, 1.0);
    let level = level_at(latitude);

    // A ripple *around* the solid, whose amplitude is the onset envelope: the
    // surface is smooth when nothing percussive is happening and breaks up when
    // it is. Its shape is arbitrary; that there is any of it is a measurement.
    let around = atan2(direction.z, direction.x);
    let ripple = sin(around * 5.0 + scene.view.w * 1.7) * sin(latitude * 9.0 - scene.view.w * 1.1);

    return scene.shape.x * scene.shape.z
        + scene.shape.y * level
        + scene.shape.w * ripple;
}

fn distance_to(point: vec3<f32>) -> f32 {
    let length_of = length(point);
    if (length_of < 1e-4) {
        return -scene.shape.x;
    }
    return length_of - radius_towards(point / length_of);
}

fn normal_at(point: vec3<f32>) -> vec3<f32> {
    // Central differences, at a step wide enough that the ripple is a shape
    // rather than noise the normal chases.
    let e = 0.004;
    return normalize(vec3<f32>(
        distance_to(point + vec3<f32>(e, 0.0, 0.0)) - distance_to(point - vec3<f32>(e, 0.0, 0.0)),
        distance_to(point + vec3<f32>(0.0, e, 0.0)) - distance_to(point - vec3<f32>(0.0, e, 0.0)),
        distance_to(point + vec3<f32>(0.0, 0.0, e)) - distance_to(point - vec3<f32>(0.0, 0.0, e)),
    ));
}

struct March {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vertex: u32) -> March {
    var corner = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    var out: March;
    let position = corner[vertex];
    out.clip = vec4<f32>(position, 0.0, 1.0);
    out.uv = position;
    return out;
}

@fragment
fn fs_march(in: March) -> @location(0) vec4<f32> {
    // Nothing measured is nothing drawn -- the bars' answer to a frame with no
    // spectrum in it (M2 decision 10), which is a different picture from a
    // sphere sitting at the display floor.
    if (scene.colour.z < 0.5) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let direction = normalize(
        scene.forward.xyz
        + scene.right.xyz * in.uv.x * scene.view.x * scene.view.y
        + scene.up.xyz * in.uv.y * scene.view.x
    );
    let eye = scene.eye.xyz;

    var travelled = 0.0;
    var hit = false;
    for (var step = 0; step < MAX_STEPS; step = step + 1) {
        let point = eye + direction * travelled;
        let distance = distance_to(point);
        if (distance < SURFACE) {
            hit = true;
            break;
        }
        travelled = travelled + distance * STEP_SCALE;
        if (travelled > MAX_DISTANCE) {
            break;
        }
    }

    if (!hit) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let point = eye + direction * travelled;
    let normal = normal_at(point);

    // Where on the spectrum this piece of the surface is, and how loud that
    // band is -- the same two numbers the shape came from, used again for the
    // light. A loud band is both further out *and* brighter, which is what makes
    // the display readable at a glance rather than only in silhouette.
    let latitude = clamp(normalize(point).y * 0.5 + 0.5, 0.0, 1.0);
    let level = level_at(latitude);
    // A gentle hue gradient along the axis on top of the centroid's own colour,
    // so the bass end and the treble end are told apart at rest. Small on
    // purpose: the colour of the *moment* is the measurement, and this is a
    // ruler drawn over it.
    let base = hue_to_rgb(scene.colour.x + (latitude - 0.5) * 0.07);

    // One key light over the viewer's shoulder and a cool fill from below, so
    // the displacement reads as relief rather than as a flat stencil.
    let key = normalize(vec3<f32>(0.45, 0.8, 0.6));
    let fill = normalize(vec3<f32>(-0.5, -0.4, 0.2));
    let lit = max(dot(normal, key), 0.0);
    let filled = max(dot(normal, fill), 0.0);

    // A rim that follows the silhouette, which is where the spectrum is
    // actually legible -- the outline is the display.
    let facing = 1.0 - max(dot(normal, -direction), 0.0);
    let rim = pow(facing, 2.5);

    // A specular lobe, because a surface with only diffuse shading on it reads
    // as a silhouette rather than as a solid -- and the whole claim of this
    // visual is that the spectrum has a *shape*.
    let halfway = normalize(key - direction);
    let specular = pow(max(dot(normal, halfway), 0.0), 24.0);

    // The body is desaturated towards its own light: a fully saturated hue
    // across a large area is a flat field of one colour whatever the geometry
    // does, which is what the first render of this looked like.
    let body = mix(base, vec3<f32>(1.0, 1.0, 1.0), 0.22);
    let colour = body * (0.10 + 0.52 * lit)
        + base * filled * 0.10
        + base * level * 0.22
        + vec3<f32>(1.0, 1.0, 1.0) * specular * 0.25
        + hue_to_rgb(scene.colour.x + 0.08) * rim * scene.colour.w;
    return vec4<f32>(colour, 1.0);
}

@group(0) @binding(2) var marched: texture_2d<f32>;
@group(0) @binding(3) var marched_sampler: sampler;
// `[srgb_target, background r, g, b]`.
@group(0) @binding(4) var<uniform> present: vec4<f32>;

@fragment
fn fs_upscale(in: March) -> @location(0) vec4<f32> {
    // The march's own texture is sampled rather than loaded: it may be smaller
    // than the surface (decision 47), and a linear filter is the whole reason
    // that is allowed to be true.
    let uv = in.uv * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let marched_colour = textureSampleLevel(marched, marched_sampler, uv, 0.0).rgb;
    // A soft shoulder rather than a clamp, the particle field's rule: the lit
    // side of a loud band and its specular highlight add past one, and clipping
    // them turns the brightest part of the display -- which is the loudest part
    // of the music -- into a flat white patch with no shape in it.
    let mapped = vec3<f32>(1.0) - exp(-marched_colour);
    let shown = present.yzw + mapped * (1.0 - present.yzw);
    return vec4<f32>(to_target(shown, present.x), 1.0);
}
