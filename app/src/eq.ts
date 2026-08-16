// Adapter for the equaliser -- the fifth of the shell-choosing modules, beside
// playlist.ts (playback), analysis.ts (metering), scope.ts (the scope's
// geometry) and library.ts (the library).
//
// Both shells run the same filter design out of `player-core`'s `eq` module,
// and both apply it in their own audio path, so what differs here is only how
// a setting gets there: a Tauri command natively, a message to the worker on
// the web (which writes the coefficients into the shared buffer the worklet
// reads). Everything past this file sees one `EqState`.
//
// It also owns the one thing neither engine should: remembering the setting
// between sessions. An EQ that resets every time the app opens is an EQ nobody
// uses, and where to keep it is a UI decision -- ten numbers and a flag, which
// is exactly what localStorage is for (unlike the library, see M7 decision 4).
import { EQ_BANDS, EQ_MAX_GAIN_DB } from "./audioSab";
import { isTauri } from "./isTauri";
import { eqState as eqStateNative, setEq as setEqNative } from "./nativePlayer";
import { eqState as eqStateWeb, setEq as setEqWeb } from "./playerEngine";
import type { EqSettings, EqState } from "./playerEngine";

export type { EqSettings, EqState };

/// Sliders, mirroring `EQ_BANDS` in player-core/src/eq.rs.
export const BAND_COUNT = EQ_BANDS;

/// How far a slider travels either way. The engine clamps to its own copy of
/// this; the value here is only for laying a control out before the engine has
/// answered.
export const MAX_GAIN_DB = EQ_MAX_GAIN_DB;

const STORAGE_KEY = "latentplayer.eq";

/// Everything at zero, enabled.
export function flat(): EqSettings {
  return { enabled: true, preampDb: 0, gainsDb: new Array(BAND_COUNT).fill(0) };
}

/// The setting this browser or desktop profile last used, or flat.
///
/// Anything unreadable is treated as flat rather than as an error: a corrupt
/// preference should cost the listener their EQ, not their player.
export function stored(): EqSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return flat();
    const parsed = JSON.parse(raw) as Partial<EqSettings>;
    const gains = flat().gainsDb;
    if (Array.isArray(parsed.gainsDb)) {
      parsed.gainsDb.slice(0, BAND_COUNT).forEach((db, band) => {
        if (typeof db === "number" && Number.isFinite(db)) gains[band] = db;
      });
    }
    return {
      enabled: parsed.enabled !== false,
      preampDb:
        typeof parsed.preampDb === "number" && Number.isFinite(parsed.preampDb)
          ? parsed.preampDb
          : 0,
      gainsDb: gains,
    };
  } catch {
    return flat();
  }
}

function remember(settings: EqSettings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // A full or disabled store costs the setting's persistence and nothing
    // else; the EQ itself is already running.
  }
}

/// The current setting and the curve that goes with it.
export async function eqState(): Promise<EqState> {
  return isTauri() ? eqStateNative() : eqStateWeb();
}

/// Moves the sliders. Returns what the engine settled on -- the clamped
/// setting and its curve -- so the display shows what is being played rather
/// than what was asked for.
export async function setEq(settings: EqSettings): Promise<EqState> {
  const state = isTauri() ? await setEqNative(settings) : await setEqWeb(settings);
  remember({
    enabled: state.enabled,
    preampDb: state.preampDb,
    gainsDb: state.gainsDb,
  });
  return state;
}

/// Pushes the remembered setting into the engine and returns the result.
///
/// Called once, when the UI mounts. Neither shell needs it again per track:
/// the desktop shell keeps the setting outside the engine and hands it to each
/// new `Playback`, and the worker writes it into every shared buffer it
/// creates -- because a gapless hand-off that flattened the EQ would be
/// audible.
export async function applyStored(): Promise<EqState> {
  return setEq(stored());
}
