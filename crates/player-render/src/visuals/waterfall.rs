//! Spectrogram waterfall: PLAN.md section 4's third built-in, and Phase 3 M3.
//!
//! The same array the bars draw, kept: the metering spectrum's last
//! [`HISTORY_SECONDS`] seconds as a terrain, frequency across, level up, time
//! running away from the viewer. Nothing here measures anything either -- the
//! levels arrive in the frame already measured, off the audio thread -- and
//! what this file decides is only how to look at them, and for how long.
//!
//! It is the first visual that keeps something worth keeping. M2's peak caps
//! are state in the small (a float per band, rebuilt into the instance buffer
//! every frame anyway); this is state that would be absurd to rebuild per
//! frame, which is the case the `Visual` trait had never been asked to carry.
//! Three decisions follow from that, all recorded in `tasks/phase-3.md`:
//!
//! - the history is a **ring of rows in a texture** read by the vertex shader,
//!   so a frame's work is one row and one integer, not one terrain (decision
//!   16);
//! - rows advance on a **clock**, never per frame ([`ROW_HZ`], decision 17) --
//!   the desktop redraws at 60Hz and a browser's animation callbacks run at
//!   whatever the display does, and a row per frame would scroll at two speeds;
//! - the texture holds **dBFS as measured**, with the display window and the
//!   display gain applied in the shader (decision 18), so changing gain re-maps
//!   the whole terrain rather than leaving a seam across it.
//!
//! The display scale itself -- what -72 dBFS means, where a band sits, where
//! the octaves are -- is [`super::scale`], shared with the bars. Two spectrum
//! visuals disagreeing about their own axis would be worse than either choice.

use wgpu::util::DeviceExt;

use crate::attachments::{
    background, shader_source, target_is_srgb, targets, DEPTH_FORMAT, SAMPLE_COUNT,
};
use crate::camera::{self, Mat4};
use crate::visual::{Gpu, Visual, VisualFrame};

use super::scale::{gain_db, slot_centre, MAX_BANDS, OCTAVE_C_HZ, RANGE_DB};

/// Where the camera sits, looking at the middle of the terrain. Higher than the
/// bars' three-quarter view and further back, because this scene has depth to
/// show: too low and the front row hides everything behind it, too high and the
/// terrain flattens into a picture of itself.
///
/// Left of centre for the reason the bars are: from -x the camera's right
/// vector points along +x, which puts the lowest band on the left and runs
/// frequency rightwards.
const EYE: [f32; 3] = [0.0, 2.0, 3.1];
const NEAR: f32 = 0.1;
const FAR: f32 = 100.0;

/// The scene's box, in world units. Only the ratios matter -- the camera is
/// fitted to whatever the three describe (see [`view_projection`]).
const SPAN: f32 = 2.6;
const HEIGHT: f32 = 1.05;
const DEPTH: f32 = 2.3;

/// How much of the frame a full-scale display fills, in its tighter axis. The
/// vectorscope's value, for the vectorscope's reason: near the edge, never at
/// it.
const FILL: f32 = 0.9;

/// How often a row is taken, in rows per second, and how many are kept.
///
/// Six seconds of history at twenty rows a second (decision 17). Both numbers
/// are chosen against pixels rather than against the analysis: a row thinner
/// than a pixel is work that cannot be seen, and the terrain's depth is a few
/// hundred pixels tall on screen even in a fullscreen window. Twenty a second
/// is also honest about what is being sampled -- the spectrum meter already
/// smooths its own release at 30dB/s, so this is a picture of a display, not a
/// transient detector.
const ROW_HZ: f32 = 20.0;
const ROWS: usize = 120;

/// How long the terrain remembers. Stated as the quantity a person would
/// recognise; the two constants above are how it is reached.
pub const HISTORY_SECONDS: f32 = ROWS as f32 / ROW_HZ;

/// Seconds between rows.
const ROW_SECONDS: f32 = 1.0 / ROW_HZ;

/// What an unwritten texel means: far below the display window, so a row that
/// has never been measured lies flat on the floor rather than standing at full
/// scale, which is what a zeroed texture would mean on a dBFS axis.
const SILENT_DB: f32 = -200.0;

/// How far the octave rails run past the terrain's near edge, as a fraction of
/// [`DEPTH`]. The part of them that is actually visible -- see `vs_rail` -- so
/// this is not a margin but the ruler itself, and 0.07 was measured to be a
/// dozen pixels of it in a 640x360 frame.
const RAIL_OVERHANG: f32 = 0.18;

/// How far below the floor the rails lie, as a fraction of [`HEIGHT`]. Enough
/// that a terrain resting at the floor does not fight them for the same pixels.
const RAIL_DROP: f32 = 0.004;

/// The uniform, as the shader sees it. See `waterfall.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    view_proj: [[f32; 4]; 4],
    /// `[span, height, depth, rail_overhang]`, in world units.
    dims: [f32; 4],
    /// `[srgb_target, bands, rows, head]`.
    grid: [f32; 4],
    /// `[range_db, gain_db, 0, 0]`.
    scale: [f32; 4],
}

/// A vertex of an octave rail, in the normalised scene coordinates the grid
/// uses.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct RailVertex([f32; 3]);

pub struct SpectrogramWaterfall {
    terrain_pipeline: wgpu::RenderPipeline,
    skirt_pipeline: wgpu::RenderPipeline,
    rail_pipeline: wgpu::RenderPipeline,
    scene_buffer: wgpu::Buffer,
    /// The history itself: dBFS, one texel per band per row, written one row at
    /// a time and never read back (decision 16).
    history: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// The terrain's triangles, in age order from the front, so that drawing
    /// only the rows that exist is a draw count rather than a second buffer.
    index_buffer: Option<wgpu::Buffer>,
    rail_buffer: wgpu::Buffer,
    rails: Vec<RailVertex>,
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// One row of dBFS, reused: a row is written as the full texture width so
    /// its size never depends on the band count.
    row: Vec<f32>,
    /// How many bands the mesh and the history are currently for. Zero until
    /// something has been measured, which is also what makes an empty feed draw
    /// nothing at all.
    bands: usize,
    /// Which texture row holds the newest measurement.
    head: usize,
    /// How many rows have been written since the last clear, capped at [`ROWS`].
    filled: usize,
    /// Time carried over towards the next row.
    pending: f32,
    /// The band count and frequency axis the mesh and rails were built for.
    built_for: Option<(usize, f32, f32)>,
    width: u32,
    height: u32,
    srgb: f32,
}

impl SpectrogramWaterfall {
    pub(crate) fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let Gpu {
            device,
            queue,
            format,
        } = *gpu;
        let srgb = target_is_srgb(format);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("waterfall-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source(include_str!("waterfall.wgsl"))),
        });

        let scene_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("waterfall-scene"),
            contents: bytemuck::bytes_of(&scene_uniform(width, height, srgb, 0, 0, 1.0)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // R32Float and `textureLoad`: the stored value is a dB figure, not a
        // colour, and nothing about it should be filtered, normalised or
        // clamped to 0..1 on the way through the GPU.
        let history = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("waterfall-history"),
            size: wgpu::Extent3d {
                width: MAX_BANDS as u32,
                height: ROWS as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let history_view = history.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("waterfall-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    // The vertex stage, which is the whole idea: the history is
                    // read where the geometry is placed, so scrolling moves an
                    // index instead of moving the terrain.
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("waterfall-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: scene_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&history_view),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("waterfall-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        // Three pipelines against one uniform and one texture, differing only in
        // topology and in how they turn a vertex index into a position: the
        // terrain is an indexed grid, the near edge's skirt is one strip, and
        // the octave rails are lines. Same depth buffer and multisampling, so
        // the rails are correctly hidden under the terrain and only their
        // overhang shows.
        let surface_target = [Some(wgpu::ColorTargetState {
            format,
            blend: Some(wgpu::BlendState::REPLACE),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let depth = Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });
        let multisample = wgpu::MultisampleState {
            count: SAMPLE_COUNT,
            ..Default::default()
        };

        let terrain_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("waterfall-terrain-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_terrain"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // No vertex buffers at all: the grid position *is* the vertex
                // index, and the height comes from the history texture. A
                // vertex buffer here would be a second copy of the rows to keep
                // in step with the first.
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_surface"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &surface_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // A terrain is a surface, and a surface seen from underneath is
                // still the surface: culling would punch holes in it wherever a
                // ridge is steeper than the camera's angle.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: depth.clone(),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        let skirt_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("waterfall-skirt-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_skirt"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_surface"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &surface_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: depth.clone(),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        let rail_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("waterfall-rail-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_rail"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RailVertex>() as wgpu::BufferAddress,
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
                entry_point: Some("fs_rail"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &surface_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: depth,
            multisample,
            multiview_mask: None,
            cache: None,
        });

        let rail_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("waterfall-rails"),
            size: (std::mem::size_of::<RailVertex>() * OCTAVE_C_HZ.len() * 2)
                as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (msaa_view, depth_view) = targets(device, format, width, height);
        let mut visual = Self {
            terrain_pipeline,
            skirt_pipeline,
            rail_pipeline,
            scene_buffer,
            history,
            bind_group,
            index_buffer: None,
            rail_buffer,
            rails: Vec::with_capacity(OCTAVE_C_HZ.len() * 2),
            msaa_view,
            depth_view,
            row: vec![SILENT_DB; MAX_BANDS],
            bands: 0,
            head: ROWS - 1,
            filled: 0,
            pending: 0.0,
            built_for: None,
            width,
            height,
            srgb,
        };
        visual.clear_history(queue);
        visual
    }

    /// Empties the history.
    ///
    /// Every texel goes to [`SILENT_DB`], not to zero: zero *is* 0 dBFS, so a
    /// cleared texture read as levels would be a wall of full scale rather than
    /// an absence.
    fn clear_history(&mut self, queue: &wgpu::Queue) {
        let blank = vec![SILENT_DB; MAX_BANDS * ROWS];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.history,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&blank),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((MAX_BANDS * 4) as u32),
                rows_per_image: Some(ROWS as u32),
            },
            wgpu::Extent3d {
                width: MAX_BANDS as u32,
                height: ROWS as u32,
                depth_or_array_layers: 1,
            },
        );
        self.head = ROWS - 1;
        self.filled = 0;
        self.pending = 0.0;
    }

    /// Writes one measurement as the newest row.
    ///
    /// Always the full texture width, padded with [`SILENT_DB`] past the band
    /// count: a row is then 1024 bytes however many bands there are, which is
    /// both a multiple of the 256-byte alignment every copy rule in WebGPU
    /// cares about and one less thing that changes when the band count does.
    fn push_row(&mut self, queue: &wgpu::Queue, levels: &[f32], bands: usize) {
        self.row.fill(SILENT_DB);
        for (slot, level) in self.row[..bands].iter_mut().zip(levels) {
            // A stored NaN would come out of `textureLoad` and into a clamp
            // whose answer is nobody's business to rely on. The floor is the
            // honest reading of a level that is not a number.
            *slot = if level.is_finite() { *level } else { SILENT_DB };
        }
        self.head = (self.head + 1) % ROWS;
        self.filled = (self.filled + 1).min(ROWS);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.history,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: self.head as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&self.row),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((MAX_BANDS * 4) as u32),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: MAX_BANDS as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Moves the history on by however many whole rows this frame's elapsed
    /// time is worth (decision 17).
    ///
    /// The first measurement of a session is written at once rather than up to
    /// a row's worth of time later: there is no history for it to be out of
    /// step with, and a visual that showed nothing for the first frames of
    /// playback would look broken rather than empty.
    fn advance(&mut self, queue: &wgpu::Queue, frame: &VisualFrame, bands: usize) {
        if self.filled == 0 {
            self.push_row(queue, frame.levels, bands);
            self.pending = 0.0;
            return;
        }
        let (rows, rest) = rows_due(self.pending + frame.elapsed_seconds.max(0.0));
        self.pending = rest;
        for _ in 0..rows {
            self.push_row(queue, frame.levels, bands);
        }
    }

    /// Rebuilds what the band count and the frequency axis decide -- the grid's
    /// triangles and the octave rails -- and clears the history when either
    /// moves (decision 22).
    ///
    /// The clear is the point rather than a side effect: rows measured against
    /// a different axis are not comparable to the ones replacing them, and
    /// keeping them would draw two different spectra as one terrain. The web's
    /// first play is exactly this transition, from nothing measured to
    /// sixty-four bands.
    fn rebuild_if_needed(&mut self, gpu: &Gpu, frame: &VisualFrame, bands: usize) {
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

        if self.bands != bands {
            self.bands = bands;
            self.index_buffer = Some(build_indices(gpu.device, bands));
        }
        self.clear_history(gpu.queue);

        self.rails.clear();
        for hz in OCTAVE_C_HZ {
            // `None` for an octave outside the display's own range, which is
            // not drawn at the edge -- a mark in the wrong place is worse than
            // no mark.
            let Some(index) = frame.band_position(hz) else {
                continue;
            };
            let x = slot_centre(index, bands);
            self.rails.push(RailVertex([x, -RAIL_DROP, -0.5]));
            self.rails
                .push(RailVertex([x, -RAIL_DROP, 0.5 + RAIL_OVERHANG]));
        }
        if !self.rails.is_empty() {
            gpu.queue
                .write_buffer(&self.rail_buffer, 0, bytemuck::cast_slice(&self.rails));
        }
    }
}

impl Visual for SpectrogramWaterfall {
    fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        let Gpu { device, format, .. } = *gpu;
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        let (msaa_view, depth_view) = targets(device, format, width, height);
        self.msaa_view = msaa_view;
        self.depth_view = depth_view;
        // The projection carries the aspect ratio, and it is written fresh on
        // every frame that draws anything -- so unlike the bars there is
        // nothing to correct here. A resize while nothing is being drawn is a
        // resize of an empty frame.
    }

    /// Draws one frame of the terrain into `target`.
    ///
    /// Of the frame this reads the levels, their axis, the gain and the elapsed
    /// time -- and never the samples. Everything it draws was measured by
    /// `player-core` before it arrived (decision 2), and everything it
    /// remembers was measured the same way.
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

        // The band count is the frame's to say, never `BAND_COUNT` (M2 decision
        // 10). Two is the smallest number that can make a surface, and also the
        // smallest a frequency axis can be read from.
        let bands = frame.levels.len().min(MAX_BANDS);
        if bands >= 2 {
            self.rebuild_if_needed(gpu, frame, bands);
            self.advance(queue, frame, bands);
        }
        // Decision 21: with nothing to measure the terrain holds still rather
        // than scrolling silence into six seconds of real history. Nothing
        // advanced above, so this frame draws exactly what the last one did.

        if self.filled > 0 {
            // Rewritten every frame rather than on change: `head` moves with
            // the history and `gain` moves with the setting, and a uniform this
            // size is cheaper to write than to reason about.
            queue.write_buffer(
                &self.scene_buffer,
                0,
                bytemuck::bytes_of(&scene_uniform(
                    self.width,
                    self.height,
                    self.srgb,
                    self.bands,
                    self.head,
                    frame.gain,
                )),
            );
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("waterfall-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("waterfall-pass"),
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
            // Nothing measured yet is nothing drawn -- not a flat floor, which
            // would be a picture of silence (decision 21, M2 decision 10's
            // rule).
            if self.filled > 0 && self.bands >= 2 {
                pass.set_bind_group(0, &self.bind_group, &[]);
                if !self.rails.is_empty() {
                    pass.set_pipeline(&self.rail_pipeline);
                    pass.set_vertex_buffer(0, self.rail_buffer.slice(..));
                    pass.draw(0..self.rails.len() as u32, 0..1);
                }
                // The near edge, which is also what a single measurement draws:
                // a surface needs two rows before it has a triangle.
                pass.set_pipeline(&self.skirt_pipeline);
                pass.draw(0..(self.bands * 2) as u32, 0..1);

                if let (Some(indices), true) = (self.index_buffer.as_ref(), self.filled >= 2) {
                    // The indices run in age order from the front, so the rows
                    // that exist are a prefix of them: how much history there
                    // is becomes a draw count rather than a second buffer.
                    let count = ((self.filled - 1) * (self.bands - 1) * 6) as u32;
                    pass.set_pipeline(&self.terrain_pipeline);
                    pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..count, 0, 0..1);
                }
            }
        }
        encoder.finish()
    }
}

/// How many whole rows a run of seconds is worth, and what is left over
/// towards the next one (decision 17).
///
/// The leftover is what makes this a clock rather than a frame counter: sixty
/// frames of a sixtieth of a second have to move the history exactly as far as
/// one frame of a whole second, or the terrain scrolls at the redraw rate --
/// which is 60Hz on the desktop and whatever the display does on the web.
///
/// The cap is the other half. A gap longer than the history is a whole new
/// terrain, and a machine resumed from sleep should arrive there in one frame
/// rather than by writing every row it slept through.
fn rows_due(pending: f32) -> (usize, f32) {
    if !pending.is_finite() || pending < ROW_SECONDS {
        return (0, pending.max(0.0));
    }
    let whole = (pending / ROW_SECONDS).floor();
    (
        whole.min(ROWS as f32) as usize,
        pending - whole * ROW_SECONDS,
    )
}

/// The terrain's triangles, in **age order from the front**.
///
/// That order is what lets a partly filled history be a draw count: the rows
/// that exist are a prefix of these indices, so the terrain grows backwards as
/// history accumulates instead of being drawn against rows that were never
/// measured.
fn grid_indices(bands: usize) -> Vec<u32> {
    let mut indices = Vec::with_capacity((ROWS - 1) * bands.saturating_sub(1) * 6);
    for age in 0..ROWS - 1 {
        for band in 0..bands.saturating_sub(1) {
            let near = (age * bands + band) as u32;
            let far = near + bands as u32;
            indices.extend_from_slice(&[near, near + 1, far + 1, near, far + 1, far]);
        }
    }
    indices
}

fn build_indices(device: &wgpu::Device, bands: usize) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("waterfall-indices"),
        contents: bytemuck::cast_slice(&grid_indices(bands)),
        usage: wgpu::BufferUsages::INDEX,
    })
}

/// The uniform for a viewport of this size, this many bands, and this much
/// history behind it.
fn scene_uniform(
    width: u32,
    height: u32,
    srgb: f32,
    bands: usize,
    head: usize,
    gain: f32,
) -> SceneUniform {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    SceneUniform {
        view_proj: view_projection(aspect).as_array(),
        dims: [SPAN, HEIGHT, DEPTH, RAIL_OVERHANG],
        grid: [srgb, bands as f32, ROWS as f32, head as f32],
        // Decision 18: the window and the gain travel to the shader, where they
        // are applied to every row at once -- rather than to the history, where
        // they would be applied to whichever rows happened to be written while
        // the setting was in force.
        scale: [RANGE_DB, gain_db(gain), 0.0, 0.0],
    }
}

/// The camera, fitted to the scene rather than opened to a chosen angle -- the
/// same treatment M1 gave the vectorscope and M2 the bars, through the same
/// [`camera::fit`].
///
/// The scene is a fixed box: the terrain spans `SPAN` because a band never
/// leaves its slot, stands `HEIGHT` at full scale, and runs `DEPTH` back
/// because that is where the oldest row is. So the fit depends on the aspect
/// ratio and on nothing else -- not on the band count, not on how much history
/// there is, and not on the music.
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

/// The eight corners of the box the display lives in. Fitting a convex hull's
/// vertices fits everything inside it (see [`camera::fit`]), and everything
/// this visual draws is inside this one -- including the octave rails, which
/// reach past the terrain's near edge and below its floor and would otherwise
/// be the one thing able to leave the frame.
fn scene_corners() -> [[f32; 3]; 8] {
    let x = SPAN * 0.5;
    let top = HEIGHT * 0.5;
    let bottom = -HEIGHT * (0.5 + RAIL_DROP);
    let back = -DEPTH * 0.5;
    let front = DEPTH * (0.5 + RAIL_OVERHANG);
    [
        [-x, bottom, back],
        [x, bottom, back],
        [-x, top, back],
        [x, top, back],
        [-x, bottom, front],
        [x, bottom, front],
        [-x, top, front],
        [x, top, front],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest |ndc| the scene reaches, per axis, at a given viewport shape
    /// -- the measurement the vectorscope's and the bars' fits are both pinned
    /// by.
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
    fn frequency_runs_left_to_right_at_both_ends_of_the_history() {
        // The bars' check, made twice: a terrain is seen in perspective, so
        // "the lowest band is on the left" has to hold at the near edge and at
        // the far one. Both use `scale::slot_centre`, which is why the two
        // spectrum visuals cannot disagree about which side a tone is on.
        let view_proj = view_projection(16.0 / 9.0);
        for z in [DEPTH * 0.5, -DEPTH * 0.5] {
            let low = view_proj.project([slot_centre(0.0, 64) * SPAN, 0.0, z]);
            let high = view_proj.project([slot_centre(63.0, 64) * SPAN, 0.0, z]);
            assert!(
                low[0] < high[0],
                "at z={z} the lowest band landed at x={} and the highest at x={}",
                low[0],
                high[0]
            );
        }
    }

    #[test]
    fn time_runs_away_from_the_viewer() {
        // The newest row is at the front. Getting this backwards would draw the
        // future arriving from the horizon, which reads as plausible right up
        // until a transient is watched.
        let view = camera::look_at(EYE, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let newest = view.project([0.0, 0.0, DEPTH * 0.5]);
        let oldest = view.project([0.0, 0.0, -DEPTH * 0.5]);
        assert!(
            -newest[2] < -oldest[2],
            "the newest row is {} from the camera and the oldest {}",
            -newest[2],
            -oldest[2]
        );
    }

    #[test]
    fn the_history_is_as_long_as_it_says_it_is() {
        assert!((HISTORY_SECONDS - 6.0).abs() < 1e-6);
        assert!((ROWS as f32 * ROW_SECONDS - HISTORY_SECONDS).abs() < 1e-4);
    }

    #[test]
    fn rows_advance_on_the_clock_and_not_on_frames() {
        // Decision 17, as arithmetic. One long frame:
        let (rows, rest) = rows_due(1.0);
        assert_eq!(rows, ROW_HZ as usize);
        assert!(rest.abs() < 1e-4, "a whole second left {rest} over");

        // ...and the same second in sixty short ones, which is the difference
        // between the two shells' redraw loops.
        let (mut total, mut pending) = (0usize, 0.0f32);
        for _ in 0..60 {
            let (rows, rest) = rows_due(pending + 1.0 / 60.0);
            total += rows;
            pending = rest;
        }
        assert_eq!(total, ROW_HZ as usize, "sixty frames moved {total} rows");

        // A frame shorter than a row moves nothing and keeps the remainder,
        // which is what stops a fast display from rounding time away.
        let (none, kept) = rows_due(ROW_SECONDS / 4.0);
        assert_eq!(none, 0);
        assert!((kept - ROW_SECONDS / 4.0).abs() < 1e-6);

        // And a machine that was asleep arrives at a whole new terrain in one
        // frame rather than writing every row it slept through.
        assert_eq!(rows_due(600.0).0, ROWS);
    }

    #[test]
    fn the_rows_that_exist_are_a_prefix_of_the_terrains_triangles() {
        // What lets "how much history is there" be a draw count. Everything in
        // the first `(filled - 1)` rows' worth of indices has to live inside
        // those `filled` rows of the grid, or a young history would be drawn
        // against rows nothing has ever written.
        let bands = 8;
        let indices = grid_indices(bands);
        assert_eq!(indices.len(), (ROWS - 1) * (bands - 1) * 6);
        for filled in [2usize, 3, 17, ROWS] {
            let count = (filled - 1) * (bands - 1) * 6;
            let highest = indices[..count].iter().copied().max().expect("indices");
            assert!(
                (highest as usize) < filled * bands,
                "{filled} rows of history reach vertex {highest}, past row {}",
                highest as usize / bands
            );
        }
    }

    #[test]
    fn the_uniform_carries_the_display_scale_rather_than_the_history() {
        // Decision 18: gain reaches the shader, not the texture. The check that
        // the whole terrain moves together when the setting changes -- there is
        // one number for every row, so there is nowhere for a seam to be.
        let unity = scene_uniform(640, 360, 0.0, 64, 3, 1.0);
        let doubled = scene_uniform(640, 360, 0.0, 64, 3, 2.0);
        assert_eq!(unity.scale[0], RANGE_DB);
        assert_eq!(unity.scale[1], 0.0);
        assert!((doubled.scale[1] - 6.0206).abs() < 1e-3);
        // And the grid the shader indexes with is the frame's band count and
        // this visual's own row count, never an assumption about either.
        assert_eq!(unity.grid[1], 64.0);
        assert_eq!(unity.grid[2], ROWS as f32);
        assert_eq!(unity.grid[3], 3.0);
    }
}
