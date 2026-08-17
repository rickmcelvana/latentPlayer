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
resampling, seek, volume, position, transport UI) is done and user-confirmed on both shells.
M2 (streaming decode: `StreamingDecoder` + a decoder thread feeding an `rtrb` ring natively, a
Worker topping up a SharedArrayBuffer ring on the web) is done and user-confirmed on both shells.
M3 (queue + gapless) is done and user-confirmed on both shells -- the engine
holds current + next, track changes ride a second ring alongside the audio
(`cargo run -p player-core --example gapless_smoke` natively, `/gapless.html` on
the web, 11/11 each), and `app/src/playlist.ts` + the playlist UI own the
ordering.

M4 (analysis tap + spectrum analyzer) is done and user-confirmed on both shells:
`tap.rs` is the one tap the scope and every meter read (snapshots for displays,
cursors for meters that must see every sample), `analysis.rs`/`spectrum.rs` are
the cross-target meters, and they run off the audio thread on both shells --
`cargo run -p player-core --example spectrum_smoke` and `/spectrum.html` report
10/10 each.

M5 (LUFS + phase metering) is done and user-confirmed on both shells:
`loudness.rs` is BS.1770-4 (K-weighting derived per
output rate, momentary/short-term/gated integrated, true peak by 4x
oversampling) and `phase.rs` is the correlation meter plus the goniometer's
points. Both join the spectrum inside `Meters`, off the one drain, and reset
whenever a continuous listen breaks -- a seek, a lap, or a gapless hand-off.
`cargo run -p player-core --example loudness_smoke` and `/loudness.html` report
15/15 each, agreeing to the hundredth of a dB.

M6 (3D vectorscope v1) is done and user-confirmed on both shells:
`player-render` now has a real 3D pipeline
(`camera.rs`, `scene.rs` with a depth buffer and 4x MSAA) and `trace.rs`
extrudes the XY figure along a time axis -- a fixed 120ms of history, on the
goniometer's own axes. The scope window is resizable and the UI owns its size
and visibility (`scope.ts`/`ScopeControls.tsx`, driving a native window on one
shell and the canvas on the other). `cargo run -p player-render --example
scope_smoke` and `/scope.html` report 8/8 each, against pixels read back rather
than eyeballed.

M7 (library + tags + playlists) is done and user-confirmed on both shells:
`tags.rs` reads ID3v2/Vorbis/RIFF/MP4
metadata and cover art off symphonia's probe without decoding audio, and both
shells use it. The desktop library (`library.rs`) walks folders into a
versioned JSON index plus a content-hashed cover cache in the app data
directory; the web build's library lives in **IndexedDB** -- rows, cover blobs,
playlists and the scanned folder's `FileSystemDirectoryHandle`, which is what
lets a later session reopen the real files. Cover art never crosses the JSON
IPC boundary (a `cover://` URI scheme protocol natively, object URLs on the
web). Scanning runs off both the audio path and the UI thread: a blocking
thread natively, a *second* worker on the web. `cargo run -p player-core
--example library_smoke` reports 9/9 and `/library.html` 12/12, agreeing figure
for figure.

M8 (basic EQ) is done and user-confirmed on both shells: `eq.rs` is a ten-band
graphic EQ (ISO octave centres, shelves at the ends, peaks between, a preamp and
a bypass) running **between the ring and the output, before the analysis tap and
before the volume** -- so every meter measures what the listener is played while
monitoring level still changes nothing. It is the one place the "all DSP is Rust
on both shells" rule gives way, because the web's audio path is an AudioWorklet:
the *design* stays in `player-core::eq` (band shapes, Q, the clamp, the curve
the UI draws) and only the difference equation is mirrored, in
`app/src/eqFilter.ts`, with `/eq.html` holding the two to each other -- they
agree exactly. `cargo run -p player-core --example eq_smoke` reports 14/14 and
`/eq.html` 15/15, measuring the EQ *through the spectrum analyser*, which only
works because the filters sit ahead of the tap.

**Phase 1's milestone list is complete** (2026-08-16) -- M1 to M8, all
user-confirmed on both shells. The two plain bugs it left standing are **fixed
and user-confirmed (2026-08-17)**, before Phase 2 rather than carried into it:
play at the end of a track now restarts it (the engines rewind on both shells;
the button is labelled
from `playing && !finished`), and the scope window can no longer be destroyed
under its renderer -- its close button hides it, and the app owns that window's
whole lifecycle, including stopping the redraw thread before exiting. See
ARCHITECTURE.md's "The scope window's lifecycle" for that decision and
`tasks/phase-1.md`'s closing sections for both. The rest of ARCHITECTURE.md's
"Known gaps" are design trades and remain open.

**Phase 2 M1 is built** (2026-08-17): the Subsonic protocol lives in
`player-core::subsonic` (IO-free, cross-target -- the desktop transport is
`subsonic/http.rs` with ureq, the web's is `fetch` in `subsonicWeb.ts` calling
the same Rust through wasm), the secret goes to the OS credential store natively
and IndexedDB on the web, and both shells have the server panel. Verified: 11/11
fake-server tests, 7/7 in a real browser against the local Navidrome, and the
native `subsonic_smoke`. The one claim still unproven is that a *correct*
password is accepted -- that needs a real account, so run `/subsonic.html` (or
`LATENTPLAYER_SUBSONIC_USER`/`_PASSWORD` + `subsonic_smoke`) to close it.

`cargo run -p player-core --example subsonic_smoke` checks the client against a
real server; `cargo test -p player-core --test subsonic_http` checks it against
a fake one that misbehaves on purpose (`tests/common/mod.rs`, which M2 reuses).

**Phase 2 is planned** (2026-08-17) -- see `tasks/phase-2.md` for
the milestone plan (M1 connection, M2 remote playback, M3 browsing, M4
playlists/scrobbling/stars, M5 failing well) and its eight decisions. The short
version: Subsonic/OpenSubsonic only (Jellyfin does not speak it natively --
PLAN.md §8.6 is corrected), the protocol lives in `player-core` while the
transport does not (ureq natively, `fetch` in the worker on the web), web parity
stays lockstep with the CORS and mixed-content limits stated rather than hidden,
and the server's library is browsed live rather than mirrored. Nothing in the
phase should touch the audio callback, the DSP path or the renderer. It needs a
real Navidrome to verify against.

`cargo run -p player-core --example seek_smoke -- <path>` checks that seeking a given file yields
the same audio as playing through to that point — written for MP3, whose bit reservoir makes
symphonia log an underflow warning on every seek (expected; see `tasks/phase-1.md`). See `ARCHITECTURE.md`'s "Known gaps" for tracked limitations (Opus,
unknown track durations, output-device changes, WebGL2 fallback, scope aspect).

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
