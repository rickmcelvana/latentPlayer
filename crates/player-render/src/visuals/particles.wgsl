// Particle field (Phase 3 M6): the first compute shader in this workspace, and
// the first scene here whose state is advanced by arithmetic rather than
// replaced by a measurement.
//
// Three pipelines, one module:
//
//   sim          -- compute: spawn, age, integrate. One dispatch per fixed
//                   step, never per frame (decision 42).
//   vs/fs_spark  -- one additive quad per particle, drawn into an HDR
//                   accumulation texture rather than straight at the target.
//   vs/fs_present -- tone-maps that accumulation onto the background.
//
// Why the accumulation texture exists is worth stating, because it looks like
// an extravagance and is a correctness fix. Additive blending happens *after*
// the fragment shader, in whatever space the target stores: an sRGB surface
// decodes, adds and re-encodes, while the web's `bgra8unorm` canvas adds the
// stored bytes as they are. One shader, two answers -- the same bug shape that
// gave the two shells different backgrounds on 2026-08-18, except that no
// single constant could fix this one. So the sparks accumulate in linear light
// at `Rgba16Float`, where both shells add identically, and exactly one pass
// converts the result for the surface in hand.

struct Particle {
    // Position in world units, and how many seconds of life are left. A life at
    // or below zero is a slot waiting to be spawned into, and draws nothing.
    position: vec3<f32>,
    life: f32,
    velocity: vec3<f32>,
    // 0..1 around the colour wheel, chosen from the spectral centroid at the
    // moment this particle was spawned -- so a field carries the brightness of
    // the music that threw it, not of the music playing now.
    hue: f32,
}

struct Sim {
    // `dt` is one fixed step, never a frame's elapsed time.
    // `energy` is the onset envelope of decision 43, `kick` the beat pulse
    // already scaled by confidence (decision 45), `level` the peak amplitude.
    step: vec4<f32>,
    // `[spawn_begin, spawn_end, particle_count, 0]` -- spawn ordinals rather
    // than slot indices, so a slot knows which *generation* of itself it is.
    spawn: vec4<u32>,
    // `[swirl, pull, drag, kick_strength]`.
    motion: vec4<f32>,
    // `[outward_speed, radius, thickness, orbit_speed]`.
    shape: vec4<f32>,
    // `[life_min, life_span, 0, 0]`.
    life: vec4<f32>,
    // `[hue, hue_spread, max_radius, 0]`.
    colour: vec4<f32>,
}

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var<uniform> sim: Sim;

// Decision 44: a particle is a pure function of its identity, so both shells
// compute the same field and a check can assert a position rather than a
// histogram. PCG's output permutation -- cheap, and its low bits are as good as
// its high ones, which the usual multiply-shift hashes cannot say.
fn hash(value: u32) -> u32 {
    var state = value * 747796405u + 2891336453u;
    state = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (state >> 22u) ^ state;
}

fn random(seed: u32) -> f32 {
    return f32(hash(seed)) / 4294967296.0;
}

@compute @workgroup_size(64)
fn sim_step(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    let count = sim.spawn.z;
    if (index >= count) {
        return;
    }
    var particle = particles[index];

    let dt = sim.step.x;
    let energy = sim.step.y;

    // Spawning, without an atomic and without a queue: particles are spawned in
    // ordinal order, so the slot for ordinal k is k % count and the generation
    // is k / count. The smallest ordinal at or after `begin` that lands on this
    // slot is one modulo away, and if it falls inside this step's range the slot
    // is (re)spawned. Exactly the number of particles the host asked for are
    // spawned, in a defined order, from arithmetic every invocation can do
    // alone.
    let begin = sim.spawn.x;
    let end = sim.spawn.y;
    let ordinal = begin + ((index + count) - (begin % count)) % count;
    if (ordinal < end) {
        let seed = ordinal * 3u;
        let around = random(seed) * 6.2831853;
        let thickness = (random(seed + 1u) - 0.5) * 2.0;
        let spread = random(seed + 2u);

        // Born on a disc rather than on a sphere, which is the difference
        // between a swirl you can see and a ball that happens to be rotating.
        // The thickness is what stops it reading as a flat plate, and the eye
        // sits above it (see `EYE_DIRECTION`) so the disc is an ellipse.
        let outward = vec3<f32>(cos(around), 0.0, sin(around));
        let tangent = vec3<f32>(-sin(around), 0.0, cos(around));
        let height = thickness * sim.shape.z;

        particle.position = outward * sim.shape.y * (0.15 + 0.85 * spread)
            + vec3<f32>(0.0, height, 0.0);
        // Mostly *along* the orbit, with the onset's share thrown outward: a
        // transient is an amount of movement, not a brightness multiplier
        // painted over the same picture.
        particle.velocity = tangent * sim.shape.w * (0.8 + 0.4 * spread)
            + outward * sim.shape.x * (0.25 + energy) * (0.6 + 0.8 * spread)
            + vec3<f32>(0.0, height * 0.5, 0.0);
        particle.life = sim.life.x + sim.life.y * spread;
        particle.hue = fract(sim.colour.x + (spread - 0.5) * sim.colour.y + 1.0);
    }

    if (particle.life > 0.0) {
        let position = particle.position;
        let radius = length(position);
        let outward = select(vec3<f32>(0.0, 1.0, 0.0), position / max(radius, 1e-6), radius > 1e-6);
        // A swirl about the vertical, faster nearer the middle, and a spring
        // back towards it: together an orbit rather than an explosion or a
        // drain. The beat pushes outward against the spring, which is what
        // makes a pulse read as the field breathing rather than as it flashing.
        let sideways = cross(vec3<f32>(0.0, 1.0, 0.0), outward);
        let swirl = sideways * sim.motion.x / (0.45 + radius);
        let spring = -outward * sim.motion.y * radius;
        let beat = outward * sim.motion.w * sim.step.z;

        var velocity = particle.velocity + (swirl + spring + beat) * dt;
        // Exponential drag, so the settling time is in seconds and does not
        // depend on how the frame was chunked.
        velocity = velocity * exp(-sim.motion.z * dt);
        var moved = position + velocity * dt;

        // The field has an edge, and it absorbs rather than reflects: a
        // particle that reaches it stops travelling outward instead of bouncing
        // back through the middle, which would read as a wall.
        let reach = length(moved);
        let limit = sim.colour.z;
        if (reach > limit) {
            moved = moved * (limit / reach);
            velocity = velocity - outward * dot(velocity, outward);
        }

        particle.position = moved;
        particle.velocity = velocity;
        particle.life = particle.life - dt;
    }

    particles[index] = particle;
}

struct Scene {
    view_proj: mat4x4<f32>,
    // The camera's own axes, for billboarding: a quad built from these always
    // faces the eye, whatever the projection does.
    right: vec4<f32>,
    up: vec4<f32>,
    // `[size, brightness, life_span, srgb_target]`.
    params: vec4<f32>,
}

@group(0) @binding(2) var<uniform> scene: Scene;

struct Spark {
    @builtin(position) clip: vec4<f32>,
    @location(0) offset: vec2<f32>,
    @location(1) colour: vec3<f32>,
};

fn hue_to_rgb(hue: f32) -> vec3<f32> {
    // A full-saturation wheel, written out rather than pulled through an HSV
    // conversion nobody here needs the other two thirds of.
    let h = fract(hue) * 6.0;
    let x = 1.0 - abs(h % 2.0 - 1.0);
    if (h < 1.0) { return vec3<f32>(1.0, x, 0.0); }
    if (h < 2.0) { return vec3<f32>(x, 1.0, 0.0); }
    if (h < 3.0) { return vec3<f32>(0.0, 1.0, x); }
    if (h < 4.0) { return vec3<f32>(0.0, x, 1.0); }
    if (h < 5.0) { return vec3<f32>(x, 0.0, 1.0); }
    return vec3<f32>(1.0, 0.0, x);
}

@vertex
fn vs_spark(
    @builtin(vertex_index) vertex: u32,
    @location(0) position_life: vec4<f32>,
    @location(1) velocity_hue: vec4<f32>,
) -> Spark {
    var out: Spark;
    let life = position_life.w;
    if (life <= 0.0) {
        // A dead slot is collapsed to a degenerate point behind the eye rather
        // than skipped -- an instanced draw has no way to skip one, and this
        // costs nothing a discarded fragment would not.
        out.clip = vec4<f32>(0.0, 0.0, -1.0, 1.0);
        out.offset = vec2<f32>(0.0, 0.0);
        out.colour = vec3<f32>(0.0, 0.0, 0.0);
        return out;
    }

    // Two triangles from six vertices, as corner offsets in camera space.
    var corner = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let offset = corner[vertex % 6u];

    // Young particles are brighter and slightly larger, and both fade to
    // nothing by the end of a life -- the decay lives in the *visual*, as the
    // bars' falling caps do, because the analysis publishes what it measured
    // rather than what looks good falling.
    // Full brightness for most of a life and a fall at the end of it: a spark
    // that dimmed from the instant it appeared would make every onset a flash
    // that was already over. The decay lives here, in the visual, as the bars'
    // falling caps do -- the analysis publishes what it measured.
    let fade = clamp(life / 0.6, 0.0, 1.0);
    let size = scene.params.x * (0.6 + 0.5 * fade);

    let centre = position_life.xyz;
    let world = centre + (scene.right.xyz * offset.x + scene.up.xyz * offset.y) * size;
    out.clip = scene.view_proj * vec4<f32>(world, 1.0);
    out.offset = offset;
    // Dimmer further out, as the same light spread over more room: a flat
    // field of equally bright sparks reads as noise, where a core that glows
    // reads as a thing with a middle.
    let reach = length(centre);
    let depth = 1.0 / (1.0 + 0.30 * reach * reach);
    out.colour = hue_to_rgb(velocity_hue.w) * scene.params.y * fade * depth;
    return out;
}

@fragment
fn fs_spark(spark: Spark) -> @location(0) vec4<f32> {
    // A round, soft spark: squared falloff to zero at the quad's inscribed
    // circle, so the quads themselves are never visible as squares.
    let radius = dot(spark.offset, spark.offset);
    if (radius > 1.0) {
        discard;
    }
    let falloff = (1.0 - radius) * (1.0 - radius);
    // Linear light, additively blended -- no colour conversion here. That is
    // `fs_present`'s single job, done once on the sum.
    return vec4<f32>(spark.colour * falloff, 1.0);
}

@group(0) @binding(3) var accumulation: texture_2d<f32>;

struct Present {
    @builtin(position) clip: vec4<f32>,
};

@vertex
fn vs_present(@builtin(vertex_index) vertex: u32) -> Present {
    // One triangle covering the viewport, from the vertex index alone.
    var corner = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    var out: Present;
    out.clip = vec4<f32>(corner[vertex], 0.0, 1.0);
    return out;
}

// `[exposure, srgb_target, background r, 0]` plus the rest of the background --
// small enough to be two vectors and clearer than a struct with one field.
struct Composite {
    settings: vec4<f32>,
    background: vec4<f32>,
}

@group(0) @binding(4) var<uniform> composite: Composite;

@fragment
fn fs_present(in: Present) -> @location(0) vec4<f32> {
    let accumulated = textureLoad(accumulation, vec2<i32>(in.clip.xy), 0).rgb;
    // A soft shoulder rather than a clamp: a hundred overlapping sparks
    // approach white instead of arriving there and staying, which is the
    // difference between a dense passage looking loud and looking broken.
    let mapped = vec3<f32>(1.0) - exp(-accumulated * composite.settings.x);
    let shown = composite.background.rgb + mapped * (1.0 - composite.background.rgb);
    return vec4<f32>(to_target(shown, composite.settings.y), 1.0);
}
