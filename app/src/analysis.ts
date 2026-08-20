// Adapter for the metering side of the engine -- the analysis counterpart of
// playlist.ts, and the only module the meter components read through.
//
// Both shells compute their levels with the same Rust (`player-core`'s
// `Meters`), so what differs here is only how the numbers arrive: a Tauri
// command natively, a region of the SharedArrayBuffer on the web. Everything
// past this file sees one async function returning dBFS.
import { LOUDNESS_FLOOR_LUFS, SPECTRUM_FLOOR_DB, TRUE_PEAK_FLOOR_DB } from "./audioSab";
import { isTauri } from "./isTauri";
import {
  goniometer as goniometerNative,
  meters as metersNative,
  musical as musicalNative,
  spectrum as spectrumNative,
  spectrumBands as spectrumBandsNative,
} from "./nativePlayer";
import type { MeterSnapshot, MusicalSnapshot } from "./playerEngine";
import {
  goniometer as goniometerWeb,
  meters as metersWeb,
  musical as musicalWeb,
  silentMeters,
  silentMusical,
  spectrum as spectrumWeb,
  spectrumBands as spectrumBandsWeb,
} from "./playerEngine";

export type { MeterSnapshot, MusicalSnapshot };

/// What a band with nothing in it reads. Mirrors `FLOOR_DB` in
/// player-core/src/spectrum.rs.
export const FLOOR_DB = SPECTRUM_FLOOR_DB;

/// What the loudness meters read with nothing to measure. Mirrors `FLOOR_LUFS`
/// and `FLOOR_DBTP` in player-core/src/loudness.rs.
export const FLOOR_LUFS = LOUDNESS_FLOOR_LUFS;
export const FLOOR_DBTP = TRUE_PEAK_FLOOR_DB;

/// What a display shows before anything has been measured.
export const SILENT_METERS: MeterSnapshot = silentMeters();
export const SILENT_MUSICAL: MusicalSnapshot = silentMusical();

/// What a tempo that has not been established reads. Mirrors `NO_TEMPO` in
/// player-core/src/tempo.rs, and is the value a readout draws as "--" rather
/// than as a measurement.
export const NO_TEMPO = 0;

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

/// Loudness and phase, read as one snapshot so a display cannot show a
/// momentary from one instant against an integrated from another.
export async function meters(): Promise<MeterSnapshot> {
  return isTauri() ? metersNative() : metersWeb();
}

/// Tempo, beat phase, onsets and brightness, read as one snapshot for the same
/// reason the meters are: they describe one instant, and are published in the
/// same step (M5 decision 38).
export async function musical(): Promise<MusicalSnapshot> {
  return isTauri() ? musicalNative() : musicalWeb();
}

/// The goniometer's points: interleaved side/mid pairs, in the mix's own units
/// (full-scale mono reaches sqrt(2)). The display chooses the scale.
export async function goniometer(): Promise<Float32Array> {
  return isTauri() ? goniometerNative() : goniometerWeb();
}
