//! Browsing a server's library (Phase 2 M3): what to ask for, and what the
//! answer means.
//!
//! Five endpoints with five different payload shapes -- `getArtists` hides its
//! artists under an alphabetical index, `getAlbum` returns one album with its
//! songs inside it, `search3` returns all three kinds at once -- and every one of
//! those shapes is a thing a client can get subtly wrong and show an empty list
//! for. So they are known in exactly one place, [`parse_browse`], and both shells
//! call it: natively through `subsonic/http.rs`, in the browser through a wasm
//! binding (tasks/phase-2.md decision 2).
//!
//! The requests are here for the same reason. A parameter name that drifts
//! between the shells (`type` vs `sort`, a `size` above the spec's maximum) is a
//! server error on one shell and not the other, so [`BrowseRequest`] owns both
//! the endpoint and its parameters and the shells only choose *which* request to
//! make.
//!
//! What comes back is deliberately not the wire shape. The structs the server
//! fills in are private to this module; what leaves it are the three row types
//! below, which are what a list in the UI actually needs and are identical on
//! both shells because both are serialised from here.

use serde::{Deserialize, Serialize};

use super::{Parsed, Song, SubsonicError};

/// Largest page `getAlbumList2` allows. Clamped rather than passed through: a
/// server answers a bigger one with error 10 (a wrong parameter), which would
/// read to the user as "browsing is broken".
const MAX_ALBUM_PAGE: u32 = 500;

/// How many of each kind `search3` returns unless asked otherwise.
const DEFAULT_SEARCH_COUNT: u32 = 40;

/// How an album list is ordered. An enum rather than a string because the server
/// rejects anything it does not recognise, and this is a value the UI picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AlbumSort {
    AlphabeticalByName,
    AlphabeticalByArtist,
    Newest,
    Recent,
    Frequent,
    Random,
    Starred,
}

impl AlbumSort {
    /// The value the `type` parameter takes. Spelled out rather than derived
    /// from the enum's own serialisation, so renaming a variant here cannot
    /// quietly change what goes on the wire.
    fn as_param(self) -> &'static str {
        match self {
            Self::AlphabeticalByName => "alphabeticalByName",
            Self::AlphabeticalByArtist => "alphabeticalByArtist",
            Self::Newest => "newest",
            Self::Recent => "recent",
            Self::Frequent => "frequent",
            Self::Random => "random",
            Self::Starred => "starred",
        }
    }
}

/// One thing the UI can ask a server's library for.
///
/// Deserialisable so that both shells can send one across their own boundary --
/// a Tauri command argument on one side, a JSON string into wasm on the other --
/// and neither has to spell out an endpoint or a parameter name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BrowseRequest {
    /// Every artist, as the server's own alphabetical index.
    Artists,
    /// One artist, and their albums.
    Artist { id: String },
    /// A page of albums.
    Albums {
        sort: AlbumSort,
        size: u32,
        offset: u32,
    },
    /// One album, and its tracks.
    Album { id: String },
    /// Free text, across artists, albums and tracks. Searching happens on the
    /// server (decision 6: the server's library is browsed live rather than
    /// mirrored), so a library too big to hold locally is still searchable.
    Search {
        query: String,
        #[serde(default)]
        count: Option<u32>,
    },
}

impl BrowseRequest {
    pub fn endpoint(&self) -> &'static str {
        match self {
            Self::Artists => "getArtists",
            Self::Artist { .. } => "getArtist",
            Self::Albums { .. } => "getAlbumList2",
            Self::Album { .. } => "getAlbum",
            Self::Search { .. } => "search3",
        }
    }

    /// The query parameters, owned because most of them are numbers that have to
    /// be rendered somewhere and this is the somewhere.
    pub fn params(&self) -> Vec<(String, String)> {
        match self {
            Self::Artists => Vec::new(),
            Self::Artist { id } | Self::Album { id } => vec![("id".into(), id.clone())],
            Self::Albums { sort, size, offset } => vec![
                ("type".into(), sort.as_param().into()),
                ("size".into(), size.min(&MAX_ALBUM_PAGE).to_string()),
                ("offset".into(), offset.to_string()),
            ],
            Self::Search { query, count } => {
                let count = count.unwrap_or(DEFAULT_SEARCH_COUNT).to_string();
                vec![
                    ("query".into(), query.clone()),
                    // All three counts, because a search that returned twenty
                    // artists and no songs would look like a broken search.
                    ("artistCount".into(), count.clone()),
                    ("albumCount".into(), count.clone()),
                    ("songCount".into(), count),
                ]
            }
        }
    }
}

/// One artist, as a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRow {
    pub id: String,
    pub name: String,
    /// The id to fetch art with -- *not* an image, and not a URL. Cover art
    /// never travels as JSON on either shell (M7 decision 5).
    pub cover_art: Option<String>,
    pub album_count: u32,
}

/// One album, as a row.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumRow {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub cover_art: Option<String>,
    pub song_count: u32,
    /// Seconds. 0 when the server did not say, which the UI renders as an em
    /// dash rather than as `0:00` -- the same distinction the local library
    /// makes for a file whose container states no length.
    pub duration_seconds: f64,
    pub year: Option<u32>,
}

/// One track, as a row -- and as everything else on this side of the boundary
/// needs it: `playlist.ts` builds a playable entry out of exactly `id` and
/// `suffix`, and the rest is what a list shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The file's own extension on the server, which the decoder wants as a
    /// format hint (see [`Song`]).
    pub suffix: String,
    pub duration_seconds: f64,
    pub cover_art: Option<String>,
    pub track_number: Option<u32>,
}

impl From<Song> for TrackRow {
    fn from(song: Song) -> Self {
        Self {
            id: song.id,
            title: song.title,
            artist: song.artist,
            album: song.album,
            suffix: song.suffix,
            duration_seconds: song.duration.unwrap_or(0) as f64,
            cover_art: song.cover_art,
            track_number: song.track,
        }
    }
}

/// What a browse request answers with.
///
/// One shape for all five requests rather than five, because the caller already
/// knows what it asked for and a single shape is what lets the UI render
/// "artists, albums and tracks" from a search and from a browse with the same
/// code. A request simply leaves the lists it has nothing to say about empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseResult {
    pub artists: Vec<ArtistRow>,
    pub albums: Vec<AlbumRow>,
    pub tracks: Vec<TrackRow>,
}

// --- the wire shapes, which stop here ---------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
struct WireArtist {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "coverArt", default)]
    cover_art: Option<String>,
    #[serde(rename = "albumCount", default)]
    album_count: Option<u32>,
}

impl From<WireArtist> for ArtistRow {
    fn from(artist: WireArtist) -> Self {
        Self {
            id: artist.id,
            name: artist.name,
            cover_art: artist.cover_art,
            album_count: artist.album_count.unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct WireAlbum {
    #[serde(default)]
    id: String,
    /// OpenSubsonic's album name...
    #[serde(default)]
    name: String,
    /// ...and the field older servers (and `getArtist`'s nested albums) use for
    /// the same thing. Both are read and whichever is present wins, because a
    /// client that reads only one shows a list of blank rows against the other.
    #[serde(default)]
    album: String,
    #[serde(default)]
    artist: String,
    #[serde(rename = "artistId", default)]
    artist_id: Option<String>,
    #[serde(rename = "coverArt", default)]
    cover_art: Option<String>,
    #[serde(rename = "songCount", default)]
    song_count: Option<u32>,
    #[serde(default)]
    duration: Option<u32>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    song: Vec<Song>,
}

impl From<WireAlbum> for AlbumRow {
    fn from(album: WireAlbum) -> Self {
        let name = if album.name.is_empty() {
            album.album
        } else {
            album.name
        };
        Self {
            id: album.id,
            name,
            artist: album.artist,
            artist_id: album.artist_id,
            cover_art: album.cover_art,
            song_count: album.song_count.unwrap_or(0),
            duration_seconds: album.duration.unwrap_or(0) as f64,
            year: album.year,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ArtistsPayload {
    #[serde(default)]
    artists: ArtistIndexes,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ArtistIndexes {
    #[serde(default)]
    index: Vec<ArtistIndex>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ArtistIndex {
    #[serde(default)]
    artist: Vec<WireArtist>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ArtistPayload {
    #[serde(default)]
    artist: WireArtistWithAlbums,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct WireArtistWithAlbums {
    #[serde(flatten)]
    artist: WireArtist,
    #[serde(default)]
    album: Vec<WireAlbum>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AlbumListPayload {
    #[serde(rename = "albumList2", default)]
    list: AlbumListInner,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AlbumListInner {
    #[serde(default)]
    album: Vec<WireAlbum>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AlbumPayload {
    #[serde(default)]
    album: WireAlbum,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SearchPayload {
    #[serde(rename = "searchResult3", default)]
    result: SearchInner,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SearchInner {
    #[serde(default)]
    artist: Vec<WireArtist>,
    #[serde(default)]
    album: Vec<WireAlbum>,
    #[serde(default)]
    song: Vec<Song>,
}

/// Reads the response to `request`.
///
/// The one place the five payload shapes are known. Note every list is
/// `#[serde(default)]` all the way down: a server with no artists at all answers
/// `{"artists":{}}` rather than with an empty index, and a client that required
/// the key would report that as a malformed response.
pub fn parse_browse(
    request: &BrowseRequest,
    bytes: &[u8],
) -> Result<Parsed<BrowseResult>, SubsonicError> {
    Ok(match request {
        BrowseRequest::Artists => {
            let parsed = super::parse::<ArtistsPayload>(bytes)?;
            let artists = parsed
                .payload
                .artists
                .index
                .into_iter()
                .flat_map(|index| index.artist)
                .map(ArtistRow::from)
                .collect();
            Parsed {
                info: parsed.info,
                payload: BrowseResult {
                    artists,
                    ..Default::default()
                },
            }
        }
        BrowseRequest::Artist { .. } => {
            let parsed = super::parse::<ArtistPayload>(bytes)?;
            let WireArtistWithAlbums { artist, album } = parsed.payload.artist;
            Parsed {
                info: parsed.info,
                payload: BrowseResult {
                    artists: vec![ArtistRow::from(artist)],
                    albums: album.into_iter().map(AlbumRow::from).collect(),
                    ..Default::default()
                },
            }
        }
        BrowseRequest::Albums { .. } => {
            let parsed = super::parse::<AlbumListPayload>(bytes)?;
            Parsed {
                info: parsed.info,
                payload: BrowseResult {
                    albums: parsed
                        .payload
                        .list
                        .album
                        .into_iter()
                        .map(AlbumRow::from)
                        .collect(),
                    ..Default::default()
                },
            }
        }
        BrowseRequest::Album { .. } => {
            let parsed = super::parse::<AlbumPayload>(bytes)?;
            let mut album = parsed.payload.album;
            let songs = std::mem::take(&mut album.song);
            Parsed {
                info: parsed.info,
                payload: BrowseResult {
                    albums: vec![AlbumRow::from(album)],
                    tracks: songs.into_iter().map(TrackRow::from).collect(),
                    ..Default::default()
                },
            }
        }
        BrowseRequest::Search { .. } => {
            let parsed = super::parse::<SearchPayload>(bytes)?;
            let SearchInner {
                artist,
                album,
                song,
            } = parsed.payload.result;
            Parsed {
                info: parsed.info,
                payload: BrowseResult {
                    artists: artist.into_iter().map(ArtistRow::from).collect(),
                    albums: album.into_iter().map(AlbumRow::from).collect(),
                    tracks: song.into_iter().map(TrackRow::from).collect(),
                },
            }
        }
    })
}

/// The size, in pixels, this app asks for cover art at.
///
/// Here rather than in either shell because both ask, and asking for different
/// sizes would mean two caches of the same picture and two answers to "is this
/// thumbnail sharp". Big enough not to be soft on a high-DPI display, small
/// enough that a screenful of rows is a few hundred kilobytes rather than a
/// gridful of 3000px originals.
pub const COVER_SIZE_PX: u32 = 300;

/// The parameters for fetching one piece of cover art.
///
/// `size` is asked for rather than taken as it comes: a browse view shows a
/// gridful of thumbnails, and a server that would happily send a 3000px original
/// for each of them is a slow list and a lot of memory. The server scales it.
pub fn cover_art_params(id: &str, size: u32) -> Vec<(String, String)> {
    vec![("id".into(), id.into()), ("size".into(), size.to_string())]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_params(request: &BrowseRequest) -> Vec<(String, String)> {
        request.params()
    }

    #[test]
    fn an_album_page_is_clamped_to_what_the_spec_allows() {
        let request = BrowseRequest::Albums {
            sort: AlbumSort::AlphabeticalByName,
            size: 5_000,
            offset: 20,
        };
        let params = request_params(&request);
        assert_eq!(request.endpoint(), "getAlbumList2");
        assert!(params.contains(&("type".into(), "alphabeticalByName".into())));
        assert!(params.contains(&("size".into(), "500".into())));
        assert!(params.contains(&("offset".into(), "20".into())));
    }

    #[test]
    fn a_search_asks_for_all_three_kinds() {
        let request = BrowseRequest::Search {
            query: "miles".into(),
            count: Some(7),
        };
        let params = request_params(&request);
        assert_eq!(request.endpoint(), "search3");
        assert!(params.contains(&("query".into(), "miles".into())));
        for key in ["artistCount", "albumCount", "songCount"] {
            assert!(
                params.contains(&(key.into(), "7".into())),
                "{key} should be asked for"
            );
        }
    }

    #[test]
    fn a_browse_request_survives_a_round_trip_through_json() {
        // Both shells send one of these across a boundary: a Tauri command
        // argument natively, a JSON string into wasm on the web.
        let request = BrowseRequest::Album { id: "42".into() };
        let json = serde_json::to_string(&request).expect("serialise");
        assert_eq!(json, r#"{"kind":"album","id":"42"}"#);
        let back: BrowseRequest = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, request);
    }

    /// The index shape is the one a client is most likely to get wrong, because
    /// the artists are two levels down and grouped by letter.
    #[test]
    fn artists_are_read_out_of_the_alphabetical_index() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","artists":{
            "ignoredArticles":"The","index":[
              {"name":"C","artist":[{"id":"1","name":"Cocteau Twins","coverArt":"ar-1","albumCount":8}]},
              {"name":"M","artist":[{"id":"2","name":"Miles Davis","albumCount":21},
                                    {"id":"3","name":"Mogwai","albumCount":10}]}]}}}"#;
        let result = parse_browse(&BrowseRequest::Artists, body).expect("parse");
        let names: Vec<&str> = result
            .payload
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["Cocteau Twins", "Miles Davis", "Mogwai"]);
        assert_eq!(result.payload.artists[0].cover_art.as_deref(), Some("ar-1"));
        assert_eq!(result.payload.artists[1].album_count, 21);
        // Nothing else was invented.
        assert!(result.payload.albums.is_empty() && result.payload.tracks.is_empty());
    }

    #[test]
    fn a_server_with_no_artists_is_an_empty_list_rather_than_an_error() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","artists":{}}}"#;
        let result = parse_browse(&BrowseRequest::Artists, body).expect("parse");
        assert!(result.payload.artists.is_empty());
    }

    #[test]
    fn an_album_carries_its_tracks_and_its_own_row() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{
            "id":"10","name":"Kind of Blue","artist":"Miles Davis","artistId":"2",
            "coverArt":"al-10","songCount":2,"duration":1234,"year":1959,
            "song":[
              {"id":"100","title":"So What","artist":"Miles Davis","album":"Kind of Blue",
               "suffix":"flac","duration":545,"track":1,"coverArt":"al-10"},
              {"id":"101","title":"Freddie Freeloader","artist":"Miles Davis",
               "album":"Kind of Blue","suffix":"flac","duration":589,"track":2}]}}}"#;
        let result = parse_browse(&BrowseRequest::Album { id: "10".into() }, body).expect("parse");

        assert_eq!(result.payload.albums.len(), 1);
        let album = &result.payload.albums[0];
        assert_eq!(album.name, "Kind of Blue");
        assert_eq!(album.song_count, 2);
        assert_eq!(album.duration_seconds, 1234.0);
        assert_eq!(album.year, Some(1959));

        assert_eq!(result.payload.tracks.len(), 2);
        let first = &result.payload.tracks[0];
        assert_eq!(first.id, "100");
        assert_eq!(first.title, "So What");
        // The two fields playback actually needs.
        assert_eq!(first.suffix, "flac");
        assert_eq!(first.duration_seconds, 545.0);
        assert_eq!(first.track_number, Some(1));
    }

    /// `getAlbumList2` states an album's name in `name`; `getArtist`'s nested
    /// albums state it in `album`. A client that reads one gets blank rows from
    /// the other, which is a bug that looks like an empty library.
    #[test]
    fn an_album_named_either_way_reads_the_same() {
        let modern = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","albumList2":{
            "album":[{"id":"1","name":"Spiderland","artist":"Slint","songCount":6}]}}}"#;
        let older = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","artist":{
            "id":"9","name":"Slint","albumCount":1,
            "album":[{"id":"1","album":"Spiderland","artist":"Slint","songCount":6}]}}}"#;

        let from_list = parse_browse(
            &BrowseRequest::Albums {
                sort: AlbumSort::Newest,
                size: 10,
                offset: 0,
            },
            modern,
        )
        .expect("parse the album list");
        let from_artist = parse_browse(&BrowseRequest::Artist { id: "9".into() }, older)
            .expect("parse the artist");

        assert_eq!(from_list.payload.albums[0].name, "Spiderland");
        assert_eq!(from_artist.payload.albums[0].name, "Spiderland");
        assert_eq!(from_artist.payload.artists[0].name, "Slint");
    }

    #[test]
    fn a_search_returns_all_three_kinds_at_once() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","searchResult3":{
            "artist":[{"id":"2","name":"Miles Davis","albumCount":21}],
            "album":[{"id":"10","name":"Kind of Blue","artist":"Miles Davis","songCount":5}],
            "song":[{"id":"100","title":"So What","artist":"Miles Davis","suffix":"flac","duration":545}]}}}"#;
        let request = BrowseRequest::Search {
            query: "miles".into(),
            count: None,
        };
        let result = parse_browse(&request, body).expect("parse");
        assert_eq!(result.payload.artists.len(), 1);
        assert_eq!(result.payload.albums.len(), 1);
        assert_eq!(result.payload.tracks.len(), 1);
        assert_eq!(result.payload.tracks[0].title, "So What");
    }

    /// The trap this whole module inherits: a failure arrives as an HTTP 200.
    /// Browsing must not turn one into an empty library.
    #[test]
    fn a_failure_envelope_is_still_a_failure_when_browsing() {
        let body = br#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":70,"message":"Artist not found"}}}"#;
        let error = parse_browse(&BrowseRequest::Artist { id: "nope".into() }, body)
            .expect_err("a failed envelope must not parse as an empty artist");
        assert!(matches!(error, SubsonicError::Server { .. }));
    }

    #[test]
    fn cover_art_is_asked_for_at_a_size() {
        let params = cover_art_params("al-10", 200);
        assert_eq!(params[0], ("id".into(), "al-10".into()));
        assert_eq!(params[1], ("size".into(), "200".into()));
    }
}
