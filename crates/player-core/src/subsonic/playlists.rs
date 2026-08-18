//! The server's playlists (Phase 2 M4): the five calls that read and change
//! them, and the shapes their answers arrive in.
//!
//! Built the way `browse.rs` is, and for the same reason -- one request type
//! that knows its own endpoint and parameters, one result shape whatever was
//! asked, and both shells calling both halves rather than spelling out an
//! endpoint name in TypeScript. What is new here is that three of the five
//! *change* something on someone else's machine, so the parameters were read off
//! the spec rather than remembered, and `tests/subsonic_write.rs` asks the fake
//! server what actually arrived.
//!
//! Two shapes worth knowing before reading the code:
//!
//! - `getPlaylists` nests its list two deep, under `playlists.playlist`, while
//!   `getPlaylist` returns a single playlist whose tracks are under `entry` --
//!   not `song`, which is what every other endpoint in this protocol calls a
//!   list of tracks.
//! - `createPlaylist` doubles as an update: given a `playlistId` it replaces
//!   that playlist's contents instead of creating one. This module never uses it
//!   that way (that is [`PlaylistRequest::AddSongs`]'s job, which appends rather
//!   than replaces), but it is why the endpoint returns a playlist where the
//!   other writes return nothing.

use serde::{Deserialize, Serialize};

use super::browse::TrackRow;
use super::{Parsed, Song, SubsonicError};

/// One thing the UI can ask about, or do to, the server's playlists.
///
/// Deserialisable for the same reason [`super::BrowseRequest`] is: each shell
/// sends one across its own boundary -- a Tauri command argument on one side, a
/// JSON string into wasm on the other -- and neither has to name an endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PlaylistRequest {
    /// Every playlist the authenticated user can see, without their contents.
    List,
    /// One playlist, with its tracks.
    Get { id: String },
    /// A new playlist holding `song_ids`, in that order.
    Create {
        name: String,
        #[serde(default)]
        song_ids: Vec<String>,
    },
    /// A new name for an existing playlist.
    Rename { id: String, name: String },
    /// Appends to a playlist, leaving what is already in it alone.
    AddSongs { id: String, song_ids: Vec<String> },
    /// Removes by *position*, which is what the endpoint takes -- see
    /// [`PlaylistRequest::params`] for why that is worth being careful about.
    RemoveIndices { id: String, indices: Vec<u32> },
    /// Deletes the playlist itself. Not the tracks -- a Subsonic playlist is a
    /// list of ids, and deleting one removes nothing from the library.
    Delete { id: String },
}

impl PlaylistRequest {
    pub fn endpoint(&self) -> &'static str {
        match self {
            Self::List => "getPlaylists",
            Self::Get { .. } => "getPlaylist",
            Self::Create { .. } => "createPlaylist",
            Self::Rename { .. } | Self::AddSongs { .. } | Self::RemoveIndices { .. } => {
                "updatePlaylist"
            }
            Self::Delete { .. } => "deletePlaylist",
        }
    }

    /// Whether this request changes something on the server, which is what the
    /// shells use to decide whether a failure is worth putting in front of the
    /// user rather than retrying quietly.
    pub fn is_write(&self) -> bool {
        !matches!(self, Self::List | Self::Get { .. })
    }

    /// The query parameters, in order.
    ///
    /// Order matters here in a way it did not for browsing: `songId`,
    /// `songIdToAdd` and `songIndexToRemove` are **repeated** parameters, one per
    /// value, and the server reads them positionally. That is why this returns a
    /// `Vec` of pairs rather than a map -- a map would keep one of them and
    /// silently add a single track out of twenty.
    ///
    /// `songIndexToRemove` is an index into the playlist as the server currently
    /// holds it, so a caller removing several must send them all in one request
    /// rather than one request per index: after the first removal every later
    /// index means a different track.
    pub fn params(&self) -> Vec<(String, String)> {
        match self {
            Self::List => Vec::new(),
            Self::Get { id } | Self::Delete { id } => vec![("id".into(), id.clone())],
            Self::Create { name, song_ids } => {
                let mut params = vec![("name".into(), name.clone())];
                params.extend(song_ids.iter().map(|id| ("songId".into(), id.clone())));
                params
            }
            Self::Rename { id, name } => vec![
                ("playlistId".into(), id.clone()),
                ("name".into(), name.clone()),
            ],
            Self::AddSongs { id, song_ids } => {
                let mut params = vec![("playlistId".into(), id.clone())];
                params.extend(song_ids.iter().map(|id| ("songIdToAdd".into(), id.clone())));
                params
            }
            Self::RemoveIndices { id, indices } => {
                let mut params = vec![("playlistId".into(), id.clone())];
                params.extend(
                    indices
                        .iter()
                        .map(|index| ("songIndexToRemove".into(), index.to_string())),
                );
                params
            }
        }
    }
}

/// One playlist, as a row.
///
/// `song_count` and `duration_seconds` come from the server rather than from
/// counting the tracks, because [`PlaylistRequest::List`] does not carry them --
/// a list of playlists that all said "0 tracks" until each was opened would be
/// the same bug as M3's blank album names.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistRow {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub owner: String,
    pub public: bool,
    pub song_count: u32,
    /// Seconds. 0 when the server did not say, rendered as an em dash rather
    /// than as `0:00` -- the same distinction the rest of this app makes.
    pub duration_seconds: f64,
    /// ISO 8601, as the server states it. Kept as the server's own string
    /// because nothing here needs to do date arithmetic with it, and parsing a
    /// timestamp only to render it again is a way to be wrong about time zones.
    pub changed: String,
    pub cover_art: Option<String>,
}

/// What a playlist request answers with.
///
/// One shape for all five, like [`super::BrowseResult`]: the reads fill in what
/// they have, the writes fill in nothing (except `Create`, which the server
/// answers with the playlist it just made), and the caller already knows what it
/// asked for.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistResult {
    pub playlists: Vec<PlaylistRow>,
    pub tracks: Vec<TrackRow>,
}

// --- the wire shapes, which stop here ---------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
struct WirePlaylist {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    public: bool,
    #[serde(rename = "songCount", default)]
    song_count: Option<u32>,
    #[serde(default)]
    duration: Option<u32>,
    #[serde(default)]
    changed: String,
    #[serde(rename = "coverArt", default)]
    cover_art: Option<String>,
    /// The tracks, on `getPlaylist` only -- and under `entry`, which is this
    /// endpoint's own name for what every other one calls `song`.
    #[serde(default)]
    entry: Vec<Song>,
}

impl From<WirePlaylist> for PlaylistRow {
    fn from(playlist: WirePlaylist) -> Self {
        Self {
            id: playlist.id,
            name: playlist.name,
            comment: playlist.comment,
            owner: playlist.owner,
            public: playlist.public,
            song_count: playlist.song_count.unwrap_or(0),
            duration_seconds: playlist.duration.unwrap_or(0) as f64,
            changed: playlist.changed,
            cover_art: playlist.cover_art,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PlaylistsPayload {
    #[serde(default)]
    playlists: PlaylistsInner,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PlaylistsInner {
    #[serde(default)]
    playlist: Vec<WirePlaylist>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PlaylistPayload {
    #[serde(default)]
    playlist: WirePlaylist,
}

/// Reads the response to `request`.
///
/// Every write is parsed too, rather than having its body thrown away: a
/// Subsonic failure is an HTTP 200 with an envelope inside it (see this module's
/// parent), so "the request went through" is a sentence only [`super::parse`]
/// can say. A playlist that failed to save silently would be indistinguishable
/// from one that saved.
pub fn parse_playlists(
    request: &PlaylistRequest,
    bytes: &[u8],
) -> Result<Parsed<PlaylistResult>, SubsonicError> {
    Ok(match request {
        PlaylistRequest::List => {
            let parsed = super::parse::<PlaylistsPayload>(bytes)?;
            Parsed {
                info: parsed.info,
                payload: PlaylistResult {
                    playlists: parsed
                        .payload
                        .playlists
                        .playlist
                        .into_iter()
                        .map(PlaylistRow::from)
                        .collect(),
                    tracks: Vec::new(),
                },
            }
        }
        PlaylistRequest::Get { .. } | PlaylistRequest::Create { .. } => {
            let parsed = super::parse::<PlaylistPayload>(bytes)?;
            let mut playlist = parsed.payload.playlist;
            let entries = std::mem::take(&mut playlist.entry);
            Parsed {
                info: parsed.info,
                payload: PlaylistResult {
                    playlists: vec![PlaylistRow::from(playlist)],
                    tracks: entries.into_iter().map(TrackRow::from).collect(),
                },
            }
        }
        // The three that answer with nothing but success. Still parsed, per this
        // function's own docs -- what is being checked is the envelope.
        PlaylistRequest::Rename { .. }
        | PlaylistRequest::AddSongs { .. }
        | PlaylistRequest::RemoveIndices { .. }
        | PlaylistRequest::Delete { .. } => {
            let parsed = super::parse::<super::NoPayload>(bytes)?;
            Parsed {
                info: parsed.info,
                payload: PlaylistResult::default(),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_of(request: &PlaylistRequest) -> Vec<(String, String)> {
        request.params()
    }

    #[test]
    fn each_request_names_its_own_endpoint() {
        assert_eq!(PlaylistRequest::List.endpoint(), "getPlaylists");
        assert_eq!(
            PlaylistRequest::Get { id: "1".into() }.endpoint(),
            "getPlaylist"
        );
        assert_eq!(
            PlaylistRequest::Create {
                name: "n".into(),
                song_ids: vec![]
            }
            .endpoint(),
            "createPlaylist"
        );
        assert_eq!(
            PlaylistRequest::Rename {
                id: "1".into(),
                name: "n".into()
            }
            .endpoint(),
            "updatePlaylist"
        );
        assert_eq!(
            PlaylistRequest::Delete { id: "1".into() }.endpoint(),
            "deletePlaylist"
        );
    }

    #[test]
    fn only_the_two_reads_are_not_writes() {
        assert!(!PlaylistRequest::List.is_write());
        assert!(!PlaylistRequest::Get { id: "1".into() }.is_write());
        assert!(PlaylistRequest::Delete { id: "1".into() }.is_write());
        assert!(PlaylistRequest::AddSongs {
            id: "1".into(),
            song_ids: vec!["a".into()]
        }
        .is_write());
    }

    /// The repeated-parameter case, which is the one a map would break.
    #[test]
    fn creating_sends_one_song_id_per_track_in_order() {
        let params = params_of(&PlaylistRequest::Create {
            name: "Late night".into(),
            song_ids: vec!["a".into(), "b".into(), "c".into()],
        });
        assert_eq!(
            params,
            vec![
                ("name".to_string(), "Late night".to_string()),
                ("songId".to_string(), "a".to_string()),
                ("songId".to_string(), "b".to_string()),
                ("songId".to_string(), "c".to_string()),
            ]
        );
    }

    #[test]
    fn adding_and_removing_name_the_playlist_the_way_update_wants_it() {
        // `updatePlaylist` takes `playlistId`, not `id` -- an easy and silent
        // thing to get wrong, since every other call here takes `id`.
        let add = params_of(&PlaylistRequest::AddSongs {
            id: "42".into(),
            song_ids: vec!["a".into(), "b".into()],
        });
        assert_eq!(add[0], ("playlistId".to_string(), "42".to_string()));
        assert_eq!(
            add[1..],
            [
                ("songIdToAdd".to_string(), "a".to_string()),
                ("songIdToAdd".to_string(), "b".to_string()),
            ]
        );

        let remove = params_of(&PlaylistRequest::RemoveIndices {
            id: "42".into(),
            indices: vec![0, 3],
        });
        assert_eq!(remove[0], ("playlistId".to_string(), "42".to_string()));
        assert_eq!(
            remove[1..],
            [
                ("songIndexToRemove".to_string(), "0".to_string()),
                ("songIndexToRemove".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn a_list_of_playlists_comes_out_from_two_levels_down() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlists":{"playlist":[
              {"id":"10","name":"Late night","songCount":12,"duration":2400,
               "owner":"rick","public":false,"changed":"2026-08-18T04:00:00Z"},
              {"id":"11","name":"Loud","songCount":3,"duration":600,"public":true}
            ]}}}"#;
        let result = parse_playlists(&PlaylistRequest::List, body)
            .expect("a well-formed list should parse")
            .payload;
        assert_eq!(result.playlists.len(), 2);
        assert_eq!(result.playlists[0].name, "Late night");
        assert_eq!(result.playlists[0].song_count, 12);
        assert_eq!(result.playlists[0].duration_seconds, 2400.0);
        assert_eq!(result.playlists[0].owner, "rick");
        assert!(!result.playlists[0].public);
        assert!(result.playlists[1].public);
        assert!(result.tracks.is_empty());
    }

    /// A server with no playlists answers `{"playlists":{}}` rather than with an
    /// empty list -- the same shape M3 had to allow for `getArtists`.
    #[test]
    fn no_playlists_at_all_is_not_a_malformed_response() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlists":{}}}"#;
        let result = parse_playlists(&PlaylistRequest::List, body)
            .expect("an empty list should parse")
            .payload;
        assert!(result.playlists.is_empty());
    }

    /// The `entry`/`song` difference, which is the one that would show an open
    /// playlist as empty.
    #[test]
    fn one_playlist_carries_its_tracks_under_entry() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlist":{"id":"10","name":"Late night","songCount":2,"duration":500,
              "entry":[
                {"id":"s1","title":"First","artist":"A","album":"X","suffix":"flac","duration":250},
                {"id":"s2","title":"Second","artist":"B","album":"Y","suffix":"mp3","duration":250}
              ]}}}"#;
        let result = parse_playlists(&PlaylistRequest::Get { id: "10".into() }, body)
            .expect("a playlist should parse")
            .payload;
        assert_eq!(result.playlists.len(), 1);
        assert_eq!(result.playlists[0].name, "Late night");
        assert_eq!(result.tracks.len(), 2);
        assert_eq!(result.tracks[0].title, "First");
        // The suffix is what playback needs -- a stream URL carries no extension.
        assert_eq!(result.tracks[1].suffix, "mp3");
    }

    #[test]
    fn creating_answers_with_the_playlist_it_made() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlist":{"id":"99","name":"New","songCount":1,"duration":100,
              "entry":[{"id":"s1","title":"First","suffix":"flac"}]}}}"#;
        let result = parse_playlists(
            &PlaylistRequest::Create {
                name: "New".into(),
                song_ids: vec!["s1".into()],
            },
            body,
        )
        .expect("a created playlist should parse")
        .payload;
        assert_eq!(result.playlists[0].id, "99");
        assert_eq!(result.tracks.len(), 1);
    }

    /// The reason the writes are parsed at all rather than having their bodies
    /// dropped: this is what a failed delete looks like, and it is an HTTP 200.
    #[test]
    fn a_write_that_failed_is_an_error_and_not_an_empty_result() {
        let body = br#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":50,"message":"not allowed to delete this playlist"}}}"#;
        let error = parse_playlists(&PlaylistRequest::Delete { id: "10".into() }, body)
            .expect_err("a failure envelope must not read as success");
        assert!(matches!(
            error,
            SubsonicError::Server {
                code: super::super::ErrorCode::NotAuthorized,
                ..
            }
        ));
    }

    #[test]
    fn a_write_that_succeeded_answers_with_nothing() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        let result = parse_playlists(&PlaylistRequest::Delete { id: "10".into() }, body)
            .expect("an empty success should parse")
            .payload;
        assert_eq!(result, PlaylistResult::default());
    }
}
