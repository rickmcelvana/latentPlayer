# Phase 3 -- Visualizer suite

PLAN.md §7's Phase 3: "the gallery of §4, in order of what unlocks what. First
the visualizer host -- selection, the analysis feed, and fullscreen on both
shells -- with the existing vectorscope as its first citizen; then the new
built-ins; then musically-intelligent reactivity and the scenes it drives; then
MilkDrop compatibility and the native preset format."

Phase 2 is closed (`tasks/phase-2.md`): the app plays local files and a
Subsonic server through one engine, meters everything it plays, and fails
well. What this phase builds is the product's stated differentiator (PLAN.md
§1, §4): not one scope but a *gallery* of visuals, any of which can go
fullscreen on either shell. Decision §8.8 already fixed the two ground rules:
several visuals behind **one host abstraction**, and **fullscreen lands with
the host, before any new visual does**.

## The piece to design rather than grow: the host

Today the vectorscope is hard-coded end to end. `player-render::Renderer` owns
a surface *and* the one `Scene`; `draw_frame(&[f32], channels)` is the
vectorscope's signature, not a visual's. Natively, `src-tauri` builds one
plain window at startup, wraps it in a `Renderer`, and a redraw thread polls
`Playback::scope_window` forever. On the web, `webScope.ts` owns a
`WasmRenderer` and a rAF loop reading the same window from the SAB. The UI
(`scope.ts`/`ScopeControls.tsx`) can say how big it is and whether it shows --
nothing can say *what* it shows. And the two shells' surfaces are genuinely
different things (an OS window vs. a canvas), which is exactly the kind of
difference that, left undesigned, becomes two visualizer implementations that
drift.

The host divides that world in two, along one seam:

**The host owns four things: the surface, the analysis feed, selection, and
fullscreen. A visual owns only its scene.**

Concretely:

- **`Visual`, a trait in `player-render`.** What today's `Scene` already is,
  named: create against a device/format/size, `resize`, and
  `draw(&mut self, gpu, target_view, frame: &VisualFrame) -> CommandBuffer`.
  The vectorscope's `Scene` becomes the first implementor, moved not rewritten
  -- its camera, depth buffer, MSAA and trace stay its own business. A visual
  that needs internal history (the waterfall) keeps it inside itself; the feed
  stays stateless.
- **`VisualKind`, the registry.** A cross-target Rust enum -- stable ids and
  display names -- so the gallery UI, the Tauri commands, the wasm bindings and
  the smoke checks all name visuals identically, and neither shell keeps its
  own list. `Renderer` gains `set_visual(VisualKind)`; construction of the
  concrete visual happens inside `player-render`, next to the GPU state it
  needs.
- **`VisualFrame`, the analysis feed.** `draw_frame`'s `&[f32]` grows into a
  struct the host assembles once per frame, off the audio thread, from what
  already exists: the tap's sample window (output layout, fixed duration --
  the vectorscope's and the tunnel's input) and the metering suite's own
  results (the spectrum's bands for the bars and waterfall; M5's beat/onset
  uniforms later). The rule that keeps PLAN.md §4's promise -- listener
  visuals and studio meters cannot disagree -- is: **a visual computes no
  DSP.** If a visual needs a quantity that doesn't exist yet, it is added to
  `player-core`'s analysis as a meter first, cross-target, under the same
  drain-and-reset discipline as the others, and then exposed in the frame.
- **The shells keep their plumbing; only its cargo changes.** Natively the
  same one scope window, the same redraw thread, the same hide-not-close
  lifecycle (2026-08-17's decision stands untouched) -- the thread assembles
  the `VisualFrame` where it currently fetches `scope_window`, and a
  `set_visual` command swaps the scene between frames on the one thread that
  owns the renderer. On the web, `webScope.ts` generalizes to a visualizer
  adapter: same canvas, same rAF loop, frame assembled from the SAB regions
  the worker already publishes. `scope.ts` grows into the visualizer adapter
  module -- size, visibility, *selection*, *fullscreen* -- and stays the one
  place the shells differ.
- **Selection is UI state, persisted.** A gallery component (generalizing
  `ScopeControls`) lists `VisualKind`s; the choice persists in localStorage on
  both shells, like the EQ's settings -- a handful of bytes, not library data.

### Fullscreen, the hard requirement

Different mechanism per shell, one meaning; the difference stops in the
adapter, like everything else.

- **Web:** the Fullscreen API on the element wrapping the canvas. Esc and the
  browser's own exit affordances come free; `fullscreenchange` drives a resize
  to the screen's size (times `devicePixelRatio`, capped if a 4K canvas turns
  out to hurt -- measure, then decide), and drives the restore on exit. Click
  to exit is a canvas listener.
- **Native:** `set_fullscreen(true)` on the scope window -- borderless
  fullscreen of the window that already exists. The redraw thread's existing
  size polling handles the resize with no new code. The researched gotcha is
  **input**: the scope window has no webview, and a plain Tauri window
  delivers window events only -- no keyboard, no mouse. So Esc cannot be a
  window listener. The design: register Esc as a global shortcut
  (`tauri-plugin-global-shortcut`) **only while fullscreen is active**, and
  additionally drop out of fullscreen when the scope window loses focus --
  which both covers alt-tab gracefully and guarantees the global Esc is never
  registered while another app is frontmost. The UI window's own controls
  remain a second exit that always works. This needs verifying live on
  Windows early in M1, not last.

Fullscreen also makes two tracked loose ends of the current renderer
unignorable, so M1 fixes them rather than inheriting them: the figure
stretches with the surface instead of holding its aspect (ARCHITECTURE.md
"Known gaps" -- fine at 480x480, wrong at 2560x1440), and the channel pairing
on mono/multichannel outputs (the "not worth fixing before the vectorscope
work replaces this renderer" note -- this is that work).

## What already exists to build on

- `player-render` is already surface-agnostic (`wgpu::SurfaceTarget` from a
  window handle or a canvas) and already split renderer-vs-scene -- the seam
  the `Visual` trait names is close to where `Scene` already sits.
- `OffscreenRenderer` + `scope_smoke` established the verification style:
  known input, rendered offscreen, pixels asserted. It generalizes to "for a
  given `VisualKind`" and every new visual inherits the harness.
- The analysis tap (Phase 1 M4) and the meters (spectrum, loudness, phase) are
  cross-target and off the audio thread on both shells -- the feed is
  assembly, not new analysis. M5's beat/onset work extends `analysis.rs` the
  way loudness and phase did.
- The scope window's lifecycle (built once, hidden not closed, redraw thread
  joined before exit) is decided, verified and user-confirmed. Nothing in
  this phase reopens it.
- `ScopeControls` already polls state rather than assuming it, because the
  native window has its own close button -- the same pattern absorbs
  fullscreen state, which the user can also change without the panel (Esc).

## Decisions for this phase (2026-08-18)

1. **The host is designed once, in `player-render`, along one seam.** The host
   owns the surface, the analysis feed, selection and fullscreen; a visual
   owns only its scene (`Visual` trait). The existing window/canvas/redraw
   plumbing survives -- what changes is what rides on it.
2. **Visuals draw the metering's own analysis, and compute no DSP.** One
   `VisualFrame`, assembled by the host off the audio thread from the tap and
   the existing meters. A visual needing a new quantity gets it added to
   `player-core`'s analysis first -- cross-target, same drain discipline -- so
   the "listen" and "studio" faces of the app can never disagree (PLAN.md §4,
   decision §8.8).
3. **`VisualKind` is cross-target Rust data.** One registry of ids and names
   for the gallery, the commands, the bindings and the smokes. Selection
   persists in localStorage on both shells, like the EQ.
4. **Fullscreen lands in M1, and its mechanics stop at the adapter.** Web:
   Fullscreen API, Esc/click free. Native: `set_fullscreen` on the existing
   scope window; Esc via a global shortcut registered only while fullscreen,
   plus exit-on-focus-loss so that shortcut never shadows another app. A
   webview-less window receives no input events -- that constraint is the
   reason for the shortcut design, and the reason this gets verified live
   before M1 builds anything on top of it.
5. **Switching visuals swaps the scene, never the surface or the window.**
   The 2026-08-17 lifecycle decision stands: one window, built at startup,
   outliving its renderer. The swap happens on the thread that owns the
   renderer, between frames -- a command natively, a call on the wasm renderer
   on the web.
6. **Every visual ships with a pixel-readback smoke and a web harness.**
   `OffscreenRenderer` generalizes to render any `VisualKind`; a visual lands
   with known-input-to-asserted-pixels checks natively and the same figures
   checked at `/visualizer.html`. Eyeballing remains what the live
   confirmation is for, not what the checks are.
7. **MilkDrop compatibility is a Butterchurn-guided port, never projectM.**
   Butterchurn is MIT and license-clean under §8.7; projectM is LGPL and is
   not the route. The milestone opens with a de-risk spike (parse one classic
   preset, run its equations, render offscreen) before the full engine is
   committed to, and the supported subset of the `.milk` surface is written
   down rather than implied. Preset *content* is licensed separately from any
   engine and is vetted before shipping.
8. **WebGL2 fallback stays out of this phase.** Visuals require WebGPU, and
   the web build already says so plainly rather than rendering blank. The
   fallback remains a Phase 4 known-gaps item; taking it here would tax every
   visual with a second constraint set while the suite is being born.
9. **Nothing in this phase touches the audio callback or the DSP output
   path.** Phase 2's rule, inherited: visuals read; they are read *from* the
   tap and the meters. New analysis (M5) runs where the meters run -- the
   analysis drain, off the audio thread, on both shells.

## Milestones

Same rule as Phases 1 and 2: a milestone is done only when it works on **both
shells** and has been verified live. For visuals that means the pixel checks
pass *and* a person has looked at the thing moving with real music -- a
renderer verified only by readback ships with the right pixels in an ugly
frame.

- **M1 -- The visualizer host.** The design above, whole: `Visual`,
  `VisualKind`, `VisualFrame`, the vectorscope as first citizen (moved, not
  rewritten), the gallery UI with its one entry, selection persisted, and
  **fullscreen on both shells** -- plus the two renderer fixes fullscreen
  makes urgent (aspect, channel pairing). Verified: `scope_smoke` generalized
  to run per-kind and across a mid-run switch; `/visualizer.html` (absorbing
  `/scope.html`'s 8 checks); fullscreen entered and exited live on both
  shells, including native Esc and focus-loss.
- **M2 -- Spectrum bars.** The first second citizen, which is what proves the
  host: a visual whose input is *analysis* (the spectrum meter's own bands),
  not raw samples. 3D-instanced bars, peak-fall, note-frequency mode (PLAN.md
  §4). If building it requires touching anything but `player-render` and the
  gallery list, the host was designed wrong -- that is this milestone's real
  assertion.
- **M3 -- Spectrogram waterfall.** The spectrum's history as a scrolling 3D
  terrain. History lives inside the visual (a ring of band rows on the GPU),
  fed one row per frame -- the first visual with internal state, which is the
  part of the `Visual` contract M2 doesn't exercise.
- **M4 -- Oscilloscope tunnel.** The waveform swept along a camera path -- the
  "modern Winamp oscilloscope." Consumes the same sample window as the
  vectorscope; its new ground is camera motion, which M1's host deliberately
  left to the visual.
- **M5 -- Musical intelligence.** Onset detection, beat tracking/BPM, spectral
  centroid and transient strength as new members of `player-core`'s analysis
  -- cross-target, off the audio thread, reset on a broken listen like the
  loudness meters -- exposed as uniforms in `VisualFrame` (and, where useful,
  in the studio metering UI: a BPM readout is a feature for both audiences).
  Verified the analysis suite's way: known answers, not plausibility -- a
  synthesized 120 BPM click track must read 120.0, an onset must land within
  a stated tolerance of where it was synthesized, both shells agreeing.
- **M6 -- Particle / raymarched scenes.** The first visuals driven by M5's
  uniforms rather than by geometry-per-sample: GPU-instanced particles and an
  SDF raymarch reacting to beats and spectral movement. Also this workspace's
  first compute shaders -- their WebGPU limits get checked here, on both
  shells, before anything depends on them.
- **M7 -- MilkDrop `.milk` compatibility.** Opens with decision 7's spike;
  then the engine for the written-down subset: preset parsing, the
  per-frame/per-vertex equation language (EEL) evaluated in Rust, warp/comp
  passes translated to WGSL, presented as one more `VisualKind` whose
  "selection" includes a preset. The largest milestone in the phase, and the
  one most likely to split or slip once the spike reports -- planned as such
  rather than discovered as such.
- **M8 -- The native WGSL preset format.** What a preset may declare --
  which `VisualFrame` quantities it binds, its passes, its parameters -- with
  the built-ins' shaders as its first documents where that turns out natural.
  Deliberately after M7: the native format should learn from what MilkDrop's
  gets right and wrong, not precede it. **AI-adaptive presets slip to Phase
  6**, as PLAN.md §7 already allows -- they need this format *and* M5's
  analysis both, and Phase 6 is where the latentmixer/latentmastering
  integration they lean on lives anyway.

Ordering rationale: M1 is the seam everything else plugs into and carries the
phase's one hard requirement; M2-M4 each prove a different clause of the
`Visual` contract (analysis input, internal state, camera motion) while
shipping the §4 built-ins; M5 is analysis work the scenes in M6 need; M7-M8
are the preset engines, biggest and least certain, last.

## What "verified" has to mean here

Three kinds of check, because this phase has three kinds of claim:

- **Pixel readback** for "it draws what the audio says": the `scope_smoke`
  pattern, per visual, both shells (`OffscreenRenderer` natively, readback at
  `/visualizer.html` on the web).
- **Known-answer analysis tests** for M5: synthesized inputs with known BPM,
  onsets and centroids, asserted to figures, native and web agreeing --
  exactly how loudness was held to BS.1770.
- **A person watching it move**, per milestone, both shells -- the half no
  readback reaches: that it looks *good*, that fullscreen feels right, that a
  beat-reactive scene reads as musical rather than twitchy.

## Deferred, on purpose

- **AI-adaptive presets** -- to Phase 6, per M8's note and PLAN.md §7.
- **VR/immersive mode** -- PLAN.md's own stretch, Phase 6 at the earliest.
- **WebGL2 fallback** (decision 8) and the rest of ARCHITECTURE.md's known
  gaps that belong to Phase 4 (player completeness) -- nothing here should
  quietly adopt them.
- **Visualizing captured audio** -- Phase 5's entire point; the host is built
  so a `CaptureSource` feeding the same tap lights up every visual for free,
  but no capture code lands here.
- **Community preset sharing** -- Phase 6, once there is a format to share.

## Risks, named now

- **Native fullscreen input.** Decision 4's Esc design (global shortcut +
  focus-loss exit) rests on how a webview-less Tauri window actually behaves
  on Windows -- verified first thing in M1, with the fallback being an
  on-screen exit affordance drawn by the visual itself if shortcuts prove
  unreliable.
- **Fullscreen surface size.** A 4K borderless surface with 4x MSAA is a
  different GPU bill than a 480px window. Measure in M1; the answer may be a
  render-scale option, which is a host feature, not per-visual.
- **The EEL language.** MilkDrop presets are programs; running arbitrary
  community presets means an expression evaluator that is fast (per-vertex,
  per-frame), safe, and faithful to quirks decades of presets depend on.
  This is M7's spike's whole job, and the reason M7 is planned as splittable.
- **Preset content licensing.** The engine being MIT-clean (Butterchurn, not
  projectM) says nothing about the `.milk` files themselves. Shipping any
  bundled presets needs provenance, not vibes.
- **Compute shaders on the web.** M6 assumes WebGPU compute in both shells'
  browsers/webviews. Almost certainly fine on this machine; checked at M6's
  start rather than assumed until its end.
- **Aesthetic verification doesn't automate.** More than prior phases, "done"
  here includes taste. The live confirmations will carry more of the weight,
  and milestones should budget for a revision pass after the first look.

## What this phase needs from you

Live confirmations per milestone, as ever -- but here that means *watching*
each visual with real music on both shells, and fullscreen entered and exited
by every route (button, Esc, focus loss, the window's own controls). For M5,
music with a firm beat *and* something rubato or ambient -- a beat tracker
verified only on four-on-the-floor is not verified. For M7, any `.milk`
presets you care about working, named early, so the supported subset is drawn
around real use rather than around what was easy. And opinions: these are the
product's face, and "the pixels are correct" is the floor, not the bar.

## Sources consulted

- PLAN.md §4 (the suite, the host's ownership list, fullscreen as
  requirement), §7 Phase 3, §8.7 (MIT; Butterchurn-not-projectM), §8.8 (suite
  + fullscreen decision), §8.9 (ordering).
- ARCHITECTURE.md -- the scope window's lifecycle (2026-08-17), the aspect and
  channel-pairing known gaps this phase absorbs, the analysis tap and meter
  architecture the feed is assembled from.
- `crates/player-render/src/{lib,scene,trace,camera}.rs` and
  `src-tauri/src/lib.rs`'s scope window/redraw thread;
  `app/src/{scope.ts,webScope.ts,ScopeControls.tsx}` -- what the host
  replaces and what it keeps.
- [Butterchurn](https://github.com/jberg/butterchurn) (MIT) -- the proof
  `.milk` portability is real, and the implementation guide for M7;
  [projectM](https://github.com/projectM-visualizer/projectm) noted only as
  the route *not* taken (LGPL).
- [Tauri 2 window API](https://docs.rs/tauri/latest/tauri/window/struct.Window.html)
  (`set_fullscreen`, `set_focus`, window events) and
  [tauri-plugin-global-shortcut](https://v2.tauri.app/plugin/global-shortcut/)
  -- the native fullscreen/Esc mechanics, to be verified live in M1.
- [MDN Fullscreen API](https://developer.mozilla.org/en-US/docs/Web/API/Fullscreen_API)
  -- the web half: element fullscreen, `fullscreenchange`, Esc handled by the
  browser.

## M1 -- The visualizer host: done, user-confirmed 2026-08-18

The seam every other milestone in this phase plugs into, plus the phase's one
hard requirement. What landed:

**The seam, once, in `player-render`** (`visual.rs`). `Visual` is what `Scene`
already was, named: `resize` and `draw(gpu, target, frame) -> CommandBuffer`,
object-safe so the renderer can hold it boxed. `VisualKind` is the registry --
id, name, description, `ALL`, `DEFAULT`, `from_id` -- and it is the only list of
visuals in the project: the gallery UI, the Tauri commands, the wasm bindings
and both smokes read it, so nothing can offer a visual another shell does not
have. `VisualFrame` is the feed (samples, their channel layout, their rate),
assembled by each shell once per frame out of the tap. `Renderer` now holds a
`BoxedVisual` and gained `set_visual`/`visual`; `OffscreenRenderer` gained the
same, which is what lets the check exercise a *switch* rather than only a fresh
renderer. The vectorscope moved to `visuals/vectorscope.rs` behind the trait --
moved, not rewritten; its camera, extrusion and shader are untouched, and
`attachments.rs` now holds the depth/MSAA formats that are the host's business
rather than each visual's.

One thing the seam forced into the open: `BoxedVisual` is `Send` natively and
plain on wasm. Natively the renderer is moved onto its own redraw thread, so the
box must be `Send`; on wasm nothing in wgpu is `Send` and requiring it does not
compile. Written as two `cfg`'d aliases with the reason beside them, rather than
discovered by whoever adds the second visual.

**Both shells wired to it, with their plumbing intact.** Natively: the same one
window, the same redraw thread, the same hide-not-close lifecycle. Selection
arrives as a *request* (`VisualizerWindow::requested`) that the redraw thread
applies between two frames, because that thread owns the renderer and a command
handler building GPU objects for a device it does not drive is a bug waiting for
a driver to notice. The commands are `visualizer_state`,
`set_visualizer_size/visible/fullscreen`, `set_visual` and `visual_catalog` --
state as one struct, for the reason `TransportState` is one struct. On the web,
`webVisualizer.ts` keeps its canvas and rAF loop and gained the scene swap, the
Fullscreen API and the visibility skip; `visualizer.ts` is the adapter both
shells meet in, and `VisualizerControls` is one component driving both.

**Fullscreen, on both shells, exactly as decision 4 designed it.** Web: the
Fullscreen API on the canvas's *wrapper* -- a bare canvas would be stretched by
the browser, where a wrapper lets the canvas be resized to the screen and
centred -- with the resize driven by `fullscreenchange` rather than by the call,
since Esc and the browser's own control are three of the four ways out. Native:
`set_fullscreen` on the existing window, Esc as a global shortcut registered
only while fullscreen is active, and `Focused(false)` leaving fullscreen, which
does two jobs at once (alt-tab gives the screen back, and the global Esc can
never be registered while another app is frontmost). Hiding the visualizer gives
the screen back first, so "fullscreen and hidden" is not a state anything can
get into.

**Two things found while building, fixed rather than noted.** The web build's
Hide button flipped its own label and nothing else -- the canvas kept drawing at
60Hz -- where the native redraw thread has parked on that condition since M6;
visibility is one of the four things the host owns, so the web loop now skips
its frames too. And a refused fullscreen was being swallowed by the same
`.catch(() => {})` the other buttons use; a refusal is the one failure here a
person can act on, so the panel says which.

**Two of ARCHITECTURE.md's "Known gaps" turned out to be stale rather than
open** -- the ones this milestone was planned to fix. M6's camera already put
the aspect ratio in the projection, and `trace::build` already de-interleaves by
the layout's real width. Both entries described the pre-M6 renderer. Fullscreen
is what makes them worth *pinning* rather than believing, so instead of a fix
they got checks: the figure at 1280x720 scales with the height (2.26x native,
2.24x web, against the 2.25x predicted), a mono output draws up the centre line,
and a 5.1 output draws its front pair to within 8px of the same audio in stereo.
The entries are struck through with the evidence, not quietly deleted.

**Verified: 14/14 natively** (`cargo run -p player-render --example
visual_smoke`) **and 17/17 in a real browser** at `/visualizer.html` -- the same
14 claims plus three the web half can make on its own (the registry is named and
described, the default is one of its entries, and a switched renderer reports
the visual it was switched to). The figures agree across the shells: 196px
against 195px for the mono figure, 2.26x against 2.24x for the 16:9 scaling, and
a switch byte-identical to a freshly built renderer on both (0 of 409,600 bytes
differ). Also green: `cargo fmt --check`, `cargo clippy --workspace --all-targets
-D warnings`, the full test suite (234 tests, including 5 new ones on the
registry and the frame), `npm run wasm`, `npx tsc -b`, `npm run build`.

**What is not verified, and why** (see ARCHITECTURE.md's "Known gaps" for the
standing entries): fullscreen itself, on either shell. The in-app automation
browser refuses `requestFullscreen` outright -- `TypeError: Permissions check
failed`, because the pane hosts the page without `allow="fullscreen"` -- so what
was checked there is that the call is reached and that the refusal is reported
rather than swallowed. Natively the webview's own buttons are out of reach of
both the automation browser and UI Automation, the identical limit the
2026-08-17 lifecycle work ran into. So **fullscreen on both shells, Esc, the
focus-loss exit and the gallery under real music are the live confirmation this
milestone is waiting on**, and they are exactly what decision 4 said to check
first.

**That confirmation came the same day, on both shells, and found three things**
-- the two bugs below and the background difference after them. Confirmed live:
fullscreen enters and leaves cleanly, Esc comes back out of it (after the fix
below), and Auto gain looks right on real music. Nothing about M1 is left waiting
on a person, which matters for M2: the next visual plugs into a host that has
been used rather than one that has only been measured.

### Two bugs from the first live desktop run (2026-08-18)

**Esc out of fullscreen froze the whole app.** Reported live, reproducible every
time, with nothing in the log -- which is itself the clue: a hang, not a crash.
The cause is structural in `tauri-plugin-global-shortcut` 2.3.2. Its dispatcher
looks the shortcut up in an `Arc<Mutex<HashMap>>` and calls the handler **while
holding that lock** (its `lib.rs:417`); `unregister` takes the same mutex (its
`lib.rs:188`), and `std::sync::Mutex` is not reentrant. The same call also hops
to the main thread and blocks on a channel until it runs, so a handler on the
main thread waits for itself a second way. Either alone is a permanent freeze.
The fix is that the handler now does *nothing* but spawn a thread to perform the
exit: the dispatcher's lock is dropped as the handler returns, and the main
thread is free by the time anything asks it for something. It is the same shape
as `VisualizerWindow::stop`, which may not run on the main thread for its own
version of this reason. The other three ways out were never affected -- a
command runs on the async pool, and the window-event handlers run on the main
thread, where the plugin's hop resolves inline.

**The figure was a small part of a fullscreen window.** Also reported live: "80%
of the screen full sized is just the panel's background". Investigated properly
rather than tuned, and the answer came in three parts.

The camera's field of view was a fixed 45 degrees, which framed a fixed *angle*
regardless of the viewport. It is now *derived*: the projection opens exactly
wide enough to hold the trace's own reachable extent at 90% of the frame, in
whichever axis binds first. What is fitted is a **diamond**, not a box -- x is
the side signal and y the mid, so |x| + |y| <= sqrt(2) and the corners of a
square are positions no signal can occupy; framing them would have wasted a
factor of sqrt(2). Those four vertices at each end of the ribbon are the eight
corners of a convex solid, and a perspective projection maps a convex hull's
extremes to its projected extremes, so fitting eight points fits every point
between them. At the shapes M1 already measured this lands within a few pixels
of the old 45 degrees -- which is the evidence that the old value was well
chosen, not that the change did nothing: it now provably cannot clip at *any*
window shape, including the taller-than-wide ones the fixed angle never
considered, and the magic number is gone.

Two other levers were tried, measured, and rejected -- both are written into the
code so they are not tried again. Lengthening the ribbon on a wide viewport, so
the spare room went to the time axis, made the picture **worse**: a longer
ribbon reaches further towards the camera, the fit must then open wide enough to
hold that near end, and everything behind it shrinks (the mono figure went from
442px tall to 335px at 1280x720, with a lens-like exaggeration at the near end).
Pulling the camera back flattens that perspective and does help -- from 57% to
65% of the frame at a realistic distance -- but spends the sense of depth that is
the point of a 3D scope for eight percentage points.

What is left is the real answer, and it is arithmetic rather than a bug: **the
frame is reserved for a full-scale signal, and music is not full-scale.** A
full-scale mono tone fills 90% of the height by construction; a master peaking at
-12 dBFS is a quarter of that, about 22%; at -18 dBFS, 11%. On a 1440p screen
that is a small figure in a large dark field, exactly as reported, and no amount
of camera work changes it. The standard answer on a hardware vectorscope is a
**display gain**, which is a product decision rather than an implementation one
-- so it is being put to the user rather than chosen here.

Verified after both fixes: **14/14** natively and **17/17** in the browser
again, with the figure fractionally *larger* than before the change (445px
against 442px at 1280x720 natively, 443px in the browser), 240 tests passing
(six new ones on the fit: that it fills the frame and cannot clip at eight
window shapes from 320x320 to 3440x1440 and 600x900, that widening a window
never shrinks the figure, that the ribbon's length no longer varies, that the
uniform carries the numbers the fit was computed from, and that a degenerate
viewport still yields a finite camera). The Esc fix needs a live re-run to
confirm, since nothing here can press a key at a webview-less window.

### Display gain (user decision, 2026-08-18)

The arithmetic above left a product decision rather than an implementation one,
and it went to the user: **manual gain plus an opt-in automatic mode**. Both
live in `player-render::gain`, not in either shell, because the automatic mode
has state -- and two copies of a state machine is two shells whose scopes
visibly disagree about the same music.

- **x1, x2, x4, x8.** A multiplier rather than decibels, because what it
  multiplies is a picture. Unity is the default: someone who wants the figure
  larger can say so, where someone reading levels should never have to discover
  that the display was scaled.
- **Auto** follows a recent peak, aiming it at 90% of the frame, with **instant
  attack and a two-second release**. The asymmetry is the part that matters: a
  scope that eased *into* a loud passage would clip its own display for as long
  as the easing took, so the figure would be wrong rather than merely late. The
  release is timed in *seconds*, from a new `elapsed_seconds` on the frame, so a
  60Hz redraw thread and a browser's animation callbacks recover identically --
  checked directly, at 60Hz against 144Hz.
- **The panel says when it stops being a measurement.** Only unity is one, which
  is `GainSetting::is_measurement`'s whole job: which settings those are is
  Rust's to say, not a component's. At anything else the row reads "display only
  -- not a level reading".
- **The frame stays pinned to unity full scale.** Gain magnifies the trace
  inside it and is deliberately *not* part of the camera fit -- a camera that
  refitted itself to the gained figure would leave the picture identical and the
  control doing nothing.

One thing worth recording about checking it: the figure's bounding box is not
linear in gain, and the first version of the check asserted that it was. The
ribbon is extruded through space and viewed from one side, so even a signal of
no amplitude draws a slanted line; the box is `slant + amplitude * gain`,
measured at 46px of slant and 18px of amplitude, which predicts 64, 82 and 118px
at x1, x2 and x4 -- exactly what came back. So what is checked is the part gain
controls: the *increment* over unity, which scales at 3.00x native and 2.91x web
against the 3x it should.

Verified: **20/20** natively and **24/24** in a real browser, 249 tests, clippy
and fmt clean on both targets, and the whole loop driven live in the browser --
the gain applies, persists to localStorage, and the "display only" note appears
and disappears with the setting.

**User-confirmed on the desktop shell the same day**: fullscreen enters and
leaves cleanly, Esc no longer freezes the app, and Auto "looks right on real
music".

### The two shells drew different backgrounds (2026-08-18)

The one difference left after that confirmation: the field behind the scope was
near-black on the web and grey on the desktop. One constant, two colours, and
neither shell was wrong about the number.

A clear value is **linear light**, always. On a non-sRGB surface it is written
through as-is, so the old `0.02` landed as byte 5 -- near-black. On an **sRGB**
surface the GPU encodes it on the way out, and the same `0.02` becomes byte ~40
-- the grey. The web's canvas is `bgra8unorm` (checked directly:
`navigator.gpu.getPreferredCanvasFormat()`) and the desktop window's surface is
sRGB, so one number produced two pictures.

The fix is to stop writing a number and start writing a *colour*:
`attachments::BACKGROUND` is what a screen should show -- rgb(5,5,8), near-black
rather than true black, so the fading end of a trace stays distinguishable from
the field it is on -- and `attachments::background(format)` converts it to
whatever the surface in hand needs. Every future visual gets that for free, which
matters because this is the kind of difference that would otherwise be
rediscovered once per visual.

Checked on both paths rather than reasoned about: the native smoke renders to an
sRGB texture and the web smoke to the non-sRGB canvas, and **both now read back
rgb(5, 5, 8)** where the native one used to read (40, 40, 48). Four unit tests
pin the conversion by round-tripping it through the display transfer function
for every surface format the shells can be handed.

Verified after the fix: **21/21** natively, **25/25** in the browser, 252 tests,
clippy and fmt clean on both targets.

## M2 -- Spectrum bars: done, user-confirmed 2026-08-19

The first *second* citizen, and therefore the milestone that tests M1 rather
than extends it. The vectorscope was moved behind the `Visual` trait; nothing
was ever built against it. Bars are, and they are deliberately the least like
the vectorscope of the three built-ins: where the scope draws **raw samples**
geometrically, bars draw **analysis** -- the spectrum meter's own bands, the
same numbers the studio Spectrum panel shows, at the same instant. That is
PLAN.md section 4's promise ("the visualizers are the listener-facing face of
exactly the analysis data the metering suite shows engineers") stated as a
picture for the first time.

The milestone's own assertion, from the list above: **if building this requires
touching anything but `player-render` and the gallery list, the host was
designed wrong.** One exception is pre-authorised and is not a failure of that
assertion -- the *feed*. `VisualFrame` was written as a struct precisely so that
"M2's spectrum bars need the metering spectrum's own bands" would be a field
rather than a signature change (`visual.rs`'s own note says so). So the honest
form of the assertion is: the feed grows by two borrowed slices and the registry
by one entry, and **nothing else outside `player-render` changes** -- no new
command, no new UI control, no new persisted setting, no new meter.

### Decisions

**10. The spectrum's bands ride in `VisualFrame`, and no new analysis is
written.** The frame gains `levels: &[f32]` (dBFS per band, as published) and
`band_centres: &[f32]` (the frequency axis those levels are against). Both
already exist on both shells and are already published off the audio thread:
`Playback::spectrum()` / `spectrum_band_centres()` natively, the SAB's spectrum
region via `spectrum()` / `spectrumBands()` on the web -- both filled by the
same `player-core::spectrum` analyser. So decision 2 is satisfied in its strong
form: this milestone adds a *field*, not a meter, and the bars cannot disagree
with the Spectrum panel because they are drawing the same array.

Two consequences to build to rather than discover:

- **The visual reads `levels.len()`, never `BAND_COUNT`.** The web's centres are
  empty until a track has loaded (the analyser is built with the output's rate)
  and its levels are empty before the shared buffer exists, where the native
  side always answers with a full row at the floor. A visual that assumed 64
  would draw garbage on the web for the first second of a session.
- **An empty `levels` draws nothing**, exactly as an empty `samples` does for
  the vectorscope -- not sixty-four bars sitting at the floor, which is a
  picture of silence rather than a picture of "nothing has played".

**11. Peak-fall is display state inside the visual.** The falling caps are an
animation over levels *already measured*, timed off `elapsed_seconds` -- the
same category as the automatic gain's release, which lives in the host as
display state for exactly this reason, and firmly not decision 2's category of
"a quantity that doesn't exist yet". The spectrum meter's own 30 dB/s release
stays what it is and is not touched; the caps sit on top of it and fall slower,
because their job is to mark where a transient reached after the bar itself has
gone.

This narrows M3's claim, and the correction is worth recording now rather than
being contradicted later: the milestone list calls the waterfall "the first
visual with internal state". Peak-fall is internal state, so M2 gets there first
-- in the small. What M3 still owns alone is *GPU-resident* history, a ring of
band rows written once and scrolled, which is a different thing from a handful
of floats on the CPU. Finding the `Visual` contract wanting over sixty-four
floats is cheaper than finding it wanting over a texture.

**12. Display gain means a decibel offset here.** `frame.gain` is an amplitude
multiplier because the vectorscope draws amplitudes; a bar display is
logarithmic, so the same setting is applied as `20*log10(gain)` dB -- x2 is
+6dB, x8 is +18dB. The alternative, ignoring gain in this visual, would make one
control do something in one visual and nothing in the next, which is worse for a
person than either behaviour is on its own. `Auto` needs no special case: it
resolves from the frame's own sample peak, which is present whichever visual is
drawing. And the panel's existing "display only -- not a level reading" note is
exactly as true of a shifted dB axis as of a magnified figure.

**13. The bars' dB window is the visual's own, and it is not `FLOOR_DB`.** The
meter reports down to -120 dBFS because that is where a *measurement* should
stop. A bar drawn on a -120..0 axis spends more than half its height on material
below the noise floor of any real source, so every bar looks tall and nothing
reads. The bars draw **-72 dBFS to 0**: full scale reaches the top of the frame,
which is the same convention the vectorscope's frame already uses (full scale
fills it, gain magnifies inside it), and -72 leaves room under a quiet master
rather than clipping it off at the usual -60.

**14. "Note-frequency mode" is a marked axis, not a second set of bands.** The
existing bands are already log-spaced -- 64 across 20Hz-20kHz, about six per
octave -- so evenly spaced bars *are* the log/note axis; the mode PLAN.md
section 4 asks for is the reference the eye needs to read it, which is octave
marks. The bars therefore draw octave gridlines at the C's (32.7, 65.4, 130.8,
261.6, 523.3, 1046.5, 2093, 4186 Hz) as geometry in the scene, placed from
`band_centres` rather than from an assumed layout.

The other reading -- respacing the bars onto a chromatic grid, one bar per
semitone -- is **deliberately not** this milestone's, and not because it is
hard: it is a *band layout* change, which under decision 2 belongs in
`player-core::spectrum` as a meter option, cross-target, with the Spectrum panel
following it. Doing it inside the visual would be a second opinion about which
frequencies a band covers. Named here so it is a deferred feature rather than an
oversight.

There are no glyphs in this crate and this milestone does not add a text
renderer, so the octave marks are lines and ticks, unlabelled. Labelling them is
a host feature (every visual would want it) and lands as one if it is ever
wanted.

**15. The surface does not change shape with the selection.** `VISUALIZER_SIZES`
is square "because the vectorscope's figure is", and bars are wider than they
are tall -- but a size list that changed with the selection is the surface
following the visual, which decision 1 forbids and decision 5's one-window rule
reinforces. Bars are fitted to whatever viewport they are given, the way the
vectorscope now is: correct in a 480x480 panel and in a 21:9 fullscreen alike.
If a wider *panel* turns out to be wanted after the live look, it is a host
feature -- a size list with shapes in it -- and it lands as one, for every
visual, not as a bars special case.

### Shape of the work

- **`visual.rs`** -- `VisualFrame` gains `levels` and `band_centres`.
  `VisualFrame::new` keeps its signature and a chained `with_spectrum(levels,
  centres)` adds them, so the vectorscope's callers and every existing check
  read unchanged and only the callers that have a spectrum say so.
- **`visuals/bars.rs` + `bars.wgsl`** -- one instanced box per band (instance
  data: the band's normalised height and its peak cap), depth and 4x MSAA out of
  `attachments`, cleared to `attachments::background(format)`. The camera is
  *derived* the way M1 derived the vectorscope's: the projection opens exactly
  wide enough to hold the bars' own reachable extent at `FILL` of the frame, so
  there is no fixed field of view to be wrong at some window shape. A full-scale
  row of bars is the extent that gets fitted.
- **`VisualKind::SpectrumBars`** -- one arm each in `ALL`, `id`
  (`"spectrum-bars"`), `name`, `description` and `create`. Both shells' gallery,
  commands, bindings and smokes pick it up with no edit, which is the assertion.
- **The feed, at both ends.** Natively the redraw thread reads the levels and
  the centres inside the `shared.lock()` it already takes for `scope_window`, so
  a frame still costs one guard. On the web `webVisualizer.ts` passes
  `spectrum()` and `spectrumBands()`, and `player-rt-wasm`'s `draw_frame` grows
  the two slices to match. Nothing else in either shell is touched.

### What "verified" has to mean here

The five host-contract checks apply to bars the moment the kind is registered
(`check_the_host_contract` already runs per kind) -- an empty feed, the
background, a live feed, a mid-run switch, a resize. Then the bars' own claims,
in the `scope_smoke` style, natively and mirrored at `/visualizer.html`:

- a tone lights the bar whose band holds its frequency, and not the far side of
  the spectrum -- the check that the axis is the right way round, which is the
  bar-chart version of the M6 check that caught the scope drawn mirrored;
- a louder tone draws a taller bar, monotonically, across three levels;
- an empty feed draws no bars at all (decision 10), and a floor-level row draws
  none either;
- after a level drops, the peak cap is above the bar and descends over a known
  elapsed time (decision 11) -- checked at two frame rates, as the gain's
  release was, so the fall is timed in seconds on both shells;
- the figure fills the frame and cannot clip at the same spread of window shapes
  the vectorscope's fit is pinned at.

And the half no readback reaches: **a person watching it move with real music on
both shells**, per the phase's rule. The phase already budgets a revision pass
after that first look, and bars are the visual most likely to need one -- "the
pixels are correct" is a low bar for a spectrum analyser, which is the most
familiar visual in the app and therefore the easiest to draw generically.

### What this milestone will not do

- **No chromatic/semitone bands** (decision 14) -- a `player-core` band-layout
  change, deferred and named.
- **No text** -- octave marks are geometry; labelling is a host feature.
- **No history** -- the waterfall is M3's, and the bars keep no rows.
- **No per-visual sizes, settings or persisted state** (decision 15). The host
  has no per-visual settings mechanism, and inventing one for a mode toggle
  would be designing M8's parameter system in a corner of M2.
- **No change to the metering panel's Spectrum display.** It and the bars are
  two presentations of one array, which is the point; the panel is already
  right.

### What landed

**The feed grew by two slices, and nothing else outside `player-render`
changed.** `VisualFrame` gained `levels` and `band_centres` through a chained
`with_spectrum`, plus `band_position(hz)` -- where a frequency falls along the
bands, interpolated in log frequency, which lives on the frame because the
waterfall will want exactly it too. Natively the redraw thread reads
`spectrum()` and `spectrum_band_centres()` inside the guard it already takes for
`scope_window`, so a frame still costs one lock; on the web `webVisualizer.ts`
passes `spectrum()` and `spectrumBands()` from the same shared buffer the
Spectrum panel reads. No new command, no new UI control, no new persisted
setting, no new meter -- and the gallery picked the visual up with **no UI edit
at all**, which is the assertion this milestone existed to make.

**`visuals/bars.rs` + `bars.wgsl`.** One instanced box per band and a second per
peak cap, drawn with the host's depth buffer and 4x MSAA, over octave marks drawn
as lines on the floor of the scene. The camera is *derived* rather than chosen,
which is now shared code: M1's fit moved out of the vectorscope into
[`camera::fit`] -- eye, target, a convex hull's corners, an aspect and a fill,
returning the narrowest projection that holds all of them. The vectorscope's six
fit tests pass unchanged against it, which is what makes the extraction an
extraction rather than a rewrite. The bars' own scene is a plain box, because a
bar never leaves its slot ([`BAR_FILL`]), so the fit depends on the aspect ratio
and on nothing else -- not on the band count, and not on the music.

**Colour by height, on the vectorscope's own two colours**: violet at the base
running to green at the top, so a bar's colour reads as its level rather than as
decoration, with per-face shading that is what makes the boxes read as solids.
Caps near-white.

### Three things found while building

**The host contract was only handing over half the feed.** The per-kind checks
passed a frame with samples and no spectrum, so the bars -- correctly, by
decision 10 -- drew nothing, and two checks failed the moment the kind was
registered. The check was wrong, not the visual: the host assembles a *whole*
feed, so the contract has to be asked with one. Fixed, and taken further --
`visual_smoke` now runs the **real analyser** (`player-core` as a dev-dependency
of `player-render`, which is a cycle-free direction) so a tone that lights a bar
is measured by the same code the meters use. A hand-written levels array would
only have been checking itself.

**The peak caps hid the level check.** "A louder tone draws a taller bar"
reported three identical heights, because each shot reused a renderer whose caps
were still up from the loud frame before it. The caps were working exactly as
designed and the check was measuring them instead of the bars. Each independent
shot now gets a renderer with no history, and the reason is written next to
`bars_renderer` so the next visual with state does not rediscover it.

**The two shells drew different colours, everywhere -- the other half of
2026-08-18's background bug.** That day's fix defined the background as the
colour a screen should show and converted it per surface format. But a *fragment
shader* writes linear light with exactly the same consequence: an sRGB surface
encodes it on the way out and the web's `bgra8unorm` canvas does not, so every
colour in every visual came out washed-out on the desktop and saturated on the
web. It went unreported because a trace is bright either way; M2 is where it
stops being subtle, since a dim octave mark that reads on one shell is nearly
invisible on the other.

So the conversion is now `visuals/shared.wgsl`, prepended to every visual's
shader by `attachments::shader_source`, with `attachments::target_is_srgb`
supplying the flag through a uniform slot each visual already had. A visual
writes the colour a screen should show and calls `to_target` on the way out.

The evidence that this was a real difference rather than a theoretical one is in
the numbers: M1 reported the vectorscope's mono figure as **196px natively
against 195px on the web**, and its 16:9 scaling as **2.26x against 2.24x**. The
two shells now report **189px and 189px**, **2.35x and 2.35x** -- identical, not
merely close. (The figures moved because the same fix made the ribbon's far end
darker than the smoke's old lit-pixel threshold of 80; that threshold is now 40,
which is still five times the brightest channel of the background, and the reason
is written at both ends.)

### Verified

**33/33** natively (`cargo run -p player-render --example visual_smoke`) and
**38/38** in a real browser at `/visualizer.html` -- the same claims plus the
five the web half can make on its own. The bars' figures agree **exactly**
across the shells, every one of them:

| claim | native | web |
| --- | --- | --- |
| 80Hz lights a bar at x= | 154 | 154 |
| 6kHz lights a bar at x= | 445 | 445 |
| tops at full scale / -20dB / -40dB | 61, 123, 182 | 61, 123, 182 |
| a full-scale row reaches row | 53 | 53 |
| a cap after one second, from row 53 | 86 | 86 |
| the same cap at 60Hz for a second | 86 | 86 |

Also green: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
warnings`, the full suite (**258 tests**, up from 252 -- six new ones on the
bars' fit, axis direction, slot arithmetic, display floor and decibel gain),
`npm run wasm`, `npx tsc -b`, `npm run build`. And the gallery lists both visuals
on both shells with no UI change, which was the milestone's real assertion.

**What was not verified by any of that**: how it *looks* with real music, on
both shells. The pixel checks say the axis runs the right way, that taller means
louder, that the caps fall in seconds and that nothing clips at any window shape
-- none of which is the same as it being good to watch. A spectrum analyser is
the most familiar visual in the app and therefore the easiest to draw
generically, so this was the milestone most likely to want the revision pass the
phase budgets.

### Closed (2026-08-19)

**User-confirmed on both shells: "works as expected and looks good -- both
shells. No reason for another pass."** So the revision pass this milestone
budgeted for was not needed, and that is worth recording as more than a tick:
the display constants M2 chose by argument are now settled by observation. The
three-quarter camera, the -72..0 dBFS window, the 12dB/s caps and the octave
marks stand as they are, and M3 inherits them as a starting point rather than as
open questions -- which matters, because the waterfall is the same analysis on
the same axis and should not look like a different application.

Two things this closes beyond the milestone itself:

- **The host is now used rather than only measured, twice.** M1 built the seam
  and the vectorscope moved behind it; M2 is the first visual built *against*
  it, and the answer is that the feed grew by two slices and nothing else
  outside `player-render` changed. The design held. M3's one new demand on the
  contract is GPU-resident history, which is the part M2 deliberately did not
  exercise.
- **The two shells agree on colour**, which they did not before this milestone
  and nobody had noticed. Every visual from here gets that for free, and both
  smokes now report identical pixel figures where they used to differ by one.

M3 (spectrogram waterfall) is next.

## M3 -- Spectrogram waterfall: done, user-confirmed 2026-08-19

The spectrum's *history* as a scrolling 3D terrain: the same bands the bars
draw, one row per moment, running away from the viewer. PLAN.md section 4's
third built-in, and the milestone that exercises the clause of the `Visual`
contract M2 left alone -- **a visual that keeps something between frames, on the
GPU**. M2's peak caps are internal state in the small (a handful of floats on
the CPU, rebuilt into the instance buffer every frame anyway); the waterfall's
history is state that would be absurd to rebuild per frame, which is the case
the trait has never been asked to carry.

M2's assertion was that the host survives a visual whose input is analysis, with
one pre-authorised exception: the feed grew by two slices. **M3's assertion is
stricter, because it has no exception to claim.** Everything the waterfall needs
is already in `VisualFrame` -- the levels, their axis, and the elapsed time it
scrolls by. So the whole milestone should land inside `player-render`: one new
module, one new shader, one arm each in the registry's five matches, and two
smokes. No new frame field, no new command, no new binding, no new UI control,
no new persisted setting, no new meter, and no edit to either shell. If
something outside the crate has to move, that is a finding about the host and it
gets written down here rather than quietly absorbed.

### Decisions

**16. The history is a ring of rows in a GPU texture, and the scroll is a
uniform.** A single `R32Float` texture, `MAX_BANDS` wide by `ROWS` tall, written
one row at a time with `write_texture` and read in the *vertex* shader by
`textureLoad`; a `head` index in the uniform says which row is newest, and
advancing time therefore costs one integer, not a copy. The mesh is a static
grid of `bands x ROWS` vertices whose height is the texel it loads.

The alternatives, and why not: rebuilding a vertex or instance buffer of every
cell each frame is CPU work proportional to the history, which defeats the point
of keeping history at all; keeping the rows on the CPU and re-uploading the
whole texture per frame is the same cost wearing a different hat; and a
`textureSample` with a sampler would raise a filtering question the geometry
does not have -- each vertex sits exactly on one texel, so `textureLoad` is both
exact and cheaper. No compute shader: a ring written by the queue needs none,
and M6 is where this workspace checks WebGPU compute on both shells (the phase's
own risk list) rather than discovering it here.

**17. Rows advance on a clock, never per frame.** `ROW_HZ` rows a second, `ROWS`
of them: **30/s over 180 rows, six seconds of history**. The visual accumulates
`frame.elapsed_seconds` and emits whole intervals, so the terrain scrolls at the
same speed on both shells.

This is not a refinement -- it is the difference between working and not. The
desktop redraw thread sleeps 16ms; the web's is `requestAnimationFrame`, which
runs at 120Hz on a high-refresh display and is throttled to a crawl when the tab
is hidden. A row per frame would scroll twice as fast on one person's monitor as
another's, and six seconds of history would mean three. It is the discipline the
peak caps and the automatic gain's release are already held to (M2 decision 11),
and it is what lets the smoke prove the scroll rate by drawing the same second
as sixty short frames and as one long one.

Catch-up is capped at `ROWS` intervals in a single frame: a gap longer than the
history is a whole new terrain, and a laptop resumed after an hour should not
loop a hundred thousand times to arrive there. The rows a stall does emit repeat
the levels in hand, which draws the stall as the flat stretch it was.

**18. The texture holds dBFS as measured; the display window and the gain are
applied when it is drawn.** The row written is the analyser's own numbers. The
mapping to height -- `RANGE_DB` below full scale at the floor, 0 dBFS at the
top, plus display gain as a decibel offset (M2 decisions 12 and 13, unchanged
and shared) -- happens in the vertex shader, against uniforms.

The cheap way round is to normalise on the way in and store 0..1. It is wrong
for one reason that shows immediately: turning display gain up would then shift
only the rows written after the change, leaving a visible seam across a terrain
that is supposed to be one continuous measurement. Storing what was measured
also keeps the stored thing honest under decision 2 -- history is a record of
the analysis, not of a display setting that happened to be in force.

**19. The axes are the bars' axes, with time added.** Frequency runs left to
right (the same camera side, for the same reason M2 gives), level is height,
and time runs **into the screen with the newest row at the front**. Colour is
the bars' violet-to-green ramp by level, and the octave marks carry over as
rails running along the time axis, placed through `VisualFrame::band_position`
exactly as the bars place theirs.

None of that is decoration. A person switching from the bars to the waterfall
should see the same picture with its past behind it, because it *is* the same
array -- the gallery reading as one instrument with several faces rather than
three demos that happen to ship together is worth more than any per-visual
styling idea, and it costs nothing to decide now and something to retrofit
later. It also gives the smoke a claim no single visual could make: the same
tone lights the same x in both visuals.

The one place the waterfall deliberately differs is the surface between bands.
The bars are sixty-four separate solids because a bar chart's gaps are what make
it readable; the terrain puts one vertex per band and lets the surface
interpolate between centres, because a spectrogram is a continuous spectrum that
was sampled, and drawing it as sixty-four walls would assert an edge at every
band boundary that the analysis does not have.

**20. Nothing resets the history, and the host is not asked for a signal that
would.** A seek, a track change or a gapless hand-off puts a discontinuity in
the terrain. That discontinuity is the truth about what was played, drawn.

The alternative wants a new `VisualFrame` field meaning "the continuity broke",
filled by both shells off the transport, so that one visual can clear a texture.
That is host surface -- the thing M1 spent a milestone bounding -- bought for a
cosmetic gain. The meters reset on a broken listen because a *measurement*
across a seek is wrong (Phase 1 M5); a picture of the last six seconds of output
across a seek is not wrong, it is a picture of a seek. If the live look says
otherwise, it becomes a host feature at M5, which is adding frame fields anyway.

**21. An empty feed freezes the terrain; it does not scroll silence into it.**
Before any row exists, the waterfall draws nothing at all -- M2 decision 10's
rule, and for its reason: an empty `levels` means *nothing has measured*, which
is a different picture from silence. If the levels go empty mid-run (the web's
shared buffer between tracks) the terrain holds still rather than pushing a wall
of floor rows through six seconds of real history.

Measured silence is not this case and does scroll: a track that ends leaves the
analyser reporting its floor, the rows record that, and the gap between tracks
becomes visible. That is the display doing its job.

**22. The band count may change underneath it, and that is the one thing that
does clear the history.** The texture is allocated once at `MAX_BANDS` (256, the
bars' headroom, against `analysis::BAND_COUNT`'s 64) and only `levels.len()`
columns are written and drawn; a change in that length rebuilds the grid mesh,
the same `built_for` pattern the bars use. It also clears the rows, because rows
measured against a different frequency axis are not comparable to the ones
replacing them -- keeping them would draw two different spectra as one terrain.
The web's first play (empty, then 64) is exactly this transition and is why it
is designed rather than discovered.

### Shape of the work

- **`visuals/waterfall.rs` + `waterfall.wgsl`** -- the ring texture and its head
  index, the grid mesh built for the current band count, the terrain pipeline
  (indexed triangles, height and colour from `textureLoad`) and the rails
  pipeline (lines, the bars' octave marks swept along time). Depth and 4x MSAA
  from `attachments`, cleared to `attachments::background(format)`, shader
  through `attachments::shader_source` so the colour conversion is the shared
  one M2 landed.
- **`VisualKind::SpectrogramWaterfall`** -- one arm each in `ALL`, `id`
  (`"spectrogram-waterfall"`), `name`, `description` and `create`. Both shells'
  galleries, commands, bindings and smokes pick it up with no edit. That
  happened once already in M2; happening twice is what turns it from a
  coincidence into a property of the design.
- **`camera::fit`'s third caller.** The scene's hull is a fixed box -- `SPAN`
  across, `HEIGHT` up, the history's depth back -- so the projection is derived
  from the scene at every window shape, and there is no field of view to be
  wrong in fullscreen. Nothing about the fit depends on the music or the band
  count, exactly as the bars' does not.
- **The scale, shared rather than copied.** `normalise`, `gain_db`, `RANGE_DB`
  and `OCTAVE_C_HZ` are wanted verbatim by a second visual now. They move to one
  place the moment the second copy would be a copy -- two visuals holding
  private opinions about what -72 dBFS means is precisely the disagreement
  `player-core` exists to prevent one crate up. Same move `camera::fit` was in
  M2: extraction when there is a second caller, not before.
- **Nothing outside `player-render`.** Named again because it is the assertion.

### What "verified" has to mean here

The host contract's five per-kind checks (empty feed, background, live feed,
mid-run switch, resize) apply the moment the kind is registered. Then the
waterfall's own claims, natively in `visual_smoke` and mirrored figure for
figure at `/visualizer.html`:

- a tone lights a ridge at the x its band belongs to -- and **at the same x the
  bars light for the same tone**, which is the first check in this project that
  holds two visuals to each other;
- **the ridge is still there after the tone stops**, and has travelled a known
  distance for a known elapsed time -- the history claim, and the only one that
  could not have been made before this milestone;
- that distance is the same when the second is drawn as sixty short frames and
  as one long one (decision 17), which is the check that would have caught a row
  per frame;
- after more than the history's own length, the ridge is gone -- the ring wrapped
  and did not wrap *around* into the front, which is the classic ring-buffer bug
  and would draw as the past reappearing in the future;
- turning display gain up moves the whole terrain, old rows included, with no
  seam at the row where the setting changed (decision 18);
- an empty feed draws nothing, and a feed that goes empty mid-run draws the same
  pixels twice rather than scrolling (decision 21);
- a band count that changes (32 to 64) rebuilds and clears without panicking
  (decision 22);
- the figure fills the frame and cannot clip, at the same eight window shapes
  the vectorscope's and the bars' fits are pinned at.

And the half no readback reaches: **a person watching it move with real music on
both shells.** Two things to look for beyond "does it look good", because they
are the ones the numbers cannot answer: whether six seconds is the right amount
of past (long enough to see a phrase, short enough that the front is not
crushed), and whether the terrain reads at a glance or turns to mush on dense
material -- a waterfall's failure mode is a fog, and it fails that way gradually
rather than obviously.

### What this milestone will not do

- **No settings** -- no scroll rate, history length, colour map or smoothing
  control. M2 decision 15 stands: the host has no per-visual settings mechanism
  and inventing one here would be designing M8's parameter system in a corner of
  M3. The constants are chosen, written down with their reasons, and revised
  after the live look if the look says so.
- **No text or labels** -- the octave rails are geometry, as the bars' marks are.
- **No new frame field, no shell edit, no new meter.** The assertion.
- **No reset-on-seek plumbing** (decision 20).
- **No compute shaders** (decision 16) -- M6's ground, checked there.
- **No chromatic band layout** -- still M2 decision 14's deferred `player-core`
  work, and the waterfall inherits the deferral rather than reopening it.

### What landed

**The assertion held exactly.** Every file this milestone changed is inside
`player-render` -- `visual.rs`'s registry, the new `visuals/waterfall.rs` and
`waterfall.wgsl`, the extracted `visuals/scale.rs`, and its own smoke -- with one
file outside it: `app/src/visualizerSmoke.ts`, which is the web *check*, not the
web shell. No `src-tauri`, no `player-rt-wasm`, no UI, no command, no binding, no
persisted setting, no new frame field, no new meter. Both galleries listed the
third visual with nothing edited, for the second milestone running, which is
what turns M1's seam from a claim into a property.

**`visuals/waterfall.rs` + `waterfall.wgsl`.** The history is an `R32Float`
texture, `MAX_BANDS` by `ROWS`, written one row at a time and read in the
**vertex** shader with `textureLoad` -- so a frame costs one row and one integer
(`head`), and there is no vertex buffer of heights to keep in step with it. The
terrain's mesh is nothing but an index buffer: `vertex_index` *is* the grid
position, band across and age down. Rows advance on a clock at 20 a second over
120 rows, which is six seconds; the leftover time is carried, so sixty frames of
a sixtieth move the history exactly as far as one frame of a whole second, and a
machine resumed from sleep arrives at a new terrain in one frame rather than
writing every row it slept through.

**The near edge earns its own pipeline.** A skirt drops the newest row's profile
to the floor, because a terrain without one is a sheet with a paper edge at the
front. It also turned out to be why the *first* measurement of a session draws
anything at all: a surface needs two rows before it has a triangle, and the
skirt needs one.

**`visuals/scale.rs`**, extracted the moment there was a second caller -- the
same rule `camera::fit` moved under in M2. `RANGE_DB`, `MAX_BANDS`,
`OCTAVE_C_HZ`, `slot_centre`, `normalise` and `gain_db` are now one axis for both
spectrum visuals, with the bars' two display tests moved beside them and a third
added: that the band arithmetic is centred and runs left to right, which is what
makes "the same tone is in the same place in both" one function rather than two
that agree today.

### Four things found while building

**The camera was inherited and was wrong.** The bars' three-quarter view (an eye
0.6 to the left) came across with the rest of the scene, and in a scene with real
depth it *skews the frequency axis*: the near-left corner is much closer than the
near-right one, so the low end of the spectrum is compressed against the high
end. Measured rather than felt -- an 80Hz tone landed at x=226 of 640 where a
6kHz tone landed at 442, offsets of -94 and +122 from centre for two tones that
should be roughly symmetric about it. Centring the camera made them -107 and
+101. This is the one thing here that no check would have caught, because every
check would still have passed: the ridge was on the correct side, in the correct
order, at the correct height. It was found by drawing the picture and looking at
it.

**The octave rails are invisible under a terrain.** The bars' marks lie on the
floor and read between the bars; a terrain covers its own floor completely, so
the same marks are hidden everywhere. They became a **ruler in front of the near
edge** instead -- which is what a waterfall wants anyway -- and the overhang was
measured up from 0.07 to 0.18 of the depth by counting rail-coloured pixels in a
real frame until there were enough of them to read.

**All three of the first failures were the checks, not the visual.** Worth
recording together, because the pattern is the same one M2 hit and the cost of
getting it wrong is a "fix" to working code:

- *"the bars and the waterfall put the same tone in the same place"* is *false*,
  and not because of a bug. Each scene has its own camera, and a perspective
  camera does not map world x to screen x linearly, so the two displays cannot
  agree on a pixel or even on a proportion. What they can agree on -- and what a
  person switching between them actually reads -- is the **order**, so the check
  became a ladder of four tones that must march left to right in both.
- *"display gain lifts the terrain"* measured the topmost lit pixel, which on a
  terrain is the far edge of the **floor** -- and no gain moves the floor. It
  now counts loud material, over a history that gained no rows, which is the
  claim decision 18 actually makes: the stored measurement is re-mapped, not the
  new rows.
- *"a change of band count clears the history"* asserted a pixel count under an
  arbitrary threshold. It now asserts byte-equality with a renderer that never
  saw the old axis, which is both stronger and self-evidently right.

**A cleared texture is a wall of full scale.** Zero is 0 dBFS, so the empty
history is written to a floor value at construction and on every clear rather
than left zeroed. Named here because it is the kind of thing that would have
drawn correctly for six seconds and then been blamed on the ring.

### Verified

**48/48** natively (`cargo run -p player-render --example visual_smoke`) and
**54/54** in a real browser at `/visualizer.html` -- the same claims plus the
extra ones only the web half can make. Every structural figure agrees to the
pixel:

| claim | native | web |
| --- | --- | --- |
| an 80Hz ridge / a 6kHz ridge at x= | 213 / 421 | 213 / 421 |
| the four-tone ladder, on the waterfall | 207/295/377/433 | 207/295/377/432 |
| the ridge's centre one second on, from | row 205, from 249 | row 205, from 249 |
| the same second in sixty frames | row 205 | row 205 |
| loud pixels at 3s / at 8s | 1984 / 0 | 1965 / 0 |
| loud pixels at x4 / at unity | 3663 / 1487 | 3639 / 1479 |
| a frozen terrain, twice | 0 bytes differ | 0 bytes differ |
| after the axis moved, against a fresh renderer | 0 bytes differ | 0 bytes differ |

The area figures differ by well under a percent (4067 against 4033 loud pixels)
because they count anti-aliased edges either side of a colour threshold, and the
two backends resolve multisampling their own way; every figure that is a
*position* is identical.

Also green: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
warnings`, the full suite (**263 tests**, up from 258 -- eight new ones on the
waterfall's fit, its time axis, the clock, the index prefix and the uniform, and
one new on the shared band arithmetic), `npm run wasm`, `npx tsc -b`, `npm run
build`.

**What the checks could not reach, and what the live look answered**: how it
looks with real music, on both shells.
The pixel checks say the axis runs the right way, that history survives and
recedes at the right rate, that it forgets on time and that gain re-maps what is
already stored -- none of which is the same as it being good to watch. Two things
to look for that no readback reaches: whether six seconds is the right amount of
past, and whether the terrain stays legible on dense material or turns to fog. A
waterfall fails gradually rather than obviously, which is the failure mode most
worth a person's eye.


### Closed (2026-08-19)

Confirmed live on both shells, in the words that closed it: *"Six seconds looks
good. It stays legible and no fog. Waterfall falls as expected. All tests passed
-- Desktop and web."*

That answers all three of the questions the pixel checks were unable to ask:

- **Six seconds is the right amount of past.** It was an argument (a row thinner
  than a pixel is work that cannot be seen; a phrase is a few seconds long) and
  is now an observation, so M4 and anything later with a time axis inherit it as
  a starting point rather than as an open question.
- **The terrain stays legible on dense material.** A waterfall's failure mode is
  fog, and it arrives gradually rather than obviously -- which is exactly why it
  had to be watched rather than measured. It did not arrive.
- **The clock reads correctly to a person.** `rows_due` proves the arithmetic and
  the two-frame-rate check proves the two shells agree; neither can say the fall
  looks right. It does, on both.

And it confirms the one change this milestone made by *looking* rather than by
failing a check: the centred camera. The inherited three-quarter eye skewed the
frequency axis in a scene with depth, no check would ever have caught it, and the
fix is confirmed on real music now rather than only on two tones.

Three milestones have now been built against M1's seam and none of them has
needed the host to change: M2 grew the feed by two slices, M3 grew nothing at
all, and neither touched a shell. M4 (oscilloscope tunnel) is next, and is the
one that exercises the clause both spectrum visuals left alone -- camera motion,
which M1 deliberately left to the visual.

## M4 -- Oscilloscope tunnel: done, user-confirmed 2026-08-19

PLAN.md section 4's "modern Winamp oscilloscope": the waveform swept along a
camera path, drawn as a tube the eye travels down. The last of the three
built-ins, and the one that exercises the clause of the `Visual` contract the
other two left alone -- **a camera that moves**. M1 decided that the host owns
the surface, the feed, selection and fullscreen and that a visual owns its
scene; a camera is part of a scene, so it has always been the visual's, and
nothing so far has taken the host up on it. The vectorscope, the bars and the
waterfall all look *at* a fixed box from a fixed eye.

This one is inside its scene, which turns two of this crate's habits upside
down and is the reason the milestone is worth its own decisions rather than
being "the vectorscope with a different cross-section".

Its input is the vectorscope's: the same `VisualFrame::samples`, the same window
the tap already hands over. So M3's assertion applies again and for the third
time -- **nothing outside `player-render` should move**: no new frame field, no
command, no binding, no UI, no persisted setting, no new meter. If that breaks,
it is a finding about the host and it gets written down here.

### Decisions

**23. The tunnel keeps its own history, on M3's mechanism.** The feed is
`TRACE_SECONDS` of audio -- 120ms, sized by `trace::window_samples`, which both
shells ask their tap for. That is a fine ribbon and far too short a tube. So the
tunnel keeps rings the way the waterfall keeps rows: a GPU-resident ring buffer,
written one cross-section per clock tick, read in the vertex shader.

The alternative -- asking the host for a longer window -- is precisely the change
this milestone must not make. `TRACE_SECONDS` is the vectorscope's own design
(an eighth note at 120bpm, long enough to have shape and short enough not to
smear), both shells size their tap slice by it, and lengthening it for one
visual would lengthen the ribbon for another and touch two shells to do it.
History that belongs to one visual lives in that visual. M3 settled that.

Which means the second caller has arrived for the mechanism itself -- a texture
ring, a `head`, a clock with carried remainder, a clear on the axis changing. If
the tunnel's use is the same shape as the waterfall's, it moves to
`visuals/history.rs` under exactly the rule `camera::fit` and `visuals::scale`
moved under: when there is a second caller, not before. If it turns out *not* to
be the same shape -- a ring is a closed loop of amplitudes where a row is an open
axis of levels -- then it does not move, and that is recorded rather than forced.

**24. `camera::fit` is not used here, and the reason is the milestone.** The fit
answers "how narrow a projection holds this hull at 90% of the frame" -- a
question about a scene you look *at*, from outside it, with corners that can
leave the frame. Inside a tube there are no corners and nothing to clip: the
scene surrounds the eye, and the failure mode is the opposite one, seeing *past*
the scene at the edges of a wide window.

So the arithmetic inverts. The field of view is chosen, and the **scene is fitted
to the camera**: the tube's radius, and how far ahead of the eye it extends, are
derived so that the wall subtends more than the frame's half-diagonal at every
aspect ratio this app can be given -- a 480px panel and a 21:9 fullscreen alike.
That is the same promise M1's fit makes ("provably cannot clip at any window
shape") turned inside out, and it is checkable the same way: as a claim about
angles, in a unit test, and as a claim about pixels, in the smoke.

**25. The path is a pure function of ring index.** Where the tunnel goes -- its
bends -- is `path(index)`, evaluated for the geometry and evaluated again at a
fractional index for the camera. Not stored per ring, and not integrated frame
by frame.

Two things follow, both of them the point. The camera and the tube **cannot
disagree** about where the tunnel is, because there is one function and no state
between them; an integrated camera would drift out through the wall after a few
thousand frames of rounding. And the bends are attached to the *history* rather
than to the clock, so a bend arrives when the audio that made it does and stays
where it was: the tube is stable in the world rather than swimming.

**26. The camera chases the present.** It sits a fixed distance behind the newest
ring -- the whole history's length -- and advances at exactly the rate rings are
created, so the picture is stable: new rings appear at the far end, the eye flies
toward them, and old rings pass behind it and expire.

Ahead is therefore *newer*, behind is older. The alternative reading (the newest
ring at the eye, the past stretching away ahead) is the one the vectorscope and
the waterfall use, and it fails here for a reason worth writing down: flying
forward into it means flying into the past, and every ring the eye is about to
reach is one it has already passed the moment it was drawn. Chasing the present
is the reading that makes forward motion mean something.

Speed is therefore not a free parameter. It is the ring rate times the ring
spacing, in seconds, which is what makes the desktop's 60Hz redraw and a
browser's animation callbacks fly at the same speed -- decision 17's discipline,
inherited rather than restated.

**27. Motion comes from time and from what is already measured.** The camera's
forward speed is the clock; any sway or roll is a function of elapsed time and of
quantities already in the frame. Nothing here detects anything.

That is not modesty, it is decision 2 and the phase's own ordering. A tunnel that
surges on a beat is a good idea and it needs a beat detector, which is M5's whole
milestone -- cross-target, in `player-core`, checked against synthesized click
tracks with known answers. Building half of one inside a visual to make the
camera lurch would be M5 done badly, in the wrong crate, with no way for the
studio side of the app to see the same number. M6's scenes are where these
uniforms drive motion, and they get to do it with a beat that was actually
measured.

**28. The wall may never reach the axis.** A ring's radius is a base plus the
waveform times display gain, clamped so it can never fall below a fraction of
the base. Full scale at x8 would otherwise close the tube around the eye, which
draws as a screenful of flat colour and reads as a bug.

Display gain is an **amplitude multiplier** here, not a decibel offset: this
visual draws samples, so it is the vectorscope's category (M2 decision 12 drew
that line, and it holds because this is the other side of it). `scale::gain_db`
is for the two spectrum displays and is not wanted here.

**29. Mid drives the radius; side twists the ring.** The waveform mapped around
the circle is the mid signal -- what both channels agree on -- and the side
signal rotates each ring slightly as it is laid down, so a wide mix corkscrews
and a mono one runs straight. The goniometer's own two quantities, which the
vectorscope already draws as its axes, doing here what they mean.

The alternative, left on the table deliberately: left around one half of the ring
and right around the other, which is more literal and reads worse -- a mono
signal would draw a mirror-symmetric tube and every stereo one an asymmetric
lump, where the mid/side reading gives a shape that is *about* the stereo field
rather than a picture of two channels stacked.

**30. Nothing outside `player-render`.** Third milestone, same assertion. The
feed already carries everything this visual reads.

### Shape of the work

- **`visuals/tunnel.rs` + `tunnel.wgsl`** -- the ring history, the path, the
  camera that rides it, and the tube's mesh. As with the waterfall, the mesh is
  an index buffer and `vertex_index` arithmetic: a vertex is (ring, angle), its
  radius comes from the history texture, and its world position comes from
  `path`. No vertex buffer of geometry, and one row of writes per tick.
- **`VisualKind::OscilloscopeTunnel`** -- one arm each in `ALL`, `id`
  (`"oscilloscope-tunnel"`), `name`, `description` and `create`. Both galleries
  pick it up with no edit, for the third time.
- **The path and the camera, in one place.** `path(index) -> point` plus its
  tangent, with the camera a `look_at` from `path(now)` toward `path(now +
  lead)`. This is the one piece of new *math* in the milestone, and it is pure,
  so it is unit-testable without a GPU -- which is where most of its checks
  belong.
- **`visuals/history.rs`, if decision 23's second caller is real.** Decided by
  building the tunnel's ring first and then looking at the two side by side, not
  by predicting it here.
- **Nothing else.** Named again because it is the assertion.

### What "verified" has to mean here

The host contract's five per-kind checks apply the moment the kind is registered.
Then the tunnel's own claims, natively in `visual_smoke` and mirrored figure for
figure at `/visualizer.html`:

- **the eye is always inside the tube** -- the distance from `path(now)` to the
  camera is zero by construction, but the *wall* is at a radius that the
  waveform moves, so the real claim is decision 28's: at full scale, at every
  gain setting, the smallest radius any ring can reach is still comfortably
  outside the near plane. A unit test, in radii, over the whole range of inputs;
- **the frame is enclosed** -- no background pixels at any of the eight window
  shapes the other two visuals' fits are pinned at. This is decision 24's pixel
  half, and it is the check that would catch a 21:9 fullscreen showing the
  outside of the tube at the corners;
- **a louder passage widens the tube's modulation**, monotonically, across three
  amplitudes -- the tunnel's version of "taller means louder", and the check that
  says the display is about the audio at all;
- **the tunnel advances on a clock**: a second of travel drawn as sixty frames
  and as one leaves the same ring at the same place, the check M3's rows already
  answer for and which every timed visual now inherits;
- **history forgets on time** -- a ring made by a burst is gone after the
  tunnel's own length, bracketed from both sides the way M3's is, so neither
  shell needs to be told the constant;
- **an empty feed draws nothing**, and a feed that stops holds the tube still
  rather than laying silent rings -- decision 21's rule, which is now the
  house rule for a visual with history;
- **the path is stable**: two renders of the same history at the same time are
  byte-identical, and the camera at ring *n* is where the tube's ring *n* is
  (decision 25, as arithmetic rather than as pixels).

And the half no readback reaches: **a person watching it move with real music on
both shells**, with two things to look for that the numbers cannot answer.
Whether the speed and the bends are *comfortable* -- a tunnel is the first visual
in this app that can make someone feel unwell, and the honest place to find that
out is a person watching it, not a pixel count. And whether the waveform is still
legible as a waveform once it is wrapped around a circle and flown through, or
whether it reads as generic sci-fi motion with the music incidental to it.

### What this milestone will not do

- **No beat-reactive motion** (decision 27) -- M5 measures the beat, M6 drives
  scenes with it.
- **No new analysis, frame field, meter, command, binding or UI** (decision 30).
- **No settings** -- speed, radius, bend and colour are chosen constants with
  their reasons written down, revised after the live look if the look says so.
  The host still has no per-visual settings mechanism and M4 is not the place to
  invent one.
- **No `camera::fit`** (decision 24), and no change to it: the two visuals that
  use it are correct and this one asks a different question.
- **No compute shaders and no particles** -- M6's ground, checked there.
- **No second sample window** -- `TRACE_SECONDS` stays what the vectorscope made
  it (decision 23).

### What landed

**The assertion held for the third time.** Everything is inside `player-render`
-- the registry, `visuals/tunnel.rs` + `tunnel.wgsl`, the extracted
`visuals/history.rs` -- with one file outside it: `app/src/visualizerSmoke.ts`,
the web *check* rather than the web shell. No `src-tauri`, no `player-rt-wasm`,
no UI, no command, no new frame field, no new meter. Both galleries list the
fourth visual with nothing edited.

**`visuals/tunnel.rs` + `tunnel.wgsl`.** Rings of the waveform, 60 a second over
240 rings -- four seconds of tube -- laid along a curve the eye flies down at
3.6 units a second, which is `RING_HZ * RING_SPACING` rather than a dial of its
own. A ring is the tap's whole 120ms window wrapped into a circle and decimated
to 96 points, so two consecutive rings share most of their shape and the overlap
is what draws the ridges running along the tube. Mid displaces the wall, side
twists each point around its ring.

**`visuals/history.rs`, the extraction decision 23 held open.** It was real: the
tunnel wanted the waterfall's ring buffer exactly, so `RingHistory` now owns the
texture, the head, the clear and the `fill`, and `RowClock` owns the arithmetic
that makes a 60Hz redraw thread and a 144Hz browser draw the same history. The
waterfall moved onto it with all 48 of its checks unchanged, which is what makes
this an extraction rather than a rewrite. What a row *means* stayed with each
visual, which is the line that made the extraction safe: dBFS per band in one,
`[mid, side]` per point in the other.

**The camera, and what it is not.** `camera::fit` is untouched and unused here.
The tube's enclosure is a different claim with different arithmetic
(`encloses`): inside a straight tube a ray at angle `theta` from the axis meets
the wall at `radius / tan(theta)`, so the corners are wall as long as the tube is
longer than `radius / tan(corner)` -- and the *middle* is the opening, by design,
fading to the background rather than showing a hole. A wide window is the easy
case; a tall one is the hard one. That is checked as angles in a unit test, as a
`debug_assert` where the projection is built, and as border pixels at seven
window shapes in both smokes.

### Four things found while building

**The field of view was the surprise, not the path.** Inside a tube the field of
view decides how much of the *near* wall is in frame, and at this crate's usual
60 degrees the near wall sits outside the frustum entirely: what is left is the
far half of the tunnel, which is small, slow and fogged, and reads as a gradient
with a hole in it. 80 degrees now, chosen by drawing it and looking -- the same
way M3's camera was.

**A tube can bend out of its own sight.** The first path constants curved the
tunnel by about a radius every hundred rings, which hides the far end behind the
near wall: there is then no tunnel to look down, only a lit surface sliding past.
So the cycle counts are a *curvature budget* rather than a taste, and they are
written down as one. Also found by looking, and also invisible to every check --
the enclosure check in particular was perfectly happy, because a tube that has
bent out of sight encloses the frame beautifully.

**Two rings of tunnel draw nothing at all.** A waterfall seen from the front
shows its newest row immediately, which is why M3's skirt was enough. A tube is
seen *end-on*: two rings of it are a hoop six hundredths of a unit long, subtending
a ring of angles far outside the field of view. Either the first measurement
fills the whole ring buffer or there are four seconds of black after pressing
play. It fills it, and the opening shape is that first measurement repeated --
replaced ring by ring as real history arrives.

**The host contract's switch check was comparing two histories.** It rendered a
switched-into visual against a renderer that had already drawn twice, which was
fine for three stateless-enough visuals and wrong the moment a visual's picture
depends on how many frames it has seen. It passed for the waterfall only because
that visual's clock is slower than the frame rate, so the extra frames pushed no
rows -- luck, not design. Both sides now start from a renderer that has drawn
nothing, on both shells. The check still catches what it was written for: a
pipeline or attachment left over from the previous visual.

One decision was refined by building it. Decision 29 said the side signal
"rotates each ring as it is laid down"; the built version twists each *point* by
the side sample at that point. A per-ring twist would have been a statistic over
the window -- a measurement -- and decision 2 says a visual does not measure. Per
sample it is a geometric transform, exactly like the goniometer's own axes.

### Verified

**60/60** natively (`cargo run -p player-render --example visual_smoke`) and
**67/67** in a real browser at `/visualizer.html`:

| claim | native | web |
| --- | --- | --- |
| up the colour ramp at full scale / a third / a twentieth | 19.5 / -18.0 / -32.7 | 19.4 / -18.0 / -32.6 |
| the loud rings' centre after a second, in one frame / in sixty | row 210 / 210 | row 211 / 211 |
| loud pixels 2s after a burst / at 6s | 78514 / 0 | 78232 / 0 |
| loud pixels at x8 / at unity | 214216 / 78134 | 213976 / 77808 |
| pixels still down the tunnel at x8 | 1163 | 1256 |
| background pixels in the border, across seven window shapes | 0 | 0 |
| two frames of a stopped feed | 0 bytes differ | 0 bytes differ |

The figures that are *positions* agree to a pixel; the ones that count
anti-aliased area agree to well under a percent, for the reason M3's did -- two
backends resolving multisampling their own way, either side of a colour
threshold.

Also green: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
warnings`, the full suite (**276 tests**, up from 263 -- eight on the tunnel's
enclosure, path, ring arithmetic and gain, three on the shared clock, and the
waterfall's own unchanged), `npm run wasm`, `npx tsc -b`, `npm run build`.

**What the checks could not reach, and what the live look answered**: how it
*feels*. This is the first visual in
the app that can make a person unwell, and the speed (3.6 units a second), the
bend rate and the field of view are all comfort judgements that no readback
reaches. The other open question is whether the waveform still reads as a
waveform once it is wrapped around a circle and flown through, or whether it
becomes generic motion with the music incidental to it. Both are for a person
watching it with real music on both shells.


### Closed (2026-08-19)

Confirmed live on both shells: *"Speed and bend rate look good. No uncomfortable
feelings. Looks good in both desktop and web."*

That answers the question this milestone was least able to ask itself. Every
other visual here fails visibly when it is wrong; a tunnel fails in the viewer,
and the only instrument for it is a person. Three things follow:

- **The speed is right**, and it is not a free number -- it is `RING_HZ *
  RING_SPACING`, so confirming it confirms the ring rate and the spacing
  together. A later visual that flies anywhere has a figure to start from.
- **The curvature budget is right.** It was chosen to keep the far end of the
  tube in sight rather than for how it feels to fly, and those could easily have
  wanted different numbers. They did not.
- **The two constants found by looking are confirmed on real music**: the
  80-degree field of view and the three-and-two bend cycles, neither of which any
  check would have objected to.

It also closes PLAN.md section 4's built-in suite. Four visuals, all
user-confirmed on both shells, and the host has not changed since M1 -- M2 grew
the feed by two slices, M3 and M4 grew nothing outside `player-render` at all.
Three milestones built against a seam without moving it is the strongest
statement available that the seam was drawn in the right place.

M5 (musical intelligence) is next, and turns the phase around: its work is
`player-core` analysis -- onsets, beat tracking, spectral centroid -- verified
the analysis suite's way, against known answers rather than against pixels.

## M5 -- Musical intelligence: done, user-confirmed 2026-08-20

PLAN.md section 4's second modernization clause: "onset/beat detection, BPM
tracking, spectral centroid, transient detection feeding shader uniforms --
reacting to musical structure, not just raw amplitude." The phase's own ordering
puts it here because M6's scenes need it, and decision 2 says where it has to
live: **a visual computes no DSP, so a quantity a visual wants is added to
`player-core`'s analysis first, cross-target, under the same drain-and-reset
discipline as the meters.** M4's decision 27 already deferred a beat-reactive
camera to exactly this milestone rather than building half a beat detector
inside a visual.

So this one turns the phase around. M2 to M4 were rendering milestones that
moved nothing outside `player-render`; M5 moves nothing *inside* it except one
chained addition to the frame. Its work is analysis, and it is verified the
analysis suite's way -- against known answers, not against pixels. A synthesized
120 BPM click track must read 120.0 on both shells, an onset must land within a
stated tolerance of where it was synthesized, and material with no steady pulse
must say so rather than report a confident wrong number.

### Decisions

**31. Musical intelligence is a meter, and it lands where the meters live.** New
modules in `player-core`, composed into `Meters`, fed from the one drain in
`analysis.rs::ingest`, reset on a broken listen exactly as loudness and
correlation are (Phase 1 M5 decision 2 -- a seek, a lap, or a gapless hand-off).
No new thread on either shell: the native analysis thread and the web worker
already call `ingest`/`update`, and neither learns anything new. Nothing touches
the audio callback (decision 9).

Being a meter also settles what it measures without a further decision: the tap
sits after the EQ and before the volume, so the tempo is the tempo of what the
listener is played, and turning the volume down does not change it. The studio
side reads the same numbers the visuals will, which is decision 2's whole point
stated for a new quantity.

**32. These are measurements over time, so they run on their own hop clock,
driven by samples rather than by the caller's cadence.** `analysis.rs` already
draws this line: the spectrum and the goniometer are *pictures of a window* read
at whatever rate the display asks, while loudness and correlation are pushed
every sample and hold their own state. Onset detection is firmly the second
kind, and getting it wrong would be a cross-shell bug of the exact family this
project keeps catching -- spectral flux measured between two drains is a
different measurement on every drain, and the native thread drains every 16ms
while the web worker drains on `requestAnimationFrame`. Two shells, two answers,
out of one implementation.

So the detector keeps its own short-time transform on a fixed grid: a window and
a hop **derived from the output rate** (like the K-weighting's coefficients,
Phase 1 M5) so the detection function runs at a fixed rate in *time*, and a
44.1kHz device and a 48kHz device resolve tempo identically. An internal
accumulator makes the caller's chunking irrelevant, and that is checkable rather
than asserted: the same audio pushed in one call and one sample at a time must
give identical figures.

**33. The display spectrum is not reused, and the detector's transform is not
published.** Reusing `SpectrumAnalyzer` looks like the thrifty choice and is the
wrong one three times over: its window is 2048 frames of *display* resolution,
its levels are 64 log bands with a 30dB/s release smoothing baked in, and it is
computed when the display asks rather than on a fixed hop. Flux across smoothed,
log-binned, irregularly-timed levels is not an onset detection function.

The converse holds too: the detector's own bins stay inside it. Publishing a
second spectrum would give the app two answers to "what does the music look
like", which is the one thing the single-tap architecture exists to prevent
(PLAN.md section 4). The cost of the second transform is the reason this needs
saying rather than an argument against it -- roughly a hundred transforms a
second of a 1024-frame window, next to the sixty of 2048 the display already
runs.

**34. Tempo is autocorrelation over the detection function, with harmonic
weighting and an interpolated peak.** Causal and streaming: a window of recent
detection function, autocorrelated over the lags that correspond to 60-200 BPM,
scored with the sum of each lag's own harmonics and weighted by a preference for
tempi near 120. Not dynamic programming over a whole track (offline, and this
app has no whole track -- it has a stream), and not a bank of comb resonators
(more state, the same answer, harder to check).

Two things about it are load-bearing. **Octave errors are the known failure
mode** -- a 120 BPM track reading 60 or 240 is what a naive tempo estimator does,
and the harmonic sum plus the tempo prior is the mitigation, checked explicitly
against a click track with an offbeat on every eighth. And **the lag grid cannot
express 120.0**: at a 100Hz detection function the lags either side of 120 BPM
are 120.0 and 117.6, so a peak reported as an integer lag is a meter that can
only ever be approximately right. The peak is interpolated in the
autocorrelation, and that is the difference between this milestone's stated
standard -- a known answer -- and mere plausibility.

**35. Beat phase is tracked, not re-derived every hop.** Once a tempo is
established the phase advances on the analyser's own sample clock and is
corrected *toward* observed onsets slowly, rather than re-estimated from scratch
each hop. A visual then gets a phase that moves smoothly and keeps counting
through a bar with nothing in it; the alternative jitters by a hop whenever a
weak onset wins an alignment, which reads as a stutter in anything driven by it.

The phase is published as a fraction of a beat, so a scene animates on it
without knowing the tempo, and it is measured at the newest analysed sample --
neither shell extrapolates it forward. If the live look says a pulse reads late,
that is a finding worth a decision in M6 rather than a guess made here.

**36. "Not established" is a value, and confidence rides beside the tempo.** A
BPM of 0.0 means no tempo: warming up, silence, or material with no steady pulse.
Not a NaN, for `FLOOR_LUFS`'s reason -- these numbers cross a JSON boundary on
the desktop shell, where a non-finite float silently becomes `null`, and both
shells have to report the same thing.

Confidence is published because it is what makes the honest answer usable: a
rubato piano piece should read low confidence rather than a firm wrong number,
and a readout or a scene can then decide whether to believe the tempo. That
makes "ambient material reports low confidence" a checkable claim rather than a
disclaimer in a doc.

**37. An onset is published as the strongest since the last drain; the
continuous figure is published raw.** Two quantities, because they answer two
questions. *Was there an onset* is an event, and a consumer polling slower than
the ~10ms hop would miss most of them if it were a flag -- so what is published
is the strength of the strongest onset since the previous drain, and a reader at
frame rate cannot miss one. *How percussive is it right now* is the normalised
detection function itself, published as measured, with any decay left to the
display: M2 decision 11 put the bars' falling caps inside the visual for this
reason, and a meter that smooths for one imagined frame rate is a meter two
shells can disagree about.

**38. A snapshot of its own, published under the same lock, in the same
instant.** `MeterSnapshot` is loudness and phase; what the music is *doing* is a
different question, and folding six musical figures into it would put a BPM in
the type the LUFS readout consumes. So `MusicalSnapshot` is its own struct with
its own `to_array` and field count -- and it is published in the same `Published`
record under the same lock natively, and written in the same step on the web, so
the two can never describe two moments. That is what Phase 1 M5 decision 4
bought, and this must not spend it.

The plumbing it adds is one of each thing, deliberately: one shared-buffer region
(before the EQ block, whose alignment helper stays what protects it), one Tauri
command, one wasm accessor, one function in `analysis.ts`, and their mirrored
field maps. Any figure written twice in TypeScript is a figure the shells can
disagree about.

**39. `VisualFrame` grows by one chained addition, and no visual reads it yet.**
`with_musical` beside `with_spectrum` -- which is the growth `visual.rs` named M5
as the reason for when the frame was made a struct rather than an argument list.
Both shells fill it where they already fill the spectrum. **The four confirmed
visuals are untouched**: M6 is where scenes are driven by beats, with M6's
decisions, and retrofitting a pulse onto the vectorscope here would be M6's work
done without them.

The loose end that leaves is named rather than hidden: the frame's new fields are
carried but unread until M6, so what proves the numbers are real in M5 is the
known-answer smokes on both shells and the readout below, not a picture.

**40. The studio side gets the numbers too.** A small readout -- tempo,
confidence, spectral centroid -- polled at the loudness panel's cadence rather
than at frame rate, because these are digits you read rather than watch. The
milestone brief already calls a BPM readout a feature for both audiences, and it
is also the cheapest instrument available for the failure this milestone is most
likely to have: a person who knows a track's tempo, watching the number, is a
better octave-error detector than any synthesized fixture.

### Shape of the work

- **`onset.rs`** -- the fixed-hop transform and everything measured on its grid:
  the mono sum, the windowed FFT, half-wave-rectified spectral flux as the
  detection function, its adaptive normalisation and peak picking (a threshold
  over a moving average, plus a refractory period so one hit is one onset), and
  the spectral centroid, which lives here because it is measured from the same
  bins on the same clock and would otherwise need a second transform to be
  honest about *when* it is.
- **`tempo.rs`** -- the detection function's history, the autocorrelation, the
  harmonic scoring and the tempo prior, the interpolated peak, the confidence,
  and decision 35's phase tracker. Pure arithmetic over a ring of floats: no FFT,
  no device, and no clock but the sample count it is told about, so most of its
  checks are unit tests with analytic answers.
- **`analysis.rs`** -- `MusicalSnapshot`, `MUSICAL_FIELDS`, `silent()`, and the
  wiring into `Meters::new`/`ingest`/`snapshot` and the discontinuity reset.
- **`playback.rs`** -- `Published` gains the snapshot, and one accessor joins
  `meters()`. The analysis loop itself does not change shape.
- **The shells, a line each in three places** -- a `musical` command and the
  redraw thread's `.with_musical(...)` natively; `WasmAnalyzer::musical()`, the
  worker's publish, and `webVisualizer`'s frame on the web; the shared buffer's
  new region and its field map in `audioSab.ts`.
- **`analysis.ts` and the readout** -- one adapter function beside `meters()`,
  and one component rendering it.
- **The checks** -- `crates/player-core/examples/musical_smoke.rs` and
  `app/musical.html` + `app/src/musicalSmoke.ts`, synthesising their own fixtures
  the way the loudness pair does, so neither needs an argument or a file.

### What "verified" has to mean here

Known answers, on both shells, agreeing figure for figure -- the standard
loudness was held to, because this is the same kind of claim. Plausibility is
specifically what a beat tracker is good at faking.

- **A 120 BPM click reads 120.0**, and a 90 BPM one reads 90.0, within a stated
  tolerance and after a stated warm-up. Two tempi rather than one, because a
  detector with a bug that reads its own prior instead of the audio passes the
  first check.
- **No octave error**: a 120 BPM click with an offbeat on every eighth still
  reads 120 rather than 240, and a click at 70 does not read 140.
- **An onset lands where it was synthesized**, within one hop plus one drain,
  measured against impulses placed at exact times.
- **Chunking cannot change the answer**: the same audio pushed whole and one
  sample at a time gives identical figures. A unit test, and decision 32's real
  content.
- **The output rate cannot change the answer**: the same material at 44.1kHz and
  48kHz reads the same tempo and the same centroid within tolerance.
- **The centroid has an analytic answer**: a 1kHz sine reads 1kHz within a bin,
  and band-limited white noise reads what its flat spectrum implies.
- **Material with no steady pulse reports low confidence** rather than a
  confident number -- the honest-failure check, and the one that keeps decision
  36 from being decoration.
- **A track hand-off restarts the estimate** rather than averaging across it: the
  move the loudness smoke already makes for the integrated measurement, and for
  the same reason -- a tempo spanning two records is a tempo nobody played.
- **Both shells agree**, `/musical.html` against `musical_smoke`, figure for
  figure. They run the same Rust, so a disagreement here is a plumbing bug, and
  finding those is what the paired smokes are for.

And the half no fixture reaches: **a person listening on both shells**, with the
material this phase's brief already named -- something with a firm beat *and*
something rubato or ambient, because a beat tracker verified only on
four-on-the-floor is not verified. Three things to watch for that no synthesized
click can answer. Whether the readout agrees with a track whose tempo the
listener knows. Whether it *settles* on real music rather than flickering between
tempi at every transition. And whether the beat reads as on time: the tap sees
audio slightly before the device plays it, while the analysis lags by up to a
drain, and those two errors point opposite ways -- if the net is visible, M6
needs to know now rather than after its scenes are built on it.

### What this milestone will not do

- **No beat-reactive visual** (decision 39). M6 drives scenes with these
  uniforms; the four confirmed visuals are not edited here.
- **No key, chord, downbeat or structure detection.** Beat phase is where a beat
  is, not where a bar is, and the section boundaries a scene might want are not
  in PLAN.md section 4's list -- so they are not started by accident.
- **No library-wide BPM.** Analysing whole files offline and storing a tempo per
  track is a library feature with a schema, a scan cost and a migration; it
  belongs to Phase 4's player-completeness work, not to a real-time meter.
- **No beat-synced transport**, no tempo-matched crossfades, no time-stretching.
  Nothing here reaches the DSP output path (decision 9).
- **No second published spectrum** (decision 33), and no change to the display
  spectrum's constants -- the four visuals confirmed on them stay confirmed.
- **No new thread on either shell, and no compute shaders.** The drain that feeds
  the meters feeds this; M6 is where WebGPU compute gets checked.

### What landed (2026-08-20)

**`onset.rs`, the detection function and the features on its grid.** A
short-time transform of its own on a hop derived from the output rate -- 480
frames at 48kHz, 441 at 44.1kHz, both landing on exactly 100Hz, so the two rates
this app actually meets resolve tempo identically. Rectified spectral flux over
log-compressed magnitudes is the detection function; an adaptive threshold over
the previous 350ms plus a 50ms refractory picks onsets from it; a slowly-released
peak normalises the transient figure to 0..1; and the spectral centroid is taken
from the same bins on the same clock, because a brightness measured on a
different window would be a brightness from a different moment. Decision 32's
promise is a test rather than a claim: the same audio pushed whole and in five
awkward sizes yields identical hops.

**`tempo.rs`, the tempo and the beat.** Autocorrelation over eight seconds of
detection function, scored across the lags for 60-200 BPM with each candidate's
own harmonics summed in and a log-normal prior at 120 BPM, then interpolated --
which is what lets 90 BPM (lag 66.67, unreachable by any integer lag) read 90.0
rather than 89.55 or 90.91. The phase is a PLL: it advances on the hop clock at
the estimated tempo and is pulled toward onsets, so it keeps counting through a
bar with nothing in it. Pure arithmetic over a ring of floats -- no FFT, no
device, no clock but the hop count it is told about -- so almost all of it is
checkable without a GPU or a browser.

**The wiring, and what it did not disturb.** `MusicalSnapshot` sits beside
`MeterSnapshot` in `analysis.rs`: a separate struct because "loudness and phase"
and "what the music is doing" are different questions, published in the same step
under the same lock because they describe the same instant. `Meters::ingest`
feeds the detector every sample and forwards each completed hop to the tracker,
and a discontinuity resets both -- a tempo measured across a seek or a hand-off
is a tempo nobody played, and the seam itself would otherwise be the loudest
onset in the track.

**One of each thing per shell**, as decision 38 said. Natively: a `musical`
command beside `meters`, and the redraw thread reading the figures out of the
same publication as the spectrum, so a scene's beat belongs to the audio in the
same frame. On the web: a six-float region in the shared buffer written in the
same step as the meters, `WasmAnalyzer::musical()`, and that region handed to
`draw_frame` *flat*, in `MusicalSnapshot::to_array`'s order, so the order is
stated once in Rust rather than once per shell. The EQ region's 8-byte alignment
helper -- written in Phase 1 M8 against "the day someone adds a meter" -- is why
that addition moved nothing else.

**`VisualFrame` grew by one chained `with_musical`, and no visual reads it**
(decision 39). So what both smokes check is that the addition is *inert*: the
same frame with a tempo, a beat phase and an onset in it draws byte-identically,
per visual, on a renderer that had drawn nothing either time.

**The studio panel is its own** (decision 40): tempo, confidence, transient and
brightness, polled at the loudness panel's 100ms because these are digits you
read rather than watch.

### Three things found while building

**Log compression made a held note fire onsets.** Compression is what stops a
kick drum from hiding every other onset in a mix, and its cost is that it
magnifies *relative* change -- which in a bin holding nothing but window leakage
is large, because the hop is not a whole number of cycles and a held sine moves
its own leakage tails every hop. Four seconds of one 440Hz note fired fifteen
onsets. Magnitudes are now normalised so a full-scale sine reads 1.0 and floored
at -80dBFS, which drops the residue from 62 to 0.18 against a click's 1005. Both
constants are sized by that measurement and say so where they are defined.

**The first confidence measure failed in the exact case it existed for.** "How
far the winning score beat the average candidate" reported *no tempo at all* for
the offbeat-eighths track -- the octave-error check, the one thing that
measurement was written to survive. A peaky pattern leaves most candidates deeply
negative, so the average says more about the pattern's shape than about
certainty. Confidence is now the periodicity itself: the normalised correlation
at the winning period, which is bounded, meaningful for any pattern, and
independent of what else was on the ballot.

**The web smoke ran a browser out of GPU devices.** The inertness check's first
version built two more `WasmRenderer`s per visual, and Dawn refused outright
(`E_OUTOFMEMORY` on the D3D12 command queue) partway through the run. The
baseline is now the fresh renderer the switch check already builds -- the same
renderer, the same empty history -- which is one device instead of two. The
native half does the same, for symmetry rather than necessity.

### Two more things found, both by the smokes

**A tempo can be confidently wrong about silence, and the native pass was
luck.** The web harness read **122.4 BPM at confidence 1.00 while a held 1kHz
tone was playing** -- with the detector reporting *zero* onsets and a peak
transient of 0.022 over the same nine seconds. The autocorrelation is normalised
by the detection function's own energy, deliberately, so that a pulse can be
found in quiet music as well as loud; the cost is that a periodicity in
numerical residue scores exactly as well as a drum. The native build read no
tempo for the identical fixture, which looked like agreement and was not: its
residue simply happened not to be periodic.

The fix is in `tempo.rs` and says what a tempo is: **beats are events, so a
window with fewer than four onsets in it has no tempo**, whatever it correlates
with. Four is lenient -- the slowest tempo tracked puts eight beats in the
eight-second window and the fastest twenty-six -- so half the beats can be
missed and a real pulse still reports. A unit test now pins both halves: a tiny
periodic detection function with no onsets reads nothing, and the identical
shape *with* onsets reads 120.0.

This is the second time in this milestone that the two shells disagreeing was
the instrument. It is also the answer to a question the unit tests could not
ask, because a synthetic detection function does not have a residue.

**The centroid was counting the noise floor.** A 1kHz tone read 1148Hz through
the real playback path where the unit test on clean synthetic audio read within
a bin of 1000. A centre of mass counts every contributor, and five hundred bins
of dither and resampler noise at -85dBFS are inaudible and *numerous*. Bins
below `MAGNITUDE_FLOOR` are now left out of the centroid entirely rather than
floored -- the same constant doing a second job, and for the same reason: what
in this spectrum is the music rather than the noise under it. The tone now reads
1061Hz natively and 999Hz on the web.

### Verified

**16/16** natively (`cargo run -p player-core --example musical_smoke`) and
**16/16** in a real browser at `/musical.html`, on fixtures generated from the
same arithmetic down to the noise bursts' seed -- the same click tracks, sample
for sample:

| claim | native | web |
| --- | --- | --- |
| a 120 BPM click track reads | 120.00 BPM | 120.00 BPM |
| ...at confidence | 1.00 | 1.00 |
| onsets over nine seconds (eighteen clicks) | 18 | 17 |
| the phase, once locked, is off the beat by | 0.062 | 0.047 |
| after a gapless hand-off into a 90 BPM track | 89.71 BPM | 89.70 BPM |
| an offbeat on every eighth reads | 120.00 BPM | 120.00 BPM |
| a 1kHz tone's brightness | 1061 Hz | 999 Hz |
| a held note's tempo | none | none |
| rubato notes: confidence / onsets | 0.33 / 38 | 0.42 / 34 |

The tempi agree to a hundredth of a BPM, which they must: both shells run the
same Rust over the same audio, and the only thing that differs is which
threads it runs on. The counted figures (onsets seen, phase at the moment of an
onset) differ by what a poller catches between two publications, and the
brightness figures by which instant was caught -- a click track's *newest*
window is silence most of the time, which is why "a click's own window is
broadband" is a maximum seen (12527 Hz natively, 21754 Hz on the web) rather
than a reading taken afterwards.

Also green: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
warnings`, the full suite (**305 tests**, up from 276 at the end of M4 -- ten on
the onset detector, fifteen on the tempo tracker, four on the composition in
`analysis.rs`), `npm run wasm`, `npx tsc -b`, `npm run build`, plus **64/64** in
`visual_smoke` and **71/71** at `/visualizer.html`, where the four new checks are
decision 39's: the musical figures change nothing a visual does not read.

**Live-confirmed on both shells (2026-08-20)**, twice: once on the plumbing and
the readout ("works as expected"), before the two fixes above, and again after
them on a longer look -- see the closing section below.
\n
### Closed (2026-08-20)

Confirmed live on both shells, on a second and more careful pass: *"It looks
smoother on web now than it did. Now it looks smooth in transitions, quiet
passages are smoothed out visually. All pass."*

That settles the two things the fixtures could not ask about.

- **The tempo gate reads correctly to a person.** "Quiet passages are smoothed
  out visually" is what `MIN_ONSETS_IN_WINDOW` looks like from the outside: the
  readout stops asserting a tempo where there are no beats to assert one from,
  rather than holding a confident number through a passage that has nothing in
  it. The web harness found that as a figure (122.4 BPM at confidence 1.00
  during a held tone); this is the same fix seen as behaviour.
- **The panel is legible while the music moves**, which is the half the smokes
  cannot reach at all -- they read numbers, not the experience of watching them
  change.

The first look of that session found something worth writing down, and it was
not a bug in this milestone: **a stale page.** The browser was still running the
bundle from before the wasm rebuild, and it *skipped a little on transitions* --
which is what a mismatched wasm module and shared-buffer layout look like from
the outside rather than a crash. A reload fixed it. WORKFLOW.md now says so
where the `npm run wasm` step is, because the failure is silent and looks like a
performance problem.

**M5 is closed.** M6 (particle and raymarched scenes) is next, and is the first
milestone whose visuals are driven by these uniforms rather than by
geometry-per-sample -- also this workspace's first compute shaders, whose WebGPU
limits get checked at its start on both shells rather than assumed until its end.
The frame fields M5 landed are carried but unread until then, which is the one
loose end this milestone leaves on purpose.

## M6 -- Particle / raymarched scenes: planned 2026-08-20

PLAN.md section 4's fifth built-in, and the first of its five modernization
clauses that needs both halves of this phase at once: "GPU-instanced particles
and SDF raymarching driven by the musical-intelligence analysis below, not raw
amplitude." M5 measured the music and published it; decision 39 deliberately
left `VisualFrame::musical` carried but unread, because driving a scene with a
beat is a set of decisions and M5 was not the milestone to make them. This is.

It is also this workspace's first **compute shaders**, which the phase's risk
list says get checked at the milestone's start rather than assumed until its
end. That check is below, and it is already answered -- which is the difference
between a risk and a fact.

Three things ship: a particle field, a raymarched solid, and -- settled by the
user rather than by argument -- M4's deferred beat-reactive camera, which had a
standing claim on this milestone (decision 27) and is one figure in one existing
visual rather than a retrofit of four.

### Decisions

**41. Compute's limits were checked before anything was built on them, and they
were already in hand.** Both shells build their device with
`wgpu::DeviceDescriptor::default()`, which requests `wgpu::Limits::default()` --
the WebGPU specification's own baseline: 256 invocations per workgroup, 65535
workgroups per dimension, 16KB of workgroup storage, 8 storage buffers per
shader stage, and a 128MiB storage binding. That request has been succeeding on
both shells since Phase 0, so compute is not a new capability to negotiate: it
is one this project has been granted all along and has not yet spent. A dispatch
of 65535 groups of 256 is 16.7 million particles, four orders of magnitude past
what this milestone wants.

The one limit that is genuinely not settled by that is **storage buffers in the
vertex stage** -- the specification splits that out per stage, and compatibility
mode grants zero of them. So the particle buffer is never bound to the vertex
stage at all: it is created `STORAGE | VERTEX` and read as an **instance-step
vertex buffer**, which is the oldest and most universally supported way to draw
one quad per particle. The compute pass writes it as a storage buffer; the draw
reads the same bytes as instance attributes. No limit anyone can vary is
involved. The milestone's opening spike confirms the real numbers on both shells
rather than trusting this paragraph.

**42. The simulation is GPU state on a fixed timestep with a carried
remainder.** A particle field is the first thing in this crate whose state is
*advanced by arithmetic* rather than replaced by a measurement -- the waterfall
and the tunnel keep history, but every row in it was written once and never
touched again. State that integrates is state whose answer depends on how it was
chunked, and this project has caught that family of bug twice already (M3's row
clock, M5's hop clock). So the sim steps at a fixed 120Hz with the leftover time
carried, driven by `RowClock` from `visuals/history.rs` -- its third caller,
extracted in M4 for exactly this reason.

The property that buys, stated as M3 stated it: **sixty frames of a sixtieth
move the field exactly as far as one frame of a whole second.** The desktop
redraws at 60Hz and a browser's animation callbacks run at whatever the display
does, and neither may be visible in the motion.

**43. An onset enters a visual only through an idempotent envelope -- never as
an edge, never as an accumulator.** This is the decision the code would get
wrong by default, and getting it wrong would be a cross-shell bug of the exact
family this project keeps finding.

`musical.onset` is not an event a visual receives. It is **the strength of the
strongest onset since the last analysis drain, held in the published snapshot
until the next drain replaces it** (M5 decision 37). The native analysis loop
publishes every 16ms; the web worker publishes on its own cadence; the
visualizer reads whatever is standing, at display rate. On a 60Hz display those
line up roughly one-to-one, by luck. On a 120 or 144Hz display -- an ordinary
laptop today -- the visualizer reads the same published onset on two or three
consecutive frames, and a visual that spawned particles per frame it *saw* an
onset would spawn two or three times as many on better hardware.

So onsets are consumed exactly one way, in both new visuals and in nothing else:

    energy = max(energy - decay * dt, onset)

Reading the same onset twice is `max` applied twice, which changes nothing.
Missing one is impossible, because the value is held rather than pulsed. The
decay is in **seconds**, like the bars' falling caps (M2 decision 11), so the
recovery is the same on every display. Emission, brightness and displacement are
then functions of `energy`, and frame-rate independent by construction.

The alternative -- publishing a monotonic onset counter so a consumer can diff
it -- was considered and rejected: it is a change to `MusicalSnapshot`, the
shared buffer's field map, a Tauri command, a wasm accessor and two mirrored
field maps in TypeScript, to buy a property `max` already has for nothing.

**44. Randomness is a pure function of index and generation, never stored
state.** A particle's spawn position, velocity and lifetime come from hashing
`(particle_index, generation)` in the shader -- so a particle is reproducible
from its identity alone, both shells compute the same field from the same feed,
and a check can assert a position rather than a histogram. No seed buffer, no
host-side RNG, and nothing that makes the picture depend on how many frames
happened to have been drawn before the check started looking.

**45. Confidence gates the beat, in every visual that uses it.** A `bpm` of 0.0
means no tempo, and low confidence means do not believe the one there is (M5
decision 36). A scene that pulsed anyway would be exactly the "stylized
visualizer faking reactivity" PLAN.md section 4 defines this suite against. So
every beat-driven quantity is scaled by `beat_confidence`, and every visual has
a defined no-tempo behaviour: it moves with onsets and levels, steadily, and
asserts no pulse. That makes "ambient material reads as calm rather than
twitchy" a claim a check can make and a person can confirm, rather than a hope.

**46. The raymarched solid is a picture of the spectrum, not a demo.** Its
surface is a sphere displaced by the analyser's **own bands** -- placed by real
frequency through `VisualFrame::band_position`, the way the bars' octave marks
and the waterfall's rails are -- pulsed on `beat_phase`, and coloured by
`centroid_hz`. Not procedural noise with an amplitude multiplier bolted on.

That choice is what keeps this visual inside decision 2 in spirit as well as in
letter (noise satisfies "a visual computes no DSP" too), and it is what makes it
checkable at all: a bass-heavy input has to change the silhouette in a stated
direction, and a bright input has to move the hue. A scene whose shape came from
a hash could only ever be checked for "something is there."

**47. The raymarcher draws without multisampling, at a capped internal
resolution -- and both are its own business, not the host's.** Each visual
already builds its own depth and multisample attachments and resolves into the
view the host hands it, so the sample count is not actually crate-wide state; it
is a constant every visual has so far chosen to use. Four samples per pixel on a
fullscreen raymarch is four times the fragment cost -- which *is* the whole cost
of a raymarch -- to antialias geometric edges the scene does not have. Its edges
come out of the distance field and are smoothed in the shader for free.

The phase's risk list guessed that fullscreen cost would want "a render-scale
option, which is a host feature." Building it says otherwise: a render scale
that applied to the vectorscope would make its lines worse for no reason,
because the vectorscope is not what a 4K surface is expensive for. The cost
belongs to the visual, so the fix does -- the solid raymarches into its own
texture, capped at a fixed width, and upscales. **Fixed rather than adaptive**:
a quality level that responds to measured frame time is a picture that depends
on what else the machine was doing, which is neither reproducible nor
checkable. `attachments.rs`'s doc comment is corrected in passing -- the formats
and the sample count are the host's business, but whether a given visual
multisamples at all is the visual's.

**48. The tunnel gets the beat as a bounded offset on the eye, never on the ring
clock.** M4 deferred a beat-reactive camera to this milestone rather than
building half a beat detector inside a visual (decision 27); the user's answer
here is that the tunnel is the one existing visual that gets touched.

The obvious implementation is wrong. The tunnel's rings are laid on a clock
(`RING_HZ`, 60 a second) and its speed is that clock times the ring spacing, so
"fly faster on the beat" by speeding the clock would change how many seconds of
audio the tube holds and what each ring is a picture of -- the beat would be
altering the measurement. Instead the beat moves **the eye along the path it was
already on**:

    eye = eye_parameter() - SURGE * confidence * (1 - cos(2 * PI * beat_phase)) / 2

Bounded by `SURGE` rings, exactly zero at every beat instant, and continuous at
the wrap -- so the eye eases back through the bar and arrives forward onto each
beat. It cannot drift, and it can never outrun the history it is flying through.
The average speed is unchanged, so the tunnel still holds and shows its stated
four seconds.

And the property that makes this safe to do to a user-confirmed visual: **with
no tempo the expression is identically `eye_parameter()`**, so M4's sixty checks
-- none of which feed a tempo -- must still pass byte for byte. They are the
regression net, not a formality.

**49. Nothing outside `player-render` changes, for the fourth milestone
running.** Two lines of registry, four new files (`visuals/particles.rs` +
`.wgsl`, `visuals/solid.rs` + `.wgsl`), one edit to `visuals/tunnel.rs`, and one
file outside the crate: the web *check*. No command, no binding, no UI control,
no persisted setting, no new meter -- and both galleries list two more visuals
with nothing edited. M2 said that if building a visual required touching
anything else, the host was designed wrong; M3 and M4 held it for a visual with
history and a visual with a moving camera. M6 asks it of a visual with a compute
pass and GPU-side state, which is the furthest the seam has been pushed.

**50. Two visuals, one milestone, with a live look between them.** The particle
field lands and is looked at on both shells before the solid is built, so the
first compute-driven scene's constants -- emission, decay, drift, how a beat
reads as motion -- are settled by observation before a second visual inherits
them, the way M2's camera, window and cap rate were settled before M3 inherited
them.

### Shape of the work

- **The spike, first, both shells.** A compute pipeline that does nothing
  interesting -- dispatch, write a storage buffer, draw it as instances -- run
  natively and in a real browser, reporting each adapter's actual limits against
  decision 41's numbers. It either becomes the particle field's skeleton or is
  thrown away; either way nothing else starts until it has run in Chrome.
- **`visuals/particles.rs` + `particles.wgsl`.** The compute pass (spawn, age,
  integrate), the instanced draw (one quad per particle, additive, no depth
  write), `RowClock` at 120Hz, decision 43's envelope, decision 44's hash, and
  decision 45's no-tempo behaviour.
- **`visuals/solid.rs` + `solid.wgsl`.** A fullscreen triangle into an
  internally-capped texture, an SDF sphere displaced by `levels` through
  `band_position`, pulsed by `beat_phase` scaled by confidence, hued by
  `centroid_hz`, and the upscale into the host's view.
- **`visuals/tunnel.rs`.** Decision 48: one expression, one constant, and a unit
  test that it vanishes without a tempo.
- **`visual.rs`.** Two `VisualKind` entries with their ids, names and
  descriptions -- the whole of what either shell learns about them.
- **`attachments.rs`.** The doc correction decision 47 names.
- **The checks.** New sections in `examples/visual_smoke.rs` and
  `app/src/visualizerSmoke.ts`, plus the per-kind host contract loop picking up
  two more kinds with no edit.

### What "verified" has to mean here

The three kinds this phase has used, plus one new claim class -- that a
simulation is a simulation:

- **The host contract, per kind, free**: silence draws nothing, audio draws
  something, a resize is survived, a switch equals a fresh renderer. Two more
  kinds, no edit here.
- **The compute pass actually ran**: with the feed held still and no new audio,
  the field must *change* between two frames -- the opposite of the tunnel's and
  the waterfall's "a stopped feed is a frozen picture", and the only check in
  this crate that asserts motion without input.
- **Chunking is invisible** (decision 42): sixty frames of a sixtieth against one
  frame of a whole second, asserted on pixels, the way M3 asserted its row clock.
- **Onsets are idempotent** (decision 43): the same onset fed on N consecutive
  frames must produce the same field as feeding it once and then zero. That is
  the 144Hz display bug written as a check -- one that fails the naive
  implementation and passes the decided one.
- **The solid is a picture of the spectrum** (decision 46): a bass-heavy
  spectrum against a treble-heavy one must move the silhouette in a stated
  direction, and a centroid sweep must move the hue monotonically.
- **The tunnel is unchanged without a tempo** (decision 48): M4's sixty checks,
  byte for byte -- plus one new one, that a tempo does move the eye, and by no
  more than the bound.
- **Both shells, agreeing** to a pixel on positions and to under a percent on
  areas, as M3 and M4 reported.
- **A person watching it, twice**: after the particle field and after the solid.
  These are the first visuals here where "correct" and "musical" are different
  properties, and only one of the two reads back.

### What this milestone will not do

- **No post-processing chain.** Bloom would flatter both of these visuals, and it
  is a host-level pass with its own decisions: what it composites, what it costs
  at 4K, whether a preset may ask for it. That question belongs to M8's format,
  and taking it here would answer it by accident.
- **No adaptive quality** (decision 47's second half).
- **No new analysis in `player-core`.** Everything both visuals need was
  measured in M5. If one of them wants a quantity that does not exist, decision
  2 says it becomes a meter first -- and that would be a finding worth writing
  down, not a shortcut to take inside a shader.
- **No retrofit of the vectorscope, bars or waterfall.** They are
  user-confirmed. The tunnel is touched because M4 explicitly deferred a decision
  to here, and that is the whole of the exception.
- **No WebGL2 fallback** (decision 8), and no second implementation of anything
  for a browser without compute: the web build already says plainly that it needs
  WebGPU.

## M6 -- Particle / raymarched scenes: done, user-confirmed 2026-08-22

Three things shipped, all confirmed live on both shells: the particle field, the
raymarched solid, and the tunnel's beat-reactive camera that M4 deferred here.
**95/95** natively (`cargo run -p player-render --example visual_smoke`, was
64/64 at the end of M5) and **104/104** at `/visualizer.html` (was 71/71), plus
**18/18** in both halves of the analysis smoke, which grew a fixture because of
what the first live look found.

The confirmations, in the user's words: density "good... a lot of different types
of music", the pulse "more of a breathing than flashing", the solid "reads as
shapes and looks like it's breathing", the tunnel's new camera "gave no
uncomfortable feelings", fullscreen on the solid "looks good", and swapping
between all six "smooth".

### What landed

**Compute, and it was already paid for.** Decision 41's spike ran first on both
shells and found the limits in hand: both request `wgpu::Limits::default()` and
have been getting it since Phase 0 (native, RTX 5060 Ti over Vulkan: 1024
invocations per workgroup, 49KB of workgroup storage; Dawn: 1024 and 32KB). The
one genuinely per-stage limit -- storage buffers in the vertex stage, zero of
them in compatibility mode -- is sidestepped by binding the particle buffer as an
instance-step vertex buffer, which turned out to be free insurance on this
machine (Dawn reports 16) and is kept for the machines that are not this one.

**`visuals/particles.rs`** -- one compute dispatch per fixed step, one additive
quad per particle, 131072 of them. Its three rules are decisions 42-44 and each
is a bug that would otherwise have been someone else's: the simulation steps on
its own 120Hz clock, an onset enters only through an idempotent `max`, and a
particle is a pure function of its spawn ordinal.

**`visuals/solid.rs`** -- a sphere whose radius at a latitude *is* the
analyser's band at that frequency. Decision 46 is the whole design and it is what
makes the visual checkable: bass-heavy material draws 0.50 of the frame wide at
the foot against 0.35 at the crown, treble-heavy reverses it (0.41 against 0.54),
and halving the band count changes the shape by nothing -- 0.50 either way,
because a band is placed by its frequency rather than by its index. Every one of
those figures is identical on both shells.

**`visuals/tunnel.rs`** -- decision 48, one expression. The beat moves the eye
along the path it was already on, never the ring clock, because the clock is what
makes a ring a picture of a fixed slice of audio.

**The assertion held, five milestones running.** Nothing outside
`player-render` changed for either new visual: two registry lines, four files,
and the web *check*. Both galleries listed the fifth and sixth visuals with
nothing edited -- verified in the running app, which lists all six.

### What the checks found, and what only a person could

Three real bugs, and the split between how they were found is the milestone's
lesson.

**The clock was wrong, in two visuals that were already confirmed.** The particle
field's own chunking check failed, and the cause was `RowClock` counting
*seconds* and dividing by an `f32` reciprocal a hair too large: a frame of
exactly one second at 120Hz asked for 119 steps rather than 120, and three frames
of exactly a third of a sixtieth -- what a 180Hz display hands over every frame
-- asked for one rather than two. It counts rows now (`1.0 * 120.0` is exactly
120.0), and the waterfall's 48 checks and the tunnel's 60 pass unchanged over the
fix. A check written for a new visual found a bug in two old ones.

**A sustained pad was a confident 142 BPM.** The user played organ and synth
material with no rhythm in it and the particle field pulsed anyway. The visual
was behaving correctly and believing a wrong number: `onset.rs` normalised
`transient` by its own recent peak alone, so material whose every flux value is
tiny still reported 1.0 -- the pad's largest detection value is 4.71 where a
piano attack reaches 136 and a click 108. The normalising peak now has an
absolute floor under it (30.0, sized from those measurements), and `tempo.rs`
says what a beat is: an onset must reach a quarter strength to count toward
`MIN_ONSETS_IN_WINDOW` or to pull the phase. The pad reports no tempo at
confidence 0.00 and the 120 BPM click track still reads 120.00 at 1.00. Both
analysis smokes gained the fixture (16/16 -> 18/18 on each), faded in over a
second, because a file that begins at half amplitude on its first sample really
does begin with an attack and the detector was right to say so.

**Fullscreen was "smooth but blurred" past about 480px.** That one was the
visual's: a spark is a quad in the *world* and the camera is fitted to the field,
so a taller viewport drew the same picture magnified rather than a finer one.
Sparks now hold a roughly constant size to the eye and emission follows the
viewport's area, so a bigger window buys both detail and density. 480 is the
reference because 480 is what had been watched and approved.

And three things only drawing the picture could have decided: particles are born
on a **disc** rather than a sphere (a sphere is a ball that happens to be
rotating), the first emission rates were five times too high and a saturated
field has nowhere to put a transient, and the solid's first broadband render was
a stack of slabs with holes punched through it -- sixty-four bands around a
sphere are walls nearly parallel to the ray at the silhouette, so the profile is
smoothed along the axis before it is ever marched. A bar may be a step because a
bar is its own object; a surface may not.

### What the measurements changed

Decision 47's cap was set with a stopwatch and the stopwatch moved it up by a
factor of three. Timed against the vectorscope at matching sizes so the readback
they share cancels: 1.34ms against 1.15 at 720p, 2.78 against 2.32 at 1080p, 4.51
against 4.57 at 1440p, 9.63 against 9.05 at 4K -- the march itself costs well
under a millisecond even at 4K here. The first guess of 2.4 million pixels would
have quietly softened every 1440p fullscreen window, which is *exactly* the
complaint the live look had raised about the particle field two days earlier. The
ceiling now sits just above 4K.

### What this milestone leaves for the ones after it

- **The web harness has a device budget**, and the gallery growing is what found
  it: four `WasmRenderer`s per visual worked at four visuals and made Dawn answer
  `E_OUTOFMEMORY` at five. The host contract now uses two per visual and every
  per-visual section runs on one renderer reset in place. M7 adds visuals with
  presets behind them, so this is the constraint to design that check against
  from the start, not to rediscover.
- **`transient` and `onset` now have an absolute scale.** Anything built on them
  later -- M8's preset format most obviously -- inherits a figure that means the
  same thing on a pad and on a drum kit, which was not true before this
  milestone.
- **Two shared pieces moved on their second caller**, as ever: `visuals/palette`
  (centroid to hue) and `hue_to_rgb` in `shared.wgsl`.

## M7 -- MilkDrop `.milk` compatibility: the spike, run 2026-08-22

Decision 7 required this milestone to open with a de-risk spike -- "parse one
classic preset, run its equations, render offscreen" -- before the engine was
committed to. It has run, against five real presets the user supplied in
`milkdrop/`, and it reports well enough to plan against. `cargo run -p
player-render --example milk_spike` is the report; it is a *report*, not a check
that passes, and it stays that way until the milestone has claims to check.

The corpus turned out to be harder than "classic", which is lucky. These are
MilkDrop2077/MilkDrop 3 presets (`MILKDROP_PRESET_VERSION` 201 and 300,
`PSVERSION` 2 through 4): HLSL pixel shaders in both passes, custom waves and
shapes, external texture packs, one sprite, and -- in `martin - Gin Tonic on Ice
003.milk` -- a **256-resonator bank with a nested integrator, written in EEL, run
every frame** to detect tempo. A spike against four-on-the-floor material would
have reported a much easier milestone than the real one.

### Leg 1 -- the format: 5/5 read

`milk/parse.rs`, 9 unit tests. The format is `key=value` lines with three habits
that are each a silent wrong answer rather than an error:

- **Code arrives one line per key and the lines join with a newline**, not by
  concatenation. The corpus writes `rx2 = gmegabuf((i2+o)%ni*8+5);//mirrored`,
  and concatenating lets that comment eat the next statement.
- **The numbering is spelled two ways.** Top-level blocks put an underscore
  before the index (`per_frame_1`, `per_pixel_1`, `per_frame_init_1`); the
  per-wave and per-shape blocks do not (`wave_0_per_frame1`,
  `wave_0_per_point1`, `shape_0_per_frame1`). Both are in this corpus. A parser
  that assumes one reads an empty program for half of a preset and reports
  nothing wrong.
- **Shader lines carry a leading backtick**, which is a marker MilkDrop 2 added
  so leading whitespace survives its own INI writer. Exactly one is stripped.

One difference between the two reference readers is worth having decided rather
than inherited: **MilkDrop asks for index 1, then 2, and stops at the first one
missing; Butterchurn takes every matching line in file order.** They differ only
on a preset with a gap, where MilkDrop truncates the block. File order is the
forgiving reading and is what this takes -- forgiving is the right default for a
feature whose whole point is a folder full of strangers' files.

What the corpus needs that this build does not do, named per preset rather than
ignored: **sprites** (MilkDrop 3's own extension, `[SPRITE1_BEGIN]`, which draws
an external PNG) and **texture packs** -- `sampler_fw_clouds`,
`sampler_MilkDrop3_002`, `sampler_MilkDrop3_003` name images that ship
separately from the presets. The two procedural noise textures are excluded from
that list because MilkDrop generates them and so can we.

### Leg 2 -- EEL: 31/31 blocks compiled, zero unknown functions

`milk/eel.rs`, 19 unit tests: a tokenizer, a precedence-climbing parser, and a
tree-walking evaluator over names interned to slot indices at compile time. Every
code block in every preset compiles -- 31 of 31 -- and the function surface the
corpus actually uses is small and closed: `megabuf`, `gmegabuf`, `sin`, `cos`,
`if`, `loop`, `max`, `min`, `rand`, `below`, `above`, `pow`, `abs`, `sqrt`,
`sqr`, `int`, `atan`, `asin`, `log`, `exp`. Twenty names.

Three semantics were verified against Cockos' WDL source rather than written
from memory, per CONVENTIONS.md, and each is load-bearing:

- **`NSEEL_CLOSEFACTOR` is `0.00001`**, and it governs both `==`/`!=` (`|a-b| <
  0.00001`) and *truthiness* (`|v| > 0.00001`). Presets are written by trying
  things until they look right, so the quirk is in the corpus whether the
  authors knew about it or not.
- **Memory is `NSEEL_RAM_BLOCKS` (128) blocks of `NSEEL_RAM_ITEMSPERBLOCK`
  (65536)**, reached lazily. The corpus opens with `loop(10000,
  megabuf(n)=gmegabuf(n)=0; n+=1;)`, so a preset really does walk it; allocating
  it eagerly would cost 64MB per preset.
- **Loops are bounded at `NSEEL_LOOPFUNC_SUPPORT_MAXLEN` (1048576)**, which caps
  one loop and says nothing about ten nested ones -- hence the budget below.

And one the corpus proved by itself: **names are case-insensitive.** One preset
writes `MINBPM = 70` in its init and reads `minbpm` in a loop three lines later.

One deliberate divergence: `rand` draws from a seeded generator the context owns.
MilkDrop's presets are not reproducible frame to frame and ours have to be, or no
preset could ever get the pixel-readback check decision 6 requires of every
visual in this crate.

**The cost, and the surprise.** Release build, this machine, ten frames each,
worst frame reported:

| preset | mesh 32x24 | mesh 64x48 | where it goes |
|---|---|---|---|
| Dude the walls are moving2 | 2122us | 2260us | per_frame, all of it |
| Royal - Mashup (190) | 2088us | 2120us | custom waves, all of it |
| riding the wave | 888us | 1146us | 245 frame / 84->360 pixel / 559 waves |
| Gin Tonic on Ice 003 | 710us | 1007us | 192 frame / 98->398 pixel / 419 waves |
| Venom Textures 2 Manda | 160us | 625us | per_pixel, all of it |

Worst case **2.3ms, or 13.6% of a 60fps frame** -- affordable, and that answers
the risk the phase named. But the *breakdown* overturns the obvious assumption.
**The cost is not per-vertex code.** In the two most expensive presets, `per_pixel`
costs nothing at all: one spends its whole budget in `per_frame` (the resonator
bank, 522k evaluated nodes in a single frame) and the other in **custom waves**
(512 points x three waves). Only the cheapest preset in the corpus is
mesh-dominated. So "shrink the mesh" -- the lever a renderer reaches for first,
and the one M6 had just used for the raymarch -- would have bought nothing on
either of the two presets that need it. Worth knowing before it is built.

One more figure for sizing: a single `per_frame_init` reached **990,045 nodes**.
A budget has to be per block and generous; `DEFAULT_BUDGET` is 4,000,000.

### Leg 3 -- the frame: it runs, and the readback earned itself immediately

`milk/frame.rs`, 6 unit tests. A MilkDrop frame is not a scene that gets
redrawn, it is a **feedback loop**: the previous frame is resampled through a
warped mesh into the new one, and everything a preset does visually is a
consequence of that. The spike stands that loop up under wgpu -- two
`Rgba16Float` targets ping-ponged, a warp mesh whose source coordinates are
computed per vertex on the CPU (which is where they have to be computed, since
`per_pixel` is EEL), and a composite pass -- seeds a bright 16x16 square into it,
advances 30 frames under each preset's own `per_pixel`, and reads the result
back.

It works, and the presets move differently from each other, which is the claim.
The quantitative one: **Gin Tonic's `per_pixel` writes `dy=0.005`**, which over
30 frames of a 128px frame predicts 19.2px of downward drift. The lit centroid
moved from 63.5 to 83.1 -- **19.6px**, the rest being the preset's own
`zoom=1.003`. The control (no preset, zoom 1.05) grows concentrically and does
not move; the preset with no `per_pixel` block at all stands perfectly still,
256 lit pixels, exactly its seed.

16-bit float for the pair is not a luxury: a loop that multiplies its own output
by a decay every frame quantises to a visible staircase in 8 bits within a
second.

**And the readback caught a bug in its first run.** The composite pass's
fullscreen triangle had its doubling applied twice -- once to the uv and once to
the clip position -- so the whole picture drew at quarter scale in one corner.
Every preset still "worked", and the lit centroid still moved. What said it was
wrong was one number: the seeded square's centre read `(31.5, 31.5)` where a
128px frame puts it at `(63.5, 63.5)`. Nothing about the picture's *behaviour*
was wrong, so a person watching it would have had no reason to look twice. Third
milestone running that a figure caught what an eye would not have.

### What the spike says the milestone is

- **The format is easy and is done.** Parsing is not the risk; it was 9 tests.
- **EEL is easy and is nearly done.** 31/31, twenty functions, and the two
  semantic traps are found and pinned. This was the phase's named risk and it is
  the smallest of the three.
- **The frame is understood and its skeleton stands.** What remains is
  MilkDrop's *full* per-vertex transform -- `zoomexp`'s radius-dependent
  exponent and the four `warp` sine terms -- which must be taken from MilkDrop's
  own source rather than anyone's memory, because getting it subtly wrong is
  invisible in a still frame and obvious in motion.
- **The shader translation is the milestone.** 244 shader lines across five
  presets use only 15 distinct constructs, which sounds small until you look at
  what two of them require. `float2 water = tex2D(sampler_noise_mq, ...)` -- HLSL
  truncates a `float4` to a `float2` implicitly and WGSL does not. `ret = 1-uv`
  where `uv` is a `float2` -- HLSL promotes the scalar and WGSL does not.
  Handling either one needs the *type of every expression*, so the translator is
  a small typed compiler and not a table of string substitutions. That is the
  piece to plan around, and it is why M7 splits.

### One thing the spike did not raise, and the user did

Presets arrive by the **hundred or the thousand** -- that is how people use
MilkDrop, and it is the stated expectation for this app. Nothing in the spike
tests it and everything in the plan has to assume it: a parse that never panics,
an unsupported feature that is a note rather than a refusal, an evaluator a bad
preset cannot hang, a selection model that can name a preset at all (`VisualKind`
is a fixed enum and cannot), and a scan that stays off the audio and UI threads
the way `library.rs` already does for music.

### Licensing, which is now a decision and not a risk

Checked rather than assumed: **the MilkDrop3 repository declares no license** --
GitHub's license field is null and there is no `LICENSE` or `COPYING` file at its
root. The five presets in `milkdrop/` came from that source. Supporting the
*format* is unaffected and license-clean (decision 7's Butterchurn-guided,
never-projectM route stands). **Shipping any preset with the app is a separate
question with no answer yet**, and until it has one, `milkdrop/` is local test
data rather than repository content.

## M7 -- MilkDrop `.milk` compatibility: planned 2026-08-22, against what the spike reported

The phase plan said M7 was "the one most likely to split or slip once the spike
reports -- planned as such rather than discovered as such". The spike has
reported, and it splits three ways along the seam it found: the format and the
equations are nearly done, the frame's skeleton stands, and the shader
translation is the milestone. So M7 lands in three parts, each of which is a
thing a person can watch:

- **M7a -- a preset draws.** The format, EEL, the full warp transform, the
  feedback frame, and the fixed-function pipeline: decay, gamma, video echo, the
  basic waveform, borders. No preset shaders. Verified on presets whose
  `PSVERSION` is 0, and on the corpus's presets with their shaders ignored.
- **M7b -- a preset draws *as itself*.** The HLSL-to-WGSL translator, warp and
  comp passes, MilkDrop's helper library and uniforms, and the blur chain the
  helpers need. This is the largest single piece in the phase.
- **M7c -- the rest of the surface.** Custom waves, custom shapes, motion
  vectors, and the preset library at the scale people actually use it.

Each is done only when it works on both shells and a person has watched it, as
ever. If M7b overruns, M7a and M7c are still shippable and still worth shipping:
a gallery that plays a thousand presets approximately is a better product than
one that plays none exactly.

### Decisions (49-58)

49. **A preset is selection state, and this is where the host changes.** Five
    milestones running, nothing outside `player-render` changed for a new
    visual. That ends here, deliberately and once: `VisualKind` is a fixed enum
    of ids and cannot name a preset, so `VisualKind::MilkDrop` becomes one entry
    whose selection *includes* a preset. The host gains a selected-preset
    alongside its selected-visual, persisted like the visual is, and the shells
    gain one command each to set it. Nothing else about M1's seam moves --
    a preset is still a scene, swapped between frames, on the thread that owns
    the renderer.
50. **The preset collection is a library, built like the music library.**
    Hundreds to thousands of `.milk` files is the stated use, so the pattern is
    the one `library.rs` already proved: a folder is scanned **off the audio and
    UI threads** (a blocking thread natively, a worker on the web) into a
    versioned index of name, parse status and unsupported notes; the `.milk`
    text is read on selection rather than held; a preset that fails to parse is
    *listed as failing* rather than hidden, because a collection where a tenth
    of the files silently do not appear is a collection nobody can debug.
51. **A preset is a stranger's program, sandboxed by budget rather than by
    trust.** Every block runs under `eel::Context::budget` and exhausting it
    ends that block's frame and marks the preset, never the process. Loops are
    bounded at EEL2's own 1048576. This is not hypothetical hardening: the
    corpus already contains a single `per_frame_init` costing 990,045 nodes, so
    the line between "expensive" and "runaway" is real and has to be drawn
    somewhere explicit.
52. **EEL is interpreted in Rust, one implementation, both shells.** Not
    transpiled to JavaScript -- that is Butterchurn's move and it is unavailable
    to a Rust core that has to run natively too -- and not compiled, because the
    spike measured a tree walk at 2.3ms worst case on the hardest preset in the
    corpus and a compiler is a large piece of work to buy back 13% of a frame
    that is not currently short.
53. **The mesh is not the performance lever, so do not build one.** Measured:
    in the two most expensive presets `per_pixel` costs nothing, and the budget
    goes to `per_frame` and to custom waves. A mesh-size or render-scale setting
    would leave both untouched. The lever that matches the measurement is the
    budget of decision 51 plus dropping a frame's *equations* rather than its
    pixels, and the mesh stays at MilkDrop's own default.
54. **HLSL is translated by a typed mini-compiler, never by text
    substitution.** Two constructs in the corpus force it: implicit truncation
    (`float2 water = tex2D(...)`, where the source is a `float4`) and scalar
    promotion (`1-uv`, where `uv` is a `float2`). WGSL permits neither, and
    deciding either one needs the type of the surrounding expression. So M7b
    parses HLSL into a typed tree and emits WGSL from it, and the emitter's
    output is checked by *compiling it*, which is a thing wgpu will do for free.
55. **MilkDrop's shader environment is ours to provide, and is written down.**
    `GetPixel`, `GetBlur1..3`, `lum`, `sampler_main`, the `texsize`/`aspect`
    uniforms, `uv`/`rad`/`ang`, `time`, `bass`/`mid`/`treb` and `q1..q32` are
    not HLSL, they are MilkDrop's standard library, and the translator is
    useless without them. They live in a WGSL prelude next to
    `visuals/shared.wgsl`, which is already the place this crate keeps the code
    every shader gets.
56. **The supported subset is a list the app can show, not a paragraph in a
    doc.** `parse::Unsupported` already names what a preset needs and this build
    does not do; the library UI shows it per preset. Out of scope for M7 and
    named as such: **sprites** (MilkDrop 3's own extension) and **texture
    packs** -- both are external image files, both are a content-distribution
    question rather than an engine one, and the procedural noise textures
    MilkDrop generates for itself are *in* scope because generating them is
    arithmetic.
57. **No preset ever ships with the app** (user decision, 2026-08-22 -- "We
    won't ship any presets. I added those for testing only"). Not "none until
    provenance is established": none. The engine's licence cleanliness
    (decision 7: Butterchurn-guided, never projectM) says nothing about `.milk`
    files, and the check that prompted the question found the MilkDrop3
    repository declaring no licence at all -- no SPDX field, no `LICENSE` file.
    So `milkdrop/` is local test data, gitignored alongside the audio for the
    same reason, and the corpus a check runs against is always whichever folder
    it is handed.

    Two consequences, both of which shape the milestone rather than following
    it. **The library of decision 50 is not a convenience, it is the only way a
    preset ever arrives** -- so "point at a folder" is a first-class flow with a
    first-run state, not a setting buried behind a working default. And **the
    gallery entry has to be honest when it is empty**: `VisualKind::MilkDrop`
    with no presets loaded must say what it needs and where such files come
    from, rather than appearing as a visual that draws nothing. Naming where to
    find presets is editorial content, not a licence question, and costs
    nothing.
58. **The web checks are designed against M6's device budget from the start.**
    `/visualizer.html` gets one `WasmRenderer` per section, reset in place with
    `setVisual` between claims -- the rule WORKFLOW.md already records. M7 adds a
    visual with N presets behind it, which is exactly the shape that found the
    limit at five visuals, so the per-preset checks run on one renderer and
    switch presets rather than building a renderer each.

### Shape of the work

**M7a.** `milk/parse.rs` and `milk/eel.rs` are done and stay. `milk/frame.rs`
grows MilkDrop's full per-vertex transform -- `zoomexp` and the four `warp` sine
terms -- **read out of MilkDrop's own source**, which is the one place in this
milestone where writing from memory is specifically forbidden and specifically
tempting, since the reduced version already looks plausible in a still frame.
Then the fixed-function state a preset carries in its 69 scalars: decay, gamma,
video echo and its orientation, `bDarkenCenter`/`bBrighten`/`bDarken`/`bInvert`/
`bSolarize`, the inner and outer borders, and the built-in waveform in its
several modes. `VisualKind::MilkDrop` joins the registry with decision 49's
selection, and the shells gain their one command each.

**M7b.** `milk/hlsl.rs`: a tokenizer, a parser producing a typed tree, and a
WGSL emitter, with the prelude of decision 55 beside it. The blur chain
(`GetBlur1..3`) is a real render pass MilkDrop runs when a preset asks for one,
so it lands here rather than in M7a, and only when asked for.

**M7c.** Custom waves and custom shapes -- the per-point and per-instance
programs the spike already runs, now drawn -- plus motion vectors, and the
library of decision 50 with its scan, index and UI.

### What "verified" has to mean here

The phase's three kinds of check apply, with one addition this milestone earns:

- **Pixel readback per claim**, both shells, as every visual since M1. The
  spike's leg 3 is the pattern: seed something known, advance a known number of
  frames under known equations, and assert *where it went* rather than that it
  changed. The spike's own bug is the argument -- the picture moved correctly
  and was still drawn at quarter scale.
- **Known-answer equation tests**, the analysis suite's way: EEL blocks with
  computed answers, and the two verified quirks (the close factor, the block
  memory) asserted rather than assumed.
- **A corpus run that must not regress.** `milk_spike` becomes `milk_smoke`: N
  presets parsed, M blocks compiled, zero panics, every unsupported feature
  named, and the worst frame cost reported. Run it against a folder of hundreds
  and the figure that matters is not that they all work -- they will not -- but
  that the count of those that do never falls.
- **A person watching it**, which for this milestone means a person watching
  *their own presets*, not ours. That is the only test of whether the warp
  transform is right.

### What this milestone will not do

- No sprite support and no texture packs (decision 56).
- No bundled presets (decision 57).
- No `.milk2` double presets, and no preset *blending* -- MilkDrop crossfades
  between two presets and that needs two engines live at once. It is a real
  feature and it is M8's problem or later.
- No preset editing. Reading is the whole milestone.
- No WebGL2 fallback (decision 8, unchanged): a preset engine is the last place
  to take on a second constraint set.

## M7a -- a preset draws: built 2026-08-22

The first of M7's three parts. What it claims is exactly its name: a `.milk`
file loads, its equations run, its picture moves the way its own arithmetic
says, and the whole thing is drawn by MilkDrop's feedback loop rather than by
something that looks like one. What it does *not* claim is that a preset looks
like itself -- that needs the shader translator, which is M7b.

Verified: **112/112** `cargo run -p player-render --example visual_smoke` (was
95/95 at M6), **123/123** at `/visualizer.html` in a real browser (was 104/104),
and **23/23** `cargo run --release -p player-render --example milk_smoke`
against the author's own five-preset corpus. 155 unit tests in `player-render`,
up from 109. Clippy (both targets), fmt, `tsc` and the web build green.

### Where the arithmetic comes from, and the licence finding that made it usable

Decision 57's licence check had already found the MilkDrop3 repository declaring
no licence at all. That is the repository the test presets came from, and it
would have been the obvious place to read the transform out of. It is the wrong
one: **MilkDrop 2 is Nullsoft's own, and is BSD 3-clause** (`Copyright 2005-2013
Nullsoft, Inc.`, in the header of every file in `vis_milk2/`). Reading that is
licence-clean; assuming its successor inherited the licence would not have been,
and the two questions look identical until someone checks.

So the warp transform, the eight waveform modes, the composite and every
scalar's default are read out of MilkDrop 2's `milkdropfs.cpp`, `plugin.cpp` and
`state.cpp`. `crates/player-render/src/milk/mod.rs` carries the attribution and
names the files, so any line here can be checked against its source, and
`THIRD-PARTY.md` records it where a reader of the repository rather than of the
crate will find it.

### The transform, which was the thing not to guess

The spike's reduced version -- zoom, stretch, rotate, translate -- turned out to
be wrong in *ordering* as well as in terms:

- **`zoomexp` makes the zoom depend on the radius**: `zoom2 = zoom ^ (zoomexp ^
  (rad*2 - 1))`. At the default `zoomexp = 1` the exponent is 1 everywhere and
  this collapses to a plain zoom, which is exactly why its absence was invisible.
- **Four sine terms ripple the picture**, at four rates, on an animated
  frequency set -- and they happen *after* the stretch and *before* the
  rotation. Moving them either way is a different picture, and there is a test
  that says which order this is: a quarter turn maps a warp displacement on u
  onto one on v, so warp-then-rotate and rotate-then-warp disagree by exactly a
  rotation, and only one of them is MilkDrop's.
- **The whole transform happens in aspect-corrected space and is un-corrected at
  the end.** That is the only reason a rotating preset draws circles rather than
  ellipses on a wide window, and it is checked as what it actually buys: a
  quarter turn moves a point the same number of *pixels* whatever the window
  shape.

`warp=1` is MilkDrop's **default**, so a preset that writes no motion at all
still ripples. That is worth knowing before writing any check about the loop:
two of `milk_smoke`'s claims set `warp=0` explicitly, because a measurement of
one behaviour with another running underneath it is not a measurement.

### The one line of MilkDrop's transform that is deliberately not copied

MilkDrop ends the per-vertex transform with `u += 0.5/texsize_x`, and its own
comment says why: without it "a 1-pixel-wide line of the image would wrap at the
top and left edges". That is the **Direct3D 9 half-texel rule** -- in D3D9 a
pixel centre and a texel centre are half a texel apart, and every D3D9 program
sampling a render target one-to-one carries a correction for it. wgpu, like
every API after D3D9, aligns them already.

Copied into a feedback loop, that line is not a correction, it is a
displacement, applied *again every frame*: the picture crawls half a pixel up
and left per frame forever. Fifteen pixels of a 128-pixel frame in half a
second, smeared, and looking like nothing at all except slow drift.

Found by a readback asserting where a seeded square *was* after thirty frames --
it read 48.5 where the frame's centre is 63.5, and 30 x 0.5 is exactly 15. There
is now a unit test that the transform is a **fixed point**: feed its own output
back in a hundred times and land where you started. Anything constant added per
frame fails that and nothing else, because a single frame looks perfect.

Fourth time in this phase that a figure caught what an eye would not have.

### What else is in it

`milk/state.rs` is the preset's ~70 scalars, and its shape is a claim about what
a `.milk` file *is*. Not a program with settings attached: a set of named
scalars, most of which are also variables the preset's own code may overwrite
every frame. `fDecay=0.98` in the file and `decay = 0.9 + 0.1*bass` in
`per_frame` are the same quantity reached two ways, and the renderer reads only
the second. So the scalars are not a struct the renderer consults -- they are
the values the per-frame variables are **reset to** before each frame's code
runs, which is what `LoadPerFrameEvallibVars` does. Three consequences the tests
pin: `q1..q32` carry from the init block into *every* frame rather than
accumulating across them; `per_pixel` starts every vertex from the frame's own
motion rather than from the previous vertex's; and a scalar the preset's code
cannot reach (`fWarpScale`, `fShader`) stays unreachable, so a preset writing to
it writes to a variable nothing reads.

`milk/wave.rs` is the eight built-in waveform modes -- the only thing that puts
*new light* into the loop, since everything else moves and fades what is already
there. A preset whose waveform does not draw fades to black in a couple of
seconds however right its warp is. Geometry only, no wgpu, because the modes are
the part with no visual tell when they are subtly wrong: a mode-4 script with
the momentum filter's two weights swapped still looks like a waveform.

`milk/frame.rs` is the loop under wgpu, in MilkDrop's order: warp with decay and
the preset's own wrap mode, then the overlays **into the feedback buffer** (so
this frame's waveform is next frame's trail), then a composite carrying video
echo, gamma, the four-corner tint and the four whole-frame filters.

`visuals/milkdrop.rs` is the `Visual`. `bass`, `mid` and `treb` are summaries of
the frame's own spectrum -- the licence `VisualFrame::peak` operates under -- so
decision 2 still holds and no DSP happens in a visual.

### Two things MilkDrop's own constants say, and both are kept

- **`6.28` in mode 0 is not tau.** It is 0.0016 short, so the ring falls a fifth
  of a degree short of closing, and the cross-fade over the first tenth of the
  ring is what hides it. Clippy flags it as an approximate constant; "correcting"
  it would change the shape of every mode-0 preset ever written.
- **Three waveform modes switch on the texture size** against four power-of-two
  cases and do nothing at all for a size that is not one of them -- which on a
  modern window means those modes draw at full alpha instead of the faint wash
  they are meant to be. This reads the same four points as a line in log width
  (0.07 at 256, +0.02 an octave), so a 1440-pixel frame gets the value the table
  implies rather than falling through.

### Decision 49, in the code

`VisualKind::MilkDrop` joins the registry, and the seam widens once and
deliberately: `Visual::set_preset`, with a default body that ignores it, plus a
selected preset beside the selected visual on both renderers. A default method
rather than a second trait, because the alternative is downcasting -- the
renderer holds a `BoxedVisual`, and asking it "are you the MilkDrop one" is
exactly the shell-side type knowledge the seam exists to prevent.

The selection is the *host's*, so it survives a visual swap: switching to the
bars and back returns to the preset that was showing. And with nothing selected
the visual draws MilkDrop's **defaults**, which are a real preset -- a slowly
fading circular waveform. A black rectangle would be the wrong answer for a
gallery entry someone is looking at before they have pointed the app anywhere,
and this is the state a fresh install is in, since no preset ships (decision 57).

One command per shell, and they differ for a reason neither side can avoid: the
desktop takes a **path** and reads the file in Rust, the web takes the **text**
because a browser has no path to give. Both end at `Renderer::set_preset`, so
both shells parse the same file with the same code and get the same list of what
it needs. The selection is deliberately not persisted: a stored path is a path
that will eventually name a file that has moved, and pointing at a folder is
M7c's flow and the one that deserves persistence.

### What the corpus check can and cannot claim

`milk_smoke` reports **1 of 5** presets drawing through M7a's own path, and that
is correct rather than disappointing. Three of the five set `fWaveAlpha=0.001`,
deliberately below MilkDrop's own 0.004 threshold, because their picture comes
from custom waves (M7c) and preset shaders (M7b). A fourth --
`MilkDrop2077 - Dude the walls are moving2`, which plainly sets `fWaveAlpha=0.303`
-- shows its waveform only once the music passes `fModWaveAlphaStart=1.110`, so
on a steady test tone sitting at exactly average by construction it is correctly
invisible.

An earlier version of that check asked the `Runtime` for the alpha and got the
first case right and the second wrong, producing a failing check against
entirely correct behaviour. **The gate and the draw have to ask the same
question**, so it asks by building the waveform. Every preset is still claimed
to load and run a whole frame without error, and the count that draws is printed
so M7b and M7c raise a figure rather than delete a claim.

### The two bugs this milestone's own checks found in it

- **A uniform's field order across the WGSL boundary.** Compositing MilkDrop's
  picture over the host background needed a new uniform field, and adding it in
  a different position on each side left the struct exactly the same size while
  `filters` read the background's alpha -- 1.0, which is `invert`, so every
  frame drew pure white. A size assertion cannot see that; there is a note
  beside it saying so.
- **The web harness's `fresh` baseline had drifted from the native one.** It
  reused a renderer that had already drawn one empty frame, on the stated
  argument that an empty feed measures nothing, pushes no history and steps no
  simulation. True of the first six visuals and **false of a feedback loop**,
  which advances on every frame it is given. The native half never had the
  problem because it builds a renderer per baseline; the web half rebuilds the
  scene instead, which costs no GPU device (decision 58).

### The background, which is a decision rather than a fix

MilkDrop paints the whole frame, so the host's near-black field would never
show. Its frame is **light on a dark field** -- black means "nothing here", not
"a black object" -- so it is *added over* `attachments::BACKGROUND` rather than
replacing it. That gives a fullscreen preset the same faint sense of a surface
behind it the rest of the gallery has, at a two-percent lift visible only in the
blacks, and it keeps a real cross-visual property true on both shells.

### What M7a leaves for a person to confirm

Everything a readback can reach is checked. What it cannot reach, and what M7b
should not be started before hearing:

- **Whether a preset looks right in motion**, on the user's own presets rather
  than on a corpus of five. The transform is the milestone and its failure mode
  is precisely the one no still frame shows.
- **Whether the feedback buffer's 2048-pixel cap is the right one.** It is
  MilkDrop's own ceiling rather than an invented number, and its rationale is
  sound -- presets are written against a frame no larger, and the loop's content
  is smear rather than detail -- but M6's resolution cap was set with a stopwatch
  and moved threefold from the cautious guess, so this one deserves a look at a
  real fullscreen window.
- **Whether the waveform reads at the right amplitude** against real music,
  since `bass`/`mid`/`treb` are this build's own normalisation of the frame's own
  bands and presets are written against MilkDrop's.

### M7a confirmed, 2026-08-22

All three, on both shells: *"some of the ones that load look right. Waveform
matches the real music on the ones that read it. Fullscreen looks good."*

The transform is right in motion, which is the milestone's own claim and the one
no still frame can make. The waveform reads at the right amplitude against real
music, which is what licenses this build's `bass`/`mid`/`treb` normalisation
standing in for MilkDrop's -- the shape matches where it matters, so decision 2
holds without a visual computing DSP. And the 2048-pixel feedback cap survives a
real fullscreen window, so it is now settled by observation rather than only by
MilkDrop's precedent.

*"Some of the ones that load"* is the honest figure and the expected one. A
preset drawing without its own pixel shaders is a preset drawing approximately;
`milk_smoke` reports the count and M7b's job is to raise it.

## The corpus, which is bigger than the spike measured and changes M7b

The live look produced a fact worth more than the confirmation. The user's
collection is **the MilkDrop3 set plus collections gathered over the years -- a
wide mix from every version of the format** -- and that is reported as the
normal way to have one: almost nobody who did not build a collection themselves,
years ago, assembles it file by file. They download collections. The format is
also **still alive**: presets are still written, still discussed, still expanded
on.

Three things follow, and they are corrections to the plan rather than additions
to it.

**The spike's construct inventory is a sample, not a specification.** 244 shader
lines and 15 distinct constructs came from *five* presets, all from one era and
one source. Designing the translator's grammar against that number is designing
against a sample of five, and the failure mode is discovering the sixteenth
construct after the emitter's shape is settled. So **M7b begins with a survey of
the real corpus** -- every shader body tokenized, every function, type,
statement form, swizzle and declaration shape counted across hundreds of files --
and the grammar is drawn from the measurement. The tokenizer is the translator's
own first component, so this costs nothing that was not already going to be
built; it just runs before the parser rather than after it.

**An era is a compatibility axis the plan had not named.** A collection spanning
every version means presets written against MilkDrop 1's fixed-function
pipeline (`PSVERSION 0`), MilkDrop 2's shader model 2, its later 3, and whatever
MilkDrop 3 added. M7a already handles the first. The survey should report the
distribution, because it says where the value is: if a third of a large
collection is `PSVERSION 0`, M7a is worth more than its five-preset corpus
suggested, and if the shader presets cluster in one model, that is the one to be
exact about first.

**A translator meets unknown constructs as a matter of course, not as an
exception.** Against thousands of strangers' files written across two decades,
"this preset uses something this build does not know" is a normal Tuesday. It
already has the right shape to land in -- `parse::Unsupported`, decision 56's
list the app can show -- and the requirement is that an unknown construct names
itself, falls back to M7a's fixed-function path for that preset, and takes
neither the frame nor the app with it. The figure that matters, per decision
50's index and the corpus check, is the count that draws, and that it never
falls.

## M7b -- a preset draws as itself: started 2026-08-22

The largest single piece in the phase, and the one the M7 split exists for.
Order of work: **survey, then grammar, then emitter**, which is a change from
the plan's shape and is the corpus finding above cashed out.

### The lexer and the type lattice, which the survey needed anyway

`milk/hlsl/lex.rs` and `milk/hlsl/ty.rs`, 23 unit tests, 178 in `player-render`
(was 155).

The lexer never fails -- an unknown character is a token carrying the character,
not an error -- for the same reason `parse::Unsupported` exists one layer up: a
collection of thousands has no useful notion of "rejecting" a file, only of
naming what it could not read. Two rules in it are the kind that are silent when
wrong. A `.` is an accessor after anything that can *end* an expression and the
start of a number anywhere else, which is what tells `texsize.zw` from
`ret.y += .11` -- both in the corpus, and the wrong reading turns an ordinary
file into an unsupported one. And comments are dropped while their newlines are
kept, because a line number is how a report tells an author which line lost, and
the corpus commonly carries a screen of commented-out shader.

`ty.rs` is decision 54's actual content: HLSL's promotion (a scalar takes the
other side's shape), its **truncation** (two vectors of different width reduce to
the narrower, silently -- which is why `float d = texsize.zw` compiles), its base
widening, and swizzle typing with the three ways a swizzle can be wrong. HLSL
writes matrices rows-by-columns and WGSL columns-by-rows, so `float3x4` is
`mat4x3<f32>`; square matrices hide that and the corpus's are square, so a test
is the only thing between here and a silent transpose the day one is not.

### The survey, and what it found before the grammar was written

`cargo run --release -p player-render --example milk_survey -- <folder>`
recurses a collection and reports rather than asserts: era distribution, every
call, every free identifier, every type, operator, swizzle, directive and
unknown character. It reproduces the spike's 244 shader lines exactly, which is
the tooling agreeing with the previous measurement before being trusted past it.

On its first run, against the same five presets the spike measured, it found
**three constructs the spike's inventory of fifteen did not contain**:

- **A shader body is not a statement list.** It is `sampler sampler_fw_clouds;`
  then `shader_body { ... }`. `shader_body` is a macro from MilkDrop's own
  prepended header -- which is where `ret` is declared too -- so the grammar's
  top level is global declarations followed by a wrapper block, and the loose
  `sampler` declaration is how a preset asks for a texture by name. That is
  decision 56's texture packs arriving through the shader rather than through a
  key, which the decision did not say.
- **Presets define their own functions.** `float sigmoid(float t, float c){...}`
  and a `vortex` beside it, in two of five files. Parameters, `return`, and a
  call graph, not a statement list.
- **A bool is a number.** `-(lum(ret)<sin(time*90))*ret*blur` multiplies a
  comparison by a `float3`. HLSL promotes silently; WGSL has no implicit
  conversion and no arithmetic on `bool` at all.

Three misses in five files is the argument for the survey, made by the sample
that prompted it. Against a collection spanning twenty years the sixteenth
construct is not a risk, it is a certainty, and the cheap time to meet it is
before the emitter has a shape.

The other figures from that run, as the baseline a real corpus will replace:
5/5 parsed, 10/10 shader blocks present, 2243 tokens, 20 distinct calls
(`GetPixel` 25, `GetBlur2` 15, `tex2D` 15, then `lum`, `saturate`, `lerp`,
`frac`, `mul`), 66 free identifiers, 18 operators, zero preprocessor directives
and **zero characters the lexer had no rule for**.

### The environment, read from MilkDrop's source rather than from memory

`milk/hlsl/env.rs`, 9 unit tests, 187 in `player-render`. Decision 55 said this
was ours to provide and had to be written down; it is now written down, out of
`plugin.cpp`'s `CPlugin::LoadShaderFromMemory` and `data/include.fx` under the
same rule M7a's transform was read under. (The `.fx` files are *shipped with the
binary* and absent from the source-only mirrors, which is worth knowing before
looking for them.)

**The shape is not what the plan assumed.** It said "the `texsize`/`aspect`
uniforms". In fact MilkDrop declares **fourteen `float4` constant registers**
`_c0.._c13` plus eight more `_qa.._qh`, and every named input is a `#define`
onto a lane: `#define time _c2.x`, `#define q17 _qe.x`, `#define aspect _c0`.
Sixty-odd names over twenty-two vectors. That is much less uniform plumbing than
one binding per name -- and it means two names can be the same storage, which
they are: `blur1_min` is `_c6.z` and `GetBlur3`'s bias is `_c6.y`.

**A blur texture is not a blurred picture.**

```text
#define GetBlur1(uv) (tex2D(sampler_blur1, uv).xyz * _c5.x + _c5.y)
```

The blur chain stores its result remapped into the `blur1_min`/`blur1_max`
range, and `_c5`/`_c6` carry the inverse so the shader gets the original values
back. A blur pass writing the blurred image straight out, with a `GetBlur1` that
merely samples it, agrees at the default settings and is wrong everywhere else.
That is the M7b equivalent of M7a's half-texel offset: a difference that reads
as "this preset looks a bit off" rather than as a bug, and the reason the blur
chain is not a thing to write from memory either.

**Two more facts worth having before any of it is built.** `ret` is declared by
the wrapper as `float3 ret = 0`, so a body that never assigns it draws black --
reaching the previous frame is what `GetPixel` is *for*, and "the warp shader
starts from the previous frame" is the intuitive reading and is wrong. And the
composite pass defines `uv_orig` as `_uv.xy`, the same as `uv`, where the warp
pass has `_uv.zw`; `plugin.cpp` carries a `//[sic]` on that line, so it is
Nullsoft's own known quirk and presets depend on it.

The five helpers are `#define`s rather than functions, which means a preset
declaring a function named `lum` does not override it -- the macro wins. That
matters given presets do declare functions of their own.

### The survey has run on a real collection: 11,217 presets

Three collections the user supplied -- Cream of the Crop, the MilkDrop3 set, and
a DeepField collection -- in nested folders, 2026-08-23. (A larger one exists,
around 150,000 files; this is already two thousand times the spike's sample and
the tail flattened long before the end of it.)

**Two things held that had to hold.** All 11,217 files **parsed, none failed**,
and across **4,359,378 tokens the lexer met zero characters it had no rule
for**. The parser was written against nine presets and the lexer against five;
neither has a corpus-shaped hole in it.

**The denominator is not the file count.** There are **6,104 distinct shader
programs** behind those 11,217 files -- a large collection is largely variants of
a smaller number of presets, so "how much of the corpus does the translator
reach" honestly means the 6,104.

**M7a is worth more than five presets suggested.** 1,764 presets -- **15.7%** --
have no shader at all (`PSVERSION 0`, `MILKDROP_PRESET_VERSION 0`), and M7a
already draws every one of them *as itself* rather than approximately. A sixth
of a real collection was finished when M7a closed.

**Per-shader profiles are mixed more often than not.** 4,684 presets are
PSVERSION 2 and 2,107 are 3, but 2,203 more set the two passes differently --
including 605 with **one pass fixed-function and the other a shader**
(`warp 0, comp 2` and its mirror). So the fallback when a shader cannot be
translated has to be **per pass**, not per preset. That is a design constraint
the plan did not have.

### What the corpus actually uses, by share of presets

Occurrence counts cannot size work -- an operator appearing 1,247 times in one
preset and in a thousand are different facts -- so the survey counts presets:

| construct | presets | share |
| --- | ---: | ---: |
| branches (`if`/`else`) | 2,816 | 25.1% |
| 3D texture sampling (`tex3D`) | 2,318 | 20.7% |
| loops (`for`/`while`) | 1,146 | 10.2% |
| `++`/`--` | 1,132 | 10.1% |
| `tex2d`, miscased | 746 | 6.7% |
| `static`/`const` globals | 677 | 6.0% |
| preset-defined functions | 644 | 5.7% |
| modulo | 633 | 5.6% |
| matrices | 379 | 3.4% |
| `#define` | 343 | 3.1% |
| array indexing | 323 | 2.9% |
| conditional compilation (`#if`) | 145 | 1.3% |
| non-float vectors | 55 | 0.5% |
| ternary `?:` | 54 | 0.5% |
| non-square matrices | 6 | 0.1% |

Six of those fifteen are constructs the spike's inventory did not contain at
all, and the top two are the expensive ones. **A quarter of the corpus
branches** and **a fifth samples a 3D texture** -- neither appears once in the
five presets M7 was planned against.

That sets the order of work, and it is not the order a translator is usually
built in. Statements before expressions get exotic: branches, loops, functions
and declarations are 25%, 10%, 5.7% and 6% of the corpus, and the preprocessor
-- which feels like it must come first because it is textually first -- is worth
3.1% and 1.3% and comes **last**.

It also promotes the noise textures from a footnote to required work. `tex3D` at
20.7% is `sampler_noisevol_lq`/`_hq`, which MilkDrop generates procedurally --
in scope by decision 56 precisely because generating them is arithmetic -- so
"the procedural noise textures are in scope" turns out to be a fifth of the
corpus rather than a nicety.

### Two things the corpus raised that are decisions rather than findings

**`tex2d` is not `tex2D`, and 746 presets write it.** HLSL is case-sensitive, so
those presets do not compile under MilkDrop either -- they fall back to its
default shader, and evidently nobody noticed, because a preset still draws.
Accepting the miscased spelling as an alias costs one line and makes 6.7% of the
corpus draw *better here than in MilkDrop*. Taken: the alias is worth more than
bug-compatibility with a compile error, and this is recorded rather than silent
because it is a deliberate divergence.

**2,204 presets -- 19.6% -- ask for a texture or a sprite this build does not
load.** 137 distinct names, led by `worms` (550), `clouds` (250), `rand00` (182)
and `seaweed` (124), plus 71 sprites. Decision 56 put both out of scope as a
content-distribution question rather than an engine one, and that stands -- but
the cost is now measured rather than guessed, and it is a fifth of a real
collection. Worth revisiting once the preset library exists, since "point at a
folder of textures" is the same shape as the flow decision 57 already forces for
presets themselves.

### The grammar, built against the measurement: 11,214 of 11,217

`milk/hlsl/ast.rs`, `milk/hlsl/parse.rs` and `milk/hlsl/pre.rs`. 227 unit tests
in `player-render`, up from 187.

Built in the order the table gave rather than the order a translator usually
takes, and measured against the whole corpus after each step. The figure is
presets whose every shader pass reads whole:

| step | presets read | share |
| --- | ---: | ---: |
| statements, declarations, functions, expressions | 9,986 | 89.0% |
| initializer lists, and MilkDrop's trailing-text discard | 10,100 | 90.0% |
| `sampler_state` blocks | 10,153 | 90.5% |
| the comma operator | 10,604 | 94.5% |
| `double` as `float` | 10,732 | 95.7% |
| the preprocessor | 11,209 | 99.9% |
| a lexer bug the last three files exposed | **11,214** | **100.0%** |

Every step after the first was a construct the corpus named and the plan did
not. **Six new constructs, none of them in the spike's inventory of fifteen and
none of them in the survey's first table either** -- because a table of what
presets *use* cannot show what a grammar *lacks*, and only running the grammar
over the corpus does.

- **Initializer lists.** `static const float2 offsets[10] = { sin(...), ... };`
  -- twenty scalars for ten pairs, so the emitter must regroup by the target's
  shape rather than trust the writer's grouping.
- **MilkDrop discards everything after the last `}`.**
  `LoadShaderFromMemory` overwrites from `strrchr(text, '}')` to the end, so
  presets that sign off after the brace -- "written by martin", "END" --
  compile there. 28 files, and reading MilkDrop's source is the only way to
  know this is legal rather than broken.
- **`sampler_state` blocks.** `sampler foo = sampler_state { AddressU = WRAP;
  };` -- the address and filter modes a wgpu sampler descriptor needs, so they
  are read rather than skipped.
- **The comma operator**, 457 shader bodies, almost always a `for`'s update
  clause. WGSL has none, so the emitter will have to unfold it.
- **`double`.** 136 shader bodies declare `double3 blur = GetBlur1(uv)`,
  plainly meaning a colour. `ty.rs` had said `double` was deliberately not
  modelled; the corpus disagreed. It folds to `float`, as the legacy D3DX
  compiler folded it, and as WGSL requires -- there is no `f64` in a fragment
  shader to be faithful to.
- **The preprocessor**, which the survey had ranked last at 3.1% and which
  turned out to be **523 of the last 528 refusals** -- the entire gap between
  95.7% and all of it. Ranking it last was right; the ranking just could not
  say that everything above it would be cleared first.

**The preprocessor is smaller than it sounds**, which the corpus also settled.
The commonest directives are `#define sat saturate`, `#if 1`, `#define MyGet
GetPixel`, `#define sampler_pic sampler_prayerwheel`, `#define PI 3.14159` --
object-like macros standing for one name or one number, and conditionals on
constants. So `pre.rs` is C's preprocessor in that shape: `#define`/`#undef`,
the full `#if` family with a constant-expression evaluator, and function-like
macros because they cost little once object-like ones exist. It works on
**tokens rather than text**, so the lexer runs once and no `.` has to be
re-judged as swizzle or number.

### The three bugs this stage found, and how

**A macro bomb test found a real expansion bug.** `#define A A` must be a fixed
point, so a name being expanded is not expanded again inside itself -- but the
first implementation kept that set flat for a whole top-level token, so
`#define SQ(x) x*x` over `SQ(A)` expanded the *first* `A` and left the second
standing. Silently: the output is still valid HLSL, just missing an expansion.
The set is now a region, closed by a marker pushed under each macro's body, and
the bomb test that was supposed to check the token cap is what exposed it.

**The lexer read `return .8` as a member access.** Its rule is that a `.` after
anything that can end an expression is an accessor -- and `return` is an
identifier to a lexer. `float dist (float2 uvi) {return .8*(uvi.y-.4);}` is in
the corpus and reported as an unsupported construct. Keywords are not values,
and the lexer now knows the short list that never are.

**The survey's own classifier counted `if` as a function call**, because `if (`
looks like one. 13,395 branches were hiding in the call list until keywords
were tested first. A measurement tool gets the same scrutiny as the thing it
measures.

### What the last three files are

Genuine syntax errors, in presets that do not compile in MilkDrop either: a
trailing comma before a closing brace, a `sampler` declaration missing its
semicolon, and an unmatched parenthesis. Nothing is gained by accepting them,
and accepting them would mean inventing tolerance HLSL does not have.

### What is next

The emitter: WGSL out of the tree, with decision 55's prelude behind it and the
types from `ty.rs` deciding every truncation and promotion. Decision 54 says its
output is checked by **compiling it**, which wgpu does for free -- so the
corpus figure to beat next is not "parses" but "compiles".
