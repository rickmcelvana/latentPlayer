// Live check of the web queue and gapless hand-off (Phase 1 M3) -- the web
// counterpart of `cargo run -p player-core --example gapless_smoke`, running
// the same measurements against the same claims so the two shells can be held
// to one standard. Served at /gapless.html by the dev server; not part of the
// production bundle.
//
// It synthesises its own WAVs rather than needing fixtures, and tells the two
// tracks apart by measuring the audio rather than trusting what the engine
// reports about itself:
//
//   left channel   a tone, 220Hz in the first track and 440Hz in the second.
//                  Counting zero crossings in the scope says which track is
//                  really playing.
//   right channel  a ramp from 0 to 1 across the track, so any sample says
//                  where in its track it came from.

import {
  clearNext,
  loadFile,
  outputChannels,
  outputSampleRate,
  play,
  scopeWindow,
  seek,
  setNext,
  setVolume,
  trackIndex,
  transport,
} from "./playerEngine";

const SRC_RATE = 44_100;
const FIRST_SECONDS = 3;
const FIRST_HZ = 220;
/// Deliberately a different length from the first, so "the duration changed at
/// the hand-off" is a real observation rather than a coincidence.
const SECOND_SECONDS = 5;
const SECOND_HZ = 440;

/// Frames of scope history to measure at a time -- ~85ms at 48kHz, well inside
/// the ring's ~170ms, so successive polls overlap and nothing played is missed.
const WINDOW_FRAMES = 4096;

/// Consecutive zero samples in the tone channel that count as a dropout. A
/// 220Hz sine crosses zero every ~109 samples but is only *exactly* zero for a
/// sample or so; a real gap would be hundreds of samples of true silence.
const DROPOUT_SAMPLES = 32;

/// The scope window is interleaved in the output's layout, which is fixed for
/// the session -- read from the engine rather than assumed.
let stride = 2;
let outputRate = 48_000;

const out = document.getElementById("out") as HTMLPreElement;
let failures = 0;

function log(line: string): void {
  out.textContent += `${line}\n`;
}

function check(label: string, ok: boolean, detail: string): void {
  if (!ok) failures += 1;
  log(`  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}`);
}

function near(got: number, want: number, tol: number): boolean {
  return Math.abs(got - want) <= tol;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/// A 16-bit stereo WAV: left is a `hz` tone, right ramps 0 -> 1 across the
/// track. Same generator as the native example's `write_wav`.
function makeWav(seconds: number, hz: number): File {
  const frames = SRC_RATE * seconds;
  const blockAlign = 4; // 2 channels * 16 bit
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
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 2, true);
  view.setUint32(24, SRC_RATE, true);
  view.setUint32(28, SRC_RATE * blockAlign, true);
  view.setUint16(32, blockAlign, true);
  view.setUint16(34, 16, true);
  ascii(36, "data");
  view.setUint32(40, dataLen, true);

  for (let i = 0; i < frames; i++) {
    const t = (2 * Math.PI * hz * i) / SRC_RATE;
    view.setInt16(44 + i * 4, Math.sin(t) * 0.7 * 32_767, true);
    view.setInt16(44 + i * 4 + 2, (i / frames) * 32_767, true);
  }
  return new File([buffer], `tone-${hz}hz-${seconds}s.wav`, { type: "audio/wav" });
}

/// One channel lifted out of the interleaved scope window.
function channel(index: number): Float32Array {
  const window = scopeWindow(WINDOW_FRAMES * stride);
  const frames = Math.floor(window.length / stride);
  const result = new Float32Array(frames);
  for (let i = 0; i < frames; i++) result[i] = window[i * stride + index];
  return result;
}

/// Frequency of the tone reaching the output, from its zero-crossing rate.
function playedHz(): number {
  const tone = channel(0);
  if (tone.length < 2) return NaN;
  let crossings = 0;
  for (let i = 1; i < tone.length; i++) {
    if (tone[i - 1] < 0 !== tone[i] < 0) crossings += 1;
  }
  const seconds = tone.length / outputRate;
  return crossings / seconds / 2;
}

/// Where in its own track the audio reaching the output came from.
function playedFraction(): number {
  const ramp = channel(1);
  if (ramp.length === 0) return NaN;
  let sum = 0;
  for (const s of ramp) sum += s;
  return sum / ramp.length;
}

/// Longest run of exactly-zero samples in the tone channel -- the fingerprint
/// of the ring running dry.
function longestSilence(): number {
  let longest = 0;
  let run = 0;
  for (const sample of channel(0)) {
    if (sample === 0) {
      run += 1;
      if (run > longest) longest = run;
    } else {
      run = 0;
    }
  }
  return longest;
}

async function watchSeam(timeoutMs: number) {
  const startIndex = trackIndex();
  const deadline = performance.now() + timeoutMs;
  const seam = { handedOff: false, longestSilence: 0, reportedFinished: false };

  while (performance.now() < deadline) {
    seam.longestSilence = Math.max(seam.longestSilence, longestSilence());
    // A hand-off is not an ending, so `finished` must stay false right across
    // it -- a caller watching that flag to advance a playlist would otherwise
    // double-advance at every track.
    if (transport().finished) seam.reportedFinished = true;
    if (trackIndex() > startIndex) {
      seam.handedOff = true;
      // Keep sampling briefly: a dropout at the seam lands just after the
      // boundary as often as just before it.
      await sleep(120);
      seam.longestSilence = Math.max(seam.longestSilence, longestSilence());
      break;
    }
    await sleep(25);
  }
  return seam;
}

async function run(): Promise<void> {
  out.textContent = "";
  failures = 0;

  if (!crossOriginIsolated) {
    log("FATAL: not crossOriginIsolated, so there is no SharedArrayBuffer.");
    return;
  }

  const first = makeWav(FIRST_SECONDS, FIRST_HZ);
  const second = makeWav(SECOND_SECONDS, SECOND_HZ);

  await loadFile(first);
  setVolume(0.1); // be kind to whoever is at the keyboard
  outputRate = outputSampleRate();
  stride = outputChannels();
  log(`output:  ${stride} ch @ ${outputRate} Hz (resampled from ${SRC_RATE})`);

  check(
    "the queue starts on the first track",
    near(transport().duration, FIRST_SECONDS, 0.05) && trackIndex() === 0,
    `${transport().duration.toFixed(3)}s, trackIndex ${trackIndex()}`,
  );

  await setNext(second);
  await play();
  await sleep(1500);

  check(
    "queuing a track does not disturb the one playing",
    near(playedHz(), FIRST_HZ, 15) && trackIndex() === 0,
    `${playedHz().toFixed(0)}Hz reaching the output (want ~${FIRST_HZ}Hz), ` +
      `trackIndex ${trackIndex()}`,
  );

  // --- the seek-during-hand-off window ---------------------------------
  //
  // The worker runs up to a ring (~2s) ahead, so 1.5s into a 3s track it has
  // already finished this track and moved on to the queued one. A seek
  // arriving now must scrub the track being *heard*, not the one being
  // decoded. Getting this wrong jumps into the next song, and only in the last
  // couple of seconds of every track.
  seek(0.5);
  await sleep(400);
  const fraction = playedFraction();
  check(
    "seeking near the end of a track stays in that track",
    near(playedHz(), FIRST_HZ, 15) &&
      trackIndex() === 0 &&
      near(fraction, 0.9 / FIRST_SECONDS, 0.06),
    `${playedHz().toFixed(0)}Hz, trackIndex ${trackIndex()}, ramp reads ` +
      `${fraction.toFixed(3)} (want ~${(0.9 / FIRST_SECONDS).toFixed(3)}; ~0.0 with a ` +
      `440Hz tone would mean it scrubbed the queued track instead)`,
  );

  // --- the seam --------------------------------------------------------
  const seam = await watchSeam(6000);
  check("the hand-off happens when the first track runs out", seam.handedOff, `trackIndex ${trackIndex()}`);
  check(
    "no silence at the seam",
    seam.longestSilence < DROPOUT_SAMPLES,
    `longest run of silence across the boundary: ${seam.longestSilence} samples ` +
      `(a dropout would be hundreds)`,
  );
  check("a hand-off is not an ending", !seam.reportedFinished, `finished stayed ${seam.reportedFinished}`);

  check(
    "the new track's length is published when its audio arrives",
    near(transport().duration, SECOND_SECONDS, 0.05),
    `${transport().duration.toFixed(3)}s (want ~${SECOND_SECONDS}s)`,
  );
  check(
    "the position restarts in the new track",
    transport().position < 0.75,
    `${transport().position.toFixed(3)}s into the new track`,
  );
  check(
    "the audio really is the second track",
    near(playedHz(), SECOND_HZ, 20),
    `${playedHz().toFixed(0)}Hz reaching the output (want ~${SECOND_HZ}Hz)`,
  );

  // --- the end of the queue --------------------------------------------
  clearNext();
  seek(SECOND_SECONDS - 0.4);
  await sleep(1200);
  check(
    "running out of queue is an ending",
    transport().finished,
    `finished=${transport().finished} at ${transport().position.toFixed(3)}s`,
  );

  // --- queuing after the music stopped ---------------------------------
  await setNext(first);
  await sleep(600);
  check(
    "a track queued after the end starts playing",
    !transport().finished && near(playedHz(), FIRST_HZ, 15),
    `finished=${transport().finished}, ${playedHz().toFixed(0)}Hz reaching the output ` +
      `(want ~${FIRST_HZ}Hz)`,
  );

  log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
}

document.getElementById("run")!.addEventListener("click", () => {
  run().catch((err) => log(`FATAL: ${String(err)}`));
});
