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
  serverState,
  type ServerState,
} from "./server";

export function ServerPanel() {
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
  useEffect(() => {
    let cancelled = false;
    serverState()
      .then((next) => {
        if (cancelled) return;
        setState(next);
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
  }, []);

  const apiKeyMode = state.supportsApiKey && state.auth === "apiKey";

  async function run(work: () => Promise<ServerState>) {
    setBusy(true);
    try {
      const next = await work();
      setState(next);
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
      setState((current) => ({ ...current, error: String(e), errorIsCredentials: false }));
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
