# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

latentPlayer — Latentbeats.com Music Player. See `PLAN.md` for product/architecture planning
(status: Phase 0 spikes in progress) and `ARCHITECTURE.md`/`CONVENTIONS.md`/`WORKFLOW.md` for
what's actually built and how to work on it.

## Status

Phase 0 (spikes) is built end to end: Rust workspace (`crates/player-core`, `crates/player-render`,
`crates/player-rt-wasm`), Tauri desktop shell (`src-tauri/`), and React/TS/Vite frontend (`app/`,
both native and web playback paths). Native is fully user-confirmed working. Web audio is
user-confirmed working. **Web scope (canvas) is a live, unresolved bug — stays blank, cause
unknown after two rounds of code-only diagnosis; see `ARCHITECTURE.md`'s "Active bug" section
and `tasks/phase-0.md`'s "Resuming this session" note before doing anything else here.** See
`ARCHITECTURE.md`'s "Known gaps" for other known limitations (Opus, resampling, etc.).

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

cargo tauri dev   # desktop dev build -- see WORKFLOW.md for a required
                  # env-var workaround on this machine's Wayland setup
```

Full detail (including the `cargo tauri dev` gotcha) is in `WORKFLOW.md`. Coding standards are
in `CONVENTIONS.md`. Crate/module map and known gaps are in `ARCHITECTURE.md`.
