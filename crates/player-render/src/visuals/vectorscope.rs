//! The 3D vectorscope: PLAN.md section 4's hero feature, shipped in Phase 1 M6
//! and the visualizer host's first citizen since Phase 3 M1.
//!
//! Everything between "here are some samples" and "a frame has been drawn into
//! that texture view". It was already split from [`crate::Renderer`] because
//! the scope has two targets that must be the *same* scene -- a surface (a
//! native window or a browser canvas) and an offscreen texture the smoke check
//! reads back pixel by pixel (M6 decision 5). M1 turned that split into the
//! [`Visual`] contract and moved this behind it: same camera, same extrusion,
//! same shader, now one entry in a gallery rather than the only thing the
//! renderer can draw.

use wgpu::util::DeviceExt;

use crate::attachments::{targets, DEPTH_FORMAT, SAMPLE_COUNT};
use crate::camera::{look_at, perspective, Mat4};
use crate::trace::{self, Vertex, TRACE_POINTS};
use crate::visual::{Gpu, Visual, VisualFrame};

/// Where the camera sits, looking at the origin. Off to one side and slightly
/// above, because a scope viewed straight down its own time axis is just the
/// 2D scope again -- the angle is what shows the extrusion.
///
/// The side it sits on is not arbitrary. From -x the camera's right vector
/// points along +z, which puts the newest end of the ribbon on the right of
/// the image and sends history off to the left -- the direction time runs in
/// every other audio display there is. Viewed from +x the same scene reads
/// backwards, which the M6 smoke check noticed before anyone had to look at it.
const EYE: [f32; 3] = [-1.05, 0.8, 3.3];
const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
const NEAR: f32 = 0.1;
const FAR: f32 = 100.0;

/// The camera uniform, as the shader sees it.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

pub struct Vectorscope {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    camera_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// The multisampled colour target the pass draws into; resolved onto
    /// whatever view the caller passes.
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// Reused across frames so a 60Hz redraw loop allocates nothing.
    vertices: Vec<Vertex>,
    width: u32,
    height: u32,
}

impl Vectorscope {
    pub(crate) fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let Gpu {
            device,
            queue,
            format,
        } = *gpu;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vectorscope-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("vectorscope.wgsl").into()),
        });

        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vectorscope-camera"),
            contents: bytemuck::bytes_of(&CameraUniform {
                view_proj: view_projection(width, height).as_array(),
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vectorscope-camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vectorscope-camera-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vectorscope-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vectorscope-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        offset: 0,
                        shader_location: 0,
                        format: wgpu::VertexFormat::Float32x3,
                    }],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineStrip,
                ..Default::default()
            },
            // The depth buffer is what makes this a 3D scene rather than a
            // flat drawing in perspective: without it the ribbon's far end
            // paints over its near end wherever it happens to be drawn later.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: SAMPLE_COUNT,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vectorscope-vertex-buffer"),
            size: (std::mem::size_of::<Vertex>() * TRACE_POINTS) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (msaa_view, depth_view) = targets(device, format, width, height);
        let scene = Self {
            pipeline,
            vertex_buffer,
            camera_buffer,
            bind_group,
            msaa_view,
            depth_view,
            vertices: Vec::with_capacity(TRACE_POINTS),
            width,
            height,
        };
        scene.write_camera(queue);
        scene
    }

    fn write_camera(&self, queue: &wgpu::Queue) {
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&CameraUniform {
                view_proj: view_projection(self.width, self.height).as_array(),
            }),
        );
    }
}

impl Visual for Vectorscope {
    /// Rebuilds the size-dependent attachments and the projection.
    fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        let Gpu {
            device,
            queue,
            format,
        } = *gpu;
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        let (msaa_view, depth_view) = targets(device, format, width, height);
        self.msaa_view = msaa_view;
        self.depth_view = depth_view;
        // The aspect ratio is in the projection, so a resize that skipped this
        // would stretch the figure instead of showing more of the scene.
        self.write_camera(queue);
    }

    /// Draws one frame of the extruded figure into `target`.
    ///
    /// Of the frame this reads the samples and their layout and nothing else:
    /// the vectorscope is a measurement, and what it measures is what reached
    /// the output (decision 2 -- no DSP here, and none needed).
    fn draw(
        &mut self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        frame: &VisualFrame,
    ) -> wgpu::CommandBuffer {
        let Gpu { device, queue, .. } = *gpu;
        trace::build(frame.samples, frame.channels, &mut self.vertices);
        if !self.vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.vertices));
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vectorscope-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vectorscope-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa_view,
                    // Multisampled colour is resolved straight onto the
                    // caller's view, so nothing downstream has to know the
                    // pass was multisampled at all.
                    resolve_target: Some(target),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.02,
                            b: 0.03,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..self.vertices.len() as u32, 0..1);
        }
        encoder.finish()
    }
}

/// The view-projection for a viewport of this size.
fn view_projection(width: u32, height: u32) -> Mat4 {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    let projection = perspective(FOV_Y, aspect, NEAR, FAR);
    let view = look_at(EYE, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    projection.mul(&view)
}
