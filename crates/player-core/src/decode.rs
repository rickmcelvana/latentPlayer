//! The decoded-audio value type, plus the whole-file decode built on top of
//! `stream.rs`.
//!
//! Phase 0 decoded ahead of time into a flat buffer and played a cursor over
//! it. Phase 1 M2 moved the real work to `stream.rs`, which decodes on demand;
//! what's left here is the shape that whole-file decoding produces, kept
//! because the smoke examples and the resampler tests genuinely want an entire
//! clip in memory. Playback does not use it any more.
//!
//! Built against symphonia-core 0.6.1's real API (verified against its
//! source, not written from memory -- 0.6 reorganized decoding around
//! separate audio/video/subtitle codec kinds, so the 0.5-era `SampleBuffer`/
//! `CODEC_TYPE_NULL`/`CodecRegistry::make()` shape this crate used to have
//! no longer exists).

pub use crate::stream::decode_all;

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
