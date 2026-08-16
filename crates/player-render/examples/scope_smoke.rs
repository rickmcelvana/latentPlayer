//! Live check of the 3D vectorscope's pipeline (Phase 1 M6):
//!
//!   cargo run -p player-render --example scope_smoke
//!
//! It renders the real scene -- same shader, same camera, same depth and
//! multisample setup the windows use -- into an offscreen texture and reads
//! the pixels back, so every claim here is about what actually came out of the
//! GPU rather than about what the code looks like it should do.
//!
//! Why bother, when the picture is right there on screen in the app? Because
//! "there is a green shape moving" is compatible with a mirrored axis, a
//! camera facing the wrong way, a depth buffer that isn't testing, and an
//! extrusion that runs backwards in time. Each of those has an arithmetic
//! consequence a test can state, and none of them is obvious by eye.

use player_render::{OffscreenRenderer, TRACE_SECONDS};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 320;
const RATE: u32 = 48_000;

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// One window's worth of interleaved stereo sine, right channel scaled by
/// `right_gain` (-1.0 inverts it, 0.0 hard-pans left).
fn tone(hz: f32, right_gain: f32) -> Vec<f32> {
    let frames = (RATE as f32 * TRACE_SECONDS) as usize;
    (0..frames)
        .flat_map(|n| {
            let s = 0.8 * (2.0 * std::f32::consts::PI * hz * n as f32 / RATE as f32).sin();
            [s, s * right_gain]
        })
        .collect()
}

/// A pixel that is part of the trace rather than the background.
///
/// The threshold is against an sRGB-encoded image, not a linear one: the pass
/// clears to a linear (0.02, 0.02, 0.03), which lands at roughly (40, 40, 48)
/// once encoded -- bright enough that a naive "is it darker than mid-grey"
/// test calls the entire background a trace, which is exactly what the first
/// run of this check reported. The dimmest thing the shader actually draws is
/// the far end of the ribbon, whose blue channel is around 125.
fn is_trace(pixel: &[u8]) -> bool {
    pixel[..3].iter().any(|&channel| channel > 80)
}

struct Figure {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
    /// Mean green of the trace's pixels in the left half of the image against
    /// the right half -- the fade runs along the time axis, which the camera
    /// sees mostly as horizontal.
    green_left: f32,
    green_right: f32,
}

fn measure(pixels: &[u8]) -> Figure {
    let (mut count, mut min_x, mut max_x, mut min_y, mut max_y) =
        (0usize, WIDTH, 0u32, HEIGHT, 0u32);
    let (mut left_sum, mut left_n, mut right_sum, mut right_n) = (0.0f32, 0usize, 0.0f32, 0usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = ((y * WIDTH + x) * 4) as usize;
            if !is_trace(&pixels[i..i + 4]) {
                continue;
            }
            count += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            if x < WIDTH / 2 {
                left_sum += f32::from(pixels[i + 1]);
                left_n += 1;
            } else {
                right_sum += f32::from(pixels[i + 1]);
                right_n += 1;
            }
        }
    }
    Figure {
        count,
        min_x,
        max_x,
        min_y,
        max_y,
        green_left: if left_n > 0 {
            left_sum / left_n as f32
        } else {
            0.0
        },
        green_right: if right_n > 0 {
            right_sum / right_n as f32
        } else {
            0.0
        },
    }
}

fn main() {
    let mut renderer = match pollster::block_on(OffscreenRenderer::new(WIDTH, HEIGHT)) {
        Ok(renderer) => renderer,
        Err(e) => {
            println!("FATAL: no usable GPU adapter for the offscreen check: {e}");
            std::process::exit(1);
        }
    };
    println!("offscreen: {WIDTH}x{HEIGHT}, {TRACE_SECONDS}s of history at {RATE} Hz");

    // Silence first: the scope has to draw *nothing* rather than a dot at the
    // origin, which is what a trace built from an empty window would look like
    // if it padded instead of stopping.
    let empty = renderer.draw_to_pixels(&[], 2).expect("render empty");
    let idle = measure(&empty);
    check(
        "nothing played draws nothing",
        idle.count == 0,
        format!("{} lit pixels", idle.count),
    );

    // Mono: identical channels have no side component, so the figure stands up
    // the centre -- narrow in x, tall in y. It is not perfectly vertical
    // because the camera is off to one side and the extrusion recedes, which
    // is the whole point of the third axis; but it must be clearly taller
    // than wide.
    let mono = renderer
        .draw_to_pixels(&tone(220.0, 1.0), 2)
        .expect("render mono");
    let figure = measure(&mono);
    let width = figure.max_x - figure.min_x;
    let height = figure.max_y - figure.min_y;
    check(
        "mono material draws a tall figure",
        figure.count > 500 && height > width * 2,
        format!("{}px tall, {}px wide, {} lit", height, width, figure.count),
    );
    check(
        "the figure stays inside the frame",
        figure.min_x > 0
            && figure.max_x < WIDTH - 1
            && figure.min_y > 0
            && figure.max_y < HEIGHT - 1,
        format!(
            "x {}..{}, y {}..{} of {WIDTH}x{HEIGHT}",
            figure.min_x, figure.max_x, figure.min_y, figure.max_y
        ),
    );

    // The fade along the time axis is what makes depth readable on a still
    // frame. The camera sits to the +x side looking back at the origin, so the
    // newest (brightest, greenest) end of the ribbon falls on the right of the
    // image and the oldest on the left.
    check(
        "the trace fades as it recedes",
        figure.green_right > figure.green_left * 1.3,
        format!(
            "mean green {:.0} near, {:.0} far",
            figure.green_right, figure.green_left
        ),
    );

    // An inverted pair has no mid component: the same audio, rotated onto the
    // other axis. This is the check that catches a scope wired to raw L/R
    // instead of the goniometer's axes -- that one would draw the same figure
    // for both signals, rotated 45 degrees, rather than swapping tall for wide.
    let inverted = renderer
        .draw_to_pixels(&tone(220.0, -1.0), 2)
        .expect("render inverted");
    let flat = measure(&inverted);
    let flat_width = flat.max_x - flat.min_x;
    let flat_height = flat.max_y - flat.min_y;
    check(
        "an inverted channel lays the figure flat",
        flat.count > 500 && flat_width > flat_height,
        format!(
            "{}px wide, {}px tall, {} lit",
            flat_width, flat_height, flat.count
        ),
    );

    // A quiet passage draws a smaller figure than a loud one. Trivial to state
    // and easy to lose: a scope that normalises its input looks livelier and
    // stops being a measurement.
    let quiet: Vec<f32> = tone(220.0, 1.0).iter().map(|s| s * 0.1).collect();
    let small = measure(&renderer.draw_to_pixels(&quiet, 2).expect("render quiet"));
    check(
        "level is not normalised away",
        (small.max_y - small.min_y) < height / 2,
        format!(
            "{}px tall at a tenth of the amplitude, against {}px",
            small.max_y - small.min_y,
            height
        ),
    );

    // A mono *output* is a mix with no width, and must still draw: the layout
    // the tap holds follows the device, so a 1-channel device has to work.
    let single: Vec<f32> = tone(220.0, 1.0).iter().step_by(2).copied().collect();
    let centred = measure(
        &renderer
            .draw_to_pixels(&single, 1)
            .expect("render mono output"),
    );
    check(
        "a one-channel output still draws",
        centred.count > 500,
        format!("{} lit pixels", centred.count),
    );

    // Resizing rebuilds the depth and multisample targets and the projection.
    // Getting that wrong is not subtle -- a stale depth texture is a validation
    // error and a stale projection stretches the figure -- but it is exactly
    // what the Phase 0 scope got wrong on Windows, so it is worth a check.
    let mut wide = pollster::block_on(OffscreenRenderer::new(WIDTH * 2, HEIGHT))
        .expect("offscreen at a different size");
    let stretched = wide
        .draw_to_pixels(&tone(220.0, 1.0), 2)
        .expect("render wide");
    let mut columns = 0u32;
    let (mut lo, mut hi) = (WIDTH * 2, 0u32);
    for y in 0..HEIGHT {
        for x in 0..WIDTH * 2 {
            let i = ((y * WIDTH * 2 + x) * 4) as usize;
            if is_trace(&stretched[i..i + 4]) {
                columns += 1;
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
    }
    let wide_width = hi - lo;
    check(
        "a wider viewport shows more scene rather than a stretched figure",
        columns > 0 && (wide_width as i32 - width as i32).abs() < (width as i32 / 4).max(4),
        format!("{wide_width}px wide at 2:1, against {width}px at 1:1"),
    );
}
