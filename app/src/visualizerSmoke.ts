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

/// The contract the *host* depends on, asked of every visual in the registry --
/// the same four claims the native check makes, in the same order.
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
  const signal = tone(220, 0.3);
  const spectrum = spectrumOf(signal, 2);
  const drawSignal = (into: WasmRenderer) =>
    into.draw_frame(signal, 2, RATE, FRAME_SECONDS, spectrum.levels, spectrum.centres);
  drawSignal(renderer);
  const lit = measure(surface);
  check("audio draws something", lit.lit > 100, `${lit.lit} lit pixels`);
  const fresh = pixels(surface);

  // A switch has to leave the renderer exactly as a fresh one would: the whole
  // of decision 5 is that swapping the scene changes nothing else, and the
  // failure this catches -- a pipeline or attachment left over from the
  // previous visual -- would look fine for the first visual and wrong for the
  // second.
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
