// The loudness readout: momentary, short-term and integrated LUFS, true peak,
// and stereo correlation.
//
// Numbers, not a meter bridge, on purpose -- these are values you read rather
// than watch, and the one thing worth watching (the correlation) gets a bar.
// Every figure comes from `player-core`'s BS.1770-4 implementation on both
// shells (M5 decision 3); this file formats and colours them and computes
// nothing.
import { useEffect, useState } from "react";
import { FLOOR_DBTP, FLOOR_LUFS, SILENT_METERS, meters } from "./analysis";
import type { MeterSnapshot } from "./analysis";

/// How often the numbers are refreshed. Deliberately not frame rate: the
/// shortest window here is 400ms, so a 60Hz poll would cost an IPC round trip
/// per frame to redraw digits that had not changed -- and digits that flicker
/// faster than they can be read are worse, not better.
const POLL_MS = 100;

/// Above this a true peak is worth flagging: EBU R128 delivery specs put the
/// ceiling at -1 dBTP, and anything over it will clip something downstream.
const TRUE_PEAK_CEILING_DBTP = -1;

interface Props {
  /// Only polls while there is something to measure.
  active: boolean;
}

/// A LUFS value, or a dash when the meter is at its floor. Showing "-70.0"
/// for silence would read as a measurement rather than as the absence of one.
function formatLufs(value: number): string {
  return value <= FLOOR_LUFS ? "--" : value.toFixed(1);
}

function formatDbtp(value: number): string {
  return value <= FLOOR_DBTP ? "--" : value.toFixed(1);
}

export function Loudness({ active }: Props) {
  const [snapshot, setSnapshot] = useState<MeterSnapshot>(SILENT_METERS);

  useEffect(() => {
    if (!active) {
      setSnapshot(SILENT_METERS);
      return;
    }
    let stopped = false;
    // One request at a time: on the desktop shell each is an IPC round trip,
    // and a backlog of them would report older and older instants.
    let inFlight = false;

    const tick = () => {
      if (inFlight || stopped) return;
      inFlight = true;
      meters()
        .then((next) => {
          inFlight = false;
          if (!stopped) setSnapshot(next);
        })
        .catch(() => {
          inFlight = false;
        });
    };
    tick();
    const id = setInterval(tick, POLL_MS);

    return () => {
      stopped = true;
      clearInterval(id);
    };
  }, [active]);

  const clipping = snapshot.truePeakDb > TRUE_PEAK_CEILING_DBTP;
  // -1..+1 onto 0..100% of the bar's width, with the zero mark in the middle.
  const correlation = Math.max(-1, Math.min(1, snapshot.correlation));
  const width = `${Math.abs(correlation) * 50}%`;

  return (
    <div className="loudness">
      <dl className="readouts">
        <div>
          <dt>Momentary</dt>
          <dd>{formatLufs(snapshot.momentaryLufs)}</dd>
        </div>
        <div>
          <dt>Short</dt>
          <dd>{formatLufs(snapshot.shortTermLufs)}</dd>
        </div>
        <div>
          <dt>Integrated</dt>
          <dd>{formatLufs(snapshot.integratedLufs)}</dd>
        </div>
        <div>
          <dt>True peak</dt>
          <dd className={clipping ? "over" : undefined}>{formatDbtp(snapshot.truePeakDb)}</dd>
        </div>
      </dl>
      <div className="units">
        <span>LUFS</span>
        <span>dBTP</span>
      </div>

      <div className="correlation">
        <span className="label">Phase</span>
        <div className="track">
          {/* Grows out from the centre, left for negative: a bar pinned to the
              left edge would make "mono" and "out of phase" look alike. */}
          <div
            className={correlation < 0 ? "fill out-of-phase" : "fill"}
            style={{ width, left: correlation < 0 ? `${50 - Math.abs(correlation) * 50}%` : "50%" }}
          />
          <div className="centre" />
        </div>
        <span className="value">{correlation.toFixed(2)}</span>
      </div>
    </div>
  );
}
