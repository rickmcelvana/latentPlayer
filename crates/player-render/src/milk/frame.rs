//! MilkDrop's frame, as wgpu draws it.
//!
//! A MilkDrop frame is not a scene that gets redrawn. It is a **feedback
//! loop**: the previous frame is resampled through a warped mesh into the new
//! one, things are drawn on top, and only then is the result composited to the
//! screen. Everything a preset does visually is a consequence of that -- the
//! trails, the tunnels, the endless zoom -- so the loop is the piece that has
//! to be right before any preset's own code matters.
//!
//! The order here is MilkDrop's `RenderFrame`, and it is not interchangeable:
//!
//! 1. **Warp.** Resample the picture through the mesh, dimmed by `decay`.
//! 2. **Overlays.** The waveform, then the centre darkening and the borders --
//!    drawn *into the feedback buffer*, which is why they persist and smear
//!    rather than sitting on top like a HUD. A preset with no waveform fades to
//!    black in a couple of seconds however right its warp is: the overlays are
//!    the only thing putting new light into the loop.
//! 3. **Composite.** Video echo, gamma, the corner tint, and the four
//!    whole-frame filters, into the caller's view.
//!
//! What M7a does not do, and M7b will: no preset shaders, so the warp and
//! composite passes run fixed WGSL; no blur chain, since the only thing that
//! samples it is a preset shader; no custom waves or shapes; no motion vectors.
//! Those are named where a preset asks for them (`parse::Unsupported`) rather
//! than silently missing.
//!
//! Colour lives in **display space** all the way through the loop -- the
//! numbers are what a screen should show -- and is converted once, in the
//! composite, by `visuals/shared.wgsl`'s `to_target`. That is the same rule
//! every other visual in this crate follows and it is why the two shells agree.

use crate::attachments;
use crate::milk::state::FrameState;
use crate::milk::warp::{Aspect, Grid, GridPoint};
use crate::milk::wave::Wave;

/// The colour format the feedback pair uses.
///
/// 16-bit float rather than 8-bit unorm, and this is not a luxury: a feedback
/// loop multiplies its own output by a decay every frame, so 8 bits of
/// precision quantises to a visible staircase within a second and dark trails
/// snap to black. It is also what lets a preset's `ret` exceed 1.0 mid-chain
/// before the composite brings it back.
pub const FEEDBACK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// MilkDrop's own default warp grid.
pub const DEFAULT_MESH: (u32, u32) = (32, 24);

/// One mesh vertex's warp: where on the previous frame this grid point reads.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WarpVertex {
    /// Grid position in clip space, -1..1.
    pub position: [f32; 2],
    /// Where to sample the previous frame, in texture space 0..1.
    pub source: [f32; 2],
}

/// A coloured vertex, for everything drawn on top of the warp.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ShapeVertex {
    position: [f32; 2],
    colour: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct WarpUniform {
    decay: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniform {
    /// The four corner tints, in the order top-left, top-right, bottom-left,
    /// bottom-right -- bilinearly interpolated across the frame, which is what
    /// drawing them as a quad's vertex colours amounted to.
    shade: [[f32; 4]; 4],
    /// zoom, alpha, flip-u, flip-v.
    echo: [f32; 4],
    /// gamma, "the target encodes sRGB", and two spare.
    misc: [f32; 4],
    /// brighten, darken, solarize, invert -- as 0.0 or 1.0, in the order
    /// MilkDrop applies them.
    filters: [f32; 4],
    /// The field this crate draws every visual on, as a display colour.
    background: [f32; 4],
}

/// What the composite pass needs that the preset's state does not carry.
pub struct CompositeSettings {
    /// Seconds since the preset was selected, for the corner tint's own drift.
    pub time: f32,
    /// `fShader`: how far the corner tint departs from white, 0..1.
    pub shader: f32,
}

/// The feedback pair and the passes that move a picture through it.
pub struct MilkFrame {
    targets: [wgpu::Texture; 2],
    views: [wgpu::TextureView; 2],
    /// Which of the two holds the picture as it currently stands.
    current: usize,
    clamp_sampler: wgpu::Sampler,
    repeat_sampler: wgpu::Sampler,
    warp_uniform: wgpu::Buffer,
    comp_uniform: wgpu::Buffer,
    warp_pipeline: wgpu::RenderPipeline,
    comp_pipeline: wgpu::RenderPipeline,
    /// Alpha-over triangles, for the centre darkening and the borders.
    shape_pipeline: wgpu::RenderPipeline,
    /// Line strips and points, over and additive: the four ways a preset can
    /// ask for its waveform.
    line_over: wgpu::RenderPipeline,
    line_add: wgpu::RenderPipeline,
    point_over: wgpu::RenderPipeline,
    point_add: wgpu::RenderPipeline,
    warp_layout: wgpu::BindGroupLayout,
    comp_layout: wgpu::BindGroupLayout,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    /// Grown as the overlays need it; never shrunk, since the frame after a
    /// busy one is usually just as busy.
    overlay: wgpu::Buffer,
    overlay_capacity: usize,
    mesh_x: u32,
    mesh_y: u32,
    width: u32,
    height: u32,
    target_format: wgpu::TextureFormat,
}

impl MilkFrame {
    /// `mesh_x`/`mesh_y` are the warp grid's *cells*; MilkDrop's own default is
    /// [`DEFAULT_MESH`] and its high setting 64x48.
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
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
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

        // Two samplers rather than one, because `bTexWrap` is a per-preset
        // setting and the difference is visible: a zooming-out preset either
        // tiles its own picture at the edges or smears the edge pixel outward,
        // and presets are written for one or the other.
        let make_sampler = |label: &str, mode: wgpu::AddressMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: mode,
                address_mode_v: mode,
                address_mode_w: mode,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            })
        };
        let clamp_sampler = make_sampler("milk-sampler-clamp", wgpu::AddressMode::ClampToEdge);
        let repeat_sampler = make_sampler("milk-sampler-repeat", wgpu::AddressMode::Repeat);

        let make_uniform = |label: &str, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let warp_uniform = make_uniform(
            "milk-warp-uniform",
            std::mem::size_of::<WarpUniform>() as u64,
        );
        let comp_uniform = make_uniform(
            "milk-comp-uniform",
            std::mem::size_of::<CompositeUniform>() as u64,
        );

        let sampled_layout = |label: &str| -> Vec<wgpu::BindGroupLayoutEntry> {
            let _ = label;
            vec![
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ]
        };

        let warp_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk-warp-layout"),
            entries: &sampled_layout("warp"),
        });
        let comp_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk-comp-layout"),
            entries: &sampled_layout("comp"),
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("milk-frame"),
            source: wgpu::ShaderSource::Wgsl(attachments::shader_source(SHADER)),
        });

        let layout_for = |label: &str, bind: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(bind)],
                immediate_size: 0,
            })
        };

        let warp_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk-warp"),
            layout: Some(&layout_for("milk-warp-pipeline-layout", &warp_layout)),
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
            layout: Some(&layout_for("milk-comp-pipeline-layout", &comp_layout)),
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

        let overlay_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("milk-overlay-pipeline-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let overlay_pipeline = |label: &str,
                                topology: wgpu::PrimitiveTopology,
                                additive: bool|
         -> wgpu::RenderPipeline {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&overlay_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("shape_vs"),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<ShapeVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 8,
                                shader_location: 1,
                            },
                        ],
                    })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("shape_fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FEEDBACK_FORMAT,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: if additive {
                                    wgpu::BlendFactor::One
                                } else {
                                    wgpu::BlendFactor::OneMinusSrcAlpha
                                },
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::REPLACE,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let shape_pipeline =
            overlay_pipeline("milk-shapes", wgpu::PrimitiveTopology::TriangleList, false);
        let line_over =
            overlay_pipeline("milk-wave-over", wgpu::PrimitiveTopology::LineStrip, false);
        let line_add = overlay_pipeline("milk-wave-add", wgpu::PrimitiveTopology::LineStrip, true);
        let point_over =
            overlay_pipeline("milk-dots-over", wgpu::PrimitiveTopology::PointList, false);
        let point_add = overlay_pipeline("milk-dots-add", wgpu::PrimitiveTopology::PointList, true);

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

        // Room for a smoothed 512-point waveform drawn four times over, plus
        // the borders and the centre -- a first frame that does not reallocate.
        let overlay_capacity = 8192;
        let overlay = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk-overlay-vertices"),
            size: (overlay_capacity * std::mem::size_of::<ShapeVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            targets,
            views,
            current: 0,
            clamp_sampler,
            repeat_sampler,
            warp_uniform,
            comp_uniform,
            warp_pipeline,
            comp_pipeline,
            shape_pipeline,
            line_over,
            line_add,
            point_over,
            point_add,
            warp_layout,
            comp_layout,
            vertices,
            indices,
            index_count,
            overlay,
            overlay_capacity,
            mesh_x,
            mesh_y,
            width,
            height,
            target_format,
        }
    }

    pub fn mesh(&self) -> (u32, u32) {
        (self.mesh_x, self.mesh_y)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn aspect(&self) -> Aspect {
        Aspect::new(self.width, self.height)
    }

    /// The texture holding the picture as it currently stands, for readback.
    pub fn current_texture(&self) -> &wgpu::Texture {
        &self.targets[self.current]
    }

    /// Empties both halves of the pair.
    ///
    /// Needed whenever the loop's history stops meaning anything -- a preset
    /// change, a resize -- because a feedback buffer holds the *last* preset's
    /// picture and warping it under new equations is a smear rather than a
    /// start.
    pub fn clear(&self, encoder: &mut wgpu::CommandEncoder) {
        for view in &self.views {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("milk-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
    }

    /// Fills the mesh's vertex buffer from a warp evaluated per grid point.
    ///
    /// The closure is what `per_pixel` drives: it is handed each grid point and
    /// answers with where that point reads. Taking the grid rather than
    /// recomputing it means `rad` and `ang` -- which every vertex needs and
    /// neither changes between frames -- are computed once when the mesh is
    /// built.
    pub fn set_mesh(
        &self,
        queue: &wgpu::Queue,
        grid: &Grid,
        mut source_for: impl FnMut(&GridPoint) -> [f32; 2],
    ) {
        debug_assert_eq!(grid.mesh_x, self.mesh_x);
        debug_assert_eq!(grid.mesh_y, self.mesh_y);
        let data: Vec<WarpVertex> = grid
            .points()
            .iter()
            .map(|point| WarpVertex {
                position: [point.x, point.y],
                source: source_for(point),
            })
            .collect();
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
    pub fn advance(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        decay: f32,
        wrap: bool,
    ) {
        queue.write_buffer(
            &self.warp_uniform,
            0,
            bytemuck::bytes_of(&WarpUniform {
                // A decay above 1 is a loop that grows without bound; MilkDrop
                // is saved from it by an 8-bit target that clamps, and a float
                // target is not. A preset writing 1.01 means "no decay".
                decay: decay.clamp(0.0, 1.0),
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
                    resource: wgpu::BindingResource::Sampler(if wrap {
                        &self.repeat_sampler
                    } else {
                        &self.clamp_sampler
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.warp_uniform.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("milk-warp-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.views[destination],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
        self.current = destination;
    }

    /// Draws the waveform, the centre darkening and the borders into the
    /// feedback buffer, in MilkDrop's order.
    ///
    /// Into the feedback buffer, not over the composite: that is what makes
    /// them part of the loop, so this frame's waveform is next frame's trail.
    pub fn draw_overlays(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        state: &FrameState,
        wave: Option<&Wave>,
    ) {
        let aspect = self.aspect();
        let mut wave_runs: Vec<(usize, usize)> = Vec::new();
        let mut vertices: Vec<ShapeVertex> = Vec::new();

        if let Some(wave) = wave {
            // Thick lines and fat dots are four copies a pixel apart, which is
            // MilkDrop's way of drawing a two-pixel line on hardware that only
            // draws one-pixel ones -- and is still the only way here, since
            // WGSL has neither a line width nor a point size.
            let offsets: &[[f32; 2]] = if wave.thick || wave.dots {
                let (dx, dy) = (2.0 / self.width as f32, 2.0 / self.height as f32);
                &[[0.0, 0.0], [dx, 0.0], [dx, dy], [0.0, dy]]
            } else {
                &[[0.0, 0.0]]
            };
            let runs: Vec<(usize, usize)> = match wave.break_at {
                Some(at) => vec![(0, at), (at, wave.points.len())],
                None => vec![(0, wave.points.len())],
            };
            for offset in offsets {
                for (from, to) in &runs {
                    if to - from < 2 {
                        continue;
                    }
                    let start = vertices.len();
                    vertices.extend(wave.points[*from..*to].iter().map(|p| ShapeVertex {
                        position: [p[0] + offset[0], p[1] + offset[1]],
                        colour: wave.colour,
                    }));
                    wave_runs.push((start, vertices.len()));
                }
            }
        }

        let shapes_start = vertices.len();
        if state.darken_center {
            push_darken_center(&mut vertices, aspect);
        }
        push_borders(&mut vertices, state);
        let shapes_end = vertices.len();

        if vertices.is_empty() {
            return;
        }
        self.ensure_overlay_capacity(device, vertices.len());
        queue.write_buffer(&self.overlay, 0, bytemuck::cast_slice(&vertices));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("milk-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.views[self.current],
                depth_slice: None,
                resolve_target: None,
                // Load, not clear: this is the picture the warp just made.
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_vertex_buffer(0, self.overlay.slice(..));

        if let Some(wave) = wave {
            let pipeline = match (wave.dots, wave.additive) {
                (true, true) => &self.point_add,
                (true, false) => &self.point_over,
                (false, true) => &self.line_add,
                (false, false) => &self.line_over,
            };
            pass.set_pipeline(pipeline);
            for (from, to) in &wave_runs {
                pass.draw(*from as u32..*to as u32, 0..1);
            }
        }
        if shapes_end > shapes_start {
            pass.set_pipeline(&self.shape_pipeline);
            pass.draw(shapes_start as u32..shapes_end as u32, 0..1);
        }
    }

    fn ensure_overlay_capacity(&mut self, device: &wgpu::Device, needed: usize) {
        if needed <= self.overlay_capacity {
            return;
        }
        let capacity = needed.next_power_of_two();
        self.overlay = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk-overlay-vertices"),
            size: (capacity * std::mem::size_of::<ShapeVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.overlay_capacity = capacity;
    }

    /// Draws the current picture into a caller's view, through the echo, the
    /// gamma, the corner tint and the four filters.
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        state: &FrameState,
        settings: &CompositeSettings,
    ) {
        let shade = corner_tints(settings.time, settings.shader);
        queue.write_buffer(
            &self.comp_uniform,
            0,
            bytemuck::bytes_of(&CompositeUniform {
                shade,
                echo: [
                    state.echo_zoom,
                    state.echo_alpha.clamp(0.0, 1.0),
                    f32::from(u8::from(state.echo_orientation % 2 != 0)),
                    f32::from(u8::from(state.echo_orientation >= 2)),
                ],
                misc: [
                    state.gamma.max(0.0),
                    attachments::target_is_srgb(self.target_format),
                    0.0,
                    0.0,
                ],
                filters: [
                    f32::from(u8::from(state.brighten)),
                    f32::from(u8::from(state.darken)),
                    f32::from(u8::from(state.solarize)),
                    f32::from(u8::from(state.invert)),
                ],
                background: {
                    let [r, g, b] = attachments::BACKGROUND;
                    [r as f32, g as f32, b as f32, 1.0]
                },
            }),
        );

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
                    // Always clamped: the composite reads inside its own frame
                    // and an echo zoomed below 1 would otherwise tile the
                    // picture at the edges, which is not what an echo is.
                    resource: wgpu::BindingResource::Sampler(&self.clamp_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.comp_uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("milk-comp-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(attachments::background(self.target_format)),
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
}

/// MilkDrop's composite tint: four corner colours drifting on three slow sines.
///
/// The three rates are close but not commensurate, so the corners never come
/// back into step -- which is the whole effect. Normalised by the largest of
/// the three so the tint changes hue without changing brightness, then pulled
/// back toward white by `1 - shader`, which is what makes `fShader` a dial
/// rather than a switch.
///
/// MilkDrop seeds these from a per-session random start; this build does not,
/// so a preset looks the same every time it is opened. That is a deliberate
/// divergence for the same reason `eel::rand` is seeded -- a visual in this
/// crate has to be checkable against pixels (decision 6), and a tint that
/// differs per run cannot be.
fn corner_tints(time: f32, shader: f32) -> [[f32; 4]; 4] {
    let mut out = [[1.0f32; 4]; 4];
    if shader <= 0.001 {
        return out;
    }
    let shader = shader.clamp(0.0, 1.0);
    for (i, corner) in out.iter_mut().enumerate() {
        let i = i as f32;
        let mut rgb = [
            0.6 + 0.3 * (time * 30.0 * 0.0143 + 3.0 + i * 21.0).sin(),
            0.6 + 0.3 * (time * 30.0 * 0.0107 + 1.0 + i * 13.0).sin(),
            0.6 + 0.3 * (time * 30.0 * 0.0129 + 6.0 + i * 9.0).sin(),
        ];
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        for channel in &mut rgb {
            *channel = if max > 1e-6 { *channel / max } else { 1.0 };
            *channel = 0.5 + 0.5 * *channel;
            *channel = *channel * shader + (1.0 - shader);
        }
        *corner = [rgb[0], rgb[1], rgb[2], 1.0];
    }
    out
}

/// The faint dark diamond MilkDrop puts over the middle of the frame.
///
/// It exists to stop a zooming feedback loop from saturating its own centre --
/// the point every frame reads from most often. A triangle fan of four
/// triangles, opaque-ish at the middle and transparent at the rim.
fn push_darken_center(out: &mut Vec<ShapeVertex>, aspect: Aspect) {
    const HALF: f32 = 0.05;
    let centre = ShapeVertex {
        position: [0.0, 0.0],
        colour: [0.0, 0.0, 0.0, 3.0 / 32.0],
    };
    let rim = |x: f32, y: f32| ShapeVertex {
        position: [x, y],
        colour: [0.0, 0.0, 0.0, 0.0],
    };
    let (w, h) = (HALF * aspect.y, HALF);
    let points = [
        rim(-w, 0.0),
        rim(0.0, -h),
        rim(w, 0.0),
        rim(0.0, h),
        rim(-w, 0.0),
    ];
    for pair in points.windows(2) {
        out.push(centre);
        out.push(pair[0]);
        out.push(pair[1]);
    }
}

/// The two border rings, outer then inner.
///
/// Each is one quad covering the right-hand strip of the frame, drawn four
/// times a quarter turn apart -- which is how MilkDrop gets a rectangular ring
/// out of one shape, and why the corners are mitred rather than butted.
fn push_borders(out: &mut Vec<ShapeVertex>, state: &FrameState) {
    let outer = state.outer_border.size;
    for (border, inner_rad, outer_rad) in [
        (&state.outer_border, 1.0 - outer, 1.0),
        (
            &state.inner_border,
            1.0 - outer - state.inner_border.size,
            1.0 - outer,
        ),
    ] {
        let colour = border.colour;
        if colour[3] <= 0.001 || !inner_rad.is_finite() || !outer_rad.is_finite() {
            continue;
        }
        let quad = [
            [inner_rad, inner_rad],
            [outer_rad, outer_rad],
            [outer_rad, -outer_rad],
            [inner_rad, -inner_rad],
        ];
        for turn in 0..4 {
            let (sin_t, cos_t) = (std::f32::consts::FRAC_PI_2 * turn as f32).sin_cos();
            let rotate = |p: [f32; 2]| ShapeVertex {
                position: [p[0] * cos_t - p[1] * sin_t, p[0] * sin_t + p[1] * cos_t],
                colour,
            };
            // The fan (0,1,2),(0,2,3), written out as a list.
            for index in [0usize, 1, 2, 0, 2, 3] {
                out.push(rotate(quad[index]));
            }
        }
    }
}

/// The passes. Fixed WGSL for M7a -- a preset's own `warp` and `comp` bodies
/// are translated into the marked places in M7b.
const SHADER: &str = r#"
// One scalar, padded on the Rust side to a uniform buffer's 16-byte minimum.
// Declaring the padding here instead would make the struct 32 bytes, since a
// vec3 aligns to 16 -- which wgpu catches as a size mismatch rather than
// letting it read garbage.
struct WarpUniform {
    decay: f32,
};

struct CompositeUniform {
    shade0: vec4<f32>,
    shade1: vec4<f32>,
    shade2: vec4<f32>,
    shade3: vec4<f32>,
    echo: vec4<f32>,
    misc: vec4<f32>,
    filters: vec4<f32>,
    background: vec4<f32>,
};

@group(0) @binding(0) var previous: texture_2d<f32>;
@group(0) @binding(1) var previous_sampler: sampler;
@group(0) @binding(2) var<uniform> warp_frame: WarpUniform;

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
    // Where a preset's translated `warp` body will run (M7b), with `uv` bound
    // to `in.source` and `ret` the value written out.
    let previous_colour = textureSample(previous, previous_sampler, in.source).rgb;
    return vec4<f32>(previous_colour * warp_frame.decay, 1.0);
}

struct ShapeOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex
fn shape_vs(@location(0) position: vec2<f32>, @location(1) colour: vec4<f32>) -> ShapeOut {
    var out: ShapeOut;
    out.clip = vec4<f32>(position, 0.0, 1.0);
    out.colour = colour;
    return out;
}

@fragment
fn shape_fs(in: ShapeOut) -> @location(0) vec4<f32> {
    // Straight through: the feedback buffer holds display-space colour, and
    // the one conversion to the surface happens in the composite.
    return in.colour;
}

struct CompOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(2) var<uniform> comp: CompositeUniform;

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
    let uv = in.uv;

    // The corner tint, bilinear across the frame -- which is what MilkDrop's
    // four vertex colours on a quad amounted to.
    let top = mix(comp.shade0.rgb, comp.shade1.rgb, uv.x);
    let bottom = mix(comp.shade2.rgb, comp.shade3.rgb, uv.x);
    let shade = mix(top, bottom, uv.y);

    let echo_alpha = comp.echo.y;
    var colour = textureSample(previous, previous_sampler, uv).rgb;
    if (echo_alpha > 0.001) {
        // The echo is the same picture at another scale, mixed in. A zoom of 2
        // reads the middle half, so the echo is the frame's centre blown up.
        let zoom = max(abs(comp.echo.x), 0.0001);
        let lo = 0.5 - 0.5 / zoom;
        let hi = 0.5 + 0.5 / zoom;
        var euv = vec2<f32>(mix(lo, hi, uv.x), mix(lo, hi, uv.y));
        if (comp.echo.z > 0.5) { euv.x = 1.0 - euv.x; }
        if (comp.echo.w > 0.5) { euv.y = 1.0 - euv.y; }
        colour = colour * (1.0 - echo_alpha)
               + textureSample(previous, previous_sampler, euv).rgb * echo_alpha;
    }

    // Gamma. MilkDrop draws this as several additive passes summing to exactly
    // `gamma`, because its 8-bit target could not hold a multiply above 1 --
    // and each pass clamps, so the sum is clamp(gamma * colour). One multiply
    // and one clamp is the same picture, in one pass.
    colour = clamp(colour * shade * comp.misc.x, vec3<f32>(0.0), vec3<f32>(1.0));

    // The four whole-frame filters, in MilkDrop's own order. Each is written
    // there as a pair of blend states over a white quad; these are what those
    // blends compute.
    if (comp.filters.x > 0.5) { colour = 1.0 - (1.0 - colour) * (1.0 - colour); }
    if (comp.filters.y > 0.5) { colour = colour * colour; }
    if (comp.filters.z > 0.5) { colour = 2.0 * colour * (1.0 - colour); }
    if (comp.filters.w > 0.5) { colour = 1.0 - colour; }

    // The picture is *light on a dark field*: a MilkDrop frame is black
    // wherever nothing has been drawn, and black there means "nothing", not
    // "a black object". So it is added over this crate's own background rather
    // than replacing it -- which is what every other visual does implicitly by
    // clearing to that colour and drawing over it, and is why a fullscreen
    // preset has the same faint sense of a surface behind it that the rest of
    // the gallery has (`attachments::BACKGROUND`). At rgb(5,5,8) the lift is
    // two percent, visible only in the blacks, which is exactly where it is
    // meant to be visible.
    colour = min(colour + comp.background.rgb, vec3<f32>(1.0));

    return vec4<f32>(to_target(colour, comp.misc.y), 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milk::state::Border;

    #[test]
    fn the_corner_tint_is_white_until_a_preset_asks_for_it() {
        // `fShader` defaults to 0 and most presets leave it there, so the
        // common case has to be exactly one, not nearly one.
        for corner in corner_tints(12.5, 0.0) {
            assert_eq!(corner, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn the_corner_tint_changes_hue_without_changing_brightness() {
        let tints = corner_tints(3.0, 1.0);
        for corner in tints {
            let max = corner[0].max(corner[1]).max(corner[2]);
            // Normalised by its own largest channel, then lifted halfway to
            // white -- so the brightest channel is always 1.0.
            assert!((max - 1.0).abs() < 1e-5, "{corner:?}");
            assert!(corner.iter().take(3).all(|c| *c >= 0.5), "{corner:?}");
        }
        // And the four corners differ, which is the whole effect.
        assert!(tints[0] != tints[1] || tints[0] != tints[2]);
    }

    #[test]
    fn a_partial_shader_amount_sits_between_white_and_the_full_tint() {
        let full = corner_tints(3.0, 1.0)[0];
        let half = corner_tints(3.0, 0.5)[0];
        for channel in 0..3 {
            let expected = full[channel] * 0.5 + 0.5;
            assert!(
                (half[channel] - expected).abs() < 1e-5,
                "{half:?} vs {full:?}"
            );
        }
    }

    #[test]
    fn the_centre_darkening_is_four_triangles_that_fade_outward() {
        let mut out = Vec::new();
        push_darken_center(&mut out, Aspect::new(128, 128));
        assert_eq!(out.len(), 12, "four triangles");
        for triangle in out.chunks(3) {
            assert_eq!(triangle[0].position, [0.0, 0.0]);
            assert!((triangle[0].colour[3] - 3.0 / 32.0).abs() < 1e-6);
            assert_eq!(triangle[1].colour[3], 0.0);
            assert_eq!(triangle[2].colour[3], 0.0);
        }
    }

    #[test]
    fn a_transparent_border_is_not_drawn_at_all() {
        let state = FrameState::default();
        let mut out = Vec::new();
        push_borders(&mut out, &state);
        assert!(out.is_empty(), "both borders default to alpha 0");
    }

    #[test]
    fn the_borders_ring_the_frame_and_the_inner_one_sits_inside_the_outer() {
        let state = FrameState {
            outer_border: Border {
                size: 0.1,
                colour: [1.0, 0.0, 0.0, 1.0],
            },
            inner_border: Border {
                size: 0.05,
                colour: [0.0, 1.0, 0.0, 1.0],
            },
            ..Default::default()
        };
        let mut out = Vec::new();
        push_borders(&mut out, &state);
        // Two rings, four quarter turns each, two triangles a quad.
        assert_eq!(out.len(), 2 * 4 * 6);

        let extent = |slice: &[ShapeVertex]| {
            slice.iter().fold((f32::MAX, 0.0f32), |(near, far), v| {
                let d = v.position[0].abs().max(v.position[1].abs());
                (near.min(d), far.max(d))
            })
        };
        let (outer_near, outer_far) = extent(&out[..24]);
        let (inner_near, inner_far) = extent(&out[24..]);
        assert!((outer_far - 1.0).abs() < 1e-6, "outer reaches the edge");
        assert!((outer_near - 0.9).abs() < 1e-6, "outer is 0.1 deep");
        assert!(
            (inner_far - 0.9).abs() < 1e-6,
            "inner starts where outer ends"
        );
        assert!((inner_near - 0.85).abs() < 1e-6, "inner is 0.05 deep");
    }

    #[test]
    fn the_uniforms_are_sized_the_way_a_uniform_buffer_needs() {
        // A struct whose size is not a multiple of 16 is a binding wgpu
        // rejects, and the failure reads as a shader mismatch rather than as
        // arithmetic -- which cost this module an afternoon during the spike.
        assert_eq!(std::mem::size_of::<WarpUniform>() % 16, 0);
        assert_eq!(std::mem::size_of::<CompositeUniform>() % 16, 0);
        assert_eq!(std::mem::size_of::<CompositeUniform>(), 128);
        // Size alone does not catch the failure that actually happened here:
        // adding `background` in a different position on each side left the
        // struct exactly as big and made `filters` read the background's
        // alpha, which is 1.0 -- so every frame drew inverted, pure white. The
        // fields must be declared in the same order as the WGSL block above,
        // and that is a thing to check by eye when either is edited.
        assert_eq!(
            std::mem::size_of::<CompositeUniform>(),
            (4 + 1 + 1 + 1 + 1) * std::mem::size_of::<[f32; 4]>()
        );
    }
}
