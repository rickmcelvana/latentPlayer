// The web build's Subsonic transport (Phase 2 M1): `fetch`, and nothing else.
//
// What this module is not allowed to contain is an opinion about the protocol.
// URLs are built by `subsonicRequestUrl` and responses are read by
// `subsonicParse`, both of which are `player_core::subsonic` compiled to wasm --
// the same code the desktop shell links natively (tasks/phase-2.md decision 2).
// So "what does error 40 mean", "is this the password's fault" and "what does a
// request have to carry" have one answer across both shells, and this file only
// decides how bytes travel.
//
// The two things it does own are the browser's own limits, which have no
// counterpart natively and must be named rather than surfaced as "failed to
// fetch" (decision 5): a page on https: may not talk to an http: server at all,
// and a server that does not send CORS headers is unreachable from any origin
// but its own.
import init, {
  subsonicParse,
  subsonicRandomSongs,
  subsonicRequestUrl,
  subsonicStreamUrl,
} from "./wasm/player_rt_wasm.js";
import {
  CURRENT_SERVER,
  deleteServer,
  putServer,
  storedServer,
  type StoredServer,
} from "./libraryStore";

/// Mirrors `ServerState` in src-tauri/src/servers.rs field for field, so
/// `server.ts` can hand either shell's answer to the same component.
export interface ServerState {
  configured: boolean;
  connected: boolean;
  baseUrl: string;
  user: string;
  auth: string;
  serverType: string;
  serverVersion: string;
  apiVersion: string;
  openSubsonic: boolean;
  supportsApiKey: boolean;
  error: string;
  errorIsCredentials: boolean;
}

export function emptyServerState(): ServerState {
  return {
    configured: false,
    connected: false,
    baseUrl: "",
    user: "",
    auth: "password",
    serverType: "",
    serverVersion: "",
    apiVersion: "",
    openSubsonic: false,
    supportsApiKey: false,
    error: "",
    errorIsCredentials: false,
  };
}

/// What `subsonicParse` hands back. The classification is Rust's; this is only
/// its shape on the way through.
interface ParsedError {
  kind: "server" | "http" | "malformed" | "transport" | "badUrl";
  code: string | null;
  message: string;
  credentials: boolean;
}

type Parsed =
  | {
      ok: true;
      info: {
        version: string;
        serverType: string;
        serverVersion: string;
        openSubsonic: boolean;
      };
      payload: Record<string, unknown>;
    }
  | { ok: false; error: ParsedError };

/// Thrown by `call` so the caller can tell a credentials problem from an
/// unreachable server without parsing message text.
export class SubsonicFailure extends Error {
  readonly kind: ParsedError["kind"];
  readonly credentials: boolean;

  constructor(error: ParsedError) {
    super(error.message);
    this.name = "SubsonicFailure";
    this.kind = error.kind;
    this.credentials = error.credentials;
  }
}

let wasmReady: Promise<unknown> | null = null;

async function ready(): Promise<void> {
  wasmReady ??= init();
  await wasmReady;
}

/// A fresh salt per request, from the browser's CSPRNG.
///
/// The native half of this is `getrandom` in player-core's http module; the
/// hashing both feed is the one in Rust (decision 3).
function freshSalt(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/// Why a `fetch` rejected, in terms of the thing the user has to change.
///
/// A browser deliberately refuses to say whether a cross-origin request failed
/// because of CORS, DNS or a refused connection -- they are all one opaque
/// `TypeError`. So this does not guess: it names the two causes that are
/// specific to running in a browser, and which the desktop build does not have,
/// so someone whose server plainly works in the desktop app is not left thinking
/// the address is wrong.
function browserFailure(baseUrl: string, cause: unknown): string {
  const mixed = location.protocol === "https:" && baseUrl.toLowerCase().startsWith("http://");
  if (mixed) {
    return (
      `This page is served over HTTPS, and browsers refuse to talk to an http:// address from ` +
      `it at all. Serve the server over HTTPS, or use the desktop app, which has no such rule.`
    );
  }
  return (
    `The browser could not reach ${baseUrl}. It is either not running, or it does not send the ` +
    `CORS headers a browser requires (Navidrome does by default). The desktop app has no CORS ` +
    `rule and can reach servers this build cannot. Original error: ${String(cause)}`
  );
}

/// One call: build the URL in Rust, fetch it, parse it in Rust.
async function call(
  server: StoredServer,
  endpoint: string,
  params: Record<string, string> = {},
): Promise<Parsed & { ok: true }> {
  const bytes = await callBytes(server, endpoint, params);
  const parsed = JSON.parse(subsonicParse(bytes)) as Parsed;
  if (!parsed.ok) throw new SubsonicFailure(parsed.error);
  return parsed;
}

/// The same call, stopping at the bytes -- for the endpoints whose response is
/// read by a different Rust parser (`subsonicRandomSongs`, and from M3 the
/// browse endpoints).
async function callBytes(
  server: StoredServer,
  endpoint: string,
  params: Record<string, string> = {},
): Promise<Uint8Array> {
  await ready();
  const url = subsonicRequestUrl(
    server.baseUrl,
    server.user,
    server.secret,
    server.auth === "apiKey",
    endpoint,
    freshSalt(),
    JSON.stringify(params),
  );
  const response = await get(server, url);
  return new Uint8Array(await response.arrayBuffer());
}

/// One GET, with the browser's own refusals turned into a `SubsonicFailure`
/// that names what to change.
async function get(server: StoredServer, url: string): Promise<Response> {
  let response: Response;
  try {
    // `cors` explicitly, which is also what satisfies this app's own
    // Cross-Origin-Embedder-Policy: require-corp -- a no-cors request would be
    // blocked by our own header before the network saw it.
    response = await fetch(url, { mode: "cors", credentials: "omit" });
  } catch (e) {
    throw new SubsonicFailure({
      kind: "transport",
      code: null,
      message: browserFailure(server.baseUrl, e),
      credentials: false,
    });
  }

  if (!response.ok) {
    // A real HTTP failure means something other than the application talking --
    // a proxy, an auth layer, a wrong path. Subsonic's own failures arrive as
    // 200s and are caught by whichever parser reads the body.
    throw new SubsonicFailure({
      kind: "http",
      code: String(response.status),
      message: `The server answered with HTTP ${response.status}.`,
      credentials: false,
    });
  }
  return response;
}

function stateFrom(server: StoredServer): ServerState {
  return {
    ...emptyServerState(),
    configured: true,
    baseUrl: server.baseUrl,
    user: server.user,
    auth: server.auth,
  };
}

/// A failed attempt, as state.
///
/// `configured` comes from what is *stored*, not from what was just typed:
/// nothing was saved, so a first attempt that fails must not leave the panel
/// believing a server is set up. The typed address and username still travel
/// back, so the form can redisplay them. Mirrors the desktop shell's
/// `connect_server` exactly.
async function failureState(server: StoredServer, e: unknown): Promise<ServerState> {
  const failure = e instanceof SubsonicFailure ? e : null;
  return {
    ...stateFrom(server),
    configured: (await storedServer()) !== null,
    error: failure ? failure.message : String(e),
    errorIsCredentials: failure?.credentials ?? false,
  };
}

/// Pings, then asks what the server can do. Extensions are information, not a
/// requirement: a server that has never heard of them answers with an error
/// envelope, which is "no extensions" rather than a failure to connect.
async function connectTo(server: StoredServer): Promise<ServerState> {
  const ping = await call(server, "ping");
  let supportsApiKey = false;
  try {
    const extensions = await call(server, "getOpenSubsonicExtensions");
    const list = (extensions.payload.openSubsonicExtensions ?? []) as { name: string }[];
    supportsApiKey = list.some((e) => e.name === "apiKeyAuthentication");
  } catch {
    supportsApiKey = false;
  }
  return {
    ...stateFrom(server),
    connected: true,
    serverType: ping.info.serverType,
    serverVersion: ping.info.serverVersion,
    apiVersion: ping.info.version,
    openSubsonic: ping.info.openSubsonic,
    supportsApiKey,
  };
}

/// What is stored, plus whether this session has reached it. Nothing here makes
/// a network call: the panel polls this, and polling a server would be a request
/// per second per open tab.
export async function serverState(): Promise<ServerState> {
  const stored = await storedServer();
  if (!stored) return emptyServerState();
  return connected ?? stateFrom(stored);
}

/// The last good connection state, so `serverState` can answer without asking
/// the network. Cleared when the server is forgotten or a reconnect fails.
let connected: ServerState | null = null;

/// Sets up (or replaces) the server, and connects.
///
/// The secret is stored only after the server has accepted it -- the same rule
/// as the desktop shell, so a typo does not leave a wrong password to be retried
/// at every launch.
export async function connectServer(
  baseUrl: string,
  user: string,
  secret: string,
  auth: "password" | "apiKey",
): Promise<ServerState> {
  const server: StoredServer = { id: CURRENT_SERVER, baseUrl, user, auth, secret };
  try {
    const state = await connectTo(server);
    await putServer(server);
    connected = state;
    return state;
  } catch (e) {
    connected = null;
    return await failureState(server, e);
  }
}

/// Tries the stored settings again -- "it was down, try now".
export async function reconnectServer(): Promise<ServerState> {
  const stored = await storedServer();
  if (!stored) return emptyServerState();
  try {
    connected = await connectTo(stored);
    return connected;
  } catch (e) {
    connected = null;
    return await failureState(stored, e);
  }
}

/// Forgets the server and its secret.
export async function forgetServer(): Promise<ServerState> {
  await deleteServer();
  connected = null;
  return emptyServerState();
}

/// One track on the server, as much of it as playing one needs. Mirrors
/// `ServerTrack` in src-tauri/src/servers.rs field for field, the way
/// `ServerState` does -- and the rows are built by the same Rust either way
/// (`subsonicRandomSongs`), so the two shells cannot disagree about what a
/// response contained.
export interface ServerTrack {
  id: string;
  title: string;
  artist: string;
  album: string;
  suffix: string;
  durationSeconds: number;
}

type ParsedSongs = { ok: true; songs: ServerTrack[] } | { ok: false; error: ParsedError };

/// A handful of tracks at random, so there is something to play before browsing
/// exists (M3). Mirrors `server_random_tracks` on the desktop shell.
export async function randomTracks(count: number): Promise<ServerTrack[]> {
  const server = await requireServer();
  const bytes = await callBytes(server, "getRandomSongs", { size: String(count) });
  const parsed = JSON.parse(subsonicRandomSongs(bytes)) as ParsedSongs;
  if (!parsed.ok) throw new SubsonicFailure(parsed.error);
  return parsed.songs;
}

/// Most of a track this build will hold in memory at once.
///
/// The same bound the desktop shell puts on a server that ignores range
/// requests and sends the whole file (`MAX_WHOLE_BYTES` in
/// player-core/src/remote.rs), for the same reason: past some size, refusing
/// with a sentence is kinder than an allocation that takes the tab down. It is
/// far above any real track -- an hour of stereo FLAC is well under it.
const MAX_TRACK_BYTES = 512 * 1024 * 1024;

/// A server track's encoded bytes, whole.
///
/// This is where web parity stops being free, and the limit is stated rather
/// than hidden (decision 5). The desktop shell streams a track through
/// `RemoteSource`: 512KB at a time, with seeks turning into range requests, so
/// the memory a track costs does not depend on its length. A browser has no
/// equivalent -- the worker decodes from a byte array it owns (worker.ts), and
/// there is no `MediaSource` for it to pull through -- so the whole track is
/// fetched before it plays. That costs the wait at the start of a track and the
/// track's size in memory, and buys the same behaviour everywhere else: seeking
/// is instant afterwards, and a network that stalls mid-track cannot underrun
/// the ring, because there is no network left to stall.
///
/// Note this is the same thing the web build already does for a local file --
/// `File.arrayBuffer()` in playerEngine.ts -- so it is not a new cost on this
/// shell, only one the desktop shell no longer pays.
export async function trackBytes(id: string): Promise<Uint8Array> {
  const server = await requireServer();
  await ready();
  const url = subsonicStreamUrl(
    server.baseUrl,
    server.user,
    server.secret,
    server.auth === "apiKey",
    freshSalt(),
    id,
  );
  const response = await get(server, url);

  // Checked before reading when the server states it, so an implausible
  // response is refused rather than buffered and then refused.
  const stated = Number(response.headers.get("content-length") ?? "");
  if (Number.isFinite(stated) && stated > MAX_TRACK_BYTES) {
    throw new SubsonicFailure({
      kind: "transport",
      code: null,
      message: `That track is ${(stated / 1024 / 1024).toFixed(0)}MB, which this build will not hold in memory. The desktop app streams it instead.`,
      credentials: false,
    });
  }

  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength > MAX_TRACK_BYTES) {
    throw new SubsonicFailure({
      kind: "transport",
      code: null,
      message: "That track is too large for this build to hold in memory. The desktop app streams it instead.",
      credentials: false,
    });
  }
  // A server that answers `stream` with an error envelope sends JSON, not
  // audio, and it arrives as an HTTP 200 like every other Subsonic failure. The
  // decoder would report it as an unsupported format, which is true and
  // useless; parsing it here means the server's own words reach the user.
  const failure = audioFailure(bytes);
  if (failure) throw failure;
  return bytes;
}

/// A `stream` response that is a Subsonic error rather than audio, if that is
/// what it is. Cheap: only bodies small enough and shaped like JSON are parsed.
function audioFailure(bytes: Uint8Array): SubsonicFailure | null {
  if (bytes.byteLength > 64 * 1024 || bytes[0] !== 0x7b /* { */) return null;
  const parsed = JSON.parse(subsonicParse(bytes)) as Parsed;
  return parsed.ok ? null : new SubsonicFailure(parsed.error);
}

async function requireServer(): Promise<StoredServer> {
  const server = await storedServer();
  if (!server) throw new Error("no music server is configured");
  return server;
}

/// Exposed for the harnesses at /subsonic.html and /remote.html, which drive the
/// same path the UI does against a server the developer names.
export const testing = { call, callBytes, connectTo, freshSalt };
