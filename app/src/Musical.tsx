// The musical readout: tempo, how much to believe it, where the beat is, and
// how bright the moment is.
//
// The studio side of Phase 3 M5 (decision 40). Every figure comes from
// `player-core`'s onset detector and tempo tracker on both shells -- the same
// numbers the visualizer's feed carries, at the same instant -- so this file
// formats and colours them and computes nothing. A BPM readout is a feature for
// both audiences, and it is also the cheapest instrument available for the
// failure this milestone is most likely to have: a person who knows a track's
// tempo, watching the number, catches an octave error that no synthesized
// fixture would.
import { useEffect, useState } from "react";
import { NO_TEMPO, SILENT_MUSICAL, musical } from "./analysis";
import type { MusicalSnapshot } from "./analysis";

/// How often the numbers are refreshed. The loudness panel's cadence, for the
/// loudness panel's reason: these are digits you read rather than watch, and a
/// 60Hz poll would cost an IPC round trip per frame on the desktop shell to
/// redraw a tempo that changes once a track.
const POLL_MS = 100;

/// Below this the tempo is a guess rather than a reading, and the panel says so
/// instead of showing a number at full brightness. Not a threshold the analysis
/// uses -- `player-core` has already declined to report a tempo it believes
/// less than 0.15 -- this is the second, softer line a display draws.
const UNCERTAIN_BELOW = 0.5;

/// A tempo, or a dash when none is established. Showing "0.0" for "no tempo
/// yet" would read as a measurement rather than as the absence of one.
function formatBpm(bpm: number): string {
  return bpm <= NO_TEMPO ? "--" : bpm.toFixed(1);
}

/// Brightness in Hz below 1kHz and kHz above it, which is how the number is
/// read out loud.
function formatCentroid(hz: number): string {
  if (hz <= 0) return "--";
  return hz < 1000 ? `${Math.round(hz)} Hz` : `${(hz / 1000).toFixed(2)} kHz`;
}

interface Props {
  /// Only polls while there is something to measure.
  active: boolean;
}

export function Musical({ active }: Props) {
  const [snapshot, setSnapshot] = useState<MusicalSnapshot>(SILENT_MUSICAL);

  useEffect(() => {
    if (!active) {
      setSnapshot(SILENT_MUSICAL);
      return;
    }
    let stopped = false;
    // One request at a time, like the loudness panel: on the desktop shell each
    // is an IPC round trip, and a backlog would report older and older
    // instants.
    let inFlight = false;

    const tick = () => {
      if (inFlight || stopped) return;
      inFlight = true;
      musical()
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

  const established = snapshot.bpm > NO_TEMPO;
  const uncertain = established && snapshot.beatConfidence < UNCERTAIN_BELOW;
  // Brightest on the beat, fading through it. Sampled at the poll rate rather
  // than animated from the tempo: what is drawn is what was measured, which is
  // the same rule the visuals are held to.
  const beat = established ? 1 - Math.min(1, Math.max(0, snapshot.beatPhase)) : 0;

  return (
    <div className="musical">
      <div className="tempo">
        <span className="dot" style={{ opacity: 0.15 + 0.85 * beat }} />
        <span className={uncertain ? "bpm unsure" : "bpm"}>{formatBpm(snapshot.bpm)}</span>
        <span className="unit">BPM</span>
      </div>

      <div className="figures">
        <div className="confidence">
          <span className="label">Confidence</span>
          <div className="track">
            <div className="fill" style={{ width: `${snapshot.beatConfidence * 100}%` }} />
          </div>
          <span className="value">{(snapshot.beatConfidence * 100).toFixed(0)}%</span>
        </div>
        <div className="confidence">
          <span className="label">Transient</span>
          <div className="track">
            <div className="fill" style={{ width: `${snapshot.transient * 100}%` }} />
          </div>
          <span className="value">{snapshot.transient.toFixed(2)}</span>
        </div>
        <div className="brightness">
          <span className="label">Brightness</span>
          <span className="value">{formatCentroid(snapshot.centroidHz)}</span>
        </div>
      </div>
    </div>
  );
}
