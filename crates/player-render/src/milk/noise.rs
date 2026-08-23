//! MilkDrop's procedural noise textures.
//!
//! `tex3D` is **20.7% of the corpus** and every one of those reads a volume
//! from this file, so these are not decoration: without them a fifth of a real
//! collection samples a blank. They are in scope by decision 56 precisely
//! because generating them is arithmetic -- unlike a texture pack, nothing has
//! to ship.
//!
//! Read from MilkDrop 2's `plugin.cpp` (`AddNoiseTex`, `dwCubicInterpolate`,
//! `fCubicInterpolate`) rather than from memory, for the same reason the warp
//! transform was: noise that is *plausible* and noise that is MilkDrop's are
//! indistinguishable in a still and quite different in motion.
//!
//! # What each one is
//!
//! ```text
//! noise_lq        256x256   zoom 1   raw noise, every texel independent
//! noise_lq_lite    32x32    zoom 1
//! noise_mq        256x256   zoom 4   smooth: one value every 4 texels, cubic between
//! noise_hq        256x256   zoom 8   smoother still
//! noisevol_lq      32^3     zoom 1
//! noisevol_hq      32^3     zoom 4
//! ```
//!
//! The zoom factor is what makes `noise_hq` usable as a slow-moving field
//! rather than as static: values are drawn on a lattice `zoom` texels apart
//! and cubically interpolated between, which is a value noise rather than a
//! white one.
//!
//! # Two deliberate divergences, both stated rather than hidden
//!
//! **The seed is fixed.** MilkDrop seeds from the performance counter, so its
//! noise differs every run. A visual in this crate has to be checkable against
//! pixels (decision 6), and a texture that differs per run cannot be -- the
//! same reason `eel::rand` is seeded and the composite's corner tints are.
//!
//! **The channel arithmetic truncates rather than overflowing.** MilkDrop
//! writes `(warand() % RANGE) + RANGE/2` into each byte of a packed colour
//! with `RANGE` up to 256, so its own values can exceed 255 and carry into the
//! next channel. That is a bug in a line that was only ever meant to produce
//! noise; each channel here is truncated to its own byte instead. The
//! distribution is the same and the channels stay independent.
//!
//! The volume textures extend `AddNoiseTex`'s method to a third axis --
//! `AddNoiseVol`'s own body was not read, so this is the 2D algorithm applied
//! along each axis in turn rather than a transcription.

/// One noise texture: its size and how it is filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoiseSpec {
    pub name: &'static str,
    /// Edge length. Square for the 2D ones, cubic for the volumes.
    pub size: u32,
    /// How far apart the drawn lattice values are; 1 means every texel.
    pub zoom: u32,
    pub volume: bool,
}

/// Every noise texture MilkDrop generates, in `plugin.cpp`'s own order.
pub const NOISE: &[NoiseSpec] = &[
    NoiseSpec {
        name: "noise_lq",
        size: 256,
        zoom: 1,
        volume: false,
    },
    NoiseSpec {
        name: "noise_lq_lite",
        size: 32,
        zoom: 1,
        volume: false,
    },
    NoiseSpec {
        name: "noise_mq",
        size: 256,
        zoom: 4,
        volume: false,
    },
    NoiseSpec {
        name: "noise_hq",
        size: 256,
        zoom: 8,
        volume: false,
    },
    NoiseSpec {
        name: "noisevol_lq",
        size: 32,
        zoom: 1,
        volume: true,
    },
    NoiseSpec {
        name: "noisevol_hq",
        size: 32,
        zoom: 4,
        volume: true,
    },
];

/// A small deterministic generator, standing in for MilkDrop's `warand()`.
///
/// xorshift32: three shifts, no state beyond a word, and the same sequence on
/// every machine and both shells -- which is the whole point.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// MilkDrop's cubic interpolant, from `fCubicInterpolate`.
///
/// Not a Catmull-Rom: `a0 = y3 - y2 - y0 + y1` is its own combination, and
/// substituting the textbook spline changes every smoothed noise texture
/// slightly everywhere.
fn cubic(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let a0 = y3 - y2 - y0 + y1;
    let a1 = y0 - y1 - a0;
    let a2 = y2 - y0;
    let a3 = y1;
    a0 * t * t2 + a1 * t2 + a2 * t + a3
}

fn cubic_bytes(y0: u8, y1: u8, y2: u8, y3: u8, t: f32) -> u8 {
    let f = cubic(
        y0 as f32 / 255.0,
        y1 as f32 / 255.0,
        y2 as f32 / 255.0,
        y3 as f32 / 255.0,
        t,
    );
    (f.clamp(0.0, 1.0) * 255.0) as u8
}

/// Generate one texture's RGBA8 bytes, row-major (and slice-major for a
/// volume).
pub fn generate(spec: &NoiseSpec, seed: u32) -> Vec<u8> {
    let size = spec.size as usize;
    let depth = if spec.volume { size } else { 1 };
    let mut out = vec![0u8; size * size * depth * 4];
    let mut rng = Rng(seed | 1);

    // MilkDrop's own range: a narrower spread once the values are going to be
    // interpolated, so a smoothed texture does not spend its range on
    // extremes it will never reach.
    let range: u32 = if spec.zoom > 1 { 216 } else { 256 };
    for byte in out.iter_mut() {
        *byte = ((rng.next() % range) + range / 2) as u8;
    }

    // "swap some pixels randomly, to improve 'randomness'" -- MilkDrop's own
    // comment. Kept because it is cheap and because leaving it out is a
    // change to a texture whose whole job is its distribution.
    let texels = size * size * depth;
    for _ in 0..texels {
        let a = (rng.next() as usize) % texels;
        let b = (rng.next() as usize) % texels;
        for c in 0..4 {
            out.swap(a * 4 + c, b * 4 + c);
        }
    }

    if spec.zoom > 1 {
        smooth(&mut out, size, depth, spec.zoom as usize);
    }
    out
}

/// Cubic smoothing along each axis in turn.
///
/// Each pass writes only the texels *between* lattice points and reads only
/// the lattice points themselves, so a pass never reads what it has written --
/// which is what lets all three run in place.
fn smooth(data: &mut [u8], size: usize, depth: usize, zoom: usize) {
    let index = |x: usize, y: usize, z: usize| ((z * size + y) * size + x) * 4;

    // Across, on the lattice rows only.
    for z in (0..depth).step_by(if depth > 1 { zoom } else { 1 }) {
        for y in (0..size).step_by(zoom) {
            for x in 0..size {
                if x % zoom == 0 {
                    continue;
                }
                let base = (x / zoom) * zoom + size;
                let t = (x % zoom) as f32 / zoom as f32;
                for c in 0..4 {
                    let at = |o: isize| {
                        let xi = ((base as isize + o) as usize) % size;
                        data[index(xi, y, z) + c]
                    };
                    let v = cubic_bytes(
                        at(-(zoom as isize)),
                        at(0),
                        at(zoom as isize),
                        at(2 * zoom as isize),
                        t,
                    );
                    data[index(x, y, z) + c] = v;
                }
            }
        }
    }

    // Down, on every column.
    for z in (0..depth).step_by(if depth > 1 { zoom } else { 1 }) {
        for x in 0..size {
            for y in 0..size {
                if y % zoom == 0 {
                    continue;
                }
                let base = (y / zoom) * zoom + size;
                let t = (y % zoom) as f32 / zoom as f32;
                for c in 0..4 {
                    let at = |o: isize| {
                        let yi = ((base as isize + o) as usize) % size;
                        data[index(x, yi, z) + c]
                    };
                    let v = cubic_bytes(
                        at(-(zoom as isize)),
                        at(0),
                        at(zoom as isize),
                        at(2 * zoom as isize),
                        t,
                    );
                    data[index(x, y, z) + c] = v;
                }
            }
        }
    }

    // And through, for a volume.
    if depth > 1 {
        for y in 0..size {
            for x in 0..size {
                for z in 0..depth {
                    if z % zoom == 0 {
                        continue;
                    }
                    let base = (z / zoom) * zoom + depth;
                    let t = (z % zoom) as f32 / zoom as f32;
                    for c in 0..4 {
                        let at = |o: isize| {
                            let zi = ((base as isize + o) as usize) % depth;
                            data[index(x, y, zi) + c]
                        };
                        let v = cubic_bytes(
                            at(-(zoom as isize)),
                            at(0),
                            at(zoom as isize),
                            at(2 * zoom as isize),
                            t,
                        );
                        data[index(x, y, z) + c] = v;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> NoiseSpec {
        *NOISE.iter().find(|s| s.name == name).expect("known noise")
    }

    #[test]
    fn every_texture_is_the_size_milkdrop_gives_it() {
        assert_eq!(NOISE.len(), 6);
        assert_eq!(spec("noise_lq").size, 256);
        assert_eq!(spec("noise_lq_lite").size, 32);
        assert_eq!(spec("noise_mq").zoom, 4);
        assert_eq!(spec("noise_hq").zoom, 8);
        assert!(spec("noisevol_hq").volume);
        assert_eq!(spec("noisevol_hq").size, 32);
    }

    #[test]
    fn the_bytes_are_rgba_and_the_volume_is_cubic() {
        let flat = generate(&spec("noise_lq_lite"), 1);
        assert_eq!(flat.len(), 32 * 32 * 4);
        let volume = generate(&spec("noisevol_lq"), 1);
        assert_eq!(volume.len(), 32 * 32 * 32 * 4);
    }

    #[test]
    fn the_same_seed_gives_the_same_texture_every_time() {
        // The divergence this build accepts on purpose: MilkDrop seeds from
        // the performance counter, and a texture that differs per run cannot
        // be checked against pixels (decision 6).
        let a = generate(&spec("noise_mq"), 7);
        let b = generate(&spec("noise_mq"), 7);
        assert_eq!(a, b);
        let c = generate(&spec("noise_mq"), 8);
        assert_ne!(a, c, "and a different seed is a different texture");
    }

    #[test]
    fn a_smoothed_texture_is_smooth_and_a_raw_one_is_not() {
        // The point of the zoom factor: `noise_hq` is a value noise a preset
        // can move through, `noise_lq` is white noise. The difference is
        // measurable as the average step between neighbouring texels.
        let step = |name: &str| {
            let s = spec(name);
            let data = generate(&s, 3);
            let size = s.size as usize;
            let mut total = 0u64;
            for y in 0..size {
                for x in 1..size {
                    let a = data[(y * size + x) * 4] as i32;
                    let b = data[(y * size + x - 1) * 4] as i32;
                    total += a.abs_diff(b) as u64;
                }
            }
            total as f64 / (size * (size - 1)) as f64
        };
        let raw = step("noise_lq");
        let smooth8 = step("noise_hq");
        assert!(raw > 50.0, "white noise steps a long way: {raw}");
        assert!(
            smooth8 < raw / 4.0,
            "zoom 8 should be far smoother: {smooth8} against {raw}"
        );
    }

    #[test]
    fn the_lattice_points_survive_the_smoothing_untouched() {
        // Each pass writes only between lattice points and reads only the
        // points themselves, which is what lets all three run in place. If
        // that were not so, smoothing would feed back on itself.
        let s = spec("noise_mq");
        let zoom = s.zoom as usize;
        let size = s.size as usize;
        let mut raw = generate(
            &NoiseSpec {
                zoom: 1,
                ..NoiseSpec {
                    name: s.name,
                    size: s.size,
                    zoom: 1,
                    volume: false,
                }
            },
            11,
        );
        // Rebuild what the smoothing pass would have started from, then run
        // it, and check the lattice is unchanged.
        let before = raw.clone();
        smooth(&mut raw, size, 1, zoom);
        for y in (0..size).step_by(zoom) {
            for x in (0..size).step_by(zoom) {
                let i = (y * size + x) * 4;
                assert_eq!(&raw[i..i + 4], &before[i..i + 4], "lattice at {x},{y}");
            }
        }
    }

    #[test]
    fn cubic_passes_through_its_middle_two_points() {
        // At t=0 the interpolant is y1 and at t=1 it is y2, which is what
        // makes the lattice values survive.
        assert!((cubic(0.1, 0.4, 0.7, 0.9, 0.0) - 0.4).abs() < 1e-6);
        assert!((cubic(0.1, 0.4, 0.7, 0.9, 1.0) - 0.7).abs() < 1e-6);
    }
}
