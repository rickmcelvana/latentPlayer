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
    /// Opens the default output device and starts a stream immediately
    /// (paused). Phase 0 doesn't attempt sample-rate conversion: if the
    /// decoded audio's rate doesn't match a config the device supports, the
    /// device's default config is used as-is and playback will sound
    /// pitched -- fine for a spike, wrong for Phase 1.
    pub fn open(audio: DecodedAudio) -> Result<Self, PlayerError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(PlayerError::NoOutputDevice)?;

        let supported = pick_config(&device, audio.sample_rate, audio.channels)
            .or_else(|| device.default_output_config().ok())
            .ok_or(PlayerError::NoOutputDevice)?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();

        let state = Arc::new(Mutex::new(PlaybackState {
            samples: audio.samples,
            channels: audio.channels,
            cursor: 0,
            playing: false,
        }));

        let stream = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, state.clone(), |s| s),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, state.clone(), |s| {
                (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            }),
            other => return Err(PlayerError::UnsupportedOutputFormat(format!("{other:?}"))),
        }?;
        stream.play().map_err(|_| PlayerError::NoOutputDevice)?;

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

fn pick_config(
    device: &cpal::Device,
    sample_rate: u32,
    channels: u16,
) -> Option<cpal::SupportedStreamConfig> {
    let candidates: Vec<_> = device
        .supported_output_configs()
        .ok()?
        .filter(|c| c.channels() == channels)
        .filter(|c| c.min_sample_rate() <= sample_rate && sample_rate <= c.max_sample_rate())
        .collect();

    // Prefer F32, then I16 -- the only formats `build_stream` (below) can
    // actually feed. Found the hard way (2026-08-15 Phase 0 live playback
    // test): a plain `.find()` over `supported_output_configs()` took
    // whatever format the device listed *first* for a matching
    // channel/rate combo, and this machine's ALSA/PipeWire device lists
    // U8 before F32/I16 for mono -- `build_stream` correctly refused it
    // rather than mangling audio, but a stereo/48kHz play would have
    // hit the same trap silently on a different device.
    candidates
        .iter()
        .find(|c| c.sample_format() == SampleFormat::F32)
        .or_else(|| {
            candidates
                .iter()
                .find(|c| c.sample_format() == SampleFormat::I16)
        })
        .cloned()
        .map(|c| c.with_sample_rate(sample_rate))
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
                        *out = convert(sample);
                    }
                    state.cursor += src_channels;
                }
            },
            |err| eprintln!("output stream error: {err}"),
            None,
        )
        .map_err(|_| PlayerError::NoOutputDevice)
}
