//! Live check of **writing** to a real server (Phase 2 M4).
//!
//!   cargo run -p player-core --example write_smoke
//!
//! `cargo test -p player-core --test subsonic_write` covers what went on the
//! wire, against a fake server that records it. This covers the half a fake
//! server cannot: whether a real Navidrome actually *did* the thing. That
//! distinction is the whole reason this milestone earns a live check -- every
//! interesting mistake available here (an album starred as if it were a folder,
//! a playlist created with one of its five tracks) is a request the server
//! answers `"status": "ok"` to. So nothing below trusts the answer to a write:
//! it asks again afterwards and looks.
//!
//! Needs credentials, through the environment for `subsonic_smoke`'s reason:
//!
//! ```text
//!   LATENTPLAYER_SUBSONIC_URL       (default http://localhost:4533)
//!   LATENTPLAYER_SUBSONIC_USER
//!   LATENTPLAYER_SUBSONIC_PASSWORD
//!   LATENTPLAYER_SUBSONIC_SCROBBLE  (opt-in; see below)
//! ```
//!
//! # What this leaves behind
//!
//! Nothing, by design, with one exception it refuses to make on its own.
//! The playlist it creates is deleted at the end; every star and rating is read
//! before it is changed and put back afterwards. The exception is a **play**:
//! `scrobble` with `submission=true` writes to a listening history that this app
//! has no way to edit, and which the server may be relaying to Last.fm. So the
//! "now playing" half runs always (it expires by itself and counts nothing) and
//! the submission runs only when `LATENTPLAYER_SUBSONIC_SCROBBLE=1` says so.

use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{
    AlbumSort, BrowseRequest, Credentials, MarkRequest, PlaylistRequest, Server, StarTarget,
};

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

    // --- something real to write about -------------------------------------
    let album = match client.browse(&BrowseRequest::Albums {
        sort: AlbumSort::AlphabeticalByName,
        size: 1,
        offset: 0,
    }) {
        Ok(result) => match result.albums.into_iter().next() {
            Some(album) => album,
            None => {
                println!("  [SKIP] everything: the server has no albums to write about");
                return;
            }
        },
        Err(e) => {
            println!("  [FAIL] asking the server for an album: {e}");
            return;
        }
    };
    let tracks = match client.browse(&BrowseRequest::Album {
        id: album.id.clone(),
    }) {
        Ok(result) => result.tracks,
        Err(e) => {
            println!("  [FAIL] opening an album: {e}");
            return;
        }
    };
    if tracks.is_empty() {
        println!("  [SKIP] everything: \"{}\" has no tracks", album.name);
        return;
    }
    println!(
        "  using \"{}\" by {} ({} tracks)",
        album.name,
        album.artist,
        tracks.len()
    );

    // --- playlists ----------------------------------------------------------
    let before = match client.playlists(&PlaylistRequest::List) {
        Ok(result) => result.playlists,
        Err(e) => {
            println!("  [FAIL] listing playlists: {e}");
            return;
        }
    };
    check(
        "the server's playlists come back",
        true,
        format!("{} playlists", before.len()),
    );

    let name = format!(
        "latentPlayer write_smoke {}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let first_two: Vec<String> = tracks.iter().take(2).map(|t| t.id.clone()).collect();
    let created = match client.playlists(&PlaylistRequest::Create {
        name: name.clone(),
        song_ids: first_two.clone(),
    }) {
        Ok(result) => result,
        Err(e) => {
            println!("  [FAIL] creating a playlist: {e}");
            return;
        }
    };
    let Some(playlist_id) = created.playlists.first().map(|p| p.id.clone()) else {
        println!("  [FAIL] creating a playlist: the server returned no playlist to work with");
        return;
    };
    // The repeated-`songId` check, live. A client that sent one would create a
    // playlist of one track and the server would still say ok.
    check(
        "a playlist is created holding every track it was given",
        created.tracks.len() == first_two.len(),
        format!(
            "asked for {}, the server made a playlist of {}",
            first_two.len(),
            created.tracks.len()
        ),
    );

    let listed = client
        .playlists(&PlaylistRequest::List)
        .map(|result| result.playlists)
        .unwrap_or_default();
    check(
        "the new playlist is in the list afterwards",
        listed.iter().any(|p| p.id == playlist_id),
        format!("{} playlists, was {}", listed.len(), before.len()),
    );

    // Appending, which is `updatePlaylist` -- the endpoint that names its id
    // `playlistId` where every other one says `id`.
    if tracks.len() > 2 {
        let extra: Vec<String> = tracks
            .iter()
            .skip(2)
            .take(2)
            .map(|t| t.id.clone())
            .collect();
        let added = extra.len();
        if let Err(e) = client.playlists(&PlaylistRequest::AddSongs {
            id: playlist_id.clone(),
            song_ids: extra,
        }) {
            check("tracks can be added to a playlist", false, format!("{e}"));
        } else {
            let now = client
                .playlists(&PlaylistRequest::Get {
                    id: playlist_id.clone(),
                })
                .map(|result| result.tracks.len())
                .unwrap_or(0);
            check(
                "tracks can be added to a playlist",
                now == first_two.len() + added,
                format!("{} tracks after adding {added}", now),
            );
        }
    }

    // Removing by position, and checking that the position removed is the one
    // that went -- an off-by-one here removes the wrong track silently.
    let held = client
        .playlists(&PlaylistRequest::Get {
            id: playlist_id.clone(),
        })
        .map(|result| result.tracks)
        .unwrap_or_default();
    if held.len() > 1 {
        let dropped = held[0].id.clone();
        if let Err(e) = client.playlists(&PlaylistRequest::RemoveIndices {
            id: playlist_id.clone(),
            indices: vec![0],
        }) {
            check("a track can be removed by position", false, format!("{e}"));
        } else {
            let left = client
                .playlists(&PlaylistRequest::Get {
                    id: playlist_id.clone(),
                })
                .map(|result| result.tracks)
                .unwrap_or_default();
            check(
                "a track can be removed by position, and it is the right one",
                left.len() == held.len() - 1 && !left.iter().any(|t| t.id == dropped),
                format!("{} tracks left, was {}", left.len(), held.len()),
            );
        }
    }

    let renamed = format!("{name} (renamed)");
    if let Err(e) = client.playlists(&PlaylistRequest::Rename {
        id: playlist_id.clone(),
        name: renamed.clone(),
    }) {
        check("a playlist can be renamed", false, format!("{e}"));
    } else {
        let now = client
            .playlists(&PlaylistRequest::Get {
                id: playlist_id.clone(),
            })
            .ok()
            .and_then(|result| result.playlists.first().map(|p| p.name.clone()))
            .unwrap_or_default();
        check(
            "a playlist can be renamed",
            now == renamed,
            format!("the server calls it \"{now}\""),
        );
    }

    // --- stars and ratings, each put back afterwards ------------------------
    //
    // The one that can only be checked here: whether the server *recorded* it.
    // An album starred through the wrong parameter is a successful request that
    // changed nothing, so this stars, asks again, and looks.
    let track = tracks[0].clone();
    let was_starred = track.starred;
    match client.mark(&if was_starred {
        MarkRequest::Unstar {
            target: StarTarget::Song,
            id: track.id.clone(),
        }
    } else {
        MarkRequest::Star {
            target: StarTarget::Song,
            id: track.id.clone(),
        }
    }) {
        Ok(()) => {
            let now = reread_track(&client, &album.id, &track.id).map(|t| t.starred);
            check(
                "starring a track is recorded by the server",
                now == Some(!was_starred),
                format!("was {was_starred}, now {now:?}"),
            );
        }
        Err(e) => check(
            "starring a track is recorded by the server",
            false,
            format!("{e}"),
        ),
    }
    // Back the way it was found.
    let _ = client.mark(&if was_starred {
        MarkRequest::Star {
            target: StarTarget::Song,
            id: track.id.clone(),
        }
    } else {
        MarkRequest::Unstar {
            target: StarTarget::Song,
            id: track.id.clone(),
        }
    });

    // The same for an album, which is the parameter-name decision itself
    // (`albumId`, not `id`) put in front of a real server.
    let album_was_starred = album.starred;
    match client.mark(&if album_was_starred {
        MarkRequest::Unstar {
            target: StarTarget::Album,
            id: album.id.clone(),
        }
    } else {
        MarkRequest::Star {
            target: StarTarget::Album,
            id: album.id.clone(),
        }
    }) {
        Ok(()) => {
            let now = client
                .browse(&BrowseRequest::Album {
                    id: album.id.clone(),
                })
                .ok()
                .and_then(|result| result.albums.first().map(|a| a.starred));
            check(
                "starring an album is recorded -- i.e. albumId is the right parameter",
                now == Some(!album_was_starred),
                format!("was {album_was_starred}, now {now:?}"),
            );
        }
        Err(e) => check(
            "starring an album is recorded -- i.e. albumId is the right parameter",
            false,
            format!("{e}"),
        ),
    }
    let _ = client.mark(&if album_was_starred {
        MarkRequest::Star {
            target: StarTarget::Album,
            id: album.id.clone(),
        }
    } else {
        MarkRequest::Unstar {
            target: StarTarget::Album,
            id: album.id.clone(),
        }
    });

    let was_rated = track.rating;
    let wanted = if was_rated == Some(4) { 3 } else { 4 };
    match client.mark(&MarkRequest::SetRating {
        id: track.id.clone(),
        rating: wanted,
    }) {
        Ok(()) => {
            let now = reread_track(&client, &album.id, &track.id).and_then(|t| t.rating);
            check(
                "a rating is recorded by the server",
                now == Some(wanted),
                format!("asked for {wanted}, the server says {now:?}"),
            );
        }
        Err(e) => check("a rating is recorded by the server", false, format!("{e}")),
    }
    let _ = client.mark(&MarkRequest::SetRating {
        id: track.id.clone(),
        rating: was_rated.unwrap_or(0),
    });

    // --- scrobbling ---------------------------------------------------------
    match client.scrobble(&track.id, false, None) {
        Ok(()) => check(
            "\"now playing\" is accepted",
            true,
            format!("announced \"{}\"", track.title),
        ),
        Err(e) => check("\"now playing\" is accepted", false, format!("{e}")),
    }

    if std::env::var("LATENTPLAYER_SUBSONIC_SCROBBLE").as_deref() == Ok("1") {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        match client.scrobble(&track.id, true, Some(now_ms)) {
            Ok(()) => check(
                "a play is accepted",
                true,
                format!("submitted \"{}\" -- this one is not undone", track.title),
            ),
            Err(e) => check("a play is accepted", false, format!("{e}")),
        }
    } else {
        println!(
            "  [SKIP] submitting a play: set LATENTPLAYER_SUBSONIC_SCROBBLE=1 to allow it. \
             It adds to a listening history nothing here can take back."
        );
    }

    // --- and put the library back --------------------------------------------
    match client.playlists(&PlaylistRequest::Delete {
        id: playlist_id.clone(),
    }) {
        Ok(_) => {
            let left = client
                .playlists(&PlaylistRequest::List)
                .map(|result| result.playlists)
                .unwrap_or_default();
            check(
                "the playlist is deleted afterwards, leaving the library as it was",
                !left.iter().any(|p| p.id == playlist_id),
                format!(
                    "{} playlists, was {} before this ran",
                    left.len(),
                    before.len()
                ),
            );
        }
        Err(e) => check(
            "the playlist is deleted afterwards, leaving the library as it was",
            false,
            format!("{e} -- \"{renamed}\" is still on the server"),
        ),
    }
}

/// One track as the server states it *now*, by asking for its album again.
///
/// There is no "get one song" in this app's browse requests, and adding one for
/// a smoke test would be the tail wagging the dog: an album is how the app
/// itself arrives at a track, so it is how this checks one.
fn reread_track(
    client: &HttpClient,
    album_id: &str,
    track_id: &str,
) -> Option<player_core::subsonic::TrackRow> {
    client
        .browse(&BrowseRequest::Album {
            id: album_id.to_string(),
        })
        .ok()?
        .tracks
        .into_iter()
        .find(|track| track.id == track_id)
}
