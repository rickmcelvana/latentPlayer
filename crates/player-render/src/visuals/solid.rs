//! Raymarched solid: PLAN.md section 4's fifth built-in alongside the particle
//! field, and the second half of Phase 3 M6.
//!
//! Decision 46 is what this visual is for: **it is a picture of the spectrum,
//! not a demo.** A raymarched blob driven by noise and an amplitude multiplier
//! would have been easier and would have looked much the same in a screenshot,
//! and there would have been nothing to check and nothing to read. Here the
//! surface's radius at a latitude *is* the analyser's own band at that
//! frequency -- placed by real Hz through [`VisualFrame::band_position`], the
//! way the bars' octave marks and the waterfall's rails are -- so the outline
//! from bottom to top is the display from low to high, and a bass-heavy passage
//! is a shape rather than a mood.
//!
//! What the music does to it, in the order a listener notices:
//!
//! - **The spectrum is the shape.** Low frequencies at the south pole, high at
//!   the north, over M2's own -72..0 dBFS window so the two displays agree
//!   about what "loud" looks like.
//! - **The beat is the size.** `beat_phase` swells the whole solid, scaled by
//!   `beat_confidence` (decision 45), so material with no pulse simply does not
//!   breathe rather than being given one.
//! - **Onsets are the surface.** The onset envelope of decision 43 -- the same
//!   idempotent `max` the particle field uses -- drives a ripple, so percussive
//!   music has a broken surface and sustained music a smooth one.
//! - **The brightness is the colour**, through [`palette`], shared with the
//!   particle field so the two cannot disagree about what 3kHz looks like.
//!
//! And decision 47, which is the other reason this file is not just a shader: a
//! fullscreen raymarch is priced in *fragments*, so it renders into a texture of
//! its own and upscales, and it does not multisample -- there is no geometric
//! edge here for four samples a pixel to improve.

use crate::attachments::{background, shader_source, target_is_srgb, BACKGROUND};
use crate::visual::{Gpu, Musical, Visual, VisualFrame};

use super::palette::hue_for_centroid;
use super::scale;

/// How many points the spectrum is resampled onto, running from
/// [`AXIS_LOW_HZ`] to [`AXIS_HIGH_HZ`].
///
/// A fixed axis rather than the frame's own band count, which is not a constant
/// a visual may assume (M2 decision 10): the shader indexes latitude straight
/// into this, and the resampling that gets it here is where the frame's real
/// frequencies are honoured. 64 also makes the texture row exactly 256 bytes,
/// which is every alignment rule a texture write has.
const AXIS_POINTS: usize = 64;

/// The frequency range the solid's outline covers, bottom to top.
///
/// Narrower at the bottom than the bars' display, deliberately: below about
/// 40Hz there is nothing on most material and a pole that never moves reads as
/// a mistake in the shape.
const AXIS_LOW_HZ: f32 = 40.0;
const AXIS_HIGH_HZ: f32 = 16_000.0;

/// The solid at rest, and how far the loudest band may push its surface out.
///
/// The displacement is over half the base radius on purpose: the spectrum has
/// to be the *dominant* feature of the silhouette, or the visual is a sphere
/// with a texture on it.
const BASE_RADIUS: f32 = 0.62;
const DISPLACE: f32 = 0.55;

/// How much the beat swells the whole solid, at full confidence.
const PULSE: f32 = 0.14;

/// How deep the onset ripple cuts at full envelope, and how fast that envelope
/// falls (per second).
///
/// The same shape of rule as the particle field's, and the same reason: an
/// onset is held in the published snapshot until the next drain replaces it, so
/// it enters through an idempotent `max` and leaves on a clock (decision 43).
const RIPPLE: f32 = 0.075;
const ONSET_DECAY: f32 = 3.5;

/// How bright the silhouette's rim is. The outline is where the spectrum is
/// actually read, so it gets its own light.
const RIM: f32 = 0.55;

/// The camera. Straight on and slightly above, because the shape is read
/// against its own axis: an off-axis eye compresses one end of the spectrum
/// into the other, which is precisely what M3 found by drawing its terrain and
/// looking at it.
const EYE: [f32; 3] = [0.0, 0.34, 2.35];

/// How much of the frame the solid fills at its largest.
const FILL: f32 = 0.86;

/// The furthest the surface can ever be from the middle.
///
/// Every term that can push it out, at once: the base at full beat swell, the
/// loudest possible band, and the deepest possible ripple. Nothing about the
/// music can exceed this, which is what lets the frame be fitted once rather
/// than watched.
fn max_reach() -> f32 {
    BASE_RADIUS * (1.0 + PULSE) + DISPLACE + RIPPLE
}

/// The tangent of the half field of view, for a viewport of this shape.
///
/// Derived rather than chosen, which is M1's lesson repeated: a fixed angle
/// frames a fixed *angle*, so it fits at one window shape and clips at the
/// next. The solid is a ball of radius [`max_reach`] at [`EYE`]'s distance, so
/// its angular radius is `asin(reach / distance)` and the tangent of that is
/// `reach / sqrt(distance^2 - reach^2)`.
///
/// Divided by the narrower of the two axes: the shader widens the horizontal by
/// the aspect ratio, so a *tall* window is bound horizontally and would clip the
/// sides of a solid fitted only to its height.
fn tan_half_fov(aspect: f32) -> f32 {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    };
    let distance = (EYE[0] * EYE[0] + EYE[1] * EYE[1] + EYE[2] * EYE[2]).sqrt();
    let reach = max_reach().min(distance * 0.98);
    // The tangent of the angle the solid actually subtends, then opened up so
    // that angle is `FILL` of the frame. Widened in *tangent* rather than in
    // angle, because a fraction of the frame is a fraction of the tangent --
    // which is how `camera::fit` states the same thing for the other visuals.
    let subtended = reach / (distance * distance - reach * reach).max(1e-4).sqrt();
    subtended / FILL / aspect.min(1.0)
}

/// The most fragments the march is allowed to cost, as a pixel count.
///
/// Decision 47's cap -- set from measurement, and the measurement moved it up by
/// a factor of three from where it was first guessed.
///
/// Timed against the vectorscope at the same sizes, so the readback they share
/// cancels out of the comparison: 1.34ms against 1.15ms at 720p, 2.78 against
/// 2.32 at 1080p, 4.51 against 4.57 at 1440p, and 9.63 against 9.05 at 4K. The
/// march itself therefore costs well under a millisecond even at 4K on this
/// machine, which is a discrete GPU and the only evidence there is -- so the
/// ceiling goes just above 4K and starts working only on surfaces past it (an
/// ultrawide span, an 8K panel).
///
/// The first guess was 2.4 million pixels, which would have quietly softened
/// every 1440p fullscreen window. That is worth recording because the live look
/// two days ago reported exactly that complaint -- "smooth but blurred" -- about
/// the particle field, and a cap chosen from caution rather than from a
/// stopwatch would have re-introduced it deliberately.
const MAX_MARCH_PIXELS: f32 = 8_300_000.0;

/// The march's own texture format: linear light, like the particle field's
/// accumulation and for the same reason -- exactly one pass converts for the
/// surface, so an sRGB window and a `bgra8unorm` canvas cannot disagree.
const MARCH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    eye: [f32; 4],
    forward: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    /// `[tan_half_fov, aspect, srgb_target, seconds]`.
    view: [f32; 4],
    /// `[base_radius, displace, pulse, ripple]`.
    shape: [f32; 4],
    /// `[hue, bands, has_spectrum, rim]`.
    colour: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PresentUniform {
    /// `[srgb_target, background r, g, b]`.
    settings: [f32; 4],
}

pub struct RaymarchedSolid {
    march_pipeline: wgpu::RenderPipeline,
    upscale_pipeline: wgpu::RenderPipeline,
    scene_buffer: wgpu::Buffer,
    present_buffer: wgpu::Buffer,
    levels_texture: wgpu::Texture,
    march_layout: wgpu::BindGroupLayout,
    upscale_layout: wgpu::BindGroupLayout,
    march_bind_group: wgpu::BindGroup,
    upscale_bind_group: wgpu::BindGroup,
    march_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// Decision 43's envelope, and the clock the ripple and the pulse are
    /// animated on. Seconds since this visual was built, which is the only
    /// thing here that is a time rather than a measurement.
    energy: f32,
    seconds: f32,
    /// The spectrum on this visual's own axis, rebuilt per frame.
    axis: Vec<f32>,
    width: u32,
    height: u32,
    srgb: f32,
}

/// The frequency of each point on the solid's axis, low to high.
fn axis_frequencies() -> Vec<f32> {
    let ratio = (AXIS_HIGH_HZ / AXIS_LOW_HZ).ln();
    (0..AXIS_POINTS)
        .map(|point| {
            let position = point as f32 / (AXIS_POINTS - 1) as f32;
            AXIS_LOW_HZ * (position * ratio).exp()
        })
        .collect()
}

/// The march's size for a given surface, honouring [`MAX_MARCH_PIXELS`].
///
/// Returns the surface's own size whenever it is affordable, so the common case
/// is native resolution and the cap is a ceiling rather than a policy.
fn march_size(width: u32, height: u32) -> (u32, u32) {
    let (width, height) = (width.max(1), height.max(1));
    let pixels = width as f32 * height as f32;
    if pixels <= MAX_MARCH_PIXELS {
        return (width, height);
    }
    let scale = (MAX_MARCH_PIXELS / pixels).sqrt();
    (
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    )
}

/// How much the beat swells the solid at this instant of it.
///
/// Zero without a tempo and quiet when the tempo is not believed (decision 45),
/// exactly as the particle field's kick is -- but a *swell* rather than an
/// impulse, since a solid that jumped on the beat would read as a flash where
/// this reads as a breath.
fn beat_swell(musical: &Musical) -> f32 {
    if musical.bpm <= 0.0 {
        return 0.0;
    }
    let phase = std::f32::consts::TAU * musical.beat_phase;
    // Highest on the beat and lowest halfway between, and continuous at the
    // wrap -- a solid that stepped at the bar line would tick rather than
    // breathe.
    let swell = (phase.cos() + 1.0) * 0.5;
    musical.beat_confidence.clamp(0.0, 1.0) * swell
}

fn march_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let (width, height) = march_size(width, height);
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("solid-march"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: MARCH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

impl RaymarchedSolid {
    pub(crate) fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let Gpu {
            device,
            queue: _,
            format,
        } = *gpu;
        let srgb = target_is_srgb(format);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("solid-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source(include_str!("solid.wgsl"))),
        });

        let scene_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("solid-scene"),
            size: std::mem::size_of::<SceneUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let present_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("solid-present"),
            size: std::mem::size_of::<PresentUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let levels_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("solid-levels"),
            size: wgpu::Extent3d {
                width: AXIS_POINTS as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let levels_view = levels_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("solid-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let march_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid-march-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Read with `textureLoad` and interpolated in the
                        // shader, which is the waterfall's arrangement: the
                        // interpolation is between *bands*, and a sampler would
                        // do it in texels.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let upscale_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid-upscale-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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

        let march_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("solid-march-bind-group"),
            layout: &march_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: scene_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&levels_view),
                },
            ],
        });

        let march_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("solid-march-pipeline-layout"),
                bind_group_layouts: &[Some(&march_layout)],
                immediate_size: 0,
            });
        let march_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid-march-pipeline"),
            layout: Some(&march_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_march"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MARCH_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            // Decision 47: no multisampling. The edges here come out of the
            // distance field and are shaded, not rasterised.
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let upscale_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("solid-upscale-pipeline-layout"),
                bind_group_layouts: &[Some(&upscale_layout)],
                immediate_size: 0,
            });
        let upscale_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid-upscale-pipeline"),
            layout: Some(&upscale_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_upscale"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let march_view = march_view(device, width, height);
        let upscale_bind_group = upscale_group(
            device,
            &upscale_layout,
            &march_view,
            &sampler,
            &present_buffer,
        );

        Self {
            march_pipeline,
            upscale_pipeline,
            scene_buffer,
            present_buffer,
            levels_texture,
            march_layout,
            upscale_layout,
            march_bind_group,
            upscale_bind_group,
            march_view,
            sampler,
            energy: 0.0,
            seconds: 0.0,
            axis: vec![0.0; AXIS_POINTS],
            width: width.max(1),
            height: height.max(1),
            srgb,
        }
    }

    /// Resamples the frame's own bands onto this visual's axis.
    ///
    /// Returns false when there is no spectrum to read, which the shader draws
    /// as *nothing* rather than as a sphere resting on the display floor -- the
    /// bars' answer to the same question (M2 decision 10).
    ///
    /// This is where decision 46 is actually kept: every point is placed by the
    /// frequency it belongs to, through the frame's own axis, so a change of
    /// output rate or band count moves nothing on the solid.
    fn resample(&mut self, frame: &VisualFrame) -> bool {
        if frame.levels.len() < 2 || frame.band_centres.len() != frame.levels.len() {
            return false;
        }
        // The display window, the height arithmetic and what gain means to a
        // logarithmic display are all `scale`'s -- M2 chose them, M3 moved them
        // there on the second caller, and this is the third. Two visuals
        // holding private opinions about what -72 dBFS looks like would be the
        // disagreement `player-core` exists one crate up to prevent.
        let gain_db = scale::gain_db(frame.gain);
        for (point, hz) in axis_frequencies().iter().enumerate() {
            self.axis[point] = match frame.band_position(*hz) {
                Some(position) => {
                    let low = position.floor() as usize;
                    let high = (low + 1).min(frame.levels.len() - 1);
                    let fraction = position - low as f32;
                    let level_db =
                        frame.levels[low] * (1.0 - fraction) + frame.levels[high] * fraction;
                    scale::normalise(level_db, gain_db)
                }
                // Off the end of the display's own axis: nothing, so a pole
                // outside what the analyser covers is flat rather than holding
                // whatever the nearest band happened to say.
                None => 0.0,
            };
        }

        // Smoothed along the axis, which is a real decision rather than a
        // polish pass. The bars draw a band as a *bar* -- a step is fine, it is
        // its own object. A surface cannot: sixty-four steps around a sphere is
        // a stack of slabs a few degrees tall, whose walls are nearly parallel
        // to the ray at the silhouette, and a raymarch through that overshoots
        // and punches holes through the solid. The first broadband render of
        // this visual was exactly that picture.
        //
        // A three-tap binomial, twice: wide enough to make a profile, narrow
        // enough that a single loud band is still a bulge you can point at.
        for _ in 0..2 {
            let mut smoothed = self.axis.clone();
            for (point, slot) in smoothed.iter_mut().enumerate() {
                let low = self.axis[point.saturating_sub(1)];
                let middle = self.axis[point];
                let high = self.axis[(point + 1).min(AXIS_POINTS - 1)];
                *slot = 0.25 * low + 0.5 * middle + 0.25 * high;
            }
            self.axis = smoothed;
        }
        true
    }
}

fn upscale_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    marched: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    present: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("solid-upscale-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(marched),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: present.as_entire_binding(),
            },
        ],
    })
}

impl Visual for RaymarchedSolid {
    fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        let Gpu { device, .. } = *gpu;
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.march_view = march_view(device, width, height);
        self.upscale_bind_group = upscale_group(
            device,
            &self.upscale_layout,
            &self.march_view,
            &self.sampler,
            &self.present_buffer,
        );
        let _ = &self.march_layout;
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

        let has_spectrum = self.resample(frame);
        if has_spectrum {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.levels_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&self.axis),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some((AXIS_POINTS * 4) as u32),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: AXIS_POINTS as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }

        // The clock only runs while something is playing, so a paused track
        // holds its surface still rather than rippling on in silence -- the
        // waterfall's and the tunnel's rule about elapsed time with nothing
        // measured (decision 21), applied to an animation instead of a history.
        if has_spectrum {
            self.seconds += frame.elapsed_seconds.max(0.0);
            let onset = frame.musical.onset.clamp(0.0, 1.0);
            self.energy = (self.energy - ONSET_DECAY * frame.elapsed_seconds.max(0.0)).max(onset);
        }

        let aspect = self.width as f32 / self.height.max(1) as f32;
        let forward = normalise([-EYE[0], -EYE[1], -EYE[2]]);
        let right = normalise(cross(forward, [0.0, 1.0, 0.0]));
        let up = cross(right, forward);
        queue.write_buffer(
            &self.scene_buffer,
            0,
            bytemuck::bytes_of(&SceneUniform {
                eye: [EYE[0], EYE[1], EYE[2], 0.0],
                forward: [forward[0], forward[1], forward[2], 0.0],
                right: [right[0], right[1], right[2], 0.0],
                up: [up[0], up[1], up[2], 0.0],
                view: [tan_half_fov(aspect), aspect, self.srgb, self.seconds],
                shape: [
                    BASE_RADIUS,
                    DISPLACE,
                    1.0 + PULSE * beat_swell(&frame.musical),
                    RIPPLE * self.energy,
                ],
                colour: [
                    hue_for_centroid(frame.musical.centroid_hz),
                    AXIS_POINTS as f32,
                    if has_spectrum { 1.0 } else { 0.0 },
                    RIM,
                ],
            }),
        );
        let [r, g, b] = BACKGROUND;
        queue.write_buffer(
            &self.present_buffer,
            0,
            bytemuck::bytes_of(&PresentUniform {
                settings: [self.srgb, r as f32, g as f32, b as f32],
            }),
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("solid-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("solid-march-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.march_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.march_pipeline);
            pass.set_bind_group(0, Some(&self.march_bind_group), &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("solid-upscale-pass"),
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
            pass.set_pipeline(&self.upscale_pipeline);
            pass.set_bind_group(0, Some(&self.upscale_bind_group), &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.finish()
    }
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
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / length, v[1] / length, v[2] / length]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_axis_is_log_spaced_between_the_frequencies_it_claims() {
        let axis = axis_frequencies();
        assert_eq!(axis.len(), AXIS_POINTS);
        assert!((axis[0] - AXIS_LOW_HZ).abs() < 0.01);
        assert!((axis[AXIS_POINTS - 1] - AXIS_HIGH_HZ).abs() < 1.0);
        // Log spacing means a constant *ratio* between neighbours, which is
        // what makes an octave the same distance along the solid wherever it
        // falls.
        let first = axis[1] / axis[0];
        for pair in axis.windows(2) {
            assert!(
                (pair[1] / pair[0] - first).abs() < 1e-3,
                "{} / {} is not the constant ratio {first}",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn the_march_runs_at_the_surface_size_until_that_gets_expensive() {
        // The common cases are native: the cap is a ceiling, not a policy, and
        // an ordinary fullscreen window is under it (decision 47).
        assert_eq!(march_size(480, 480), (480, 480));
        assert_eq!(march_size(1920, 1080), (1920, 1080));
        assert_eq!(march_size(2560, 1440), (2560, 1440));
        // Measured at under a millisecond, so 4K is native too.
        assert_eq!(march_size(3840, 2160), (3840, 2160));

        // Past 4K it is scaled by area rather than clamped by width, so the
        // picture keeps its shape.
        let (width, height) = march_size(5120, 2880);
        assert!(width < 5120 && height < 2880);
        let pixels = width as f32 * height as f32;
        assert!(
            (pixels - MAX_MARCH_PIXELS).abs() / MAX_MARCH_PIXELS < 0.01,
            "{pixels} pixels against a cap of {MAX_MARCH_PIXELS}"
        );
        let ratio = width as f32 / height as f32;
        assert!(
            (ratio - 5120.0 / 2880.0).abs() < 0.01,
            "aspect became {ratio}"
        );

        // A zero-sized surface is a size nobody may divide by.
        assert_eq!(march_size(0, 0), (1, 1));
    }

    /// The frame holds the solid at every shape a window can be -- M1's fit,
    /// arrived at the same way and for the same reason.
    #[test]
    fn the_frame_holds_the_solid_at_every_window_shape() {
        let distance = (EYE[0] * EYE[0] + EYE[1] * EYE[1] + EYE[2] * EYE[2]).sqrt();
        let reach = max_reach();
        for (width, height) in [
            (320.0, 320.0),
            (640.0, 480.0),
            (1280.0, 720.0),
            (3440.0, 1440.0),
            (600.0, 900.0),
            (400.0, 1200.0),
        ] {
            let aspect = width / height;
            let tangent = tan_half_fov(aspect);
            // The half-angles the two axes actually cover, and the angle the
            // solid subtends. Both have to be the larger.
            let vertical = tangent.atan();
            let horizontal = (tangent * aspect).atan();
            let subtended = (reach / distance).asin();
            assert!(
                vertical > subtended && horizontal > subtended,
                "{width}x{height}: solid subtends {subtended}, frame covers {vertical} by {horizontal}"
            );
            // And it fills the frame rather than sitting in the middle of a
            // mostly empty one: at the binding axis it is close to `FILL`.
            let binding = vertical.min(horizontal);
            let filled = subtended.tan() / binding.tan();
            assert!(
                filled > FILL - 0.06 && filled <= 1.0,
                "{width}x{height}: fills {filled} of the frame"
            );
        }
    }

    #[test]
    fn the_beat_swells_only_when_there_is_a_beat_to_believe() {
        assert_eq!(beat_swell(&Musical::default()), 0.0);

        let unsure = Musical {
            bpm: 120.0,
            beat_confidence: 0.0,
            ..Musical::default()
        };
        assert_eq!(beat_swell(&unsure), 0.0);

        let sure = Musical {
            bpm: 120.0,
            beat_confidence: 1.0,
            beat_phase: 0.0,
            ..Musical::default()
        };
        assert!((beat_swell(&sure) - 1.0).abs() < 1e-5);
        // Smallest halfway between two beats, and continuous where the phase
        // wraps -- a solid that stepped at the bar line would tick.
        assert!(
            beat_swell(&Musical {
                beat_phase: 0.5,
                ..sure
            }) < 1e-5
        );
        let nearly = beat_swell(&Musical {
            beat_phase: 0.999,
            ..sure
        });
        assert!((nearly - beat_swell(&sure)).abs() < 0.01);
    }
}
