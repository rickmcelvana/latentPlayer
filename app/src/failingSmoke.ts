// Failing well, on the web shell (Phase 2 M5) -- the counterpart of
// `cargo test -p player-core --test failing_well`. Served at /failing.html; not
// part of the production bundle.
//
// Two things only this side can check.
//
// The first is the one that would otherwise rot silently: `audioSab.ts` mirrors
// `BUFFERING_AFTER_SECONDS` from player-core, because `transport()` runs on the
// UI thread ten times a second where there is no wasm to ask. A mirror nobody
// checks is a mirror that drifts, and the drift here would be two shells
// disagreeing about when a stall is worth mentioning. So the Rust value is
// fetched through wasm and held against the TypeScript one -- the same
// arrangement eqFilter.ts's copy of the biquad lives under.
//
// The second is that this shell classifies a failure with the same Rust the
// desktop shell uses (decision 17), rather than by reading message text. Both
// run offline: there is no server involved and none needs to be connected.
import init, { bufferingAfterSeconds, subsonicFailureKind } from "./wasm/player_rt_wasm.js";
import { BUFFERING_AFTER_SECONDS, bufferingAfterFrames } from "./audioSab";

const out = document.getElementById("out") as HTMLPreElement;
let passed = 0;
let failed = 0;

function check(label: string, ok: boolean, detail: string) {
  if (ok) passed++;
  else failed++;
  out.textContent += `  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}\n`;
}

const bytes = (json: string) => new TextEncoder().encode(json);

async function run() {
  out.textContent = "";
  passed = 0;
  failed = 0;
  await init();

  // --- the mirrored threshold ------------------------------------------------
  const fromRust = bufferingAfterSeconds();
  check(
    "the buffering threshold matches player-core's",
    BUFFERING_AFTER_SECONDS === fromRust,
    `TypeScript says ${BUFFERING_AFTER_SECONDS}s, Rust says ${fromRust}s`,
  );
  check(
    "and it converts to frames the same way at any rate",
    bufferingAfterFrames(44_100) === Math.floor(fromRust * 44_100) &&
      bufferingAfterFrames(48_000) === Math.floor(fromRust * 48_000) &&
      bufferingAfterFrames(96_000) === bufferingAfterFrames(48_000) * 2,
    `${bufferingAfterFrames(44_100)} frames at 44.1k, ${bufferingAfterFrames(48_000)} at 48k`,
  );

  // --- the classification ----------------------------------------------------
  //
  // What the panel draws its advice from. Each of these used to produce one
  // sentence for three unrelated problems.
  const credentials = bytes(
    '{"subsonic-response":{"status":"failed","version":"1.16.1",' +
      '"error":{"code":40,"message":"Wrong username or password"}}}',
  );
  check(
    "a rejected password is a credentials failure",
    subsonicFailureKind(credentials) === "credentials",
    subsonicFailureKind(credentials),
  );

  const notFound = bytes(
    '{"subsonic-response":{"status":"failed","version":"1.16.1",' +
      '"error":{"code":70,"message":"not found"}}}',
  );
  check(
    "a server saying no for its own reasons is a server failure",
    subsonicFailureKind(notFound) === "server",
    subsonicFailureKind(notFound),
  );

  const portal = bytes("<html><body>Sign in to the wifi</body></html>");
  check(
    "a captive portal is not mistaken for an empty library",
    subsonicFailureKind(portal) === "malformed",
    subsonicFailureKind(portal),
  );

  const ok = bytes('{"subsonic-response":{"status":"ok","version":"1.16.1"}}');
  check(
    "and a success is not a failure of any kind",
    subsonicFailureKind(ok) === "ok",
    subsonicFailureKind(ok),
  );

  out.textContent += `\n${passed} passed, ${failed} failed\n`;
}

document.getElementById("run")?.addEventListener("click", () => {
  run().catch((e) => {
    out.textContent += `\n  [FAIL] the harness itself threw: ${String(e)}\n`;
  });
});
