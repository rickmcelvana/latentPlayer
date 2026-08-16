// Live check of the web loudness and phase meters (Phase 1 M5) -- the web
// counterpart of `cargo run -p player-core --example loudness_smoke`, running
// the same measurements against the same claims so the two shells can be held
// to one standard. Served at /loudness.html by the dev server; not part of the
// production bundle.
//
// Everything here is measured against tones this file synthesises, so each
// check has an arithmetic answer rather than a plausible-looking number: a tone
// generated at -23 LUFS must read -23 LUFS, an inverted channel must read a
// correlation of -1, and a track change must start the integrated measurement
// over rather than averaging across it.

import {
  goniometer,
  loudnessFloorLufs,
  meters,
  pause,
  play,
  seek,
  setNext,
  setVolume,
  truePeakFloorDb,
} from "./playerEngine";
import { loadFile, outputChannels, outputSampleRate } from "./playerEngine";

const SRC_RATE = 44_100;
const TONE_SECONDS = 10;
const TONE_HZ = 1000;

/// The two tracks' loudness, in LUFS. -23 is EBU R128's delivery target; the
/// second is 10 LU quieter so that "did the integrated measurement start over
/// at the hand-off" has an answer 7 LU away from the one a meter that averaged
/// across the seam would give.
const FIRST_LUFS = -23;
const SECOND_LUFS = -33;

/// Amplitude of a sine that measures `lufs` when it is in both channels.
/// BS.1770-4 sums the weighted channels, so identical channels are 3.01 LU
/// louder than one alone; a sine's RMS is its amplitude over sqrt(2). The
/// native harness generates its tones from the same arithmetic.
function amplitudeFor(lufs: number): number {
  return Math.SQRT2 * 10 ** ((lufs - 3.01) / 20);
}

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

/// A 16-bit stereo WAV of one sine, with the right channel scaled by
/// `rightGain` (-1 inverts it). Same generator as the native example's, so both
/// shells measure the identical signal.
function makeWav(amplitude: number, rightGain: number): File {
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
    const s = amplitude * Math.sin((2 * Math.PI * TONE_HZ * i) / SRC_RATE) * 32_767;
    view.setInt16(44 + i * 4, s, true);
    view.setInt16(44 + i * 4 + 2, s * rightGain, true);
  }
  return new File([buffer], `tone-${amplitude.toFixed(4)}.wav`, { type: "audio/wav" });
}

/// How far the goniometer's points stray off an axis. Mono material has no side
/// component and an inverted pair has no mid one, so one coordinate of every
/// point should be ~0.
function offAxis(points: Float32Array, coordinate: number): number {
  let worst = 0;
  for (let i = coordinate; i < points.length; i += 2) worst = Math.max(worst, Math.abs(points[i]));
  return worst;
}

async function run(): Promise<void> {
  out.textContent = "";
  failures = 0;

  if (!crossOriginIsolated) {
    log("FATAL: not crossOriginIsolated, so there is no SharedArrayBuffer.");
    return;
  }

  await loadFile(makeWav(amplitudeFor(FIRST_LUFS), 1));
  setVolume(0.1); // be kind to whoever is at the keyboard
  log(
    `output:  ${outputSampleRate()} Hz, ${outputChannels()} ch; ` +
      `tones at ${FIRST_LUFS} and ${SECOND_LUFS} LUFS`,
  );

  const idle = await meters();
  check(
    "nothing measured yet reads the floor",
    idle.momentaryLufs <= loudnessFloorLufs() &&
      idle.integratedLufs <= loudnessFloorLufs() &&
      idle.truePeakDb <= truePeakFloorDb(),
    `M ${idle.momentaryLufs.toFixed(1)}, I ${idle.integratedLufs.toFixed(1)} LUFS, ` +
      `TP ${idle.truePeakDb.toFixed(1)} dBTP`,
  );

  await play();
  // Long enough for the 3s short-term window to fill.
  await sleep(4000);

  const m = await meters();
  check(
    "the tone reads the loudness it was generated at",
    Math.abs(m.momentaryLufs - FIRST_LUFS) < 0.5,
    `${m.momentaryLufs.toFixed(2)} LUFS momentary, want ${FIRST_LUFS}`,
  );
  check(
    "short-term agrees with momentary on steady material",
    Math.abs(m.shortTermLufs - m.momentaryLufs) < 0.3,
    `${m.shortTermLufs.toFixed(2)} short-term against ${m.momentaryLufs.toFixed(2)} momentary`,
  );
  check(
    "integrated agrees with them",
    Math.abs(m.integratedLufs - FIRST_LUFS) < 0.5,
    `${m.integratedLufs.toFixed(2)} LUFS integrated`,
  );
  // A sine's peak is its amplitude, and `amplitudeFor` puts that at exactly the
  // target loudness in dBFS for this signal -- so the true peak lands on the
  // same number by arithmetic rather than by coincidence.
  check(
    "true peak reads the tone's own peak",
    Math.abs(m.truePeakDb - FIRST_LUFS) < 0.5,
    `${m.truePeakDb.toFixed(2)} dBTP, want ${FIRST_LUFS}`,
  );
  check(
    "identical channels read a correlation of +1",
    Math.abs(m.correlation - 1) < 0.02,
    `${m.correlation.toFixed(3)}`,
  );

  const points = await goniometer();
  check(
    "the goniometer stands mono material up the centre",
    points.length > 0 && offAxis(points, 0) < 0.02,
    `${points.length / 2} points, ${offAxis(points, 0).toFixed(4)} of side energy at most`,
  );

  // The gain is on a GainNode after the tap, so the meters read the programme
  // rather than the monitoring level -- the same place the native callback
  // taps, and the only sane answer for a loudness meter.
  setVolume(0.5);
  await sleep(600);
  const louder = await meters();
  check(
    "the volume control does not move the meters",
    Math.abs(louder.momentaryLufs - m.momentaryLufs) < 0.3,
    `${m.momentaryLufs.toFixed(2)} LUFS at volume 0.1, ${louder.momentaryLufs.toFixed(2)} at 0.5`,
  );
  setVolume(0.1);

  // A gapless hand-off into the quieter, out-of-phase track. The audio is
  // continuous across the seam -- that is the point of M3 -- so nothing in the
  // tap marks it, and the meters only reset because the worker watches
  // `trackIndex` (M5 decision 2).
  await setNext(makeWav(amplitudeFor(SECOND_LUFS), -1));
  seek(TONE_SECONDS - 0.3);
  await sleep(4000);

  const after = await meters();
  check(
    "an inverted channel reads a correlation of -1",
    Math.abs(after.correlation + 1) < 0.02,
    `${after.correlation.toFixed(3)}`,
  );
  const flat = await goniometer();
  check(
    "the goniometer lays an out-of-phase pair flat",
    offAxis(flat, 1) < 0.02,
    `${offAxis(flat, 1).toFixed(4)} of mid energy at most`,
  );
  check(
    "the new track's momentary loudness is its own",
    Math.abs(after.momentaryLufs - SECOND_LUFS) < 0.5,
    `${after.momentaryLufs.toFixed(2)} LUFS, want ${SECOND_LUFS}`,
  );
  // Averaged across the seam instead of restarted, this would read about -25.6:
  // the mean power of four seconds at -23 and four at -33.
  check(
    "integrated starts over at the hand-off rather than averaging across it",
    Math.abs(after.integratedLufs - SECOND_LUFS) < 0.5,
    `${after.integratedLufs.toFixed(2)} LUFS integrated ` +
      `(-25.60 would mean it averaged the two tracks)`,
  );
  check(
    "true peak restarts with it",
    Math.abs(after.truePeakDb - SECOND_LUFS) < 0.5,
    `${after.truePeakDb.toFixed(2)} dBTP, want ${SECOND_LUFS}`,
  );

  // Pausing sends the output silence, and the tap records it. The momentary
  // window empties within 400ms; the integrated number must *not* follow it
  // down, because BS.1770-4's absolute gate exists precisely so that silence
  // between the music doesn't count as quiet music.
  const beforePause = await meters();
  pause();
  await sleep(1000);
  const paused = await meters();
  check(
    "a pause empties the momentary window",
    paused.momentaryLufs <= loudnessFloorLufs(),
    `${beforePause.momentaryLufs.toFixed(1)} -> ${paused.momentaryLufs.toFixed(1)} LUFS`,
  );
  check(
    "the gate keeps that silence out of the integrated number",
    Math.abs(paused.integratedLufs - beforePause.integratedLufs) < 0.3,
    `${beforePause.integratedLufs.toFixed(2)} -> ${paused.integratedLufs.toFixed(2)} LUFS ` +
      `across a second of silence`,
  );

  log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => log(`ERROR: ${e}`));
});
