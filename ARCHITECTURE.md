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

## Resolved: the blank web scope was environmental (closed 2026-08-15)

**Superseded by the Windows bring-up below -- kept for the record because
the same symptom will reappear on any machine whose browser can't get a
WebGPU adapter.** The scope now renders on the Windows dev machine, whose
Chromium hands out a real adapter (NVIDIA/Blackwell) where the Linux box's
did not. Nothing in this repo's code changed to fix it; see
"Windows dev environment" below for what was verified.

The original report, for reference -- root cause confirmed via live
`claude-in-chrome` devtools console on the **Linux** machine. Native scope
worked (user-confirmed live). Web audio sounded correct (user-confirmed
live, see "Known gaps" below for the fix). The web *scope* threw on load:

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
`chrome://gpu` shows WebGPU hardware-accelerated. Never re-tested with the
flags on -- development moved to Windows before that happened, and the
scope renders there without any flag change, which is the same conclusion
by a different route. If Linux becomes the test machine again, this is
still the first thing to check.

## Windows dev environment (primary as of 2026-08-15)

Development moved to Windows (`Z:\_dev\latent\latentPlayer`) because the
Linux box's blockers were environmental and not worth working around. Whole
toolchain was already present (rustc 1.97.1 MSVC, cargo-tauri 2.11.4, node
22, wasm-pack 0.15, wasm32-unknown-unknown target); only `npm install` was
needed. Every command in WORKFLOW.md passes: native check/test/clippy/fmt,
wasm check/clippy, `npx tsc -b`, `npm run build`, `npm run wasm`.
`cargo tauri dev` needs **no** env-var workaround here -- the Wayland
gotcha is Linux-only.

What was verified live on Windows (via the in-app browser against
`npm run dev`), and what wasn't:

- **WebGPU adapter is real**: `navigator.gpu.requestAdapter()` resolves to
  an NVIDIA/Blackwell adapter, and `crossOriginIsolated` is true with
  `SharedArrayBuffer` available -- i.e. the COOP/COEP setup works.
- **The wgpu render path genuinely draws**: `WasmRenderer.create` +
  `draw_frame` on a test canvas produced a real image -- background
  `rgb(5,5,8)` plus ~580 trace pixels of `rgb(38,242,140)` -- read back
  pixel-by-pixel, not eyeballed. This is the thing that was blank on Linux.
- **The whole web chain runs end to end**: a synthetic stereo WAV pushed
  through the app's own file input decoded to the right duration, started
  playing, and `scopeWindow(2048)` returned live samples at the exact
  amplitude generated, with the window's contents *changing* between polls
  -- so decode -> SAB -> worklet -> cursor -> scope feed all work.
- **Not verified programmatically: how it looks on screen.** The automation
  browser pane runs hidden, and a hidden tab gets zero
  `requestAnimationFrame` callbacks, so `webScope.ts`'s rAF loop never ticks
  there -- the app's own canvas reads back empty for that reason alone.
  Every piece that loop calls was driven manually instead and works.
  **User-confirmed live the same day: "Web passes. Sounds perfect, scope
  shows up."** Web is closed.

## Fixed: native output was broken on most Windows devices (2026-08-15)

The first Windows desktop test failed in two ways that turned out to share
one cause. Reported live: "no usable audio output device" on most default
devices, and *very* sped-up playback on the one device that did work.

`Playback::open` used to search `supported_output_configs()` for an entry
matching the file's own rate and channel count. On WASAPI that is exactly
backwards, for the reason now written up under "Known gaps" below: cpal
advertises configs it will not build, and only the device's mix format is
actually openable. Probing all of this machine's output devices
(`cargo run -p player-core --example device_probe`) showed the full shape:

- 6 devices: the old search *matched* 2ch/44.1kHz, and the build was then
  refused -- "Stream configuration is not supported in shared mode",
  flattened by this app into the misleading `NoOutputDevice` ("no usable
  audio output device"). The device was fine; only the request was wrong.
- 3 devices (SteelSeries Sonar virtual, 8ch/96kHz): the old channel filter
  found nothing, fell through to the mix format, and played -- at
  96000/44100 = **2.177x speed**. The one that "worked" only worked because
  the picker had failed on it.
- 1 device happened to run at 44.1kHz natively and was correct by luck.

Three changes, all in `player-core`:

1. `device_config` takes the device's mix format and does not negotiate.
2. `resample.rs` converts the decoded audio to that rate (see "Known gaps"
   for what this resampler is and isn't).
3. `PlayerError::OutputStream` carries the backend's real message instead
   of flattening every failure into `NoOutputDevice`. The old text actively
   misled the debugging of this very bug.

After the fix, all 10 output devices on the machine open successfully and
at the correct rate, re-verified with the same probe.

## Known gaps (tracked, not forgotten)

- **No Opus decode.** Symphonia 0.6.1 has no Opus support at all (checked
  its own feature list, not assumed) -- PLAN.md §3 lists Opus as a target
  format; that needs a second decode path (real question) or dropping Opus
  from the supported-format list (real answer), not decided yet.
- **Native output format support is F32/I16 only** (`player-core/src/playback.rs`).
  A device whose mix format is e.g. U8/I24/I32 fails loudly rather than
  mangling audio. Note this is now genuinely fatal for such a device rather
  than something to search around: per the entry below, no other format
  would open it either. Broader format support is Phase 1, not Phase 0.
- **Native output negotiates nothing with the device** -- it takes the
  device's mix format and resamples to it (`playback::device_config`). This
  is forced by cpal 0.18.1's WASAPI backend, which advertises configs it
  will then refuse to build: `supported_output_configs()` skips its own
  `is_format_supported` check for output devices (on the theory that
  `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` makes Initialize accept anything),
  while `build_output_stream_raw()` runs that check and rejects anything
  the engine doesn't take natively. Verified by reading cpal's source and
  by probing all 10 output devices on the Windows dev machine
  (`cargo run -p player-core --example device_probe`). Consequences worth
  knowing: no bit-perfect/exclusive-mode path, and no way to ask the OS
  resampler to do the work instead. Both are Phase 1 questions, and both
  get easier if cpal fixes the advertise/accept mismatch upstream.
- **Resampling is linear interpolation** (`player-core/src/resample.rs`),
  which fixes pitch/duration but is not transparent -- no anti-alias filter
  when downsampling. `rubato` is the Phase 1 answer, and matters more once
  the metering suite lands, since LUFS/true-peak off a linear-interpolated
  signal would be subtly wrong. Note the web path doesn't use this at all:
  it sets the `AudioContext` to the file's own rate instead.
- **No response to the OS default output device changing** (Windows).
  Observed live during the first Windows `cargo tauri dev` run:
  `output stream error: Default audio device changed`, twice. Checked cpal
  0.18.1's own WASAPI source rather than guessing at the message: it emits
  this through the error callback and then returns
  `ControlFlow::Continue(false)`, i.e. the stream is *not* torn down -- it's
  a notification that the default device moved, offered so the app can
  decide to reopen against the new one. `build_stream`'s error callback
  (`player-core/src/playback.rs`) only `eprintln!`s it, so playback keeps
  feeding the device the stream was originally opened against. Reopening on
  device change is Phase 1, not Phase 0.
- ~~**No sample-rate conversion.**~~ Fixed 2026-08-15 after it broke the
  first Windows desktop test -- see "Fixed: native output was broken on most
  Windows devices" above. The remaining limitation is the *quality* of that
  conversion, tracked in its own entry above.
- **Fixed-size scope window**, not resizable, no live-resize handling.
- **No WebGL2 fallback on the web build.** `player-render` on wasm32 only
  targets wgpu's WebGPU backend; a browser/GPU without WebGPU gets a clear
  error (`webScope.ts` checks `navigator.gpu` up front) rather than a
  silent blank canvas, but there's still no fallback rendering path. Real
  WebGL2 support (`wgpu`'s `webgl` feature + backend selection) is Phase 1.
- **Web AudioContext sample rate must match the decoded track exactly**
  (`playerEngine.ts`'s `getAudioContext`) -- there's no resampling on the
  web path at all; it sidesteps the problem by running the context at the
  file's rate, where the native path instead resamples to the device's.
  Found live (2026-08-15): the web build ran the AudioContext at
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
