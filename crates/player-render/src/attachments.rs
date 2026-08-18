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

/// The field a visual draws on, as an 8-bit **display** colour -- what a screen
/// should actually show, not a number to hand a GPU.
///
/// Near-black rather than black: a true 0 makes the darkest end of a fading
/// trace indistinguishable from nothing at all, and gives a fullscreen
/// visualizer no sense of a surface behind it.
pub const BACKGROUND: [f64; 3] = [5.0 / 255.0, 5.0 / 255.0, 8.0 / 255.0];

/// [`BACKGROUND`] as this surface wants to be given it.
///
/// This exists because of a real difference between the two shells, reported
/// live 2026-08-18: the same clear colour drew near-black on the web and grey on
/// the desktop. Neither was a mistake in the value -- it was the *colour space*
/// of the surface each shell gets.
///
/// A clear value is always linear light. On a non-sRGB surface it is written
/// through as-is, so 0.02 lands in the framebuffer as byte 5 and a screen shows
/// near-black. On an **sRGB** surface the GPU encodes it on the way out, and the
/// same 0.02 linear becomes byte ~40 -- the grey. The web's canvas is
/// `bgra8unorm` (checked: `navigator.gpu.getPreferredCanvasFormat()` on this
/// machine's Chrome) while the desktop window's surface is sRGB, so one constant
/// gave two colours.
///
/// So the background is defined once, above, as the colour a *person* should
/// see, and converted here to whatever the surface in hand needs to produce it.
/// Both shells then show the same field, and a future visual that clears to
/// something else gets the same treatment for free.
pub fn background(format: wgpu::TextureFormat) -> wgpu::Color {
    let [r, g, b] = BACKGROUND;
    let convert = |channel: f64| {
        if format.is_srgb() {
            srgb_to_linear(channel)
        } else {
            channel
        }
    };
    wgpu::Color {
        r: convert(r),
        g: convert(g),
        b: convert(b),
        a: 1.0,
    }
}

/// Whether this surface encodes on a shader's behalf, as the number a uniform
/// carries: 1.0 for an sRGB surface, 0.0 for one that writes through.
///
/// The fragment-shader half of what [`background`] does for the clear colour --
/// see `visuals/shared.wgsl`, which is where the conversion itself lives, and
/// which every visual's shader is given by [`shader_source`].
pub fn target_is_srgb(format: wgpu::TextureFormat) -> f32 {
    if format.is_srgb() {
        1.0
    } else {
        0.0
    }
}

/// A visual's shader, with the shared preamble in front of it.
///
/// One copy of the colour conversion rather than one per visual: it is six
/// lines, and six lines duplicated per visual is where the fifth visual quietly
/// gets it wrong. WGSL has no include of its own, so this is the concatenation
/// that stands in for one.
pub fn shader_source(body: &str) -> std::borrow::Cow<'static, str> {
    format!("{}\n{}", include_str!("visuals/shared.wgsl"), body).into()
}

/// The sRGB transfer function, inverted. The piecewise form rather than a plain
/// 2.2 power: the linear segment near black is exactly where this background
/// sits, and the approximation is wrong by a whole byte there.
fn srgb_to_linear(channel: f64) -> f64 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// What an 8-bit screen ends up showing, given what was handed to the
    /// surface -- the inverse of `background`, so the two have to agree.
    fn displayed(colour: wgpu::Color, format: wgpu::TextureFormat) -> [u8; 3] {
        let encode = |linear: f64| {
            let display = if format.is_srgb() {
                if linear <= 0.0031308 {
                    linear * 12.92
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                }
            } else {
                linear
            };
            (display * 255.0).round() as u8
        };
        [encode(colour.r), encode(colour.g), encode(colour.b)]
    }

    #[test]
    fn the_background_looks_the_same_whatever_the_surfaces_colour_space() {
        // The bug this pins, reported live 2026-08-18: near-black on the web,
        // grey on the desktop, from one constant. The web's canvas is
        // `bgra8unorm` and the desktop window's surface is sRGB, and a clear
        // value is linear light -- so the sRGB surface encoded it on the way
        // out and the other did not.
        let want = [5u8, 5, 8];
        for format in [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            let shown = displayed(background(format), format);
            assert_eq!(
                shown, want,
                "{format:?} would show {shown:?} where every other surface shows {want:?}"
            );
        }
    }

    #[test]
    fn an_srgb_surface_is_handed_a_visibly_darker_number_than_a_linear_one() {
        // The direction matters, and getting it backwards would swap which
        // shell looks wrong rather than fixing either.
        let linear = background(wgpu::TextureFormat::Bgra8Unorm);
        let srgb = background(wgpu::TextureFormat::Bgra8UnormSrgb);
        assert!(
            srgb.r < linear.r,
            "sRGB was handed {}, linear {}",
            srgb.r,
            linear.r
        );
        // And the size of it: this is the grey-against-black the user saw.
        assert!((linear.r * 255.0 - 5.0).abs() < 0.01);
        assert!(srgb.r * 255.0 < 0.5);
    }

    #[test]
    fn the_background_is_dark_but_not_nothing() {
        // A true black leaves the fading end of a trace indistinguishable from
        // the field it is drawn on.
        assert!(BACKGROUND.iter().all(|c| *c > 0.0 && *c < 0.05));
    }
}
