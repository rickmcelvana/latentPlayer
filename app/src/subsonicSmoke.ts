// Live check of the web build's Subsonic client (Phase 2 M1) -- the web
// counterpart of `cargo test -p player-core --test subsonic_http` and
// `cargo run -p player-core --example subsonic_smoke`, run against a real
// server from inside a browser. Served at /subsonic.html; not part of the
// production bundle.
//
// Two of these checks have no native counterpart, and are why this harness
// exists at all: a browser can only reach a server that sends CORS headers, and
// cannot reach an http:// server from an https:// page. Both are ways the web
// build fails where the desktop build works, so both have to be *shown* to
// produce a message that names the cause (tasks/phase-2.md decision 5) rather
// than "failed to fetch".
import init, { subsonicParse, subsonicRequestUrl } from "./wasm/player_rt_wasm.js";
import { SubsonicFailure, testing } from "./subsonicWeb";
import { CURRENT_SERVER, type StoredServer } from "./libraryStore";

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

function value(id: string): string {
  return (document.getElementById(id) as HTMLInputElement).value.trim();
}

function server(baseUrl: string, user: string, secret: string): StoredServer {
  return { id: CURRENT_SERVER, baseUrl, user, secret, auth: "password" };
}

async function run() {
  out.textContent = "";
  passed = 0;
  failed = 0;
  await init();

  const baseUrl = value("url");
  const user = value("user");
  const secret = value("secret");

  // --- the protocol, with no network involved --------------------------------
  //
  // The same known answer the Rust tests assert on: the spec's own worked
  // example. This is the check that would catch a wasm build whose md5 differs
  // from the native one -- the failure that would make the desktop app work and
  // this one not, for reasons nobody would look for in a *hash*.
  const specUrl = subsonicRequestUrl(
    "http://example.com",
    "rick",
    "sesame",
    false,
    "ping",
    "c19b2d",
    "",
  );
  check(
    "the token matches the spec's worked example",
    specUrl.includes("t=26719a1196d2a940705a59634eb18eab"),
    specUrl.replace(/^https?:\/\/example\.com/, ""),
  );
  check(
    "the password never appears in the URL",
    !specUrl.includes("sesame") && !specUrl.includes("p="),
    "u/t/s only",
  );

  // The trap, from the same bytes the native tests use: a failure delivered as
  // an HTTP 200 has to come back as a failure on this side too, with the same
  // classification.
  const failedBody = new TextEncoder().encode(
    '{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}',
  );
  const parsedFailure = JSON.parse(subsonicParse(failedBody));
  check(
    "a failure inside an HTTP 200 parses as a failure",
    parsedFailure.ok === false &&
      parsedFailure.error.code === "BadCredentials" &&
      parsedFailure.error.credentials === true,
    JSON.stringify(parsedFailure.error),
  );

  const salts = new Set([testing.freshSalt(), testing.freshSalt(), testing.freshSalt()]);
  check("every request gets a fresh salt", salts.size === 3, `${salts.size} distinct of 3`);

  // --- the network, which is the half only a browser can check ---------------
  if (!baseUrl) {
    skip("the live checks", "no address given");
    return summary();
  }

  const rejected = server(baseUrl, "not-a-real-user", "not-a-real-password");
  try {
    await testing.call(rejected, "ping");
    check("a wrong password is rejected", false, "the server accepted credentials it should not");
  } catch (e) {
    if (e instanceof SubsonicFailure && e.kind === "server") {
      check(
        "a wrong password is reported as a credentials problem",
        e.credentials,
        `${e.kind}: ${e.message}`,
      );
      // Reaching a server error at all means the browser got through: CORS
      // headers present, no mixed-content block, the request shape accepted.
      check(
        "the browser can reach the server (CORS, and no mixed-content block)",
        true,
        `${location.origin} -> ${baseUrl}`,
      );
    } else if (e instanceof SubsonicFailure && e.kind === "transport") {
      check(
        "the browser can reach the server (CORS, and no mixed-content block)",
        false,
        e.message,
      );
      return summary();
    } else {
      check("a wrong password is reported as a credentials problem", false, String(e));
      return summary();
    }
  }

  try {
    const extensions = await testing.call(rejected, "getOpenSubsonicExtensions");
    const list = (extensions.payload.openSubsonicExtensions ?? []) as { name: string }[];
    check(
      "the server's OpenSubsonic extensions parse",
      Array.isArray(list),
      list.map((e) => e.name).join(", ") || "none advertised",
    );
  } catch (e) {
    check("the server's OpenSubsonic extensions parse", false, String(e));
  }

  // --- and finally, that the token this client computes is accepted ----------
  if (!user || !secret) {
    skip("the authenticated checks", "fill in a username and password to run them");
    return summary();
  }

  try {
    const state = await testing.connectTo(server(baseUrl, user, secret));
    check(
      "a correct password authenticates",
      state.connected,
      `${state.serverType} ${state.serverVersion}, API ${state.apiVersion}${
        state.openSubsonic ? ", OpenSubsonic" : ""
      }${state.supportsApiKey ? ", API keys supported" : ""}`,
    );
  } catch (e) {
    check(
      "a correct password authenticates",
      false,
      `${e} -- if this says wrong username or password, the token scheme is wrong`,
    );
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
