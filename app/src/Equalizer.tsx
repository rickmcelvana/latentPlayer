// The ten-band graphic EQ's controls and its response curve (M8).
//
// Like every other display in this app, it draws numbers it is given and
// computes none: the curve behind the sliders is `magnitude_response` in
// player-core/src/eq.rs, evaluated for the *output device's* rate, and it
// arrives with every answer the engine gives -- so the curve on screen is
// always the curve of the filters actually running, not a second opinion drawn
// from the slider positions.
import { useCallback, useEffect, useRef, useState } from "react";
import { MAX_GAIN_DB, applyStored, setEq, type EqSettings, type EqState } from "./eq";

/// The curve's vertical range. Wider than the sliders travel, so a boost at the
/// top of its range doesn't touch the top of the graph, and shelves that add up
/// past 12dB still have somewhere to go.
const CURVE_MAX_DB = 18;

const CURVE_WIDTH = 480;
const CURVE_HEIGHT = 120;

/// Gridlines, in dB.
const GRID_DB = [12, 6, 0, -6, -12];

function formatHz(hz: number): string {
  if (hz >= 1000) {
    const k = hz / 1000;
    return `${Number.isInteger(k) ? k : k.toFixed(1)}k`;
  }
  return `${Math.round(hz)}`;
}

interface Props {
  /// Reported so the shell can show a message rather than a dead panel if the
  /// engine never answered.
  onError?: (message: string) => void;
}

export function Equalizer({ onError }: Props) {
  const [state, setState] = useState<EqState | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  const fail = useCallback(
    (e: unknown) => {
      if (onError) onError(String(e));
    },
    [onError],
  );

  // The setting the listener last used, pushed into the engine on mount. Both
  // shells then keep it across track changes on their own (see eq.ts).
  useEffect(() => {
    applyStored().then(setState).catch(fail);
  }, [fail]);

  // The curve. Redrawn when the setting changes and never per frame -- it is a
  // function of the sliders, not of the audio.
  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx || !state) return;

    ctx.fillStyle = "#050508";
    ctx.fillRect(0, 0, CURVE_WIDTH, CURVE_HEIGHT);

    const y = (db: number) => ((CURVE_MAX_DB - db) / (2 * CURVE_MAX_DB)) * CURVE_HEIGHT;

    ctx.strokeStyle = "#1c1c26";
    ctx.fillStyle = "#55556a";
    ctx.font = "10px ui-monospace, monospace";
    ctx.lineWidth = 1;
    for (const db of GRID_DB) {
      ctx.beginPath();
      ctx.moveTo(0, y(db));
      ctx.lineTo(CURVE_WIDTH, y(db));
      ctx.stroke();
      if (db !== 0) ctx.fillText(`${db > 0 ? "+" : ""}${db}`, 2, y(db) - 2);
    }

    // The sliders' own frequencies, so a bell can be read against the control
    // that made it.
    for (const hz of state.bandFrequencies) {
      const x = position(hz) * CURVE_WIDTH;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, CURVE_HEIGHT);
      ctx.stroke();
    }

    const curve = state.responseDb;
    const frequencies = state.responseFrequencies;
    if (curve.length === 0 || curve.length !== frequencies.length) return;
    ctx.strokeStyle = state.enabled ? "#26f28c" : "#3a3a4a";
    ctx.lineWidth = 2;
    ctx.beginPath();
    for (let i = 0; i < curve.length; i++) {
      const x = position(frequencies[i]) * CURVE_WIDTH;
      const point = y(Math.max(-CURVE_MAX_DB, Math.min(CURVE_MAX_DB, curve[i])));
      if (i === 0) ctx.moveTo(x, point);
      else ctx.lineTo(x, point);
    }
    ctx.stroke();
  }, [state]);

  const change = useCallback(
    (next: EqSettings) => {
      // Shown immediately, then corrected by whatever the engine settled on --
      // a slider that waits for an IPC round trip before it moves feels broken,
      // and the engine's answer is what the display ends up holding.
      setState((shown) => (shown ? { ...shown, ...next } : shown));
      setEq(next).then(setState).catch(fail);
    },
    [fail],
  );

  if (!state) return <section className="eq">Equaliser starting&hellip;</section>;

  const settings: EqSettings = {
    enabled: state.enabled,
    preampDb: state.preampDb,
    gainsDb: state.gainsDb,
  };
  const max = state.maxGainDb || MAX_GAIN_DB;

  return (
    <section className={state.enabled ? "eq" : "eq off"}>
      <header>
        <h2>Equaliser</h2>
        <label className="eq-toggle">
          <input
            type="checkbox"
            checked={state.enabled}
            onChange={(e) => change({ ...settings, enabled: e.target.checked })}
          />
          On
        </label>
        <button
          type="button"
          onClick={() =>
            change({ enabled: state.enabled, preampDb: 0, gainsDb: state.gainsDb.map(() => 0) })
          }
        >
          Reset
        </button>
      </header>

      <canvas ref={canvasRef} width={CURVE_WIDTH} height={CURVE_HEIGHT} className="eq-curve" />

      <div className="eq-bands">
        <div className="eq-band preamp">
          <input
            type="range"
            aria-label="Preamp"
            min={-max}
            max={max}
            step={0.5}
            value={state.preampDb}
            onChange={(e) => change({ ...settings, preampDb: Number(e.target.value) })}
          />
          <span className="eq-gain">{formatDb(state.preampDb)}</span>
          <span className="eq-label">Pre</span>
        </div>

        {state.gainsDb.map((db, band) => (
          <div className="eq-band" key={band}>
            <input
              type="range"
              aria-label={`${formatHz(state.bandFrequencies[band] ?? 0)} Hz`}
              min={-max}
              max={max}
              step={0.5}
              value={db}
              onChange={(e) => {
                const gains = state.gainsDb.slice();
                gains[band] = Number(e.target.value);
                change({ ...settings, gainsDb: gains });
              }}
            />
            <span className="eq-gain">{formatDb(db)}</span>
            <span className="eq-label">{formatHz(state.bandFrequencies[band] ?? 0)}</span>
          </div>
        ))}
      </div>
    </section>
  );
}

function formatDb(db: number): string {
  if (db === 0) return "0";
  return `${db > 0 ? "+" : ""}${db.toFixed(db % 1 === 0 ? 0 : 1)}`;
}

/// Where a frequency sits across the curve, 0..1, on the log axis the curve's
/// own frequencies are spaced on (20Hz..20kHz).
function position(hz: number): number {
  const low = Math.log(20);
  const high = Math.log(20_000);
  return (Math.log(Math.max(hz, 20)) - low) / (high - low);
}
