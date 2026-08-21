// Live check of the web build's musical intelligence (Phase 3 M5) -- the web
// counterpart of `cargo run -p player-core --example musical_smoke`, running
// the same checks against the same claims so the two shells can be held to one
// standard. Served at /musical.html by the dev server; not part of the
// production bundle.
//
// The fixtures are synthesised here from the arithmetic the native harness uses
// -- including the same linear congruential generator behind the noise bursts,
// so the click track this file writes is the one that file writes, sample for
// sample. That is what makes the two shells' figures comparable rather than
// merely similar.
//
// Every check has a known answer: a click track generated at 120 BPM must read
// 120, one at 90 must read 90 (a tempo no integer autocorrelation lag can
// express), an offbeat on every eighth must not double the reading, material
// with no steady pulse must say so, and a track change must start the estimate
// over rather than carrying the previous track's tempo into it.

import { NO_TEMPO } from "./analysis";
import {
  loadFile,
  musical,
  outputChannels,
  outputSampleRate,
  pause,
  play,
  seek,
  setNext,
  setVolume,
} from "./playerEngine";

const SRC_RATE = 44_100;
const TRACK_SECONDS = 30;

/// The two click tracks' tempi. 120 is the tempo prior's own centre; 90 is well
/// off it *and* falls between autocorrelation lags (66.67 at a 100Hz detection
/// function), so a tracker that reported its prior, or one that reported an
/// integer lag, fails one of the two.
const FIRST_BPM = 120;
const SECOND_BPM = 90;

/// The tone used for the brightness check, and for "a held note is not a beat".
const TONE_HZ = 1000;

/// How long to let a tempo settle before reading it: the tracker's warm-up is
/// four seconds and its autocorrelation window is eight, so nine seconds is a
/// full window plus the time for one refresh.
const SETTLE_MS = 9000;

/// How close a reading has to be to the tempo the fixture was generated at.
const BPM_TOLERANCE = 0.5;

const out = document.getElementById("out") as HTMLPreElement;
let failures = 0;
let run = 0;

function log(line: string): void {
  out.textContent += `${line}\n`;
}

function check(label: string, ok: boolean, detail: string): void {
  run += 1;
  if (!ok) failures += 1;
  log(`  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}`);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/// The pseudo-random source both shells use for their noise bursts -- the same
/// LCG as the native harness's `Noise`, in the same order, so the audio is
/// identical.
function noise(): () => number {
  let state = 2_463_534_242;
  return () => {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    return (state >>> 8) / 8_388_608 - 1;
  };
}

type Fixture =
  | { kind: "clicks"; bpm: number; offbeat: boolean }
  | { kind: "tone" }
  | { kind: "rubato" }
  | { kind: "pad" };

/// Frames between clicks at `bpm`. Rounded, so both shells place them
/// identically.
function clickPeriod(bpm: number): number {
  return Math.round((60 / bpm) * SRC_RATE);
}

/// How long the pad takes to reach full level. See the pad fixture below.
const PAD_FADE_SECONDS = 1.0;

/// Onsets in seconds and the note each starts, for the rubato fixture.
/// Irregular on purpose: no two gaps are the same, and no gap is a multiple of
/// another, so there is no tempo here to find.
const NOTES: [number, number][] = [
  [0.0, 220.0],
  [0.71, 277.2],
  [1.13, 329.6],
  [2.02, 261.6],
  [2.35, 392.0],
  [3.4, 220.0],
  [3.79, 293.7],
  [4.9, 349.2],
  [5.22, 246.9],
  [6.4, 196.0],
  [7.05, 311.1],
  [8.3, 261.6],
];

/// One fixture as mono samples. The native half is `render()` in
/// musical_smoke.rs, and the arithmetic is line for line the same.
function render(fixture: Fixture, frames: number): Float32Array {
  const next = noise();
  const out = new Float32Array(frames);
  if (fixture.kind === "clicks") {
    const period = clickPeriod(fixture.bpm);
    const half = Math.floor(period / 2);
    for (let n = 0; n < frames; n++) {
      const into = n % period;
      // 128 frames of decaying noise: about 3ms, short enough to be one onset
      // and broad enough to move every bin.
      if (into < 128) {
        out[n] = next() * (1 - into / 128) * 0.7;
      } else if (fixture.offbeat && into >= half && into < half + 128) {
        const since = into - half;
        out[n] = next() * (1 - since / 128) * 0.35;
      }
    }
    return out;
  }
  if (fixture.kind === "pad") {
    // Four partials, slightly detuned so they beat against each other, under a
    // slow swell -- every ingredient of the material M6's live look found being
    // misread as a confident 142 BPM, and no event anywhere in it.
    for (let n = 0; n < frames; n++) {
      const t = n / SRC_RATE;
      const swell = 0.5 + 0.5 * Math.sin(2 * Math.PI * 0.07 * t);
      let voices = 0;
      for (const [hz, amp] of [
        [110.0, 0.5],
        [164.8, 0.35],
        [220.5, 0.3],
        [330.0, 0.2],
      ]) {
        voices += amp * Math.sin(2 * Math.PI * hz * t);
      }
      // Faded in over a second, because a file that begins at half amplitude
      // on its first sample *does* begin with an attack -- and the detector is
      // right to say so. Without this the fixture's only transient is the
      // fixture's own start, which is not what it is here to measure.
      const fade = Math.min(t / PAD_FADE_SECONDS, 1);
      out[n] = 0.35 * fade * swell * voices;
    }
    return out;
  }
  if (fixture.kind === "tone") {
    for (let n = 0; n < frames; n++) {
      out[n] = 0.5 * Math.sin((2 * Math.PI * TONE_HZ * n) / SRC_RATE);
    }
    return out;
  }
  const loopFrames = Math.floor(9.5 * SRC_RATE);
  for (let start = 0; start < frames; start += loopFrames) {
    for (const [at, hz] of NOTES) {
      const onset = start + Math.floor(at * SRC_RATE);
      for (let i = 0; i < SRC_RATE; i++) {
        if (onset + i >= frames) break;
        const t = i / SRC_RATE;
        // A 30ms attack and a slow decay: an onset a detector can find, with
        // none of a drum's sharpness.
        const attack = Math.min(t / 0.03, 1);
        const decay = Math.exp(-t * 2.5);
        out[onset + i] += 0.35 * attack * decay * Math.sin(2 * Math.PI * hz * t);
      }
    }
  }
  return out;
}

/// A 16-bit stereo WAV of `fixture`, identical to the native harness's.
function makeWav(fixture: Fixture, name: string): File {
  const frames = SRC_RATE * TRACK_SECONDS;
  const mono = render(fixture, frames);
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
    const value = Math.max(-1, Math.min(1, mono[i])) * 32_767;
    view.setInt16(44 + i * 4, value, true);
    view.setInt16(44 + i * 4 + 2, value, true);
  }
  return new File([buffer], name, { type: "audio/wav" });
}

interface Watched {
  onsets: number;
  /// Beat phases sampled at the moment an onset was reported.
  phaseAtOnsets: number[];
  lowestPhase: number;
  highestPhase: number;
  /// The brightest window seen. A click track's *newest* window is silence most
  /// of the time -- the clicks are 3ms apart in a 500ms beat -- so the centroid
  /// of a percussive track is a thing you catch, not a thing you read
  /// afterwards.
  brightest: number;
  /// The strongest transient seen. What M6's live look turned into a figure: on
  /// material with no attacks this has to stay near zero, and it read 1.000
  /// until `onset.rs` was given an absolute reference to measure against.
  strongestTransient: number;
}

/// Plays for `ms`, polling faster than the analysis publishes so that onsets
/// are seen rather than stepped over.
async function watch(ms: number): Promise<Watched> {
  const until = performance.now() + ms;
  const watched: Watched = {
    onsets: 0,
    phaseAtOnsets: [],
    lowestPhase: Number.POSITIVE_INFINITY,
    highestPhase: Number.NEGATIVE_INFINITY,
    brightest: 0,
    strongestTransient: 0,
  };
  let lastOnset = 0;
  while (performance.now() < until) {
    const m = musical();
    if (m.onset > 0 && lastOnset === 0) {
      watched.onsets += 1;
      if (m.bpm > NO_TEMPO) watched.phaseAtOnsets.push(m.beatPhase);
    }
    lastOnset = m.onset;
    watched.brightest = Math.max(watched.brightest, m.centroidHz);
    watched.strongestTransient = Math.max(watched.strongestTransient, m.transient);
    if (m.bpm > NO_TEMPO) {
      watched.lowestPhase = Math.min(watched.lowestPhase, m.beatPhase);
      watched.highestPhase = Math.max(watched.highestPhase, m.beatPhase);
    }
    await sleep(10);
  }
  return watched;
}

/// How far a phase is from the nearest beat, in beats.
function offBeat(phase: number): number {
  return Math.min(phase, 1 - phase);
}

async function runSmoke(): Promise<void> {
  out.textContent = "";
  failures = 0;
  run = 0;

  if (!crossOriginIsolated) {
    log("FATAL: not crossOriginIsolated, so there is no SharedArrayBuffer.");
    return;
  }

  const first = makeWav({ kind: "clicks", bpm: FIRST_BPM, offbeat: false }, "click-120.wav");
  const second = makeWav({ kind: "clicks", bpm: SECOND_BPM, offbeat: false }, "click-90.wav");
  const offbeat = makeWav({ kind: "clicks", bpm: FIRST_BPM, offbeat: true }, "click-offbeat.wav");
  const tone = makeWav({ kind: "tone" }, "tone.wav");
  const rubato = makeWav({ kind: "rubato" }, "rubato.wav");
  const pad = makeWav({ kind: "pad" }, "pad.wav");

  await loadFile(first);
  setVolume(0.1); // be kind to whoever is at the keyboard
  log(
    `output:  ${outputSampleRate()} Hz, ${outputChannels()} ch; ` +
      `click tracks at ${FIRST_BPM} and ${SECOND_BPM} BPM`,
  );

  const idle = musical();
  check(
    "nothing measured yet reports no tempo",
    idle.bpm === NO_TEMPO && idle.beatConfidence === 0 && idle.onset === 0,
    `${idle.bpm.toFixed(1)} BPM, confidence ${idle.beatConfidence.toFixed(2)}`,
  );

  await play();
  let watched = await watch(SETTLE_MS);

  const m = musical();
  check(
    "a click track reads the tempo it was generated at",
    Math.abs(m.bpm - FIRST_BPM) < BPM_TOLERANCE,
    `${m.bpm.toFixed(2)} BPM, want ${FIRST_BPM}`,
  );
  check(
    "and says it is sure of it",
    m.beatConfidence > 0.5,
    `confidence ${m.beatConfidence.toFixed(2)}`,
  );
  // Nine seconds at 120 BPM is eighteen clicks, minus whatever the ring and the
  // tap were still filling when play() returned.
  check(
    "every click is an onset, and no click is two",
    watched.onsets >= 14 && watched.onsets <= 18,
    `${watched.onsets} onsets over ${SETTLE_MS / 1000}s`,
  );
  // The last few, not all of them: the phase is a loop that locks, and the
  // first onsets after a tempo is established are the ones it is locking
  // *with*.
  const locked = watched.phaseAtOnsets.slice(-5);
  const worst = locked.reduce((worst, phase) => Math.max(worst, offBeat(phase)), 0);
  check(
    "the beat phase is where the beat is, once it has locked",
    locked.length >= 3 && worst < 0.15,
    `${worst.toFixed(3)} of a beat out at worst over the last ${locked.length} of ` +
      `${watched.phaseAtOnsets.length} onsets`,
  );
  check(
    "and it sweeps the whole beat rather than sticking",
    watched.lowestPhase < 0.25 && watched.highestPhase > 0.75,
    `phase spanned ${watched.lowestPhase.toFixed(2)}..${watched.highestPhase.toFixed(2)}`,
  );
  check(
    "a click's own window is broadband",
    watched.brightest > 2000,
    `${watched.brightest.toFixed(0)} Hz at brightest`,
  );

  // Silence is not music: the onsets stop immediately, because the detection
  // function has nothing to detect.
  pause();
  await sleep(400);
  check(
    "a pause stops the onsets",
    musical().onset === 0,
    `onset ${musical().onset.toFixed(3)} while paused`,
  );
  await play();

  // A gapless hand-off into the slower track. The audio is continuous across
  // the seam -- that is the point of Phase 1 M3 -- so nothing in the tap marks
  // it, and the tempo only starts over because the worker watches the track
  // counter (decision 31).
  await setNext(second);
  seek(TRACK_SECONDS - 0.3);
  await sleep(SETTLE_MS);

  const after = musical();
  check(
    "the new track's tempo is its own, not the last one's",
    Math.abs(after.bpm - SECOND_BPM) < BPM_TOLERANCE,
    `${after.bpm.toFixed(2)} BPM, want ${SECOND_BPM} ` +
      `(${FIRST_BPM} would mean it carried across the seam)`,
  );
  // 90 BPM is lag 66.67 at a 100Hz detection function, and the integer lags
  // either side of it are 89.55 and 90.91 -- so this figure is also the check
  // that the autocorrelation peak is interpolated (decision 34).
  check(
    "a tempo between two lags is still reported exactly",
    Math.abs(after.bpm - SECOND_BPM) < BPM_TOLERANCE,
    `${after.bpm.toFixed(2)} BPM, against 89.55 and 90.91 either side of it`,
  );

  // The octave error, which is what every naive tempo estimator does with a
  // hi-hat: something on every eighth, half the strength of the beat.
  await loadFile(offbeat);
  setVolume(0.1);
  await play();
  await sleep(SETTLE_MS);
  const eighths = musical();
  check(
    "an offbeat on every eighth does not double the tempo",
    Math.abs(eighths.bpm - FIRST_BPM) < 1,
    `${eighths.bpm.toFixed(2)} BPM, want ${FIRST_BPM} (240 or 60 would be an octave error)`,
  );

  // A held tone: a brightness with an arithmetic answer, and no beat at all.
  await loadFile(tone);
  setVolume(0.1);
  await play();
  await sleep(SETTLE_MS);
  const held = musical();
  check(
    "brightness reads the tone's own frequency",
    Math.abs(held.centroidHz - TONE_HZ) < 100,
    `${held.centroidHz.toFixed(0)} Hz, want ${TONE_HZ}`,
  );
  check(
    "a held note is not a beat",
    held.bpm === NO_TEMPO,
    `${held.bpm.toFixed(1)} BPM, confidence ${held.beatConfidence.toFixed(2)}`,
  );

  // Rubato: onsets to find, and no tempo to find. The honest failure.
  await loadFile(rubato);
  setVolume(0.1);
  await play();
  watched = await watch(SETTLE_MS);
  const free = musical();
  check(
    "material with no steady pulse is not reported as confident",
    free.beatConfidence < 0.5,
    `confidence ${free.beatConfidence.toFixed(2)} at ${free.bpm.toFixed(1)} BPM`,
  );
  check(
    "but its onsets are still found",
    watched.onsets >= 6,
    `${watched.onsets} onsets over ${SETTLE_MS / 1000}s`,
  );
  check(
    "and its notes are darker than a click",
    watched.brightest > 0 && watched.brightest < 2000,
    `${watched.brightest.toFixed(0)} Hz at brightest`,
  );

  // The pad, which is where M6's live look found this suite believing itself.
  // Sustained material with no attacks must report no tempo and no transient --
  // the second is the cause and the first is the symptom, so both are checked
  // rather than only the one a listener noticed.
  await loadFile(pad);
  setVolume(0.1);
  await play();
  watched = await watch(SETTLE_MS);
  const sustained = musical();
  check(
    "a sustained pad is not a beat",
    sustained.bpm === NO_TEMPO && sustained.beatConfidence === 0,
    `${sustained.bpm.toFixed(1)} BPM, confidence ${sustained.beatConfidence.toFixed(2)} (142 BPM at 1.00 was the bug)`,
  );
  check(
    "and nothing in it reads as a transient",
    watched.strongestTransient < 0.25,
    `${watched.strongestTransient.toFixed(3)} at strongest, against 1.000 before the reference was absolute`,
  );

  pause();
  log(`\n${run - failures}/${run} checks passed`);
}

document.getElementById("run")?.addEventListener("click", () => {
  void runSmoke();
});
