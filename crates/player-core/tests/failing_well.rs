//! Failing well (Phase 2 M5), against a server that fails on purpose.
//!
//! The milestone's own claim, as tests: a hiccup is survived rather than
//! reported, a refusal is reported rather than survived, and the difference
//! between those two is decided once. Every case here is one this phase could
//! plausibly meet against a real self-hosted server on a real home network, and
//! none of them can be arranged on cue against a real one -- which is what the
//! fake server is for.

mod common;

use std::io::Read;

use common::{wav_bytes, FakeServer, Reply};
use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{Credentials, FailureKind, Server, SubsonicError};
use player_core::{PlaybackSource, RemoteSource};

fn client(server: &FakeServer) -> HttpClient {
    HttpClient::new(
        Server::new(&server.base_url()).expect("a loopback address is a usable server address"),
        Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        },
    )
}

/// The milestone's headline: a server having a moment no longer ends the track.
#[test]
fn a_server_that_stumbles_and_recovers_still_plays_the_track() {
    let audio = wav_bytes(44_100, 0.5);
    let server = FakeServer::start();
    // Two 503s, then the music. Before M5 the first of those ended the track:
    // the read was an error, and the decoder treats a broken source the way it
    // treats a corrupt file.
    server.on(
        "stream",
        Reply::FlakyRanged {
            body: audio.clone(),
            fail_until: 2,
        },
    );

    let source = RemoteSource::new(
        format!("{}/rest/stream?id=1", server.base_url()),
        Some("wav".into()),
    );
    let mut opened = source.open().expect("the third attempt should succeed");

    let mut read = Vec::new();
    opened.read_to_end(&mut read).expect("reading the track");
    assert_eq!(
        read, audio,
        "what arrived after the retries is the whole track, byte for byte"
    );
    assert!(
        server.request_count() >= 3,
        "it should have taken more than one attempt: {} requests",
        server.request_count()
    );
}

/// And the other half of decision 14: what will never work is reported at once.
#[test]
fn a_refusal_is_not_retried() {
    let server = FakeServer::start();
    server.on("stream", Reply::status(404, "no such track"));

    let source = RemoteSource::new(
        format!("{}/rest/stream?id=nope", server.base_url()),
        Some("wav".into()),
    );
    // `Box<dyn MediaSource>` has no `Debug`, so the success case is named
    // rather than unwrapped through `expect_err`.
    let Err(error) = source.open() else {
        panic!("a 404 must not open as a playable track");
    };
    assert!(
        error.to_string().contains("404"),
        "the failure should name what happened: {error}"
    );
    // One attempt, not four. A track the server does not have will not appear
    // three seconds later, and retrying only delays saying so.
    assert_eq!(server.request_count(), 1);
}

#[test]
fn a_server_that_never_answers_is_reported_rather_than_waited_on_forever() {
    let server = FakeServer::start();
    server.on("ping", Reply::Hang);

    let error = client(&server)
        .ping()
        .expect_err("a server that never answers is a failure");
    assert_eq!(error.kind(), FailureKind::Unreachable);
    assert!(
        !error.is_credentials_problem(),
        "a silent server says nothing about the password"
    );
}

#[test]
fn a_server_that_is_not_there_names_the_right_thing_to_fix() {
    // Started and immediately dropped, so the port is closed.
    let base = {
        let server = FakeServer::start();
        server.base_url()
    };
    let client = HttpClient::new(
        Server::new(&base).expect("a usable address"),
        Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        },
    );
    let error = client.ping().expect_err("nothing is listening");
    assert_eq!(error.kind(), FailureKind::Unreachable);
}

/// A token that stops being accepted: the server answers 200 with a refusal
/// inside, and the client has to send the listener to the password field.
#[test]
fn a_token_the_server_has_stopped_accepting_is_a_credentials_problem() {
    let server = FakeServer::start();
    server.on(
        "ping",
        Reply::json(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                "error":{"code":40,"message":"Wrong username or password"}}}"#,
        ),
    );

    let error = client(&server).ping().expect_err("a rejected token");
    assert_eq!(error.kind(), FailureKind::Credentials);
    assert!(error.is_credentials_problem());
    assert!(!error.is_transient(), "sending it again will not help");
}

/// An auth layer in front of the server refuses with a real status code, unlike
/// Subsonic itself -- and it is still the listener's password.
#[test]
fn a_proxy_asking_for_its_own_password_is_a_credentials_problem() {
    let server = FakeServer::start();
    server.on("ping", Reply::status(401, "Unauthorized"));

    let error = client(&server).ping().expect_err("a proxy refusing");
    assert_eq!(error.kind(), FailureKind::Credentials);
    assert!(!error.is_transient());
}

/// A captive portal, a proxy error page, a web server where the API should be:
/// all answer 200 with something that is not this protocol.
#[test]
fn something_that_is_not_the_server_says_so_rather_than_looking_empty() {
    let server = FakeServer::start();
    server.on(
        "ping",
        Reply::text("<html><body>Sign in to the wifi</body></html>"),
    );

    let error = client(&server).ping().expect_err("not a Subsonic server");
    assert_eq!(error.kind(), FailureKind::Malformed);
    assert!(!error.is_transient());
}

/// The one this milestone had to be careful about: a 5xx *is* worth another go,
/// so a browse that hits a restarting server should not read as a broken one.
#[test]
fn a_server_error_is_transient_and_a_client_error_is_not() {
    let server = FakeServer::start();
    server.on("ping", Reply::status(503, "restarting"));
    let error = client(&server).ping().expect_err("a 503");
    assert!(error.is_transient());
    assert_eq!(error.kind(), FailureKind::Server);

    server.on("ping", Reply::status(404, "no such path"));
    let error = client(&server).ping().expect_err("a 404");
    assert!(!error.is_transient());
}

/// A server that goes away *mid-track* -- the case ARCHITECTURE.md named as an
/// open gap after M2. The stall is survived while it lasts and the audio
/// arrives complete.
#[test]
fn a_stall_partway_through_a_track_does_not_lose_any_audio() {
    let audio = wav_bytes(44_100, 0.5);
    let server = FakeServer::start();
    server.on(
        "stream",
        Reply::Stalling {
            body: audio.clone(),
            delay: std::time::Duration::from_millis(200),
        },
    );

    let source = RemoteSource::new(
        format!("{}/rest/stream?id=1", server.base_url()),
        Some("wav".into()),
    );
    let mut opened = source.open().expect("opening a stalling server");
    let mut read = Vec::new();
    opened
        .read_to_end(&mut read)
        .expect("reading through a stall");
    assert_eq!(read, audio, "a stall costs time, never samples");
}

/// Certificates cannot be exercised over the plaintext fake server, so what is
/// checked here is the classification every shell's message is drawn from --
/// the piece that decides whether the listener is told to change a password, an
/// address, or a trust store.
#[test]
fn a_certificate_failure_is_its_own_thing_and_is_never_retried() {
    let error = SubsonicError::Certificate("unknown issuer".into());
    assert_eq!(error.kind(), FailureKind::Certificate);
    assert!(!error.is_transient(), "trust is a decision, not a hiccup");
    assert!(
        !error.is_credentials_problem(),
        "sending the password again cannot help"
    );
    assert!(
        error.to_string().contains("certificate"),
        "the message should name the thing: {error}"
    );
}
