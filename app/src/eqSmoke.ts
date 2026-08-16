// Live check of the web build's ten-band EQ (Phase 1 M8) -- the web
// counterpart of `cargo run -p player-core --example eq_smoke`, running the
// same measurements against the same claims so the two shells can be held to
// one standard. Served at /eq.html by the dev server; not part of the
// production bundle.
//
// The measuring instrument is the spectrum analyser, which is deliberate twice
// over: every check has an arithmetic answer (a +12dB slider must move its own
// band by 12dB and its neighbours by none), and being able to measure the EQ
// with the meters at all is only true because the EQ runs before the analysis
// tap (M8 decision 2).
//
// One check here has no native counterpart, and is the reason this harness
// matters more than the others: the difference equation in eqFilter.ts is the
// one piece of DSP in this player written twice, so it is held to the Rust
// `EqProcessor` sample for sample on the same input.

import init, { eqCoefficients, eqProcess } from "./wasm/player_rt_wasm.js";
import { EqFilter } from "./eqFilter";
import { eqState, setEq } from "./eq";
import {
  loadFile,
  meters,
  outputSampleRate,
  pause,
  play,
  seek,
  setVolume,
  spectrum,
  spectrumBands,
  transport,
} from "./playerEngine";

const SRC_RATE = 44_100;
const TONE_SECONDS = 30;
/// The two tones the test file carries: one in the 1kHz band, one in the 8kHz
/// band. Five octaves apart, so a boost of either has nowhere to hide.
const LOW_HZ = 1000;
const HIGH_HZ = 8000;
/// -12dBFS each, so a +12dB boost still has somewhere to go before it clips.
const TONE_AMPLITUDE = 0.25;

/// Which slider each tone belongs to.
const LOW_BAND = 5;
const HIGH_BAND = 8;

/// How long to let the display catch up after a change. A rise is immediate,
/// but a fall decays at 30dB/s, so a 12dB cut takes 0.4s to arrive.
const SETTLE = 900;

const out = document.getElementById("out") as HTMLPreElement;
let failures = 0;

function log(line: string): void {
  out.textContent += `${line}\n`;
}

function check(label: string, ok: boolean, detail: string): void {
  if (!ok) failures += 1;
  log(`  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}`);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const flat = () => ({ enabled: true, preampDb: 0, gainsDb: new Array(10).fill(0) as number[] });

function withBand(band: number, gainDb: number) {
  const settings = flat();
  settings.gainsDb[band] = gainDb;
  return settings;
}

/// A 16-bit stereo WAV of two summed sines. Same generator as the native
/// example's, so both shells filter the identical signal.
function makeWav(): File {
  const frames = SRC_RATE * TONE_SECONDS;
  const blockAlign = 4;
  const dataLen = frames * blockAlign;
  const buffer = new ArrayBuffer(44 + dataLen);
  const view = new DataView(buffer);
  const ascii = (offset: number, text: string) => {
    for (let i = 0; i < text.length; i++) view.setUint8(offset + i, text.charCodeAt(i));
  };

  ascii(0, "RIFF");
  view.setUint32(4, 36 + dataLen, true);
  ascii(8, "WAVEfmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 2, true);
  view.setUint32(24, SRC_RATE, true);
  view.setUint32(28, SRC_RATE * blockAlign, true);
  view.setUint16(32, blockAlign, true);
  view.setUint16(34, 16, true);
  ascii(36, "data");
  view.setUint32(40, dataLen, true);

  for (let i = 0; i < frames; i++) {
    const t = i / SRC_RATE;
    const low = Math.sin(2 * Math.PI * LOW_HZ * t);
    const high = Math.sin(2 * Math.PI * HIGH_HZ * t);
    const s = (low + high) * TONE_AMPLITUDE * 32_767;
    view.setInt16(44 + i * 4, s, true);
    view.setInt16(44 + i * 4 + 2, s, true);
  }
  return new File([buffer], "eq-tones.wav", { type: "audio/wav" });
}

/// Index of the display band whose centre is nearest `hz`, on a log scale.
function bandAt(bands: Float32Array, hz: number): number {
  let best = 0;
  let bestDistance = Infinity;
  for (let i = 0; i < bands.length; i++) {
    const d = Math.abs(Math.log10(bands[i]) - Math.log10(hz));
    if (d < bestDistance) {
      bestDistance = d;
      best = i;
    }
  }
  return best;
}

/// What the drawn curve claims at `hz`.
function curveAt(state: { responseDb: Float32Array; responseFrequencies: Float32Array }, hz: number): number {
  let best = 0;
  let bestDistance = Infinity;
  for (let i = 0; i < state.responseFrequencies.length; i++) {
    const d = Math.abs(state.responseFrequencies[i] - hz);
    if (d < bestDistance) {
      bestDistance = d;
      best = i;
    }
  }
  return state.responseDb[best] ?? 0;
}

/// The check with no native counterpart: eqFilter.ts against `EqProcessor`.
///
/// Runs both over the same interleaved stereo signal, with a cascade that has
/// every kind of band in it -- shelves at the ends, peaks in the middle, a
/// preamp -- and reports the largest disagreement in dB. Anything above the
/// last bits of f32 rounding means the two implementations have drifted.
async function filtersAgree(rate: number): Promise<void> {
  await init();
  const settings = {
    enabled: true,
    preampDb: -3,
    gainsDb: [6, -3, 0, 9, -12, 4, 0, -6, 3, 12],
  };
  const gains = new Float32Array(settings.gainsDb);

  const frames = 4096;
  const input = new Float32Array(frames * 2);
  for (let i = 0; i < frames; i++) {
    // Two unrelated waveforms, so a channel swap or a shared filter state shows
    // up rather than cancelling out.
    input[i * 2] = Math.sin(i * 0.03) * 0.4;
    input[i * 2 + 1] = Math.cos(i * 0.07) * 0.4;
  }

  const reference = eqProcess(input, 2, settings.enabled, settings.preampDb, gains, rate);

  const filter = new EqFilter(2);
  filter.set(eqCoefficients(settings.enabled, settings.preampDb, gains, rate));
  const mine = new Float32Array(input);
  const frame = new Float64Array(2);
  for (let i = 0; i < frames; i++) {
    frame[0] = mine[i * 2];
    frame[1] = mine[i * 2 + 1];
    filter.process(frame);
    mine[i * 2] = frame[0];
    mine[i * 2 + 1] = frame[1];
  }

  let worst = 0;
  let peak = 0;
  for (let i = 0; i < reference.length; i++) {
    worst = Math.max(worst, Math.abs(reference[i] - mine[i]));
    peak = Math.max(peak, Math.abs(reference[i]));
  }
  check(
    "the worklet's filter and the Rust one produce the same audio",
    worst < 1e-6 && peak > 0.1,
    `worst sample differs by ${worst.toExponential(2)} on a signal peaking at ${peak.toFixed(3)}`,
  );
}

async function run(): Promise<void> {
  out.textContent = "";
  failures = 0;

  if (!crossOriginIsolated) {
    log("FATAL: not crossOriginIsolated, so there is no SharedArrayBuffer.");
    return;
  }

  // Start from a known state: this page runs against the same localStorage the
  // app uses, and a previous session's EQ would offset every measurement here.
  await setEq(flat());

  await loadFile(makeWav());
  setVolume(0.1); // be kind to whoever is at the keyboard
  const rate = outputSampleRate();
  const bands = await spectrumBands();
  const low = bandAt(bands, LOW_HZ);
  const high = bandAt(bands, HIGH_HZ);
  log(`output:  ${rate} Hz, tones at ${LOW_HZ}Hz (display band ${low}) and ${HIGH_HZ}Hz (band ${high})`);

  const start = await eqState();
  check(
    "the EQ starts flat and enabled",
    start.enabled &&
      start.preampDb === 0 &&
      start.gainsDb.every((db) => db === 0) &&
      start.responseDb.every((db) => Math.abs(db) < 0.001),
    `${start.gainsDb.length} sliders at 0, preamp 0, curve within ` +
      `${Math.max(...Array.from(start.responseDb).map(Math.abs)).toFixed(4)}dB of flat`,
  );

  await play();
  await sleep(SETTLE);
  const flatLevels = await spectrum();
  const flatLoudness = (await meters()).momentaryLufs;
  log(
    `baseline: ${flatLevels[low].toFixed(2)}dBFS at ${bands[low].toFixed(0)}Hz, ` +
      `${flatLevels[high].toFixed(2)}dBFS at ${bands[high].toFixed(0)}Hz, ` +
      `${flatLoudness.toFixed(2)} LUFS`,
  );

  // --- one slider, one band ---

  const boost = withBand(LOW_BAND, 12);
  const boosted = await setEq(boost);
  await sleep(SETTLE);
  const afterBoost = await spectrum();
  const moved = afterBoost[low] - flatLevels[low];
  check(
    "a +12dB slider moves its own band by 12dB",
    Math.abs(moved - 12) < 1.5,
    `${flatLevels[low].toFixed(2)}dBFS -> ${afterBoost[low].toFixed(2)}dBFS ` +
      `(${moved >= 0 ? "+" : ""}${moved.toFixed(2)}dB) at ${bands[low].toFixed(0)}Hz`,
  );
  const bystander = afterBoost[high] - flatLevels[high];
  check(
    "it leaves the other band alone",
    Math.abs(bystander) < 1.5,
    `${flatLevels[high].toFixed(2)}dBFS -> ${afterBoost[high].toFixed(2)}dBFS ` +
      `(${bystander >= 0 ? "+" : ""}${bystander.toFixed(2)}dB) at ${bands[high].toFixed(0)}Hz`,
  );
  check(
    "the audio matches the curve the UI draws",
    Math.abs(moved - curveAt(boosted, LOW_HZ)) < 1.5,
    `audio moved ${moved.toFixed(2)}dB, curve says ${curveAt(boosted, LOW_HZ).toFixed(2)}dB at ${LOW_HZ}Hz`,
  );

  await setEq(withBand(HIGH_BAND, -12));
  await sleep(SETTLE);
  const afterCut = await spectrum();
  check(
    "a -12dB slider cuts its own band by 12dB",
    Math.abs(afterCut[high] - flatLevels[high] + 12) < 1.5,
    `${flatLevels[high].toFixed(2)}dBFS -> ${afterCut[high].toFixed(2)}dBFS ` +
      `(${(afterCut[high] - flatLevels[high]).toFixed(2)}dB) at ${bands[high].toFixed(0)}Hz`,
  );
  check(
    "and releases the band the previous setting had boosted",
    Math.abs(afterCut[low] - flatLevels[low]) < 1.5,
    `${afterCut[low].toFixed(2)}dBFS against a flat ${flatLevels[low].toFixed(2)}dBFS`,
  );

  // --- the preamp ---

  await setEq({ ...flat(), preampDb: -6 });
  await sleep(SETTLE);
  const quieter = await spectrum();
  check(
    "the preamp moves the whole spectrum together",
    Math.abs(quieter[low] - flatLevels[low] + 6) < 1 &&
      Math.abs(quieter[high] - flatLevels[high] + 6) < 1,
    `${(quieter[low] - flatLevels[low]).toFixed(2)}dB at ${bands[low].toFixed(0)}Hz and ` +
      `${(quieter[high] - flatLevels[high]).toFixed(2)}dB at ${bands[high].toFixed(0)}Hz for a -6dB preamp`,
  );

  // The whole reason the filters sit ahead of the tap: what the loudness meter
  // reports is what the listener is actually being played (M8 decision 2).
  const quieterLoudness = (await meters()).momentaryLufs;
  check(
    "the loudness meter measures the EQ",
    Math.abs(quieterLoudness - flatLoudness + 6) < 1,
    `${flatLoudness.toFixed(2)} LUFS -> ${quieterLoudness.toFixed(2)} LUFS`,
  );

  // ...while the volume control still does not, which is the other half of the
  // same decision: monitoring level is not part of the programme.
  setVolume(0.5);
  await sleep(SETTLE);
  const louder = await spectrum();
  check(
    "the volume control still does not move the meters",
    Math.abs(louder[low] - quieter[low]) < 1.5,
    `${quieter[low].toFixed(2)}dBFS at volume 0.1, ${louder[low].toFixed(2)}dBFS at 0.5`,
  );
  setVolume(0.1);

  // --- bypass ---

  const bypassed = await setEq({ ...withBand(LOW_BAND, 12), preampDb: -6, enabled: false });
  await sleep(SETTLE);
  const off = await spectrum();
  check(
    "bypass returns the audio to where it started",
    Math.abs(off[low] - flatLevels[low]) < 1 && Math.abs(off[high] - flatLevels[high]) < 1,
    `${off[low].toFixed(2)}dBFS and ${off[high].toFixed(2)}dBFS against a flat ` +
      `${flatLevels[low].toFixed(2)} and ${flatLevels[high].toFixed(2)}`,
  );
  check(
    "and flattens the curve with it",
    bypassed.responseDb.every((db) => Math.abs(db) < 0.001),
    `max ${Math.max(...Array.from(bypassed.responseDb).map(Math.abs)).toFixed(4)}dB while the ` +
      `sliders still read ${bypassed.gainsDb[LOW_BAND].toFixed(0)}/${bypassed.preampDb.toFixed(0)}`,
  );

  // --- what the UI reads back ---

  const held = await setEq({
    enabled: true,
    preampDb: 99,
    gainsDb: [99, -99, NaN, 0, 0, 6, 0, 0, 0, 0],
  });
  check(
    "settings are clamped before the engine keeps them",
    held.preampDb === 12 &&
      held.gainsDb[0] === 12 &&
      held.gainsDb[1] === -12 &&
      held.gainsDb[2] === 0 &&
      held.gainsDb[5] === 6,
    `preamp ${held.preampDb.toFixed(0)}, sliders ${held.gainsDb[0].toFixed(0)}/` +
      `${held.gainsDb[1].toFixed(0)}/${held.gainsDb[2].toFixed(0)}`,
  );

  // --- a seek does not lose the setting ---

  await setEq(withBand(LOW_BAND, 12));
  seek(10);
  await sleep(SETTLE);
  const afterSeek = await spectrum();
  check(
    "the EQ survives a seek",
    Math.abs(afterSeek[low] - flatLevels[low] - 12) < 1.5,
    `${(afterSeek[low] - flatLevels[low]).toFixed(2)}dB at ${bands[low].toFixed(0)}Hz, ` +
      `${transport().position.toFixed(2)}s into the track`,
  );

  // --- the axis itself ---

  const axis = await eqState();
  check(
    "the curve's axis is log-spaced across the audible band",
    axis.responseFrequencies.length === 128 &&
      Math.abs(axis.responseFrequencies[0] - 20) < 0.01 &&
      Math.abs(axis.responseFrequencies[axis.responseFrequencies.length - 1] - 20_000) < 1 &&
      axis.responseFrequencies.every((hz, i) => i === 0 || hz > axis.responseFrequencies[i - 1]) &&
      axis.bandFrequencies.length === 10,
    `${axis.responseFrequencies.length} points, ${axis.responseFrequencies[0].toFixed(0)}Hz..` +
      `${axis.responseFrequencies[axis.responseFrequencies.length - 1].toFixed(0)}Hz, for ` +
      `${axis.bandFrequencies.length} sliders at ${axis.bandFrequencies[0].toFixed(2)}Hz..` +
      `${axis.bandFrequencies[axis.bandFrequencies.length - 1].toFixed(0)}Hz`,
  );

  // --- the two copies of the difference equation ---

  await filtersAgree(rate);

  pause();
  await setEq(flat());
  log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => log(`ERROR: ${e}`));
});
