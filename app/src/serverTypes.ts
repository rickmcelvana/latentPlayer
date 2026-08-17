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
