//! The display scale two visuals share: what a dB is worth, where a band sits,
//! and where the octaves are.
//!
//! Extracted in M3, when the waterfall wanted every one of these verbatim from
//! the bars. The rule is the one `camera::fit` was extracted under -- move it
//! when there is a second caller, not before -- and the reason is stronger
//! here than convenience: two visuals holding private opinions about what -72
//! dBFS means would be exactly the disagreement `player-core` exists to prevent
//! one crate up. The bars and the waterfall draw the same array and must draw
//! it on the same axis, or switching between them says the music changed.

/// The dB the bottom of a spectrum display means. **Not**
/// `player-core::spectrum::FLOOR_DB`, which is -120 because that is where a
/// *measurement* should stop; a display drawn on a -120..0 axis spends more
/// than half its height on material below the noise floor of any real source,
/// so everything looks tall and nothing reads (M2 decision 13). The top is 0
/// dBFS: full scale fills the frame, exactly as it does for the vectorscope.
pub const RANGE_DB: f32 = 72.0;

/// The most bands a visual will draw. `analysis::BAND_COUNT` is 64 and the
/// buffers are sized once at construction; this is the headroom, not an
/// expectation. The band count itself is always the frame's to say (M2
/// decision 10).
pub const MAX_BANDS: usize = 256;

/// The C's, in Hz -- equal temperament, A4 = 440. The octave marks (M2 decision
/// 14): the reference an eye needs to read a log axis, placed by *frequency*
/// through the frame's own band centres rather than by an assumed band layout.
pub const OCTAVE_C_HZ: [f32; 10] = [
    32.703, 65.406, 130.813, 261.626, 523.251, 1046.502, 2093.005, 4186.009, 8372.018, 16744.036,
];

/// Where a band's slot sits across the display, -0.5 to 0.5.
///
/// `index` is fractional so an octave mark can land between two bands, which is
/// where most of them do: the C's are frequencies, and the bands are a display
/// grid that has no reason to line up with them.
pub fn slot_centre(index: f32, bands: usize) -> f32 {
    (index + 0.5) / bands.max(1) as f32 - 0.5
}

/// A dBFS level as a height, 0 at [`RANGE_DB`] below full scale and 1 at it.
pub fn normalise(level_db: f32, offset_db: f32) -> f32 {
    if !level_db.is_finite() {
        return 0.0;
    }
    ((level_db + offset_db) / RANGE_DB + 1.0).clamp(0.0, 1.0)
}

/// Display gain, as a spectrum display applies it: **decibels** (M2 decision
/// 12).
///
/// `frame.gain` is an amplitude multiplier because the vectorscope draws
/// amplitudes; a spectrum display is logarithmic, so x2 is +6dB here. Ignoring
/// gain instead would leave one control doing something in one visual and
/// nothing in the next, which is worse for a person than either behaviour
/// alone.
pub fn gain_db(gain: f32) -> f32 {
    if gain.is_finite() && gain > 0.0 {
        20.0 * gain.log10()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_display_floor_is_the_visuals_own_and_not_the_meters() {
        // Decision 13: -72dB is the bottom of the picture. A level at the
        // meter's -120 floor draws nothing at all rather than something short.
        // `player-core::spectrum::FLOOR_DB`, which this display deliberately
        // does not use as its own bottom.
        assert_eq!(normalise(-120.0, 0.0), 0.0);
        assert_eq!(normalise(-RANGE_DB, 0.0), 0.0);
        assert_eq!(normalise(0.0, 0.0), 1.0);
        // Halfway up the display is halfway down the range, which is what makes
        // the axis readable rather than merely monotonic.
        assert!((normalise(-RANGE_DB / 2.0, 0.0) - 0.5).abs() < 1e-6);
        // Above full scale is still the top: nothing leaves the frame.
        assert_eq!(normalise(6.0, 0.0), 1.0);
    }

    #[test]
    fn display_gain_is_decibels_here() {
        // Decision 12: the multiplier the vectorscope magnifies by is an
        // offset on a logarithmic axis -- x2 is +6dB, x8 is +18dB.
        assert!((gain_db(2.0) - 6.0206).abs() < 1e-3);
        assert!((gain_db(8.0) - 18.0618).abs() < 1e-3);
        assert_eq!(gain_db(1.0), 0.0);
        // A gain of zero or worse is not a silent display, it is a bug
        // upstream; unity is the safe reading of it.
        assert_eq!(gain_db(0.0), 0.0);
        assert_eq!(gain_db(f32::NAN), 0.0);
        // And it moves the display the way a person would expect.
        let quiet = normalise(-24.0, 0.0);
        let lifted = normalise(-24.0, gain_db(2.0));
        assert!((lifted - quiet - 6.0206 / RANGE_DB).abs() < 1e-3);
    }

    #[test]
    fn the_bands_run_left_to_right_and_fill_the_width() {
        // The arithmetic both spectrum visuals place their bands with, which is
        // why "the same tone lights the same side" is one function rather than
        // two that agree today. Band 0 is the lowest frequency and sits left.
        for bands in [1usize, 8, 64, MAX_BANDS] {
            let first = slot_centre(0.0, bands);
            let last = slot_centre(bands as f32 - 1.0, bands);
            assert!(first < last || bands == 1, "{bands} bands run backwards");
            assert!(first >= -0.5 && last <= 0.5, "{bands} bands leave the span");
            // Centred: the first slot's gap to the edge equals the last one's.
            assert!(((first + 0.5) - (0.5 - last)).abs() < 1e-6);
        }
    }
}
