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

**Phase 3 M1 is done and user-confirmed** (2026-08-18): the visualizer host.
Confirmed live on **both shells**: fullscreen enters and leaves cleanly, Esc
comes back out of it, and Auto gain "looks right on real music". That closes the
one thing no automation here can reach -- the in-app browser pane refuses
`requestFullscreen` and the desktop webview's buttons are out of reach of both
the automation browser and UI Automation, so fullscreen was always going to be a
person's observation. `player-render` is now split along one seam (`visual.rs`) --
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
Two bugs came out of the first live desktop run and are fixed. **Esc froze the
app**: `tauri-plugin-global-shortcut` calls the handler while holding the mutex
`unregister` needs, so the handler now spawns a thread and does nothing else.
**The figure was small in a fullscreen window**: the camera's fixed 45-degree
field of view is now *derived*, fitting the trace's own reachable extent (a
diamond, since |side| + |mid| <= sqrt(2)) at 90% of the frame -- which lands
within a few pixels of the old value where it applied, and provably cannot clip
at any window shape. Two other levers were measured and rejected in the code:
lengthening the ribbon on a wide viewport made the figure *smaller*, and pulling
the camera back buys eight percentage points for the sense of depth. What
remained was arithmetic -- the frame is sized for a full-scale signal and music
is not full-scale -- so **display gain** landed on the user's decision: x1/x2/
x4/x8 plus an opt-in Auto (instant attack, two-second release, timed in seconds
so both shells recover identically), living in `player-render::gain` with the
panel saying "display only" whenever the setting stops being a measurement.

One more difference showed up in that live run and is fixed: the two shells drew
different backgrounds (near-black on the web, grey on the desktop) because a
clear value is *linear light* and only one of the two surfaces is sRGB -- the
web's canvas is `bgra8unorm`, the desktop window's surface is not. The
background is now defined as the colour a screen should show (rgb(5,5,8), in
`attachments::BACKGROUND`) and converted per surface format, and both smokes read
that value back.

Verified: **21/21** `cargo run -p player-render --example visual_smoke` and
**25/25** at `/visualizer.html` in a real browser, agreeing figure for figure (a
switch is byte-identical to a freshly built renderer on both). Two of
ARCHITECTURE.md's "Known gaps" -- the scope's aspect and its channel pairing --
turned out to describe the pre-M6 renderer rather than the current one; they are
struck through with the checks that now pin them. **What is still to confirm is
fullscreen itself**, on both shells: the automation browser refuses
`requestFullscreen` outright (the pane hosts the page without
`allow="fullscreen"`) and the desktop webview's buttons are out of reach of the
automation, so Esc, the focus-loss exit and how it all looks under real music
are a person's observation. **All of that is now user-confirmed on the desktop
shell (2026-08-18)**: fullscreen and Esc work, and Auto gain "looks right on real
music". `cargo run -p player-render --example visual_smoke` checks every visual
in the registry; `/visualizer.html` is its web half.

**Phase 3 M2 is done and user-confirmed on both shells** (2026-08-19): spectrum bars,
the gallery's second citizen and the milestone that tests the host rather than
extending it. Where the vectorscope draws raw samples geometrically, the bars
draw **analysis** -- `player-core`'s own spectrum bands, the same array the
studio Spectrum panel shows, at the same instant. That is PLAN.md §4's promise
stated as a picture for the first time. `VisualFrame` grew `levels` and
`band_centres` (its designed growth -- `visual.rs` named M2 as the reason it is a
struct) plus `band_position`, and **nothing else outside `player-render`
changed**: no new command, no new UI control, no new persisted setting, no new
meter, and the gallery listed the new visual with no UI edit at all.
`visuals/bars.rs` is instanced boxes with peak caps that fall in *seconds* (12
dB/s, display state inside the visual -- the caps are an animation over levels
already measured, not a meter), a display window of -72..0 dBFS that is the
visual's own rather than the meter's -120, display gain applied as a **decibel
offset** since a bar display is logarithmic, and octave marks placed by frequency
through the frame's own band centres. M1's camera fit moved to `camera::fit` and
is now shared; the vectorscope's six fit tests pass unchanged against it.

That milestone also closed the other half of 2026-08-18's background bug. That
day's fix defined the *clear colour* as what a screen should show and converted
it per surface format -- but a fragment shader writes linear light with exactly
the same consequence, so every colour in every visual was washed out on the
desktop's sRGB surface and saturated on the web's `bgra8unorm` canvas. The
conversion now lives once in `visuals/shared.wgsl`, prepended to every visual's
shader by `attachments::shader_source`. The evidence it was real: M1 reported the
vectorscope's mono figure as 196px natively against 195px on the web and its 16:9
scaling as 2.26x against 2.24x; both shells now report **189px and 2.35x** --
identical, not merely close.
Verified: **33/33** `cargo run -p player-render --example visual_smoke` (which
now runs the real `player-core` analyser, so a tone that lights a bar is measured
by the same code the meters use) and **38/38** at `/visualizer.html`, agreeing
exactly on every bars figure; 258 tests, clippy and fmt clean on both targets.
**Confirmed live on both shells (2026-08-19): "works as expected and looks good",
with no revision pass wanted** -- so the constants this milestone chose (the
three-quarter camera, the -72dB window, the 12dB/s caps, the octave marks) are
settled by observation rather than only by argument, and M3's waterfall inherits
them as a starting point rather than as open questions.

**Phase 3 M3 is done and user-confirmed on both shells** (2026-08-19): the spectrogram
waterfall, and the first visual that *keeps* anything. M2 grew the feed by two
slices; M3 grew nothing -- the levels, their axis and the elapsed time were
already there, so the whole milestone landed inside `player-render` (the registry,
`visuals/waterfall.rs` + `waterfall.wgsl`, the extracted `visuals/scale.rs`) with
one file outside it: the web *check*. No shell, no UI, no binding, no command, no
new meter, and both galleries listed the third visual with nothing edited -- the
second milestone running, which is what turns M1's seam from a claim into a
property.
The history is an **R32Float ring of rows read in the vertex shader**
(`textureLoad`, no sampler, no vertex buffer of heights: `vertex_index` *is* the
grid position), so a frame costs one row and one `head`. Rows advance on a
**clock** -- 20 a second over 120 rows, six seconds -- never per frame, because
the desktop redraws at 60Hz and a browser's animation callbacks run at whatever
the display does; the leftover time is carried, so sixty frames of a sixtieth
move the history exactly as far as one frame of a whole second. The texture holds
**dBFS as measured**, with the -72..0 window and the display gain applied in the
shader, so turning gain up re-maps rows written long before the setting changed
rather than leaving a seam. A seek is drawn as the discontinuity it is: nothing
resets the history, and the host was not asked for a signal that would.
Two things came out of building it. The bars' three-quarter camera was inherited
and was **wrong in a scene with depth** -- an off-axis eye compresses the low end
of the spectrum against the high end, which measured as an 80Hz tone at -94px
from centre against a 6kHz tone at +101px... and no check would ever have caught
it, because the ridge was on the right side, in the right order, at the right
height. It was found by drawing the picture and looking at it. And the octave
marks are **invisible under a terrain** (a terrain covers its own floor), so they
became a ruler in front of the near edge, lengthened until there were enough
rail-coloured pixels in a real frame to read.
Verified: **48/48** `cargo run -p player-render --example visual_smoke` and
**54/54** at `/visualizer.html`, agreeing to the pixel on every figure that is a
*position* (the ridge at x=213 and 421 on both, its centre at row 205 on both,
zero bytes differing on both frozen-terrain checks) and to under a percent on the
figures that count anti-aliased area; 263 tests, clippy and fmt clean on both
targets. **Confirmed live on both shells (2026-08-19): "six seconds looks good,
it stays legible and no fog, waterfall falls as expected"** -- which settles the
three things no readback could reach. Six seconds of past is the right amount,
so a later time-axis visual inherits it rather than reopening it; the terrain
survives dense material, which is a waterfall's one gradual failure mode; and
the clock the rows advance on reads correctly to a person, where the smoke could
only prove it arithmetically. The centred camera -- the change this milestone
made by looking at the picture rather than by failing a check -- is confirmed on
real music too.

**Phase 3 M4 is done and user-confirmed on both shells** (2026-08-19): the oscilloscope
tunnel -- the waveform wrapped into rings and flown through, and the first visual
whose **camera moves**. The milestone's assertion held for the third time:
everything is inside `player-render` (the registry, `visuals/tunnel.rs` +
`tunnel.wgsl`, the extracted `visuals/history.rs`) with one file outside it, the
web *check*. No shell, no UI, no binding, no command, no new frame field, no new
meter, and both galleries listed the fourth visual with nothing edited.
Two of this crate's habits invert inside a tube. **`camera::fit` is not used**:
it frames a scene seen from outside, with corners that can leave the frame, and
here the scene surrounds the eye and the failure is seeing *past* it at the
edges. So the field of view is chosen and the scene is fitted to it -- inside a
straight tube a ray at angle `theta` meets the wall at `radius / tan(theta)`, so
the corners are wall as long as the tube is longer than `radius / tan(corner)`,
and the middle is the opening by design (it fades to the background rather than
showing a hole). Checked as angles in a unit test, as a `debug_assert` where the
projection is built, and as border pixels at seven window shapes on both shells.
And **the path is a pure function of ring index**, evaluated in Rust for the
camera *and* for the geometry it hands the shader, so the eye cannot drift out
through the wall it is flying down. History is `visuals/history.rs` --
M3's ring buffer, extracted here on the second caller, `RowClock` and all; the
waterfall moved onto it with its 48 checks unchanged.
Three things came out of building it, two of them invisible to every check. The
**field of view** was the surprise: at this crate's usual 60 degrees the near
wall sits outside the frustum entirely and the picture reads as a gradient with a
hole in it. A **tube can bend out of its own sight** -- the first path constants
curved it by a radius every hundred rings, hiding the far end behind the near
wall, so the cycle counts are a curvature budget rather than a taste. Both were
found by drawing the picture and looking at it. The third was a check: the host
contract compared a switched-into visual against a renderer that had *already
drawn twice*, which is comparing two histories rather than two renderers, and it
passed for the waterfall only by luck of that visual's slower clock. Both shells
now start both sides from a renderer that has drawn nothing.
Verified: **60/60** `cargo run -p player-render --example visual_smoke` and
**67/67** at `/visualizer.html`, agreeing to a pixel on every position (the loud
rings centred on row 210 against 211, zero background pixels in the border across
seven window shapes on both, zero bytes differing across two frames of a stopped
feed on both) and to under a percent on the area counts; 276 tests, clippy and
fmt clean on both targets. **Confirmed live on both shells (2026-08-19): "speed
and bend rate look good, no uncomfortable feelings, looks good in both desktop
and web"** -- which settles the one thing this milestone could not check for
itself. A tunnel is the first visual here that can make a person unwell, and
comfort is not a figure any readback produces; the speed (3.6 units a second,
which is the ring rate times the ring spacing rather than a dial) and the
curvature budget are now settled by observation. So are the two constants found
by drawing the picture rather than by failing a check: the 80-degree field of
view and the three-and-two bend cycles.

**That closes PLAN.md section 4's built-in suite** -- vectorscope, bars,
waterfall, tunnel -- all four user-confirmed on both shells, and the host
unchanged by the last three.

**Phase 3 M5 (musical intelligence) is done and user-confirmed on both shells**
(2026-08-20), and turned the phase around: its work is analysis rather than rendering. `onset.rs` is the
detection function and the features on its grid (rectified spectral flux over
log-compressed magnitudes, transients, onset picking, the spectral centroid) and
`tempo.rs` is the tempo and beat phase taken from it (autocorrelation, harmonic
scoring, a log-normal prior at 120 BPM, an interpolated peak, and a PLL for the
phase). Both run on a **hop clock derived from the output rate** -- 480 frames
at 48kHz, 441 at 44.1kHz, both exactly 100Hz -- rather than on the caller's
cadence, because flux measured between two drains is a different measurement on
every drain and the two shells drain on different clocks; an internal
accumulator makes chunking irrelevant, which is asserted rather than claimed.
The display spectrum is deliberately not reused (smoothed, log-binned,
irregularly timed) and the detector's own bins are never published.
`MusicalSnapshot` joins `MeterSnapshot` on the one drain and resets with it on a
broken listen. The plumbing is one of each thing per shell: a `musical` command
natively, a six-float shared-buffer region on the web, `VisualFrame::with_musical`
in `player-render`, and a studio panel (`Musical.tsx`) of its own.
**No visual reads the new frame fields yet** -- M6 drives scenes with beats,
with M6's decisions -- so both smokes check instead that the addition is *inert*:
64/64 in `visual_smoke` (was 60/60) and 71/71 at `/visualizer.html` (was 67/67),
byte-identical with and without a tempo in the frame.

Verified the analysis suite's way, against known answers: **16/16** in
`cargo run -p player-core --example musical_smoke` and **16/16** at
`/musical.html`, on fixtures generated from the same arithmetic down to the
noise bursts' seed -- 120.00 BPM against 120.00, 89.71 against 89.70 after a
gapless hand-off into a 90 BPM track (a tempo no integer autocorrelation lag can
express), 120.00 against 120.00 with an offbeat on every eighth, and a 1kHz
tone's brightness at 1061Hz against 999Hz. 305 tests, up from 276 at the end of
M4; clippy, fmt, `tsc` and the web build green.

Those two harnesses earned themselves twice over. The web one caught a tempo of
**122.4 BPM at confidence 1.00 during a held tone** -- with zero onsets detected
-- because an autocorrelation normalised by its own energy scores a periodicity
in numerical residue as confidently as a drum; the native build read no tempo for
the same fixture, which was luck rather than agreement. `tempo.rs` now says what
a tempo is: fewer than four onsets in the window means no tempo, whatever it
correlates with. And the native one caught the spectral centroid counting the
*noise floor* -- a 1kHz tone read 1148Hz through the real playback path where the
unit test on clean audio read within a bin -- so bins under `MAGNITUDE_FLOOR` are
now left out of the centroid rather than floored into it.

**User-confirmed live on both shells (2026-08-20)**, twice -- once on the
plumbing ("works as expected") and again on a longer look after the two fixes:
*"It looks smoother on web now than it did. Now it looks smooth in transitions,
quiet passages are smoothed out visually. All pass."* The second look is what
confirms the tempo gate from the outside: the readout stops asserting a tempo
where there are no beats to assert one from.

That first look also found a trap worth knowing: **a stale page.** The browser
was running the bundle from before a `npm run wasm` rebuild, which *skips a
little on transitions* rather than failing -- a mismatched wasm module and
shared-buffer layout look like a performance problem from the outside. Reload
after rebuilding; WORKFLOW.md says so at the step.

**Phase 3 M6 is done and user-confirmed on both shells** (2026-08-22): the
particle field, the raymarched solid, and the beat-reactive camera M4 deferred
here. **95/95** `visual_smoke` (was 64/64), **104/104** at `/visualizer.html`
(was 71/71), and **18/18** in both halves of the analysis smoke. Confirmed live:
density right across "a lot of different types of music", the pulse "more of a
breathing than flashing", the solid "reads as shapes and looks like it's
breathing", the tunnel's new camera "gave no uncomfortable feelings", fullscreen
on the solid good, and swapping between all six smooth.

`visuals/particles.rs` is this workspace's **first compute shader** and the first
scene whose state is advanced by arithmetic rather than replaced by a
measurement. Its limits were checked before anything was built on them and were
already in hand -- both shells request `wgpu::Limits::default()` and have had it
since Phase 0 -- and the particle buffer is still bound as an instance vertex
buffer rather than a vertex-stage storage binding, which is the one limit
compatibility mode grants zero of. Three rules shape it: the simulation steps on
its own 120Hz clock with the remainder carried, an **onset is idempotent**
(`musical.onset` is *held* in the published snapshot until the next drain, so a
180Hz display reads the same one three times -- it enters only through
`max(energy - decay*dt, onset)`), and a particle is a pure function of its spawn
ordinal. `visuals/solid.rs` is the SDF half, and decision 46 is its whole design:
the surface's radius at a latitude **is** the analyser's band at that frequency,
so bass-heavy material measurably draws wider at the foot than at the crown (0.50
against 0.35, identical on both shells) and halving the band count changes the
shape by nothing. `visuals/tunnel.rs` gained the beat as a bounded offset on the
*eye* rather than on its ring clock -- and is byte-identical to M4's tunnel
whenever no tempo is playing, which is what keeps M4's sixty checks meaningful.
Nothing outside `player-render` changed for either new visual, five milestones
running.

Three bugs, and how each was found is the milestone's lesson. **A check written
for a new visual found a clock bug in two confirmed ones**: `RowClock` counted
seconds and divided by an `f32` reciprocal a hair too large, so a frame of
exactly one second at 120Hz advanced 119 steps instead of 120 and three exact
thirds of a sixtieth advanced one instead of two. It counts rows now, and the
waterfall's and tunnel's checks pass unchanged over the fix. **A live look found
the meter lying**: sustained organ material read a confident 142 BPM because
`onset.rs` normalised `transient` by its own recent peak alone, so flux thirty
times too small to be an attack still read as full strength -- the peak now has
an absolute floor (30.0, measured) and `tempo.rs` says what a beat is (a quarter
strength, or it counts toward nothing). Both analysis smokes gained a pad
fixture. **And fullscreen was "smooth but blurred"** because a spark is a quad in
the world and the camera is fitted to the field, so a bigger window magnified the
picture instead of resolving it; sparks now hold their size to the eye and
emission follows the viewport's area.

Decision 47's resolution cap was set with a stopwatch, and it moved the number up
threefold: timed against the vectorscope at matching sizes, the march costs under
a millisecond even at 4K, where the cautious first guess would have softened
every 1440p fullscreen window -- the exact complaint the live look had just
raised about the particle field.

One constraint to carry forward: **the web harness has a GPU device budget**.
Four `WasmRenderer`s per visual worked at four visuals and made Dawn answer
`E_OUTOFMEMORY` at five, so the host contract now uses two per visual and each
per-visual section runs on one renderer reset in place. M7 should be designed
against that from the start.

**M7 (MilkDrop `.milk` compatibility): decision 7's spike has run (2026-08-22),
and M7 is planned against what it reported** -- see `tasks/phase-3.md` for the
findings and for the three-way split and ten decisions (49-58) they support.
`cargo run -p player-render --example milk_spike` is the report, run against the
user's own presets in `milkdrop/` -- local test data, gitignored, because
**decision 57 is that no preset ever ships with the app** (user decision
2026-08-22; the licence check that raised the question found the MilkDrop3
repository declaring none at all). Supporting the format is licence-clean;
distributing someone's `.milk` file is simply not something this app does. Two
consequences: the preset library is the *only* way a preset arrives, so
"point at a folder" is a first-class flow rather than a setting, and the gallery
entry has to say what it needs when no presets are loaded.

The short version. **The format and the equations are nearly done and were the
smaller risks**: `milk/parse.rs` reads 5/5 presets and `milk/eel.rs` compiles
**31/31** of their code blocks with zero unknown functions -- 34 new unit tests,
109 in `player-render`, up from 75. EEL's two load-bearing quirks are verified
against Cockos' WDL source rather than memory (`NSEEL_CLOSEFACTOR` is 0.00001
and governs `==` *and* truthiness; memory is 128 blocks of 65536, reached
lazily) and one the corpus proved by itself (names are case-insensitive -- a
preset sets `MINBPM` and reads `minbpm`). **The equations are affordable and the
obvious optimisation is the wrong one**: worst frame 2.3ms, 13.6% of a 60fps
frame -- but in the two most expensive presets `per_pixel` costs *nothing*, the
budget going to `per_frame` (a 256-resonator tempo bank, 522k nodes in one
frame) and to custom waves. Shrinking the mesh, the lever M6 had just used for
the raymarch, would have bought nothing on either.

**The frame's skeleton stands**: `milk/frame.rs` runs MilkDrop's feedback loop
under wgpu -- two `Rgba16Float` targets ping-ponged through a warped mesh, plus
a composite -- and a seeded square advanced 30 frames under a real preset's
`per_pixel` lands where its arithmetic says (`dy=0.005` predicts 19.2px of
drift; measured 19.6). What remains is MilkDrop's *full* per-vertex transform,
to be read from its own source rather than anyone's memory. The readback earned
itself on its first run, third milestone in a row: every preset moved correctly
and the whole picture was still drawn at quarter scale in one corner, and the
only thing that said so was the seeded square's centre reading (31.5,31.5) where
a 128px frame puts it at (63.5,63.5).

**The shader translation is the milestone**, and is why M7 splits three ways
(M7a a preset draws, M7b a preset draws as itself, M7c waves/shapes/library).
244 shader lines use only 15 distinct constructs, but two of them --
`float2 water = tex2D(...)` truncating a float4 implicitly, and `1-uv` promoting
a scalar against a float2 -- are illegal in WGSL and need the type of the
surrounding expression, so the translator is a small typed compiler rather than
a substitution table.

One constraint the spike did not raise and the user did: **presets arrive by the
hundred or the thousand.** That shapes decisions 49-51 -- a preset is selection
state (`VisualKind` is a fixed enum and cannot name one), the collection is a
library scanned off the audio and UI threads like `library.rs`, and a stranger's
program is sandboxed by an evaluation budget so a bad preset ends its own frame
rather than the app.

**Phase 3 M7a is done and user-confirmed on both shells** (2026-08-22): a preset
draws. `VisualKind::MilkDrop` is the gallery's seventh entry and the first whose
*scene is data*. Verified **112/112** `visual_smoke` (was 95/95), **123/123** at
`/visualizer.html` (was 104/104), and **23/23** in the renamed `milk_smoke`;
155 unit tests in `player-render`, up from 109.

**The licence check decided where to read from, and the answer was not the
obvious one.** Decision 57 had found the MilkDrop3 repository declaring no
licence -- and that is the repository the test presets came from. **MilkDrop 2
is Nullsoft's own and is BSD 3-clause**, so the transform, the eight waveform
modes, the composite and every scalar default are read out of *that*
(`milkdropfs.cpp`, `plugin.cpp`, `state.cpp`), with the attribution in
`milk/mod.rs` and in a new `THIRD-PARTY.md`. Two repositories, two answers, and
they look identical until someone checks.

The spike's reduced transform was wrong in **ordering** as well as in terms:
`zoomexp` makes the zoom depend on the radius (and collapses to a plain zoom at
its default, which is why its absence was invisible); four sine terms ripple the
picture *after* the stretch and *before* the rotation; and the whole thing runs
in aspect-corrected space and is un-corrected at the end, which is the only
reason a rotating preset draws circles rather than ellipses on a wide window.
`warp=1` is MilkDrop's **default**, so a preset that writes no motion at all
still ripples -- worth knowing before writing any check about the loop.

**One line of MilkDrop's transform is deliberately not copied**: its half-texel
offset is a Direct3D 9 rasterisation artefact, and wgpu aligns pixel and texel
centres already. Copied into a feedback loop it is not a correction but a
displacement applied *every frame* -- the picture crawls half a pixel up and
left, 15px of a 128px frame in half a second, looking like nothing but slow
drift. Found by a readback asserting where a seeded square *was* after thirty
frames; there is now a unit test that the transform is a fixed point over a
hundred. Fourth time in this phase a figure caught what an eye would not have.

New modules, all inside `player-render`: `milk/warp.rs` (the transform and the
mesh's fixed geometry), `milk/state.rs` (the ~70 scalars as MilkDrop actually
treats them -- not a struct the renderer reads but the values the per-frame
variables are *reset to*, so `q` carries from init into every frame rather than
accumulating and `per_pixel` starts each vertex from the frame's own motion),
`milk/wave.rs` (the eight built-in modes, the only thing putting new light into
the loop), a rewritten `milk/frame.rs`, and `visuals/milkdrop.rs`.

Decision 49's seam change landed once and deliberately: `Visual::set_preset`
with a default body, and a selected preset beside the selected visual on the
host, so it survives a visual swap. One command per shell -- the desktop takes a
path and reads the file in Rust, the web takes the text because a browser has no
path to give -- both ending at the same `Renderer::set_preset`. With nothing
selected the visual draws MilkDrop's *defaults*, which are a real preset, since
a fresh install has none (decision 57).

**What the corpus check can claim is narrower than it looks, and that is
right**: only 1 of the 5 presets draws through M7a's own path. Three set
`fWaveAlpha=0.001`, deliberately under MilkDrop's own 0.004 threshold, because
their picture comes from custom waves (M7c) and shaders (M7b); a fourth shows
its waveform only above average loudness and is correctly invisible on a steady
test tone. An earlier gate read the alpha off the `Runtime` and failed against
entirely correct behaviour -- the gate and the draw have to ask the same
question, so it asks by building the waveform.

**Confirmed live on both shells** (2026-08-22): *"some of the ones that load look
right. Waveform matches the real music on the ones that read it. Fullscreen looks
good."* That settles the three things no readback could reach -- the transform in
motion, which is the milestone and whose failure mode no still frame shows; the
waveform's amplitude against real music, which is what licenses this build's own
`bass`/`mid`/`treb` normalisation; and the 2048-pixel feedback cap at a real
fullscreen window. *"Some of the ones that load"* is the honest figure and the
expected one: a preset drawing without its own shaders is a preset drawing
approximately, which is exactly what M7b is for.

That look also produced the fact that reshapes the rest of M7. The corpus is not
five presets, it is **the MilkDrop3 collection plus collections gathered over the
years -- a wide mix from every version of the format**, which the user reports is
how essentially everyone who did not build a collection themselves comes to have
one. The format is also still alive: presets are still being written and the
collections are still growing. Two consequences. The construct inventory the
spike measured (244 shader lines, 15 distinct constructs) is a sample of five and
is not a specification, so **M7b begins by surveying the real corpus** rather than
by designing against that sample. And a translator meeting a construct it does
not know must *say so per preset and keep the app running*, because against
thousands of strangers' files written across two decades that case is normal
rather than exceptional.


**Phase 3 M7b is built and awaiting a live look** (2026-08-23): a preset draws
*as itself*. **118/118** `visual_smoke` (was 112/112), **129/129** at
`/visualizer.html` in a real browser (was 123/123), 266 unit tests in
`player-render` (was 155 at M7a).

The milestone was designed against a **measurement rather than a sample**. The
user's own collection -- 11,217 presets, ~6,100 distinct shader programs -- is
in `milkdrop/collection` (gitignored, decision 57), and
`cargo run --release -p player-render --example milk_survey -- <folder>` is what
reports on it. Every figure below comes from that run, and the survey is why
they are figures at all: the M7 spike's inventory of "15 distinct constructs"
was five files from one era, and six of the fifteen constructs the grammar
actually needed were absent from it.

**The corpus reads: 11,214 of 11,217 presets parse whole.** The three that do
not are genuine syntax errors MilkDrop rejects too. Built in the order the
corpus dictated rather than the usual one -- statements first, preprocessor
last (it was ranked at 3.1% and turned out to be 523 of the final 528
refusals). `milk/hlsl/` is lexer, type lattice, environment, preprocessor,
grammar and emitter; the environment (`env.rs`, `prelude.wgsl`) is MilkDrop's
`include.fx` written down, which turned out to be **macros over fourteen
constant registers** rather than the uniforms the plan assumed.

**The corpus translates: 99.5% of shader bodies, and 98.2% of presets draw
through their own shaders.** The first run compiled *nothing* -- every shader
failed on `invalid left-hand side of assignment`, because the emitter
parenthesised the base of every accessor. One defensive habit, right for
reading an expression and fatal for writing to one, taking the whole corpus
down at once. Nine more followed, and what they share is the point: **each is
invisible at five presets and obvious at eleven thousand.** WGSL forbids
assigning to a swizzle at all; parameters are immutable there and mutable
copies in HLSL (presets assign to `uv` constantly); HLSL's uniforms are
writable globals (`q25 = 1;` is in 504 bodies); `float2x2(_qb)` is a
constructor rather than a conversion; a bool is a number in three separate
places.

**The matrix mapping is the one the folklore gets backwards.** Both languages
put the first constructor group at index 0 -- HLSL's first *row*, WGSL's first
*column* -- so taking HLSL's rows to be WGSL's columns keeps the two numbers in
order and makes construction and indexing agree. The entire price is that
`mul(a, b)` emits `b * a`.

**And it runs.** `milk/shaders.rs` is the environment on the GPU and
`milk/noise.rs` is MilkDrop's procedural noise, read from `AddNoiseTex` rather
than invented. The blur chain is not decoration -- `GetBlur1` is the corpus's
second-commonest call after `tex2D`, and a build that bound black there would
have passed every other check. A blur texture holds a *remapped* image, so the
chain's `fscale`/`fbias` and the prelude's `GetBlur` constants are one mapping
and its inverse. MilkDrop's blur offsets are deliberately **not** copied: they
are the D3D9 half-texel rule again, recognisable only because M7a had already
met it in the warp transform.

Four divergences, each written down: `textureSampleLevel` rather than
`textureSample` (a quarter of the corpus branches, and WGSL forbids implicit
derivatives under non-uniform control flow); every preset name prefixed `p_`;
**a guard counter on every loop**, because a runaway loop in a fragment shader
is a hung display rather than a slow frame; and a texture this build does not
load drawing a placeholder while being *named* through `Visual::preset_report`.

Compilation is **per pass**, because 605 presets give one pass a shader and
leave the other fixed-function. A warp shader owns its own fading (so `fDecay`
is not applied twice) and a composite shader replaces the fixed-function
composite (so the video echo and gamma are not applied over it).

The six new pixel claims are the chain end to end, and the two shells agree:
a composite shader paints r=255 on both, a warp shader owns the feedback at
g=255 on both, `q1` from `per_frame` arrives on both, the blur spreads
32079/2328 against 31708/2279, and the 3D noise volume reads **mean 198, spread
30.4 on both** -- identical rather than close, which is the deterministic
generator proving itself byte for byte across two GPUs.

`cargo run --release -p player-render --example milk_wgsl -- <preset> [warp|comp]`
prints what one preset's shader became and runs naga over it; it is what found
most of those ten bugs. **What is still to confirm is a person watching their
own presets** -- the same thing M7a needed, for the same reason.

**Phase 3 M7b is live-looked, and M7c is re-planned** (2026-09-01): the user
ran their own collection through the build. *"A lot of the high-shader presets
worked, but not all; most of the others did not."* That is the corpus seen from
outside: M7b's 98.2% is shader *translation*, and a translated shader sampling a
texture pack still draws the placeholder, so "not all" of the shader presets
look like themselves; the "others" -- the non-shader presets that draw through
**custom waves and custom shapes** -- are M7c's ground and do not exist yet, so
they fade toward black.

**Decision 56 is reverted (decision 59).** Sprites and texture packs are in
scope. The app ships no preset (57) and no image, so an image a preset
references is the user's own file in their own folder -- loading it is engine
work, not a content-distribution question. The new MilkDrop on GitHub makes
images first-class (presets are created in-app referencing whatever images the
author likes), so "a preset may reference an image" is the format's current
truth. M7c's order is re-set by "how many presets stop looking wrong": custom
waves/shapes, then texture packs + sprites, then the preset library. See
`tasks/phase-3.md` "M7c -- re-planned".

**Design has a plan at last** (`tasks/design.md`). The app's chrome has never
been designed -- one centred 560px column in `App.tsx`, in the order things were
added. The plan consolidates it into a persistent header (transport/now-playing/
volume) + visualizer left + library/sources right + a collapsible Studio section
for the five meters, working toward PLAN.md §4's listen/studio split. D1 is
already done: `index.css` is re-tokenized to the latentbeats.com palette
(dark-only, blue `#58a6ff` accent, matching latentmastering.com/latentmixer.com),
the `prefers-color-scheme` split is dropped, and the hard-coded colours in
`App.css` became tokens. D2 (two-column layout) is next.

**Phase 3 M7c first item is done and user-confirmed on both shells**
(2026-09-01): custom waves and custom shapes. `milk/custom.rs` is `WaveRig` and
`ShapeRig`, each with its own variable pool (its own `Symbols`/`Context`,
MilkDrop's three-pool rule). The format was confirmed against the corpus rather
than guessed -- `wavecode_N_<field>` scalars beside `wave_N_per_point1..N` code
(and `shapecode_N_*` + `shape_N_per_frame`) -- and `value1`/`value2` are the
waveform samples, or the spectrum when `bSpectrum` is set. `q1..q32` seed from
the preset's per-frame result and `t1..t8` from each rig's own init; a wave's
per-point block runs on one context so `x`/`y`/colour carry point-to-point.
`frame.rs`'s `draw_overlays` now takes `waves: &[Wave]` + `shapes: &[ShapeGeom]`
with a new additive shape pipeline; shapes are instanced n-gon fans (centre-to-
rim colour) plus an optional border. A shape with no per-frame code draws at the
file's defaults, and a disabled wave/shape draws nothing. Textured shapes are
deferred. Verified: **122/122** `visual_smoke` (four new M7c checks: the wave's
`y` orientation -- 0 bottom/1 top, `value1` carrying the waveform sample, the
shape's centre/radius, and the `enabled=0` gate) and **133/133** at
`/visualizer.html`, agreeing figure for figure; 272 unit tests, `milk_smoke`
168/168 over 150 real presets, clippy/fmt clean on both targets. **Confirmed
live on both shells: some presets work well and are not mirrored.**

**Phase 3 M7c second item is done** (2026-09-02/03): texture packs and sprites
(decision 59). A preset's `sampler_fw_clouds` now resolves to a layer of one
`texture_2d_array` -- 8 layers of 512-square, the corpus's most demanding preset
naming six -- instead of a single grey placeholder, and a `[SPRITE_BEGIN]`
block draws its image as a textured quad over the preset. `binding_array<T>`
was tried and rejected: wgpu's default limits forbid it
(`max_binding_array_elements: 0`), and `texture_2d_array` keeps the
sampled-texture count at eleven. The emitter maps each name to a layer via
`emit_with_textures` (a sorted preset-wide list shared across both passes, so a
warp and a composite read the same texture from the same layer); `milk/image.rs`
decodes PNG/JPEG (the whole of what the corpus ships); the host reads the files
from the preset's own folder (`textures/`/`sprites/` subfolders, or a sibling
of a `presets/` folder, walking up to two ancestors). `milk/sprite.rs` is
`SpriteRig` -- the sprite's EEL code in its own variable pool (the ten frame
inputs, no `q`/`t` bridges), the transform read out of MilkDrop 2's
`DrawUserSprites` (image-aspect, rotate, translate, screen-aspect) -- and
`frame.rs` gained a sprite pass: one pipeline per blend state (0/4 alpha, 1
decal, 2 additive, 3 srccolor, 5 multiply, 6 subtract, 9 darken; MilkDrop 3's
7/8/10 fall back to alpha), the colour key applied in the fragment shader, drawn
into the feedback buffer so a sprite is part of the loop rather than pasted
over the finished frame. The web shell's `setTextures` is the engine half of
the same path -- a browser's single-file input cannot resolve a folder, so the
running web app catches up with the preset library (M7c item 3). Verified:
**124/124** `visual_smoke` (a texture-pack check feeds a red PNG to
`sampler_fw_test` and reads it back; a sprite check draws a green quad at
centre), 280 unit tests (up from 272), `milk_smoke` 369/371 against the full
353-preset set (the two failures are the pre-existing EEL budget/`memcpy`
limits), clippy/fmt clean, wasm check clean. **Texture packs are
user-confirmed live on the desktop (2026-09-02): "tested on a cloud and worm
preset and it works."** Sprites are **not yet live-looked** -- the y-orientation
and the 7/8/10 blend-mode fallbacks are the two things a readback cannot
settle. The web smoke's sprite check and the docs are the remaining M7c item 2
work; the preset library (item 3) is next.

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

`cargo run --release -p player-render --example milk_smoke -- <folder>` runs a
whole folder of `.milk` files: every preset parses, every block compiles, every
one draws a frame without taking the app with it, and the worst frame cost is
reported. There is **no default corpus in the repository** -- no preset ships
with the app (decision 57) -- so point it at a folder of your own.

`cargo run -p player-core --example musical_smoke` checks the tempo, the beat,
the onsets and the brightness through the whole playback path against
synthesized click tracks with known answers; `/musical.html` is its web half,
generating the identical fixtures. Both report 18/18 since M6 added the
sustained-pad fixture -- material with no attacks in it, which read as a
confident 142 BPM until a live look caught it.

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
cd app && npm run wasm   # rebuilds it for wasm32 via wasm-pack -- reload any
                         # page left open, or it keeps the old module (WORKFLOW.md)

cd app && npx tsc -b && npm run build   # frontend
cd app && npm run dev                    # web dev server

cargo tauri dev   # desktop dev build -- stop any standalone `npm run dev` first
                  # (port 5173; see WORKFLOW.md). On Linux only, this needs an
                  # env-var workaround for Wayland -- also in WORKFLOW.md.
```

Full detail (including the `cargo tauri dev` gotcha) is in `WORKFLOW.md`. Coding standards are
in `CONVENTIONS.md`. Crate/module map and known gaps are in `ARCHITECTURE.md`.
