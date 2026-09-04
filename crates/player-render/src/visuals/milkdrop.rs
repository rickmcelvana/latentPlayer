//! MilkDrop presets as a visual (Phase 3 M7a): a preset draws.
//!
//! The gallery's seventh entry and the first whose *scene is data*. Every
//! visual before it is a scene this project wrote; this one is an interpreter
//! for scenes strangers wrote, thousands at a time. That is the whole reason
//! decision 49 exists -- [`crate::VisualKind`] is a fixed enum of ids and
//! cannot name a preset, so selection here is two things (the visual, and the
//! preset it is showing) where every other visual's selection is one.
//!
//! What this visual owns is only what the host seam allows it (decision 1): a
//! scene. The preset text arrives through [`crate::Visual::set_preset`], the
//! analysis arrives in the [`VisualFrame`] like every other visual's, and
//! **no DSP happens here** (decision 2) -- `bass`, `mid` and `treb` are
//! summaries of the frame's own spectrum, computed the way [`VisualFrame::peak`]
//! is and for the same reason.
//!
//! With no preset loaded it draws MilkDrop's *defaults*, which are a real
//! preset: a slowly fading circular waveform. That is a better answer than a
//! black frame for a gallery entry someone is looking at before they have
//! pointed the app at a folder -- and it is honest, because it is exactly what
//! MilkDrop shows for a preset that sets nothing.

use std::sync::Arc;

use crate::milk::custom::{ShapeGeom, ShapeRig, WaveRig};
use crate::milk::frame::{CompositeSettings, MilkFrame, DEFAULT_MESH};
use crate::milk::shaders::{PresetInputs, PresetPasses};
use crate::milk::sprite::{SpriteDraw, SpriteRig};
use crate::milk::state::{FrameState, Inputs, Runtime};
use crate::milk::warp::{Grid, Warp};
use crate::milk::wave::{self, Wave, WaveInput, WAVE_SAMPLES};
use crate::milk::Preset;
use crate::visual::{Gpu, Visual, VisualFrame};

/// The largest feedback buffer this visual will allocate on its long edge.
///
/// MilkDrop's own ceiling: its texture-size menu stops here, and several
/// waveform modes have hard-coded behaviour for 256/512/1024/2048 and nothing
/// beyond. A preset is therefore *written* against a frame no larger than
/// this, and there is nothing in the loop -- a smeared, fed-back picture -- that
/// more texels would resolve. It also bounds the pair at 128MB rather than
/// letting a 4K fullscreen window decide.
const MAX_FEEDBACK_EDGE: u32 = 2048;

/// Where the spectrum is divided into MilkDrop's three bands, in Hz.
///
/// MilkDrop splits its own FFT into three contiguous ranges and calls them
/// bass, mid and treble; these are the frequencies those ranges land on for
/// the material presets are written against. Stated as frequencies rather than
/// as band indices because the frame's band count is not a constant a visual
/// may assume (M2 decision 10).
const BAND_EDGES: [f32; 4] = [20.0, 250.0, 2_000.0, 11_000.0];

/// How long the running average that `bass`/`mid`/`treb` are measured against
/// takes to follow the music, in seconds.
///
/// These three are *relative* levels in MilkDrop -- roughly 1.0 on ordinary
/// material and above it on a hit -- which is what makes a preset written
/// against one track look right on another. The average has to be long enough
/// not to chase the beat and short enough to follow a track change; MilkDrop's
/// own is a few seconds.
const AVERAGE_SECONDS: f32 = 4.0;

/// How long the `_att` variants lag their instantaneous counterparts.
const ATTACK_SECONDS: f32 = 0.25;

/// The floor a running average is held above, so silence does not divide a
/// tiny number by a tinier one and report a hit.
const LEVEL_FLOOR: f32 = 1e-4;

/// The three relative levels a preset reads, and the state behind them.
struct Levels {
    average: [f32; 3],
    attenuated: [f32; 3],
    relative: [f32; 3],
    started: bool,
}

impl Levels {
    fn new() -> Self {
        Self {
            average: [LEVEL_FLOOR; 3],
            attenuated: [1.0; 3],
            relative: [1.0; 3],
            started: false,
        }
    }

    /// Reads the frame's own spectrum and updates the running averages.
    ///
    /// A summary of data already in the frame, not a measurement: exactly the
    /// licence [`VisualFrame::peak`] operates under.
    fn update(&mut self, frame: &VisualFrame, dt: f32) {
        let mut raw = [0.0f32; 3];
        if !frame.levels.is_empty() && frame.levels.len() == frame.band_centres.len() {
            let mut counts = [0u32; 3];
            for (level, centre) in frame.levels.iter().zip(frame.band_centres) {
                let Some(band) = BAND_EDGES
                    .windows(2)
                    .position(|w| *centre >= w[0] && *centre < w[1])
                else {
                    continue;
                };
                // dBFS back to an amplitude: presets multiply by these, and a
                // decibel is not a thing to multiply by.
                raw[band] += 10f32.powf(level / 20.0);
                counts[band] += 1;
            }
            for (band, count) in raw.iter_mut().zip(counts) {
                if count > 0 {
                    *band /= count as f32;
                }
            }
        }

        if !self.started {
            // The first frame sets the average rather than easing toward it,
            // so a preset does not open on four seconds of a spurious hit.
            self.average = raw.map(|v| v.max(LEVEL_FLOOR));
            self.started = true;
        }
        let follow = |seconds: f32| 1.0 - (-dt.max(0.0) / seconds).exp();
        let slow = follow(AVERAGE_SECONDS);
        let fast = follow(ATTACK_SECONDS);
        for (((average, relative), attenuated), raw) in self
            .average
            .iter_mut()
            .zip(self.relative.iter_mut())
            .zip(self.attenuated.iter_mut())
            .zip(raw)
        {
            *average += (raw - *average) * slow;
            *relative = (raw / average.max(LEVEL_FLOOR)).clamp(0.0, 8.0);
            *attenuated += (*relative - *attenuated) * fast;
        }
    }
}

/// One MilkDrop preset, drawing.
pub struct MilkDropVisual {
    preset: Arc<Preset>,
    runtime: Runtime,
    frame: MilkFrame,
    grid: Grid,
    levels: Levels,
    /// Seconds since this preset was selected -- a preset's own clock, which
    /// MilkDrop restarts per preset and thousands of presets assume is small.
    time: f32,
    frames: u32,
    fps: f32,
    /// The two channels, kept between frames so [`wave::prepare`]'s smoothing
    /// has somewhere to run and the allocation happens once.
    left: Vec<f32>,
    right: Vec<f32>,
    /// Cleared on the next draw, so the loop never warps the previous preset's
    /// picture under the new one's equations.
    needs_clear: bool,
    /// The environment a preset's own shaders read, and the pipelines they
    /// compile to. Present whether or not this preset has any -- the blur
    /// chain and the noise textures live here too.
    passes: PresetPasses,
    /// What the translator made of this preset, for a report: which passes
    /// took, and what each one asked for that this build does not have.
    report: Vec<String>,
    /// The preset's texture-pack names, in the bindless table's slot order
    /// (decision 59). Set by [`Self::translate`]; a name is a slot.
    wants: Vec<String>,
    /// The preset's custom waves and shapes, one rig each (M7c).
    waves: Vec<WaveRig>,
    shapes: Vec<ShapeRig>,
    /// The preset's sprites (M7c), and each one's decoded image, parallel to
    /// the rigs. `None` until the host hands the image over.
    sprites: Vec<SpriteRig>,
    sprite_textures: Vec<Option<wgpu::TextureView>>,
}

impl MilkDropVisual {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        Self::with_preset(gpu, width, height, Arc::new(Preset::default()))
    }

    pub fn with_preset(gpu: &Gpu, width: u32, height: u32, preset: Arc<Preset>) -> Self {
        let (w, h) = feedback_size(width, height);
        let frame = MilkFrame::new(gpu.device, gpu.format, w, h, DEFAULT_MESH.0, DEFAULT_MESH.1);
        let grid = Grid::new(DEFAULT_MESH.0, DEFAULT_MESH.1, frame.aspect());
        let passes = PresetPasses::new(gpu.device, gpu.queue, w, h, gpu.format);
        let (waves, shapes) = build_rigs(&preset);
        let sprites: Vec<SpriteRig> = preset.sprites.iter().map(SpriteRig::new).collect();
        let sprite_textures = vec![None; sprites.len()];
        Self {
            passes,
            report: Vec::new(),
            wants: Vec::new(),
            runtime: Runtime::new(&preset),
            preset,
            frame,
            grid,
            levels: Levels::new(),
            time: 0.0,
            frames: 0,
            fps: 60.0,
            left: vec![0.0; WAVE_SAMPLES],
            right: vec![0.0; WAVE_SAMPLES],
            needs_clear: true,
            waves,
            shapes,
            sprites,
            sprite_textures,
        }
    }

    /// Swaps the preset being drawn, keeping the surface and the pipelines.
    ///
    /// The same shape as the host's own visual swap (decision 5) one level
    /// down: a preset change rebuilds the equations and empties the loop, and
    /// touches nothing else -- which is what makes flipping through a folder of
    /// a thousand presets cheap.
    pub fn set_preset(&mut self, preset: Arc<Preset>) {
        let (waves, shapes) = build_rigs(&preset);
        self.sprites = preset.sprites.iter().map(SpriteRig::new).collect();
        self.sprite_textures = vec![None; self.sprites.len()];
        self.runtime = Runtime::new(&preset);
        self.preset = preset;
        self.waves = waves;
        self.shapes = shapes;
        self.time = 0.0;
        self.frames = 0;
        self.levels = Levels::new();
        self.needs_clear = true;
    }

    /// Decodes the images the host read beside the preset and binds them. A
    /// name that matches a texture-pack name fills that layer of the
    /// user-texture array; one that matches a sprite's `SpriteName` becomes
    /// that sprite's image (decision 59).
    pub fn set_textures(&mut self, gpu: &Gpu, textures: &[(String, Vec<u8>)]) {
        let mut ordered: Vec<Option<crate::milk::image::DecodedImage>> =
            vec![None; self.wants.len()];
        for (name, bytes) in textures {
            if let Some(slot) = self.wants.iter().position(|w| w == name) {
                ordered[slot] = crate::milk::image::decode(bytes);
            }
            for (rig, texture) in self.sprites.iter_mut().zip(self.sprite_textures.iter_mut()) {
                if rig.name == *name {
                    if let Some(image) = crate::milk::image::decode(bytes) {
                        rig.img_size = [image.width as f32, image.height as f32];
                        *texture = Some(upload_sprite_image(gpu, &image));
                    }
                }
            }
        }
        self.passes.set_user_textures(gpu.queue, &ordered);
    }

    /// Translate this preset's shaders and build what compiles.
    ///
    /// Per pass, not per preset: **605 presets in the corpus give one pass a
    /// shader and leave the other fixed-function**, and a composite that will
    /// not translate should not cost a preset its warp. Whatever does not take
    /// falls back to M7a's path, which is a picture rather than nothing.
    ///
    /// The translation itself is [`crate::milk::report::translate_shaders`],
    /// shared with the preset library so a row and a selection cannot disagree
    /// about what a preset needs.
    fn translate(&mut self, device: &wgpu::Device) {
        self.report.clear();
        let textures = self.preset.texture_names();
        self.wants = textures
            .iter()
            .take(crate::milk::shaders::MAX_USER_TEXTURES)
            .cloned()
            .collect();
        for overflow in textures
            .iter()
            .skip(crate::milk::shaders::MAX_USER_TEXTURES)
        {
            self.report.push(format!(
                "texture:{overflow}: not loaded; more textures than one preset can name"
            ));
        }
        let (warp, comp) = crate::milk::report::translate_shaders(&self.preset, &mut self.report);
        let (warp_ok, comp_ok) = self
            .passes
            .set_shaders(device, warp.as_deref(), comp.as_deref());
        if warp.is_some() && !warp_ok {
            self.report
                .push("warp shader: translated but would not compile".into());
        }
        if comp.is_some() && !comp_ok {
            self.report
                .push("composite shader: translated but would not compile".into());
        }
    }

    fn rebuild(&mut self, gpu: &Gpu, width: u32, height: u32) {
        let (w, h) = feedback_size(width, height);
        // The blur chain's targets are sized from the feedback buffer, so a
        // resize rebuilds both together -- and the preset's pipelines with
        // them, since they are bound to this layout.
        self.passes = PresetPasses::new(gpu.device, gpu.queue, w, h, gpu.format);
        self.translate(gpu.device);
        self.frame = MilkFrame::new(gpu.device, gpu.format, w, h, DEFAULT_MESH.0, DEFAULT_MESH.1);
        self.grid = Grid::new(DEFAULT_MESH.0, DEFAULT_MESH.1, self.frame.aspect());
        self.needs_clear = true;
    }

    /// Copies the newest samples out of the tap into the two channel buffers,
    /// scaled and smoothed the way MilkDrop does before any mode sees them.
    fn load_waveform(&mut self, frame: &VisualFrame, scale: f32, smoothing: f32) {
        self.left.clear();
        self.right.clear();
        let channels = usize::from(frame.channels.max(1));
        let available = frame.samples.len() / channels;
        let take = available.min(WAVE_SAMPLES);
        // Silence in front rather than behind: the newest sample has to land
        // at the end, or a preset that has just started plays its waveform
        // backwards through the modes that read ahead.
        self.left.resize(WAVE_SAMPLES - take, 0.0);
        self.right.resize(WAVE_SAMPLES - take, 0.0);
        let start = available - take;
        for i in 0..take {
            let base = (start + i) * channels;
            self.left.push(frame.samples[base]);
            // A mono device is one channel drawn twice, which is what the
            // stereo modes should show for it.
            self.right
                .push(frame.samples[base + usize::from(channels > 1)]);
        }
        // Display gain applies here and nowhere else in this visual: it is the
        // one place an amplitude becomes a size on screen (the warp and the
        // filters are ratios, and scaling those would change the preset).
        let scale = scale * frame.gain;
        wave::prepare(&mut self.left, scale, smoothing);
        wave::prepare(&mut self.right, scale, smoothing);
    }

    fn inputs(&self) -> Inputs {
        let (w, h) = self.frame.size();
        let aspect = self.frame.aspect();
        Inputs {
            time: self.time,
            fps: self.fps,
            frame: self.frames,
            // Meaningless without a preset rotation, which is M7c. Zero is
            // what MilkDrop reports at the start of a preset, so a preset that
            // fades itself in over `progress` simply stays at its start.
            progress: 0.0,
            bass: self.levels.relative[0],
            mid: self.levels.relative[1],
            treb: self.levels.relative[2],
            bass_att: self.levels.attenuated[0],
            mid_att: self.levels.attenuated[1],
            treb_att: self.levels.attenuated[2],
            mesh_x: self.grid.mesh_x,
            mesh_y: self.grid.mesh_y,
            pixels_x: w,
            pixels_y: h,
            // MilkDrop publishes the inverses under these names; `state.rs`
            // says so where the field is declared.
            aspect_x: aspect.inv_x,
            aspect_y: aspect.inv_y,
        }
    }

    fn build_mesh(&mut self, queue: &wgpu::Queue, state: &FrameState) {
        let fixed = self.runtime.fixed;
        let (width, height) = self.frame.size();
        let warp = Warp::new(
            width,
            height,
            self.time,
            fixed.warp_anim_speed,
            fixed.warp_scale,
        );
        // Evaluated on the CPU because `per_pixel` is EEL and there is nothing
        // to run it on a GPU with. It is also, measurably, not where the cost
        // is: the spike found `per_pixel` free in the two most expensive
        // presets in the corpus (decision 53).
        let runtime = &mut self.runtime;
        let grid = &self.grid;
        self.frame.set_mesh(queue, grid, |point| {
            let (px, py) = grid.pixel_inputs(point);
            let motion = runtime.run_pixel(&state.motion, px, py, point.rad, point.ang);
            warp.source_for(&motion, point)
        });
    }
}

/// Compiles the preset's custom waves and shapes into their rigs (M7c).
fn build_rigs(preset: &Preset) -> (Vec<WaveRig>, Vec<ShapeRig>) {
    (
        preset.waves.iter().map(WaveRig::new).collect(),
        preset.shapes.iter().map(ShapeRig::new).collect(),
    )
}

/// Uploads a sprite's decoded image at its own size -- unlike a texture pack,
/// a sprite is not resized, because its quad's aspect correction reads the
/// image's real dimensions.
fn upload_sprite_image(gpu: &Gpu, image: &crate::milk::image::DecodedImage) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: image.width.max(1),
        height: image.height.max(1),
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("milk-sprite-image"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &image.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(image.width.max(1) * 4),
            rows_per_image: Some(image.height.max(1)),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The feedback pair's size for a given viewport.
///
/// The window's shape, capped on its long edge -- so a preset's aspect
/// behaviour follows the window it is actually in, and a 4K fullscreen does not
/// allocate a third of a gigabyte to hold a picture that is mostly smear.
fn feedback_size(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (width.max(1), height.max(1));
    let long = w.max(h);
    if long <= MAX_FEEDBACK_EDGE {
        return (w, h);
    }
    let scale = MAX_FEEDBACK_EDGE as f32 / long as f32;
    (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    )
}

impl Visual for MilkDropVisual {
    fn set_preset(&mut self, _gpu: &Gpu, preset: Option<Arc<Preset>>) {
        // `None` is not "leave it alone": it is the host saying nothing is
        // selected, which happens when a library is cleared or a stored
        // selection names a file that has since moved. MilkDrop's defaults are
        // the honest picture for that.
        MilkDropVisual::set_preset(self, preset.unwrap_or_else(|| Arc::new(Preset::default())));
        self.translate(_gpu.device);
    }

    fn set_textures(&mut self, gpu: &Gpu, textures: &[(String, Vec<u8>)]) {
        MilkDropVisual::set_textures(self, gpu, textures);
    }

    fn preset_report(&self) -> Vec<String> {
        self.report.clone()
    }

    fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        if (width, height) == (0, 0) {
            return;
        }
        if feedback_size(width, height) != self.frame.size() {
            self.rebuild(gpu, width, height);
        }
    }

    fn draw(
        &mut self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        frame: &VisualFrame,
    ) -> wgpu::CommandBuffer {
        let dt = frame.elapsed_seconds.clamp(0.0, 0.25);
        self.time += dt;
        self.frames = self.frames.saturating_add(1);
        if dt > 1e-4 {
            // Smoothed, because a preset dividing by `fps` should not see a
            // number that jumps by a factor of two when one frame is late.
            self.fps += (1.0 / dt - self.fps) * 0.1;
        }
        self.levels.update(frame, dt);

        let state = self.runtime.run_frame(&self.inputs());
        let fixed = self.runtime.fixed;
        self.load_waveform(frame, fixed.wave_scale, fixed.wave_smoothing);
        self.build_mesh(gpu.queue, &state);

        let aspect = self.frame.aspect();
        let (feedback_width, _) = self.frame.size();
        let engine_inputs = self.inputs();
        let q_seed: [f64; 32] = {
            let q = self.runtime.q();
            std::array::from_fn(|i| q[i] as f64)
        };
        let left = self.left.clone();
        let right = self.right.clone();

        let mut waves: Vec<Wave> = Vec::new();
        if let Some(built_in) = wave::build(
            &state.wave,
            &fixed,
            &WaveInput {
                left: &left,
                right: &right,
                time: self.time,
                aspect,
                width: feedback_width,
                bass: self.levels.relative[0],
                mid: self.levels.relative[1],
                treb: self.levels.relative[2],
            },
        ) {
            waves.push(built_in);
        }
        for rig in &mut self.waves {
            if let Some(custom) = rig.build(&engine_inputs, &q_seed, &left, &right, frame.levels) {
                waves.push(custom);
            }
        }

        let mut shapes: Vec<ShapeGeom> = Vec::new();
        for rig in &mut self.shapes {
            shapes.extend(rig.build(&engine_inputs, &q_seed, aspect));
        }

        // Sprites (M7c): each one a textured quad, its transform and blend
        // settled by its own code. The screen aspect is the feedback buffer's.
        let screen_aspect = {
            let (w, h) = self.frame.size();
            w as f32 / h.max(1) as f32
        };
        let mut sprites: Vec<(SpriteDraw, wgpu::TextureView)> = Vec::new();
        for (rig, texture) in self.sprites.iter_mut().zip(self.sprite_textures.iter()) {
            if let Some(texture) = texture {
                if let Some(draw) = rig.build(&engine_inputs, screen_aspect) {
                    sprites.push((draw, texture.clone()));
                }
            }
        }

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("milkdrop-visual"),
            });
        if std::mem::take(&mut self.needs_clear) {
            self.frame.clear(&mut encoder);
        }

        let (fw, fh) = self.frame.size();
        let inputs = PresetInputs {
            time: self.time,
            fps: self.fps,
            frame: self.frames,
            progress: 0.0,
            bass: self.levels.relative[0],
            mid: self.levels.relative[1],
            treb: self.levels.relative[2],
            vol: (self.levels.relative[0] + self.levels.relative[1] + self.levels.relative[2])
                / 3.0,
            bass_att: self.levels.attenuated[0],
            mid_att: self.levels.attenuated[1],
            treb_att: self.levels.attenuated[2],
            vol_att: (self.levels.attenuated[0]
                + self.levels.attenuated[1]
                + self.levels.attenuated[2])
                / 3.0,
            q: self.runtime.q(),
            blur_min: state.blur_min,
            blur_max: state.blur_max,
            aspect,
            width: fw,
            height: fh,
        };
        self.passes.write_uniforms(gpu.queue, &inputs);

        // The blur runs on the picture as it stands *before* this frame's
        // warp, which is what a warp shader reading `GetBlur1` expects: the
        // blur of what it is about to move, not of what it has just drawn.
        if self.passes.wants_blur() {
            self.passes.run_blur(
                gpu.device,
                gpu.queue,
                &mut encoder,
                self.frame.current_view(),
                &inputs,
            );
        }

        match self.passes.warp_pipeline() {
            Some(pipeline) => {
                let bind = self
                    .passes
                    .bind_group(gpu.device, self.frame.current_view());
                self.frame.advance_with(&mut encoder, pipeline, &bind);
            }
            None => {
                self.frame
                    .advance(gpu.device, gpu.queue, &mut encoder, state.decay, state.wrap)
            }
        }

        self.frame
            .draw_overlays(gpu.device, gpu.queue, &mut encoder, &state, &waves, &shapes);
        self.frame
            .draw_sprites(gpu.device, gpu.queue, &mut encoder, &sprites);

        let settings = CompositeSettings {
            time: self.time,
            shader: fixed.shader,
        };
        match self.passes.comp_pipeline() {
            Some(pipeline) => {
                let bind = self
                    .passes
                    .bind_group(gpu.device, self.frame.current_view());
                let extra = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("milk-comp-extra"),
                    layout: self.passes.comp_extra_layout(),
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.frame.composite_uniform().as_entire_binding(),
                    }],
                });
                self.frame.composite_with(
                    gpu.queue,
                    &mut encoder,
                    target,
                    &state,
                    &settings,
                    pipeline,
                    &bind,
                    &extra,
                );
            }
            None => self.frame.composite(
                gpu.device,
                gpu.queue,
                &mut encoder,
                target,
                &state,
                &settings,
            ),
        }
        encoder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feedback_buffer_follows_the_window_until_milkdrops_own_ceiling() {
        assert_eq!(feedback_size(1280, 720), (1280, 720));
        assert_eq!(feedback_size(2048, 1536), (2048, 1536));
        // A 4K window, capped on its long edge with the shape kept -- so the
        // preset still knows what shape of frame it is drawing into.
        let (w, h) = feedback_size(3840, 2160);
        assert_eq!(w, MAX_FEEDBACK_EDGE);
        let before = 3840.0 / 2160.0;
        let after = w as f32 / h as f32;
        assert!((before - after).abs() < 0.01, "{w}x{h}");
        // And a degenerate size does not produce a zero-sized texture.
        assert_eq!(feedback_size(0, 0), (1, 1));
    }

    #[test]
    fn the_band_edges_are_ordered_and_cover_the_audible_spectrum() {
        assert!(BAND_EDGES.windows(2).all(|w| w[0] < w[1]));
        assert!(BAND_EDGES.iter().all(|hz| (20.0..=20_000.0).contains(hz)));
    }

    #[test]
    fn silence_reads_as_ordinary_rather_than_as_a_hit() {
        // The failure this guards is division by a running average that has
        // decayed toward zero: a preset driven by `bass` would then explode
        // the moment anything at all sounded after a quiet passage.
        let mut levels = Levels::new();
        let quiet = vec![-120.0f32; 8];
        let centres: Vec<f32> = (0..8).map(|i| 40.0 * 2f32.powi(i)).collect();
        let samples = [0.0f32; 4];
        let frame =
            VisualFrame::new(&samples, 2, 48_000, 1.0 / 60.0).with_spectrum(&quiet, &centres);
        for _ in 0..600 {
            levels.update(&frame, 1.0 / 60.0);
        }
        for band in levels.relative {
            assert!(band.is_finite(), "{band}");
            assert!(band < 2.0, "silence read as {band}");
        }
    }

    #[test]
    fn a_frame_with_no_spectrum_yet_does_not_produce_a_nan() {
        // The web shell's rows are empty until its shared buffer exists (M2
        // decision 10), so this is the first frame on that shell every time.
        let mut levels = Levels::new();
        let samples = [0.0f32; 4];
        let frame = VisualFrame::new(&samples, 2, 48_000, 1.0 / 60.0);
        levels.update(&frame, 1.0 / 60.0);
        assert!(levels.relative.iter().all(|v| v.is_finite()));
        assert!(levels.attenuated.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_loud_band_reads_above_one_against_its_own_average() {
        let mut levels = Levels::new();
        let centres: Vec<f32> = (0..8).map(|i| 40.0 * 2f32.powi(i)).collect();
        let samples = [0.0f32; 4];
        let steady = vec![-30.0f32; 8];
        let frame =
            VisualFrame::new(&samples, 2, 48_000, 1.0 / 60.0).with_spectrum(&steady, &centres);
        for _ in 0..600 {
            levels.update(&frame, 1.0 / 60.0);
        }
        for band in levels.relative {
            assert!((band - 1.0).abs() < 0.05, "steady material: {band}");
        }

        // Now a hit, one frame of it: 12dB is four times the amplitude.
        let loud = vec![-18.0f32; 8];
        let hit = VisualFrame::new(&samples, 2, 48_000, 1.0 / 60.0).with_spectrum(&loud, &centres);
        levels.update(&hit, 1.0 / 60.0);
        for band in levels.relative {
            assert!(band > 3.5 && band < 4.5, "a hit read {band}");
        }
    }
}
