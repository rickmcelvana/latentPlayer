//! Track metadata: what a file says about itself.
//!
//! M7's first piece, and the answer to Phase 1's opening complaint that
//! "tags/metadata are probed by Symphonia and then discarded". Symphonia
//! already opens every one of these files in order to play it, and its 0.6
//! metadata API hands back *parsed* tags (`StandardTag`) rather than raw
//! key/value pairs -- so one reader covers ID3v2, Vorbis comments, RIFF INFO
//! and MP4 `ilst` alike, and neither shell needs a tag parser of its own
//! (M7 decision 1).
//!
//! Cross-target on purpose. The web build reads tags through `player-rt-wasm`
//! and the desktop build calls straight in; both run this code, so the two
//! shells cannot disagree about what a file is called -- the same rule M4's
//! spectrum and M5's goniometer follow.
//!
//! Built against symphonia 0.6.1's real API (`Metadata`/`MetadataRevision`,
//! `StandardTag`, `Visual`), read out of its source per CONVENTIONS.md. 0.6's
//! metadata module is a redesign of 0.5's -- `StandardTagKey` and
//! `Tag::value` are gone -- so anything written from memory of the older API
//! would not compile, let alone work.

use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag, StandardVisualKey};

use crate::error::PlayerError;
use crate::source::PlaybackSource;

/// An embedded image, as stored in the file.
///
/// Kept encoded rather than decoded to pixels: every consumer of this is
/// either writing it back out to a cache file or handing it to a browser,
/// both of which want the original bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverArt {
    /// e.g. `image/jpeg`. Symphonia sniffs the image itself where it can and
    /// only falls back to what the tag claimed, so this describes the bytes
    /// rather than the metadata's opinion of them.
    pub media_type: Option<String>,
    pub data: Vec<u8>,
}

/// What a file says about itself.
///
/// Every field is optional because every field genuinely can be missing -- a
/// freshly ripped WAV has none of them. Callers display a fallback (the file
/// name) rather than being handed a guess from here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<u32>,
    /// From the container, exactly as `StreamingDecoder` reports it -- and
    /// `None` for the same reasons (see its `duration_seconds`).
    pub duration_seconds: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// The front cover if the file names one, otherwise the first image it
    /// carries. Absent when it carries none.
    pub cover: Option<CoverArt>,
}

impl TrackTags {
    /// True when the file said nothing at all -- the case a caller falls back
    /// to the file name for.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.album_artist.is_none()
            && self.genre.is_none()
            && self.track_number.is_none()
            && self.year.is_none()
    }
}

/// Reads `source`'s tags, cover art and stream properties.
///
/// Opens and probes the container but decodes **no audio**, so this costs a
/// handful of reads per file rather than a decode -- which is what makes
/// scanning a folder of a few thousand tracks reasonable at all.
pub fn read_tags(source: &dyn PlaybackSource) -> Result<TrackTags, PlayerError> {
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

    let mut tags = TrackTags {
        // Deliberately read before the metadata below: a file with no tags at
        // all still has a length and a rate, and a library row wants them.
        ..stream_properties(format.as_ref())
    };

    // Symphonia keeps a *log* of metadata revisions rather than one tag set,
    // and the order matters. `Probe::probe` runs `probe_trailing` before it
    // scans forward for the container, so an MP3 carrying both tags produces
    // [ID3v1, ID3v2] -- oldest first. Applying them in that order, letting
    // later ones overwrite, is therefore what makes ID3v2 win; first-wins
    // would hand the UI ID3v1's 30-character truncated title instead
    // (M7 decision 2).
    let mut metadata = format.metadata();
    loop {
        if let Some(revision) = metadata.current() {
            apply_revision(&mut tags, revision);
        }
        if metadata.pop().is_none() {
            break;
        }
    }

    Ok(tags)
}

/// Rate, channel count and length, off the container's own track description.
fn stream_properties(format: &dyn FormatReader) -> TrackTags {
    let Some(track) = format.first_track_known_codec(TrackType::Audio) else {
        return TrackTags::default();
    };
    let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
        return TrackTags::default();
    };

    let sample_rate = params.sample_rate;
    // Same derivation `StreamingDecoder::open` uses, and the same caveat:
    // `num_frames` already excludes encoder delay and padding, the timebase
    // duration is the container's own claim, and either can be missing.
    let duration_seconds = track
        .num_frames
        .zip(sample_rate)
        .map(|(frames, rate)| frames as f64 / rate.max(1) as f64)
        .or_else(|| {
            let tb = track.time_base?;
            let ticks = track.duration?;
            Some(
                tb.calc_time_saturating(ticks.get().try_into().ok()?)
                    .as_secs_f64(),
            )
        });

    TrackTags {
        duration_seconds,
        sample_rate,
        channels: params.channels.as_ref().map(|c| c.count() as u16),
        ..TrackTags::default()
    }
}

/// Folds one revision's tags and pictures into `tags`, overwriting what an
/// earlier revision set. See `read_tags` for why later wins.
fn apply_revision(tags: &mut TrackTags, revision: &MetadataRevision) {
    for tag in &revision.media.tags {
        let Some(std) = tag.std.as_ref() else {
            continue;
        };
        match std {
            StandardTag::TrackTitle(v) => tags.title = Some(v.to_string()),
            StandardTag::Artist(v) => tags.artist = Some(v.to_string()),
            StandardTag::Album(v) => tags.album = Some(v.to_string()),
            StandardTag::AlbumArtist(v) => tags.album_artist = Some(v.to_string()),
            StandardTag::Genre(v) => tags.genre = Some(v.to_string()),
            StandardTag::TrackNumber(v) => tags.track_number = u32::try_from(*v).ok(),
            StandardTag::DiscNumber(v) => tags.disc_number = u32::try_from(*v).ok(),
            // Symphonia parses a bare year into `RecordingYear` and a full
            // date into `RecordingDate`; a library row only wants the year, so
            // the date's leading four digits are taken when that is all there
            // is. `OriginalRecordingYear` is deliberately not used -- a
            // reissue's original year is not the year of this recording.
            StandardTag::RecordingYear(v) => tags.year = Some(u32::from(*v)),
            StandardTag::ReleaseYear(v) => {
                tags.year.get_or_insert(u32::from(*v));
            }
            StandardTag::RecordingDate(v) | StandardTag::ReleaseDate(v) => {
                if let Some(year) = leading_year(v) {
                    tags.year.get_or_insert(year);
                }
            }
            _ => {}
        }
    }

    // The front cover if the file names one, else the first picture present.
    // A well-tagged album often carries several (front, back, artist), and
    // "whichever came first" would show the back of the sleeve about as often
    // as the front.
    let visuals = &revision.media.visuals;
    let chosen = visuals
        .iter()
        .find(|v| v.usage == Some(StandardVisualKey::FrontCover))
        .or_else(|| visuals.first());
    if let Some(visual) = chosen {
        tags.cover = Some(CoverArt {
            media_type: visual.media_type.clone(),
            data: visual.data.to_vec(),
        });
    }
}

/// The year out of a `YYYY-MM-DD`-ish date string, if it starts with one.
fn leading_year(value: &str) -> Option<u32> {
    let digits: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() != 4 {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::InMemorySource;

    /// A tiny 16-bit PCM WAV, in the same hand-built style as stream.rs's --
    /// the tests need no fixture files.
    fn wav(frames: usize) -> Vec<u8> {
        let (channels, rate, bits) = (2u16, 44_100u32, 16u16);
        let block_align = channels * bits / 8;
        let data_len = frames * block_align as usize;

        let mut out = Vec::with_capacity(44 + data_len);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        out.resize(out.len() + data_len, 0);
        out
    }

    /// One ID3v2.3 text/binary frame: four-character id, big-endian size,
    /// two flag bytes, then the body.
    fn id3_frame(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(10 + body.len());
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(body);
        out
    }

    /// A text frame's body: one encoding byte (0 = ISO-8859-1) then the text.
    fn text_frame(id: &[u8; 4], text: &str) -> Vec<u8> {
        let mut body = vec![0u8];
        body.extend_from_slice(text.as_bytes());
        id3_frame(id, &body)
    }

    /// An `APIC` body: encoding, null-terminated media type, picture type,
    /// null-terminated description, then the image itself.
    fn picture_frame(media_type: &str, picture_type: u8, image: &[u8]) -> Vec<u8> {
        let mut body = vec![0u8];
        body.extend_from_slice(media_type.as_bytes());
        body.push(0);
        body.push(picture_type);
        body.push(0); // empty description
        body.extend_from_slice(image);
        id3_frame(b"APIC", &body)
    }

    /// Wraps frames in an ID3v2.3 tag. Sizes are syncsafe: seven bits per
    /// byte, which is what makes a tag impossible to mistake for an MPEG
    /// frame sync -- and what makes a plain `to_be_bytes()` here wrong.
    fn id3v2(frames: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = frames.concat();
        let size = body.len() as u32;
        let syncsafe = [
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ];

        let mut out = Vec::with_capacity(10 + body.len());
        out.extend_from_slice(b"ID3");
        out.extend_from_slice(&[3, 0]); // v2.3.0
        out.push(0); // no flags
        out.extend_from_slice(&syncsafe);
        out.extend_from_slice(&body);
        out
    }

    /// The smallest thing symphonia will recognise as a PNG: the signature
    /// plus an IHDR declaring 1x1. Used so the cover's media type comes from
    /// sniffing the image rather than from trusting the tag.
    fn png_1x1() -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&1u32.to_be_bytes()); // width
        out.extend_from_slice(&1u32.to_be_bytes()); // height
        out.extend_from_slice(&[8, 6, 0, 0, 0]); // depth, colour type, etc.
        out.extend_from_slice(&0u32.to_be_bytes()); // CRC, unchecked here
        out
    }

    /// A tagged file: an ID3v2 tag in front of a WAV. Symphonia's probe reads
    /// leading metadata elements before it looks for a container (verified in
    /// `formats/probe.rs`), so this exercises the real ID3v2 reader without
    /// needing a real MP3 to be synthesised.
    fn tagged(frames: &[Vec<u8>], audio_frames: usize) -> InMemorySource {
        let mut bytes = id3v2(frames);
        bytes.extend_from_slice(&wav(audio_frames));
        InMemorySource::new(bytes, Some("wav".to_string()))
    }

    #[test]
    fn reads_the_common_id3v2_fields() {
        let source = tagged(
            &[
                text_frame(b"TIT2", "Latent Motion"),
                text_frame(b"TPE1", "The Vectorscopes"),
                text_frame(b"TALB", "Phosphor"),
                text_frame(b"TPE2", "Various Artists"),
                text_frame(b"TCON", "Electronic"),
                text_frame(b"TRCK", "7/12"),
                text_frame(b"TPOS", "2/2"),
                text_frame(b"TYER", "1998"),
            ],
            44_100,
        );

        let tags = read_tags(&source).unwrap();
        assert_eq!(tags.title.as_deref(), Some("Latent Motion"));
        assert_eq!(tags.artist.as_deref(), Some("The Vectorscopes"));
        assert_eq!(tags.album.as_deref(), Some("Phosphor"));
        assert_eq!(tags.album_artist.as_deref(), Some("Various Artists"));
        assert_eq!(tags.genre.as_deref(), Some("Electronic"));
        assert_eq!(tags.track_number, Some(7));
        assert_eq!(tags.disc_number, Some(2));
        assert_eq!(tags.year, Some(1998));
        assert!(!tags.is_empty());
    }

    #[test]
    fn reads_stream_properties_even_with_no_tags() {
        // The untagged case is the common one for a freshly ripped WAV, and a
        // library row still wants a length and a format for it.
        let source = InMemorySource::new(wav(44_100), Some("wav".to_string()));
        let tags = read_tags(&source).unwrap();

        assert!(tags.is_empty(), "an untagged file must report itself empty");
        assert_eq!(tags.sample_rate, Some(44_100));
        assert_eq!(tags.channels, Some(2));
        let duration = tags.duration_seconds.expect("WAV states its length");
        assert!(
            (duration - 1.0).abs() < 0.001,
            "expected ~1.0s, got {duration}"
        );
    }

    #[test]
    fn prefers_the_front_cover_over_other_pictures() {
        // A well-tagged album carries several images. Taking whichever came
        // first would show the back of the sleeve about as often as the front,
        // so picture type 3 (front cover) wins when it is present.
        let back = vec![0xAA; 32];
        let mut front = png_1x1();
        front.extend_from_slice(&[0x11; 16]);

        let source = tagged(
            &[
                picture_frame("image/jpeg", 4, &back),
                picture_frame("image/png", 3, &front),
            ],
            4_410,
        );

        let cover = read_tags(&source).unwrap().cover.expect("a cover");
        assert_eq!(cover.data, front, "the front cover, not the first picture");
        assert_eq!(cover.media_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn falls_back_to_the_only_picture_when_none_is_marked_front() {
        let image = vec![0x5A; 24];
        let source = tagged(&[picture_frame("image/jpeg", 0, &image)], 4_410);

        let cover = read_tags(&source).unwrap().cover.expect("a cover");
        assert_eq!(cover.data, image);
    }

    #[test]
    fn no_cover_is_absent_rather_than_empty() {
        let source = tagged(&[text_frame(b"TIT2", "Bare")], 4_410);
        assert!(read_tags(&source).unwrap().cover.is_none());
    }

    #[test]
    fn takes_the_year_out_of_a_full_date() {
        // ID3v2.4 writes TDRC as a full ISO date; a library row wants the
        // year, and parsing the whole date to get it is not worth a dependency.
        let source = tagged(&[text_frame(b"TDRC", "2011-04-19")], 4_410);
        assert_eq!(read_tags(&source).unwrap().year, Some(2011));
    }

    #[test]
    fn a_malformed_date_leaves_the_year_unset() {
        // Rather than reporting a wrong year, which would sort the row into
        // the wrong place with no way for the user to tell why.
        assert_eq!(leading_year("sometime in the 90s"), None);
        assert_eq!(leading_year("98"), None);
        assert_eq!(leading_year("2011-04-19"), Some(2011));
    }
}
