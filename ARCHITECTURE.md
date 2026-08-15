# ARCHITECTURE.md

One Rust core, two shells -- see PLAN.md §5 for the full rationale. This
file tracks what actually exists, not the aspirational end state.

## Crate map

```
crates/
  player-core/      Symphonia decode, PlaybackSource trait (LocalFile,
                     InMemorySource), native cpal output (playback.rs,
                     not built on wasm32). No UI/windowing code.
  player-render/    wgpu render engine. Takes any wgpu::SurfaceTarget
                     (native window handle or web canvas) -- knows nothing
                     about Tauri or the browser. Currently a 2D XY
                     oscilloscope; the literal precursor to the 3D
                     vectorscope (PLAN.md §4).
  player-rt-wasm/   wasm-bindgen glue for player-core's decoder and
                     player-render's Renderer (wraps both -- neither of
                     those two crates knows wasm-bindgen exists). wasm-only;
                     excluded from native `--workspace` commands, see
                     WORKFLOW.md. No per-block wasm calls on the audio path
                     -- decode happens once, up front, in a Worker; the
                     AudioWorkletProcessor itself is plain TS.

src-tauri/           Tauri 2 desktop shell. Two windows: the React webview
                     (UI, transport controls) and a second, plain
                     (no-webview) native window hosting a player-render
                     Renderer directly on a wgpu Surface built from that
                     window's raw handle (tauri::window::WindowBuilder,
                     requires the `unstable` tauri feature -- verified
                     against tauri 2.11.5's own source, see git history).
                     Playback and the scope's sample feed are both pure
                     Rust; the webview only sends transport commands
                     (load_file/set_playing) and never touches audio data.

app/                 React + TS + Vite. Same component tree renders in the
                     Tauri webview and (eventually) the plain web build --
                     matches ../latent-mixing/app/ exactly. isTauri() gates
                     which playback adapter a component uses:
                       - nativePlayer.ts: Tauri commands (desktop; decode +
                         output + the scope run entirely in Rust already)
                       - playerEngine.ts: AudioWorklet + wasm (web; decode
                         runs once in worker.ts, the worklet itself is
                         plain TS -- see player-rt-wasm above)
                     webScope.ts drives the web build's scope canvas
                     (WasmRenderer, via rAF) -- the web-side counterpart of
                     src-tauri's native redraw thread.
```

## Why native rendering works the way it does

The open question Phase 0's native spike existed to answer: can a
`wgpu::Surface` attach to a Tauri-managed window with no webview? Yes --
`tauri::window::Window<R>` implements `raw_window_handle::HasWindowHandle`
+ `HasDisplayHandle` directly (verified against tauri 2.11.5's source,
`src/window/mod.rs`), which is exactly what `wgpu::SurfaceTarget`'s blanket
`From` impl requires. Surface/device/pipeline creation happens on the main
thread during `tauri::Builder::setup()` (wgpu's own docs: Metal surface
creation must happen on the main thread, so this is done unconditionally
rather than "working by luck" on Linux/Windows only); only the redraw loop
itself moves to a background thread.

## Active bug (as of 2026-08-15, root-caused -- environmental, not app code)

**Web scope stays blank -- root cause confirmed via live `claude-in-chrome`
devtools console.** Native scope works (user-confirmed live). Web audio
sounds correct (user-confirmed live, see "Known gaps" below for the fix).
The web *scope* throws on load:

```
TypeError: Cannot read properties of null (reading 'requestDevice')
    at __wbg_requestDevice_ab46d0519ea1cc34 (player_rt_wasm.js)
```

Confirmed live in the page console (`navigator.gpu.requestAdapter()`
resolves to `null` on this dev machine's Chrome, checked directly via
`javascript_tool`) that this is Chrome's WebGPU backend (Dawn) returning no
adapter, *not* a missing-GPU or missing-Vulkan problem: WebGL2 on the same
tab reports real hardware acceleration (`ANGLE ... NVIDIA GeForce RTX 5060
Ti ... OpenGL ES 3.2`), and `vulkaninfo`/`nvidia-smi` on the host confirm a
working Vulkan 1.4 stack with NVIDIA driver 610.57.04. Per Chrome's own
WebGPU troubleshooting docs, Dawn carries its own adapter blocklist
(separate from `--ignore-gpu-blocklist`) that rejects NVIDIA driver 570+ by
default on Linux -- this machine's 610.57.04 driver is well past that
threshold, which matches the symptom exactly.

`player-render`'s own code (`Renderer::new` in
`crates/player-render/src/lib.rs`) does `instance.request_adapter(...).await?`,
expecting wgpu to turn a failed/null adapter into a proper `Err`. It
doesn't in this case -- the raw JS exception happens inside wgpu's own
generated webgpu-backend bindings, before request_adapter's Rust wrapper
gets a chance to check for `None`, so nothing in this app's code could have
caught it more gracefully. Not a bug in this repo.

**Fix is a local Chrome flag, not a code change**: enable
`chrome://flags/#enable-unsafe-webgpu` (disables Dawn's adapter blocklist)
and `chrome://flags/#enable-vulkan`, restart Chrome, then verify
`chrome://gpu` shows WebGPU hardware-accelerated. Not yet re-tested live
with the flags on -- that's the next step.

## Known gaps (tracked, not forgotten)

- **No Opus decode.** Symphonia 0.6.1 has no Opus support at all (checked
  its own feature list, not assumed) -- PLAN.md §3 lists Opus as a target
  format; that needs a second decode path (real question) or dropping Opus
  from the supported-format list (real answer), not decided yet.
- **Native output format support is F32/I16 only** (`player-core/src/playback.rs`).
  A device whose only matching config is e.g. U8/I24/I32 fails loudly
  rather than mangling audio -- found live on this dev machine (its ALSA
  device's mono config list put U8 before F32), fixed by having
  `pick_config` prefer F32 > I16 explicitly instead of taking the first
  match. Broader format support is Phase 1, not Phase 0.
- **No sample-rate conversion.** If a file's rate doesn't match a config
  the output device supports, playback uses the device's default config
  as-is -- audible pitch/speed error. Real resampling is Phase 1.
- **Fixed-size scope window**, not resizable, no live-resize handling.
- **No WebGL2 fallback on the web build.** `player-render` on wasm32 only
  targets wgpu's WebGPU backend; a browser/GPU without WebGPU gets a clear
  error (`webScope.ts` checks `navigator.gpu` up front) rather than a
  silent blank canvas, but there's still no fallback rendering path. Real
  WebGL2 support (`wgpu`'s `webgl` feature + backend selection) is Phase 1.
- **Web AudioContext sample rate must match the decoded track exactly**
  (`playerEngine.ts`'s `getAudioContext`) -- there's no resampling, same
  simplification as the native side's own missing sample-rate conversion
  (above). Found live (2026-08-15): the web build ran the AudioContext at
  its default rate regardless of the file's real rate, so playback was
  audibly pitched (high vocals) even though the native build, which uses
  the file's real rate via cpal, sounded correct. Fixed by constructing the
  `AudioContext` with `{ sampleRate: decoded.sampleRate }`. Loading a
  second file at a different rate closes and recreates the context, since
  Web Audio has no way to change an existing context's rate.
  **User-confirmed fixed (2026-08-15, live retest): "sounds good now."**
- **The scope's rAF loop now survives a bad frame** (`webScope.ts`). It
  used to have no try/catch around `draw_frame`; a single thrown frame
  (e.g. a transient `SurfaceUnavailable` from `Renderer::draw_frame`'s own
  documented recoverable cases) silently killed the entire render loop
  forever, since nothing scheduled the next `requestAnimationFrame` after
  an exception. This was a real bug worth having fixed (now logs and keeps
  going) -- but it was **not** the cause of the blank web scope: re-tested
  live (2026-08-15) with this fix in place and the scope is still blank.
  See "Active bug" above -- root cause now confirmed (Chrome/Dawn WebGPU
  adapter blocklist on this dev machine's NVIDIA driver, not an app bug).
