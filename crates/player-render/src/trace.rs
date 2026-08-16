//! Turning a window of played audio into the vertices of the extruded scope.
//!
//! This is the whole difference between the 2D scope and the 3D one: the same
//! stereo pair still gives a point's x and y, but its *age* now gives a z, so
//! a moment of audio is a curve through space rather than a figure redrawn on
//! top of itself.
//!
//! Two properties are deliberate and tested here rather than left to the
//! caller (M6 decisions 1 and 2):
//!
//! - **A fixed duration, decimated to a fixed point count.** The depth axis is
//!   time, so a fixed *sample* count would show half as much music at 96kHz as
//!   at 48kHz and the same track would look different on different hardware.
//!   [`window_samples`] is the one place that turns the duration into a count.
//! - **The goniometer's axes.** x is the side signal, y the mid -- the same
//!   orthonormal rotation `player-core`'s `phase::goniometer_points` uses, so
//!   the flat meter and the 3D one agree about what mono looks like.

use bytemuck::{Pod, Zeroable};

/// How much history the extrusion shows. Roughly an eighth note at 120bpm:
/// long enough for the ribbon to have shape, short enough that it still moves
/// with the music instead of smearing into a tangle.
pub const TRACE_SECONDS: f32 = 0.12;

/// Points drawn per frame, whatever the sample rate. Also the vertex buffer's
/// size, so the draw call costs the same on every device.
pub const TRACE_POINTS: usize = 2048;

/// One point of the extruded trace.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable, PartialEq)]
pub struct Vertex {
    /// `[side, mid, age]`. Side and mid are real amplitudes (full-scale mono
    /// reaches sqrt(2), as in the goniometer); age runs 0 at the newest sample
    /// to 1 at the oldest. The shader scales them to fit the view -- how big
    /// the figure is drawn is a display decision, where the numbers are not.
    pub position: [f32; 3],
}

/// Interleaved samples one frame of the scope should be drawn from, for an
/// output running at `sample_rate` with `channels` channels.
///
/// Callers ask their own tap for exactly this many, which is what makes the
/// extrusion the same 120ms everywhere.
pub fn window_samples(sample_rate: u32, channels: u16) -> usize {
    let frames = (sample_rate as f32 * TRACE_SECONDS).round().max(1.0) as usize;
    frames * usize::from(channels.max(1))
}

/// Builds the trace's vertices into `out`, oldest first.
///
/// `samples` is interleaved in the output's layout, newest last -- exactly
/// what the analysis tap hands over. Shorter input than a full window is drawn
/// as-is rather than padded: at the start of a track there genuinely is less
/// history, and a ribbon that grows into its full length is honest about that.
pub fn build(samples: &[f32], channels: u16, out: &mut Vec<Vertex>) {
    out.clear();
    let channels = usize::from(channels.max(1));
    let frames = samples.len() / channels;
    if frames == 0 {
        return;
    }

    // Every nth frame rather than an average of each group: averaging pulls a
    // wide signal towards the centre and would quietly make every mix look
    // more mono than it is -- the same reason `goniometer_points` decimates
    // this way.
    let stride = frames.div_ceil(TRACE_POINTS).max(1);
    let scale = std::f32::consts::FRAC_1_SQRT_2;
    let oldest = frames.saturating_sub(1).max(1) as f32;

    let mut frame = 0;
    while frame < frames {
        let base = frame * channels;
        let left = samples[base];
        // A mono output has no stereo field: everything sits on the centre
        // line, which is the truthful picture of it.
        let right = if channels > 1 {
            samples[base + 1]
        } else {
            left
        };
        // Age, not index: the newest frame is at the end of the slice and must
        // be the near end of the ribbon.
        let age = 1.0 - (frame as f32 / oldest);
        out.push(Vertex {
            position: [(left - right) * scale, (left + right) * scale, age],
        });
        frame += stride;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interleaved stereo, right channel scaled by `right_gain`.
    fn tone(right_gain: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = (n as f32 / 16.0).sin();
                [s, s * right_gain]
            })
            .collect()
    }

    #[test]
    fn the_window_is_a_duration_rather_than_a_sample_count() {
        // The property the whole decision rests on: twice the rate, twice the
        // samples, same 120ms of music.
        let at_48 = window_samples(48_000, 2);
        let at_96 = window_samples(96_000, 2);
        assert_eq!(at_96, at_48 * 2);
        assert_eq!(at_48, (48_000.0 * TRACE_SECONDS) as usize * 2);
        // Channels are a width, not a duration.
        assert_eq!(window_samples(48_000, 1) * 2, at_48);
    }

    #[test]
    fn the_point_count_is_bounded_whatever_the_rate() {
        let mut out = Vec::new();
        for frames in [1usize, 100, TRACE_POINTS, 11_520] {
            build(&tone(1.0, frames), 2, &mut out);
            assert!(
                !out.is_empty() && out.len() <= TRACE_POINTS,
                "{frames} frames produced {} points",
                out.len()
            );
        }
    }

    #[test]
    fn age_runs_from_the_oldest_sample_to_the_newest() {
        let mut out = Vec::new();
        build(&tone(1.0, 1024), 2, &mut out);
        assert!((out[0].position[2] - 1.0).abs() < 1e-6, "oldest is age 1");
        assert!(
            out.last().expect("points").position[2] < 0.01,
            "newest is age 0"
        );
        assert!(
            out.windows(2).all(|w| w[1].position[2] < w[0].position[2]),
            "age must fall monotonically towards the newest sample"
        );
    }

    #[test]
    fn mono_material_has_no_side_component() {
        let mut out = Vec::new();
        build(&tone(1.0, 512), 2, &mut out);
        assert!(out.iter().all(|v| v.position[0].abs() < 1e-6));
        assert!(out.iter().any(|v| v.position[1].abs() > 0.5));
    }

    #[test]
    fn an_inverted_channel_has_no_mid_component() {
        let mut out = Vec::new();
        build(&tone(-1.0, 512), 2, &mut out);
        assert!(out.iter().all(|v| v.position[1].abs() < 1e-6));
        assert!(out.iter().any(|v| v.position[0].abs() > 0.5));
    }

    #[test]
    fn a_mono_output_draws_up_the_centre_line() {
        // One channel is not "no stereo information available", it is a mix
        // with no width -- which is exactly the centre line.
        let mut out = Vec::new();
        build(&[0.5, -0.5, 0.25], 1, &mut out);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|v| v.position[0] == 0.0));
        assert!((out[0].position[1] - 0.5 * 2.0 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn no_audio_draws_nothing() {
        // Not a centre dot: before anything has played there is no trace, and
        // a point at the origin would read as a signal.
        let mut out = vec![Vertex::default()];
        build(&[], 2, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn amplitudes_are_the_goniometers_own() {
        // Full-scale mono reaches sqrt(2) on the mid axis, matching
        // player-core's `phase::goniometer_points` exactly -- the two displays
        // have to be drawing the same numbers.
        let mut out = Vec::new();
        build(&[1.0, 1.0], 2, &mut out);
        assert!((out[0].position[1] - 2f32.sqrt()).abs() < 1e-6);
        assert_eq!(out[0].position[0], 0.0);
    }
}
