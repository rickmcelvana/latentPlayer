// The spectrum analyser display: M4's first consumer of the analysis tap.
//
// It draws numbers and does not compute them. Every level here is dBFS from
// `player-core`'s `SpectrumAnalyzer` -- windowing, normalisation, log banding
// and the decay all happen in Rust, on both shells (M4 decision 3), so the
// picture is the same in the desktop app and the browser by construction
// rather than by two implementations agreeing.
//
// Canvas 2D rather than the wgpu renderer: this is a bar chart, and
// player-render's pipeline exists for the 3D vectorscope (M6). Putting the
// spectrum through it would mean a second render surface in the desktop shell
// for no visual gain.
import { useEffect, useRef } from "react";
import { FLOOR_DB, spectrum, spectrumBands } from "./analysis";

/// Bottom of the drawn range. Well above the data floor: -90dBFS is already
/// below the noise floor of any real master, so drawing down to -120 would
/// spend a third of the height on nothing.
const DISPLAY_FLOOR_DB = -90;

/// Axis labels. Decade-ish, and the ones people actually name.
const LABELS = [50, 100, 500, 1_000, 5_000, 10_000];

interface Props {
  /// Only polls while there is something to measure -- an idle tab shouldn't
  /// hold a frame loop open over a silent engine.
  active: boolean;
  width?: number;
  height?: number;
}

function formatHz(hz: number): string {
  return hz >= 1000 ? `${hz / 1000}k` : `${hz}`;
}

export function Spectrum({ active, width = 480, height = 160 }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const bandsRef = useRef<Float32Array>(new Float32Array(0));

  useEffect(() => {
    if (!active) return;
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;

    let frame = 0;
    let stopped = false;
    // One request at a time. On the desktop shell every frame is an IPC round
    // trip, and queueing them up behind a slow one would build a backlog of
    // spectra that are already out of date by the time they arrive.
    let inFlight = false;

    spectrumBands()
      .then((bands) => {
        bandsRef.current = bands;
      })
      .catch(() => {
        // An axis we couldn't fetch is a missing label, not a broken display.
      });

    const draw = (levels: Float32Array) => {
      const bands = bandsRef.current;
      ctx.clearRect(0, 0, width, height);
      ctx.fillStyle = "#050508";
      ctx.fillRect(0, 0, width, height);

      // Frequency gridlines, positioned by where their label's band actually
      // sits -- the bands are log-spaced, so this is a log axis for free.
      if (bands.length === levels.length && levels.length > 0) {
        ctx.strokeStyle = "#1c1c26";
        ctx.fillStyle = "#55556a";
        ctx.font = "10px ui-monospace, monospace";
        ctx.lineWidth = 1;
        for (const hz of LABELS) {
          if (hz > bands[bands.length - 1]) continue;
          const index = nearestBand(bands, hz);
          const x = ((index + 0.5) / levels.length) * width;
          ctx.beginPath();
          ctx.moveTo(x, 0);
          ctx.lineTo(x, height - 12);
          ctx.stroke();
          ctx.fillText(formatHz(hz), x + 2, height - 2);
        }
      }

      const barWidth = width / Math.max(levels.length, 1);
      ctx.fillStyle = "#26f28c";
      for (let i = 0; i < levels.length; i++) {
        const db = Math.max(levels[i], DISPLAY_FLOOR_DB);
        const norm = (db - DISPLAY_FLOOR_DB) / (0 - DISPLAY_FLOOR_DB);
        const barHeight = Math.max(0, norm * (height - 14));
        // A band at the floor draws nothing at all rather than a row of stubs.
        if (levels[i] <= FLOOR_DB) continue;
        ctx.fillRect(i * barWidth + 0.5, height - 14 - barHeight, barWidth - 1, barHeight);
      }
    };

    const tick = () => {
      if (stopped) return;
      frame = requestAnimationFrame(tick);
      if (inFlight) return;
      inFlight = true;
      spectrum()
        .then((levels) => {
          inFlight = false;
          if (!stopped) draw(levels);
        })
        .catch(() => {
          inFlight = false;
        });
    };
    frame = requestAnimationFrame(tick);

    return () => {
      stopped = true;
      cancelAnimationFrame(frame);
    };
  }, [active, width, height]);

  return <canvas ref={canvasRef} width={width} height={height} className="spectrum" />;
}

/// Index of the band whose centre is closest to `hz`, on a log scale -- the
/// scale the display is drawn on, so "closest" has to be measured there too.
function nearestBand(bands: Float32Array, hz: number): number {
  let best = 0;
  let bestDistance = Infinity;
  const target = Math.log10(hz);
  for (let i = 0; i < bands.length; i++) {
    const distance = Math.abs(Math.log10(bands[i]) - target);
    if (distance < bestDistance) {
      bestDistance = distance;
      best = i;
    }
  }
  return best;
}
