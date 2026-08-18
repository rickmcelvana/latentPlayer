//! The desktop shell's Subsonic transport: one blocking GET, one parsed
//! response. Phase 2 M1.
//!
//! Blocking on purpose. Every caller here is a thread that is allowed to
//! block -- a Tauri command on the blocking pool, or (from M2) the decoder
//! thread -- and none of them is the audio callback. An async client would buy
//! nothing and cost this crate an executor it does not otherwise have.
//!
//! What this file is *not* allowed to become is a second opinion about the
//! protocol: it moves bytes and hands them to [`super::parse`]. Where a
//! response means something, it means it in `mod.rs`, so the web shell -- whose
//! bytes arrive through `fetch` instead -- reaches the same conclusion.

use std::time::Duration;

use serde::de::DeserializeOwned;

use super::browse::{cover_art_params, parse_browse, BrowseRequest, BrowseResult, TrackRow};
use super::playlists::{parse_playlists, PlaylistRequest, PlaylistResult};
use super::scrobble::scrobble_params;
use super::stars::{parse_mark, MarkRequest};
use super::{
    request_url, salt_hex, stream_params, Credentials, Extensions, NoPayload, Parsed, RandomSongs,
    Server, ServerInfo, SubsonicError,
};
use crate::remote::RemoteSource;

/// How long any one request may take, end to end.
///
/// Generous rather than snappy: this is someone's own server, possibly a
/// Raspberry Pi with a cold cache, reached over a home network or a VPN. What it
/// protects against is not slowness but a connection that will never answer at
/// all, which without a timeout hangs the calling thread forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Most a metadata response may be before this refuses to hold it.
///
/// A browse response is kilobytes; a whole library could be megabytes. 32MB is
/// far past either and still a bound, which is the point -- an unbounded read
/// from a server that is not the server we think it is (a captive portal, a
/// proxy error page, a misconfigured reverse proxy streaming a video) is how a
/// client ends up allocating until it dies. Audio does not come through here:
/// M2's `RemoteSource` streams it.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// Bytes of salt per request. The spec's floor is six *characters*; this is
/// sixteen bytes, i.e. 32 hex characters of real randomness.
const SALT_BYTES: usize = 16;

/// Most one piece of cover art may be (M3). A thumbnail asked for at a size is
/// tens of kilobytes; this is far above that and still a bound, for the same
/// reason [`MAX_RESPONSE_BYTES`] is one -- what arrives is not always what was
/// asked for.
const MAX_COVER_BYTES: u64 = 16 * 1024 * 1024;

/// One image, with the type the server said it is.
///
/// Carried as bytes rather than written to a file because each shell puts it
/// somewhere different: the desktop shell answers a `cover://` request with it,
/// the web shell makes a blob URL. Neither sends it through JSON.
pub struct CoverArt {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

/// Its size and type rather than its bytes: a derived `Debug` on an image prints
/// a hundred kilobytes of numbers into whatever was logging it.
impl std::fmt::Debug for CoverArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CoverArt({}, {} bytes)",
            self.content_type,
            self.bytes.len()
        )
    }
}

/// A connection to one server, with the credentials to use on it.
///
/// Cheap to clone in the sense that matters -- the `ureq::Agent` inside pools
/// connections, so a clone shares the pool and a second request to the same
/// server reuses the first one's TCP and TLS handshake rather than paying for
/// them again. Cloning is what lets a caller take the client out from under a
/// lock and then spend a network round trip without holding it (M3's cover art
/// does exactly that: a gridful of thumbnails must not queue up behind one
/// mutex).
#[derive(Clone)]
pub struct HttpClient {
    agent: ureq::Agent,
    server: Server,
    credentials: Credentials,
}

impl HttpClient {
    pub fn new(server: Server, credentials: Credentials) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .tls_config(platform_tls())
            .build();
        Self {
            agent: config.into(),
            server,
            credentials,
        }
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Calls `endpoint` and parses what comes back.
    pub fn get<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<Parsed<T>, SubsonicError> {
        let url = request_url(
            &self.server,
            &self.credentials,
            endpoint,
            params,
            &fresh_salt()?,
        )?;
        let bytes = self.fetch(&url)?;
        super::parse(&bytes)
    }

    /// `ping` -- the "are you there, and do you know me" call.
    ///
    /// Returns what the server said about itself, which is what the UI shows to
    /// prove the connection is to the server the user meant.
    pub fn ping(&self) -> Result<ServerInfo, SubsonicError> {
        Ok(self.get::<NoPayload>("ping", &[])?.info)
    }

    /// `getOpenSubsonicExtensions`.
    ///
    /// A server that has never heard of it answers with an error envelope
    /// rather than a payload, and that is not a failure to connect -- it is an
    /// old server saying "no extensions". Reported as an empty list, so callers
    /// ask "does it support X" rather than "did the question work".
    pub fn extensions(&self) -> Result<Extensions, SubsonicError> {
        match self.get::<Extensions>("getOpenSubsonicExtensions", &[]) {
            Ok(parsed) => Ok(parsed.payload),
            Err(SubsonicError::Server { .. }) | Err(SubsonicError::Malformed(_)) => {
                Ok(Extensions::default())
            }
            Err(other) => Err(other),
        }
    }

    /// A handful of songs at random (Phase 2 M2).
    ///
    /// Not a browse API -- that is M3 -- but the smallest call that produces
    /// real, playable track ids, which is what M2 needs to prove that remote
    /// playback works at all.
    pub fn random_songs(&self, count: u32) -> Result<Vec<TrackRow>, SubsonicError> {
        let size = count.to_string();
        Ok(self
            .get::<RandomSongs>("getRandomSongs", &[("size", size.as_str())])?
            .payload
            .inner
            .song
            .into_iter()
            .map(TrackRow::from)
            .collect())
    }

    /// One browse request (Phase 2 M3): artists, one artist's albums, a page of
    /// albums, one album's tracks, or a search.
    ///
    /// Which endpoint that is and what it must carry are [`BrowseRequest`]'s
    /// business, and what comes back is [`parse_browse`]'s -- so this method
    /// knows nothing about any of the five and cannot disagree with the web
    /// shell, which calls the same two functions through wasm.
    pub fn browse(&self, request: &BrowseRequest) -> Result<BrowseResult, SubsonicError> {
        let url = self.url_for(request.endpoint(), &request.params())?;
        Ok(parse_browse(request, &self.fetch(&url)?)?.payload)
    }

    /// One playlist request (Phase 2 M4): the two reads and the four writes.
    ///
    /// One method for all six, like [`Self::browse`] -- which endpoint it is,
    /// what it carries and where the answer hides its list are
    /// `player_core::subsonic::playlists`' business on both shells.
    pub fn playlists(&self, request: &PlaylistRequest) -> Result<PlaylistResult, SubsonicError> {
        let bytes = self.fetch(&self.url_for(request.endpoint(), &request.params())?)?;
        Ok(parse_playlists(request, &bytes)?.payload)
    }

    /// One star, unstar or rating (Phase 2 M4).
    ///
    /// Answers with nothing on success -- what is being checked is the
    /// envelope, since a Subsonic refusal is an HTTP 200 and a write that
    /// quietly did nothing is this milestone's worst available failure.
    pub fn mark(&self, request: &MarkRequest) -> Result<(), SubsonicError> {
        let bytes = self.fetch(&self.url_for(request.endpoint(), &request.params())?)?;
        parse_mark(&bytes)?;
        Ok(())
    }

    /// One scrobble (Phase 2 M4): `submission=false` is "now playing",
    /// `submission=true` is a play.
    ///
    /// *When* to call this is not decided here -- that is
    /// `subsonic::scrobble::ScrobbleTracker`, which both shells drive so they
    /// cannot disagree about what a play is (decision 10).
    pub fn scrobble(
        &self,
        id: &str,
        submission: bool,
        time_ms: Option<u64>,
    ) -> Result<(), SubsonicError> {
        let params = scrobble_params(id, submission, time_ms);
        let bytes = self.fetch(&self.url_for("scrobble", &params)?)?;
        parse_mark(&bytes)?;
        Ok(())
    }

    /// A URL for an endpoint whose parameters are owned strings, which is what
    /// every request type in this protocol produces (they render numbers).
    fn url_for(&self, endpoint: &str, owned: &[(String, String)]) -> Result<String, SubsonicError> {
        let params: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        request_url(
            &self.server,
            &self.credentials,
            endpoint,
            &params,
            &fresh_salt()?,
        )
    }

    /// One piece of cover art, scaled by the server.
    ///
    /// The odd one out among these calls: it answers with an *image*, not an
    /// envelope, so there is nothing to parse and the usual "a failure is an
    /// HTTP 200" rule needs its own handling -- a server that cannot find the
    /// art sends a document rather than a picture, which this turns into an
    /// error instead of handing the webview bytes it will not draw.
    pub fn cover_art(&self, id: &str, size: u32) -> Result<CoverArt, SubsonicError> {
        let url = self.url_for("getCoverArt", &cover_art_params(id, size))?;

        let mut response = self.agent.get(&url).call().map_err(classify)?;
        let content_type = response
            .headers()
            .get(ureq::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_COVER_BYTES)
            .read_to_vec()
            .map_err(|e| SubsonicError::Transport(format!("reading the cover failed: {e}")))?;

        if !content_type.starts_with("image/") {
            // The spec says an error here is an XML document; Navidrome sends
            // the ordinary JSON envelope instead. So the envelope is tried
            // first, because it carries the server's own words -- and anything
            // that is neither an image nor an envelope (a proxy's login page,
            // say) is reported by naming what did arrive, which is the only
            // useful thing left to say about it.
            return Err(match super::parse::<NoPayload>(&bytes) {
                Err(server @ SubsonicError::Server { .. }) => server,
                _ => {
                    SubsonicError::Malformed(format!("asked for cover art and got {content_type}"))
                }
            });
        }
        Ok(CoverArt {
            bytes,
            content_type,
        })
    }

    /// A `PlaybackSource` for one track on this server.
    ///
    /// The URL carries its own authentication -- for this protocol that is what
    /// a credential *is* -- so the source needs nothing else from the client and
    /// can be handed to the engine, which has no idea a network is involved.
    ///
    /// Note the salt is fixed at the moment the URL is built rather than per
    /// request: the URL is used for the life of the track, including its range
    /// requests. That is how every Subsonic client streams, and the token stays
    /// valid because it is a hash of a password that has not changed.
    pub fn track_source(&self, id: &str, suffix: &str) -> Result<RemoteSource, SubsonicError> {
        let url = request_url(
            &self.server,
            &self.credentials,
            "stream",
            &stream_params(id),
            &fresh_salt()?,
        )?;
        let hint = (!suffix.is_empty()).then(|| suffix.to_string());
        Ok(RemoteSource::new(url, hint))
    }

    /// One GET, with the transport's own failures translated into this crate's
    /// error type -- so a caller matches on `SubsonicError` and never on
    /// `ureq::Error`.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, SubsonicError> {
        let mut response = self.agent.get(url).call().map_err(classify)?;

        response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|e| SubsonicError::Transport(format!("reading the response failed: {e}")))
    }
}

/// TLS that trusts what the OS trusts (M5 decision 16).
///
/// Not the default, and the difference is the whole point. ureq's default is
/// rustls with **webpki-roots** -- Mozilla's root bundle, compiled into the
/// binary -- under which a self-hoster who installs their own CA in the Windows
/// certificate store, the macOS keychain or `/etc/ssl/certs` changes nothing
/// whatsoever. That is the normal situation on the boxes this phase exists to
/// reach, so the honest diagnosis under the old default ("install the
/// certificate") would have been advice that does not work.
///
/// The alternative considered and rejected was pinning a fingerprint the user
/// confirms once. It is a security-sensitive thing to get right, it asks a
/// listener to compare a hex string they have no safe way to obtain, and it is
/// unnecessary the moment the OS store is the authority -- adding a CA there is
/// something the person running the server has already done for their browser.
pub fn platform_tls() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build()
}

/// One `ureq::Error`, as this crate's error type (M5).
///
/// The one place ureq's twenty-nine variants become the handful of things a
/// person can actually do something about -- so `remote.rs` and this module
/// classify a failure identically, and neither decides by reading message text.
pub fn classify(error: ureq::Error) -> SubsonicError {
    match error {
        // 4xx/5xx: a real HTTP failure, which for this protocol means something
        // *other* than the application talking -- a proxy, a wrong path, an
        // auth layer in front of the server. Subsonic's own failures arrive as
        // 200s (see `mod.rs`).
        ureq::Error::StatusCode(status) => SubsonicError::Http { status },
        // Every way TLS can refuse. Its own kind because it is the one network
        // failure with a specific cause and a specific fix, and because
        // retrying it is pointless by definition.
        ureq::Error::Tls(what) => SubsonicError::Certificate(what.to_string()),
        ureq::Error::Rustls(e) => SubsonicError::Certificate(e.to_string()),
        ureq::Error::Pem(e) => SubsonicError::Certificate(e.to_string()),
        ureq::Error::TlsRequired => SubsonicError::Certificate(
            "the server offered an unencrypted connection where https was required".into(),
        ),
        ureq::Error::HostNotFound => {
            SubsonicError::Transport("that host could not be found".into())
        }
        ureq::Error::Timeout(what) => {
            SubsonicError::Transport(format!("the server did not answer in time ({what})"))
        }
        other => SubsonicError::Transport(other.to_string()),
    }
}

/// A fresh salt from the OS.
///
/// Decision 3: randomness belongs to the shell, hashing to the protocol. This is
/// the native half of that; the web half is `crypto.getRandomValues`.
fn fresh_salt() -> Result<String, SubsonicError> {
    let mut bytes = [0u8; SALT_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|e| SubsonicError::Transport(format!("no randomness for the auth salt: {e}")))?;
    Ok(salt_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_salt_is_fresh_and_long_enough() {
        let first = fresh_salt().expect("the OS should have randomness");
        let second = fresh_salt().expect("the OS should have randomness");
        assert_eq!(first.len(), SALT_BYTES * 2);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        // Not a statistical test -- just the one that catches a "random" salt
        // that is actually a constant, which is the failure that would silently
        // hand every request the same token.
        assert_ne!(first, second);
    }
}
