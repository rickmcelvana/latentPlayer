struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    // Phosphor-green trace -- placeholder color for the 2D walking skeleton;
    // the real 3D vectorscope (PLAN.md §4) gets a real design pass later.
    return vec4<f32>(0.15, 0.95, 0.55, 1.0);
}
