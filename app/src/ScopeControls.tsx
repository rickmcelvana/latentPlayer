// Size and visibility for the 3D scope, on both shells.
//
// One component for two very different hosts: natively these buttons move a
// separate OS window, on the web they resize a canvas in this page. What the
// user is doing is the same in both, which is the argument for the adapter
// underneath (scope.ts) rather than two components that happen to look alike.
import { useEffect, useState } from "react";
import { DEFAULT_SCOPE_SIZE, SCOPE_SIZES, scopeState, setScopeSize, setScopeVisible } from "./scope";

export function ScopeControls() {
  const [size, setSize] = useState<number>(DEFAULT_SCOPE_SIZE);
  const [visible, setVisible] = useState(true);

  // The UI is not the only party that can change this -- the scope window has
  // edges the user can drag natively -- so the initial state is read rather
  // than assumed.
  useEffect(() => {
    let cancelled = false;
    scopeState()
      .then((state) => {
        if (cancelled) return;
        setSize(state.width);
        setVisible(state.visible);
      })
      .catch(() => {
        // A scope we can't ask about is still a scope with working buttons.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const choose = (next: number) => {
    setSize(next);
    void setScopeSize(next, next).catch(() => {});
  };

  const toggle = () => {
    const next = !visible;
    setVisible(next);
    void setScopeVisible(next).catch(() => {});
  };

  return (
    <div className="scope-controls">
      <span className="label">Scope</span>
      {SCOPE_SIZES.map((option) => (
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
    </div>
  );
}
