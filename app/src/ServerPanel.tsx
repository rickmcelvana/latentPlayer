// Setting up a music server, on both shells (Phase 2 M1).
//
// One component for two very different stores -- an OS credential store on the
// desktop, IndexedDB in the browser -- for ScopeControls' reason: what the user
// is doing is the same in both, so the difference belongs in the adapter
// (server.ts) rather than in two components that look alike.
//
// The panel's job beyond collecting three fields is to say what went wrong in
// terms of the thing to change. A Subsonic failure is not an exception here: a
// wrong password comes back as state, and the panel puts the cursor back in the
// password field rather than showing a red string and leaving the user to guess
// whether the address or the account is at fault.
import { useEffect, useState } from "react";
import {
  connectServer,
  emptyServerState,
  forgetServer,
  reconnectServer,
  secretStorageNote,
  serverRandomTracks,
  serverState,
  type ServerState,
} from "./server";
import { trackFromServer, type PlaylistTrack } from "./playlist";
import { isTauri } from "./isTauri";

/// How many tracks "Play random tracks" asks for. Enough that the gapless
/// hand-off between two *server* tracks actually gets exercised by pressing the
/// button, which is the point of the control existing this early.
const RANDOM_COUNT = 10;

/// What to go and do about a failure, per kind (M5 decision 17).
///
/// The certificate case is the one worth spelling out. Self-signed certificates
/// are the norm on a self-hosted box, and this app now verifies against the
/// **OS** trust store (decision 16) rather than a bundle compiled into the
/// binary -- which is what makes "add it to your system trust store" real advice
/// rather than a sentence that changes nothing. The web build gets a different
/// half of the same answer: a browser has its own store and its own way of being
/// asked, and visiting the server once is how you ask it.
function advice(kind: string): string {
  switch (kind) {
    case "certificate":
      return isTauri()
        ? "Self-signed certificates are normal on a self-hosted server. Add its certificate (or its CA) to this machine's trust store and reconnect -- this app trusts what the OS trusts. A certificate from Let's Encrypt is the other way out."
        : "Open the server's address in a browser tab first and accept its certificate there; this page cannot ask on your behalf. A certificate from Let's Encrypt is the other way out.";
    case "unreachable":
      return "Check the address and that the server is running. A server on another machine also has to be reachable from this one.";
    case "credentials":
      return "The server did not accept this username and password.";
    case "address":
      return "";
    case "server":
      return "The server answered, and refused. Its own words are above.";
    case "malformed":
      return "The answer did not look like a Subsonic server's. Check that the address points at the server itself rather than at a page in front of it.";
    default:
      return "";
  }
}

interface Props {
  /// Replaces the queue with these and starts at `index` -- the same callback
  /// the library panel gets, because "play this" means the same thing wherever
  /// the rows came from.
  onPlay: (tracks: PlaylistTrack[], index: number) => void;
  /// Told whenever the connection's state changes, so the app can offer the
  /// library panel's server tab the moment there is a server to browse rather
  /// than at the next reload (M3).
  onState: (state: ServerState) => void;
}

export function ServerPanel({ onPlay, onState }: Props) {
  const [state, setState] = useState<ServerState>(emptyServerState());
  const [baseUrl, setBaseUrl] = useState("");
  const [user, setUser] = useState("");
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);
  // Whether the form is up. Held rather than derived from `configured`: a failed
  // attempt has to leave the form open with the typed values still in it, and
  // deriving "show the form when nothing is configured" closed it the moment an
  // attempt came back -- which made a rejected address look like a saved server
  // (found by driving the panel in a browser, not by reading it).
  const [showForm, setShowForm] = useState(true);

  // Read once at mount. Not polled, unlike the scope's geometry: nothing else
  // changes this, and polling it would mean a network round trip per second on
  // the web build.
  //
  // The desktop shell reconnects a stored server at startup, off the main
  // thread, so this can land before that finishes -- which is why the panel's
  // Check button exists and why `onState` is called from `run` too.
  useEffect(() => {
    let cancelled = false;
    serverState()
      .then((next) => {
        if (cancelled) return;
        setState(next);
        onState(next);
        setBaseUrl(next.baseUrl);
        setUser(next.user);
        setShowForm(!next.configured);
      })
      .catch(() => {
        // A server we cannot ask about is still a panel that can add one.
      });
    return () => {
      cancelled = true;
    };
    // Mount only: `onState` is a callback the app holds steady, and re-running
    // this on every render would be a request per render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const apiKeyMode = state.supportsApiKey && state.auth === "apiKey";

  /// Pulls a few tracks off the server and plays them (Phase 2 M2).
  ///
  /// Random rather than browsed, because browsing is M3 and this milestone
  /// still has to be something a person can press: it is the shortest path from
  /// a connected server to sound coming out, and it is what makes the
  /// remote path testable by hand on both shells.
  async function playRandom() {
    setBusy(true);
    setState((current) => ({ ...current, error: "" }));
    try {
      const tracks = await serverRandomTracks(RANDOM_COUNT);
      if (tracks.length === 0) {
        setState((current) => ({ ...current, error: "The server returned no tracks to play." }));
        return;
      }
      onPlay(tracks.map(trackFromServer), 0);
    } catch (e) {
      setState((current) => ({
        ...current,
        error: String(e),
        errorIsCredentials: false,
        errorKind: "",
      }));
    } finally {
      setBusy(false);
    }
  }

  async function run(work: () => Promise<ServerState>) {
    setBusy(true);
    try {
      const next = await work();
      setState(next);
      onState(next);
      setBaseUrl(next.baseUrl || baseUrl);
      setUser(next.user || user);
      if (next.connected) {
        setShowForm(false);
        // The secret has been accepted and stored; there is no reason for this
        // component to keep holding it.
        setSecret("");
      } else if (!next.configured) {
        // Nothing is stored, so the form is still the thing to be looking at.
        setShowForm(true);
      }
    } catch (e) {
      setState((current) => ({
        ...current,
        error: String(e),
        errorIsCredentials: false,
        errorKind: "",
      }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="server-panel">
      <div className="server-header">
        <span className="label">Music server</span>
        {state.configured && !showForm && (
          <span className={state.connected ? "server-status connected" : "server-status"}>
            {state.connected
              ? `${state.serverType || "connected"} ${state.serverVersion}`.trim()
              : "not connected"}
          </span>
        )}
      </div>

      {state.configured && !showForm && (
        <>
          <p className="server-address">
            {state.baseUrl}
            {state.user && <span className="server-user"> as {state.user}</span>}
          </p>
          {state.connected && (
            <p className="server-detail">
              Subsonic API {state.apiVersion}
              {state.openSubsonic ? ", OpenSubsonic" : ""}
              {state.supportsApiKey ? ", API keys supported" : ""}
            </p>
          )}
          <div className="server-actions">
            {state.connected && (
              <button type="button" onClick={() => void playRandom()} disabled={busy}>
                Play random tracks
              </button>
            )}
            <button type="button" onClick={() => void run(reconnectServer)} disabled={busy}>
              {state.connected ? "Check" : "Reconnect"}
            </button>
            <button type="button" onClick={() => setShowForm(true)} disabled={busy}>
              Edit
            </button>
            <button type="button" onClick={() => void run(forgetServer)} disabled={busy}>
              Forget
            </button>
          </div>
        </>
      )}

      {showForm && (
        <form
          className="server-form"
          onSubmit={(e) => {
            e.preventDefault();
            void run(() =>
              connectServer(baseUrl.trim(), user.trim(), secret, apiKeyMode ? "apiKey" : "password"),
            );
          }}
        >
          <label>
            Address
            <input
              type="url"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="http://localhost:4533"
              autoComplete="off"
              required
            />
          </label>
          {!apiKeyMode && (
            <label>
              Username
              <input
                type="text"
                value={user}
                onChange={(e) => setUser(e.target.value)}
                autoComplete="off"
                required
              />
            </label>
          )}
          <label>
            {apiKeyMode ? "API key" : "Password"}
            <input
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              autoComplete="off"
              required
            />
          </label>
          {/* Said where the secret is typed, not in a settings page nobody
              opens: the two shells genuinely differ in what they can promise. */}
          <p className="server-note">{secretStorageNote()}</p>
          <div className="server-actions">
            <button type="submit" disabled={busy}>
              {busy ? "Connecting…" : "Connect"}
            </button>
            {state.configured && (
              <button type="button" onClick={() => setShowForm(false)} disabled={busy}>
                Cancel
              </button>
            )}
          </div>
        </form>
      )}

      {state.error && (
        <p className="server-error">
          {state.error}
          {/* What to do about it, which until M5 was one sentence for three
              entirely different problems. The kind is decided in Rust on both
              shells (decision 17) so this never reads message text. */}
          {advice(state.errorKind) && <span className="server-advice">{advice(state.errorKind)}</span>}
          {state.errorIsCredentials && !showForm && (
            <button type="button" className="link" onClick={() => setShowForm(true)}>
              Change the password
            </button>
          )}
        </p>
      )}
    </div>
  );
}
