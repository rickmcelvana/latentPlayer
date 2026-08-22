// Adapter for the *visualizer host* -- the fourth of the shell-choosing
// modules, beside playlist.ts (playback), analysis.ts (metering) and
// library.ts.
//
// The two shells host the visualizer very differently and cannot be made to
// agree on that: natively it is a second, plain window with a wgpu surface
// built from its handle, because a wgpu surface cannot composite into a
// webview; on the web it is a canvas in the page. What they *can* agree on is
// everything the host owns (tasks/phase-3.md decision 1) -- how big it is,
// whether it is up, **which visual it draws**, and **whether it has the whole
// screen**. This module is where the difference stops, and it is the only
// module in the app that knows fullscreen means two different mechanisms.
import { isTauri } from "./isTauri";
import {
  gainCatalog as gainCatalogNative,
  setGain as setGainNative,
  setMilkPreset as setMilkPresetNative,
  setVisual as setVisualNative,
  setVisualizerFullscreen as setVisualizerFullscreenNative,
  setVisualizerSize as setVisualizerSizeNative,
  setVisualizerVisible as setVisualizerVisibleNative,
  visualCatalog as visualCatalogNative,
  visualizerState as visualizerStateNative,
} from "./nativePlayer";
import {
  currentGain as currentGainWeb,
  currentPresetReportWeb,
  currentPresetWeb,
  currentVisual as currentVisualWeb,
  gainCatalogWeb,
  setPresetWeb,
  isCanvasFullscreen,
  isVisualizerVisibleWeb,
  resizeVisualizer,
  setCanvasFullscreen,
  setGainWeb,
  setVisualWeb,
  setVisualizerVisibleWeb,
  visualCatalogWeb,
} from "./webVisualizer";

/// The sizes the UI offers, square because the vectorscope's figure is. Values
/// are physical pixels: a visualizer doubling as a measurement display is sized
/// in pixels rather than in layout units a zoom level could quietly change.
///
/// Fullscreen is deliberately *not* one of these. It is a different state
/// rather than a bigger size -- it hides the chrome, it is left by Esc, and on
/// both shells it is the OS or the browser that decides the pixels.
export const VISUALIZER_SIZES = [360, 480, 640, 800] as const;

export const DEFAULT_VISUALIZER_SIZE = 480;

/// Where the chosen visual is remembered between sessions.
///
/// localStorage, like the EQ's settings and for the same reason: it is one
/// short string, not library data, and it has to be readable synchronously
/// while the first frame is being set up. The stored value is a
/// `VisualKind::id` -- the stable name Rust owns, never a display name and
/// never an index, so a release that reorders the gallery does not silently
/// change what someone chose.
const SELECTION_KEY = "latentplayer.visual";

/// Where the display gain is remembered, on the same terms.
const GAIN_KEY = "latentplayer.gain";

/// One entry in the gallery. A transcription of `VisualInfo` in
/// src-tauri/src/lib.rs and of `visualCatalog()`'s JSON in player-rt-wasm --
/// both of which are filled from `player_render::VisualKind`, which is the one
/// list of visuals in the project (decision 3).
export interface VisualInfo {
  id: string;
  name: string;
  description: string;
}

/// One step of display gain. A transcription of `GainInfo` in
/// src-tauri/src/lib.rs and of `gainCatalog()`'s JSON in player-rt-wasm, both
/// filled from `player_render::GainSetting`.
export interface GainInfo {
  id: string;
  label: string;
  /// True only at unity. The panel says when it stops being true, because a
  /// scope that has been scaled without saying so is worse than one that
  /// cannot scale at all.
  isMeasurement: boolean;
}

/// Everything the host owns, in one read. One shape for both shells, so
/// `VisualizerControls` never branches on which one it is in.
export interface VisualizerState {
  width: number;
  height: number;
  visible: boolean;
  fullscreen: boolean;
  /// A `VisualKind::id`.
  visual: string;
  /// A `GainSetting::id`.
  gain: string;
  gainIsMeasurement: boolean;
  /// The selected MilkDrop preset's file name, or null when there is none.
  /// Selection state beside the visual rather than part of it (decision 49):
  /// it survives switching to another visual and back.
  preset: string | null;
  /// What that preset needs and this build does not do, plus anything that went
  /// wrong compiling it -- shown beside its name (decision 56).
  presetReport: string[];
}

/// The web build's canvas, handed over by the component that owns it so this
/// module can size it without reaching into the DOM on its own.
let webCanvas: HTMLCanvasElement | null = null;

/// Registers (or clears) the canvas the web build draws into.
export function attachWebCanvas(canvas: HTMLCanvasElement | null): void {
  webCanvas = canvas;
}

/// The gallery. Asked of Rust on both shells rather than written out here:
/// a list of visuals in TypeScript is a list that will disagree with the
/// registry the moment a visual is added.
export async function visualCatalog(): Promise<VisualInfo[]> {
  if (isTauri()) return visualCatalogNative();
  return visualCatalogWeb();
}

/// The display-gain steps on offer. Asked of Rust for the same reason the
/// gallery is.
export async function gainCatalog(): Promise<GainInfo[]> {
  if (isTauri()) return gainCatalogNative();
  return gainCatalogWeb();
}

/// The gain chosen last session, if this build still offers it.
export function storedGain(catalog: GainInfo[]): string | null {
  try {
    const stored = localStorage.getItem(GAIN_KEY);
    return stored && catalog.some((entry) => entry.id === stored) ? stored : null;
  } catch {
    return null;
  }
}

/// Sets the display gain, and remembers it.
///
/// A display decision, not a measurement one: it magnifies the drawn figure and
/// changes nothing the meters read. The frame stays pinned to unity full scale,
/// so what a gained figure means is "this signal, drawn n times larger" rather
/// than "this signal, rescaled to fit".
export async function setGain(id: string): Promise<void> {
  try {
    localStorage.setItem(GAIN_KEY, id);
  } catch {
    // Not remembering is a smaller problem than not applying.
  }
  if (isTauri()) return setGainNative(id);
  setGainWeb(id);
}

/// The visual chosen last session, if this build still has it.
///
/// An unknown id -- a selection made by a later build, or a visual since
/// removed -- resolves to null rather than to something else, and the caller
/// falls back to the registry's own default. Silently substituting would mean
/// a gallery that quietly forgets what someone picked.
export function storedVisual(catalog: VisualInfo[]): string | null {
  try {
    const stored = localStorage.getItem(SELECTION_KEY);
    return stored && catalog.some((entry) => entry.id === stored) ? stored : null;
  } catch {
    // A browser with storage disabled still has a working gallery; it just
    // opens on the default every time.
    return null;
  }
}

function rememberVisual(id: string): void {
  try {
    localStorage.setItem(SELECTION_KEY, id);
  } catch {
    // See above -- not remembering is a smaller problem than not switching.
  }
}

/// Current host state. Async because natively it is a round trip to a window
/// the user may also have resized by dragging its edge, closed with its own
/// close button, or dropped out of fullscreen with Esc -- the UI is one of
/// several parties that can change this, not the only one.
export async function visualizerState(): Promise<VisualizerState> {
  if (isTauri()) return visualizerStateNative();
  return {
    width: webCanvas?.width ?? DEFAULT_VISUALIZER_SIZE,
    height: webCanvas?.height ?? DEFAULT_VISUALIZER_SIZE,
    visible: isVisualizerVisibleWeb(),
    fullscreen: isCanvasFullscreen(),
    visual: currentVisualWeb(),
    gain: currentGainWeb(),
    gainIsMeasurement: currentGainWeb() === "1",
    preset: currentPresetWeb(),
    presetReport: currentPresetReportWeb(),
  };
}

/// Resizes the visualizer. Natively this moves a window; on the web it resizes
/// the canvas and tells the renderer, which has to reconfigure its surface and
/// rebuild its attachments either way.
export async function setVisualizerSize(width: number, height: number): Promise<void> {
  if (isTauri()) return setVisualizerSizeNative(width, height);
  if (webCanvas) {
    webCanvas.width = width;
    webCanvas.height = height;
    resizeVisualizer(width, height);
  }
}

/// Shows or hides the visualizer. Natively the window is hidden rather than
/// closed, since the renderer holds a surface built from its handle.
export async function setVisualizerVisible(visible: boolean): Promise<void> {
  if (isTauri()) return setVisualizerVisibleNative(visible);
  setVisualizerVisibleWeb(visible);
}

/// Chooses a visual, and remembers it.
///
/// The swap itself never touches the surface on either shell (decision 5):
/// natively this is a request the redraw thread applies between two frames, on
/// the web it is a call on the renderer that owns the canvas.
export async function setVisual(id: string): Promise<void> {
  rememberVisual(id);
  if (isTauri()) return setVisualNative(id);
  setVisualWeb(id);
}

/// Takes the whole screen, or gives it back (decision 4).
///
/// The one place in this app where the two shells do genuinely different
/// things, rather than the same thing through different plumbing:
///
/// - **Web**: the Fullscreen API on the element wrapping the canvas. Esc and
///   the browser's own exit affordances come free, and the resize is driven by
///   the `fullscreenchange` event rather than from here.
/// - **Native**: borderless fullscreen of the window that already exists. Esc
///   has to be arranged for -- that window has no webview and receives no key
///   events at all -- so the desktop shell registers a global shortcut while
///   fullscreen is active and drops it again on the way out.
///
/// Both are reversible by the same call with `false`, which is what lets one
/// button in one component drive both.
export async function setVisualizerFullscreen(fullscreen: boolean): Promise<void> {
  if (isTauri()) return setVisualizerFullscreenNative(fullscreen);
  return setCanvasFullscreen(fullscreen);
}

/// Loads a MilkDrop preset, or clears the selection with `null` (Phase 3 M7a).
///
/// The last thing in this module that differs by shell, and it differs for a
/// reason neither side can avoid: the desktop shell has a path and the web shell
/// has a `File`. Both end at the same `Renderer::set_preset` in Rust, so both
/// parse the same file with the same code and get the same answer about what it
/// needs -- which is what the return value carries.
///
/// Not remembered between sessions, deliberately. A stored path is a path that
/// will eventually name a file that has moved, and this milestone has nowhere
/// to put a library yet: pointing at a folder is M7c's, and it is the flow that
/// deserves the persistence.
export async function setMilkPreset(file: MilkPresetChoice | null): Promise<string[]> {
  if (isTauri()) return setMilkPresetNative(file?.path ?? null);
  if (!file) return setPresetWeb(null, null);
  return setPresetWeb(file.name, await file.text());
}

/// A preset the user picked, in whichever form their shell offers. The desktop
/// shell fills in `path`; the web shell fills in `text`.
export interface MilkPresetChoice {
  name: string;
  /// Native only -- a path this process can read.
  path?: string;
  /// The `.milk` text. Read lazily, since the native shell never needs it here.
  text(): Promise<string>;
}
