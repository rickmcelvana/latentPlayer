// The library view (M7): folders, rows, search and saved playlists -- and,
// since Phase 2 M3, a switch between the music on this machine and the music on
// the server.
//
// Draws what library.ts hands it and computes nothing of its own -- the same
// rule Spectrum.tsx, Loudness.tsx and Goniometer.tsx follow. Which shell it is
// running in never appears here; that is the adapter's business.
//
// The two sources are genuinely different underneath (a scanned index here, a
// live server there, and browsing one is nothing like filtering the other), so
// the server's half is its own component. What they share is what the listener
// sees: the same rows, the same search box in the same place, and the same
// meaning for clicking one -- which is why Rows.tsx exists.
import { useCallback, useEffect, useMemo, useState } from "react";
import { Row } from "./Rows";
import { ServerBrowser } from "./ServerBrowser";
import {
  addFolder,
  deletePlaylist,
  displayName,
  folders as readFolders,
  forgetFolder,
  playlists as readPlaylists,
  savePlaylist,
  search,
  toPlaylistTrack,
  tracks as readTracks,
  type LibraryFolder,
  type LibraryPlaylist,
  type LibraryTrack,
} from "./library";
import type { PlaylistTrack } from "./playlist";

interface LibraryPanelProps {
  /// Replaces the queue with `tracks` and starts playing at `index`. Clicking
  /// a row plays the list you can see, from where you clicked -- so the rest
  /// of the search results follow gaplessly rather than the track playing
  /// alone.
  onPlay: (tracks: PlaylistTrack[], index: number) => void;
  /// Appends to the queue without disturbing what is playing.
  onQueue: (tracks: PlaylistTrack[]) => void;
  /// The current queue, so a playlist can be saved out of it.
  queue: PlaylistTrack[];
  /// Whether there is a server to browse. The panel offers the switch only when
  /// there is one, rather than showing a tab that can only produce an error.
  serverConnected: boolean;
}

export function LibraryPanel({
  onPlay,
  onQueue,
  queue,
  serverConnected,
}: LibraryPanelProps) {
  /// Which music is being looked at. Not persisted: it costs one click to
  /// change and the honest default is the music that is definitely there.
  const [source, setSource] = useState<"library" | "server">("library");
  const [rows, setRows] = useState<LibraryTrack[]>([]);
  const [folders, setFolders] = useState<LibraryFolder[]>([]);
  const [playlists, setPlaylists] = useState<LibraryPlaylist[]>([]);
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [playlistName, setPlaylistName] = useState("");

  const refresh = useCallback(async () => {
    const [nextRows, nextFolders, nextPlaylists] = await Promise.all([
      readTracks(),
      readFolders(),
      readPlaylists(),
    ]);
    setRows(nextRows);
    setFolders(nextFolders);
    setPlaylists(nextPlaylists);
  }, []);

  useEffect(() => {
    refresh().catch((e) => setError(String(e)));
  }, [refresh]);

  // Recomputed only when the rows or the query change -- a library of tens of
  // thousands of rows is filtered on every keystroke otherwise, including the
  // keystrokes that only moved the caret.
  const visible = useMemo(() => search(rows, query), [rows, query]);

  /// Turns library rows into playable tracks, dropping the ones that can no
  /// longer be opened.
  ///
  /// Tolerant on purpose. Clicking a row queues everything visible so the rest
  /// of the list follows gaplessly, and a row can fail to resolve for ordinary
  /// reasons -- a file moved or deleted since the scan, a folder handle whose
  /// permission lapsed. `Promise.all` would reject the whole list on the first
  /// of those, so one moved file would stop the library playing *anything*.
  /// Found live during M7 by clicking a row next to a stale one. Same rule the
  /// scanner already follows: one bad file is skipped and counted, never fatal.
  const resolve = useCallback(
    async (
      list: LibraryTrack[],
    ): Promise<{ tracks: PlaylistTrack[]; survived: boolean[] }> => {
      const settled = await Promise.allSettled(list.map(toPlaylistTrack));
      const tracks: PlaylistTrack[] = [];
      const survived = settled.map((result) => {
        if (result.status !== "fulfilled") return false;
        tracks.push(result.value);
        return true;
      });
      return { tracks, survived };
    },
    [],
  );

  async function handleAddFolder() {
    setError(null);
    setScanning(true);
    setStatus("Scanning…");
    try {
      const summary = await addFolder((done, total) =>
        setStatus(`Read ${done} of ${total}…`),
      );
      if (!summary) {
        setStatus(null);
        return;
      }
      await refresh();
      // "found" and "added" differing is the interesting case, so it is
      // reported rather than collapsed into a single count.
      setStatus(
        summary.failed > 0
          ? `Added ${summary.added} of ${summary.found} tracks (${summary.failed} unreadable)`
          : `Added ${summary.added} tracks`,
      );
    } catch (e) {
      setError(String(e));
      setStatus(null);
    } finally {
      setScanning(false);
    }
  }

  async function handlePlay(index: number) {
    setError(null);
    try {
      const { tracks, survived } = await resolve(visible);
      if (!survived[index]) {
        // The row that was actually clicked is the one failure worth
        // reporting: the listener asked for *that* track, so silently starting
        // a different one would be worse than saying so.
        setError(
          `"${displayName(visible[index] ?? visible[0]!)}" could not be opened`,
        );
        return;
      }
      // The clicked row's position in the surviving list, which is not its
      // position in the visible one once anything ahead of it dropped out.
      const start = survived.slice(0, index).filter(Boolean).length;
      onPlay(tracks, start);
      const lost = survived.filter((ok) => !ok).length;
      setStatus(
        lost > 0
          ? `${lost} track(s) could not be opened and were skipped`
          : null,
      );
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleQueue(row: LibraryTrack) {
    setError(null);
    try {
      const { tracks } = await resolve([row]);
      if (tracks.length === 0) {
        setError(`"${displayName(row)}" could not be opened`);
        return;
      }
      onQueue(tracks);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleForget(folder: LibraryFolder) {
    setError(null);
    try {
      await forgetFolder(folder.id);
      await refresh();
      setStatus(`Removed ${folder.name}`);
    } catch (e) {
      setError(String(e));
    }
  }

  /// Saves the current queue as a named playlist. Stored as library ids, so
  /// tracks added straight from the file picker -- which have no library row
  /// to point at -- are left out rather than saved as something that could not
  /// be loaded again.
  async function handleSavePlaylist() {
    const name = playlistName.trim();
    if (!name) return;
    const trackIds = queue
      .map((track) => track.libraryId)
      .filter((id): id is string => id !== null);
    if (trackIds.length === 0) {
      setError(
        "Nothing in the queue came from the library, so there is nothing to save",
      );
      return;
    }
    setError(null);
    try {
      await savePlaylist({ id: `${Date.now().toString(36)}`, name, trackIds });
      setPlaylistName("");
      await refresh();
      const skipped = queue.length - trackIds.length;
      setStatus(
        skipped > 0
          ? `Saved "${name}" (${trackIds.length} tracks, ${skipped} not from the library)`
          : `Saved "${name}"`,
      );
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleLoadPlaylist(playlist: LibraryPlaylist) {
    setError(null);
    try {
      // Resolved against the library as it is now, not as it was when saved.
      // A row that has since been removed simply drops out, which is why this
      // maps through the current rows rather than trusting the stored ids.
      const byId = new Map(rows.map((row) => [row.id, row]));
      const found = playlist.trackIds
        .map((id) => byId.get(id))
        .filter((row): row is LibraryTrack => row !== undefined);
      if (found.length === 0) {
        setError(
          `None of "${playlist.name}"'s tracks are in the library any more`,
        );
        return;
      }
      const { tracks } = await resolve(found);
      if (tracks.length === 0) {
        setError(`None of "${playlist.name}"'s tracks could be opened`);
        return;
      }
      onPlay(tracks, 0);
      const missing = playlist.trackIds.length - tracks.length;
      setStatus(
        missing > 0 ? `Loaded "${playlist.name}" (${missing} missing)` : null,
      );
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleDeletePlaylist(playlist: LibraryPlaylist) {
    setError(null);
    try {
      await deletePlaylist(playlist.id);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <section className="library">
      <header className="library-head">
        <h2>Library</h2>
        {serverConnected && (
          <div className="library-sources">
            <button
              type="button"
              className={source === "library" ? "tab current" : "tab"}
              onClick={() => setSource("library")}
            >
              This machine
            </button>
            <button
              type="button"
              className={source === "server" ? "tab current" : "tab"}
              onClick={() => setSource("server")}
            >
              Server
            </button>
          </div>
        )}
        {source === "library" && (
          <>
            <button
              type="button"
              onClick={() => void handleAddFolder()}
              disabled={scanning}
            >
              {scanning ? "Scanning…" : "Add folder…"}
            </button>
            <input
              className="library-search"
              type="search"
              placeholder="Search title, artist, album…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              aria-label="Search the library"
            />
            <span className="library-count">
              {query
                ? `${visible.length} of ${rows.length}`
                : `${rows.length} tracks`}
            </span>
          </>
        )}
      </header>

      {/* The server's own view. Its rows are the same rows, and clicking one
          means the same thing -- what differs is that this list is asked for
          rather than held (decision 6). */}
      {source === "server" && (
        <ServerBrowser onPlay={onPlay} onQueue={onQueue} queue={queue} />
      )}
      {source === "library" && (
        <>
          {folders.length > 0 && (
            <ul className="folders">
              {folders.map((folder) => (
                <li
                  key={folder.id}
                  className={folder.available ? undefined : "unavailable"}
                >
                  <span className="folder-name" title={folder.id}>
                    {folder.name}
                  </span>
                  {!folder.available && (
                    <span className="folder-note">
                      needs adding again to play
                    </span>
                  )}
                  <button
                    type="button"
                    className="remove"
                    aria-label={`Remove ${folder.name}`}
                    onClick={() => void handleForget(folder)}
                  >
                    &times;
                  </button>
                </li>
              ))}
            </ul>
          )}

          {status && <p className="library-status">{status}</p>}
          {error && <p className="error">{error}</p>}

          {rows.length === 0 && !scanning && (
            <p className="hint">
              Add a folder of music and its tags, album art and lengths get read
              into the library.
            </p>
          )}

          {visible.length > 0 && (
            <ol className="library-rows">
              {visible.map((row, index) => (
                <Row
                  key={row.id}
                  coverSource="library"
                  coverId={row.coverId}
                  name={displayName(row)}
                  meta={
                    [row.artist, row.album].filter(Boolean).join(" — ") ||
                    row.fileName
                  }
                  durationSeconds={row.durationSeconds}
                  onOpen={() => void handlePlay(index)}
                  onQueue={() => void handleQueue(row)}
                />
              ))}
            </ol>
          )}

          {/* Saved playlists are this machine's, stored as local library ids
              (M7). The server's own playlists are M4, and putting them here
              would be the place to do it. */}
          <div className="playlists">
            <div className="playlist-save">
              <input
                type="text"
                placeholder="Save queue as…"
                value={playlistName}
                onChange={(e) => setPlaylistName(e.target.value)}
                aria-label="Playlist name"
              />
              <button
                type="button"
                onClick={() => void handleSavePlaylist()}
                disabled={!playlistName.trim() || queue.length === 0}
              >
                Save
              </button>
            </div>

            {playlists.length > 0 && (
              <ul className="saved-playlists">
                {playlists.map((playlist) => (
                  <li key={playlist.id}>
                    <button
                      type="button"
                      className="row"
                      onClick={() => void handleLoadPlaylist(playlist)}
                    >
                      <span className="name">{playlist.name}</span>
                      <span className="meta">
                        {playlist.trackIds.length} tracks
                      </span>
                    </button>
                    <button
                      type="button"
                      className="remove"
                      aria-label={`Delete ${playlist.name}`}
                      onClick={() => void handleDeletePlaylist(playlist)}
                    >
                      &times;
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}
    </section>
  );
}
