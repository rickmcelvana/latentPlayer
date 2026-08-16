//! Native (cpal) playback. Not built on wasm32 -- cpal has no wasm target;
//! the web build drives output through the Web Audio API / AudioWorklet
//! instead (see `player-rt-wasm`, and PLAN.md §5's "Web audio I/O" note).
//!
//! # Shape (Phase 1 M2)
//!
//! ```text
//!   decoder thread                                ring (rtrb)   cpal callback
//!   StreamingDecoder -> resampler -> channels -> [ ~2s ] -> output + ScopeRing
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
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use crate::error::PlayerError;
use crate::resample::StreamingResampler;
use crate::scope::ScopeRing;
use crate::source::PlaybackSource;
use crate::stream::StreamingDecoder;

/// Upper bound on source channels, rejected at `open` rather than discovered
/// in the callback. Well past anything Symphonia decodes (22.2 surround is
/// 24).
const MAX_CHANNELS: usize = 32;

/// How much audio the ring holds, in seconds of output. Big enough that a
/// decoder hiccup (a slow read, a page fault, the OS descheduling the thread)
/// doesn't reach the speakers; small enough that a seek doesn't have to throw
/// much away and memory stays trivial (~768KB at 48kHz stereo).
const RING_SECONDS: f64 = 2.0;

/// Samples of playback history kept for the scope. 32768 is ~170ms of 48kHz
/// stereo -- far more than the 2048 a frame draws, so a slow render thread
/// still reads intact data.
const SCOPE_RING_SAMPLES: usize = 32_768;

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

    /// The decoder has produced the last sample of the track. Distinct from
    /// `finished`: audio can still be queued when this is set.
    eos: AtomicBool,
    /// End of stream *and* the ring is empty -- i.e. the track is really over.
    /// The distinction matters because an empty ring on its own means an
    /// underrun, where the right response is silence and a held position, not
    /// stopping the transport.
    finished: AtomicBool,
    /// Set once the ring has been filled enough to start playing.
    primed: AtomicBool,
    /// Tells the decoder thread to exit (set by `Drop`).
    stop: AtomicBool,
    scope: ScopeRing,
}

impl Shared {
    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
}

pub struct Playback {
    shared: Arc<Shared>,
    /// Output channels, which need not match the source's.
    channels: u16,
    src_channels: u16,
    /// Output sample rate -- what `position_frames` counts in.
    sample_rate: u32,
    /// `None` when the container doesn't state a length.
    duration: Option<f64>,
    decoder: Option<JoinHandle<()>>,
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
    pub fn open(source: &dyn PlaybackSource) -> Result<Self, PlayerError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(PlayerError::NoOutputDevice)?;

        let supported = device_config(&device)?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();

        // Opened here rather than on the decoder thread so a bad file is an
        // error from `open`, not a thread that quietly dies.
        let decoder = StreamingDecoder::open(source)?;
        let src_channels = decoder.channels().max(1);
        if src_channels as usize > MAX_CHANNELS {
            return Err(PlayerError::UnsupportedOutputFormat(format!(
                "{src_channels} source channels (this player handles at most {MAX_CHANNELS})"
            )));
        }
        let duration = decoder.duration_seconds();

        // The device dictates the rate (again: see `device_config`), so the
        // audio has to move to meet it. Without this, a 44.1kHz file on a
        // 96kHz device plays at 2.177x -- found live on Windows, 2026-08-15.
        let resampler =
            StreamingResampler::new(src_channels, decoder.sample_rate(), config.sample_rate)?;

        // Sized in *output* frames, since that is what the ring now carries.
        let ring_samples =
            (config.sample_rate as f64 * RING_SECONDS) as usize * config.channels.max(1) as usize;
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(ring_samples);

        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            position_frames: AtomicU64::new(0),
            seek_target_seconds: AtomicU64::new(0.0f64.to_bits()),
            seek_seq: AtomicU64::new(0),
            decoder_ready_seq: AtomicU64::new(0),
            drained_seq: AtomicU64::new(0),
            eos: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            primed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            scope: ScopeRing::new(SCOPE_RING_SAMPLES),
        });

        let prime_target = (ring_samples as f64 * PREBUFFER_FRACTION) as usize;
        let out_channels = config.channels.max(1);
        let decoder_thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("player-decode".to_string())
                .spawn(move || {
                    decoder_loop(
                        decoder,
                        resampler,
                        producer,
                        shared,
                        Shape {
                            src_channels,
                            out_channels,
                        },
                        prime_target,
                    )
                })
                .map_err(|e| PlayerError::OutputStream(format!("decoder thread: {e}")))?
        };

        let stream = match sample_format {
            SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, Arc::clone(&shared), consumer, |s| s)
            }
            SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, Arc::clone(&shared), consumer, |s| {
                    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                })
            }
            other => return Err(PlayerError::UnsupportedOutputFormat(format!("{other:?}"))),
        }?;
        stream
            .play()
            .map_err(|e| PlayerError::OutputStream(e.to_string()))?;

        let playback = Self {
            channels: config.channels,
            src_channels,
            sample_rate: config.sample_rate,
            duration,
            shared,
            decoder: Some(decoder_thread),
            _stream: stream,
        };
        playback.wait_for_prebuffer();
        Ok(playback)
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

    pub fn play(&self) {
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
        let target = match self.duration {
            Some(duration) => seconds.clamp(0.0, duration),
            None => seconds.max(0.0),
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

    /// Total length in seconds, or `0.0` when the container doesn't state one
    /// (a VBR MP3 with no Xing header, say). Phase 0 always had a real number
    /// here because it had decoded the whole file and could measure it; that
    /// is exactly the cost this milestone removed, so callers now have to cope
    /// with not knowing.
    pub fn duration_seconds(&self) -> f64 {
        self.duration.unwrap_or(0.0)
    }

    /// Whether playback has run off the end of the track.
    ///
    /// Specifically end of stream *with an empty ring* -- an empty ring on its
    /// own is an underrun, and reporting that as "finished" would stop the
    /// transport every time the decoder fell behind for a moment.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }

    pub fn output_channels(&self) -> u16 {
        self.channels
    }

    pub fn output_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channels the track itself has, before adaptation to the device. For
    /// display only -- `scope_window` is in the *output* layout, so use
    /// `output_channels` to de-interleave it.
    pub fn source_channels(&self) -> u16 {
        self.src_channels
    }

    /// The most recently played `n` interleaved samples, for the scope render
    /// loop to draw a live trace from. Read from the history the callback
    /// keeps (`scope.rs`) rather than from a decoded buffer -- with streaming
    /// there is no longer any such buffer to look back into.
    pub fn scope_window(&self, n: usize) -> Vec<f32> {
        self.shared.scope.window(n)
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // The decoder thread parks in sleeps rather than blocking, so setting
        // this is enough to bring it down; joining keeps it from outliving the
        // ring it writes into.
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
    }
}

/// How wide a frame is either side of the channel adaptation.
struct Shape {
    src_channels: u16,
    out_channels: u16,
}

/// Rewrites `src_channels`-interleaved frames as `out_channels`-interleaved
/// ones, appending to `dst`.
///
/// Source channels are reused cyclically: mono to stereo duplicates the one
/// channel, and a surround track on a stereo device keeps its first pair.
/// Crude, and deliberately so -- a real downmix has coefficients and a
/// channel map behind it, which is its own job and not one the queue needs.
///
/// Any trailing partial frame is dropped. The resampler emits whole frames,
/// so in practice there is never one.
fn adapt_channels(src: &[f32], shape: &Shape, dst: &mut Vec<f32>) {
    let src_channels = shape.src_channels.max(1) as usize;
    let out_channels = shape.out_channels.max(1) as usize;
    if src_channels == out_channels {
        dst.extend_from_slice(src);
        return;
    }
    let frames = src.len() / src_channels;
    dst.reserve(frames * out_channels);
    for frame in 0..frames {
        let base = frame * src_channels;
        for ch in 0..out_channels {
            dst.push(src[base + ch % src_channels]);
        }
    }
}

/// Decode -> resample -> channel adaptation -> ring, until the track ends or
/// `Playback` is dropped.
///
/// Runs on its own thread and owns the only handles to the decoder and the
/// producer end of the ring, so nothing here needs a lock.
fn decoder_loop(
    mut decoder: StreamingDecoder,
    mut resampler: StreamingResampler,
    mut producer: rtrb::Producer<f32>,
    shared: Arc<Shared>,
    shape: Shape,
    prime_target: usize,
) {
    let mut seq = 0u64;
    // Resampler output, still in the source's channel layout.
    let mut resampled: Vec<f32> = Vec::new();
    // Output-shaped audio waiting for room in the ring.
    let mut carry: Vec<f32> = Vec::new();
    let mut carry_pos = 0usize;
    let mut source_done = false;
    let mut pushed_total = 0usize;

    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }

        let requested = shared.seek_seq.load(Ordering::Acquire);
        if requested != seq {
            let target = f64::from_bits(shared.seek_target_seconds.load(Ordering::Relaxed));
            if let Err(e) = decoder.seek(target) {
                // A container that can't seek shouldn't kill playback; stay
                // where we are and keep going.
                eprintln!("seek to {target}s failed: {e}");
            }
            // The samples either side of a seek are unrelated, so the filter's
            // history has to go with them.
            resampler.reset();
            carry.clear();
            carry_pos = 0;
            source_done = false;
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

        // Everything decoded and handed over: park until a seek or shutdown.
        if source_done && carry_pos >= carry.len() {
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
            match decoder.next_block() {
                Ok(Some(block)) => {
                    if let Err(e) = resampler.process(block, &mut resampled) {
                        eprintln!("resample failed: {e}");
                        source_done = true;
                    }
                }
                Ok(None) => {
                    if let Err(e) = resampler.finish(&mut resampled) {
                        eprintln!("resampler flush failed: {e}");
                    }
                    source_done = true;
                }
                Err(e) => {
                    eprintln!("decode failed: {e}");
                    source_done = true;
                }
            }
            adapt_channels(&resampled, &shape, &mut carry);
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
    convert: fn(f32) -> T,
) -> Result<cpal::Stream, PlayerError>
where
    T: cpal::SizedSample + Send + 'static,
{
    let out_channels = config.channels.max(1) as usize;
    let sample_rate = config.sample_rate as f64;

    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                // Real-time thread. Everything below is lock-free and
                // allocation-free by construction -- no `Vec`, no `Mutex`, no
                // logging.
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
                        let target =
                            f64::from_bits(shared.seek_target_seconds.load(Ordering::Relaxed));
                        shared
                            .position_frames
                            .store((target * sample_rate) as u64, Ordering::Relaxed);
                        shared.finished.store(false, Ordering::Relaxed);
                        // Otherwise the next scope frame is spliced together
                        // from both sides of the seek.
                        shared.scope.clear();
                        shared.drained_seq.store(requested, Ordering::Release);
                    }
                    // Either way, play nothing this block: what's queued is
                    // audio from where we no longer are.
                    silence(data, convert);
                    return;
                }

                if !shared.playing.load(Ordering::Relaxed) {
                    silence(data, convert);
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
                        for (out, sample) in data.iter_mut().zip(&first[..split]) {
                            *out = convert(sample * volume);
                        }
                        for (out, sample) in data[split..usable]
                            .iter_mut()
                            .zip(&second[..usable - split])
                        {
                            *out = convert(sample * volume);
                        }

                        // The scope shows what was played, which since M3 means
                        // the output's channel layout rather than the source's.
                        // It is the layout that stays put across a queue.
                        shared.scope.push(&first[..split]);
                        if usable > split {
                            shared.scope.push(&second[..usable - split]);
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
                if filled > 0 {
                    shared
                        .position_frames
                        .fetch_add((filled / out_channels) as u64, Ordering::Relaxed);
                }

                // Finished only when the decoder is done *and* nothing is left
                // to play. Either alone is an ordinary mid-track state.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn adapt(src: &[f32], src_channels: u16, out_channels: u16) -> Vec<f32> {
        let mut dst = Vec::new();
        adapt_channels(
            src,
            &Shape {
                src_channels,
                out_channels,
            },
            &mut dst,
        );
        dst
    }

    #[test]
    fn matching_layouts_pass_through_untouched() {
        let src = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(adapt(&src, 2, 2), src);
    }

    #[test]
    fn mono_is_duplicated_across_the_outputs() {
        assert_eq!(adapt(&[1.0, 2.0], 1, 2), [1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn extra_source_channels_are_dropped() {
        // 5.1-ish frame down to a stereo device: the first pair survives.
        let src = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(adapt(&src, 6, 2), [1.0, 2.0]);
    }

    #[test]
    fn stereo_up_to_more_outputs_cycles_the_pair() {
        assert_eq!(adapt(&[1.0, 2.0], 2, 4), [1.0, 2.0, 1.0, 2.0]);
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped_rather_than_misaligned() {
        // Half a frame at the end would shift every channel after it, so it
        // goes. The resampler emits whole frames, so this is belt and braces.
        assert_eq!(adapt(&[1.0, 2.0, 3.0], 2, 1), [1.0]);
    }

    #[test]
    fn appending_twice_keeps_the_frames_in_order() {
        // The decoder thread reuses one buffer across blocks, so adaptation
        // has to append rather than replace.
        let mut dst = Vec::new();
        let shape = Shape {
            src_channels: 1,
            out_channels: 2,
        };
        adapt_channels(&[1.0], &shape, &mut dst);
        adapt_channels(&[2.0], &shape, &mut dst);
        assert_eq!(dst, [1.0, 1.0, 2.0, 2.0]);
    }
}
