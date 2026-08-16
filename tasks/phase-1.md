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
  between tracks, crossfade if it falls out cheaply. **Done, verified live
  on both shells** (crossfade deferred, see the decisions below).
- **M4 -- Analysis tap + spectrum analyzer.** The per-block tap every meter
  hangs off, plus the first consumer (FFT spectrum, log/linear). **Done,
  verified live on both shells.**
- **M5 -- Loudness + phase metering.** LUFS (integrated/short/momentary, per
  ITU-R BS.1770-4), true-peak, phase correlation, goniometer.
- **M6 -- 3D vectorscope v1.** PLAN.md §4's hero feature: extrude the 2D XY
  scope along a time axis. `player-render` grows a real 3D pipeline.
- **M7 -- Library + tags + playlists.** Folder scan, ID3/Vorbis comment
  reading, album art, search, persisted playlists. **Done, verified live on
  both shells.**
- **M8 -- Basic EQ.** Biquad chain in the DSP path. **Done, verified live on
  both shells.**

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

## M3 -- Queue + gapless: done

The player has a queue and crosses between tracks without a hole, on both
shells. Crossfade was deliberately deferred (see below); everything else the
milestone asked for is in and user-confirmed. Decisions, the hand-offs and
their live checks follow.

### Decisions (2026-08-15)

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

### The native hand-off (2026-08-15)

The engine holds two tracks: what is playing and what is next
(`Playback::set_next` / `clear_next`). Everything about *which* track that is
stays with the caller, which watches `Playback::track_index` -- a counter of
hand-offs, not a playlist index -- and queues the one after it.

**Track changes ride in the audio stream.** A second `rtrb` queue carries a
`Boundary { at_sample, duration_seconds, src_channels }` alongside the audio,
and the cpal callback applies one when its running consume total reaches
`at_sample`. The alternative -- having the decoder publish "now playing" when
it opens the file -- is wrong by up to a whole ring, so the title would change
while the previous song was still coming out of the speakers. The two counters
stay in step because the seek drain adds what it discards.

**The seam itself** is just ordering: at end of track the outgoing resampler's
tail is flushed into the ring *before* the next track is opened, so the last
partial window of one track and the first sample of the next are adjacent with
nothing between them. The callback cannot tell a track change from any other
block of audio, which is the point.

**Seeking in the last two seconds of a track had to be handled explicitly.**
The decoder runs up to a ring ahead, so near the end of a track it has already
moved on to the next one while the listener is still hearing the current one.
Seeking the track being *decoded* would scrub a song the user cannot hear yet.
The decoder therefore keeps the outgoing track's source until its successor's
boundary is actually consumed, and a seek arriving in that window rewinds to
it, handing the upcoming track back to the queue so gapless still works
afterwards.

#### The scope was hiding dropouts

Verifying "no silence at the seam" meant sabotaging the hand-off and checking
the test noticed. It didn't: stalling the decoder for three seconds -- well
past the ring's two -- still measured zero silence.

The cause was in `scope.rs`, not the test. The callback pushed to the scope
only what it read *from the ring*; the silence it synthesises during an
underrun went straight to the device and was never recorded. So a dropout left
the scope frozen on the last good audio instead of showing the hole -- the one
moment the scope is most useful was the one it hid. `ScopeRing::push_silence`
fixes it, and with the fix the sabotage fails the check as it should (4096
samples of silence).

Worth keeping from the same experiment: a **500ms** stall at the seam is
absorbed entirely by the ring and never reaches the speakers. That is the
buffer doing its job, and it sets the real margin for opening the next file.

The second sabotage -- disabling the seek rewind -- makes the seek test report
a 439Hz tone where it wants 220Hz, i.e. it scrubbed the queued track. Both
checks are load-bearing.

`cargo run -p player-core --example gapless_smoke` is the live check: two
synthesised tracks with different tone frequencies and lengths, so which track
is playing is measured from the audio (zero-crossing rate) rather than taken
from the engine's own reports.

### The web hand-off (2026-08-16)

Same design as the native side, same claims, and -- deliberately -- the same
checks. `app/gapless.html` (dev-only, served by `npm run dev`, never bundled)
runs the web counterpart of the `gapless_smoke` example, generating the same
two test tones and measuring the same things. Both shells report 11/11.

Three things had to move to make it possible:

**One AudioContext rate for the session.** M1's fix for pitched web audio was
to build the context at the *file's* rate. That works for one track and cannot
survive a queue -- Web Audio has no way to change a live context's rate -- so
the web path now does what the native one does: pick a rate once and resample
every track to it. `WasmDecoder` gained a `StreamingResampler` and the channel
adaptation, so `read` hands back audio already in the output's shape, exactly
as the native decoder thread does. `adapt_channels` moved into its own
cross-target module (`channels.rs`) to be shared.

**Boundary records in the SAB.** Four slots after the control block, an SPSC
queue with head/tail indices, carrying the incoming track's start frame and
length. `writtenFrames`/`consumedFrames` are Int32 and wrap; comparisons go
through `framesUntil`, which forces the difference back into int32. That is
exact because the two counters are only ever compared while within a ring
(~2s) of each other, nowhere near 2^31 apart.

**The scope's silence, again.** The web worklet had the identical blind spot
the native callback did -- it recorded only samples that came *out of the
ring*, so underrun silence never reached the scope. Fixed the same way, and
confirmed the same way: stalling the web hand-off past the ring makes the seam
check report 4096 samples of silence.

One measurement earns its keep twice. Reading the played tone's frequency from
its zero-crossing rate says which track is audible, but it is also a direct
check on the resampling that M1's pitched-audio bug was about: a broken
44.1k -> 48k conversion would read 239Hz where the test wants 220Hz. Both
shells measure within a few Hz.

### The playlist UI (2026-08-16)

The engine holds two tracks; something has to hold the rest. `playlist.ts` is
that something, and it is also the only module that chooses between the two
playback adapters -- it owns the list and turns "play track n" into the
load/queue pair both shells already expose. The one difference left between
them (a desktop track is a path for `LocalFile`, a web track is a `File` whose
bytes go to the worker) stops there rather than spreading through the UI, which
is the same argument nativePlayer/playerEngine already make with their shared
`TransportState`.

**`trackIndex` rides in the transport snapshot.** The native side gained
`set_next`/`clear_next` commands and a `track_index` field on `TransportState`
rather than a command of its own -- for the reason the struct exists at all: the
UI decides which row is highlighted from the same snapshot it draws the clock
from, so the two cannot disagree mid-poll. The counter is hand-offs since the
last load, not a playlist index, so App.tsx adds it to the index it loaded. That
keeps the engine's "I know two tracks" contract intact and means the row
highlights when the audio actually changes, not when the decoder moved on (up to
a ring earlier).

**A load clears the queue, so ordering the two matters.** The first version
queued the next track from an effect on the current index, which React runs as
soon as the row highlights -- while the load is still in flight. Both shells
clear the next slot as part of opening a track (`Playback::open` builds a fresh
engine; worker.ts's `load` handler sets `queued = null` after its `await`), so a
queue message that overtakes the load is silently dropped. Found live rather
than by reading: the first track played, then stopped at its end and the second
never started. The queue effect now waits on the load having settled.

Everything else is ordinary UI: add several files at once (both pickers take
multiple), click a row to jump, prev/next, remove. Deliberate jumps are *not*
gapless -- they replace what is playing, because the listener asked for it now.

**Verified live, web shell**, driving the real UI in-browser (files injected
into the file input, so React's own handlers run) against the dev server:
- Two synthesised tracks, 3s/220Hz and 5s/440Hz. The hand-off flipped the
  highlighted row, the title and the duration (0:03 -> 0:05) at **t+3.15s**,
  i.e. one 100ms poll after the first track's audio ran out, with the position
  restarting at 0.07 rather than continuing.
- After a deliberate jump back to track 1, the queue re-armed and the hand-off
  happened again -- the case the load/queue ordering bug above would break.
- Clicking a row, Next, Prev, pause (position held at 0.6 across 600ms), play,
  removing the playing row (its successor took over), and removing the last row
  (transport reset, list empty) all behave. No console errors on a fresh load,
  no dev-server errors.
- `/gapless.html` still reports **11/11** -- the engine underneath is unchanged,
  and this is the check that says so.

**Native UI, user-confirmed live** (2026-08-16): "desktop test passes,
everything works". That covers what the automated checks can't -- React driving
the new `set_next`/`clear_next` commands, where a mismatched argument name fails
silently at the IPC boundary -- and closes M3.

## M4 -- Analysis tap + spectrum analyzer: done

The tap every meter will hang off is in, with the spectrum analyser as its
first reader, on both shells. Decisions first, then what building it turned up.

### Decisions (2026-08-16)

1. **One tap, not a second copy.** Both shells already keep a ring of exactly
   what reached the output, written by the party that knows -- `ScopeRing`
   natively, the scope region of the SAB on the web. That *is* the analysis
   tap; meters read it rather than the audio thread pushing to a second ring.
   What it lacks is a way for a reader to know it was lapped, so the ring grows
   a monotonic written-count and readers hold cursors. The spectrum does not
   need that continuity -- an FFT of the last N samples at 60Hz is exactly
   right -- but M5's *integrated* LUFS must see every sample it claims to have
   measured, and building the cursor now costs a counter.
2. **Analysis never runs on the audio thread.** Not in cpal's callback, not in
   the AudioWorklet. Natively it gets its own thread; on the web it runs in the
   worker that already hosts wasm and already has a pump loop. This is the same
   rule M2 applied to decoding, for the same reason: a meter that overruns its
   budget must degrade into a stale reading, never into a dropout.
3. **The DSP is Rust, shared by both shells** (`player-core::spectrum`),
   including the window, the dB conversion, the log banding and the temporal
   smoothing. TypeScript draws bars from numbers it is handed and nothing else.
   Phase 0's two worst bugs were both web-only; a second implementation of the
   analysis in TS would be the same mistake with a slower feedback loop.
4. **Bins cross to the UI as dBFS**, not as 0..1 heights. The display floor is
   a display decision and belongs where the drawing is; the meaning belongs
   where the DSP is.
5. **Mono sum for v1.** Per-channel spectra are a display option, not a
   different measurement, and PLAN.md §4's spectrum analyzer is the summed one.
   Mid/side and per-channel are M5-and-later work.

### What it is

**One tap, two kinds of reader.** `scope.rs` became `tap.rs`: the same ring the
audio callback already filled with what it sent the device, now with a
`TapCursor` beside the existing `window(n)` snapshot. The distinction is the
milestone's whole point -- a display wants "the last N samples" and is right to
miss what happened between frames, while M5's integrated LUFS has to have seen
every sample it claims to have measured. The cursor cannot stop the audio thread
lapping it (nothing may make that thread wait) but it reports when it happened,
which is the difference between a meter that degrades honestly and one that
lies. The web build has the identical pair: `CONTROL.tapWritten` next to the
scope region it already had.

**The meters, in Rust, off the audio thread.** `analysis.rs` holds the drain
loop and `spectrum.rs` the first meter -- periodic Hann, real FFT (`realfft`,
already in the tree under rubato), 64 log-spaced bands from 20Hz, peak bin per
band, dBFS normalised so a full-scale sine reads 0. Both are cross-target and
free of threads: natively a `player-analysis` thread drives them, on the web the
worker's existing pump does, and neither is the audio thread. `dt` is passed in
rather than assumed, so the 30dB/s decay looks the same at 30fps and 144fps.

**Levels cross to the UI as dBFS** and `Spectrum.tsx` draws bars, choosing its
own display floor (-90dB, against the data's -120). The bands are log-spaced, so
laying the bars out evenly *is* the log axis.

**The tap is pre-volume on both shells** -- native pushes what came out of the
ring before applying gain, the worklet writes to the tap before its GainNode.
That is the only sane answer for a loudness meter: turning the monitoring level
down must not change what the track measures. Both smoke checks assert it.

### Two things the checks caught

**Adjacent bands were sharing a bin.** Band edges rounded outwards -- floor the
bottom, ceil the top -- so the bin holding a 440Hz tone belonged to both band 28
and band 29, which then read *exactly* the same level. "Which band is loudest"
came down to how a max broke ties, and the two shells broke it differently:
the web harness reported 433Hz, the native one 483Hz, on identical audio. Bands
now take a half-open `[lo, hi)` of bin centres, so no bin is in two bands, with
a test that says so. (Bands below ~200Hz are narrower than the 23Hz bin spacing
and still share bins by necessity -- better than a display with holes in the
bass.)

**A paused engine froze the meters.** Both callbacks silenced the output and
returned *without* recording anything, so the tap stopped advancing, the
analyser re-ran the same window forever, and the display held its last bar
indefinitely. This is the same bug M3 found in the scope during dropouts, in a
different place: the tap holds what the device received, and while paused the
device receives silence. Both shells now push that silence, and the meters decay
at the release rate instead -- measured at exactly 30dB in one second on both.

### Verified

- **Native**, `cargo run -p player-core --example spectrum_smoke` (new): 10/10
  against the real default device. A -3.1dBFS 440Hz tone reads **-3.39dBFS in
  band 28 (433Hz)**, 52dB down two octaves away; the volume control moves it by
  0.00dB; a hand-off to 3520Hz moves the peak to band 47 and empties band 28;
  pause decays -3.3 -> -33.3dB in a second; a seek recovers to -3.29dBFS.
- **Web**, `/spectrum.html` (new, the counterpart harness, same claims, same
  synthesised tones): 10/10, **-3.39dBFS in band 28** -- the same number to the
  hundredth of a dB, which is the evidence that decision 3 (one implementation,
  two hosts) actually holds rather than being an intention.
- **The UI canvas**, read back pixel by pixel in-browser: background
  `rgb(5,5,8)`, bars `rgb(38,242,140)`, tallest bar at **band 28**, 140px of the
  146 available -- which is exactly where -3.4dBFS lands on a -90..0 scale. (The
  Browser pane doesn't composite, so `requestAnimationFrame` never fires there;
  the check drives the component's own loop with a timer instead.)
- **Native UI, user-confirmed live** (2026-08-16): "tested and works
  perfectly". That covers the path the automated checks can't -- React driving
  the new `spectrum`/`spectrum_bands` commands across the IPC boundary, and a
  display judged by eye rather than by an assertion.
- 49 unit tests in `player-core`, including the spectrum's analytic ones (a
  full-scale sine at 0dBFS, halving amplitude costing 6.02dB, leakage two
  octaves out, the release rate, mono and stereo of the same signal agreeing)
  and the tap's cursor ones (every sample seen, lapping reported, a clear
  reported).

## M5 -- Loudness + phase metering: done

The second and third readers of M4's tap: a BS.1770-4 loudness meter (momentary,
short-term, integrated, true peak) and a phase pair (correlation, goniometer).
Decisions first, so the reasoning is on the record before the code is.

### Decisions (2026-08-16)

1. **The K-weighting coefficients are derived per rate, not tabulated at
   48kHz.** BS.1770-4 states its filter as coefficients for 48kHz, and most
   implementations paste that table -- which is wrong on this machine, whose
   output devices run at 96kHz (M4's own smoke check opens at whatever rate the
   default device offers). The filters are built by bilinear transform from the
   spec's analog prototype instead, with a test asserting the derivation
   reproduces the published 48kHz table to 1e-6. That test is the whole reason
   to trust the derivation at 44.1 or 96.
2. **Integrated loudness measures one continuous listen, and resets when the
   listen breaks.** A new file, a seek, a gapless hand-off, or a tap
   discontinuity all reset it. Integrating across a splice would report a number
   for a timeline nobody heard -- and M3's hand-off is exactly what makes "the
   track changed" an event the engine can see (`track_index`), so there is no
   excuse for pretending two tracks are one measurement. Momentary and
   short-term reset with it.
3. **The loudness and phase meters share the spectrum's drain.** They are
   readers of the same `Meters::ingest` the spectrum already gets every sample
   from -- which is what `TapCursor` was built for in M4 (decision 1). No second
   tap, no second cadence, and the discontinuity flag that M4 needed for the
   FFT window is the same one integrated loudness needs for its gating blocks.
4. **The meters cross to the UI as one snapshot**, the way `TransportState`
   does: momentary/short/integrated LUFS, true peak in dBTP, correlation. Five
   numbers that describe one moment must be read in one go, or a display can
   show a momentary from one instant against an integrated from another.
5. **Silence reads a floor, not negative infinity.** -70 LUFS, which is
   BS.1770-4's own absolute gate. The honest value is -inf, but it cannot cross
   the Tauri boundary (serde_json turns a non-finite float into `null`) and the
   two shells have to report the same thing -- so the floor is the engine's
   answer everywhere, and "below the gate" is what it means.
6. **True peak is measured, not approximated by the sample peak.** 4x
   oversampling through a windowed-sinc polyphase interpolator, per BS.1770-4
   Annex 2's method (its method, not its exact filter table). The test is
   analytic: a full-scale sine at fs/4 sampled through its zero crossings has a
   sample peak of -3dBFS and a true peak of 0dBTP, and a meter that reports
   -3 there is the one this exists to replace.
7. **The goniometer's points are computed in Rust and published with the
   meters** -- the same rule as the spectrum (M4 decision 3), even though the
   transform is two lines of arithmetic. Two lines are enough to disagree
   across shells. Decimated to a fixed count so the per-frame cost across the
   IPC boundary is bounded rather than a function of the sample rate.

### What it is

**BS.1770-4, derived rather than pasted.** `loudness.rs` holds the K-weighting
(high shelf into an RLB high-pass), the 100ms sub-block accumulation everything
else is built from, momentary (400ms), short-term (3s), gated integrated, and
true peak. The filters are built by bilinear transform for whatever rate the
output device opened at, and `derivation_matches_the_published_table` holds that
derivation to the spec's own 48kHz coefficients to 1e-6. That test is what makes
the 96kHz case trustworthy: there is nothing to compare against at 96kHz, so the
evidence has to come from agreeing with the standard where the standard states
an answer.

**True peak is measured.** 4x oversampling through a 48-tap windowed-sinc
polyphase interpolator, BS.1770-4 Annex 2's method rather than its table (which
is 48kHz-only for the same reason). The test is analytic: a sine at fs/4 sampled
through its zero crossings has every sample at -3.01dBFS and reaches full scale
between them. The interpolator reads **-0.07 dBTP** where the answer is 0.

**Phase, two ways.** `phase.rs` has the correlation meter (a one-pole 300ms
average of the L/R sums, so the needle doesn't depend on how the caller chunks
its drains -- there is a test for exactly that) and the goniometer's points. The
points are the orthonormal mid/side rotation, so they are real amplitudes: a
hard-panned full-scale sample plots at 0.707 and full-scale mono at 1.414. What
to do with that is the display's business, as the spectrum's floor is.

**One drain, four meters.** `Meters::ingest` feeds all of them. The spectrum and
the goniometer are pictures of a window; loudness and correlation are
measurements over time and get every sample. That is M4's `window`/`TapCursor`
distinction one layer up, which is why the cursor was built then.

**Five numbers cross as one snapshot** (`MeterSnapshot`), the way
`TransportState` does, and the goniometer's points cross decimated to 256 so the
per-frame cost is a property of the display rather than of the sample rate.
`Loudness.tsx` polls at 10Hz -- the shortest window here is 400ms, and digits
that flicker faster than they can be read are worse, not better -- while
`Goniometer.tsx` runs at frame rate, because it is a picture.

### Two things the checks caught

**The hand-off leaked one track's peak into the next.** The first native run
reported the -33 LUFS track holding the previous track's **-23.0 dBTP**. A break
is only noticed once the drain containing it has been read, so that drain holds
audio from both sides -- and a peak meter latches the loudest sample it ever
saw. Resetting and then measuring those samples put the outgoing track's peak
into the incoming track's hold. The drain carrying a break is now dropped
outright, which costs at most 16ms (native) or ~20ms (web) of audio at each
break, well under one sub-block of the loudness measurement.

**A stale wasm build showed up as exactly one failing check.** The web harness
reproduced that same true-peak failure after the native side was fixed, because
`npm run wasm` had run before the fix. Worth recording as the shape of the
symptom: when the two shells disagree on exactly one check and the native side
just changed, suspect the wasm bundle before the code.

### Verified

- **Native**, `cargo run -p player-core --example loudness_smoke` (new): **15/15**
  against the real default device at 48kHz. A tone generated at -23 LUFS reads
  **-23.00** momentary, short-term and integrated, with a true peak of -23.00
  dBTP; the volume control moves it by 0.00; identical channels read a
  correlation of **1.000** and an inverted pair **-1.000**; the goniometer holds
  mono material to 0.0000 of side energy and an inverted pair to 0.0000 of mid.
  After a gapless hand-off into a -33 LUFS out-of-phase track, integrated reads
  **-33.00** rather than the -25.6 an unreset meter would average to. A pause
  empties the momentary window to the floor while integrated moves 0.23 LU --
  the absolute gate doing its job.
- **Web**, `/loudness.html` (new, the counterpart harness, same claims, same
  synthesised tones): **15/15**, and every figure matches the native run to the
  hundredth -- -23.00, -33.00, -33.00 dBTP. The same evidence M4 produced for
  the spectrum, for the same reason: one implementation, two hosts.
- **The UI**, read back pixel by pixel in-browser against a 440Hz tone generated
  at -23 LUFS: the readout shows **-23.7 / -23.7 / -23.7 LUFS and -23.0 dBTP**
  with a correlation of 1.00 and the phase bar filling the right half. The 0.7
  LU difference from the harness is the K-weighting itself -- the shelf gives
  1kHz +0.69dB and 440Hz nothing -- and the true peak, which is unweighted, is
  unchanged at -23.0. The goniometer's points are all within 2px of the centre
  line, spanning 11px vertically, which is where a quiet mono tone belongs.
- **Native UI, user-confirmed live** (2026-08-16): "passes all tests, M5 is
  confirmed". That covers what the automated checks cannot -- React driving the
  new `meters`/`goniometer` commands across the IPC boundary at two different
  cadences, and meters judged against real music by eye rather than against
  synthesised tones by assertion.
- **No regressions**: `/gapless.html` and the native `gapless_smoke` still
  11/11, `/spectrum.html` and `spectrum_smoke` still 10/10 (-3.39dBFS in band
  28, unchanged).
- 73 unit tests in `player-core`, up from 49: the K-weighting against the
  published table, the -23 LUFS calibration, 6.02 LU per halving, rate
  independence across 44.1/48/96kHz, gating a silent passage out of an
  integrated number, the inter-sample peak, correlation at +1/0/-1, and the
  goniometer's rotation.

## M6 -- 3D vectorscope v1: done

PLAN.md §4's hero feature: extrude the XY scope along a time axis, and grow
`player-render` from Phase 0's flat line strip into a real 3D pipeline.
Decisions first, then what building it turned up.

### Decisions (2026-08-16)

1. **The extrusion shows a fixed duration, not a fixed sample count.** 120ms
   of history, decimated to a fixed 2048 points. The depth axis *is* time, so
   a fixed sample count would show half as much music at 96kHz as at 48kHz --
   the same track would look different on different hardware, which is exactly
   the class of bug M4's band edges and M5's K-weighting table both were. A
   fixed point count on top of it keeps the vertex buffer and the draw call the
   same size whatever the device runs at. 120ms is roughly an eighth note at
   120bpm: long enough that the ribbon has shape, short enough that it still
   moves with the music rather than smearing into a tangle.
2. **The axes are the goniometer's.** x is the side signal `(L-R)/sqrt(2)`, y
   the mid `(L+R)/sqrt(2)` -- the same orthonormal rotation M5's goniometer
   draws -- and z is age. So the two displays agree about what mono looks like
   (vertical in both) and the 3D one is honestly the 2D one with time added,
   rather than a second opinion about the same audio.
3. **The renderer owns the camera, the depth buffer and the multisampling;
   callers still only hand over samples.** `draw_frame(samples, channels)` is
   the whole interface, with `channels` new because the tap is in the output's
   layout and that need not be stereo. Both shells keep calling the one method,
   which is what has kept them from drifting since Phase 0.
4. **The native scope stays its own window, but the UI owns its geometry.** A
   wgpu surface cannot composite into a webview, so the second window is not a
   limitation to design away -- it is the only way the same renderer runs
   natively at all. What *was* wrong is that its size was a constant in Rust:
   the window is now resizable and the UI sets its size and visibility through
   commands, with the web canvas taking the identical control, so scope sizing
   is one component on both shells.
5. **The pipeline is verified by rendering offscreen and reading back pixels.**
   `Renderer::offscreen` runs the same scene into a texture rather than a
   surface, so a smoke example can assert where the figure actually landed --
   a mono tone must draw a vertical ribbon, an inverted pair a horizontal one,
   and the far end of the extrusion must be dimmer than the near end. Checking
   a renderer by looking at it is how a wrong camera matrix ships.

### What it is

**A real 3D pipeline, in the crate that already ran on both shells.**
`camera.rs` is the matrix arithmetic (hand-written, because one perspective and
one look-at have exact answers a test can state, and wgpu's 0..1 clip-space
depth convention is the part that actually catches people out -- there is a
test that says so). `scene.rs` owns the pipeline, the uniform the camera rides
in, the depth buffer and 4x multisampling; `trace.rs` turns a window of played
audio into points. `lib.rs`'s `Renderer` is still "hand over samples, get a
frame", which is what has kept the two shells from drifting since Phase 0.

**The extrusion.** Each decimated frame of audio becomes a point: x is the side
signal, y the mid, z its age. The shader places the newest sample nearest the
viewer and fades the ribbon from phosphor green to the brand's violet as it
recedes, which is what makes depth readable on a still frame rather than a
tangle of overlapping lines.

**One duration, asked once.** `window_samples(rate, channels)` is the only
place the 120ms becomes a sample count. `src-tauri`'s redraw thread calls it
directly; the web's draw loop calls it through `scopeWindowSamples` in the wasm
glue. Neither shell owns a constant of its own, so neither can drift.

**The scope's geometry moved to the UI.** The native window is resizable, and
`scope_state`/`set_scope_size`/`set_scope_visible` let React set it; on the web
the same `ScopeControls` component resizes the canvas and tells the renderer.
`scope.ts` is the third shell-choosing adapter, beside playlist.ts and
analysis.ts.

### What the checks caught

**The camera was on the wrong side.** The first offscreen run reported the
ribbon's bright, newest end on the *left* of the image and history running off
to the right -- backwards from every other audio display there is. Nothing was
broken; the camera simply sat at +x, where its right vector points along -z.
Moving it to -x fixed it, and the comment on `EYE` now says why the side is not
arbitrary. This is exactly the class of thing "it looks fine" would have
shipped: the picture was perfectly plausible.

**The background is not black.** The first version of the check called all
102,400 pixels part of the trace. The pass clears to a linear (0.02, 0.02,
0.03), which is roughly (40, 40, 48) once sRGB-encoded -- so a threshold picked
for linear values matches everything. Worth writing down because the same trap
sits in front of anyone reading pixels back from an sRGB target.

### Verified

- **Native**, `cargo run -p player-render --example scope_smoke` (new): **8/8**,
  rendering the real scene into an offscreen texture and reading it back. A
  mono tone draws a figure **196px tall and 74px wide**; an inverted pair
  **213x61**, i.e. the same audio rotated onto the other axis; a tenth of the
  amplitude draws 62px against 196px, so nothing is being normalised; the near
  half of the ribbon means green 172 against the far half's 114; silence draws
  **0 lit pixels**; and a 2:1 viewport leaves the figure 74px wide rather than
  stretching it.
- **Web**, `/scope.html` (new, the counterpart harness, same claims, same
  synthesised audio, drawn through the browser's own WebGPU into a real canvas
  and read back): **8/8**, with the figure at **195px tall and 66px wide**
  against the native run's 196x74, in the same place on screen (x 141..207 and
  y 83..278, against 133..207 and 82..278). The few pixels of difference are
  line rasterisation and MSAA resolve between Dawn and the native backend --
  the geometry, the axes and the fade agree.
- **The UI controls**, driven in-browser: the size buttons take the canvas from
  480 to 640 (CSS width follows), the current size is highlighted, and Hide
  toggles to Show with no errors.
- **Native UI, user-confirmed live** (2026-08-16): "works as expected, no
  issues or errors". That covers the two paths the automated checks cannot --
  the real wgpu surface on a native window rather than an offscreen texture,
  and React resizing that window live through `set_scope_size`, which the
  renderer only notices via its own size poll.
- **No regressions**: `/loudness.html` still ALL PASS, and the 73 `player-core`
  tests are joined by **16 in `player-render`** -- the camera's conventions
  (what the camera looks at lands in the middle, +x is right, +y is up, near
  sorts in front of far, depth runs 0..1, a wide viewport does not stretch) and
  the trace's (a duration not a sample count, bounded point count, age falling
  towards the newest sample, mono up the centre, inverted flat, and the
  amplitudes matching the goniometer's).

## M7 -- Library + tags + playlists: done

The first milestone in this phase that is not about audio. Phase 1's opening
note says tags are "probed by Symphonia and then discarded"; this is where they
stop being discarded, and where the player gets something to play *from*
besides a file picker.

### Decisions (2026-08-16)

1. **Tag reading is Rust, cross-target** (`player-core::tags`), for the third
   time and the same reason as M4 decision 3 and M5 decision 7: one
   implementation, two hosts. There is a real temptation to reach for a
   JS tag library on the web side, because they exist and they are easy; that
   is precisely the mistake Phase 0's two worst bugs were. Symphonia already
   opens every one of these files to play it, and its 0.6 metadata API hands
   back parsed `StandardTag`s across ID3v2, Vorbis comments, RIFF INFO and MP4
   `ilst` alike -- so one reader covers every format the player supports, and
   both shells get the same answer by construction.
2. **Metadata revisions are applied oldest-first, so the newest wins.**
   Symphonia keeps a *log* of revisions, not one tag set, and the order is not
   arbitrary: `Probe::probe` calls `probe_trailing` **before** it scans forward
   for the container, so an MP3 carrying both tags yields `[ID3v1, ID3v2]`.
   First-wins would therefore let ID3v1's 30-character truncated title beat the
   real one. Verified by reading symphonia 0.6.1's `formats/probe.rs` rather
   than assumed from the shape of the API.
3. **The scan belongs to the shell; the index belongs to the UI.** Same split
   M3 decision 1 made for the queue. `player-core` answers "what are this
   source's tags" and, natively, "walk this folder" -- it has a filesystem.
   The browser does not: it has *handles*. So the two shells cannot share a
   scanner, and pretending otherwise would mean a lowest-common-denominator
   one. What they do share is everything above it -- rows, search, playlists --
   which is one TS module (`library.ts`, the fourth adapter beside playlist.ts,
   analysis.ts and scope.ts) over a per-shell store.
4. **Where the web build's library lives: IndexedDB, folder handle included.**
   The one genuinely open question in this milestone, so the reasoning is worth
   stating rather than just the answer:
   - *localStorage* is synchronous, capped around 5MB, and stores strings. A
     library index is neither small nor stringy, and writing it on the main
     thread would stutter the scope's own rAF loop -- the thing this app is
     built to show off.
   - *OPFS* would mean copying the user's music into browser-private storage:
     gigabytes duplicated, and an index that silently goes stale against the
     real folder the moment anything changes on disk.
   - *IndexedDB* is asynchronous, structured-clone capable, and -- the property
     that actually decides it -- can store a `FileSystemDirectoryHandle`. That
     is what makes a persisted library possible on the web *at all*: the rows
     survive a reload and so does the ability to reopen the real files, subject
     to a permission check on a user gesture. Cover art rides along as Blobs,
     deduplicated by content hash so an album's twelve tracks hold one image.
   - Where `showDirectoryPicker` is missing (Firefox, Safari) the scan falls
     back to `<input webkitdirectory>`: the rows still persist, the handles do
     not, so those rows are marked as needing a rescan rather than pretending
     to be playable. Acceptable as a fallback rather than a gap because the web
     build already requires WebGPU and `crossOriginIsolated` -- it is a
     Chromium build today whatever this milestone does.
5. **Cover art never crosses the JSON IPC boundary.** A 50KB JPEG through
   `invoke` is a JSON array of 50,000 numbers -- ~150KB of text, per row, for a
   list that might show two hundred of them. Natively the art is cached to the
   app data directory and served to the webview through a `cover://` URI scheme
   protocol; on the web it is an object URL over the IndexedDB blob. Both hand
   `<img src>` an ordinary URL, so one component draws art on both shells and
   neither one ships an image through a command.
6. **Scanning runs off the audio path *and* off the UI thread.** Natively that
   is a background thread; on the web it is a **second** worker rather than
   worker.ts, which owns the decoder pump and the meters. A five-thousand-file
   tag sweep sharing that worker would stall ring refills, i.e. it would be
   audible. This is M4 decision 2's rule ("analysis never runs on the audio
   thread") one layer out: nothing the library does may reach the speakers.
7. **Playlists persist as references, not copies** -- a name and a list of
   library ids. A playlist that copied its rows would go stale against the
   library the first time a tag was fixed.

### What it is

**One tag reader, two shells.** `player-core/src/tags.rs` reads title, artist,
album, album artist, genre, track/disc number, year, cover art and the stream's
own properties out of whatever symphonia's probe found -- ID3v2, Vorbis
comments, RIFF INFO and MP4 `ilst` alike, because 0.6's metadata API hands back
parsed `StandardTag`s rather than raw strings. It decodes no audio, which is
what makes scanning a few thousand files reasonable. `player-rt-wasm`'s
`WasmTags` is the web's way in; `src-tauri` calls straight in. Same code, so
the two shells cannot disagree about what a file is called.

**The desktop library** (`player-core/src/library.rs`) walks a folder, reads
each file's tags, and keeps a versioned JSON index plus a `covers/` cache in the
app data directory. Cover files are named by the *content* hash of the image, so
an album's twelve tracks share one file. Rows are keyed by path, which is what
makes a rescan idempotent -- a file still there is updated, a deleted one drops
out, a new one appears -- and what lets a saved playlist survive a rescan. The
index is written through a temporary file and renamed, so a crash mid-write
leaves the previous index rather than half of a new one.

**The web library** is IndexedDB (`app/src/libraryStore.ts`), holding the rows,
the cover blobs, the playlists, and the scanned folder's
`FileSystemDirectoryHandle`. That last one is the whole reason for the choice:
it is what lets a later session reopen the real files rather than asking for the
folder again. `libraryWorker.ts` does the tag reading in a second worker, off
both the UI thread and the decoder's.

**Cover art never crosses a command.** Natively `src-tauri` registers a
`cover://` URI scheme protocol serving the cached file; on the web it is an
object URL over the stored blob. Both give `<img src>` an ordinary URL, so one
component draws art on both shells.

**Search is genuinely one implementation** rather than a shared intention: by
the time either shell has rows, it has the same array of them, so `search` in
library.ts filters both. Every word must match something, so "phosphor decay"
finds the one track rather than everything on the album.

### Three things the checks caught

**The `cover://` response needs an explicit CORP header.** This app sets
`Cross-Origin-Embedder-Policy: require-corp` so the audio path can have a
`SharedArrayBuffer`, and under that header a subresource without
`Cross-Origin-Resource-Policy` is blocked. Every cover would have failed to load
and the symptom would have looked exactly like a wrong path. Set deliberately
rather than discovered the hard way, because M2's COOP/COEP work made the
interaction predictable -- but it is the kind of thing worth writing down.

**One album's cover produced three object URLs.** Found by reading the DOM
rather than by looking at it: three rows sharing one cover had three distinct
`blob:` URLs. The cache in `coverUrl` was keyed on the *resolved* URL, so all
three rows -- which render in the same tick and ask in the same tick -- missed
it, and two of the three URLs were live references to a blob that nothing would
ever revoke. Caching the promise instead of its result fixes it; re-measured at
three rows, one URL.

**One moved file stopped the library playing anything.** Clicking a row queues
everything visible so the rest of the list follows gaplessly, and the resolve
step was a `Promise.all` -- which rejects on the first row whose file has
moved or whose folder permission has lapsed. So a single stale row broke every
click. Found live by clicking a good row sitting next to stale ones. It is now
`allSettled`: unopenable rows are skipped and counted, and only the row actually
clicked failing is reported as an error, since that is the one the listener
asked for. Same rule the scanner already followed -- one bad file is skipped,
never fatal.

### Verified

- **Native**, `cargo run -p player-core --example library_smoke` (new): **9/9**
  against a real directory tree, real ID3v2 tags and a real cover cache. Five
  audio files found and a `.txt` and a `.jpg` correctly ignored; an album's
  tags read back including the `"7/12"` track-number spelling; **3 covers, 1
  distinct** file on disk holding exactly the 516 embedded bytes; an untagged
  file still a usable row (44100Hz, 0.100s, falling back to its file name); a
  rescan producing the same five ids rather than ten rows; and the index and a
  playlist read back after the library was dropped and reopened.
- **Web**, `/library.html` (new, the counterpart harness, same claims, same
  synthesised files): **12/12**, agreeing with the native run figure for figure
  -- 5 found, 3-track album, **1 distinct cover of 516 bytes**, untagged row at
  44100Hz, rescan idempotent, playlist references still resolving. It also
  covers the two things only the web has: the IndexedDB store surviving a
  round trip, and search (4 rows -> 3 for an artist -> 1 for two words).
- **The UI**, driven in-browser against the dev server:
  - The panel renders from IndexedDB: 4 rows with durations formatted from the
    tags (3:06/3:07/3:08), an em dash for the row whose length the container
    never stated, and the folder marked "needs adding again to play" because
    its handle was absent -- the fallback path's honest state.
  - Search through React's own handlers: **4 tracks -> 3 of 4** for
    "vectorscopes" -> **1** for "phosphor decay" -> back to 4 when cleared.
  - Cover art really decodes: the blob URL resolves to a 1x1 image, and the
    three rows sharing a cover now share **one** URL. (The inline `<img>`
    reports `complete: false` in the automation pane for the same reason the
    scope's rAF loop never ticks there -- a hidden tab does not composite, so
    `loading="lazy"` never fires. The blob was fetched and decoded directly to
    prove the pipeline.)
  - **Playing from the library works end to end.** A real
    `FileSystemDirectoryHandle` was obtained through OPFS -- the same type
    `showDirectoryPicker` returns -- with a real 3s 440Hz WAV in it, stored as
    the folder's handle, and the row clicked for real. The handle walk found
    the file, the worker decoded it, and the position advanced **0.260 ->
    0.760 -> 1.250 -> 1.760** across four 500ms samples: 0.5s of audio per
    500ms of wall clock. The three stale rows beside it were skipped with a
    status line rather than an error, which is the `allSettled` fix above
    being load-bearing.
- **No regressions**: `/loudness.html` still ALL PASS (15/15) and the native
  `gapless_smoke` still **11/11**.
- **89 unit tests in `player-core`**, up from 73: the tag reader's (the common
  ID3v2 fields, stream properties with no tags at all, the front cover winning
  over the other pictures in the file, a lone picture used when none is marked
  front, a year taken out of a full ISO date, a malformed date leaving the year
  unset) and the library's (recursive scan ignoring non-audio, a rescan
  updating rather than duplicating, a bad file counted and skipped, the index
  surviving a reopen, a corrupt index opening empty rather than refusing to
  start, forgetting one folder leaving the others, playlists replaced by id,
  content hashing, and album grouping sorted by disc then track).
- **Native UI, user-confirmed live** (2026-08-16): "loading a folder works, as
  does saving a playlist. It's there after app restart too. it loaded the meta
  info and art." That covers what the automated checks cannot reach -- React
  driving the seven new `#[tauri::command]`s across the IPC boundary, where a
  mismatched argument name fails silently; a real folder of real MP3s rather
  than synthesised WAVs; cover art arriving over the `cover://` protocol into
  a real webview under the COEP header; and the index surviving a genuine
  process restart rather than a `drop` and reopen in one process.

  Two things about that session are worth keeping. The player's log shows
  clusters of seven and eight MP3s opening inside the same second -- the
  signature of the scan reading tags -- which is the scan doing in a second
  what playing those files would take twenty minutes to do. And the session
  ran ~90 minutes, during which the *scope* window's redraw thread panicked;
  that is pre-existing M6 code rather than anything here (a destroyed window
  arrives as `Outdated`, and M6's fix for `Outdated` calls `surface.configure`,
  which panics on a dead surface). It is written up in ARCHITECTURE.md's known
  gaps, unfixed, because deciding what the scope's lifecycle should be when its
  window is closed is scope work, not library work.

## M8 -- Basic EQ: done

The last milestone of Phase 1, and the first one where the audio the listener
hears is not the audio the file holds. Ten bands, a preamp and a bypass, in the
audio path on both shells. Decisions first, since one of them gives way on a
rule the last four milestones set.

### Decisions (2026-08-16)

1. **A ten-band graphic EQ, not a parametric one.** ISO octave centres from
   31.25Hz to 16kHz, +-12dB each, plus a preamp -- the layout a listener coming
   from any other player already knows how to read, and what PLAN.md's "basic
   EQ" means in the company it keeps there (§7's Phase 1 list, beside the tag
   editor and the library). The ends are a low and a high **shelf** rather than
   peaks: a peaking filter at 31Hz leaves everything below it alone, so "more
   bass" would do nothing to the lowest octave a system reproduces. Q for the
   eight peaks is `sqrt(2)`, one octave of bandwidth, matching the band spacing
   so a row of equal boosts sums to something close to a flat lift instead of a
   comb.
2. **The EQ runs before the analysis tap, and the volume still runs after it.**
   The chain is ring -> EQ -> tap -> volume -> device, identically on both
   shells. That keeps M4 decision 1's invariant exactly as it was -- the tap
   holds what reached the output -- and it is what makes the spectrum show the
   curve doing its work and the loudness meter report what the listener is
   actually being played, clipping included. Monitoring level is not part of the
   programme and the tap is the line between the two.
3. **The filters live on the audio thread**, in the cpal callback and in the
   AudioWorklet, not in the decoder thread or the worker. This is the one place
   the rule "nothing but output on the audio thread" gives way, and the reason
   is that everything upstream sits behind a ring holding two seconds of audio
   that has *already been filtered* -- an EQ a listener cannot hear move is not
   an EQ. A ten-biquad cascade is bounded, branch-free, allocation-free work,
   which is the kind of thing an audio callback may do; a flat setting is
   detected and skipped entirely.
4. **The design is Rust; the difference equation is written twice.** M4 decision
   3, M5 decision 7 and M7 decision 1 all say the DSP is Rust and both shells
   run it. That cannot be literally true here, because on the web the audio path
   *is* TypeScript (`player-rt-wasm`'s own header: no per-block wasm calls on
   the audio path, and an AudioWorklet cannot host wasm-bindgen glue). So the
   line is drawn where the bugs live: everything that could be *decided*
   differently -- what a slider position means, each band's shape and Q, the
   centres, the preamp, the curve the UI draws -- stays in `player-core::eq`,
   and what is duplicated is five multiply-adds. `/eq.html` then holds the two
   copies to each other sample for sample, which is what makes the duplication
   acceptable rather than a hope.
   - The alternative considered and rejected: chaining Web Audio's own
     `BiquadFilterNode`s after the worklet. No TypeScript DSP at all, but the
     two shells would then run different filter implementations, and the tap
     sits *before* those nodes -- so the meters and the vectorscope could not
     see the EQ, which is decision 2 gone.
5. **Coefficients cross to the audio thread as data, never behind a lock.**
   Natively they ride an `rtrb` SPSC queue, the way M3's track boundaries do,
   and the callback takes the newest of them before any of its early returns --
   so a slider moved while paused or mid-seek is still there when the music
   comes back, and a queue nothing drains cannot fill. On the web they sit in
   the shared buffer behind a generation counter the worklet reads as a seqlock:
   read the generation, copy, read it again, retry next block if it moved. The
   worklet keeps its own copy and refreshes only on a change, so the steady
   state costs one atomic load per block.
6. **The setting outlives the engine.** A new `Playback` is built per track, so
   an EQ that lived only in the engine would flatten itself at every track
   change -- audibly, at a gapless one. The desktop shell keeps it beside the
   playback slot and hands it to each new engine; the worker writes it into
   every shared buffer it creates. Persisting it between *sessions* is the UI's
   job (`eq.ts`, localStorage): ten numbers and a flag is exactly what
   localStorage is for, which is the same argument M7 decision 4 used for
   sending the library somewhere else.

### What it is

**One design, two appliers.** `player-core/src/eq.rs` holds the band layout, the
RBJ-cookbook derivations of the peak and the two shelves, the clamp, the flat
wire format both shells store coefficients in, `magnitude_response` for the
curve the UI draws, and `EqProcessor` -- the cascade as the native callback runs
it. `app/src/eqFilter.ts` is the worklet's copy of the difference equation and
nothing else: it is handed finished coefficients and never computes one.

**f64 all the way across**, including the shared buffer's EQ region (a
`Float64Array`, aligned deliberately rather than by luck). The lowest band is
31.25Hz, which at a 96kHz output rate -- what several of this machine's devices
run at -- is 0.0003 of the sample rate, and in f32 the difference between that
pole pair and the unit circle is a handful of representable values wide. Same
argument `loudness.rs` makes about its 38Hz high-pass, one band lower down.

**The curve is computed where the filters are.** `magnitude_response` evaluates
`|H(e^jw)|` over the actual cascade at the *output device's* rate, recomputed
when a slider moves and never per frame. So the picture behind the sliders is
the response of the filters that are running rather than a second drawing of the
same intention -- and both smoke checks compare the two directly, measuring the
audio and then asking the curve what it claims at that frequency.

**A flat EQ is bit-transparent, not nearly transparent.** A zero-gain band is
skipped at design time rather than turned into a unity biquad, and both
processors check for a transparent cascade and return without touching a sample.
Worth having as an equality rather than an approximation: it is the difference
between a player that is transparent with the EQ untouched and one that is only
very close to it, and the M4/M5 harnesses reporting exactly their old figures is
the evidence.

### Three things the checks caught

**An infinity is not an extreme.** The clamp's first version turned any
non-finite gain into zero while the unit test asserted `+inf` should clamp to
+12dB. Neither is obviously right, and the test failing is what forced the
choice: an infinity arriving over an IPC boundary is a bug upstream, and the
safe reading of a bug is "leave this control alone" rather than "move it as far
as it goes". It is in `clamped`'s own doc comment, because the next person will
wonder.

**The top band fell off a low-rate device.** 16kHz is above Nyquist at a 32kHz
output rate, where a centre frequency means nothing at all -- and this player
takes whatever rate the device offers rather than choosing one. The design now
pulls any centre down to 0.45 of the rate, so the top slider still reaches the
top of what the device can carry instead of silently doing nothing. Same class
of bug as M5's 48kHz-only K-weighting table and M6's fixed sample count, caught
the same way: by asking what happens at a rate nobody tested on.

**The curve mirrored back down past Nyquist.** Found immediately after, since
the axis runs to 20kHz whatever the rate: a discrete filter's response folds
around Nyquist, so on that same 32kHz device the drawn curve would have swept
back up through frequencies the device cannot carry. It holds its last real
value instead, which is honest about there being nothing there to draw.

And one thing writing the web harness caught, which was a divergence rather than
a bug: `set_eq` on the desktop shell answers with the *clamped* setting, so the
display shows what is being played. The web path answered with what was asked
for. Both now return the clamped values out of the same Rust clamp -- the check
that sends +99 to a slider and reads +12 back passes on both shells for the same
reason.

### Verified

- **Native**, `cargo run -p player-core --example eq_smoke` (new): **14/14**
  against the real default output device at 48kHz, with the spectrum analyser as
  the measuring instrument -- which is only possible because the EQ runs ahead
  of the tap, so a build with those two the other way round fails every check in
  it. A +12dB slider moves its own band by exactly **+12.00dB** and its
  neighbour five octaves away by **+0.11dB**; a -12 cut reads **-12.00**; a -6dB
  preamp moves both bands by **-6.00** and the momentary loudness from **-7.04
  to -13.04 LUFS**; the volume control still moves the meters by 0.00dB; bypass
  returns the audio to the flat baseline to the hundredth and flattens the curve
  while the sliders stay where they were; +99 clamps to +12 and a NaN to 0; and
  the setting survives a seek.
- **Web**, `/eq.html` (new, the counterpart harness, same claims, same
  synthesised tones): **15/15**, agreeing with the native run figure for figure
  -- the same -12.67dBFS baseline, the same +12.00/-12.00/-6.00, the same
  -7.04 -> -13.04 LUFS. The fifteenth check is the one with no native
  counterpart, and the reason this harness matters more than the others:
  eqFilter.ts against `EqProcessor` over the same 4096-frame stereo signal
  through a cascade with both shelves, six peaks and a preamp in it -- **worst
  sample differs by 0.00e+0**. Not "within tolerance": identical.
- **The UI**, driven in-browser through React's own handlers against the dev
  server:
  - Moving the 1kHz slider to +12 moved the momentary readout from **-7.0 to
    +0.5 LUFS** and put the curve's peak at **x=271, y=19** of a 480x120 canvas.
    Both are arithmetic: 1kHz sits at 0.566 of a 20Hz..20kHz log axis (x=272),
    and +12dB on a +-18dB scale is y=20.
  - A -6dB preamp moved the readout by exactly **-6.0 LU** and flattened the
    curve to **y=79**, where 0dB is y=60 and -6dB is y=80.
  - Bypass moved the loudness **-13.0 -> -7.0 -> -13.0** with the sliders
    untouched, greying the panel rather than resetting it.
  - The setting survived a **full page reload**: preamp -6 and 63Hz +9 came back
    on the sliders, in the readouts and in the curve, whose highest point sat at
    **x=77, y=49** -- 62.5Hz is x=79 on that axis, and +9 through a -6 preamp is
    +3dB, which is y=50.
- **No regressions**, which matters more here than in any earlier milestone
  because M8 puts new arithmetic in the path of every sample on both shells:
  `/loudness.html` **15/15** and the native `loudness_smoke` unchanged to the
  hundredth (-23.00/-33.00), `/spectrum.html` and `spectrum_smoke` **10/10**
  each (-3.39dBFS in band 28, the same number M4 and M5 recorded), and
  `/gapless.html` and `gapless_smoke` **11/11** each. A flat EQ really is
  transparent.
- **Native UI, user-confirmed live** (2026-08-16): "desktop test passes,
  everything works". That covers what the automated checks cannot reach --
  React driving the new `eq_state`/`set_eq` commands across the IPC boundary,
  where a mismatched argument name fails silently; the setting surviving a real
  process restart rather than a page reload; and an EQ judged against real music
  by ear rather than against synthesised tones by assertion. It closes M8, and
  with it Phase 1's milestone list.
- **101 unit tests in `player-core`**, up from 89, twelve of them the EQ's: each
  band reaching its own gain at its own centre measured *on the audio* rather
  than on its coefficients, a bell leaving two octaves out alone, the measured
  response agreeing with the drawn curve, the preamp, flat being bit-exact,
  bypass ignoring the sliders, rate independence across 44.1/48/96kHz, channels
  filtered independently, a block split mid-frame filtering identically to a
  whole one (the wrapped-ring case), a reset matching a fresh processor, the
  clamp, and the top band on a 32kHz device.

## Phase 1's milestone list is complete (2026-08-16)

M1 to M8 are all done and user-confirmed on both shells, which is everything
PLAN.md §7 asked of this phase: a local file player with a library, tagging,
playlists, a basic EQ, a spectrum analyser, phase correlation, LUFS metering and
the 3D vectorscope, shipping as a Tauri desktop build and a web build.

What is *not* claimed by that: the tracked limitations in ARCHITECTURE.md's
"Known gaps" are still open, and two of them are ordinary bugs rather than
design trades -- pressing play at the end of a track pauses instead of
restarting (found during M7, a UI-level disagreement between `playing` and
`finished`), and closing the scope window by hand panics its redraw thread
(pre-existing M6 code). Neither belonged inside a milestone that wasn't about
them, and both are worth clearing before Phase 2 rather than carrying forward.
