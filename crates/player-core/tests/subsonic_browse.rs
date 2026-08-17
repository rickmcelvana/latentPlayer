//! Browsing a server's library over HTTP (Phase 2 M3), against a server that
//! answers the way a real one does and one that misbehaves.
//!
//! `browse.rs`'s own tests already hold the five payload shapes to known
//! answers. What only this file can check is the half that involves the wire:
//! that the right endpoint was called with the right parameters, that a browse
//! response travelling over real TCP still parses, and that cover art -- the one
//! call in the protocol that answers with an image instead of an envelope --
//! is told apart from a server saying no.

mod common;

use common::{FakeServer, Reply};
use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{AlbumSort, BrowseRequest, Credentials, Server};

fn client(server: &FakeServer) -> HttpClient {
    HttpClient::new(
        Server::new(&server.base_url()).expect("a loopback address is a usable server address"),
        Credentials::Password {
            user: "rick".into(),
            password: "sesame".into(),
        },
    )
}

const ALBUM_LIST: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","albumList2":{
    "album":[
      {"id":"1","name":"Spiderland","artist":"Slint","artistId":"9","coverArt":"al-1",
       "songCount":6,"duration":2399,"year":1991},
      {"id":"2","name":"Spirit of Eden","artist":"Talk Talk","songCount":6}]}}}"#;

const ALBUM: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{
    "id":"1","name":"Spiderland","artist":"Slint","coverArt":"al-1","songCount":2,
    "song":[
      {"id":"100","title":"Breadcrumb Trail","artist":"Slint","album":"Spiderland",
       "suffix":"flac","duration":347,"track":1,"coverArt":"al-1"},
      {"id":"101","title":"Nosferatu Man","artist":"Slint","album":"Spiderland",
       "suffix":"flac","duration":336,"track":2,"coverArt":"al-1"}]}}}"#;

#[test]
fn a_page_of_albums_asks_for_what_it_means_and_reads_what_comes_back() {
    let server = FakeServer::start();
    server.on("getAlbumList2", Reply::json(ALBUM_LIST));

    let result = client(&server)
        .browse(&BrowseRequest::Albums {
            sort: AlbumSort::AlphabeticalByName,
            size: 50,
            offset: 100,
        })
        .expect("browse albums");

    // What went on the wire, not what the client meant to send.
    let seen = server.last().expect("the server saw a request");
    assert_eq!(seen.path, "/rest/getAlbumList2");
    assert_eq!(seen.param("type"), Some("alphabeticalByName"));
    assert_eq!(seen.param("size"), Some("50"));
    assert_eq!(seen.param("offset"), Some("100"));
    // Auth still travels on every call, browse included.
    assert!(seen.param("t").is_some() && seen.param("s").is_some());
    assert_eq!(seen.param("p"), None, "the password must never be sent");

    assert_eq!(result.albums.len(), 2);
    assert_eq!(result.albums[0].name, "Spiderland");
    assert_eq!(result.albums[0].artist_id.as_deref(), Some("9"));
    assert_eq!(result.albums[0].duration_seconds, 2399.0);
    // An album the server said nothing more about is still a usable row.
    assert_eq!(result.albums[1].year, None);
    assert_eq!(result.albums[1].duration_seconds, 0.0);
}

#[test]
fn an_albums_tracks_come_back_playable() {
    let server = FakeServer::start();
    server.on("getAlbum", Reply::json(ALBUM));

    let result = client(&server)
        .browse(&BrowseRequest::Album { id: "1".into() })
        .expect("browse one album");

    assert_eq!(server.last().expect("a request").param("id"), Some("1"));
    assert_eq!(result.tracks.len(), 2);
    // The two fields playback needs, and the one the queue orders by.
    assert_eq!(result.tracks[0].id, "100");
    assert_eq!(result.tracks[0].suffix, "flac");
    assert_eq!(result.tracks[0].track_number, Some(1));
    // And the album itself, so a view that opened it has its own row.
    assert_eq!(result.albums.len(), 1);
    assert_eq!(result.albums[0].name, "Spiderland");
}

#[test]
fn a_search_sends_the_query_and_all_three_counts() {
    let server = FakeServer::start();
    server.on(
        "search3",
        Reply::json(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","searchResult3":{
                "song":[{"id":"5","title":"Good Morning, Captain","suffix":"flac","duration":382}]}}}"#,
        ),
    );

    let result = client(&server)
        .browse(&BrowseRequest::Search {
            query: "captain & co".into(),
            count: Some(5),
        })
        .expect("search");

    let seen = server.last().expect("a request");
    assert_eq!(seen.path, "/rest/search3");
    // Decoded on the server's side, which is how this checks the *encoding*:
    // an ampersand that was not escaped would arrive as a second parameter.
    assert_eq!(seen.param("query"), Some("captain & co"));
    for key in ["artistCount", "albumCount", "songCount"] {
        assert_eq!(seen.param(key), Some("5"), "{key}");
    }
    assert_eq!(result.tracks.len(), 1);
    // A search that matched only songs is not a malformed response.
    assert!(result.artists.is_empty() && result.albums.is_empty());
}

/// The trap, again, on the browse path: a failure arrives as an HTTP 200 and
/// must not read as an empty library.
#[test]
fn a_failure_while_browsing_is_an_error_rather_than_no_albums() {
    let server = FakeServer::start();
    server.on(
        "getAlbumList2",
        Reply::json(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                "error":{"code":40,"message":"Wrong username or password"}}}"#,
        ),
    );

    let error = client(&server)
        .browse(&BrowseRequest::Albums {
            sort: AlbumSort::Newest,
            size: 10,
            offset: 0,
        })
        .expect_err("a failed envelope must not parse as an empty list");
    assert!(
        error.to_string().to_lowercase().contains("password"),
        "the server's own words should reach the caller: {error}"
    );
}

/// Cover art is the one call that answers with an image, so it is the one call
/// where "did this work" cannot be answered by parsing an envelope.
#[test]
fn cover_art_comes_back_as_an_image_with_its_type() {
    let server = FakeServer::start();
    // A one-pixel PNG, so the bytes are a real image rather than a string that
    // happens to be served as one.
    let png: Vec<u8> = vec![
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R',
    ];
    server.on(
        "getCoverArt",
        Reply::Body {
            status: 200,
            content_type: "image/png",
            body: png.clone(),
        },
    );

    let cover = client(&server)
        .cover_art("al-1", 300)
        .expect("fetch the cover");
    let seen = server.last().expect("a request");
    assert_eq!(seen.param("id"), Some("al-1"));
    assert_eq!(
        seen.param("size"),
        Some("300"),
        "art is asked for at a size"
    );
    assert_eq!(cover.content_type, "image/png");
    assert_eq!(cover.bytes, png);
}

#[test]
fn a_server_that_has_no_such_cover_is_an_error_rather_than_a_broken_image() {
    let server = FakeServer::start();
    // Navidrome answers this one with the ordinary envelope -- an HTTP 200
    // carrying a failure -- rather than with the XML the spec describes. Either
    // way it is not an image, and handing those bytes to an `<img>` would draw
    // a broken icon with no explanation anywhere.
    server.on(
        "getCoverArt",
        Reply::json(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                "error":{"code":70,"message":"Cover art not found"}}}"#,
        ),
    );

    let error = client(&server)
        .cover_art("nope", 300)
        .expect_err("a failure must not come back as image bytes");
    assert!(
        error.to_string().to_lowercase().contains("cover art"),
        "the server's own words should reach the caller: {error}"
    );
}

#[test]
fn a_cover_that_is_neither_an_image_nor_an_envelope_says_what_arrived() {
    let server = FakeServer::start();
    // A reverse proxy's login page, which is the realistic version of this: it
    // is HTML, it is an HTTP 200, and it is not art.
    server.on("getCoverArt", Reply::text("<html>please log in</html>"));

    let error = client(&server)
        .cover_art("al-1", 300)
        .expect_err("html is not cover art");
    assert!(
        error.to_string().contains("text/html"),
        "the error should name what did arrive: {error}"
    );
}
