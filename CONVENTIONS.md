# CONVENTIONS.md -- latentPlayer code standards

Adapted from `../latent-mixing/CONVENTIONS.md` (same standards, this
repo's shape). See PLAN.md §9 for why matching the sibling repos' stack
was the deliberate choice here rather than an accident.

## Rust
- Edition 2021; `cargo fmt` mandatory; `cargo clippy --all-targets -- -D warnings` clean.
- Errors: `thiserror` per crate; no `unwrap()`/`expect()` outside tests, `main.rs`, and scratch `examples/`.
- Third-party API surfaces (symphonia, cpal, wgpu, tauri, wasm-bindgen) are
  **verified against actual source/docs before landing** -- never written
  from memory. This mattered in practice: symphonia 0.6 and wgpu 30 both
  turned out to have significantly different APIs than their well-known
  older versions, and Tauri's plain (non-webview) `Window`/`WindowBuilder`
  is gated behind a non-obvious `unstable` feature flag.
- `player-core`/`player-render` know nothing about Tauri or the browser --
  they take generic inputs (a `PlaybackSource`, a `wgpu::SurfaceTarget`) and
  are driven by `src-tauri`/`player-rt-wasm`, never the reverse.
- New Rust module with logic gets `#[cfg(test)] mod tests`.

## TypeScript / React
- Strict TS, no `any`.
- Functional components. State via hooks for now; adopt Zustand once state
  actually gets shared across more than one or two components (matches
  sibling repos' pattern, not preemptively added).
- Tauri-specific APIs (`@tauri-apps/*`) only behind `isTauri()` (`app/src/isTauri.ts`)
  -- the web build must never hit them. `nativePlayer.ts` is the one module
  that touches them; `playerEngine.ts` (Phase 0 M4) is its web-build
  equivalent. Components call these adapters, never `invoke`/wasm directly.
- Styling: plain CSS (`App.css`/`index.css`), dark theme, violet accent --
  matches Latent Mastering/Mixer's visual language.

## Testing & verification
- `cargo check/test --workspace` and `npx tsc -b` green before treating a
  change as done.
- Anything touching `src-tauri/` or `player-render` needs a **real run**,
  not just a clean compile -- see WORKFLOW.md's verification note and its
  `GDK_BACKEND`/`WEBKIT_DISABLE_COMPOSITING_MODE` gotcha for this machine.
- Scratch diagnostic examples (`crates/player-core/examples/*_smoke.rs`)
  are legitimate, kept-around dev tools for exactly this reason -- they're
  the fast way to test decode/playback without booting the whole GUI app.

## General
- No TODO comments -- unfinished work goes in ARCHITECTURE.md's "Known
  gaps" section or `tasks/`.
- ASCII in code/comments (UI strings may use Unicode).
