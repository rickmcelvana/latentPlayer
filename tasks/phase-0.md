# Phase 0 -- spikes

Working log for PLAN.md's Phase 0 (see PLAN.md §7 and the approved plan in
this session). Full plan/rationale lives in the session's plan file; this
tracks status and what was actually verified.

## M0 -- Workspace & doc scaffold: done
Workspace `Cargo.toml`, `crates/player-core`/`player-render`/`player-rt-wasm`
skeletons, `src-tauri` via `cargo tauri init`, `app/` via Vite react-ts +
zustand/@tauri-apps deps, COOP/COEP headers in `vite.config.ts` +
`tauri.conf.json`. `ARCHITECTURE.md`/`CONVENTIONS.md`/`WORKFLOW.md` written.
wasm32-unknown-unknown target already present via Fedora's
`rust-std-static-wasm32-unknown-unknown` package -- no rustup needed.

## M1 -- player-core: decode + native ring buffer: done, verified live
Symphonia-based decode (`decode_all`), `PlaybackSource`/`LocalFile`/
`InMemorySource`, cpal output (`Playback`). Built against symphonia-core
0.6.1's real API (significantly different from the well-known 0.5.x shape
-- verified against fetched source, not memory).

**Verified live** (`crates/player-core/examples/decode_smoke.rs` and
`playback_smoke.rs`, run against `/usr/share/sounds/speech-dispatcher/pipe.wav`):
decode produced correct sample count/duration/non-silence; playback opened
a real ALSA/PipeWire output device and `scope_window()` showed the play
cursor genuinely advancing over time.

**Real bug found and fixed via this live test**: `pick_config` took the
first `supported_output_configs()` entry matching channels/rate regardless
of sample format; this machine's device lists U8 before F32/I16 for mono,
so `Playback::open` correctly refused (`UnsupportedOutputFormat("U8")`)
rather than mangling audio -- but a different device could have hit the
same trap on a format `build_stream` *does* handle. Fixed by having
`pick_config` explicitly prefer F32 > I16.

## M2 -- player-render: wgpu 2D scope: done
Pure-Rust `Renderer` (no windowing deps), 2D XY oscilloscope via a
line-strip pipeline. Built against wgpu 30's real API (also substantially
different from older wgpu docs/examples floating around online --
`push_constant_ranges` -> `immediate_size`, `get_current_texture()` returns
an enum not a `Result`, `Queue::present()` not `SurfaceTexture::present()`,
etc. -- each verified against fetched wgpu 30.0.0/wgpu-types 30.0.0 source).

## M3 -- Native integration (Tauri): done, verified live
Two-window app: webview (React UI) + plain `tauri::window::WindowBuilder`
window (no webview) hosting a `player-render::Renderer` on a `wgpu::Surface`
built from that window's raw handle. Confirmed `tauri::window::Window<R>`
implements the exact `HasWindowHandle`/`HasDisplayHandle` traits
`wgpu::SurfaceTarget` needs, by reading tauri 2.11.5's own source (gated
behind the `unstable` Cargo feature, also confirmed from source).

**Verified live**: `cargo tauri dev` (with the `GDK_BACKEND=x11
WEBKIT_DISABLE_COMPOSITING_MODE=1` workaround -- see WORKFLOW.md) opens
both windows and stays up with zero crashes over a sustained run; a
screenshot confirms the scope window's wgpu surface is genuinely rendering
(cleared to the configured background color each frame). Without the env
vars, even a zero-wgpu single-webview-window Tauri app crashes instantly on
this machine's native Wayland session (confirmed by stripping the scope
window out entirely and reproducing the same crash) -- a WebKitGTK/Wayland
issue, not a bug in this app's code.

**Not yet verified**: an actual interactive "click Open File, hear sound,
watch the trace move" pass -- this needs a human at the keyboard (no input-
automation tool was available in this session) or a future session with one.
That's the one open item before M3 can be called fully closed.

## M4 -- Web/wasm parity: done, partially verified live

`player-rt-wasm` (wasm-bindgen glue for both `player-core`'s decode and
`player-render`'s `Renderer` -- neither of those two crates itself knows
wasm-bindgen exists, per CONVENTIONS.md), `app/src/worker.ts` (decode off
the main thread, allocates the shared SAB), `app/src/worklet/player-worklet.ts`
(plain TS AudioWorkletProcessor -- no per-block wasm calls needed, unlike
the mixer's worklet, since there's no real-time DSP happening here),
`app/src/playerEngine.ts` + `app/src/webScope.ts` (adapter modules, web
build's equivalent of `nativePlayer.ts`), `App.tsx` wired to branch on
`isTauri()`.

**Real risk resolved**: Symphonia (`player-core`) and wgpu's `Canvas`
surface target (`player-render`) both compile to `wasm32-unknown-unknown`
cleanly -- confirmed by actually building for that target, not assumed.
This was PLAN.md's originally-flagged "single biggest open technical risk"
(§5) and it's resolved.

**Real bug found and fixed via code review before it shipped**: `pollster::block_on`
has no wasm32 support (checked its source) and would have hung the page.
Fixed by making `player_render::Renderer::new` genuinely `async` (no
internal blocking) and moving `pollster::block_on` to the one native call
site (`src-tauri`) that actually has a real thread to block.

**Real workspace-hygiene issue found and fixed**: `player-rt-wasm` uses
`wgpu::SurfaceTarget::Canvas`, which only exists under wgpu's `cfg(web)` --
so it fails to compile for the native host target by design. Plain `cargo
check --workspace` doesn't know that and tries anyway. Fixed by excluding
it from workspace-wide native commands (`--exclude player-rt-wasm`,
documented in WORKFLOW.md) rather than fighting the cfg.

**Verified**: full `npm run build` succeeds end to end, including the
~1.5MB wasm binary; `npx tsc -b` clean; the dev server serves
`Cross-Origin-Opener-Policy`/`Cross-Origin-Embedder-Policy` correctly on
the main page, the worklet bundle, and the wasm binary (checked via
`curl -I` against the running dev server) -- the literal prerequisite for
`crossOriginIsolated`/`SharedArrayBuffer` to work at all.

**User live-tested both builds (2026-08-15), side by side against the same
MP3s on a real web player**: native "plays correctly and the scope
displays." Web had two real bugs, both root-caused from the report alone
(no browser access needed) and fixed:

1. **Web playback was audibly pitched (high vocals)**, native sounded
   correct. Root cause: `playerEngine.ts` created its `AudioContext` with
   no explicit `sampleRate`, so it ran at the browser's default output rate
   (commonly 48kHz) regardless of the decoded track's real rate (commonly
   44.1kHz for MP3) -- the worklet writes raw samples straight through with
   no resampling, so a rate mismatch is a direct pitch/speed error. Fixed:
   `AudioContext` is now constructed with `{ sampleRate: decoded.sampleRate }`;
   loading a second file at a different rate closes and recreates the
   context (Web Audio can't change an existing context's rate).
2. **Web scope stayed blank** even while audio was audibly playing (which
   itself proves the worklet/cursor/SAB path was fine). Root cause:
   `webScope.ts`'s rAF loop had no try/catch around `draw_frame` -- a
   single thrown frame (e.g. one of `Renderer::draw_frame`'s own documented
   recoverable `SurfaceUnavailable` cases) silently killed the entire loop
   forever, since nothing ever scheduled the next frame after an exception.
   Fixed: errors are now logged and the loop continues. Also added an
   explicit `navigator.gpu` check with a clear message (no WebGL2 fallback
   exists yet -- see ARCHITECTURE.md) so a real WebGPU-unavailable case
   fails loudly instead of just sitting blank.

Both fixes shipped; `tsc -b`/`npm run build` clean.

**Re-tested (2026-08-15): pitch fix confirmed -- "Web player sounds good
now."** The blank-scope fix did **not** resolve it: **scope is still blank
on web**, audio still fine. So the rAF-loop try/catch (real bug, worth
having fixed regardless) was not the actual cause, or wasn't the only one --
diagnosis from code alone has run out of road here.

**Root-caused (2026-08-15, later same day): `claude-in-chrome` connected
successfully this time** (previous session's three `/chrome` attempts had
no visible effect -- whatever was blocking it is resolved now, per the
user). Opened the web build, read the devtools console directly:

```
TypeError: Cannot read properties of null (reading 'requestDevice')
    at __wbg_requestDevice_ab46d0519ea1cc34 (player_rt_wasm.js)
```

Confirmed via `javascript_tool` in the live tab that
`navigator.gpu.requestAdapter()` resolves to `null` on this dev machine,
while WebGL2 in the same tab gets real hardware acceleration (`ANGLE ...
NVIDIA GeForce RTX 5060 Ti`) and the host's Vulkan stack (`vulkaninfo`,
driver 610.57.04) is healthy -- so this isn't a missing-GPU problem, it's
specifically Chrome's WebGPU backend (Dawn) refusing this NVIDIA driver.
Chrome's own WebGPU troubleshooting docs confirm Dawn has its own adapter
blocklist (separate from `--ignore-gpu-blocklist`) that rejects NVIDIA
driver 570+ by default on Linux -- 610.57.04 is well past that. **Not an
app bug** -- see `ARCHITECTURE.md`'s "Active bug" section for full detail
and the fix (`chrome://flags/#enable-unsafe-webgpu` +
`chrome://flags/#enable-vulkan`, restart Chrome, verify via `chrome://gpu`).

**Not yet done: re-test live with those flags enabled** to confirm the
scope actually renders once Chrome gets a real adapter. That's the
remaining step before Phase 0's web scope can be marked closed.

## M5 -- Sanity pass: done
`cargo check/test/clippy --workspace --exclude player-rt-wasm` and `cargo
fmt --check` all clean. `player-rt-wasm` separately clean for
`wasm32-unknown-unknown` (check + clippy). `npx tsc -b` and `npm run build`
(in `app/`) both clean, worklet bundle included.

## M6 -- Windows bring-up (2026-08-15): done

Development moved from the Linux box to Windows
(`Z:\_dev\latent\latentPlayer`), on the call that the Linux blockers were
environmental and not worth the detour. Linux stays a later test target.

The toolchain was already installed (rustc 1.97.1 MSVC, cargo-tauri 2.11.4,
node 22.22, npm 10.9, wasm-pack 0.15, `wasm32-unknown-unknown` +
`x86_64-pc-windows-msvc`); `npm install` in `app/` was the only setup step.

Everything in WORKFLOW.md passes here: `cargo check/test/clippy/fmt`
(native), `cargo check/clippy -p player-rt-wasm --target
wasm32-unknown-unknown`, `npm run wasm`, `npx tsc -b`, `npm run build`,
`npm run dev`, `cargo tauri dev` -- the last with **no** env-var workaround,
since the Wayland gotcha is Linux-only.

**The blank web scope does not reproduce on Windows.** Verified against the
running dev server, by reading pixels rather than trusting appearances:

- `navigator.gpu.requestAdapter()` returns a real NVIDIA/Blackwell adapter
  (the exact call that resolved to `null` on Linux), and the page is
  `crossOriginIsolated` with `SharedArrayBuffer` available.
- `WasmRenderer.create` + `draw_frame` on a test canvas drew an actual
  image: `rgb(5,5,8)` background plus ~580 `rgb(38,242,140)` trace pixels,
  counted from `getImageData`.
- A synthetic stereo WAV fed through the app's own file input decoded to
  the correct duration, played, and `scopeWindow(2048)` returned live
  samples at the generated amplitude whose contents changed between polls
  -- the full decode -> SAB -> worklet -> cursor -> scope feed, working.

**Still not verified: the on-screen result.** The automation browser pane
runs hidden and a hidden tab fires no `requestAnimationFrame` callbacks, so
`webScope.ts`'s loop never ticks there; every call that loop makes was
driven by hand instead. Same for the native app -- it launches, stays up,
and the webview + scope windows exist, but "audio audible, trace moving" is
a human-eyeball check.

**New gap found live**: the first Windows `cargo tauri dev` run logged
`output stream error: Default audio device changed` twice. Read cpal
0.18.1's WASAPI source to be sure of the meaning -- it's a notification,
not a stream teardown, and the app ignores it. Tracked in ARCHITECTURE.md's
"Known gaps"; Phase 1 work.

**User live-tested both builds on Windows (2026-08-15).** Web: "Web passes.
Sounds perfect, scope shows up" -- web is closed, both audio and scope, on
real music files across several different output devices. Native: two real
bugs, fixed in M7 below.

## M7 -- Native output was broken on most Windows devices: fixed

Reported from the live test: "no usable audio output device" on most
default devices, and *very* sped-up audio on the one device that did work.
Same root cause, and it is not the obvious one -- full write-up in
ARCHITECTURE.md's "Fixed: native output was broken on most Windows
devices". Short version: `Playback::open` searched
`supported_output_configs()` for the file's own rate, and on WASAPI cpal
0.18.1 advertises configs it will then refuse to build, so only the
device's mix format is ever openable.

`crates/player-core/examples/device_probe.rs` was written to get evidence
instead of theories -- it enumerates every output device and shows what the
old and new config paths each do, including the real cpal error text. On
this machine the old path failed to build on 6 of 10 devices and played at
2.177x on 3 more (96kHz mix rate vs 44.1kHz files); exactly one device was
correct, by luck. After the fix all 10 open at the correct rate. Keep the
probe -- it turns "audio doesn't work on my machine" into one command.

Fixes, all in `player-core`: `device_config` takes the mix format and stops
negotiating; `resample.rs` converts to it (linear interpolation, honestly
documented as a stopgap, with unit tests including one asserting a 440Hz
second stays a 440Hz second); `PlayerError::OutputStream` carries the
backend's real message instead of flattening everything into the actively
misleading `NoOutputDevice`.

Re-ran clean: `cargo test/clippy/fmt` (native + wasm) all pass. The rebuilt
app then ran ~3.5 minutes with an MP3 loaded and logged **no** stream errors
at all, where the previous run had failed. Whether it now *sounds* right is
still a human check.

**Observed, believed benign**: 308 `scope draw_frame error: ... "Outdated"`
warnings, all inside the final 5 seconds before the app exited and none in
the 3.5 minutes before that -- i.e. the scope's redraw thread drawing into a
window being torn down, not a render failure. Worth confirming if it ever
shows up mid-run.

## What's left before Phase 0 is fully closed
- **Native: closed.** User-confirmed (2026-08-15) live: plays correctly,
  scope displays.
- **Web audio: closed.** User-confirmed (2026-08-15): sample-rate fix
  resolved the pitch bug, sounds correct now.
- **Web: closed.** User-confirmed on Windows (2026-08-15): "Web passes.
  Sounds perfect, scope shows up", across several output devices.
- **Native: config/resampling bugs fixed (M7), sound not yet re-confirmed.**
  All 10 output devices now open at the correct rate, verified with
  `device_probe`; a human still needs to confirm it sounds right and the
  scope moves.
- **Web scope: no longer blocked.** It was environmental, not an app bug --
  `navigator.gpu.requestAdapter()` returned `null` on the Linux box (Dawn
  blocklists NVIDIA driver 570+ there by default; that machine ran
  610.57.04). On Windows the same code gets a real adapter and the renderer
  demonstrably draws (see M6). What's left is an eyeball check on a visible
  window, not a fix.

Everything else Phase 0 asked for -- the wgpu-rendering-shared-natively-
and-in-browser question, the AudioWorklet/SharedArrayBuffer port, and a
minimal local-playback walking skeleton on both targets -- is built and
compiles/builds clean; native is fully user-confirmed working, web audio
is user-confirmed working, web video (the scope) is the one open bug.

## Resuming this session after a restart
**The dev machine is Windows now** (`Z:\_dev\latent\latentPlayer`); the
Linux env-var and Chrome-flag workarounds in WORKFLOW.md/ARCHITECTURE.md
don't apply here. M0-M6 above are all done -- don't redo or re-explain
them. Every build/check command passes on this machine.

Web is user-confirmed closed (audio + scope). The only open item is a
listen-test of the **native** build after M7's output fixes: `cargo tauri
dev` (no env vars needed -- but stop any standalone dev server first, see
WORKFLOW.md's port note), open a real music file, confirm it plays at the
right speed and the scope window's trace moves. If a device misbehaves,
`cargo run -p player-core --example device_probe` prints exactly what that
device offers and what the app will do with it.
