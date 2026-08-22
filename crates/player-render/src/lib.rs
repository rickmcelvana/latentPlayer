//! The visualizer host and its visuals. This crate only knows how to draw into
//! a `wgpu::Surface` (or an offscreen texture) it is handed -- nothing here is
//! native- or web-specific. `src-tauri` hands it a Tauri `Window`'s raw handle;
//! `player-rt-wasm` hands it an `HtmlCanvasElement`. Same shader, same
//! pipeline, same draw call, both places -- the thing Phase 0 existed to prove,
//! M6 kept true through a real 3D pipeline, and Phase 3 M1 now keeps true for a
//! *gallery* of scenes rather than one.
//!
//! # What M1 changed
//!
//! M6 left one scene wired straight into the renderer, and `draw_frame(&[f32],
//! channels)` was the vectorscope's signature rather than a visual's. PLAN.md
//! section 4 asks for several selectable visuals, so M1 drew the line once
//! (`visual.rs`, tasks/phase-3.md decision 1): **the host owns the surface, the
//! analysis feed, selection and fullscreen; a visual owns only its scene.**
//!
//! What that means here: [`Renderer`] holds a [`BoxedVisual`] instead of a
//! concrete scene, [`Renderer::set_visual`] swaps it between frames without
//! touching the surface (decision 5), and [`Renderer::draw_frame`] takes a
//! [`VisualFrame`] -- the feed the host assembles once per frame from the
//! analysis tap and the meters. Fullscreen is the shells', since a surface is
//! all this crate ever sees of a window.

mod attachments;
mod camera;
mod error;
mod gain;
pub mod milk;
pub mod trace;
mod visual;
mod visuals;

pub use error::RenderError;
pub use gain::GainSetting;
pub use trace::{window_samples, TRACE_POINTS, TRACE_SECONDS};
pub use visual::{BoxedVisual, Gpu, Musical, Visual, VisualFrame, VisualKind};
// How long the waterfall remembers -- a figure both smokes report, the way
// `TRACE_SECONDS` is reported for the vectorscope.
pub use visuals::{HISTORY_SECONDS, TUNNEL_SECONDS};

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    visual: BoxedVisual,
    kind: VisualKind,
    /// Display gain, and the automatic mode's memory (`gain.rs`). Host state
    /// rather than a visual's: one control, one meaning, every visual.
    gain: gain::Gain,
    /// Set when the surface told us its texture no longer matches it, so
    /// the *next* frame reconfigures before drawing. Deferred rather than
    /// done on the spot because the surface texture is still in hand at
    /// that point, and reconfiguring underneath it is not worth the risk
    /// when the frame is perfectly drawable as-is.
    needs_reconfigure: bool,
}

impl Renderer {
    /// `target` must be `'static` (an owned window handle, not a borrow) --
    /// see PLAN.md section 5's native-window note for why: the surface stores
    /// it internally to outlive short-lived borrows.
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
        kind: VisualKind,
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

        let visual = kind.create(
            &Gpu {
                device: &device,
                queue: &queue,
                format: config.format,
            },
            config.width,
            config.height,
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
            visual,
            kind,
            gain: gain::Gain::new(GainSetting::DEFAULT),
            needs_reconfigure: false,
        })
    }

    /// The GPU handles as a visual sees them.
    fn gpu(&self) -> Gpu<'_> {
        Gpu {
            device: &self.device,
            queue: &self.queue,
            format: self.config.format,
        }
    }

    /// Which visual is being drawn.
    pub fn visual(&self) -> VisualKind {
        self.kind
    }

    /// The display gain in effect.
    pub fn gain(&self) -> GainSetting {
        self.gain.setting()
    }

    /// Sets the display gain. Costs nothing and takes effect on the next frame:
    /// it is a number in the frame the visual is handed, not a rebuild.
    pub fn set_gain(&mut self, setting: GainSetting) {
        self.gain.set(setting);
    }

    /// Selects a visual, building its scene at the current size.
    ///
    /// The surface, the window and the canvas are all untouched (decision 5):
    /// switching visuals is a scene swap, not a teardown -- which is what lets
    /// the native shell keep one window for the whole session, and what keeps
    /// the swap cheap enough to happen between two frames of the redraw loop.
    ///
    /// Selecting the visual already showing rebuilds it anyway rather than
    /// returning early. It costs one pipeline and happens only when a person
    /// clicks something, and the alternative is a "reset this visual" path that
    /// would exist purely to be a second way of doing this.
    pub fn set_visual(&mut self, kind: VisualKind) {
        self.visual = kind.create(&self.gpu(), self.config.width, self.config.height);
        self.kind = kind;
    }

    /// Reconfigures the surface for a new window/canvas size. Callers must
    /// drive this from whatever tells them the size changed -- the renderer
    /// holds no window handle and cannot ask.
    ///
    /// Since M6 this also rebuilds the depth and multisample targets and the
    /// projection: a scope whose surface resized but whose camera did not
    /// would stretch the figure rather than showing more of the scene.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        let gpu = Gpu {
            device: &self.device,
            queue: &self.queue,
            format: self.config.format,
        };
        self.visual.resize(&gpu, width, height);
        self.needs_reconfigure = false;
    }

    /// Draws one frame of the selected visual from the host's feed.
    ///
    /// How many samples the frame should carry is [`window_samples`]: a fixed
    /// *duration* of history, so a visual means the same thing at every output
    /// rate (M6 decision 1, now the host's question rather than the scope's).
    pub fn draw_frame(&mut self, frame_data: &VisualFrame) -> Result<(), RenderError> {
        // Gain is resolved here, not by the shell and not by the visual: the
        // automatic mode is a state machine and there must be exactly one of it
        // (`gain.rs`). The frame the visual sees carries the answer.
        let frame_data = &VisualFrame {
            gain: self
                .gain
                .resolve(frame_data.peak(), frame_data.elapsed_seconds),
            ..*frame_data
        };
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

        let gpu = Gpu {
            device: &self.device,
            queue: &self.queue,
            format: self.config.format,
        };
        let commands = self.visual.draw(&gpu, &view, frame_data);
        self.queue.submit(Some(commands));
        self.queue.present(frame);
        Ok(())
    }
}

/// The same visuals, rendered into a texture instead of onto a screen.
///
/// This exists to be *checked* (M6 decision 5, carried into M1's decision 6): a
/// renderer verified by looking at it is a renderer that ships with a mirrored
/// axis or a camera facing the wrong way.
/// `crates/player-render/examples/visual_smoke.rs` draws known audio through
/// this, per visual, and asserts where the figure landed.
///
/// Native-only, because reading pixels back means blocking on a device poll,
/// and on the web that has to be awaited instead -- the check runs where the
/// checking happens, while the shader and pipeline it exercises are the shared
/// ones.
#[cfg(not(target_arch = "wasm32"))]
pub struct OffscreenRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    visual: BoxedVisual,
    gain: gain::Gain,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

#[cfg(not(target_arch = "wasm32"))]
impl OffscreenRenderer {
    /// Colour format of the rendered image and of what `read_pixels` returns.
    /// sRGB, like a real surface, so the check sees the values a screen would.
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    pub async fn new(width: u32, height: u32, kind: VisualKind) -> Result<Self, RenderError> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;

        let width = width.max(1);
        let height = height.max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scope-offscreen"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let visual = kind.create(
            &Gpu {
                device: &device,
                queue: &queue,
                format: Self::FORMAT,
            },
            width,
            height,
        );

        Ok(Self {
            device,
            queue,
            visual,
            gain: gain::Gain::new(GainSetting::DEFAULT),
            texture,
            width,
            height,
        })
    }

    /// Selects a visual, at the same size. The offscreen path swaps scenes the
    /// way the on-screen one does, so the check can exercise a *switch* rather
    /// than only a freshly built renderer per kind (decision 6).
    pub fn set_visual(&mut self, kind: VisualKind) {
        self.visual = kind.create(
            &Gpu {
                device: &self.device,
                queue: &self.queue,
                format: Self::FORMAT,
            },
            self.width,
            self.height,
        );
    }

    /// Sets the display gain, as the on-screen renderer does -- so the checks
    /// measure the same path the app draws through.
    pub fn set_gain(&mut self, setting: GainSetting) {
        self.gain.set(setting);
    }

    /// Draws one frame, then reads it back as RGBA8 rows, top row first.
    pub fn draw_to_pixels(&mut self, frame: &VisualFrame) -> Result<Vec<u8>, RenderError> {
        let frame = &VisualFrame {
            gain: self.gain.resolve(frame.peak(), frame.elapsed_seconds),
            ..*frame
        };
        let view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let gpu = Gpu {
            device: &self.device,
            queue: &self.queue,
            format: Self::FORMAT,
        };
        let commands = self.visual.draw(&gpu, &view, frame);
        self.queue.submit(Some(commands));
        self.read_pixels()
    }

    fn read_pixels(&self) -> Result<Vec<u8>, RenderError> {
        // Copies out of a texture are padded to 256-byte rows, which is a
        // hardware requirement rather than a preference -- so the buffer is
        // wider than the image and the padding is dropped on the way out.
        let unpadded = self.width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scope-readback"),
            size: u64::from(padded) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scope-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RenderError::SurfaceUnavailable(format!("device poll failed: {e}")))?;
        receiver
            .recv()
            .map_err(|e| RenderError::SurfaceUnavailable(format!("readback never landed: {e}")))?
            .map_err(|e| RenderError::SurfaceUnavailable(format!("buffer map failed: {e}")))?;

        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| RenderError::SurfaceUnavailable(format!("mapped range: {e}")))?;
        let mut pixels = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height as usize {
            let start = row * padded as usize;
            pixels.extend_from_slice(&mapped[start..start + unpadded as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(pixels)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}
