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
  seek as seekNative,
  setNext as setNextNative,
  setPlaying as setPlayingNative,
  setVolume as setVolumeNative,
  transport as transportNative,
  type TransportState,
} from "./nativePlayer";
import {
  clearNext as clearNextWeb,
  loadFile as loadFileWeb,
  pause as pauseWeb,
  play as playWeb,
  seek as seekWeb,
  setNext as setNextWeb,
  setVolume as setVolumeWeb,
  transport as transportWeb,
} from "./playerEngine";

export type { TransportState };

/// One entry in the playlist. Exactly one of `path`/`file` is set, whichever
/// the running shell can open.
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
}

let nextId = 0;

export function trackFromPath(path: string): PlaylistTrack {
  return {
    id: nextId++,
    name: path.split(/[\\/]/).pop() ?? path,
    path,
    file: null,
    libraryId: null,
  };
}

export function trackFromFile(file: File): PlaylistTrack {
  return { id: nextId++, name: file.name, path: null, file, libraryId: null };
}

/// Starts `track` immediately, replacing whatever was playing and dropping
/// anything queued behind it. This is the deliberate jump -- clicking a row,
/// or next/previous -- as opposed to the gapless hand-off, which happens in
/// the engine with no call from here at all.
export async function loadTrack(track: PlaylistTrack): Promise<TransportState> {
  if (isTauri()) return loadFileNative(requirePath(track));
  await loadFileWeb(requireFile(track));
  // The native command starts playback itself; match it here so callers get
  // one behaviour rather than two.
  await playWeb();
  return transportWeb();
}

/// Hands the engine the track to play when the current one runs out, or
/// clears the slot when there is nothing after it.
export async function queueTrack(track: PlaylistTrack | null): Promise<void> {
  if (isTauri()) {
    return track ? setNextNative(requirePath(track)) : clearNextNative();
  }
  if (track) return setNextWeb(requireFile(track));
  clearNextWeb();
}

export async function readTransport(): Promise<TransportState> {
  return isTauri() ? transportNative() : transportWeb();
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
