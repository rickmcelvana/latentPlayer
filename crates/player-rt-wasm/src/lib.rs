//! Web glue for `player-core` (decode) and `player-render` (the wgpu
//! scope). Mirrors `../latent-mixing/crates/mixer-rt-wasm`'s role, but
//! smaller: a plain player has no per-block DSP to run in wasm the way a
//! mixer does, so the only wasm entry points are "give me more audio" (once
//! per ring refill, on a Worker -- not per audio block) and "drive the
//! renderer" (once per animation frame). The AudioWorkletProcessor itself
//! (app/src/worklet/player-worklet.ts) is plain TS reading directly off the
//! SharedArrayBuffer -- there's no per-block wasm call on the audio path at
//! all, deliberately simpler than the mixer's ring-buffer+wasm design
//! because there's no real-time mixing/DSP happening here.

use js_sys::Float32Array;
use player_core::eq::{self, EqCoefficients, EqSettings, EQ_BANDS};
use player_core::resample::StreamingResampler;
use player_core::{
    adapt_channels, read_tags, ChannelShape, InMemorySource, Meters, StreamingDecoder, TrackTags,
    GONIOMETER_POINTS,
};
use player_render::{GainSetting, VisualFrame, VisualKind};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// One file's tags, read by the same `player_core::tags` the desktop shell
/// calls directly (M7 decision 1).
///
/// Exposed as getters rather than serialised into a JS object, so no
/// serialisation dependency is needed on either side and the cover's bytes are
/// only copied across if something actually asks for them -- which, for a scan
/// of several thousand files where most covers are duplicates of each other,
/// is most of the cost avoided.
#[wasm_bindgen]
pub struct WasmTags {
    inner: TrackTags,
}

#[wasm_bindgen]
impl WasmTags {
    /// Reads the tags of a file the browser handed over. Probes the container
    /// but decodes no audio.
    #[wasm_bindgen(js_name = read)]
    pub fn read(bytes: Vec<u8>, extension_hint: Option<String>) -> Result<WasmTags, JsValue> {
        let source = InMemorySource::new(bytes, extension_hint);
        let inner = read_tags(&source).map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(WasmTags { inner })
    }

    #[wasm_bindgen(getter)]
    pub fn title(&self) -> Option<String> {
        self.inner.title.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn artist(&self) -> Option<String> {
        self.inner.artist.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn album(&self) -> Option<String> {
        self.inner.album.clone()
    }

    #[wasm_bindgen(getter, js_name = albumArtist)]
    pub fn album_artist(&self) -> Option<String> {
        self.inner.album_artist.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn genre(&self) -> Option<String> {
        self.inner.genre.clone()
    }

    #[wasm_bindgen(getter, js_name = trackNumber)]
    pub fn track_number(&self) -> Option<u32> {
        self.inner.track_number
    }

    #[wasm_bindgen(getter, js_name = discNumber)]
    pub fn disc_number(&self) -> Option<u32> {
        self.inner.disc_number
    }

    #[wasm_bindgen(getter)]
    pub fn year(&self) -> Option<u32> {
        self.inner.year
    }

    /// Length in seconds, or `None` when the container doesn't state one --
    /// the same `Option` `StreamingDecoder::duration_seconds` reports, kept as
    /// an `Option` here because a library row showing `0:00` and a row showing
    /// "unknown" are different things.
    #[wasm_bindgen(getter, js_name = durationSeconds)]
    pub fn duration_seconds(&self) -> Option<f64> {
        self.inner.duration_seconds
    }

    #[wasm_bindgen(getter, js_name = sampleRate)]
    pub fn sample_rate(&self) -> Option<u32> {
        self.inner.sample_rate
    }

    #[wasm_bindgen(getter)]
    pub fn channels(&self) -> Option<u16> {
        self.inner.channels
    }

    #[wasm_bindgen(getter, js_name = coverMediaType)]
    pub fn cover_media_type(&self) -> Option<String> {
        self.inner
            .cover
            .as_ref()
            .and_then(|cover| cover.media_type.clone())
    }

    /// The embedded image's bytes, if it has one. Copies, so callers ask once
    /// and keep the result.
    #[wasm_bindgen(getter, js_name = coverData)]
    pub fn cover_data(&self) -> Option<Vec<u8>> {
        self.inner.cover.as_ref().map(|cover| cover.data.clone())
    }
}

/// A decoder held open across the life of a track, driven from worker.ts.
///
/// Phase 0 and M1 exposed a one-shot `decode()` that returned the entire
/// track, and the Worker copied all of it into a `SharedArrayBuffer` sized to
/// the file. That is the web half of what M2 removes: an hour-long FLAC meant
/// an hour-long SAB, allocated before a note played. Now the Worker keeps this
/// object and tops up a small ring instead.
///
/// Since M3 it also converts to the output's shape before handing anything
/// over, exactly as the native decoder thread does (`player-core`'s
/// `playback.rs`): `read` returns audio at the caller's rate and channel
/// count, never the file's. A `AudioContext`'s sample rate cannot be changed
/// once it exists, so a queue that played each track at its own rate would
/// have to tear the context down between tracks -- gapless except when the
/// rate changes, which is not a property worth shipping.
#[wasm_bindgen]
pub struct WasmDecoder {
    inner: StreamingDecoder,
    resampler: StreamingResampler,
    shape: ChannelShape,
    /// Resampler output, still in the file's channel layout.
    resampled: Vec<f32>,
    /// Output-shaped audio not yet handed to JS. Needed because a decoded
    /// block is whatever size the codec's packet happens to be, while `read`
    /// is asked for exactly the space the caller has -- which near the end of
    /// a ring is a partial block.
    carry: Vec<f32>,
    carry_pos: usize,
    source_done: bool,
}

#[wasm_bindgen]
impl WasmDecoder {
    /// Opens `bytes` (a whole file, already read into memory by the browser's
    /// file picker -- a browser has no path to stream from) and reads far
    /// enough to know its format. Decodes no audio yet.
    ///
    /// `out_rate` and `out_channels` are the output's shape, fixed for the
    /// session by the `AudioContext`, and every track is converted to them.
    ///
    /// Not a `#[wasm_bindgen(constructor)]` so the name matches
    /// `WasmRenderer::create` below.
    #[wasm_bindgen(js_name = create)]
    pub fn create(
        bytes: Vec<u8>,
        extension_hint: Option<String>,
        out_rate: u32,
        out_channels: u16,
    ) -> Result<WasmDecoder, JsValue> {
        let source = InMemorySource::new(bytes, extension_hint);
        let inner =
            StreamingDecoder::open(&source).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let src_channels = inner.channels().max(1);
        let resampler = StreamingResampler::new(src_channels, inner.sample_rate(), out_rate)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(WasmDecoder {
            inner,
            resampler,
            shape: ChannelShape {
                src_channels,
                out_channels: out_channels.max(1),
            },
            resampled: Vec::new(),
            carry: Vec::new(),
            carry_pos: 0,
            source_done: false,
        })
    }

    /// Channels `read` produces -- the output's, not the file's.
    #[wasm_bindgen(getter)]
    pub fn channels(&self) -> u16 {
        self.shape.out_channels
    }

    /// Channels the file itself has, before adaptation. For display.
    #[wasm_bindgen(getter)]
    pub fn source_channels(&self) -> u16 {
        self.shape.src_channels
    }

    #[wasm_bindgen(getter)]
    pub fn source_sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    /// Track length in seconds, or 0 when the container doesn't state one.
    /// `Option<f64>` would cross the boundary as `undefined`, which reads no
    /// better on the JS side than an explicit zero.
    #[wasm_bindgen(getter)]
    pub fn duration_seconds(&self) -> f64 {
        self.inner.duration_seconds().unwrap_or(0.0)
    }

    /// True once every sample has been handed over.
    #[wasm_bindgen(getter)]
    pub fn is_finished(&self) -> bool {
        self.source_done && self.carry_pos >= self.carry.len()
    }

    /// Decodes into `dst` at `offset`, writing at most `max_samples`
    /// interleaved samples, and returns how many it wrote.
    ///
    /// Writing "at most" rather than "a block" is what lets the caller hand
    /// over exactly the contiguous space left in its ring, and call again
    /// after the wrap -- the split is JS's business, and a decoded block that
    /// doesn't fit stays here until it does.
    ///
    /// Zero means either end of stream (check `is_finished`) or a block
    /// boundary with no room.
    pub fn read(
        &mut self,
        dst: &Float32Array,
        offset: u32,
        max_samples: u32,
    ) -> Result<u32, JsValue> {
        if max_samples == 0 {
            return Ok(0);
        }
        if self.carry_pos >= self.carry.len() {
            self.refill()?;
        }
        if self.carry_pos >= self.carry.len() {
            return Ok(0);
        }

        let available = self.carry.len() - self.carry_pos;
        let n = available.min(max_samples as usize);
        let end = self.carry_pos + n;
        let view = dst.subarray(offset, offset + n as u32);
        view.copy_from(&self.carry[self.carry_pos..end]);
        self.carry_pos = end;
        Ok(n as u32)
    }

    /// Jumps to `seconds`, returning the position actually landed on. Drops
    /// any audio decoded before the seek that hadn't been read yet -- it
    /// belongs to where the track no longer is.
    pub fn seek(&mut self, seconds: f64) -> Result<f64, JsValue> {
        self.carry.clear();
        self.carry_pos = 0;
        self.resampled.clear();
        // The samples either side of a seek are unrelated, so the filter's
        // history has to go with them.
        self.resampler.reset();
        self.source_done = false;
        self.inner
            .seek(seconds)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

impl WasmDecoder {
    /// Decodes one block, converts it to the output's shape, and leaves it in
    /// `carry`. Not exported: `read` is the only way in from JS.
    fn refill(&mut self) -> Result<(), JsValue> {
        self.carry.clear();
        self.carry_pos = 0;
        self.resampled.clear();
        if self.source_done {
            return Ok(());
        }
        match self.inner.next_block() {
            Ok(Some(block)) => self
                .resampler
                .process(block, &mut self.resampled)
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            Ok(None) => {
                // The tail the resampler is still holding belongs to this
                // track and has to be handed over before the next track's
                // first sample is, or the gapless seam swallows it.
                self.resampler
                    .finish(&mut self.resampled)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                self.source_done = true;
            }
            Err(e) => return Err(JsValue::from_str(&e.to_string())),
        }
        adapt_channels(&self.resampled, &self.shape, &mut self.carry);
        Ok(())
    }
}

/// The meters, driven from worker.ts -- the web half of M4's analysis tap.
///
/// Deliberately knows nothing about the SharedArrayBuffer: the worker reads its
/// own tap region (it owns that layout, see app/src/audioSab.ts) and hands the
/// samples over, exactly as it hands the decoder a slice of ring to write into.
/// What happens to those samples is `player_core::Meters`, the same code the
/// native analysis thread runs, so the two shells cannot measure differently.
///
/// It runs in the worker rather than the worklet for the reason M4's decision 2
/// gives: an overrunning meter must degrade into a stale reading, never into a
/// dropout, and the worklet *is* the audio thread here.
#[wasm_bindgen]
pub struct WasmAnalyzer {
    meters: Meters,
}

#[wasm_bindgen]
impl WasmAnalyzer {
    /// `sample_rate`/`channels` are the output's -- what the tap holds.
    #[wasm_bindgen(js_name = create)]
    pub fn create(sample_rate: u32, channels: u16) -> WasmAnalyzer {
        WasmAnalyzer {
            meters: Meters::new(sample_rate, channels),
        }
    }

    /// Adds audio drained from the tap. `discontinuity` says these samples do
    /// not continue the previous ones -- the reader was lapped, or a seek
    /// cleared the tap under it.
    pub fn ingest(&mut self, samples: &[f32], discontinuity: bool) {
        self.meters.ingest(samples, discontinuity);
    }

    /// Runs the meters and returns the levels, one dBFS value per band.
    /// `dt_seconds` is real elapsed time, so the decay looks the same here as
    /// it does natively whatever the worker's cadence happens to be.
    pub fn update(&mut self, dt_seconds: f32) -> Vec<f32> {
        self.meters.update(dt_seconds).to_vec()
    }

    /// Centre frequency of each band. Fixed for the session.
    #[wasm_bindgen(js_name = bandCentres)]
    pub fn band_centres(&self) -> Vec<f32> {
        self.meters.band_centres().to_vec()
    }

    /// Loudness and phase as five floats, in `MeterSnapshot::to_array`'s order
    /// -- which `METER` in app/src/audioSab.ts mirrors, since that is the
    /// layout the worker copies them into.
    ///
    /// A flat array rather than a struct because wasm-bindgen would have to
    /// build a JS object per call otherwise, and this is read at frame rate;
    /// the naming happens once, on the TypeScript side.
    pub fn meters(&self) -> Vec<f32> {
        self.meters.snapshot().to_array().to_vec()
    }

    /// The goniometer's points for the window just ingested: interleaved
    /// side/mid pairs (`player_core::phase::goniometer_points`).
    pub fn goniometer(&self) -> Vec<f32> {
        self.meters.goniometer(GONIOMETER_POINTS)
    }
}

/// The EQ's design side, driven from worker.ts (M8).
///
/// This is the half of the equaliser that stays in Rust on both shells: what a
/// slider position means, what each band's shape and Q are, and what curve the
/// whole cascade adds up to. The *application* of the resulting coefficients is
/// the AudioWorklet's, in TypeScript, because on this shell the audio path is
/// TypeScript -- see app/src/eqFilter.ts, which is held to `EqProcessor` sample
/// for sample by `/eq.html`.
///
/// Free functions rather than a struct: there is no state to keep. Coefficients
/// are designed when a slider moves and written straight into the shared buffer
/// the worklet reads.
#[wasm_bindgen(js_name = eqCoefficients)]
pub fn eq_coefficients(
    enabled: bool,
    preamp_db: f32,
    gains_db: &[f32],
    sample_rate: u32,
) -> Vec<f64> {
    EqCoefficients::design(&settings_from(enabled, preamp_db, gains_db), sample_rate)
        .to_array()
        .to_vec()
}

/// The curve the UI draws: gain in dB at each of [`eq_response_frequencies`].
///
/// Computed when a setting changes rather than per frame, and in the same Rust
/// the filters come from -- a display drawn from a second implementation of the
/// response would eventually disagree with what is being heard.
#[wasm_bindgen(js_name = eqResponse)]
pub fn eq_response(enabled: bool, preamp_db: f32, gains_db: &[f32], sample_rate: u32) -> Vec<f32> {
    eq::magnitude_response(&settings_from(enabled, preamp_db, gains_db), sample_rate)
}

/// The frequencies that curve is reported against.
#[wasm_bindgen(js_name = eqResponseFrequencies)]
pub fn eq_response_frequencies() -> Vec<f32> {
    eq::response_frequencies()
}

/// The sliders' centre frequencies, for their labels.
#[wasm_bindgen(js_name = eqBandFrequencies)]
pub fn eq_band_frequencies() -> Vec<f32> {
    eq::BAND_FREQUENCIES.to_vec()
}

/// The settings as the engine will actually use them: the preamp followed by
/// the ten band gains, clamped.
///
/// The desktop shell's `set_eq` returns this as part of its answer, so the web
/// build has to as well -- a display that showed the +99 someone typed while
/// the audio ran +12 would be lying about the same thing on one shell and not
/// the other.
#[wasm_bindgen(js_name = eqClamped)]
pub fn eq_clamped(preamp_db: f32, gains_db: &[f32]) -> Vec<f32> {
    let settings = settings_from(true, preamp_db, gains_db);
    let mut out = Vec::with_capacity(1 + EQ_BANDS);
    out.push(settings.preamp_db);
    out.extend_from_slice(&settings.gains_db);
    out
}

/// Runs the Rust EQ over `samples` and hands back the result.
///
/// Exists for `/eq.html`, which holds app/src/eqFilter.ts to it sample for
/// sample. That comparison is the whole justification for M8's one departure
/// from "the DSP is Rust on both shells": the difference equation is written
/// twice, so something has to prove the two copies agree, and a claim that they
/// look the same is not that.
#[wasm_bindgen(js_name = eqProcess)]
pub fn eq_process(
    samples: &[f32],
    channels: u16,
    enabled: bool,
    preamp_db: f32,
    gains_db: &[f32],
    sample_rate: u32,
) -> Vec<f32> {
    let settings = settings_from(enabled, preamp_db, gains_db);
    let mut processor = player_core::EqProcessor::new(sample_rate, channels);
    processor.set(EqCoefficients::design(&settings, sample_rate));
    let mut audio = samples.to_vec();
    processor.process(&mut audio);
    audio
}

/// How far a slider or the preamp may travel, in dB either way.
#[wasm_bindgen(js_name = eqMaxGainDb)]
pub fn eq_max_gain_db() -> f32 {
    eq::MAX_GAIN_DB
}

/// The settings a caller sent, with a short or long list of gains taken as far
/// as it goes -- the same forgiveness `set_eq` shows on the desktop shell, and
/// for the same reason: this is a boundary where a length mismatch is otherwise
/// a silent failure.
fn settings_from(enabled: bool, preamp_db: f32, gains_db: &[f32]) -> EqSettings {
    let mut gains = [0.0f32; EQ_BANDS];
    for (slot, &db) in gains.iter_mut().zip(gains_db.iter()) {
        *slot = db;
    }
    EqSettings {
        enabled,
        preamp_db,
        gains_db: gains,
    }
    .clamped()
}

/// wasm-bindgen wrapper around `player_render::Renderer` -- the visualizer
/// host, the same one `src-tauri` drives natively: same registry, same shaders,
/// same pipelines, same draw call (PLAN.md §5/§8.1's whole point, and Phase 3
/// M1's decision 1 for everything above it).
///
/// What this crate deliberately does *not* add is a second opinion about
/// anything. Selection is by `VisualKind::id`, the gallery comes from
/// [`visual_catalog`], and the frame is the same `VisualFrame` the desktop
/// shell assembles -- so the two shells cannot offer different visuals, name
/// them differently, or feed them differently.
#[wasm_bindgen]
pub struct WasmRenderer {
    inner: player_render::Renderer,
}

#[wasm_bindgen]
impl WasmRenderer {
    /// Not a `#[wasm_bindgen(constructor)]` because it's async -- wasm-bindgen
    /// constructors can't be. JS calls this as a plain async function and
    /// gets a `Promise<WasmRenderer>`.
    ///
    /// `visual` is a `VisualKind::id`; an unknown one is an error rather than a
    /// silent fallback, so a stored selection this build does not have is
    /// something the caller decides about (see `visualizer.ts`).
    #[wasm_bindgen(js_name = create)]
    pub async fn create(
        canvas: HtmlCanvasElement,
        width: u32,
        height: u32,
        visual: &str,
    ) -> Result<WasmRenderer, JsValue> {
        let kind = VisualKind::from_id(visual)
            .ok_or_else(|| JsValue::from_str(&format!("no visual called {visual:?}")))?;
        let inner =
            player_render::Renderer::new(wgpu::SurfaceTarget::Canvas(canvas), width, height, kind)
                .await
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(WasmRenderer { inner })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.inner.resize(width, height);
    }

    /// Selects a visual by id. The canvas and the surface are untouched
    /// (decision 5) -- this is the web half of the same scene swap the desktop
    /// shell's redraw thread performs.
    #[wasm_bindgen(js_name = setVisual)]
    pub fn set_visual(&mut self, visual: &str) -> Result<(), JsValue> {
        let kind = VisualKind::from_id(visual)
            .ok_or_else(|| JsValue::from_str(&format!("no visual called {visual:?}")))?;
        self.inner.set_visual(kind);
        Ok(())
    }

    /// The id of the visual currently being drawn.
    #[wasm_bindgen(js_name = currentVisual)]
    pub fn current_visual(&self) -> String {
        self.inner.visual().id().to_string()
    }

    /// Sets the display gain by id (`GainSetting::id`).
    #[wasm_bindgen(js_name = setGain)]
    pub fn set_gain(&mut self, gain: &str) -> Result<(), JsValue> {
        let setting = GainSetting::from_id(gain)
            .ok_or_else(|| JsValue::from_str(&format!("no gain setting called {gain:?}")))?;
        self.inner.set_gain(setting);
        Ok(())
    }

    /// The id of the display gain in effect.
    #[wasm_bindgen(js_name = currentGain)]
    pub fn current_gain(&self) -> String {
        self.inner.gain().id().to_string()
    }

    /// Draws one frame from the host's feed.
    ///
    /// `channels` is the interleaving width of `samples` -- the output's, since
    /// that is what the tap holds. Passing it rather than assuming stereo is
    /// M6's fix for a device that isn't; `sample_rate` is what makes the window
    /// a duration rather than a count; `elapsed_seconds` is what anything that
    /// moves over time is timed from, the automatic gain's release included.
    pub fn draw_frame(
        &mut self,
        samples: &[f32],
        channels: u16,
        sample_rate: u32,
        elapsed_seconds: f32,
    ) -> Result<(), JsValue> {
        self.inner
            .draw_frame(&VisualFrame::new(
                samples,
                channels,
                sample_rate,
                elapsed_seconds,
            ))
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

/// The display-gain steps, as JSON: `[{id, label, isMeasurement}]`.
///
/// Out of `player_render::GainSetting`, for the reason the visual catalog comes
/// out of `VisualKind`: one list, so the two shells cannot offer different
/// gains or disagree about which of them is still a measurement.
#[wasm_bindgen(js_name = gainCatalog)]
pub fn gain_catalog() -> String {
    let entries: Vec<serde_json::Value> = GainSetting::ALL
        .iter()
        .map(|setting| {
            serde_json::json!({
                "id": setting.id(),
                "label": setting.label(),
                "isMeasurement": setting.is_measurement(),
            })
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}

/// The id of the gain the visualizer opens at.
#[wasm_bindgen(js_name = defaultGain)]
pub fn default_gain() -> String {
    GainSetting::DEFAULT.id().to_string()
}

/// The gallery, as JSON: `[{id, name, description}]`.
///
/// Out of `player_render::VisualKind`, which is the project's one list of
/// visuals (decision 3) -- the web build no more keeps its own than the desktop
/// one does. JSON rather than a structured value for the reason the Subsonic
/// bindings below use it: this crate has no serde-wasm-bindgen and needs none
/// for a call that happens when a panel mounts.
#[wasm_bindgen(js_name = visualCatalog)]
pub fn visual_catalog() -> String {
    let entries: Vec<serde_json::Value> = VisualKind::ALL
        .iter()
        .map(|kind| {
            serde_json::json!({
                "id": kind.id(),
                "name": kind.name(),
                "description": kind.description(),
            })
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}

/// The id of the visual the gallery opens at when nothing has been chosen.
#[wasm_bindgen(js_name = defaultVisual)]
pub fn default_visual() -> String {
    VisualKind::DEFAULT.id().to_string()
}

/// How many interleaved samples one frame's feed should carry.
///
/// A fixed *duration* of history rather than a fixed sample count, so a visual
/// means the same thing at every output rate (M6 decision 1, the host's
/// question since M1). Exposed as a free function because the web's draw loop
/// lives in TypeScript and has to ask exactly what `src-tauri`'s redraw thread
/// asks -- of the same code, so the two shells cannot answer it differently.
#[wasm_bindgen(js_name = visualWindowSamples)]
pub fn visual_window_samples(sample_rate: u32, channels: u16) -> usize {
    player_render::window_samples(sample_rate, channels)
}

// --- Subsonic (Phase 2 M1) --------------------------------------------------
//
// The web shell's transport is `fetch`, in TypeScript; what it is emphatically
// not allowed to be is a second implementation of the protocol. So the two
// halves that decide anything -- what a request must contain, and what a
// response means -- cross to JS through here, out of the same
// `player_core::subsonic` the desktop shell calls directly (tasks/phase-2.md
// decision 2).
//
// Both entry points speak JSON strings rather than structured JS objects: this
// crate has no serde-wasm-bindgen and needs none for two calls that happen when
// a user types a server address, not per audio block.

/// Builds the URL for one call.
///
/// `salt` comes from the browser's own `crypto.getRandomValues` (decision 3:
/// randomness is the shell's, hashing is the protocol's) and is ignored for
/// API-key credentials.
#[wasm_bindgen(js_name = subsonicRequestUrl)]
pub fn subsonic_request_url(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    endpoint: &str,
    salt: &str,
    params_json: &str,
) -> Result<String, JsValue> {
    let server = subsonic_server(base_url)?;
    let credentials = subsonic_credentials(user, secret, is_api_key);
    // Extra parameters arrive as a JSON object so this signature does not grow
    // a variant per endpoint.
    let extra: std::collections::BTreeMap<String, String> = if params_json.trim().is_empty() {
        Default::default()
    } else {
        serde_json::from_str(params_json)
            .map_err(|e| JsValue::from_str(&format!("bad parameters: {e}")))?
    };
    let params: Vec<(&str, &str)> = extra
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    player_core::subsonic::request_url(&server, &credentials, endpoint, &params, salt)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Builds the URL that streams one track's audio (Phase 2 M2).
///
/// Separate from `subsonicRequestUrl` for one reason: the parameters are
/// `stream_params`, i.e. Rust's, so `format=raw` and the reasoning behind it
/// (decision 7) stay in the protocol module rather than being retyped in the
/// browser where they could quietly drift from the desktop shell's.
#[wasm_bindgen(js_name = subsonicStreamUrl)]
pub fn subsonic_stream_url(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    salt: &str,
    id: &str,
) -> Result<String, JsValue> {
    let server = subsonic_server(base_url)?;
    let credentials = subsonic_credentials(user, secret, is_api_key);
    player_core::subsonic::request_url(
        &server,
        &credentials,
        "stream",
        &player_core::subsonic::stream_params(id),
        salt,
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Builds the URL for one browse request (Phase 2 M3).
///
/// `request_json` is a `BrowseRequest` -- `{"kind":"albums","sort":...}` -- so
/// which endpoint that is and what parameters it must carry are decided in Rust,
/// by the same code the desktop shell calls. The browser chooses *what to ask
/// for*, never how to ask.
#[wasm_bindgen(js_name = subsonicBrowseUrl)]
pub fn subsonic_browse_url(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    salt: &str,
    request_json: &str,
) -> Result<String, JsValue> {
    let request = browse_request(request_json)?;
    let owned = request.params();
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    player_core::subsonic::request_url(
        &subsonic_server(base_url)?,
        &subsonic_credentials(user, secret, is_api_key),
        request.endpoint(),
        &params,
        salt,
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Reads the answer to one browse request.
///
/// The five endpoints hide their lists in five different places, and that is
/// known once, in `player_core::subsonic::browse` -- so this returns the same
/// rows the desktop shell's `server_browse` command does, out of the same code:
///
/// ```json
/// {"ok":true,"artists":[...],"albums":[...],"tracks":[...]}
/// {"ok":false,"error":{"kind":"server","code":"BadCredentials",...}}
/// ```
#[wasm_bindgen(js_name = subsonicBrowseResult)]
pub fn subsonic_browse_result(request_json: &str, bytes: &[u8]) -> String {
    let request = match browse_request(request_json) {
        Ok(request) => request,
        Err(_) => {
            return serde_json::json!({
                "ok": false,
                "error": {
                    "kind": "malformed",
                    "code": serde_json::Value::Null,
                    "message": "the browse request could not be read",
                    "credentials": false,
                },
            })
            .to_string()
        }
    };
    match player_core::subsonic::parse_browse(&request, bytes) {
        Ok(parsed) => {
            let mut value = serde_json::to_value(&parsed.payload)
                .unwrap_or_else(|_| serde_json::json!({"artists":[],"albums":[],"tracks":[]}));
            if let Some(object) = value.as_object_mut() {
                object.insert("ok".into(), serde_json::Value::Bool(true));
            }
            value.to_string()
        }
        Err(error) => subsonic_failure_json(&error).to_string(),
    }
}

/// The URL for one piece of cover art, at the size both shells ask for.
#[wasm_bindgen(js_name = subsonicCoverUrl)]
pub fn subsonic_cover_url(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    salt: &str,
    id: &str,
) -> Result<String, JsValue> {
    let owned = player_core::subsonic::cover_art_params(id, player_core::subsonic::COVER_SIZE_PX);
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    player_core::subsonic::request_url(
        &subsonic_server(base_url)?,
        &subsonic_credentials(user, secret, is_api_key),
        "getCoverArt",
        &params,
        salt,
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))
}

fn browse_request(request_json: &str) -> Result<player_core::subsonic::BrowseRequest, JsValue> {
    serde_json::from_str(request_json)
        .map_err(|e| JsValue::from_str(&format!("bad browse request: {e}")))
}

/// Builds the URL for one playlist request (Phase 2 M4).
///
/// `request_json` is a `PlaylistRequest`, so which of the five endpoints it is
/// and what it carries are decided by the same Rust the desktop shell calls --
/// including the repeated `songId`/`songIdToAdd` parameters, which a browser
/// building this URL itself would have to know are repeated rather than a list.
#[wasm_bindgen(js_name = subsonicPlaylistUrl)]
pub fn subsonic_playlist_url(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    salt: &str,
    request_json: &str,
) -> Result<String, JsValue> {
    let request = playlist_request(request_json)?;
    subsonic_url_with(
        base_url,
        user,
        secret,
        is_api_key,
        salt,
        request.endpoint(),
        &request.params(),
    )
}

/// Reads the answer to one playlist request.
///
/// Including the writes, whose bodies are not thrown away: a Subsonic failure is
/// an HTTP 200, so this is the only place "the playlist actually saved" can be
/// established.
#[wasm_bindgen(js_name = subsonicPlaylistResult)]
pub fn subsonic_playlist_result(request_json: &str, bytes: &[u8]) -> String {
    let request = match playlist_request(request_json) {
        Ok(request) => request,
        Err(_) => return unreadable_request_json("the playlist request could not be read"),
    };
    match player_core::subsonic::parse_playlists(&request, bytes) {
        Ok(parsed) => {
            let mut value = serde_json::to_value(&parsed.payload)
                .unwrap_or_else(|_| serde_json::json!({"playlists":[],"tracks":[]}));
            if let Some(object) = value.as_object_mut() {
                object.insert("ok".into(), serde_json::Value::Bool(true));
            }
            value.to_string()
        }
        Err(error) => subsonic_failure_json(&error).to_string(),
    }
}

/// Builds the URL for one star, unstar or rating (Phase 2 M4).
///
/// The reason this is not "an endpoint and an id" from TypeScript: `star` spells
/// its id three different ways depending on what is being starred, and the wrong
/// one succeeds while doing nothing (see `player_core::subsonic::stars`).
#[wasm_bindgen(js_name = subsonicMarkUrl)]
pub fn subsonic_mark_url(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    salt: &str,
    request_json: &str,
) -> Result<String, JsValue> {
    let request: player_core::subsonic::MarkRequest = serde_json::from_str(request_json)
        .map_err(|e| JsValue::from_str(&format!("bad mark request: {e}")))?;
    subsonic_url_with(
        base_url,
        user,
        secret,
        is_api_key,
        salt,
        request.endpoint(),
        &request.params(),
    )
}

/// Builds the URL for one scrobble (Phase 2 M4).
///
/// `request_json` is a `ScrobbleRequest` -- `{"id":..,"submission":..,"timeMs":..}`
/// -- for the same reason the three above take one: the browser chooses what to
/// say, never how to say it.
#[wasm_bindgen(js_name = subsonicScrobbleUrl)]
pub fn subsonic_scrobble_url(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    salt: &str,
    request_json: &str,
) -> Result<String, JsValue> {
    let request: player_core::subsonic::ScrobbleRequest = serde_json::from_str(request_json)
        .map_err(|e| JsValue::from_str(&format!("bad scrobble request: {e}")))?;
    subsonic_url_with(
        base_url,
        user,
        secret,
        is_api_key,
        salt,
        request.endpoint(),
        &request.params(),
    )
}

/// Reads the answer to a write that carries no payload -- a mark, or a scrobble.
///
/// `{"ok":true}` or the same failure shape every other entry point here returns.
#[wasm_bindgen(js_name = subsonicWriteResult)]
pub fn subsonic_write_result(bytes: &[u8]) -> String {
    match player_core::subsonic::parse_mark(bytes) {
        Ok(_) => serde_json::json!({ "ok": true }).to_string(),
        Err(error) => subsonic_failure_json(&error).to_string(),
    }
}

/// When a play counts, on the web shell (Phase 2 M4).
///
/// Stateful, like `WasmAnalyzer` and for the same reason: the rule is about what
/// happened between polls, not about one poll. It is
/// `player_core::subsonic::ScrobbleTracker` itself rather than a
/// reimplementation -- decision 10, so the two shells cannot come to disagree
/// about what a listener has heard.
#[wasm_bindgen]
pub struct WasmScrobbler {
    inner: player_core::subsonic::ScrobbleTracker,
}

impl Default for WasmScrobbler {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl WasmScrobbler {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmScrobbler {
        WasmScrobbler {
            inner: player_core::subsonic::ScrobbleTracker::new(),
        }
    }

    /// One observation of where playback is; answers with what to send, as
    /// `["nowPlaying:<id>", "submit:<id>:<timeMs>"]`.
    ///
    /// A string list rather than a structure because it crosses the wasm
    /// boundary on every poll and the caller's whole job is to turn each entry
    /// into one request.
    pub fn observe(
        &mut self,
        track_id: Option<String>,
        position_seconds: f64,
        duration_seconds: f64,
        playing: bool,
        now_ms: f64,
    ) -> Vec<String> {
        use player_core::subsonic::ScrobbleAction;
        self.inner
            .observe(
                track_id.as_deref(),
                position_seconds,
                duration_seconds,
                playing,
                now_ms.max(0.0) as u64,
            )
            .into_iter()
            .map(|action| match action {
                ScrobbleAction::NowPlaying { id } => format!("nowPlaying:{id}"),
                ScrobbleAction::Submit { id, time_ms } => format!("submit:{id}:{time_ms}"),
            })
            .collect()
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    /// Seconds of continuous playback accumulated for what is playing, so the
    /// web smoke can watch the rule work rather than only its outcome.
    #[wasm_bindgen(js_name = heardSeconds)]
    pub fn heard_seconds(&self) -> f64 {
        self.inner.heard_seconds()
    }
}

/// How long a stall has to last before it counts as buffering (Phase 2 M5).
///
/// `audioSab.ts` mirrors this number, because `transport()` runs on the UI
/// thread ten times a second where there is no wasm to ask. This binding is what
/// /failing.html holds that mirror to -- the same arrangement eqFilter.ts's copy
/// of the biquad lives under.
#[wasm_bindgen(js_name = bufferingAfterSeconds)]
pub fn buffering_after_seconds() -> f64 {
    player_core::stream::BUFFERING_AFTER_SECONDS
}

/// What kind of failure a Subsonic error is, as the shells need to say it
/// (Phase 2 M5): `"credentials"`, `"certificate"`, `"unreachable"`,
/// `"address"`, `"server"` or `"malformed"`.
///
/// Exposed so the web shell classifies a failure with the same Rust the desktop
/// shell uses rather than by reading message text -- and so /failing.html can
/// check that it does.
#[wasm_bindgen(js_name = subsonicFailureKind)]
pub fn subsonic_failure_kind(bytes: &[u8]) -> String {
    match player_core::subsonic::parse::<serde_json::Value>(bytes) {
        Ok(_) => "ok".to_string(),
        Err(error) => failure_kind_name(&error),
    }
}

fn failure_kind_name(error: &player_core::subsonic::SubsonicError) -> String {
    use player_core::subsonic::FailureKind;
    match error.kind() {
        FailureKind::Address => "address",
        FailureKind::Unreachable => "unreachable",
        FailureKind::Certificate => "certificate",
        FailureKind::Credentials => "credentials",
        FailureKind::Server => "server",
        FailureKind::Malformed => "malformed",
    }
    .to_string()
}

fn playlist_request(request_json: &str) -> Result<player_core::subsonic::PlaylistRequest, JsValue> {
    serde_json::from_str(request_json)
        .map_err(|e| JsValue::from_str(&format!("bad playlist request: {e}")))
}

/// A request URL from parameters that are already owned strings, which is what
/// every request type in this protocol produces.
fn subsonic_url_with(
    base_url: &str,
    user: &str,
    secret: &str,
    is_api_key: bool,
    salt: &str,
    endpoint: &str,
    owned: &[(String, String)],
) -> Result<String, JsValue> {
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    player_core::subsonic::request_url(
        &subsonic_server(base_url)?,
        &subsonic_credentials(user, secret, is_api_key),
        endpoint,
        &params,
        salt,
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// The failure shape for a request this side could not even read.
fn unreadable_request_json(message: &str) -> String {
    serde_json::json!({
        "ok": false,
        "error": {
            "kind": "malformed",
            "code": serde_json::Value::Null,
            "message": message,
            "credentials": false,
        },
    })
    .to_string()
}

fn subsonic_server(base_url: &str) -> Result<player_core::subsonic::Server, JsValue> {
    player_core::subsonic::Server::new(base_url).map_err(|e| JsValue::from_str(&e.to_string()))
}

fn subsonic_credentials(
    user: &str,
    secret: &str,
    is_api_key: bool,
) -> player_core::subsonic::Credentials {
    if is_api_key {
        player_core::subsonic::Credentials::ApiKey {
            key: secret.to_string(),
        }
    } else {
        player_core::subsonic::Credentials::Password {
            user: user.to_string(),
            password: secret.to_string(),
        }
    }
}

/// Parses one response body, and classifies a failure.
///
/// Returns JSON rather than throwing, in both directions, because a Subsonic
/// failure is not an exception -- it is the answer, and the UI draws it. The
/// shape:
///
/// ```json
/// {"ok":true,"info":{...},"payload":{...}}
/// {"ok":false,"error":{"kind":"server","code":40,"message":"...","credentials":true}}
/// ```
///
/// `kind`/`credentials` are decided here rather than in TypeScript for the
/// reason this whole seam exists: "is this the password's fault" must have one
/// answer on both shells.
#[wasm_bindgen(js_name = subsonicParse)]
pub fn subsonic_parse(bytes: &[u8]) -> String {
    match player_core::subsonic::parse::<serde_json::Value>(bytes) {
        Ok(parsed) => serde_json::json!({
            "ok": true,
            "info": {
                "version": parsed.info.version,
                "serverType": parsed.info.server_type,
                "serverVersion": parsed.info.server_version,
                "openSubsonic": parsed.info.open_subsonic,
            },
            "payload": parsed.payload,
        })
        .to_string(),
        Err(error) => subsonic_failure_json(&error).to_string(),
    }
}

/// Parses a `getRandomSongs` response into playable rows (Phase 2 M2).
///
/// `subsonicParse` would hand the same bytes back as untyped JSON and leave the
/// browser to know that the songs are under `randomSongs.song`, that `suffix` is
/// what the decoder needs as a hint and that `duration` is whole seconds. That
/// knowledge is the protocol's, and it already exists once in
/// `player_core::subsonic::Song` -- so it is applied here, and both shells end up
/// with the same rows out of the same response.
#[wasm_bindgen(js_name = subsonicRandomSongs)]
pub fn subsonic_random_songs(bytes: &[u8]) -> String {
    match player_core::subsonic::parse::<player_core::subsonic::RandomSongs>(bytes) {
        Ok(parsed) => {
            let songs: Vec<player_core::subsonic::TrackRow> = parsed
                .payload
                .inner
                .song
                .into_iter()
                .map(player_core::subsonic::TrackRow::from)
                .collect();
            serde_json::json!({ "ok": true, "songs": songs }).to_string()
        }
        Err(error) => subsonic_failure_json(&error).to_string(),
    }
}

/// One failure, in the shape both parse entry points hand back.
fn subsonic_failure_json(error: &player_core::subsonic::SubsonicError) -> serde_json::Value {
    use player_core::subsonic::SubsonicError;

    let (kind, code) = match error {
        SubsonicError::Server { code, .. } => ("server", Some(format!("{code:?}"))),
        SubsonicError::Http { .. } => ("http", None),
        SubsonicError::Malformed(_) => ("malformed", None),
        SubsonicError::Transport(_) => ("transport", None),
        SubsonicError::Certificate(_) => ("certificate", None),
        SubsonicError::BadUrl(_) => ("badUrl", None),
    };
    serde_json::json!({
        "ok": false,
        "error": {
            "kind": kind,
            "code": code,
            "message": error.to_string(),
            // Both from the error itself now (M5 decision 17), so a proxy's 401
            // counts as a credentials problem on this shell exactly as it does
            // natively -- and so a shell picks its sentence from a name rather
            // than by reading message text.
            "credentials": error.is_credentials_problem(),
            "failure": failure_kind_name(error),
        },
    })
}
