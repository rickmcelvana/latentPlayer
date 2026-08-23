//! Running a preset's translated shaders: the environment they read, the blur
//! chain they sample, and the two pipelines they are.
//!
//! [`hlsl`](super::hlsl) turns a preset's HLSL into WGSL that compiles. This
//! is what makes that WGSL *run* -- the uniform block, the sixteen bindings
//! [`prelude.wgsl`](super::hlsl::emit::PRELUDE) declares, and the passes that
//! fill them.
//!
//! # The blur chain is not optional decoration
//!
//! `GetBlur1` is the second-commonest call in the corpus after `tex2D` --
//! 27,921 uses -- and all three levels together outnumber `GetPixel`. A build
//! that bound a black texture there would draw a fifth of the collection
//! wrong in a way that looks like a preset's own choice.
//!
//! Two passes per level, horizontal then vertical, read out of MilkDrop's
//! `blur1_ps.fx`, `blur2_ps.fx` and `BlurPasses`: an eight-tap and a four-tap
//! filter over the weights `{4.0, 3.8, 3.5, 2.9, 1.9, 1.2, 0.7, 0.3}`, paired
//! so that four samples do the work of eight by landing between texels. Each
//! level is half the size of the one before.
//!
//! **A blur texture holds a remapped image.** The chain normalises into the
//! preset's `blur1_min`/`blur1_max` range, which is why `GetBlur1` in the
//! prelude reads `x * _c5.x + _c5.y` -- those two are the inverse of this
//! pass's `fscale`/`fbias`, and a chain that wrote the image straight out
//! would agree at the default settings and nowhere else.
//!
//! # The offsets MilkDrop's blur carries are not copied
//!
//! `blur1_ps.fx` offsets its reads by a whole source texel and `blur2_ps.fx`
//! by one in x. Both are the Direct3D 9 half-texel rule in disguise: a
//! destination half-texel is `0.5/dest`, and with the destination at half the
//! source's size that is exactly `1.0/src`. wgpu aligns pixel and texel
//! centres already, so copying them would displace every blur level by a
//! texel. Same finding as M7a's warp transform, one pass along.

use crate::attachments;

use super::hlsl::emit::PRELUDE;
use super::warp::Aspect;

/// How many blur levels a preset can read: `GetBlur1`, `2` and `3`.
pub const BLUR_LEVELS: usize = 3;

/// MilkDrop's blur kernel, from `BlurPasses`.
const W: [f32; 8] = [4.0, 3.8, 3.5, 2.9, 1.9, 1.2, 0.7, 0.3];

/// The uniform block the prelude declares. **Field order must match it.**
///
/// Fourteen constant registers plus eight `q` registers, which is MilkDrop's
/// own layout rather than one binding per name -- see `env.rs` for why.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PresetUniform {
    c: [[f32; 4]; 14],
    q: [[f32; 4]; 8],
    rand_frame: [f32; 4],
    rand_preset: [f32; 4],
    texsize_noise: [[f32; 4]; 6],
    /// `mat4x3<f32>`: four columns of three, each padded to sixteen bytes.
    rot: [[[f32; 4]; 4]; 24],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniform {
    /// Source size: w, h, 1/w, 1/h.
    src: [f32; 4],
    weights: [f32; 4],
    offsets: [f32; 4],
    /// fscale, fbias, w_div, edge_c3.
    misc: [f32; 4],
    /// edge_c1, edge_c2, and two spare.
    edge: [f32; 4],
}

/// Everything a preset's shaders read that is not the feedback buffer itself.
pub struct PresetInputs {
    pub time: f32,
    pub fps: f32,
    pub frame: u32,
    pub progress: f32,
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
    pub vol: f32,
    pub bass_att: f32,
    pub mid_att: f32,
    pub treb_att: f32,
    pub vol_att: f32,
    pub q: [f32; 32],
    pub blur_min: [f32; BLUR_LEVELS],
    pub blur_max: [f32; BLUR_LEVELS],
    pub aspect: Aspect,
    pub width: u32,
    pub height: u32,
}

/// The environment and passes a translated preset shader needs.
pub struct PresetPasses {
    uniform: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    /// filter/address in MilkDrop's own order: fw, fc, pw, pc.
    samplers: [wgpu::Sampler; 4],
    noise: Vec<wgpu::TextureView>,
    placeholder: wgpu::TextureView,
    blur: Vec<wgpu::TextureView>,
    blur_temp: Vec<wgpu::TextureView>,
    blur_size: Vec<(u32, u32)>,
    blur_layout: wgpu::BindGroupLayout,
    blur_horizontal: wgpu::RenderPipeline,
    blur_vertical: wgpu::RenderPipeline,
    blur_uniforms: Vec<wgpu::Buffer>,
    comp_extra_layout: wgpu::BindGroupLayout,
    warp: Option<wgpu::RenderPipeline>,
    comp: Option<wgpu::RenderPipeline>,
    wants_blur: bool,
    width: u32,
    height: u32,
    target_format: wgpu::TextureFormat,
}

impl PresetPasses {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk-preset-uniform"),
            size: std::mem::size_of::<PresetUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = |name, filter, address| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(name),
                address_mode_u: address,
                address_mode_v: address,
                address_mode_w: address,
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        use wgpu::FilterMode::{Linear, Nearest};
        use wgpu::{AddressMode::ClampToEdge as Clamp, AddressMode::Repeat as Wrap};
        let samplers = [
            sampler("milk-fw", Linear, Wrap),
            sampler("milk-fc", Linear, Clamp),
            sampler("milk-pw", Nearest, Wrap),
            sampler("milk-pc", Nearest, Clamp),
        ];

        // The noise textures, generated once. Deterministic on purpose; see
        // `noise.rs`.
        let mut noise = Vec::new();
        for spec in super::noise::NOISE {
            let data = super::noise::generate(spec, 0x5eed_1234);
            let depth = if spec.volume { spec.size } else { 1 };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(spec.name),
                size: wgpu::Extent3d {
                    width: spec.size,
                    height: spec.size,
                    depth_or_array_layers: depth,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: if spec.volume {
                    wgpu::TextureDimension::D3
                } else {
                    wgpu::TextureDimension::D2
                },
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(spec.size * 4),
                    rows_per_image: Some(spec.size),
                },
                wgpu::Extent3d {
                    width: spec.size,
                    height: spec.size,
                    depth_or_array_layers: depth,
                },
            );
            noise.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
        }

        // A texture pack this build does not load reads from here rather than
        // refusing the shader (decision 56). Mid grey rather than black, so a
        // preset multiplying by it still shows something.
        let placeholder = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("milk-placeholder"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &placeholder,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[128u8, 128, 128, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let placeholder = placeholder.create_view(&wgpu::TextureViewDescriptor::default());

        let layout = environment_layout(device);
        let (blur, blur_temp, blur_size) = blur_targets(device, width, height);
        let blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk-blur-layout"),
            entries: &[
                uniform_entry(0),
                texture_entry(1, wgpu::TextureViewDimension::D2),
                sampler_entry(2),
            ],
        });
        let blur_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("milk-blur"),
            source: wgpu::ShaderSource::Wgsl(BLUR_SHADER.into()),
        });
        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("milk-blur-pipeline-layout"),
            bind_group_layouts: &[Some(&blur_layout)],
            immediate_size: 0,
        });
        let blur_pipeline = |entry: &str, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&blur_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &blur_module,
                    entry_point: Some("blur_vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &blur_module,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: super::frame::FEEDBACK_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let blur_horizontal = blur_pipeline("blur_h_fs", "milk-blur-h");
        let blur_vertical = blur_pipeline("blur_v_fs", "milk-blur-v");
        let blur_uniforms = (0..BLUR_LEVELS * 2)
            .map(|i| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("milk-blur-uniform-{i}")),
                    size: std::mem::size_of::<BlurUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();

        let comp_extra_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk-comp-extra-layout"),
            entries: &[uniform_entry(0)],
        });

        Self {
            uniform,
            layout,
            samplers,
            noise,
            placeholder,
            blur,
            blur_temp,
            blur_size,
            blur_layout,
            blur_horizontal,
            blur_vertical,
            blur_uniforms,
            comp_extra_layout,
            warp: None,
            comp: None,
            wants_blur: false,
            width,
            height,
            target_format,
        }
    }

    /// Compile a preset's translated passes, each on its own.
    ///
    /// Per pass rather than per preset, because **605 presets in the corpus
    /// give one pass a shader and leave the other fixed-function**. A preset
    /// whose composite will not compile should still get its warp.
    ///
    /// Returns which of the two took.
    pub fn set_shaders(
        &mut self,
        device: &wgpu::Device,
        warp: Option<&str>,
        comp: Option<&str>,
    ) -> (bool, bool) {
        self.warp = warp.and_then(|src| {
            self.build(
                device,
                src,
                WARP_ENTRY,
                "milk_warp_vs",
                "milk_warp_fs",
                true,
            )
        });
        self.comp = comp.and_then(|src| {
            self.build(
                device,
                src,
                COMP_ENTRY,
                "milk_comp_vs",
                "milk_comp_fs",
                false,
            )
        });
        self.wants_blur = [warp, comp]
            .iter()
            .flatten()
            .any(|src| src.contains("GetBlur"));
        (self.warp.is_some(), self.comp.is_some())
    }

    fn build(
        &self,
        device: &wgpu::Device,
        translated: &str,
        entry: &str,
        vs: &str,
        fs: &str,
        mesh: bool,
    ) -> Option<wgpu::RenderPipeline> {
        // `shader_source` prepends `visuals/shared.wgsl`, which is where
        // `to_target` lives -- the composite needs it and every visual in this
        // crate gets it.
        let source = attachments::shader_source(&format!("{translated}\n{entry}"));
        // A translated preset is a stranger's program, so a module that will
        // not compile has to be a `None` rather than a panic -- and wgpu's own
        // answer arrives through an error scope, which is a future. naga is
        // wgpu's front end, so asking it directly is the same acceptance taken
        // now, in time for the other pass to still be built.
        if !valid_wgsl(&source) {
            return None;
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("milk-preset"),
            source: wgpu::ShaderSource::Wgsl(source),
        });
        let layouts: Vec<Option<&wgpu::BindGroupLayout>> = if mesh {
            vec![Some(&self.layout)]
        } else {
            vec![Some(&self.layout), Some(&self.comp_extra_layout)]
        };
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("milk-preset-layout"),
            bind_group_layouts: &layouts,
            immediate_size: 0,
        });
        let buffers: Vec<Option<wgpu::VertexBufferLayout>> = if mesh {
            vec![Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<super::frame::WarpVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &super::frame::WARP_ATTRIBUTES,
            })]
        } else {
            vec![]
        };
        let target_format = if mesh {
            super::frame::FEEDBACK_FORMAT
        } else {
            self.target_format
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk-preset-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(vs),
                buffers: &buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(fs),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Some(pipeline)
    }

    pub fn warp_pipeline(&self) -> Option<&wgpu::RenderPipeline> {
        self.warp.as_ref()
    }

    pub fn comp_pipeline(&self) -> Option<&wgpu::RenderPipeline> {
        self.comp.as_ref()
    }

    pub fn wants_blur(&self) -> bool {
        self.wants_blur
    }

    pub fn comp_extra_layout(&self) -> &wgpu::BindGroupLayout {
        &self.comp_extra_layout
    }

    /// Fill the uniform block for this frame.
    pub fn write_uniforms(&self, queue: &wgpu::Queue, input: &PresetInputs) {
        let mut u = PresetUniform {
            c: [[0.0; 4]; 14],
            q: [[0.0; 4]; 8],
            rand_frame: [0.0; 4],
            rand_preset: [0.0; 4],
            texsize_noise: [[0.0; 4]; 6],
            rot: [[[0.0; 4]; 4]; 24],
        };
        let a = input.aspect;
        u.c[0] = [a.x, a.y, a.inv_x, a.inv_y];
        u.c[2] = [input.time, input.fps, input.frame as f32, input.progress];
        u.c[3] = [input.bass, input.mid, input.treb, input.vol];
        u.c[4] = [input.bass_att, input.mid_att, input.treb_att, input.vol_att];
        // `_c5`/`_c6` undo what the blur chain's own scale and bias did, which
        // is what `GetBlur1..3` in the prelude multiply by.
        let span = |i: usize| (input.blur_max[i] - input.blur_min[i]).max(1e-6);
        u.c[5] = [span(0), input.blur_min[0], span(1), input.blur_min[1]];
        u.c[6] = [
            span(2),
            input.blur_min[2],
            input.blur_min[0],
            input.blur_max[0],
        ];
        let (w, h) = (input.width.max(1) as f32, input.height.max(1) as f32);
        u.c[7] = [w, h, 1.0 / w, 1.0 / h];
        // The roaming oscillators, at MilkDrop's own four rates.
        let roam = |rates: [f32; 4], sin: bool| {
            let mut out = [0.0; 4];
            for (i, r) in rates.iter().enumerate() {
                let t = input.time * r;
                out[i] = 0.5 + 0.5 * if sin { t.sin() } else { t.cos() };
            }
            out
        };
        u.c[8] = roam([0.3, 1.3, 5.0, 20.0], false);
        u.c[9] = roam([0.3, 1.3, 5.0, 20.0], true);
        u.c[10] = roam([0.005, 0.008, 0.013, 0.022], false);
        u.c[11] = roam([0.005, 0.008, 0.013, 0.022], true);
        u.c[13] = [
            input.blur_min[1],
            input.blur_max[1],
            input.blur_min[2],
            input.blur_max[2],
        ];
        for i in 0..8 {
            u.q[i] = [
                input.q[i * 4],
                input.q[i * 4 + 1],
                input.q[i * 4 + 2],
                input.q[i * 4 + 3],
            ];
        }
        // Deterministic, like everything else a check has to read back.
        let mut r = input.frame.wrapping_mul(2_654_435_761);
        let mut next = || {
            r ^= r << 13;
            r ^= r >> 17;
            r ^= r << 5;
            (r >> 8) as f32 / 16_777_216.0
        };
        u.rand_frame = [next(), next(), next(), next()];
        u.rand_preset = [0.37, 0.11, 0.83, 0.52];
        for (i, spec) in super::noise::NOISE.iter().enumerate() {
            let s = spec.size as f32;
            u.texsize_noise[i] = [s, s, 1.0 / s, 1.0 / s];
        }
        // The rotation families, at six speeds. MilkDrop builds these from a
        // per-session random basis; that generator was not read, so these are
        // this build's own -- fixed axes turning at the rates the family names
        // imply. A preset using them gets rotations that behave, rather than
        // rotations that match.
        const RATES: [f32; 6] = [0.0, 0.03, 0.2, 0.9, 4.0, 0.0];
        for (family, rate) in RATES.iter().enumerate() {
            for n in 0..4 {
                let index = family * 4 + n;
                let phase = if family == 5 {
                    // `rot_rand*`: re-rolled every frame.
                    (input.frame as f32) * 1.7 + n as f32 * 2.3
                } else {
                    input.time * rate + (index as f32) * 0.61
                };
                u.rot[index] = rotation(phase, index as f32 * 0.37);
            }
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
    }

    /// The bind group a preset's shaders read, over `main` as the feedback.
    pub fn bind_group(&self, device: &wgpu::Device, main: &wgpu::TextureView) -> wgpu::BindGroup {
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: self.uniform.as_entire_binding(),
        }];
        for (i, sampler) in self.samplers.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: 1 + i as u32,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: 5,
            resource: wgpu::BindingResource::TextureView(main),
        });
        for i in 0..BLUR_LEVELS {
            entries.push(wgpu::BindGroupEntry {
                binding: 6 + i as u32,
                resource: wgpu::BindingResource::TextureView(&self.blur[i]),
            });
        }
        for (i, view) in self.noise.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: 9 + i as u32,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: 15,
            resource: wgpu::BindingResource::TextureView(&self.placeholder),
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("milk-preset-bind"),
            layout: &self.layout,
            entries: &entries,
        })
    }

    /// Run the three blur levels over `main`.
    pub fn run_blur(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        main: &wgpu::TextureView,
        input: &PresetInputs,
    ) {
        // The horizontal pass folds eight taps into four by sampling between
        // texels; the vertical pass folds eight into two the same way.
        let (h_w, h_d, h_div) = horizontal_kernel();
        let (v_w, v_d, v_div) = vertical_kernel();

        let mut source_size = (self.width.max(1), self.height.max(1));
        for level in 0..BLUR_LEVELS {
            let (dw, dh) = self.blur_size[level];
            // The chain maps into the preset's own range for this level, and
            // `_c5`/`_c6` undo it in the shader.
            let lo = input.blur_min[level];
            let hi = input.blur_max[level];
            let scale = 1.0 / (hi - lo).max(1e-6);
            let bias = -lo * scale;
            let edge = 0.25;

            let src = [
                source_size.0 as f32,
                source_size.1 as f32,
                1.0 / source_size.0 as f32,
                1.0 / source_size.1 as f32,
            ];
            queue.write_buffer(
                &self.blur_uniforms[level * 2],
                0,
                bytemuck::bytes_of(&BlurUniform {
                    src,
                    weights: h_w,
                    offsets: h_d,
                    misc: [scale, bias, h_div, 5.0],
                    edge: [1.0 - edge, edge, 0.0, 0.0],
                }),
            );
            let temp_size = [dw as f32, dh as f32, 1.0 / dw as f32, 1.0 / dh as f32];
            queue.write_buffer(
                &self.blur_uniforms[level * 2 + 1],
                0,
                bytemuck::bytes_of(&BlurUniform {
                    src: temp_size,
                    weights: v_w,
                    offsets: v_d,
                    // The vertical pass does not rescale -- the horizontal one
                    // already put the values in range.
                    misc: [1.0, 0.0, v_div, 5.0],
                    edge: [1.0 - edge, edge, 0.0, 0.0],
                }),
            );

            let source_view = if level == 0 {
                main
            } else {
                &self.blur[level - 1]
            };
            self.blur_pass(
                device,
                encoder,
                &self.blur_horizontal,
                &self.blur_uniforms[level * 2],
                source_view,
                &self.blur_temp[level],
            );
            self.blur_pass(
                device,
                encoder,
                &self.blur_vertical,
                &self.blur_uniforms[level * 2 + 1],
                &self.blur_temp[level],
                &self.blur[level],
            );
            source_size = (dw, dh);
        }
    }

    fn blur_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        uniform: &wgpu::Buffer,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("milk-blur-bind"),
            layout: &self.blur_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    // Clamped: a blur that wrapped would drag the far edge
                    // into the near one.
                    resource: wgpu::BindingResource::Sampler(&self.samplers[1]),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("milk-blur-pass"),
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Whether naga accepts a module, which is what wgpu will do with it.
pub fn valid_wgsl(source: &str) -> bool {
    let Ok(module) = naga::front::wgsl::parse_str(source) else {
        return false;
    };
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .is_ok()
}

/// A rotation matrix as `mat4x3` columns, from two angles.
///
/// Four columns of three: MilkDrop's `float4x3`, which a preset uses with
/// `mul` to turn a coordinate. The fourth column is the translation, which is
/// zero here.
fn rotation(phase: f32, tilt: f32) -> [[f32; 4]; 4] {
    let (s, c) = phase.sin_cos();
    let (st, ct) = tilt.sin_cos();
    [
        [c * ct, s, -c * st, 0.0],
        [-s * ct, c, s * st, 0.0],
        [st, 0.0, ct, 0.0],
        [0.0, 0.0, 0.0, 0.0],
    ]
}

/// The horizontal pass's four weights and offsets, from `BlurPasses`.
fn horizontal_kernel() -> ([f32; 4], [f32; 4], f32) {
    let w1 = W[0] + W[1];
    let w2 = W[2] + W[3];
    let w3 = W[4] + W[5];
    let w4 = W[6] + W[7];
    let d1 = 2.0 * W[1] / w1;
    let d2 = 2.0 + 2.0 * W[3] / w2;
    let d3 = 4.0 + 2.0 * W[5] / w3;
    let d4 = 6.0 + 2.0 * W[7] / w4;
    let div = 0.5 / (w1 + w2 + w3 + w4);
    ([w1, w2, w3, w4], [d1, d2, d3, d4], div)
}

/// The vertical pass's two, which fold the same eight taps into four samples.
fn vertical_kernel() -> ([f32; 4], [f32; 4], f32) {
    let w1 = W[0] + W[1] + W[2] + W[3];
    let w2 = W[4] + W[5] + W[6] + W[7];
    let d1 = 2.0 * ((W[2] + W[3]) / w1);
    let d2 = 2.0 + 2.0 * ((W[6] + W[7]) / w2);
    let div = 1.0 / ((w1 + w2) * 2.0);
    ([w1, w2, 0.0, 0.0], [d1, d2, 0.0, 0.0], div)
}

fn blur_targets(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (
    Vec<wgpu::TextureView>,
    Vec<wgpu::TextureView>,
    Vec<(u32, u32)>,
) {
    let mut blur = Vec::new();
    let mut temp = Vec::new();
    let mut sizes = Vec::new();
    for level in 0..BLUR_LEVELS {
        // Each level is half the one before, which is what makes three levels
        // reach a genuinely wide radius for twelve taps.
        let w = (width >> (level + 1)).max(1);
        let h = (height >> (level + 1)).max(1);
        sizes.push((w, h));
        let make = |label: &str| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: super::frame::FEEDBACK_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        blur.push(make("milk-blur"));
        temp.push(make("milk-blur-temp"));
    }
    (blur, temp, sizes)
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(
    binding: u32,
    dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

/// The sixteen bindings `prelude.wgsl` declares, in its order.
fn environment_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    use wgpu::TextureViewDimension::{D2, D3};
    let mut entries = vec![uniform_entry(0)];
    for i in 0..4 {
        entries.push(sampler_entry(1 + i));
    }
    // 5 is the feedback, 6..8 the three blur levels, 9..12 the flat noise:
    // eight two-dimensional textures before the two volumes.
    for i in 0..8 {
        entries.push(texture_entry(5 + i, D2));
    }
    entries.push(texture_entry(13, D3));
    entries.push(texture_entry(14, D3));
    entries.push(texture_entry(15, D2));
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("milk-preset-layout"),
        entries: &entries,
    })
}

/// The vertex and fragment stages wrapped around a translated warp shader.
///
/// MilkDrop's own entry point takes the warped uv in `.xy` and the un-warped
/// in `.zw`, which is what `env::Pass::Warp`'s defines say -- so the mesh
/// carries both, plus the radius and angle the preset can read.
const WARP_ENTRY: &str = r#"
struct MilkWarpOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) uv_orig: vec2<f32>,
    @location(2) rad_ang: vec2<f32>,
};

@vertex
fn milk_warp_vs(
    @location(0) position: vec2<f32>,
    @location(1) source: vec2<f32>,
    @location(2) orig: vec2<f32>,
    @location(3) rad_ang: vec2<f32>,
) -> MilkWarpOut {
    var out: MilkWarpOut;
    out.clip = vec4<f32>(position, 0.0, 1.0);
    out.uv = source;
    out.uv_orig = orig;
    out.rad_ang = rad_ang;
    return out;
}

@fragment
fn milk_warp_fs(in: MilkWarpOut) -> @location(0) vec4<f32> {
    // The diffuse alpha is 1 here: MilkDrop's warp mesh is drawn opaque, and
    // the preset's `ret` is the whole colour.
    return milk_shader(vec4<f32>(in.uv, in.uv_orig), in.rad_ang, vec4<f32>(1.0, 1.0, 1.0, 1.0));
}
"#;

/// The same for a composite shader, which owns the final colour.
///
/// When a preset has one, MilkDrop's fixed-function composite -- the video
/// echo and the gamma -- is *not* also applied: those are the non-shader path,
/// and the shader is given the corner tint as `hue_shader` to use as it likes.
/// The four whole-frame filters are separate passes in MilkDrop and still
/// apply, and so does this crate's background lift.
const COMP_ENTRY: &str = r#"
struct MilkCompExtra {
    shade0: vec4<f32>,
    shade1: vec4<f32>,
    shade2: vec4<f32>,
    shade3: vec4<f32>,
    echo: vec4<f32>,
    misc: vec4<f32>,
    filters: vec4<f32>,
    background: vec4<f32>,
};

@group(1) @binding(0) var<uniform> comp_extra: MilkCompExtra;

struct MilkCompOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn milk_comp_vs(@builtin(vertex_index) index: u32) -> MilkCompOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: MilkCompOut;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn milk_comp_fs(in: MilkCompOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let top = mix(comp_extra.shade0.rgb, comp_extra.shade1.rgb, uv.x);
    let bottom = mix(comp_extra.shade2.rgb, comp_extra.shade3.rgb, uv.x);
    let hue = mix(top, bottom, uv.y);

    // `rad` runs 0 at the centre to 1 at the edge and `ang` is the angle
    // there, both aspect-corrected -- the same pair the warp mesh carries.
    let d = (uv - vec2<f32>(0.5)) * vec2<f32>(2.0) * vec2<f32>(u.c0.x, u.c0.y);
    let rad = length(d);
    let ang = atan2(d.y, d.x);

    var colour = milk_shader(
        vec4<f32>(uv, uv),
        vec2<f32>(rad, ang),
        vec4<f32>(hue, 1.0),
    ).rgb;

    if (comp_extra.filters.x > 0.5) { colour = 1.0 - (1.0 - colour) * (1.0 - colour); }
    if (comp_extra.filters.y > 0.5) { colour = colour * colour; }
    if (comp_extra.filters.z > 0.5) { colour = 2.0 * colour * (1.0 - colour); }
    if (comp_extra.filters.w > 0.5) { colour = 1.0 - colour; }

    colour = min(colour + comp_extra.background.rgb, vec3<f32>(1.0));
    return vec4<f32>(to_target(colour, comp_extra.misc.y), 1.0);
}
"#;

/// MilkDrop's two blur passes, as one module.
const BLUR_SHADER: &str = r#"
struct BlurUniform {
    src: vec4<f32>,
    weights: vec4<f32>,
    offsets: vec4<f32>,
    misc: vec4<f32>,
    edge: vec4<f32>,
};

@group(0) @binding(0) var<uniform> b: BlurUniform;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

struct BlurOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn blur_vs(@builtin(vertex_index) index: u32) -> BlurOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: BlurOut;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn tap(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(source, source_sampler, uv, 0.0).rgb;
}

// Eight taps in four samples, across. MilkDrop's own offsets land each sample
// between two texels so the hardware's bilinear filter does half the sum.
@fragment
fn blur_h_fs(in: BlurOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let dx = b.src.z;
    var blur = (tap(uv + vec2<f32>(b.offsets.x * dx, 0.0))
              + tap(uv - vec2<f32>(b.offsets.x * dx, 0.0))) * b.weights.x
             + (tap(uv + vec2<f32>(b.offsets.y * dx, 0.0))
              + tap(uv - vec2<f32>(b.offsets.y * dx, 0.0))) * b.weights.y
             + (tap(uv + vec2<f32>(b.offsets.z * dx, 0.0))
              + tap(uv - vec2<f32>(b.offsets.z * dx, 0.0))) * b.weights.z
             + (tap(uv + vec2<f32>(b.offsets.w * dx, 0.0))
              + tap(uv - vec2<f32>(b.offsets.w * dx, 0.0))) * b.weights.w;
    blur = blur * b.misc.z;
    // The scale and bias that put the result in the preset's blur range; the
    // prelude's GetBlur undoes exactly this.
    blur = blur * b.misc.x + b.misc.y;
    return vec4<f32>(blur, 1.0);
}

@fragment
fn blur_v_fs(in: BlurOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let dy = b.src.w;
    var blur = (tap(uv + vec2<f32>(0.0, b.offsets.x * dy))
              + tap(uv - vec2<f32>(0.0, b.offsets.x * dy))) * b.weights.x
             + (tap(uv + vec2<f32>(0.0, b.offsets.y * dy))
              + tap(uv - vec2<f32>(0.0, b.offsets.y * dy))) * b.weights.y;
    blur = blur * b.misc.z;
    // MilkDrop darkens the blur toward the frame's edge, so a wide blur does
    // not drag the border inward.
    var t = min(min(uv.x, uv.y), 1.0 - max(uv.x, uv.y));
    t = sqrt(max(t, 0.0));
    t = b.edge.x + b.edge.y * saturate(t * b.misc.w);
    blur = blur * t;
    return vec4<f32>(blur, 1.0);
}
"#;

/// The full WGSL a translated warp shader becomes, for a check to compile.
pub fn warp_module(translated: &str) -> String {
    attachments::shader_source(&format!("{translated}\n{WARP_ENTRY}")).into_owned()
}

/// The full WGSL a translated composite shader becomes.
pub fn comp_module(translated: &str) -> String {
    attachments::shader_source(&format!("{translated}\n{COMP_ENTRY}")).into_owned()
}

/// The prelude on its own, for a check that wants to know it parses.
pub fn prelude() -> &'static str {
    PRELUDE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blur_weights_are_milkdrops_own_and_sum_to_one() {
        // The kernel is normalised by `w_div`, so a flat field blurs to
        // itself -- which is the one property a blur must have and the one a
        // transcription error breaks.
        let (w, _, div) = horizontal_kernel();
        let total: f32 = w.iter().sum::<f32>() * 2.0 * div;
        assert!((total - 1.0).abs() < 1e-5, "horizontal sums to {total}");
        let (w, _, div) = vertical_kernel();
        let total: f32 = (w[0] + w[1]) * 2.0 * div;
        assert!((total - 1.0).abs() < 1e-5, "vertical sums to {total}");
    }

    #[test]
    fn the_offsets_are_ordered_and_reach_the_kernels_width() {
        let (_, d, _) = horizontal_kernel();
        assert!(d[0] < d[1] && d[1] < d[2] && d[2] < d[3]);
        // Eight taps either side, folded into four samples, so the furthest
        // lands near seven texels out.
        assert!(d[3] > 6.0 && d[3] < 8.0, "{d:?}");
    }

    #[test]
    fn the_uniform_block_is_the_size_the_prelude_declares() {
        // 22 vec4 registers, 2 random, 6 noise sizes, 24 mat4x3 of 64 bytes.
        let expected = (14 + 8 + 1 + 1 + 6) * 16 + 24 * 64;
        assert_eq!(std::mem::size_of::<PresetUniform>(), expected);
    }

    #[test]
    fn a_rotation_is_a_rotation() {
        // Three unit columns, which is what `mul` needs to turn a coordinate
        // without also scaling it.
        let m = rotation(0.7, 0.3);
        for column in m.iter().take(3) {
            let len =
                (column[0] * column[0] + column[1] * column[1] + column[2] * column[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "column length {len}");
        }
    }

    #[test]
    fn the_prelude_and_both_entry_points_are_valid_wgsl_on_their_own() {
        // The entry wrappers are the one part of a preset's module that is
        // this crate's own, so they are checked without a preset -- with a
        // `milk_shader` that does nothing, which is what a preset supplies.
        let stub = format!(
            "{PRELUDE}\nfn milk_shader(uv_in: vec4<f32>, rad_ang_in: vec2<f32>, \
             diffuse_in: vec4<f32>) -> vec4<f32> {{ return vec4<f32>(0.0); }}"
        );
        for module in [warp_module(&stub), comp_module(&stub)] {
            let parsed = naga::front::wgsl::parse_str(&module)
                .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&module)));
            let mut v = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            );
            v.validate(&parsed)
                .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&module)));
        }
    }

    #[test]
    fn the_blur_shader_is_valid_wgsl() {
        let parsed = naga::front::wgsl::parse_str(BLUR_SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(BLUR_SHADER)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        );
        v.validate(&parsed)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(BLUR_SHADER)));
    }
}
