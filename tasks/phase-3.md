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
