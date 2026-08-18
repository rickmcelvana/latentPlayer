// The gallery, and the host's controls around it (Phase 3 M1).
//
// One component for two very different hosts: natively these buttons drive a
// separate OS window, on the web they drive a canvas in this page. What the
// user is doing is the same in both -- pick a visual, size it, put it away,
// give it the whole screen -- which is the argument for the adapter underneath
// (visualizer.ts) rather than two components that happen to look alike.
import { useEffect, useState } from "react";
import {
  DEFAULT_VISUALIZER_SIZE,
  VISUALIZER_SIZES,
  gainCatalog,
  setGain,
  setVisual,
  setVisualizerFullscreen,
  setVisualizerSize,
  setVisualizerVisible,
  storedGain,
  storedVisual,
  visualCatalog,
  visualizerState,
  type GainInfo,
  type VisualInfo,
} from "./visualizer";

export function VisualizerControls() {
  const [catalog, setCatalog] = useState<VisualInfo[]>([]);
  const [visual, setVisualId] = useState<string>("");
  const [size, setSize] = useState<number>(DEFAULT_VISUALIZER_SIZE);
  const [visible, setVisible] = useState(true);
  const [fullscreen, setFullscreen] = useState(false);
  const [gains, setGains] = useState<GainInfo[]>([]);
  const [gain, setGainId] = useState<string>("");
  const [gainIsMeasurement, setGainIsMeasurement] = useState(true);
  /// Why fullscreen did not happen, when it did not. Cleared on the next try.
  const [refused, setRefused] = useState<string | null>(null);

  // The gallery, and the selection this session opens on. Both come from Rust:
  // the list is `VisualKind::ALL` (decision 3) and the stored id is only
  // honoured if this build still has a visual by that name -- otherwise the
  // registry's own first entry wins rather than something silently different.
  useEffect(() => {
    let cancelled = false;
    visualCatalog()
      .then((entries) => {
        if (cancelled || entries.length === 0) return;
        setCatalog(entries);
        const chosen = storedVisual(entries) ?? entries[0].id;
        setVisualId(chosen);
        void setVisual(chosen).catch(() => {});
      })
      .catch(() => {
        // A gallery we can't list is a visualizer with working size and
        // visibility buttons, which is still worth having on screen.
      });
    gainCatalog()
      .then((entries) => {
        if (cancelled || entries.length === 0) return;
        setGains(entries);
        const chosen = storedGain(entries) ?? entries[0].id;
        setGainId(chosen);
        void setGain(chosen).catch(() => {});
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // The UI is not the only party that can change this. Natively the visualizer
  // window has edges the user can drag and a close button of its own, which
  // hides it (`VisualizerWindow` in src-tauri/src/lib.rs); and since M1 both
  // shells can leave fullscreen without this panel being involved at all --
  // Esc, the browser's own control, or the window losing focus. So this is
  // polled rather than read once at mount and assumed to stay true: a panel
  // whose button says "Hide" about a window the user just closed takes two
  // presses to bring it back, and looks broken doing it. A second is cheap and
  // is well inside how long anyone takes to look back at this panel.
  useEffect(() => {
    let cancelled = false;
    const read = () => {
      visualizerState()
        .then((state) => {
          if (cancelled) return;
          setSize(state.width);
          setVisible(state.visible);
          setFullscreen(state.fullscreen);
          if (state.visual) setVisualId(state.visual);
          if (state.gain) setGainId(state.gain);
          setGainIsMeasurement(state.gainIsMeasurement);
        })
        .catch(() => {
          // A visualizer we can't ask about is still one with working buttons.
        });
    };
    read();
    const id = setInterval(read, 1000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  const choose = (next: number) => {
    setSize(next);
    void setVisualizerSize(next, next).catch(() => {});
  };

  const chooseVisual = (id: string) => {
    setVisualId(id);
    void setVisual(id).catch(() => {});
  };

  const chooseGain = (id: string) => {
    setGainId(id);
    void setGain(id).catch(() => {});
  };

  const toggle = () => {
    const next = !visible;
    setVisible(next);
    void setVisualizerVisible(next).catch(() => {});
  };

  const toggleFullscreen = () => {
    const next = !fullscreen;
    // Not applied optimistically, unlike the others: entering fullscreen on the
    // web needs a user gesture and can be refused outright -- a page inside an
    // iframe without `allow="fullscreen"` is told "Permissions check failed" --
    // and a button that has already redrawn itself as "Exit fullscreen" over a
    // window that never went fullscreen is a button that now does the wrong
    // thing. The poll above corrects it either way within the second; this just
    // avoids being wrong on purpose.
    setRefused(null);
    void setVisualizerFullscreen(next)
      .then(() => setFullscreen(next))
      .catch((e) => {
        // A refusal is the one failure here a person can act on -- and the only
        // one that leaves a pressed button looking broken -- so it is said out
        // loud rather than swallowed like the rest.
        setRefused(next ? String(e) : null);
      });
  };

  return (
    <div className="visualizer-controls">
      {catalog.length > 0 && (
        <div className="gallery">
          <span className="label">Visual</span>
          {catalog.map((entry) => (
            <button
              key={entry.id}
              type="button"
              className={entry.id === visual ? "visual current" : "visual"}
              title={entry.description}
              onClick={() => chooseVisual(entry.id)}
            >
              {entry.name}
            </button>
          ))}
        </div>
      )}
      {gains.length > 0 && (
        <div className="gain">
          <span className="label">Gain</span>
          {gains.map((entry) => (
            <button
              key={entry.id}
              type="button"
              className={entry.id === gain ? "step current" : "step"}
              onClick={() => chooseGain(entry.id)}
            >
              {entry.label}
            </button>
          ))}
          {/* Said out loud, because a scope that has been scaled without saying
              so is worse than one that cannot scale at all. Unity is the only
              setting where the figure's size still means the signal's level,
              and which settings those are is Rust's to say, not this
              component's. */}
          {!gainIsMeasurement && (
            <span className="hint scaled">display only -- not a level reading</span>
          )}
        </div>
      )}
      <div className="host">
        <span className="label">Size</span>
        {VISUALIZER_SIZES.map((option) => (
          <button
            key={option}
            type="button"
            className={option === size ? "size current" : "size"}
            onClick={() => choose(option)}
          >
            {option}
          </button>
        ))}
        <button type="button" onClick={toggle}>
          {visible ? "Hide" : "Show"}
        </button>
        <button type="button" onClick={toggleFullscreen}>
          {fullscreen ? "Exit fullscreen" : "Fullscreen"}
        </button>
        {fullscreen && <span className="hint">Esc to come back</span>}
        {refused && <span className="hint refused">Fullscreen refused: {refused}</span>}
      </div>
    </div>
  );
}
