//! The render-target attachments a 3D visual needs, in one place.
//!
//! Depth and multisample targets are per-*visual* state -- they are rebuilt on
//! resize, and a visual may reasonably want neither -- but their formats and
//! sample count are the host's business. Two visuals disagreeing about the
//! sample count would be two pipelines that cannot share a surface, and
//! switching between them would mean rebuilding rather more than the scene.

/// Samples per pixel. Lines are the one primitive that really needs this: an
/// aliased line strip on a dark field reads as a dotted, crawling mess, and the
/// vectorscope is nothing but line strips.
pub const SAMPLE_COUNT: u32 = 4;

/// Depth format. 32-bit float is universally supported as a render target
/// (including on WebGPU, where 24-bit variants are not guaranteed).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The multisampled colour target and the depth buffer, which are the two
/// things that have to be rebuilt whenever the viewport changes size.
pub fn targets(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (wgpu::TextureView, wgpu::TextureView) {
    let size = wgpu::Extent3d {
        width: width.max(1),
        height: height.max(1),
        depth_or_array_layers: 1,
    };
    let msaa = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("visual-msaa"),
        size,
        mip_level_count: 1,
        sample_count: SAMPLE_COUNT,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("visual-depth"),
        size,
        mip_level_count: 1,
        sample_count: SAMPLE_COUNT,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    (
        msaa.create_view(&wgpu::TextureViewDescriptor::default()),
        depth.create_view(&wgpu::TextureViewDescriptor::default()),
    )
}
