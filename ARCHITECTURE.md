# ARCHITECTURE.md

One Rust core, two shells -- see PLAN.md §5 for the full rationale. This
file tracks what actually exists, not the aspirational end state.

## Crate map

```
crates/
  player-core/      Symphonia decode, PlaybackSource trait (LocalFile,
                     InMemorySource), native cpal output (playback.rs,
                     not built on wasm32). No UI/windowing code.

                     Decode streams (stream.rs's StreamingDecoder): the
                     format reader stays open for the life of the track and
                     hands out one block at a time. Native playback drives it
                     from a decoder thread into an rtrb lock-free SPSC ring
                     (~2s) that the cpal callback consumes, so the callback
                     takes no lock and memory is bounded by the ring rather
                     than by the track. resample.rs's StreamingResampler and
                     the channel adaptation sit between the two, so the ring
                     holds audio already in the device's shape; tap.rs keeps
                     a small ring of what was actually played.

                     That ring is the analysis tap (M4): the scope draws
                     snapshots of it (`window`), and meters read it through a
                     cursor that reports when it has been lapped -- because a
                     display only wants what is on screen now, while an
                     integrated loudness reading has to have seen every sample
                     it claims to measure. analysis.rs owns the meters and the
                     drain; spectrum.rs (windowed FFT, log bands, dBFS),
                     loudness.rs (BS.1770-4: K-weighting, momentary/short-term/
                     gated integrated, true peak by 4x oversampling) and
                     phase.rs (correlation, goniometer points) are the meters
                     themselves. All cross-target: the native analysis thread
                     and the web worker run the same code, so the two shells
                     cannot measure differently. None runs on the audio thread.

                     The loudness meters measure a *continuous listen*, so a
                     seek, a lap or a gapless hand-off resets them (M5). The
                     hand-off is the awkward one -- the audio is continuous by
                     design, so nothing in the tap marks it and both shells
                     watch their track counter instead. The drain carrying a
                     break is dropped rather than measured, since it holds
                     audio from both sides of it.

                     A seek is a real container seek, which means discarding
                     the audio already queued. That takes a three-counter
                     handshake between the caller, the decoder thread and the
                     callback -- see `Shared` in playback.rs; the web build
                     runs the identical protocol in app/src/audioSab.ts.

                     The engine knows two tracks: playing and next
                     (Playback::set_next). Track changes travel in a second
                     rtrb queue alongside the audio and are applied by the
                     callback when the device reaches them, so "now playing"
                     changes with the music rather than a ring early.

                     tags.rs (M7) reads what a file says about itself --
                     title/artist/album/year/cover and the stream's own
                     properties -- off symphonia's probe without decoding any
                     audio. Cross-target, so the desktop shell and the web
                     worker get identical answers. Revisions are applied
                     oldest-first so the newest wins: symphonia probes trailing
                     metadata *before* the container, so an MP3 yields
                     [ID3v1, ID3v2] and first-wins would prefer the truncated
                     ID3v1 title.
                     library.rs is the desktop library and is native-only by
                     necessity -- a browser has directory handles, not paths.
                     It walks folders, keeps a versioned JSON index plus a
                     content-hashed cover cache in a directory it is handed,
                     and is idempotent under rescan (rows keyed by path), which
                     is what lets saved playlists reference rows by id.
  player-render/    wgpu render engine. Takes any wgpu::SurfaceTarget
                     (native window handle or web canvas) -- knows nothing
                     about Tauri or the browser. Since M6 it is PLAN.md §4's
                     3D vectorscope: trace.rs turns a window of played audio
                     into points (x side, y mid -- the goniometer's axes --
                     and z their age), camera.rs is the perspective/look-at
                     math with wgpu's clip-space conventions pinned by tests,
                     and scene.rs owns the pipeline, depth buffer and 4x MSAA.
                     How much history it shows is a fixed *duration*
                     (window_samples), so the figure means the same thing at
                     any output rate. OffscreenRenderer renders that same
                     scene into a texture, which is how the pipeline is
                     checked (examples/scope_smoke.rs) rather than eyeballed.
  player-rt-wasm/   wasm-bindgen glue for player-core's decoder and
                     player-render's Renderer (wraps both -- neither of
                     those two crates knows wasm-bindgen exists). wasm-only;
                     excluded from native `--workspace` commands, see
                     WORKFLOW.md. No per-block wasm calls on the audio path
                     -- WasmDecoder is driven from a Worker, once per ring
                     refill; the AudioWorkletProcessor itself is plain TS.

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

                     Transport (play/pause/seek/volume/position) is exposed
                     as `#[tauri::command]`s returning one `TransportState`
                     struct rather than a command per field, so the seek
                     bar, clock and play button can't disagree mid-poll.

                     The library's commands live here too (M7), with the scan
                     itself on a blocking thread rather than the main one --
                     a folder of a few thousand tracks is tens of seconds of
                     IO. Cover art does *not* travel through a command: a
                     `cover://` URI scheme protocol serves the cached image
                     straight to the webview, because a 50KB JPEG through
                     `invoke` is a JSON array of 50,000 numbers, per row. That
                     response must carry `Cross-Origin-Resource-Policy:
                     cross-origin` -- the COEP header below is `require-corp`
                     for the SharedArrayBuffer's sake, and a subresource
                     without CORP is blocked under it.

app/                 React + TS + Vite. Same component tree renders in the
                     Tauri webview and (eventually) the plain web build --
                     matches ../latent-mixing/app/ exactly. isTauri() gates
                     which playback adapter a component uses:
                       - nativePlayer.ts: Tauri commands (desktop; decode +
                         output + the scope run entirely in Rust already)
                       - playerEngine.ts: AudioWorklet + wasm (web; worker.ts
                         keeps a ring topped up, the worklet itself is
                         plain TS -- see player-rt-wasm above)
                     Both adapters expose the *same* `TransportState` shape,
                     so App.tsx drives the transport without branching on
                     which shell it's in. playlist.ts is the only module that
                     picks between the two: it owns the track list and turns
                     "play track n" into the load/queue pair both shells
                     expose, so the one remaining difference between them --
                     a desktop track is a path, a web track is a File --
                     stops there. The engine still holds only current + next
                     (M3's decision); ordering lives here.
                     audioSab.ts holds the one definition
                     of the SharedArrayBuffer all three web parties use --
                     control block, playback ring, scope history, and the
                     spectrum/meter/goniometer regions the worker publishes into
                     -- including
                     the seek handshake that mirrors the native one, and the
                     boundary records that carry gapless track changes along
                     with the audio. One AudioContext rate serves the whole
                     session; the worker resamples every track to it. Web
                     volume rides a GainNode, since Web Audio smooths it and
                     the worklet would zipper.
                     playlist.ts, analysis.ts, scope.ts and library.ts are the
                     four adapter modules components actually call: playlist.ts
                     picks the shell for playback, analysis.ts for metering,
                     scope.ts for the scope's size and visibility (a separate
                     OS window natively, a canvas on the web -- one component,
                     ScopeControls, drives both), and library.ts for the
                     library. The web build's library lives in IndexedDB
                     (libraryStore.ts): rows, cover blobs, playlists, and the
                     scanned folder's `FileSystemDirectoryHandle` -- that last
                     one being the property that decided the choice, since it
                     is what lets a later session reopen the real files instead
                     of asking for the folder again. localStorage is
                     synchronous and ~5MB; OPFS would mean copying the user's
                     music into browser-private storage. Tag reading runs in
                     libraryWorker.ts, a *second* worker, because worker.ts
                     owns the decoder pump and a few thousand files of tag
                     parsing sharing it would be audible. Search is one
                     function for both shells, since by that point both hold
                     the same array of rows. Spectrum.tsx,
                     Loudness.tsx and Goniometer.tsx draw the dBFS, LUFS and
                     mid/side pairs their adapter hands them and compute
                     nothing -- including the goniometer's 45-degree rotation,
                     which is two lines of arithmetic and still lives in Rust
                     so the two shells cannot draw different figures.
                     gapless.html, spectrum.html, loudness.html, scope.html and
                     library.html
                     (with their *Smoke.ts) are dev-only harnesses -- served at
                     those paths, never bundled -- running the same checks as
                     the native gapless_smoke, spectrum_smoke, loudness_smoke,
                     scope_smoke and library_smoke examples.
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

- ~~**Whole-file decode.**~~ Replaced in Phase 1 M2 by streaming decode on
  both shells (`stream.rs`, and worker.ts on the web). Measured: the web
  build's decoded-audio buffer is 0.837MB for a 300s track and 0.837MB for a
  30s one, where before it was 105.8MB and 10.6MB; natively, `open()` on a
  300s track takes 0.9x the time of a 30s one rather than 10x. See
  `tasks/phase-1.md`'s M2 section.
- **Track duration can now be unknown.** It used to be measured off the
  decoded buffer and was therefore always exact; it now comes from the
  container (`Track::num_frames`, falling back to the timebase duration),
  which some files don't state -- a VBR MP3 with no Xing header is the usual
  case. Both shells report 0.0 for it, which the transport renders as a
  `0:00` total and a degenerate seek bar. Deliberate: the alternative is
  decoding an hour of audio to find out how long it is, which is the thing M2
  removed. A real fix is a duration *estimate* (bitrate x file size) or a
  background scan, and belongs with the library work in M7.
  **Still open after M7.** The library scan reads the length from the same
  container field the transport does, so a file that does not state one is
  unknown in the library too -- the library view renders that as an em dash
  rather than as `0:00`, which is at least honest about the difference. What M7
  did *not* add is the estimate or the decode-to-measure pass, either of which
  would need the scan to do real work per file rather than a probe.
- **Closing the scope window by hand panics the redraw thread** (observed live
  2026-08-16, in a ~90-minute desktop session; pre-existing M6 code, surfaced
  while M7 was being confirmed). The log:

  ```
  [wgpu_hal::vulkan::swapchain::native][ERROR]
      get_physical_device_surface_capabilities: ERROR_SURFACE_LOST_KHR
  [wgpu::backend::wgpu_core][ERROR] Handling wgpu errors as fatal by default
  thread '<unnamed>' panicked at wgpu-30.0.0 .../wgpu_core.rs:3981:
    wgpu error: Validation Error
    Caused by: In Surface::configure
      Surface does not support the adapter's queue family
  ```

  `draw_frame` handles `CurrentSurfaceTexture::Lost` deliberately and well --
  it returns `SurfaceUnavailable` for the caller to deal with, because
  recreating a surface needs the window handle the renderer doesn't keep. The
  trouble is that a *destroyed* window does not arrive as `Lost`: it arrives as
  `Outdated`, and M6's fix for `Outdated` (call `surface.configure()` and skip
  the frame, which was the right fix for a resize) does not return an error on
  a dead surface -- it panics inside wgpu, since wgpu treats validation errors
  as fatal by default. The same panic is reachable from the `needs_reconfigure`
  branch at the top of `draw_frame`.

  Consequences are contained but real: the redraw thread dies and the scope
  goes permanently dark, while the rest of the app carries on -- the same
  session decoded another MP3 31 seconds after the panic. `set_scope_visible`
  only ever *hides* the window precisely because the renderer holds a surface
  built from its handle, but nothing stops the user closing it with its own
  close button. A fix needs both halves: stop the redraw loop (or rebuild the
  surface) when the window is gone, and prevent or intercept the manual close
  rather than assuming the UI is the only thing that can hide the window.
- **Play at the end of a track pauses instead of restarting** (both shells,
  found live during M7 but *not* caused by it -- the transport is untouched by
  that milestone). When a track finishes, `TransportState::playing` stays true,
  so the button still reads "Pause"; clicking it therefore calls
  `set_playing(false)` and the position sits at the end. Measured on the web
  build: position parked at 3.000 on a 3s track, the label flipping
  Pause -> Play but never moving. M2's own check exercised the engine's `play()`
  directly and so never saw this -- it is a UI-level disagreement between
  `playing` and `finished`, not an engine one. A fix is small (treat `finished`
  as not-playing when labelling the button, and make play-from-finished seek to
  zero first) but it belongs with transport work rather than being smuggled into
  a library milestone.
- **The web library reads each file whole to scan it.** `WasmTags::read` takes
  the file's bytes, because a browser hands over a `File` rather than something
  seekable a decoder can read a few hundred bytes of. Reading a prefix instead
  is not correct in general: MP4 can carry its `moov` box at the end and ID3v1
  lives in the last 128 bytes. Files are read one at a time and released, so
  memory is bounded by the largest single file, but a large library is read off
  disk in full to be scanned -- the native side, which streams through real file
  handles, has no such cost. A real fix means teaching the wasm source to read
  through `Blob.slice` on demand rather than owning a `Vec<u8>`.
- **Cover blobs are never garbage collected on the web.** They are keyed by
  content hash and shared between rows, so removing one folder cannot know
  whether another still references an image; `deleteFolder` deliberately leaves
  them. Costs kilobytes per orphaned image and needs a sweep (or reference
  counts) if a library is added and removed repeatedly.
- **The decoder polls rather than parks.** Both the native decoder thread and
  the web Worker sleep on a short timer when the ring is full (2ms and 20ms)
  instead of waiting on a condvar or `Atomics.waitAsync`. It costs a wakeup
  or two per ring's worth of audio and keeps the consumer free of any
  signalling work, which is the trade that matters on the audio callback --
  but it is a poll, and worth revisiting if it ever shows up in a profile.
- **AAC/M4A encoder delay and padding are not trimmed.** For MP3 and Ogg this
  is handled for us: the demuxer populates `Packet::trim_start`/`trim_end` and
  the decoder applies them itself (`symphonia-bundle-mp3/src/decoder.rs:131`
  calls `buf.trim(...)`), so a decoded MP3 is already free of its ~1105 priming
  frames and its tail padding -- measured on a real file, where packet 0
  declares `trim_start=1105` and decodes to 47 frames. Symphonia's `isomp4`
  demuxer sets neither field and the AAC decoder applies neither, so an M4A
  keeps its priming frames. Harmless for single-track playback (a few tens of
  milliseconds of near-silence at each end) but it means gapless is only really
  gapless for MP3, Ogg, FLAC and WAV. Fixing it means reading the mp4
  `edts`/`sgpd` boxes ourselves, or waiting for symphonia to.
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
- ~~**Resampling is linear interpolation.**~~ Replaced in Phase 1 M1 with
  `rubato` 5's FFT-based synchronous resampler -- the right family for a
  fixed rational ratio like 44100 -> 48000, and it applies a real
  anti-aliasing window. Measured at ~85dB tone-vs-everything-else on a
  440Hz sweep through 44.1 -> 48kHz (see `resample.rs`'s own test, which
  asserts >70dB). Cross-target as of M3: the web path needs it too, because
  a live `AudioContext`'s rate cannot be changed, so the web build picks one
  rate per session and converts every track to it.
  One property worth knowing: rubato leaves a constant sub-sample group
  delay (~half a sample here), so comparing its output sample-by-sample
  against an analytic reference measures *that*, not quality -- which is
  why the test measures spectral purity instead.
  M2 added `StreamingResampler` alongside it for the streaming path, driven
  a block at a time; the two are asserted bit-exact against each other, so
  the measurement above still describes what plays.
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
- **The scope window is built `.resizable(false)`, which Windows does not
  fully honour** -- it can still be resized (by hand, and programmatically
  via `MoveWindow`). Resizing is now handled properly rather than assumed
  away: the redraw thread polls `inner_size()` and calls `Renderer::resize`
  on a change, and `draw_frame` reconfigures the surface on `Outdated`
  instead of retrying forever. Verified live on Windows 2026-08-15 by
  resizing the window five times (496x519 -> 300 -> 700 -> 480 -> 900 ->
  520) and capturing it: zero `Outdated` warnings, live trace still drawing
  at the new size. What's still missing is any *layout* response -- the
  scope is drawn in normalised device coordinates, so it stretches with the
  window rather than holding a square aspect.
- **The scope pairs the first two channels and calls them X and Y**
  (`player-render`'s `draw_frame` takes `chunks_exact(2)` of whatever it is
  given). Since M3 the samples it gets are in the *output* layout -- device
  rate, device channel count -- which is a real improvement over the source
  layout, because the output shape is fixed for the session while the source
  shape changes with every track in a queue. Two loose ends remain: a mono
  output device makes consecutive samples of one channel into XY pairs,
  which draws a diagonal smear rather than an error, and a device with more
  than two channels contributes only channels 0 and 1. Neither is worth
  fixing before the vectorscope work replaces this renderer.
- **No WebGL2 fallback on the web build.** `player-render` on wasm32 only
  targets wgpu's WebGPU backend; a browser/GPU without WebGPU gets a clear
  error (`webScope.ts` checks `navigator.gpu` up front) rather than a
  silent blank canvas, but there's still no fallback rendering path. Real
  WebGL2 support (`wgpu`'s `webgl` feature + backend selection) is Phase 1.
- ~~**Web AudioContext sample rate must match the decoded track exactly.**~~
  Closed in M3. The original bug (2026-08-15) was that the web build ran the
  context at its default rate regardless of the file's, so playback was
  audibly pitched -- high vocals -- while the native build, which resamples
  to the device via cpal, sounded correct. **User-confirmed fixed the same
  day: "sounds good now."** M1 fixed it by building the context at the
  *file's* rate, which works for one track and cannot survive a queue: Web
  Audio has no way to change a live context's rate, so per-track rates mean
  tearing the context down between tracks, i.e. gapless except when the rate
  changes. Since M3 the web path does what the native one does -- one rate
  for the session, every track resampled to it in the worker (`WasmDecoder`
  now owns a `StreamingResampler`). The pitch itself is checked rather than
  assumed: /gapless.html measures the played tone's frequency, and a
  resampling error would read 239Hz where it wants 220Hz.
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
