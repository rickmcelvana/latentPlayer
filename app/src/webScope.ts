// Adapter module for the web-build 2D scope: owns the WasmRenderer
// instance and the requestAnimationFrame loop, reading its sample window
// from playerEngine.ts each frame -- same shape as the native path
// (src-tauri's redraw thread reading player-core::Playback::scope_window),
// just driven by rAF instead of a background OS thread.
import init, { WasmRenderer, scopeWindowSamples } from "./wasm/player_rt_wasm.js";
import { outputChannels, outputSampleRate, scopeWindow } from "./playerEngine";

let renderer: WasmRenderer | null = null;
let rafHandle: number | null = null;
let wasmReady: Promise<unknown> | null = null;

function drawLoop(): void {
  if (!renderer) return;
  try {
    // A duration of history, not a sample count -- asked of the same Rust the
    // native redraw thread asks, so the two shells extrude the same 120ms
    // (M6 decision 1).
    const channels = outputChannels();
    const wanted = scopeWindowSamples(outputSampleRate(), channels);
    renderer.draw_frame(scopeWindow(wanted), channels);
  } catch (e) {
    // Found live (2026-08-15): a single thrown frame (e.g. a transient
    // Outdated/Occluded surface -- see Renderer::draw_frame's own comment)
    // used to kill the whole rAF chain permanently, since nothing here ever
    // scheduled the next frame after an exception. That's the most likely
    // explanation for "the scope just stayed blank forever" -- one bad
    // frame, and no code path ever tried again. Log and keep going instead.
    console.error("scope draw_frame error (continuing):", e);
  }
  rafHandle = requestAnimationFrame(drawLoop);
}

/// Starts rendering the scope into `canvas`. Safe to call once per canvas
/// element; call stopScope() before calling this again with a different
/// canvas.
export async function startScope(canvas: HTMLCanvasElement): Promise<void> {
  // Fail with a clear, specific message rather than whatever generic error
  // wgpu's adapter request throws -- there's no WebGL2 fallback wired up
  // yet (ARCHITECTURE.md's "Known gaps"), so no WebGPU means no scope at
  // all on this build, and that's worth saying plainly.
  if (!("gpu" in navigator)) {
    throw new Error(
      "This browser has no WebGPU support (navigator.gpu is undefined) -- the scope needs it; there's no WebGL2 fallback yet.",
    );
  }
  wasmReady ??= init();
  await wasmReady;
  try {
    renderer = await WasmRenderer.create(canvas, canvas.width, canvas.height);
  } catch (e) {
    console.error("WasmRenderer.create failed:", e);
    throw e;
  }
  if (rafHandle === null) rafHandle = requestAnimationFrame(drawLoop);
}

export function resizeScope(width: number, height: number): void {
  renderer?.resize(width, height);
}

export function stopScope(): void {
  if (rafHandle !== null) {
    cancelAnimationFrame(rafHandle);
    rafHandle = null;
  }
  renderer = null;
}
