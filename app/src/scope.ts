// Adapter for the scope's *geometry* -- the third of the shell-choosing
// modules, beside playlist.ts (playback) and analysis.ts (metering).
//
// The two shells host the scope very differently and cannot be made to agree
// on that: natively it is a second, plain window with a wgpu surface built
// from its handle, because a wgpu surface cannot composite into a webview; on
// the web it is a canvas in the page. What they *can* agree on is who decides
// how big it is, which since M6 is the UI (M6 decision 4). This module is
// where that difference stops.
import { isTauri } from "./isTauri";
import {
  scopeState as scopeStateNative,
  setScopeSize as setScopeSizeNative,
  setScopeVisible as setScopeVisibleNative,
} from "./nativePlayer";
import { resizeScope } from "./webScope";

/// The sizes the UI offers, square because the figure is. Values are physical
/// pixels: the scope is a measurement display, so it is sized in pixels rather
/// than in layout units that a zoom level could quietly change.
export const SCOPE_SIZES = [360, 480, 640, 800] as const;

export const DEFAULT_SCOPE_SIZE = 480;

export interface ScopeState {
  width: number;
  height: number;
  visible: boolean;
}

/// The web build's canvas, handed over by the component that owns it so this
/// module can size it without reaching into the DOM on its own.
let webCanvas: HTMLCanvasElement | null = null;
let webVisible = true;

/// Registers (or clears) the canvas the web build draws into.
export function attachWebCanvas(canvas: HTMLCanvasElement | null): void {
  webCanvas = canvas;
}

/// Current geometry. Async because natively it is a round trip to a window the
/// user may also have resized by dragging its edge -- the UI is one of two
/// parties that can change this, not the only one.
export async function scopeState(): Promise<ScopeState> {
  if (isTauri()) return scopeStateNative();
  return {
    width: webCanvas?.width ?? DEFAULT_SCOPE_SIZE,
    height: webCanvas?.height ?? DEFAULT_SCOPE_SIZE,
    visible: webVisible,
  };
}

/// Resizes the scope. Natively this moves a window; on the web it resizes the
/// canvas and tells the renderer, which has to reconfigure its surface and
/// rebuild its depth buffer either way.
export async function setScopeSize(width: number, height: number): Promise<void> {
  if (isTauri()) return setScopeSizeNative(width, height);
  if (webCanvas) {
    webCanvas.width = width;
    webCanvas.height = height;
    resizeScope(width, height);
  }
}

/// Shows or hides the scope. Natively the window is hidden rather than closed,
/// since the renderer holds a surface built from its handle.
export async function setScopeVisible(visible: boolean): Promise<void> {
  if (isTauri()) return setScopeVisibleNative(visible);
  webVisible = visible;
}
