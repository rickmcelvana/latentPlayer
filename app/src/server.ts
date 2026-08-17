// Adapter for the music server -- the sixth of the shell-choosing modules,
// beside playlist.ts (playback), analysis.ts (metering), scope.ts (the scope),
// library.ts (the local library) and eq.ts (the equaliser).
//
// The two shells differ in exactly two places here, and both are about where a
// secret can safely live and what a client is allowed to reach:
//
//   - Desktop: the address goes in the app data directory and the secret in the
//     OS credential store; the requests are made by Rust with a blocking HTTP
//     client. No CORS rule applies, and no mixed-content rule either.
//   - Web: both go in IndexedDB (the browser has no keychain), and the requests
//     are made by `fetch` -- which can only reach a server that sends CORS
//     headers, and cannot reach an http:// server at all from an https:// page.
//
// What does *not* differ is the protocol: both shells build their URLs and read
// their responses with the same `player_core::subsonic`, natively in one case
// and compiled to wasm in the other (tasks/phase-2.md decision 2).
import { isTauri } from "./isTauri";
import {
  connectServer as connectNative,
  forgetServer as forgetNative,
  reconnectServer as reconnectNative,
  serverBrowse as browseNative,
  serverCoverUrl as coverNative,
  serverRandomTracks as randomNative,
  serverState as stateNative,
  type ServerState,
} from "./nativePlayer";
import {
  browse as browseWeb,
  connectServer as connectWeb,
  emptyServerState,
  forgetServer as forgetWeb,
  randomTracks as randomWeb,
  reconnectServer as reconnectWeb,
  serverCoverUrl as coverWeb,
  serverState as stateWeb,
} from "./subsonicWeb";
import type { BrowseRequest, BrowseResult, ServerTrack } from "./serverTypes";

export type { ServerState };
export type { AlbumRow, AlbumSort, ArtistRow, BrowseRequest, BrowseResult, ServerTrack } from "./serverTypes";
export { emptyBrowseResult } from "./serverTypes";
export { emptyServerState };

/// How the secret is being kept, so the panel can say it plainly where the user
/// types it rather than leaving them to assume the shells are equivalent.
export function secretStorageNote(): string {
  return isTauri()
    ? "Stored in the OS credential store, not in a file."
    : "Stored in this browser's IndexedDB, unencrypted -- a browser has no credential store. The desktop app uses the OS one.";
}

export async function serverState(): Promise<ServerState> {
  return isTauri() ? stateNative() : stateWeb();
}

export async function connectServer(
  baseUrl: string,
  user: string,
  secret: string,
  auth: "password" | "apiKey",
): Promise<ServerState> {
  return isTauri()
    ? connectNative(baseUrl, user, secret, auth)
    : connectWeb(baseUrl, user, secret, auth);
}

export async function reconnectServer(): Promise<ServerState> {
  return isTauri() ? reconnectNative() : reconnectWeb();
}

export async function forgetServer(): Promise<ServerState> {
  return isTauri() ? forgetNative() : forgetWeb();
}

/// A handful of tracks at random from the server (Phase 2 M2).
///
/// The smallest thing that produces playable rows, and deliberately not a browse
/// API -- that is M3. What it is here for is to make remote playback something a
/// person can press rather than something only a harness exercises.
export async function serverRandomTracks(count: number): Promise<ServerTrack[]> {
  return isTauri() ? randomNative(count) : randomWeb(count);
}

/// Browsing the server's library (Phase 2 M3): artists, one artist's albums, a
/// page of albums, one album's tracks, or a search.
///
/// The whole adapter is these two lines, which is the point: what a request is
/// and what its answer means live in Rust, so this only picks the shell.
export async function serverBrowse(request: BrowseRequest): Promise<BrowseResult> {
  return isTauri() ? browseNative(request) : browseWeb(request);
}

/// A URL for one piece of the server's cover art, or null when there is none to
/// be had.
///
/// Asynchronous on both shells even though the desktop one answers immediately:
/// the web shell has to fetch the bytes before it has a URL at all (COEP blocks
/// a plain cross-origin `<img src>`), and a component should not have to know
/// which shell it is drawing in -- the same shape `coverUrl` takes in library.ts.
export async function serverCoverUrl(coverArt: string | null): Promise<string | null> {
  if (!coverArt) return null;
  return isTauri() ? coverNative(coverArt) : coverWeb(coverArt);
}
