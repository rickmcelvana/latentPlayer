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
use player_render::{
    GainSetting, Musical, OffscreenRenderer, VisualFrame, VisualKind, HISTORY_SECONDS,
    TRACE_SECONDS, TUNNEL_SECONDS,
};

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

/// A frame that advances the clock by `seconds` over a given spectrum, with no
/// samples in it.
///
/// Every claim about something that moves in *time* is made with this: the
/// bars' caps falling (M2) and the waterfall's history scrolling (M3) are both
/// checked by handing over a second in one piece and then in sixty, which is
/// the difference between the two shells' redraw loops.
fn timed_frame<'a>(spectrum: &'a Spectrum, seconds: f32) -> VisualFrame<'a> {
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

    /// The same measurement against a different frequency axis -- every `step`
    /// band of it. What the web shell's first play looks like from a visual's
    /// side (nothing, then sixty-four bands), and what a change of output rate
    /// would look like.
    fn rebanded(source: &Spectrum, bands: usize) -> Self {
        let step = (source.levels.len() / bands.max(1)).max(1);
        Self {
            levels: source.levels.iter().copied().step_by(step).collect(),
            centres: source.centres.iter().copied().step_by(step).collect(),
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
        .draw_to_pixels(&timed_frame(&quiet, 1.0))
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
            .draw_to_pixels(&timed_frame(&quiet, 1.0 / 60.0))
            .expect("a 60Hz frame of falling");
    }
    let stepwise_pixels = stepwise
        .draw_to_pixels(&timed_frame(&quiet, 0.0))
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

/// The waterfall is looked at in the same shape the bars are, and for the same
/// reason: a spectrum runs across.
const FALL_WIDTH: u32 = 640;
const FALL_HEIGHT: u32 = 360;

/// A waterfall renderer with no history at all.
///
/// The bars needed this because their peak caps outlive a frame; the waterfall
/// needs it far more, since history is the entire visual. Every independent
/// claim below starts from an empty terrain.
fn waterfall_renderer() -> OffscreenRenderer {
    pollster::block_on(OffscreenRenderer::new(
        FALL_WIDTH,
        FALL_HEIGHT,
        VisualKind::SpectrogramWaterfall,
    ))
    .expect("offscreen renderer for the waterfall")
}

/// A pixel from the loud end of the colour ramp -- green-dominant, where the
/// quiet end is violet (`waterfall.wgsl`'s `fs_surface`, the bars' own two
/// colours).
///
/// This is how a ridge is found among the terrain it stands in. A brightness
/// threshold could not: the whole surface is drawn, floor included, so
/// "something is lit here" is true of every column. Being *loud* is the thing
/// the history is supposed to remember.
fn is_loud(pixel: &[u8]) -> bool {
    pixel[1] > 70 && pixel[1] as i32 > pixel[0] as i32 + 25
}

/// Where the loud material is: how many pixels of it there are, and its centre
/// of mass down and across the screen.
struct Ridge {
    count: usize,
    centre_row: f32,
    centre_column: f32,
}

fn ridge(pixels: &[u8], width: u32, height: u32) -> Option<Ridge> {
    let (mut count, mut rows, mut columns) = (0usize, 0.0f64, 0.0f64);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            if is_loud(&pixels[i..i + 4]) {
                count += 1;
                rows += y as f64;
                columns += x as f64;
            }
        }
    }
    (count > 0).then(|| Ridge {
        count,
        centre_row: (rows / count as f64) as f32,
        centre_column: (columns / count as f64) as f32,
    })
}

/// Where the loudest thing on a spectrum display is, across the image.
///
/// The topmost *loud* pixel rather than the topmost lit one, which is what
/// makes this the one measurement both spectrum visuals can be asked for: the
/// waterfall draws a floor and octave rails that reach further up the screen
/// than any ridge does, being further away. Averaged across the row it is found
/// in, so a peak between two pixels does not land on whichever of them the
/// rasteriser preferred.
fn loud_peak_column(pixels: &[u8], width: u32, height: u32) -> Option<f32> {
    for y in 0..height {
        let (mut count, mut sum) = (0usize, 0.0f64);
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            if is_loud(&pixels[i..i + 4]) {
                count += 1;
                sum += x as f64;
            }
        }
        if count > 0 {
            return Some((sum / count as f64) as f32);
        }
    }
    None
}

/// A row of measured columns, for a check's own report.
fn columns(xs: &[f32]) -> String {
    xs.iter()
        .map(|x| format!("{x:.0}"))
        .collect::<Vec<_>>()
        .join("/")
}

/// Plays `spectrum` at the visual for `seconds`, one 60Hz frame at a time --
/// which is how the desktop shell feeds it, and how history actually
/// accumulates.
fn play(renderer: &mut OffscreenRenderer, spectrum: &Spectrum, seconds: f32) -> Vec<u8> {
    let frames = (seconds / FRAME_SECONDS).round().max(1.0) as usize;
    let mut pixels = Vec::new();
    for _ in 0..frames {
        pixels = renderer
            .draw_to_pixels(&timed_frame(spectrum, FRAME_SECONDS))
            .expect("a frame of the waterfall");
    }
    pixels
}

/// What only the spectrogram waterfall claims (Phase 3 M3).
fn check_the_spectrogram_waterfall() {
    println!(
        "\n{} -- its own claims:",
        VisualKind::SpectrogramWaterfall.name()
    );

    // Decision 21, and M2 decision 10's rule: nothing measured is nothing
    // drawn. Not a flat floor stretching into the distance, which would be a
    // picture of six seconds of silence that was never listened to.
    let unmeasured = waterfall_renderer()
        .draw_to_pixels(&frame(&tone(1000.0, 1.0)))
        .expect("render audio with no spectrum");
    let lit = measure_at(&unmeasured, FALL_WIDTH, FALL_HEIGHT).count;
    check(
        "audio with nothing measured yet draws nothing at all",
        lit == 0,
        format!("{lit} lit pixels"),
    );

    // The axis. Both tones go through the real analyser, and both visuals place
    // their bands with `scale::slot_centre` -- so this is the same claim the
    // bars make, checked against the same tones, in a scene with a different
    // camera in front of it.
    let low = tone(80.0, 1.0);
    let low_spectrum = Spectrum::of(&low, 2);
    let high = tone(6000.0, 1.0);
    let high_spectrum = Spectrum::of(&high, 2);
    let low_pixels = play(&mut waterfall_renderer(), &low_spectrum, 0.5);
    let high_pixels = play(&mut waterfall_renderer(), &high_spectrum, 0.5);
    let low_ridge = ridge(&low_pixels, FALL_WIDTH, FALL_HEIGHT).expect("a low tone draws a ridge");
    let high_ridge =
        ridge(&high_pixels, FALL_WIDTH, FALL_HEIGHT).expect("a high tone draws a ridge");
    check(
        "a tone raises a ridge on the side of the display its frequency belongs to",
        low_ridge.centre_column < FALL_WIDTH as f32 / 2.0
            && high_ridge.centre_column > FALL_WIDTH as f32 / 2.0,
        format!(
            "80Hz at x={:.0}, 6kHz at x={:.0}, of {FALL_WIDTH}",
            low_ridge.centre_column, high_ridge.centre_column
        ),
    );

    // And the two spectrum visuals agree about where a tone belongs.
    //
    // Not to the pixel, and not even in proportion: each scene is seen from its
    // own three-quarter view, and an off-axis camera does not map world x to
    // screen x linearly -- the far side of a display is compressed against the
    // near side, by different amounts in two different scenes. What has to
    // agree, and what a person actually reads, is the *order*: four tones an
    // octave-and-a-bit apart have to march left to right in both, because both
    // place a band with `scale::slot_centre` off the same measured axis.
    let ladder = [80.0f32, 400.0, 2000.0, 6000.0];
    let mut on_bars = Vec::new();
    let mut on_waterfall = Vec::new();
    for hz in ladder {
        let samples = tone(hz, 1.0);
        let spectrum = Spectrum::of(&samples, 2);
        let bars_pixels = bars_renderer()
            .draw_to_pixels(&full_frame(&samples, &spectrum))
            .expect("the bars, one tone of the ladder");
        on_bars.push(loud_peak_column(&bars_pixels, BARS_WIDTH, BARS_HEIGHT).expect("a bar"));
        // The waterfall's newest measurement, at its near edge: one row of
        // history draws the profile of that row and nothing else.
        let fall_pixels = waterfall_renderer()
            .draw_to_pixels(&timed_frame(&spectrum, FRAME_SECONDS))
            .expect("the waterfall, one tone of the ladder");
        on_waterfall
            .push(loud_peak_column(&fall_pixels, FALL_WIDTH, FALL_HEIGHT).expect("a profile"));
    }
    let rising = |xs: &[f32]| xs.windows(2).all(|pair| pair[0] < pair[1]);
    check(
        "the bars and the waterfall put a rising set of tones in the same order",
        rising(&on_bars) && rising(&on_waterfall),
        format!(
            "{ladder:?} Hz at {} on the bars and {} on the waterfall",
            columns(&on_bars),
            columns(&on_waterfall)
        ),
    );

    // The claim no visual in this project could make before M3: what was played
    // is still on screen after it stops. A second of tone, then a second of
    // silence -- the ridge has to survive it, and to have moved away.
    let silence = Spectrum::flat(-120.0);
    let mut recording = waterfall_renderer();
    let fresh = play(&mut recording, &low_spectrum, 1.0);
    let fresh_ridge = ridge(&fresh, FALL_WIDTH, FALL_HEIGHT).expect("the tone draws");
    let after = play(&mut recording, &silence, 1.0);
    let aged = ridge(&after, FALL_WIDTH, FALL_HEIGHT);
    check(
        "a ridge outlives the sound that made it",
        aged.is_some(),
        match &aged {
            Some(r) => format!("{} loud pixels a second after the tone stopped", r.count),
            None => "nothing left a second after the tone stopped".into(),
        },
    );
    let aged = aged.expect("a ridge a second later");
    check(
        "and has travelled away from the viewer",
        aged.centre_row < fresh_ridge.centre_row,
        format!(
            "centre at row {:.0}, having been at row {:.0}",
            aged.centre_row, fresh_ridge.centre_row
        ),
    );

    // Decision 17: the history scrolls on a clock, so the same second of
    // silence has to leave the ridge in the same place whether it arrived as
    // sixty frames or as one. This is the check that would have caught a row
    // per frame -- the mistake that scrolls at 60Hz on the desktop and at
    // whatever the display does on the web.
    let mut in_one_go = waterfall_renderer();
    play(&mut in_one_go, &low_spectrum, 1.0);
    let long_frame = in_one_go
        .draw_to_pixels(&timed_frame(&silence, 1.0))
        .expect("one long frame of silence");
    let long_ridge = ridge(&long_frame, FALL_WIDTH, FALL_HEIGHT).expect("a ridge after one frame");
    check(
        "the history scrolls at the same rate whatever the frame rate",
        (long_ridge.centre_row - aged.centre_row).abs() <= 2.0,
        format!(
            "centre at row {:.0} after one long frame against {:.0} after sixty short ones",
            long_ridge.centre_row, aged.centre_row
        ),
    );

    // The ring wraps rather than wrapping *around*: the tone is still there
    // partway through the history and gone past the end of it, and it has not
    // reappeared at the front, which is what an off-by-one in the ring's
    // arithmetic would draw. Bracketed from both sides rather than checked
    // against the constant, so the web half can make exactly this claim without
    // being told the number.
    let mut forgetting = waterfall_renderer();
    play(&mut forgetting, &low_spectrum, 0.5);
    let midway = play(&mut forgetting, &silence, 3.0);
    let remembered = ridge(&midway, FALL_WIDTH, FALL_HEIGHT);
    let later = play(&mut forgetting, &silence, 5.0);
    let forgotten = ridge(&later, FALL_WIDTH, FALL_HEIGHT);
    check(
        "history is as long as it claims to be, and no longer",
        remembered.is_some() && forgotten.is_none(),
        format!(
            "{} loud pixels 3s after the tone, {} at 8s ({HISTORY_SECONDS}s of history)",
            remembered.map_or(0, |r| r.count),
            forgotten.map_or(0, |r| r.count)
        ),
    );

    // Decision 18: the texture holds dBFS as measured, and the display window
    // and gain are applied when it is drawn. So turning gain up re-maps rows
    // written long before the setting changed -- a frozen history, drawn twice
    // at two gains, has to draw two different pictures. If the levels had been
    // normalised on the way in, these two would be identical.
    let quiet: Vec<f32> = tone(80.0, 1.0).iter().map(|s| s * 0.02).collect();
    let quiet_spectrum = Spectrum::of(&quiet, 2);
    let mut gained = waterfall_renderer();
    play(&mut gained, &quiet_spectrum, 1.0);
    let at_unity = gained
        .draw_to_pixels(&timed_frame(&quiet_spectrum, 0.0))
        .expect("the terrain at unity");
    gained.set_gain(GainSetting::Fixed(4.0));
    let at_four = gained
        .draw_to_pixels(&timed_frame(&quiet_spectrum, 0.0))
        .expect("the same terrain, magnified");
    // Measured in loud material rather than in the terrain's silhouette: the
    // surface covers its whole floor, so the topmost lit pixel is the far edge
    // of the floor plane and stays where it is whatever the levels do.
    let unity_loud = ridge(&at_unity, FALL_WIDTH, FALL_HEIGHT).map_or(0, |r| r.count);
    let four_loud = ridge(&at_four, FALL_WIDTH, FALL_HEIGHT).map_or(0, |r| r.count);
    check(
        "display gain lifts the whole terrain, history included",
        four_loud > unity_loud,
        format!(
            "{four_loud} loud pixels at x4 against {unity_loud} at unity, over a history that gained no rows"
        ),
    );
    gained.set_gain(GainSetting::Fixed(1.0));

    // Decision 21 again, at the other end: a feed that goes empty mid-session
    // -- the web shell between tracks -- freezes the terrain rather than
    // scrolling six seconds of real history out behind a wall of silence that
    // was never measured.
    let mut interrupted = waterfall_renderer();
    play(&mut interrupted, &low_spectrum, 1.0);
    let first = interrupted
        .draw_to_pixels(&frame(&[]))
        .expect("an empty feed after some history");
    let second = interrupted.draw_to_pixels(&frame(&[])).expect("and again");
    let held = measure_at(&first, FALL_WIDTH, FALL_HEIGHT).count;
    check(
        "a feed that stops holds the terrain still instead of scrolling",
        first == second && held > 100,
        format!(
            "{} of {} bytes differ across two empty frames, over {held} lit pixels",
            first.iter().zip(&second).filter(|(a, b)| a != b).count(),
            first.len()
        ),
    );

    // Decision 22: the one thing that does clear the history is the frequency
    // axis moving under it, because rows measured against a different axis are
    // not comparable to the ones replacing them. Thirty-two bands after
    // sixty-four is a rebuild of the mesh and an empty terrain, not a panic and
    // not a terrain half in each layout.
    let mut relaid = waterfall_renderer();
    play(&mut relaid, &low_spectrum, 1.0);
    let narrow = Spectrum::rebanded(&low_spectrum, 32);
    let relaid_pixels = relaid
        .draw_to_pixels(&timed_frame(&narrow, FRAME_SECONDS))
        .expect("a different band count");
    let from_scratch = waterfall_renderer()
        .draw_to_pixels(&timed_frame(&narrow, FRAME_SECONDS))
        .expect("the same feed into a new renderer");
    check(
        "a change of band count starts the history again",
        relaid_pixels == from_scratch,
        format!(
            "{} of {} bytes differ from a renderer that never saw the old axis",
            relaid_pixels
                .iter()
                .zip(&from_scratch)
                .filter(|(a, b)| a != b)
                .count(),
            from_scratch.len()
        ),
    );
}

/// The tunnel is looked at in the same shape the other two are, so the three
/// report comparable pixel figures.
const TUBE_WIDTH: u32 = 640;
const TUBE_HEIGHT: u32 = 360;

/// A tunnel renderer with no history at all. Every independent claim starts
/// from an empty tube, for the reason the bars and the waterfall each needed
/// their own: history is state, and state left over from the last shot is what
/// the next one would end up measuring.
fn tunnel_renderer(width: u32, height: u32) -> OffscreenRenderer {
    pollster::block_on(OffscreenRenderer::new(
        width,
        height,
        VisualKind::OscilloscopeTunnel,
    ))
    .expect("offscreen renderer for the tunnel")
}

/// Flies the tunnel for `seconds` on a 60Hz feed of `samples` -- which is how
/// the desktop shell feeds it, and how the tube actually accumulates.
/// Flies for `seconds` with a beat behind the music, holding the phase where a
/// caller wants to look at it. M6's decision 48 is checked through this: the
/// tunnel's own clock is untouched and the *eye* is what the beat moves.
fn fly_to_the_beat(
    renderer: &mut OffscreenRenderer,
    samples: &[f32],
    seconds: f32,
    musical: Musical,
) -> Vec<u8> {
    let frames = (seconds / FRAME_SECONDS).round().max(1.0) as usize;
    let mut pixels = Vec::new();
    for _ in 0..frames {
        pixels = renderer
            .draw_to_pixels(&frame(samples).with_musical(musical))
            .expect("a frame of the tunnel with a beat");
    }
    pixels
}

fn fly(renderer: &mut OffscreenRenderer, samples: &[f32], seconds: f32) -> Vec<u8> {
    let frames = (seconds / FRAME_SECONDS).round().max(1.0) as usize;
    let mut pixels = Vec::new();
    for _ in 0..frames {
        pixels = renderer
            .draw_to_pixels(&frame(samples))
            .expect("a frame of the tunnel");
    }
    pixels
}

/// Measured silence: a window of samples that were played and were zero, which
/// is a different thing from an empty window (nothing played at all) and has to
/// draw differently -- rings at rest rather than no rings.
fn silence() -> Vec<f32> {
    vec![0.0; tone(1.0, 1.0).len()]
}

/// How many pixels in the frame's outer border are background rather than
/// tunnel wall.
///
/// The pixel half of decision 24. Deliberately the border and not the whole
/// image: the middle of the frame is the opening at the far end of the tube,
/// which fades to the background *by design* (`tunnel.wgsl`'s fog) and is the
/// one place a hole is not a hole. What may never happen is seeing past the
/// tube at the edges, which is what a wide window would do to a tube too short
/// for its corners.
fn border_holes(pixels: &[u8], width: u32, height: u32) -> usize {
    let band = (width.min(height) / 16).max(2);
    let mut holes = 0;
    for y in 0..height {
        for x in 0..width {
            let edge = x < band || y < band || x + band >= width || y + band >= height;
            if !edge {
                continue;
            }
            let i = ((y * width + x) * 4) as usize;
            if !is_trace(&pixels[i..i + 4]) {
                holes += 1;
            }
        }
    }
    holes
}

/// How far up the colour ramp the picture sits, on average: the mean amount by
/// which green leads red across everything lit.
///
/// The tunnel's "how loud is this" measurement, and it exists because counting
/// *loud* pixels cannot answer the question at quiet levels -- a third of full
/// scale lands halfway up a violet-to-green ramp, where neither colour is
/// dominant and a threshold reports nothing at all. Greenness is continuous
/// where a threshold is a step, and the wall's displacement is what drives it.
fn ramp_position(pixels: &[u8], width: u32, height: u32) -> f32 {
    let (mut sum, mut count) = (0i64, 0usize);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            if is_trace(&pixels[i..i + 4]) {
                sum += pixels[i + 1] as i64 - pixels[i] as i64;
                count += 1;
            }
        }
    }
    if count == 0 {
        return 0.0;
    }
    sum as f32 / count as f32
}

/// What only the oscilloscope tunnel claims (Phase 3 M4).
fn check_the_oscilloscope_tunnel() {
    println!(
        "\n{} -- its own claims:",
        VisualKind::OscilloscopeTunnel.name()
    );

    // Nothing played is nothing drawn -- the vectorscope's answer to an empty
    // window, which this visual reads the same feed as.
    let unplayed = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT)
        .draw_to_pixels(&frame(&[]))
        .expect("render an empty feed");
    let lit = measure_at(&unplayed, TUBE_WIDTH, TUBE_HEIGHT).count;
    check(
        "nothing played draws nothing at all",
        lit == 0,
        format!("{lit} lit pixels"),
    );

    // Decision 24, in pixels: the tube encloses the frame. A short tube in a
    // wide window is the failure this catches, so it is asked at the same eight
    // shapes the other three visuals' cameras are pinned at.
    let signal = tone(220.0, 0.6);
    let shapes = [
        (320, 320),
        (480, 480),
        (800, 600),
        (1280, 720),
        (1920, 1080),
        (3440, 1440),
        (600, 900),
    ];
    let mut worst = (0usize, shapes[0]);
    for (width, height) in shapes {
        let mut renderer = tunnel_renderer(width, height);
        let pixels = fly(&mut renderer, &signal, 0.2);
        let holes = border_holes(&pixels, width, height);
        if holes >= worst.0 {
            worst = (holes, (width, height));
        }
    }
    check(
        "the tube encloses the frame at every window shape",
        worst.0 == 0,
        format!(
            "{} background pixels in the border across {} shapes, worst at {}x{}",
            worst.0,
            shapes.len(),
            worst.1 .0,
            worst.1 .1
        ),
    );

    // Level, which is what makes this a display of the audio rather than a
    // screensaver: a louder passage pushes the wall further, and the colour
    // ramp says so.
    let levels: Vec<f32> = [1.0f32, 0.3, 0.05]
        .iter()
        .map(|amplitude| {
            let samples: Vec<f32> = tone(220.0, 0.6).iter().map(|s| s * amplitude).collect();
            let mut renderer = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
            let pixels = fly(&mut renderer, &samples, 0.3);
            ramp_position(&pixels, TUBE_WIDTH, TUBE_HEIGHT)
        })
        .collect();
    check(
        "a louder passage pushes the wall further",
        levels[0] > levels[1] && levels[1] > levels[2],
        format!(
            "{:.1}, {:.1}, {:.1} up the colour ramp at full scale, a third and a twentieth",
            levels[0], levels[1], levels[2]
        ),
    );

    // Decision 17's discipline, inherited through `RingHistory`: the tunnel
    // flies at the same speed whatever the frame rate. The same second of
    // silence after the same burst has to leave the tube in the same place
    // drawn as sixty frames or as one.
    let quiet = silence();
    let mut stepwise = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    fly(&mut stepwise, &signal, 0.2);
    let sixty = fly(&mut stepwise, &quiet, 1.0);
    let mut one_go = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    fly(&mut one_go, &signal, 0.2);
    let mut long = frame(&quiet);
    long.elapsed_seconds = 1.0;
    let once = one_go
        .draw_to_pixels(&long)
        .expect("one long frame of flight");
    let stepwise_ridge = ridge(&sixty, TUBE_WIDTH, TUBE_HEIGHT).map_or(0.0, |r| r.centre_row);
    let once_ridge = ridge(&once, TUBE_WIDTH, TUBE_HEIGHT).map_or(0.0, |r| r.centre_row);
    check(
        "the tunnel flies at the same speed whatever the frame rate",
        (stepwise_ridge - once_ridge).abs() <= 2.0,
        format!(
            "the loud rings are centred on row {once_ridge:.0} after one long frame against {stepwise_ridge:.0} after sixty short ones"
        ),
    );

    // The tube is exactly as long as it says: a burst is still in it partway
    // along and gone past the end, and it has not reappeared at the near end,
    // which is what an off-by-one in the ring would draw. Bracketed from both
    // sides rather than against the constant, so the web half can make the same
    // claim without being told the number.
    let mut passing = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    fly(&mut passing, &signal, 0.3);
    let midway = fly(&mut passing, &quiet, 2.0);
    let still_there = ridge(&midway, TUBE_WIDTH, TUBE_HEIGHT).map_or(0, |r| r.count);
    let later = fly(&mut passing, &quiet, 4.0);
    let gone = ridge(&later, TUBE_WIDTH, TUBE_HEIGHT).map_or(0, |r| r.count);
    check(
        "the tunnel is as long as it says it is, and no longer",
        still_there > 0 && gone == 0,
        format!("{still_there} loud pixels 2s after the burst, {gone} at 6s"),
    );

    // Decision 21's rule, which is now the house rule for a visual with
    // history: a feed that stops holds the tube still rather than flying on
    // through rings nobody played.
    let mut interrupted = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    fly(&mut interrupted, &signal, 0.5);
    let first = interrupted
        .draw_to_pixels(&frame(&[]))
        .expect("an empty feed after some flight");
    let second = interrupted.draw_to_pixels(&frame(&[])).expect("and again");
    let held = measure_at(&first, TUBE_WIDTH, TUBE_HEIGHT).count;
    check(
        "a feed that stops holds the tunnel still instead of flying on",
        first == second && held > 100,
        format!(
            "{} of {} bytes differ across two empty frames, over {held} lit pixels",
            first.iter().zip(&second).filter(|(a, b)| a != b).count(),
            first.len()
        ),
    );

    // Decision 28: display gain is a multiplier here, and the clamp that keeps
    // the wall off the eye survives it. At x8 on a full-scale signal the wall
    // must push visibly further *and* still leave a tunnel to look down --
    // a closed tube is a screenful of flat colour, which reads as a bug.
    let loud = tone(220.0, 0.6);
    let mut gained = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    let unity = fly(&mut gained, &loud, 0.3);
    let unity_loud = ridge(&unity, TUBE_WIDTH, TUBE_HEIGHT).map_or(0, |r| r.count);
    gained.set_gain(GainSetting::Fixed(8.0));
    let eight = fly(&mut gained, &loud, 0.3);
    let eight_loud = ridge(&eight, TUBE_WIDTH, TUBE_HEIGHT).map_or(0, |r| r.count);
    let opening = measure_at(&eight, TUBE_WIDTH, TUBE_HEIGHT);
    check(
        "gain widens the wall without closing the tube on the eye",
        eight_loud > unity_loud && opening.count < (TUBE_WIDTH * TUBE_HEIGHT) as usize,
        format!(
            "{eight_loud} loud pixels at x8 against {unity_loud} at unity, with {} of {} pixels still down the tunnel",
            (TUBE_WIDTH * TUBE_HEIGHT) as usize - opening.count,
            TUBE_WIDTH * TUBE_HEIGHT
        ),
    );
    gained.set_gain(GainSetting::Fixed(1.0));

    // Decision 48, and M4's deferred decision 27 arriving: a believed beat
    // moves the eye along the path it was already on. The tunnel's own ring
    // clock is untouched -- speeding *that* would change what a ring is a
    // picture of, and how many seconds the tube holds.
    let on_the_move = Musical {
        bpm: 120.0,
        beat_confidence: 1.0,
        // Halfway between two beats, which is where the surge is largest.
        beat_phase: 0.5,
        ..Musical::default()
    };
    let mut with_beat = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    let beaten = fly_to_the_beat(&mut with_beat, &signal, 0.4, on_the_move);
    let mut without_beat = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    let steady = fly(&mut without_beat, &signal, 0.4);
    let moved = beaten.iter().zip(&steady).filter(|(a, b)| a != b).count();
    check(
        "a believed beat moves the flight",
        moved > 0,
        format!("{moved} of {} bytes differ at 120 BPM", steady.len()),
    );

    // And exactly on the beat it is *precisely* the flight with no tempo at
    // all: the surge is zero there, which is what stops it drifting and what
    // keeps every M4 check above asking about the tunnel it was written for.
    let mut on_the_beat = tunnel_renderer(TUBE_WIDTH, TUBE_HEIGHT);
    let arrived = fly_to_the_beat(
        &mut on_the_beat,
        &signal,
        0.4,
        Musical {
            beat_phase: 0.0,
            ..on_the_move
        },
    );
    let differ = arrived.iter().zip(&steady).filter(|(a, b)| a != b).count();
    check(
        "on the beat it is exactly the flight with no tempo",
        differ == 0,
        format!("{differ} of {} bytes differ", steady.len()),
    );

    // The surge may not push the eye anywhere the tube stops enclosing it --
    // the one way this change could break a claim M4 confirmed live.
    let mut worst = (0usize, (0u32, 0u32), 0.0f32);
    for shape in [(320u32, 320u32), (1280, 720), (600, 900)] {
        for phase in [0.0, 0.25, 0.5, 0.75] {
            let mut renderer = tunnel_renderer(shape.0, shape.1);
            let pixels = fly_to_the_beat(
                &mut renderer,
                &signal,
                0.3,
                Musical {
                    beat_phase: phase,
                    ..on_the_move
                },
            );
            let holes = border_holes(&pixels, shape.0, shape.1);
            if holes >= worst.0 {
                worst = (holes, shape, phase);
            }
        }
    }
    check(
        "the tube still encloses the frame wherever the beat pushes the eye",
        worst.0 == 0,
        format!(
            "{} background pixels in the border, worst at {}x{} on phase {:.2}",
            worst.0, worst.1 .0, worst.1 .1, worst.2
        ),
    );
}

/// The contract the *host* depends on, asked of every registered visual.
///
/// Deliberately weak about what a visual draws -- that is the visual's own
/// business and its own checks below -- and strong about the four things the
/// host would break on: an empty feed, a live feed, a resize, and a switch.
/// Whether this visual reads `VisualFrame::musical` at all.
///
/// Stated here rather than in the registry because it is a claim about the
/// *picture* rather than about the gallery -- neither shell has any use for it,
/// and a visual may start or stop reading the beat without either of them
/// learning a new fact about it. The web half carries the same list, and the
/// two are held together by the figures they report.
fn reads_musical(kind: VisualKind) -> bool {
    match kind {
        VisualKind::Vectorscope | VisualKind::SpectrumBars | VisualKind::SpectrogramWaterfall => {
            false
        }
        // The tunnel joined them in M6: decision 48 gives its camera the beat,
        // which M4 deferred to this milestone rather than guessing at.
        VisualKind::OscilloscopeTunnel
        | VisualKind::ParticleField
        | VisualKind::RaymarchedSolid => true,
        // MilkDrop presets are written against `bass`/`mid`/`treb`, which this
        // build summarises from the frame's own spectrum -- so the tempo and
        // onset figures reach nothing, and the host contract checks that they
        // change nothing.
        VisualKind::MilkDrop => false,
        // `VisualKind` is `#[non_exhaustive]`, so a visual added in a later
        // build reaches here rather than failing to compile. "Reads nothing"
        // is the safe answer: the contract check then asserts the musical
        // figures change its picture by nothing, which a visual that *does*
        // read them will fail loudly rather than pass quietly.
        _ => false,
    }
}

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
    //
    // The scene is rebuilt first so this is the *first* frame it has drawn,
    // which is what the `fresh` baseline below is too -- and, since M7a, what
    // the empty frame above no longer leaves it as: a feedback loop advances on
    // every frame it is given. Without this the two halves of the check would
    // report different pixel counts for the same claim.
    let signal = tone(220.0, 0.3);
    let spectrum = Spectrum::of(&signal, 2);
    renderer.set_visual(kind);
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
    //
    // Both sides get a renderer that has drawn *nothing* before this frame,
    // which matters from M3 onwards: a visual with history is not expected to
    // draw the same picture after one frame as after three, so comparing a
    // switched renderer against one that has already been drawn to would be
    // comparing two histories rather than two renderers.
    let mut switched =
        pollster::block_on(OffscreenRenderer::new(WIDTH, HEIGHT, VisualKind::DEFAULT))
            .expect("offscreen renderer to switch");
    switched.set_visual(kind);
    let after = switched
        .draw_to_pixels(&full_frame(&signal, &spectrum))
        .expect("render after a switch");
    let fresh = pollster::block_on(OffscreenRenderer::new(WIDTH, HEIGHT, kind))
        .expect("offscreen renderer built as this kind")
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

    // M5 put the musical figures in the feed and M6 is where scenes started
    // reading them, so this is two claims in one: a visual that reads them must
    // draw something different when they change, and every visual that does not
    // must draw *exactly* what it drew without them. The second half is what
    // stops a beat leaking into the four visuals confirmed before M6.
    // Two fresh renderers rather than two draws, for the switch check's reason.
    let beat = Musical {
        bpm: 128.0,
        beat_confidence: 0.9,
        beat_phase: 0.25,
        onset: 0.8,
        transient: 0.6,
        centroid_hz: 3200.0,
    };
    // The baseline is `fresh` above -- already a renderer that had drawn
    // nothing before exactly this frame. The web half shares the reason and
    // has a harder one: a browser page only has so many GPU devices in it.
    let without = &fresh;
    let with_beat = pollster::block_on(OffscreenRenderer::new(WIDTH, HEIGHT, kind))
        .expect("offscreen renderer with musical figures")
        .draw_to_pixels(&full_frame(&signal, &spectrum).with_musical(beat))
        .expect("render with musical figures");
    let changed = without
        .iter()
        .zip(&with_beat)
        .filter(|(a, b)| a != b)
        .count();
    check(
        if reads_musical(kind) {
            "the musical figures change what this visual draws"
        } else {
            "the musical figures change nothing a visual does not read"
        },
        (changed > 0) == reads_musical(kind),
        format!(
            "{changed} of {} bytes differ at {} BPM",
            without.len(),
            beat.bpm
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

/// A renderer showing the particle field, at the usual size.
fn particles_renderer() -> OffscreenRenderer {
    pollster::block_on(OffscreenRenderer::new(
        WIDTH,
        HEIGHT,
        VisualKind::ParticleField,
    ))
    .expect("offscreen renderer for the particle field")
}

/// What the music is doing, for a scene that reads it: a firm 120 BPM at
/// `phase`, with `onset` as the strongest since the last drain.
fn musical_at(phase: f32, onset: f32, centroid_hz: f32) -> Musical {
    Musical {
        bpm: 120.0,
        beat_confidence: 0.9,
        beat_phase: phase,
        onset,
        transient: 0.5,
        centroid_hz,
    }
}

/// Runs the field for `seconds` of 60Hz frames over a held feed, returning the
/// last frame's pixels.
///
/// The beat and the onsets are synthesised the way the analysis would publish
/// them: a tempo throughout, and an onset standing in the snapshot for the one
/// frame after each beat -- which is what a drain publishes and holds (M5
/// decision 37).
fn spin(
    renderer: &mut OffscreenRenderer,
    samples: &[f32],
    seconds: f32,
    beats: bool,
    centroid_hz: f32,
) -> Vec<u8> {
    let frames = (seconds / FRAME_SECONDS).round() as usize;
    let mut pixels = Vec::new();
    for n in 0..frames {
        let elapsed = n as f32 * FRAME_SECONDS;
        let musical = if beats {
            let phase = (elapsed * 2.0).fract();
            let onset = if phase < FRAME_SECONDS * 2.0 {
                0.9
            } else {
                0.0
            };
            musical_at(phase, onset, centroid_hz)
        } else {
            Musical {
                centroid_hz,
                ..Musical::default()
            }
        };
        pixels = renderer
            .draw_to_pixels(&frame(samples).with_musical(musical))
            .expect("render the particle field");
    }
    pixels
}

/// The mean colour of everything drawn, which is how a field of thousands of
/// sparks is asked what colour it is.
fn mean_colour(pixels: &[u8]) -> [f32; 3] {
    let (mut sums, mut count) = ([0.0f32; 3], 0usize);
    for pixel in pixels.chunks_exact(4) {
        if !is_trace(pixel) {
            continue;
        }
        count += 1;
        for (sum, channel) in sums.iter_mut().zip(&pixel[..3]) {
            *sum += f32::from(*channel);
        }
    }
    let count = count.max(1) as f32;
    [sums[0] / count, sums[1] / count, sums[2] / count]
}

/// What only the particle field claims (Phase 3 M6).
fn check_the_particle_field() {
    println!("\n{} -- its own claims:", VisualKind::ParticleField.name());
    let signal = tone(220.0, 0.6);

    // Nothing measured is nothing drawn, the answer every visual here gives an
    // empty feed -- and for this one it also means the simulation never ran, so
    // there is no field to age.
    let mut renderer = particles_renderer();
    let empty = measure(
        &renderer
            .draw_to_pixels(&frame(&[]))
            .expect("render an empty feed"),
    );
    check(
        "nothing played draws nothing",
        empty.count == 0,
        format!("{} lit pixels", empty.count),
    );

    // Silence is measured and draws nothing either, which is a different claim:
    // the clock runs, the simulation steps, and no particle is spawned because
    // there is nothing to spawn one for.
    let mut renderer = particles_renderer();
    let silent = measure(&spin(&mut renderer, &silence(), 1.0, false, 0.0));
    check(
        "silence runs the simulation and still draws nothing",
        silent.count == 0,
        format!("{} lit pixels after a second of silence", silent.count),
    );

    // Audio fills the field. The figure is a disc seen from above its own
    // plane, so it is wider than it is tall -- which is also the check that the
    // camera is where this visual thinks it is.
    let mut renderer = particles_renderer();
    let lit = measure(&spin(&mut renderer, &signal, 2.0, false, 1200.0));
    let width = lit.max_x - lit.min_x + 1;
    let height = lit.max_y - lit.min_y + 1;
    check(
        "audio fills a disc wider than it is tall",
        lit.count > 2_000 && width > height,
        format!(
            "{} lit pixels, {width}px wide by {height}px tall",
            lit.count
        ),
    );

    // The compute pass ran. Nothing else in this crate asserts *motion without
    // input*: the waterfall and the tunnel hold still when the feed does, and
    // this one must not, because its state is advanced by arithmetic rather
    // than replaced by a measurement.
    let mut renderer = particles_renderer();
    let _ = spin(&mut renderer, &signal, 1.0, false, 1200.0);
    let before = renderer
        .draw_to_pixels(&frame(&signal))
        .expect("render a frame");
    let after = renderer
        .draw_to_pixels(&frame(&signal))
        .expect("render the next frame");
    let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
    check(
        "the simulation advances on the same feed",
        moved > 1_000,
        format!("{moved} of {} bytes differ between two frames", after.len()),
    );

    // Decision 42: a second delivered in one piece and in sixty moves the field
    // exactly as far. The failure this catches is a simulation stepped once per
    // frame, which would run at the display's rate rather than at its own.
    let mut whole = particles_renderer();
    let mut split = particles_renderer();
    let one_second = {
        let mut frame = frame(&signal);
        frame.elapsed_seconds = 1.0;
        whole.draw_to_pixels(&frame).expect("render a whole second")
    };
    let sixtieths = {
        let mut pixels = Vec::new();
        for _ in 0..60 {
            pixels = split
                .draw_to_pixels(&frame(&signal))
                .expect("render a sixtieth");
        }
        pixels
    };
    let differ = one_second
        .iter()
        .zip(&sixtieths)
        .filter(|(a, b)| a != b)
        .count();
    check(
        "a second in one frame moves the field as far as sixty frames of a sixtieth",
        differ == 0,
        format!("{differ} of {} bytes differ", one_second.len()),
    );

    // Decision 43, and the bug this milestone was most likely to ship. An onset
    // is *held* in the published snapshot until the next analysis drain
    // replaces it, so a 180Hz display reads the same onset three times where a
    // 60Hz display reads it once. Spawning per frame that saw one would put
    // three times as many particles on the better machine; spawning per second
    // of simulated time cannot.
    let mut once = particles_renderer();
    let mut thrice = particles_renderer();
    let burst = musical_at(0.5, 0.9, 1200.0);
    let single = once
        .draw_to_pixels(&frame(&signal).with_musical(burst))
        .expect("render one frame with an onset");
    let repeated = {
        let mut pixels = Vec::new();
        for _ in 0..3 {
            let mut third = frame(&signal).with_musical(burst);
            third.elapsed_seconds = FRAME_SECONDS / 3.0;
            pixels = thrice
                .draw_to_pixels(&third)
                .expect("render a third of a frame with the same onset");
        }
        pixels
    };
    let differ = single.iter().zip(&repeated).filter(|(a, b)| a != b).count();
    check(
        "the same onset read three times throws what reading it once throws",
        differ == 0,
        format!("{differ} of {} bytes differ", single.len()),
    );

    // A beat is movement: the same audio with a tempo behind it pushes the
    // field wider than the same audio without one. Decision 45 is the other
    // half of this -- material with no tempo gets the second picture, not a
    // pulse invented for it.
    let mut with_beat = particles_renderer();
    let mut without_beat = particles_renderer();
    let beaten = measure(&spin(&mut with_beat, &signal, 3.0, true, 1200.0));
    let steady = measure(&spin(&mut without_beat, &signal, 3.0, false, 1200.0));
    let beaten_width = beaten.max_x - beaten.min_x + 1;
    let steady_width = steady.max_x - steady.min_x + 1;
    check(
        "onsets and the beat throw the field wider than a steady tone does",
        beaten_width > steady_width,
        format!("{beaten_width}px wide with a beat, {steady_width}px without"),
    );

    // Decision 46's sibling: the colour is a measurement. A dark, bass-heavy
    // moment is blue; a bright one is warm. Checked as the ratio of red to
    // blue, because "which hue" is not a thing a pixel says on its own.
    let mut dark = particles_renderer();
    let mut bright = particles_renderer();
    let low = mean_colour(&spin(&mut dark, &signal, 1.5, false, 120.0));
    let high = mean_colour(&spin(&mut bright, &signal, 1.5, false, 6_000.0));
    check(
        "the spectral centroid colours the field",
        low[2] > low[0] && high[0] > low[0],
        format!(
            "rgb({:.0}, {:.0}, {:.0}) at 120Hz against rgb({:.0}, {:.0}, {:.0}) at 6kHz",
            low[0], low[1], low[2], high[0], high[1], high[2]
        ),
    );
}

/// A renderer showing the raymarched solid, at the usual size.
fn solid_renderer(width: u32, height: u32) -> OffscreenRenderer {
    pollster::block_on(OffscreenRenderer::new(
        width,
        height,
        VisualKind::RaymarchedSolid,
    ))
    .expect("offscreen renderer for the raymarched solid")
}

/// How wide the drawn figure is across one row band, as a fraction of the
/// image: `from` and `to` are fractions of the height, 0 at the top.
///
/// The measurement decision 46 is checked through. The solid's outline *is* the
/// spectrum -- low frequencies at the foot, high at the crown -- so "is this
/// material bass-heavy" and "is the bottom of the figure wider than the top"
/// have to be the same question.
fn width_between(pixels: &[u8], width: u32, height: u32, from: f32, to: f32) -> f32 {
    let first = (height as f32 * from) as u32;
    let last = (height as f32 * to) as u32;
    let (mut min_x, mut max_x) = (width, 0u32);
    for y in first..last.min(height) {
        for x in 0..width {
            let at = ((y * width + x) * 4) as usize;
            if is_trace(&pixels[at..at + 4]) {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
            }
        }
    }
    if max_x < min_x {
        return 0.0;
    }
    (max_x - min_x + 1) as f32 / width as f32
}

/// A spectrum sloping from `low_db` at the bottom of the axis to `high_db` at
/// the top -- bass-heavy one way round and treble-heavy the other.
fn sloped(low_db: f32, high_db: f32) -> Spectrum {
    let centres = band_centres(RATE);
    let last = (centres.len() - 1).max(1) as f32;
    let levels = (0..centres.len())
        .map(|band| low_db + (high_db - low_db) * (band as f32 / last))
        .collect();
    Spectrum { levels, centres }
}

/// How many pixels of the image's outer border are *not* background.
///
/// The solid must not touch the edge at any window shape -- the opposite of the
/// tunnel's claim, and the same arithmetic. A camera fitted in angle rather than
/// in tangent, or one fitted to the height alone, fails this on a tall window.
fn border_touches(pixels: &[u8], width: u32, height: u32) -> usize {
    let mut touched = 0;
    for x in 0..width {
        for y in [0, height - 1] {
            let at = ((y * width + x) * 4) as usize;
            if is_trace(&pixels[at..at + 4]) {
                touched += 1;
            }
        }
    }
    for y in 0..height {
        for x in [0, width - 1] {
            let at = ((y * width + x) * 4) as usize;
            if is_trace(&pixels[at..at + 4]) {
                touched += 1;
            }
        }
    }
    touched
}

/// What only the raymarched solid claims (Phase 3 M6).
fn check_the_raymarched_solid() {
    println!(
        "\n{} -- its own claims:",
        VisualKind::RaymarchedSolid.name()
    );
    let signal = tone(220.0, 0.6);

    // Nothing measured is nothing drawn -- the bars' answer (M2 decision 10),
    // and a different picture from a sphere resting on the display floor.
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let unmeasured = measure(
        &renderer
            .draw_to_pixels(&frame(&signal))
            .expect("render audio with no spectrum"),
    );
    check(
        "audio with nothing measured yet draws nothing at all",
        unmeasured.count == 0,
        format!("{} lit pixels", unmeasured.count),
    );

    // Decision 46, and the reason this visual is a picture of the spectrum
    // rather than a demo: the silhouette is the display. Bass-heavy material
    // has to be wider at the foot than at the crown, and treble-heavy material
    // the other way round -- not "different", but different in the stated
    // direction.
    let bass = sloped(-6.0, -66.0);
    let treble = sloped(-66.0, -6.0);
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let heavy_low = renderer
        .draw_to_pixels(&full_frame(&signal, &bass))
        .expect("render a bass-heavy spectrum");
    let low_foot = width_between(&heavy_low, WIDTH, HEIGHT, 0.62, 0.88);
    let low_crown = width_between(&heavy_low, WIDTH, HEIGHT, 0.12, 0.38);

    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let heavy_high = renderer
        .draw_to_pixels(&full_frame(&signal, &treble))
        .expect("render a treble-heavy spectrum");
    let high_foot = width_between(&heavy_high, WIDTH, HEIGHT, 0.62, 0.88);
    let high_crown = width_between(&heavy_high, WIDTH, HEIGHT, 0.12, 0.38);
    check(
        "the silhouette is the spectrum, bass at the foot and treble at the crown",
        low_foot > low_crown && high_crown > high_foot,
        format!(
            "bass-heavy: {:.2} foot against {:.2} crown; treble-heavy: {:.2} foot against {:.2} crown",
            low_foot, low_crown, high_foot, high_crown
        ),
    );

    // The frequencies are placed by *Hz* through the frame's own axis, so the
    // same measurement on a different band count is the same solid. The web
    // shell's rows arrive at whatever width the analyser publishes, and an
    // output rate change moves every centre.
    let halved = Spectrum::rebanded(&bass, bass.levels.len() / 2);
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let coarse = renderer
        .draw_to_pixels(&full_frame(&signal, &halved))
        .expect("render half as many bands");
    let coarse_foot = width_between(&coarse, WIDTH, HEIGHT, 0.62, 0.88);
    check(
        "half as many bands is the same shape, because a band is placed by its frequency",
        (coarse_foot - low_foot).abs() < 0.04,
        format!(
            "{coarse_foot:.2} of the frame at {} bands, {low_foot:.2} at {}",
            halved.levels.len(),
            bass.levels.len()
        ),
    );

    // The frame holds it at every window shape -- M1's fit, arrived at here
    // through the angle a ball subtends rather than through a hull, and checked
    // the way the tunnel's enclosure is: in pixels, at the shapes a real window
    // takes.
    let shapes = [
        (320u32, 320u32),
        (800, 600),
        (1280, 720),
        (1920, 1080),
        (600, 900),
        (400, 1200),
    ];
    let mut worst = (0usize, shapes[0]);
    for shape in shapes {
        let mut renderer = solid_renderer(shape.0, shape.1);
        let pixels = renderer
            .draw_to_pixels(&full_frame(&signal, &bass))
            .expect("render at this shape");
        let touched = border_touches(&pixels, shape.0, shape.1);
        if touched >= worst.0 {
            worst = (touched, shape);
        }
    }
    check(
        "the solid stays inside the frame at every window shape",
        worst.0 == 0,
        format!(
            "{} border pixels touched across {} shapes, worst at {}x{}",
            worst.0,
            shapes.len(),
            worst.1 .0,
            worst.1 .1
        ),
    );

    // The beat is the size (decision 45's other half): a believed tempo swells
    // the solid on the beat, and the same audio with no tempo does not.
    let on_the_beat = Musical {
        bpm: 120.0,
        beat_confidence: 1.0,
        beat_phase: 0.0,
        ..Musical::default()
    };
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let swollen = measure(
        &renderer
            .draw_to_pixels(&full_frame(&signal, &bass).with_musical(on_the_beat))
            .expect("render on the beat"),
    );
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let resting = measure(
        &renderer
            .draw_to_pixels(&full_frame(&signal, &bass))
            .expect("render with no tempo"),
    );
    check(
        "a believed beat swells the solid",
        swollen.count > resting.count,
        format!(
            "{} lit pixels on the beat, {} with no tempo",
            swollen.count, resting.count
        ),
    );

    // And decision 45 exactly: halfway between two beats the swell is zero, so
    // material *with* a tempo at that instant draws precisely what material
    // with no tempo draws. Byte for byte, because there is nothing else in the
    // frame for a tempo to touch.
    let between_beats = Musical {
        beat_phase: 0.5,
        ..on_the_beat
    };
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let midway = renderer
        .draw_to_pixels(&full_frame(&signal, &bass).with_musical(between_beats))
        .expect("render between two beats");
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let untempoed = renderer
        .draw_to_pixels(&full_frame(&signal, &bass))
        .expect("render with no tempo at all");
    let differ = midway
        .iter()
        .zip(&untempoed)
        .filter(|(a, b)| a != b)
        .count();
    check(
        "between two beats it is exactly what material with no tempo draws",
        differ == 0,
        format!("{differ} of {} bytes differ", untempoed.len()),
    );

    // Display gain is decibels here, as it is for every spectrum display in
    // this crate (M2 decision 12) -- so turning it up pushes the surface out
    // rather than doing nothing.
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let quiet = sloped(-48.0, -66.0);
    let unity = measure(
        &renderer
            .draw_to_pixels(&full_frame(&signal, &quiet))
            .expect("render at unity"),
    );
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    renderer.set_gain(GainSetting::Fixed(4.0));
    let magnified = measure(
        &renderer
            .draw_to_pixels(&full_frame(&signal, &quiet))
            .expect("render at x4"),
    );
    check(
        "display gain pushes the surface out",
        magnified.count > unity.count,
        format!(
            "{} lit pixels at x4, {} at unity",
            magnified.count, unity.count
        ),
    );

    // The colour is the brightness of the moment, through the same `palette`
    // the particle field uses -- so the two cannot disagree about 3kHz.
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let dark = mean_colour(
        &renderer
            .draw_to_pixels(&full_frame(&signal, &bass).with_musical(Musical {
                centroid_hz: 120.0,
                ..Musical::default()
            }))
            .expect("render a dark moment"),
    );
    let mut renderer = solid_renderer(WIDTH, HEIGHT);
    let bright = mean_colour(
        &renderer
            .draw_to_pixels(&full_frame(&signal, &bass).with_musical(Musical {
                centroid_hz: 6_000.0,
                ..Musical::default()
            }))
            .expect("render a bright moment"),
    );
    check(
        "the spectral centroid colours the solid",
        dark[2] > dark[0] && bright[0] > dark[0],
        format!(
            "rgb({:.0}, {:.0}, {:.0}) at 120Hz against rgb({:.0}, {:.0}, {:.0}) at 6kHz",
            dark[0], dark[1], dark[2], bright[0], bright[1], bright[2]
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

/// A preset that draws a bright circular waveform and nothing else, plus
/// whatever extra keys a claim needs.
///
/// `warp=0` and `fWaveSmoothing=0` throughout: the point of each check below is
/// one named behaviour, and the default ripple and the sample low-pass are two
/// other behaviours that would blur what is being measured. Both have their own
/// checks elsewhere.
fn milk_preset(extra: &str) -> String {
    format!(
        "[preset00]\n\
         warp=0\n\
         nWaveMode=0\n\
         fWaveAlpha=1.0\n\
         fWaveScale=1.0\n\
         fWaveSmoothing=0\n\
         bMaximizeWaveColor=0\n\
         fGammaAdj=1.0\n\
         bTexWrap=0\n\
         {extra}"
    )
}

/// Mean brightness of the whole frame, 0..255. The figure that says whether a
/// feedback loop is holding its picture or letting it go.
fn mean_brightness(pixels: &[u8]) -> f32 {
    let mut sum = 0.0f32;
    for pixel in pixels.chunks_exact(4) {
        sum += f32::from(pixel[0].max(pixel[1]).max(pixel[2]));
    }
    sum / (pixels.len() / 4) as f32
}

/// Mean brightness inside a centred box of `fraction` of the frame.
fn mean_centre(pixels: &[u8], fraction: f32) -> f32 {
    let half_w = (WIDTH as f32 * fraction * 0.5) as u32;
    let half_h = (HEIGHT as f32 * fraction * 0.5) as u32;
    let (cx, cy) = (WIDTH / 2, HEIGHT / 2);
    let (mut sum, mut n) = (0.0f32, 0usize);
    for y in cy.saturating_sub(half_h)..(cy + half_h).min(HEIGHT) {
        for x in cx.saturating_sub(half_w)..(cx + half_w).min(WIDTH) {
            let i = ((y * WIDTH + x) * 4) as usize;
            sum += f32::from(pixels[i].max(pixels[i + 1]).max(pixels[i + 2]));
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

/// Draws `frames` frames of the loaded preset and answers with the last.
fn run_preset(renderer: &mut OffscreenRenderer, text: &str, frames: usize) -> Vec<u8> {
    renderer.set_preset(Some(text)).expect("a preset parses");
    let signal = tone(220.0, 0.9);
    let spectrum = Spectrum::of(&signal, 2);
    let mut last = Vec::new();
    for _ in 0..frames {
        last = renderer
            .draw_to_pixels(&full_frame(&signal, &spectrum))
            .expect("a MilkDrop frame renders");
    }
    last
}

/// A shader block written the way a `.milk` file carries one: numbered lines,
/// each behind the backtick MilkDrop's INI writer puts there.
fn milk_shader_lines(key: &str, body: &str) -> String {
    body.lines()
        .enumerate()
        .map(|(i, line)| format!("{key}_{}=`{line}\n", i + 1))
        .collect()
}

/// A preset whose composite shader is exactly `body`.
fn milk_comp(body: &str) -> String {
    milk_preset(&milk_shader_lines(
        "comp",
        &format!("shader_body\n{{\n{body}\n}}"),
    ))
}

/// A preset that draws only a custom wave: a horizontal white line at the given
/// `y` (0..1), the built-in waveform switched off. `value1` plays no part, so
/// the line's position is exactly what the per-point code wrote -- which is what
/// lets a check pin the `y` orientation.
fn custom_wave_line(y: f32) -> String {
    milk_preset(&format!(
        "fWaveAlpha=0\n\
         wavecode_0_enabled=1\n\
         wavecode_0_samples=64\n\
         wave_0_per_point1=x=sample;\n\
         wave_0_per_point2=y={y};\n\
         wave_0_per_point3=a=1;\n"
    ))
}

/// A preset that draws only a custom shape: an opaque white 4-sided polygon at
/// the centre, of the given radius (0..1). No per-frame code -- it draws at the
/// file's own defaults, which is the shape form with the least to go wrong.
fn custom_square(rad: f32) -> String {
    milk_preset(&format!(
        "fWaveAlpha=0\n\
         shapecode_0_enabled=1\n\
         shapecode_0_sides=4\n\
         shapecode_0_x=0.5\n\
         shapecode_0_y=0.5\n\
         shapecode_0_rad={rad}\n\
         shapecode_0_r=1\nshapecode_0_g=1\nshapecode_0_b=1\nshapecode_0_a=1\n\
         shapecode_0_r2=1\nshapecode_0_g2=1\nshapecode_0_b2=1\nshapecode_0_a2=1\n"
    ))
}

/// The mean of one channel across the frame, 0..255.
fn mean_channel(pixels: &[u8], channel: usize) -> f32 {
    let mut sum = 0.0f32;
    for pixel in pixels.chunks_exact(4) {
        sum += f32::from(pixel[channel]);
    }
    sum / (pixels.len() / 4) as f32
}

/// A solid-colour PNG, for a texture-pack check that has to feed real bytes.
fn solid_png(r: u8, g: u8, b: u8) -> Vec<u8> {
    let mut img = image::RgbaImage::new(4, 4);
    for pixel in img.pixels_mut() {
        *pixel = image::Rgba([r, g, b, 255]);
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// How much a frame varies, as the mean absolute deviation of its brightness.
///
/// The figure that tells a flat fill from a texture: a bound noise volume has
/// a large one and an unbound (black) sampler has none at all.
fn spread(pixels: &[u8]) -> f32 {
    let mean = mean_brightness(pixels);
    let mut sum = 0.0f32;
    for pixel in pixels.chunks_exact(4) {
        sum += (f32::from(pixel[0].max(pixel[1]).max(pixel[2])) - mean).abs();
    }
    sum / (pixels.len() / 4) as f32
}

/// The near-black field every visual draws on, as the bytes a readback sees.
/// Named here because the MilkDrop checks measure light *against* it.
const BACKGROUND_BYTES: [u8; 3] = [5, 5, 8];

fn check_milkdrop() {
    println!("\nMilkDrop -- its own claims:");
    // One renderer for the whole section, presets swapped in place. M6's live
    // run found the web harness answering `E_OUTOFMEMORY` past four renderers
    // per visual, and the same shape applies here with more force: this visual
    // has N presets behind it rather than one scene (decision 58).
    let mut renderer =
        pollster::block_on(OffscreenRenderer::new(WIDTH, HEIGHT, VisualKind::MilkDrop))
            .expect("offscreen renderer for MilkDrop");

    // --- The loop itself: decay is what makes a feedback frame a feedback
    // frame, and it is the one figure every preset in existence depends on. ---
    let held = mean_brightness(&run_preset(&mut renderer, &milk_preset("fDecay=1.0\n"), 60));
    let fading = mean_brightness(&run_preset(&mut renderer, &milk_preset("fDecay=0.5\n"), 60));
    check(
        "a preset that does not decay accumulates its own waveform",
        held > fading * 1.5,
        format!("mean {held:.1} at decay 1.0 against {fading:.1} at 0.5"),
    );

    // --- The four whole-frame filters. Each is a pair of D3D9 blend states in
    // MilkDrop and a line of arithmetic here, and each has an exact
    // consequence: `invert` turns the field itself white. ---
    let inverted = run_preset(&mut renderer, &milk_preset("bInvert=1\nfWaveAlpha=0\n"), 4);
    let corner = &inverted[..3];
    check(
        "bInvert turns the frame's own background inside out",
        corner.iter().all(|c| *c > 200),
        format!("rgb({}, {}, {})", corner[0], corner[1], corner[2]),
    );

    // `darken` squares the picture, so a mid-grey halves and the whole frame
    // gets dimmer -- while `brighten` does the reverse. Measured against the
    // same preset with neither, so the waveform is identical in all three.
    let plain = mean_brightness(&run_preset(&mut renderer, &milk_preset("fDecay=0.9\n"), 30));
    let darker = mean_brightness(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nbDarken=1\n"),
        30,
    ));
    let brighter = mean_brightness(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nbBrighten=1\n"),
        30,
    ));
    check(
        "bDarken and bBrighten bend the frame the two ways round",
        darker < plain && plain < brighter,
        format!("{darker:.1} darkened, {plain:.1} plain, {brighter:.1} brightened"),
    );

    // --- Gamma is a multiply, so twice the gamma is a brighter frame; it is
    // the setting most presets in the wild lean on, and MilkDrop's own default
    // is 2.0 rather than 1.0.
    //
    // Measured as *light* rather than as the frame's mean: a waveform is a thin
    // line on a near-black field, so almost all of the mean is the background
    // and four times the gamma moves it by a quarter. Subtracting the field is
    // what turns that into the figure the setting actually controls. ---
    let field = f32::from(BACKGROUND_BYTES[2]);
    let dim = mean_brightness(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nfGammaAdj=0.5\n"),
        30,
    )) - field;
    let bright = mean_brightness(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nfGammaAdj=2.0\n"),
        30,
    )) - field;
    check(
        "gamma scales the light in the composited frame",
        bright > dim * 1.5,
        format!("{bright:.2} of light at gamma 2.0 against {dim:.2} at 0.5"),
    );

    // --- The borders, which are drawn *into* the feedback buffer and so are
    // part of the loop rather than an overlay on it. ---
    let bordered = run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nfWaveAlpha=0\nob_size=0.05\nob_r=1\nob_g=0\nob_b=0\nob_a=1\n"),
        4,
    );
    let edge = {
        let i = ((HEIGHT / 2 * WIDTH) * 4) as usize; // left edge, mid height
        [bordered[i], bordered[i + 1], bordered[i + 2]]
    };
    let middle = mean_centre(&bordered, 0.2);
    check(
        "an outer border rings the frame in the colour the preset asked for",
        edge[0] > 150 && edge[1] < 80 && edge[2] < 80 && middle < 20.0,
        format!(
            "edge rgb({}, {}, {}), middle {middle:.1}",
            edge[0], edge[1], edge[2]
        ),
    );

    // --- The centre darkening, which exists to stop a zooming loop saturating
    // the one point every frame reads from most often. ---
    //
    // `fWaveParam=-0.45` pulls mode 0's ring in from radius 0.5 to 0.05, so the
    // waveform sweeps through the middle and there is light there to take down.
    // A ring at its usual radius leaves the centre empty, and "darker than
    // nothing" is not a measurement -- which is how this check first read.
    let centred_wave = "fDecay=1.0\nzoom=1.01\nfWaveParam=-0.45\n";
    let undarkened = mean_centre(
        &run_preset(&mut renderer, &milk_preset(centred_wave), 60),
        0.04,
    );
    let darkened = mean_centre(
        &run_preset(
            &mut renderer,
            &milk_preset(&format!("{centred_wave}bDarkenCenter=1\n")),
            60,
        ),
        0.04,
    );
    check(
        "bDarkenCenter takes the middle down",
        darkened < undarkened,
        format!("{darkened:.1} against {undarkened:.1} in the middle twenty-fifth"),
    );

    // --- Video echo: a second copy of the frame at another scale, mixed in.
    // At zoom 2 it reads the middle half, so the echo is the centre magnified
    // and the frame gains light away from where the waveform is. ---
    let no_echo = run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nfVideoEchoAlpha=0\n"),
        20,
    );
    let echoed = run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nfVideoEchoAlpha=0.5\nfVideoEchoZoom=2.0\n"),
        20,
    );
    let ring_without = measure(&no_echo);
    let ring_with = measure(&echoed);
    check(
        "video echo puts a second copy of the picture in the frame",
        ring_with.count > ring_without.count,
        format!(
            "{} lit pixels with the echo, {} without",
            ring_with.count, ring_without.count
        ),
    );

    // --- The waveform's own position, which a preset drives per frame. ---
    let centred = measure(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.0\nwave_x=0.5\nwave_y=0.5\n"),
        4,
    ));
    let moved = measure(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.0\nwave_x=0.75\nwave_y=0.5\n"),
        4,
    ));
    let shift = moved.min_x as i64 - centred.min_x as i64;
    // wave_x runs 0..1 across the frame and 0.5 is the middle, so a quarter of
    // the way over is a quarter of the frame's width.
    let expected = (WIDTH / 4) as i64;
    check(
        "wave_x moves the waveform a quarter of the frame per quarter turn",
        (shift - expected).abs() < 12,
        format!("moved {shift}px, expected about {expected}px"),
    );

    // --- Zoom, which is the loop's other defining motion: reading from a
    // smaller region every frame is what makes a picture grow outward. ---
    let still = measure(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=1.0\nzoom=1.0\n"),
        40,
    ));
    let growing = measure(&run_preset(
        &mut renderer,
        &milk_preset("fDecay=1.0\nzoom=1.03\n"),
        40,
    ));
    check(
        "a zoom above one grows the picture outward frame after frame",
        growing.max_x - growing.min_x > still.max_x - still.min_x,
        format!(
            "{}px wide zooming, {}px wide still",
            growing.max_x - growing.min_x,
            still.max_x - still.min_x
        ),
    );

    // --- A stranger's program, bounded by budget rather than by trust
    // (decision 51). The claim is not that it finishes -- it will not -- but
    // that the frame after it is a frame, and that the rest of the preset is
    // still drawn. ---
    let runaway = run_preset(
        &mut renderer,
        &milk_preset("fDecay=0.9\nper_pixel_1=loop(1000000, zoom = zoom + 0.000001;);\n"),
        8,
    );
    check(
        "a preset whose equations run away still draws its frame",
        mean_brightness(&runaway) > 0.0 && runaway.len() == (WIDTH * HEIGHT * 4) as usize,
        format!("mean {:.1}", mean_brightness(&runaway)),
    );

    // --- Clearing the selection is a state, not a no-op: it goes back to
    // MilkDrop's defaults, which are a real preset and draw a real picture.
    // Anything else would leave a gallery entry looking broken the moment a
    // library is emptied. ---
    renderer.set_preset(None).expect("clearing cannot fail");
    let signal = tone(220.0, 0.9);
    let spectrum = Spectrum::of(&signal, 2);
    let mut defaults = Vec::new();
    for _ in 0..30 {
        defaults = renderer
            .draw_to_pixels(&full_frame(&signal, &spectrum))
            .expect("defaults render");
    }
    check(
        "clearing the preset falls back to MilkDrop's defaults rather than to nothing",
        measure(&defaults).count > 100,
        format!("{} lit pixels", measure(&defaults).count),
    );

    // --- M7b: a preset's own shaders, translated and run. Everything above
    // this point is M7a's fixed-function path; everything below only draws if
    // HLSL became WGSL, compiled, bound and ran. ---

    let painted = run_preset(&mut renderer, &milk_comp("ret = float3(1,0,0);"), 4);
    check(
        "a composite shader paints the colour it asks for",
        mean_channel(&painted, 0) > 200.0 && mean_channel(&painted, 1) < 40.0,
        format!(
            "r {:.0}, g {:.0}, b {:.0}",
            mean_channel(&painted, 0),
            mean_channel(&painted, 1),
            mean_channel(&painted, 2)
        ),
    );

    // The warp shader writes into the feedback buffer rather than to the
    // screen, so this is read through the *fixed-function* composite -- which
    // is what makes it a different claim from the one above.
    let warped = run_preset(
        &mut renderer,
        &milk_preset(&milk_shader_lines(
            "warp",
            "shader_body\n{\nret = float3(0,1,0);\n}",
        )),
        4,
    );
    check(
        "a warp shader owns the feedback buffer",
        mean_channel(&warped, 1) > 150.0 && mean_channel(&warped, 0) < 60.0,
        format!(
            "r {:.0}, g {:.0}",
            mean_channel(&warped, 0),
            mean_channel(&warped, 1)
        ),
    );

    // `q1..q32` are the widest channel between a preset's equations and its
    // shaders, and they run through the uniform block this milestone built.
    let mut through_q = milk_comp("ret = float3(q1, 0, 0);");
    through_q.push_str("per_frame_1=q1 = 1;\n");
    let through_q = run_preset(&mut renderer, &through_q, 4);
    check(
        "a value set in per_frame reaches the shader as q1",
        mean_channel(&through_q, 0) > 200.0,
        format!("r {:.0}", mean_channel(&through_q, 0)),
    );

    // The blur chain: `GetBlur1` is the corpus's second-commonest call, and a
    // build that bound black there would pass every check above.
    let blurred = run_preset(&mut renderer, &milk_comp("ret = GetBlur3(uv)*4;"), 30);
    let sharp = run_preset(&mut renderer, &milk_comp("ret = GetPixel(uv)*4;"), 30);
    let lit = |px: &[u8]| {
        px.chunks_exact(4)
            .filter(|p| p[0].max(p[1]).max(p[2]) > BACKGROUND_BYTES[2] + 12)
            .count()
    };
    check(
        "the blur chain runs, and spreads the picture wider than a plain read",
        lit(&blurred) > lit(&sharp) * 2,
        format!(
            "{} lit through GetBlur3 against {}",
            lit(&blurred),
            lit(&sharp)
        ),
    );

    // The procedural noise volumes -- `tex3D` is 20.7% of the corpus, and
    // every one of those reads a texture this build has to generate.
    let noise = run_preset(
        &mut renderer,
        &milk_comp("ret = tex3D(sampler_noisevol_hq, float3(uv, 0.5)).xyz;"),
        4,
    );
    check(
        "a 3D noise volume is bound and is not flat",
        spread(&noise) > 5.0 && mean_brightness(&noise) > 40.0,
        format!(
            "mean {:.0}, spread {:.1}",
            mean_brightness(&noise),
            spread(&noise)
        ),
    );

    // Per pass, not per preset: 605 presets in the corpus give one pass a
    // shader and leave the other fixed-function, and a composite that will not
    // translate must not cost a preset its warp.
    let mut half = milk_preset(&milk_shader_lines(
        "warp",
        "shader_body\n{\nret = float3(0,1,0);\n}",
    ));
    half.push_str(&milk_shader_lines(
        "comp",
        "shader_body\n{\nret = nosuchfunction(uv);\n}",
    ));
    let half = run_preset(&mut renderer, &half, 4);
    check(
        "a composite that will not translate does not cost the preset its warp",
        mean_channel(&half, 1) > 150.0,
        format!(
            "g {:.0} through the fallback composite",
            mean_channel(&half, 1)
        ),
    );

    // --- M7c: custom waves and shapes, drawn from the preset's own code. ---

    // A custom wave's per-point code owns each point's position; here it writes
    // a horizontal line at a chosen y. y runs 0 at the bottom to 1 at the top,
    // so the line at 0.75 must sit higher (smaller row) than the one at 0.25,
    // by half the frame -- and it pins the vertical orientation in the bargain.
    let low = measure(&run_preset(&mut renderer, &custom_wave_line(0.25), 4));
    let high = measure(&run_preset(&mut renderer, &custom_wave_line(0.75), 4));
    let dy = low.min_y as i64 - high.min_y as i64;
    check(
        "a custom wave draws, and its y is 0 at the bottom, 1 at the top",
        low.count > 200 && high.count > 200 && dy > 100 && dy < 220,
        format!(
            "y=0.25 at row {}, y=0.75 at row {} ({}px apart)",
            low.min_y, high.min_y, dy
        ),
    );

    // value1 is the left channel's sample. A tone spreads a wave that reads it
    // across x; silence leaves it on the centre column.
    let value1 = milk_preset(
        "fWaveAlpha=0\n\
         wavecode_0_enabled=1\n\
         wavecode_0_samples=64\n\
         wave_0_per_point1=x=value1*0.5+0.5;\n\
         wave_0_per_point2=y=sample;\n\
         wave_0_per_point3=a=1;\n",
    );
    let loud = measure(&run_preset(&mut renderer, &value1, 4));
    let quiet_signal = silence();
    let quiet_spectrum = Spectrum::of(&quiet_signal, 2);
    renderer.set_preset(Some(&value1)).expect("a preset parses");
    let mut quiet = Vec::new();
    for _ in 0..4 {
        quiet = renderer
            .draw_to_pixels(&full_frame(&quiet_signal, &quiet_spectrum))
            .expect("a MilkDrop frame renders");
    }
    let quiet = measure(&quiet);
    check(
        "a custom wave's value1 carries the waveform sample",
        (loud.max_x as i64 - loud.min_x as i64) > (quiet.max_x as i64 - quiet.min_x as i64) + 60,
        format!(
            "tone {}px wide, silence {}px wide",
            loud.max_x - loud.min_x,
            quiet.max_x - quiet.min_x
        ),
    );

    // A custom shape draws at its centre and radius: a radius of 0.25 around the
    // centre spans a quarter of the frame on a side.
    let square = measure(&run_preset(&mut renderer, &custom_square(0.25), 4));
    check(
        "a custom shape draws at its centre and radius",
        square.count > 5000
            && (square.min_x as i64 - 80).abs() < 12
            && (square.max_x as i64 - 240).abs() < 12
            && (square.min_y as i64 - 80).abs() < 12
            && (square.max_y as i64 - 240).abs() < 12,
        format!(
            "{}px, x {}..{}, y {}..{}",
            square.count, square.min_x, square.max_x, square.min_y, square.max_y
        ),
    );

    // An `enabled=0` wave or shape is not drawn, and with the built-in waveform
    // off there is nothing else to light the frame.
    let disabled = measure(&run_preset(
        &mut renderer,
        &milk_preset(
            "fWaveAlpha=0\n\
             wavecode_0_enabled=0\n\
             wavecode_0_samples=64\n\
             wave_0_per_point1=x=0.5;\n\
             wave_0_per_point2=y=0.5;\n\
             wave_0_per_point3=a=1;\n\
             shapecode_0_enabled=0\n\
             shapecode_0_sides=4\n\
             shapecode_0_rad=0.25\n",
        ),
        4,
    ));
    check(
        "a disabled wave and shape draw nothing",
        disabled.count == 0,
        format!("{} lit px", disabled.count),
    );

    // --- M7c: texture packs. A named sampler resolves to a layer of the
    // user-texture array, filled from the preset's own folder (decision 59). A
    // composite that samples its own texture paints the whole frame with it. ---
    let textured = milk_preset(&milk_shader_lines(
        "comp",
        "sampler sampler_fw_test;\nshader_body\n{\nret = tex2D(sampler_fw_test, uv).xyz;\n}",
    ));
    renderer
        .set_preset(Some(&textured))
        .expect("a preset parses");
    renderer.set_textures(&[("test".to_string(), solid_png(220, 30, 30))]);
    let signal = tone(220.0, 0.9);
    let spectrum = Spectrum::of(&signal, 2);
    let mut tex = Vec::new();
    for _ in 0..4 {
        tex = renderer
            .draw_to_pixels(&full_frame(&signal, &spectrum))
            .expect("a MilkDrop frame renders");
    }
    check(
        "a texture pack image is sampled from the user-texture array",
        mean_channel(&tex, 0) > 150.0
            && mean_channel(&tex, 1) < 80.0
            && mean_channel(&tex, 2) < 80.0,
        format!(
            "r {:.0} g {:.0} b {:.0}",
            mean_channel(&tex, 0),
            mean_channel(&tex, 1),
            mean_channel(&tex, 2)
        ),
    );
}

fn main() {
    println!(
        "offscreen: {WIDTH}x{HEIGHT}, {RATE} Hz -- {TRACE_SECONDS}s of trace,          {HISTORY_SECONDS}s of spectrogram, {TUNNEL_SECONDS}s of tunnel"
    );
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
    check_the_spectrogram_waterfall();
    check_the_oscilloscope_tunnel();
    check_the_particle_field();
    check_the_raymarched_solid();
    check_milkdrop();
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
