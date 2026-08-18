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

/// The listener's own marks on a row (M4): the star, and optionally a rating.
///
/// Optional on a row rather than always present, because only the server's rows
/// have them -- a local library row is a file on this machine and there is
/// nothing to tell anyone about it.
export interface RowMarks {
  starred: boolean;
  /// Called with what the star should become. The caller is expected to move
  /// its own state first and put it back if the server refuses: a star that
  /// waits for a network round trip before drawing feels broken, and a star that
  /// stays on after a failure is a lie.
  onStar: (starred: boolean) => void;
  /// 1-5, or null for unrated. Omitted entirely for rows that do not offer one.
  rating?: number | null;
  /// Called with 1-5, or 0 to clear -- which is what clicking the current
  /// rating does.
  onRate?: (rating: number) => void;
}

/// A star, filled or not.
export function Star({ starred, onStar, label }: { starred: boolean; onStar: (starred: boolean) => void; label: string }) {
  return (
    <button
      type="button"
      className={starred ? "star starred" : "star"}
      aria-pressed={starred}
      aria-label={starred ? `Unstar ${label}` : `Star ${label}`}
      title={starred ? "Starred" : "Star"}
      onClick={() => onStar(!starred)}
    >
      {starred ? "★" : "☆"}
    </button>
  );
}

/// Five stars, one of which may be the current rating.
///
/// Clicking the rating a thing already has clears it, which is what the
/// endpoint's own 0 means -- otherwise a mis-click is permanent.
export function Rating({
  rating,
  onRate,
  label,
}: {
  rating: number | null;
  onRate: (rating: number) => void;
  label: string;
}) {
  const current = rating ?? 0;
  return (
    <span className="rating" role="group" aria-label={`Rate ${label}`}>
      {[1, 2, 3, 4, 5].map((value) => (
        <button
          key={value}
          type="button"
          className={value <= current ? "rate on" : "rate"}
          aria-pressed={value <= current}
          aria-label={`${value} of 5`}
          onClick={() => onRate(value === current ? 0 : value)}
        >
          {value <= current ? "★" : "☆"}
        </button>
      ))}
    </span>
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
  /// The listener's star and rating (M4). Omitted for local rows, which have
  /// nowhere to keep either.
  marks?: RowMarks;
  /// A `-` button, for rows that can be taken out of the list they are in --
  /// a track inside a server playlist. Omitted everywhere else.
  onRemove?: () => void;
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
  marks,
  onRemove,
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
      {/* The marks sit beside the row's button, never inside it: a button
          nested in a button is invalid HTML, and browsers close the outer one
          early rather than nesting -- which would tear the row in half. It is
          the same reason the queue button below has always been a sibling. */}
      {marks?.onRate && (
        <Rating rating={marks.rating ?? null} onRate={marks.onRate} label={name} />
      )}
      {marks && <Star starred={marks.starred} onStar={marks.onStar} label={name} />}
      {onQueue && (
        <button type="button" className="queue" aria-label={`Queue ${name}`} onClick={onQueue}>
          +
        </button>
      )}
      {onRemove && (
        <button
          type="button"
          className="queue"
          aria-label={`Remove ${name} from this playlist`}
          onClick={onRemove}
        >
          &minus;
        </button>
      )}
    </li>
  );
}
