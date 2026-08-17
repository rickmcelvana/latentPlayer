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

use super::{
    request_url, salt_hex, Credentials, Extensions, NoPayload, Parsed, Server, ServerInfo,
    SubsonicError,
};

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

/// A connection to one server, with the credentials to use on it.
///
/// Cheap to clone in the sense that matters -- the `ureq::Agent` inside pools
/// connections, so a second request to the same server reuses the first one's
/// TCP and TLS handshake rather than paying for them again.
pub struct HttpClient {
    agent: ureq::Agent,
    server: Server,
    credentials: Credentials,
}

impl HttpClient {
    pub fn new(server: Server, credentials: Credentials) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
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

    /// One GET, with the transport's own failures translated into this crate's
    /// error type -- so a caller matches on `SubsonicError` and never on
    /// `ureq::Error`.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, SubsonicError> {
        let mut response = self.agent.get(url).call().map_err(|e| match e {
            // 4xx/5xx: a real HTTP failure, which for this protocol means
            // something *other* than the application talking -- a proxy, a
            // wrong path, an auth layer in front of the server. Subsonic's own
            // failures arrive as 200s (see `mod.rs`).
            ureq::Error::StatusCode(status) => SubsonicError::Http { status },
            other => SubsonicError::Transport(other.to_string()),
        })?;

        response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|e| SubsonicError::Transport(format!("reading the response failed: {e}")))
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
