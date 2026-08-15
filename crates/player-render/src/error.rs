#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("failed to create a wgpu surface: {0}")]
    CreateSurface(#[from] wgpu::CreateSurfaceError),

    #[error("no suitable wgpu adapter for this surface: {0}")]
    RequestAdapter(#[from] wgpu::RequestAdapterError),

    #[error("failed to request a wgpu device: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),

    #[error("surface is not supported by the adapter (no default config)")]
    UnsupportedSurface,

    /// `wgpu::CurrentSurfaceTexture`'s non-`Success`/`Suboptimal` variants
    /// (Timeout/Occluded/Outdated/Lost/Validation) -- all recoverable by
    /// skipping this frame and trying again, per that type's own docs, so
    /// this crate exposes them as one variant rather than mirroring each.
    #[error("surface frame unavailable this frame: {0:?}")]
    SurfaceUnavailable(String),
}
