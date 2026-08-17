//! Live check of browsing a **real** server (Phase 2 M3).
//!
//!   cargo run -p player-core --example browse_smoke
//!
//! `cargo test -p player-core --test subsonic_browse` covers what a fake server
//! can be made to say, including the shapes nobody can produce on cue. This
//! covers what it cannot: a real Navidrome, with a real library in it, whose
//! answers this client has never seen.
//!
//! Needs credentials, through the environment for `subsonic_smoke`'s reason (a
//! password on a command line is in the shell history and in every process
//! listing):
//!
//! ```text
//!   LATENTPLAYER_SUBSONIC_URL       (default http://localhost:4533)
//!   LATENTPLAYER_SUBSONIC_USER
//!   LATENTPLAYER_SUBSONIC_PASSWORD
//! ```
//!
//! Makes no sound: it browses, and then proves that what it browsed to is
//! playable by opening the stream rather than by playing it.

use std::time::Instant;

use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{AlbumSort, BrowseRequest, Credentials, Server, COVER_SIZE_PX};
use player_core::PlaybackSource;

const DEFAULT_URL: &str = "http://localhost:4533";

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

fn main() {
    let url = std::env::var("LATENTPLAYER_SUBSONIC_URL").unwrap_or_else(|_| DEFAULT_URL.into());
    let (Ok(user), Ok(password)) = (
        std::env::var("LATENTPLAYER_SUBSONIC_USER"),
        std::env::var("LATENTPLAYER_SUBSONIC_PASSWORD"),
    ) else {
        println!(
            "  [SKIP] everything: set LATENTPLAYER_SUBSONIC_USER and \
             LATENTPLAYER_SUBSONIC_PASSWORD (and optionally _URL) to run this"
        );
        return;
    };

    let server = match Server::new(&url) {
        Ok(server) => server,
        Err(e) => {
            println!("{url} is not a usable server address: {e}");
            return;
        }
    };
    let client = HttpClient::new(server, Credentials::Password { user, password });

    // --- artists ----------------------------------------------------------
    //
    // The alphabetical index is the shape most likely to be read wrongly, and
    // reading it wrongly produces an empty list rather than an error -- which is
    // exactly what a server with no music also produces. So this asks for a real
    // library and expects to find something in it.
    let started = Instant::now();
    let artists = match client.browse(&BrowseRequest::Artists) {
        Ok(result) => result.artists,
        Err(e) => {
            println!("  [FAIL] asking for the server's artists: {e}");
            return;
        }
    };
    check(
        "the server's artists come back out of the alphabetical index",
        !artists.is_empty(),
        format!(
            "{} artists in {:.0}ms, first: {}",
            artists.len(),
            started.elapsed().as_secs_f64() * 1000.0,
            artists.first().map(|a| a.name.as_str()).unwrap_or("none")
        ),
    );

    // --- albums, and the page after the first ------------------------------
    let page = |offset: u32| BrowseRequest::Albums {
        sort: AlbumSort::AlphabeticalByName,
        size: 10,
        offset,
    };
    let first = match client.browse(&page(0)) {
        Ok(result) => result.albums,
        Err(e) => {
            println!("  [FAIL] asking for a page of albums: {e}");
            return;
        }
    };
    check(
        "a page of albums comes back",
        !first.is_empty(),
        format!(
            "{} albums, first: {} -- {}",
            first.len(),
            first.first().map(|a| a.artist.as_str()).unwrap_or(""),
            first.first().map(|a| a.name.as_str()).unwrap_or("none")
        ),
    );

    // Paging is where an offset that is ignored -- or sent under the wrong name
    // -- shows up: the second page would simply be the first one again.
    if first.len() >= 10 {
        match client.browse(&page(10)) {
            Ok(result) => {
                let second = result.albums;
                let overlap = second
                    .iter()
                    .filter(|album| first.iter().any(|a| a.id == album.id))
                    .count();
                check(
                    "the second page is different music from the first",
                    overlap == 0,
                    format!("{} albums, {overlap} of them repeated", second.len()),
                );
            }
            Err(e) => check(
                "the second page is different music from the first",
                false,
                format!("{e}"),
            ),
        }
    }

    // --- one album, its tracks, and its art --------------------------------
    let Some(album) = first.first() else {
        println!("  [SKIP] the rest: the server returned no albums to open");
        return;
    };
    let contents = match client.browse(&BrowseRequest::Album {
        id: album.id.clone(),
    }) {
        Ok(result) => result,
        Err(e) => {
            println!("  [FAIL] opening an album: {e}");
            return;
        }
    };
    check(
        "an album's tracks come back with what playback needs",
        !contents.tracks.is_empty() && contents.tracks.iter().all(|t| !t.id.is_empty()),
        format!(
            "{} tracks, first: {} ({})",
            contents.tracks.len(),
            contents
                .tracks
                .first()
                .map(|t| t.title.as_str())
                .unwrap_or("none"),
            contents
                .tracks
                .first()
                .map(|t| t.suffix.as_str())
                .unwrap_or("")
        ),
    );
    // The server states a song count in the browse row; the album's own
    // response lists them. Disagreement means one of the two was misread.
    check(
        "the album's track count agrees with what the list said",
        album.song_count == 0 || album.song_count as usize == contents.tracks.len(),
        format!(
            "the list said {} tracks, the album has {}",
            album.song_count,
            contents.tracks.len()
        ),
    );

    if let Some(cover_id) = album.cover_art.as_deref() {
        match client.cover_art(cover_id, COVER_SIZE_PX) {
            Ok(cover) => check(
                "cover art comes back as an image",
                cover.content_type.starts_with("image/") && cover.bytes.len() > 100,
                format!(
                    "{} {} bytes at {COVER_SIZE_PX}px",
                    cover.content_type,
                    cover.bytes.len()
                ),
            ),
            Err(e) => check("cover art comes back as an image", false, format!("{e}")),
        }
    }

    // --- and that a browsed track is a playable one ------------------------
    //
    // The whole point of browsing. Opening the stream rather than playing it:
    // the audio path is `remote_smoke`'s to prove, and what is new here is that
    // an id which arrived through a *browse* response builds a source that a
    // real server will actually serve.
    if let Some(track) = contents.tracks.first() {
        let opened = client
            .track_source(&track.id, &track.suffix)
            .map_err(|e| e.to_string())
            .and_then(|source| source.open().map_err(|e| e.to_string()));
        match opened {
            Ok(stream) => check(
                "a browsed track opens as a playable stream",
                stream.byte_len().is_some_and(|len| len > 0),
                format!(
                    "{} bytes, seekable: {}",
                    stream.byte_len().unwrap_or(0),
                    stream.is_seekable()
                ),
            ),
            Err(e) => check("a browsed track opens as a playable stream", false, e),
        }
    }

    // --- search -------------------------------------------------------------
    //
    // Searched for by a word taken from the library itself, so this is a claim
    // about the search working rather than about the library's contents.
    if let Some(needle) = artists
        .first()
        .and_then(|artist| artist.name.split_whitespace().next())
    {
        match client.browse(&BrowseRequest::Search {
            query: needle.to_string(),
            count: Some(10),
        }) {
            Ok(result) => check(
                "searching the server finds the artist it was asked about",
                result
                    .artists
                    .iter()
                    .any(|a| a.name.to_lowercase().contains(&needle.to_lowercase()))
                    || !result.tracks.is_empty()
                    || !result.albums.is_empty(),
                format!(
                    "\"{needle}\": {} artists, {} albums, {} tracks",
                    result.artists.len(),
                    result.albums.len(),
                    result.tracks.len()
                ),
            ),
            Err(e) => check(
                "searching the server finds the artist it was asked about",
                false,
                format!("{e}"),
            ),
        }
    }

    // A query that matches nothing is an empty result, not an error -- the
    // difference between "no results" and "browsing is broken", which the UI
    // has to be able to tell apart.
    match client.browse(&BrowseRequest::Search {
        query: "zzzzz-not-in-any-library-zzzzz".into(),
        count: Some(5),
    }) {
        Ok(result) => check(
            "a search that matches nothing is empty rather than an error",
            result.artists.is_empty() && result.albums.is_empty() && result.tracks.is_empty(),
            format!(
                "{} artists, {} albums, {} tracks",
                result.artists.len(),
                result.albums.len(),
                result.tracks.len()
            ),
        ),
        Err(e) => check(
            "a search that matches nothing is empty rather than an error",
            false,
            format!("{e}"),
        ),
    }
}
