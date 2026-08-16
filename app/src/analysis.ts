// Adapter for the metering side of the engine -- the analysis counterpart of
// playlist.ts, and the only module the meter components read through.
//
// Both shells compute their levels with the same Rust (`player-core`'s
// `Meters`), so what differs here is only how the numbers arrive: a Tauri
// command natively, a region of the SharedArrayBuffer on the web. Everything
// past this file sees one async function returning dBFS.
import { SPECTRUM_FLOOR_DB } from "./audioSab";
import { isTauri } from "./isTauri";
import {
  spectrum as spectrumNative,
  spectrumBands as spectrumBandsNative,
} from "./nativePlayer";
import { spectrum as spectrumWeb, spectrumBands as spectrumBandsWeb } from "./playerEngine";

/// What a band with nothing in it reads. Mirrors `FLOOR_DB` in
/// player-core/src/spectrum.rs.
export const FLOOR_DB = SPECTRUM_FLOOR_DB;

/// The latest spectrum, one dBFS level per band. Empty before a track is
/// loaded.
export async function spectrum(): Promise<Float32Array> {
  return isTauri() ? spectrumNative() : spectrumWeb();
}

/// Centre frequency of each band, for the axis. Fixed for the session, so a
/// caller can fetch it once per track rather than per frame.
export async function spectrumBands(): Promise<Float32Array> {
  return isTauri() ? spectrumBandsNative() : spectrumBandsWeb();
}
