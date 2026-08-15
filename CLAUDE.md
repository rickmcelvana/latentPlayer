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

**Phase 0 is closed** (2026-08-15): both shells are user-confirmed working on real music files —
web ("sounds perfect, scope shows up") and native ("no error with the wired headset or wireless,
sounds good"). Getting there on Windows took three real fixes, all written up in
`tasks/phase-0.md` M6–M8: the blank web scope turned out to be environmental (the Linux box's
Chrome/Dawn adapter blocklist), native output opened devices at the wrong sample rate (cpal's
WASAPI backend then either refused outright or played at 2.177x), and the scope window couldn't
survive being resized. `cargo run -p player-core --example device_probe` diagnoses any output
device.

**Phase 1 is underway** — see `tasks/phase-1.md` for the milestone plan and decisions (engine
foundation before metering; web parity lockstep per milestone). M1 (transport: real `rubato`
resampling, seek, volume, position, transport UI) is done and user-confirmed on both shells; M2 is
streaming decode. See `ARCHITECTURE.md`'s "Known gaps" for tracked limitations (Opus,
whole-file decode, output-device changes, WebGL2 fallback, scope aspect).

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
