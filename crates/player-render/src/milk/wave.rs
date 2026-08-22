//! MilkDrop's built-in waveform: eight shapes made from the same two channels.
//!
//! The one thing drawn *into* the feedback buffer that is not the previous
//! frame, so it is what keeps a preset alive -- everything else in the loop
//! only moves and fades what is already there. A preset whose waveform does not
//! draw is a preset that fades to black in a couple of seconds, however right
//! the warp is.
//!
//! Geometry only: this module answers with points in clip space and a colour,
//! and knows nothing about wgpu. That is not tidiness -- the eight modes are
//! the part with no visual tell when they are subtly wrong (a mode-4 script
//! with the momentum filter's two weights swapped still looks like a
//! waveform), so they are arithmetic a unit test can pin against MilkDrop's
//! own figures.
//!
//! Read from `milkdropfs.cpp`'s `DrawWave` and `SmoothWave`; see
//! `crates/player-render/src/milk/mod.rs` for the attribution.

use crate::milk::state::{Fixed, WaveState};
use crate::milk::warp::Aspect;

/// How many samples per channel the modes expect to have.
///
/// 576 rather than 512: the modes are allowed to look 64 samples past the end
/// of the 512 they draw (`fL[i+32]`, `fR[i+25+offset]`), which is MilkDrop's
/// own arrangement and is why its buffer is the longer of the two numbers.
pub const WAVE_SAMPLES: usize = 576;

/// How many of those any one mode will actually walk.
const DRAWN_SAMPLES: usize = 512;

/// A waveform ready to draw.
pub struct Wave {
    /// Clip-space points, in order along the line.
    pub points: Vec<[f32; 2]>,
    /// Where the line is in two pieces rather than one -- mode 7 draws two
    /// channels and joining them would put a stroke across the frame.
    pub break_at: Option<usize>,
    pub colour: [f32; 4],
    pub additive: bool,
    pub dots: bool,
    pub thick: bool,
}

/// What the waveform is drawn from.
pub struct WaveInput<'a> {
    /// Both channels, [`WAVE_SAMPLES`] long, already scaled and smoothed by
    /// [`prepare`].
    pub left: &'a [f32],
    pub right: &'a [f32],
    /// Seconds since the preset was selected -- modes 0, 1 and 5 rotate.
    pub time: f32,
    pub aspect: Aspect,
    /// The render target's width, which decides two of the modes' alpha and
    /// caps the vertex count of two others. MilkDrop tabulates these against
    /// power-of-two texture sizes because that is all it ever had; this build
    /// interpolates instead (see [`size_alpha`]).
    pub width: u32,
    /// The audio summary the frame already carries, used by the volume
    /// modulation and by mode 3's treble response.
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
}

/// Scales and smooths one channel the way MilkDrop does before any mode sees
/// it.
///
/// The smoothing is a one-pole filter **along the sample index**, not across
/// frames -- it is a low-pass on the drawn shape, which is what stops a
/// waveform looking like noise, and it is why `fWaveSmoothing` defaults as high
/// as 0.75. MilkDrop also divides by 128 here, because its samples arrive as
/// bytes; ours are already -1..1, so that factor is not ours to apply.
pub fn prepare(samples: &mut [f32], scale: f32, smoothing: f32) {
    let mix2 = smoothing.clamp(0.0, 0.99);
    let mix1 = scale * (1.0 - mix2);
    if let Some(first) = samples.first_mut() {
        *first *= scale;
    }
    for i in 1..samples.len() {
        samples[i] = samples[i] * mix1 + samples[i - 1] * mix2;
    }
}

/// The alpha multiplier modes 2, 3 and 5 take from the frame's size.
///
/// MilkDrop switches on the texture size against four power-of-two cases and
/// does nothing at all for a size that is not one of them -- which on a modern
/// window means those three modes draw at full alpha instead of the faint wash
/// they are supposed to be. So this reads the same four points as a line in
/// log width: 0.07 at 256 doubling to 0.13 at 2048 is +0.02 per octave, which
/// is what the table is.
fn size_alpha(width: u32) -> f32 {
    let octaves = (width.max(1) as f32 / 256.0).log2();
    (0.07 + 0.02 * octaves).clamp(0.07, 0.13)
}

/// Builds the frame's waveform, or `None` when there is nothing to draw.
pub fn build(state: &WaveState, fixed: &Fixed, input: &WaveInput) -> Option<Wave> {
    if input.left.len() < WAVE_SAMPLES || input.right.len() < WAVE_SAMPLES {
        return None;
    }

    let (mut r, mut g, mut b) = (
        state.r.clamp(0.0, 1.0),
        state.g.clamp(0.0, 1.0),
        state.b.clamp(0.0, 1.0),
    );
    if state.brighten {
        let max = r.max(g).max(b);
        if max > 0.01 {
            r /= max;
            g /= max;
            b /= max;
        }
    }

    let pos_x = state.x * 2.0 - 1.0;
    let pos_y = state.y * 2.0 - 1.0;

    // The mystery parameter is folded rather than clamped for the three modes
    // that use it as a distance, so a preset animating it past the ends
    // reverses instead of sticking.
    let mut mystery = state.mystery;
    if matches!(state.mode, 0 | 1 | 4) && !(-1.0..=1.0).contains(&mystery) {
        mystery = mystery * 0.5 + 0.5;
        mystery -= mystery.floor();
        mystery = mystery.abs() * 2.0 - 1.0;
    }

    let mut alpha = state.alpha;
    let mut apply_volume = true;
    let (fl, fr) = (input.left, input.right);
    let aspect = input.aspect;
    let mut points: Vec<[f32; 2]> = Vec::new();
    let mut break_at = None;

    match state.mode {
        0 => {
            // A circle whose radius is the right channel.
            //
            // The 6.28 below is MilkDrop's own and is deliberately *not* tau:
            // it is 0.0016 short, so the ring falls a fifth of a degree short
            // of closing. That gap is real and visible in MilkDrop, and the
            // cross-fade over the first tenth of the ring is what hides it --
            // "correcting" it to tau would change the shape of every mode-0
            // preset ever written.
            let n = DRAWN_SAMPLES / 2;
            let offset = (DRAWN_SAMPLES - n) / 2;
            let inv = 1.0 / (n - 1) as f32;
            points.reserve(n + 1);
            #[allow(clippy::approx_constant)] // see the note above: not tau
            for i in 0..n {
                let mut rad = 0.5 + 0.4 * fr[i + offset] + mystery;
                let ang = i as f32 * inv * 6.28 + input.time * 0.2;
                // The first tenth is cross-faded with the samples one lap
                // ahead, so the ring closes on itself smoothly instead of
                // showing the seam where the buffer wrapped.
                if i < n / 10 {
                    let mix = i as f32 / (n as f32 * 0.1);
                    let mix = 0.5 - 0.5 * (mix * std::f32::consts::PI).cos();
                    let ahead = 0.5 + 0.4 * fr[i + n + offset] + mystery;
                    rad = ahead * (1.0 - mix) + rad * mix;
                }
                points.push([
                    rad * ang.cos() * aspect.y + pos_x,
                    rad * ang.sin() * aspect.x + pos_y,
                ]);
            }
            if let Some(first) = points.first().copied() {
                points.push(first);
            }
        }
        1 => {
            // The same ring, but the angle comes from the left channel, so it
            // winds instead of sweeping.
            alpha *= 1.25;
            let n = DRAWN_SAMPLES / 2;
            points.reserve(n);
            for i in 0..n {
                let rad = 0.53 + 0.43 * fr[i] + mystery;
                let ang = fl[i + 32] * 1.57 + input.time * 2.3;
                points.push([
                    rad * ang.cos() * aspect.y + pos_x,
                    rad * ang.sin() * aspect.x + pos_y,
                ]);
            }
        }
        2 | 3 => {
            // Left against right: a Lissajous figure, which is the same
            // picture this app's own vectorscope draws.
            if state.mode == 2 {
                alpha *= size_alpha(input.width);
            } else {
                // Mode 3 *replaces* the preset's alpha rather than scaling it,
                // then ties it to the treble -- it is the one mode whose
                // visibility the preset does not control.
                alpha = size_alpha(input.width) * 2.0 * 1.3 * input.treb.max(0.0).powi(2);
            }
            points.reserve(DRAWN_SAMPLES);
            for i in 0..DRAWN_SAMPLES {
                points.push([fr[i] * aspect.y + pos_x, fl[i + 32] * aspect.x + pos_y]);
            }
        }
        4 => {
            // A horizontal trace with momentum: each point is pulled toward
            // the straight-line continuation of the previous two, which is
            // what gives this mode its handwriting look.
            let n = DRAWN_SAMPLES.min((input.width / 3).max(2) as usize);
            let offset = (DRAWN_SAMPLES - n) / 2;
            let w1 = 0.45 + 0.5 * (mystery * 0.5 + 0.5);
            let w2 = 1.0 - w1;
            let inv = 1.0 / n as f32;
            points.reserve(n);
            for i in 0..n {
                let mut x = -1.0 + 2.0 * (i as f32 * inv) + pos_x + fr[i + 25 + offset] * 0.44;
                let mut y = fl[i + offset] * 0.47 + pos_y;
                if i > 1 {
                    let (a, b) = (points[i - 1], points[i - 2]);
                    x = x * w2 + w1 * (a[0] * 2.0 - b[0]);
                    y = y * w2 + w1 * (a[1] * 2.0 - b[1]);
                }
                points.push([x, y]);
            }
        }
        5 => {
            // Products and differences of the two channels, turning slowly:
            // a figure with no straight reading, which is the point of it.
            alpha *= size_alpha(input.width);
            let (sin_rot, cos_rot) = (input.time * 0.3).sin_cos();
            points.reserve(DRAWN_SAMPLES);
            for i in 0..DRAWN_SAMPLES {
                let x0 = fr[i] * fl[i + 32] + fl[i] * fr[i + 32];
                let y0 = fr[i] * fr[i] - fl[i + 32] * fl[i + 32];
                points.push([
                    (x0 * cos_rot - y0 * sin_rot) * aspect.y + pos_x,
                    (x0 * sin_rot + y0 * cos_rot) * aspect.x + pos_y,
                ]);
            }
        }
        _ => {
            // Modes 6 and 7: a line at an angle the preset chooses, with the
            // waveform displacing it sideways. Mode 7 draws both channels,
            // separated.
            let n = 256.min((input.width / 3).max(2) as usize);
            let offset = (DRAWN_SAMPLES - n) / 2;
            let ang = 1.57 * mystery;
            let (dy0, dx0) = ang.sin_cos();

            // The line is defined far outside the frame and then clipped back
            // to it, so that its *sample spacing* depends on how much of it is
            // visible rather than on the angle -- which is what keeps the
            // waveform the same density at every angle.
            let perp = ang + 1.57;
            let mut ex = [
                pos_x * perp.cos() - dx0 * 3.0,
                pos_x * perp.cos() + dx0 * 3.0,
            ];
            let mut ey = [
                pos_x * perp.sin() - dy0 * 3.0,
                pos_x * perp.sin() + dy0 * 3.0,
            ];
            for i in 0..2 {
                let other = 1 - i;
                for edge in 0..4 {
                    let (over, limit, on_x) = match edge {
                        0 => (ex[i] > 1.1, 1.1, true),
                        1 => (ex[i] < -1.1, -1.1, true),
                        2 => (ey[i] > 1.1, 1.1, false),
                        _ => (ey[i] < -1.1, -1.1, false),
                    };
                    if !over {
                        continue;
                    }
                    let (a, b) = if on_x {
                        (ex[i], ex[other])
                    } else {
                        (ey[i], ey[other])
                    };
                    let span = a - b;
                    if span.abs() < 1e-9 {
                        continue;
                    }
                    let t = (limit - b) / span;
                    let (sx, sy) = (ex[i] - ex[other], ey[i] - ey[other]);
                    ex[i] = ex[other] + sx * t;
                    ey[i] = ey[other] + sy * t;
                }
            }

            let dx = (ex[1] - ex[0]) / n as f32;
            let dy = (ey[1] - ey[0]) / n as f32;
            let ang2 = dy.atan2(dx) + 1.57;
            let (perp_dy, perp_dx) = ang2.sin_cos();

            if state.mode == 6 {
                points.reserve(n);
                for i in 0..n {
                    let s = 0.25 * fl[i + offset];
                    points.push([
                        ex[0] + dx * i as f32 + perp_dx * s,
                        ey[0] + dy * i as f32 + perp_dy * s,
                    ]);
                }
            } else {
                let sep = (pos_y * 0.5 + 0.5).powi(2);
                points.reserve(n * 2);
                for i in 0..n {
                    let s = 0.25 * fl[i + offset] + sep;
                    points.push([
                        ex[0] + dx * i as f32 + perp_dx * s,
                        ey[0] + dy * i as f32 + perp_dy * s,
                    ]);
                }
                break_at = Some(n);
                for i in 0..n {
                    let s = 0.25 * fr[i + offset] - sep;
                    points.push([
                        ex[0] + dx * i as f32 + perp_dx * s,
                        ey[0] + dy * i as f32 + perp_dy * s,
                    ]);
                }
            }
            apply_volume = true;
        }
    }

    if apply_volume && fixed.mod_wave_alpha_by_volume {
        let span = fixed.mod_wave_alpha_end - fixed.mod_wave_alpha_start;
        if span.abs() > 1e-6 {
            let level = (input.bass + input.mid + input.treb) * 0.333;
            alpha *= (level - fixed.mod_wave_alpha_start) / span;
        }
    }
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        0.0
    };
    // MilkDrop's own threshold. Below it the line would be a rounding error
    // added to the feedback buffer every frame, which is a slow grey wash
    // rather than a waveform.
    if alpha < 0.004 || points.len() < 2 {
        return None;
    }

    // Clip space has y up and every mode above was written with y down, so
    // the flip happens once, here, exactly where MilkDrop does it.
    for p in &mut points {
        p[1] = -p[1];
    }

    let (points, break_at) = match break_at {
        None => (smooth(&points), None),
        Some(at) => {
            let head = smooth(&points[..at]);
            let tail = smooth(&points[at..]);
            let moved = head.len();
            let mut all = head;
            all.extend_from_slice(&tail);
            (all, Some(moved))
        }
    };

    Some(Wave {
        points,
        break_at,
        colour: [r, g, b, alpha],
        additive: state.additive,
        dots: state.dots,
        thick: state.thick,
    })
}

/// MilkDrop's one-pass tessellation: a new point between every pair, placed on
/// a four-tap curve through the neighbours.
///
/// The weights sum to 2.0 and two of them are negative, which is a mild
/// sharpening rather than an average -- the inserted point sits slightly
/// *outside* the straight line between its neighbours, so a smoothed waveform
/// keeps its peaks instead of rounding them off.
fn smooth(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    if points.len() < 2 {
        return points.to_vec();
    }
    const C: [f32; 4] = [-0.15, 1.15, 1.15, -0.15];
    let inv_sum = 1.0 / (C[0] + C[1] + C[2] + C[3]);
    let last = points.len() - 1;
    let mut out = Vec::with_capacity(points.len() * 2 - 1);
    let mut below = 0usize;
    let mut above2 = 1usize;
    for i in 0..last {
        let above = above2;
        above2 = last.min(i + 2);
        out.push(points[i]);
        out.push([
            (C[0] * points[below][0]
                + C[1] * points[i][0]
                + C[2] * points[above][0]
                + C[3] * points[above2][0])
                * inv_sum,
            (C[0] * points[below][1]
                + C[1] * points[i][1]
                + C[2] * points[above][1]
                + C[3] * points[above2][1])
                * inv_sum,
        ]);
        below = i;
    }
    out.push(points[last]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milk::state::WaveState;

    fn state(mode: i32) -> WaveState {
        WaveState {
            mode,
            alpha: 1.0,
            r: 1.0,
            g: 1.0,
            b: 1.0,
            x: 0.5,
            y: 0.5,
            mystery: 0.0,
            dots: false,
            thick: false,
            additive: false,
            brighten: false,
        }
    }

    /// A sine in each channel, a quarter turn apart, so a Lissajous mode draws
    /// a circle and a radial mode draws a ring of a known size.
    fn tone() -> (Vec<f32>, Vec<f32>) {
        let left: Vec<f32> = (0..WAVE_SAMPLES)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect();
        let right: Vec<f32> = (0..WAVE_SAMPLES)
            .map(|i| (i as f32 * 0.05).cos() * 0.5)
            .collect();
        (left, right)
    }

    fn input<'a>(left: &'a [f32], right: &'a [f32]) -> WaveInput<'a> {
        WaveInput {
            left,
            right,
            time: 0.0,
            aspect: Aspect::new(512, 512),
            width: 512,
            bass: 1.0,
            mid: 1.0,
            treb: 1.0,
        }
    }

    #[test]
    fn every_mode_draws_something_finite_inside_a_sane_range() {
        let (l, r) = tone();
        for mode in 0..8 {
            let wave = build(&state(mode), &Fixed::default(), &input(&l, &r))
                .unwrap_or_else(|| panic!("mode {mode} drew nothing"));
            assert!(wave.points.len() > 32, "mode {mode}: {}", wave.points.len());
            for p in &wave.points {
                assert!(
                    p[0].is_finite() && p[1].is_finite(),
                    "mode {mode} produced {p:?}"
                );
                // Modes are allowed to run off the frame -- several do, on
                // purpose -- but not by orders of magnitude, which is what a
                // sign or an aspect error looks like.
                assert!(p[0].abs() < 8.0 && p[1].abs() < 8.0, "mode {mode}: {p:?}");
            }
        }
    }

    #[test]
    fn the_circular_mode_closes_its_own_ring() {
        let (l, r) = tone();
        let wave = build(&state(0), &Fixed::default(), &input(&l, &r)).expect("mode 0 draws");
        let first = wave.points[0];
        let last = wave.points[wave.points.len() - 1];
        assert!(
            (first[0] - last[0]).abs() < 1e-5 && (first[1] - last[1]).abs() < 1e-5,
            "{first:?} vs {last:?}"
        );
    }

    #[test]
    fn only_the_two_channel_mode_breaks_its_line() {
        let (l, r) = tone();
        for mode in 0..8 {
            let wave = build(&state(mode), &Fixed::default(), &input(&l, &r)).expect("draws");
            if mode == 7 {
                let at = wave.break_at.expect("mode 7 draws two channels");
                assert!(at > 0 && at < wave.points.len(), "break at {at}");
            } else {
                assert_eq!(wave.break_at, None, "mode {mode} should be one line");
            }
        }
    }

    #[test]
    fn a_silent_input_still_draws_the_shapes_that_are_shapes() {
        // Mode 0 is a circle whether or not anything is playing; mode 2 is a
        // dot. Both are correct, and neither may be a NaN.
        let silence = vec![0.0f32; WAVE_SAMPLES];
        let ring =
            build(&state(0), &Fixed::default(), &input(&silence, &silence)).expect("mode 0 draws");
        let radius = ring
            .points
            .iter()
            .map(|p| p[0].hypot(p[1]))
            .fold(0.0f32, f32::max);
        assert!((radius - 0.5).abs() < 1e-4, "radius {radius}");
    }

    #[test]
    fn the_mystery_parameter_folds_rather_than_sticking() {
        // 1.5 folds to -0.5, so the ring is smaller than at mystery 0 rather
        // than half a frame wider.
        let silence = vec![0.0f32; WAVE_SAMPLES];
        let mut s = state(0);
        s.mystery = 1.5;
        let wave = build(&s, &Fixed::default(), &input(&silence, &silence)).expect("draws");
        let radius = wave
            .points
            .iter()
            .map(|p| p[0].hypot(p[1]))
            .fold(0.0f32, f32::max);
        assert!((radius - 0.0).abs() < 1e-4, "radius {radius}");
    }

    #[test]
    fn an_alpha_below_milkdrops_threshold_draws_nothing_at_all() {
        let (l, r) = tone();
        let mut s = state(0);
        s.alpha = 0.003;
        assert!(build(&s, &Fixed::default(), &input(&l, &r)).is_none());
        s.alpha = 0.005;
        assert!(build(&s, &Fixed::default(), &input(&l, &r)).is_some());
    }

    #[test]
    fn brightening_the_colour_raises_its_largest_channel_to_one() {
        let (l, r) = tone();
        let mut s = state(0);
        s.brighten = true;
        s.r = 0.2;
        s.g = 0.1;
        s.b = 0.05;
        let wave = build(&s, &Fixed::default(), &input(&l, &r)).expect("draws");
        assert!((wave.colour[0] - 1.0).abs() < 1e-6, "{:?}", wave.colour);
        assert!((wave.colour[1] - 0.5).abs() < 1e-6, "{:?}", wave.colour);
    }

    #[test]
    fn volume_modulation_can_close_the_waveform_and_open_it() {
        let (l, r) = tone();
        let fixed = Fixed {
            mod_wave_alpha_by_volume: true,
            ..Default::default()
        };
        let mut quiet = input(&l, &r);
        quiet.bass = 0.0;
        quiet.mid = 0.0;
        quiet.treb = 0.0;
        assert!(build(&state(0), &fixed, &quiet).is_none(), "quiet: no wave");

        let loud = input(&l, &r);
        let wave = build(&state(0), &fixed, &loud).expect("loud: a wave");
        assert!((wave.colour[3] - 1.0).abs() < 1e-6, "{:?}", wave.colour);
    }

    #[test]
    fn smoothing_doubles_the_points_and_keeps_the_ends() {
        let line = vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0]];
        let out = smooth(&line);
        assert_eq!(out.len(), 7);
        assert_eq!(out[0], line[0]);
        assert_eq!(out[6], line[3]);
        // On a straight line the inserted points land on it.
        assert!((out[3][0] - 1.5).abs() < 1e-5, "{:?}", out[3]);
    }

    #[test]
    fn smoothing_sharpens_a_peak_rather_than_rounding_it_off() {
        // The two negative weights are the whole point of MilkDrop's curve;
        // a plain average would place the inserted point *inside* the corner.
        let peak = vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]];
        let out = smooth(&peak);
        // Between the rise and the fall, the new point sits above the mean of
        // its neighbours (0.5) rather than at it.
        assert!(out[3][1] > 0.55, "{:?}", out[3]);
    }

    #[test]
    fn preparation_scales_and_low_passes_along_the_samples() {
        // A step input: with no smoothing it stays a step, with smoothing it
        // becomes a ramp -- which is what the filter is for.
        let mut sharp = vec![0.0f32, 1.0, 1.0, 1.0];
        prepare(&mut sharp, 1.0, 0.0);
        assert_eq!(sharp, vec![0.0, 1.0, 1.0, 1.0]);

        let mut soft = vec![0.0f32, 1.0, 1.0, 1.0];
        prepare(&mut soft, 1.0, 0.5);
        assert_eq!(soft[0], 0.0);
        assert!((soft[1] - 0.5).abs() < 1e-6, "{soft:?}");
        assert!(soft[2] > soft[1] && soft[2] < 1.0, "{soft:?}");

        let mut scaled = vec![2.0f32; 4];
        prepare(&mut scaled, 0.5, 0.0);
        assert!(scaled.iter().all(|v| (*v - 1.0).abs() < 1e-6), "{scaled:?}");
    }

    #[test]
    fn too_few_samples_draw_nothing_rather_than_reading_past_the_end() {
        // The modes index up to 64 past the samples they draw, so a short
        // buffer is a panic waiting for the first preset that picks mode 2.
        let short = vec![0.0f32; DRAWN_SAMPLES];
        assert!(build(&state(2), &Fixed::default(), &input(&short, &short)).is_none());
    }

    #[test]
    fn the_size_alpha_reads_milkdrops_table_at_its_own_points() {
        for (width, expected) in [(256u32, 0.07f32), (512, 0.09), (1024, 0.11), (2048, 0.13)] {
            let got = size_alpha(width);
            assert!(
                (got - expected).abs() < 1e-6,
                "{width}: {got} vs {expected}"
            );
        }
        // And in between, where MilkDrop's switch would have fallen through
        // and left these modes at full strength.
        assert!(size_alpha(1440) > 0.11 && size_alpha(1440) < 0.13);
        assert_eq!(size_alpha(64), 0.07);
        assert_eq!(size_alpha(8192), 0.13);
    }
}
