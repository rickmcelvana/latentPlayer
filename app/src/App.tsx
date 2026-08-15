import { useEffect, useRef, useState } from "react";
import "./App.css";
import { isTauri } from "./isTauri";
import { loadFile as loadFileNative, pickAudioFile, setPlaying as setPlayingNative } from "./nativePlayer";
import { loadFile as loadFileWeb, pause as pauseWeb, play as playWeb } from "./playerEngine";
import { startScope } from "./webScope";

function App() {
  const [fileName, setFileName] = useState<string | null>(null);
  const [duration, setDuration] = useState<number | null>(null);
  const [playing, setPlayingState] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const scopeStarted = useRef(false);

  const tauri = isTauri();

  // Web build only: the scope canvas lives inline in this window (no
  // second native window to put it in -- see PLAN.md §3/§8.3, web is the
  // player; a second always-on wgpu window isn't a thing a browser tab has).
  useEffect(() => {
    if (tauri || !canvasRef.current || scopeStarted.current) return;
    scopeStarted.current = true;
    startScope(canvasRef.current).catch((e) => setError(String(e)));
  }, [tauri]);

  async function handleOpenNative() {
    setError(null);
    try {
      const path = await pickAudioFile();
      if (!path) return;
      const seconds = await loadFileNative(path);
      setFileName(path.split(/[\\/]/).pop() ?? path);
      setDuration(seconds);
      setPlayingState(true);
    } catch (e) {
      setError(String(e));
    }
  }

  async function togglePlayingNative() {
    const next = !playing;
    await setPlayingNative(next);
    setPlayingState(next);
  }

  async function handleOpenWeb(e: React.ChangeEvent<HTMLInputElement>) {
    setError(null);
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const seconds = await loadFileWeb(file);
      setFileName(file.name);
      setDuration(seconds);
      await playWeb();
      setPlayingState(true);
    } catch (err) {
      setError(String(err));
    }
  }

  async function togglePlayingWeb() {
    if (playing) {
      pauseWeb();
      setPlayingState(false);
    } else {
      await playWeb();
      setPlayingState(true);
    }
  }

  return (
    <main className="player">
      <h1>latentPlayer</h1>
      <p className="subtitle">Phase 0 walking skeleton</p>

      {tauri ? (
        <div className="controls">
          <button type="button" onClick={handleOpenNative}>
            Open file&hellip;
          </button>
          <button type="button" onClick={togglePlayingNative} disabled={!fileName}>
            {playing ? "Pause" : "Play"}
          </button>
          {fileName && (
            <p className="now-playing">
              {fileName}
              {duration != null && ` (${duration.toFixed(1)}s)`}
            </p>
          )}
          {error && <p className="error">{error}</p>}
          <p className="hint">
            The live 2D scope is in the second window this app opened alongside this one.
          </p>
        </div>
      ) : (
        <div className="controls">
          <label className="file-input">
            Open file&hellip;
            <input type="file" accept="audio/*" onChange={handleOpenWeb} />
          </label>
          <button type="button" onClick={togglePlayingWeb} disabled={!fileName}>
            {playing ? "Pause" : "Play"}
          </button>
          {fileName && (
            <p className="now-playing">
              {fileName}
              {duration != null && ` (${duration.toFixed(1)}s)`}
            </p>
          )}
          {error && <p className="error">{error}</p>}
          <canvas ref={canvasRef} width={480} height={480} className="scope" />
        </div>
      )}
    </main>
  );
}

export default App;
