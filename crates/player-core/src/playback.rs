//! Native (cpal) playback. Not built on wasm32 -- cpal has no wasm target;
//! the web build drives output through the Web Audio API / AudioWorklet
//! instead (see `player-rt-wasm`, and PLAN.md §5's "Web audio I/O" note).
//!
//! # Shape (Phase 1 M2)
//!
//! ```text
//!   decoder thread                                ring (rtrb)   cpal callback
//!   StreamingDecoder -> resampler -> channels -> [ ~2s ] -> output + AnalysisTap
//! ```
//!
//! Everything left of the ring is the decoder thread's, and it hands over
//! audio already in the device's shape -- output rate, output channel count.
//! M2 left the channel adaptation in the callback; M3 moved it here so that
//! the ring's frame width is a property of the device rather than of the
//! track, which is what lets a queue hand from a mono file to a stereo one
//! without re-interleaving the samples already queued behind the seam.
//!
//! Phase 0 and M1 had no producer at all: the entire track was decoded into a
//! `Vec<f32>` up front and the callback walked a cursor over it under a
//! `Mutex`. That was fine for a spike and wrong for a player -- a 40-minute
//! FLAC cost its full decoded size in RAM before the first sample played, and
//! the callback could block on a lock held by whoever else wanted the buffer.
//!
//! M2 replaces both. Audio is produced a block at a time by a decoder thread
//! and handed over through a lock-free SPSC ring, so the callback never locks,
//! never allocates, and memory is bounded by the ring rather than by the
//! track. What it costs is that "where are we" and "go here" stop being
//! trivial -- see `Shared` and `seek` for how those are kept honest.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use crate::analysis::{MeterSnapshot, Meters, MusicalSnapshot, GONIOMETER_POINTS};
use crate::channels::{adapt_channels, ChannelShape};
use crate::eq::{EqCoefficients, EqProcessor, EqSettings};
use crate::error::PlayerError;
use crate::resample::StreamingResampler;
use crate::source::PlaybackSource;
use crate::stream::StreamingDecoder;
use crate::tap::{AnalysisTap, TapCursor};

/// Upper bound on source channels, rejected at `open` rather than discovered
/// in the callback. Well past anything Symphonia decodes (22.2 surround is
/// 24).
const MAX_CHANNELS: usize = 32;

/// How much audio the ring holds, in seconds of output. Big enough that a
/// decoder hiccup (a slow read, a page fault, the OS descheduling the thread)
/// doesn't reach the speakers; small enough that a seek doesn't have to throw
/// much away and memory stays trivial (~768KB at 48kHz stereo).
const RING_SECONDS: f64 = 2.0;

/// Samples of playback history the analysis tap holds. 32768 is ~170ms of
/// 48kHz stereo -- far more than the 2048 a scope frame or an FFT window
/// draws, so a slow render or analysis thread still reads intact data
/// (`tap.rs`).
const TAP_SAMPLES: usize = 32_768;

/// How full the ring must be before `open` returns, as a fraction of it.
/// Without this, a caller that plays immediately after loading (which is
/// exactly what the Tauri shell does) can start on a nearly empty ring.
const PREBUFFER_FRACTION: f64 = 0.5;

/// Longest `open` will wait for that prebuffer. Reaching it is not fatal --
/// playback just starts thinner than ideal -- so a pathologically slow decode
/// delays the UI rather than failing it.
const PREBUFFER_TIMEOUT: Duration = Duration::from_secs(5);

/// State shared between the three parties: whoever calls the transport
/// methods, the decoder thread, and the cpal callback.
///
/// All of it is atomic because the callback touches it: a `Mutex` here would
/// put an unbounded wait on the real-time path, which is the bug this module
/// spent Phase 0 having.
struct Shared {
    playing: AtomicBool,
    /// Linear gain as f32 bits -- there is no `AtomicF32`.
    volume: AtomicU32,
    /// Output frames played. The authoritative position: it counts what the
    /// device actually consumed, so it can't drift from what's audible the way
    /// a decoder-side counter would (the decoder runs up to a ring ahead).
    position_frames: AtomicU64,
    /// Seek destination in seconds, as f64 bits. Written before `seek_seq` is
    /// bumped, so anyone who sees the new sequence number sees this too.
    seek_target_seconds: AtomicU64,

    // --- the seek handshake ---
    //
    // A seek has to discard audio that is already in the ring (up to two
    // seconds of it) without discarding any of the audio decoded *after* the
    // seek. Only the consumer can drop items from an SPSC ring, and only the
    // producer knows which side of the seek a sample came from, so neither end
    // can do it alone. These three counters pass the job between them:
    //
    //   1. `seek_seq`          bumped by the caller: "go somewhere else".
    //   2. `decoder_ready_seq` set by the decoder once it has seeked and
    //                          *before* it pushes anything new -- so when the
    //                          callback sees this, everything in the ring is
    //                          old and nothing new is on its way.
    //   3. `drained_seq`       set by the callback once the ring is empty.
    //                          The decoder waits for it before pushing.
    //
    // Without step 2 there is a real race: the decoder can finish a push that
    // started before the seek, landing stale audio *behind* the drain.
    seek_seq: AtomicU64,
    decoder_ready_seq: AtomicU64,
    drained_seq: AtomicU64,

    // --- what is currently audible ---
    //
    // Republished by the callback as each boundary lands (see `Boundary`), so
    // all three describe the track the device is playing rather than the one
    // the decoder has moved on to.
    /// Length of the current track in seconds, `0.0` when unknown.
    duration_seconds: AtomicU64,
    /// Channels the current track has before adaptation, for display.
    src_channels: AtomicU32,
    /// How many gapless hand-offs have reached the device. Not an index into
    /// any playlist -- the engine only ever knows two tracks -- just a counter
    /// the caller can watch to know the music moved on.
    track_index: AtomicU64,

    /// The decoder has produced the last sample of the queue, with nothing
    /// behind it. Distinct from `finished`: audio can still be queued when
    /// this is set.
    eos: AtomicBool,
    /// End of stream *and* the ring is empty -- i.e. the track is really over.
    /// The distinction matters because an empty ring on its own means an
    /// underrun, where the right response is silence and a held position, not
    /// stopping the transport.
    finished: AtomicBool,
    /// Set once the ring has been filled enough to start playing.
    primed: AtomicBool,
    /// Consecutive frames the callback has had to fill with silence because
    /// there was nothing to play (M5). Reset by any callback that fills
    /// completely, so it measures the *current* stall rather than a total.
    starved_frames: AtomicU64,
    /// Tells the decoder thread to exit (set by `Drop`).
    stop: AtomicBool,
    tap: AnalysisTap,
}

impl Shared {
    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    fn duration(&self) -> f64 {
        f64::from_bits(self.duration_seconds.load(Ordering::Relaxed))
    }
}

use crate::stream::buffering_after_frames;

/// A track change riding in the stream, one queue slot per hand-off.
///
/// Boundaries travel alongside the audio rather than being announced out of
/// band because the only honest answer to "which track is playing" is
/// "whichever one the sample the device just consumed came from". The decoder
/// runs up to a ring ahead, so anything it published directly would be up to
/// two seconds early -- the title would change while the previous song was
/// still coming out of the speakers.
#[derive(Clone, Copy)]
struct Boundary {
    /// Total output samples pushed into the audio ring before this track's
    /// first sample. The callback compares its own running consume total
    /// against this; the two stay in step because a seek drain counts what it
    /// throws away.
    at_sample: u64,
    duration_seconds: f64,
    src_channels: u16,
}

/// Samples the callback filters at a time.
///
/// The EQ works in place, and the callback's own output buffer is whatever
/// sample format the device takes (`i16` on some), so filtered audio needs
/// somewhere f32-shaped to live for the length of a block. Allocated once when
/// the stream is built -- the callback may not allocate -- and large enough
/// that a device asking for a big period still crosses it in one pass; a bigger
/// one is simply handled in two.
const EQ_SCRATCH_SAMPLES: usize = 8192;

/// Coefficient sets that can be waiting for the callback at once. A slider drag
/// posts one per pointer event and the callback takes all of them every block,
/// so this only has to outlast one callback period's worth of events.
const MAX_PENDING_EQ: usize = 64;

/// Hand-offs that can be in flight at once. One is the realistic maximum --
/// the caller only ever queues a single track ahead -- so this is slack for
/// the pathological case of several tracks shorter than the ring passing
/// through before the device consumes any of them.
const MAX_PENDING_BOUNDARIES: usize = 16;

/// What the analysis thread hands to whoever is drawing.
///
/// One record under one lock rather than three, because these describe the same
/// instant: a display that took the spectrum from one publication and the
/// loudness from the next would be drawing two moments as one (M5 decision 4).
struct Published {
    /// Spectrum levels, dBFS per band.
    levels: Vec<f32>,
    /// Loudness and phase.
    snapshot: MeterSnapshot,
    /// Tempo, beat phase, onsets and the centroid. Under this lock rather than
    /// one of its own, for the same reason the loudness is: these describe the
    /// same instant, and a display that fetched them separately could draw a
    /// beat from one moment against a spectrum from another (M5 decision 38).
    musical: MusicalSnapshot,
    /// Goniometer points, interleaved side/mid.
    goniometer: Vec<f32>,
}

impl Published {
    fn empty() -> Self {
        Self {
            levels: vec![crate::spectrum::FLOOR_DB; crate::analysis::BAND_COUNT],
            snapshot: MeterSnapshot::silent(),
            musical: MusicalSnapshot::silent(),
            goniometer: Vec::new(),
        }
    }
}

/// Where the caller leaves the track to play after the current one.
///
/// A `Mutex` and not an atomic handshake, deliberately: this is touched by
/// the caller and the decoder thread only. The cpal callback never sees it,
/// so there is no real-time path to keep lock-free here, and a lock is the
/// honest tool for moving an owned `Box` between two threads.
type NextSlot = Arc<Mutex<Option<Box<dyn PlaybackSource>>>>;

pub struct Playback {
    shared: Arc<Shared>,
    /// Output channels, which need not match the source's.
    channels: u16,
    /// Output sample rate -- what `position_frames` counts in.
    sample_rate: u32,
    next: NextSlot,
    /// Where the EQ's sliders currently are, and the way new coefficients reach
    /// the callback.
    ///
    /// The settings are kept rather than derived so the UI can read back what
    /// it set, and the coefficients travel through an SPSC queue for the same
    /// reason M3's track boundaries do: the callback applies them, and the
    /// callback may not take a lock. The `Mutex` is on the *producer*, which
    /// only ever blocks whoever moved a slider.
    eq: Mutex<EqSettings>,
    eq_updates: Mutex<rtrb::Producer<EqCoefficients>>,
    /// Everything the analysis thread publishes. A `Mutex` is fine here where
    /// it would not be in `Shared`: neither end of this is the audio callback.
    meters: Arc<Mutex<Published>>,
    /// The frequency axis those levels are against. Fixed for the session, so
    /// it is computed once rather than sent with every frame.
    band_centres: Vec<f32>,
    decoder: Option<JoinHandle<()>>,
    analysis: Option<JoinHandle<()>>,
    // Kept alive only to keep the stream running -- cpal stops the stream
    // when this is dropped. Never read directly.
    _stream: cpal::Stream,
}

impl Playback {
    /// Opens `source` for streaming playback on the default output device,
    /// starting the stream immediately (paused).
    ///
    /// The device's format is taken as given rather than negotiated, which
    /// looks lazy and isn't -- see `device_config` below for the evidence.
    /// Decoding starts before this returns and runs until the ring is
    /// `PREBUFFER_FRACTION` full, so callers that play straight away don't
    /// open on an empty buffer.
    pub fn open(source: Box<dyn PlaybackSource>) -> Result<Self, PlayerError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(PlayerError::NoOutputDevice)?;

        let supported = device_config(&device)?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let out_channels = config.channels.max(1);

        // Opened here rather than on the decoder thread so a bad file is an
        // error from `open`, not a thread that quietly dies.
        let track = open_track(&*source, out_channels, config.sample_rate)?;

        // Sized in *output* frames, since that is what the ring now carries.
        let ring_samples =
            (config.sample_rate as f64 * RING_SECONDS) as usize * usize::from(out_channels);
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(ring_samples);
        let (boundary_producer, boundary_consumer) =
            rtrb::RingBuffer::<Boundary>::new(MAX_PENDING_BOUNDARIES);
        let (eq_producer, eq_consumer) = rtrb::RingBuffer::<EqCoefficients>::new(MAX_PENDING_EQ);

        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            position_frames: AtomicU64::new(0),
            seek_target_seconds: AtomicU64::new(0.0f64.to_bits()),
            seek_seq: AtomicU64::new(0),
            decoder_ready_seq: AtomicU64::new(0),
            drained_seq: AtomicU64::new(0),
            duration_seconds: AtomicU64::new(track.duration_seconds.to_bits()),
            src_channels: AtomicU32::new(u32::from(track.shape.src_channels)),
            track_index: AtomicU64::new(0),
            starved_frames: AtomicU64::new(0),
            eos: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            primed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            tap: AnalysisTap::new(TAP_SAMPLES),
        });

        let prime_target = (ring_samples as f64 * PREBUFFER_FRACTION) as usize;
        let next: NextSlot = Arc::new(Mutex::new(None));
        let decoder_thread = {
            let shared = Arc::clone(&shared);
            let next = Arc::clone(&next);
            let rate = config.sample_rate;
            std::thread::Builder::new()
                .name("player-decode".to_string())
                .spawn(move || {
                    decoder_loop(
                        Decoding {
                            track,
                            source,
                            out_channels,
                            out_rate: rate,
                        },
                        producer,
                        boundary_producer,
                        shared,
                        next,
                        prime_target,
                    )
                })
                .map_err(|e| PlayerError::OutputStream(format!("decoder thread: {e}")))?
        };

        let stream = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(
                &device,
                &config,
                Arc::clone(&shared),
                consumer,
                boundary_consumer,
                eq_consumer,
                |s| s,
            ),
            SampleFormat::I16 => build_stream::<i16>(
                &device,
                &config,
                Arc::clone(&shared),
                consumer,
                boundary_consumer,
                eq_consumer,
                |s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16,
            ),
            other => return Err(PlayerError::UnsupportedOutputFormat(format!("{other:?}"))),
        }?;
        stream
            .play()
            .map_err(|e| PlayerError::OutputStream(e.to_string()))?;

        // The meters live off the audio thread (M4 decision 2): a meter that
        // overruns its budget must degrade into a stale reading, never into a
        // dropout. This thread only ever *reads* the tap, so nothing it does
        // can block the callback.
        let meters = Arc::new(Mutex::new(Published::empty()));
        let analysis_thread = {
            let shared = Arc::clone(&shared);
            let meters = Arc::clone(&meters);
            let rate = config.sample_rate;
            let channels = config.channels;
            std::thread::Builder::new()
                .name("player-analysis".to_string())
                .spawn(move || analysis_loop(shared, meters, rate, channels))
                .map_err(|e| PlayerError::OutputStream(format!("analysis thread: {e}")))?
        };

        let playback = Self {
            channels: config.channels,
            sample_rate: config.sample_rate,
            next,
            shared,
            eq: Mutex::new(EqSettings::flat()),
            eq_updates: Mutex::new(eq_producer),
            meters,
            band_centres: crate::analysis::band_centres(config.sample_rate),
            decoder: Some(decoder_thread),
            analysis: Some(analysis_thread),
            _stream: stream,
        };
        playback.wait_for_prebuffer();
        Ok(playback)
    }

    /// Queues the track to play when the current one ends, replacing whatever
    /// was queued before.
    ///
    /// The engine holds exactly this much of a playlist: what is playing and
    /// what is next. Which track that is, and what happens after it, stay the
    /// caller's business -- watch `track_index` for the hand-off and queue the
    /// one after it. Queuing early is the point: the decoder picks the track
    /// up when it runs out of the current one, so the seam is filled before
    /// the device reaches it. Queuing after the music has already stopped
    /// works too, and simply starts it.
    pub fn set_next(&self, source: Box<dyn PlaybackSource>) {
        self.store_next(Some(source));
    }

    /// Forgets any queued track, so the current one is the last.
    pub fn clear_next(&self) {
        self.store_next(None);
    }

    fn store_next(&self, source: Option<Box<dyn PlaybackSource>>) {
        match self.next.lock() {
            Ok(mut slot) => *slot = source,
            // Only reachable if the decoder thread panicked holding the lock,
            // in which case there is nothing left to hand the track to.
            Err(e) => eprintln!("next-track slot is poisoned: {e}"),
        }
    }

    /// How many gapless hand-offs have reached the device.
    ///
    /// Increments at the moment the new track's first sample is played, not
    /// when the decoder opened it -- so a caller that redraws on a change
    /// redraws in time with the music rather than up to a ring early.
    pub fn track_index(&self) -> u64 {
        self.shared.track_index.load(Ordering::Acquire)
    }

    /// Blocks until the decoder has filled the ring enough to play from, or
    /// until `PREBUFFER_TIMEOUT`. A timeout is not an error: playback simply
    /// starts with less margin.
    fn wait_for_prebuffer(&self) {
        let deadline = Instant::now() + PREBUFFER_TIMEOUT;
        while !self.shared.primed.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Starts or resumes playback.
    ///
    /// Playing a track that has already run to the end starts it again rather
    /// than resuming at the end, where there is nothing left to play: `finished`
    /// means the queue ran out, and "play" from there can only sensibly mean
    /// "again". The rewind is the engine's job and not the shell's so that both
    /// shells answer the same press the same way -- the web engine has done
    /// exactly this since M2 (`playerEngine.ts`'s `play()`), and only the
    /// desktop one was left parked at the end.
    ///
    /// Note what this deliberately does *not* do: it does not clear `playing`
    /// when a track finishes. That flag stays true precisely so that a track
    /// queued after the music stopped starts on its own, which is how a
    /// playlist extended at the end keeps going. The disagreement it causes --
    /// a transport reporting `playing` with nothing playing -- is resolved
    /// where it is visible, by the UI labelling the button from `playing &&
    /// !finished` (App.tsx).
    pub fn play(&self) {
        if self.is_finished() {
            self.seek(0.0);
        }
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Linear gain, clamped to `0.0..=1.0`. Not a dB curve -- callers that
    /// want a fader taper are expected to shape it themselves, since the right
    /// curve is a UI decision rather than an engine one.
    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        self.shared.volume()
    }

    /// Moves the EQ's sliders (M8).
    ///
    /// The design happens here, on the caller's thread, and only the finished
    /// coefficients cross to the callback -- ten bilinear transforms are not
    /// work for a real-time thread, and doing them here means a slider drag
    /// costs the audio path one queue pop per block whatever the UI does.
    ///
    /// Audible immediately, which is why the filters live in the callback at
    /// all: anything earlier is behind a ring holding two seconds of audio that
    /// has already been filtered with the old curve.
    pub fn set_eq(&self, settings: EqSettings) {
        let settings = settings.clamped();
        let coefficients = EqCoefficients::design(&settings, self.sample_rate);
        match self.eq.lock() {
            Ok(mut held) => *held = settings,
            Err(poisoned) => *poisoned.into_inner() = settings,
        }
        if let Ok(mut producer) = self.eq_updates.lock() {
            // A full queue needs `MAX_PENDING_EQ` slider events inside a single
            // callback period -- 64 of them in ~10ms, which no pointer device
            // or UI produces. If it ever did happen the newest setting would be
            // dropped and the display would disagree with the audio until the
            // next change; the callback drains this queue before every early
            // return precisely so that a paused or seeking player cannot let it
            // fill up.
            let _ = producer.push(coefficients);
        }
    }

    /// Where the EQ's sliders are, as the UI last set them.
    pub fn eq(&self) -> EqSettings {
        match self.eq.lock() {
            Ok(settings) => *settings,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// The EQ's curve for the display: gain in dB at each of
    /// `eq::response_frequencies`, for the *output* rate, since that is the
    /// rate the filters actually run at.
    pub fn eq_response(&self) -> Vec<f32> {
        crate::eq::magnitude_response(&self.eq(), self.sample_rate)
    }

    /// Jumps to `seconds` from the start, clamped to the track.
    ///
    /// Returns as soon as the request is posted; the audio changes a moment
    /// later, once the decoder has moved and the callback has thrown away
    /// what was queued (see `Shared`'s handshake). M1's seek was an index into
    /// an already-decoded buffer and so was instant and exact -- that was a
    /// property of holding the whole track in memory, not something to
    /// preserve. The reported position updates immediately regardless, so the
    /// seek bar doesn't snap back under the user's finger.
    pub fn seek(&self, seconds: f64) {
        let duration = self.shared.duration();
        let target = if duration > 0.0 {
            seconds.clamp(0.0, duration)
        } else {
            seconds.max(0.0)
        };
        self.shared
            .seek_target_seconds
            .store(target.to_bits(), Ordering::Relaxed);
        self.shared
            .position_frames
            .store((target * self.sample_rate as f64) as u64, Ordering::Relaxed);
        self.shared.finished.store(false, Ordering::Relaxed);
        // Release: pairs with the Acquire loads in the decoder thread and the
        // callback, so both see the target above before they act on the bump.
        self.shared.seek_seq.fetch_add(1, Ordering::Release);
    }

    /// Current play position in seconds, measured by what the output device
    /// has consumed.
    pub fn position_seconds(&self) -> f64 {
        self.shared.position_frames.load(Ordering::Relaxed) as f64 / self.sample_rate as f64
    }

    /// Length of the track currently playing, or `0.0` when the container
    /// doesn't state one (a VBR MP3 with no Xing header, say). Phase 0 always
    /// had a real number here because it had decoded the whole file and could
    /// measure it; that is exactly the cost M2 removed, so callers now have to
    /// cope with not knowing.
    pub fn duration_seconds(&self) -> f64 {
        self.shared.duration()
    }

    /// Whether playback has run off the end of the *queue*.
    ///
    /// Specifically end of stream with an empty ring and nothing queued behind
    /// it. An empty ring on its own is an underrun, and reporting that as
    /// "finished" would stop the transport every time the decoder fell behind
    /// for a moment; a track ending with another queued is a hand-off, which
    /// is not an ending at all.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }

    /// Whether playback is waiting for audio that has not arrived (M5).
    ///
    /// The state both engines have always had and neither could say: an empty
    /// ring with the track not ended. Over a network that is the difference
    /// between "this is slow" and "this is broken", and until M5 the two looked
    /// identical from outside -- the transport reported `playing` with the
    /// position held, and nothing said why.
    ///
    /// False at the end of a track, where there is nothing left to wait for.
    pub fn is_buffering(&self) -> bool {
        !self.is_finished()
            && self.shared.starved_frames.load(Ordering::Relaxed)
                >= buffering_after_frames(self.sample_rate)
    }

    pub fn output_channels(&self) -> u16 {
        self.channels
    }

    pub fn output_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channels the currently playing track has, before adaptation to the
    /// device. For display only -- `scope_window` is in the *output* layout,
    /// so use `output_channels` to de-interleave it.
    pub fn source_channels(&self) -> u16 {
        self.shared.src_channels.load(Ordering::Relaxed) as u16
    }

    /// The most recently played `n` interleaved samples, for the scope render
    /// loop to draw a live trace from. Read from the history the callback
    /// keeps (`tap.rs`) rather than from a decoded buffer -- with streaming
    /// there is no longer any such buffer to look back into.
    pub fn scope_window(&self, n: usize) -> Vec<f32> {
        self.shared.tap.window(n)
    }

    /// The latest spectrum, one dBFS level per display band, as of whenever the
    /// analysis thread last ran (~60Hz). Reading it never waits on audio.
    pub fn spectrum(&self) -> Vec<f32> {
        self.published(|published| published.levels.clone())
    }

    /// Loudness and phase as of the same instant as each other (M5 decision 4).
    pub fn meters(&self) -> MeterSnapshot {
        self.published(|published| published.snapshot)
    }

    /// Tempo, beat phase, onsets and the spectral centroid, as of the same
    /// instant as the meters above (Phase 3 M5).
    pub fn musical(&self) -> MusicalSnapshot {
        self.published(|published| published.musical)
    }

    /// The goniometer's points, interleaved side/mid, for the window the
    /// analysis thread last read.
    pub fn goniometer(&self) -> Vec<f32> {
        self.published(|published| published.goniometer.clone())
    }

    /// Reads the analysis thread's publication.
    ///
    /// A poisoned lock is only reachable if that thread panicked mid-publish,
    /// and a display holding its last frame beats propagating a panic to a
    /// caller who only wanted to draw one.
    fn published<T>(&self, read: impl FnOnce(&Published) -> T) -> T {
        match self.meters.lock() {
            Ok(published) => read(&published),
            Err(poisoned) => read(&poisoned.into_inner()),
        }
    }

    /// Centre frequency of each band in `spectrum`, for the axis. Constant for
    /// the life of this `Playback`.
    pub fn spectrum_band_centres(&self) -> &[f32] {
        &self.band_centres
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // Both threads park in sleeps rather than blocking, so setting this is
        // enough to bring them down; joining keeps them from outliving the ring
        // and the tap they work on.
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.analysis.take() {
            let _ = handle.join();
        }
    }
}

/// Drains the tap into the meters and publishes what they read.
///
/// Runs at roughly display rate rather than as fast as it can: the spectrum is
/// a display, and a meter that consumes a core to redraw the same frame twice
/// is a worse player, not a better one. Everything it drains is handed over,
/// though, not just the newest window -- M5's integrated loudness has to see
/// every sample it claims to have measured, and this is where it reads them.
fn analysis_loop(
    shared: Arc<Shared>,
    published: Arc<Mutex<Published>>,
    sample_rate: u32,
    channels: u16,
) {
    const INTERVAL: Duration = Duration::from_millis(16);

    let mut meters = Meters::new(sample_rate, channels);
    let mut cursor = TapCursor::new(&shared.tap);
    let mut drained = Vec::new();
    let mut last = Instant::now();
    let mut heard_track = shared.track_index.load(Ordering::Acquire);

    while !shared.stop.load(Ordering::Relaxed) {
        drained.clear();
        let read = shared.tap.read(&mut cursor, &mut drained);

        // A gapless hand-off is continuous audio and so no discontinuity to the
        // tap -- which is exactly why it has to be one here. The samples either
        // side of the seam belong to different tracks, and an integrated
        // loudness spanning both would be a number for a record nobody made
        // (M5 decision 2). This is the one break only the transport can see.
        let track = shared.track_index.load(Ordering::Acquire);
        let handed_over = track != heard_track;
        heard_track = track;

        meters.ingest(&drained, read.discontinuity || handed_over);

        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32();
        last = now;
        meters.update(dt);

        if let Ok(mut published) = published.lock() {
            published.levels.clear();
            published.levels.extend_from_slice(meters.levels());
            published.snapshot = meters.snapshot();
            published.musical = meters.musical();
            published.goniometer = meters.goniometer(GONIOMETER_POINTS);
        }

        std::thread::sleep(INTERVAL);
    }
}

/// One track's worth of decoding machinery, plus the device shape it is being
/// converted to. Rebuilt from scratch at every hand-off, since a new track can
/// arrive at any rate and any channel count.
struct Track {
    decoder: StreamingDecoder,
    resampler: StreamingResampler,
    shape: ChannelShape,
    /// `0.0` when the container states no length.
    duration_seconds: f64,
}

/// Opens `source` and builds everything needed to play it on a device of
/// `out_channels` at `out_rate`.
///
/// The device dictates the rate (see `device_config`), so the audio has to
/// move to meet it. Without that, a 44.1kHz file on a 96kHz device plays at
/// 2.177x -- found live on Windows, 2026-08-15.
fn open_track(
    source: &dyn PlaybackSource,
    out_channels: u16,
    out_rate: u32,
) -> Result<Track, PlayerError> {
    let decoder = StreamingDecoder::open(source)?;
    let src_channels = decoder.channels().max(1);
    if usize::from(src_channels) > MAX_CHANNELS {
        return Err(PlayerError::UnsupportedOutputFormat(format!(
            "{src_channels} source channels (this player handles at most {MAX_CHANNELS})"
        )));
    }
    let resampler = StreamingResampler::new(src_channels, decoder.sample_rate(), out_rate)?;
    Ok(Track {
        duration_seconds: decoder.duration_seconds().unwrap_or(0.0),
        decoder,
        resampler,
        shape: ChannelShape {
            src_channels,
            out_channels,
        },
    })
}

/// What the decoder thread starts with: the first track, the source it came
/// from (kept so a seek can reopen it -- see the rewind in `decoder_loop`),
/// and the device shape every track has to be converted to.
struct Decoding {
    track: Track,
    source: Box<dyn PlaybackSource>,
    out_channels: u16,
    out_rate: u32,
}

/// Decode -> resample -> channel adaptation -> ring, across as many tracks as
/// the caller queues, until `Playback` is dropped.
///
/// Runs on its own thread and owns the only handles to the decoders and the
/// producer ends of both rings, so the only lock here is the one on the
/// next-track slot -- which the callback never touches.
///
/// # The seam
///
/// When a track runs out, the outgoing resampler's tail is flushed into the
/// ring first and only then is the next track opened, so the last partial
/// window of the old track and the first sample of the new one are adjacent
/// in the ring with nothing between them. The callback cannot tell a track
/// change from any other block of audio, which is the whole point -- there is
/// no seam to hear because there is no seam in the buffer.
fn decoder_loop(
    start: Decoding,
    mut producer: rtrb::Producer<f32>,
    mut boundaries: rtrb::Producer<Boundary>,
    shared: Arc<Shared>,
    next_slot: NextSlot,
    prime_target: usize,
) {
    let Decoding {
        mut track,
        mut source,
        out_channels,
        out_rate,
    } = start;

    let mut seq = 0u64;
    // Resampler output, still in the source's channel layout.
    let mut resampled: Vec<f32> = Vec::new();
    // Output-shaped audio waiting for room in the ring.
    let mut carry: Vec<f32> = Vec::new();
    let mut carry_pos = 0usize;
    // The current track has produced its last sample and is waiting for
    // something to follow it. Replaces M2's `source_done`: with a queue,
    // running out of track is a question ("what's next?") rather than an
    // ending.
    let mut awaiting_handoff = false;
    let mut pushed_total = 0usize;

    // Which track the decoder is on, against `shared.track_index`, which is
    // the one the listener is on. They differ for as long as a hand-off sits
    // undelivered in the ring.
    let mut decoding_index = 0u64;
    // The track still being heard after the decoder has moved past it. Held
    // only until its successor's boundary lands, and only so a seek arriving
    // in that window can rewind to it.
    let mut previous: Option<(u64, Box<dyn PlaybackSource>)> = None;

    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }

        // The listener has caught up; the old track can go.
        if previous.is_some() && shared.track_index.load(Ordering::Acquire) >= decoding_index {
            previous = None;
        }

        let requested = shared.seek_seq.load(Ordering::Acquire);
        if requested != seq {
            // The decoder can be a whole ring ahead, so a seek can arrive when
            // it has already moved on to the next track while the listener is
            // still hearing the previous one. Seeking the track being decoded
            // would scrub a song the user cannot hear yet -- and the seek bar
            // they dragged belongs to the one they can. Rewind, and hand the
            // upcoming track back to the queue so gapless still works after
            // the seek lands.
            if let Some((prev_index, prev_source)) = previous.take() {
                let upcoming = std::mem::replace(&mut source, prev_source);
                match next_slot.lock() {
                    Ok(mut slot) => *slot = Some(upcoming),
                    Err(e) => eprintln!("next-track slot is poisoned: {e}"),
                }
                decoding_index = prev_index;
                match open_track(&*source, out_channels, out_rate) {
                    Ok(reopened) => track = reopened,
                    // Nothing safe to fall back to: the track being decoded is
                    // the wrong one and its predecessor won't reopen. Seek what
                    // we have rather than going silent.
                    Err(e) => eprintln!("rewinding to the track being heard failed: {e}"),
                }
            }

            let target = f64::from_bits(shared.seek_target_seconds.load(Ordering::Relaxed));
            if let Err(e) = track.decoder.seek(target) {
                // A container that can't seek shouldn't kill playback; stay
                // where we are and keep going.
                eprintln!("seek to {target}s failed: {e}");
            }
            // The samples either side of a seek are unrelated, so the filter's
            // history has to go with them.
            track.resampler.reset();
            carry.clear();
            carry_pos = 0;
            awaiting_handoff = false;
            shared.eos.store(false, Ordering::Release);

            // Step 2 of the handshake (see `Shared`): from here until the
            // callback acknowledges, nothing is pushed, so everything in the
            // ring is pre-seek and safe to throw away.
            seq = requested;
            shared.decoder_ready_seq.store(seq, Ordering::Release);
            while shared.drained_seq.load(Ordering::Acquire) != seq {
                if shared.stop.load(Ordering::Relaxed) {
                    return;
                }
                // A newer seek supersedes this one; go round again rather than
                // waiting for an acknowledgement that will never come.
                if shared.seek_seq.load(Ordering::Acquire) != seq {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            continue;
        }

        // --- the seam ---
        //
        // The outgoing track's tail is already in `carry` and pushed by the
        // time we get here, so `pushed_total` is exactly the sample index the
        // incoming track starts at. Opening it now rather than earlier is what
        // keeps the two adjacent in the ring.
        if awaiting_handoff && carry_pos >= carry.len() {
            let queued = match next_slot.lock() {
                Ok(mut slot) => slot.take(),
                Err(e) => {
                    eprintln!("next-track slot is poisoned: {e}");
                    None
                }
            };
            if let Some(queued) = queued {
                match open_track(&*queued, out_channels, out_rate) {
                    Ok(opened) => {
                        let boundary = Boundary {
                            at_sample: pushed_total as u64,
                            duration_seconds: opened.duration_seconds,
                            src_channels: opened.shape.src_channels,
                        };
                        if boundaries.push(boundary).is_err() {
                            // Only reachable with more tracks in flight than
                            // the ring can hold audio for. Publishing here
                            // instead means the change is reported early
                            // rather than lost -- the audio is unaffected.
                            eprintln!(
                                "more than {MAX_PENDING_BOUNDARIES} track changes in flight; \
                                 reporting this one early"
                            );
                            publish_track(&shared, &boundary);
                        }
                        previous = Some((decoding_index, std::mem::replace(&mut source, queued)));
                        decoding_index += 1;
                        track = opened;
                        // Order matters: clearing `eos` first stops the
                        // callback latching `finished` in between.
                        shared.eos.store(false, Ordering::Release);
                        shared.finished.store(false, Ordering::Relaxed);
                        awaiting_handoff = false;
                    }
                    Err(e) => {
                        // A bad file ends the queue rather than stalling it.
                        // The caller sees `is_finished` and can queue another.
                        eprintln!("opening the queued track failed: {e}");
                    }
                }
            }
        }

        // Out of track with nothing queued: park until something is, or until
        // a seek or shutdown. Queuing late still works -- the branch above runs
        // every time round.
        if awaiting_handoff && carry_pos >= carry.len() {
            shared.eos.store(true, Ordering::Release);
            shared.primed.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        if producer.slots() == 0 {
            // The ring is full, which is the steady state while playing and
            // permanent while paused. Polling rather than parking on a condvar
            // keeps the callback free of any signalling work.
            shared.primed.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        if carry_pos >= carry.len() {
            carry.clear();
            carry_pos = 0;
            resampled.clear();
            match track.decoder.next_block() {
                Ok(Some(block)) => {
                    if let Err(e) = track.resampler.process(block, &mut resampled) {
                        eprintln!("resample failed: {e}");
                        awaiting_handoff = true;
                    }
                }
                Ok(None) => {
                    // The tail the resampler is still holding belongs to this
                    // track and has to reach the ring before the next track's
                    // first sample does, or the seam swallows it.
                    if let Err(e) = track.resampler.finish(&mut resampled) {
                        eprintln!("resampler flush failed: {e}");
                    }
                    awaiting_handoff = true;
                }
                Err(e) => {
                    // Treat a broken track as a finished one, so a corrupt
                    // file in a playlist moves on instead of stopping it.
                    eprintln!("decode failed: {e}");
                    awaiting_handoff = true;
                }
            }
            adapt_channels(&resampled, &track.shape, &mut carry);
        }

        if carry_pos < carry.len() {
            let (pushed, _remainder) = producer.push_partial_slice(&carry[carry_pos..]);
            carry_pos += pushed.len();
            pushed_total += pushed.len();
            if pushed_total >= prime_target {
                shared.primed.store(true, Ordering::Release);
            }
        }
    }
}

/// Makes a boundary's track the one the world sees.
///
/// Called from the cpal callback, so: no allocation, no locks, and
/// `track_index` last with a `Release` so a caller that notices the counter
/// move sees the duration and channel count that go with it.
fn publish_track(shared: &Shared, boundary: &Boundary) {
    shared
        .duration_seconds
        .store(boundary.duration_seconds.to_bits(), Ordering::Relaxed);
    shared
        .src_channels
        .store(u32::from(boundary.src_channels), Ordering::Relaxed);
    // A hand-off is the opposite of an ending, whatever the callback may have
    // latched while the ring ran dry across the seam.
    shared.finished.store(false, Ordering::Relaxed);
    shared.track_index.fetch_add(1, Ordering::Release);
}

/// The device's own mix format -- the only config that can actually be
/// opened, so there is nothing to choose between.
///
/// This *used* to search `supported_output_configs()` for an entry matching
/// the file's rate and channel count. That is the obvious thing to do and
/// it is wrong on Windows, which cost a full desktop test to find
/// (2026-08-15). Reading cpal 0.18.1's WASAPI backend rather than guessing:
///
/// - `supported_output_configs()` skips its own `is_format_supported` check
///   for output devices (`device.rs`, "Output streams use
///   AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM so Initialize accepts any format"),
///   so it advertises ~112 configs per device spanning every common rate.
/// - `build_output_stream_raw()` then *does* run that check, and its
///   `is_format_supported` returns true only for `S_OK` -- i.e. only for a
///   format the engine takes natively, with no conversion. Anything else is
///   rejected with "Stream configuration is not supported in shared mode"
///   before the AUTOCONVERTPCM flag is ever reached.
///
/// So cpal advertises configs it will refuse to build, and in practice only
/// the mix format survives. Asking for the file's rate got a hard failure on
/// 6 of this machine's 9 output devices; the 3 that "worked" were the ones
/// where the old channel filter happened to miss, falling through to this
/// same mix format -- at 96kHz, hence the 2.177x speed-up. Both symptoms,
/// one cause.
///
/// Taking the mix format and resampling to it (see `open`) makes every
/// device behave the same way. If cpal fixes the advertise/accept mismatch,
/// requesting the file's native rate becomes worth revisiting -- it would
/// let the OS resampler do this job, or let bit-perfect output happen when
/// the rates already agree.
fn device_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, PlayerError> {
    let config = device
        .default_output_config()
        .map_err(|e| PlayerError::OutputStream(e.to_string()))?;

    // `build_stream` below can only feed F32 and I16. Since no *other*
    // config would build either, a mix format outside those two is a real
    // dead end rather than something to search around -- say so plainly.
    match config.sample_format() {
        SampleFormat::F32 | SampleFormat::I16 => Ok(config),
        other => Err(PlayerError::UnsupportedOutputFormat(format!(
            "{other:?} (the device's mix format; cpal will not open this device in any other format)"
        ))),
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: Arc<Shared>,
    mut consumer: rtrb::Consumer<f32>,
    mut boundaries: rtrb::Consumer<Boundary>,
    mut eq_updates: rtrb::Consumer<EqCoefficients>,
    convert: fn(f32) -> T,
) -> Result<cpal::Stream, PlayerError>
where
    T: cpal::SizedSample + Send + 'static,
{
    let out_channels = config.channels.max(1) as usize;
    let sample_rate = config.sample_rate as f64;
    // The EQ and the buffer it works in, both built here rather than in the
    // callback for the obvious reason.
    let mut eq = EqProcessor::new(config.sample_rate, config.channels);
    let mut scratch = vec![0.0f32; EQ_SCRATCH_SAMPLES];
    // Output samples taken off the ring since the stream opened, counting the
    // ones a seek threw away. Only this closure touches it, so it needs no
    // atomic; it exists to be compared against `Boundary::at_sample`, which
    // the decoder measures the same way from the other end.
    let mut consumed_total = 0u64;

    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                // Real-time thread. Everything below is lock-free and
                // allocation-free by construction -- no `Vec`, no `Mutex`, no
                // logging.

                // Before any of the early returns below: a listener who moves a
                // slider while paused, or mid-seek, must still have the EQ they
                // asked for when the music comes back -- and a queue nothing
                // drains is a queue that eventually fills. Taking all of them
                // and keeping the last is what makes a drag land on its final
                // position rather than on whichever value a block happened to
                // catch.
                let mut latest = None;
                while let Ok(coefficients) = eq_updates.pop() {
                    latest = Some(coefficients);
                }
                if let Some(coefficients) = latest {
                    eq.set(coefficients);
                }

                let requested = shared.seek_seq.load(Ordering::Acquire);
                if requested != shared.drained_seq.load(Ordering::Relaxed) {
                    // Step 3 of the handshake (see `Shared`): once the decoder
                    // says it has moved, everything queued is from before the
                    // seek. Drop all of it.
                    if shared.decoder_ready_seq.load(Ordering::Acquire) == requested {
                        let stale = consumer.slots();
                        if stale > 0 {
                            if let Ok(chunk) = consumer.read_chunk(stale) {
                                chunk.commit_all();
                            }
                        }
                        // Count what was discarded, or every boundary after
                        // this one lands `stale` samples late.
                        consumed_total += stale as u64;
                        // Any hand-off queued behind the drain described audio
                        // that no longer exists. The decoder is blocked until
                        // `drained_seq` below, so nothing new can arrive here.
                        while boundaries.pop().is_ok() {}
                        let target =
                            f64::from_bits(shared.seek_target_seconds.load(Ordering::Relaxed));
                        shared
                            .position_frames
                            .store((target * sample_rate) as u64, Ordering::Relaxed);
                        shared.finished.store(false, Ordering::Relaxed);
                        // Otherwise the next scope frame is spliced together
                        // from both sides of the seek.
                        shared.tap.clear();
                        // The filters' memory belongs to where the track was,
                        // not where it is going: carrying it over rings the new
                        // position with the old one's tail.
                        eq.reset();
                        shared.drained_seq.store(requested, Ordering::Release);
                    }
                    // Either way, play nothing this block: what's queued is
                    // audio from where we no longer are.
                    silence(data, convert);
                    return;
                }

                if !shared.playing.load(Ordering::Relaxed) {
                    silence(data, convert);
                    // A pause is not a stall. Without this, pausing during an
                    // underrun and resuming later would report buffering off a
                    // count that stopped being true when the listener pressed
                    // the button.
                    shared.starved_frames.store(0, Ordering::Relaxed);
                    // Paused audio is still audio the device received, and the
                    // tap holds what the device received. Without this the
                    // meters freeze on the last frame before the pause and hold
                    // it indefinitely -- a spectrum standing still under a
                    // stopped transport reads as a hung analyser, which is the
                    // same class of lie as the dropout `push_silence` exists to
                    // stop hiding (found by the M4 smoke check, 2026-08-16).
                    shared.tap.push_silence(data.len());
                    return;
                }

                let volume = shared.volume();
                let available = consumer.slots();
                // The ring is already in the device's shape, so this is a copy
                // and a gain -- whole output frames only, since a partial frame
                // at the end would misalign every channel after it.
                let usable = data.len().min(available - available % out_channels);
                let mut filled = 0;

                if usable > 0 {
                    if let Ok(chunk) = consumer.read_chunk(usable) {
                        let (first, second) = chunk.as_slices();
                        let split = first.len().min(usable);
                        // Two slices because a ring read that wraps arrives in
                        // two pieces, and the split need not land on a frame
                        // boundary -- which is why `EqProcessor` carries its
                        // own channel phase rather than assuming each call
                        // starts on the left.
                        play_samples(
                            &first[..split],
                            data,
                            0,
                            &mut scratch,
                            &mut eq,
                            &shared.tap,
                            volume,
                            convert,
                        );
                        if usable > split {
                            play_samples(
                                &second[..usable - split],
                                data,
                                split,
                                &mut scratch,
                                &mut eq,
                                &shared.tap,
                                volume,
                                convert,
                            );
                        }

                        filled = usable;
                        chunk.commit(usable);
                    }
                }

                // Whatever wasn't filled: end of track, or the decoder falling
                // behind. Silence either way, and the position doesn't move --
                // an underrun is a stall, not playback.
                for out in data.iter_mut().skip(filled) {
                    *out = convert(0.0);
                }
                if filled < data.len() {
                    // The scope draws what the device received, and during an
                    // underrun that is this silence. Leaving it out froze the
                    // trace on the last good audio -- hiding a dropout at the
                    // exact moment the scope would have shown it.
                    shared.tap.push_silence(data.len() - filled);
                    // And the same silence is what "buffering" is made of (M5).
                    // Counted in frames rather than callbacks so the threshold
                    // is a duration rather than a number that means something
                    // different at every block size.
                    shared.starved_frames.fetch_add(
                        ((data.len() - filled) / out_channels) as u64,
                        Ordering::Relaxed,
                    );
                } else {
                    // A block that filled completely ends the stall, whatever
                    // came before it.
                    shared.starved_frames.store(0, Ordering::Relaxed);
                }
                if filled > 0 {
                    consumed_total += filled as u64;
                    shared
                        .position_frames
                        .fetch_add((filled / out_channels) as u64, Ordering::Relaxed);
                }

                // --- track changes land here ---
                //
                // Not when the decoder opened the file: this is the moment the
                // new track's first sample reaches the device. Noticing it a
                // block late costs nothing, because the position is derived
                // from how far past the boundary we are rather than from when
                // we looked. A loop, since a track shorter than one callback
                // block would be crossed entirely within it.
                loop {
                    let boundary = match boundaries.peek() {
                        Ok(boundary) if boundary.at_sample <= consumed_total => *boundary,
                        _ => break,
                    };
                    let _ = boundaries.pop();
                    shared.position_frames.store(
                        (consumed_total - boundary.at_sample) / out_channels as u64,
                        Ordering::Relaxed,
                    );
                    publish_track(&shared, &boundary);
                }

                // Finished only when the decoder has run out of queue *and*
                // nothing is left to play. Either alone is an ordinary
                // mid-track state.
                if shared.eos.load(Ordering::Acquire) && consumer.slots() < out_channels {
                    shared.finished.store(true, Ordering::Relaxed);
                }
            },
            |err| eprintln!("output stream error: {err}"),
            None,
        )
        .map_err(|e| PlayerError::OutputStream(e.to_string()))
}

fn silence<T>(data: &mut [T], convert: fn(f32) -> T)
where
    T: cpal::SizedSample,
{
    for sample in data.iter_mut() {
        *sample = convert(0.0);
    }
}

/// Plays `src` -- audio straight off the ring -- into `data[at..]`: EQ, then
/// the tap, then the volume.
///
/// The order is the whole of M8 decision 2. The EQ is part of what the player
/// produces, so the tap holds it and every meter measures it; the volume is
/// monitoring level, so it comes after the tap and changes nothing anyone
/// measures. Called from the audio callback, so it works in a buffer the stream
/// allocated at build time and in slices of it, never in one it makes.
#[allow(clippy::too_many_arguments)]
fn play_samples<T>(
    src: &[f32],
    data: &mut [T],
    at: usize,
    scratch: &mut [f32],
    eq: &mut EqProcessor,
    tap: &AnalysisTap,
    volume: f32,
    convert: fn(f32) -> T,
) where
    T: cpal::SizedSample,
{
    let mut done = 0;
    while done < src.len() {
        let n = (src.len() - done).min(scratch.len());
        let block = &mut scratch[..n];
        block.copy_from_slice(&src[done..done + n]);
        eq.process(block);
        for (out, sample) in data[at + done..at + done + n].iter_mut().zip(block.iter()) {
            *out = convert(sample * volume);
        }
        // The scope shows what was played, which since M3 means the output's
        // channel layout rather than the source's -- and since M8, the audio as
        // the EQ left it.
        tap.push(block);
        done += n;
    }
}
