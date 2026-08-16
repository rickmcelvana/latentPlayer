//! The scope's renderer. This crate only knows how to draw into a
//! `wgpu::Surface` (or an offscreen texture) it is handed -- nothing here is
//! native- or web-specific. `src-tauri` hands it a Tauri `Window`'s raw handle;
//! `player-rt-wasm` hands it an `HtmlCanvasElement`. Same shader, same
//! pipeline, same draw call, both places -- the thing Phase 0 existed to prove
//! and the thing M6 keeps true now that the pipeline is a real 3D one.
//!
//! # What M6 changed
//!
//! Phase 0 drew a flat XY line strip in clip space: no camera, no depth, no
//! third dimension to have an opinion about. The scope now extrudes that
//! figure along a time axis (`trace.rs`), views it through a perspective
//! camera (`camera.rs`), and draws it depth-tested and multisampled
//! (`scene.rs`) -- PLAN.md section 4's hero feature, in its first form.
//!
//! The public surface is deliberately unchanged in shape: hand over the
//! samples that reached the output and the layout they are interleaved in, and
//! a frame gets drawn. Everything about how it looks stays in here.

mod camera;
mod error;
mod scene;
pub mod trace;

pub use error::RenderError;
pub use trace::{window_samples, TRACE_POINTS, TRACE_SECONDS};

use scene::Scene;

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scene: Scene,
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

        let scene = Scene::new(&device, &queue, config.format, config.width, config.height);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            scene,
            needs_reconfigure: false,
        })
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
        self.scene
            .resize(&self.device, &self.queue, self.config.format, width, height);
        self.needs_reconfigure = false;
    }

    /// `samples` is interleaved audio in the output's channel layout, newest
    /// last -- exactly what the analysis tap holds. `channels` is that layout's
    /// width, which need not be 2: the tap follows the output device, and a
    /// device with more channels would otherwise be de-interleaved wrongly.
    ///
    /// How many samples to hand over is [`window_samples`]: a fixed *duration*
    /// of history, so the extrusion means the same thing at every sample rate
    /// (M6 decision 1).
    pub fn draw_frame(&mut self, samples: &[f32], channels: u16) -> Result<(), RenderError> {
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

        let commands = self
            .scene
            .draw(&self.device, &self.queue, &view, samples, channels);
        self.queue.submit(Some(commands));
        self.queue.present(frame);
        Ok(())
    }
}

/// The same scene, rendered into a texture instead of onto a screen.
///
/// This exists to be *checked* (M6 decision 5): a renderer verified by looking
/// at it is a renderer that ships with a mirrored axis or a camera facing the
/// wrong way. `crates/player-render/examples/scope_smoke.rs` draws known audio
/// through this and asserts where the figure landed.
///
/// Native-only, because reading pixels back means blocking on a device poll,
/// and on the web that has to be awaited instead -- the check runs where the
/// checking happens, while the shader and pipeline it exercises are the shared
/// ones.
#[cfg(not(target_arch = "wasm32"))]
pub struct OffscreenRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

#[cfg(not(target_arch = "wasm32"))]
impl OffscreenRenderer {
    /// Colour format of the rendered image and of what `read_pixels` returns.
    /// sRGB, like a real surface, so the check sees the values a screen would.
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    pub async fn new(width: u32, height: u32) -> Result<Self, RenderError> {
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
        let scene = Scene::new(&device, &queue, Self::FORMAT, width, height);

        Ok(Self {
            device,
            queue,
            scene,
            texture,
            width,
            height,
        })
    }

    /// Draws one frame, then reads it back as RGBA8 rows, top row first.
    pub fn draw_to_pixels(
        &mut self,
        samples: &[f32],
        channels: u16,
    ) -> Result<Vec<u8>, RenderError> {
        let view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let commands = self
            .scene
            .draw(&self.device, &self.queue, &view, samples, channels);
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
