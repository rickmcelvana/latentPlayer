# WORKFLOW.md

How to actually build/run/verify this repo. Mirrors `../latent-mastering` and
`../latent-mixing`'s WORKFLOW.md in spirit -- feed this to Aider/Claude at
the start of a session alongside `CONVENTIONS.md`.

## Commands

```
cargo check --workspace --exclude player-rt-wasm   # fast compile check, native crates
cargo test --workspace --exclude player-rt-wasm    # unit tests
cargo clippy --workspace --exclude player-rt-wasm --all-targets -- -D warnings
cargo fmt --check                                  # or drop --check to apply

cd app && npm run wasm                             # rebuild player-rt-wasm for wasm32
cargo check -p player-rt-wasm --target wasm32-unknown-unknown
cargo clippy -p player-rt-wasm --target wasm32-unknown-unknown --all-targets -- -D warnings

cd app && npx tsc -b && npm run build              # frontend typecheck + build
cd app && npm run dev                              # web dev server (localhost:5173)

cargo tauri dev                                    # desktop dev build (see gotcha below)
```

## Reload the page after `npm run wasm`

The dev server hot-reloads TypeScript; it does **not** reload the wasm module a
running page already instantiated. A page left open across a rebuild therefore
runs the old module against the new TypeScript -- and when the two disagree
about the shared buffer's layout, the symptom is not an error. It is audio that
*skips a little at transitions*, which reads as a performance problem and sent
one Phase 3 M5 session looking for one (2026-08-20).

Reload the page after every `npm run wasm`. The smoke harnesses
(`/musical.html`, `/visualizer.html`, and the rest) are the usual victims,
because they are the pages most likely to be sitting open while the Rust under
them is being changed.

## Why `--exclude player-rt-wasm`

`player-rt-wasm` calls `wgpu::SurfaceTarget::Canvas`, which only exists
under wgpu's internal `cfg(web)` (active for `wasm32` targets). Plain
`cargo check --workspace` builds every member for the *host* target by
default, where that variant doesn't exist -- so it fails to compile there
by design, not by bug. It's a real workspace member (for `version.workspace
= true` etc. and so `wasm-pack`/`cargo build -p player-rt-wasm --target
wasm32-unknown-unknown` both just work), just never part of a native
`--workspace` command.

## Windows (the current dev machine)

Everything above runs as written -- no env-var workaround, no flag change.
Toolchain: rustc 1.97.1 (MSVC host), cargo-tauri 2.11.4, node 22, wasm-pack
0.15, `wasm32-unknown-unknown` installed. A fresh clone needs `npm install`
in `app/` and nothing else.

Two Windows-specific things worth knowing:

- **Only run one dev server at a time.** `tauri.conf.json`'s `devUrl` is
  hardcoded to `http://localhost:5173`, and its `beforeDevCommand` starts
  its own `npm run dev`. If something else already holds 5173, Vite quietly
  moves to 5174 while the Tauri webview still loads 5173 -- so the desktop
  app ends up rendering *the other* server's page. It looks like it works
  (same app, after all), which is exactly what makes it confusing. Stop a
  standalone `npm run dev` before `cargo tauri dev`.
- **`output stream error: Default audio device changed`** in the console is
  cpal telling you Windows moved the default output device; it does not
  kill the stream, and the app doesn't act on it yet. See ARCHITECTURE.md's
  "Known gaps".

## Known environment gotcha: `cargo tauri dev` on the Linux box

**On that Linux/NVIDIA/Wayland (KWin) setup, `cargo tauri dev` crashes
instantly with `Gdk-Message: Error 71 (Protocol error) dispatching to
Wayland display`** -- confirmed (2026-08-15) to happen even with zero wgpu
code involved, i.e. a plain single-webview-window Tauri app crashes the same
way. This is a WebKitGTK-on-native-Wayland compatibility issue, not a bug in
this app.

**Fix: force XWayland**, and disable WebKit's hardware compositing (avoids a
separate `Failed to create GBM buffer` spam from the NVIDIA/Mesa GBM path):

```
GDK_BACKEND=x11 WEBKIT_DISABLE_COMPOSITING_MODE=1 cargo tauri dev
```

Not needed on Windows -- this section applies only when working on that
Linux machine. Verified (2026-08-15): with these two env vars, both windows (webview UI +
the wgpu-rendered scope window) launch and stay up cleanly -- no crash over
a sustained run, confirmed both via process liveness and a screenshot
showing the scope window's cleared wgpu surface rendering correctly.
Without them, the same code crashes in under a second. If a real
Wayland-native run ever becomes necessary (e.g. testing without XWayland
available), that's new investigation, not assumed to "just work" by then.

## Verifying a change touching `src-tauri/` or `crates/player-render`

`cargo check`/`clippy` catch compile errors but not "does a window actually
render." For anything touching windowing or the render pipeline, run it for
real with the env vars above and screenshot or eyeball both windows --
compiling clean is not sufficient evidence, per the live-verification habit
both sibling repos already follow.
