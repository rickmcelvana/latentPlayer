// Live check of the web analysis tap and spectrum analyser (Phase 1 M4) -- the
// web counterpart of `cargo run -p player-core --example spectrum_smoke`,
// running the same measurements against the same claims so the two shells can
// be held to one standard. Served at /spectrum.html by the dev server; not part
// of the production bundle.
//
// Everything here is measured against tones this file synthesises, so each
// check has an arithmetic answer rather than a plausible-looking picture:
// a -3dBFS sine at 440Hz must appear in the band containing 440Hz, at -3dBFS,
// and nowhere else.

import {
  loadFile,
  outputSampleRate,
  pause,
  play,
  seek,
  setVolume,
  spectrum,
  spectrumBands,
  spectrumFloorDb,
  transport,
} from "./playerEngine";

const SRC_RATE = 44_100;
const TONE_SECONDS = 8;
const LOW_HZ = 440;
const HIGH_HZ = 3520;
/// Linear amplitude of the test tones: -3.1dBFS, chosen so "did the level
/// survive the whole path" has an answer that isn't 0dB (which a stuck
/// normalisation would also produce).
const TONE_AMPLITUDE = 0.7;
const TONE_DBFS = 20 * Math.log10(TONE_AMPLITUDE);

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

/// A 16-bit stereo WAV of a single sine. Same generator as the native
/// example's, so both shells analyse the identical signal.
function makeWav(seconds: number, hz: number): File {
  const frames = SRC_RATE * seconds;
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
    const s = TONE_AMPLITUDE * Math.sin((2 * Math.PI * hz * i) / SRC_RATE) * 32_767;
    view.setInt16(44 + i * 4, s, true);
    view.setInt16(44 + i * 4 + 2, s, true);
  }
  return new File([buffer], `tone-${hz}hz.wav`, { type: "audio/wav" });
}

/// Index of the band whose centre is nearest `hz` on a log scale.
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

function loudestBand(levels: Float32Array): number {
  let best = 0;
  for (let i = 1; i < levels.length; i++) if (levels[i] > levels[best]) best = i;
  return best;
}

async function run(): Promise<void> {
  out.textContent = "";
  failures = 0;

  if (!crossOriginIsolated) {
    log("FATAL: not crossOriginIsolated, so there is no SharedArrayBuffer.");
    return;
  }

  await loadFile(makeWav(TONE_SECONDS, LOW_HZ));
  setVolume(0.1); // be kind to whoever is at the keyboard
  const rate = outputSampleRate();
  const bands = await spectrumBands();
  log(`output:  ${rate} Hz, ${bands.length} bands, tones at ${TONE_DBFS.toFixed(1)} dBFS`);

  check(
    "the axis arrives from the engine",
    bands.length === 64 &&
      bands[0] >= 20 &&
      bands[bands.length - 1] < rate / 2 &&
      bands.every((hz, i) => i === 0 || hz > bands[i - 1]),
    `${bands.length} bands, ${bands[0].toFixed(1)}Hz..${bands[bands.length - 1].toFixed(0)}Hz, ascending`,
  );

  const idle = await spectrum();
  check(
    "nothing measured yet reads the floor, not full scale",
    idle.length === bands.length && idle.every((db) => db <= spectrumFloorDb()),
    `max ${Math.max(...idle).toFixed(1)}dB across ${idle.length} bands`,
  );

  await play();
  await sleep(600);

  const low = await spectrum();
  const wantLow = bandAt(bands, LOW_HZ);
  check(
    "the tone lands in the band that holds it",
    loudestBand(low) === wantLow,
    `loudest band ${loudestBand(low)} (${bands[loudestBand(low)].toFixed(0)}Hz), ` +
      `want ${wantLow} (${bands[wantLow].toFixed(0)}Hz)`,
  );
  check(
    "it reads the level it was generated at",
    Math.abs(low[wantLow] - TONE_DBFS) < 2,
    `${low[wantLow].toFixed(2)}dBFS, want ${TONE_DBFS.toFixed(2)}dBFS`,
  );

  // Two octaves down is 12 bands away at 6 bands/octave: far outside the
  // window's skirt, so anything there is leakage or a broken window.
  const far = bandAt(bands, LOW_HZ / 4);
  check(
    "energy stays where it belongs",
    low[far] < low[wantLow] - 40,
    `${low[far].toFixed(1)}dB two octaves down against a ${low[wantLow].toFixed(1)}dB peak`,
  );

  // The gain is on a GainNode after the tap, so the meters read the program
  // material rather than the monitoring level -- the same place the native
  // callback taps, and the only sane answer for a loudness meter.
  setVolume(0.5);
  await sleep(300);
  const louder = await spectrum();
  check(
    "the volume control does not move the meters",
    Math.abs(louder[wantLow] - low[wantLow]) < 1.5,
    `${low[wantLow].toFixed(2)}dBFS at volume 0.1, ${louder[wantLow].toFixed(2)}dBFS at 0.5`,
  );
  setVolume(0.1);

  // A second tone, at a frequency three octaves up: the display has to follow
  // the audio, not hold the first picture it drew.
  await loadFile(makeWav(TONE_SECONDS, HIGH_HZ));
  setVolume(0.1);
  await play();
  await sleep(800);
  const high = await spectrum();
  const wantHigh = bandAt(bands, HIGH_HZ);
  check(
    "a different tone moves the peak",
    loudestBand(high) === wantHigh && Math.abs(high[wantHigh] - TONE_DBFS) < 2,
    `loudest band ${loudestBand(high)} (${bands[loudestBand(high)].toFixed(0)}Hz) ` +
      `at ${high[wantHigh].toFixed(2)}dBFS, want ${wantHigh} (${bands[wantHigh].toFixed(0)}Hz)`,
  );
  check(
    "the band the old tone was in has gone quiet",
    high[wantLow] < high[wantHigh] - 40,
    `${high[wantLow].toFixed(1)}dB at ${bands[wantLow].toFixed(0)}Hz`,
  );

  // Pausing stops the audio but not the meters: they decay at the release rate
  // rather than freezing on the last frame, which is what makes a stalled
  // analysis thread visible instead of indistinguishable from a held note.
  const before = (await spectrum())[wantHigh];
  pause();
  await sleep(1000);
  const after = (await spectrum())[wantHigh];
  check(
    "a silent input decays rather than freezing",
    after < before - 20,
    `${before.toFixed(1)}dB -> ${after.toFixed(1)}dB over ~1s (release is 30dB/s)`,
  );

  // A seek clears the tap under the reader; the meters must come back rather
  // than latch whatever the discontinuity did to them.
  seek(2);
  await play();
  await sleep(700);
  const resumed = await spectrum();
  check(
    "the meters recover after a seek",
    loudestBand(resumed) === wantHigh && Math.abs(resumed[wantHigh] - TONE_DBFS) < 2,
    `${resumed[wantHigh].toFixed(2)}dBFS in band ${loudestBand(resumed)} at ` +
      `${transport().position.toFixed(2)}s`,
  );

  pause();
  log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => log(`ERROR: ${e}`));
});
