//! Oscilloscope tunnel: PLAN.md section 4's fourth built-in, and Phase 3 M4.
//!
//! The waveform swept along a camera path -- the modern reading of the Winamp
//! oscilloscope. Its input is the vectorscope's: the same window of played
//! samples the tap already hands over, wrapped around a circle one ring at a
//! time and laid down along a curve the eye flies through.
//!
//! It is the first visual in this crate whose **camera moves**, which turns two
//! of the crate's habits inside out (`tasks/phase-3.md` decisions 23-30):
//!
//! - [`camera::fit`] is not used, and its absence is the milestone. The fit asks
//!   how narrow a projection holds a hull at 90% of the frame -- a question
//!   about a scene seen from outside, with corners that can leave the frame.
//!   Inside a tube there are no corners and the failure is the opposite one:
//!   seeing *past* the scene at the edge of a wide window. So the field of view
//!   is chosen and the scene is fitted to it, provably ([`encloses`]).
//! - The path is a pure function of ring index ([`path_at`]), evaluated in Rust
//!   for the camera *and* for the geometry, which is handed over already
//!   computed. One implementation, so the eye cannot end up outside the tube it
//!   is flying down.
//!
//! History is [`RingHistory`] -- the waterfall's mechanism, extracted in this
//! milestone when it turned out to have a second caller. The feed is 120ms
//! (`trace::TRACE_SECONDS`) and lengthening it would lengthen the vectorscope's
//! ribbon and touch both shells to do it, so a tube's worth of past belongs to
//! the tube.

use wgpu::util::DeviceExt;

use crate::attachments::{
    background, shader_source, target_is_srgb, targets, BACKGROUND, DEPTH_FORMAT, SAMPLE_COUNT,
};
use crate::camera;
use crate::visual::{Gpu, Musical, Visual, VisualFrame};

use super::history::RingHistory;

/// Rings a second, and how many are kept: four seconds of tunnel.
///
/// One ring per 60Hz frame at the desktop's redraw rate, which is the densest
/// the feed can honestly support -- a ring is a picture of the tap's window, and
/// two rings closer together than the tap moves would be the same picture twice.
const RING_HZ: f32 = 60.0;
const RINGS: usize = 240;

/// How long the tunnel remembers. The figure a person would recognise; the two
/// constants above are how it is reached.
pub const TUNNEL_SECONDS: f32 = RINGS as f32 / RING_HZ;

/// Points around a ring. Chosen with two constraints in mind: enough that a
/// waveform is a shape rather than a polygon, and such that a row of two-channel
/// texels (`96 * 2 * 4` bytes) is a multiple of 256 and therefore clear of every
/// texture-copy alignment rule there is.
const RING_POINTS: usize = 96;

/// The tube's resting radius, and how far a full-scale sample pushes the wall
/// out from it before display gain.
const BASE_RADIUS: f32 = 1.0;
const WAVE_DEPTH: f32 = 0.55;

/// The clamp of decision 28: the wall may never reach the axis, and may not
/// balloon away either. Full scale at x8 gain would otherwise close the tube
/// around the eye -- a screenful of flat colour, which reads as a bug rather
/// than as loud music.
const MIN_SCALE: f32 = 0.35;
const MAX_SCALE: f32 = 2.5;

/// How far the beat may push the eye along its own path, in rings.
///
/// M4 deferred a beat-reactive camera to M6 rather than building half a beat
/// detector inside a visual (decision 27); this is it, and the shape of it is
/// decision 48. The obvious implementation -- speeding the ring clock on the
/// beat -- is wrong: the clock is what makes a ring a picture of a fixed slice
/// of audio, so changing it would let the beat alter the *measurement* and
/// quietly change how many seconds the tube holds.
///
/// So the beat moves the eye along the path it was already on, by at most this
/// many rings, and by exactly zero at every beat instant. The eye surges after
/// a beat and eases back into the next one; it cannot drift, because the offset
/// is a function of the phase rather than an accumulation, and it cannot leave
/// the tube backwards, because the offset is never negative.
///
/// Two rings is about a fifth of the flight speed at 120 BPM. Deliberately
/// modest: M4's live look confirmed this visual causes no discomfort, and a
/// camera that surges hard is exactly how that answer would change.
const BEAT_SURGE_RINGS: f32 = 2.0;

/// World units between rings. With [`RING_HZ`] this is also the speed: the eye
/// covers `RING_HZ * RING_SPACING` units a second, which is 3.6 -- a little over
/// three radii a second, and the one number the live look is most likely to want
/// changed.
const RING_SPACING: f32 = 0.06;

/// How far the side signal twists a point round its ring, in radians at full
/// scale.
const TWIST: f32 = 0.35;

/// The camera's angles. A chosen field of view, because the scene is fitted to
/// the camera here rather than the other way round (decision 24).
///
/// 1.4 radians -- about 80 degrees, which is wide for this crate and right for
/// this visual: inside a tube, the field of view decides how much of the *near*
/// wall is in frame, and a narrow cone shows only the far half of the tunnel,
/// where everything is small, slow and fogged. Measured rather than guessed:
/// at 60 degrees the near wall sits outside the frustum entirely and the
/// picture reads as a gradient with a hole in it.
const FOV_Y: f32 = 1.4;
const NEAR: f32 = 0.02;
const FAR: f32 = 60.0;

/// How far ahead the eye looks, in rings. Far enough that a bend turns the view
/// before it arrives, near enough that the tube does not appear to be steered
/// from somewhere else.
const LOOK_AHEAD: f32 = 24.0;

/// The path's shape: how far it wanders in x and y, and how many times it does
/// so in [`PATH_PERIOD`] rings.
///
/// The cycle counts are a **curvature** budget rather than a taste: bend a tube
/// faster than about a radius per hundred rings and it curves out of its own
/// sight, so the far end is hidden behind the near wall and there is no tunnel
/// to look down -- only a lit surface. Three and two over an hour's worth of
/// rings leave a visible drift at the far end and a straight enough near half
/// to see through. The amplitudes then only decide how far the drift goes.
///
/// The period is a whole number of rings *and* a whole number of cycles, which
/// is what lets the ring counter be taken modulo it without a seam: the sines
/// are exactly periodic there, so wrapping is invisible and an `f32` never has
/// to hold the minutes-long count that the tunnel actually has behind it.
const PATH_PERIOD: f32 = 3600.0;
const BEND_X: f32 = 0.9;
const BEND_Y: f32 = 0.6;
const BEND_CYCLES_X: f32 = 3.0;
const BEND_CYCLES_Y: f32 = 2.0;

/// How many rings ahead the fog has faded to the background.
const FADE_RINGS: f32 = 190.0;

/// The uniform, as the shader sees it. See `tunnel.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    view_proj: [[f32; 4]; 4],
    /// `[base_radius, wave_depth * gain, min_scale, max_scale]`.
    shape: [f32; 4],
    /// `[srgb_target, points, filled, head]`.
    grid: [f32; 4],
    /// `[background r, g, b, fade_rings]`.
    fog: [f32; 4],
    /// `[twist, rings, 0, 0]`.
    extra: [f32; 4],
    /// Each ring's centre relative to the eye, `w` being how many rings ahead.
    centres: [[f32; 4]; RINGS],
}

pub struct OscilloscopeTunnel {
    pipeline: wgpu::RenderPipeline,
    scene_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    history: RingHistory,
    bind_group: wgpu::BindGroup,
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// One ring's worth of `[mid, side]` pairs, reused so a 60Hz loop allocates
    /// nothing.
    ring: Vec<f32>,
    /// How many rings have ever been laid. The camera's position along the path
    /// is derived from this, and it is a `u64` because a tunnel left running for
    /// a day is 5 million rings and an `f32` stops counting them long before
    /// that.
    created: u64,
    width: u32,
    height: u32,
    srgb: f32,
}

impl OscilloscopeTunnel {
    pub(crate) fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let Gpu {
            device,
            queue,
            format,
        } = *gpu;
        let srgb = target_is_srgb(format);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tunnel-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source(include_str!("tunnel.wgsl"))),
        });

        let history = RingHistory::new(
            device,
            "tunnel-history",
            wgpu::TextureFormat::Rg32Float,
            RING_POINTS,
            RINGS,
            RING_HZ,
            0.0,
        );

        let scene_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tunnel-scene"),
            size: std::mem::size_of::<SceneUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tunnel-layout"),
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
            label: Some("tunnel-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: scene_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(history.view()),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tunnel-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tunnel-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_tube"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_tube"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // The eye is inside the tube, so what it sees is the far side of
                // every ring: culling here would cull the whole visual.
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

        // Built once and never rebuilt: unlike the waterfall's grid, this one's
        // shape is two constants rather than the frame's band count, so there is
        // nothing a feed can do to change it.
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tunnel-indices"),
            contents: bytemuck::cast_slice(&tube_indices()),
            usage: wgpu::BufferUsages::INDEX,
        });

        let (msaa_view, depth_view) = targets(device, format, width, height);
        let mut visual = Self {
            pipeline,
            scene_buffer,
            index_buffer,
            history,
            bind_group,
            msaa_view,
            depth_view,
            ring: vec![0.0; RING_POINTS * 2],
            created: 0,
            width,
            height,
            srgb,
        };
        visual.history.clear(queue);
        visual
    }

    /// Turns this frame's window of samples into one ring.
    ///
    /// The whole window goes round the circle, decimated to [`RING_POINTS`] --
    /// so a ring is the vectorscope's own ribbon, closed into a loop, and two
    /// rings a sixtieth of a second apart share most of their shape. That
    /// overlap is what draws the ridges that run along the tube.
    ///
    /// Every nth frame rather than an average of each group, for `trace::build`'s
    /// reason: averaging pulls a wide signal towards the centre and would
    /// quietly make every mix look more mono than it is.
    fn build_ring(&mut self, frame: &VisualFrame) -> bool {
        let channels = usize::from(frame.channels.max(1));
        let frames = frame.samples.len() / channels;
        if frames == 0 {
            return false;
        }
        let last = frames.saturating_sub(1).max(1);
        let scale = std::f32::consts::FRAC_1_SQRT_2;
        for point in 0..RING_POINTS {
            let index = (point * last) / RING_POINTS.saturating_sub(1).max(1);
            let base = index.min(last) * channels;
            let left = frame.samples[base];
            // A mono output has no stereo field: every point sits on the ring
            // where the waveform puts it, with no twist at all.
            let right = if channels > 1 {
                frame.samples[base + 1]
            } else {
                left
            };
            self.ring[point * 2] = (left + right) * scale;
            self.ring[point * 2 + 1] = (left - right) * scale;
        }
        true
    }

    /// Lays down however many rings this frame's elapsed time is worth.
    ///
    /// The first window of a session fills the **whole** tunnel rather than
    /// laying one ring, and that is a real decision rather than a convenience.
    /// A tube is seen end-on: two rings of it are a hoop 0.06 units long seen
    /// from inside, which subtends a ring of angles far outside the field of
    /// view and therefore draws *nothing at all*. Waiting for four seconds of
    /// history before the first pixel would be four seconds of black after
    /// pressing play. So the opening shape is the first measurement repeated --
    /// "this is what the sound is doing" -- and real history replaces it, ring
    /// by ring, over the next four seconds.
    fn advance(&mut self, queue: &wgpu::Queue, frame: &VisualFrame) {
        if !self.build_ring(frame) {
            // Decision 21's rule, inherited: nothing measured means the tube
            // holds still rather than laying silent rings through real history.
            return;
        }
        if self.history.filled() == 0 {
            self.history.fill(queue, &self.ring);
            self.created += RINGS as u64;
            return;
        }
        let due = self.history.rows_due(frame.elapsed_seconds);
        for _ in 0..due {
            self.history.push(queue, &self.ring);
        }
        self.created += due as u64;
    }

    /// Where the eye is along the path, as a parameter [`path_at`] takes.
    ///
    /// The eye is level with the oldest ring that still exists and moves on as
    /// rings are created, so what lies ahead of it is the tube and what has gone
    /// behind is dropped (decision 26). Taken modulo [`PATH_PERIOD`], which is
    /// exact because the path is periodic there -- and necessary, because the
    /// counter climbs for as long as the app is open.
    fn eye_parameter(&self, musical: &Musical) -> f32 {
        let filled = self.history.filled() as u64;
        let behind = self.created.saturating_sub(filled);
        // The fraction only counts once the tube is full length. Before that the
        // tunnel grows *ahead* of a stationary eye, and gliding on a fraction
        // that resets every tick would be a judder rather than a flight.
        let gliding = if self.history.filled() == RINGS {
            self.history.fraction()
        } else {
            0.0
        };
        (behind % PATH_PERIOD as u64) as f32 + gliding + beat_surge(musical)
    }
}

impl Visual for OscilloscopeTunnel {
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
        // The projection carries the aspect ratio and is written fresh on every
        // frame that draws anything, so there is nothing to correct here.
    }

    /// Draws one frame of the tunnel into `target`.
    ///
    /// Of the frame this reads the samples, their layout and the elapsed time,
    /// and the gain the host resolved -- the vectorscope's inputs exactly. It
    /// measures nothing (decision 2): a ring is a transform of samples, the way
    /// the goniometer's axes are.
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

        self.advance(queue, frame);

        let filled = self.history.filled();
        if filled >= 2 {
            queue.write_buffer(
                &self.scene_buffer,
                0,
                bytemuck::bytes_of(&scene_uniform(
                    self.width,
                    self.height,
                    self.srgb,
                    self.eye_parameter(&frame.musical),
                    filled,
                    self.history.head(),
                    self.history.fraction(),
                    frame.gain,
                )),
            );
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tunnel-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tunnel-pass"),
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
            // Nothing played is nothing drawn, the same answer the vectorscope
            // gives an empty window and the waterfall an empty spectrum.
            if filled >= 2 {
                // The indices run from the newest ring backwards, so the rings
                // that exist are a prefix of them and how much tunnel there is
                // becomes a draw count.
                let count = ((filled - 1) * RING_POINTS * 6) as u32;
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..count, 0, 0..1);
            }
        }
        encoder.finish()
    }
}

/// Where the path is at parameter `p`, in rings.
///
/// **The one implementation.** The camera is this function and so is every
/// ring's centre (decision 25): a camera integrated frame by frame drifts out
/// through the wall after a few thousand frames of rounding, and a path written
/// twice -- once in Rust for the eye and once in WGSL for the tube -- agrees
/// until one of them is edited.
///
/// `z` runs negative with `p` because the tunnel is laid out ahead of the eye,
/// and the bends are sines of the *ring* index rather than of time, which is
/// what attaches them to the history: a bend arrives when the audio that made it
/// does, and then stays where it was rather than swimming.
fn path_at(p: f32) -> [f32; 3] {
    let x = BEND_X * (std::f32::consts::TAU * BEND_CYCLES_X * p / PATH_PERIOD).sin();
    let y = BEND_Y * (std::f32::consts::TAU * BEND_CYCLES_Y * p / PATH_PERIOD).sin();
    [x, y, -RING_SPACING * p]
}

fn subtract(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// How far the beat has pushed the eye along the path, in rings.
///
/// Zero without a tempo and quiet when the tempo is not believed (decision 45),
/// which is what keeps M4's sixty checks meaningful: none of them feed a tempo,
/// so all of them are still asking about exactly the tunnel they were written
/// for.
///
/// Zero at every beat instant and continuous where the phase wraps, so the
/// flight cannot drift and cannot step. Never negative, which matters more than
/// it looks: the eye sits level with the *oldest* ring, and pushing it backwards
/// would put it outside the tube's own mouth -- where the enclosure claim
/// (decision 24) is simply false.
fn beat_surge(musical: &Musical) -> f32 {
    if musical.bpm <= 0.0 {
        return 0.0;
    }
    let phase = std::f32::consts::TAU * musical.beat_phase;
    let surge = (1.0 - phase.cos()) * 0.5;
    BEAT_SURGE_RINGS * musical.beat_confidence.clamp(0.0, 1.0) * surge
}

/// How many rings ahead of the eye the ring of age `age` sits.
///
/// The oldest ring drifts from one ring ahead to level with the eye over a tick
/// and is then dropped, so the eye is always *inside* a segment rather than at
/// the mouth of one -- which is both what a tunnel should feel like and what
/// makes [`encloses`] true at any window shape.
fn rings_ahead(age: usize, filled: usize, fraction: f32) -> f32 {
    (filled.saturating_sub(1).saturating_sub(age)) as f32 - fraction
}

/// The uniform for a viewport of this size, with the eye here and this much
/// tunnel in front of it.
#[allow(clippy::too_many_arguments)]
fn scene_uniform(
    width: u32,
    height: u32,
    srgb: f32,
    eye: f32,
    filled: usize,
    head: usize,
    fraction: f32,
    gain: f32,
) -> SceneUniform {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    // Decision 24's guarantee, checked where it is relied on rather than only in
    // a test: if this ever fails, the window has a shape whose corners see past
    // the tube, and the fix is the scene's constants rather than the camera's.
    debug_assert!(
        encloses(aspect),
        "the tube does not enclose a {aspect} frame"
    );
    let here = path_at(eye);
    // The eye is the origin and the tube is placed around it, rather than the
    // other way round: the ring counter climbs for as long as the app is open,
    // and world coordinates that climb with it would lose their precision
    // exactly where all the detail is.
    let view = camera::look_at(
        [0.0, 0.0, 0.0],
        subtract(path_at(eye + LOOK_AHEAD), here),
        [0.0, 1.0, 0.0],
    );
    let view_proj = camera::perspective(FOV_Y, aspect, NEAR, FAR).mul(&view);

    let mut centres = [[0.0f32; 4]; RINGS];
    for (age, centre) in centres.iter_mut().enumerate().take(filled.min(RINGS)) {
        let ahead = rings_ahead(age, filled, fraction);
        let point = subtract(path_at(eye + ahead), here);
        *centre = [point[0], point[1], point[2], ahead];
    }

    let gain = if gain.is_finite() && gain > 0.0 {
        gain
    } else {
        1.0
    };
    let [r, g, b] = BACKGROUND;
    SceneUniform {
        view_proj: view_proj.as_array(),
        // Display gain is an amplitude multiplier here, not a decibel offset:
        // this visual draws samples, which is the vectorscope's side of the line
        // M2 drew (decision 28).
        shape: [BASE_RADIUS, WAVE_DEPTH * gain, MIN_SCALE, MAX_SCALE],
        grid: [srgb, RING_POINTS as f32, filled as f32, head as f32],
        fog: [r as f32, g as f32, b as f32, FADE_RINGS],
        extra: [TWIST, RINGS as f32, 0.0, 0.0],
        centres,
    }
}

/// The tube's triangles, newest ring first.
///
/// Two properties, both load-bearing: the ring closes (the last point joins the
/// first, so there is no seam down the tunnel), and the rings that exist are a
/// prefix (so a young tunnel is a draw count rather than a second buffer).
fn tube_indices() -> Vec<u32> {
    let mut indices = Vec::with_capacity((RINGS - 1) * RING_POINTS * 6);
    for age in 0..RINGS - 1 {
        for point in 0..RING_POINTS {
            let next = (point + 1) % RING_POINTS;
            let near = (age * RING_POINTS) as u32;
            let far = ((age + 1) * RING_POINTS) as u32;
            let (a, b) = (near + point as u32, near + next as u32);
            let (c, d) = (far + next as u32, far + point as u32);
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    indices
}

/// Whether the tube is long enough to cover the edges of the frame at this
/// aspect ratio.
///
/// Decision 24, as arithmetic -- and the arithmetic is not the fit's, inverted;
/// it is a different question with a different answer. Inside a straight tube,
/// a ray leaving the eye at an angle `theta` from the axis meets the wall at
/// `radius / tan(theta)`, so every angle from `atan(radius / length)` outwards
/// lands on wall and everything nearer the axis than that lands on the opening
/// at the far end. The frame reaches `theta` = its half-diagonal in the corners
/// and zero in the middle, so:
///
/// - the **corners** are wall as long as the tube is longer than
///   `radius / tan(corner)` -- which is what this returns, at the largest radius
///   the clamp allows;
/// - the **middle** is the opening, always, and is meant to be: it is the
///   vanishing point, and it fades to the background rather than showing a hole
///   (see `tunnel.wgsl`'s fog). That is why the smoke's pixel half of this
///   checks a border rather than the whole image.
///
/// A wider window is the *easier* case, not the harder one: its corners sit
/// further off the axis, where the wall is nearer.
fn encloses(aspect: f32) -> bool {
    let half_height = (FOV_Y * 0.5).tan();
    let corner = (half_height * (1.0 + aspect * aspect).sqrt()).atan();
    let length = RINGS as f32 * RING_SPACING;
    let widest = BASE_RADIUS * MAX_SCALE;
    length * corner.tan() > widest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decision 48: the beat moves the eye, bounded, zero on the beat, and
    /// gone entirely without a tempo -- which is what keeps M4's sixty checks
    /// asking about the tunnel they were written for.
    #[test]
    fn the_beat_moves_the_eye_and_nothing_else() {
        assert_eq!(beat_surge(&Musical::default()), 0.0);

        let believed = Musical {
            bpm: 120.0,
            beat_confidence: 1.0,
            beat_phase: 0.0,
            ..Musical::default()
        };
        // Exactly zero on the beat, so the flight arrives where the ring clock
        // says it should and cannot drift away from it.
        assert!(beat_surge(&believed).abs() < 1e-6);
        // Furthest along halfway between two beats, and never further than the
        // bound.
        let midway = beat_surge(&Musical {
            beat_phase: 0.5,
            ..believed
        });
        assert!((midway - BEAT_SURGE_RINGS).abs() < 1e-5);
        for step in 0..=100 {
            let phase = step as f32 / 100.0;
            let surge = beat_surge(&Musical {
                beat_phase: phase,
                ..believed
            });
            assert!(
                (0.0..=BEAT_SURGE_RINGS).contains(&surge),
                "at phase {phase} the eye moved {surge} rings"
            );
        }
        // Continuous where the phase wraps: a step at the bar line would be a
        // teleport rather than a flight.
        let nearly = beat_surge(&Musical {
            beat_phase: 0.999,
            ..believed
        });
        assert!(nearly < 0.001, "the wrap steps by {nearly} rings");

        // A tempo nobody believes moves it as little as no tempo at all
        // (decision 45).
        let unsure = Musical {
            beat_confidence: 0.0,
            beat_phase: 0.5,
            ..believed
        };
        assert_eq!(beat_surge(&unsure), 0.0);
        // And half-believed is half as much, rather than on or off.
        let half = beat_surge(&Musical {
            beat_confidence: 0.5,
            beat_phase: 0.5,
            ..believed
        });
        assert!((half - BEAT_SURGE_RINGS / 2.0).abs() < 1e-5);
    }

    /// Window shapes worth checking, from a small panel to an ultrawide
    /// fullscreen and a window taller than it is wide -- the same spread the
    /// other three visuals' cameras are pinned at.
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
    fn the_wall_encloses_the_frame_at_every_shape() {
        // Decision 24: the claim `camera::fit` makes for the other visuals,
        // inverted. Nothing can leave this frame, so what must be shown is that
        // nothing outside the tube can enter it -- at the corner of a 21:9
        // window, with the wall at its smallest permitted radius.
        for (width, height) in SHAPES {
            let aspect = width as f32 / height as f32;
            assert!(
                encloses(aspect),
                "{width}x{height} would show past the tube"
            );
        }
        // A tall window is the hard case, not a wide one: its corners sit
        // nearer the axis, where the wall is further away.
        assert!(encloses(0.25), "a tall window sees past the tube");
        assert!(encloses(8.0));
    }

    #[test]
    fn the_eye_stays_inside_the_tube() {
        // What "inside" reduces to: the centreline may not move further between
        // two rings than the wall's smallest radius, or the tube would bend
        // clean past its own axis and leave the eye outside it.
        let step_limit = BASE_RADIUS * MIN_SCALE;
        let mut worst = 0.0f32;
        for ring in 0..(PATH_PERIOD as usize) {
            let here = path_at(ring as f32);
            let next = path_at(ring as f32 + 1.0);
            let lateral = ((next[0] - here[0]).powi(2) + (next[1] - here[1]).powi(2)).sqrt();
            worst = worst.max(lateral);
        }
        assert!(
            worst < step_limit,
            "the path moves {worst} between rings against a wall at {step_limit}"
        );
    }

    #[test]
    fn the_oldest_ring_is_the_one_the_eye_is_in() {
        // The relative-coordinate arithmetic, which is the part most able to be
        // quietly wrong: the ring the eye is level with has to be at the origin,
        // not a tube's length away.
        for eye in [0.0f32, 137.0, 3599.5] {
            let uniform = scene_uniform(1280, 720, 0.0, eye, RINGS, 0, 0.0, 1.0);
            let oldest = uniform.centres[RINGS - 1];
            let distance =
                (oldest[0] * oldest[0] + oldest[1] * oldest[1] + oldest[2] * oldest[2]).sqrt();
            assert!(distance < 1e-5, "the eye sits {distance} from its own ring");
            // ...and the newest is a whole tunnel ahead.
            let newest = uniform.centres[0];
            assert!(
                (newest[2] + RING_SPACING * (RINGS - 1) as f32).abs() < 1e-3,
                "the far end is at z={}",
                newest[2]
            );
            assert!((newest[3] - (RINGS - 1) as f32).abs() < 1e-3);
        }
    }

    #[test]
    fn the_newest_ring_is_the_furthest_ahead() {
        // Decision 26: ahead is newer. Getting this backwards draws a tunnel
        // that is flown into the past, where every ring the eye is about to
        // reach is one it has already passed.
        let filled = RINGS;
        for fraction in [0.0f32, 0.5, 0.99] {
            let mut previous = f32::INFINITY;
            for age in 0..filled {
                let ahead = rings_ahead(age, filled, fraction);
                assert!(ahead < previous, "age {age} is not behind age {}", age - 1);
                previous = ahead;
            }
            // The oldest has just passed the eye, which is what puts the eye
            // inside a segment rather than at the mouth of one.
            assert!((rings_ahead(filled - 1, filled, fraction) + fraction).abs() < 1e-6);
        }
    }

    #[test]
    fn the_paths_wrap_is_invisible() {
        // The ring counter is taken modulo `PATH_PERIOD`, which is only sound
        // because the bends complete a whole number of cycles there. If they did
        // not, the tunnel would jump sideways once a minute.
        for p in [0.0f32, 12.5, 811.0, 2999.75] {
            let here = path_at(p);
            let wrapped = path_at(p + PATH_PERIOD);
            assert!((here[0] - wrapped[0]).abs() < 1e-4, "x jumps at {p}");
            assert!((here[1] - wrapped[1]).abs() < 1e-4, "y jumps at {p}");
        }
        assert_eq!(BEND_CYCLES_X.fract(), 0.0);
        assert_eq!(BEND_CYCLES_Y.fract(), 0.0);
    }

    #[test]
    fn display_gain_is_a_multiplier_here() {
        // Decision 28, and the other side of M2 decision 12's line: this visual
        // draws samples, so x2 is twice the displacement rather than +6dB.
        let unity = scene_uniform(640, 360, 0.0, 0.0, RINGS, 0, 0.0, 1.0);
        let doubled = scene_uniform(640, 360, 0.0, 0.0, RINGS, 0, 0.0, 2.0);
        assert!((unity.shape[1] - WAVE_DEPTH).abs() < 1e-6);
        assert!((doubled.shape[1] - WAVE_DEPTH * 2.0).abs() < 1e-6);
        // A gain of zero or worse is a bug upstream, and unity is the safe
        // reading of it.
        let broken = scene_uniform(640, 360, 0.0, 0.0, RINGS, 0, 0.0, f32::NAN);
        assert!((broken.shape[1] - WAVE_DEPTH).abs() < 1e-6);
        // And the clamp that keeps the wall off the eye travels with it.
        assert_eq!(doubled.shape[2], MIN_SCALE);
        const { assert!(MIN_SCALE > 0.0 && MIN_SCALE < 1.0 && MAX_SCALE > 1.0) };
    }

    #[test]
    fn the_tube_closes_and_its_rings_are_a_prefix() {
        let indices = tube_indices();
        assert_eq!(indices.len(), (RINGS - 1) * RING_POINTS * 6);
        assert!(indices.iter().all(|i| (*i as usize) < RINGS * RING_POINTS));
        // The seam: the last point of a ring joins the first, so there is no
        // gap running the length of the tunnel.
        let last_quad = &indices[(RING_POINTS - 1) * 6..RING_POINTS * 6];
        assert!(
            last_quad.contains(&0) && last_quad.contains(&(RING_POINTS as u32 - 1)),
            "the ring does not close: {last_quad:?}"
        );
        // The prefix property, which is what lets a young tunnel be a draw
        // count: the first `filled - 1` rings' worth of indices stay inside
        // those rings.
        for filled in [2usize, 3, 91, RINGS] {
            let count = (filled - 1) * RING_POINTS * 6;
            let highest = indices[..count].iter().copied().max().expect("indices");
            assert!((highest as usize) < filled * RING_POINTS);
        }
    }

    #[test]
    fn the_tunnel_is_as_long_as_it_says_it_is() {
        assert!((TUNNEL_SECONDS - 4.0).abs() < 1e-6);
        // And the speed follows from it rather than being its own dial.
        assert!((RING_HZ * RING_SPACING - 3.6).abs() < 1e-4);
    }
}
