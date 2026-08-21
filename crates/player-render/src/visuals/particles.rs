//! Particle field: PLAN.md section 4's fifth built-in, and Phase 3 M6.
//!
//! The first visual here driven by **what the music is doing** rather than by
//! its samples or its spectrum -- M5 measured onsets, tempo, beat phase and
//! brightness, and decision 39 deliberately left them unread until there were
//! decisions about how a scene should move to them. It is also this workspace's
//! first compute shader, and the first scene whose state is *advanced by
//! arithmetic* rather than replaced by a measurement.
//!
//! Three rules shape everything below, and each of them is a bug that would
//! otherwise have been found on someone else's machine:
//!
//! - **The simulation steps on a fixed clock** ([`RowClock`], decision 42), so
//!   sixty frames of a sixtieth move the field exactly as far as one frame of a
//!   whole second. The desktop redraws at 60Hz; a browser draws at whatever the
//!   display does.
//! - **An onset is idempotent** (decision 43). `musical.onset` is not an event
//!   this visual receives -- it is the strongest since the last analysis drain,
//!   *held* until the next drain replaces it, so at 120Hz a frame reads the same
//!   onset two or three times. It therefore enters only through
//!   `max(energy - decay * dt, onset)`, where reading it twice changes nothing
//!   and missing it is impossible.
//! - **A particle is a pure function of its identity** (decision 44): spawn
//!   position, speed and lifetime are hashed from the spawn ordinal, so there is
//!   no random state to diverge between two shells, and a check can assert where
//!   the field went.

use wgpu::util::DeviceExt;

use crate::attachments::{background, shader_source, target_is_srgb, BACKGROUND};
use crate::camera;
use crate::visual::{Gpu, Musical, Visual, VisualFrame};

use super::history::RowClock;

/// How many particles exist. Every one of them is simulated every step whether
/// it is alive or not, which is what makes the cost of this visual a constant
/// rather than a function of the music -- 131072 is 2048 workgroups of 64, and
/// four megabytes of state.
///
/// It is the *budget* rather than the population: how many are alive at once is
/// the emission rate times a lifetime, and a fullscreen window is allowed four
/// times what a 480px one uses (see [`density_scale`]). Sized so the largest
/// case fits with room to spare, because a slot recycled while its particle is
/// still alive is a spark that vanishes mid-flight.
const PARTICLES: u32 = 131_072;
const WORKGROUP: u32 = 64;

/// The simulation's clock: 120 steps a second, and a stall budget of two
/// seconds' worth.
///
/// Twice the desktop's redraw rate, so a spring stiff enough to orbit is still
/// integrated stably, and the budget is what stops a window that spent a minute
/// minimised from spending a minute catching up (decision 42 -- a simulation may
/// not skip work, but it may decline to relive a suspension).
const STEP_HZ: f32 = 120.0;
const MAX_STEPS: usize = 240;

/// Seconds a particle lives: the shortest, and how much longer the longest is.
const LIFE_MIN: f32 = 1.6;
const LIFE_SPAN: f32 = 2.0;

/// Particles a second at full level with nothing percussive happening, and the
/// rate a full-strength onset adds on top of it.
///
/// The first is what makes a held chord a field rather than an empty screen; the
/// second is what makes a snare visible. Steady state is roughly rate times
/// life: the base holds a couple of thousand sparks, and a beat adds about nine
/// hundred over the fifth of a second the envelope takes to fall. The first
/// numbers tried were five times these, and the field saturated -- a screen full
/// of sparks has nowhere to put a transient, so the transients disappeared.
const BASE_RATE: f32 = 800.0;
const ONSET_RATE: f32 = 6_500.0;

/// How fast the onset envelope falls, per second (decision 43).
const ONSET_DECAY: f32 = 5.0;

/// Where particles are born, how far out of the disc they may start, how hard
/// an onset throws them outward, and how fast they orbit.
///
/// A **disc** rather than a sphere, which is the difference between a swirl a
/// person can see and a ball that happens to be rotating -- found by rendering
/// the first version and looking at it, the way M3's camera and M4's field of
/// view were. The thickness is what keeps it from reading as a flat plate.
const SPAWN_RADIUS: f32 = 0.55;
const DISC_THICKNESS: f32 = 0.22;
const SPAWN_SPEED: f32 = 2.4;
const ORBIT_SPEED: f32 = 1.5;

/// The force field: a swirl about the vertical, a spring back to the middle,
/// drag, and what a beat adds outward against the spring.
///
/// Swirl and spring together are an *orbit*: without the spring the field is an
/// explosion that leaves, and without the swirl it is a fountain that falls
/// back through itself. The kick is deliberately larger than either, because it
/// has only the width of a beat to be visible in.
const SWIRL: f32 = 1.3;
const PULL: f32 = 1.7;
const DRAG: f32 = 0.85;
const KICK: f32 = 3.5;

/// How far the field may reach. Bounded so the camera can be fitted to it
/// once and provably hold everything (see [`hull`]) -- an unbounded field would
/// either be framed for a case that never happens or would leave the frame on a
/// loud passage.
const MAX_RADIUS: f32 = 3.2;

/// A particle's radius in world units at [`REFERENCE_HEIGHT`], before its
/// life-fade shrinks it.
const SPARK_SIZE: f32 = 0.04;

/// The viewport height [`SPARK_SIZE`] is stated at, and the range the scaling
/// below is allowed to reach.
///
/// A spark is a quad in the *world*, and the camera is fitted to the field --
/// so at twice the viewport height the same field is drawn twice as large and
/// every spark with it. That is right for a trace and wrong for a field of
/// them: it means a bigger window shows the same picture magnified, which is
/// what "smooth but blurred, and the resolution suffers past 480" was when this
/// was first watched fullscreen (reported live 2026-08-20).
///
/// So the world size is scaled by the reference height over the real one, which
/// holds a spark at roughly the same size *to the eye* and lets a fullscreen
/// window spend its pixels on more of them rather than on bigger ones. The
/// reference is the default visualizer size, so the size that was watched and
/// approved is the size that is preserved.
///
/// Clamped at both ends: below about two pixels a spark stops being a shape and
/// starts being aliasing that crawls, and above the top of the range a small
/// window would show a handful of blobs.
const REFERENCE_HEIGHT: f32 = 480.0;
const SPARK_SCALE_RANGE: (f32, f32) = (0.25, 1.6);

/// How much more to emit for a viewport of this height.
///
/// The other half of the same finding. Holding a spark's *pixel* size fixed
/// stops a big window from magnifying the picture, but on its own it leaves a
/// fullscreen field looking thin -- the same population thrown across five times
/// the pixels. Emission therefore follows the viewport's area, so a bigger
/// window buys both more detail and more of it.
///
/// Never below the reference: a small window keeps the density that was watched
/// and approved rather than being thinned to match its pixel count. Capped at
/// four, which is where the fixed budget above stops being comfortable.
fn density_scale(height: u32) -> f32 {
    let ratio = (height.max(1) as f32) / REFERENCE_HEIGHT;
    (ratio * ratio).clamp(1.0, 4.0)
}

/// How much to scale [`SPARK_SIZE`] for a viewport of this height.
fn spark_scale(height: u32) -> f32 {
    let height = (height.max(1) as f32).max(1.0);
    (REFERENCE_HEIGHT / height).clamp(SPARK_SCALE_RANGE.0, SPARK_SCALE_RANGE.1)
}

/// How bright one spark is, and how hard the tone map's shoulder pulls the sum
/// of many back towards white.
const SPARK_BRIGHTNESS: f32 = 1.0;
const EXPOSURE: f32 = 1.0;

/// The frequencies the hue runs between, and how far two particles born in the
/// same instant may differ.
///
/// Deep blue for a bass-heavy moment, warm violet for a bright one -- the app's
/// own accent colour is the middle of that range. Read from the spectral
/// centroid, so the colour is a *measurement* rather than a palette cycling on
/// its own.
const HUE_LOW_HZ: f32 = 80.0;
const HUE_HIGH_HZ: f32 = 8_000.0;
const HUE_AT_LOW: f32 = 0.62;
const HUE_AT_HIGH: f32 = 0.92;
const HUE_SPREAD: f32 = 0.07;

/// Where the eye sits, as a direction and a distance. Slightly above the
/// field's own plane, which is what makes the swirl read as a swirl rather than
/// as horizontal drift.
const EYE_DIRECTION: [f32; 3] = [0.0, 0.72, 1.0];
const EYE_DISTANCE: f32 = 9.0;
const NEAR: f32 = 0.1;
const FAR: f32 = 40.0;
/// How much of the frame the field fills at its widest.
const FILL: f32 = 0.86;

/// The radius the camera is framed for.
///
/// Smaller than [`MAX_RADIUS`], and the difference is deliberate: fitting the
/// frame to everything a particle *could* reach leaves ordinary music as a
/// small blob in the middle of a mostly empty screen, which is what the first
/// render of this visual actually looked like. So the frame is sized for where
/// the field lives, and a hard transient is allowed to throw sparks past the
/// edge -- which reads as force rather than as a mistake, and is still bounded,
/// because nothing can pass `MAX_RADIUS` at all.
const FRAMED_RADIUS: f32 = 1.8;

/// The accumulation texture's format: linear light, so that additive blending
/// means the same thing on an sRGB surface and on the web's `bgra8unorm` canvas
/// (see the head of `particles.wgsl`). Renderable and blendable in core WebGPU.
const ACCUMULATION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Bytes between two steps' uniforms.
///
/// The sim uniform changes *per step* and several steps are encoded into one
/// command buffer, so they cannot be written into one slot: `queue.write_buffer`
/// lands before the whole encoder rather than between two of its passes. So
/// every step this frame is written once, into its own slot of one buffer, and
/// the passes read them back through a dynamic offset -- which must be a
/// multiple of `min_uniform_buffer_offset_alignment`, 256 in the baseline
/// limits.
const SIM_STRIDE: u64 = 256;

/// The range spawn ordinals are counted in before they wrap.
///
/// A multiple of [`PARTICLES`] (both are powers of two), which is what lets the
/// count wrap without moving which slot an ordinal belongs to -- and far enough
/// below `u32::MAX` that adding a step's worth of spawns to it never overflows,
/// so the shader's `ordinal < end` is true exactly for the ordinals the host
/// issued.
const ORDINAL_WINDOW: u64 = 1 << 31;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SimUniform {
    /// `[dt, energy, kick, level]`.
    step: [f32; 4],
    /// `[spawn_begin, spawn_end, particle_count, 0]`, as spawn ordinals.
    spawn: [u32; 4],
    /// `[swirl, pull, drag, kick_strength]`.
    motion: [f32; 4],
    /// `[outward_speed, radius, thickness, orbit_speed]`.
    shape: [f32; 4],
    /// `[life_min, life_span, 0, 0]`.
    life: [f32; 4],
    /// `[hue, hue_spread, max_radius, 0]`.
    colour: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    view_proj: [[f32; 4]; 4],
    right: [f32; 4],
    up: [f32; 4],
    /// `[size, brightness, life_span, srgb_target]`.
    params: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniform {
    /// `[exposure, srgb_target, 0, 0]`.
    settings: [f32; 4],
    background: [f32; 4],
}

pub struct ParticleField {
    sim_pipeline: wgpu::ComputePipeline,
    spark_pipeline: wgpu::RenderPipeline,
    present_pipeline: wgpu::RenderPipeline,
    particles: wgpu::Buffer,
    sim_buffer: wgpu::Buffer,
    scene_buffer: wgpu::Buffer,
    composite_buffer: wgpu::Buffer,
    sim_bind_group: wgpu::BindGroup,
    scene_bind_group: wgpu::BindGroup,
    present_layout: wgpu::BindGroupLayout,
    present_bind_group: wgpu::BindGroup,
    accumulation: wgpu::TextureView,
    clock: RowClock,
    /// Decision 43's envelope. The one piece of state this visual keeps that is
    /// not on the GPU, because it is what the *host* has to know to decide how
    /// many particles to spawn.
    energy: f32,
    /// Spawn ordinals issued so far, and the fraction of one carried towards
    /// the next. A `u64` for the tunnel's reason: a field left running for a day
    /// issues more ordinals than an `f32` can count.
    spawned: u64,
    carry: f32,
    /// This frame's steps, staged in one write rather than one per step.
    staging: Vec<u8>,
    width: u32,
    height: u32,
    srgb: f32,
}

/// The ball the camera is framed for: radius [`FRAMED_RADIUS`], as a hull.
///
/// [`camera::fit`] frames the convex hull of the points it is given, and the 26
/// directions with coordinates in `{-1, 0, 1}`, normalised, are a ball's
/// rhombicuboctahedron -- within 4% of the sphere everywhere and always outside
/// it, so the fit is conservative rather than approximate.
fn hull() -> Vec<[f32; 3]> {
    let mut points = Vec::with_capacity(26);
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if x == 0 && y == 0 && z == 0 {
                    continue;
                }
                let (x, y, z) = (x as f32, y as f32, z as f32);
                let length = (x * x + y * y + z * z).sqrt();
                points.push([
                    x / length * FRAMED_RADIUS,
                    y / length * FRAMED_RADIUS,
                    z / length * FRAMED_RADIUS,
                ]);
            }
        }
    }
    points
}

/// Where the eye is, and the two axes a billboard is built from.
///
/// The quads have to face the camera, and the camera's own right and up are
/// what that means. Computed here rather than taken out of the view matrix
/// because [`camera::fit`] hands back a view-*projection*, and pulling a basis
/// back out of a matrix that has a field of view baked into it is arithmetic
/// nobody should have to check twice.
fn eye_basis() -> ([f32; 3], [f32; 3], [f32; 3]) {
    let [dx, dy, dz] = EYE_DIRECTION;
    let length = (dx * dx + dy * dy + dz * dz).sqrt().max(f32::EPSILON);
    let eye = [
        dx / length * EYE_DISTANCE,
        dy / length * EYE_DISTANCE,
        dz / length * EYE_DISTANCE,
    ];
    // Looking at the origin, so forward is -eye normalised.
    let forward = [
        -eye[0] / EYE_DISTANCE,
        -eye[1] / EYE_DISTANCE,
        -eye[2] / EYE_DISTANCE,
    ];
    let world_up = [0.0f32, 1.0, 0.0];
    let right = normalise(cross(forward, world_up));
    let up = cross(right, forward);
    (eye, right, up)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalise(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < f32::EPSILON {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / length, v[1] / length, v[2] / length]
    }
}

/// Where a spectral centroid falls on the colour wheel.
///
/// Logarithmic, because pitch is: the octave from 100Hz to 200Hz has to move the
/// colour as far as the one from 2kHz to 4kHz, which a linear map would not do.
/// A centroid of zero means nothing is sounding, and takes the low end rather
/// than a colour of its own -- there are no particles to paint with it.
fn hue_for(centroid_hz: f32) -> f32 {
    if !centroid_hz.is_finite() || centroid_hz <= 0.0 {
        return HUE_AT_LOW;
    }
    let position = (centroid_hz.clamp(HUE_LOW_HZ, HUE_HIGH_HZ) / HUE_LOW_HZ).ln()
        / (HUE_HIGH_HZ / HUE_LOW_HZ).ln();
    HUE_AT_LOW + (HUE_AT_HIGH - HUE_AT_LOW) * position
}

/// How hard the beat pushes, at this instant of it.
///
/// Zero without a tempo and quiet when the tempo is not believed (decision 45):
/// `beat_confidence` scales the whole thing, so ambient material gets a field
/// that drifts rather than one that insists on a pulse it cannot hear. The
/// eighth power is what makes it a *beat* rather than a wobble -- it is at half
/// strength for about a seventh of a beat either side of the instant.
fn beat_kick(musical: &Musical) -> f32 {
    if musical.bpm <= 0.0 {
        return 0.0;
    }
    let phase = std::f32::consts::TAU * musical.beat_phase;
    let pulse = phase.cos().max(0.0).powi(8);
    musical.beat_confidence.clamp(0.0, 1.0) * pulse
}

impl ParticleField {
    pub(crate) fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let Gpu {
            device,
            queue: _,
            format,
        } = *gpu;
        let srgb = target_is_srgb(format);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particles-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source(include_str!("particles.wgsl"))),
        });

        // Every slot starts dead (a life of zero), so a field that has never
        // been fed draws nothing at all -- the same answer the vectorscope gives
        // an empty window.
        let particles = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("particles-state"),
            contents: bytemuck::cast_slice(&vec![0.0f32; PARTICLES as usize * 8]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX,
        });
        let sim_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particles-sim"),
            size: SIM_STRIDE * MAX_STEPS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particles-scene"),
            size: std::mem::size_of::<SceneUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particles-composite"),
            size: std::mem::size_of::<CompositeUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Three layouts rather than one, because the accumulation texture is a
        // render attachment while the sparks are being drawn and a binding only
        // once they are done -- one bind group covering both could not legally
        // be bound in either pass.
        let sim_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particles-sim-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<SimUniform>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particles-scene-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let present_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particles-present-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Read with `textureLoad` at one texel per pixel, so
                        // there is no sampler and nothing to filter.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let sim_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particles-sim-bind-group"),
            layout: &sim_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: particles.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &sim_buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<SimUniform>() as u64),
                    }),
                },
            ],
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particles-scene-bind-group"),
            layout: &scene_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: scene_buffer.as_entire_binding(),
            }],
        });

        let sim_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particles-sim-pipeline-layout"),
            bind_group_layouts: &[Some(&sim_layout)],
            immediate_size: 0,
        });
        let sim_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("particles-sim-pipeline"),
            layout: Some(&sim_pipeline_layout),
            module: &shader,
            entry_point: Some("sim_step"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let spark_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particles-spark-pipeline-layout"),
                bind_group_layouts: &[Some(&scene_layout)],
                immediate_size: 0,
            });
        let spark_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particles-spark-pipeline"),
            layout: Some(&spark_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_spark"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // Decision 41: the state the compute pass writes is read back by
                // the draw as *instance attributes*, never as a vertex-stage
                // storage binding -- a limit compatibility mode grants zero of.
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_spark"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: ACCUMULATION_FORMAT,
                    // Light adds. No depth and no sorting either: what a
                    // hundred sparks look like piled up does not depend on the
                    // order they were drawn in, which is the one property that
                    // makes an unsorted particle field correct rather than
                    // merely quick.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::REPLACE,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let present_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particles-present-pipeline-layout"),
                bind_group_layouts: &[Some(&present_layout)],
                immediate_size: 0,
            });
        let present_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particles-present-pipeline"),
            layout: Some(&present_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_present"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_present"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            // Decision 47: no multisampling. These are soft round sprites and a
            // full-viewport composite -- neither has a geometric edge for four
            // samples a pixel to improve.
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let accumulation = accumulation_view(device, width, height);
        let present_bind_group =
            present_group(device, &present_layout, &accumulation, &composite_buffer);

        Self {
            sim_pipeline,
            spark_pipeline,
            present_pipeline,
            particles,
            sim_buffer,
            scene_buffer,
            composite_buffer,
            sim_bind_group,
            scene_bind_group,
            present_layout,
            present_bind_group,
            accumulation,
            clock: RowClock::new(STEP_HZ, MAX_STEPS),
            energy: 0.0,
            spawned: 0,
            carry: 0.0,
            staging: Vec::new(),
            width: width.max(1),
            height: height.max(1),
            srgb,
        }
    }

    /// Runs the envelope, the emission and the clock for this frame, staging one
    /// uniform per step. Returns how many steps to dispatch.
    ///
    /// All of it in the host's arithmetic rather than the shader's, because the
    /// number of particles to spawn is exactly what the shader cannot work out
    /// for itself without an atomic -- and an atomic is a different answer on
    /// every run.
    fn stage_steps(&mut self, frame: &VisualFrame) -> usize {
        let steps = self.clock.due(frame.elapsed_seconds);
        if steps == 0 {
            return 0;
        }
        let dt = 1.0 / STEP_HZ;
        let density = density_scale(self.height);
        let level = (frame.peak() * frame.gain).clamp(0.0, 1.0);
        let onset = frame.musical.onset.clamp(0.0, 1.0);
        let kick = beat_kick(&frame.musical);
        let hue = hue_for(frame.musical.centroid_hz);

        self.staging.clear();
        self.staging.resize(SIM_STRIDE as usize * steps, 0);
        for step in 0..steps {
            // Decision 43, per step rather than per frame: the same onset seen
            // on two frames leaves the envelope where one frame would, and the
            // envelope falls at the same rate however the time arrived.
            self.energy = (self.energy - ONSET_DECAY * dt).max(onset);

            // Emission is a rate integrated over the step, with the fraction
            // carried -- so a frame worth half a particle spawns none now and
            // one later, rather than none ever.
            let rate = (BASE_RATE * level.sqrt() + ONSET_RATE * self.energy) * density;
            self.carry += rate * dt;
            let issue = self.carry.floor().clamp(0.0, PARTICLES as f32);
            self.carry -= issue;

            // Ordinals, narrowed to what a shader can hold without ever
            // wrapping mid-range. The window is a multiple of the particle
            // count, so which slot an ordinal lands on is unchanged, and it is
            // large enough that a spawn's hashed identity does not repeat until
            // the field has recycled itself sixty-five thousand times.
            let begin = (self.spawned % ORDINAL_WINDOW) as u32;
            let issued = issue as u32;
            self.spawned += u64::from(issued);

            let uniform = SimUniform {
                step: [dt, self.energy, kick, level],
                spawn: [begin, begin + issued, PARTICLES, 0],
                motion: [SWIRL, PULL, DRAG, KICK],
                shape: [SPAWN_SPEED, SPAWN_RADIUS, DISC_THICKNESS, ORBIT_SPEED],
                life: [LIFE_MIN, LIFE_SPAN, 0.0, 0.0],
                colour: [hue, HUE_SPREAD, MAX_RADIUS, 0.0],
            };

            let at = SIM_STRIDE as usize * step;
            self.staging[at..at + std::mem::size_of::<SimUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
        }
        steps
    }
}

fn accumulation_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("particles-accumulation"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ACCUMULATION_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn present_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    accumulation: &wgpu::TextureView,
    composite: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("particles-present-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(accumulation),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: composite.as_entire_binding(),
            },
        ],
    })
}

impl Visual for ParticleField {
    fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        let Gpu { device, .. } = *gpu;
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.accumulation = accumulation_view(device, width, height);
        self.present_bind_group = present_group(
            device,
            &self.present_layout,
            &self.accumulation,
            &self.composite_buffer,
        );
    }

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

        // Nothing played is nothing measured, and the field holds where it is
        // rather than ageing through history that never happened -- decision
        // 21's rule, inherited from the tunnel and the waterfall.
        let steps = if frame.frames() == 0 {
            0
        } else {
            self.stage_steps(frame)
        };
        if steps > 0 {
            queue.write_buffer(&self.sim_buffer, 0, &self.staging);
        }

        let (eye, right, up) = eye_basis();
        let aspect = self.width as f32 / self.height.max(1) as f32;
        let view_proj = camera::fit(
            eye,
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            &hull(),
            aspect,
            FILL,
            NEAR,
            FAR,
        );
        let level = (frame.peak() * frame.gain).clamp(0.0, 1.0);
        queue.write_buffer(
            &self.scene_buffer,
            0,
            bytemuck::bytes_of(&SceneUniform {
                view_proj: view_proj.as_array(),
                right: [right[0], right[1], right[2], 0.0],
                up: [up[0], up[1], up[2], 0.0],
                params: [
                    SPARK_SIZE * spark_scale(self.height),
                    SPARK_BRIGHTNESS * (0.45 + 0.55 * level),
                    LIFE_MIN + LIFE_SPAN,
                    self.srgb,
                ],
            }),
        );
        let [r, g, b] = BACKGROUND;
        queue.write_buffer(
            &self.composite_buffer,
            0,
            bytemuck::bytes_of(&CompositeUniform {
                settings: [EXPOSURE, self.srgb, 0.0, 0.0],
                background: [r as f32, g as f32, b as f32, 1.0],
            }),
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("particles-encoder"),
        });
        for step in 0..steps {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("particles-sim-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.sim_pipeline);
            pass.set_bind_group(
                0,
                Some(&self.sim_bind_group),
                &[SIM_STRIDE as u32 * step as u32],
            );
            pass.dispatch_workgroups(PARTICLES.div_ceil(WORKGROUP), 1, 1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("particles-spark-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.accumulation,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Cleared every frame: the accumulation holds *this*
                        // frame's light, and the history a particle field has is
                        // the particles themselves.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.spark_pipeline);
            pass.set_bind_group(0, Some(&self.scene_bind_group), &[]);
            pass.set_vertex_buffer(0, self.particles.slice(..));
            pass.draw(0..6, 0..PARTICLES);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("particles-present-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(background(format)),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.present_pipeline);
            pass.set_bind_group(0, Some(&self.present_bind_group), &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hull_is_the_ball_the_camera_is_framed_for() {
        // Every hull point is on the framed ball rather than inside it, or the
        // fit would frame something smaller than it was asked to.
        for point in hull() {
            let radius = (point[0] * point[0] + point[1] * point[1] + point[2] * point[2]).sqrt();
            assert!(
                (radius - FRAMED_RADIUS).abs() < 1e-4,
                "hull point at {radius} is not on the framed ball"
            );
        }
        assert_eq!(hull().len(), 26);
        // And the hard bound is outside the framing, which is what makes a
        // transient throwing sparks past the edge of the frame a bounded event
        // rather than an unbounded one.
        const { assert!(MAX_RADIUS > FRAMED_RADIUS) };
    }

    #[test]
    fn the_billboard_axes_are_orthonormal_and_face_the_eye() {
        let (eye, right, up) = eye_basis();
        let distance = (eye[0] * eye[0] + eye[1] * eye[1] + eye[2] * eye[2]).sqrt();
        assert!((distance - EYE_DISTANCE).abs() < 1e-4);
        for axis in [right, up] {
            let length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
            assert!((length - 1.0).abs() < 1e-4, "axis of length {length}");
        }
        let dot = right[0] * up[0] + right[1] * up[1] + right[2] * up[2];
        assert!(dot.abs() < 1e-4, "axes are not perpendicular: {dot}");
        // A quad built from these has to be edge-on to nothing: the plane they
        // span must face the eye, i.e. their cross product is the view
        // direction.
        let normal = cross(right, up);
        let towards_eye = normalise(eye);
        let facing =
            normal[0] * towards_eye[0] + normal[1] * towards_eye[1] + normal[2] * towards_eye[2];
        assert!(
            (facing.abs() - 1.0).abs() < 1e-3,
            "quads face {facing} of the way at the eye"
        );
    }

    #[test]
    fn a_spark_holds_its_size_to_the_eye_rather_than_in_the_world() {
        // The bug this pins was reported from a fullscreen look, not from a
        // check: sparks sized in world units are magnified along with the field
        // the camera is fitted to, so a 1440p window drew the 480px picture
        // three times bigger and called it blurry.
        assert!((spark_scale(480) - 1.0).abs() < 1e-6);
        // Twice the height, half the world size -- which is the same pixels.
        assert!((spark_scale(960) - 0.5).abs() < 1e-6);
        // A fullscreen 1440p window lands inside the range rather than on its
        // floor, so the compensation is real there rather than clamped away.
        assert!(spark_scale(1440) > SPARK_SCALE_RANGE.0);
        assert!((spark_scale(1440) - 1.0 / 3.0).abs() < 1e-6);
        // 4K is past the floor, and stops there: a spark under a couple of
        // pixels is aliasing rather than a shape.
        assert_eq!(spark_scale(2160), SPARK_SCALE_RANGE.0);
        // And a tiny window gets the other end of the clamp rather than sparks
        // the size of the field.
        assert_eq!(spark_scale(120), SPARK_SCALE_RANGE.1);
        assert_eq!(spark_scale(0), SPARK_SCALE_RANGE.1);
    }

    #[test]
    fn a_bigger_window_gets_more_sparks_rather_than_bigger_ones() {
        // The other half of the fullscreen finding: pixel-sized sparks alone
        // leave a large window looking thin.
        assert!((density_scale(480) - 1.0).abs() < 1e-6);
        assert!((density_scale(720) - 2.25).abs() < 1e-6);
        // Fullscreen asks for five times the reference and is given four, which
        // is what the fixed budget can hold.
        assert_eq!(density_scale(1080), 4.0);
        assert_eq!(density_scale(2160), 4.0);
        // And a small window is never *thinned* -- the approved density is a
        // floor, not a ratio.
        assert_eq!(density_scale(320), 1.0);
        assert_eq!(density_scale(1), 1.0);
    }

    #[test]
    fn the_hue_follows_the_centroid_up_the_octaves() {
        assert!((hue_for(HUE_LOW_HZ) - HUE_AT_LOW).abs() < 1e-4);
        assert!((hue_for(HUE_HIGH_HZ) - HUE_AT_HIGH).abs() < 1e-4);
        // Logarithmic: the geometric middle of the range is the middle of the
        // colour run, which a linear map would put a long way lower.
        let middle = (HUE_LOW_HZ * HUE_HIGH_HZ).sqrt();
        assert!((hue_for(middle) - (HUE_AT_LOW + HUE_AT_HIGH) / 2.0).abs() < 1e-3);
        // Outside the range is the end of it, not a wrapped colour.
        assert!((hue_for(20.0) - HUE_AT_LOW).abs() < 1e-4);
        assert!((hue_for(20_000.0) - HUE_AT_HIGH).abs() < 1e-4);
        // Nothing sounding is a value, not a missing one (M5 decision 36).
        assert!((hue_for(0.0) - HUE_AT_LOW).abs() < 1e-4);
        assert!((hue_for(f32::NAN) - HUE_AT_LOW).abs() < 1e-4);
    }

    #[test]
    fn the_beat_pushes_only_when_there_is_a_beat_to_believe() {
        let none = Musical::default();
        assert_eq!(beat_kick(&none), 0.0);

        // A tempo nobody believes moves the field as little as no tempo at all.
        let unsure = Musical {
            bpm: 120.0,
            beat_confidence: 0.0,
            beat_phase: 0.0,
            ..Musical::default()
        };
        assert_eq!(beat_kick(&unsure), 0.0);

        let on_the_beat = Musical {
            bpm: 120.0,
            beat_confidence: 1.0,
            beat_phase: 0.0,
            ..Musical::default()
        };
        assert!((beat_kick(&on_the_beat) - 1.0).abs() < 1e-5);

        // And it is a beat rather than a wobble: half a beat later there is
        // nothing left of it at all.
        let off_the_beat = Musical {
            beat_phase: 0.5,
            ..on_the_beat
        };
        assert!(beat_kick(&off_the_beat) < 1e-6);

        // Continuous where the phase wraps, or every beat would arrive as a
        // step change.
        let nearly_round = Musical {
            beat_phase: 0.999,
            ..on_the_beat
        };
        assert!((beat_kick(&nearly_round) - beat_kick(&on_the_beat)).abs() < 0.01);
    }
}
