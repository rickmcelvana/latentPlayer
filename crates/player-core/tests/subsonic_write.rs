//! Writing to a server (Phase 2 M4), against a fake one that records what
//! actually arrived.
//!
//! The modules' own unit tests already hold the payload shapes and the rule for
//! when a play counts to known answers. What only this file can check is the
//! half that involves the wire, and for a milestone that *changes* someone
//! else's library that half is the dangerous one: every mistake available here
//! is a request the server answers `"status": "ok"` to while doing nothing, or
//! doing the wrong thing quietly.
//!
//! Three of those in particular:
//!
//! - A repeated parameter sent once. `songId`/`songIdToAdd`/`songIndexToRemove`
//!   are repeated to mean a list, and a client that sends one creates a playlist
//!   holding one track.
//! - An album starred through `id` instead of `albumId`, which names it the way
//!   a different API names a different thing and stars nothing at all.
//! - `updatePlaylist`'s `playlistId`, where every other call in the protocol
//!   takes `id`.

mod common;

use common::{FakeServer, Reply};
use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{
    Credentials, MarkRequest, PlaylistRequest, Server, StarTarget, SubsonicError,
};

fn client(server: &FakeServer) -> HttpClient {
    HttpClient::new(
        Server::new(&server.base_url()).expect("a loopback address is a usable server address"),
        Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        },
    )
}

/// What every write that succeeded answers with.
const OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome"}}"#;

const PLAYLISTS: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlists":{
    "playlist":[
      {"id":"10","name":"Late night","owner":"rick","public":false,"songCount":12,
       "duration":2400,"changed":"2026-08-18T04:00:00Z","coverArt":"pl-10"},
      {"id":"11","name":"Loud","owner":"rick","public":true,"songCount":3,"duration":600}]}}}"#;

const PLAYLIST: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlist":{
    "id":"10","name":"Late night","owner":"rick","songCount":2,"duration":683,
    "entry":[
      {"id":"100","title":"Breadcrumb Trail","artist":"Slint","album":"Spiderland",
       "suffix":"flac","duration":347,"starred":"2026-08-01T12:00:00Z","userRating":5},
      {"id":"101","title":"Nosferatu Man","artist":"Slint","album":"Spiderland",
       "suffix":"flac","duration":336}]}}}"#;

#[test]
fn the_playlists_come_back_as_rows_over_real_tcp() {
    let server = FakeServer::start();
    server.on("getPlaylists", Reply::json(PLAYLISTS));

    let result = client(&server)
        .playlists(&PlaylistRequest::List)
        .expect("listing playlists");

    let seen = server.last().expect("the server saw a request");
    assert_eq!(seen.path, "/rest/getPlaylists");
    // Auth travels on every call, writes and reads alike -- and never as a
    // password.
    assert_eq!(seen.param("u"), Some("rick"));
    assert!(seen.param("t").is_some());
    assert_eq!(seen.param("p"), None);

    assert_eq!(result.playlists.len(), 2);
    assert_eq!(result.playlists[0].name, "Late night");
    assert_eq!(result.playlists[0].song_count, 12);
    assert_eq!(result.playlists[0].duration_seconds, 2400.0);
    assert!(!result.playlists[0].public);
    assert!(result.playlists[1].public);
}

#[test]
fn one_playlist_comes_back_with_its_tracks_and_their_marks() {
    let server = FakeServer::start();
    server.on("getPlaylist", Reply::json(PLAYLIST));

    let result = client(&server)
        .playlists(&PlaylistRequest::Get { id: "10".into() })
        .expect("opening a playlist");

    assert_eq!(server.last().expect("a request").param("id"), Some("10"));
    assert_eq!(result.tracks.len(), 2);
    // The suffix is what playback needs; a Subsonic stream URL carries no
    // extension for symphonia's probe to read.
    assert_eq!(result.tracks[0].suffix, "flac");
    // `starred` arrives as a date and leaves as the boolean a row draws.
    assert!(result.tracks[0].starred);
    assert_eq!(result.tracks[0].rating, Some(5));
    assert!(!result.tracks[1].starred);
    assert_eq!(result.tracks[1].rating, None);
}

/// The repeated-parameter case: the one a map-shaped client gets wrong.
#[test]
fn creating_a_playlist_sends_every_track_and_not_just_the_last() {
    let server = FakeServer::start();
    server.on("createPlaylist", Reply::json(PLAYLIST));

    client(&server)
        .playlists(&PlaylistRequest::Create {
            name: "Late night".into(),
            song_ids: vec!["100".into(), "101".into(), "102".into()],
        })
        .expect("creating a playlist");

    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/createPlaylist");
    assert_eq!(seen.param("name"), Some("Late night"));
    assert_eq!(seen.values("songId"), vec!["100", "101", "102"]);
}

#[test]
fn updating_a_playlist_names_it_playlist_id_and_repeats_its_lists() {
    let server = FakeServer::start();
    server.on("updatePlaylist", Reply::json(OK));
    let client = client(&server);

    client
        .playlists(&PlaylistRequest::AddSongs {
            id: "10".into(),
            song_ids: vec!["200".into(), "201".into()],
        })
        .expect("adding to a playlist");
    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/updatePlaylist");
    // Not `id`: this is the one endpoint in the protocol that spells it
    // differently, and `id` here would be a missing required parameter.
    assert_eq!(seen.param("playlistId"), Some("10"));
    assert_eq!(seen.param("id"), None);
    assert_eq!(seen.values("songIdToAdd"), vec!["200", "201"]);

    client
        .playlists(&PlaylistRequest::RemoveIndices {
            id: "10".into(),
            indices: vec![0, 3, 4],
        })
        .expect("removing from a playlist");
    let seen = server.last().expect("a request");
    assert_eq!(seen.param("playlistId"), Some("10"));
    // All three in one request: each index counts positions in the playlist as
    // the server currently holds it, so removing them one request at a time
    // would remove three different tracks.
    assert_eq!(seen.values("songIndexToRemove"), vec!["0", "3", "4"]);

    client
        .playlists(&PlaylistRequest::Rename {
            id: "10".into(),
            name: "Even later".into(),
        })
        .expect("renaming a playlist");
    let seen = server.last().expect("a request");
    assert_eq!(seen.param("playlistId"), Some("10"));
    assert_eq!(seen.param("name"), Some("Even later"));
}

#[test]
fn deleting_a_playlist_is_its_own_endpoint_with_a_plain_id() {
    let server = FakeServer::start();
    server.on("deletePlaylist", Reply::json(OK));

    client(&server)
        .playlists(&PlaylistRequest::Delete { id: "10".into() })
        .expect("deleting a playlist");

    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/deletePlaylist");
    assert_eq!(seen.param("id"), Some("10"));
}

/// A write that failed is an HTTP 200 with a refusal inside it. This is the
/// check that stops "saved" from being printed over "not allowed".
#[test]
fn a_refused_write_is_reported_rather_than_read_as_success() {
    let server = FakeServer::start();
    server.on(
        "deletePlaylist",
        Reply::json(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                "error":{"code":50,"message":"not allowed"}}}"#,
        ),
    );

    let error = client(&server)
        .playlists(&PlaylistRequest::Delete { id: "10".into() })
        .expect_err("a refusal must not read as success");
    assert!(
        matches!(error, SubsonicError::Server { ref message, .. } if message == "not allowed"),
        "{error:?}"
    );
}

/// The milestone's quietest available bug, as a test: three kinds of star, three
/// different parameters.
#[test]
fn each_kind_of_star_puts_its_id_in_the_parameter_that_kind_uses() {
    let server = FakeServer::start();
    server.on("star", Reply::json(OK));
    server.on("unstar", Reply::json(OK));
    let client = client(&server);

    client
        .mark(&MarkRequest::Star {
            target: StarTarget::Song,
            id: "100".into(),
        })
        .expect("starring a song");
    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/star");
    assert_eq!(seen.param("id"), Some("100"));

    client
        .mark(&MarkRequest::Star {
            target: StarTarget::Album,
            id: "al-1".into(),
        })
        .expect("starring an album");
    let seen = server.last().expect("a request");
    // The whole point: an ID3-tagged album is `albumId`. Sent as `id` the
    // server looks for a *folder* with that id, finds none, and answers ok.
    assert_eq!(seen.param("albumId"), Some("al-1"));
    assert_eq!(seen.param("id"), None);

    client
        .mark(&MarkRequest::Star {
            target: StarTarget::Artist,
            id: "ar-9".into(),
        })
        .expect("starring an artist");
    let seen = server.last().expect("a request");
    assert_eq!(seen.param("artistId"), Some("ar-9"));
    assert_eq!(seen.param("id"), None);

    client
        .mark(&MarkRequest::Unstar {
            target: StarTarget::Album,
            id: "al-1".into(),
        })
        .expect("unstarring an album");
    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/unstar");
    assert_eq!(seen.param("albumId"), Some("al-1"));
}

#[test]
fn a_rating_travels_as_a_number_in_range() {
    let server = FakeServer::start();
    server.on("setRating", Reply::json(OK));
    let client = client(&server);

    client
        .mark(&MarkRequest::SetRating {
            id: "100".into(),
            rating: 4,
        })
        .expect("rating a song");
    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/setRating");
    assert_eq!(seen.param("id"), Some("100"));
    assert_eq!(seen.param("rating"), Some("4"));

    // 0 is not a rating -- it clears one -- and must survive as itself.
    client
        .mark(&MarkRequest::SetRating {
            id: "100".into(),
            rating: 0,
        })
        .expect("clearing a rating");
    assert_eq!(server.last().expect("a request").param("rating"), Some("0"));

    // Above the range is clamped here rather than refused there: the server
    // answers error 10, which reads to a listener as "rating is broken".
    client
        .mark(&MarkRequest::SetRating {
            id: "100".into(),
            rating: 99,
        })
        .expect("an out-of-range rating");
    assert_eq!(server.last().expect("a request").param("rating"), Some("5"));
}

#[test]
fn a_now_playing_and_a_play_are_the_same_endpoint_said_differently() {
    let server = FakeServer::start();
    server.on("scrobble", Reply::json(OK));
    let client = client(&server);

    client
        .scrobble("100", false, None)
        .expect("announcing a track");
    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/scrobble");
    assert_eq!(seen.param("id"), Some("100"));
    assert_eq!(seen.param("submission"), Some("false"));
    // Now-playing carries no time: there is no play to date yet.
    assert_eq!(seen.param("time"), None);

    client
        .scrobble("100", true, Some(1_700_000_000_000))
        .expect("submitting a play");
    let seen = server.last().expect("a request");
    assert_eq!(seen.param("submission"), Some("true"));
    // When the play *started*, not when it was submitted -- a client that sends
    // the submission time files every track minutes late.
    assert_eq!(seen.param("time"), Some("1700000000000"));
}

/// Scrobbling is the one write whose failure the listener never sees, so it must
/// still be a failure to its caller rather than a silent nothing.
#[test]
fn a_scrobble_the_server_refuses_is_still_an_error() {
    let server = FakeServer::start();
    server.on(
        "scrobble",
        Reply::json(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                "error":{"code":70,"message":"song not found"}}}"#,
        ),
    );

    let error = client(&server)
        .scrobble("nope", true, None)
        .expect_err("a refusal must not read as success");
    assert!(
        matches!(error, SubsonicError::Server { ref message, .. } if message == "song not found"),
        "{error:?}"
    );
}
