// Prepended to every visual's shader by `attachments::shader_source`.
//
// One job, and it is the fragment-shader half of the bug `attachments::background`
// fixed for the clear colour (2026-08-18, reported live: the same constant drew
// near-black on the web and grey on the desktop). A fragment shader writes
// *linear* light. An sRGB surface encodes it on the way out; the web's canvas is
// `bgra8unorm` and does not, so it shows the raw linear number -- and one
// constant produced two colours, exactly as it did for the background.
//
// The background was only the most visible instance, because it was a flat
// field with nowhere for the difference to hide. Every colour a visual has ever
// drawn carries it, and M2's dim octave marks are where it stops being subtle:
// a grid that reads on one shell would be nearly invisible on the other.
//
// So a visual writes the colour a *screen should show* -- exactly how
// `attachments::BACKGROUND` is written -- and calls `to_target` on the way out.
// The direction is the same one `attachments::background` takes, for the same
// reason: an sRGB surface is going to encode whatever it is handed, so it must
// be handed the decoded value.

/// The sRGB transfer function, inverted -- display to linear. The piecewise
/// form rather than a plain 2.2 power, matching `attachments::srgb_to_linear`
/// exactly: the linear segment near black is where a fading trace and a dim
/// gridline both live, and the approximation is wrong by a whole byte there.
fn srgb_to_linear(channel: f32) -> f32 {
    if (channel <= 0.04045) {
        return channel / 12.92;
    }
    return pow((channel + 0.055) / 1.055, 2.4);
}

/// A display colour, converted to what this surface must be handed to show it.
///
/// `srgb_target` is 1.0 when the surface will encode on the shader's behalf and
/// 0.0 when it writes through -- see `attachments::target_is_srgb`.
fn to_target(colour: vec3<f32>, srgb_target: f32) -> vec3<f32> {
    if (srgb_target < 0.5) {
        return colour;
    }
    return vec3<f32>(
        srgb_to_linear(colour.r),
        srgb_to_linear(colour.g),
        srgb_to_linear(colour.b),
    );
}

/// A hue on the full-saturation colour wheel, as RGB.
///
/// Here rather than in a visual since M6, when the raymarched solid became the
/// second caller: both it and the particle field colour themselves from the
/// spectral centroid through `visuals::palette`, and a wheel written twice is
/// two visuals that disagree about what 3kHz looks like. Written out rather
/// than pulled through an HSV conversion nobody here needs the other two thirds
/// of.
fn hue_to_rgb(hue: f32) -> vec3<f32> {
    let h = fract(hue) * 6.0;
    let x = 1.0 - abs(h % 2.0 - 1.0);
    if (h < 1.0) { return vec3<f32>(1.0, x, 0.0); }
    if (h < 2.0) { return vec3<f32>(x, 1.0, 0.0); }
    if (h < 3.0) { return vec3<f32>(0.0, 1.0, x); }
    if (h < 4.0) { return vec3<f32>(0.0, x, 1.0); }
    if (h < 5.0) { return vec3<f32>(x, 0.0, 1.0); }
    return vec3<f32>(1.0, 0.0, x);
}
