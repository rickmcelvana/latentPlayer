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
import { Row } from "./Rows";
import {
  emptyBrowseResult,
  serverBrowse,
  type AlbumRow,
  type AlbumSort,
  type ArtistRow,
  type BrowseRequest,
  type BrowseResult,
  type ServerTrack,
} from "./server";
import { trackFromServer, type PlaylistTrack } from "./playlist";

interface Props {
  /// Replaces the queue with `tracks` and starts at `index` -- the same
  /// callback the local library gets, because "play this" means the same thing
  /// wherever the rows came from.
  onPlay: (tracks: PlaylistTrack[], index: number) => void;
  /// Appends to the queue without disturbing what is playing.
  onQueue: (tracks: PlaylistTrack[]) => void;
}

/// Where the browser is. `search` is not one of these: a query overrides
/// whatever place is current and going back to it means clearing the box.
type Place =
  | { kind: "albums" }
  | { kind: "artists" }
  | { kind: "artist"; id: string; name: string }
  | { kind: "album"; id: string; name: string };

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

export function ServerBrowser({ onPlay, onQueue }: Props) {
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

  useEffect(() => {
    const id = setTimeout(() => setAsked(query.trim()), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(id);
  }, [query]);

  /// What to ask for, given where we are. A search wins over the place, which
  /// is why the two are separate pieces of state rather than one.
  const request: BrowseRequest = asked
    ? { kind: "search", query: asked }
    : place.kind === "albums"
      ? { kind: "albums", sort, size: PAGE, offset }
      : place.kind === "artists"
        ? { kind: "artists" }
        : place.kind === "artist"
          ? { kind: "artist", id: place.id }
          : { kind: "album", id: place.id };

  // Serialised rather than passed as an object: this is an effect dependency,
  // and a fresh object every render would ask the server on every render.
  const requestKey = JSON.stringify(request);

  useEffect(() => {
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
  }, [requestKey]);

  /// Moves to a place, from the top. Every navigation clears the search box,
  /// since a search result is what you clicked *out of*.
  const go = useCallback((next: Place) => {
    setPlace(next);
    setQuery("");
    setAsked("");
    setOffset(0);
    setResult(emptyBrowseResult());
  }, []);

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

      {busy && <p className="library-status">Asking the server…</p>}
      {error && <p className="error">{error}</p>}

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
              />
            ))}
          </ol>
        </>
      )}

      {!busy &&
        !error &&
        result.artists.length === 0 &&
        result.albums.length === 0 &&
        result.tracks.length === 0 && (
          <p className="hint">
            {searching ? `Nothing on the server matches "${asked}".` : "The server returned nothing."}
          </p>
        )}
    </div>
  );
}
