//! The desktop Subsonic transport against a server that misbehaves on purpose
//! (Phase 2 M1).
//!
//! `mod.rs`'s own tests check what a response *means*; these check what happens
//! between a URL and those bytes, which needs a real socket. Everything here is
//! deterministic and runs under `cargo test --workspace` -- the live checks
//! against a real Navidrome live in `examples/subsonic_smoke.rs`, because they
//! need a server this repo cannot carry.

mod common;

use common::{FakeServer, Reply};
use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{Credentials, ErrorCode, Extensions, Server, SubsonicError};

const OK_PING: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.63.2","openSubsonic":true}}"#;

/// The shape that makes this protocol worth a transport test at all: a failure
/// delivered with a 200.
const FAILED_AUTH: &str = r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#;

fn client(server: &FakeServer) -> HttpClient {
    HttpClient::new(
        Server::new(&server.base_url()).expect("the fake server's URL is well formed"),
        Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        },
    )
}

#[test]
fn a_ping_reports_what_the_server_says_about_itself() {
    let server = FakeServer::start();
    server.on("ping", Reply::json(OK_PING));

    let info = client(&server).ping().expect("the ping should succeed");
    assert_eq!(info.server_type, "navidrome");
    assert_eq!(info.server_version, "0.63.2");
    assert!(info.open_subsonic);
}

/// What the client puts on the wire, read off the wire rather than off the
/// function that built it.
#[test]
fn the_request_carries_the_auth_parameters_and_not_the_password() {
    let server = FakeServer::start();
    server.on("ping", Reply::json(OK_PING));
    client(&server).ping().expect("the ping should succeed");

    let seen = server
        .last()
        .expect("the server should have seen a request");
    assert_eq!(seen.path, "/rest/ping");
    assert_eq!(seen.param("u"), Some("rick"));
    assert_eq!(seen.param("v"), Some("1.16.1"));
    assert_eq!(seen.param("c"), Some("latentPlayer"));
    assert_eq!(seen.param("f"), Some("json"));
    assert!(seen.param("p").is_none(), "no plaintext password parameter");

    // The token is the hash of the password and the salt that travelled with
    // it, which is the property the whole scheme rests on -- checked here
    // against the salt the server actually received.
    let salt = seen.param("s").expect("a salt should have been sent");
    assert!(salt.len() >= 6);
    assert_eq!(
        seen.param("t"),
        Some(player_core::subsonic::auth_token("sesame", salt).as_str())
    );
}

/// Two requests must not reuse a salt, which is the one property a single
/// request cannot show.
#[test]
fn every_request_gets_a_new_salt() {
    let server = FakeServer::start();
    server.on("ping", Reply::json(OK_PING));
    let client = client(&server);
    client.ping().expect("first ping");
    client.ping().expect("second ping");

    let seen = server.seen();
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0].param("s"), seen[1].param("s"));
    assert_ne!(seen[0].param("t"), seen[1].param("t"));
}

#[test]
fn a_failure_inside_an_http_200_is_still_a_failure() {
    let server = FakeServer::start();
    server.on("ping", Reply::json(FAILED_AUTH));

    match client(&server).ping() {
        Err(SubsonicError::Server { code, message }) => {
            assert_eq!(code, ErrorCode::BadCredentials);
            assert!(code.is_credentials_problem());
            assert!(message.contains("Wrong username or password"));
        }
        other => panic!("expected a credentials error, got {other:?}"),
    }
}

/// A real HTTP failure means something other than the application talking --
/// a reverse proxy, an auth layer, a wrong path -- and has to stay
/// distinguishable from the envelope above.
#[test]
fn a_real_http_error_is_reported_as_one() {
    let server = FakeServer::start();
    server.on("ping", Reply::status(502, "Bad Gateway"));

    match client(&server).ping() {
        Err(SubsonicError::Http { status }) => assert_eq!(status, 502),
        other => panic!("expected an HTTP error, got {other:?}"),
    }
}

#[test]
fn something_that_is_not_a_subsonic_server_says_so_rather_than_panicking() {
    let server = FakeServer::start();
    server.on(
        "ping",
        Reply::text("<html><body>Router login</body></html>"),
    );

    match client(&server).ping() {
        Err(SubsonicError::Malformed(message)) => assert!(message.contains("not JSON")),
        other => panic!("expected a malformed-response error, got {other:?}"),
    }
}

#[test]
fn a_server_that_is_not_there_is_a_transport_error() {
    // Bound and dropped, so the port is almost certainly closed and nothing is
    // listening on it.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let client = HttpClient::new(
        Server::new(&format!("http://127.0.0.1:{port}")).expect("well formed"),
        Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        },
    );

    match client.ping() {
        Err(SubsonicError::Transport(_)) => {}
        other => panic!("expected a transport error, got {other:?}"),
    }
}

/// A connection that dies mid-body must not look like a short but valid
/// response.
#[test]
fn a_truncated_response_is_an_error_rather_than_half_a_payload() {
    let server = FakeServer::start();
    server.on(
        "ping",
        Reply::Truncated {
            promised: 4096,
            sent: OK_PING.as_bytes()[..40].to_vec(),
        },
    );

    match client(&server).ping() {
        Err(SubsonicError::Transport(_)) | Err(SubsonicError::Malformed(_)) => {}
        other => panic!("expected the truncation to be caught, got {other:?}"),
    }
}

/// An old server answers `getOpenSubsonicExtensions` with an error envelope,
/// and that is an answer -- "no extensions" -- not a failure to connect. A
/// client that treated it as fatal would refuse to talk to every pre-2023
/// server.
#[test]
fn a_server_that_has_never_heard_of_extensions_reports_none() {
    let server = FakeServer::start();
    server.on(
        "getOpenSubsonicExtensions",
        Reply::json(
            r#"{"subsonic-response":{"status":"failed","version":"1.14.0","error":{"code":70,"message":"not found"}}}"#,
        ),
    );

    let extensions: Extensions = client(&server)
        .extensions()
        .expect("an old server is not an error");
    assert!(extensions.extensions.is_empty());
    assert!(!extensions.supports_api_key());
}

#[test]
fn extensions_are_read_when_the_server_does_offer_them() {
    let server = FakeServer::start();
    server.on(
        "getOpenSubsonicExtensions",
        Reply::json(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","openSubsonicExtensions":[{"name":"apiKeyAuthentication","versions":[1]},{"name":"formPost","versions":[1]}]}}"#,
        ),
    );

    let extensions = client(&server)
        .extensions()
        .expect("extensions should read");
    assert!(extensions.supports_api_key());
    assert!(extensions.supports("formPost"));
    assert!(!extensions.supports("songLyrics"));
}

/// An API key must reach the server as one, with no username beside it.
#[test]
fn api_key_credentials_travel_as_a_key() {
    let server = FakeServer::start();
    server.on("ping", Reply::json(OK_PING));
    let client = HttpClient::new(
        Server::new(&server.base_url()).expect("well formed"),
        Credentials::ApiKey {
            key: "an-opaque-key".into(),
        },
    );
    client.ping().expect("the ping should succeed");

    let seen = server.last().expect("a request should have been seen");
    assert_eq!(seen.param("apiKey"), Some("an-opaque-key"));
    assert!(seen.param("u").is_none());
    assert!(seen.param("t").is_none());
    assert!(seen.param("s").is_none());
}
