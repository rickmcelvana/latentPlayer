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

use crate::attachments::{
    background, shader_source, target_is_srgb, targets, DEPTH_FORMAT, SAMPLE_COUNT,
};
use crate::camera::{self, Mat4};
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
const NEAR: f32 = 0.1;
const FAR: f32 = 100.0;

/// How large the figure is drawn, in world units, at full scale. Only its ratio
/// to [`DEPTH`] matters to the picture: the camera is fitted to whatever these
/// two describe (see [`view_projection`]), so scaling both changes nothing.
const FIGURE_SCALE: f32 = 0.6;

/// How far the trace's fixed 120ms of history reaches into the scene, in world
/// units, in a square viewport.
///
/// How *long* to draw a fixed duration is a display decision, the same kind as
/// how large to draw a given amplitude -- the measurement is the 120ms, which
/// does not change (M6 decision 1). See [`depth_for`] for what a wide viewport
/// does with it.
const DEPTH: f32 = 2.0;

/// How much of the frame the figure fills at full scale, in its tighter axis.
///
/// The margin is small on purpose: this is a measurement display, so a
/// full-scale signal should reach *near* the edge and nothing should ever reach
/// it. What is fitted is the trace's own reachable extent, so 0.9 means a
/// full-scale mono tone stands 90% of the way up the frame.
const FILL: f32 = 0.9;

/// The camera uniform, as the shader sees it.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    /// `[figure_scale, depth, gain, srgb_target]`. The world transform lives in the shader
    /// but its numbers come from here, because the camera is *fitted* to the
    /// first two -- a shader constant and a Rust constant drifting apart would be
    /// a camera framing a scene of a different size than the one drawn.
    ///
    /// `gain` is deliberately *not* part of the fit. That is the whole point of
    /// it: the frame stays pinned to unity full scale, and gain magnifies the
    /// trace inside that frame. A camera that refitted itself to the gained
    /// figure would leave the picture identical and the control useless.
    ///
    /// `srgb_target` is the surface's colour space, which the *fragment* stage
    /// needs for the same reason the clear colour does -- see
    /// `attachments::target_is_srgb` and `visuals/shared.wgsl`.
    figure: [f32; 4],
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
    /// The gain the uniform currently holds, so an unchanged one is not
    /// rewritten. `f32::NAN` would compare unequal forever; 1.0 is what the
    /// buffer is built with.
    gain: f32,
    /// The surface's colour space, as the shader's flag. Fixed for this
    /// visual's life: swapping visuals never swaps the surface (decision 5).
    srgb: f32,
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
            source: wgpu::ShaderSource::Wgsl(shader_source(include_str!("vectorscope.wgsl"))),
        });

        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vectorscope-camera"),
            contents: bytemuck::bytes_of(&camera_uniform(
                width,
                height,
                1.0,
                target_is_srgb(format),
            )),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vectorscope-camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The fragment stage reads the surface's colour space out of
                // the same uniform (`figure.w`).
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let mut scene = Self {
            pipeline,
            vertex_buffer,
            camera_buffer,
            bind_group,
            msaa_view,
            depth_view,
            vertices: Vec::with_capacity(TRACE_POINTS),
            width,
            height,
            gain: 1.0,
            srgb: target_is_srgb(format),
        };
        scene.write_camera(queue, 1.0);
        scene
    }

    /// Writes the camera and the world transform, if either changed.
    ///
    /// Gain can change every frame (the automatic mode follows the music), so
    /// this is checked per frame rather than only on resize -- but it is eighty
    /// bytes, and skipping the unchanged case keeps a 60Hz redraw loop from
    /// queueing a write it does not need.
    fn write_camera(&mut self, queue: &wgpu::Queue, gain: f32) {
        if gain == self.gain {
            return;
        }
        self.gain = gain;
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&camera_uniform(self.width, self.height, gain, self.srgb)),
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
        // would stretch the figure instead of showing more of the scene. Forced,
        // since the gain has not changed and `write_camera` would otherwise
        // decline to write the new projection.
        let gain = self.gain;
        self.gain = f32::NAN;
        self.write_camera(queue, gain);
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
        let Gpu {
            device,
            queue,
            format,
        } = *gpu;
        self.write_camera(queue, frame.gain);
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
                        // Asked of the surface's own colour space rather than
                        // written as a number: the same constant drew near-black
                        // on one shell and grey on the other until this did
                        // (see `attachments::background`).
                        load: wgpu::LoadOp::Clear(background(format)),
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

/// The camera and the world transform for a viewport of this size -- written
/// together because the first is fitted to the second.
fn camera_uniform(width: u32, height: u32, gain: f32, srgb: f32) -> CameraUniform {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    CameraUniform {
        view_proj: view_projection(width, height).as_array(),
        figure: [FIGURE_SCALE, depth_for(aspect), gain, srgb],
    }
}

/// How long the ribbon is drawn, in world units.
///
/// A constant, and deliberately so after measuring the alternative. Phase 3 M1's
/// first answer to "the figure is barely a fraction of the screen" was to
/// lengthen the ribbon on a wide viewport, so the spare room went to the time
/// axis. It made the picture *worse*, for a reason worth writing down: a longer
/// ribbon reaches further towards the camera, the fit below must then open wide
/// enough to hold that near end, and everything behind it shrinks. Measured at
/// 1280x720, the mono figure went from 442px tall to 335px. The near end also
/// ends up close enough to the eye to exaggerate the perspective, which reads as
/// a lens effect rather than as depth.
///
/// Pulling the camera back instead flattens that perspective and does help --
/// but only from 57% to 65% of the frame at a realistic distance, in exchange
/// for the sense of depth that is the whole point of a 3D scope. Neither is the
/// real lever, which is amplitude: see [`FILL`].
fn depth_for(_aspect: f32) -> f32 {
    DEPTH
}

/// The furthest the trace can reach in world space -- the corners of the scene
/// the camera has to frame.
///
/// Not a box. The trace's x is the side signal and its y the mid, both scaled by
/// `1/sqrt(2)`, so from |left| and |right| <= 1 it follows that |x| + |y| <=
/// sqrt(2): the reachable region is a *diamond* with vertices on the axes, not a
/// square with corners between them. Framing the square would waste a factor of
/// sqrt(2) of the frame on positions no signal can occupy. Full-scale mono sits
/// at one vertex (0, sqrt(2)), fully out-of-phase at another (sqrt(2), 0), which
/// is exactly what should touch the edge.
///
/// Those four vertices at each end of the ribbon are the eight corners of a
/// convex solid, and a perspective projection maps a convex hull's extremes to
/// its projected extremes -- so fitting these eight fits every point between
/// them.
fn scene_corners(depth: f32) -> [[f32; 3]; 8] {
    let a = FIGURE_SCALE * std::f32::consts::SQRT_2;
    let z = depth * 0.5;
    [
        [a, 0.0, z],
        [-a, 0.0, z],
        [0.0, a, z],
        [0.0, -a, z],
        [a, 0.0, -z],
        [-a, 0.0, -z],
        [0.0, a, -z],
        [0.0, -a, -z],
    ]
}

/// The view-projection for a viewport of this size, fitted to the scene.
///
/// The field of view is *derived* rather than chosen (Phase 3 M1, after the
/// fullscreen report): the camera opens exactly wide enough to hold
/// [`scene_corners`] at [`FILL`] of the frame, in whichever axis binds first. A
/// fixed field of view instead framed a fixed angle, which is why the figure
/// held its share of the *height* and left a widescreen mostly empty -- and why
/// a taller-than-wide window used to clip nothing but showed the figure small.
///
/// The arithmetic is [`camera::fit`]'s, which is where it moved when M2's bars
/// became its second caller. What stays here is the part that is the
/// vectorscope's own: where the eye sits, and which eight points a signal can
/// actually reach.
fn view_projection(width: u32, height: u32) -> Mat4 {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    camera::fit(
        EYE,
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        &scene_corners(depth_for(aspect)),
        aspect,
        FILL,
        NEAR,
        FAR,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest |ndc| the scene reaches, per axis, at a given viewport shape.
    fn extent(width: u32, height: u32) -> (f32, f32) {
        let aspect = width as f32 / height as f32;
        let view_proj = view_projection(width, height);
        let (mut x, mut y) = (0.0f32, 0.0f32);
        for corner in scene_corners(depth_for(aspect)) {
            let ndc = view_proj.project(corner);
            x = x.max(ndc[0].abs());
            y = y.max(ndc[1].abs());
        }
        (x, y)
    }

    /// Every shape a window can be, including the ones a person actually makes:
    /// square, the 480px default, a widescreen, a fullscreen 16:9 and 16:10, an
    /// ultrawide, and a window dragged taller than it is wide.
    const SHAPES: [(u32, u32); 8] = [
        (320, 320),
        (480, 480),
        (800, 600),
        (1280, 720),
        (1920, 1200),
        (2560, 1440),
        (3440, 1440),
        (600, 900),
    ];

    #[test]
    fn the_figure_fills_the_frame_at_every_shape() {
        // The bug this pins, reported live 2026-08-18: fullscreen drew the
        // figure "barely a fraction of the screen", because the field of view
        // was a fixed angle and framed a fixed share of the *height* whatever
        // the width was. The fit makes the tighter axis land on FILL by
        // construction, so what is asserted is that it really does -- at every
        // shape, and not by luck at one of them.
        for (width, height) in SHAPES {
            let (x, y) = extent(width, height);
            let filled = x.max(y);
            assert!(
                (filled - FILL).abs() < 0.01,
                "{width}x{height}: fills {filled:.3} of the frame, wanted {FILL}"
            );
        }
    }

    #[test]
    fn nothing_a_signal_can_reach_falls_outside_the_frame() {
        // The other half of the same claim, and the one that matters for a
        // measurement: a full-scale signal must be *near* the edge and never
        // past it, at any shape. Clipping is the failure that would make the
        // scope lie rather than merely look wrong.
        for (width, height) in SHAPES {
            let (x, y) = extent(width, height);
            assert!(
                x <= 1.0 && y <= 1.0,
                "{width}x{height}: reaches {x:.3},{y:.3} in NDC -- outside the frame"
            );
        }
    }

    #[test]
    fn the_ribbon_is_the_same_length_whatever_the_window_shape() {
        // See `depth_for`: stretching it by aspect was tried, measured, and
        // reverted. The duration was never in question -- that is
        // `trace::window_samples`, which does not consult the viewport at all.
        for aspect in [0.5, 1.0, 16.0 / 9.0, 32.0 / 9.0] {
            assert_eq!(depth_for(aspect), DEPTH);
        }
    }

    #[test]
    fn widening_a_window_never_shrinks_the_figure() {
        // The measurement guarantee, and the reason `depth_for` gives the height
        // to the amplitude axes *before* the time axis takes the width. A first
        // cut of this fix lengthened the ribbon by aspect ratio alone, and the
        // longer scene pushed the vertical fit *down*: the same master read as a
        // smaller figure at 16:9 than at 1:1 (measured 385px against a previous
        // 442px). A scope may not do that -- pulling a window wider must never
        // make the signal look quieter.
        let mut previous = 0.0f32;
        for width in [720, 800, 1000, 1280, 1600, 1920, 2560, 3440] {
            let (_, y) = extent(width, 720);
            assert!(
                y >= previous - 0.001,
                "{width}x720: full scale reaches {y:.3} of the height, down from {previous:.3}"
            );
            previous = y;
        }
        // And once there is spare width, the height is what binds -- the
        // amplitude axes have taken it, and the ribbon lengthened into what was
        // left rather than into what they needed.
        for (width, height) in [(1280, 720), (1920, 1200), (2560, 1440)] {
            let (_, y) = extent(width, height);
            assert!(
                (y - FILL).abs() < 0.01,
                "{width}x{height}: full scale reaches {y:.3} of the height, wanted {FILL}"
            );
        }
    }

    #[test]
    fn the_camera_frames_the_scene_the_shader_actually_draws() {
        // The uniform carries the two numbers the fit was computed from, which
        // is the whole reason they are not shader constants: a copy in the WGSL
        // that drifted from the Rust would frame a scene of a different size
        // than the one being drawn, and nothing would report it.
        let uniform = camera_uniform(1280, 720, 1.0, 0.0);
        assert_eq!(uniform.figure[0], FIGURE_SCALE);
        assert_eq!(uniform.figure[1], depth_for(1280.0 / 720.0));
        assert_eq!(uniform.figure[2], 1.0);
        // And gain rides along without touching the projection, which is what
        // keeps the frame pinned to unity while the trace inside it grows.
        let gained = camera_uniform(1280, 720, 4.0, 0.0);
        assert_eq!(gained.figure[2], 4.0);
        assert_eq!(gained.view_proj, uniform.view_proj);
    }

    #[test]
    fn a_degenerate_viewport_still_produces_a_usable_camera() {
        // A window dragged to nothing, or a canvas sized before layout: the
        // renderer already refuses to resize to zero, but the fit must not
        // produce NaNs if it ever sees one.
        for (width, height) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let matrix = view_projection(width, height);
            assert!(
                matrix.as_array().iter().flatten().all(|v| v.is_finite()),
                "{width}x{height} produced a non-finite view-projection"
            );
        }
    }
}
