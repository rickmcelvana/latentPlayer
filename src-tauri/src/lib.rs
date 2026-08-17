//! Phase 0's native spike (PLAN.md §5/§8.1): a Tauri app with two windows --
//! the usual webview window (React UI: pick a file, play/pause) and a
//! second, plain window with **no webview at all**, hosting a
//! `player-render::Renderer` directly on a `wgpu::Surface` built from that
//! window's raw handle. Proves the same Rust render code that will later
//! target a browser canvas (`player-rt-wasm`) also runs natively.

mod servers;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use servers::SharedServer;

use player_core::eq::{self, EqSettings, BAND_FREQUENCIES, EQ_BANDS};
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

/// The scope window's lifecycle, shared with the thread that redraws it.
///
/// The decision this holds (2026-08-17): **the scope window belongs to the app,
/// not to the window manager.** It is built once at startup, hidden and shown on
/// request, and destroyed only when the app itself goes away -- so the window
/// always outlives the renderer drawing into it.
///
/// That is forced by what the renderer is. `Renderer::new` consumes the window
/// it turns into a `wgpu::Surface` and keeps no handle of its own, so it cannot
/// rebuild a surface whose window has gone; and a *destroyed* window's surface
/// does not report itself as `Lost` (which `draw_frame` returns to the caller)
/// but as `Outdated`, whose handling is to call `surface.configure()` -- which
/// panics inside wgpu, since wgpu treats validation errors as fatal by default.
/// Closing the window by hand therefore used to kill the redraw thread and leave
/// the scope dark for the rest of the session (observed live 2026-08-16).
///
/// The alternative was to teach `Renderer` to rebuild its surface from a handle
/// it kept, and let the window come and go. That is real work in the crate
/// furthest from this problem, in exchange for a window the user can destroy and
/// then has to be given some way to recreate -- when "hide it" is what the close
/// button already means to everyone pressing it.
struct ScopeWindow {
    /// Cleared to stop the redraw thread for good. Only shutdown sets it.
    running: AtomicBool,
    /// The redraw thread, so shutdown can wait for it to stop *before* the
    /// window it draws into is destroyed.
    redraw: Mutex<Option<JoinHandle<()>>>,
}

impl ScopeWindow {
    fn new() -> Self {
        Self {
            running: AtomicBool::new(true),
            redraw: Mutex::new(None),
        }
    }

    /// Stops the redraw thread and waits for it, which drops the renderer and
    /// with it the surface.
    ///
    /// **Never call this from the main thread.** The redraw loop asks the window
    /// how big it is, and that question is answered by the main thread's event
    /// loop -- so a main thread blocked in this join would be waiting for a
    /// thread that is waiting for it. Callers on the main thread (the window
    /// event handlers in `setup`) hand this to a thread of its own.
    fn stop(&self) {
        self.running.store(false, Ordering::Release);
        let handle = self.redraw.lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

type SharedPlayback = Arc<Mutex<Option<Playback>>>;

/// Where the EQ's sliders are, independent of whether anything is playing.
///
/// The engine is rebuilt for every track (`Playback::open` builds a fresh one),
/// so an EQ that lived only in the engine would flatten itself at every track
/// change -- including a gapless one, where the listener would hear it happen.
/// This is the source of truth; `load_file` hands it to each new engine.
type SharedEq = Arc<Mutex<EqSettings>>;

/// The rate the EQ's curve is drawn for when nothing is open. The filters are
/// designed for the *output* device's rate, which is only known once a stream
/// exists; 48kHz is the usual answer and the curve is redrawn against the real
/// rate as soon as a track is loaded.
const DEFAULT_EQ_RATE: u32 = 48_000;

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
fn load_file(
    path: String,
    state: tauri::State<SharedPlayback>,
    eq: tauri::State<SharedEq>,
) -> Result<TransportState, String> {
    // Streaming since Phase 1 M2: this returns once the decoder thread has a
    // prebuffer's worth of audio ready, not once the whole file is decoded.
    let playback = Playback::open(Box::new(LocalFile::new(path))).map_err(|e| e.to_string())?;
    // Before the first sample plays: a new engine starts flat, and the listener
    // set their EQ once, not once per track.
    playback.set_eq(*eq.lock().unwrap());
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

/// Opens a track from the music server and starts playing it (Phase 2 M2).
///
/// The same shape as `load_file`, and deliberately so: what differs is only
/// which `PlaybackSource` is built. Everything downstream -- the ring, the EQ,
/// the tap, every meter and the scope -- is unchanged and cannot tell.
#[tauri::command]
async fn load_server_track(
    id: String,
    suffix: String,
    state: tauri::State<'_, SharedPlayback>,
    eq: tauri::State<'_, SharedEq>,
    server: tauri::State<'_, SharedServer>,
) -> Result<TransportState, String> {
    let source = servers::track_source(&server, &id, &suffix)?;
    let settings = *eq.lock().unwrap();
    // On the blocking pool: `Playback::open` reaches the network now (the first
    // range request happens before it returns, so a bad track is an error here
    // rather than silence later), and the main thread may not wait for that.
    let playback = tauri::async_runtime::spawn_blocking(move || {
        let playback = Playback::open(Box::new(source)).map_err(|e| e.to_string())?;
        playback.set_eq(settings);
        playback.play();
        Ok::<_, String>(playback)
    })
    .await
    .map_err(|e| e.to_string())??;

    let transport = TransportState::read(&playback);
    *state.lock().unwrap() = Some(playback);
    Ok(transport)
}

/// Queues a server track to play when the current one ends -- the gapless
/// hand-off, over the network. The decoder opens it when it runs out of the
/// current track, which is where the prefetch happens: by the time the seam
/// arrives, the first chunk is already in hand.
#[tauri::command]
fn set_next_server_track(
    id: String,
    suffix: String,
    state: tauri::State<SharedPlayback>,
    server: tauri::State<SharedServer>,
) -> Result<(), String> {
    let source = servers::track_source(&server, &id, &suffix)?;
    if let Some(playback) = state.lock().unwrap().as_ref() {
        playback.set_next(Box::new(source));
    }
    Ok(())
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

/// The EQ as the UI sees it: where the sliders are, what they are labelled, and
/// the curve they add up to (M8).
///
/// One struct rather than a command per part, for `TransportState`'s reason:
/// the sliders and the curve drawn behind them describe one setting, and a UI
/// that fetched them separately could draw a curve for a setting it isn't
/// showing. The two axes are constants of the build, but they are cheap and
/// sending them here means the UI never has a copy of them to disagree with.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EqState {
    enabled: bool,
    preamp_db: f32,
    gains_db: Vec<f32>,
    /// The sliders' centre frequencies, for their labels.
    band_frequencies: Vec<f32>,
    /// The curve: gain in dB at each of `response_frequencies`.
    response_db: Vec<f32>,
    response_frequencies: Vec<f32>,
    /// How far a slider can travel either way, so the UI doesn't hold its own
    /// idea of the range the engine will clamp to.
    max_gain_db: f32,
}

impl EqState {
    fn read(settings: EqSettings, sample_rate: u32) -> Self {
        Self {
            enabled: settings.enabled,
            preamp_db: settings.preamp_db,
            gains_db: settings.gains_db.to_vec(),
            band_frequencies: BAND_FREQUENCIES.to_vec(),
            response_db: eq::magnitude_response(&settings, sample_rate),
            response_frequencies: eq::response_frequencies(),
            max_gain_db: eq::MAX_GAIN_DB,
        }
    }
}

/// The rate the running engine's filters are designed for, or the default when
/// nothing is open.
fn eq_rate(playback: &SharedPlayback) -> u32 {
    playback
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.output_sample_rate())
        .unwrap_or(DEFAULT_EQ_RATE)
}

#[tauri::command]
fn eq_state(state: tauri::State<SharedPlayback>, eq: tauri::State<SharedEq>) -> EqState {
    let settings = *eq.lock().unwrap();
    EqState::read(settings, eq_rate(state.inner()))
}

/// Moves the sliders, and returns the state that resulted -- including the
/// clamping, so the UI shows what the engine will actually play rather than
/// what was asked for.
#[tauri::command]
fn set_eq(
    enabled: bool,
    preamp_db: f32,
    gains_db: Vec<f32>,
    state: tauri::State<SharedPlayback>,
    eq: tauri::State<SharedEq>,
) -> EqState {
    let mut gains = [0.0f32; EQ_BANDS];
    // Taking what fits rather than rejecting a wrong-length list: this crosses
    // an IPC boundary where a mismatch is a silent failure, and a missing band
    // is a slider that doesn't move rather than a player that stops.
    for (slot, &db) in gains.iter_mut().zip(gains_db.iter()) {
        *slot = db;
    }
    let settings = EqSettings {
        enabled,
        preamp_db,
        gains_db: gains,
    }
    .clamped();

    *eq.lock().unwrap() = settings;
    if let Some(playback) = state.lock().unwrap().as_ref() {
        playback.set_eq(settings);
    }
    EqState::read(settings, eq_rate(state.inner()))
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

/// Shows or hides the scope window. Hiding rather than closing it: the renderer
/// holds the surface built from this window's handle, and closing it would leave
/// a renderer drawing into nothing (see `ScopeWindow`). Pressing the window's own
/// close button ends up here too, by way of the handler in `setup`.
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

/// What marks a `cover://` id as the music server's rather than the local
/// library's (M3).
///
/// One scheme for both, rather than a second protocol, because the difference is
/// where the bytes come from and nothing else: the component drawing the row
/// asks for a URL and gets one, exactly as it did in M7.
const SERVER_COVER_PREFIX: &str = "server:";

/// One cover out of the local library's cache.
fn library_cover(app: &tauri::AppHandle, id: &str) -> tauri::http::Response<Vec<u8>> {
    let Some(library) = app.try_state::<SharedLibrary>() else {
        return cover_response(404, "application/octet-stream", Vec::new());
    };
    let path = {
        let guard = library.lock().unwrap();
        let known = guard
            .tracks()
            .iter()
            .any(|track| track.cover_id.as_deref() == Some(id));
        known.then(|| guard.cover_path(id))
    };
    match path.and_then(|path| std::fs::read(path).ok()) {
        Some(bytes) => cover_response(200, sniff_image_type(&bytes), bytes),
        None => cover_response(404, "application/octet-stream", Vec::new()),
    }
}

/// One cover off the music server, fetched or held (M3).
///
/// A failure here is a 404 and a blank square, not a message: art is decoration,
/// and a server that cannot produce a thumbnail must not make a playable row
/// look broken. The reason still reaches the log.
fn server_cover(app: &tauri::AppHandle, id: &str) -> tauri::http::Response<Vec<u8>> {
    let (Some(server), Some(covers)) = (
        app.try_state::<servers::SharedServer>(),
        app.try_state::<servers::SharedCovers>(),
    ) else {
        return cover_response(404, "application/octet-stream", Vec::new());
    };
    match servers::cover_art(&server, &covers, id) {
        Ok(cover) => {
            // The server's own content type, but only after sniffing agrees it
            // is an image at all -- the same rule the library covers follow, and
            // for the same reason: a browser told the wrong type draws nothing.
            let sniffed = sniff_image_type(&cover.bytes);
            let content_type = if sniffed == "application/octet-stream" {
                cover.content_type.as_str()
            } else {
                sniffed
            };
            cover_response(200, content_type, cover.bytes.clone())
        }
        Err(e) => {
            eprintln!("fetching cover art {id} from the server failed: {e}");
            cover_response(404, "application/octet-stream", Vec::new())
        }
    }
}

/// Percent-decoding for one path segment.
///
/// `convertFileSrc` builds these with `encodeURIComponent`, so an id containing
/// anything outside the unreserved set -- including the `:` this app's own
/// server prefix uses -- arrives escaped.
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                    16,
                ) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
        .manage(SharedEq::new(Mutex::new(EqSettings::flat())))
        .invoke_handler(tauri::generate_handler![
            load_file,
            set_next,
            clear_next,
            set_playing,
            transport,
            seek,
            set_volume,
            eq_state,
            set_eq,
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
            delete_playlist,
            load_server_track,
            set_next_server_track,
            servers::server_random_tracks,
            servers::server_browse,
            servers::server_state,
            servers::connect_server,
            servers::reconnect_server,
            servers::forget_server
        ])
        // Cover art, served straight to the webview rather than serialised
        // through a command (M7 decision 5). The response carries an explicit
        // `Cross-Origin-Resource-Policy: cross-origin`, which is not optional
        // here: this app sets `Cross-Origin-Embedder-Policy: require-corp` for
        // the SharedArrayBuffer the audio path needs (tauri.conf.json), and
        // under that header a subresource without CORP is blocked. Without it
        // every cover would silently fail to load and the cause would look
        // like a broken path.
        // Asynchronous since Phase 2 M3, and that is not a refactor: a server
        // cover is *fetched*, and answering a request on the main thread would
        // mean the whole UI waiting on the network once per row. The library's
        // own covers are a file read and could still be answered inline; they
        // go the same way so there is one path to reason about.
        .register_asynchronous_uri_scheme_protocol(COVER_SCHEME, |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            // The id is the last path segment: `cover://localhost/<id>`, which
            // on Windows the webview rewrites to `http://cover.localhost/<id>`.
            // Taking the segment rather than the whole path is also what keeps
            // a crafted `..` from naming a file outside the cache -- a library
            // id is then checked against the library's own rows, so only a
            // cover this app wrote can be served.
            let id = percent_decode(request.uri().path().rsplit('/').next().unwrap_or_default());

            std::thread::spawn(move || {
                responder.respond(match id.strip_prefix(SERVER_COVER_PREFIX) {
                    Some(server_id) => server_cover(&app, server_id),
                    None => library_cover(&app, &id),
                });
            });
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

            // The music server (Phase 2 M1). The directory is managed too, since
            // the server's settings live beside the library index and the
            // commands that read them run long after `setup` has returned.
            app.manage(data_dir.clone());
            let server = SharedServer::default();
            app.manage(Arc::clone(&server));
            // Cover art fetched from the server, kept for the session (M3). Its
            // own state rather than part of the connection, so a fetch never
            // holds the lock the browse commands need.
            app.manage(servers::SharedCovers::default());
            // Reconnecting at startup happens off the main thread: it is a
            // network round trip, and a server that is not there takes the
            // request timeout to say so -- which on this thread would be the
            // window not appearing for twenty seconds.
            std::thread::spawn(move || servers::reconnect_stored(&data_dir, &server));

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

            // Not managed state: nothing the webview can invoke touches this.
            // The scope window's lifecycle is the shell's own business, and the
            // UI's part in it is `set_scope_visible`, which speaks to the window
            // rather than to this.
            let scope = Arc::new(ScopeWindow::new());

            // The close button hides the window instead of destroying it, which
            // is the whole of the lifecycle decision written up on
            // `ScopeWindow`: the renderer cannot survive its window going away,
            // and every user pressing that button means "put it away", not
            // "tear down the visualiser for the rest of this session".
            {
                let handle = app.handle().clone();
                let scope = Arc::clone(&scope);
                scope_window.on_window_event(move |event| match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        if let Some(window) = handle.get_window(SCOPE_LABEL) {
                            if let Err(e) = window.hide() {
                                log::warn!("hiding the scope window failed: {e}");
                            }
                        }
                    }
                    // Not reachable while the app is running, given the above --
                    // but if anything ever does destroy this window, the redraw
                    // thread has to hear about it before it draws again.
                    tauri::WindowEvent::Destroyed => {
                        scope.running.store(false, Ordering::Release);
                    }
                    _ => {}
                });
            }

            // Closing the *main* window ends the app, and does it in an order
            // that leaves nothing drawing into a window that is going away: stop
            // the redraw thread first, exit second. Without this the app would
            // not even quit -- Tauri exits when its last window closes, and the
            // scope window (which no longer closes) is a window, so the UI would
            // disappear leaving the process and the scope behind it.
            //
            // On a thread of its own because `ScopeWindow::stop` may not run on
            // the main thread: see its own note.
            if let Some(main_window) = app.get_window("main") {
                let handle = app.handle().clone();
                let scope = Arc::clone(&scope);
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let handle = handle.clone();
                        let scope = Arc::clone(&scope);
                        std::thread::spawn(move || {
                            scope.stop();
                            handle.exit(0);
                        });
                    }
                });
            }

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
            let redraw_scope = Arc::clone(&scope);
            let redraw = std::thread::spawn(move || {
                let mut last_size = size;
                while redraw_scope.running.load(Ordering::Acquire) {
                    // A hidden scope is drawn for nobody, so this parks instead
                    // of spending a GPU frame every 16ms on something no one can
                    // see -- which is now a state the user can reach directly,
                    // since the window's own close button hides it.
                    //
                    // Asked of the window rather than tracked alongside it: the
                    // window is hidden by the UI's toggle, by that close button
                    // and by anything else that gets a handle to it, and a flag
                    // mirroring all of those is a flag that will eventually
                    // disagree with the truth. This is a message to the same
                    // event loop the size question below already asks.
                    if matches!(size_probe.is_visible(), Ok(false)) {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }

                    // Resizing the window invalidates the swapchain. The
                    // renderer can rebuild it at the old size on its own, but
                    // only the window knows the *new* size -- found live on
                    // Windows (2026-08-15): resizing the scope window threw a
                    // continuous stream of "Outdated" frames. Polling here
                    // rather than subscribing to a resize event keeps this on
                    // the one thread that owns the renderer; at 60Hz the cost
                    // is a syscall per frame.
                    match size_probe.inner_size() {
                        Ok(current) => {
                            if current != last_size {
                                renderer.resize(current.width, current.height);
                                last_size = current;
                            }
                        }
                        // The window stopped answering, which means it is no
                        // longer there to answer -- the size question is a round
                        // trip to the main thread's event loop. Drawing the next
                        // frame would reconfigure a dead surface and panic
                        // inside wgpu, so stop instead. Nothing should be able
                        // to get here (see `ScopeWindow`); this is what happens
                        // if something does.
                        Err(e) => {
                            log::warn!(
                                "the scope window stopped answering, so the scope stops: {e}"
                            );
                            return;
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
            // Handed over so shutdown can wait for this thread rather than
            // racing it (`ScopeWindow::stop`).
            *scope.redraw.lock().unwrap() = Some(redraw);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
