// Standard Tauri v2 runtime check: the injected internals global only
// exists inside a Tauri webview, never in a plain browser tab. Gate every
// `@tauri-apps/*` call behind this so the same React tree also runs as the
// web build (PLAN.md §8.3 -- web is a real player, not a crippled preview).
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
