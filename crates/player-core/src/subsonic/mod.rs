//! The Subsonic/OpenSubsonic protocol: what a request *is* and what a response
//! *says*. Phase 2 M1.
//!
//! # What is deliberately not here
//!
//! Any IO at all. This module builds URLs and parses bytes; the desktop shell
//! performs the request with a blocking HTTP client on a thread that is allowed
//! to block, and the web shell performs it with `fetch` in the browser and hands
//! the bytes back here to parse (tasks/phase-2.md decision 2). One definition of
//! the protocol, two ways of moving the bytes -- the same split M8 made for the
//! EQ, and for the same reason: two implementations of an error code is two
//! shells that disagree about what went wrong.
//!
//! Randomness is likewise the caller's (decision 3). Token auth needs a fresh
//! salt per request, and `getrandom` on wasm needs its own feature flag and a
//! build-time opinion; each shell already has a perfectly good RNG, so it passes
//! one in and this module does the hashing.
//!
//! # The trap this module exists to not fall into
//!
//! **A Subsonic error is an HTTP 200.** The status line says nothing; the
//! envelope inside says `"status": "failed"` and carries a code. Captured from
//! the Navidrome this was built against, unauthenticated:
//!
//! ```json
//! {"subsonic-response":{"status":"failed","version":"1.16.1","type":"navidrome",
//!  "serverVersion":"0.63.2","openSubsonic":true,
//!  "error":{"code":10,"message":"missing parameter: 'u'"}}}
//! ```
//!
//! A client that checks the HTTP status believes every failure succeeded, which
//! is why [`parse`] is the only way this module hands back a payload and why it
//! returns `Err` for the body above.

/// The desktop shell's transport: blocking HTTP, on threads that may block.
/// Native-only, exactly as `playback.rs` (cpal) and `library.rs` (paths) are --
/// the web build has `fetch` and needs no HTTP client compiled into wasm.
#[cfg(not(target_arch = "wasm32"))]
pub mod http;

/// Browsing the server's library (Phase 2 M3): the five requests, and the five
/// payload shapes their answers arrive in. Cross-target, like this module.
pub mod browse;

/// The server's playlists (Phase 2 M4), reading and writing.
pub mod playlists;

/// Scrobbling (Phase 2 M4), including the rule for when a play counts.
pub mod scrobble;

/// Stars and ratings (Phase 2 M4).
pub mod stars;

pub use browse::{
    cover_art_params, parse_browse, AlbumRow, AlbumSort, ArtistRow, BrowseRequest, BrowseResult,
    TrackRow, COVER_SIZE_PX,
};
pub use playlists::{parse_playlists, PlaylistRequest, PlaylistResult, PlaylistRow};
pub use scrobble::{scrobble_params, ScrobbleAction, ScrobbleRequest, ScrobbleTracker};
pub use stars::{parse_mark, MarkRequest, StarTarget};

use serde::de::DeserializeOwned;
use serde::Deserialize;

/// What this client calls itself to servers (`c`). Subsonic requires it, logs
/// it, and shows it in the server's own "clients" UI, so it is the app's name
/// rather than something internal.
pub const CLIENT_NAME: &str = "latentPlayer";

/// The protocol version this client speaks (`v`). 1.16.1 is the last Subsonic
/// version and the one OpenSubsonic builds on; Navidrome reports it too.
///
/// Note what a version does *not* buy: servers implement subsets of it, so
/// capability is discovered per endpoint (`getOpenSubsonicExtensions`, and an
/// error 30/70 from a call) rather than assumed from this number.
pub const API_VERSION: &str = "1.16.1";

/// Shortest salt the spec allows. Ours are longer -- see [`salt_hex`].
const MIN_SALT_CHARS: usize = 6;

/// A server, as a normalised base URL with no trailing slash and no `/rest`.
///
/// Normalising once here rather than at each call site is what stops
/// `http://host:4533/` and `http://host:4533/rest/` producing a double slash or
/// a doubled path segment -- both of which some reverse proxies answer with a
/// redirect and others with a 404.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    base: String,
}

impl Server {
    /// Takes a user-typed URL.
    ///
    /// The scheme is **required**, deliberately. Guessing it is a security
    /// decision made on the user's behalf: guess `http` and a client that could
    /// have been encrypted is not, guess `https` and a plain self-hosted box
    /// looks broken. It is also the difference between a web build that can
    /// reach the server and one that cannot (a page on `https:` may not talk to
    /// `http:` at all), so the answer belongs to whoever knows their own setup.
    pub fn new(url: &str) -> Result<Self, SubsonicError> {
        let trimmed = url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(SubsonicError::BadUrl("no server address given".into()));
        }
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err(SubsonicError::BadUrl(format!(
                "{trimmed} needs a scheme -- write http://{trimmed} or https://{trimmed}"
            )));
        }
        // A user who pasted the address they use in a browser may well have
        // brought `/rest` with them; taking it off is kinder than a 404.
        let base = lower
            .strip_suffix("/rest")
            .map(|_| &trimmed[..trimmed.len() - "/rest".len()])
            .unwrap_or(trimmed);
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
        })
    }

    /// The base URL as normalised, for display and for storing.
    pub fn base_url(&self) -> &str {
        &self.base
    }
}

/// How this client proves who it is.
///
/// Two shapes because servers offer two. The password variant is not a
/// preference: Subsonic's token scheme hashes the password *per request*, so a
/// client speaking it must hold the password itself -- there is no
/// derived-secret-at-rest. OpenSubsonic's API-key extension is what removes
/// that, where a server implements it (the Navidrome this was built against,
/// 0.63.2, does not).
#[derive(Debug, Clone)]
pub enum Credentials {
    Password { user: String, password: String },
    ApiKey { key: String },
}

/// Hex for a salt, from randomness the caller supplies (decision 3).
///
/// Hex rather than base64 so the salt is URL-safe without encoding, and so the
/// same bytes always produce the same string -- a salt that needed escaping
/// would be a token computed over one string and sent as another.
pub fn salt_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `md5(password + salt)`, lowercase hex -- the spec's token.
///
/// Checked against the spec's own worked example rather than against itself:
/// see this module's tests.
pub fn auth_token(password: &str, salt: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    let digest = hasher.finalize();
    salt_hex(&digest)
}

/// The full GET URL for one call.
///
/// `salt` is only read for password credentials, and must be at least
/// [`MIN_SALT_CHARS`] characters -- a short salt is the spec's own floor and is
/// worth refusing here rather than sending.
pub fn request_url(
    server: &Server,
    credentials: &Credentials,
    endpoint: &str,
    params: &[(&str, &str)],
    salt: &str,
) -> Result<String, SubsonicError> {
    let mut url = format!("{}/rest/{endpoint}", server.base);
    let mut query: Vec<(String, String)> = Vec::new();

    match credentials {
        Credentials::Password { user, password } => {
            if salt.chars().count() < MIN_SALT_CHARS {
                return Err(SubsonicError::BadUrl(format!(
                    "salt must be at least {MIN_SALT_CHARS} characters"
                )));
            }
            query.push(("u".into(), user.clone()));
            query.push(("t".into(), auth_token(password, salt)));
            query.push(("s".into(), salt.to_string()));
        }
        // The extension is explicit that a client sending `apiKey` must not
        // also send `u`; a server seeing both is entitled to reject the pair.
        Credentials::ApiKey { key } => query.push(("apiKey".into(), key.clone())),
    }

    query.push(("v".into(), API_VERSION.into()));
    query.push(("c".into(), CLIENT_NAME.into()));
    // JSON rather than the protocol's default XML: this app has serde_json
    // already (the library index) and no XML parser, and every server that
    // speaks 1.16.1 speaks JSON.
    query.push(("f".into(), "json".into()));
    for (key, value) in params {
        query.push(((*key).to_string(), (*value).to_string()));
    }

    url.push('?');
    for (i, (key, value)) in query.iter().enumerate() {
        if i > 0 {
            url.push('&');
        }
        url.push_str(&encode(key));
        url.push('=');
        url.push_str(&encode(value));
    }
    Ok(url)
}

/// Percent-encodes one query component.
///
/// Hand-rolled against a deliberately small rule -- everything outside
/// unreserved characters is escaped, including the ones a lazier encoder leaves
/// alone (`+`, `/`, `:`) -- because the alternative is a password containing `&`
/// or `+` silently authenticating as something else. `+` matters especially:
/// as a query value it means a space to many servers.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// What every response carries, whether it succeeded or not.
///
/// Worth keeping rather than discarding: `server_type`/`server_version` are what
/// the UI shows to prove *which* server answered, and `open_subsonic` is the
/// one-bit answer to "is it worth asking about extensions at all".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerInfo {
    /// The API version the server speaks, e.g. `1.16.1`.
    #[serde(default)]
    pub version: String,
    /// The implementation, e.g. `navidrome`. Absent on older servers.
    #[serde(rename = "type", default)]
    pub server_type: String,
    /// The implementation's own version, e.g. `0.63.2`. Absent on older servers.
    #[serde(rename = "serverVersion", default)]
    pub server_version: String,
    /// Whether the server claims OpenSubsonic.
    #[serde(rename = "openSubsonic", default)]
    pub open_subsonic: bool,
}

/// A parsed response: what the server said about itself, plus the payload the
/// endpoint returns.
#[derive(Debug, Clone)]
pub struct Parsed<T> {
    pub info: ServerInfo,
    pub payload: T,
}

/// The payload of an endpoint that returns nothing but success -- `ping`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NoPayload {}

/// One OpenSubsonic extension the server advertises.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Extension {
    pub name: String,
    /// Which versions of that extension, newest not necessarily last.
    #[serde(default)]
    pub versions: Vec<u32>,
}

/// `getOpenSubsonicExtensions`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Extensions {
    #[serde(rename = "openSubsonicExtensions", default)]
    pub extensions: Vec<Extension>,
}

impl Extensions {
    /// The name of the extension that lets this client stop holding a password.
    pub const API_KEY: &'static str = "apiKeyAuthentication";

    pub fn supports(&self, name: &str) -> bool {
        self.extensions.iter().any(|e| e.name == name)
    }

    /// Whether [`Credentials::ApiKey`] is worth offering for this server.
    pub fn supports_api_key(&self) -> bool {
        self.supports(Self::API_KEY)
    }
}

/// One track on the server, as much of it as playing one needs (Phase 2 M2).
///
/// Browsing proper is M3's; this is the subset every browse response carries,
/// so the same struct will serve it. `suffix` earns its place here: Subsonic's
/// stream URL has no extension in it, and symphonia's probe wants a hint --
/// without it a bare AAC stream is ambiguous to the format reader.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Song {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    /// The file's own extension on the server, e.g. `flac`.
    #[serde(default)]
    pub suffix: String,
    /// Seconds, as the server states them. Absent on some servers, which is why
    /// the transport still measures its own position from what it played.
    #[serde(default)]
    pub duration: Option<u32>,
    /// Bytes, which is how a caller can tell an unranged whole-file buffer is
    /// going to be expensive before asking for it.
    #[serde(default)]
    pub size: Option<u64>,
    /// The id to fetch this track's art with (M3). An id, not an image and not
    /// a URL -- art is fetched separately on both shells, never carried as JSON.
    #[serde(rename = "coverArt", default)]
    pub cover_art: Option<String>,
    /// Track number within its album, for ordering an album view.
    #[serde(default)]
    pub track: Option<u32>,
    /// When the listener starred this, if they did (M4). A *date*, not a
    /// boolean -- absent when it is not starred. `stars::is_starred` is the one
    /// place that becomes the boolean a row draws.
    #[serde(default)]
    pub starred: Option<String>,
    /// The listener's own rating, 1-5, if they set one (M4). Not
    /// `averageRating`, which is everyone's.
    #[serde(rename = "userRating", default)]
    pub user_rating: Option<u32>,
}

/// `getRandomSongs`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RandomSongs {
    #[serde(rename = "randomSongs", default)]
    pub inner: SongList,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SongList {
    #[serde(default)]
    pub song: Vec<Song>,
}

/// The parameters for streaming a track's *original* file.
///
/// `format=raw` is decision 7 in one parameter: this player decodes with
/// symphonia on the listener's own machine, so a server-side transcode is wasted
/// work -- and worse, a transcoding response usually arrives chunked, with no
/// length and no ranges, which is exactly what seeking needs. Servers that do
/// not understand `raw` ignore it and send the original anyway; ones that
/// transcode regardless are handled by `RemoteSource`'s whole-body fallback.
pub fn stream_params(id: &str) -> [(&str, &str); 2] {
    [("id", id), ("format", "raw")]
}

/// Parses one response body.
///
/// The only way this module hands back a payload, so there is no path that skips
/// the envelope check -- see the module docs for why that matters.
pub fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<Parsed<T>, SubsonicError> {
    let root: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| SubsonicError::Malformed(format!("response was not JSON: {e}")))?;
    let response = root.get("subsonic-response").ok_or_else(|| {
        SubsonicError::Malformed(
            "no subsonic-response in the body -- is this a Subsonic server?".into(),
        )
    })?;

    let info: ServerInfo =
        serde_json::from_value(response.clone()).unwrap_or_else(|_| ServerInfo {
            version: String::new(),
            server_type: String::new(),
            server_version: String::new(),
            open_subsonic: false,
        });

    let status = response
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if status != "ok" {
        let error = response.get("error");
        let code = error
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32;
        let message = error
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("the server reported a failure with no message")
            .to_string();
        return Err(SubsonicError::Server {
            code: ErrorCode::from_code(code),
            message,
        });
    }

    let payload: T = serde_json::from_value(response.clone())
        .map_err(|e| SubsonicError::Malformed(format!("unexpected response shape: {e}")))?;
    Ok(Parsed { info, payload })
}

/// The spec's error codes, kept as themselves rather than flattened into a
/// string, because the UI's response differs per code: 40 sends the user back to
/// the password field, 30/70 are the server saying "not me/not that", and 20
/// means this client is too old to talk to it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// 0 -- generic.
    Generic,
    /// 10 -- a required parameter is missing.
    MissingParameter,
    /// 20 -- the client's protocol version is too old for the server.
    ClientTooOld,
    /// 30 -- the server's protocol version is too old for the client.
    ServerTooOld,
    /// 40 -- wrong username or password.
    BadCredentials,
    /// 41 -- the token scheme is not supported (server wants an API key), which
    /// OpenSubsonic added and which older servers never send.
    TokenAuthNotSupported,
    /// 50 -- authenticated, but not allowed to do this.
    NotAuthorized,
    /// 60 -- Subsonic's own trial expiry. Not reachable on Navidrome.
    TrialExpired,
    /// 70 -- the thing asked for is not there.
    NotFound,
    /// Anything else, kept rather than folded into `Generic` so a server that
    /// invents a code does not become indistinguishable from one that failed
    /// vaguely.
    Unknown(u32),
}

impl ErrorCode {
    pub fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Generic,
            10 => Self::MissingParameter,
            20 => Self::ClientTooOld,
            30 => Self::ServerTooOld,
            40 => Self::BadCredentials,
            41 => Self::TokenAuthNotSupported,
            50 => Self::NotAuthorized,
            60 => Self::TrialExpired,
            70 => Self::NotFound,
            other => Self::Unknown(other),
        }
    }

    /// Whether re-entering credentials is the useful thing to offer.
    pub fn is_credentials_problem(self) -> bool {
        matches!(
            self,
            Self::BadCredentials | Self::TokenAuthNotSupported | Self::NotAuthorized
        )
    }
}

/// Everything that can go wrong between typing a server address and holding a
/// parsed payload.
///
/// `Transport` is the one variant this module never constructs: it is where the
/// shells put what their own HTTP client told them, so that a caller matches on
/// one error type rather than on two.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubsonicError {
    #[error("{0}")]
    BadUrl(String),
    #[error("could not reach the server: {0}")]
    Transport(String),
    #[error("the server answered with HTTP {status}")]
    Http { status: u16 },
    #[error("the server's answer could not be read: {0}")]
    Malformed(String),
    #[error("{message}")]
    Server { code: ErrorCode, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's own worked example (subsonic.org's API page): password
    /// `sesame`, salt `c19b2d`, token `26719a1196d2a940705a59634eb18eab`. A
    /// known answer, so this test fails if the hash is right about itself and
    /// wrong about the protocol -- which is the failure a round-trip test of our
    /// own token against our own token would miss entirely.
    #[test]
    fn the_auth_token_matches_the_specs_worked_example() {
        assert_eq!(
            auth_token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[test]
    fn salts_are_lowercase_hex_of_the_bytes_given() {
        assert_eq!(salt_hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn a_url_needs_a_scheme_rather_than_having_one_guessed() {
        let error = Server::new("music.example.com:4533").unwrap_err();
        assert!(matches!(error, SubsonicError::BadUrl(_)));
        // And the message offers both, since only the user knows which.
        assert!(error.to_string().contains("https://"));
        assert!(error.to_string().contains("http://"));
    }

    #[test]
    fn base_urls_normalise_to_one_shape() {
        let expected = "http://localhost:4533";
        for typed in [
            "http://localhost:4533",
            "http://localhost:4533/",
            "http://localhost:4533/rest",
            "http://localhost:4533/rest/",
            "  http://localhost:4533  ",
        ] {
            assert_eq!(Server::new(typed).unwrap().base_url(), expected, "{typed}");
        }
    }

    #[test]
    fn a_password_request_carries_the_token_and_never_the_password() {
        let server = Server::new("http://localhost:4533").unwrap();
        let credentials = Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        };
        let url = request_url(&server, &credentials, "ping", &[], "c19b2d").unwrap();
        assert!(url.starts_with("http://localhost:4533/rest/ping?"));
        assert!(url.contains("u=rick"));
        assert!(url.contains("t=26719a1196d2a940705a59634eb18eab"));
        assert!(url.contains("s=c19b2d"));
        assert!(url.contains("v=1.16.1"));
        assert!(url.contains("c=latentPlayer"));
        assert!(url.contains("f=json"));
        assert!(!url.contains("sesame"));
        assert!(!url.contains("p="));
    }

    #[test]
    fn an_api_key_request_sends_no_username() {
        let server = Server::new("http://localhost:4533").unwrap();
        let credentials = Credentials::ApiKey {
            key: "abc123".into(),
        };
        let url = request_url(&server, &credentials, "ping", &[], "").unwrap();
        assert!(url.contains("apiKey=abc123"));
        assert!(!url.contains("u="));
        assert!(!url.contains("t="));
        assert!(!url.contains("s="));
    }

    /// The characters that make a hand-rolled encoder worth having: `&` ends the
    /// value, `+` becomes a space, `%` starts an escape, and a space is not
    /// legal in a URL at all.
    #[test]
    fn parameters_are_percent_encoded() {
        let server = Server::new("http://localhost:4533").unwrap();
        let credentials = Credentials::Password {
            user: "rick smith".into(),
            password: "a&b+c%d".into(),
        };
        let url = request_url(
            &server,
            &credentials,
            "search3",
            &[("query", "sam & max")],
            "abcdef",
        )
        .unwrap();
        assert!(url.contains("u=rick%20smith"));
        assert!(url.contains("query=sam%20%26%20max"));
        // The password is hashed, so what matters is that the *token* is the
        // hash of the real password rather than of an escaped one.
        assert!(url.contains(&format!("t={}", auth_token("a&b+c%d", "abcdef"))));
    }

    #[test]
    fn a_short_salt_is_refused_rather_than_sent() {
        let server = Server::new("http://localhost:4533").unwrap();
        let credentials = Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        };
        assert!(request_url(&server, &credentials, "ping", &[], "abc").is_err());
    }

    /// Captured from the Navidrome this was built against (0.63.2, 2026-08-17),
    /// unauthenticated. The HTTP status was **200**.
    const REAL_ERROR_BODY: &[u8] = br#"{"subsonic-response":{"status":"failed","version":"1.16.1","type":"navidrome","serverVersion":"0.63.2 (be10f89c)","openSubsonic":true,"error":{"code":10,"message":"missing parameter: 'u'"}}}"#;

    /// Captured from the same server, same day -- this one really did succeed.
    const REAL_EXTENSIONS_BODY: &[u8] = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.63.2 (be10f89c)","openSubsonic":true,"openSubsonicExtensions":[{"name":"transcodeOffset","versions":[1]},{"name":"formPost","versions":[1]},{"name":"songLyrics","versions":[1,2]},{"name":"indexBasedQueue","versions":[1]},{"name":"transcoding","versions":[1]},{"name":"playbackReport","versions":[1]}]}}"#;

    #[test]
    fn a_failed_envelope_is_an_error_even_though_the_http_status_was_200() {
        let result = parse::<NoPayload>(REAL_ERROR_BODY);
        match result {
            Err(SubsonicError::Server { code, message }) => {
                assert_eq!(code, ErrorCode::MissingParameter);
                assert!(message.contains("missing parameter"));
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn a_real_extensions_response_parses() {
        let parsed = parse::<Extensions>(REAL_EXTENSIONS_BODY).unwrap();
        assert_eq!(parsed.info.server_type, "navidrome");
        assert_eq!(parsed.info.version, "1.16.1");
        assert!(parsed.info.server_version.starts_with("0.63.2"));
        assert!(parsed.info.open_subsonic);
        assert!(parsed.payload.supports("songLyrics"));
        assert_eq!(
            parsed
                .payload
                .extensions
                .iter()
                .find(|e| e.name == "songLyrics")
                .unwrap()
                .versions,
            vec![1, 2]
        );
        // The server this was captured from does not offer it, which is why
        // decision 4's password path is the one that gets exercised here.
        assert!(!parsed.payload.supports_api_key());
    }

    #[test]
    fn a_ping_parses_to_what_the_server_says_about_itself() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.63.2","openSubsonic":true}}"#;
        let parsed = parse::<NoPayload>(body).unwrap();
        assert_eq!(parsed.info.server_type, "navidrome");
        assert_eq!(parsed.info.server_version, "0.63.2");
    }

    /// An old server states neither, and must still parse rather than being
    /// rejected for being old.
    #[test]
    fn a_response_without_the_opensubsonic_fields_still_parses() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.14.0"}}"#;
        let parsed = parse::<NoPayload>(body).unwrap();
        assert_eq!(parsed.info.version, "1.14.0");
        assert_eq!(parsed.info.server_type, "");
        assert!(!parsed.info.open_subsonic);
    }

    #[test]
    fn something_that_is_not_a_subsonic_server_says_so() {
        match parse::<NoPayload>(b"<html>hello</html>") {
            Err(SubsonicError::Malformed(message)) => assert!(message.contains("not JSON")),
            other => panic!("expected malformed, got {other:?}"),
        }
        match parse::<NoPayload>(br#"{"something":"else"}"#) {
            Err(SubsonicError::Malformed(message)) => {
                assert!(message.contains("subsonic-response"))
            }
            other => panic!("expected malformed, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_error_code_is_kept_rather_than_flattened() {
        let body = br#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":99,"message":"who knows"}}}"#;
        match parse::<NoPayload>(body) {
            Err(SubsonicError::Server { code, .. }) => assert_eq!(code, ErrorCode::Unknown(99)),
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn the_codes_that_send_the_user_back_to_the_password_field() {
        assert!(ErrorCode::BadCredentials.is_credentials_problem());
        assert!(ErrorCode::TokenAuthNotSupported.is_credentials_problem());
        assert!(ErrorCode::NotAuthorized.is_credentials_problem());
        assert!(!ErrorCode::NotFound.is_credentials_problem());
        assert!(!ErrorCode::MissingParameter.is_credentials_problem());
    }
}
