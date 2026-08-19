// Spectrogram waterfall (Phase 3 M3). The metering spectrum's last few seconds
// as a terrain: frequency across, level up, time into the screen with the
// newest row at the front. `shared.wgsl` is prepended by
// `attachments::shader_source`.
//
// The history is a ring of rows in a texture (decision 16) and this shader
// reads it directly -- there is no vertex buffer of heights, and scrolling is
// the `head` index below moving rather than anything being copied.

struct Scene {
    view_proj: mat4x4<f32>,
    // [span, height, depth, rail_overhang] in world units -- see waterfall.rs.
    // The camera there is *fitted* to the first three, so these arrive from
    // Rust rather than being repeated here.
    dims: vec4<f32>,
    // [srgb_target, bands, rows, head]. `bands` is the frame's to say and
    // `head` is which texture row holds the newest measurement.
    grid: vec4<f32>,
    // [range_db, gain_db, 0, 0] -- the display window and the display gain,
    // applied here rather than on the way in (decision 18) so that changing
    // either re-maps the *whole* terrain instead of leaving a seam at the row
    // where the setting changed.
    scale: vec4<f32>,
};

@group(0) @binding(0) var<uniform> scene: Scene;
// dBFS as measured, one texel per band per row. Sampled with `textureLoad`
// rather than a sampler: every vertex sits exactly on one texel, so there is
// no filtering question to answer and no sampler to bind.
@group(0) @binding(1) var history: texture_2d<f32>;

/// Where the light comes from, in world space -- the bars' own light, from the
/// side the camera is on, so the two spectrum visuals are lit alike.
const LIGHT: vec3<f32> = vec3<f32>(-0.45, 0.82, 0.36);

/// The dBFS at (band, age), where age 0 is the newest row. Clamped rather than
/// wrapped at both edges: a slope computed at the edge of the display should
/// lean on the edge itself, not on the far side of the spectrum or on the
/// oldest row.
fn level_at(band: i32, age: i32) -> f32 {
    let bands = i32(scene.grid.y);
    let rows = i32(scene.grid.z);
    let head = i32(scene.grid.w);
    let b = clamp(band, 0, max(bands - 1, 0));
    let j = clamp(age, 0, max(rows - 1, 0));
    // The ring, read backwards from the newest row.
    let row = (head + rows - j) % rows;
    return textureLoad(history, vec2<i32>(b, row), 0).r;
}

/// That level as a height, 0 at the bottom of the display window and 1 at full
/// scale -- `scale::normalise` and `scale::gain_db`, in the shader, because
/// this is where a stored measurement becomes a picture.
fn height_at(band: i32, age: i32) -> f32 {
    let db = level_at(band, age);
    return clamp((db + scene.scale.y) / scene.scale.x + 1.0, 0.0, 1.0);
}

/// Grid position to world space. The scene is centred on the origin, which is
/// the box the camera was fitted to, so the floor is at -height/2.
fn world_at(band: f32, age: f32, height: f32) -> vec3<f32> {
    let bands = max(scene.grid.y, 1.0);
    let rows = max(scene.grid.z - 1.0, 1.0);
    return vec3<f32>(
        ((band + 0.5) / bands - 0.5) * scene.dims.x,
        height * scene.dims.y - scene.dims.y * 0.5,
        (0.5 - age / rows) * scene.dims.z,
    );
}

struct SurfaceOut {
    @builtin(position) clip_position: vec4<f32>,
    // 0 at the display's floor, 1 at full scale -- the colour ramp's input, so
    // colour reads as level exactly as it does on the bars.
    @location(0) height: f32,
    @location(1) shade: f32,
    // 0 for the newest row, 1 for the oldest: how far into the past this
    // fragment is, which is what dims it.
    @location(2) age: f32,
};

/// The terrain, drawn as one indexed grid. `vertex_index` is the grid index
/// itself -- band across, age down -- so no vertex buffer exists to be kept in
/// step with the history.
@vertex
fn vs_terrain(@builtin(vertex_index) vertex_index: u32) -> SurfaceOut {
    let bands = max(u32(scene.grid.y), 1u);
    let band = i32(vertex_index % bands);
    let age = i32(vertex_index / bands);
    let height = height_at(band, age);

    // The surface normal from the four neighbours, in world units -- what makes
    // a ridge read as a ridge rather than as a coloured stripe.
    let dx = scene.dims.x / max(scene.grid.y, 1.0);
    let dz = scene.dims.z / max(scene.grid.z - 1.0, 1.0);
    let slope_x = (height_at(band + 1, age) - height_at(band - 1, age)) * 0.5 * scene.dims.y;
    let slope_z = (height_at(band, age - 1) - height_at(band, age + 1)) * 0.5 * scene.dims.y;
    let along_x = vec3<f32>(dx, slope_x, 0.0);
    let along_z = vec3<f32>(0.0, slope_z, dz);

    var out: SurfaceOut;
    out.clip_position = scene.view_proj * vec4<f32>(world_at(f32(band), f32(age), height), 1.0);
    out.height = height;
    out.shade = 0.42 + 0.58 * max(dot(normalize(cross(along_z, along_x)), normalize(LIGHT)), 0.0);
    out.age = f32(age) / max(scene.grid.z - 1.0, 1.0);
    return out;
}

/// The near edge, dropped to the floor: the cross-section of the newest row.
///
/// Not decoration. Without it the terrain is a sheet with a paper edge at the
/// front, and -- the reason it earns its own pipeline -- the very first
/// measurement of a session draws a whole profile rather than nothing, because
/// a surface needs two rows before it has a triangle.
@vertex
fn vs_skirt(@builtin(vertex_index) vertex_index: u32) -> SurfaceOut {
    let band = i32(vertex_index / 2u);
    let top = (vertex_index % 2u) == 0u;
    let height = height_at(band, 0);

    var out: SurfaceOut;
    out.clip_position = scene.view_proj
        * vec4<f32>(world_at(f32(band), 0.0, select(0.0, height, top)), 1.0);
    out.height = height;
    // A flat face towards the viewer, lit as one.
    out.shade = 0.42 + 0.58 * max(dot(vec3<f32>(0.0, 0.0, 1.0), normalize(LIGHT)), 0.0);
    out.age = 0.0;
    return out;
}

@fragment
fn fs_surface(in: SurfaceOut) -> @location(0) vec4<f32> {
    // The bars' colours, meaning what they mean there: violet is quiet, green
    // is loud. A person switching between the two spectrum visuals should see
    // the same picture with its past behind it.
    let quiet = vec3<f32>(0.55, 0.30, 0.95);
    let loud = vec3<f32>(0.15, 0.95, 0.55);
    let colour = mix(quiet, loud, clamp(in.height, 0.0, 1.0));
    // Older is dimmer, the vectorscope's own depth cue: perspective alone is a
    // weak sense of distance on a surface with no texture.
    let distance = 1.0 - 0.55 * clamp(in.age, 0.0, 1.0);
    return vec4<f32>(to_target(colour * in.shade * distance, scene.grid.x), 1.0);
}

struct RailOut {
    @builtin(position) clip_position: vec4<f32>,
};

/// The octave rails, in the same normalised coordinates the grid uses.
///
/// They run along the floor under the terrain and out past its near edge by
/// `dims.w`, which is the part that is actually seen: a terrain at rest covers
/// its whole floor, so a mark lying on it would be hidden everywhere the bars'
/// equivalent is visible between two bars. What is left is a frequency ruler at
/// the front, which is what a waterfall wants anyway.
@vertex
fn vs_rail(@location(0) position: vec3<f32>) -> RailOut {
    var out: RailOut;
    out.clip_position = scene.view_proj * vec4<f32>(
        position.x * scene.dims.x,
        position.y * scene.dims.y - scene.dims.y * 0.5,
        position.z * scene.dims.z,
        1.0,
    );
    return out;
}

@fragment
fn fs_rail() -> @location(0) vec4<f32> {
    // The bars' gridline colour, for the bars' reason: read out of the corner
    // of the eye while the music is what is being watched.
    return vec4<f32>(to_target(vec3<f32>(0.30, 0.26, 0.44), scene.grid.x), 1.0);
}
