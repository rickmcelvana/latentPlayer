import { useCallback, useEffect, useRef, useState } from "react";
import "./App.css";
import { isTauri } from "./isTauri";
import {
  loadFile as loadFileNative,
  pickAudioFile,
  seek as seekNative,
  setPlaying as setPlayingNative,
  setVolume as setVolumeNative,
  transport as transportNative,
  type TransportState,
} from "./nativePlayer";
import {
  loadFile as loadFileWeb,
  pause as pauseWeb,
  play as playWeb,
  seek as seekWeb,
  setVolume as setVolumeWeb,
  transport as transportWeb,
} from "./playerEngine";
import { startScope } from "./webScope";

const EMPTY: TransportState = {
  position: 0,
  duration: 0,
  playing: false,
  volume: 1,
  finished: false,
};

/// mm:ss. Negative/NaN durations collapse to 0:00 rather than rendering
/// "NaN:NaN" while nothing is loaded.
function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const total = Math.floor(seconds);
  const mins = Math.floor(total / 60);
  const secs = total % 60;
  return `${mins}:${secs.toString().padStart(2, "0")}`;
}

function App() {
  const [fileName, setFileName] = useState<string | null>(null);
  const [state, setState] = useState<TransportState>(EMPTY);
  const [error, setError] = useState<string | null>(null);
  // While the user drags the seek bar, the slider follows the pointer rather
  // than the poll -- otherwise every poll yanks the thumb back to the real
  // play position mid-drag.
  const [scrubbing, setScrubbing] = useState<number | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const scopeStarted = useRef(false);

  const tauri = isTauri();
  const loaded = fileName !== null;

  // Web build only: the scope canvas lives inline in this window (no
  // second native window to put it in -- see PLAN.md §3/§8.3, web is the
  // player; a second always-on wgpu window isn't a thing a browser tab has).
  useEffect(() => {
    if (tauri || !canvasRef.current || scopeStarted.current) return;
    scopeStarted.current = true;
    startScope(canvasRef.current).catch((e) => setError(String(e)));
  }, [tauri]);

  const readTransport = useCallback(
    async () => (tauri ? await transportNative() : transportWeb()),
    [tauri],
  );

  // One poll drives the seek bar, the clock and the play/pause button. 10Hz
  // is plenty for a clock reading whole seconds and keeps the Tauri IPC
  // round trips modest; the scope has its own 60Hz path and doesn't come
  // through here.
  useEffect(() => {
    if (!loaded) return;
    const id = setInterval(() => {
      readTransport()
        .then(setState)
        .catch((e) => setError(String(e)));
    }, 100);
    return () => clearInterval(id);
  }, [loaded, readTransport]);

  async function handleOpenNative() {
    setError(null);
    try {
      const path = await pickAudioFile();
      if (!path) return;
      setState(await loadFileNative(path));
      setFileName(path.split(/[\\/]/).pop() ?? path);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleOpenWeb(e: React.ChangeEvent<HTMLInputElement>) {
    setError(null);
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      setState(await loadFileWeb(file));
      setFileName(file.name);
      await playWeb();
      setState(transportWeb());
    } catch (err) {
      setError(String(err));
    }
  }

  async function togglePlaying() {
    const next = !state.playing;
    try {
      if (tauri) {
        await setPlayingNative(next);
        setState(await transportNative());
      } else {
        if (next) await playWeb();
        else pauseWeb();
        setState(transportWeb());
      }
    } catch (e) {
      setError(String(e));
    }
  }

  async function commitSeek(seconds: number) {
    setScrubbing(null);
    try {
      setState(tauri ? await seekNative(seconds) : seekWeb(seconds));
    } catch (e) {
      setError(String(e));
    }
  }

  async function changeVolume(volume: number) {
    setState((s) => ({ ...s, volume }));
    try {
      if (tauri) await setVolumeNative(volume);
      else setVolumeWeb(volume);
    } catch (e) {
      setError(String(e));
    }
  }

  const shown = scrubbing ?? state.position;

  return (
    <main className="player">
      <h1>latentPlayer</h1>
      <p className="subtitle">Phase 1 &mdash; transport</p>

      <div className="controls">
        {tauri ? (
          <button type="button" onClick={handleOpenNative}>
            Open file&hellip;
          </button>
        ) : (
          <label className="file-input">
            Open file&hellip;
            <input type="file" accept="audio/*" onChange={handleOpenWeb} />
          </label>
        )}

        {fileName && <p className="now-playing">{fileName}</p>}

        <div className="transport">
          <button type="button" onClick={togglePlaying} disabled={!loaded}>
            {state.playing ? "Pause" : "Play"}
          </button>
          <span className="time">{formatTime(shown)}</span>
          <input
            className="seek"
            type="range"
            min={0}
            max={Math.max(state.duration, 0.001)}
            step={0.01}
            value={Math.min(shown, state.duration)}
            disabled={!loaded}
            onChange={(e) => setScrubbing(Number(e.target.value))}
            onPointerUp={(e) => void commitSeek(Number(e.currentTarget.value))}
            onKeyUp={(e) => void commitSeek(Number(e.currentTarget.value))}
          />
          <span className="time">{formatTime(state.duration)}</span>
        </div>

        <div className="volume">
          <label htmlFor="volume">Volume</label>
          <input
            id="volume"
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={state.volume}
            onChange={(e) => void changeVolume(Number(e.target.value))}
          />
        </div>

        {error && <p className="error">{error}</p>}

        {tauri ? (
          <p className="hint">
            The live 2D scope is in the second window this app opened alongside this one.
          </p>
        ) : (
          <canvas ref={canvasRef} width={480} height={480} className="scope" />
        )}
      </div>
    </main>
  );
}

export default App;
