// The 3D vectorscope (Phase 1 M6). One vertex per decimated sample, drawn as
// a line strip through space: x and y are the stereo field (side and mid, the
// goniometer's own axes) and z is how long ago the sample played.

struct Camera {
    view_proj: mat4x4<f32>,
    // [figure_scale, depth, 0, 0] -- see vectorscope.rs. How large to draw a
    // given amplitude, and how long to draw the fixed 120ms, are both display
    // decisions; they arrive from Rust because the camera there is *fitted* to
    // them, and a copy here that drifted would frame a different scene than
    // this shader draws.
    figure: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) age: f32,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> VertexOutput {
    var out: VertexOutput;
    let age = position.z;
    // Centred on the origin so the camera looks at the middle of the ribbon,
    // with the newest sample nearest the viewer -- the trace grows towards you
    // and recedes as it ages, rather than the other way round.
    let world = vec3<f32>(
        position.x * camera.figure.x,
        position.y * camera.figure.x,
        (0.5 - age) * camera.figure.y,
    );
    out.clip_position = camera.view_proj * vec4<f32>(world, 1.0);
    out.age = age;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Phosphor green at the near end, falling away to the brand's violet as it
    // recedes. The fade is what makes the depth readable on a still frame:
    // without it the extrusion reads as a tangle of overlapping lines.
    let now = vec3<f32>(0.15, 0.95, 0.55);
    let past = vec3<f32>(0.55, 0.30, 0.95);
    let colour = mix(now, past, in.age);
    let brightness = 0.22 + 0.78 * (1.0 - in.age);
    return vec4<f32>(colour * brightness, 1.0);
}
