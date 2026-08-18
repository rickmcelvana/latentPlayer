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

## M2 -- Spectrum bars: built, awaiting the live look on both shells (2026-08-18)

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

**What is not verified, and why**: how it *looks* with real music, on both
shells. The pixel checks say the axis runs the right way, that taller means
louder, that the caps fall in seconds and that nothing clips at any window shape
-- none of which is the same as it being good to watch. A spectrum analyser is
the most familiar visual in the app and therefore the easiest to draw
generically, so this is the milestone most likely to want the revision pass the
phase already budgets. The live look is what M2 is waiting on.
