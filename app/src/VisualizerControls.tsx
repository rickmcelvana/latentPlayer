// The gallery, and the host's controls around it (Phase 3 M1).
//
// One component for two very different hosts: natively these buttons drive a
// separate OS window, on the web they drive a canvas in this page. What the
// user is doing is the same in both -- pick a visual, size it, put it away,
// give it the whole screen -- which is the argument for the adapter underneath
// (visualizer.ts) rather than two components that happen to look alike.
import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { isTauri } from "./isTauri";
import {
  DEFAULT_VISUALIZER_SIZE,
  VISUALIZER_SIZES,
  gainCatalog,
  setGain,
  setMilkPreset,
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
  /// The selected MilkDrop preset's name, and what it needs that this build
  /// does not do (Phase 3 M7a, decisions 49 and 56).
  const [preset, setPreset] = useState<string | null>(null);
  const [presetReport, setPresetReport] = useState<string[]>([]);
  /// Why a preset would not load. A file that is not a preset is the one
  /// failure here a person can act on, so it is said rather than swallowed.
  const [presetError, setPresetError] = useState<string | null>(null);
  /// The web shell's file input. Hidden, and clicked from the button, because
  /// a bare `<input type="file">` cannot be styled like the rest of the panel.
  const fileInput = useRef<HTMLInputElement | null>(null);
  const milkdropShowing = visual === "milkdrop";

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
          setPreset(state.preset);
          setPresetReport(state.presetReport);
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

  /// Picks a `.milk` file, in whichever way this shell picks files.
  ///
  /// Two mechanisms because there is no one mechanism: the desktop shell opens
  /// a native dialog and gets a path, the browser opens a file input and gets a
  /// `File`. Both hand `visualizer.ts` something with a name and a way to reach
  /// the bytes, and it is the last place in the app that knows the difference.
  const choosePreset = async () => {
    setPresetError(null);
    try {
      if (isTauri()) {
        const picked = await open({
          multiple: false,
          filters: [{ name: "MilkDrop preset", extensions: ["milk"] }],
        });
        if (typeof picked !== "string") return;
        const name = picked.split(/[\\/]/).pop() ?? picked;
        const report = await setMilkPreset({ name, path: picked, text: async () => "" });
        setPreset(name);
        setPresetReport(report);
      } else {
        fileInput.current?.click();
      }
    } catch (e) {
      setPresetError(String(e));
    }
  };

  const onFilePicked = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    // Cleared so picking the same file twice in a row fires again -- which is
    // exactly what someone editing a preset does.
    event.target.value = "";
    if (!file) return;
    setPresetError(null);
    try {
      const report = await setMilkPreset({ name: file.name, text: () => file.text() });
      setPreset(file.name);
      setPresetReport(report);
    } catch (e) {
      setPresetError(String(e));
    }
  };

  const clearPreset = async () => {
    setPresetError(null);
    try {
      await setMilkPreset(null);
      setPreset(null);
      setPresetReport([]);
    } catch (e) {
      setPresetError(String(e));
    }
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
      {/* Only while MilkDrop is the visual showing: a preset means nothing to
          the other six, and a control that is always there but usually inert is
          a control people learn to ignore. It says what it needs when nothing
          is loaded, because nothing is loaded on a fresh install -- no preset
          ships with the app (decision 57), so an empty gallery entry is the
          normal first experience rather than an error. */}
      {milkdropShowing && (
        <div className="presets">
          <span className="label">Preset</span>
          <button type="button" onClick={() => void choosePreset()}>
            {preset ? "Change preset" : "Load a .milk preset"}
          </button>
          {preset && (
            <>
              <span className="hint">{preset}</span>
              <button type="button" onClick={() => void clearPreset()}>
                Clear
              </button>
            </>
          )}
          {!preset && (
            <span className="hint">
              None loaded -- showing MilkDrop's defaults. No presets ship with this app.
            </span>
          )}
          {presetReport.length > 0 && (
            <span className="hint scaled" title={presetReport.join("\n")}>
              {presetReport.length} thing{presetReport.length === 1 ? "" : "s"} this preset needs
              that this build does not do
            </span>
          )}
          {presetError && <span className="hint refused">{presetError}</span>}
          <input
            ref={fileInput}
            type="file"
            accept=".milk"
            style={{ display: "none" }}
            onChange={(event) => void onFilePicked(event)}
          />
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
