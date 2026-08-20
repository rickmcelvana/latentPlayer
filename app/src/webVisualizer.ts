// The web build's half of the visualizer host: owns the WasmRenderer, the
// requestAnimationFrame loop, the canvas's fullscreen, and nothing else. It
// reads its feed from playerEngine.ts each frame -- the same shape as the
// native path (src-tauri's redraw thread reading player-core::Playback's tap),
// just driven by rAF instead of a background OS thread.
//
// Everything above this -- which visual, how big, whether it is up -- is
// visualizer.ts's, which is the module both shells share. What lives here is
// only what a browser does differently.
import init, {
  WasmRenderer,
  defaultGain,
  defaultVisual,
  gainCatalog,
  visualCatalog,
  visualWindowSamples,
} from "./wasm/player_rt_wasm.js";
import {
  outputChannels,
  outputSampleRate,
  scopeWindow,
  musicalArray,
  spectrum,
  spectrumBands,
} from "./playerEngine";
import type { GainInfo, VisualInfo } from "./visualizer";

let renderer: WasmRenderer | null = null;
let rafHandle: number | null = null;
let wasmReady: Promise<unknown> | null = null;
/// The element the Fullscreen API is asked about, which is the canvas's
/// *wrapper* rather than the canvas: a fullscreen canvas is stretched by the
/// browser to fill the screen at whatever backing size it happens to have,
/// where a wrapper lets the canvas be resized to the screen and centred in it.
let fullscreenTarget: HTMLElement | null = null;
/// The size to go back to when fullscreen ends. Remembered rather than
/// recomputed, because the browser's exit does not say what the canvas used to
/// be and the user can leave fullscreen by four different routes.
let sizeBeforeFullscreen: { width: number; height: number } | null = null;
/// What the gallery last asked for. Held so the id survives a renderer that has
/// not been created yet -- the panel can mount before WebGPU has answered.
let selected = "";
/// The display gain last asked for, held for the same reason as `selected`.
let selectedGain = "";
/// When the previous frame was drawn, for the frame's `elapsed_seconds`. The
/// automatic gain's release is timed from this, so it must be real elapsed time
/// and not an assumed 60Hz -- a browser throttles animation callbacks whenever
/// it feels like it.
let lastFrame = 0;
/// Whether the visualizer is up. The rAF loop keeps ticking while this is
/// false but draws nothing, which is the browser's version of the native
/// redraw thread parking on a hidden window: cheap, and it resumes on the very
/// next frame rather than needing the loop restarted.
let visible = true;

function drawLoop(): void {
  if (!renderer) return;
  try {
    // A hidden visualizer is drawn for nobody. The native shell has parked its
    // redraw thread on this condition since M6 (measured there at 281ms of CPU
    // per 3s with the scope up, 0ms hidden); on the web the same button used to
    // flip its own label and nothing else, which M1 fixes because visibility is
    // one of the four things the host owns.
    if (!visible) {
      rafHandle = requestAnimationFrame(drawLoop);
      return;
    }
    // The feed (tasks/phase-3.md decision 2), assembled here from what the
    // engine already publishes -- no visual computes anything about the audio.
    // A duration of history, not a sample count, asked of the same Rust the
    // native redraw thread asks, so the two shells hand over the same 120ms
    // (M6 decision 1).
    const channels = outputChannels();
    const rate = outputSampleRate();
    const wanted = visualWindowSamples(rate, channels);
    const now = performance.now();
    const elapsed = lastFrame === 0 ? 1 / 60 : (now - lastFrame) / 1000;
    lastFrame = now;
    // The spectrum joined the feed in M2, for the bars. Read from the same
    // shared buffer the Spectrum panel reads, so the two are drawing one
    // measurement rather than two -- and empty until the worker has published
    // any, which is a state a visual draws as nothing. M5's musical figures
    // ride along on the same terms, in the flat order Rust named them in, out
    // of the region the worker writes in the same step as the meters.
    renderer.draw_frame(
      scopeWindow(wanted),
      channels,
      rate,
      elapsed,
      spectrum(),
      spectrumBands(),
      musicalArray(),
    );
  } catch (e) {
    // Found live (2026-08-15): a single thrown frame (e.g. a transient
    // Outdated/Occluded surface -- see Renderer::draw_frame's own comment)
    // used to kill the whole rAF chain permanently, since nothing here ever
    // scheduled the next frame after an exception. That's the most likely
    // explanation for "the scope just stayed blank forever" -- one bad
    // frame, and no code path ever tried again. Log and keep going instead.
    console.error("visualizer draw_frame error (continuing):", e);
  }
  rafHandle = requestAnimationFrame(drawLoop);
}

/// The gallery, out of the same Rust registry the desktop shell reads.
export async function visualCatalogWeb(): Promise<VisualInfo[]> {
  wasmReady ??= init();
  await wasmReady;
  return JSON.parse(visualCatalog()) as VisualInfo[];
}

/// Starts rendering `visual` into `canvas`. Safe to call once per canvas
/// element; call stopVisualizer() before calling this again with a different
/// canvas.
export async function startVisualizer(
  canvas: HTMLCanvasElement,
  visual?: string,
): Promise<void> {
  // Fail with a clear, specific message rather than whatever generic error
  // wgpu's adapter request throws -- there's no WebGL2 fallback wired up
  // yet (ARCHITECTURE.md's "Known gaps", and tasks/phase-3.md decision 8
  // keeps it out of this phase deliberately), so no WebGPU means no
  // visualizer at all on this build, and that's worth saying plainly.
  if (!("gpu" in navigator)) {
    throw new Error(
      "This browser has no WebGPU support (navigator.gpu is undefined) -- the visualizer needs it; there's no WebGL2 fallback yet.",
    );
  }
  wasmReady ??= init();
  await wasmReady;
  selected = visual ?? selected ?? defaultVisual();
  if (!selected) selected = defaultVisual();
  try {
    renderer = await WasmRenderer.create(canvas, canvas.width, canvas.height, selected);
  } catch (e) {
    console.error("WasmRenderer.create failed:", e);
    throw e;
  }
  // The wrapper, if the canvas has one -- see `fullscreenTarget` above. A
  // canvas with no wrapper is still fullscreen-able, just stretched.
  if (selectedGain) renderer.setGain(selectedGain);
  fullscreenTarget = canvas.parentElement ?? canvas;
  document.addEventListener("fullscreenchange", onFullscreenChange);
  if (rafHandle === null) rafHandle = requestAnimationFrame(drawLoop);
}

export function resizeVisualizer(width: number, height: number): void {
  renderer?.resize(width, height);
}

/// The display-gain steps, out of the same Rust registry the desktop shell
/// reads.
export async function gainCatalogWeb(): Promise<GainInfo[]> {
  wasmReady ??= init();
  await wasmReady;
  return JSON.parse(gainCatalog()) as GainInfo[];
}

/// Sets the display gain. A number in the next frame, not a rebuild.
export function setGainWeb(id: string): void {
  selectedGain = id;
  renderer?.setGain(id);
}

/// The gain in effect (or the one asked for, if the renderer is still starting).
export function currentGain(): string {
  return renderer?.currentGain() ?? selectedGain ?? defaultGain();
}

/// Swaps the scene. The canvas, the surface and the rAF loop are untouched --
/// the web half of decision 5.
export function setVisualWeb(id: string): void {
  selected = id;
  renderer?.setVisual(id);
}

/// The id of the visual being drawn (or the one asked for, if the renderer is
/// still starting up).
export function currentVisual(): string {
  return renderer?.currentVisual() ?? selected;
}

/// Shows or hides the visualizer: the canvas goes out of the page and the draw
/// loop stops drawing. The web counterpart of hiding the native window, and
/// hiding rather than tearing down for the same reason -- the renderer holds a
/// surface built from this canvas, and it has to be there to come back to.
export function setVisualizerVisibleWeb(next: boolean): void {
  visible = next;
  if (fullscreenTarget) fullscreenTarget.style.display = next ? "" : "none";
}

export function isVisualizerVisibleWeb(): boolean {
  return visible;
}

export function isCanvasFullscreen(): boolean {
  return fullscreenTarget !== null && document.fullscreenElement === fullscreenTarget;
}

/// Enters or leaves fullscreen on the canvas's wrapper.
///
/// The resize is deliberately *not* done here but in the `fullscreenchange`
/// handler: the user can also leave by Esc or by the browser's own control,
/// and a canvas resized only on the path this function takes would come back
/// the wrong size on the three paths it doesn't.
export async function setCanvasFullscreen(fullscreen: boolean): Promise<void> {
  if (!fullscreenTarget) return;
  if (fullscreen) {
    if (!isCanvasFullscreen()) await fullscreenTarget.requestFullscreen();
  } else if (document.fullscreenElement) {
    await document.exitFullscreen();
  }
}

/// Sizes the canvas to whatever just happened -- the screen on the way in, its
/// old size on the way out.
function onFullscreenChange(): void {
  const canvas = fullscreenTarget?.querySelector("canvas") ?? null;
  if (!(canvas instanceof HTMLCanvasElement)) return;
  if (isCanvasFullscreen()) {
    sizeBeforeFullscreen = { width: canvas.width, height: canvas.height };
    // Physical pixels, not CSS ones: a 1440p screen at devicePixelRatio 1.5
    // asked for a 1707x960 backing store would be visibly soft. Capped at
    // 4x the CSS size so a very high ratio cannot ask for a surface that
    // costs more than it shows (4x MSAA is already multiplying this).
    const ratio = Math.min(window.devicePixelRatio || 1, 4);
    canvas.width = Math.round(window.screen.width * ratio);
    canvas.height = Math.round(window.screen.height * ratio);
  } else if (sizeBeforeFullscreen) {
    canvas.width = sizeBeforeFullscreen.width;
    canvas.height = sizeBeforeFullscreen.height;
    sizeBeforeFullscreen = null;
  }
  resizeVisualizer(canvas.width, canvas.height);
}

export function stopVisualizer(): void {
  if (rafHandle !== null) {
    cancelAnimationFrame(rafHandle);
    rafHandle = null;
  }
  document.removeEventListener("fullscreenchange", onFullscreenChange);
  fullscreenTarget = null;
  renderer = null;
}
