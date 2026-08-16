//! Phase 0's native spike (PLAN.md §5/§8.1): a Tauri app with two windows --
//! the usual webview window (React UI: pick a file, play/pause) and a
//! second, plain window with **no webview at all**, hosting a
//! `player-render::Renderer` directly on a `wgpu::Surface` built from that
//! window's raw handle. Proves the same Rust render code that will later
//! target a browser canvas (`player-rt-wasm`) also runs natively.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use player_core::library::{Library, LibraryTrack, Playlist, ScanSummary};
use player_core::{LocalFile, Playback};
use tauri::Manager;

/// The scope window's label, and the sizes the UI offers.
///
/// Since M6 the window is resizable and the UI owns its geometry: what was a
/// constant in this file is now `set_scope_size`, so "make the visualiser
/// bigger" is a thing the user can do rather than a rebuild (M6 decision 4).
const SCOPE_LABEL: &str = "scope";

/// What the scope window opens at.
const SCOPE_DEFAULT_SIZE: f64 = 480.0;

type SharedPlayback = Arc<Mutex<Option<Playback>>>;

/// The music library (M7). Built in `setup` against the app data directory,
/// so unlike the playback slot it is always present -- there is no "no library
/// open" state for the UI to handle.
type SharedLibrary = Arc<Mutex<Library>>;

/// The URI scheme cover art is served over.
///
/// Art does not travel through `invoke`: a 50KB JPEG becomes a JSON array of
/// 50,000 numbers on that path, per row, and a library view shows a lot of
/// rows (M7 decision 5). The webview fetches these as ordinary images
/// instead, which also means the same `<img src>` works on both shells --
/// the web build points it at an object URL.
const COVER_SCHEME: &str = "cover";

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

/// The latest spectrum: one dBFS level per display band, as of whenever the
/// analysis thread last ran.
///
/// A separate command from `transport` rather than a field on it, unlike
/// `track_index`: this is 64 floats polled at frame rate by one component,
/// where the transport is five scalars polled at 10Hz by all of them. Bundling
/// them would make every clock tick carry a spectrum.
#[tauri::command]
fn spectrum(state: tauri::State<SharedPlayback>) -> Vec<f32> {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|playback| playback.spectrum())
        .unwrap_or_default()
}

/// Loudness and phase, read as one snapshot.
///
/// The five numbers describe one instant, so they cross together (M5 decision
/// 4). They are also all finite by construction -- `player-core` reports a floor
/// rather than negative infinity precisely because a non-finite float becomes
/// `null` on the way through this boundary (M5 decision 5).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MeterState {
    momentary_lufs: f32,
    short_term_lufs: f32,
    integrated_lufs: f32,
    true_peak_db: f32,
    correlation: f32,
}

impl From<player_core::MeterSnapshot> for MeterState {
    fn from(snapshot: player_core::MeterSnapshot) -> Self {
        Self {
            momentary_lufs: snapshot.momentary_lufs,
            short_term_lufs: snapshot.short_term_lufs,
            integrated_lufs: snapshot.integrated_lufs,
            true_peak_db: snapshot.true_peak_db,
            correlation: snapshot.correlation,
        }
    }
}

#[tauri::command]
fn meters(state: tauri::State<SharedPlayback>) -> MeterState {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|playback| playback.meters().into())
        .unwrap_or_else(|| player_core::MeterSnapshot::silent().into())
}

/// The goniometer's points for the window the analysis thread last read:
/// interleaved side/mid pairs, already decimated to a fixed count in Rust so
/// what crosses here per frame doesn't grow with the sample rate.
#[tauri::command]
fn goniometer(state: tauri::State<SharedPlayback>) -> Vec<f32> {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|playback| playback.goniometer())
        .unwrap_or_default()
}

/// Centre frequency of each band `spectrum` returns. Fixed once a track is
/// open, so the UI fetches it once rather than per frame.
#[tauri::command]
fn spectrum_bands(state: tauri::State<SharedPlayback>) -> Vec<f32> {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|playback| playback.spectrum_band_centres().to_vec())
        .unwrap_or_default()
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

/// Walks `path`, reads every audio file's tags, and merges the result into the
/// library.
///
/// `async` on purpose, and doing its work in `spawn_blocking`: a sync command
/// runs on the main thread, and a folder of several thousand tracks is tens of
/// seconds of file IO and tag parsing. On the main thread that is a frozen UI
/// and -- because the scope's redraw thread shares nothing with it but the
/// playback mutex -- a scan that could be *heard* if it ever took that lock at
/// a bad moment. Nothing the library does may reach the audio path
/// (M7 decision 6).
#[tauri::command]
async fn scan_folder(
    path: String,
    state: tauri::State<'_, SharedLibrary>,
) -> Result<ScanSummary, String> {
    let library = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        library
            .lock()
            .unwrap()
            .scan(&path)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Every row in the library. Sent whole rather than paged: a row is a couple
/// of hundred bytes, so even a large library is a few megabytes read once,
/// and search then runs over the same array on both shells rather than being
/// implemented twice.
#[tauri::command]
fn library_tracks(state: tauri::State<SharedLibrary>) -> Vec<LibraryTrack> {
    state.lock().unwrap().tracks().to_vec()
}

#[tauri::command]
fn library_folders(state: tauri::State<SharedLibrary>) -> Vec<String> {
    state
        .lock()
        .unwrap()
        .folders()
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Forgets a folder and its rows. The files themselves are untouched.
#[tauri::command]
fn forget_folder(path: String, state: tauri::State<SharedLibrary>) -> Result<(), String> {
    state
        .lock()
        .unwrap()
        .forget_folder(&path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn playlists(state: tauri::State<SharedLibrary>) -> Vec<Playlist> {
    state.lock().unwrap().playlists().to_vec()
}

#[tauri::command]
fn save_playlist(playlist: Playlist, state: tauri::State<SharedLibrary>) -> Result<(), String> {
    state
        .lock()
        .unwrap()
        .save_playlist(playlist)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_playlist(id: String, state: tauri::State<SharedLibrary>) -> Result<(), String> {
    state
        .lock()
        .unwrap()
        .delete_playlist(&id)
        .map_err(|e| e.to_string())
}

/// The scope window's geometry, as the UI sees it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeState {
    width: u32,
    height: u32,
    visible: bool,
}

#[tauri::command]
fn scope_state(app: tauri::AppHandle) -> ScopeState {
    let window = app.get_window(SCOPE_LABEL);
    let size = window
        .as_ref()
        .and_then(|w| w.inner_size().ok())
        .unwrap_or(tauri::PhysicalSize {
            width: SCOPE_DEFAULT_SIZE as u32,
            height: SCOPE_DEFAULT_SIZE as u32,
        });
    ScopeState {
        width: size.width,
        height: size.height,
        // A window that failed to report is treated as hidden rather than
        // shown: the toggle then offers to bring it back, which is the useful
        // direction to be wrong in.
        visible: window.and_then(|w| w.is_visible().ok()).unwrap_or(false),
    }
}

/// Resizes the scope window. The renderer notices on its own -- its redraw
/// loop polls the window's size (see `setup` below) -- so this only has to
/// move the window.
#[tauri::command]
fn set_scope_size(width: u32, height: u32, app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_window(SCOPE_LABEL)
        .ok_or_else(|| "the scope window is not open".to_string())?;
    window
        .set_size(tauri::Size::Physical(tauri::PhysicalSize {
            width: width.max(160),
            height: height.max(160),
        }))
        .map_err(|e| e.to_string())
}

/// Shows or hides the scope window. Hiding rather than closing it: the
/// renderer holds the surface built from this window's handle, and closing it
/// would leave a renderer drawing into nothing.
#[tauri::command]
fn set_scope_visible(visible: bool, app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_window(SCOPE_LABEL)
        .ok_or_else(|| "the scope window is not open".to_string())?;
    if visible {
        window.show().map_err(|e| e.to_string())
    } else {
        window.hide().map_err(|e| e.to_string())
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

/// Builds one `cover://` response.
fn cover_response(
    status: u16,
    content_type: &str,
    bytes: Vec<u8>,
) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header(tauri::http::header::CONTENT_TYPE, content_type)
        .header("Cross-Origin-Resource-Policy", "cross-origin")
        .body(bytes)
        // The builder only fails on a malformed header, all of which are
        // literals above; an empty 500 is still a response the webview can
        // handle, where an unwrap would take the whole app down over a
        // missing album cover.
        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
}

/// The image's type from its own first bytes rather than from what the tag
/// claimed. `player-core` records the media type symphonia reported, but this
/// side only has the cached file, and a browser that is told `image/jpeg`
/// about a PNG will refuse to draw it.
fn sniff_image_type(bytes: &[u8]) -> &'static str {
    match bytes {
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', ..] => "image/gif",
        [b'B', b'M', ..] => "image/bmp",
        // RIFF....WEBP
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ => "application/octet-stream",
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
            set_volume,
            spectrum,
            spectrum_bands,
            meters,
            goniometer,
            scope_state,
            set_scope_size,
            set_scope_visible,
            scan_folder,
            library_tracks,
            library_folders,
            forget_folder,
            playlists,
            save_playlist,
            delete_playlist
        ])
        // Cover art, served straight to the webview rather than serialised
        // through a command (M7 decision 5). The response carries an explicit
        // `Cross-Origin-Resource-Policy: cross-origin`, which is not optional
        // here: this app sets `Cross-Origin-Embedder-Policy: require-corp` for
        // the SharedArrayBuffer the audio path needs (tauri.conf.json), and
        // under that header a subresource without CORP is blocked. Without it
        // every cover would silently fail to load and the cause would look
        // like a broken path.
        .register_uri_scheme_protocol(COVER_SCHEME, |ctx, request| {
            let Some(library) = ctx.app_handle().try_state::<SharedLibrary>() else {
                return cover_response(404, "application/octet-stream", Vec::new());
            };
            // The id is the last path segment: `cover://localhost/<id>`, which
            // on Windows the webview rewrites to `http://cover.localhost/<id>`.
            // Taking the segment rather than the whole path is also what keeps
            // a crafted `..` from naming a file outside the cache -- the id is
            // then checked against the library's own rows below, so only a
            // cover this app wrote can be served.
            let id = request
                .uri()
                .path()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();

            let path = {
                let guard = library.lock().unwrap();
                let known = guard
                    .tracks()
                    .iter()
                    .any(|track| track.cover_id.as_deref() == Some(id.as_str()));
                if !known {
                    None
                } else {
                    Some(guard.cover_path(&id))
                }
            };

            match path.and_then(|path| std::fs::read(path).ok()) {
                Some(bytes) => cover_response(200, sniff_image_type(&bytes), bytes),
                None => cover_response(404, "application/octet-stream", Vec::new()),
            }
        })
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // The library lives in the app's own data directory -- the one
            // place a desktop app may write without asking. Managed here
            // rather than at build time because only the running app knows
            // where that is (it depends on the bundle identifier and the OS).
            let data_dir = app.path().app_data_dir()?;
            app.manage(SharedLibrary::new(Mutex::new(Library::open(&data_dir)?)));

            // Second, plain (no-webview) window hosting the wgpu-rendered
            // scope. Built and configured here, on the main thread -- wgpu
            // requires Metal surface creation to happen there, so doing it
            // unconditionally keeps this portable rather than only
            // "working by luck" on Linux/Windows. Only the redraw loop
            // below moves to a background thread.
            let scope_window = tauri::window::WindowBuilder::new(app, SCOPE_LABEL)
                .title("latentPlayer -- scope")
                .inner_size(SCOPE_DEFAULT_SIZE, SCOPE_DEFAULT_SIZE)
                // Resizable since M6: the UI sets the size through
                // `set_scope_size`, and dragging the window's own edge has to
                // work for the same reason. Phase 0 pinned this shut because
                // the renderer couldn't survive a resize; it can now, and the
                // size-poll below is what tells it.
                .resizable(true)
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

                    // How much history to draw is a duration, not a sample
                    // count (M6 decision 1), so it depends on the rate the
                    // device opened at -- which is why this asks the engine
                    // rather than using a constant.
                    let (samples, channels) = {
                        let guard = shared.lock().unwrap();
                        match guard.as_ref() {
                            Some(p) => {
                                let channels = p.output_channels();
                                let wanted =
                                    player_render::window_samples(p.output_sample_rate(), channels);
                                (p.scope_window(wanted), channels)
                            }
                            None => (Vec::new(), 2),
                        }
                    };
                    if let Err(e) = renderer.draw_frame(&samples, channels) {
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
