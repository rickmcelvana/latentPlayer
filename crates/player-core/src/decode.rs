//! Whole-file decode into interleaved f32 PCM. Phase 0's walking skeleton
//! decodes ahead of time rather than streaming (both the native cpal path
//! and the wasm/AudioWorklet path just need a flat sample buffer to play
//! from and to read a scope window out of) -- streaming decode is a Phase 1+
//! concern once files/sources get large enough to matter.
//!
//! Built against symphonia-core 0.6.1's real API (verified against its
//! source, not written from memory -- 0.6 reorganized decoding around
//! separate audio/video/subtitle codec kinds, so the 0.5-era `SampleBuffer`/
//! `CODEC_TYPE_NULL`/`CodecRegistry::make()` shape this crate used to have
//! no longer exists).

use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

use crate::error::PlayerError;
use crate::source::PlaybackSource;

/// Interleaved f32 PCM plus the format info needed to play it back.
pub struct DecodedAudio {
    /// Interleaved samples: `[L0, R0, L1, R1, ...]` for stereo.
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

impl DecodedAudio {
    pub fn duration_seconds(&self) -> f64 {
        let frames = self.samples.len() / self.channels.max(1) as usize;
        frames as f64 / self.sample_rate.max(1) as f64
    }
}

pub fn decode_all(source: &dyn PlaybackSource) -> Result<DecodedAudio, PlayerError> {
    let media_source: Box<dyn MediaSource> = source.open()?;
    let mss = MediaSourceStream::new(media_source, Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = source.extension_hint() {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let track = format
        .first_track_known_codec(TrackType::Audio)
        .ok_or(PlayerError::NoAudioTrack)?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(codec_params)) = track.codec_params.clone() else {
        return Err(PlayerError::NoAudioTrack);
    };
    let sample_rate = codec_params.sample_rate.unwrap_or(44_100);
    let channels = codec_params
        .channels
        .as_ref()
        .map(|c| c.count() as u16)
        .unwrap_or(2);

    let mut decoder = symphonia::default::get_codecs()
        .get_audio_decoder(codec_params.codec)
        .ok_or(PlayerError::UnsupportedCodec)
        .and_then(|reg| {
            (reg.factory)(&codec_params, &Default::default()).map_err(PlayerError::from)
        })?;

    let mut samples = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();

    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                decoded.copy_to_vec_interleaved(&mut scratch);
                samples.extend_from_slice(&scratch);
            }
            // A single bad packet shouldn't abort the whole file -- skip it
            // and keep decoding, matching Symphonia's own documented advice.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }

    Ok(DecodedAudio {
        samples,
        channels,
        sample_rate,
    })
}
