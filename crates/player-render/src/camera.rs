//! The 4x4 matrix arithmetic the 3D scope needs, and nothing else.
//!
//! Hand-written rather than pulled from a math crate: this is one perspective
//! matrix and one look-at, both of which have exact answers a test can state
//! (a point at the target projects to the middle of the screen; a point to the
//! right lands right of it). A dependency would be more code to verify against
//! its own source, not less -- and the wgpu clip-space convention is the part
//! that actually catches people out, so it is worth having in the open here.
//!
//! Conventions, chosen to match wgpu's:
//!
//! - Right-handed view space: the camera looks down its own -z, +y is up.
//! - Clip space z runs 0 (near plane) to 1 (far plane), *not* -1 to 1 as
//!   OpenGL has it. A matrix built for OpenGL renders here with everything
//!   nearer than halfway through the frustum clipped away, which looks like a
//!   depth-buffer bug and is not one.
//! - Column-major storage, because that is what WGSL's `mat4x4<f32>` reads.

/// A 4x4 matrix in column-major order: `m[c][r]`, the layout WGSL expects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4(pub [[f32; 4]; 4]);

impl Mat4 {
    /// Only the tests need these two: the pipeline hands its matrix straight
    /// to the GPU and never asks where a point landed. They are how the
    /// conventions above are *stated* -- projecting a known point and checking
    /// where it comes out is the only way a sign error is a failing test
    /// rather than a puzzling picture.
    #[cfg(test)]
    pub const IDENTITY: Mat4 = Mat4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    /// `self * other`, i.e. apply `other` first.
    pub fn mul(&self, other: &Mat4) -> Mat4 {
        let mut out = [[0.0f32; 4]; 4];
        for (c, column) in out.iter_mut().enumerate() {
            for (r, value) in column.iter_mut().enumerate() {
                *value = (0..4).map(|k| self.0[k][r] * other.0[c][k]).sum();
            }
        }
        Mat4(out)
    }

    /// Transforms a point, returning it divided through by w -- i.e. normalised
    /// device coordinates when `self` is a view-projection.
    #[cfg(test)]
    pub fn project(&self, point: [f32; 3]) -> [f32; 3] {
        let mut out = [0.0f32; 4];
        for (r, value) in out.iter_mut().enumerate() {
            *value = self.0[0][r] * point[0]
                + self.0[1][r] * point[1]
                + self.0[2][r] * point[2]
                + self.0[3][r];
        }
        let w = if out[3].abs() < f32::EPSILON {
            f32::EPSILON
        } else {
            out[3]
        };
        [out[0] / w, out[1] / w, out[2] / w]
    }

    pub fn as_array(&self) -> [[f32; 4]; 4] {
        self.0
    }
}

/// Right-handed perspective projection into wgpu's 0..1 depth range.
pub fn perspective(fov_y_radians: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    };
    let f = 1.0 / (fov_y_radians / 2.0).tan();
    Mat4([
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ])
}

/// Right-handed look-at: places the camera at `eye` looking towards `target`.
pub fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat4 {
    let forward = normalise(sub(target, eye));
    let right = normalise(cross(forward, up));
    // Recomputed from the other two rather than taken as given, so a caller
    // passing an `up` that isn't perpendicular to the view direction still
    // gets an orthonormal basis instead of a sheared one.
    let true_up = cross(right, forward);
    Mat4([
        [right[0], true_up[0], -forward[0], 0.0],
        [right[1], true_up[1], -forward[1], 0.0],
        [right[2], true_up[2], -forward[2], 0.0],
        [-dot(right, eye), -dot(true_up, eye), dot(forward, eye), 1.0],
    ])
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalise(v: [f32; 3]) -> [f32; 3] {
    let length = dot(v, v).sqrt();
    if length < f32::EPSILON {
        return [0.0, 0.0, 0.0];
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOV: f32 = std::f32::consts::FRAC_PI_4;

    fn view_projection(aspect: f32) -> Mat4 {
        let projection = perspective(FOV, aspect, 0.1, 100.0);
        let view = look_at([0.0, 0.0, 3.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        projection.mul(&view)
    }

    #[test]
    fn identity_leaves_a_point_alone() {
        assert_eq!(Mat4::IDENTITY.project([0.3, -0.4, 0.5]), [0.3, -0.4, 0.5]);
    }

    #[test]
    fn multiplication_applies_the_right_hand_side_first() {
        let translate = Mat4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [1.0, 2.0, 3.0, 1.0],
        ]);
        let scale = Mat4([
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        // Translate, then scale: the translation is scaled too.
        assert_eq!(scale.mul(&translate).project([0.0; 3]), [2.0, 4.0, 6.0]);
        // Scale, then translate: it isn't.
        assert_eq!(translate.mul(&scale).project([0.0; 3]), [1.0, 2.0, 3.0]);
    }

    #[test]
    fn what_the_camera_looks_at_lands_in_the_middle() {
        let ndc = view_projection(1.0).project([0.0, 0.0, 0.0]);
        assert!(ndc[0].abs() < 1e-5 && ndc[1].abs() < 1e-5, "{ndc:?}");
    }

    #[test]
    fn right_is_right_and_up_is_up() {
        // The sign convention, stated once so a flipped scope is a failing
        // test rather than a puzzling picture.
        let matrix = view_projection(1.0);
        assert!(matrix.project([0.5, 0.0, 0.0])[0] > 0.0, "+x must be right");
        assert!(matrix.project([0.0, 0.5, 0.0])[1] > 0.0, "+y must be up");
    }

    #[test]
    fn depth_runs_zero_to_one_the_way_wgpu_wants() {
        // The convention that catches people out: an OpenGL-style projection
        // puts the near plane at -1 here, and everything in the near half of
        // the frustum is then clipped.
        let projection = perspective(FOV, 1.0, 0.1, 100.0);
        let near = projection.project([0.0, 0.0, -0.1])[2];
        let far = projection.project([0.0, 0.0, -100.0])[2];
        assert!((near - 0.0).abs() < 1e-4, "near plane at {near}");
        assert!((far - 1.0).abs() < 1e-4, "far plane at {far}");
    }

    #[test]
    fn nearer_things_have_smaller_depth() {
        let matrix = view_projection(1.0);
        let near = matrix.project([0.0, 0.0, 1.0])[2];
        let far = matrix.project([0.0, 0.0, -1.0])[2];
        assert!(near < far, "near {near} should sort in front of far {far}");
    }

    #[test]
    fn a_wide_viewport_does_not_stretch_the_figure() {
        // Same world point, twice as wide a window: it moves proportionally
        // less across the screen, which is what keeps a circle a circle.
        let square = view_projection(1.0).project([0.5, 0.5, 0.0]);
        let wide = view_projection(2.0).project([0.5, 0.5, 0.0]);
        assert!((wide[1] - square[1]).abs() < 1e-5, "height must not change");
        assert!(
            (wide[0] - square[0] / 2.0).abs() < 1e-5,
            "{} against {}",
            wide[0],
            square[0]
        );
    }

    #[test]
    fn a_degenerate_aspect_ratio_does_not_produce_nonsense() {
        // A window can genuinely report zero width mid-resize.
        let ndc = perspective(FOV, 0.0, 0.1, 100.0).project([0.0, 0.0, -1.0]);
        assert!(ndc.iter().all(|value| value.is_finite()), "{ndc:?}");
    }
}
