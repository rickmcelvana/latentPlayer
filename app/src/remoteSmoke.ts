// Live check of remote playback in the browser (Phase 2 M2) -- the web
// counterpart of `cargo run -p player-core --example remote_smoke` and
// `cargo test -p player-core --test remote_source`. Served at /remote.html; not
// part of the production bundle.
//
// It drives the same modules the UI does -- playlist.ts's `loadTrack` and
// `queueTrack`, which is where the two shells' one difference lives -- rather
// than a parallel path of its own, so a pass here is a statement about the app
// and not about the harness.
//
// The server it uses is the one the app's own panel stored, read out of the same
// IndexedDB. So there is nowhere to type a password here: connect in the app
// first, then run this.
import init, { subsonicRandomSongs, subsonicStreamUrl } from "./wasm/player_rt_wasm.js";
import { scopeWindow } from "./playerEngine";
import { loadTrack, queueTrack, readTransport, seek, setVolume, trackFromServer } from "./playlist";
import { randomTracks, serverState, trackBytes } from "./subsonicWeb";

const out = document.getElementById("out") as HTMLPreElement;
let passed = 0;
let failed = 0;

function check(label: string, ok: boolean, detail: string) {
  if (ok) passed++;
  else failed++;
  out.textContent += `  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}\n`;
}

function skip(label: string, why: string) {
  out.textContent += `  [SKIP] ${label}: ${why}\n`;
}

function note(text: string) {
  out.textContent += `${text}\n`;
}

function near(got: number, want: number, tol: number): boolean {
  return Math.abs(got - want) <= tol;
}

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/// A `getRandomSongs` body with one known song in it, for the offline half.
const CANNED_SONGS = new TextEncoder().encode(
  '{"subsonic-response":{"status":"ok","version":"1.16.1","randomSongs":{"song":[' +
    '{"id":"300","title":"Sleep","artist":"Godspeed","album":"Lift Yr Skinny Fists",' +
    '"suffix":"flac","duration":137,"size":41234567}]}}}',
);

/// A failure inside an HTTP 200, which is how this protocol reports everything.
const CANNED_FAILURE = new TextEncoder().encode(
  '{"subsonic-response":{"status":"failed","version":"1.16.1",' +
    '"error":{"code":70,"message":"Song not found"}}}',
);

async function run() {
  out.textContent = "";
  passed = 0;
  failed = 0;
  await init();

  // --- the protocol, with no network involved --------------------------------
  //
  // The stream URL is built by the same Rust the desktop shell calls, and this
  // is the check that would catch it drifting: `format=raw` is decision 7, and a
  // build that quietly dropped it would still play -- transcoded, unseekable,
  // and slower -- which is exactly the kind of failure nobody notices.
  const url = subsonicStreamUrl("http://example.com", "rick", "sesame", false, "c19b2d", "300");
  check(
    "a stream URL carries the track, the format and the token",
    url.includes("id=300") &&
      url.includes("format=raw") &&
      url.includes("t=26719a1196d2a940705a59634eb18eab") &&
      !url.includes("sesame"),
    url.replace(/^https?:\/\/example\.com/, ""),
  );

  const parsedSongs = JSON.parse(subsonicRandomSongs(CANNED_SONGS));
  check(
    "a song list parses into playable rows",
    parsedSongs.ok === true &&
      parsedSongs.songs.length === 1 &&
      parsedSongs.songs[0].id === "300" &&
      parsedSongs.songs[0].suffix === "flac" &&
      parsedSongs.songs[0].durationSeconds === 137,
    JSON.stringify(parsedSongs.songs?.[0] ?? parsedSongs),
  );

  const parsedFailure = JSON.parse(subsonicRandomSongs(CANNED_FAILURE));
  check(
    "a failure inside an HTTP 200 is a failure here too",
    parsedFailure.ok === false && parsedFailure.error.code === "NotFound",
    JSON.stringify(parsedFailure.error ?? parsedFailure),
  );

  // --- and now the network ---------------------------------------------------
  const state = await serverState();
  if (!state.configured) {
    skip("the live checks", "no server is set up -- connect one in the app, then run this");
    return summary();
  }
  note(`server:  ${state.baseUrl} as ${state.user}`);

  let songs;
  try {
    songs = await randomTracks(3);
  } catch (e) {
    check("the server hands back something to play", false, String(e));
    return summary();
  }
  if (songs.length === 0) {
    skip("the live checks", "the server has no songs in it yet");
    return summary();
  }
  const song = songs[0]!;
  note(`track:   ${song.artist} -- ${song.title} (${song.album}, ${song.durationSeconds}s, ${song.suffix})`);

  // The fetch, measured: this is the cost the web shell pays that the desktop
  // shell does not (a whole track before the first sample, rather than 512KB).
  // Reported rather than asserted on -- it is a property of the network, not of
  // the code -- but reported because it is the number that decides whether this
  // is acceptable on a real connection.
  let bytes: Uint8Array;
  const startedFetch = performance.now();
  try {
    bytes = await trackBytes(song.id);
  } catch (e) {
    check("a server track's bytes arrive", false, String(e));
    return summary();
  }
  const fetched = performance.now() - startedFetch;
  check(
    "a server track's bytes arrive, and are audio rather than an error page",
    bytes.byteLength > 1024,
    `${(bytes.byteLength / 1024 / 1024).toFixed(1)}MB in ${fetched.toFixed(0)}ms`,
  );

  // --- through the real playlist path ---------------------------------------
  const entry = trackFromServer(song);
  setVolume(0.1); // be kind to whoever is at the keyboard
  let loaded;
  const startedOpen = performance.now();
  try {
    loaded = await loadTrack(entry);
  } catch (e) {
    check("the track opens and plays through the app's own path", false, String(e));
    return summary();
  }
  const opened = performance.now() - startedOpen;
  check(
    "the track opens and plays through the app's own path",
    loaded.duration > 0 || loaded.playing,
    `${opened.toFixed(0)}ms to first audio, ${loaded.duration.toFixed(1)}s decoded length`,
  );

  // The server states a duration and the container states one; disagreeing
  // means the wrong track was fetched, which a URL built with a wrong id would
  // do silently.
  check(
    "the track opened is the track asked for",
    song.durationSeconds === 0 ||
      loaded.duration === 0 ||
      near(loaded.duration, song.durationSeconds, 2),
    `the server says ${song.durationSeconds}s, the container says ${loaded.duration.toFixed(1)}s`,
  );

  await wait(1500);
  const advanced = await readTransport();
  check(
    "playback advances in real time",
    near(advanced.position, 1.5, 0.35),
    `${advanced.position.toFixed(3)}s after 1.5s of wall clock`,
  );

  // Audio actually reaching the output, rather than a position counter moving on
  // its own: the scope's window holds what was played, and silence reads as
  // zeroes. Any non-zero sample rather than a level, so a genuinely quiet track
  // is not a false failure.
  const window = scopeWindow(4096);
  let peak = 0;
  for (const sample of window) peak = Math.max(peak, Math.abs(sample));
  check(
    "real audio reached the output",
    peak > 0,
    `peak ${peak.toFixed(4)} over ${window.length} samples of history`,
  );

  const target = Math.max(loaded.duration * 0.6, 3);
  await seek(target);
  await wait(800);
  const afterSeek = await readTransport();
  check(
    "seeking a network track lands and keeps playing",
    near(afterSeek.position, target + 0.8, 0.4),
    `${afterSeek.position.toFixed(3)}s after seeking to ${target.toFixed(1)}s and waiting 0.8s`,
  );

  // --- the gapless hand-off, between two server tracks ----------------------
  if (songs.length > 1) {
    const next = songs[1]!;
    try {
      // Queuing is where the web shell prefetches -- the whole of the next
      // track, now, so that the seam has nothing left to wait for.
      const startedQueue = performance.now();
      await queueTrack(trackFromServer(next));
      const prefetched = performance.now() - startedQueue;
      await seek(Math.max(loaded.duration - 1, 0));
      let handedOver = false;
      for (let attempt = 0; attempt < 60 && !handedOver; attempt++) {
        await wait(100);
        handedOver = (await readTransport()).trackIndex > 0;
      }
      check(
        "the queued server track takes over at the seam",
        handedOver,
        handedOver
          ? `prefetched in ${prefetched.toFixed(0)}ms, then handed over to ${next.title}`
          : `still on the first track after 6s (next: ${next.title})`,
      );
    } catch (e) {
      check("the queued server track takes over at the seam", false, String(e));
    }
  }

  summary();
}

function summary() {
  out.textContent += `\n${passed}/${passed + failed} checks passed\n`;
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => {
    out.textContent += `\nthe harness itself failed: ${e}\n`;
  });
});
