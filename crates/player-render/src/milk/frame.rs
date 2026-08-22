//! MilkDrop's frame, as wgpu sees it -- the third leg of M7's spike.
//!
//! A MilkDrop frame is not a scene that gets redrawn. It is a **feedback
//! loop**: the previous frame is resampled through a warped mesh into the new
//! one, things are drawn on top, and only then is the result composited to the
//! screen. Everything a preset does visually is a consequence of that -- the
//! trails, the tunnels, the endless zoom -- so the loop is the piece that has
//! to be right before any preset's own code matters.
//!
//! What this proves, and only this: two colour targets ping-ponged across
//! frames, a warp mesh whose source coordinates are computed per vertex on the
//! CPU (which is where MilkDrop computes them, since `per_pixel` is EEL), and a
//! composite pass into a caller's view. It renders offscreen and its result is
//! read back, per decision 6's standing rule that a renderer verified by
//! looking at it is a renderer that ships mirrored.
//!
//! What this deliberately does **not** do, and what M7 has to:
//!
//! - **The warp transform here is a reduced one** (zoom, rotate, scale, centre,
//!   translate). MilkDrop's full per-vertex transform adds `zoomexp`'s
//!   radius-dependent exponent and four `warp` sine terms whose exact
//!   arguments and ordering matter to how thousands of presets look. Those get
//!   taken from MilkDrop's own source rather than from anyone's memory,
//!   per CONVENTIONS.md -- and getting them subtly wrong is invisible in a
//!   still frame and obvious in motion, which is why they are not guessed here.
//! - **No blur chain, no motion vectors, no waves or shapes drawn on top, and
//!   no translated preset shaders.** The warp and composite passes run fixed
//!   WGSL written here. Translating a preset's HLSL is the milestone's other
//!   large piece and is sized separately (`hlsl.rs`).

use crate::milk::eel;

/// The colour format the feedback pair uses.
///
/// 16-bit float rather than 8-bit unorm, and this is not a luxury: a feedback
/// loop multiplies its own output by a decay every frame, so 8 bits of
/// precision quantises to a visible staircase within a second and dark trails
/// snap to black. It is also what lets a preset's `ret` exceed 1.0 mid-chain
/// before the composite brings it back.
pub const FEEDBACK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// One mesh vertex's warp: where on the previous frame this grid point reads.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WarpVertex {
    /// Grid position in clip space, -1..1.
    pub position: [f32; 2],
    /// Where to sample the previous frame, in texture space 0..1.
    pub source: [f32; 2],
}

/// The per-vertex quantities MilkDrop's `per_pixel` block produces.
///
/// Named here so the spike's transform and the milestone's real one take the
/// same input; only the arithmetic between this and a [`WarpVertex`] is
/// reduced (see the module note).
#[derive(Clone, Copy, Debug)]
pub struct WarpParams {
    pub zoom: f32,
    pub rot: f32,
    pub cx: f32,
    pub cy: f32,
    pub dx: f32,
    pub dy: f32,
    pub sx: f32,
    pub sy: f32,
}

impl Default for WarpParams {
    fn default() -> Self {
        // MilkDrop's own defaults for a preset that sets none of them: no
        // motion at all.
        Self {
            zoom: 1.0,
            rot: 0.0,
            cx: 0.5,
            cy: 0.5,
            dx: 0.0,
            dy: 0.0,
            sx: 1.0,
            sy: 1.0,
        }
    }
}

impl WarpParams {
    /// Reads the eight values out of an evaluated `per_pixel` context.
    ///
    /// A name the preset never mentions keeps its default rather than reading
    /// zero -- a preset that never writes `zoom` means "no zoom", which is 1.0,
    /// and reading 0.0 there collapses the whole frame to a point.
    pub fn from_context(ctx: &eel::Context, symbols: &eel::Symbols) -> Self {
        let mut params = WarpParams::default();
        let read = |name: &str, fallback: f32| -> f32 {
            match symbols.existing(name) {
                Some(slot) => {
                    let v = ctx.get(slot) as f32;
                    if v.is_finite() {
                        v
                    } else {
                        fallback
                    }
                }
                None => fallback,
            }
        };
        params.zoom = read("zoom", 1.0);
        params.rot = read("rot", 0.0);
        params.cx = read("cx", 0.5);
        params.cy = read("cy", 0.5);
        params.dx = read("dx", 0.0);
        params.dy = read("dy", 0.0);
        params.sx = read("sx", 1.0);
        params.sy = read("sy", 1.0);
        params
    }

    /// The reduced transform (see the module note). `x`/`y` are the grid point
    /// in 0..1 with y running down, matching texture space.
    pub fn source_for(&self, x: f32, y: f32) -> [f32; 2] {
        // Zoom about the centre. A zoom above 1 means the frame reads from a
        // smaller region of the previous one, which is what makes a picture
        // grow outward over successive frames.
        let inv = if self.zoom.abs() < 1e-6 {
            1.0
        } else {
            1.0 / self.zoom
        };
        let mut u = (x - 0.5) * inv + 0.5;
        let mut v = (y - 0.5) * inv + 0.5;

        // Stretch about the preset's own centre rather than the frame's.
        let sx = if self.sx.abs() < 1e-6 { 1.0 } else { self.sx };
        let sy = if self.sy.abs() < 1e-6 { 1.0 } else { self.sy };
        u = (u - self.cx) / sx + self.cx;
        v = (v - self.cy) / sy + self.cy;

        // Rotate about the same centre.
        let (sin_r, cos_r) = self.rot.sin_cos();
        let (ru, rv) = (u - self.cx, v - self.cy);
        u = ru * cos_r - rv * sin_r + self.cx;
        v = ru * sin_r + rv * cos_r + self.cy;

        [u - self.dx, v - self.dy]
    }
}

/// The uniforms the warp and composite passes read.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameUniform {
    decay: f32,
    _pad: [f32; 3],
}

/// The feedback pair and the two passes that move a picture through it.
pub struct MilkFrame {
    targets: [wgpu::Texture; 2],
    views: [wgpu::TextureView; 2],
    /// Which of the two holds the picture as it currently stands.
    current: usize,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    warp_pipeline: wgpu::RenderPipeline,
    comp_pipeline: wgpu::RenderPipeline,
    warp_layout: wgpu::BindGroupLayout,
    comp_layout: wgpu::BindGroupLayout,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    mesh_x: u32,
    mesh_y: u32,
    width: u32,
    height: u32,
}

impl MilkFrame {
    /// `mesh_x`/`mesh_y` are the warp grid's *cells*; MilkDrop's own default
    /// is 32x24 and its high setting 64x48.
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        mesh_x: u32,
        mesh_y: u32,
    ) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        let (mesh_x, mesh_y) = (mesh_x.max(1), mesh_y.max(1));

        let make_target = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FEEDBACK_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let targets = [
            make_target("milk-feedback-a"),
            make_target("milk-feedback-b"),
        ];
        let views = [
            targets[0].create_view(&wgpu::TextureViewDescriptor::default()),
            targets[1].create_view(&wgpu::TextureViewDescriptor::default()),
        ];

        // Clamped rather than repeating, for the spike: `bTexWrap` is a
        // per-preset setting and honouring it means two samplers, which is a
        // milestone detail rather than a pipeline question.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("milk-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk-uniform"),
            size: std::mem::size_of::<FrameUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let entries = |with_uniform: bool| -> Vec<wgpu::BindGroupLayoutEntry> {
            let mut v = vec![
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ];
            if with_uniform {
                v.push(wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                });
            }
            v
        };

        let warp_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk-warp-layout"),
            entries: &entries(true),
        });
        let comp_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk-comp-layout"),
            entries: &entries(false),
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("milk-frame"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let warp_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk-warp"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("milk-warp-pipeline-layout"),
                    bind_group_layouts: &[Some(&warp_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("warp_vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<WarpVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("warp_fs"),
                targets: &[Some(FEEDBACK_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let comp_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk-comp"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("milk-comp-pipeline-layout"),
                    bind_group_layouts: &[Some(&comp_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("comp_vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("comp_fs"),
                targets: &[Some(target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let vertex_count = ((mesh_x + 1) * (mesh_y + 1)) as u64;
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk-mesh-vertices"),
            size: vertex_count * std::mem::size_of::<WarpVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut index_data: Vec<u32> = Vec::with_capacity((mesh_x * mesh_y * 6) as usize);
        let stride = mesh_x + 1;
        for row in 0..mesh_y {
            for col in 0..mesh_x {
                let a = row * stride + col;
                let (b, c, d) = (a + 1, a + stride, a + stride + 1);
                index_data.extend_from_slice(&[a, c, b, b, c, d]);
            }
        }
        let index_count = index_data.len() as u32;
        // Mapped at creation: the index buffer is written exactly once, here,
        // and this is the one path that does not need the queue.
        let indices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk-mesh-indices"),
            size: (index_data.len() * 4) as u64,
            usage: wgpu::BufferUsages::INDEX,
            mapped_at_creation: true,
        });
        if let Ok(mut mapped) = indices.slice(..).get_mapped_range_mut() {
            mapped.copy_from_slice(bytemuck::cast_slice(&index_data));
        }
        indices.unmap();

        Self {
            targets,
            views,
            current: 0,
            sampler,
            uniform,
            warp_pipeline,
            comp_pipeline,
            warp_layout,
            comp_layout,
            vertices,
            indices,
            index_count,
            mesh_x,
            mesh_y,
            width,
            height,
        }
    }

    pub fn mesh(&self) -> (u32, u32) {
        (self.mesh_x, self.mesh_y)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The texture holding the picture as it currently stands, for readback.
    pub fn current_texture(&self) -> &wgpu::Texture {
        &self.targets[self.current]
    }

    /// Fills the mesh's vertex buffer from a warp evaluated per grid point.
    ///
    /// The closure is what a milestone would drive from `per_pixel`: it is
    /// handed the grid point in 0..1 and answers with that point's warp.
    pub fn set_mesh(&self, queue: &wgpu::Queue, mut warp_at: impl FnMut(f32, f32) -> WarpParams) {
        let stride = self.mesh_x + 1;
        let mut data: Vec<WarpVertex> = Vec::with_capacity((stride * (self.mesh_y + 1)) as usize);
        for row in 0..=self.mesh_y {
            for col in 0..=self.mesh_x {
                let x = col as f32 / self.mesh_x as f32;
                let y = row as f32 / self.mesh_y as f32;
                let params = warp_at(x, y);
                data.push(WarpVertex {
                    // Clip space has y up; the grid and texture space have y
                    // down, so exactly one of the two flips here.
                    position: [x * 2.0 - 1.0, 1.0 - y * 2.0],
                    source: params.source_for(x, y),
                });
            }
        }
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&data));
    }

    /// Writes a picture directly into the feedback pair, so a check has
    /// something whose motion it can follow.
    pub fn seed(&self, queue: &wgpu::Queue, rgba16f_rows: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.targets[self.current],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba16f_rows,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 8),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// One turn of the loop: resample the current picture through the mesh into
    /// the other target, which then becomes current.
    pub fn advance(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, decay: f32) {
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&FrameUniform {
                decay,
                _pad: [0.0; 3],
            }),
        );
        let source = self.current;
        let destination = 1 - self.current;

        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("milk-warp-bind"),
            layout: &self.warp_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.views[source]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniform.as_entire_binding(),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("milk-advance"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("milk-warp-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.views[destination],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.warp_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.index_count, 0, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        self.current = destination;
    }

    /// Draws the current picture into a caller's view.
    pub fn composite(
        &self,
        device: &wgpu::Device,
        target: &wgpu::TextureView,
    ) -> wgpu::CommandBuffer {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("milk-comp-bind"),
            layout: &self.comp_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.views[self.current]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("milk-composite"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("milk-comp-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.comp_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.finish()
    }
}

/// The two passes. Fixed WGSL for the spike -- a preset's own `warp` and `comp`
/// bodies would be translated into the marked places (see the module note).
const SHADER: &str = r#"
// One scalar, padded on the Rust side to a uniform buffer's 16-byte minimum.
// Declaring the padding here instead would make the struct 32 bytes, since a
// vec3 aligns to 16 -- which wgpu catches as a size mismatch rather than
// letting it read garbage.
struct Uniform {
    decay: f32,
};

@group(0) @binding(0) var previous: texture_2d<f32>;
@group(0) @binding(1) var previous_sampler: sampler;
@group(0) @binding(2) var<uniform> frame: Uniform;

struct WarpOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) source: vec2<f32>,
};

@vertex
fn warp_vs(@location(0) position: vec2<f32>, @location(1) source: vec2<f32>) -> WarpOut {
    var out: WarpOut;
    out.clip = vec4<f32>(position, 0.0, 1.0);
    out.source = source;
    return out;
}

@fragment
fn warp_fs(in: WarpOut) -> @location(0) vec4<f32> {
    // Where a preset's translated `warp` body would run, with `uv` bound to
    // `in.source` and `ret` the value written out.
    let previous_colour = textureSample(previous, previous_sampler, in.source).rgb;
    return vec4<f32>(previous_colour * frame.decay, 1.0);
}

struct CompOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn comp_vs(@builtin(vertex_index) index: u32) -> CompOut {
    // One oversized triangle rather than two, so there is no seam down the
    // diagonal for a filtered sample to find. Its uv runs 0..2, and the
    // half of it past 1 is the part that falls outside the viewport -- the
    // doubling belongs in the clip-space step, not in the uv, and putting it
    // in both draws the whole picture at quarter scale in one corner.
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: CompOut;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn comp_fs(in: CompOut) -> @location(0) vec4<f32> {
    // Where a preset's translated `comp` body would run.
    let colour = textureSample(previous, previous_sampler, in.uv).rgb;
    return vec4<f32>(colour, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_still_preset_reads_where_it_writes() {
        // zoom 1, no rotation, no translation: every grid point samples itself,
        // so a frame advanced through the mesh is the frame it started as.
        let params = WarpParams::default();
        for (x, y) in [(0.0, 0.0), (0.5, 0.5), (1.0, 1.0), (0.25, 0.75)] {
            let [u, v] = params.source_for(x, y);
            assert!((u - x).abs() < 1e-6, "u {u} vs x {x}");
            assert!((v - y).abs() < 1e-6, "v {v} vs y {y}");
        }
    }

    #[test]
    fn zooming_in_reads_from_a_smaller_region() {
        // Which is what makes a picture grow outward frame after frame.
        let params = WarpParams {
            zoom: 2.0,
            ..Default::default()
        };
        let [u, _] = params.source_for(1.0, 0.5);
        assert!(u < 1.0 && u > 0.5, "corner should read inward, got {u}");
        let [u, v] = params.source_for(0.5, 0.5);
        assert!(
            (u - 0.5).abs() < 1e-6 && (v - 0.5).abs() < 1e-6,
            "centre is fixed"
        );
    }

    #[test]
    fn translation_moves_the_picture_the_other_way() {
        // A positive dx shifts what is drawn to the right, which means reading
        // from further left.
        let params = WarpParams {
            dx: 0.1,
            ..Default::default()
        };
        let [u, _] = params.source_for(0.5, 0.5);
        assert!((u - 0.4).abs() < 1e-6, "expected 0.4, got {u}");
    }

    #[test]
    fn a_quarter_turn_takes_the_right_edge_to_the_bottom() {
        let params = WarpParams {
            rot: std::f32::consts::FRAC_PI_2,
            ..Default::default()
        };
        let [u, v] = params.source_for(1.0, 0.5);
        assert!((u - 0.5).abs() < 1e-6, "u {u}");
        assert!((v - 1.0).abs() < 1e-6, "v {v}");
    }

    #[test]
    fn a_degenerate_zoom_or_scale_does_not_collapse_the_frame() {
        // A preset that writes zoom=0 -- and they exist -- must not fold every
        // vertex onto one texel.
        let params = WarpParams {
            zoom: 0.0,
            sx: 0.0,
            ..Default::default()
        };
        let [u, v] = params.source_for(0.25, 0.75);
        assert!(u.is_finite() && v.is_finite(), "{u},{v}");
        assert!((u - 0.25).abs() < 1e-6 && (v - 0.75).abs() < 1e-6);
    }

    #[test]
    fn missing_names_keep_their_defaults_rather_than_reading_zero() {
        // A preset with no per_pixel block at all must still stand still.
        let symbols = eel::Symbols::new();
        let ctx = eel::Context::new(&symbols);
        let params = WarpParams::from_context(&ctx, &symbols);
        assert_eq!(params.zoom, 1.0);
        assert_eq!(params.sx, 1.0);
        assert_eq!(params.cx, 0.5);
    }
}
