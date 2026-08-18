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
//!   axes, fade, level and layout, carried over from M6 unchanged; and from M2,
//!   the bars' axis direction, height scale, empty case and falling peaks.
//!
//! M2 also made this check run the *real* analyser: the bars draw
//! `player-core`'s spectrum bands, so the tone that lights a bar is measured by
//! the same code the meters use rather than by a levels array written here,
//! which would only be checking itself.

use std::sync::atomic::{AtomicUsize, Ordering};

use player_core::analysis::{band_centres, Meters};
use player_render::{GainSetting, OffscreenRenderer, VisualFrame, VisualKind, TRACE_SECONDS};

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
/// The threshold sits well clear of both: the background is a near-black (5, 5,
/// 8) as displayed (`attachments::BACKGROUND`), and the dimmest thing any
/// shader here draws is the far end of the vectorscope's ribbon, at roughly
/// (31, 17, 53).
///
/// It was 80 until M2, when the shaders started converting their colours for
/// the surface's colour space (`visuals/shared.wgsl`) -- the fragment half of
/// the background bug fixed on 2026-08-18. That made everything on an sRGB
/// surface show the colour it was written as rather than a washed-out encoding
/// of it, which is darker, and left the ribbon's far end below the old
/// threshold. 40 is still five times the brightest channel of the background.
fn is_trace(pixel: &[u8]) -> bool {
    pixel[..3].iter().any(|&channel| channel > 40)
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

/// A 60Hz frame, which is the cadence the desktop redraw thread aims for.
const FRAME_SECONDS: f32 = 1.0 / 60.0;

/// A stereo frame at this example's rate, carrying samples and nothing else --
/// what the vectorscope reads.
fn frame(samples: &[f32]) -> VisualFrame<'_> {
    VisualFrame::new(samples, 2, RATE, FRAME_SECONDS)
}

/// The whole feed the host assembles: samples *and* the spectrum measured from
/// them. What a visual is actually handed at runtime, and therefore what the
/// host contract has to be asked with -- a frame carrying only samples is a
/// frame the bars correctly draw nothing from (M2 decision 10).
fn full_frame<'a>(samples: &'a [f32], spectrum: &'a Spectrum) -> VisualFrame<'a> {
    frame(samples).with_spectrum(&spectrum.levels, &spectrum.centres)
}

/// A frame that advances the clock by `seconds` over a spectrum that is not
/// moving -- for watching the peak caps come down.
fn falling_frame<'a>(spectrum: &'a Spectrum, seconds: f32) -> VisualFrame<'a> {
    let mut frame = full_frame(&[], spectrum);
    frame.elapsed_seconds = seconds;
    frame
}

/// The metering spectrum of some audio, measured by `player-core` itself.
struct Spectrum {
    levels: Vec<f32>,
    centres: Vec<f32>,
}

impl Spectrum {
    /// Runs the real analyser over `samples`, exactly as the analysis thread
    /// does -- so what the bars are handed here is what they are handed by the
    /// app.
    fn of(samples: &[f32], channels: u16) -> Self {
        let mut meters = Meters::new(RATE, channels);
        meters.ingest(samples, false);
        let levels = meters.update(FRAME_SECONDS).to_vec();
        Self {
            levels,
            centres: band_centres(RATE),
        }
    }

    /// A row held at one level across every band. For the checks that are about
    /// the *display* -- how tall full scale draws, how a peak falls -- where a
    /// measured row would only make the arithmetic harder to state.
    fn flat(level_db: f32) -> Self {
        let centres = band_centres(RATE);
        Self {
            levels: vec![level_db; centres.len()],
            centres,
        }
    }
}

/// How tall each column of the image is drawn, as the topmost lit row in it --
/// `None` for a column with nothing in it.
///
/// The measurement the bars are checked through, and it exists because a
/// brightness threshold cannot separate a dim bar from an octave mark: the
/// marks are drawn a violet-grey whose blue channel sits between a quiet bar's
/// and a loud one's. Height separates them completely, because the marks lie on
/// the floor and a bar stands up from it -- and "which bar is tallest" is the
/// question a spectrum display exists to answer anyway.
fn column_tops(pixels: &[u8], width: u32, height: u32) -> Vec<Option<u32>> {
    let mut tops = vec![None; width as usize];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            if is_trace(&pixels[i..i + 4]) && tops[x as usize].is_none() {
                tops[x as usize] = Some(y);
            }
        }
    }
    tops
}

/// The x of the tallest column, and how high it reaches.
fn tallest_column(pixels: &[u8], width: u32, height: u32) -> Option<(u32, u32)> {
    column_tops(pixels, width, height)
        .into_iter()
        .enumerate()
        .filter_map(|(x, top)| top.map(|y| (x as u32, y)))
        .min_by_key(|(_, y)| *y)
}

/// The highest row anything reaches. Smaller is taller: row 0 is the top of the
/// image.
fn top_row(pixels: &[u8], width: u32, height: u32) -> Option<u32> {
    tallest_column(pixels, width, height).map(|(_, y)| y)
}

/// Wider than tall, because a row of bars is: this is the shape the visual is
/// actually looked at in, and 64 bands across 320px would be three pixels each.
const BARS_WIDTH: u32 = 640;
const BARS_HEIGHT: u32 = 360;

/// A bars renderer with no history, which is what most of these checks need:
/// the peak caps survive between frames on purpose (decision 11), so a shot
/// drawn on a renderer that has already seen a loud frame is measuring the
/// leftover cap rather than the bar underneath it. Found by the "louder is
/// taller" check below, which reported three identical heights until each shot
/// got its own renderer.
fn bars_renderer() -> OffscreenRenderer {
    pollster::block_on(OffscreenRenderer::new(
        BARS_WIDTH,
        BARS_HEIGHT,
        VisualKind::SpectrumBars,
    ))
    .expect("offscreen renderer for the bars")
}

/// What only the spectrum bars claim (Phase 3 M2).
fn check_the_spectrum_bars() {
    println!("\n{} -- its own claims:", VisualKind::SpectrumBars.name());

    // Decision 10: a frame with audio but no measurement is not silence, it is
    // "nothing has been measured yet" -- which is the web shell's first moments
    // of a session, before its shared buffer exists. Sixty-four bars resting on
    // the floor would be a picture of the wrong thing.
    let unmeasured = bars_renderer()
        .draw_to_pixels(&frame(&tone(1000.0, 1.0)))
        .expect("render audio with no spectrum");
    check(
        "audio with nothing measured yet draws nothing at all",
        measure_at(&unmeasured, BARS_WIDTH, BARS_HEIGHT).count == 0,
        format!(
            "{} lit pixels",
            measure_at(&unmeasured, BARS_WIDTH, BARS_HEIGHT).count
        ),
    );

    // The axis, and the check that catches a display drawn backwards -- the
    // bar-chart form of the mistake M6's camera check caught in the
    // vectorscope. Both tones go through the real analyser, so what is checked
    // is the whole path: audio, `player-core`'s bands, the frame, the
    // instances, the pixels.
    let low = tone(80.0, 1.0);
    let low_spectrum = Spectrum::of(&low, 2);
    let low_pixels = bars_renderer()
        .draw_to_pixels(&full_frame(&low, &low_spectrum))
        .expect("render a low tone");
    let high = tone(6000.0, 1.0);
    let high_spectrum = Spectrum::of(&high, 2);
    let high_pixels = bars_renderer()
        .draw_to_pixels(&full_frame(&high, &high_spectrum))
        .expect("render a high tone");
    let low_bar = tallest_column(&low_pixels, BARS_WIDTH, BARS_HEIGHT).expect("a low tone draws");
    let high_bar =
        tallest_column(&high_pixels, BARS_WIDTH, BARS_HEIGHT).expect("a high tone draws");
    check(
        "a tone lights a bar on the side of the display its frequency belongs to",
        low_bar.0 < BARS_WIDTH / 2 && high_bar.0 > BARS_WIDTH / 2 && low_bar.0 < high_bar.0,
        format!(
            "80Hz at x={}, 6kHz at x={}, of {BARS_WIDTH}",
            low_bar.0, high_bar.0
        ),
    );

    // Level, which is the other half of what a spectrum display says. Three
    // amplitudes twenty dB apart, through the real analyser: taller has to mean
    // louder every time, or the display is decorative.
    let heights: Vec<u32> = [1.0f32, 0.1, 0.01]
        .iter()
        .map(|amplitude| {
            let samples: Vec<f32> = tone(1000.0, 1.0).iter().map(|s| s * amplitude).collect();
            let spectrum = Spectrum::of(&samples, 2);
            let pixels = bars_renderer()
                .draw_to_pixels(&full_frame(&samples, &spectrum))
                .expect("render a tone at some level");
            top_row(&pixels, BARS_WIDTH, BARS_HEIGHT).unwrap_or(BARS_HEIGHT)
        })
        .collect();
    check(
        "a louder tone draws a taller bar",
        heights[0] < heights[1] && heights[1] < heights[2],
        format!(
            "tops at row {}, {}, {} for full scale, -20dB and -40dB",
            heights[0], heights[1], heights[2]
        ),
    );

    // Decision 13: the display's floor is -72dBFS, not the meter's -120, so a
    // row at the meter's floor has no bars in it at all -- only the octave
    // marks, which lie on the floor of the scene.
    let floor = Spectrum::flat(-120.0);
    let at_floor = bars_renderer()
        .draw_to_pixels(&full_frame(&[], &floor))
        .expect("render a floor-level row");
    let floor_top = top_row(&at_floor, BARS_WIDTH, BARS_HEIGHT);
    check(
        "a row at the meter's floor draws no bars, only the octave marks",
        floor_top.is_none_or(|top| top > BARS_HEIGHT / 2),
        format!("tallest column reaches row {floor_top:?} of {BARS_HEIGHT}"),
    );

    // And full scale reaches near the top of the frame without touching it --
    // the frame is pinned to a full-scale display, exactly as the vectorscope's
    // is.
    let loud = Spectrum::flat(0.0);
    let mut falling = bars_renderer();
    let full_scale = falling
        .draw_to_pixels(&full_frame(&[], &loud))
        .expect("render a full-scale row");
    let full_top = top_row(&full_scale, BARS_WIDTH, BARS_HEIGHT).expect("full scale draws bars");
    check(
        "a full-scale row reaches near the top of the frame without clipping",
        full_top > 0 && full_top < BARS_HEIGHT / 4,
        format!("top at row {full_top} of {BARS_HEIGHT}"),
    );

    // Decision 11: the caps are display state, and they fall in *seconds* --
    // the property that makes a 60Hz redraw thread and a browser's animation
    // callbacks draw the same thing. Checked the way the automatic gain's
    // release was: one long frame against many short ones. `falling` carries
    // the full-scale frame above, which is what put the caps up there.
    let quiet = Spectrum::flat(-120.0);
    let one_second = falling
        .draw_to_pixels(&falling_frame(&quiet, 1.0))
        .expect("render a second of falling");
    let after_one = top_row(&one_second, BARS_WIDTH, BARS_HEIGHT).expect("a cap is still up");
    check(
        "a peak cap outlives the bar under it and falls from where it reached",
        after_one > full_top,
        format!("cap at row {after_one}, having peaked at row {full_top}"),
    );

    let mut stepwise = bars_renderer();
    stepwise
        .draw_to_pixels(&full_frame(&[], &loud))
        .expect("raise the caps");
    for _ in 0..60 {
        stepwise
            .draw_to_pixels(&falling_frame(&quiet, 1.0 / 60.0))
            .expect("a 60Hz frame of falling");
    }
    let stepwise_pixels = stepwise
        .draw_to_pixels(&falling_frame(&quiet, 0.0))
        .expect("read the caps back");
    let stepwise_top =
        top_row(&stepwise_pixels, BARS_WIDTH, BARS_HEIGHT).expect("a cap is still up at 60Hz");
    check(
        "the caps fall at the same rate whatever the frame rate",
        after_one.abs_diff(stepwise_top) <= 2,
        format!(
            "row {after_one} after one long frame against {stepwise_top} after sixty short ones"
        ),
    );
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

    // Before anything has played there is no audio and nothing has been
    // measured, and every visual has to survive being asked to draw that. Each
    // additionally draws *nothing at all* from it, which is its own claim,
    // checked below.
    let empty = renderer
        .draw_to_pixels(&frame(&[]))
        .expect("render an empty feed");
    check(
        "an empty feed still renders a frame",
        empty.len() == (WIDTH * HEIGHT * 4) as usize,
        format!("{} bytes back", empty.len()),
    );

    // What a screen shows behind the visual, which has to be the same on both
    // shells -- reported live 2026-08-18 as near-black on the web and grey on
    // the desktop, from one constant handed to two surfaces of different colour
    // spaces. This texture is sRGB (`OffscreenRenderer::FORMAT`) where the web's
    // canvas is not, so the two halves of this check are exercising precisely
    // the two paths that disagreed.
    let corner = &empty[..3];
    check(
        "the background is the same near-black a screen should show",
        corner
            .iter()
            .zip([5u8, 5, 8])
            .all(|(got, want)| got.abs_diff(want) <= 1),
        format!(
            "rgb({}, {}, {}) against rgb(5, 5, 8)",
            corner[0], corner[1], corner[2]
        ),
    );

    // A whole feed, because the host assembles a whole one: the vectorscope
    // reads the samples and the bars read the spectrum, and a contract check
    // that handed over only half of it would be asking half the gallery to draw
    // nothing.
    let signal = tone(220.0, 0.3);
    let spectrum = Spectrum::of(&signal, 2);
    let lit = measure(
        &renderer
            .draw_to_pixels(&full_frame(&signal, &spectrum))
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
        .draw_to_pixels(&full_frame(&signal, &spectrum))
        .expect("render after a switch");
    let fresh = renderer
        .draw_to_pixels(&full_frame(&signal, &spectrum))
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
    let stretched = wide
        .draw_to_pixels(&full_frame(&signal, &spectrum))
        .expect("render at 2:1");
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
            .draw_to_pixels(&VisualFrame::new(&single, 1, RATE, FRAME_SECONDS))
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
            .draw_to_pixels(&VisualFrame::new(&surround, 6, RATE, FRAME_SECONDS))
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

/// Display gain (added 2026-08-18, after the first live fullscreen run).
///
/// The complaint it answers is arithmetic rather than framing: the frame is sized
/// for a full-scale signal, so a master peaking at -12 dBFS draws at about a
/// quarter of it. These checks are what stop the cure from being worse than the
/// disease -- gain has to magnify the *trace* while leaving the frame, and
/// therefore the meaning of the frame, exactly where it was.
fn check_display_gain() {
    println!(
        "
Display gain:"
    );
    let mut renderer = pollster::block_on(OffscreenRenderer::new(
        WIDTH,
        HEIGHT,
        VisualKind::Vectorscope,
    ))
    .expect("offscreen renderer");

    // A quiet passage: a tenth of full scale, which is roughly -20 dBFS and
    // close to what a dynamic master actually sits at.
    let quiet: Vec<f32> = tone(220.0, 1.0).iter().map(|s| s * 0.125).collect();
    let unity = measure(&renderer.draw_to_pixels(&frame(&quiet)).expect("unity"));
    let unity_height = unity.max_y - unity.min_y;

    renderer.set_gain(GainSetting::Fixed(2.0));
    let doubled = measure(&renderer.draw_to_pixels(&frame(&quiet)).expect("x2"));
    let doubled_height = doubled.max_y - doubled.min_y;

    renderer.set_gain(GainSetting::Fixed(4.0));
    let quadrupled = measure(&renderer.draw_to_pixels(&frame(&quiet)).expect("x4"));
    let quadrupled_height = quadrupled.max_y - quadrupled.min_y;

    // Gain is linear in amplitude, but the *bounding box* is not linear in gain,
    // and the difference is worth stating rather than fudging a tolerance
    // around. The ribbon is extruded through space and viewed from off to one
    // side, so even a signal of no amplitude at all draws a slanted line whose
    // vertical extent is pure geometry -- call it the slant. The box is
    // therefore `slant + amplitude * gain`: measured here as 46px of slant and
    // 18px of amplitude, which predicts 64, 82 and 118px at x1, x2 and x4 and is
    // exactly what came back. So what is checked is the part gain controls: the
    // *increment* over unity, which must scale with the gain.
    let over_unity = |height: u32| height as f32 - unity_height as f32;
    check(
        "gain magnifies the trace in proportion to itself",
        (over_unity(quadrupled_height) / over_unity(doubled_height) - 3.0).abs() < 0.25,
        format!(
            "x2 adds {:.0}px over unity, x4 adds {:.0}px ({:.2}x, wanted 3x)",
            over_unity(doubled_height),
            over_unity(quadrupled_height),
            over_unity(quadrupled_height) / over_unity(doubled_height)
        ),
    );
    check(
        "a magnified figure is still inside the frame",
        quadrupled.min_y > 0
            && quadrupled.max_y < HEIGHT - 1
            && quadrupled.min_x > 0
            && quadrupled.max_x < WIDTH - 1,
        format!(
            "x {}..{}, y {}..{} of {WIDTH}x{HEIGHT} at x4",
            quadrupled.min_x, quadrupled.max_x, quadrupled.min_y, quadrupled.max_y
        ),
    );

    // The claim that keeps this honest for the studio audience: gain is a
    // magnifier, not a re-frame. If the camera refitted itself to the gained
    // figure the picture would be identical at every setting -- which is both a
    // useless control and a scope that silently normalises.
    check(
        "gain magnifies the trace rather than re-framing the scene",
        doubled_height > unity_height && quadrupled_height > doubled_height,
        format!("{unity_height}px, {doubled_height}px, {quadrupled_height}px at x1, x2, x4"),
    );

    // Auto: a quiet passage should end up filling much of the frame, where unity
    // left it small. This is the listening mode, and it is opt-in precisely
    // because the figure's size stops meaning level.
    renderer.set_gain(GainSetting::Auto);
    let automatic = measure(&renderer.draw_to_pixels(&frame(&quiet)).expect("auto"));
    let automatic_height = automatic.max_y - automatic.min_y;
    check(
        "auto lifts a quiet passage to fill the frame",
        automatic_height > (HEIGHT as f32 * 0.6) as u32
            && automatic.min_y > 0
            && automatic.max_y < HEIGHT - 1,
        format!(
            "{automatic_height}px of {HEIGHT} ({:.0}%), against {unity_height}px at unity",
            100.0 * automatic_height as f32 / HEIGHT as f32
        ),
    );

    // ...and gives way to a loud one at once rather than clipping the display
    // while it eases off. One frame of full scale is all it gets.
    let loud = tone(220.0, 1.0);
    let after = measure(&renderer.draw_to_pixels(&frame(&loud)).expect("auto loud"));
    check(
        "auto gives way to a loud passage in one frame",
        after.min_y > 0 && after.max_y < HEIGHT - 1,
        format!("y {}..{} of {HEIGHT}", after.min_y, after.max_y),
    );

    // Unity is still unity afterwards: the setting is the setting, and nothing
    // about the automatic mode leaks into a fixed one.
    renderer.set_gain(GainSetting::Fixed(1.0));
    let again = measure(
        &renderer
            .draw_to_pixels(&frame(&quiet))
            .expect("unity again"),
    );
    check(
        "unity is unchanged by having visited the other settings",
        (again.max_y - again.min_y) == unity_height,
        format!("{}px against {unity_height}px", again.max_y - again.min_y),
    );
}

fn main() {
    println!("offscreen: {WIDTH}x{HEIGHT}, {TRACE_SECONDS}s of history at {RATE} Hz");
    println!(
        "gain: {}",
        GainSetting::ALL
            .iter()
            .map(|g| g.label())
            .collect::<Vec<_>>()
            .join(", ")
    );
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
    check_the_spectrum_bars();
    check_display_gain();

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
