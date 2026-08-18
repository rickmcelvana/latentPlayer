// The playlist, and the one place the two playback adapters are chosen
// between.
//
// M3's decision was that the engine holds exactly two tracks -- what is
// playing and what is next -- and that ordering is the caller's business
// (tasks/phase-1.md, `Playback::set_next`). This module is that caller: it
// owns the list, and turns "play track 4" into the load/queue pair both
// shells expose.
//
// nativePlayer.ts and playerEngine.ts already expose the same `TransportState`
// so components don't branch on the shell; the remaining difference is that a
// desktop track is a path the Rust decoder opens and a web track is a `File`
// whose bytes go to the worker. That difference stops here rather than
// spreading through the UI.
import { isTauri } from "./isTauri";
import {
  clearNext as clearNextNative,
  loadFile as loadFileNative,
  loadServerTrack as loadServerNative,
  seek as seekNative,
  setNext as setNextNative,
  setNextServerTrack as setNextServerNative,
  setPlaying as setPlayingNative,
  setVolume as setVolumeNative,
  transport as transportNative,
  type TransportState,
} from "./nativePlayer";
import {
  clearNext as clearNextWeb,
  loadEncoded as loadEncodedWeb,
  loadFile as loadFileWeb,
  pause as pauseWeb,
  play as playWeb,
  seek as seekWeb,
  setNext as setNextWeb,
  setNextEncoded as setNextEncodedWeb,
  setVolume as setVolumeWeb,
  transport as transportWeb,
} from "./playerEngine";
import { fetchingTrack, trackBytes } from "./subsonicWeb";
import type { ServerTrack } from "./server";

export type { TransportState };

/// Where a playlist entry's audio comes from. Exactly one of `path`/`file`/
/// `server` is set -- the first two are this machine, whichever shell is
/// running, and the third is the music server (Phase 2 M2).
export interface PlaylistTrack {
  /// Stable across reordering and duplicates, which an array index isn't --
  /// the same file can legitimately appear twice in a playlist.
  id: number;
  name: string;
  /// Desktop: the filesystem path `player-core`'s `LocalFile` opens.
  path: string | null;
  /// Web: the picked file, whose bytes the worker decodes.
  file: File | null;
  /// The library row this came from, when it came from the library at all
  /// (M7). Null for a track added straight from the file picker, which has no
  /// library row to point at. A saved playlist stores these rather than the
  /// tracks themselves, so fixing a tag fixes it everywhere the track appears.
  libraryId: string | null;
  /// The server track this plays, when it is one. The id is the server's, and
  /// the suffix is the decoder's format hint -- a Subsonic stream URL carries no
  /// extension, so without it a bare AAC stream is ambiguous to symphonia's
  /// probe. Both shells need exactly this pair and nothing else, which is why
  /// there is no third adapter here: the difference is one source deep.
  server: { id: string; suffix: string } | null;
}

let nextId = 0;

export function trackFromPath(path: string): PlaylistTrack {
  return {
    id: nextId++,
    name: path.split(/[\\/]/).pop() ?? path,
    path,
    file: null,
    libraryId: null,
    server: null,
  };
}

export function trackFromFile(file: File): PlaylistTrack {
  return { id: nextId++, name: file.name, path: null, file, libraryId: null, server: null };
}

export function trackFromServer(track: ServerTrack): PlaylistTrack {
  const name = track.artist ? `${track.artist} - ${track.title}` : track.title || track.id;
  return {
    id: nextId++,
    name,
    path: null,
    file: null,
    libraryId: null,
    server: { id: track.id, suffix: track.suffix },
  };
}

/// Starts `track` immediately, replacing whatever was playing and dropping
/// anything queued behind it. This is the deliberate jump -- clicking a row,
/// or next/previous -- as opposed to the gapless hand-off, which happens in
/// the engine with no call from here at all.
export async function loadTrack(track: PlaylistTrack): Promise<TransportState> {
  if (track.server) {
    const { id, suffix } = track.server;
    // Desktop: the engine opens the network stream itself and reads it as it
    // decodes. Web: the bytes are fetched here first, because the worker
    // decodes from an array it owns and there is nothing for it to pull
    // through (see `trackBytes` for what that costs and what it buys).
    if (isTauri()) return loadServerNative(id, suffix);
    await loadEncodedWeb(await trackBytes(id), suffix || null);
  } else if (isTauri()) {
    return loadFileNative(requirePath(track));
  } else {
    await loadFileWeb(requireFile(track));
  }
  // The native commands start playback themselves; match that here so callers
  // get one behaviour rather than two.
  await playWeb();
  return transportWeb();
}

/// Hands the engine the track to play when the current one runs out, or
/// clears the slot when there is nothing after it.
///
/// For a server track this is also the prefetch, on both shells: the desktop
/// engine opens the stream when it runs out of the current track, and the web
/// shell fetches the whole thing now. Either way the work happens when the
/// track is queued -- a track ahead of the seam -- rather than at the seam,
/// which is what keeps a hand-off between two server tracks gapless.
export async function queueTrack(track: PlaylistTrack | null): Promise<void> {
  if (track?.server) {
    const { id, suffix } = track.server;
    if (isTauri()) return setNextServerNative(id, suffix);
    setNextEncodedWeb(await trackBytes(id), suffix || null);
    return;
  }
  if (isTauri()) {
    return track ? setNextNative(requirePath(track)) : clearNextNative();
  }
  if (track) return setNextWeb(requireFile(track));
  clearNextWeb();
}

export async function readTransport(): Promise<TransportState> {
  if (isTauri()) return transportNative();
  const state = transportWeb();
  // The web shell has a second way to be waiting for audio, and it is the one
  // a listener meets first: a server track is fetched *whole* before it plays
  // (the parity limit M2 named and stated rather than hid). No ring is
  // involved and the engine cannot see it, but it is buffering by any honest
  // reading -- so it is ORed in here, at the seam that already knows a server
  // track on this shell is a download.
  return state.buffering ? state : { ...state, buffering: fetchingTrack() };
}

export async function setPlaying(playing: boolean): Promise<TransportState> {
  if (isTauri()) {
    await setPlayingNative(playing);
    return transportNative();
  }
  if (playing) await playWeb();
  else pauseWeb();
  return transportWeb();
}

export async function seek(seconds: number): Promise<TransportState> {
  return isTauri() ? seekNative(seconds) : seekWeb(seconds);
}

export async function setVolume(volume: number): Promise<void> {
  if (isTauri()) return setVolumeNative(volume);
  setVolumeWeb(volume);
}

function requirePath(track: PlaylistTrack): string {
  if (track.path === null) throw new Error(`no path for "${track.name}" on the desktop shell`);
  return track.path;
}

function requireFile(track: PlaylistTrack): File {
  if (track.file === null) throw new Error(`no file for "${track.name}" on the web shell`);
  return track.file;
}
