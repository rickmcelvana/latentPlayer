//! Phase 0's native spike (PLAN.md §5/§8.1): a Tauri app with two windows --
//! the usual webview window (React UI: pick a file, play/pause) and a
//! second, plain window with **no webview at all**, hosting a
//! `player-render::Renderer` directly on a `wgpu::Surface` built from that
//! window's raw handle. Proves the same Rust render code that will later
//! target a browser canvas (`player-rt-wasm`) also runs natively.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use player_core::{LocalFile, Playback};
use tauri::Manager;

/// How many interleaved samples the scope reads per frame. Arbitrary for
/// Phase 0 -- a real vectorscope will tune this against sample rate/refresh
/// rate deliberately (Phase 4).
const SCOPE_WINDOW_SAMPLES: usize = 2048;

type SharedPlayback = Arc<Mutex<Option<Playback>>>;

/// What the UI needs to render its transport after any state change, in one
/// round trip. Sending these as a struct rather than as four separate
/// commands keeps the seek bar, the clock and the play/pause button from
/// disagreeing with each other mid-poll.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportState {
    position: f64,
    duration: f64,
    playing: bool,
    volume: f32,
    finished: bool,
    /// Gapless hand-offs that have reached the device since `load_file`. Rides
    /// in this struct rather than in a command of its own for the same reason
    /// the rest of it does: the UI resolves "which track is playing" from the
    /// same snapshot it draws the clock from, so the two cannot disagree.
    track_index: u64,
}

impl TransportState {
    fn read(playback: &Playback) -> Self {
        Self {
            position: playback.position_seconds(),
            duration: playback.duration_seconds(),
            playing: playback.is_playing(),
            volume: playback.volume(),
            finished: playback.is_finished(),
            track_index: playback.track_index(),
        }
    }

    /// What the UI shows with nothing loaded.
    fn empty() -> Self {
        Self {
            position: 0.0,
            duration: 0.0,
            playing: false,
            volume: 1.0,
            finished: false,
            track_index: 0,
        }
    }
}

#[tauri::command]
fn load_file(path: String, state: tauri::State<SharedPlayback>) -> Result<TransportState, String> {
    // Streaming since Phase 1 M2: this returns once the decoder thread has a
    // prebuffer's worth of audio ready, not once the whole file is decoded.
    let playback = Playback::open(Box::new(LocalFile::new(path))).map_err(|e| e.to_string())?;
    playback.play();
    let transport = TransportState::read(&playback);
    *state.lock().unwrap() = Some(playback);
    Ok(transport)
}

/// Queues the track to play when the current one ends, replacing whatever was
/// queued before. The engine holds one track of lookahead and no more; the
/// playlist itself lives in the UI (see `Playback::set_next`).
#[tauri::command]
fn set_next(path: String, state: tauri::State<SharedPlayback>) {
    if let Some(playback) = state.lock().unwrap().as_ref() {
        playback.set_next(Box::new(LocalFile::new(path)));
    }
}

/// Forgets any queued track, so the current one is the last.
#[tauri::command]
fn clear_next(state: tauri::State<SharedPlayback>) {
    if let Some(playback) = state.lock().unwrap().as_ref() {
        playback.clear_next();
    }
}

#[tauri::command]
fn transport(state: tauri::State<SharedPlayback>) -> TransportState {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(TransportState::read)
        .unwrap_or_else(TransportState::empty)
}

#[tauri::command]
fn seek(seconds: f64, state: tauri::State<SharedPlayback>) -> TransportState {
    let guard = state.lock().unwrap();
    match guard.as_ref() {
        Some(playback) => {
            playback.seek(seconds);
            TransportState::read(playback)
        }
        None => TransportState::empty(),
    }
}

#[tauri::command]
fn set_volume(volume: f32, state: tauri::State<SharedPlayback>) {
    if let Some(playback) = state.lock().unwrap().as_ref() {
        playback.set_volume(volume);
    }
}

#[tauri::command]
fn set_playing(playing: bool, state: tauri::State<SharedPlayback>) {
    if let Some(playback) = state.lock().unwrap().as_ref() {
        if playing {
            playback.play();
        } else {
            playback.pause();
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(SharedPlayback::default())
        .invoke_handler(tauri::generate_handler![
            load_file,
            set_next,
            clear_next,
            set_playing,
            transport,
            seek,
            set_volume
        ])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Second, plain (no-webview) window hosting the wgpu-rendered
            // scope. Built and configured here, on the main thread -- wgpu
            // requires Metal surface creation to happen there, so doing it
            // unconditionally keeps this portable rather than only
            // "working by luck" on Linux/Windows. Only the redraw loop
            // below moves to a background thread.
            let scope_window = tauri::window::WindowBuilder::new(app, "scope")
                .title("latentPlayer -- scope")
                .inner_size(480.0, 480.0)
                .resizable(false)
                .build()?;
            let size = scope_window.inner_size()?;

            // Keep a handle for the redraw loop to ask the window how big it
            // currently is. `Renderer::new` consumes the one it turns into a
            // surface target, and the renderer holds no window handle of its
            // own, so without this clone nothing can ever tell it the size
            // changed. Window<R> is Clone (tauri 2.11.5 `window/mod.rs`) and
            // the clone shares the same underlying window.
            let size_probe = scope_window.clone();

            let mut renderer = pollster::block_on(player_render::Renderer::new(
                scope_window,
                size.width,
                size.height,
            ))
            .map_err(|e| format!("scope renderer init failed: {e}"))?;

            let shared = app.state::<SharedPlayback>().inner().clone();
            std::thread::spawn(move || {
                let mut last_size = size;
                loop {
                    // Resizing the window invalidates the swapchain. The
                    // renderer can rebuild it at the old size on its own, but
                    // only the window knows the *new* size -- found live on
                    // Windows (2026-08-15): resizing the scope window threw a
                    // continuous stream of "Outdated" frames. Polling here
                    // rather than subscribing to a resize event keeps this on
                    // the one thread that owns the renderer; at 60Hz the cost
                    // is a syscall per frame.
                    if let Ok(current) = size_probe.inner_size() {
                        if current != last_size {
                            renderer.resize(current.width, current.height);
                            last_size = current;
                        }
                    }

                    let samples = {
                        let guard = shared.lock().unwrap();
                        guard
                            .as_ref()
                            .map(|p| p.scope_window(SCOPE_WINDOW_SAMPLES))
                            .unwrap_or_default()
                    };
                    if let Err(e) = renderer.draw_frame(&samples) {
                        log::warn!("scope draw_frame error: {e}");
                    }
                    std::thread::sleep(Duration::from_millis(16));
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
