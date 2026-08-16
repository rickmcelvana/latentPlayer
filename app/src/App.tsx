import { useCallback, useEffect, useRef, useState } from "react";
import "./App.css";
import { isTauri } from "./isTauri";
import { pickAudioFiles } from "./nativePlayer";
import {
  loadTrack,
  queueTrack,
  readTransport,
  seek,
  setPlaying,
  setVolume,
  trackFromFile,
  trackFromPath,
  type PlaylistTrack,
  type TransportState,
} from "./playlist";
import { Spectrum } from "./Spectrum";
import { startScope } from "./webScope";

const EMPTY: TransportState = {
  position: 0,
  duration: 0,
  playing: false,
  volume: 1,
  finished: false,
  trackIndex: 0,
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
  const [tracks, setTracks] = useState<PlaylistTrack[]>([]);
  const [current, setCurrent] = useState<number | null>(null);
  const [state, setState] = useState<TransportState>(EMPTY);
  // Whether a load has finished. The row highlights the moment it's clicked,
  // which is well before the engine has the track open, and queuing the
  // *next* one into a load that hasn't landed yet loses it: both shells clear
  // the next slot as part of opening a track (`Playback::open` builds a fresh
  // engine, worker.ts's `load` sets `queued = null`), so a queue message that
  // overtakes the load is discarded. Found live: the second track never
  // played.
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // While the user drags the seek bar, the slider follows the pointer rather
  // than the poll -- otherwise every poll yanks the thumb back to the real
  // play position mid-drag.
  const [scrubbing, setScrubbing] = useState<number | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const scopeStarted = useRef(false);
  // The playlist index that was last handed to `loadTrack`. The engine's
  // `trackIndex` counts hand-offs since then, so the track actually coming
  // out of the speakers is the sum of the two.
  const loadedIndex = useRef(0);
  // The id of the track currently sitting in the engine's next slot, so the
  // queue is only rewritten when it should actually change. Null after a load,
  // which clears that slot on both shells.
  const queuedId = useRef<number | null>(null);

  const tauri = isTauri();
  const loaded = current !== null;

  // Web build only: the scope canvas lives inline in this window (no
  // second native window to put it in -- see PLAN.md §3/§8.3, web is the
  // player; a second always-on wgpu window isn't a thing a browser tab has).
  useEffect(() => {
    if (tauri || !canvasRef.current || scopeStarted.current) return;
    scopeStarted.current = true;
    startScope(canvasRef.current).catch((e) => setError(String(e)));
  }, [tauri]);

  const play = useCallback(async (index: number, list: PlaylistTrack[]) => {
    const track = list[index];
    if (!track) return;
    setError(null);
    setCurrent(index);
    setReady(false);
    loadedIndex.current = index;
    queuedId.current = null;
    try {
      setState(await loadTrack(track));
      setReady(true);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // One poll drives the seek bar, the clock, the play/pause button *and*
  // which row is highlighted. The last of those is why `trackIndex` travels
  // in the transport snapshot: a hand-off is only real once the audio reaches
  // the output, and this is where the UI learns it did.
  useEffect(() => {
    if (!loaded) return;
    const id = setInterval(() => {
      readTransport()
        .then((next) => {
          setState(next);
          const playing = loadedIndex.current + next.trackIndex;
          setCurrent((shown) => (playing !== shown && playing < tracks.length ? playing : shown));
        })
        .catch((e) => setError(String(e)));
    }, 100);
    return () => clearInterval(id);
  }, [loaded, tracks.length]);

  // Keep the engine's one-deep lookahead pointed at whatever follows the
  // track being heard. Queuing early is the point -- the decoder picks the
  // next track up when it runs out of this one, so the seam is filled well
  // before the device reaches it.
  useEffect(() => {
    if (current === null || !ready) return;
    const next = tracks[current + 1] ?? null;
    const id = next?.id ?? null;
    if (queuedId.current === id) return;
    queuedId.current = id;
    queueTrack(next).catch((e) => setError(String(e)));
  }, [current, ready, tracks]);

  const add = useCallback(
    (added: PlaylistTrack[]) => {
      if (added.length === 0) return;
      const list = [...tracks, ...added];
      setTracks(list);
      // An empty playlist starts playing; adding to a live one just extends
      // it, and the queue effect picks up the new tail if it's next.
      if (current === null) void play(list.length - added.length, list);
    },
    [current, play, tracks],
  );

  async function handleOpenNative() {
    setError(null);
    try {
      add((await pickAudioFiles()).map(trackFromPath));
    } catch (e) {
      setError(String(e));
    }
  }

  function handleOpenWeb(e: React.ChangeEvent<HTMLInputElement>) {
    setError(null);
    add(Array.from(e.target.files ?? []).map(trackFromFile));
    // Let the same file be picked again after it's been removed from the list.
    e.target.value = "";
  }

  async function togglePlaying() {
    try {
      setState(await setPlaying(!state.playing));
    } catch (e) {
      setError(String(e));
    }
  }

  /// Jump to another track by hand. Deliberately not gapless: the listener
  /// asked for it now, so it replaces what's playing rather than waiting for
  /// the seam.
  function skip(delta: number) {
    if (current === null) return;
    const index = current + delta;
    if (index < 0 || index >= tracks.length) return;
    void play(index, tracks);
  }

  function remove(index: number) {
    const list = tracks.filter((_, i) => i !== index);
    setTracks(list);
    if (current === null) return;
    if (index === current) {
      // Removing what's playing plays what fell into its place, or stops if
      // that was the end of the list.
      if (index < list.length) void play(index, list);
      else {
        setCurrent(null);
        setReady(false);
        setState(EMPTY);
        void setPlaying(false);
      }
    } else if (index < current) {
      // Everything after the hole shifted down, including the loaded index
      // the hand-off counter is measured from.
      setCurrent(current - 1);
      loadedIndex.current -= 1;
    }
  }

  async function commitSeek(seconds: number) {
    setScrubbing(null);
    try {
      setState(await seek(seconds));
    } catch (e) {
      setError(String(e));
    }
  }

  async function changeVolume(volume: number) {
    setState((s) => ({ ...s, volume }));
    try {
      await setVolume(volume);
    } catch (e) {
      setError(String(e));
    }
  }

  const shown = scrubbing ?? state.position;

  return (
    <main className="player">
      <h1>latentPlayer</h1>
      <p className="subtitle">Phase 1 &mdash; queue</p>

      <div className="controls">
        {tauri ? (
          <button type="button" onClick={handleOpenNative}>
            Add files&hellip;
          </button>
        ) : (
          <label className="file-input">
            Add files&hellip;
            <input type="file" accept="audio/*" multiple onChange={handleOpenWeb} />
          </label>
        )}

        {current !== null && <p className="now-playing">{tracks[current]?.name}</p>}

        <div className="transport">
          <button type="button" onClick={() => skip(-1)} disabled={!loaded || current === 0}>
            Prev
          </button>
          <button type="button" onClick={togglePlaying} disabled={!loaded}>
            {state.playing ? "Pause" : "Play"}
          </button>
          <button
            type="button"
            onClick={() => skip(1)}
            disabled={!loaded || current === tracks.length - 1}
          >
            Next
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

        <Spectrum active={loaded} />

        {tracks.length > 0 && (
          <ol className="playlist">
            {tracks.map((track, index) => (
              <li key={track.id} className={index === current ? "playing" : undefined}>
                <button type="button" className="row" onClick={() => void play(index, tracks)}>
                  <span className="num">{index + 1}</span>
                  <span className="name">{track.name}</span>
                </button>
                <button
                  type="button"
                  className="remove"
                  aria-label={`Remove ${track.name}`}
                  onClick={() => remove(index)}
                >
                  &times;
                </button>
              </li>
            ))}
          </ol>
        )}

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
