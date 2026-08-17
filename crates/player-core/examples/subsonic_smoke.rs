//! Live check of the Subsonic client against a **real** server (Phase 2 M1).
//!
//!   cargo run -p player-core --example subsonic_smoke
//!
//! `tests/subsonic_http.rs` covers everything a fake server can be made to do,
//! deterministically and under `cargo test`. This covers the half a fake server
//! cannot: a real implementation only ever returns what its authors wrote, not
//! what this client's author expected them to write.
//!
//! # What it needs
//!
//! Nothing, for the unauthenticated checks -- it defaults to a Navidrome on
//! `http://localhost:4533` and skips cleanly if nothing is there. For the
//! authenticated checks, credentials through the environment rather than
//! through this file or the command line:
//!
//! ```text
//!   LATENTPLAYER_SUBSONIC_URL       (default http://localhost:4533)
//!   LATENTPLAYER_SUBSONIC_USER
//!   LATENTPLAYER_SUBSONIC_PASSWORD
//! ```
//!
//! The environment, deliberately: a password on a command line is in the shell
//! history and in every process listing on the machine. This example never
//! prints it, and the client never puts it in a URL (see `subsonic::mod`'s
//! token auth). Without the two credential variables the authenticated checks
//! are skipped and say so, rather than failing as though the server were wrong.

use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{Credentials, ErrorCode, Server, SubsonicError};

const DEFAULT_URL: &str = "http://localhost:4533";

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

fn skip(label: &str, why: &str) {
    println!("  [SKIP] {label}: {why}");
}

fn main() {
    let url = std::env::var("LATENTPLAYER_SUBSONIC_URL").unwrap_or_else(|_| DEFAULT_URL.into());
    let user = std::env::var("LATENTPLAYER_SUBSONIC_USER").ok();
    let password = std::env::var("LATENTPLAYER_SUBSONIC_PASSWORD").ok();

    let server = match Server::new(&url) {
        Ok(server) => server,
        Err(e) => {
            println!("{url} is not a usable server address: {e}");
            return;
        }
    };
    println!("server:  {}", server.base_url());

    // --- unauthenticated: does it answer, and does it answer like Subsonic? ---
    //
    // Deliberately wrong credentials. This cannot prove the token is *right* --
    // a malformed token is rejected the same way a wrong password is -- but it
    // proves the request shape was accepted (a missing parameter would be error
    // 10, not 40), that the server verifies what it is sent, and that a failure
    // wrapped in an HTTP 200 comes back through this client as an error.
    let rejected = HttpClient::new(
        server.clone(),
        Credentials::Password {
            user: "not-a-real-user".into(),
            password: "not-a-real-password".into(),
        },
    );
    match rejected.ping() {
        Err(SubsonicError::Server { code, message }) => {
            check(
                "a wrong password is reported as a credentials problem",
                code == ErrorCode::BadCredentials && code.is_credentials_problem(),
                format!("{code:?} -- {message:?}"),
            );
        }
        Err(SubsonicError::Transport(e)) => {
            skip(
                "the whole live half",
                &format!("nothing answered at {url} ({e}). Start the server, or set LATENTPLAYER_SUBSONIC_URL."),
            );
            return;
        }
        other => check(
            "a wrong password is reported as a credentials problem",
            false,
            format!("expected a server error, got {other:?}"),
        ),
    }

    // Navidrome answers this one without credentials, which makes it the one
    // call that can prove parsing against a real server with no secrets in play.
    match rejected.extensions() {
        Ok(extensions) => {
            let names: Vec<&str> = extensions
                .extensions
                .iter()
                .map(|e| e.name.as_str())
                .collect();
            check(
                "the server's OpenSubsonic extensions parse",
                !names.is_empty(),
                names.join(", "),
            );
            println!(
                "note:    apiKeyAuthentication is {} here, so credentials {}",
                if extensions.supports_api_key() {
                    "offered"
                } else {
                    "NOT offered"
                },
                if extensions.supports_api_key() {
                    "can be an opaque key (phase-2 decision 4)"
                } else {
                    "must be a password this client keeps (phase-2 decision 4)"
                }
            );
        }
        Err(e) => check(
            "the server's OpenSubsonic extensions parse",
            false,
            format!("{e}"),
        ),
    }

    // --- authenticated: the token is actually right ---
    let (Some(user), Some(password)) = (user, password) else {
        skip(
            "the authenticated checks",
            "set LATENTPLAYER_SUBSONIC_USER and LATENTPLAYER_SUBSONIC_PASSWORD to run them",
        );
        return;
    };

    let client = HttpClient::new(server, Credentials::Password { user, password });
    match client.ping() {
        Ok(info) => {
            check(
                "a correct password authenticates",
                !info.version.is_empty(),
                format!(
                    "{} {} speaking API {}{}",
                    if info.server_type.is_empty() {
                        "an unnamed server"
                    } else {
                        &info.server_type
                    },
                    info.server_version,
                    info.version,
                    if info.open_subsonic {
                        ", OpenSubsonic"
                    } else {
                        ""
                    }
                ),
            );
        }
        Err(e) => check(
            "a correct password authenticates",
            false,
            format!("{e} -- if this says wrong username or password, the token scheme is wrong"),
        ),
    }
}
