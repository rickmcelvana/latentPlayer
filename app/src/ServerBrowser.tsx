// Browsing the music server's library (Phase 2 M3).
//
// The server's library is browsed *live* rather than mirrored (decision 6): a
// view asks for what it is about to show and nothing else. So there is no index
// here, no sync, and no answer to "is my copy stale" -- what is on screen is
// what the server said a moment ago.
//
// The shape is deliberately the shallowest thing that reaches a track: a place
// to be (all albums, all artists, one artist, one album), a search box that
// replaces it with results, and rows. Clicking a track plays the list you can
// see from where you clicked, exactly as the local library does, so the rest of
// the album follows gaplessly.
import { useCallback, useEffect, useState } from "react";
import { formatDuration } from "./format";
import { Row, type RowMarks } from "./Rows";
import {
  emptyBrowseResult,
  emptyPlaylistResult,
  serverBrowse,
  serverMark,
  serverPlaylists,
  type AlbumRow,
  type AlbumSort,
  type ArtistRow,
  type BrowseRequest,
  type BrowseResult,
  type MarkRequest,
  type PlaylistRequest,
  type PlaylistResult,
  type ServerTrack,
  type StarTarget,
} from "./server";
import { trackFromServer, type PlaylistTrack } from "./playlist";

interface Props {
  /// Replaces the queue with `tracks` and starts at `index` -- the same
  /// callback the local library gets, because "play this" means the same thing
  /// wherever the rows came from.
  onPlay: (tracks: PlaylistTrack[], index: number) => void;
  /// Appends to the queue without disturbing what is playing.
  onQueue: (tracks: PlaylistTrack[]) => void;
  /// The current queue, so a server playlist can be made out of it (M4). Only
  /// its server tracks can go: a playlist on the server is a list of the
  /// server's own ids, and a file on this machine has none.
  queue: PlaylistTrack[];
}

/// Where the browser is. `search` is not one of these: a query overrides
/// whatever place is current and going back to it means clearing the box.
type Place =
  | { kind: "albums" }
  | { kind: "artists" }
  | { kind: "artist"; id: string; name: string }
  | { kind: "album"; id: string; name: string }
  | { kind: "playlists" }
  | { kind: "playlist"; id: string; name: string };

/// Whether a place is one of the playlist views, which are asked for through
/// `serverPlaylists` rather than `serverBrowse` -- a different endpoint family
/// with a different answer, so a different effect below.
function isPlaylistPlace(place: Place): boolean {
  return place.kind === "playlists" || place.kind === "playlist";
}

/// Albums per page. The spec allows up to 500 and the server is being asked
/// live, so this trades a slightly longer wait for far fewer round trips than a
/// screenful at a time would cost.
const PAGE = 60;

/// How long the search box waits after the last keystroke before asking.
///
/// Local search filters an array already in memory and can run per keystroke;
/// this one is a request to someone's server, so "miles" typed at speed must be
/// one search rather than five.
const SEARCH_DEBOUNCE_MS = 300;

export function ServerBrowser({ onPlay, onQueue, queue }: Props) {
  const [place, setPlace] = useState<Place>({ kind: "albums" });
  const [sort, setSort] = useState<AlbumSort>("alphabeticalByName");
  const [query, setQuery] = useState("");
  /// The query actually asked about, which trails what has been typed by
  /// `SEARCH_DEBOUNCE_MS`.
  const [asked, setAsked] = useState("");
  const [result, setResult] = useState<BrowseResult>(emptyBrowseResult());
  const [offset, setOffset] = useState(0);
  /// Whether the last page came back full, i.e. whether there may be more.
  const [more, setMore] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /// The playlist views' own result (M4), kept apart from `result` because they
  /// come from a different family of endpoints and answer with a different
  /// shape.
  const [playlists, setPlaylists] = useState<PlaylistResult>(emptyPlaylistResult());
  const [newName, setNewName] = useState("");
  /// Bumped to ask the playlist views for their contents again, after a write
  /// that changed them.
  const [reload, setReload] = useState(0);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    const id = setTimeout(() => setAsked(query.trim()), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(id);
  }, [query]);

  /// What to ask for, given where we are. A search wins over the place, which
  /// is why the two are separate pieces of state rather than one.
  ///
  /// The two playlist places have no browse request of their own -- they are a
  /// different family of endpoints -- so they name the album list here and the
  /// effect below declines to send it. That keeps this a total function over
  /// `Place` rather than one with a hole in it.
  const request: BrowseRequest = asked
    ? { kind: "search", query: asked }
    : place.kind === "artists"
      ? { kind: "artists" }
      : place.kind === "artist"
        ? { kind: "artist", id: place.id }
        : place.kind === "album"
          ? { kind: "album", id: place.id }
          : { kind: "albums", sort, size: PAGE, offset };

  // Serialised rather than passed as an object: this is an effect dependency,
  // and a fresh object every render would ask the server on every render.
  const requestKey = JSON.stringify(request);
  const playlistPlace = isPlaylistPlace(place) && !asked;

  useEffect(() => {
    // The playlist views ask a different family of endpoints; their effect is
    // below. Without this the browser would issue a browse for a place that has
    // no browse request to make.
    if (playlistPlace) return;
    let live = true;
    setBusy(true);
    setError(null);
    serverBrowse(JSON.parse(requestKey) as BrowseRequest)
      .then((next) => {
        if (!live) return;
        setResult((previous) => {
          const paging = (JSON.parse(requestKey) as BrowseRequest).kind === "albums";
          // A page after the first extends the list rather than replacing it,
          // which is what makes "Load more" feel like more of the same list.
          const appending = paging && offset > 0;
          return appending
            ? { ...next, albums: [...previous.albums, ...next.albums] }
            : next;
        });
        setMore(next.albums.length >= PAGE);
      })
      .catch((e) => {
        if (live) setError(String(e));
      })
      .finally(() => {
        if (live) setBusy(false);
      });
    return () => {
      live = false;
    };
    // `offset` is deliberately not a dependency of its own: it is already part
    // of `requestKey` for the album list, and reading it here only decides
    // whether to append.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requestKey, playlistPlace]);

  // The playlists, and one playlist's tracks (M4). Re-run by `reload` after any
  // write, because a playlist that was just renamed or added to is exactly the
  // thing on screen.
  useEffect(() => {
    if (!playlistPlace) return;
    let live = true;
    setBusy(true);
    setError(null);
    const request: PlaylistRequest =
      place.kind === "playlist" ? { kind: "get", id: place.id } : { kind: "list" };
    serverPlaylists(request)
      .then((next) => {
        if (live) setPlaylists(next);
      })
      .catch((e) => {
        if (live) setError(String(e));
      })
      .finally(() => {
        if (live) setBusy(false);
      });
    return () => {
      live = false;
    };
  }, [playlistPlace, place, reload]);

  /// Moves to a place, from the top. Every navigation clears the search box,
  /// since a search result is what you clicked *out of*.
  const go = useCallback((next: Place) => {
    setPlace(next);
    setQuery("");
    setAsked("");
    setOffset(0);
    setResult(emptyBrowseResult());
    setPlaylists(emptyPlaylistResult());
    setNote(null);
  }, []);

  /// Stars, unstars or rates something, moving the rows first.
  ///
  /// Optimistic on purpose: a star that waits for a round trip before drawing
  /// feels broken on a server across a network. `revert` puts it back if the
  /// server refuses, so a failure is never left looking like a success -- which
  /// matters more here than anywhere else in the app, since this is the one
  /// place the app writes to someone else's library.
  const send = useCallback(async (request: MarkRequest, apply: () => void, revert: () => void) => {
    apply();
    setError(null);
    try {
      await serverMark(request);
    } catch (e) {
      revert();
      setError(String(e));
    }
  }, []);

  /// The star and rating for one track row, wherever it is being listed.
  function trackMarks(track: ServerTrack, update: (next: ServerTrack) => void): RowMarks {
    const put = (changed: Partial<ServerTrack>) => update({ ...track, ...changed });
    return {
      starred: track.starred,
      onStar: (starred) =>
        void send(
          { kind: starred ? "star" : "unstar", target: "song", id: track.id },
          () => put({ starred }),
          () => put({ starred: !starred }),
        ),
      rating: track.rating,
      onRate: (rating) =>
        void send(
          { kind: "setRating", id: track.id, rating },
          () => put({ rating: rating === 0 ? null : rating }),
          () => put({ rating: track.rating }),
        ),
    };
  }

  /// The same, for a row that is not a track. `target` is what keeps the id in
  /// the right parameter -- see `player_core::subsonic::stars`.
  function starMarks(
    target: StarTarget,
    id: string,
    starred: boolean,
    update: (starred: boolean) => void,
  ): RowMarks {
    return {
      starred,
      onStar: (next) =>
        void send(
          { kind: next ? "star" : "unstar", target, id },
          () => update(next),
          () => update(!next),
        ),
    };
  }

  /// Replaces one track in whichever list is holding it, so a star drawn
  /// optimistically survives until the next request.
  function replaceTrack(where: "browse" | "playlist", next: ServerTrack) {
    const swap = (tracks: ServerTrack[]) =>
      tracks.map((track) => (track.id === next.id ? next : track));
    if (where === "browse") setResult((previous) => ({ ...previous, tracks: swap(previous.tracks) }));
    else setPlaylists((previous) => ({ ...previous, tracks: swap(previous.tracks) }));
  }

  /// The queue's server tracks, which are the only ones a server playlist can
  /// hold.
  const queueServerIds = queue.map((track) => track.server?.id).filter((id): id is string => !!id);

  /// One playlist write, followed by asking for the result again rather than
  /// guessing at it: the server owns the order and the count, and a playlist
  /// this app has just changed is exactly the one worth being right about.
  async function writePlaylist(request: PlaylistRequest, said: string) {
    setError(null);
    setNote(null);
    setBusy(true);
    try {
      await serverPlaylists(request);
      setNote(said);
      setReload((n) => n + 1);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  function playTracks(tracks: ServerTrack[], index: number) {
    if (tracks.length === 0) return;
    onPlay(tracks.map(trackFromServer), index);
  }

  /// Plays a whole album, which needs its tracks first -- an album row carries
  /// a song count, not songs.
  async function playAlbum(album: AlbumRow, queue: boolean) {
    setError(null);
    setBusy(true);
    try {
      const contents = await serverBrowse({ kind: "album", id: album.id });
      if (contents.tracks.length === 0) {
        setError(`"${album.name}" has no tracks on the server`);
        return;
      }
      const tracks = contents.tracks.map(trackFromServer);
      if (queue) onQueue(tracks);
      else onPlay(tracks, 0);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const searching = asked.length > 0;
  const showAlbums = result.albums.length > 0 && !(place.kind === "album" && !searching);
  const backTo: Place | null =
    place.kind === "artist" ? { kind: "artists" } : place.kind === "album" ? { kind: "albums" } : null;

  return (
    <div className="server-browser">
      <div className="browser-head">
        <div className="browser-tabs">
          <button
            type="button"
            className={place.kind === "albums" && !searching ? "tab current" : "tab"}
            onClick={() => go({ kind: "albums" })}
          >
            Albums
          </button>
          <button
            type="button"
            className={place.kind === "artists" && !searching ? "tab current" : "tab"}
            onClick={() => go({ kind: "artists" })}
          >
            Artists
          </button>
          <button
            type="button"
            className={isPlaylistPlace(place) && !searching ? "tab current" : "tab"}
            onClick={() => go({ kind: "playlists" })}
          >
            Playlists
          </button>
          {place.kind === "albums" && !searching && (
            <select
              value={sort}
              onChange={(e) => {
                setOffset(0);
                setResult(emptyBrowseResult());
                setSort(e.target.value as AlbumSort);
              }}
              aria-label="Order albums by"
            >
              <option value="alphabeticalByName">By name</option>
              <option value="alphabeticalByArtist">By artist</option>
              <option value="newest">Newest</option>
              <option value="recent">Recently played</option>
              <option value="frequent">Most played</option>
              <option value="random">Random</option>
              <option value="starred">Starred</option>
            </select>
          )}
        </div>
        <input
          className="library-search"
          type="search"
          placeholder="Search the server…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="Search the server"
        />
      </div>

      {(place.kind === "artist" || place.kind === "album") && !searching && (
        <p className="browser-crumb">
          {backTo && (
            <button type="button" className="link" onClick={() => go(backTo)}>
              &larr; Back
            </button>
          )}
          <span className="crumb-name">{place.name}</span>
          {place.kind === "album" && result.albums[0] && (
            <span className="crumb-meta">
              {[
                result.albums[0].artist,
                result.albums[0].year ? String(result.albums[0].year) : "",
                formatDuration(result.albums[0].durationSeconds),
              ]
                .filter(Boolean)
                .join(" — ")}
            </span>
          )}
          {place.kind === "album" && (
            <button
              type="button"
              className="link"
              onClick={() => playTracks(result.tracks, 0)}
              disabled={result.tracks.length === 0}
            >
              Play album
            </button>
          )}
        </p>
      )}

      {place.kind === "playlist" && !searching && (
        <p className="browser-crumb">
          <button type="button" className="link" onClick={() => go({ kind: "playlists" })}>
            &larr; Back
          </button>
          <span className="crumb-name">{place.name}</span>
          {playlists.playlists[0] && (
            <span className="crumb-meta">
              {`${playlists.playlists[0].songCount} tracks`}
              {playlists.playlists[0].owner ? ` — ${playlists.playlists[0].owner}` : ""}
              {` — ${formatDuration(playlists.playlists[0].durationSeconds)}`}
            </span>
          )}
          <button
            type="button"
            className="link"
            onClick={() => playTracks(playlists.tracks, 0)}
            disabled={playlists.tracks.length === 0}
          >
            Play
          </button>
          <button
            type="button"
            className="link"
            disabled={busy || queueServerIds.length === 0}
            onClick={() =>
              void writePlaylist(
                { kind: "addSongs", id: place.id, songIds: queueServerIds },
                `Added ${queueServerIds.length} from the queue`,
              )
            }
          >
            Add queue
          </button>
          <button
            type="button"
            className="link"
            disabled={busy}
            onClick={() => {
              const name = newName.trim();
              if (!name) {
                setError("Type a new name first");
                return;
              }
              setNewName("");
              setPlace({ ...place, name });
              void writePlaylist({ kind: "rename", id: place.id, name }, `Renamed to "${name}"`);
            }}
          >
            Rename to
          </button>
          <input
            className="library-search"
            type="text"
            placeholder="new name…"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            aria-label="New name for this playlist"
          />
        </p>
      )}

      {place.kind === "playlists" && !searching && (
        <p className="browser-crumb">
          <span className="crumb-meta">
            {queueServerIds.length > 0
              ? `${queueServerIds.length} server tracks in the queue`
              : "Queue up some server tracks to save them here"}
          </span>
          <input
            className="library-search"
            type="text"
            placeholder="New playlist name…"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            aria-label="Name for a new server playlist"
          />
          <button
            type="button"
            className="link"
            disabled={busy || queueServerIds.length === 0 || newName.trim().length === 0}
            onClick={() => {
              const name = newName.trim();
              setNewName("");
              void writePlaylist(
                { kind: "create", name, songIds: queueServerIds },
                `Saved "${name}" to the server`,
              );
            }}
          >
            Save queue
          </button>
        </p>
      )}

      {busy && <p className="library-status">Asking the server…</p>}
      {note && <p className="library-status">{note}</p>}
      {error && <p className="error">{error}</p>}

      {place.kind === "playlists" && !searching && playlists.playlists.length > 0 && (
        <ol className="library-rows">
          {playlists.playlists.map((playlist) => (
            <Row
              key={playlist.id}
              coverSource="server"
              coverId={playlist.coverArt}
              name={playlist.name}
              meta={[playlist.owner, playlist.comment].filter(Boolean).join(" — ")}
              count={`${playlist.songCount} tracks`}
              onOpen={() => go({ kind: "playlist", id: playlist.id, name: playlist.name })}
              onRemove={() =>
                void writePlaylist(
                  { kind: "delete", id: playlist.id },
                  `Deleted "${playlist.name}"`,
                )
              }
            />
          ))}
        </ol>
      )}

      {place.kind === "playlist" && !searching && (
        <ol className="library-rows">
          {playlists.tracks.map((track, index) => (
            <Row
              key={`${track.id}-${index}`}
              coverSource="server"
              coverId={track.coverArt}
              name={track.title}
              meta={[track.artist, track.album].filter(Boolean).join(" — ")}
              durationSeconds={track.durationSeconds}
              onOpen={() => playTracks(playlists.tracks, index)}
              onQueue={() => onQueue([trackFromServer(track)])}
              marks={trackMarks(track, (next) => replaceTrack("playlist", next))}
              onRemove={() =>
                void writePlaylist(
                  // By position, which is what the endpoint takes -- and why a
                  // removal asks for the playlist again rather than splicing
                  // its own copy: every later index means a different track
                  // once the server has done it.
                  { kind: "removeIndices", id: place.id, indices: [index] },
                  `Removed "${track.title}"`,
                )
              }
            />
          ))}
        </ol>
      )}

      {result.artists.length > 0 && (
        <>
          {searching && <p className="browser-section">Artists</p>}
          <ol className="library-rows">
            {result.artists.map((artist: ArtistRow) => (
              <Row
                key={artist.id}
                coverSource="server"
                coverId={artist.coverArt}
                name={artist.name}
                count={artist.albumCount > 0 ? `${artist.albumCount} albums` : ""}
                onOpen={() => go({ kind: "artist", id: artist.id, name: artist.name })}
                marks={starMarks("artist", artist.id, artist.starred, (starred) =>
                  setResult((previous) => ({
                    ...previous,
                    artists: previous.artists.map((row) =>
                      row.id === artist.id ? { ...row, starred } : row,
                    ),
                  })),
                )}
              />
            ))}
          </ol>
        </>
      )}

      {showAlbums && (
        <>
          {searching && <p className="browser-section">Albums</p>}
          <ol className="library-rows">
            {result.albums.map((album) => (
              <Row
                key={album.id}
                coverSource="server"
                coverId={album.coverArt}
                name={album.name}
                meta={[album.artist, album.year ? String(album.year) : ""]
                  .filter(Boolean)
                  .join(" — ")}
                count={`${album.songCount} tracks`}
                onOpen={() => go({ kind: "album", id: album.id, name: album.name })}
                onQueue={() => void playAlbum(album, true)}
                marks={starMarks("album", album.id, album.starred, (starred) =>
                  setResult((previous) => ({
                    ...previous,
                    albums: previous.albums.map((row) =>
                      row.id === album.id ? { ...row, starred } : row,
                    ),
                  })),
                )}
              />
            ))}
          </ol>
          {place.kind === "albums" && !searching && more && (
            <button
              type="button"
              className="browser-more"
              disabled={busy}
              onClick={() => setOffset(offset + PAGE)}
            >
              Load more
            </button>
          )}
        </>
      )}

      {result.tracks.length > 0 && (
        <>
          {searching && <p className="browser-section">Tracks</p>}
          <ol className="library-rows">
            {result.tracks.map((track, index) => (
              <Row
                key={track.id}
                coverSource="server"
                coverId={track.coverArt}
                name={track.title}
                meta={[track.artist, track.album].filter(Boolean).join(" — ")}
                durationSeconds={track.durationSeconds}
                onOpen={() => playTracks(result.tracks, index)}
                onQueue={() => onQueue([trackFromServer(track)])}
                marks={trackMarks(track, (next) => replaceTrack("browse", next))}
              />
            ))}
          </ol>
        </>
      )}

      {!busy &&
        !error &&
        result.artists.length === 0 &&
        result.albums.length === 0 &&
        result.tracks.length === 0 &&
        playlists.playlists.length === 0 &&
        playlists.tracks.length === 0 && (
          <p className="hint">
            {searching
              ? `Nothing on the server matches "${asked}".`
              : place.kind === "playlists"
                ? "No playlists on the server yet."
                : place.kind === "playlist"
                  ? "This playlist is empty."
                  : "The server returned nothing."}
          </p>
        )}
    </div>
  );
}
