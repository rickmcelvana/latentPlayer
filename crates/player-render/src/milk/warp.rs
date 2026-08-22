//! MilkDrop's per-vertex warp: where each mesh point reads from the last frame.
//!
//! This is the arithmetic the spike deliberately did not guess. Its reduced
//! version -- zoom, stretch, rotate, translate -- looks entirely plausible in a
//! still frame and is wrong in motion, which is the worst way for a transform
//! to be wrong: nothing fails, thousands of presets simply do not look like
//! themselves. So every line below is read out of MilkDrop 2's own source
//! (`milkdropfs.cpp`'s per-vertex loop and `plugin.cpp`'s grid setup), and the
//! *order* is as load-bearing as the terms -- the warp displacement happens
//! after the stretch and before the rotation, and moving it either way is a
//! different picture.
//!
//! Two things the reduced version had no idea about:
//!
//! - **`zoomexp` makes the zoom depend on the radius.** `zoom2 = zoom ^
//!   (zoomexp ^ (rad*2 - 1))`, so a preset can zoom in at the centre and out at
//!   the edges with one number. At the default `zoomexp = 1` the exponent is 1
//!   everywhere and this collapses to the plain zoom, which is why its absence
//!   was invisible.
//! - **The whole transform happens in aspect-corrected space and is then
//!   un-corrected.** A circle stays a circle on a wide window only because the
//!   coordinates are squared up before the rotation and squashed back after it.
//!
//! Aspect here is MilkDrop's own: derived from the *render target*, not the
//! window, and expressed as a pair that is 1.0 on the longer axis.
//!
//! # The one line of MilkDrop's transform this does *not* copy
//!
//! MilkDrop ends the per-vertex transform with `u += 0.5/texsize_x` and the
//! same on v, and its own comment explains why: without it "a 1-pixel-wide line
//! of the image would wrap at the top and left edges". That is the Direct3D 9
//! half-texel rule -- in D3D9 a pixel centre and a texel centre are half a
//! texel apart, and every D3D9 program that samples a render target one-to-one
//! carries a correction for it. **wgpu, like every API after D3D9, aligns them
//! already**, so the same line here is not a correction, it is a displacement.
//!
//! In one frame it is invisible: half a pixel. In a *feedback loop* it is
//! applied again every frame, and the picture crawls half a pixel up and left
//! per frame forever -- 15 pixels of a 128-pixel frame in half a second, with
//! the trail smeared behind it. Nothing about the picture looks wrong while it
//! happens; it just slides. It was found by a readback asserting where a seeded
//! square *was* after thirty frames, which is the fourth time in this phase
//! that a figure caught what an eye would not have.

use crate::milk::state::Motion;

/// The aspect pair, and its inverse, exactly as MilkDrop derives them.
#[derive(Clone, Copy, Debug)]
pub struct Aspect {
    pub x: f32,
    pub y: f32,
    pub inv_x: f32,
    pub inv_y: f32,
}

impl Aspect {
    pub fn new(width: u32, height: u32) -> Self {
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        // Whichever axis is longer gets 1.0 and the other gets the ratio, so
        // both are in (0, 1]. Not `w/h` -- that would put a number above 1 on
        // one axis and the arithmetic downstream assumes it never is.
        let x = if h > w { w / h } else { 1.0 };
        let y = if w > h { h / w } else { 1.0 };
        Self {
            x,
            y,
            inv_x: 1.0 / x,
            inv_y: 1.0 / y,
        }
    }
}

impl Default for Aspect {
    fn default() -> Self {
        Aspect::new(1, 1)
    }
}

/// One mesh vertex's fixed geometry: where it is, and how far and which way
/// round it sits from the middle.
///
/// `rad` and `ang` are inputs to `per_pixel` and `rad` also drives `zoomexp`,
/// so they are computed once when the mesh is built rather than per frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct GridPoint {
    /// Clip-space position, -1..1, y **up**.
    pub x: f32,
    pub y: f32,
    /// Distance from the centre in aspect-corrected space.
    pub rad: f32,
    /// Angle from the centre in the same space.
    pub ang: f32,
}

/// The mesh's fixed geometry for a given grid size and aspect.
pub struct Grid {
    pub mesh_x: u32,
    pub mesh_y: u32,
    pub aspect: Aspect,
    points: Vec<GridPoint>,
}

impl Grid {
    pub fn new(mesh_x: u32, mesh_y: u32, aspect: Aspect) -> Self {
        let (mesh_x, mesh_y) = (mesh_x.max(1), mesh_y.max(1));
        let mut points = Vec::with_capacity(((mesh_x + 1) * (mesh_y + 1)) as usize);
        for row in 0..=mesh_y {
            for col in 0..=mesh_x {
                let x = col as f32 / mesh_x as f32 * 2.0 - 1.0;
                let y = row as f32 / mesh_y as f32 * 2.0 - 1.0;
                let (ax, ay) = (x * aspect.x, y * aspect.y);
                // MilkDrop special-cases the exact centre vertex to angle zero
                // rather than letting `atan2(0, 0)` answer. Kept because a
                // preset that reads `ang` at the centre reads 0 there, and the
                // platforms do not all agree on what `atan2(0,0)` is.
                let centre = row == mesh_y / 2 && col == mesh_x / 2;
                points.push(GridPoint {
                    x,
                    y,
                    rad: (ax * ax + ay * ay).sqrt(),
                    ang: if centre { 0.0 } else { ay.atan2(ax) },
                });
            }
        }
        Self {
            mesh_x,
            mesh_y,
            aspect,
            points,
        }
    }

    pub fn points(&self) -> &[GridPoint] {
        &self.points
    }

    /// Where `per_pixel` is told this vertex is: 0..1 with y running **down**,
    /// which is not the clip-space position and is not texture space either
    /// (it has no half-texel offset). MilkDrop's own conversion.
    pub fn pixel_inputs(&self, point: &GridPoint) -> (f32, f32) {
        (
            point.x * 0.5 * self.aspect.x + 0.5,
            point.y * -0.5 * self.aspect.y + 0.5,
        )
    }
}

/// The warp's whole-frame constants: everything the transform needs that does
/// not vary from vertex to vertex.
///
/// No half-texel offset among them -- see the module note for why MilkDrop has
/// one and this does not.
///
/// The four `f` values are an animated frequency set, so the warp's ripples
/// drift rather than standing still. Their constants are MilkDrop's, and there
/// is nothing to derive them from -- they are chosen numbers that make the
/// motion look unrepetitive, so they are copied exactly.
#[derive(Clone, Copy, Debug)]
pub struct Warp {
    pub aspect: Aspect,
    warp_time: f32,
    warp_scale_inv: f32,
    f: [f32; 4],
}

impl Warp {
    /// `width`/`height` are the render target's, `time` is seconds since the
    /// preset was selected, and the two speeds come from the preset's own
    /// fixed scalars.
    pub fn new(width: u32, height: u32, time: f32, warp_anim_speed: f32, warp_scale: f32) -> Self {
        let warp_time = time * warp_anim_speed;
        // A preset with `fWarpScale=0` would divide by zero here and send every
        // vertex to NaN; the sine terms then vanish, which is what a zero-scale
        // warp should look like anyway.
        let warp_scale_inv = if warp_scale.abs() < 1e-6 {
            0.0
        } else {
            1.0 / warp_scale
        };
        Self {
            aspect: Aspect::new(width, height),
            warp_time,
            warp_scale_inv,
            f: [
                11.68 + 4.0 * (warp_time * 1.413 + 10.0).cos(),
                8.77 + 3.0 * (warp_time * 1.113 + 7.0).cos(),
                10.54 + 3.0 * (warp_time * 1.233 + 3.0).cos(),
                11.49 + 4.0 * (warp_time * 0.933 + 5.0).cos(),
            ],
        }
    }

    /// Where this vertex reads the previous frame, in texture space 0..1.
    ///
    /// The steps are MilkDrop's, in MilkDrop's order. Each guard against a
    /// degenerate value is this build's own and is marked: a `.milk` file is a
    /// stranger's program, and `sx = 0` appears in real presets.
    pub fn source_for(&self, motion: &Motion, point: &GridPoint) -> [f32; 2] {
        // Radius-dependent zoom. `powf` of a negative base is NaN, and a
        // preset writing a negative zoom is writing something MilkDrop's own
        // `powf` would have answered indefinite to -- so it falls back to
        // "no zoom" rather than to a vertex that disappears.
        let exponent = powf_or(motion.zoomexp, point.rad * 2.0 - 1.0, 1.0);
        let zoom2 = powf_or(motion.zoom, exponent, 1.0);
        let inv_zoom = if zoom2.abs() < 1e-6 { 1.0 } else { 1.0 / zoom2 };

        let mut u = point.x * self.aspect.x * 0.5 * inv_zoom + 0.5;
        let mut v = -point.y * self.aspect.y * 0.5 * inv_zoom + 0.5;

        // Stretch about the preset's own centre.
        let sx = non_zero(motion.sx, 1.0);
        let sy = non_zero(motion.sy, 1.0);
        u = (u - motion.cx) / sx + motion.cx;
        v = (v - motion.cy) / sy + motion.cy;

        // The warp proper: four displacements, two on each axis, at four
        // different rates. The mixed sin/cos and the mixed signs are not
        // decoration -- they are what stops the four terms from lining up into
        // one big wave.
        let (w, t, k) = (motion.warp * 0.0035, self.warp_time, self.warp_scale_inv);
        let (x, y) = (point.x, point.y);
        let f = self.f;
        u += w * (t * 0.333 + k * (x * f[0] - y * f[3])).sin();
        v += w * (t * 0.375 - k * (x * f[2] + y * f[1])).cos();
        u += w * (t * 0.753 - k * (x * f[1] - y * f[2])).cos();
        v += w * (t * 0.825 + k * (x * f[0] + y * f[3])).sin();

        // Rotate about the same centre the stretch used.
        let (sin_r, cos_r) = motion.rot.sin_cos();
        let (u2, v2) = (u - motion.cx, v - motion.cy);
        u = u2 * cos_r - v2 * sin_r + motion.cx;
        v = u2 * sin_r + v2 * cos_r + motion.cy;

        // Translate, then undo the aspect correction the very first step
        // applied. MilkDrop's half-texel offset would go here and deliberately
        // does not -- module note.
        u -= motion.dx;
        v -= motion.dy;
        u = (u - 0.5) * self.aspect.inv_x + 0.5;
        v = (v - 0.5) * self.aspect.inv_y + 0.5;

        if u.is_finite() && v.is_finite() {
            [u, v]
        } else {
            // One NaN vertex is two triangles that vanish, which reads as a
            // hole torn in the picture. Reading where this vertex sits is the
            // least surprising thing to do instead.
            [point.x * 0.5 + 0.5, -point.y * 0.5 + 0.5]
        }
    }
}

fn non_zero(v: f32, fallback: f32) -> f32 {
    if v.abs() < 1e-6 || !v.is_finite() {
        fallback
    } else {
        v
    }
}

fn powf_or(base: f32, exponent: f32, fallback: f32) -> f32 {
    let v = base.powf(exponent);
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square target, so the aspect terms are all 1.0 and a test can talk
    /// about the transform rather than about the window.
    fn square(time: f32) -> Warp {
        Warp::new(128, 128, time, 1.0, 1.0)
    }

    fn still() -> Motion {
        Motion {
            warp: 0.0,
            ..Default::default()
        }
    }

    fn point(x: f32, y: f32) -> GridPoint {
        let (ax, ay) = (x, y);
        GridPoint {
            x,
            y,
            rad: (ax * ax + ay * ay).sqrt(),
            ang: ay.atan2(ax),
        }
    }

    #[test]
    fn a_still_preset_reads_where_it_writes() {
        let warp = square(0.0);
        let motion = still();
        for (x, y) in [(0.0, 0.0), (1.0, 1.0), (-1.0, 0.5), (0.25, -0.75)] {
            let p = point(x, y);
            let [u, v] = warp.source_for(&motion, &p);
            // Exactly where it sits, with nothing added: the picture must be
            // able to stand perfectly still through any number of frames.
            let expect_u = x * 0.5 + 0.5;
            let expect_v = -y * 0.5 + 0.5;
            assert!((u - expect_u).abs() < 1e-6, "u {u} vs {expect_u}");
            assert!((v - expect_v).abs() < 1e-6, "v {v} vs {expect_v}");
        }
    }

    #[test]
    fn zoomexp_makes_the_zoom_depend_on_the_radius() {
        // The term the spike's reduced transform had no idea about. At
        // zoomexp=2 the exponent runs from 0.5 at the centre to 2.0 at rad=1,
        // so the same `zoom` bites much harder at the edge than in the middle.
        let warp = square(0.0);
        let motion = Motion {
            zoom: 2.0,
            zoomexp: 2.0,
            ..still()
        };
        let near = warp.source_for(&motion, &point(0.5, 0.0))[0];
        let far = warp.source_for(&motion, &point(1.0, 0.0))[0];

        // rad = 0.5 -> exponent 2^0 = 1 -> zoom2 = 2, so u = 0.5*0.5/2 + 0.5.
        assert!((near - (0.5 * 0.5 / 2.0 + 0.5)).abs() < 1e-6, "{near}");
        // rad = 1.0 -> exponent 2^1 = 2 -> zoom2 = 4, so u = 1*0.5/4 + 0.5.
        assert!((far - (0.5 / 4.0 + 0.5)).abs() < 1e-6, "{far}");
    }

    #[test]
    fn zoomexp_of_one_is_exactly_a_plain_zoom() {
        // Which is why its absence was invisible in the spike: at the default
        // every vertex takes the same exponent.
        let warp = square(0.0);
        let plain = Motion {
            zoom: 1.7,
            ..still()
        };
        for (x, y) in [(1.0, 0.0), (0.3, -0.9), (0.0, 0.0)] {
            let p = point(x, y);
            let [u, v] = warp.source_for(&plain, &p);
            let expect_u = x * 0.5 / 1.7 + 0.5;
            let expect_v = -y * 0.5 / 1.7 + 0.5;
            assert!((u - expect_u).abs() < 1e-6, "{u} vs {expect_u}");
            assert!((v - expect_v).abs() < 1e-6, "{v} vs {expect_v}");
        }
    }

    #[test]
    fn the_warp_terms_move_the_picture_and_settle_when_warp_is_zero() {
        let warp = square(0.0);
        let quiet = warp.source_for(&still(), &point(0.6, -0.2));
        let rippled = warp.source_for(
            &Motion {
                warp: 1.0,
                ..Default::default()
            },
            &point(0.6, -0.2),
        );
        let moved = (rippled[0] - quiet[0]).hypot(rippled[1] - quiet[1]);
        // Four terms of 0.0035 each, so the displacement is bounded by 0.007
        // per axis and is a fraction of a percent of the frame -- small, and
        // exactly why it has to be right: it is a texture-space nudge that
        // compounds over a feedback loop's hundreds of frames.
        assert!(moved > 1e-4, "warp should displace, moved {moved}");
        assert!(moved < 0.01, "warp should stay small, moved {moved}");
    }

    #[test]
    fn the_warp_animates_over_time() {
        let motion = Motion {
            warp: 1.0,
            ..Default::default()
        };
        let p = point(0.6, -0.2);
        let a = square(0.0).source_for(&motion, &p);
        let b = square(1.0).source_for(&motion, &p);
        assert!((a[0] - b[0]).abs() + (a[1] - b[1]).abs() > 1e-5);
    }

    #[test]
    fn the_transform_is_ordered_warp_then_rotate() {
        // Warping after rotating instead of before it gives a different
        // answer, and this is the assertion that says which one this is. A
        // quarter turn maps a warp displacement on u into one on v, so
        // rotating first and warping second would leave the u displacement on
        // u -- the two orders disagree, and only one of them is MilkDrop's.
        let warp = square(0.0);
        let motion = Motion {
            warp: 1.0,
            rot: std::f32::consts::FRAC_PI_2,
            ..Default::default()
        };
        let p = point(0.6, -0.2);
        let with = warp.source_for(&motion, &p);
        let without = warp.source_for(
            &Motion {
                warp: 0.0,
                ..motion
            },
            &p,
        );
        let (du, dv) = (with[0] - without[0], with[1] - without[1]);

        // Recompute what the four terms displace *before* the rotation, then
        // rotate that displacement by the same quarter turn: that is what
        // warp-then-rotate predicts, to the last bit.
        let flat = square(0.0);
        let unrotated_with = flat.source_for(&Motion { rot: 0.0, ..motion }, &p);
        let unrotated_without = flat.source_for(
            &Motion {
                rot: 0.0,
                warp: 0.0,
                ..motion
            },
            &p,
        );
        let (fu, fv) = (
            unrotated_with[0] - unrotated_without[0],
            unrotated_with[1] - unrotated_without[1],
        );
        // Quarter turn: (u, v) -> (-v, u).
        assert!((du - -fv).abs() < 1e-6, "du {du} vs {}", -fv);
        assert!((dv - fu).abs() < 1e-6, "dv {dv} vs {fu}");
    }

    #[test]
    fn a_quarter_turn_takes_the_right_edge_to_the_bottom() {
        let warp = square(0.0);
        let motion = Motion {
            rot: std::f32::consts::FRAC_PI_2,
            ..still()
        };
        let [u, v] = warp.source_for(&motion, &point(1.0, 0.0));
        assert!((u - 0.5).abs() < 1e-6, "u {u}");
        assert!((v - 1.0).abs() < 1e-6, "v {v}");
    }

    #[test]
    fn translation_moves_the_picture_the_other_way() {
        let warp = square(0.0);
        let motion = Motion { dx: 0.1, ..still() };
        let [u, _] = warp.source_for(&motion, &point(0.0, 0.0));
        assert!((u - 0.4).abs() < 1e-6, "u {u}");
    }

    #[test]
    fn a_rotation_preserves_pixel_distance_on_a_wide_target() {
        // This is what the aspect pair buys, stated the way it can be checked:
        // a quarter turn moves a point the same number of *pixels* whatever
        // the window shape. Without the correction, texture-space distance
        // would be preserved instead, and every rotating preset would trace
        // ellipses on any window that is not square.
        let (w, h) = (256u32, 128u32);
        let warp = Warp::new(w, h, 0.0, 1.0, 1.0);
        assert!((warp.aspect.x - 1.0).abs() < 1e-6);
        assert!((warp.aspect.y - 0.5).abs() < 1e-6);

        let p = point(1.0 / 3.0, 0.0);
        let before = warp.source_for(&still(), &p);
        let after = warp.source_for(
            &Motion {
                rot: std::f32::consts::FRAC_PI_2,
                ..still()
            },
            &p,
        );

        // Distance from the frame's centre, in pixels, on each side of the
        // turn. The point starts out purely horizontal and ends purely
        // vertical, so this compares one axis against the other.
        let horizontal = (before[0] - 0.5).abs() * w as f32;
        let vertical = (after[1] - 0.5).abs() * h as f32;
        assert!((before[1] - 0.5).abs() < 1e-6);
        assert!((after[0] - 0.5).abs() < 1e-6);
        assert!(
            (horizontal - vertical).abs() < 1e-3,
            "{horizontal}px across vs {vertical}px down"
        );
        // And it really is a wide frame doing it: 1/3 of half the width.
        assert!((horizontal - (w as f32 / 6.0)).abs() < 1e-3, "{horizontal}");
    }

    #[test]
    fn a_degenerate_preset_does_not_tear_a_hole_in_the_frame() {
        let warp = square(0.0);
        for motion in [
            Motion {
                zoom: 0.0,
                ..still()
            },
            Motion { sx: 0.0, ..still() },
            Motion {
                zoom: -2.0,
                zoomexp: 0.5,
                ..still()
            },
            Motion {
                zoomexp: 0.0,
                ..still()
            },
        ] {
            let [u, v] = warp.source_for(&motion, &point(0.5, 0.5));
            assert!(u.is_finite() && v.is_finite(), "{u},{v} for {motion:?}");
        }
        // And a zero warp scale, which would otherwise divide by zero.
        let warp = Warp::new(128, 128, 1.0, 1.0, 0.0);
        let [u, v] = warp.source_for(
            &Motion {
                warp: 1.0,
                ..Default::default()
            },
            &point(0.5, 0.5),
        );
        assert!(u.is_finite() && v.is_finite(), "{u},{v}");
    }

    #[test]
    fn a_still_frame_stays_still_however_many_times_it_goes_round() {
        // The regression the half-texel offset was: each pass is a fixed
        // point, so applying the transform a hundred times is applying it
        // once. Anything constant added per frame fails this and nothing
        // else -- a single frame looks perfect.
        let warp = square(0.0);
        let motion = still();
        let mut p = point(0.4, -0.6);
        let first = warp.source_for(&motion, &p);
        for _ in 0..100 {
            let [u, v] = warp.source_for(&motion, &p);
            // Feed the result back in as the next frame's grid position, the
            // way the loop does: texture space 0..1 back to clip space -1..1.
            p = point(u * 2.0 - 1.0, 1.0 - v * 2.0);
        }
        let last = warp.source_for(&motion, &p);
        assert!(
            (first[0] - last[0]).abs() < 1e-5 && (first[1] - last[1]).abs() < 1e-5,
            "drifted from {first:?} to {last:?}"
        );
    }

    #[test]
    fn the_grid_puts_the_corners_where_clip_space_does() {
        let grid = Grid::new(4, 4, Aspect::new(128, 128));
        let points = grid.points();
        assert_eq!(points.len(), 25);
        assert_eq!((points[0].x, points[0].y), (-1.0, -1.0));
        assert_eq!((points[24].x, points[24].y), (1.0, 1.0));
        // The centre vertex is the one MilkDrop special-cases.
        let centre = &points[12];
        assert_eq!((centre.x, centre.y), (0.0, 0.0));
        assert_eq!(centre.ang, 0.0);
        assert_eq!(centre.rad, 0.0);
    }

    #[test]
    fn the_grids_radius_is_measured_in_aspect_corrected_space() {
        // On a wide target the vertical extent is short, so a corner is
        // nearer than sqrt(2) -- which is what makes `zoomexp` behave the same
        // way on every window shape.
        let wide = Grid::new(2, 2, Aspect::new(256, 128));
        let corner = wide.points().last().expect("a grid has points");
        assert!(
            (corner.rad - (1.0f32 + 0.25).sqrt()).abs() < 1e-6,
            "{corner:?}"
        );
    }

    #[test]
    fn per_pixel_is_told_the_vertex_position_with_y_running_down() {
        let grid = Grid::new(2, 2, Aspect::new(128, 128));
        let top_left = &grid.points()[0]; // clip (-1, -1), i.e. bottom left
        let (x, y) = grid.pixel_inputs(top_left);
        assert!((x - 0.0).abs() < 1e-6, "x {x}");
        assert!((y - 1.0).abs() < 1e-6, "y {y}");
    }
}
