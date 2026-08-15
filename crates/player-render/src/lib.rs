//! Phase 0's wgpu rendering spike (PLAN.md §5/§8.1). This crate only knows
//! how to draw a 2D XY oscilloscope into a `wgpu::Surface` it's handed --
//! nothing here is native- or web-specific. `src-tauri` hands it a Tauri
//! `Window`'s raw handle; `player-rt-wasm` hands it an `HtmlCanvasElement`.
//! Same shader, same pipeline, same draw call, both places -- that's the
//! thing this milestone exists to prove.
//!
//! The 2D line-strip scope here is the direct precursor to the 3D
//! vectorscope (PLAN.md §4): same input (interleaved L/R samples), same
//! idea (literal stereo-field data as the visual), one dimension short.

mod error;

pub use error::RenderError;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// A point on the XY scope: one L/R sample pair.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
}

/// Upper bound on how many L/R points are drawn per frame. Excess samples
/// are dropped rather than resizing the vertex buffer every frame -- a real
/// 3D vectorscope will window/decimate its input deliberately (Phase 4);
/// this is just enough to keep Phase 0's draw call simple.
const MAX_POINTS: usize = 4096;

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    /// Set when the surface told us its texture no longer matches it, so
    /// the *next* frame reconfigures before drawing. Deferred rather than
    /// done on the spot because the surface texture is still in hand at
    /// that point, and reconfiguring underneath it is not worth the risk
    /// when the frame is perfectly drawable as-is.
    needs_reconfigure: bool,
}

impl Renderer {
    /// `target` must be `'static` (an owned window handle, not a borrow) --
    /// see PLAN.md §5's native-window note for why: the surface stores it
    /// internally to outlive short-lived borrows.
    ///
    /// Deliberately `async` rather than blocking internally (e.g. via
    /// `pollster::block_on`): `pollster` has no wasm32 support (checked its
    /// source, not assumed) and would hang the page rather than await the
    /// adapter/device promises on the web build. Native callers that need a
    /// sync call (src-tauri, on its own real OS thread) wrap this with
    /// `pollster::block_on` themselves at the call site instead.
    pub async fn new(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(target)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;

        let config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(RenderError::UnsupportedSurface)?;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scope-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scope-pipeline-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scope-pipeline"),
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
                        format: wgpu::VertexFormat::Float32x2,
                    }],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scope-vertex-buffer"),
            contents: bytemuck::cast_slice(&[Vertex { pos: [0.0, 0.0] }; MAX_POINTS]),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            vertex_buffer,
            needs_reconfigure: false,
        })
    }

    /// Reconfigures the surface for a new window/canvas size. Callers must
    /// drive this from whatever tells them the size changed -- the renderer
    /// holds no window handle and cannot ask.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.needs_reconfigure = false;
    }

    /// `samples` is interleaved `[L0, R0, L1, R1, ...]`, already in roughly
    /// `[-1, 1]` (raw PCM range) -- drawn directly as XY points, no scaling.
    pub fn draw_frame(&mut self, samples: &[f32]) -> Result<(), RenderError> {
        let points: Vec<Vertex> = samples
            .chunks_exact(2)
            .take(MAX_POINTS)
            .map(|pair| Vertex {
                pos: [pair[0], pair[1]],
            })
            .collect();

        self.queue
            .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&points));

        if self.needs_reconfigure {
            self.surface.configure(&self.device, &self.config);
            self.needs_reconfigure = false;
        }

        // `get_current_texture` returns an enum rather than a `Result`, and
        // the variants do *not* all mean the same thing. This used to treat
        // every non-Success case as "skip the frame, the caller will retry",
        // which is wrong for `Outdated`: wgpu's own docs say to call
        // `configure()` and try again, so retrying without reconfiguring
        // just spins. Found live on Windows (2026-08-15) -- resizing the
        // scope window produced an unbroken flood of Outdated frames and the
        // scope never recovered, because nothing here ever reconfigured.
        //
        // Note `resize()` is still the real fix for a size change: this
        // rebuilds the swapchain at the size the config already knows,
        // which recovers a surface whose size did *not* change (a DPI or
        // monitor switch, a compositor restart) and keeps a genuine resize
        // from spinning until the caller notices.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            // Still a usable texture -- draw it, then reconfigure next frame.
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                self.needs_reconfigure = true;
                t
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(()); // frame deliberately skipped; the next one draws
            }
            // `Lost` needs the *surface* recreated, which needs the window
            // handle this type doesn't keep -- so it stays an error the
            // caller has to deal with, rather than being quietly swallowed.
            other => return Err(RenderError::SurfaceUnavailable(format!("{other:?}"))),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scope-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scope-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
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
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..points.len() as u32, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }
}
