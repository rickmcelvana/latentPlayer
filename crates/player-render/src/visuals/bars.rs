//! Spectrum bars: PLAN.md section 4's second built-in, and Phase 3 M2.
//!
//! The visual that tests the host rather than extending it. Where the
//! vectorscope draws **raw samples** geometrically, this draws **analysis** --
//! `player-core`'s own spectrum bands, the same array the studio Spectrum panel
//! shows, at the same instant (M2 decision 10). Nothing here measures anything:
//! the levels arrive in the frame already measured, off the audio thread, and
//! what this file decides is only how to look at them.
//!
//! Three display decisions, each recorded in `tasks/phase-3.md` because each
//! could reasonably have gone the other way:
//!
//! - the dB window is this visual's, not the meter's ([`RANGE_DB`], decision 13);
//! - display gain is a decibel offset here rather than a multiplier ([`gain_db`],
//!   decision 12);
//! - the peak caps are display state, timed in seconds, not a new meter
//!   ([`PEAK_FALL_DB_PER_SECOND`], decision 11).

use wgpu::util::DeviceExt;

use crate::attachments::{
    background, shader_source, target_is_srgb, targets, DEPTH_FORMAT, SAMPLE_COUNT,
};
use crate::camera::{self, Mat4};
use crate::visual::{Gpu, Visual, VisualFrame};

/// Where the camera sits, looking at the middle of the row. Slightly left of
/// centre and above, which is the classic three-quarter view of a bar chart:
/// enough angle that the boxes read as boxes and their tops are visible,
/// little enough that the far end of the row is not crushed by perspective.
///
/// The side matters here as it does for the vectorscope. From -x the camera's
/// right vector points along +x, which puts the lowest band on the left and
/// runs frequency rightwards -- the direction every spectrum display there is
/// runs in.
const EYE: [f32; 3] = [-0.6, 0.85, 2.7];
const NEAR: f32 = 0.1;
const FAR: f32 = 100.0;

/// How wide the whole row is drawn, in world units. Only the ratios between
/// this, [`HEIGHT`] and [`DEPTH`] matter: the camera is fitted to whatever the
/// three describe, so scaling all of them changes nothing.
const SPAN: f32 = 2.6;

/// How tall a full-scale bar stands.
const HEIGHT: f32 = 1.3;

/// How deep each bar is. Enough to read as a solid, little enough that the row
/// does not become a wall the front faces disappear into.
const DEPTH: f32 = 0.5;

/// How much of its slot a bar occupies, the rest being the gap to its
/// neighbour. Because the bars sit inside their slots, the row can never reach
/// past `SPAN/2` however many bands there are -- which is what lets the camera
/// be fitted once and left alone when the band count changes.
const BAR_FILL: f32 = 0.72;

/// How much of the frame a full-scale display fills, in its tighter axis. The
/// vectorscope's value, for the vectorscope's reason: near the edge, never at
/// it.
const FILL: f32 = 0.9;

/// The dB the bottom of the display means. **Not** `spectrum::FLOOR_DB`, which
/// is -120 because that is where a *measurement* should stop; a bar drawn on a
/// -120..0 axis spends more than half its height on material below the noise
/// floor of any real source, so every bar looks tall and nothing reads
/// (decision 13). The top is 0 dBFS: full scale fills the frame, exactly as it
/// does for the vectorscope.
const RANGE_DB: f32 = 72.0;

/// How fast a peak cap falls, in dB per second.
///
/// Slower than the analyser's own 30dB/s release (`player-core::spectrum`), and
/// that difference is the whole point of the caps: the bar shows where the
/// music is now, the cap remembers where it just was. Timed in *seconds* off
/// the frame's own elapsed time, so a 60Hz redraw thread and a browser's
/// animation callbacks agree -- the same discipline the automatic gain's
/// release is held to.
const PEAK_FALL_DB_PER_SECOND: f32 = 12.0;

/// How thick a cap is drawn, as a fraction of full scale.
const CAP_THICKNESS: f32 = 0.014;

/// The most bands this will draw. `analysis::BAND_COUNT` is 64 and the buffers
/// are sized once at construction; this is the headroom, not an expectation.
const MAX_BANDS: usize = 256;

/// The C's, in Hz -- equal temperament, A4 = 440. The octave marks (decision
/// 14): the reference an eye needs to read a log axis, placed by *frequency*
/// through the frame's own band centres rather than by an assumed band layout.
const OCTAVE_C_HZ: [f32; 10] = [
    32.703, 65.406, 130.813, 261.626, 523.251, 1046.502, 2093.005, 4186.009, 8372.018, 16744.036,
];

/// The uniform, as the shader sees it.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    view_proj: [[f32; 4]; 4],
    /// `[span, height, depth, bar_width]`, in world units.
    dims: [f32; 4],
    /// `[srgb_target, 0, 0, 0]` -- the surface's colour space
    /// (`attachments::target_is_srgb`). A vec4 rather than a loose float
    /// because uniform members align to sixteen bytes anyway.
    flags: [f32; 4],
}

/// One corner of the unit box every bar is an instance of. `y` runs 0 at the
/// base to 1 at the top, so scaling a bar is a multiply on one axis.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BoxVertex {
    position: [f32; 3],
    normal: [f32; 3],
}

/// One box to draw: `[x, base, size, cap]` -- see `bars.wgsl`'s `vs_bar`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance([f32; 4]);

/// A vertex of an octave mark, in the same normalised scene coordinates the
/// instances use.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex([f32; 3]);

pub struct SpectrumBars {
    bar_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    box_vertices: wgpu::Buffer,
    box_indices: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    line_buffer: wgpu::Buffer,
    scene_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// Reused across frames so a 60Hz loop allocates nothing.
    instances: Vec<Instance>,
    lines: Vec<LineVertex>,
    /// The peak caps' heights, 0..1 of full scale, one per band. **The one
    /// piece of state a visual keeps in M2** (decision 11): an animation over
    /// levels already measured, not a measurement.
    peaks: Vec<f32>,
    /// What the uniform and the octave marks were last built for. The band
    /// count decides how wide a bar is and where an octave falls, and neither
    /// is worth recomputing sixty times a second for a number that changes
    /// once a session.
    built_for: Option<(usize, f32, f32)>,
    width: u32,
    height: u32,
    srgb: f32,
}

impl SpectrumBars {
    pub(crate) fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let Gpu { device, format, .. } = *gpu;
        let srgb = target_is_srgb(format);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bars-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source(include_str!("bars.wgsl"))),
        });

        let scene_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bars-scene"),
            contents: bytemuck::bytes_of(&scene_uniform(width, height, 1, srgb)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bars-scene-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
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
            label: Some("bars-scene-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bars-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        // Two pipelines against one uniform: the boxes are instanced triangles
        // and the octave marks are lines, and they place their vertices
        // differently (a box is offset into its band's slot, a mark spans the
        // whole width). Same depth buffer, same multisampling, so the marks are
        // correctly hidden behind the bars in front of them.
        let bar_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bars-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bar"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<BoxVertex>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x3,
                            },
                            wgpu::VertexAttribute {
                                offset: 12,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x3,
                            },
                        ],
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                        }],
                    }),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bar"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // No culling: a bar of zero height is a degenerate box whose
                // winding is not worth reasoning about, and at 64 boxes the
                // saving would be unmeasurable anyway.
                cull_mode: None,
                ..Default::default()
            },
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

        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bars-line-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
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
                entry_point: Some("fs_line"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
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

        let (vertices, indices) = unit_box();
        let box_vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bars-box-vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let box_indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bars-box-indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bars-instances"),
            // Two per band: the bar and its cap.
            size: (std::mem::size_of::<Instance>() * MAX_BANDS * 2) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let line_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bars-lines"),
            size: (std::mem::size_of::<LineVertex>() * OCTAVE_C_HZ.len() * 2)
                as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (msaa_view, depth_view) = targets(device, format, width, height);
        Self {
            bar_pipeline,
            line_pipeline,
            box_vertices,
            box_indices,
            instance_buffer,
            line_buffer,
            scene_buffer,
            bind_group,
            msaa_view,
            depth_view,
            instances: Vec::with_capacity(MAX_BANDS * 2),
            lines: Vec::with_capacity(OCTAVE_C_HZ.len() * 2),
            peaks: Vec::with_capacity(MAX_BANDS),
            built_for: None,
            width,
            height,
            srgb,
        }
    }

    /// Rebuilds what depends on the band count and the frequency axis -- the
    /// uniform's bar width and the octave marks -- when either changes.
    ///
    /// Both change once a session in practice (when the output device's rate
    /// settles), so this is a comparison per frame rather than work per frame.
    fn rebuild_axis_if_needed(&mut self, queue: &wgpu::Queue, frame: &VisualFrame, bands: usize) {
        let centres = frame.band_centres;
        let key = (
            bands,
            centres.first().copied().unwrap_or(0.0),
            centres.last().copied().unwrap_or(0.0),
        );
        if self.built_for == Some(key) {
            return;
        }
        self.built_for = Some(key);
        queue.write_buffer(
            &self.scene_buffer,
            0,
            bytemuck::bytes_of(&scene_uniform(self.width, self.height, bands, self.srgb)),
        );

        self.lines.clear();
        for hz in OCTAVE_C_HZ {
            // `None` for an octave outside the display's own range, which is
            // not drawn at the edge -- a mark in the wrong place is worse than
            // no mark.
            let Some(index) = frame.band_position(hz) else {
                continue;
            };
            let x = slot_centre(index, bands);
            self.lines.push(LineVertex([x, 0.0, -0.5]));
            self.lines.push(LineVertex([x, 0.0, 0.5]));
        }
        if !self.lines.is_empty() {
            queue.write_buffer(&self.line_buffer, 0, bytemuck::cast_slice(&self.lines));
        }
    }

    /// Turns this frame's levels into boxes, advancing the peak caps.
    fn build_instances(&mut self, frame: &VisualFrame, bands: usize) {
        if self.peaks.len() != bands {
            self.peaks.clear();
            self.peaks.resize(bands, 0.0);
        }
        let offset = gain_db(frame.gain);
        let fall = (PEAK_FALL_DB_PER_SECOND / RANGE_DB) * frame.elapsed_seconds.max(0.0);

        self.instances.clear();
        for (band, level) in frame.levels[..bands].iter().enumerate() {
            let height = normalise(*level, offset);
            let x = slot_centre(band as f32, bands);
            let peak = &mut self.peaks[band];
            // Instant attack, timed release -- the shape the automatic gain
            // uses, and for the related reason: a cap that eased upwards would
            // be *below* a transient it exists to record.
            *peak = if height >= *peak {
                height
            } else {
                (*peak - fall).max(height)
            };
            if height > 0.0 {
                self.instances.push(Instance([x, 0.0, height, 0.0]));
            }
            if *peak > 0.0 {
                let base = peak.min(1.0 - CAP_THICKNESS);
                self.instances.push(Instance([x, base, CAP_THICKNESS, 1.0]));
            }
        }
    }
}

impl Visual for SpectrumBars {
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
        // would stretch the row instead of showing more of the scene.
        let bands = self.built_for.map(|(bands, ..)| bands).unwrap_or(1);
        queue.write_buffer(
            &self.scene_buffer,
            0,
            bytemuck::bytes_of(&scene_uniform(width, height, bands, self.srgb)),
        );
    }

    /// Draws one frame of bars into `target`.
    ///
    /// Of the frame this reads the levels, their axis, the gain and the elapsed
    /// time -- and never the samples. Everything it draws was measured by
    /// `player-core` before it arrived (decision 2).
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

        // The band count is the frame's to say, never `BAND_COUNT` (decision
        // 10): the web's rows are empty until its shared buffer exists, and an
        // assumed 64 would draw sixty-four bars' worth of nothing there.
        let bands = frame.levels.len().min(MAX_BANDS);
        if bands == 0 {
            // Nothing has been measured, which is a different picture from
            // silence: no bars, no caps, no marks. Same answer the vectorscope
            // gives an empty sample window.
            self.instances.clear();
            self.lines.clear();
            self.peaks.clear();
            self.built_for = None;
        } else {
            self.rebuild_axis_if_needed(queue, frame, bands);
            self.build_instances(frame, bands);
            if !self.instances.is_empty() {
                queue.write_buffer(
                    &self.instance_buffer,
                    0,
                    bytemuck::cast_slice(&self.instances),
                );
            }
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bars-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bars-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa_view,
                    resolve_target: Some(target),
                    ops: wgpu::Operations {
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
            pass.set_bind_group(0, &self.bind_group, &[]);
            if !self.lines.is_empty() {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_vertex_buffer(0, self.line_buffer.slice(..));
                pass.draw(0..self.lines.len() as u32, 0..1);
            }
            if !self.instances.is_empty() {
                pass.set_pipeline(&self.bar_pipeline);
                pass.set_vertex_buffer(0, self.box_vertices.slice(..));
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                pass.set_index_buffer(self.box_indices.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..BOX_INDICES, 0, 0..self.instances.len() as u32);
            }
        }
        encoder.finish()
    }
}

/// Where a band's slot sits across the display, -0.5 to 0.5.
///
/// `index` is fractional so an octave mark can land between two bands, which is
/// where most of them do: the C's are frequencies, and the bands are a display
/// grid that has no reason to line up with them.
fn slot_centre(index: f32, bands: usize) -> f32 {
    (index + 0.5) / bands.max(1) as f32 - 0.5
}

/// A dBFS level as a height, 0 at [`RANGE_DB`] below full scale and 1 at it.
fn normalise(level_db: f32, offset_db: f32) -> f32 {
    if !level_db.is_finite() {
        return 0.0;
    }
    ((level_db + offset_db) / RANGE_DB + 1.0).clamp(0.0, 1.0)
}

/// Display gain, as this visual applies it: **decibels** (decision 12).
///
/// `frame.gain` is an amplitude multiplier because the vectorscope draws
/// amplitudes; a bar display is logarithmic, so x2 is +6dB here. Ignoring gain
/// instead would leave one control doing something in one visual and nothing in
/// the next, which is worse for a person than either behaviour alone.
fn gain_db(gain: f32) -> f32 {
    if gain.is_finite() && gain > 0.0 {
        20.0 * gain.log10()
    } else {
        0.0
    }
}

/// Indices in [`unit_box`]: six faces, two triangles each.
const BOX_INDICES: u32 = 36;

/// The box every bar is an instance of: `x` and `z` in -0.5..0.5, `y` in 0..1
/// so that scaling the height is a multiply that leaves the base on the floor.
///
/// Four vertices per face rather than eight shared ones, because each face
/// needs its own normal -- shared corners would average the three faces meeting
/// there and the boxes would read as rounded blobs instead of solids.
fn unit_box() -> (Vec<BoxVertex>, Vec<u16>) {
    const FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
        // +z, the face towards the viewer.
        (
            [0.0, 0.0, 1.0],
            [
                [-0.5, 0.0, 0.5],
                [0.5, 0.0, 0.5],
                [0.5, 1.0, 0.5],
                [-0.5, 1.0, 0.5],
            ],
        ),
        (
            [0.0, 0.0, -1.0],
            [
                [0.5, 0.0, -0.5],
                [-0.5, 0.0, -0.5],
                [-0.5, 1.0, -0.5],
                [0.5, 1.0, -0.5],
            ],
        ),
        (
            [-1.0, 0.0, 0.0],
            [
                [-0.5, 0.0, -0.5],
                [-0.5, 0.0, 0.5],
                [-0.5, 1.0, 0.5],
                [-0.5, 1.0, -0.5],
            ],
        ),
        (
            [1.0, 0.0, 0.0],
            [
                [0.5, 0.0, 0.5],
                [0.5, 0.0, -0.5],
                [0.5, 1.0, -0.5],
                [0.5, 1.0, 0.5],
            ],
        ),
        // The top, which the camera looks down on and which therefore carries
        // most of the sense that these are solids.
        (
            [0.0, 1.0, 0.0],
            [
                [-0.5, 1.0, 0.5],
                [0.5, 1.0, 0.5],
                [0.5, 1.0, -0.5],
                [-0.5, 1.0, -0.5],
            ],
        ),
        (
            [0.0, -1.0, 0.0],
            [
                [-0.5, 0.0, -0.5],
                [0.5, 0.0, -0.5],
                [0.5, 0.0, 0.5],
                [-0.5, 0.0, 0.5],
            ],
        ),
    ];

    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, corners) in FACES {
        let base = vertices.len() as u16;
        for position in corners {
            vertices.push(BoxVertex { position, normal });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

/// The uniform for a viewport of this size and this many bands.
fn scene_uniform(width: u32, height: u32, bands: usize, srgb: f32) -> SceneUniform {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    SceneUniform {
        view_proj: view_projection(aspect).as_array(),
        dims: [SPAN, HEIGHT, DEPTH, SPAN / bands.max(1) as f32 * BAR_FILL],
        flags: [srgb, 0.0, 0.0, 0.0],
    }
}

/// The camera, fitted to the scene rather than opened to a chosen angle -- the
/// same treatment M1 gave the vectorscope, through the same [`camera::fit`].
///
/// The scene is a plain box here: the row spans `SPAN` because a bar never
/// leaves its own slot ([`BAR_FILL`]), and a full-scale bar stands `HEIGHT`.
/// So the fit depends on the aspect ratio and on nothing else -- not on the
/// band count, and not on the music.
fn view_projection(aspect: f32) -> Mat4 {
    camera::fit(
        EYE,
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        &scene_corners(),
        aspect,
        FILL,
        NEAR,
        FAR,
    )
}

/// The eight corners of the box the display lives in, centred on the origin.
/// Fitting a convex hull's vertices fits everything inside it (see
/// [`camera::fit`]), and every bar is inside this one.
fn scene_corners() -> [[f32; 3]; 8] {
    let (x, y, z) = (SPAN * 0.5, HEIGHT * 0.5, DEPTH * 0.5);
    [
        [-x, -y, -z],
        [x, -y, -z],
        [-x, y, -z],
        [x, y, -z],
        [-x, -y, z],
        [x, -y, z],
        [-x, y, z],
        [x, y, z],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest |ndc| the scene reaches, per axis, at a given viewport
    /// shape -- the same measurement the vectorscope's fit is pinned by.
    fn extent(width: u32, height: u32) -> (f32, f32) {
        let view_proj = view_projection(width as f32 / height as f32);
        let (mut x, mut y) = (0.0f32, 0.0f32);
        for corner in scene_corners() {
            let ndc = view_proj.project(corner);
            x = x.max(ndc[0].abs());
            y = y.max(ndc[1].abs());
        }
        (x, y)
    }

    /// Window shapes worth checking, from a small panel to an ultrawide
    /// fullscreen and a window taller than it is wide.
    const SHAPES: [(u32, u32); 8] = [
        (320, 320),
        (480, 480),
        (800, 600),
        (1280, 720),
        (1920, 1080),
        (2560, 1440),
        (3440, 1440),
        (600, 900),
    ];

    #[test]
    fn the_display_fills_the_frame_at_every_shape() {
        for (width, height) in SHAPES {
            let (x, y) = extent(width, height);
            let filled = x.max(y);
            assert!(
                (filled - FILL).abs() < 0.02,
                "{width}x{height} filled {filled} of the frame, not {FILL}"
            );
        }
    }

    #[test]
    fn nothing_the_display_can_draw_falls_outside_the_frame() {
        for (width, height) in SHAPES {
            let (x, y) = extent(width, height);
            assert!(x <= 1.0 && y <= 1.0, "{width}x{height} clips at ({x}, {y})");
        }
    }

    #[test]
    fn frequency_runs_left_to_right() {
        // The check that catches a display drawn backwards, which is exactly
        // the mistake the M6 smoke caught in the vectorscope's camera. Band 0
        // is the lowest frequency and must be on the left of the image.
        let view_proj = view_projection(16.0 / 9.0);
        let low = view_proj.project([slot_centre(0.0, 64) * SPAN, 0.0, 0.0]);
        let high = view_proj.project([slot_centre(63.0, 64) * SPAN, 0.0, 0.0]);
        assert!(
            low[0] < high[0],
            "the lowest band landed at x={} and the highest at x={}",
            low[0],
            high[0]
        );
    }

    #[test]
    fn every_bar_stays_inside_its_own_slot() {
        // What lets the camera be fitted once and left alone when the band
        // count changes: however many bands there are, the row never reaches
        // past SPAN/2, so the scene the fit was computed for is the scene drawn.
        for bands in [1usize, 8, 64, 200, MAX_BANDS] {
            let half_bar = 0.5 * BAR_FILL / bands as f32;
            let outermost = slot_centre(bands as f32 - 1.0, bands) + half_bar;
            assert!(
                outermost <= 0.5,
                "{bands} bands reach {outermost} past the half-span"
            );
            assert!((slot_centre(0.0, bands) - half_bar) >= -0.5);
        }
    }

    #[test]
    fn the_display_floor_is_the_visuals_own_and_not_the_meters() {
        // Decision 13: -72dB is the bottom of the picture. A level at the
        // meter's -120 floor draws no bar at all rather than a short one.
        // `player-core::spectrum::FLOOR_DB`, which this display deliberately
        // does not use as its own bottom.
        assert_eq!(normalise(-120.0, 0.0), 0.0);
        assert_eq!(normalise(-RANGE_DB, 0.0), 0.0);
        assert_eq!(normalise(0.0, 0.0), 1.0);
        // Halfway up the display is halfway down the range, which is what makes
        // the axis readable rather than merely monotonic.
        assert!((normalise(-RANGE_DB / 2.0, 0.0) - 0.5).abs() < 1e-6);
        // Above full scale is still the top: a bar cannot leave the frame.
        assert_eq!(normalise(6.0, 0.0), 1.0);
    }

    #[test]
    fn display_gain_is_decibels_here() {
        // Decision 12: the multiplier the vectorscope magnifies by is an
        // offset on a logarithmic axis -- x2 is +6dB, x8 is +18dB.
        assert!((gain_db(2.0) - 6.0206).abs() < 1e-3);
        assert!((gain_db(8.0) - 18.0618).abs() < 1e-3);
        assert_eq!(gain_db(1.0), 0.0);
        // A gain of zero or worse is not a silent display, it is a bug
        // upstream; unity is the safe reading of it.
        assert_eq!(gain_db(0.0), 0.0);
        assert_eq!(gain_db(f32::NAN), 0.0);
        // And it moves the bars the way a person would expect.
        let quiet = normalise(-24.0, 0.0);
        let lifted = normalise(-24.0, gain_db(2.0));
        assert!((lifted - quiet - 6.0206 / RANGE_DB).abs() < 1e-3);
    }
}
