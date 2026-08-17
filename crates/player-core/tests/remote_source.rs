//! Reading a track over HTTP (Phase 2 M2), against a server that ranges and one
//! that refuses to.
//!
//! The end of this file is the check the rest exists to support: a real WAV,
//! served over real TCP, decoded through `RemoteSource` and compared sample for
//! sample against the same bytes decoded locally. A remote source that is subtly
//! wrong -- an off-by-one at a chunk boundary, a seek that lands a byte early --
//! produces audio that still plays and is not the track. Only a comparison
//! catches that.

mod common;

use std::io::{Read, Seek, SeekFrom};

use common::{wav_bytes, FakeServer, Reply};
use player_core::remote::RemoteSource;
use player_core::{InMemorySource, PlaybackSource, StreamingDecoder};

/// Bigger than the source's 512KB chunk, so reading it whole must cross chunk
/// boundaries rather than fitting in the first fetch.
const BIG: usize = 1_400_000;

fn body(len: usize) -> Vec<u8> {
    // Values that make an off-by-one visible: every byte says where it is.
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn source(server: &FakeServer, path: &str) -> RemoteSource {
    RemoteSource::new(format!("{}/rest/{path}", server.base_url()), None)
}

#[test]
fn a_ranged_server_gives_a_seekable_stream_of_a_known_length() {
    let server = FakeServer::start();
    let content = body(BIG);
    server.on(
        "stream",
        Reply::Ranged {
            body: content.clone(),
        },
    );

    let stream = source(&server, "stream").open().expect("open the track");
    assert!(stream.is_seekable());
    assert_eq!(stream.byte_len(), Some(BIG as u64));
}

#[test]
fn reading_the_whole_stream_gives_back_exactly_what_was_served() {
    let server = FakeServer::start();
    let content = body(BIG);
    server.on(
        "stream",
        Reply::Ranged {
            body: content.clone(),
        },
    );

    let mut stream = source(&server, "stream").open().expect("open the track");
    let mut read = Vec::new();
    stream.read_to_end(&mut read).expect("read to the end");

    assert_eq!(read.len(), content.len());
    assert!(
        read == content,
        "the bytes read differ from the bytes served"
    );
    // More than one request, i.e. the chunking really happened rather than the
    // whole file arriving in the first fetch and hiding every boundary bug.
    assert!(
        server.request_count() >= 3,
        "expected several range requests for {BIG} bytes, saw {}",
        server.request_count()
    );
}

#[test]
fn a_seek_lands_on_the_right_byte() {
    let server = FakeServer::start();
    let content = body(BIG);
    server.on(
        "stream",
        Reply::Ranged {
            body: content.clone(),
        },
    );
    let mut stream = source(&server, "stream").open().expect("open the track");

    for at in [
        0u64,
        1,
        999,
        512 * 1024 - 1,
        512 * 1024,
        512 * 1024 + 1,
        1_000_000,
    ] {
        stream.seek(SeekFrom::Start(at)).expect("seek");
        let mut buf = [0u8; 16];
        stream.read_exact(&mut buf).expect("read after the seek");
        assert_eq!(
            &buf[..],
            &content[at as usize..at as usize + 16],
            "wrong bytes after seeking to {at}"
        );
    }

    // From the end, which needs the length the server stated.
    stream.seek(SeekFrom::End(-4)).expect("seek from the end");
    let mut tail = Vec::new();
    stream.read_to_end(&mut tail).expect("read the tail");
    assert_eq!(tail, content[content.len() - 4..]);
}

#[test]
fn reading_past_the_end_is_end_of_stream_rather_than_an_error() {
    let server = FakeServer::start();
    let content = body(4096);
    server.on("stream", Reply::Ranged { body: content });
    let mut stream = source(&server, "stream").open().expect("open the track");

    stream.seek(SeekFrom::Start(4096)).expect("seek to the end");
    let mut buf = [0u8; 8];
    assert_eq!(stream.read(&mut buf).expect("read at the end"), 0);
}

/// Decision 7's fallback: a transcoding endpoint ignores the range and sends the
/// whole thing. It must still play, and still seek.
#[test]
fn a_server_that_ignores_ranges_still_plays_and_still_seeks() {
    let server = FakeServer::start();
    let content = body(300_000);
    server.on(
        "stream",
        Reply::Body {
            status: 200,
            content_type: "audio/mpeg",
            body: content.clone(),
        },
    );

    let mut stream = source(&server, "stream").open().expect("open the track");
    assert!(stream.is_seekable(), "a whole body in memory is seekable");
    assert_eq!(stream.byte_len(), Some(content.len() as u64));

    stream.seek(SeekFrom::Start(299_000)).expect("seek");
    let mut tail = Vec::new();
    stream.read_to_end(&mut tail).expect("read the tail");
    assert_eq!(tail, content[299_000..]);

    // And it asked only once: having been sent everything, there is nothing to
    // go back for.
    assert_eq!(server.request_count(), 1);
}

#[test]
fn a_track_that_is_not_there_fails_at_open_rather_than_later() {
    let server = FakeServer::start();
    // Nothing registered for this path, so the fake server answers 404.
    let error = match source(&server, "stream").open() {
        Ok(_) => panic!("opening a missing track should fail"),
        Err(e) => e,
    };
    assert!(
        error.to_string().contains("404"),
        "the error should name the status: {error}"
    );
}

#[test]
fn a_server_that_is_not_running_fails_at_open() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let source = RemoteSource::new(format!("http://127.0.0.1:{port}/rest/stream"), None);
    assert!(source.open().is_err());
}

/// The one that matters: audio decoded over the network is the same audio.
///
/// Seven seconds of stereo WAV is ~1.2MB, so it crosses chunk boundaries -- and
/// every sample carries its own position (the right channel ramps 0 to 1), so a
/// chunk stitched together wrongly is not merely different, it is detectably
/// out of order.
#[test]
fn audio_decoded_over_http_matches_the_same_audio_decoded_locally() {
    let wav = wav_bytes(44_100, 7.0);
    assert!(
        wav.len() > 512 * 1024,
        "the fixture must span several chunks"
    );

    let server = FakeServer::start();
    server.on("stream", Reply::Ranged { body: wav.clone() });

    let local = decode_all(&InMemorySource::new(wav.clone(), Some("wav".into())));
    let remote = decode_all(&RemoteSource::new(
        format!("{}/rest/stream", server.base_url()),
        Some("wav".into()),
    ));

    assert_eq!(local.len(), remote.len(), "different number of samples");
    assert!(!local.is_empty(), "the fixture decoded to nothing");
    let worst = local
        .iter()
        .zip(remote.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert_eq!(worst, 0.0, "decoded audio differs by up to {worst}");
}

/// And that seeking a *decoder* over the network lands where it says it does --
/// the case where a byte-level seek can be right and the container-level one
/// still wrong.
#[test]
fn seeking_a_remote_track_lands_where_it_says() {
    let wav = wav_bytes(44_100, 7.0);
    let server = FakeServer::start();
    server.on("stream", Reply::Ranged { body: wav.clone() });

    let mut decoder = StreamingDecoder::open(&RemoteSource::new(
        format!("{}/rest/stream", server.base_url()),
        Some("wav".into()),
    ))
    .expect("open the remote track");
    assert_eq!(decoder.sample_rate(), 44_100);
    assert_eq!(decoder.channels(), 2);

    decoder.seek(3.5).expect("seek to the middle");
    let block = decoder
        .next_block()
        .expect("decode after the seek")
        .expect("there should be audio at 3.5s");

    // The right channel ramps 0 -> 1 across 7 seconds, so halfway through it
    // reads ~0.5. That is a claim about *which* audio came back, which a
    // byte-range bug would fail even when the byte count was right.
    let ramp: Vec<f32> = block.iter().skip(1).step_by(2).copied().collect();
    let mean = ramp.iter().sum::<f32>() / ramp.len() as f32;
    assert!(
        (mean - 0.5).abs() < 0.02,
        "audio after seeking to 3.5s of 7s reads {mean:.3}, wanted ~0.5"
    );
}

/// A network that stalls is a delay, not damage.
///
/// This is the failure a local file cannot have, and the one the phase's
/// decision 8 called out as playback's only exception: partway through a track
/// the bytes stop coming for a while. What the engine does with that is already
/// settled and is not new here -- an empty ring with the track not ended is an
/// underrun, and the callback answers it with silence and a held position
/// (`Shared::finished`'s note in playback.rs), the same as a disk that is slow.
/// What this checks is the half that *is* new: that the source waits and then
/// carries on, so what eventually reaches the decoder is the whole track in the
/// right order rather than a gap stitched over.
#[test]
fn a_stalled_network_delays_the_audio_rather_than_breaking_it() {
    let wav = wav_bytes(44_100, 7.0);
    let server = FakeServer::start();
    server.on(
        "stream",
        Reply::Stalling {
            body: wav.clone(),
            delay: std::time::Duration::from_millis(250),
        },
    );

    let started = std::time::Instant::now();
    let stalled = decode_all(&RemoteSource::new(
        format!("{}/rest/stream", server.base_url()),
        Some("wav".into()),
    ));
    let elapsed = started.elapsed();
    let local = decode_all(&InMemorySource::new(wav, Some("wav".into())));

    assert_eq!(local.len(), stalled.len(), "different number of samples");
    let worst = local
        .iter()
        .zip(stalled.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert_eq!(
        worst, 0.0,
        "audio through a stalling server differs by {worst}"
    );
    // And the stall really happened rather than the whole track arriving in the
    // first, un-delayed response -- otherwise this passes for the wrong reason.
    assert!(
        elapsed >= std::time::Duration::from_millis(250),
        "expected the delayed chunks to show up in the time taken, took {elapsed:?}"
    );
}

/// Decodes a whole source into interleaved samples, the way the decoder thread
/// would drain it.
fn decode_all(source: &dyn PlaybackSource) -> Vec<f32> {
    let mut decoder = StreamingDecoder::open(source).expect("open the source");
    let mut all = Vec::new();
    while let Some(block) = decoder.next_block().expect("decode a block") {
        all.extend_from_slice(block);
    }
    all
}
