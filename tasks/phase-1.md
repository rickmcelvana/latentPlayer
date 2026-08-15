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
  full size in RAM. Native and web (worker) both. **Done, verified live on
  both shells.**
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

## M2 -- Streaming decode: done

Nothing decodes a whole track into memory any more. Both shells now hold a
fixed ~2s ring and refill it as it drains.

**The shared half.** `player-core/src/stream.rs` is a new `StreamingDecoder`:
it keeps the Symphonia format reader and codec open for the life of the track
and hands out one block at a time (`next_block`), with `seek` asking the
container to move rather than moving a cursor. Deliberately free of threads,
devices and ring buffers, because the native decoder thread and the web Worker
need the same two operations and neither should inherit the other's
concurrency model. `decode_all` survives as a thin wrapper over it -- the smoke
examples and the resampler tests want a whole clip -- which also gives a test
asserting the streamed blocks are bit-exact against the old whole-file decode.

**Duration stopped being free.** It used to be the length of a buffer; it now
comes from the container (`Track::num_frames`, falling back to the timebase
duration) and can be absent, e.g. a VBR MP3 with no Xing header. That
`Option<f64>` travels to the UI as 0.0-means-unknown. Tracked in
ARCHITECTURE.md's "Known gaps" -- it is a real regression in what the player
knows, and the price of not decoding an hour of audio to find out.

**Seek is now a real seek**, with the two failure modes that come with one:
the codec carries state across packets (MP3's bit reservoir, FLAC's inter-frame
prediction), so the decoder is `reset()`; and a reader may only land on a
container boundary at or before the requested time, so the difference is
decoded and discarded. `seek_lands_on_the_requested_frame` proves the second
by reading a frame counter encoded in the test WAV's right channel.

**Resampling had to become incremental** (`StreamingResampler`). `process_all`
is whole-buffer by construction and was doing three things that now have to be
done by hand: trimming `output_delay()` so a track doesn't open on silence and
run permanently late, feeding the final partial chunk via
`Indexing::partial_len`, and pumping zeros at the end so the audio still inside
the filter comes out. The streamed and whole-buffer paths are asserted
bit-exact against each other, so M1's measured ~85dB quality carries over
rather than needing to be re-argued. One deliberate difference: the output
length is computed from the integer rates, because `ceil(44100 * (48000 /
44100))` in f64 is 48001 -- a stray frame of filter tail on every track, landing
exactly in M3's gapless seam.

**Native output** (`playback.rs`) is now a decoder thread feeding an `rtrb`
lock-free SPSC ring, with the cpal callback consuming it. The callback no
longer takes a `Mutex` -- Phase 0's design let a decode stall or an unlucky
lock holder reach the speakers.

**The seek handshake** is the one genuinely subtle piece, and it is identical
on both shells (`Shared` in playback.rs, `CONTROL` in app/src/audioSab.ts). A
seek must discard up to two seconds of already-queued audio without discarding
any decoded *after* it. Only the consumer can drop items from an SPSC ring and
only the producer knows which side of the seek a sample came from, so it takes
three counters: the caller bumps `seek_seq`; the producer seeks and sets
`decoder_ready_seq` **before** writing anything new; the consumer, seeing that,
empties the ring and sets `drained_seq`, which the producer waits for. Without
the middle step there is a real race -- a push that began before the seek lands
*behind* the drain and plays the old position anyway.

**The scope needed its own memory.** `scope_window` used to be a slice of the
whole-file buffer ending at the cursor; with streaming, played audio is gone.
Both shells now keep a small ring of what was actually sent to the output,
written by the consumer (`scope.rs` natively, in the SAB on the web). Arguably
more correct than before: the scope shows what the device played, not what the
decoder produced.

**Underrun stopped being indistinguishable from the end.** An empty ring alone
now means the producer fell behind -- silence, position holds, transport
untouched. Only end-of-stream *and* an empty ring reports `finished`.

**Web layout is one definition now** (`app/src/audioSab.ts`). Phase 0 had the
same constants copied into three files under a comment asking them to stay in
sync; M2 roughly quadruples the field count, so worker, worklet and engine all
import the layout instead. Frame counts are held as a `filledFrames` counter
the two ends add to and subtract from, rather than a pair of monotonic
indices -- an Int32 monotonic counter would wrap after ~12 hours.

### Verified

- **Native engine**, `cargo run -p player-core --example transport_smoke`
  (rewritten: it synthesises WAVs whose right channel is a 0->1 ramp across
  the track, so "did the audio really come from where the transport says"
  is measurable rather than something only a listener could answer). All 13
  checks pass against the real default device, including the 44.1 -> 48kHz
  resample:
  - `open()` does not scale with track length: 30s took 87ms, 300s (10x the
    audio) took 75ms -- 0.9x. Under whole-file decode this was linear.
  - No pre-seek audio survives a seek: after seeking to 20s the ramp read
    0.682 against an expected 0.683; a broken handshake would have read ~0.03.
  - Duration from the container, position in real time, pause freezing it,
    volume clamping, finished at the end, and seeking back un-finishing it.
- **Web**, in-browser against the running dev server:
  - The decoded-audio allocation no longer tracks track length: **0.837MB for
    both a 30s and a 300s track** (ratio 1.000), where Phase 0 would have
    allocated 10.6MB and 105.8MB. Load took 10ms and 24ms, not 10x apart.
  - Position advanced 1.010s per second of wall clock; the scope's ramp read
    0.754 against a position implying 0.755.
  - `seek(20)` reported 20.000 immediately, and 0.7s later the audio itself
    read 0.689 against an expected 0.690 -- i.e. the queued audio really was
    thrown away, not played out.
  - Finished exactly at 30.000; pressing play again restarted from 0.380.
  - Pause froze the position and did not report finished.
  - The scope still draws from the new ring: `draw_frame(scopeWindow(2048))`
    on a test canvas produced background `rgb(5,5,8)` plus 168 trace pixels of
    `rgb(38,242,140)`, read back pixel-by-pixel. DOM showed `0:13 / 0:30` with
    the slider at 13.02. No console or dev-server errors.
- **Native UI, user-confirmed live**: "Desktop test passes. Everything works as
  expected and sounds good. No errors." That covers the paths the automated
  checks can't reach -- React driving the Tauri commands, and a real compressed
  file, where the automated checks use synthesised WAV (a trivially seekable
  container). The one thing the session did surface was symphonia's MP3
  bit-reservoir warning on seek, written up below.

### The MP3 seek warning (2026-08-15)

The desktop test surfaced one thing worth writing down. Seeking in an MP3 logs

```
[symphonia_bundle_mp3::layer3][WARN] mpa: invalid main_data_begin, underflow by 424 bytes
```

and seeking in a WAV logs nothing. Read out of symphonia 0.6.1's source rather
than guessed at, it is expected and benign, for a reason that is actually the
pre-roll machinery working:

- MP3 frames don't carry all their own data. `main_data_begin` is a *backwards*
  offset into previous frames' bit reservoir.
- So symphonia's MP3 demuxer, on a seek, deliberately lands *before* the
  requested time: it walks back to the reference frame containing the first
  byte of the target frame's main data, and reports that earlier position as
  `SeekedTo::actual_ts` (`demuxer.rs`, "attempt to find the first (oldest)
  reference frame, then select 1 frame before that one to actually seek to").
  It is handing the caller a pre-roll and expecting it to be decoded and
  discarded.
- `StreamingDecoder::seek` already discards exactly `required_ts - actual_ts`
  (that's what `discard_frames` is for), so those frames never reach the
  device.
- The warning fires on the *first* of those pre-roll frames, which legitimately
  references bytes from before itself that nothing has. Symphonia's response is
  to zero the affected granules (`layer3/mod.rs`'s `read_main_data`,
  `underflow_bits`) -- silence, not garbage -- and the reservoir has refilled by
  the time the target frame decodes.

WAV has no bit reservoir, hence no warning there.

`cargo run -p player-core --example seek_smoke -- <path>` is the new example
that measures this instead of reasoning about it: for three positions it
decodes the same moment twice, once by playing through from the start and once
by seeking, then reports the leading run of exact zeros (the fingerprint of a
zeroed granule reaching playback) and the RMS difference between the two. On a
synthesised WAV: 0 silent frames and 0.00% difference at all three positions,
i.e. bit-exact. **User-confirmed passing on a real MP3 too** -- so the warning
is what the reading above says it is, and none of the zeroed audio it refers to
reaches playback.

## M3 -- Queue + gapless: decisions (2026-08-15)

1. **The engine holds current + next; the UI owns the playlist.** Gapless needs
   a one-deep lookahead in `player-core` and nothing more -- the decoder must be
   able to open the next track before the current one ends. Ordering, shuffle
   and repeat stay in the UI, on the same argument playback.rs already makes
   about volume curves: they're product decisions, and baking them into the
   audio core makes them harder to change than they should be.
2. **The web build runs one AudioContext rate for the session and resamples to
   it.** Web Audio cannot change a live context's rate, so per-track rates mean
   tearing the context down between tracks -- i.e. "gapless except when the rate
   changes", which is not a property worth shipping. Resampling in the worker
   instead reuses `StreamingResampler` unchanged (rubato 5 compiles clean for
   wasm32, checked), unifies both shells on one model, and closes
   ARCHITECTURE.md's "AudioContext sample rate must match the decoded track"
   gap as a side effect.
3. **Crossfade is deferred**, against the milestone's own "if it falls out
   cheaply" test -- it doesn't. A gapless hand-off is one decoder feeding one
   ring; a crossfade needs two tracks decoded concurrently and mixed, which is
   two decoders, two resamplers and a mix stage. That's its own milestone, not a
   rider on this one.

Three things follow from the shape rather than from taste, and are in scope:

- **Encoder delay and padding must be trimmed** (`Track::delay`/`Track::padding`,
  which `StreamingDecoder` currently ignores). Every MP3 carries ~1105 frames of
  encoder delay at the head; untrimmed, each boundary gets ~25ms of silence,
  which is exactly the gap this milestone is about.
- **Track boundaries have to travel with the audio.** The ring holds two tracks
  at once and the decoder runs up to a ring ahead of the speakers, so the
  position must reset when the boundary reaches the *output*. A second SPSC
  queue of boundary records, popped by the consumer as its frame total crosses
  them -- the same family as M2's seek handshake.
- **The ring becomes output-shaped.** It is currently interleaved in *source*
  channels with the callback adapting cyclically; a queue of mixed-channel
  tracks would change that geometry mid-stream. Normalising in the decoder
  thread keeps the ring one shape for the life of the stream.

### Encoder delay/padding: already handled (measured, 2026-08-15)

The first item M3's plan listed as in-scope turned out to be done already, and
the check was worth running before writing code rather than after.

Symphonia 0.6.1's `Packet` carries `trim_start` and `trim_end`, documented as
"the duration of decoded frames that should be trimmed... to remove encoder
delay/padding". Read as a request, which is the natural reading, that means the
caller should trim -- and `StreamingDecoder` didn't. But measuring a real MP3
before trusting that reading:

```
packet #0:    pts=-1105 dur=1152 trim_start=1105 trim_end=0   decoded_frames=47
packet #6678: pts=7691951      trim_start=0    trim_end=530 decoded_frames=622
```

47 = 1152 - 1105 and 622 = 1152 - 530: the decoder has **already** applied the
trims, and the packet fields report what it did.
`symphonia-bundle-mp3/src/decoder.rs:131` confirms it --
`self.buf.trim(packet.trim_start.get(), packet.trim_end.get())` inside `decode`.
Trimming again cut a further 577 frames of real audio off the track (47 from the
head, 530 from the tail), which is how the discrepancy showed up: total decoded
frames stopped matching the container's `num_frames`.

So: nothing to build, and a bug avoided that would have clipped the first
frames of every MP3 -- at exactly the track boundary M3 is trying to make
seamless. The change was reverted.

One real limitation falls out of the same investigation, now in
ARCHITECTURE.md's "Known gaps": symphonia's `isomp4` demuxer sets neither trim
field and its AAC decoder applies neither, so **M4A/AAC keeps its priming
frames** and gapless will only be truly gapless for MP3, Ogg, FLAC and WAV.
