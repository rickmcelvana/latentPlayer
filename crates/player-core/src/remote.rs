//! A `PlaybackSource` that reads a track over HTTP (Phase 2 M2).
//!
//! Deliberately knows nothing about Subsonic. It is handed a URL that already
//! carries whatever authentication it needs -- Subsonic's is query parameters,
//! so a URL is the whole credential -- and its job is to make that URL behave
//! like a file: `Read + Seek`, with a length and an honest answer to "can I
//! seek this". `subsonic::http` builds the URL; this moves the bytes.
//!
//! # Why this can block
//!
//! Every read here is a network round trip in the worst case, and that is fine
//! *where it happens*: the decoder thread (native) is allowed to block, and the
//! two seconds of audio in the ring ahead of it are exactly the margin that
//! covers a fetch. What must never happen is a read on the audio callback, and
//! nothing here is reachable from it.
//!
//! # The two shapes a server answers in
//!
//! One request settles both. The first read asks for `Range: bytes=0-...`:
//!
//! - **206 Partial Content** -- the server ranges. The total length comes from
//!   `Content-Range`, seeking is real, and memory is one chunk.
//! - **200 OK** -- the server ignored the range and is sending the whole thing.
//!   That is the transcoding case decision 7 warns about, and there is nothing
//!   clever to do: the body is buffered whole, which is still seekable and still
//!   plays, at the cost of holding the track in memory. Bounded, because a
//!   server that streams forever must not take the process with it.

#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

use symphonia::core::io::MediaSource;

use crate::error::PlayerError;
use crate::source::PlaybackSource;
use crate::subsonic::SubsonicError;

/// How much is fetched per range request.
///
/// A compromise measured in round trips against memory: symphonia reads in
/// kilobytes, so serving it a request at a time would be hundreds of round
/// trips per track. 512KB is ~13 seconds of a 320kbps MP3 and ~5 of a CD-rate
/// FLAC -- comfortably more than the ring holds, so in steady state the decoder
/// blocks on the network about once every several seconds rather than
/// continuously.
const CHUNK_BYTES: u64 = 512 * 1024;

/// Most that will be held for a server that refuses to range.
///
/// Generous (a 24-bit/96kHz FLAC track runs ~35MB) but finite: without a bound,
/// a misconfigured server streaming something endless would be answered by
/// allocating until the process died.
const MAX_WHOLE_BYTES: u64 = 512 * 1024 * 1024;

/// Longer than the metadata timeout in `subsonic::http`: this one covers a
/// *body*, and the first chunk of a track on a slow link legitimately takes
/// longer than a JSON response ever should.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait before each retry of a chunk that failed (M5 decision 14).
///
/// Three attempts after the first, backing off, which buys about four seconds
/// of patience on top of the timeouts themselves. Long enough for the hiccups
/// that actually happen -- a wifi roam, a VPN reconnecting, a server paging
/// something back in -- and short enough that a server which is really gone is
/// reported while the listener still remembers pressing play.
///
/// What gets retried is `SubsonicError::is_transient`'s business, and it is
/// narrow on purpose: a 4xx is never retried, because a token the server
/// rejects is still rejected three seconds later and retrying it only delays
/// telling someone what they have to fix.
const RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(3),
];

/// A track on a server, identified by an already-authenticated URL.
pub struct RemoteSource {
    url: String,
    /// What the file is called, or enough of it to hint the container. Subsonic
    /// does not put an extension in the URL, so this comes from the track's
    /// metadata (`suffix`) rather than from the address.
    extension_hint: Option<String>,
    agent: ureq::Agent,
}

impl RemoteSource {
    pub fn new(url: impl Into<String>, extension_hint: Option<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            // The same trust store the metadata client uses (M5 decision 16) --
            // it would be its own kind of bug for a server to browse and then
            // refuse to stream over a certificate question.
            .tls_config(crate::subsonic::http::platform_tls())
            .build();
        Self {
            url: url.into(),
            extension_hint,
            agent: config.into(),
        }
    }

    /// The URL this reads, for diagnostics. Carries credentials, so it is not
    /// something to log casually.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl PlaybackSource for RemoteSource {
    fn open(&self) -> Result<Box<dyn MediaSource>, PlayerError> {
        Ok(Box::new(HttpStream::open(
            self.agent.clone(),
            self.url.clone(),
        )?))
    }

    fn extension_hint(&self) -> Option<&str> {
        self.extension_hint.as_deref()
    }
}

/// One open read of one URL.
struct HttpStream {
    agent: ureq::Agent,
    url: String,
    /// Total size, when the server stated one.
    len: Option<u64>,
    /// Where the next read starts. Moved by `seek` without any IO -- the fetch
    /// happens on the read that needs the bytes, which is what makes symphonia's
    /// habit of seeking and then reading a few bytes cheap.
    pos: u64,
    /// The chunk in hand, and where it starts in the stream.
    buffer: Vec<u8>,
    buffer_start: u64,
    /// Set when the server would not range and the whole body is in `buffer`.
    whole: bool,
}

impl HttpStream {
    fn open(agent: ureq::Agent, url: String) -> Result<Self, PlayerError> {
        let mut stream = Self {
            agent,
            url,
            len: None,
            pos: 0,
            buffer: Vec::new(),
            buffer_start: 0,
            whole: false,
        };
        // Eagerly, so that "this track cannot be played" is an error from
        // `open` -- where the caller can report it against a track -- rather
        // than a decode failure later.
        stream.fetch(0)?;
        Ok(stream)
    }

    /// Fetches the chunk containing `at`, retrying a failure that could
    /// plausibly go away (M5 decision 14).
    ///
    /// This is the difference between a five-second outage and a skipped track.
    /// Before M5 a single failed read was fatal to the track: the decoder treats
    /// a broken source the way it treats a corrupt file, by moving on to
    /// whatever is queued. While this is retrying, the ring drains and the
    /// callback plays silence with the position held -- which is exactly the
    /// state `Playback::is_buffering` now reports, so the wait is something the
    /// listener can see rather than a mute pause.
    fn fetch(&mut self, at: u64) -> Result<(), PlayerError> {
        let mut attempt = 0;
        loop {
            let failure = match self.fetch_once(at) {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            // Asked in this order deliberately: something that will never
            // work must be reported as itself, not as "gave up after four
            // attempts" -- which would describe patience this never spent.
            if !failure.is_transient() {
                return Err(PlayerError::Remote(failure.to_string()));
            }
            let Some(backoff) = RETRY_BACKOFF.get(attempt) else {
                return Err(PlayerError::Remote(format!(
                    "{failure} (gave up after {} attempts)",
                    RETRY_BACKOFF.len() + 1
                )));
            };
            std::thread::sleep(*backoff);
            attempt += 1;
        }
    }

    /// One attempt at the chunk containing `at`, replacing whatever is buffered.
    fn fetch_once(&mut self, at: u64) -> Result<(), SubsonicError> {
        if self.whole {
            // The whole body is already here; there is nothing to fetch and
            // nothing that could arrive.
            return Ok(());
        }

        let last = at.saturating_add(CHUNK_BYTES).saturating_sub(1);
        let mut response = self
            .agent
            .get(&self.url)
            .header("Range", format!("bytes={at}-{last}"))
            .call()
            // The same classification the metadata client uses, so "is this
            // worth trying again" has one answer in this crate rather than two.
            .map_err(crate::subsonic::http::classify)?;

        let status = response.status().as_u16();
        let content_range = response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        // 206 means the range was honoured; anything else 2xx means the server
        // sent the whole body regardless of what was asked for.
        let ranged = status == 206;
        // Slack above what was asked for, deliberately. This limit is a guard
        // against a server that answers a range with something enormous, not a
        // statement about the chunk size -- and a server is entitled to return
        // a *wider* range than requested. Sized exactly, it rejects the very
        // response it asked for: 512KB inclusive is 524288 bytes, which ureq
        // counts as over a 524288-byte limit.
        let limit = if ranged {
            CHUNK_BYTES + 64 * 1024
        } else {
            MAX_WHOLE_BYTES
        };
        let body = response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            // A body that stops arriving mid-chunk is exactly the hiccup this
            // milestone exists to survive, so it is transport rather than
            // something final.
            .map_err(|e| SubsonicError::Transport(format!("reading the track failed: {e}")))?;

        if ranged {
            if self.len.is_none() {
                self.len = content_range.as_deref().and_then(total_from_content_range);
            }
            self.buffer = body;
            self.buffer_start = at;
        } else {
            // Not rangeable. Everything the server sent is everything there is,
            // so keep it and stop asking (decision 7's fallback).
            if body.len() as u64 >= MAX_WHOLE_BYTES {
                // Not transient: asking again gets the same enormous body.
                return Err(SubsonicError::Malformed(format!(
                    "the server will not serve byte ranges and the track is larger than \
                     {MAX_WHOLE_BYTES} bytes, so it cannot be buffered whole"
                )));
            }
            self.len = Some(body.len() as u64);
            self.buffer = body;
            self.buffer_start = 0;
            self.whole = true;
        }
        Ok(())
    }

    /// Whether `pos` is inside the buffer currently held.
    fn buffered(&self) -> bool {
        self.pos >= self.buffer_start && self.pos < self.buffer_start + self.buffer.len() as u64
    }
}

impl Read for HttpStream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if let Some(len) = self.len {
            if self.pos >= len {
                return Ok(0);
            }
        }
        if !self.buffered() {
            self.fetch(self.pos)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            // A server that answers a range past the end with an empty body is
            // at end of stream, not broken.
            if !self.buffered() {
                return Ok(0);
            }
        }

        let offset = (self.pos - self.buffer_start) as usize;
        let available = &self.buffer[offset..];
        let n = available.len().min(out.len());
        out[..n].copy_from_slice(&available[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for HttpStream {
    /// Moves the cursor and nothing else -- no request is made until something
    /// is read. Symphonia seeks by probing (read a little, seek, read a little),
    /// and a fetch per seek would make every probe a round trip.
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.pos as i128 + n as i128,
            SeekFrom::End(n) => match self.len {
                Some(len) => len as i128 + n as i128,
                None => return Err(std::io::Error::other(
                    "cannot seek from the end of a stream whose length the server did not state",
                )),
            },
        };
        if target < 0 {
            return Err(std::io::Error::other("seek before the start of the track"));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

impl MediaSource for HttpStream {
    /// A ranged stream is seekable; so, trivially, is one held whole in memory.
    /// The only unseekable case is a server that neither ranges nor states a
    /// length, which this reports honestly so symphonia does not try.
    fn is_seekable(&self) -> bool {
        self.whole || self.len.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

/// The total size out of a `Content-Range: bytes 0-511/12345` header.
///
/// `*` for the total is legal and means the server will not say, which is
/// treated as unknown rather than as zero -- the difference between "seek
/// anywhere" and "seek nowhere".
fn total_from_content_range(value: &str) -> Option<u64> {
    let (_, total) = value.rsplit_once('/')?;
    total.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_content_range_states_the_total() {
        assert_eq!(total_from_content_range("bytes 0-511/12345"), Some(12345));
        assert_eq!(total_from_content_range("bytes 512-1023/1024"), Some(1024));
    }

    #[test]
    fn an_unknown_total_is_unknown_rather_than_zero() {
        assert_eq!(total_from_content_range("bytes 0-511/*"), None);
        assert_eq!(total_from_content_range("nonsense"), None);
        assert_eq!(total_from_content_range(""), None);
    }
}
