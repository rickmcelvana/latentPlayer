# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

latentPlayer — Latentbeats.com Music Player. See `PLAN.md` for product/architecture planning
(status: Phase 0 spikes in progress) and `ARCHITECTURE.md`/`CONVENTIONS.md`/`WORKFLOW.md` for
what's actually built and how to work on it.

## Status

Phase 0 (spikes) is built end to end: Rust workspace (`crates/player-core`, `crates/player-render`,
`crates/player-rt-wasm`), Tauri desktop shell (`src-tauri/`), and React/TS/Vite frontend (`app/`,
both native and web playback paths). Every build/check/test command below passes.

**The dev machine is Windows** (`Z:\_dev\latent\latentPlayer`) as of 2026-08-15; Linux is a later
test target. The Linux-only workarounds still documented in `WORKFLOW.md` (Wayland env vars) and
`ARCHITECTURE.md` (Chrome WebGPU flags) do not apply here.

**Web is user-confirmed working on Windows** (audio + scope, 2026-08-15); the old blank-scope bug
was environmental, not app code (`tasks/phase-0.md` M6). **Native output was genuinely broken on
most Windows devices and is now fixed** (`tasks/phase-0.md` M7, full write-up in
`ARCHITECTURE.md`) — it opened devices at the wrong rate, which cpal's WASAPI backend then either
refused outright or played back at 2.177x. `cargo run -p player-core --example device_probe`
diagnoses any output device. The one thing still open in Phase 0 is a listen-test of the native
build after that fix. See `ARCHITECTURE.md`'s "Known gaps" for known limitations (Opus, resampler
quality, output-device changes, etc.).

## Build/lint/test

```
cargo check --workspace --exclude player-rt-wasm
cargo test --workspace --exclude player-rt-wasm
cargo clippy --workspace --exclude player-rt-wasm --all-targets -- -D warnings
cargo fmt --check
# player-rt-wasm is wasm-only (see WORKFLOW.md for why it's excluded above):
cd app && npm run wasm   # rebuilds it for wasm32 via wasm-pack

cd app && npx tsc -b && npm run build   # frontend
cd app && npm run dev                    # web dev server

cargo tauri dev   # desktop dev build -- stop any standalone `npm run dev` first
                  # (port 5173; see WORKFLOW.md). On Linux only, this needs an
                  # env-var workaround for Wayland -- also in WORKFLOW.md.
```

Full detail (including the `cargo tauri dev` gotcha) is in `WORKFLOW.md`. Coding standards are
in `CONVENTIONS.md`. Crate/module map and known gaps are in `ARCHITECTURE.md`.
