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
use player_render::{GainSetting, Musical, VisualFrame, VisualKind};
use tauri::Manager;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};

/// The visualizer window's label.
///
/// Since M6 the window is resizable and the UI owns its geometry: what was a
/// constant in this file is now `set_visualizer_size`, so "make the visualiser
/// bigger" is a thing the user can do rather than a rebuild (M6 decision 4).
/// Since Phase 3 M1 the UI also owns *which visual* it draws and whether it is
/// fullscreen -- the window itself is unchanged, which is the point (decision
/// 5: switching visuals swaps the scene, never the surface or the window).
const VISUALIZER_LABEL: &str = "visualizer";

/// What the visualizer window opens at.
const VISUALIZER_DEFAULT_SIZE: f64 = 480.0;

/// The key that leaves fullscreen on the desktop shell.
///
/// A global shortcut, which is not a preference: this window has no webview, so
/// it is delivered no keyboard events of its own -- a plain window's event
/// stream is moves, resizes, focus and close, and nothing else. Registering
/// globally is therefore the only way Esc reaches it, and registering it
/// *only while fullscreen is active* is what keeps this from swallowing Esc
/// for every other application on the machine (tasks/phase-3.md decision 4).
/// (A function rather than a constant because `Shortcut::new` is not `const`.)
fn fullscreen_exit_key() -> Shortcut {
    Shortcut::new(None, Code::Escape)
}

/// The visualizer window's lifecycle, shared with the thread that redraws it.
///
/// The decision this holds (2026-08-17): **this window belongs to the app,
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
/// the visualizer dark for the rest of the session (observed live 2026-08-16).
///
/// The alternative was to teach `Renderer` to rebuild its surface from a handle
/// it kept, and let the window come and go. That is real work in the crate
/// furthest from this problem, in exchange for a window the user can destroy and
/// then has to be given some way to recreate -- when "hide it" is what the close
/// button already means to everyone pressing it.
struct VisualizerWindow {
    /// Cleared to stop the redraw thread for good. Only shutdown sets it.
    running: AtomicBool,
    /// The redraw thread, so shutdown can wait for it to stop *before* the
    /// window it draws into is destroyed.
    redraw: Mutex<Option<JoinHandle<()>>>,
    /// Which visual the UI has asked for (Phase 3 M1).
    ///
    /// A *request* rather than the renderer's own state, because the renderer
    /// lives on the redraw thread and nothing else may touch it: `set_visual`
    /// leaves the choice here and that thread picks it up between two frames.
    /// The truth about what is currently drawn is the renderer's; this is the
    /// truth about what has been asked for, which is what the UI is drawing a
    /// selected button from.
    requested: Mutex<VisualKind>,
    /// The display gain the UI has asked for, reaching the renderer the same
    /// way and for the same reason.
    gain: Mutex<GainSetting>,
    /// The MilkDrop preset the UI has asked for (Phase 3 M7a), reaching the
    /// renderer the same way again.
    ///
    /// Carries a generation rather than being compared by value: the renderer
    /// cannot be asked which preset it holds without reaching across to the
    /// redraw thread, and comparing 50KB of `.milk` text every frame to find
    /// out whether it changed would be the most expensive thing in the loop.
    preset: Mutex<PresetRequest>,
}

/// A preset the UI has selected, and a counter that says when it changed.
#[derive(Clone, Default)]
struct PresetRequest {
    generation: u64,
    /// What to show a person -- the file's name, not its path.
    name: Option<String>,
    /// The `.milk` text itself. `None` means no preset is selected, which is a
    /// state rather than an absence: the visual falls back to MilkDrop's own
    /// defaults, since no preset ships with the app (decision 57).
    text: Option<String>,
    /// What this preset needs that this build does not do, and anything that
    /// went wrong compiling it. Settled when it loads and shown beside its
    /// name (decision 56).
    report: Vec<String>,
    /// The texture-pack images the preset named, as `(name, bytes)`, read from
    /// the preset's own folder (decision 59). Reaches the renderer after the
    /// text does, since decoding and upload happen inside the visual.
    textures: Vec<(String, Vec<u8>)>,
}

/// Managed so the UI can reach the selection. The rest of this type is still
/// the shell's own business -- the window's lifecycle is not something the
/// webview gets a handle on.
type SharedVisualizer = Arc<VisualizerWindow>;

impl VisualizerWindow {
    fn new() -> Self {
        Self {
            running: AtomicBool::new(true),
            redraw: Mutex::new(None),
            requested: Mutex::new(VisualKind::DEFAULT),
            gain: Mutex::new(GainSetting::DEFAULT),
            preset: Mutex::new(PresetRequest::default()),
        }
    }

    fn request_preset(
        &self,
        name: Option<String>,
        text: Option<String>,
        report: Vec<String>,
        textures: Vec<(String, Vec<u8>)>,
    ) {
        if let Ok(mut slot) = self.preset.lock() {
            slot.generation = slot.generation.wrapping_add(1);
            slot.name = name;
            slot.text = text;
            slot.report = report;
            slot.textures = textures;
        }
    }

    fn requested_preset(&self) -> PresetRequest {
        self.preset
            .lock()
            .map(|slot| slot.clone())
            .unwrap_or_default()
    }

    fn request(&self, kind: VisualKind) {
        if let Ok(mut slot) = self.requested.lock() {
            *slot = kind;
        }
    }

    fn requested(&self) -> VisualKind {
        self.requested
            .lock()
            .map(|slot| *slot)
            .unwrap_or(VisualKind::DEFAULT)
    }

    fn request_gain(&self, setting: GainSetting) {
        if let Ok(mut slot) = self.gain.lock() {
            *slot = setting;
        }
    }

    fn requested_gain(&self) -> GainSetting {
        self.gain
            .lock()
            .map(|slot| *slot)
            .unwrap_or(GainSetting::DEFAULT)
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

/// The preset library (Phase 3 M7c item 3). Built in `setup` against the app
/// data directory, beside the music library, and always present for the same
/// reason.
type SharedPresetLibrary = Arc<Mutex<player_render::milk::PresetLibrary>>;

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
    /// Waiting for audio that has not arrived (Phase 2 M5). Over a network this
    /// is the difference between "slow" and "broken", and until M5 the two were
    /// the same picture: `playing`, with the position held and nothing said.
    buffering: bool,
}

impl TransportState {
    fn read(playback: &Playback) -> Self {
        Self {
            position: playback.position_seconds(),
            duration: playback.duration_seconds(),
            playing: playback.is_playing(),
            volume: playback.volume(),
            finished: playback.is_finished(),
            buffering: playback.is_buffering(),
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
            buffering: false,
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

/// What the music is doing: tempo, beat phase, onsets and brightness.
///
/// One snapshot for `MeterState`'s reason -- these describe one instant -- and
/// separate from it because "loudness and phase" and "what the music is doing"
/// are different questions (Phase 3 M5 decision 38). Finite by construction on
/// the same terms: `player-core` reports 0.0 for "no tempo" rather than a NaN,
/// because a non-finite float becomes `null` crossing this boundary.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MusicalState {
    bpm: f32,
    beat_confidence: f32,
    beat_phase: f32,
    onset: f32,
    transient: f32,
    centroid_hz: f32,
}

impl From<player_core::MusicalSnapshot> for MusicalState {
    fn from(snapshot: player_core::MusicalSnapshot) -> Self {
        Self {
            bpm: snapshot.bpm,
            beat_confidence: snapshot.beat_confidence,
            beat_phase: snapshot.beat_phase,
            onset: snapshot.onset,
            transient: snapshot.transient,
            centroid_hz: snapshot.centroid_hz,
        }
    }
}

#[tauri::command]
fn musical(state: tauri::State<SharedPlayback>) -> MusicalState {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|playback| playback.musical().into())
        .unwrap_or_else(|| player_core::MusicalSnapshot::silent().into())
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

/// Walks `path`, parses every `.milk` file, and merges the result into the
/// preset library (Phase 3 M7c item 3).
///
/// `async` and doing its work in `spawn_blocking`, for `scan_folder`'s reason:
/// a folder of several thousand presets is seconds of file IO and parsing, and
/// on the main thread that is a frozen UI. Nothing the preset library does may
/// reach the audio path or the renderer.
#[tauri::command]
async fn scan_preset_folder(
    path: String,
    state: tauri::State<'_, SharedPresetLibrary>,
) -> Result<player_render::milk::PresetScanSummary, String> {
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

/// Every row in the preset library. Sent whole rather than paged, for
/// `library_tracks`'s reason: a row is a couple of hundred bytes, so even a
/// large collection is a few megabytes read once.
#[tauri::command]
fn preset_rows(state: tauri::State<SharedPresetLibrary>) -> Vec<player_render::milk::PresetRow> {
    state.lock().unwrap().rows().to_vec()
}

#[tauri::command]
fn preset_folders(state: tauri::State<SharedPresetLibrary>) -> Vec<String> {
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
fn forget_preset_folder(
    path: String,
    state: tauri::State<SharedPresetLibrary>,
) -> Result<(), String> {
    state
        .lock()
        .unwrap()
        .forget_folder(&path)
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

/// The visualizer, as the UI sees it: where it is, whether it is up, whether it
/// has the screen, and which visual it is drawing.
///
/// One struct rather than a command per field, for the reason `TransportState`
/// is one struct: the size buttons, the show/hide toggle, the fullscreen button
/// and the gallery are all drawn from a single poll, and four separate round
/// trips would let them disagree about the same window mid-refresh.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VisualizerState {
    width: u32,
    height: u32,
    visible: bool,
    fullscreen: bool,
    /// `VisualKind::id` -- the stable name, never the display one.
    visual: String,
    /// `GainSetting::id`.
    gain: String,
    /// Whether the figure's size still means the signal's level, which is only
    /// true at unity. The panel says so, because a scope that has been scaled
    /// without saying so is worse than one that cannot scale.
    gain_is_measurement: bool,
    /// The selected MilkDrop preset's file name, or `None` when there is none.
    /// Part of the *visualizer's* state rather than the gallery's because it is
    /// selection, and it survives switching to another visual and back
    /// (decision 49).
    preset: Option<String>,
    /// What that preset needs and this build does not do, plus anything that
    /// went wrong compiling it -- one line each, ready to show (decision 56).
    preset_report: Vec<String>,
}

/// One entry in the gallery. Filled from `player_render::VisualKind`, which is
/// the only list of visuals in the project (decision 3): this shell does not
/// keep its own, and neither does the UI.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VisualInfo {
    id: String,
    name: String,
    description: String,
}

/// One display-gain step, out of `player_render::GainSetting` -- the same shape
/// and the same reason as `VisualInfo`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GainInfo {
    id: String,
    label: String,
    /// See `VisualizerState::gain_is_measurement`.
    is_measurement: bool,
}

#[tauri::command]
fn gain_catalog() -> Vec<GainInfo> {
    GainSetting::ALL
        .iter()
        .map(|setting| GainInfo {
            id: setting.id().to_string(),
            label: setting.label().to_string(),
            is_measurement: setting.is_measurement(),
        })
        .collect()
}

/// Sets the display gain (Phase 3 M1, added after the first live fullscreen
/// run). Like `set_visual`, this leaves a request for the redraw thread.
#[tauri::command]
fn set_gain(id: String, visualizer: tauri::State<SharedVisualizer>) -> Result<(), String> {
    let setting = GainSetting::from_id(&id)
        .ok_or_else(|| format!("this build has no gain setting called {id:?}"))?;
    visualizer.request_gain(setting);
    Ok(())
}

/// Loads a MilkDrop preset from a path, or clears the selection with `None`
/// (Phase 3 M7a) -- decision 49's one command per shell.
///
/// Reads the file here rather than in the UI, so that a `.milk` on disk and a
/// `.milk` dropped into a browser reach exactly the same parser. The web half
/// is `WasmRenderer::setPreset`, which takes the text because a browser has no
/// path to give.
///
/// Parsed here as well as on the redraw thread, which is two parses of one
/// file: it is what lets the command answer *now* -- a bad file is an error the
/// person who clicked sees, and a good one comes back with the list of what it
/// needs -- rather than the UI discovering both a frame later by polling. A
/// `.milk` is tens of kilobytes of string work; the milestone's own measurements
/// put every one of the corpus under a millisecond.
#[tauri::command]
fn set_milk_preset(
    path: Option<String>,
    visualizer: tauri::State<SharedVisualizer>,
) -> Result<Vec<String>, String> {
    let Some(path) = path else {
        visualizer.request_preset(None, None, Vec::new(), Vec::new());
        return Ok(Vec::new());
    };
    let path = std::path::PathBuf::from(path);
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // Lossy rather than strict: presets are twenty years of text files written
    // by people on whatever their editor did, and a stray byte in a comment is
    // not a reason to refuse to draw one.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let preset = player_render::milk::Preset::parse(&text)
        .map_err(|e| format!("{} is not a MilkDrop preset: {e}", path.display()))?;

    // The texture pack, read from the preset's own folder (decision 59): one
    // `(name, bytes)` per distinct sampler the shaders name. A name whose file
    // is not found is reported rather than silently dropped -- the visual then
    // draws that layer grey.
    let folder = path.parent().unwrap_or(std::path::Path::new("."));
    let mut report: Vec<String> = Vec::new();
    let mut textures: Vec<(String, Vec<u8>)> = Vec::new();
    for name in preset.texture_names() {
        match find_image(folder, &name).and_then(|p| std::fs::read(&p).ok()) {
            Some(bytes) => textures.push((name, bytes)),
            None => report.push(format!("texture:{name}: image not found beside the preset")),
        }
    }
    // Sprites (decision 59), one image each, keyed by the path the preset wrote
    // in `SpriteName` -- the same string the visual matches by.
    for sprite in &preset.sprites {
        let name = sprite
            .base
            .get("Name")
            .map(String::as_str)
            .unwrap_or("")
            .replace('\\', "/");
        if name.is_empty() {
            continue;
        }
        match find_sprite_image(folder, &name).and_then(|p| std::fs::read(&p).ok()) {
            Some(bytes) => textures.push((name, bytes)),
            None => report.push(format!("sprite:{name}: image not found beside the preset")),
        }
    }
    report.extend(
        player_render::milk::Runtime::new(&preset)
            .faults()
            .iter()
            .map(|f| f.to_string()),
    );
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    visualizer.request_preset(Some(name), Some(text), report.clone(), textures);
    Ok(report)
}

/// Finds the image file for a texture-pack name beside a preset, by stem.
///
/// MilkDrop's convention is a `textures/` (or `sprites/`) folder beside the
/// presets, and MilkDrop 3 treats the two as interchangeable; a collection that
/// keeps its presets in a `presets/` subfolder has the images as a sibling
/// instead, and one that nests presets a level deeper has them an ancestor up.
/// `name` is the stem -- `clouds` finds `textures/clouds.jpg`.
fn find_image(folder: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    const EXTS: &[&str] = &["png", "jpg", "jpeg", "jfif"];
    let mut dirs = Vec::new();
    let mut ancestor = Some(folder);
    for _ in 0..3 {
        if let Some(dir) = ancestor {
            dirs.push(dir.join("textures"));
            dirs.push(dir.join("sprites"));
            ancestor = dir.parent();
        }
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name))
            {
                continue;
            }
            let known_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()));
            if known_ext {
                return Some(path);
            }
        }
    }
    None
}

/// Finds a sprite's image file. `name` is the path the preset wrote in
/// `SpriteName` (`sprites/x1.png`), resolved against the preset's folder and its
/// ancestors -- the same walk as [`find_image`], but the name already carries
/// its folder, so it is tried as a path rather than a stem.
fn find_sprite_image(folder: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let rel = std::path::Path::new(name);
    let mut ancestor = Some(folder);
    for _ in 0..3 {
        if let Some(dir) = ancestor {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
            ancestor = dir.parent();
        }
    }
    None
}

#[tauri::command]
fn visual_catalog() -> Vec<VisualInfo> {
    VisualKind::ALL
        .iter()
        .map(|kind| VisualInfo {
            id: kind.id().to_string(),
            name: kind.name().to_string(),
            description: kind.description().to_string(),
        })
        .collect()
}

#[tauri::command]
fn visualizer_state(
    app: tauri::AppHandle,
    visualizer: tauri::State<SharedVisualizer>,
) -> VisualizerState {
    let window = app.get_window(VISUALIZER_LABEL);
    let requested_preset = visualizer.requested_preset();
    let size = window
        .as_ref()
        .and_then(|w| w.inner_size().ok())
        .unwrap_or(tauri::PhysicalSize {
            width: VISUALIZER_DEFAULT_SIZE as u32,
            height: VISUALIZER_DEFAULT_SIZE as u32,
        });
    VisualizerState {
        width: size.width,
        height: size.height,
        // A window that failed to report is treated as hidden rather than
        // shown: the toggle then offers to bring it back, which is the useful
        // direction to be wrong in. Fullscreen reads the same way -- and both
        // are *asked of the window* rather than mirrored beside it, since
        // several parties change them (the UI, the window's own close button,
        // Esc, focus loss) and a mirror of all of those eventually disagrees.
        visible: window
            .as_ref()
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false),
        fullscreen: window.and_then(|w| w.is_fullscreen().ok()).unwrap_or(false),
        visual: visualizer.requested().id().to_string(),
        gain: visualizer.requested_gain().id().to_string(),
        gain_is_measurement: visualizer.requested_gain().is_measurement(),
        preset: requested_preset.name,
        preset_report: requested_preset.report,
    }
}

/// Resizes the visualizer window. The renderer notices on its own -- its redraw
/// loop polls the window's size (see `setup` below) -- so this only has to move
/// the window.
#[tauri::command]
fn set_visualizer_size(width: u32, height: u32, app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_window(VISUALIZER_LABEL)
        .ok_or_else(|| "the visualizer window is not open".to_string())?;
    window
        .set_size(tauri::Size::Physical(tauri::PhysicalSize {
            width: width.max(160),
            height: height.max(160),
        }))
        .map_err(|e| e.to_string())
}

/// Shows or hides the visualizer window. Hiding rather than closing it: the
/// renderer holds the surface built from this window's handle, and closing it
/// would leave a renderer drawing into nothing (see `VisualizerWindow`).
/// Pressing the window's own close button ends up here too, by way of the
/// handler in `setup`.
#[tauri::command]
fn set_visualizer_visible(visible: bool, app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_window(VISUALIZER_LABEL)
        .ok_or_else(|| "the visualizer window is not open".to_string())?;
    if visible {
        window.show().map_err(|e| e.to_string())
    } else {
        // Fullscreen *and* hidden is a state nothing can get out of by looking
        // at it, and it would leave Esc registered over a window nobody can
        // see. Putting the visualizer away therefore gives the screen back
        // first.
        leave_fullscreen(&app);
        window.hide().map_err(|e| e.to_string())
    }
}

/// Takes the whole screen, or gives it back (Phase 3 M1, decision 4).
///
/// Borderless fullscreen of the window that already exists -- no second window,
/// no surface rebuilt, and the redraw thread's existing size poll handles the
/// new geometry with no new code. What this *does* have to arrange is the way
/// back out: see [`fullscreen_exit_key`] for why Esc has to be a global
/// shortcut here, and `setup` for the focus-loss half.
#[tauri::command]
fn set_visualizer_fullscreen(fullscreen: bool, app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_window(VISUALIZER_LABEL)
        .ok_or_else(|| "the visualizer window is not open".to_string())?;
    if fullscreen {
        // Fullscreen of a hidden window is a screen of nothing, and the redraw
        // thread parks while the window is hidden -- so showing it is part of
        // what this command means rather than a courtesy.
        window.show().map_err(|e| e.to_string())?;
        window.set_fullscreen(true).map_err(|e| e.to_string())?;
        // Focus is what makes the focus-loss exit work at all: a window that
        // never had focus cannot lose it, and Esc would then be registered
        // globally with nothing to unregister it.
        let _ = window.set_focus();
        register_fullscreen_exit(&app);
    } else {
        leave_fullscreen(&app);
    }
    Ok(())
}

/// Chooses the visual (decision 3's id, decision 5's swap).
///
/// This does not touch the renderer: the redraw thread owns it, and a scene
/// built on this thread would be a second thread creating GPU objects for a
/// device it does not drive. What lands here is the *request*, which that
/// thread applies between two frames.
#[tauri::command]
fn set_visual(id: String, visualizer: tauri::State<SharedVisualizer>) -> Result<(), String> {
    let kind = VisualKind::from_id(&id)
        .ok_or_else(|| format!("this build has no visual called {id:?}"))?;
    visualizer.request(kind);
    Ok(())
}

/// Registers Esc for as long as the visualizer holds the screen.
///
/// **The handler does no work of its own, and that is not tidiness -- it is the
/// difference between Esc working and the app hanging.** Found live 2026-08-18
/// on the desktop shell: fullscreen worked, and Esc froze the whole application
/// with no error in the log, every time.
///
/// The cause is structural, in `tauri-plugin-global-shortcut` 2.3.2, rather than
/// a mistake to correct here. Its dispatcher looks the shortcut up in an
/// `Arc<Mutex<HashMap>>` and calls the handler **while still holding that lock**
/// (its `lib.rs:417` -- the guard is a temporary that lives to the end of the
/// `if let`). `unregister` takes that same mutex (its `lib.rs:188`). So
/// unregistering from inside a handler is a thread waiting for a lock it is
/// already holding, and `std::sync::Mutex` is not reentrant. The same call also
/// routes through the plugin's `run_main_thread!`, which blocks on a channel
/// until the main thread runs the task -- so if the handler is *on* the main
/// thread, it waits for itself a second way. Neither shows up as an error: the
/// process just stops answering, which is exactly what was reported.
///
/// Handing the exit to a thread of its own answers both halves: the handler
/// returns immediately, the dispatcher drops its lock, and the spawned thread's
/// `unregister` then takes it uncontended and asks a main thread that is free
/// again. It is the same shape as `VisualizerWindow::stop`, which may not run on
/// the main thread for its own version of this reason.
fn register_fullscreen_exit(app: &tauri::AppHandle) {
    let handle = app.clone();
    if let Err(e) =
        app.global_shortcut()
            .on_shortcut(fullscreen_exit_key(), move |_app, _shortcut, event| {
                // Both the press and the release arrive; acting on the press alone
                // keeps one keystroke from being two exits.
                if event.state() == ShortcutState::Pressed {
                    let handle = handle.clone();
                    std::thread::spawn(move || leave_fullscreen(&handle));
                }
            })
    {
        // Not fatal: fullscreen still works, and the UI's own toggle still comes
        // back out of it. Worth logging loudly, because the *reason* anyone
        // would find themselves stuck is exactly this line having failed.
        log::warn!("could not register Esc to leave fullscreen: {e}");
    }
}

/// Gives the screen back, and takes Esc back with it.
///
/// Idempotent on purpose: it is reached from the UI's toggle, from Esc itself,
/// from the window losing focus and from hiding the visualizer, and any of
/// those can arrive when the visualizer is already windowed.
///
/// **Never call this from inside a global-shortcut handler** -- see
/// `register_fullscreen_exit` for the deadlock that causes and why the handler
/// spawns a thread instead. Every other caller is fine: a command runs on the
/// async runtime's pool, and the window-event handlers run on the main thread,
/// where the plugin's main-thread hop resolves inline (`send_user_message`
/// checks the thread id before using the proxy).
fn leave_fullscreen(app: &tauri::AppHandle) {
    if let Some(window) = app.get_window(VISUALIZER_LABEL) {
        if let Err(e) = window.set_fullscreen(false) {
            log::warn!("leaving fullscreen failed: {e}");
        }
    }
    // Unconditional rather than "if we believe it is registered": this is the
    // one thing that must not be left behind, since a stray Esc handler is a
    // key taken away from every other application on the machine.
    if let Err(e) = app.global_shortcut().unregister(fullscreen_exit_key()) {
        log::debug!("no Esc shortcut to unregister: {e}");
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
        // Esc out of fullscreen, and nothing else (Phase 3 M1). The shortcut
        // itself is registered and unregistered around fullscreen rather than
        // held for the session -- see `fullscreen_exit_key`.
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
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
            musical,
            goniometer,
            visualizer_state,
            set_visualizer_size,
            set_visualizer_visible,
            set_visualizer_fullscreen,
            set_visual,
            set_milk_preset,
            visual_catalog,
            set_gain,
            gain_catalog,
            scan_folder,
            library_tracks,
            library_folders,
            forget_folder,
            playlists,
            save_playlist,
            delete_playlist,
            scan_preset_folder,
            preset_rows,
            preset_folders,
            forget_preset_folder,
            load_server_track,
            set_next_server_track,
            servers::server_random_tracks,
            servers::server_browse,
            servers::server_playlists,
            servers::server_mark,
            servers::server_scrobble_tick,
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
            app.manage(SharedPresetLibrary::new(Mutex::new(
                player_render::milk::PresetLibrary::open(&data_dir)?,
            )));

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
            // The scrobble tracker (M4). Its own state for the same reason: it
            // outlives any one track, and it is consulted from the transport
            // poll rather than from anything that holds the connection.
            app.manage(servers::SharedScrobbler::default());
            // Why the connection stopped working, when it did (M5). Its own
            // state because it outlives the connection it describes -- that is
            // the whole point: the connection is gone and this says why.
            app.manage(servers::SharedFailure::default());
            // Reconnecting at startup happens off the main thread: it is a
            // network round trip, and a server that is not there takes the
            // request timeout to say so -- which on this thread would be the
            // window not appearing for twenty seconds.
            std::thread::spawn(move || servers::reconnect_stored(&data_dir, &server));

            // Second, plain (no-webview) window hosting the wgpu-rendered
            // visualizer. Built and configured here, on the main thread -- wgpu
            // requires Metal surface creation to happen there, so doing it
            // unconditionally keeps this portable rather than only
            // "working by luck" on Linux/Windows. Only the redraw loop
            // below moves to a background thread.
            let visualizer_window = tauri::window::WindowBuilder::new(app, VISUALIZER_LABEL)
                .title("latentPlayer -- visualizer")
                .inner_size(VISUALIZER_DEFAULT_SIZE, VISUALIZER_DEFAULT_SIZE)
                // Resizable since M6: the UI sets the size through
                // `set_visualizer_size`, and dragging the window's own edge has to
                // work for the same reason. Phase 0 pinned this shut because
                // the renderer couldn't survive a resize; it can now, and the
                // size-poll below is what tells it.
                .resizable(true)
                .build()?;
            let size = visualizer_window.inner_size()?;

            // Managed since Phase 3 M1, and only for the selection: the window's
            // *lifecycle* is still the shell's own business, and the UI's part
            // in it is `set_visualizer_visible`, which speaks to the window
            // rather than to this. What the UI does now own is which visual is
            // drawn, and that request has to reach the redraw thread from a
            // command (decision 1: selection is the host's).
            let visualizer: SharedVisualizer = Arc::new(VisualizerWindow::new());
            app.manage(Arc::clone(&visualizer));

            // The close button hides the window instead of destroying it, which
            // is the whole of the lifecycle decision written up on
            // `VisualizerWindow`: the renderer cannot survive its window going away,
            // and every user pressing that button means "put it away", not
            // "tear down the visualiser for the rest of this session".
            {
                let handle = app.handle().clone();
                let visualizer = Arc::clone(&visualizer);
                visualizer_window.on_window_event(move |event| match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        // Via the command rather than `window.hide()` directly,
                        // so this button and the UI's toggle are one behaviour
                        // with two entry points -- including giving the screen
                        // back if the visualizer was fullscreen.
                        if let Err(e) = set_visualizer_visible(false, handle.clone()) {
                            log::warn!("hiding the visualizer window failed: {e}");
                        }
                    }
                    // Fullscreen's second way out (Phase 3 M1, decision 4).
                    //
                    // Two jobs in one line. It is the graceful answer to
                    // alt-tabbing away from a fullscreen visualizer -- the
                    // window gives the screen back rather than sitting on top of
                    // whatever the user switched to. And it is what guarantees
                    // the Esc shortcut, which is *global* while fullscreen is
                    // active, is never registered while another application is
                    // frontmost: losing focus is exactly the moment that would
                    // start being true.
                    tauri::WindowEvent::Focused(false) => {
                        leave_fullscreen(&handle);
                    }
                    // Not reachable while the app is running, given the above --
                    // but if anything ever does destroy this window, the redraw
                    // thread has to hear about it before it draws again.
                    tauri::WindowEvent::Destroyed => {
                        visualizer.running.store(false, Ordering::Release);
                    }
                    _ => {}
                });
            }

            // Closing the *main* window ends the app, and does it in an order
            // that leaves nothing drawing into a window that is going away: stop
            // the redraw thread first, exit second. Without this the app would
            // not even quit -- Tauri exits when its last window closes, and the
            // visualizer window (which no longer closes) is a window, so the UI
            // would disappear leaving the process and the visualizer behind it.
            //
            // On a thread of its own because `VisualizerWindow::stop` may not run on
            // the main thread: see its own note.
            if let Some(main_window) = app.get_window("main") {
                let handle = app.handle().clone();
                let visualizer = Arc::clone(&visualizer);
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let handle = handle.clone();
                        let visualizer = Arc::clone(&visualizer);
                        std::thread::spawn(move || {
                            visualizer.stop();
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
            let size_probe = visualizer_window.clone();

            let mut renderer = pollster::block_on(player_render::Renderer::new(
                visualizer_window,
                size.width,
                size.height,
                visualizer.requested(),
            ))
            .map_err(|e| format!("visualizer renderer init failed: {e}"))?;

            let shared = app.state::<SharedPlayback>().inner().clone();
            let redraw_visualizer = Arc::clone(&visualizer);
            let redraw = std::thread::spawn(move || {
                let mut last_size = size;
                // What the frame's `elapsed_seconds` reports. Measured rather
                // than assumed to be the sleep below: a frame that took longer
                // than 16ms has to say so, or anything timed off it (the
                // automatic gain's release, and every animated visual after M1)
                // runs slow in exactly the moments it matters.
                let mut last_frame = std::time::Instant::now();
                // Which preset generation the renderer is holding. Zero is the
                // "none selected" the renderer starts in, so a first selection
                // is a change and re-selecting the same file still reloads it
                // -- which is what someone editing a preset expects.
                let mut last_preset = 0u64;
                while redraw_visualizer.running.load(Ordering::Acquire) {
                    // A hidden visualizer is drawn for nobody, so this parks instead
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
                    // Windows (2026-08-15): resizing the window threw a
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
                        // to get here (see `VisualizerWindow`); this is what happens
                        // if something does.
                        Err(e) => {
                            log::warn!("the visualizer window stopped answering, so it stops: {e}");
                            return;
                        }
                    }

                    // Selection, applied between two frames on the one thread
                    // that owns the renderer (decision 5). The swap is a scene
                    // rebuild and nothing more -- the window, the surface and
                    // this loop are all untouched by it.
                    let wanted_visual = redraw_visualizer.requested();
                    if wanted_visual != renderer.visual() {
                        renderer.set_visual(wanted_visual);
                    }
                    // Display gain, the same way. Cheap enough to set every
                    // frame, but only set when it changed so the automatic
                    // mode's own state is never disturbed by a no-op.
                    let wanted_gain = redraw_visualizer.requested_gain();
                    if wanted_gain != renderer.gain() {
                        renderer.set_gain(wanted_gain);
                    }
                    // The selected preset, by generation rather than by value.
                    // A failure here is logged and dropped rather than stopping
                    // the loop: the command already rejected an unparseable
                    // file, so anything reaching this is a surprise, and a
                    // surprise should not take the visualizer down with it.
                    let wanted_preset = redraw_visualizer.requested_preset();
                    if wanted_preset.generation != last_preset {
                        last_preset = wanted_preset.generation;
                        if let Err(e) = renderer.set_preset(wanted_preset.text.as_deref()) {
                            log::warn!("the selected preset would not load: {e}");
                        }
                        // The texture pack, after the text: the visual decodes
                        // and binds it once the preset's own name list is known.
                        renderer.set_textures(&wanted_preset.textures);
                    }

                    // The analysis feed (decision 2). Assembled here, off the
                    // audio thread, out of what the tap and the meters already
                    // hold -- no visual computes anything about the audio.
                    //
                    // How much history to hand over is a duration, not a sample
                    // count (M6 decision 1), so it depends on the rate the
                    // device opened at -- which is why this asks the engine
                    // rather than using a constant.
                    //
                    // M2 added the spectrum to the feed, for the bars: the
                    // meters' own levels and the axis they are against, read
                    // inside the same guard as the samples so a frame still
                    // costs one lock rather than two. M5 added the musical
                    // figures on the same terms -- and out of the same
                    // publication, so the beat a scene draws belongs to the
                    // audio in the same frame.
                    let (samples, channels, rate, levels, centres, musical) = {
                        let guard = shared.lock().unwrap();
                        match guard.as_ref() {
                            Some(p) => {
                                let channels = p.output_channels();
                                let rate = p.output_sample_rate();
                                let wanted = player_render::window_samples(rate, channels);
                                (
                                    p.scope_window(wanted),
                                    channels,
                                    rate,
                                    p.spectrum(),
                                    p.spectrum_band_centres().to_vec(),
                                    p.musical(),
                                )
                            }
                            None => (
                                Vec::new(),
                                2,
                                48_000,
                                Vec::new(),
                                Vec::new(),
                                player_core::MusicalSnapshot::silent(),
                            ),
                        }
                    };
                    let elapsed = last_frame.elapsed().as_secs_f32();
                    last_frame = std::time::Instant::now();
                    if let Err(e) = renderer.draw_frame(
                        &VisualFrame::new(&samples, channels, rate, elapsed)
                            .with_spectrum(&levels, &centres)
                            .with_musical(Musical {
                                bpm: musical.bpm,
                                beat_confidence: musical.beat_confidence,
                                beat_phase: musical.beat_phase,
                                onset: musical.onset,
                                transient: musical.transient,
                                centroid_hz: musical.centroid_hz,
                            }),
                    ) {
                        log::warn!("visualizer draw_frame error: {e}");
                    }
                    std::thread::sleep(Duration::from_millis(16));
                }
            });
            // Handed over so shutdown can wait for this thread rather than
            // racing it (`VisualizerWindow::stop`).
            *visualizer.redraw.lock().unwrap() = Some(redraw);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::find_image;

    /// Builds `base/presets/p.milk`, `base/textures/clouds.jpg` and answers
    /// with the `presets` folder, the shape a MilkDrop collection takes.
    fn make_collection() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("milk-find-image-{}", std::process::id()));
        let presets = base.join("presets");
        let textures = base.join("textures");
        std::fs::create_dir_all(&presets).unwrap();
        std::fs::create_dir_all(&textures).unwrap();
        std::fs::write(presets.join("p.milk"), b"unused").unwrap();
        std::fs::write(textures.join("clouds.jpg"), b"unused").unwrap();
        presets
    }

    #[test]
    fn finds_a_texture_pack_image_in_the_sibling_folder() {
        let presets = make_collection();
        let found = find_image(&presets, "clouds").expect("clouds.jpg is found");
        assert!(found.ends_with("clouds.jpg"), "{found:?}");
        assert!(find_image(&presets, "missing").is_none());
    }
}
