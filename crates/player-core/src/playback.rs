//! Native (cpal) playback. Not built on wasm32 -- cpal has no wasm target;
//! the web build drives output through the Web Audio API / AudioWorklet
//! instead (see `player-rt-wasm`, and PLAN.md §5's "Web audio I/O" note).
#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use crate::decode::DecodedAudio;
use crate::error::PlayerError;

/// Phase 0's stand-in for §5's ring buffer: since the whole file is already
/// decoded into memory up front (decode.rs), there's no producer/consumer
/// backpressure problem to solve yet, so a shared cursor over a shared
/// sample buffer is enough to feed both the cpal callback and a render
/// loop's "what's on screen right now" read. Real streaming (Subsonic,
/// Phase 2+) is what will need an actual ring buffer here.
struct PlaybackState {
    samples: Vec<f32>, // interleaved
    channels: u16,
    /// Sample index (not frame) of the next sample the cpal callback will
    /// write out.
    cursor: usize,
    playing: bool,
    /// Linear gain applied on the way out. Not a dB curve -- callers that
    /// want a fader taper are expected to shape it themselves, since the
    /// right curve is a UI decision rather than an engine one.
    volume: f32,
}

pub struct Playback {
    state: Arc<Mutex<PlaybackState>>,
    channels: u16,
    sample_rate: u32,
    // Kept alive only to keep the stream running -- cpal stops the stream
    // when this is dropped. Never read directly.
    _stream: cpal::Stream,
}

impl Playback {
    /// Opens the default output device at *its own* mix format and starts a
    /// stream immediately (paused), resampling the decoded audio to match.
    ///
    /// The device's format is taken as given rather than negotiated, which
    /// looks lazy and isn't -- see `device_config` below for the evidence.
    pub fn open(audio: DecodedAudio) -> Result<Self, PlayerError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(PlayerError::NoOutputDevice)?;

        let supported = device_config(&device)?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();

        // The device dictates the rate (again: see `device_config`), so the
        // audio has to move to meet it. Without this, a 44.1kHz file on a
        // 96kHz device plays at 2.177x -- found live on Windows, 2026-08-15.
        let samples = crate::resample::resample_interleaved(
            audio.samples,
            audio.channels,
            audio.sample_rate,
            config.sample_rate,
        )?;

        let state = Arc::new(Mutex::new(PlaybackState {
            samples,
            channels: audio.channels,
            cursor: 0,
            playing: false,
            volume: 1.0,
        }));

        let stream = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, state.clone(), |s| s),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, state.clone(), |s| {
                (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            }),
            other => return Err(PlayerError::UnsupportedOutputFormat(format!("{other:?}"))),
        }?;
        stream
            .play()
            .map_err(|e| PlayerError::OutputStream(e.to_string()))?;

        Ok(Self {
            channels: config.channels,
            sample_rate: config.sample_rate,
            state,
            _stream: stream,
        })
    }

    pub fn play(&self) {
        self.state.lock().unwrap().playing = true;
    }

    pub fn pause(&self) {
        self.state.lock().unwrap().playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.state.lock().unwrap().playing
    }

    /// Linear gain, clamped to `0.0..=1.0`.
    pub fn set_volume(&self, volume: f32) {
        self.state.lock().unwrap().volume = volume.clamp(0.0, 1.0);
    }

    pub fn volume(&self) -> f32 {
        self.state.lock().unwrap().volume
    }

    /// Jumps to `seconds` from the start. Clamped to the track, so seeking
    /// past the end parks on the end rather than reading out of bounds.
    ///
    /// Note this seeks within the already-decoded buffer -- cheap and exact.
    /// Once decoding streams (Phase 1 M2) this becomes a real demux/decoder
    /// seek and stops being free.
    pub fn seek(&self, seconds: f64) {
        let mut state = self.state.lock().unwrap();
        let channels = state.channels.max(1) as usize;
        let frame = (seconds.max(0.0) * self.sample_rate as f64) as usize;
        let total_frames = state.samples.len() / channels;
        state.cursor = frame.min(total_frames) * channels;
    }

    /// Current play position in seconds.
    pub fn position_seconds(&self) -> f64 {
        let state = self.state.lock().unwrap();
        let channels = state.channels.max(1) as usize;
        (state.cursor / channels) as f64 / self.sample_rate as f64
    }

    /// Total length in seconds, measured on the *resampled* buffer that is
    /// actually being played -- so it matches what `position_seconds`
    /// counts up to, rather than the source file's own duration.
    pub fn duration_seconds(&self) -> f64 {
        let state = self.state.lock().unwrap();
        let channels = state.channels.max(1) as usize;
        (state.samples.len() / channels) as f64 / self.sample_rate as f64
    }

    /// Whether playback has run off the end of the track. The UI uses this
    /// to flip its transport back to a stopped state without polling for
    /// exact equality on a moving cursor.
    pub fn is_finished(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.cursor >= state.samples.len()
    }

    pub fn output_channels(&self) -> u16 {
        self.channels
    }

    pub fn output_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The most recently played `n` interleaved samples, for the scope
    /// render loop to draw a live trace from.
    pub fn scope_window(&self, n: usize) -> Vec<f32> {
        let state = self.state.lock().unwrap();
        let end = state.cursor.min(state.samples.len());
        let start = end.saturating_sub(n);
        state.samples[start..end].to_vec()
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
    state: Arc<Mutex<PlaybackState>>,
    convert: fn(f32) -> T,
) -> Result<cpal::Stream, PlayerError>
where
    T: cpal::SizedSample + Send + 'static,
{
    let out_channels = config.channels as usize;
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                let mut state = state.lock().unwrap();
                let src_channels = state.channels.max(1) as usize;
                let volume = state.volume;
                for frame in data.chunks_mut(out_channels) {
                    if !state.playing || state.cursor >= state.samples.len() {
                        frame.fill(convert(0.0));
                        continue;
                    }
                    for (ch, out) in frame.iter_mut().enumerate() {
                        // Simple channel adaptation: reuse source channels
                        // cyclically (mono -> N duplicates the one channel;
                        // anything fancier is Phase 1 territory).
                        let src_idx = state.cursor + (ch % src_channels);
                        let sample = state.samples.get(src_idx).copied().unwrap_or(0.0);
                        *out = convert(sample * volume);
                    }
                    state.cursor += src_channels;
                }
            },
            |err| eprintln!("output stream error: {err}"),
            None,
        )
        .map_err(|e| PlayerError::OutputStream(e.to_string()))
}
