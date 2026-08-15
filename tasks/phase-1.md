# Phase 1 -- MVP

PLAN.md §7's Phase 1: "local file player, tagging/library, playlists, basic
EQ, spectrum analyzer, phase correlation meter, LUFS meter, 3D vectorscope
v1; ships as both a Tauri desktop build and a web build."

Phase 0 is closed (`tasks/phase-0.md`) -- both shells play local files and
draw a live 2D scope, user-confirmed. What Phase 0 deliberately left thin,
and what this phase has to replace:

- Whole-file decode into a `Vec<f32>`, held in memory in its entirety.
- Playback is a cursor over that Vec. **No seek, no volume, no queue, no
  gapless.**
- Tags/metadata are probed by Symphonia and then discarded.
- No analysis of any kind -- the scope reads raw samples directly, so
  there's nowhere for metering to hang off yet.

## Decisions for this phase (2026-08-15)

1. **Engine foundation first**, before metering or library work. Every
   Phase 1 feature lands on the audio engine; building the metering suite
   on Phase 0's spike would mean rewriting it once the engine changes.
2. **Web parity is lockstep, per milestone** -- each milestone lands on both
   shells before the next one starts. Phase 0's two worst bugs (pitched web
   audio, blank web scope) were both web-only and both found late; keeping
   the shells in step is worth the slower milestones.

## Milestones

Each is "done" only when it works on **both** shells and has been verified
live, per the habit the sibling repos and Phase 0 already follow -- compiling
clean is not evidence.

- **M1 -- Transport foundation.** Real resampling (replacing Phase 0's
  linear-interpolation stopgap), seek, volume, accurate position/duration
  reporting. Transport UI: seek bar, volume, time readout. **Done, verified
  live on both shells.**
- **M2 -- Streaming decode.** Replace whole-file-into-memory decode with a
  decoder thread feeding a ring buffer, so a long FLAC doesn't cost its
  full size in RAM. Native and web (worker) both.
- **M3 -- Queue + gapless.** Playlist model, next/prev, gapless hand-off
  between tracks, crossfade if it falls out cheaply.
- **M4 -- Analysis tap + spectrum analyzer.** The per-block tap every meter
  hangs off, plus the first consumer (FFT spectrum, log/linear).
- **M5 -- Loudness + phase metering.** LUFS (integrated/short/momentary, per
  ITU-R BS.1770-4), true-peak, phase correlation, goniometer.
- **M6 -- 3D vectorscope v1.** PLAN.md §4's hero feature: extrude the 2D XY
  scope along a time axis. `player-render` grows a real 3D pipeline.
- **M7 -- Library + tags + playlists.** Folder scan, ID3/Vorbis comment
  reading, album art, search, persisted playlists.
- **M8 -- Basic EQ.** Biquad chain in the DSP path.

Ordering rationale: M1-M3 are the engine, M4 is the seam everything visual
hangs off, M5-M6 are the differentiator (PLAN.md §4), M7-M8 are table
stakes that don't block anything else.

## M1 -- Transport foundation: done

It behaves like a player now rather than a "press play once" spike.

**Real resampling.** `resample.rs` is now `rubato` 5's FFT-based
synchronous resampler, replacing Phase 0's openly-labelled linear
interpolation. Synchronous is the right family here: the ratio is fixed and
rational (44100 -> 48000 and friends). Built against rubato 5.0.0's actual
source -- 5.x moved buffers onto the `audioadapter` traits and renamed the
resamplers (`Fft`/`FixedSync`, `Async`/`FixedAsync`), so the
`SincFixedIn`/`Vec<Vec<f32>>` shape in most online examples doesn't exist
any more. `process_all` handles chunking, the final partial chunk, trimming
the start-up delay, and truncating -- doing that by hand is the classic way
to leak silence onto the front of every track.

**A measurement worth keeping.** The first quality test compared the
resampled sine sample-by-sample against an analytic one and reported only
30dB, which looked like the resampler was bad. It wasn't: scanning integer
offsets showed the best fit sitting *between* two samples, i.e. a constant
~half-sample group delay, which caps a sample-wise SNR around 31dB no
matter how clean the waveform is. Measured phase-independently (tone energy
vs everything else, over an exact number of cycles so there's no spectral
leakage) it comes out at **~85dB**. The test asserts >70dB and explains
why it doesn't do the obvious sample-wise comparison.

**Transport.** `seek`, `set_volume`/`volume`, `position_seconds`,
`duration_seconds`, `is_finished` on both shells, exposed through one
`TransportState` shape shared by `nativePlayer.ts` and `playerEngine.ts`
so `App.tsx` never branches on which shell it's in. Native sends it as a
single `#[tauri::command]` struct rather than a command per field, so the
seek bar, clock and play button can't disagree mid-poll. Web seeks by
storing a frame index into the shared control block (the worklet already
reads its cursor from there every block -- no message round trip) and puts
volume on a GainNode, since Web Audio smooths it where a per-block
multiply in the worklet would zipper.

**UI.** Seek bar with drag-to-scrub (the slider follows the pointer while
dragging instead of being yanked back by the 10Hz poll), elapsed/total
clock in tabular figures so it doesn't shove the bar around at 9:59 ->
10:00, and a volume slider.

**Verified live, both shells:**
- Web, in-browser against the running dev server: position advanced
  1.451s -> 2.453s over one second of wall clock, `seek(20)` landed on
  exactly 20.000, playback continued from there, pause froze the position,
  volume round-tripped. The DOM showed `0:20 / 0:30`, slider at 20.71,
  no errors.
- Native engine, via `cargo run -p player-core --example transport_smoke`
  (new, self-contained -- synthesises its own tone, needs no file): all 8
  checks pass against the real default device, including the 44100 ->
  48000 resample with duration preserved at 30.000s, seek exactness,
  pause, volume clamping, and seeking past the end parking at the end.
- Native UI, **user-confirmed live on a real MP3**: "plays good, scope
  works, volume works, seek works, pause still works". That covers the one
  path the automated checks can't reach -- React driving the Tauri
  commands, where an argument-name mismatch would fail silently at the IPC
  boundary. Session log was clean: no surface, stream, or draw errors.
