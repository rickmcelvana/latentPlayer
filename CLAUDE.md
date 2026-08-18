# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

latentPlayer — Latentbeats.com Music Player. Open source (MIT). See `PLAN.md` for the
product-level plan (audiences, feature set, phase ordering, decisions — revised 2026-08-18:
visualizer suite + fullscreen requirement, player-completeness phase, roadmap reorder) and
`ARCHITECTURE.md`/`CONVENTIONS.md`/`WORKFLOW.md` for what's actually built and how to work on it.

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
than eyeballed. (Phase 3 M1 renamed all five of those: the modules are
`visualizer.ts`/`webVisualizer.ts`/`VisualizerControls.tsx` and the checks are
`visual_smoke`/`/visualizer.html`, which run M6's eight claims among their own.)

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
ARCHITECTURE.md's "The visualizer window's lifecycle" for that decision and
`tasks/phase-1.md`'s closing sections for both. The rest of ARCHITECTURE.md's
"Known gaps" are design trades and remain open.

**Phase 2 M1 is done** (2026-08-17): the Subsonic protocol lives in
`player-core::subsonic` (IO-free, cross-target -- the desktop transport is
`subsonic/http.rs` with ureq, the web's is `fetch` in `subsonicWeb.ts` calling
the same Rust through wasm), the secret goes to the OS credential store natively
and IndexedDB on the web, and both shells have the server panel. Verified: 11/11
fake-server tests, **8/8 in a real browser** against the local Navidrome (the
eighth user-run, closing the token scheme end to end -- "a correct password
authenticates: navidrome 0.63.2, API 1.16.1, OpenSubsonic"), the native
`subsonic_smoke`, and a real round trip through the Windows Credential Manager.
**M1 is closed, user-confirmed on both shells** -- the desktop shell's live
authenticated run passed too, so the token scheme is proven through both
transports against a real server.

`cargo run -p player-core --example subsonic_smoke` checks the client against a
real server; `cargo test -p player-core --test subsonic_http` checks it against
a fake one that misbehaves on purpose (`tests/common/mod.rs`, which M2-M4 reuse
-- M4 taught it to remember repeated parameters, since a map keeps one `songId`
out of three).

**Phase 2 M2 is done and user-confirmed on both shells** (2026-08-17), and is the phase's point: a track on the
server plays through the same engine as a file on disk. `player-core::remote` is
a `PlaybackSource` over HTTP -- one 512KB chunk in memory, seeks that issue no
request, and a first response that settles whether the server ranges (206) or
ignored the range and sent everything (200, a transcoding endpoint, buffered
whole and still seekable). Nothing downstream changed: the ring, the EQ, the tap,
the meters and the scope cannot tell. The web shell's parity limit is real and
stated rather than hidden -- a browser has no `MediaSource` for the worker, so
`subsonicWeb.ts` fetches a server track **whole** before playing it, which is
what a picked local file has always cost there. Queuing a server track is the
prefetch on both shells, which is what keeps a gapless hand-off between two of
them gapless. `playlist.ts` gained a third kind of entry (an id and a format
hint) beside a path and a `File`, and "Play random tracks" on the server panel is
the entry point until browsing lands in M3.
Verified: **10/10** `cargo test -p player-core --test remote_source` (including
audio decoded over real TCP matching the same bytes decoded locally, worst
difference 0.0, and a server that stalls mid-track), plus both live runs against
the real Navidrome -- `cargo run -p player-core --example remote_smoke` and
`/remote.html` in a browser, **user-run 2026-08-17, all checks passed on both**.

**Phase 2 M3 is done and user-confirmed on both shells** (2026-08-17): browsing the server. `subsonic/browse.rs`
holds the five requests *and* the five payload shapes their answers arrive in
(`getArtists` hides artists under an alphabetical index, `getAlbum` nests songs
inside the album, `search3` returns all three kinds), so no endpoint name or
query parameter appears in TypeScript on either shell, and all five answer with
one `BrowseResult` -- which is what lets one set of rows draw a search and a
browse. Cover art goes over the existing `cover://` scheme natively, with a
`server:` prefix and an **asynchronous** handler (a server cover is fetched, and
the main thread may not wait on the network per row) plus a byte-bounded session
cache; on the web it is fetched in CORS mode and wrapped in a blob URL, because
this app's own `Cross-Origin-Embedder-Policy: require-corp` blocks a plain
cross-origin `<img src>` before the network sees it. The library panel gained a
source switch, and `Rows.tsx` is now every row in the app -- local or server,
same markup, same meaning for clicking one.
Verified: **7/7** `cargo test -p player-core --test subsonic_browse`, 11 new unit
tests in `browse.rs` (129 in `player-core`, up from 119), and both live runs
against the real Navidrome -- `cargo run -p player-core --example browse_smoke`
and `/browse.html` in a browser, **user-run 2026-08-17, all checks passed on
both**, the web one including the check only a browser can make (cover art from
another origin decoding under this app's COEP header). M4 (server playlists,
scrobbling, stars) is next, and is the first milestone here that writes to the
server.

**Phase 2 M4 is done and user-confirmed on both shells** (2026-08-18):
server playlists, scrobbling, stars and ratings -- the first milestone in this
phase that *writes* to the server, which is why it is checked on what arrived
rather than on what came back. `subsonic/playlists.rs` owns the five playlist
calls and their repeated parameters (`songId`, `songIdToAdd`,
`songIndexToRemove` -- a map-shaped client sends one of three and the server says
ok), `subsonic/stars.rs` owns the fact that `star` spells its id three ways
(`id`/`albumId`/`artistId`, and the wrong one stars nothing *successfully*), and
`subsonic/scrobble.rs` owns the **rule**: half the track or four minutes,
whichever comes first, never under thirty seconds, measured in time actually
played rather than the position reached -- so seeking to the end earns nothing.
That rule is a state machine both shells drive with the position they already
poll (natively in the Tauri state, on the web as a wasm object holding the very
same Rust), because four numbers written twice is two shells that eventually
disagree about what a listener heard. The library panel grew a Playlists tab and
every server row grew a star and a rating, drawn optimistically and put back if
the server refuses.
Verified: **10/10** `cargo test -p player-core --test subsonic_write`, 28 new
unit tests (157 in `player-core`, up from 129), **6/6 in a real browser** at
`/write.html` (the browser's tracker agreeing with the native one action for
action), and both live runs against the real Navidrome -- `cargo run -p
player-core --example write_smoke` and `/write.html`, **user-run 2026-08-18, all
checks passed on both**, including the one only a real server can answer:
**starring read back correct**, i.e. an album starred through `albumId` is a
mark the server actually recorded where `id` would have been answered "ok" and
changed nothing. Both smokes put the library back the way they found it, except
a *play*, which is opt-in on each because it writes to a listening history
nothing here can edit. M5 (failing well) is next and is the last of Phase 2.

**Phase 2 M5 is done, and Phase 2 is closed** (2026-08-18): failing well. Three
things that a network feature earns and this one did not have. **A stalled read
is retried** rather than ending the track -- `remote.rs` backs off 250ms/1s/3s
behind `fetch_once`, and what is worth retrying is narrow on purpose
(`SubsonicError::is_transient`: a transport failure or a 5xx/408/429, never a 4xx
and never a certificate, since a token the server rejects is still rejected three
seconds later). **Buffering is a state the UI draws** -- the audio callback and
the AudioWorklet each count consecutive silent frames, and `buffering` rides the
transport snapshot past 250ms (`BUFFERING_AFTER_SECONDS` in `stream.rs`, mirrored
in `audioSab.ts` and held to it by `/failing.html`); the web shell ORs in its
whole-track fetch, which is buffering with no ring involved. **Every failure says
which thing to change** -- `FailureKind` classifies in Rust so neither shell reads
message text, a proxy's 401 counts as a credentials problem, and a credentials
refusal *mid-session* drops the connection and says why instead of failing one
request quietly.

**Phase 3 is planned** (2026-08-18) -- see `tasks/phase-3.md` for the milestone
plan (M1 visualizer host + fullscreen, M2-M4 the built-in visuals, M5 musical
intelligence, M6 particle/raymarched scenes, M7 MilkDrop, M8 the native preset
format) and its nine decisions. The short version: the host owns the surface,
the analysis feed, selection and fullscreen while a visual owns only its scene
(`Visual` trait in `player-render`); visuals draw the metering's own analysis
and compute no DSP; fullscreen lands in M1 on both shells (Fullscreen API on
the web, `set_fullscreen` + a fullscreen-scoped global shortcut natively,
because the webview-less scope window receives no input events); switching
visuals swaps the scene, never the window; MilkDrop goes the
Butterchurn-guided route (MIT), never projectM (LGPL); AI-adaptive presets
slip to Phase 6.

**Phase 3 M1 is built and awaiting live confirmation** (2026-08-18): the
visualizer host. `player-render` is now split along one seam (`visual.rs`) --
**the host owns the surface, the analysis feed, selection and fullscreen; a
visual owns only its scene** -- so `Renderer` holds a `BoxedVisual`,
`set_visual` swaps the scene without touching the surface, and `draw_frame`
takes a `VisualFrame` each shell assembles once per frame off the tap. A visual
computes no DSP. `VisualKind` is the project's one registry of visuals (id,
name, description), read by the gallery UI, the Tauri commands, the wasm
bindings and both smokes, so no shell keeps a list of its own. The vectorscope
moved to `visuals/vectorscope.rs` behind the trait, untouched.

Fullscreen landed with the host, before any new visual, as decision 4 required:
the Fullscreen API on the canvas's wrapper on the web, `set_fullscreen` on the
existing window natively -- with Esc as a **global shortcut registered only
while fullscreen is active**, because a webview-less window is delivered no
keyboard events at all, and an exit on focus loss so that shortcut can never be
held while another application is frontmost. `scope.ts`/`webScope.ts`/
`ScopeControls.tsx` are now `visualizer.ts`/`webVisualizer.ts`/
`VisualizerControls.tsx`, and the selection persists in localStorage as a
`VisualKind::id`.
Verified: **14/14** `cargo run -p player-render --example visual_smoke` and
**17/17** at `/visualizer.html` in a real browser, agreeing figure for figure (a
switch is byte-identical to a freshly built renderer on both). Two of
ARCHITECTURE.md's "Known gaps" -- the scope's aspect and its channel pairing --
turned out to describe the pre-M6 renderer rather than the current one; they are
struck through with the checks that now pin them. **What is still to confirm is
fullscreen itself**, on both shells: the automation browser refuses
`requestFullscreen` outright (the pane hosts the page without
`allow="fullscreen"`) and the desktop webview's buttons are out of reach of the
automation, so Esc, the focus-loss exit and how it all looks under real music
are a person's observation. `cargo run -p player-render --example visual_smoke`
checks every visual in the registry; `/visualizer.html` is its web half.

The certificate answer changed a finding into a fix: ureq defaults to
webpki-roots, a bundle compiled into the binary, under which installing a
self-hosted server's CA in the OS store does nothing -- so the obvious advice
would have been advice that does not work. The desktop client now verifies
against the **platform trust store** (`platform-verifier`), which makes "add your
CA to this machine" real. No trust-on-first-use fingerprint flow, deliberately.
Verified: **10/10** `cargo test -p player-core --test failing_well` (including a
server that answers two 503s and then the music, playing through byte for byte,
and a 404 reported after one request rather than four), 9 new unit tests (161 in
`player-core`), and **6/6 in a real browser** at `/failing.html`.

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

`cargo test -p player-core --test failing_well` checks what happens when the
server misbehaves -- retries, refusals, a captive portal, a token that stopped
working; `/failing.html` is its web half and is what holds `audioSab.ts`'s
mirrored buffering threshold to `player-core`'s.

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
