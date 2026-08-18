//! Live check of the visualizer host and every visual in it (Phase 3 M1,
//! extending Phase 1 M6's `scope_smoke`):
//!
//!   cargo run -p player-render --example visual_smoke
//!
//! It renders the real scenes -- same shaders, same camera, same depth and
//! multisample setup the windows use -- into an offscreen texture and reads the
//! pixels back, so every claim here is about what actually came out of the GPU
//! rather than about what the code looks like it should do.
//!
//! Why bother, when the picture is right there on screen in the app? Because
//! "there is a green shape moving" is compatible with a mirrored axis, a camera
//! facing the wrong way, a depth buffer that isn't testing, and an extrusion
//! that runs backwards in time. Each of those has an arithmetic consequence a
//! test can state, and none of them is obvious by eye.
//!
//! M1 splits the checks in two, which is what makes this a host check rather
//! than a scope check:
//!
//! - **Per kind, for every kind in `VisualKind::ALL`** -- the contract the host
//!   relies on: silence draws nothing, audio draws something, a resize is
//!   survived, and a *switch* into a visual leaves the renderer in the same
//!   state as building it fresh. Adding a visual to the registry adds it to
//!   this loop with no edit here, which is decision 6 in practice.
//! - **Per visual, for what only that visual claims** -- the vectorscope's
//!   axes, fade, level and layout, carried over from M6 unchanged.

use std::sync::atomic::{AtomicUsize, Ordering};

use player_render::{OffscreenRenderer, VisualFrame, VisualKind, TRACE_SECONDS};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 320;
const RATE: u32 = 48_000;

/// The counters exist so a run ends with a figure that can be compared against
/// the web half -- "8/8 each" is how every milestone in this project has been
/// reported -- rather than a wall of lines to scan for the word FAIL.
static CHECKS: AtomicUsize = AtomicUsize::new(0);
static FAILURES: AtomicUsize = AtomicUsize::new(0);

fn check(label: &str, ok: bool, detail: String) {
    CHECKS.fetch_add(1, Ordering::Relaxed);
    if !ok {
        FAILURES.fetch_add(1, Ordering::Relaxed);
    }
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

/// A pixel that is part of the drawing rather than the background.
///
/// The threshold is against an sRGB-encoded image, not a linear one: the pass
/// clears to a linear (0.02, 0.02, 0.03), which lands at roughly (40, 40, 48)
/// once encoded -- bright enough that a naive "is it darker than mid-grey" test
/// calls the entire background a trace, which is exactly what the first run of
/// this check reported. The dimmest thing the vectorscope's shader actually
/// draws is the far end of the ribbon, whose blue channel is around 125.
fn is_trace(pixel: &[u8]) -> bool {
    pixel[..3].iter().any(|&channel| channel > 80)
}

struct Figure {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
    /// Mean green of the drawn pixels in the left half of the image against the
    /// right half -- the vectorscope's fade runs along the time axis, which the
    /// camera sees mostly as horizontal.
    green_left: f32,
    green_right: f32,
}

fn measure_at(pixels: &[u8], width: u32, height: u32) -> Figure {
    let (mut count, mut min_x, mut max_x, mut min_y, mut max_y) =
        (0usize, width, 0u32, height, 0u32);
    let (mut left_sum, mut left_n, mut right_sum, mut right_n) = (0.0f32, 0usize, 0.0f32, 0usize);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            if !is_trace(&pixels[i..i + 4]) {
                continue;
            }
            count += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            if x < width / 2 {
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

fn measure(pixels: &[u8]) -> Figure {
    measure_at(pixels, WIDTH, HEIGHT)
}

/// A stereo frame at this example's rate.
fn frame(samples: &[f32]) -> VisualFrame<'_> {
    VisualFrame::new(samples, 2, RATE)
}

/// The contract the *host* depends on, asked of every registered visual.
///
/// Deliberately weak about what a visual draws -- that is the visual's own
/// business and its own checks below -- and strong about the four things the
/// host would break on: an empty feed, a live feed, a resize, and a switch.
fn check_the_host_contract(kind: VisualKind) {
    println!("\n{} ({}):", kind.name(), kind.id());
    let mut renderer = pollster::block_on(OffscreenRenderer::new(WIDTH, HEIGHT, kind))
        .expect("offscreen renderer for this visual");

    // Before anything has played there is no audio, and every visual has to
    // survive being asked to draw that. The vectorscope additionally draws
    // *nothing at all*, which is its own claim, checked below.
    let empty = renderer
        .draw_to_pixels(&frame(&[]))
        .expect("render an empty feed");
    check(
        "an empty feed still renders a frame",
        empty.len() == (WIDTH * HEIGHT * 4) as usize,
        format!("{} bytes back", empty.len()),
    );

    let signal = tone(220.0, 0.3);
    let lit = measure(
        &renderer
            .draw_to_pixels(&frame(&signal))
            .expect("render audio"),
    );
    check(
        "audio draws something",
        lit.count > 100,
        format!("{} lit pixels", lit.count),
    );

    // A switch has to leave the renderer exactly as a fresh one would: the
    // whole of decision 5 is that swapping the scene changes nothing else, and
    // the failure this catches -- a pipeline or attachment left over from the
    // previous visual -- would look fine for the first visual and wrong for
    // the second.
    let mut switched =
        pollster::block_on(OffscreenRenderer::new(WIDTH, HEIGHT, VisualKind::DEFAULT))
            .expect("offscreen renderer to switch");
    switched.set_visual(kind);
    let after = switched
        .draw_to_pixels(&frame(&signal))
        .expect("render after a switch");
    let fresh = renderer
        .draw_to_pixels(&frame(&signal))
        .expect("render without a switch");
    check(
        "switching into this visual draws what building it fresh does",
        after == fresh,
        format!(
            "{} of {} bytes differ",
            after.iter().zip(&fresh).filter(|(a, b)| a != b).count(),
            fresh.len()
        ),
    );

    // Resizing rebuilds whatever the visual keeps that depends on the viewport.
    // Getting it wrong is not subtle -- a stale depth texture is a validation
    // error -- but it is exactly what the Phase 0 scope got wrong on Windows.
    let mut wide = pollster::block_on(OffscreenRenderer::new(WIDTH * 2, HEIGHT, kind))
        .expect("offscreen at a different size");
    let stretched = wide.draw_to_pixels(&frame(&signal)).expect("render at 2:1");
    let widened = measure_at(&stretched, WIDTH * 2, HEIGHT);
    check(
        "a different viewport renders rather than failing",
        widened.count > 100,
        format!("{} lit pixels at 2:1", widened.count),
    );
}

/// What only the vectorscope claims (all of it carried over from M6, where
/// each of these caught something or was written for something that had gone
/// wrong).
fn check_the_vectorscope() {
    println!("\n{} -- its own claims:", VisualKind::Vectorscope.name());
    let mut renderer = pollster::block_on(OffscreenRenderer::new(
        WIDTH,
        HEIGHT,
        VisualKind::Vectorscope,
    ))
    .expect("offscreen renderer");

    // Silence: the scope has to draw *nothing* rather than a dot at the origin,
    // which is what a trace built from an empty window would look like if it
    // padded instead of stopping.
    let idle = measure(&renderer.draw_to_pixels(&frame(&[])).expect("render empty"));
    check(
        "nothing played draws nothing",
        idle.count == 0,
        format!("{} lit pixels", idle.count),
    );

    // Mono: identical channels have no side component, so the figure stands up
    // the centre -- narrow in x, tall in y. It is not perfectly vertical
    // because the camera is off to one side and the extrusion recedes, which is
    // the whole point of the third axis; but it must be clearly taller than
    // wide.
    let mono = tone(220.0, 1.0);
    let figure = measure(&renderer.draw_to_pixels(&frame(&mono)).expect("render mono"));
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
    // frame. The camera sits to the -x side looking back at the origin, so the
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
    let inverted = tone(220.0, -1.0);
    let flat = measure(
        &renderer
            .draw_to_pixels(&frame(&inverted))
            .expect("render inverted"),
    );
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
    let quiet: Vec<f32> = mono.iter().map(|s| s * 0.1).collect();
    let small = measure(
        &renderer
            .draw_to_pixels(&frame(&quiet))
            .expect("render quiet"),
    );
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
    // the tap holds follows the device, so a 1-channel device has to work. This
    // is also the half of ARCHITECTURE.md's old "channel pairing" gap that
    // matters -- a mono device must not have consecutive samples of one channel
    // read as XY pairs.
    let single: Vec<f32> = mono.iter().step_by(2).copied().collect();
    let centred = measure(
        &renderer
            .draw_to_pixels(&VisualFrame::new(&single, 1, RATE))
            .expect("render mono output"),
    );
    check(
        "a one-channel output still draws",
        centred.count > 500,
        format!("{} lit pixels", centred.count),
    );

    // A device with more than two channels contributes its first two -- front
    // left and right, which is what a stereo field means on a surround device.
    // Checked rather than assumed, because the failure (de-interleaving by the
    // wrong width) draws a plausible-looking smear rather than an error.
    let mut surround = Vec::new();
    for pair in mono.chunks_exact(2) {
        surround.extend_from_slice(&[pair[0], pair[1], 0.0, 0.0, 0.0, 0.0]);
    }
    let six = measure(
        &renderer
            .draw_to_pixels(&VisualFrame::new(&surround, 6, RATE))
            .expect("render 5.1 output"),
    );
    check(
        "a 5.1 output draws its front pair, not a smear",
        six.count > 500
            && (six.max_y - six.min_y) > (six.max_x - six.min_x) * 2
            && (six.max_y as i32 - six.min_y as i32 - height as i32).abs() < 8,
        format!(
            "{}px tall, {}px wide, against {}px tall from the same audio in stereo",
            six.max_y - six.min_y,
            six.max_x - six.min_x,
            height
        ),
    );

    // Aspect: a wider viewport shows more scene, and does not stretch the
    // figure. This is what fullscreen turns from a nicety into a requirement --
    // a 480px window and a 2560x1440 screen are the same scene at two aspects.
    let mut wide = pollster::block_on(OffscreenRenderer::new(
        WIDTH * 2,
        HEIGHT,
        VisualKind::Vectorscope,
    ))
    .expect("offscreen at a different size");
    let stretched = wide.draw_to_pixels(&frame(&mono)).expect("render wide");
    let widened = measure_at(&stretched, WIDTH * 2, HEIGHT);
    let wide_width = widened.max_x - widened.min_x;
    check(
        "a wider viewport shows more scene rather than a stretched figure",
        widened.count > 0 && (wide_width as i32 - width as i32).abs() < (width as i32 / 4).max(4),
        format!("{wide_width}px wide at 2:1, against {width}px at 1:1"),
    );

    // And the same claim at a real screen's shape rather than a convenient
    // one: 16:9 is what "fullscreen" actually means on this machine.
    let mut screen = pollster::block_on(OffscreenRenderer::new(1280, 720, VisualKind::Vectorscope))
        .expect("offscreen at 16:9");
    let full = measure_at(
        &screen.draw_to_pixels(&frame(&mono)).expect("render 16:9"),
        1280,
        720,
    );
    let full_height = full.max_y - full.min_y;
    check(
        "at 16:9 the figure scales with the height, not the width",
        full.count > 500
            && (full_height as f32 / height as f32 - 720.0 / HEIGHT as f32).abs() < 0.15,
        format!(
            "{}px tall at 1280x720, {}px at {WIDTH}x{HEIGHT} ({:.2}x against {:.2}x)",
            full_height,
            height,
            full_height as f32 / height as f32,
            720.0 / HEIGHT as f32
        ),
    );
}

fn main() {
    println!("offscreen: {WIDTH}x{HEIGHT}, {TRACE_SECONDS}s of history at {RATE} Hz");
    println!(
        "registry: {} visual(s) -- {}",
        VisualKind::ALL.len(),
        VisualKind::ALL
            .iter()
            .map(|k| k.id())
            .collect::<Vec<_>>()
            .join(", ")
    );

    for kind in VisualKind::ALL {
        check_the_host_contract(*kind);
    }
    check_the_vectorscope();

    let checks = CHECKS.load(Ordering::Relaxed);
    let failures = FAILURES.load(Ordering::Relaxed);
    println!(
        "\n{}/{} checks passed{}",
        checks - failures,
        checks,
        if failures == 0 { "" } else { " -- FAILURES" }
    );
    if failures > 0 {
        std::process::exit(1);
    }
}
