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

## Why `--exclude player-rt-wasm`

`player-rt-wasm` calls `wgpu::SurfaceTarget::Canvas`, which only exists
under wgpu's internal `cfg(web)` (active for `wasm32` targets). Plain
`cargo check --workspace` builds every member for the *host* target by
default, where that variant doesn't exist -- so it fails to compile there
by design, not by bug. It's a real workspace member (for `version.workspace
= true` etc. and so `wasm-pack`/`cargo build -p player-rt-wasm --target
wasm32-unknown-unknown` both just work), just never part of a native
`--workspace` command.

## Known environment gotcha: `cargo tauri dev` on this machine

**On this Linux/NVIDIA/Wayland (KWin) setup, `cargo tauri dev` crashes
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

Verified (2026-08-15): with these two env vars, both windows (webview UI +
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
