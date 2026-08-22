//! The one place a measurement becomes a colour.
//!
//! Extracted in M6 on its second caller, the rule `camera::fit`,
//! [`super::scale`] and [`super::history`] all moved under: the particle field
//! and the raymarched solid both colour themselves by the spectral centroid,
//! and two visuals answering "what colour is 3kHz" separately is two visuals
//! that disagree about the same music while sitting in the same gallery.

/// The frequencies the hue runs between, and the colours at each end.
///
/// Deep blue for a bass-heavy moment, warm violet for a bright one -- the app's
/// own accent colour is the middle of that range. Read from the spectral
/// centroid, so the colour is a *measurement* rather than a palette cycling on
/// its own (decision 46's rule, applied to colour as well as to shape).
pub const HUE_LOW_HZ: f32 = 80.0;
pub const HUE_HIGH_HZ: f32 = 8_000.0;
pub const HUE_AT_LOW: f32 = 0.62;
pub const HUE_AT_HIGH: f32 = 0.92;

/// Where a spectral centroid falls on the colour wheel.
///
/// Logarithmic, because pitch is: the octave from 100Hz to 200Hz has to move the
/// colour as far as the one from 2kHz to 4kHz, which a linear map would not do.
/// A centroid of zero means nothing is sounding, and takes the low end rather
/// than a colour of its own -- there is nothing to paint with it.
pub fn hue_for_centroid(centroid_hz: f32) -> f32 {
    if !centroid_hz.is_finite() || centroid_hz <= 0.0 {
        return HUE_AT_LOW;
    }
    let position = (centroid_hz.clamp(HUE_LOW_HZ, HUE_HIGH_HZ) / HUE_LOW_HZ).ln()
        / (HUE_HIGH_HZ / HUE_LOW_HZ).ln();
    HUE_AT_LOW + (HUE_AT_HIGH - HUE_AT_LOW) * position
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hue_follows_the_centroid_up_the_octaves() {
        assert!((hue_for_centroid(HUE_LOW_HZ) - HUE_AT_LOW).abs() < 1e-4);
        assert!((hue_for_centroid(HUE_HIGH_HZ) - HUE_AT_HIGH).abs() < 1e-4);
        // Logarithmic: the geometric middle of the range is the middle of the
        // colour run, which a linear map would put a long way lower.
        let middle = (HUE_LOW_HZ * HUE_HIGH_HZ).sqrt();
        assert!((hue_for_centroid(middle) - (HUE_AT_LOW + HUE_AT_HIGH) / 2.0).abs() < 1e-3);
        // Outside the range is the end of it, not a wrapped colour.
        assert!((hue_for_centroid(20.0) - HUE_AT_LOW).abs() < 1e-4);
        assert!((hue_for_centroid(20_000.0) - HUE_AT_HIGH).abs() < 1e-4);
        // Nothing sounding is a value, not a missing one (M5 decision 36).
        assert!((hue_for_centroid(0.0) - HUE_AT_LOW).abs() < 1e-4);
        assert!((hue_for_centroid(f32::NAN) - HUE_AT_LOW).abs() < 1e-4);
    }
}
