// The shapes a music server's library arrives in (Phase 2 M3), for both shells.
//
// Every other pair of shell-specific modules in this app mirrors its types --
// `TransportState` is written out twice, in nativePlayer.ts and playerEngine.ts,
// so each adapter reads as a self-contained account of its own shell. That works
// for one struct of six numbers and does not for five, so the browse types live
// here once and both adapters import them.
//
// They are not invented here either. Every field below is a field of
// `ArtistRow`/`AlbumRow`/`TrackRow`/`BrowseRequest` in
// player-core/src/subsonic/browse.rs, which is what serialises them on both
// shells -- through Tauri's command boundary on one, through a wasm binding on
// the other. So this file is a transcription, and the thing that keeps it honest
// is that both shells fill it from the same Rust.

/// One artist.
export interface ArtistRow {
  id: string;
  name: string;
  /// The id to fetch art with -- not a URL, and never the image itself.
  coverArt: string | null;
  albumCount: number;
  /// Whether the listener has starred this (M4). A boolean here and a *date* on
  /// the wire -- the conversion happens once, in Rust.
  starred: boolean;
}

/// One album.
export interface AlbumRow {
  id: string;
  name: string;
  artist: string;
  artistId: string | null;
  coverArt: string | null;
  songCount: number;
  /// 0 when the server did not say, which the UI renders as an em dash rather
  /// than as 0:00 -- the same distinction the local library makes.
  durationSeconds: number;
  year: number | null;
  starred: boolean;
  /// The listener's own rating, 1-5, or null (M4).
  rating: number | null;
}

/// One track. `id` and `suffix` are what playback needs (playlist.ts turns them
/// into a queue entry); the rest is what a list shows.
export interface ServerTrack {
  id: string;
  title: string;
  artist: string;
  album: string;
  suffix: string;
  durationSeconds: number;
  coverArt: string | null;
  trackNumber: number | null;
  starred: boolean;
  rating: number | null;
}

/// How an album list is ordered. The server rejects anything else.
export type AlbumSort =
  | "alphabeticalByName"
  | "alphabeticalByArtist"
  | "newest"
  | "recent"
  | "frequent"
  | "random"
  | "starred";

/// One thing the UI can ask the server's library for.
///
/// Sent across each shell's own boundary as-is -- a command argument natively, a
/// JSON string into wasm on the web -- so no endpoint name or query parameter
/// appears in TypeScript at all.
export type BrowseRequest =
  | { kind: "artists" }
  | { kind: "artist"; id: string }
  | { kind: "albums"; sort: AlbumSort; size: number; offset: number }
  | { kind: "album"; id: string }
  | { kind: "search"; query: string; count?: number };

/// What a browse request answers with: one shape for all five requests, with
/// whatever the request had nothing to say about left empty.
export interface BrowseResult {
  artists: ArtistRow[];
  albums: AlbumRow[];
  tracks: ServerTrack[];
}

export function emptyBrowseResult(): BrowseResult {
  return { artists: [], albums: [], tracks: [] };
}

/// One playlist on the server (M4). `PlaylistRow` in
/// player-core/src/subsonic/playlists.rs, transcribed like everything above it.
export interface ServerPlaylist {
  id: string;
  name: string;
  comment: string;
  owner: string;
  public: boolean;
  songCount: number;
  /// 0 when the server did not say, rendered as an em dash rather than 0:00.
  durationSeconds: number;
  /// ISO 8601, as the server states it.
  changed: string;
  coverArt: string | null;
}

/// One thing the UI can ask about, or do to, the server's playlists.
///
/// The four writes are why the ids are spelled out rather than passed loosely:
/// `updatePlaylist` takes a `playlistId` where every other call takes an `id`,
/// and `songIndexToRemove` counts positions in the playlist as the server
/// currently holds it -- neither of which appears on this side of the boundary.
export type PlaylistRequest =
  | { kind: "list" }
  | { kind: "get"; id: string }
  | { kind: "create"; name: string; songIds: string[] }
  | { kind: "rename"; id: string; name: string }
  | { kind: "addSongs"; id: string; songIds: string[] }
  | { kind: "removeIndices"; id: string; indices: number[] }
  | { kind: "delete"; id: string };

/// What a playlist request answers with: one shape for all six, with whatever
/// the request had nothing to say about left empty.
export interface PlaylistResult {
  playlists: ServerPlaylist[];
  tracks: ServerTrack[];
}

export function emptyPlaylistResult(): PlaylistResult {
  return { playlists: [], tracks: [] };
}

/// What is being starred, which decides how its id is spelled on the wire --
/// `id` for a song, `albumId` for an album, `artistId` for an artist. Sending
/// the wrong one succeeds and stars nothing, which is why this is part of the
/// request rather than something a caller remembers.
export type StarTarget = "song" | "album" | "artist";

/// One mark to put on, or take off, something in the server's library.
export type MarkRequest =
  | { kind: "star"; target: StarTarget; id: string }
  | { kind: "unstar"; target: StarTarget; id: string }
  /// 1-5, or 0 to clear. `setRating` takes one id for every kind, unlike star.
  | { kind: "setRating"; id: string; rating: number };
