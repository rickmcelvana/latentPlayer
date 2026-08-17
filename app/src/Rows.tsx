// One row, and one cover, for every list in this app (Phase 2 M3).
//
// Extracted from LibraryPanel when the server browser arrived, for the reason
// the milestone plan named: the rows are one implementation whatever they list.
// A local library row and a server row have different fields and identical
// shape on screen -- cover, name, something smaller underneath, a length, and a
// way to queue it -- and two components drifting apart would show the listener
// two kinds of list for no reason they could see.
//
// Neither of these computes anything. Which shell they are drawn in never
// appears here either; that is the adapters' business (library.ts, server.ts).
import { useEffect, useState } from "react";
import { formatDuration } from "./format";
import { coverUrl as libraryCoverUrl } from "./library";
import { serverCoverUrl } from "./server";

/// Where a cover comes from. The two stores are genuinely different -- a hash
/// into the local cache, an id on the server -- and this is the whole of the
/// difference as far as a component is concerned.
export type CoverSource = "library" | "server";

/// One row's cover thumbnail.
///
/// Resolves its own URL, because a URL is asynchronous on the web (a blob, out
/// of IndexedDB or off the server) and synchronous natively (a `cover://`
/// address), and a component should not have to know which.
export function Cover({ source, id }: { source: CoverSource; id: string | null }) {
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setUrl(null);
    const pending = source === "server" ? serverCoverUrl(id) : libraryCoverUrl(id);
    pending
      .then((next) => {
        if (live) setUrl(next);
      })
      .catch(() => {
        // A missing cover is a blank square, not an error message: the row is
        // still perfectly playable.
        if (live) setUrl(null);
      });
    return () => {
      live = false;
    };
  }, [source, id]);

  return url ? (
    <img className="cover" src={url} alt="" loading="lazy" />
  ) : (
    <span className="cover cover-empty" aria-hidden="true" />
  );
}

export interface RowProps {
  coverSource: CoverSource;
  coverId: string | null;
  /// The line that names the thing: a track's title, an album's name.
  name: string;
  /// The smaller line under it. Empty renders as nothing rather than as a gap.
  meta?: string;
  /// Shown on the right. Null when there is nothing to say -- see
  /// `formatDuration`.
  durationSeconds?: number | null;
  /// A count where a duration would go, for rows that are collections: "12
  /// tracks", "3 albums".
  count?: string;
  onOpen: () => void;
  /// The `+` button. Omitted for rows there is nothing to queue -- an artist,
  /// say, which is a place to go rather than something to play.
  onQueue?: () => void;
}

export function Row({
  coverSource,
  coverId,
  name,
  meta,
  durationSeconds,
  count,
  onOpen,
  onQueue,
}: RowProps) {
  return (
    <li>
      <button type="button" className="row" onClick={onOpen}>
        <Cover source={coverSource} id={coverId} />
        <span className="row-main">
          <span className="name">{name}</span>
          {meta && <span className="meta">{meta}</span>}
        </span>
        <span className="time">
          {count ?? (durationSeconds === undefined ? "" : formatDuration(durationSeconds))}
        </span>
      </button>
      {onQueue && (
        <button type="button" className="queue" aria-label={`Queue ${name}`} onClick={onQueue}>
          +
        </button>
      )}
    </li>
  );
}
