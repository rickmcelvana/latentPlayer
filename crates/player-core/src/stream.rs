//! Streaming decode: pull one block of interleaved f32 PCM at a time, seek by
//! asking the container to move rather than by moving a cursor.
//!
//! Phase 0 (and Phase 1 M1) decoded the whole file into a `Vec<f32>` up front,
//! which made everything downstream simple -- position was a cursor, duration
//! was a length, and the scope could look backwards into the buffer at will.
//! It also meant a 40-minute FLAC cost its full decoded size in RAM
//! (~400MB at 24/96 stereo) before a single sample played. This module is M2's
//! replacement: the decoder stays open for the life of the track and hands out
//! blocks on demand.
//!
//! Deliberately target-agnostic -- no threads, no output device, no ring
//! buffer. Native playback drives it from a decoder thread (`playback.rs`) and
//! the web build drives it from a Worker (`player-rt-wasm`); both need the
//! same "give me the next block" and "go to this time" operations, and neither
//! wants the other's concurrency model baked in here.
//!
//! Built against symphonia 0.6.1's real API (`FormatReader::seek`,
//! `SeekTo::Time`, `Track::num_frames`/`time_base`), verified against its
//! source per CONVENTIONS.md.

use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

use crate::error::PlayerError;
use crate::source::PlaybackSource;

/// An open decoder positioned somewhere in a track.
///
/// Not `Clone` and not shareable: it owns a single read position in a single
/// container, so exactly one thread drives it at a time.
pub struct StreamingDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    channels: u16,
    sample_rate: u32,
    duration: Option<f64>,
    /// Reused across calls so steady-state decoding does no allocation after
    /// the first few blocks.
    block: Vec<f32>,
    /// Frames to throw away before handing anything back, set when a seek
    /// lands earlier than asked for -- see `seek`.
    discard_frames: usize,
    finished: bool,
}

impl StreamingDecoder {
    /// Opens `source` and decodes far enough to know its format. Does not
    /// decode any audio yet.
    pub fn open(source: &dyn PlaybackSource) -> Result<Self, PlayerError> {
        let media_source: Box<dyn MediaSource> = source.open()?;
        let mss = MediaSourceStream::new(media_source, Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = source.extension_hint() {
            hint.with_extension(ext);
        }

        let format = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;

        let track = format
            .first_track_known_codec(TrackType::Audio)
            .ok_or(PlayerError::NoAudioTrack)?;
        let track_id = track.id;
        let time_base = track.time_base;
        let num_frames = track.num_frames;
        let track_duration = track.duration;
        let Some(CodecParameters::Audio(codec_params)) = track.codec_params.clone() else {
            return Err(PlayerError::NoAudioTrack);
        };
        let sample_rate = codec_params.sample_rate.unwrap_or(44_100);
        let channels = codec_params
            .channels
            .as_ref()
            .map(|c| c.count() as u16)
            .unwrap_or(2);

        let decoder = symphonia::default::get_codecs()
            .get_audio_decoder(codec_params.codec)
            .ok_or(PlayerError::UnsupportedCodec)
            .and_then(|reg| {
                (reg.factory)(&codec_params, &Default::default()).map_err(PlayerError::from)
            })?;

        // Duration now comes from the container rather than from the length of
        // a buffer that no longer exists. `num_frames` is the better source of
        // the two symphonia offers -- it already excludes encoder delay and
        // padding, where the timebase `duration` is the container's own claim
        // and can disagree slightly (symphonia says so itself in `Track`'s
        // docs). Either can be absent, e.g. a CBR-less MP3 with no Xing
        // header, which is why this is an `Option` all the way to the UI.
        let duration = num_frames
            .map(|frames| frames as f64 / sample_rate.max(1) as f64)
            .or_else(|| {
                let tb = time_base?;
                let ticks = track_duration?;
                Some(
                    tb.calc_time_saturating(ticks.get().try_into().ok()?)
                        .as_secs_f64(),
                )
            });

        Ok(Self {
            format,
            decoder,
            track_id,
            time_base,
            channels,
            sample_rate,
            duration,
            block: Vec::new(),
            discard_frames: 0,
            finished: false,
        })
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Total length in seconds, or `None` when the container doesn't say.
    /// Callers are expected to handle the unknown case (the transport shows
    /// elapsed time only) rather than substituting a wrong number.
    pub fn duration_seconds(&self) -> Option<f64> {
        self.duration
    }

    /// The next block of interleaved samples, or `None` at end of stream.
    ///
    /// Block length is whatever the codec's packet decodes to (1152 frames for
    /// MP3, 1024 for AAC, up to 4096 for FLAC), so callers must not assume a
    /// fixed size. Borrowed from an internal buffer that the next call
    /// overwrites.
    pub fn next_block(&mut self) -> Result<Option<&[f32]>, PlayerError> {
        if self.finished {
            return Ok(None);
        }
        let channels = self.channels.max(1) as usize;

        loop {
            let Some(packet) = self.format.next_packet()? else {
                self.finished = true;
                return Ok(None);
            };
            if packet.track_id != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    decoded.copy_to_vec_interleaved(&mut self.block);
                }
                // A single bad packet shouldn't abort the whole file -- skip it
                // and keep decoding, matching Symphonia's own documented advice
                // and Phase 0's behaviour.
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            }

            // A seek can land before the requested time (see `seek`); drop the
            // frames between where it landed and where the caller asked for.
            if self.discard_frames > 0 {
                let available = self.block.len() / channels;
                let drop = self.discard_frames.min(available);
                self.discard_frames -= drop;
                self.block.drain(..drop * channels);
            }

            if !self.block.is_empty() {
                return Ok(Some(&self.block));
            }
        }
    }

    /// Moves to `seconds` from the start, returning the position actually
    /// landed on.
    ///
    /// Unlike M1's seek -- which was an index into an already-decoded buffer,
    /// and therefore both free and exact -- this asks the container to move,
    /// which is neither. Two consequences are handled here:
    ///
    /// - The decoder carries state across packets (MP3's bit reservoir, FLAC's
    ///   inter-frame prediction), so it must be `reset()` or the first block
    ///   after a seek is garbage.
    /// - Even in `Accurate` mode a reader may only be able to land on a
    ///   container-level boundary at or before the requested time. The
    ///   difference is decoded and thrown away (`discard_frames`) so callers
    ///   get the sample they asked for, not the nearest frame boundary.
    pub fn seek(&mut self, seconds: f64) -> Result<f64, PlayerError> {
        let target = match self.duration {
            // Seeking exactly to the end has nothing to land on in most
            // containers; back off a hair so it parks on the last frame
            // instead of erroring.
            Some(duration) => seconds.clamp(0.0, (duration - 0.001).max(0.0)),
            None => seconds.max(0.0),
        };

        let time = Time::try_new(
            target.trunc() as i64,
            (target.fract() * 1_000_000_000.0) as u32,
        )
        .ok_or_else(|| PlayerError::Seek(format!("{target} is not a representable time")))?;

        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )?;
        self.decoder.reset();
        self.finished = false;

        // How far short the seek landed, in frames. Timestamps are in timebase
        // units, which for audio is usually but not always 1/sample_rate --
        // going via seconds is correct for either.
        self.discard_frames = match self.time_base {
            Some(tb) => {
                let landed = tb.calc_time_saturating(seeked.actual_ts).as_secs_f64();
                let wanted = tb.calc_time_saturating(seeked.required_ts).as_secs_f64();
                ((wanted - landed).max(0.0) * self.sample_rate as f64).round() as usize
            }
            None => 0,
        };

        Ok(target)
    }
}

/// Decodes an entire track into one interleaved buffer.
///
/// Kept as a thin wrapper over `StreamingDecoder` rather than as a second
/// decode path: it's what the smoke examples and the resampler tests want, and
/// having it share the streaming code means the two can't drift -- see
/// `streamed_blocks_equal_a_whole_file_decode` below.
pub fn decode_all(source: &dyn PlaybackSource) -> Result<crate::decode::DecodedAudio, PlayerError> {
    let mut decoder = StreamingDecoder::open(source)?;
    let channels = decoder.channels();
    let sample_rate = decoder.sample_rate();

    let mut samples = Vec::new();
    while let Some(block) = decoder.next_block()? {
        samples.extend_from_slice(block);
    }

    Ok(crate::decode::DecodedAudio {
        samples,
        channels,
        sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::InMemorySource;

    /// A tiny 16-bit PCM WAV, built by hand so the tests need no fixture file.
    /// `frames` frames of a 440Hz tone at 44.1kHz stereo.
    fn wav(frames: usize) -> Vec<u8> {
        let (channels, rate, bits) = (2u16, 44_100u32, 16u16);
        let block_align = channels * bits / 8;
        let data_len = frames * block_align as usize;

        let mut out = Vec::with_capacity(44 + data_len);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let left = ((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 16_384.0) as i16;
            // Right channel counts frames instead, so a test can tell exactly
            // which frame any sample came from.
            let right = (i % 32_767) as i16;
            out.extend_from_slice(&left.to_le_bytes());
            out.extend_from_slice(&right.to_le_bytes());
        }
        out
    }

    /// Undoes the i16 -> f32 normalisation symphonia applies on the way out,
    /// recovering the frame counter `wav` wrote into the right channel.
    fn frame_number(right_channel: f32) -> usize {
        (right_channel * 32_768.0).round() as usize
    }

    fn source(frames: usize) -> InMemorySource {
        InMemorySource::new(wav(frames), Some("wav".to_string()))
    }

    #[test]
    fn reports_the_format_without_decoding_audio() {
        let decoder = StreamingDecoder::open(&source(44_100)).unwrap();
        assert_eq!(decoder.channels(), 2);
        assert_eq!(decoder.sample_rate(), 44_100);
        let duration = decoder.duration_seconds().expect("WAV states its length");
        assert!(
            (duration - 1.0).abs() < 0.001,
            "expected ~1.0s, got {duration}"
        );
    }

    #[test]
    fn blocks_cover_the_whole_track_exactly_once() {
        let frames = 44_100;
        let mut decoder = StreamingDecoder::open(&source(frames)).unwrap();
        let mut total = 0;
        while let Some(block) = decoder.next_block().unwrap() {
            assert!(!block.is_empty(), "a returned block is never empty");
            total += block.len();
        }
        assert_eq!(total, frames * 2, "every frame, no more and no fewer");
        assert!(
            decoder.next_block().unwrap().is_none(),
            "end of stream stays ended"
        );
    }

    #[test]
    fn streamed_blocks_equal_a_whole_file_decode() {
        // The guarantee that keeps M2 honest: streaming must be the same audio
        // as Phase 0's whole-file decode, not merely similar.
        let src = source(30_000);
        let whole = decode_all(&src).unwrap();

        let mut decoder = StreamingDecoder::open(&src).unwrap();
        let mut streamed = Vec::new();
        while let Some(block) = decoder.next_block().unwrap() {
            streamed.extend_from_slice(block);
        }
        assert_eq!(streamed, whole.samples, "streamed audio must be bit-exact");
    }

    #[test]
    fn seek_lands_on_the_requested_frame() {
        // The right channel is a frame counter (see `wav`), so the first
        // sample after a seek says exactly where the seek landed.
        let mut decoder = StreamingDecoder::open(&source(44_100)).unwrap();
        let landed = decoder.seek(0.5).unwrap();
        assert!((landed - 0.5).abs() < 1e-9, "seek reported {landed}");

        let block = decoder.next_block().unwrap().expect("audio after the seek");
        let frame = frame_number(block[1]);
        let expected = (0.5 * 44_100.0) as usize;
        assert_eq!(
            frame, expected,
            "first frame after seeking to 0.5s should be frame {expected}"
        );
    }

    #[test]
    fn seeking_backwards_replays_the_same_audio() {
        let src = source(44_100);
        let mut decoder = StreamingDecoder::open(&src).unwrap();
        decoder.seek(0.25).unwrap();
        let first: Vec<f32> = decoder.next_block().unwrap().unwrap().to_vec();

        // Somewhere else entirely, then back.
        decoder.seek(0.75).unwrap();
        decoder.next_block().unwrap();
        decoder.seek(0.25).unwrap();
        let again: Vec<f32> = decoder.next_block().unwrap().unwrap().to_vec();

        assert_eq!(first, again, "the same seek must produce the same audio");
    }

    #[test]
    fn seeking_past_the_end_parks_at_the_end() {
        let mut decoder = StreamingDecoder::open(&source(44_100)).unwrap();
        let landed = decoder.seek(999.0).unwrap();
        assert!(
            landed > 0.9 && landed < 1.0,
            "seeking past the end should clamp into the track, got {landed}"
        );
    }

    #[test]
    fn seeking_revives_a_finished_decoder() {
        let mut decoder = StreamingDecoder::open(&source(10_000)).unwrap();
        while decoder.next_block().unwrap().is_some() {}
        decoder.seek(0.0).unwrap();
        assert!(
            decoder.next_block().unwrap().is_some(),
            "seeking after end of stream must start decoding again"
        );
    }
}
