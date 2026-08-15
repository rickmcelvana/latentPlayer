# latentPlayer — Product & Architecture Plan

Status: **planning only** — no implementation yet. This document is the reference for scoping and sequencing work.

## 1. Vision

A free, open-source, cross-platform (desktop + web, same codebase) music player under the **latent** brand — sibling to **latentmastering** and **latentmixer** (AI mixing/mastering, currently in alpha). Two audiences it needs to serve at once:

- **Everyday listeners** — a fast, good-looking player for local files and self-hosted libraries, competitive with Winamp/foobar2000/AIMP/Musicbee, plus a universal capture-based visualizer for whatever else they're already playing.
- **Artists & mixing/mastering engineers** — the audience latentmastering/latentmixer already targets. For them, latentPlayer should double as a **free, always-open reference-monitoring tool**: LUFS/true-peak/phase metering and a striking 3D vectorscope that are normally locked behind $50–200 plugins. This is the app's actual differentiator and the reason it's worth building under this brand rather than being "one more Winamp clone."

The unifying hero feature: a **3D vectorscope** — literal, audio-accurate stereo-field data rendered as real-time 3D art. It's simultaneously a professional mixing tool and the "modern Winamp visualizer" everyone keeps asking for.

## 2. Competitive landscape (research summary)

- **Winamp-lineage players** (Winamp itself via webamp.org, AIMP, foobar2000, Audacious, Clementine/Strawberry) still lead on format support, plugins, and skinning, but their visualizers are 2D holdovers (oscilloscope, spectrum, MilkDrop-style pixel shaders) and their metering is basic VU/peak at best.
- **MilkDrop's** legacy lives on via **Butterchurn**, a WebGL port that runs existing MilkDrop `.milk` presets in-browser — proof that the preset format and community content (tens of thousands of presets) are portable to a modern web stack. There's no WebGPU-native successor yet; that's an open gap.
- Standalone 3D visualizers exist (Plane9, Luminant Music, Overture — the latter even has a VR/Oculus mode) but none are bundled into a full-featured *player*, and none pair visuals with real mix-analysis metering.
- **Pro metering** (LUFS, true peak, phase correlation, goniometer, dynamic-range) lives almost exclusively in paid mastering plugins (iZotope Insight, Mastering The Mix LEVELS, LVC-Meter) or a few free single-purpose VSTs (Youlean Loudness Meter, Voxengo SPAN). None of it is bundled into a casual listening app — that's the open gap latentPlayer can own.
- **Self-hosted/streaming-adjacent Tauri players already exist**: Feishin (TS, Subsonic/Navidrome/Jellyfin only, no Spotify/Apple), Musicat (Svelte+Tauri, local files + tagging), rustmusic (SvelteKit+Tauri+Rust, audiophile local playback: DSD, DLNA, OS media integration). None combine streaming-service plugins with pro metering/visuals — that combination doesn't exist in the free tier anywhere.

## 3. Playback vs. Capture — no per-service integration, no ToS exposure

Decision: **drop per-service streaming integration entirely.** No Spotify SDK, no Apple MusicKit, no YouTube embed, no unofficial/reverse-engineered clients (librespot, ytmusicapi, etc.). None of that ships. This removes every category of ToS/legal exposure the earlier draft carried, and — as a side effect — makes the visualizer *more* universal, not less: instead of supporting a hand-picked list of services, it works with **anything** the user is already playing, on any app, including ones we'll never write an integration for (Tidal, Deezer, SoundCloud, a browser tab, a game).

The app has two independent modes instead of a tiered streaming ladder:

### Mode A — Player (in-app playback, full fidelity)
- Local files (MP3/FLAC/WAV/AAC/OGG/ALAC/Opus, DSD as a stretch goal) via **Symphonia** (pure-Rust decode) + **rodio/cpal** (output).
- Subsonic-compatible self-hosted servers (Navidrome, Jellyfin, Airsonic) — open protocol, zero legal risk, and it's how a large chunk of the self-hosted/audiophile community already listens.
- This is the tier where the full pro visualizer/metering suite works perfectly, because we own the raw PCM directly — no capture step at all. Also the mode that best matches latentmastering/latentmixer's actual users: people listening to their own tracks/masters.

### Mode B — Capture (visualizer/metering only, source-agnostic)
A floating/overlay visualizer + metering panel that doesn't play or control anything — it just listens to audio the OS is already outputting, from whatever app the user chose to play it in, and drives the exact same 3D vectorscope/LUFS/phase/spectrum suite off that signal. No auth, no per-service code, no ToS surface at all: it's OS-level audio routing, the same mechanism screen-recording/streaming software already uses to capture desktop audio.

Capture Mode is **desktop-only** (see Open Decisions §8.3): the web build has no OS-level audio-capture API to call, full stop. The web build never shows a broken/greyed-out Capture UI for this — instead, its player surfaces a simple "want the 3D vectorscope on anything you're playing? Get the desktop app" link/CTA. Web's job is to be a complete local/self-hosted *player* on its own merits, not a teaser missing half its features.

Two capture granularities, both legitimate OS features (not a hack):
- **Per-application capture** (preferred when available) — isolate just the target app's output, so system notification dings and other apps don't pollute the signal:
  - **Windows**: `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS` (Win10 2004+/Build 19041+) via WASAPI's `ActivateAudioInterfaceAsync` — capture (or exclude) a specific process and its children.
  - **macOS**: ScreenCaptureKit's `SCContentFilter`, scoped to one app's audio. This rides on the screen-recording permission even though no video is used. **Resolved UX (§8.4):** detect macOS and show our own explanatory dialog *before* triggering the OS prompt — plain-language, honest copy ("latentPlayer needs macOS's screen-recording permission to isolate just [App]'s audio for the visualizer. We do not record or capture your screen.") with Continue/Not now buttons. Continue triggers the real OS permission dialog; Not now backs out to whole-system-mix fallback or no capture, no dead end. Same pattern as a camera/mic pre-prompt in any polished macOS app — never let the OS's generically-worded system dialog be the user's first exposure to why we're asking.
  - **Linux**: PipeWire per-app virtual-sink routing — the same mechanism the `obs-pipewire-audio-capture` plugin uses; well-established, no special permission model.
- **Whole-system-mix fallback** (WASAPI loopback / CoreAudio tap / PulseAudio monitor) when per-app isn't available or the user picks "capture everything."

This means: "play your own YouTube Music / Apple Music / Spotify / anything, in whatever app you already use, and point latentPlayer's visualizer/metering at it" — same value proposition the earlier streaming-integration idea was chasing, but with zero services to integrate, zero accounts to build against, and zero ToS to violate. It's also lower friction for adoption than "switch your primary player": Capture Mode is a companion app, not a replacement.

## 4. The visualizer/metering suite (the differentiator)

This is where latentPlayer earns the "latent" branding, sharing DNA with latentmastering/latentmixer's analysis engine rather than being a bolt-on.

**Core metering (professional, always free, identical whether the signal comes from direct playback or Capture Mode):**
- LUFS loudness meter — integrated / short-term / momentary, per ITU-R BS.1770-4 / EBU R128.
- True-peak meter (oversampled, inter-sample-peak detection).
- Phase correlation meter + full goniometer.
- Real-time spectrum analyzer (linear & log/note-frequency modes) and a spectrogram/waterfall view.
- Dynamic-range (DR/crest-factor) meter, mid-side balance meter.
- **Reference-track A/B, loudness-matched comparison** — load a reference master and A/B it against the current track at matched LUFS, overlaying spectra. This is a staple mastering-engineer workflow (see: Metric AB) that no free listening app offers.

**The 3D vectorscope (hero feature):** extrude the classic 2D Lissajous/XY scope along a time axis into a literal 3D ribbon — accurate stereo-field data as a piece of generative art, not a stylized "visualizer" faking reactivity.

**Modernizing the Winamp/MilkDrop visualizer:**
1. Real 3D scenes (particles, SDF raymarching, GPU-instanced geometry) via compute shaders, not MilkDrop's flat-plane pseudo-3D.
2. Musically-intelligent reactivity: onset/beat detection, BPM tracking, spectral centroid, transient detection feeding shader uniforms — reacting to musical structure, not just raw amplitude.
3. **MilkDrop `.milk` preset compatibility** (Butterchurn already proved this is portable) so the app launches with instant access to decades of community presets, plus a native WGSL-based 3D preset format for new content.
4. **AI-adaptive/generative presets** powered by the same audio-analysis core as latentmixer/latentmastering — a feature no competitor (free or paid) currently offers, and a natural cross-promotion: "see what the AI mastering engine hears."
5. Stretch: VR/immersive mode — `wgpu` already runs on OpenXR (Quest 3 confirmed in the wild), so this is a real possibility later, not vaporware.

## 5. Architecture

**Core principle:** one Rust core, two shells.

```
                     ┌───────────────────────────────┐
                     │        latent-core (Rust)      │
                     │  audio decode/mix/output        │
                     │  analysis (FFT/LUFS/beat/etc.)  │
                     │  wgpu-based render engine       │
                     │  PlaybackSource trait (local,    │
                     │   Subsonic) + CaptureSource      │
                     │   trait (per-OS loopback)         │
                     └───────┬─────────────────┬───────┘
                             │                 │
                 compiles to │                 │ compiles to
                    native   ▼                 ▼   wasm32
              ┌─────────────────────┐   ┌─────────────────────┐
              │   Tauri desktop      │   │   Web (browser)      │
              │  React/Vite webview, │   │  React/Vite,          │
              │  #[tauri::command]s  │   │  wgpu → WebGPU/       │
              │  native window,      │   │  WebGL2 fallback,     │
              │  WASAPI/CoreAudio/   │   │  audio I/O via         │
              │  Pulse loopback      │   │  AudioWorklet + SAB    │
              │  (Capture Mode only) │   │  (Capture Mode: N/A)  │
              └─────────────────────┘   └─────────────────────┘
```

One React/TS codebase (Vite) renders in both shells — the Tauri webview and the browser — same component tree, calling into the Rust core via Tauri's `invoke()` on desktop and `wasm-bindgen`/`wasm-pack` on web. This mirrors latent-mixing's `app/` exactly (see §9), not a new pattern invented for this app.

**Crate choices (from research, not guesses):**
- Decode: **Symphonia** (pure Rust — MP3/FLAC/AAC/ALAC/OGG/Vorbis/WAV/Opus/etc.)
- Playback/output: **rodio** (built on Symphonia) or raw **cpal** for tighter control over the mix graph.
- Spectral analysis: **rustfft** (already a Symphonia dependency) or the `spectrum-analyzer` crate.
- 3D rendering, both shells: **wgpu** — the one dependency choice that makes "same visuals natively and in-browser" actually true: it targets Vulkan/Metal/D3D12 natively and WebGPU-with-WebGL2-fallback on wasm.
- Playback abstraction: a small `PlaybackSource` trait (Local, Subsonic) — the only two sources the app plays directly.
- Capture abstraction: a `CaptureSource` trait with per-OS backends (Windows process-loopback, macOS `SCContentFilter`, Linux PipeWire app-sink) behind one interface, feeding the same analysis/render pipeline as playback does. Desktop-only — the web build has no OS-level audio-capture API to call, so Capture Mode is a Tauri-only feature (§8.3).

**Web audio I/O — de-risked, not just spiked.** `cpal` doesn't target wasm natively, so the web build needs a Web Audio API path underneath the same core API. Rather than a from-scratch spike, latent-mixing already ships a working reference for exactly this: per-track SPSC ring buffers in a `SharedArrayBuffer` (`Atomics`-based, main thread produces / `AudioWorklet` consumes), a pre-compiled `WebAssembly.Module` handed to the worklet via `processorOptions`, the worklet bundled separately by esbuild (not Vite) to a stable unhashed URL, and COOP/COEP headers configured in both the Vite dev server and the Tauri packaged-build headers so `SharedArrayBuffer` is available in both shells. latentPlayer's spike is "port this pattern," not "invent it" — see `../latent-mixing/ARCHITECTURE.md` and `app/src/worklet/`.

**UI framework — RESOLVED 2026-08-15: React + TypeScript + Vite + Zustand.** Both latent-mastering and latent-mixing already use this stack (Tauri 2 wraps the same React UI for desktop in latent-mixing); there is no Svelte anywhere in the ecosystem. Matching it means latentPlayer can reuse the adapter-module pattern (components never touch wasm/worklet directly — see `mixerEngine.ts`/`engine.ts`), the wasm-pack + esbuild build tooling, the COOP/COEP setup above, and eventually shared visual language/components, rather than standing up a third, unrelated frontend stack. (Original recommendation was Svelte/SolidJS, based on prior art *outside* this codebase's own ecosystem — Musicat, rustmusic. Superseded once the sibling repos were checked directly.)

## 6. Feature set

### Table stakes (must match existing free players)
Local playback of common formats, gapless playback, crossfade, playlists incl. smart playlists, queue, tag editor (ID3/Vorbis comments) with MusicBrainz lookup, library scan + album art fetch, search, basic EQ, ReplayGain/loudness-normalized playback, synced (LRC) lyrics, Last.fm/ListenBrainz scrobbling, OS media integration (MPRIS/SMTC/macOS Now Playing/Media Session API), system tray + global media keys, mini-player, theming (optional legacy `.wsz` Winamp skin import as a nostalgia hook).

### Playback & Capture (§3)
Local files + Subsonic/Navidrome/Jellyfin (Mode A, in-app playback) — the only sources the app actually plays. Universal per-app or whole-system audio Capture Mode (Mode B) for visuals/metering against anything playing anywhere else on the machine, with no per-service integration at all.

### Visualization & metering (flagship — §4)
3D vectorscope, phase correlation meter, LUFS meter, true-peak meter, spectrum analyzer + spectrogram, DR/mid-side meters, reference-track loudness-matched A/B, MilkDrop-compatible + native 3D preset engine, AI-adaptive presets, VR mode (stretch). All of it works identically whether the signal comes from Mode A (direct PCM) or Mode B (captured audio).

### What's genuinely not in any free app today (the pitch, in one place)
- A full pro metering suite (LUFS/true-peak/phase/DR) bundled free into a listening app instead of gated behind paid plugins.
- Reference-track, loudness-matched A/B comparison in a consumer player.
- A literal 3D vectorscope as the primary visual identity.
- A universal, source-agnostic visualizer/metering companion — point it at *any* app (Spotify, YouTube Music, Apple Music, a browser tab, a game) with no account, no integration, no ToS exposure, via legitimate OS-level per-app audio capture.
- AI-adaptive visual presets driven by the same analysis engine as latentmastering/latentmixer.
- One-click handoff — open the current track/selection directly in latentmastering or latentmixer (ecosystem funnel, real product rationale for the shared brand).

## 7. Phased roadmap

- **Phase 0 — Spikes:** validate `wgpu` rendering shared between Tauri native and wasm/browser; port latent-mixing's AudioWorklet+SharedArrayBuffer pattern (§5, §9) for the web audio path rather than designing one from scratch; minimal local playback + 2D scope as a walking skeleton.
- **Phase 1 — MVP:** local file player, tagging/library, playlists, basic EQ, spectrum analyzer, phase correlation meter, LUFS meter, 3D vectorscope v1; ships as both a Tauri desktop build and a web build.
- **Phase 2 — Self-hosted playback:** Subsonic/Navidrome/Jellyfin support — zero legal risk, immediately unlocks large existing libraries for self-hosting users.
- **Phase 3 — Capture Mode:** per-OS `CaptureSource` backends (Windows process-loopback, macOS `SCContentFilter`, Linux PipeWire), desktop-only; universal visualizer/metering companion for any other app on the machine.
- **Phase 4 — Visualizer v2:** MilkDrop preset compatibility, native WGSL 3D preset format, AI-adaptive presets; VR mode as stretch.
- **Phase 5 — Ecosystem:** latentmastering/latentmixer handoff integrations, community preset sharing, plugin SDK.

## 8. Decisions (resolved 2026-08-15)

1. **UI framework: React + TypeScript + Vite + Zustand**, not Svelte/SolidJS. Matches both sibling apps exactly (see §9) — reuses their build tooling, adapter-module pattern, and COOP/COEP+SharedArrayBuffer audio path instead of standing up a third stack. Detail and rationale folded into §5.
2. **Self-hosting scope for v1: Phase 1 is local-files-only.** Subsonic/Navidrome/Jellyfin support waits for Phase 2, as the roadmap in §7 already lays out — no change needed there, just confirmed rather than left as a question.
3. **Capture Mode and the web build: web is local/self-hosted player only.** Capture Mode never ships to web — not "coming later," not partially present. The web player's UI includes a link to the desktop app for anyone who wants the visualizer/metering-on-anything pitch. Detail folded into §3.
4. **macOS permission UX: pre-prompt before the OS dialog.** Detect macOS, show our own honest explanatory dialog (Continue/Not now) before ScreenCaptureKit's screen-recording permission prompt ever appears, so the OS's generic wording is never the user's first exposure to why we're asking. Detail folded into §3.
5. **Brand integration depth: simple, account-free handoff; exact UX decided per-app later.** Both latentmastering and latentmixer are free to use with no account today (confirmed against their own PROJECT.md decisions — see §9), so latentPlayer's handoff has no account/session state to bridge either. Both sibling apps should be able to **send to latentPlayer**, and latentPlayer should be able to **send to both of them** — the exact mechanics (file handoff vs. a `latentplayer://` deep link vs. shared local storage) get decided in a dedicated session with each app rather than guessed now, matching how latent-mixing's own "Send to Latent Mastering" feature (already on its Phase 3 roadmap, undesigned in detail) is being handled. Scope for now: the *capability* is confirmed in both directions; the *protocol* is not designed yet — track as an open item when that session happens, not a blocker to starting latentPlayer's build.

## 9. Ecosystem context (from `../latent-mastering` and `../latent-mixing`)

Findings from reading the sibling repos directly, kept here so they don't need re-deriving later:

- **Both are closed-source/commercial** (proprietary `LICENSE` + `THIRD-PARTY-LICENSES.md`, permissive-license-only dependency policy). latentPlayer's own licensing model isn't decided yet — it's a separate open question, not assumed to inherit theirs, since the earlier README/CLAUDE.md history here doesn't commit either way.
- **Monetization precedent (not yet a latentPlayer decision, just context):** both sibling apps settled on *web free permanently, desktop is the eventual paid product* — explicitly "no metering, quota, account, billing or entitlement work belongs in the web app." If latentPlayer follows the same shape, phase 1–3 (web + desktop, no accounts) cost nothing to build under that model either way; only a future paid-desktop-tier decision would need revisiting.
- **Hosting pattern**: self-hosted Debian + Caddy (moved off Cloudflare Pages / dev-tunnel setups once real), each app at `app.latent<name>.com`, marketing content under the shared `latentbeats.com` umbrella (its own separate repo), each app's site cross-linking its siblings. latentPlayer would presumably follow the same shape (`app.latentplayer.com` + a `latentbeats.com/player/` section) when it reaches the equivalent phase — not a decision to make now, just the established convention to default to later.
- **Shared feedback API**: both apps already POST to one `/api/feedback` endpoint (with a `source` field distinguishing which app) rather than each rolling its own. latentPlayer should plug into that same endpoint when it needs feedback collection, not build a third one.
- **Realtime web-audio architecture is proven, not theoretical** — see §5's rewritten "Web audio I/O" note. This resolves what PLAN.md previously flagged as the single biggest open technical risk.

## Sources consulted

- [cpal WASAPI loopback discussion](https://github.com/RustAudio/cpal/issues/476) / [Windows loopback recording docs](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- Per-app capture: [Windows Application Loopback Audio Capture sample](https://learn.microsoft.com/en-us/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/) (`AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`, Win10 2004+) / [Apple ScreenCaptureKit — capturing screen content on macOS](https://developer.apple.com/documentation/ScreenCaptureKit/capturing-screen-content-in-macos) (`SCContentFilter` per-app audio) / [obs-pipewire-audio-capture](https://github.com/dimtpap/obs-pipewire-audio-capture) (Linux per-app capture precedent)
- [Feishin](https://feishin.net/) / [Musicat](https://github.com/basharovV/musicat) / [rustmusic](https://github.com/larevuegeek/rustmusic)
- [Symphonia](https://docs.rs/symphonia/) / [Rodio](https://lib.rs/crates/rodio) / [spectrum-analyzer crate](https://crates.io/crates/spectrum-analyzer)
- [Butterchurn (MilkDrop on WebGL)](https://github.com/jberg/butterchurn)
- [wgpu](https://wgpu.rs/) — cross-platform native + WebGPU/WebGL2
- Metering landscape: [Youlean Loudness Meter / Voxengo SPAN roundup](https://www.whippedcreamsounds.com/6-best-free-metering-plugins/), [LVC-Meter/LEVELS goniometer feature comparison](https://lordreverb.com/plugins/metering-plugins-guide/)
- Winamp-alternative landscape: [AlternativeTo open-source Winamp alternatives](https://alternativeto.net/software/winamp/?license=opensource)

**Rejected approaches** (researched, not pursued — see §3): official streaming SDKs ([Spotify Web Playback SDK](https://developer.spotify.com/documentation/web-playback-sdk), [Apple MusicKit JS](https://js-cdn.music.apple.com/musickit/v1/index.html)) and unofficial/reverse-engineered clients ([ytmusicapi](https://ytmusicapi.readthedocs.io/), [librespot](https://github.com/librespot-org/librespot)) — dropped in favor of source-agnostic Capture Mode to keep the service list unlimited and ToS exposure at zero.
