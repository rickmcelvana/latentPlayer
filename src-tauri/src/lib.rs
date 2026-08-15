//! Phase 0's native spike (PLAN.md §5/§8.1): a Tauri app with two windows --
//! the usual webview window (React UI: pick a file, play/pause) and a
//! second, plain window with **no webview at all**, hosting a
//! `player-render::Renderer` directly on a `wgpu::Surface` built from that
//! window's raw handle. Proves the same Rust render code that will later
//! target a browser canvas (`player-rt-wasm`) also runs natively.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use player_core::{decode_all, LocalFile, Playback};
use tauri::Manager;

/// How many interleaved samples the scope reads per frame. Arbitrary for
/// Phase 0 -- a real vectorscope will tune this against sample rate/refresh
/// rate deliberately (Phase 4).
const SCOPE_WINDOW_SAMPLES: usize = 2048;

type SharedPlayback = Arc<Mutex<Option<Playback>>>;

#[tauri::command]
fn load_file(path: String, state: tauri::State<SharedPlayback>) -> Result<f64, String> {
    let source = LocalFile::new(path);
    let audio = decode_all(&source).map_err(|e| e.to_string())?;
    let duration = audio.duration_seconds();
    let playback = Playback::open(audio).map_err(|e| e.to_string())?;
    playback.play();
    *state.lock().unwrap() = Some(playback);
    Ok(duration)
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
        .invoke_handler(tauri::generate_handler![load_file, set_playing])
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

            let mut renderer = pollster::block_on(player_render::Renderer::new(
                scope_window,
                size.width,
                size.height,
            ))
            .map_err(|e| format!("scope renderer init failed: {e}"))?;

            let shared = app.state::<SharedPlayback>().inner().clone();
            std::thread::spawn(move || loop {
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
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
