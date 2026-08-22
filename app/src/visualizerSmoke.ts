// Live check of the web build's visualizer host and every visual in it
// (Phase 3 M1, extending Phase 1 M6's scope smoke) -- the web counterpart of
// `cargo run -p player-render --example visual_smoke`, running the same checks
// against the same claims so the two shells can be held to one standard.
// Served at /visualizer.html by the dev server; not part of the production
// bundle.
//
// The native check renders offscreen and reads the texture back; this one
// renders into a real canvas through the browser's own WebGPU and reads that
// back. Different targets, deliberately: what is being compared is the shaders
// and the pipelines, and the interesting failure would be Dawn disagreeing with
// the native backend about them.
//
// No audio engine is involved. The samples are synthesised here and handed
// straight to the renderer, so a failure is the renderer's rather than the
// player's.

import init, {
  WasmAnalyzer,
  WasmRenderer,
  defaultGain,
  defaultVisual,
  gainCatalog,
  visualCatalog,
  visualWindowSamples,
} from "./wasm/player_rt_wasm.js";

const RATE = 48_000;

/// A 60Hz frame, matching the native example -- the frame's `elapsed_seconds`,
/// which the automatic gain's release is timed from.
const FRAME_SECONDS = 1 / 60;

interface VisualInfo {
  id: string;
  name: string;
  description: string;
}

interface GainInfo {
  id: string;
  label: string;
  isMeasurement: boolean;
}

const out = document.getElementById("out") as HTMLPreElement;
const canvas = document.getElementById("scope") as HTMLCanvasElement;
let failures = 0;
let checks = 0;

function log(line: string): void {
  out.textContent += `${line}\n`;
}

function check(label: string, ok: boolean, detail: string): void {
  checks += 1;
  if (!ok) failures += 1;
  log(`  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}`);
}

/// One window's worth of interleaved stereo sine, right channel scaled by
/// `rightGain`. Same signal the native example generates.
function tone(hz: number, rightGain: number, amplitude = 0.8): Float32Array {
  const samples = visualWindowSamples(RATE, 2);
  const frames = samples / 2;
  const out = new Float32Array(samples);
  for (let n = 0; n < frames; n++) {
    const s = amplitude * Math.sin((2 * Math.PI * hz * n) / RATE);
    out[n * 2] = s;
    out[n * 2 + 1] = s * rightGain;
  }
  return out;
}

interface Figure {
  lit: number;
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
  greenNear: number;
  greenFar: number;
}

/// Reads a WebGPU canvas back by snapshotting it into a 2D one -- the only way
/// to get at a WebGPU canvas's pixels from script, and the reason this check
/// draws into a canvas at all rather than a texture.
///
/// The threshold sits well clear of the background, which is a near-black
/// (5, 5, 8) as displayed -- see `attachments::BACKGROUND`, and the 2026-08-18
/// fix that made it the same number on both shells. It was 80 until M2, when
/// the shaders started converting their colours for the surface's colour space
/// too (`visuals/shared.wgsl`): everything on an sRGB surface then showed the
/// colour it was written as rather than a washed-out encoding of it, which is
/// darker. 40 is still five times the brightest channel of the background.
function pixels(target: HTMLCanvasElement): Uint8ClampedArray {
  const snapshot = document.createElement("canvas");
  snapshot.width = target.width;
  snapshot.height = target.height;
  const ctx = snapshot.getContext("2d");
  if (!ctx) throw new Error("no 2D context for the snapshot");
  ctx.drawImage(target, 0, 0);
  return ctx.getImageData(0, 0, snapshot.width, snapshot.height).data;
}

function measure(target: HTMLCanvasElement = canvas): Figure {
  const px = pixels(target);
  const width = target.width;
  const height = target.height;
  const figure: Figure = {
    lit: 0,
    minX: width,
    maxX: 0,
    minY: height,
    maxY: 0,
    greenNear: 0,
    greenFar: 0,
  };
  let near = 0;
  let nearCount = 0;
  let far = 0;
  let farCount = 0;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      if (px[i] <= 40 && px[i + 1] <= 40 && px[i + 2] <= 40) continue;
      figure.lit += 1;
      figure.minX = Math.min(figure.minX, x);
      figure.maxX = Math.max(figure.maxX, x);
      figure.minY = Math.min(figure.minY, y);
      figure.maxY = Math.max(figure.maxY, y);
      // The camera sits to the -x side, so the newest end of the ribbon falls
      // on the right of the image and history recedes to the left.
      if (x >= width / 2) {
        near += px[i + 1];
        nearCount += 1;
      } else {
        far += px[i + 1];
        farCount += 1;
      }
    }
  }
  figure.greenNear = nearCount ? near / nearCount : 0;
  figure.greenFar = farCount ? far / farCount : 0;
  return figure;
}

/// A second canvas, added to the page so a check can compare two renderers.
function extraCanvas(width: number, height: number): HTMLCanvasElement {
  const extra = document.createElement("canvas");
  extra.width = width;
  extra.height = height;
  extra.style.display = "none";
  document.body.appendChild(extra);
  return extra;
}

/// Nothing measured. The state the web build is genuinely in before its shared
/// buffer exists, and what every vectorscope check passes: that visual reads
/// samples and no spectrum at all.
const NO_SPECTRUM = new Float32Array(0);

/// The metering spectrum of some audio, measured by the same Rust the worker
/// runs (`WasmAnalyzer` wraps `player_core::Meters`). The native check builds
/// this from `Meters` directly, so the two halves are handing their bars the
/// same numbers -- which is what makes "figure for figure" mean anything here.
function spectrumOf(samples: Float32Array, channels: number): SpectrumFeed {
  const analyzer = WasmAnalyzer.create(RATE, channels);
  analyzer.ingest(samples, false);
  const levels = analyzer.update(FRAME_SECONDS);
  const centres = analyzer.bandCentres();
  analyzer.free();
  return { levels, centres };
}

/// A row held at one level across every band -- for the checks that are about
/// the display rather than about the analysis.
function flatSpectrum(levelDb: number): SpectrumFeed {
  const analyzer = WasmAnalyzer.create(RATE, 2);
  const centres = analyzer.bandCentres();
  analyzer.free();
  return { levels: new Float32Array(centres.length).fill(levelDb), centres };
}

interface SpectrumFeed {
  levels: Float32Array;
  centres: Float32Array;
}

/// How tall each column of the image is drawn, as the topmost lit row in it --
/// `null` for a column with nothing in it.
///
/// The measurement the bars are checked through, for the reason the native
/// check gives at greater length: a brightness threshold cannot separate a dim
/// bar from an octave mark, where height separates them completely.
function columnTops(target: HTMLCanvasElement): (number | null)[] {
  const px = pixels(target);
  const tops: (number | null)[] = new Array(target.width).fill(null);
  for (let y = 0; y < target.height; y++) {
    for (let x = 0; x < target.width; x++) {
      const i = (y * target.width + x) * 4;
      if (tops[x] !== null) continue;
      if (px[i] > 40 || px[i + 1] > 40 || px[i + 2] > 40) tops[x] = y;
    }
  }
  return tops;
}

/// The x of the tallest column and how high it reaches, or null for an empty
/// image.
function tallestColumn(target: HTMLCanvasElement): { x: number; y: number } | null {
  let best: { x: number; y: number } | null = null;
  columnTops(target).forEach((top, x) => {
    if (top === null) return;
    if (best === null || top < best.y) best = { x, y: top };
  });
  return best;
}

/// The highest row anything reaches. Smaller is taller: row 0 is the top.
function topRow(target: HTMLCanvasElement): number | null {
  return tallestColumn(target)?.y ?? null;
}

/// Wider than tall, because a row of bars is -- the native check's own shape,
/// so the two report comparable pixel figures.
const BARS_WIDTH = 640;
const BARS_HEIGHT = 360;

/// A bars renderer with no history, on a canvas of its own.
///
/// Most of these checks need one: the peak caps survive between frames on
/// purpose (decision 11), so a shot drawn on a renderer that has already seen a
/// loud frame measures the leftover cap rather than the bar under it. The
/// native check found that the hard way.
async function barsRenderer(): Promise<{ renderer: WasmRenderer; surface: HTMLCanvasElement }> {
  const surface = extraCanvas(BARS_WIDTH, BARS_HEIGHT);
  const renderer = await WasmRenderer.create(
    surface,
    surface.width,
    surface.height,
    "spectrum-bars",
  );
  return { renderer, surface };
}

/// What only the spectrum bars claim (Phase 3 M2) -- the same claims the native
/// example makes, in the same order.
async function checkTheSpectrumBars(): Promise<void> {
  log("\nSpectrum bars -- its own claims:");

  // Decision 10: a frame with audio but no measurement is not silence, it is
  // "nothing has been measured yet" -- which is exactly this shell's first
  // moments of a session, before the worker has published anything. Sixty-four
  // bars resting on the floor would be a picture of the wrong thing.
  {
    const { renderer, surface } = await barsRenderer();
    renderer.draw_frame(tone(1000, 1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
    check(
      "audio with nothing measured yet draws nothing at all",
      measure(surface).lit === 0,
      `${measure(surface).lit} lit pixels`,
    );
  }

  // The axis, and the check that catches a display drawn backwards. Both tones
  // go through the real analyser, so what is checked is the whole path: audio,
  // player-core's bands, the frame, the instances, the pixels.
  const low = tone(80, 1);
  const lowSpectrum = spectrumOf(low, 2);
  const lowShot = await barsRenderer();
  lowShot.renderer.draw_frame(
    low,
    2,
    RATE,
    FRAME_SECONDS,
    lowSpectrum.levels,
    lowSpectrum.centres,
  );
  const lowBar = tallestColumn(lowShot.surface);

  const high = tone(6000, 1);
  const highSpectrum = spectrumOf(high, 2);
  const highShot = await barsRenderer();
  highShot.renderer.draw_frame(
    high,
    2,
    RATE,
    FRAME_SECONDS,
    highSpectrum.levels,
    highSpectrum.centres,
  );
  const highBar = tallestColumn(highShot.surface);
  check(
    "a tone lights a bar on the side of the display its frequency belongs to",
    lowBar !== null &&
      highBar !== null &&
      lowBar.x < BARS_WIDTH / 2 &&
      highBar.x > BARS_WIDTH / 2 &&
      lowBar.x < highBar.x,
    `80Hz at x=${lowBar?.x}, 6kHz at x=${highBar?.x}, of ${BARS_WIDTH}`,
  );

  // Level, which is the other half of what a spectrum display says. Three
  // amplitudes twenty dB apart, through the real analyser.
  const heights: number[] = [];
  for (const amplitude of [0.8, 0.08, 0.008]) {
    const samples = tone(1000, 1, amplitude);
    const spectrum = spectrumOf(samples, 2);
    const shot = await barsRenderer();
    shot.renderer.draw_frame(
      samples,
      2,
      RATE,
      FRAME_SECONDS,
      spectrum.levels,
      spectrum.centres,
    );
    heights.push(topRow(shot.surface) ?? BARS_HEIGHT);
  }
  check(
    "a louder tone draws a taller bar",
    heights[0] < heights[1] && heights[1] < heights[2],
    `tops at row ${heights[0]}, ${heights[1]}, ${heights[2]} for full scale, -20dB and -40dB`,
  );

  // Decision 13: the display's floor is -72dBFS, not the meter's -120, so a row
  // at the meter's floor has no bars in it -- only the octave marks, which lie
  // on the floor of the scene.
  const floor = flatSpectrum(-120);
  {
    const { renderer, surface } = await barsRenderer();
    renderer.draw_frame(NO_SPECTRUM, 2, RATE, FRAME_SECONDS, floor.levels, floor.centres);
    const floorTop = topRow(surface);
    check(
      "a row at the meter's floor draws no bars, only the octave marks",
      floorTop === null || floorTop > BARS_HEIGHT / 2,
      `tallest column reaches row ${floorTop} of ${BARS_HEIGHT}`,
    );
  }

  // Full scale reaches near the top of the frame without touching it -- the
  // frame is pinned to a full-scale display, exactly as the vectorscope's is.
  const loud = flatSpectrum(0);
  const falling = await barsRenderer();
  falling.renderer.draw_frame(NO_SPECTRUM, 2, RATE, FRAME_SECONDS, loud.levels, loud.centres);
  const fullTop = topRow(falling.surface) ?? BARS_HEIGHT;
  check(
    "a full-scale row reaches near the top of the frame without clipping",
    fullTop > 0 && fullTop < BARS_HEIGHT / 4,
    `top at row ${fullTop} of ${BARS_HEIGHT}`,
  );

  // Decision 11: the caps are display state, and they fall in *seconds* -- the
  // property that makes this shell's animation callbacks and the desktop's
  // 60Hz redraw thread draw the same thing. Checked the way the automatic
  // gain's release was: one long frame against many short ones.
  falling.renderer.draw_frame(NO_SPECTRUM, 2, RATE, 1.0, floor.levels, floor.centres);
  const afterOne = topRow(falling.surface) ?? BARS_HEIGHT;
  check(
    "a peak cap outlives the bar under it and falls from where it reached",
    afterOne > fullTop,
    `cap at row ${afterOne}, having peaked at row ${fullTop}`,
  );

  const stepwise = await barsRenderer();
  stepwise.renderer.draw_frame(NO_SPECTRUM, 2, RATE, FRAME_SECONDS, loud.levels, loud.centres);
  for (let n = 0; n < 60; n++) {
    stepwise.renderer.draw_frame(NO_SPECTRUM, 2, RATE, 1 / 60, floor.levels, floor.centres);
  }
  stepwise.renderer.draw_frame(NO_SPECTRUM, 2, RATE, 0, floor.levels, floor.centres);
  const stepwiseTop = topRow(stepwise.surface) ?? BARS_HEIGHT;
  check(
    "the caps fall at the same rate whatever the frame rate",
    Math.abs(afterOne - stepwiseTop) <= 2,
    `row ${afterOne} after one long frame against ${stepwiseTop} after sixty short ones`,
  );
}

/// The waterfall is looked at in the same shape the bars are, and the same
/// shape the native check uses -- so the two report comparable pixel figures.
const FALL_WIDTH = 640;
const FALL_HEIGHT = 360;

/// A waterfall renderer with no history at all, on a canvas of its own.
///
/// The bars needed a fresh renderer because their caps outlive a frame; this
/// needs one far more, since history is the whole visual.
async function waterfallRenderer(): Promise<{
  renderer: WasmRenderer;
  surface: HTMLCanvasElement;
}> {
  const surface = extraCanvas(FALL_WIDTH, FALL_HEIGHT);
  const renderer = await WasmRenderer.create(
    surface,
    surface.width,
    surface.height,
    "spectrogram-waterfall",
  );
  return { renderer, surface };
}

/// A pixel from the loud end of the colour ramp -- green-dominant, where the
/// quiet end is violet.
///
/// How a ridge is found among the terrain it stands in. A brightness threshold
/// could not: the whole surface is drawn, floor included, so "something is lit
/// here" is true of every column.
function isLoud(px: Uint8ClampedArray, i: number): boolean {
  return px[i + 1] > 70 && px[i + 1] > px[i] + 25;
}

/// Where the loud material is: how much of it there is, and its centre of mass
/// down and across the image.
function ridge(
  target: HTMLCanvasElement,
): { count: number; centreRow: number; centreColumn: number } | null {
  const px = pixels(target);
  let count = 0;
  let rows = 0;
  let columns = 0;
  for (let y = 0; y < target.height; y++) {
    for (let x = 0; x < target.width; x++) {
      const i = (y * target.width + x) * 4;
      if (isLoud(px, i)) {
        count++;
        rows += y;
        columns += x;
      }
    }
  }
  return count > 0 ? { count, centreRow: rows / count, centreColumn: columns / count } : null;
}

/// Where the loudest thing on a spectrum display is, across the image.
///
/// The topmost *loud* pixel rather than the topmost lit one, which is what
/// makes this the one measurement both spectrum visuals can be asked for: the
/// waterfall draws a floor and octave rails that reach further up the screen
/// than any ridge does, being further away.
function loudPeakColumn(target: HTMLCanvasElement): number | null {
  const px = pixels(target);
  for (let y = 0; y < target.height; y++) {
    let count = 0;
    let sum = 0;
    for (let x = 0; x < target.width; x++) {
      const i = (y * target.width + x) * 4;
      if (isLoud(px, i)) {
        count++;
        sum += x;
      }
    }
    if (count > 0) return sum / count;
  }
  return null;
}

/// A row of measured columns, for a check's own report.
function columns(xs: number[]): string {
  return xs.map((x) => x.toFixed(0)).join("/");
}

/// The same measurement against a different frequency axis -- every `step` band
/// of it. What this shell's first play looks like from a visual's side
/// (nothing, then sixty-four bands).
function rebanded(source: SpectrumFeed, bands: number): SpectrumFeed {
  const step = Math.max(1, Math.floor(source.levels.length / Math.max(1, bands)));
  const levels: number[] = [];
  const centres: number[] = [];
  for (let i = 0; i < source.levels.length; i += step) {
    levels.push(source.levels[i]);
    centres.push(source.centres[i]);
  }
  return { levels: new Float32Array(levels), centres: new Float32Array(centres) };
}

/// Plays a spectrum at the visual for `seconds`, one 60Hz frame at a time --
/// which is how this shell's animation callbacks feed it, and how history
/// actually accumulates.
function play(renderer: WasmRenderer, spectrum: SpectrumFeed, seconds: number): void {
  const frames = Math.max(1, Math.round(seconds / FRAME_SECONDS));
  for (let n = 0; n < frames; n++) {
    renderer.draw_frame(
      NO_SPECTRUM,
      2,
      RATE,
      FRAME_SECONDS,
      spectrum.levels,
      spectrum.centres,
    );
  }
}

/// What only the spectrogram waterfall claims (Phase 3 M3) -- the same claims
/// the native example makes, in the same order.
async function checkTheSpectrogramWaterfall(): Promise<void> {
  log("\nSpectrogram waterfall -- its own claims:");

  // Decision 21, and M2 decision 10's rule: nothing measured is nothing drawn.
  // Not a flat floor stretching into the distance, which would be a picture of
  // six seconds of silence that was never listened to.
  {
    const { renderer, surface } = await waterfallRenderer();
    renderer.draw_frame(tone(1000, 1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
    check(
      "audio with nothing measured yet draws nothing at all",
      measure(surface).lit === 0,
      `${measure(surface).lit} lit pixels`,
    );
  }

  const low = tone(80, 1);
  const lowSpectrum = spectrumOf(low, 2);
  const high = tone(6000, 1);
  const highSpectrum = spectrumOf(high, 2);

  const lowShot = await waterfallRenderer();
  play(lowShot.renderer, lowSpectrum, 0.5);
  const lowRidge = ridge(lowShot.surface);
  const highShot = await waterfallRenderer();
  play(highShot.renderer, highSpectrum, 0.5);
  const highRidge = ridge(highShot.surface);
  check(
    "a tone raises a ridge on the side of the display its frequency belongs to",
    lowRidge !== null &&
      highRidge !== null &&
      lowRidge.centreColumn < FALL_WIDTH / 2 &&
      highRidge.centreColumn > FALL_WIDTH / 2,
    `80Hz at x=${lowRidge?.centreColumn.toFixed(0)}, 6kHz at x=${highRidge?.centreColumn.toFixed(0)}, of ${FALL_WIDTH}`,
  );

  // And the two spectrum visuals agree about where a tone belongs.
  //
  // Not to the pixel, and not even in proportion: each scene is seen from its
  // own three-quarter view, and an off-axis camera does not map world x to
  // screen x linearly. What has to agree, and what a person actually reads, is
  // the *order* -- four tones have to march left to right in both, because both
  // place a band with `scale::slot_centre` off the same measured axis.
  const ladder = [80, 400, 2000, 6000];
  const onBars: number[] = [];
  const onWaterfall: number[] = [];
  for (const hz of ladder) {
    const samples = tone(hz, 1);
    const spectrum = spectrumOf(samples, 2);
    const bars = await barsRenderer();
    bars.renderer.draw_frame(samples, 2, RATE, FRAME_SECONDS, spectrum.levels, spectrum.centres);
    onBars.push(loudPeakColumn(bars.surface) ?? -1);
    // The waterfall's newest measurement, at its near edge: one row of history
    // draws the profile of that row and nothing else.
    const fall = await waterfallRenderer();
    fall.renderer.draw_frame(
      NO_SPECTRUM,
      2,
      RATE,
      FRAME_SECONDS,
      spectrum.levels,
      spectrum.centres,
    );
    onWaterfall.push(loudPeakColumn(fall.surface) ?? -1);
  }
  const rising = (xs: number[]) => xs.every((x, i) => i === 0 || xs[i - 1] < x);
  check(
    "the bars and the waterfall put a rising set of tones in the same order",
    rising(onBars) && rising(onWaterfall),
    `${ladder.join(", ")} Hz at ${columns(onBars)} on the bars and ${columns(onWaterfall)} on the waterfall`,
  );

  // The claim no visual in this project could make before M3: what was played
  // is still on screen after it stops.
  const silence = flatSpectrum(-120);
  const recording = await waterfallRenderer();
  play(recording.renderer, lowSpectrum, 1.0);
  const freshRidge = ridge(recording.surface);
  play(recording.renderer, silence, 1.0);
  const aged = ridge(recording.surface);
  check(
    "a ridge outlives the sound that made it",
    aged !== null,
    aged === null
      ? "nothing left a second after the tone stopped"
      : `${aged.count} loud pixels a second after the tone stopped`,
  );
  check(
    "and has travelled away from the viewer",
    aged !== null && freshRidge !== null && aged.centreRow < freshRidge.centreRow,
    `centre at row ${aged?.centreRow.toFixed(0)}, having been at row ${freshRidge?.centreRow.toFixed(0)}`,
  );

  // Decision 17: the history scrolls on a clock, so the same second of silence
  // has to leave the ridge in the same place whether it arrived as sixty frames
  // or as one. This is the check that would have caught a row per frame -- the
  // mistake that scrolls at whatever this display's refresh rate happens to be.
  const inOneGo = await waterfallRenderer();
  play(inOneGo.renderer, lowSpectrum, 1.0);
  inOneGo.renderer.draw_frame(NO_SPECTRUM, 2, RATE, 1.0, silence.levels, silence.centres);
  const longFrame = ridge(inOneGo.surface);
  check(
    "the history scrolls at the same rate whatever the frame rate",
    longFrame !== null && aged !== null && Math.abs(longFrame.centreRow - aged.centreRow) <= 2,
    `centre at row ${longFrame?.centreRow.toFixed(0)} after one long frame against ${aged?.centreRow.toFixed(0)} after sixty short ones`,
  );

  // The ring wraps rather than wrapping *around*: the tone is still there
  // partway through the history and gone past the end of it, and it has not
  // reappeared at the front, which is what an off-by-one in the ring's
  // arithmetic would draw.
  const forgetting = await waterfallRenderer();
  play(forgetting.renderer, lowSpectrum, 0.5);
  play(forgetting.renderer, silence, 3.0);
  const remembered = ridge(forgetting.surface);
  play(forgetting.renderer, silence, 5.0);
  const forgotten = ridge(forgetting.surface);
  check(
    "history is as long as it claims to be, and no longer",
    remembered !== null && forgotten === null,
    `${remembered?.count ?? 0} loud pixels 3s after the tone, ${forgotten?.count ?? 0} at 8s`,
  );

  // Decision 18: the texture holds dBFS as measured, and the display window and
  // gain are applied when it is drawn. So turning gain up re-maps rows written
  // long before the setting changed -- a frozen history, drawn twice at two
  // gains, has to draw two different pictures. Had the levels been normalised
  // on the way in, these two would be identical.
  const quiet = tone(80, 1, 0.016);
  const quietSpectrum = spectrumOf(quiet, 2);
  const gained = await waterfallRenderer();
  play(gained.renderer, quietSpectrum, 1.0);
  gained.renderer.draw_frame(NO_SPECTRUM, 2, RATE, 0, quietSpectrum.levels, quietSpectrum.centres);
  const unityLoud = ridge(gained.surface)?.count ?? 0;
  gained.renderer.setGain("4");
  gained.renderer.draw_frame(NO_SPECTRUM, 2, RATE, 0, quietSpectrum.levels, quietSpectrum.centres);
  const fourLoud = ridge(gained.surface)?.count ?? 0;
  gained.renderer.setGain("1");
  check(
    "display gain lifts the whole terrain, history included",
    fourLoud > unityLoud,
    `${fourLoud} loud pixels at x4 against ${unityLoud} at unity, over a history that gained no rows`,
  );

  // Decision 21 again, at the other end: a feed that goes empty mid-session --
  // this shell between tracks -- freezes the terrain rather than scrolling six
  // seconds of real history out behind a wall of silence nobody measured.
  const interrupted = await waterfallRenderer();
  play(interrupted.renderer, lowSpectrum, 1.0);
  interrupted.renderer.draw_frame(NO_SPECTRUM, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const first = pixels(interrupted.surface);
  interrupted.renderer.draw_frame(NO_SPECTRUM, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const second = pixels(interrupted.surface);
  let differing = 0;
  for (let i = 0; i < first.length; i++) if (first[i] !== second[i]) differing++;
  const held = measure(interrupted.surface).lit;
  check(
    "a feed that stops holds the terrain still instead of scrolling",
    differing === 0 && held > 100,
    `${differing} of ${first.length} bytes differ across two empty frames, over ${held} lit pixels`,
  );

  // Decision 22: the one thing that does clear the history is the frequency
  // axis moving under it, because rows measured against a different axis are
  // not comparable to the ones replacing them. What it draws afterwards is what
  // a renderer that never saw the old axis draws.
  const relaid = await waterfallRenderer();
  play(relaid.renderer, lowSpectrum, 1.0);
  const narrow = rebanded(lowSpectrum, 32);
  relaid.renderer.draw_frame(NO_SPECTRUM, 2, RATE, FRAME_SECONDS, narrow.levels, narrow.centres);
  const relaidPixels = pixels(relaid.surface);
  const scratch = await waterfallRenderer();
  scratch.renderer.draw_frame(NO_SPECTRUM, 2, RATE, FRAME_SECONDS, narrow.levels, narrow.centres);
  const scratchPixels = pixels(scratch.surface);
  let apart = 0;
  for (let i = 0; i < relaidPixels.length; i++) {
    if (relaidPixels[i] !== scratchPixels[i]) apart++;
  }
  check(
    "a change of band count starts the history again",
    apart === 0,
    `${apart} of ${relaidPixels.length} bytes differ from a renderer that never saw the old axis`,
  );
}

/// The tunnel is looked at in the same shape the other two are, and the same
/// shape the native check uses.
const TUBE_WIDTH = 640;
const TUBE_HEIGHT = 360;

/// A tunnel renderer with no history at all, on a canvas of its own.
async function tunnelRenderer(
  width: number,
  height: number,
): Promise<{ renderer: WasmRenderer; surface: HTMLCanvasElement }> {
  const surface = extraCanvas(width, height);
  const renderer = await WasmRenderer.create(
    surface,
    surface.width,
    surface.height,
    "oscilloscope-tunnel",
  );
  return { renderer, surface };
}

/// Flies the tunnel for `seconds` on a 60Hz feed -- which is how this shell's
/// animation callbacks feed it, and how the tube actually accumulates.
function fly(renderer: WasmRenderer, samples: Float32Array, seconds: number): void {
  const frames = Math.max(1, Math.round(seconds / FRAME_SECONDS));
  for (let n = 0; n < frames; n++) {
    renderer.draw_frame(samples, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  }
}

/// Measured silence: samples that were played and were zero, which is a
/// different thing from an empty window (nothing played at all) and has to draw
/// differently -- rings at rest rather than no rings.
function silence(): Float32Array {
  return new Float32Array(tone(1, 1).length);
}

/// How many pixels in the frame's outer border are background rather than
/// tunnel wall -- the pixel half of decision 24.
///
/// The border and not the whole image, deliberately: the middle of the frame is
/// the opening at the far end of the tube, which fades to the background by
/// design. What may never happen is seeing *past* the tube at the edges.
function borderHoles(target: HTMLCanvasElement): number {
  const px = pixels(target);
  const band = Math.max(2, Math.floor(Math.min(target.width, target.height) / 16));
  let holes = 0;
  for (let y = 0; y < target.height; y++) {
    for (let x = 0; x < target.width; x++) {
      const edge =
        x < band || y < band || x + band >= target.width || y + band >= target.height;
      if (!edge) continue;
      const i = (y * target.width + x) * 4;
      if (!(px[i] > 40 || px[i + 1] > 40 || px[i + 2] > 40)) holes += 1;
    }
  }
  return holes;
}

/// How far up the colour ramp the picture sits, on average: the mean amount by
/// which green leads red across everything lit.
///
/// The tunnel's "how loud is this" measurement. Counting *loud* pixels cannot
/// answer it at quiet levels -- a third of full scale lands halfway up a
/// violet-to-green ramp, where neither colour is dominant -- and greenness is
/// continuous where a threshold is a step.
function rampPosition(target: HTMLCanvasElement): number {
  const px = pixels(target);
  let sum = 0;
  let count = 0;
  for (let i = 0; i < px.length; i += 4) {
    if (px[i] > 40 || px[i + 1] > 40 || px[i + 2] > 40) {
      sum += px[i + 1] - px[i];
      count += 1;
    }
  }
  return count === 0 ? 0 : sum / count;
}

/// What only the oscilloscope tunnel claims (Phase 3 M4) -- the same claims the
/// native example makes, in the same order.
async function checkTheOscilloscopeTunnel(): Promise<void> {
  log("\nOscilloscope tunnel -- its own claims:");

  // Nothing played is nothing drawn -- the vectorscope's answer to an empty
  // window, which this visual reads the same feed as.
  {
    const { renderer, surface } = await tunnelRenderer(TUBE_WIDTH, TUBE_HEIGHT);
    renderer.draw_frame(new Float32Array(0), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
    check(
      "nothing played draws nothing at all",
      measure(surface).lit === 0,
      `${measure(surface).lit} lit pixels`,
    );
  }

  // Decision 24, in pixels: the tube encloses the frame. A tube too short for a
  // window's corners is the failure this catches, so it is asked at the same
  // shapes the other three visuals' cameras are pinned at.
  const signal = tone(220, 0.6);
  const shapes: [number, number][] = [
    [320, 320],
    [480, 480],
    [800, 600],
    [1280, 720],
    [1920, 1080],
    [3440, 1440],
    [600, 900],
  ];
  let worst = { holes: 0, shape: shapes[0] };
  for (const shape of shapes) {
    const { renderer, surface } = await tunnelRenderer(shape[0], shape[1]);
    fly(renderer, signal, 0.2);
    const holes = borderHoles(surface);
    if (holes >= worst.holes) worst = { holes, shape };
  }
  check(
    "the tube encloses the frame at every window shape",
    worst.holes === 0,
    `${worst.holes} background pixels in the border across ${shapes.length} shapes, worst at ${worst.shape[0]}x${worst.shape[1]}`,
  );

  // Level, which is what makes this a display of the audio rather than a
  // screensaver: a louder passage pushes the wall further, and the colour ramp
  // says so.
  const levels: number[] = [];
  for (const amplitude of [1.0, 0.3, 0.05]) {
    const samples = tone(220, 0.6, 0.8 * amplitude);
    const { renderer, surface } = await tunnelRenderer(TUBE_WIDTH, TUBE_HEIGHT);
    fly(renderer, samples, 0.3);
    levels.push(rampPosition(surface));
  }
  check(
    "a louder passage pushes the wall further",
    levels[0] > levels[1] && levels[1] > levels[2],
    `${levels[0].toFixed(1)}, ${levels[1].toFixed(1)}, ${levels[2].toFixed(1)} up the colour ramp at full scale, a third and a twentieth`,
  );

  // Decision 17's discipline, inherited through the shared ring: the tunnel
  // flies at the same speed whatever the frame rate -- which on this shell is
  // whatever the display happens to run at.
  const quiet = silence();
  const stepwise = await tunnelRenderer(TUBE_WIDTH, TUBE_HEIGHT);
  fly(stepwise.renderer, signal, 0.2);
  fly(stepwise.renderer, quiet, 1.0);
  const stepwiseRidge = ridge(stepwise.surface)?.centreRow ?? 0;
  const oneGo = await tunnelRenderer(TUBE_WIDTH, TUBE_HEIGHT);
  fly(oneGo.renderer, signal, 0.2);
  oneGo.renderer.draw_frame(quiet, 2, RATE, 1.0, NO_SPECTRUM, NO_SPECTRUM);
  const onceRidge = ridge(oneGo.surface)?.centreRow ?? 0;
  check(
    "the tunnel flies at the same speed whatever the frame rate",
    Math.abs(stepwiseRidge - onceRidge) <= 2,
    `the loud rings are centred on row ${onceRidge.toFixed(0)} after one long frame against ${stepwiseRidge.toFixed(0)} after sixty short ones`,
  );

  // The tube is exactly as long as it says: a burst is still in it partway
  // along and gone past the end, and it has not reappeared at the near end,
  // which is what an off-by-one in the ring would draw.
  const passing = await tunnelRenderer(TUBE_WIDTH, TUBE_HEIGHT);
  fly(passing.renderer, signal, 0.3);
  fly(passing.renderer, quiet, 2.0);
  const stillThere = ridge(passing.surface)?.count ?? 0;
  fly(passing.renderer, quiet, 4.0);
  const gone = ridge(passing.surface)?.count ?? 0;
  check(
    "the tunnel is as long as it says it is, and no longer",
    stillThere > 0 && gone === 0,
    `${stillThere} loud pixels 2s after the burst, ${gone} at 6s`,
  );

  // Decision 21's rule, the house rule for a visual with history: a feed that
  // stops holds the tube still rather than flying on through rings nobody
  // played.
  const interrupted = await tunnelRenderer(TUBE_WIDTH, TUBE_HEIGHT);
  fly(interrupted.renderer, signal, 0.5);
  interrupted.renderer.draw_frame(
    new Float32Array(0),
    2,
    RATE,
    FRAME_SECONDS,
    NO_SPECTRUM,
    NO_SPECTRUM,
  );
  const first = pixels(interrupted.surface);
  interrupted.renderer.draw_frame(
    new Float32Array(0),
    2,
    RATE,
    FRAME_SECONDS,
    NO_SPECTRUM,
    NO_SPECTRUM,
  );
  const second = pixels(interrupted.surface);
  let differing = 0;
  for (let i = 0; i < first.length; i++) if (first[i] !== second[i]) differing += 1;
  const held = measure(interrupted.surface).lit;
  check(
    "a feed that stops holds the tunnel still instead of flying on",
    differing === 0 && held > 100,
    `${differing} of ${first.length} bytes differ across two empty frames, over ${held} lit pixels`,
  );

  // Decision 28: display gain is a multiplier here, and the clamp that keeps
  // the wall off the eye survives it. At x8 on a full-scale signal the wall
  // must push visibly further *and* still leave a tunnel to look down.
  const gained = await tunnelRenderer(TUBE_WIDTH, TUBE_HEIGHT);
  fly(gained.renderer, signal, 0.3);
  const unityLoud = ridge(gained.surface)?.count ?? 0;
  gained.renderer.setGain("8");
  fly(gained.renderer, signal, 0.3);
  const eightLoud = ridge(gained.surface)?.count ?? 0;
  const downTheTunnel = TUBE_WIDTH * TUBE_HEIGHT - measure(gained.surface).lit;
  gained.renderer.setGain("1");
  check(
    "gain widens the wall without closing the tube on the eye",
    eightLoud > unityLoud && downTheTunnel > 0,
    `${eightLoud} loud pixels at x8 against ${unityLoud} at unity, with ${downTheTunnel} of ${TUBE_WIDTH * TUBE_HEIGHT} pixels still down the tunnel`,
  );
}

/// The contract the *host* depends on, asked of every visual in the registry --
/// The particle field's checks all run on **one** renderer, reset between them
/// by asking for the same visual again -- which rebuilds the scene and leaves
/// the surface alone, exactly as decision 5 says a swap does.
///
/// That is not tidiness. Each `WasmRenderer` holds a real GPU device, and the
/// first version of this section asked Dawn for nine of them: it answered
/// `E_OUTOFMEMORY` on the D3D12 command queue and the section could not run at
/// all (2026-08-20). Freeing them as it went did not help, so the fix is not to
/// ask.
function resetField(renderer: WasmRenderer): void {
  renderer.setVisual("particle-field");
}

/// What the music is doing, in `MusicalSnapshot::to_array`'s order: a firm 120
/// BPM at `phase`, with `onset` as the strongest since the last drain.
function musicalAt(phase: number, onset: number, centroidHz: number): Float32Array {
  return new Float32Array([120, 0.9, phase, onset, 0.5, centroidHz]);
}

/// Runs the field for `seconds` of 60Hz frames over a held feed.
///
/// The beat and the onsets are synthesised the way the analysis publishes them
/// -- a tempo throughout, and an onset standing in the snapshot for the frame
/// after each beat (M5 decision 37) -- and identically to the native half, which
/// is what makes the two comparable figure for figure.
function spin(
  renderer: WasmRenderer,
  samples: Float32Array,
  seconds: number,
  beats: boolean,
  centroidHz: number,
): void {
  const frames = Math.round(seconds / FRAME_SECONDS);
  for (let n = 0; n < frames; n++) {
    const elapsed = n * FRAME_SECONDS;
    let musical: Float32Array;
    if (beats) {
      const phase = (elapsed * 2) % 1;
      const onset = phase < FRAME_SECONDS * 2 ? 0.9 : 0;
      musical = musicalAt(phase, onset, centroidHz);
    } else {
      musical = new Float32Array([0, 0, 0, 0, 0, centroidHz]);
    }
    renderer.draw_frame(samples, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM, musical);
  }
}

/// The mean colour of everything drawn -- how a field of thousands of sparks is
/// asked what colour it is.
function meanColour(target: HTMLCanvasElement): [number, number, number] {
  const px = pixels(target);
  let count = 0;
  const sums = [0, 0, 0];
  for (let i = 0; i < px.length; i += 4) {
    if (px[i] <= 40 && px[i + 1] <= 40 && px[i + 2] <= 40) continue;
    count += 1;
    for (let c = 0; c < 3; c++) sums[c] += px[i + c];
  }
  const n = Math.max(count, 1);
  return [sums[0] / n, sums[1] / n, sums[2] / n];
}

function differingBytes(a: Uint8ClampedArray, b: Uint8ClampedArray): number {
  let differing = 0;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) differing += 1;
  return differing;
}

/// What only the particle field claims (Phase 3 M6) -- the same claims, in the
/// same order, as the native half's.
async function checkTheParticleField(): Promise<void> {
  log("\nParticle field -- its own claims:");
  const signal = tone(220, 0.6);
  const surface = extraCanvas(320, 320);
  const renderer = await WasmRenderer.create(
    surface,
    surface.width,
    surface.height,
    "particle-field",
  );
  const plain = (samples: Float32Array, elapsed = FRAME_SECONDS) =>
    renderer.draw_frame(samples, 2, RATE, elapsed, NO_SPECTRUM, NO_SPECTRUM);

  // Nothing measured is nothing drawn -- and for this visual it also means the
  // simulation never ran, so there is no field to age.
  plain(new Float32Array(0));
  check(
    "nothing played draws nothing",
    measure(surface).lit === 0,
    `${measure(surface).lit} lit pixels`,
  );

  // Silence is measured and still draws nothing, which is a different claim:
  // the clock runs, the simulation steps, and nothing is spawned because there
  // is nothing to spawn one for.
  spin(renderer, silence(), 1.0, false, 0);
  check(
    "silence runs the simulation and still draws nothing",
    measure(surface).lit === 0,
    `${measure(surface).lit} lit pixels after a second of silence`,
  );

  // Audio fills the field: a disc seen from above its own plane, so it is wider
  // than it is tall -- which is also the check that the camera is where this
  // visual thinks it is.
  spin(renderer, signal, 2.0, false, 1200);
  const lit = measure(surface);
  const litWidth = lit.maxX - lit.minX + 1;
  const litHeight = lit.maxY - lit.minY + 1;
  check(
    "audio fills a disc wider than it is tall",
    lit.lit > 2000 && litWidth > litHeight,
    `${lit.lit} lit pixels, ${litWidth}px wide by ${litHeight}px tall`,
  );

  // The compute pass ran. Nothing else in this crate asserts *motion without
  // input*: the waterfall and the tunnel hold still when the feed does, and
  // this one must not, because its state is advanced by arithmetic rather than
  // replaced by a measurement.
  plain(signal);
  const before = pixels(surface);
  plain(signal);
  const moved = differingBytes(before, pixels(surface));
  check(
    "the simulation advances on the same feed",
    moved > 1000,
    `${moved} of ${before.length} bytes differ between two frames`,
  );

  // Decision 42: a second delivered in one piece and in sixty moves the field
  // exactly as far. On this shell that is not a hypothetical -- animation
  // callbacks run at whatever the display does.
  resetField(renderer);
  plain(signal, 1.0);
  const oneSecond = pixels(surface);
  resetField(renderer);
  for (let n = 0; n < 60; n++) plain(signal);
  const sixtieths = pixels(surface);
  check(
    "a second in one frame moves the field as far as sixty frames of a sixtieth",
    differingBytes(oneSecond, sixtieths) === 0,
    `${differingBytes(oneSecond, sixtieths)} of ${oneSecond.length} bytes differ`,
  );

  // Decision 43, and the bug this milestone was most likely to ship. An onset
  // is *held* in the published snapshot until the next drain replaces it, so a
  // 180Hz display reads the same onset three times where a 60Hz display reads
  // it once. Spawning per frame that saw one would put three times as many
  // particles on the better machine; spawning per second of simulated time
  // cannot.
  const burst = musicalAt(0.5, 0.9, 1200);
  const withBurst = (elapsed: number) =>
    renderer.draw_frame(signal, 2, RATE, elapsed, NO_SPECTRUM, NO_SPECTRUM, burst);
  resetField(renderer);
  withBurst(FRAME_SECONDS);
  const single = pixels(surface);
  resetField(renderer);
  for (let n = 0; n < 3; n++) withBurst(FRAME_SECONDS / 3);
  const repeated = pixels(surface);
  check(
    "the same onset read three times throws what reading it once throws",
    differingBytes(single, repeated) === 0,
    `${differingBytes(single, repeated)} of ${single.length} bytes differ`,
  );

  // A beat is movement: the same audio with a tempo behind it pushes the field
  // wider than the same audio without one. Decision 45 is the other half --
  // material with no tempo gets the second picture, not a pulse invented for
  // it.
  resetField(renderer);
  spin(renderer, signal, 3.0, true, 1200);
  const beatenFigure = measure(surface);
  resetField(renderer);
  spin(renderer, signal, 3.0, false, 1200);
  const steadyFigure = measure(surface);
  const beatenWidth = beatenFigure.maxX - beatenFigure.minX + 1;
  const steadyWidth = steadyFigure.maxX - steadyFigure.minX + 1;
  check(
    "onsets and the beat throw the field wider than a steady tone does",
    beatenWidth > steadyWidth,
    `${beatenWidth}px wide with a beat, ${steadyWidth}px without`,
  );

  // The colour is a measurement: a dark, bass-heavy moment is blue and a bright
  // one is warm. Checked as red against blue, because "which hue" is not a
  // thing a pixel says on its own.
  resetField(renderer);
  spin(renderer, signal, 1.5, false, 120);
  const low = meanColour(surface);
  resetField(renderer);
  spin(renderer, signal, 1.5, false, 6000);
  const high = meanColour(surface);
  check(
    "the spectral centroid colours the field",
    low[2] > low[0] && high[0] > low[0],
    `rgb(${low[0].toFixed(0)}, ${low[1].toFixed(0)}, ${low[2].toFixed(0)}) at 120Hz against rgb(${high[0].toFixed(0)}, ${high[1].toFixed(0)}, ${high[2].toFixed(0)}) at 6kHz`,
  );
}

/// How wide the drawn figure is across one row band, as a fraction of the
/// image: `from` and `to` are fractions of the height, 0 at the top.
///
/// The measurement decision 46 is checked through, and the native half's
/// `width_between` line for line. The solid's outline *is* the spectrum -- low
/// frequencies at the foot, high at the crown -- so "is this material
/// bass-heavy" and "is the bottom of the figure wider than the top" have to be
/// the same question.
function widthBetween(
  target: HTMLCanvasElement,
  from: number,
  to: number,
): number {
  const px = pixels(target);
  const width = target.width;
  const height = target.height;
  let minX = width;
  let maxX = -1;
  for (let y = Math.floor(height * from); y < Math.min(Math.floor(height * to), height); y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      if (px[i] <= 40 && px[i + 1] <= 40 && px[i + 2] <= 40) continue;
      minX = Math.min(minX, x);
      maxX = Math.max(maxX, x);
    }
  }
  if (maxX < minX) return 0;
  return (maxX - minX + 1) / width;
}

/// A spectrum sloping from `lowDb` at the bottom of the axis to `highDb` at the
/// top -- bass-heavy one way round and treble-heavy the other.
function sloped(lowDb: number, highDb: number): SpectrumFeed {
  const analyzer = WasmAnalyzer.create(RATE, 2);
  const centres = analyzer.bandCentres();
  analyzer.free();
  const last = Math.max(centres.length - 1, 1);
  const levels = new Float32Array(centres.length);
  for (let band = 0; band < centres.length; band++) {
    levels[band] = lowDb + (highDb - lowDb) * (band / last);
  }
  return { levels, centres };
}

/// How many pixels of the image's outer border are *not* background.
function borderTouches(target: HTMLCanvasElement): number {
  const px = pixels(target);
  const width = target.width;
  const height = target.height;
  const lit = (x: number, y: number) => {
    const i = (y * width + x) * 4;
    return !(px[i] <= 40 && px[i + 1] <= 40 && px[i + 2] <= 40);
  };
  let touched = 0;
  for (let x = 0; x < width; x++) {
    if (lit(x, 0)) touched += 1;
    if (lit(x, height - 1)) touched += 1;
  }
  for (let y = 0; y < height; y++) {
    if (lit(0, y)) touched += 1;
    if (lit(width - 1, y)) touched += 1;
  }
  return touched;
}

/// What only the raymarched solid claims (Phase 3 M6) -- the same claims, in the
/// same order, as the native half's.
///
/// One renderer again, reset between claims and resized where a claim is about
/// the window's shape: this page's GPU device budget is the particle field's
/// story and applies here twice over.
async function checkTheRaymarchedSolid(): Promise<void> {
  log("\nRaymarched solid -- its own claims:");
  const signal = tone(220, 0.6);
  const surface = extraCanvas(320, 320);
  const renderer = await WasmRenderer.create(
    surface,
    surface.width,
    surface.height,
    "raymarched-solid",
  );
  const reset = () => renderer.setVisual("raymarched-solid");
  const draw = (feed: SpectrumFeed, musical?: Float32Array) =>
    renderer.draw_frame(signal, 2, RATE, FRAME_SECONDS, feed.levels, feed.centres, musical);

  // Nothing measured is nothing drawn -- the bars' answer (M2 decision 10), and
  // a different picture from a sphere resting on the display floor.
  renderer.draw_frame(signal, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  check(
    "audio with nothing measured yet draws nothing at all",
    measure(surface).lit === 0,
    `${measure(surface).lit} lit pixels`,
  );

  // Decision 46: the silhouette is the display. Bass-heavy material has to be
  // wider at the foot than at the crown, and treble-heavy material the other
  // way round -- not "different", but different in the stated direction.
  const bass = sloped(-6, -66);
  const treble = sloped(-66, -6);
  reset();
  draw(bass);
  const lowFoot = widthBetween(surface, 0.62, 0.88);
  const lowCrown = widthBetween(surface, 0.12, 0.38);
  reset();
  draw(treble);
  const highFoot = widthBetween(surface, 0.62, 0.88);
  const highCrown = widthBetween(surface, 0.12, 0.38);
  check(
    "the silhouette is the spectrum, bass at the foot and treble at the crown",
    lowFoot > lowCrown && highCrown > highFoot,
    `bass-heavy: ${lowFoot.toFixed(2)} foot against ${lowCrown.toFixed(2)} crown; treble-heavy: ${highFoot.toFixed(2)} foot against ${highCrown.toFixed(2)} crown`,
  );

  // The frequencies are placed by *Hz* through the frame's own axis, so the
  // same measurement on a different band count is the same solid. On this shell
  // that is not hypothetical: the rows arrive at whatever width the analyser
  // publishes.
  const halved: SpectrumFeed = {
    levels: bass.levels.filter((_, band) => band % 2 === 0),
    centres: bass.centres.filter((_, band) => band % 2 === 0),
  };
  reset();
  draw(halved);
  const coarseFoot = widthBetween(surface, 0.62, 0.88);
  check(
    "half as many bands is the same shape, because a band is placed by its frequency",
    Math.abs(coarseFoot - lowFoot) < 0.04,
    `${coarseFoot.toFixed(2)} of the frame at ${halved.levels.length} bands, ${lowFoot.toFixed(2)} at ${bass.levels.length}`,
  );

  // The frame holds it at every window shape -- M1's fit, arrived at here
  // through the angle a ball subtends, and checked in pixels at the shapes a
  // real window takes.
  const shapes: [number, number][] = [
    [320, 320],
    [800, 600],
    [1280, 720],
    [600, 900],
    [400, 1200],
  ];
  let worst = { touched: 0, shape: shapes[0] };
  for (const shape of shapes) {
    surface.width = shape[0];
    surface.height = shape[1];
    renderer.resize(shape[0], shape[1]);
    reset();
    draw(bass);
    const touched = borderTouches(surface);
    if (touched >= worst.touched) worst = { touched, shape };
  }
  check(
    "the solid stays inside the frame at every window shape",
    worst.touched === 0,
    `${worst.touched} border pixels touched across ${shapes.length} shapes, worst at ${worst.shape[0]}x${worst.shape[1]}`,
  );
  surface.width = 320;
  surface.height = 320;
  renderer.resize(320, 320);

  // The beat is the size (decision 45's other half): a believed tempo swells
  // the solid on the beat, and the same audio with no tempo does not.
  const onTheBeat = new Float32Array([120, 1, 0, 0, 0, 0]);
  reset();
  draw(bass, onTheBeat);
  const swollen = measure(surface).lit;
  reset();
  draw(bass);
  const resting = measure(surface).lit;
  check(
    "a believed beat swells the solid",
    swollen > resting,
    `${swollen} lit pixels on the beat, ${resting} with no tempo`,
  );

  // And decision 45 exactly: halfway between two beats the swell is zero, so
  // material *with* a tempo at that instant draws precisely what material with
  // no tempo draws. Byte for byte.
  reset();
  draw(bass, new Float32Array([120, 1, 0.5, 0, 0, 0]));
  const midway = pixels(surface);
  reset();
  draw(bass);
  const untempoed = pixels(surface);
  check(
    "between two beats it is exactly what material with no tempo draws",
    differingBytes(midway, untempoed) === 0,
    `${differingBytes(midway, untempoed)} of ${untempoed.length} bytes differ`,
  );

  // Display gain is decibels here, as it is for every spectrum display in this
  // crate (M2 decision 12).
  const quiet = sloped(-48, -66);
  reset();
  draw(quiet);
  const unity = measure(surface).lit;
  renderer.setGain("4");
  reset();
  draw(quiet);
  const magnified = measure(surface).lit;
  renderer.setGain("1");
  check(
    "display gain pushes the surface out",
    magnified > unity,
    `${magnified} lit pixels at x4, ${unity} at unity`,
  );

  // The colour is the brightness of the moment, through the same `palette` the
  // particle field uses -- so the two cannot disagree about 3kHz.
  reset();
  draw(bass, new Float32Array([0, 0, 0, 0, 0, 120]));
  const dark = meanColour(surface);
  reset();
  draw(bass, new Float32Array([0, 0, 0, 0, 0, 6000]));
  const bright = meanColour(surface);
  check(
    "the spectral centroid colours the solid",
    dark[2] > dark[0] && bright[0] > dark[0],
    `rgb(${dark[0].toFixed(0)}, ${dark[1].toFixed(0)}, ${dark[2].toFixed(0)}) at 120Hz against rgb(${bright[0].toFixed(0)}, ${bright[1].toFixed(0)}, ${bright[2].toFixed(0)}) at 6kHz`,
  );
}


/// Whether this visual reads the musical figures at all.
///
/// The native half carries the same list, for the same reason: this is a claim
/// about the *picture* rather than about the gallery, so neither shell has any
/// use for it and the registry does not carry it. What holds the two lists
/// together is that both smokes report the figure.
function readsMusical(id: string): boolean {
  return id === "particle-field" || id === "raymarched-solid";
}

/// The contract every visual in the registry has to keep, checked on **two**
/// GPU devices per visual rather than four.
///
/// That budget is the page's, not a preference. Each `WasmRenderer` holds a
/// real device, and this harness asked for four per visual until M6 -- which
/// worked at four visuals and stopped working at five: Dawn answered
/// `E_OUTOFMEMORY` creating the D3D12 command queue, and the run ended part-way
/// through (2026-08-20). A gallery that grows is the whole point of this phase,
/// so the harness had to stop growing with it.
///
/// What the two are:
///
/// - `renderer`, built as this visual, which every claim about *this* visual
///   uses. Where a claim needs a scene with no history behind it, the visual is
///   rebuilt in place (`setVisual`) -- the same swap decision 5 describes, and
///   for a visual's own state it is exactly a fresh one, since all a swap keeps
///   is the surface.
/// - `switched`, built as some *other* visual and then switched into this one,
///   which is the only claim here that genuinely needs a second device.
///
/// The native half still builds a fresh renderer per claim, because nothing
/// there is scarce -- and because the two halves arriving at the same figures
/// by different routes is worth more than the two being written the same way.
async function checkTheHostContract(entry: VisualInfo): Promise<void> {
  log(`\n${entry.name} (${entry.id}):`);
  const surface = extraCanvas(320, 320);
  const renderer = await WasmRenderer.create(surface, surface.width, surface.height, entry.id);

  const empty = new Float32Array(0);
  renderer.draw_frame(empty, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const blank = pixels(surface);
  check(
    "an empty feed still renders a frame",
    blank.length === surface.width * surface.height * 4,
    `${surface.width}x${surface.height} read back`,
  );

  // What a screen shows behind the visual, which has to be the same on both
  // shells -- reported live 2026-08-18 as near-black here and grey on the
  // desktop, from one constant handed to two surfaces of different colour
  // spaces. This canvas is `bgra8unorm` where the native window's surface is
  // sRGB, so this half and the native one exercise exactly the two paths that
  // disagreed, and must now agree figure for figure.
  check(
    "the background is the same near-black a screen should show",
    [5, 5, 8].every((want, i) => Math.abs(blank[i] - want) <= 1),
    `rgb(${blank[0]}, ${blank[1]}, ${blank[2]}) against rgb(5, 5, 8)`,
  );

  // A whole feed, because the host assembles a whole one: the vectorscope
  // reads the samples and the bars read the spectrum, and a contract check
  // handing over only half of it would be asking half the gallery to draw
  // nothing.
  //
  // This is also the `fresh` baseline the next two claims compare against: an
  // empty feed measures nothing, pushes no history and steps no simulation, so
  // this frame is the first thing this renderer has actually drawn.
  const signal = tone(220, 0.3);
  const spectrum = spectrumOf(signal, 2);
  const drawSignal = (into: WasmRenderer, musical?: Float32Array) =>
    into.draw_frame(
      signal,
      2,
      RATE,
      FRAME_SECONDS,
      spectrum.levels,
      spectrum.centres,
      musical,
    );
  drawSignal(renderer);
  const fresh = pixels(surface);
  const lit = measure(surface);
  check("audio draws something", lit.lit > 100, `${lit.lit} lit pixels`);

  // A switch has to leave the renderer exactly as a fresh one would: the whole
  // of decision 5 is that swapping the scene changes nothing else, and the
  // failure this catches -- a pipeline or attachment left over from the
  // previous visual -- would look fine for the first visual and wrong for the
  // second.
  //
  // Both sides get a renderer that has drawn *nothing* before this frame, which
  // matters from M3 onwards: a visual with history is not expected to draw the
  // same picture after one frame as after three, so comparing against a
  // renderer that has already been drawn to would be comparing two histories
  // rather than two renderers.
  const other = extraCanvas(320, 320);
  const switched = await WasmRenderer.create(other, other.width, other.height, defaultVisual());
  switched.setVisual(entry.id);
  drawSignal(switched);
  const after = pixels(other);
  let differing = 0;
  for (let i = 0; i < after.length; i++) if (after[i] !== fresh[i]) differing += 1;
  check(
    "switching into this visual draws what building it fresh does",
    differing === 0,
    `${differing} of ${fresh.length} bytes differ`,
  );
  check(
    "the renderer reports the visual it was switched to",
    switched.currentVisual() === entry.id,
    `${switched.currentVisual()}`,
  );

  // M5 put the musical figures in the feed and M6 is where scenes started
  // reading them, so this is two claims in one: a visual that reads them must
  // draw something different when they change, and every visual that does not
  // must draw *exactly* what it drew without them -- which is what stops a beat
  // leaking into the four visuals confirmed before M6.
  //
  // The scene is rebuilt first, so this frame is again the first thing the
  // visual has drawn and the only difference from `fresh` is the beat.
  const beat = new Float32Array([128, 0.9, 0.25, 0.8, 0.6, 3200]);
  renderer.setVisual(entry.id);
  drawSignal(renderer, beat);
  const withBeat = pixels(surface);
  let beatDiffering = 0;
  for (let i = 0; i < fresh.length; i++) {
    if (fresh[i] !== withBeat[i]) beatDiffering += 1;
  }
  check(
    readsMusical(entry.id)
      ? "the musical figures change what this visual draws"
      : "the musical figures change nothing a visual does not read",
    beatDiffering > 0 === readsMusical(entry.id),
    `${beatDiffering} of ${fresh.length} bytes differ at ${beat[0]} BPM`,
  );

  // Resizing rebuilds whatever the visual keeps that depends on the viewport.
  surface.width = 640;
  renderer.resize(surface.width, surface.height);
  drawSignal(renderer);
  check(
    "a different viewport renders rather than failing",
    measure(surface).lit > 100,
    `${measure(surface).lit} lit pixels at 2:1`,
  );
}

/// What only the vectorscope claims -- all of it carried over from M6, where
/// each check caught something or was written for something that had gone
/// wrong.
async function checkTheVectorscope(): Promise<void> {
  log("\n3D vectorscope -- its own claims:");
  const renderer = await WasmRenderer.create(canvas, canvas.width, canvas.height, "vectorscope");

  // Silence first: the scope has to draw *nothing* rather than a dot at the
  // origin, which is what a trace built from an empty window would look like
  // if it padded instead of stopping.
  renderer.draw_frame(new Float32Array(0), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const idle = measure();
  check("nothing played draws nothing", idle.lit === 0, `${idle.lit} lit pixels`);

  // Mono: identical channels have no side component, so the figure stands up
  // the centre. Not perfectly vertical -- the camera is off to one side and
  // the extrusion recedes, which is the point of the third axis -- but clearly
  // taller than wide.
  renderer.draw_frame(tone(220, 1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const figure = measure();
  const width = figure.maxX - figure.minX;
  const height = figure.maxY - figure.minY;
  check(
    "mono material draws a tall figure",
    figure.lit > 500 && height > width * 2,
    `${height}px tall, ${width}px wide, ${figure.lit} lit`,
  );
  check(
    "the figure stays inside the frame",
    figure.minX > 0 &&
      figure.maxX < canvas.width - 1 &&
      figure.minY > 0 &&
      figure.maxY < canvas.height - 1,
    `x ${figure.minX}..${figure.maxX}, y ${figure.minY}..${figure.maxY} of ${canvas.width}x${canvas.height}`,
  );
  check(
    "the trace fades as it recedes",
    figure.greenNear > figure.greenFar * 1.3,
    `mean green ${figure.greenNear.toFixed(0)} near, ${figure.greenFar.toFixed(0)} far`,
  );

  // An inverted pair has no mid component: the same audio, rotated onto the
  // other axis. This is what catches a scope wired to raw L/R rather than the
  // goniometer's axes -- that one draws the same figure for both signals.
  renderer.draw_frame(tone(220, -1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const flat = measure();
  check(
    "an inverted channel lays the figure flat",
    flat.lit > 500 && flat.maxX - flat.minX > flat.maxY - flat.minY,
    `${flat.maxX - flat.minX}px wide, ${flat.maxY - flat.minY}px tall, ${flat.lit} lit`,
  );

  // A quiet passage draws a smaller figure than a loud one. Easy to lose: a
  // scope that normalises its input looks livelier and stops being a
  // measurement.
  renderer.draw_frame(tone(220, 1, 0.08), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const quiet = measure();
  check(
    "level is not normalised away",
    quiet.maxY - quiet.minY < height / 2,
    `${quiet.maxY - quiet.minY}px tall at a tenth of the amplitude, against ${height}px`,
  );

  // A mono *output* is a mix with no width, and must still draw: the layout
  // the tap holds follows the output device.
  const stereo = tone(220, 1);
  const single = new Float32Array(stereo.length / 2);
  for (let i = 0; i < single.length; i++) single[i] = stereo[i * 2];
  renderer.draw_frame(single, 1, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  check("a one-channel output still draws", measure().lit > 500, `${measure().lit} lit pixels`);

  // A device with more than two channels contributes its front pair, which is
  // what a stereo field means on a surround device. The failure this catches --
  // de-interleaving by the wrong width -- draws a plausible smear rather than
  // an error.
  const surround = new Float32Array((stereo.length / 2) * 6);
  for (let n = 0; n < stereo.length / 2; n++) {
    surround[n * 6] = stereo[n * 2];
    surround[n * 6 + 1] = stereo[n * 2 + 1];
  }
  renderer.draw_frame(surround, 6, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const six = measure();
  check(
    "a 5.1 output draws its front pair, not a smear",
    six.lit > 500 &&
      six.maxY - six.minY > (six.maxX - six.minX) * 2 &&
      Math.abs(six.maxY - six.minY - height) < 8,
    `${six.maxY - six.minY}px tall, ${six.maxX - six.minX}px wide, against ${height}px in stereo`,
  );

  // Resizing rebuilds the depth and multisample targets and the projection.
  // A stale depth texture is a validation error and a stale projection
  // stretches the figure -- and resizing this scope is exactly what broke on
  // Windows in Phase 0.
  canvas.width = 640;
  renderer.resize(canvas.width, canvas.height);
  renderer.draw_frame(tone(220, 1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const wide = measure();
  check(
    "a wider viewport shows more scene rather than a stretched figure",
    wide.lit > 500 && Math.abs(wide.maxX - wide.minX - width) < Math.max(width / 4, 4),
    `${wide.maxX - wide.minX}px wide at 2:1, against ${width}px at 1:1`,
  );

  // And the same claim at a real screen's shape rather than a convenient one:
  // 16:9 is what fullscreen actually means on this machine, which is what turns
  // this from a nicety into a requirement.
  canvas.width = 1280;
  canvas.height = 720;
  renderer.resize(canvas.width, canvas.height);
  renderer.draw_frame(tone(220, 1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const full = measure();
  const fullHeight = full.maxY - full.minY;
  check(
    "at 16:9 the figure scales with the height, not the width",
    full.lit > 500 && Math.abs(fullHeight / height - 720 / 320) < 0.15,
    `${fullHeight}px tall at 1280x720, ${height}px at 320x320 (${(fullHeight / height).toFixed(2)}x against ${(720 / 320).toFixed(2)}x)`,
  );

  // Left as it started, so a second run measures what the first did.
  canvas.width = 320;
  canvas.height = 320;
  renderer.resize(canvas.width, canvas.height);
  renderer.draw_frame(tone(220, 1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
}


/// Display gain (added 2026-08-18, after the first live fullscreen run) -- the
/// same claims the native example makes, in the same order.
async function checkDisplayGain(): Promise<void> {
  log("\nDisplay gain:");
  const surface = extraCanvas(320, 320);
  const renderer = await WasmRenderer.create(
    surface,
    surface.width,
    surface.height,
    "vectorscope",
  );

  const catalog = JSON.parse(gainCatalog()) as GainInfo[];
  check(
    "the gain steps are named, unique, and only unity is a measurement",
    catalog.length > 0 &&
      catalog.every((entry) => entry.id && entry.label) &&
      new Set(catalog.map((entry) => entry.id)).size === catalog.length &&
      catalog.filter((entry) => entry.isMeasurement).length === 1 &&
      catalog.find((entry) => entry.isMeasurement)?.id === defaultGain(),
    `${catalog.map((entry) => entry.label).join(", ")}; measurement: ${defaultGain()}`,
  );

  // A quiet passage: an eighth of full scale, roughly -18 dBFS, which is the
  // case that started all this.
  const quiet = tone(220, 1, 0.125);
  renderer.draw_frame(quiet, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const unity = measure(surface);
  const unityHeight = unity.maxY - unity.minY;

  renderer.setGain("2");
  renderer.draw_frame(quiet, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const doubled = measure(surface);
  const doubledHeight = doubled.maxY - doubled.minY;

  renderer.setGain("4");
  renderer.draw_frame(quiet, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const quadrupled = measure(surface);
  const quadrupledHeight = quadrupled.maxY - quadrupled.minY;

  // The bounding box is `slant + amplitude * gain` -- the ribbon is extruded
  // through space and viewed from one side, so even a signal of no amplitude
  // draws a slanted line. What gain controls is the increment over unity, and
  // that is what must scale. (The native example measures the same thing and
  // explains the arithmetic at greater length.)
  const overUnity = (height: number) => height - unityHeight;
  check(
    "gain magnifies the trace in proportion to itself",
    Math.abs(overUnity(quadrupledHeight) / overUnity(doubledHeight) - 3) < 0.25,
    `x2 adds ${overUnity(doubledHeight)}px over unity, x4 adds ${overUnity(quadrupledHeight)}px (${(overUnity(quadrupledHeight) / overUnity(doubledHeight)).toFixed(2)}x, wanted 3x)`,
  );
  check(
    "a magnified figure is still inside the frame",
    quadrupled.minY > 0 &&
      quadrupled.maxY < surface.height - 1 &&
      quadrupled.minX > 0 &&
      quadrupled.maxX < surface.width - 1,
    `x ${quadrupled.minX}..${quadrupled.maxX}, y ${quadrupled.minY}..${quadrupled.maxY} at x4`,
  );
  check(
    "gain magnifies the trace rather than re-framing the scene",
    doubledHeight > unityHeight && quadrupledHeight > doubledHeight,
    `${unityHeight}px, ${doubledHeight}px, ${quadrupledHeight}px at x1, x2, x4`,
  );

  // Auto: the listening mode. A quiet passage ends up filling much of the
  // frame, where unity left it small.
  renderer.setGain("auto");
  renderer.draw_frame(quiet, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const automatic = measure(surface);
  const automaticHeight = automatic.maxY - automatic.minY;
  check(
    "auto lifts a quiet passage to fill the frame",
    automaticHeight > surface.height * 0.6 &&
      automatic.minY > 0 &&
      automatic.maxY < surface.height - 1,
    `${automaticHeight}px of ${surface.height} (${Math.round((100 * automaticHeight) / surface.height)}%), against ${unityHeight}px at unity`,
  );

  // ...and gives way to a loud one in a single frame rather than clipping the
  // display while it eases off.
  renderer.draw_frame(tone(220, 1), 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  const after = measure(surface);
  check(
    "auto gives way to a loud passage in one frame",
    after.minY > 0 && after.maxY < surface.height - 1,
    `y ${after.minY}..${after.maxY} of ${surface.height}`,
  );

  renderer.setGain("1");
  renderer.draw_frame(quiet, 2, RATE, FRAME_SECONDS, NO_SPECTRUM, NO_SPECTRUM);
  check(
    "unity is unchanged by having visited the other settings",
    measure(surface).maxY - measure(surface).minY === unityHeight,
    `${measure(surface).maxY - measure(surface).minY}px against ${unityHeight}px`,
  );
}

async function run(): Promise<void> {
  out.textContent = "";
  failures = 0;
  checks = 0;
  document.querySelectorAll("canvas:not(#scope)").forEach((extra) => extra.remove());

  if (!("gpu" in navigator)) {
    log("FATAL: this browser has no WebGPU, so there is no visualizer to check.");
    return;
  }
  await init();

  // The gallery, out of the same Rust registry the desktop shell reads. Checked
  // rather than assumed: this list being wrong is a gallery that offers a visual
  // this build cannot draw, or hides one it can (decision 3).
  const catalog = JSON.parse(visualCatalog()) as VisualInfo[];
  log(
    `registry: ${catalog.length} visual(s) -- ${catalog.map((entry) => entry.id).join(", ")}`,
  );
  log(`canvas: ${canvas.width}x${canvas.height}, ${visualWindowSamples(RATE, 2) / 2} frames at ${RATE} Hz`);
  check(
    "every registered visual is named and described",
    catalog.length > 0 &&
      catalog.every((entry) => entry.id && entry.name && entry.description) &&
      new Set(catalog.map((entry) => entry.id)).size === catalog.length,
    `${catalog.map((entry) => entry.name).join(", ")}`,
  );
  check(
    "the default visual is one the registry offers",
    catalog.some((entry) => entry.id === defaultVisual()),
    `${defaultVisual()}`,
  );

  for (const entry of catalog) {
    await checkTheHostContract(entry);
  }
  await checkTheVectorscope();
  await checkTheSpectrumBars();
  await checkTheSpectrogramWaterfall();
  await checkTheOscilloscopeTunnel();
  await checkTheParticleField();
  await checkTheRaymarchedSolid();
  await checkDisplayGain();

  log(
    failures === 0
      ? `\n${checks}/${checks} checks passed`
      : `\n${checks - failures}/${checks} checks passed -- FAILURES`,
  );
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => log(`ERROR: ${e}`));
});
