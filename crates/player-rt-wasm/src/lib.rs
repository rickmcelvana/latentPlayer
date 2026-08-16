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

/// wasm-bindgen wrapper around `player_render::Renderer`, the same render
/// engine `src-tauri` drives natively -- same shader, same pipeline, same
/// draw call (PLAN.md §5/§8.1's whole point).
#[wasm_bindgen]
pub struct WasmRenderer {
    inner: player_render::Renderer,
}

#[wasm_bindgen]
impl WasmRenderer {
    /// Not a `#[wasm_bindgen(constructor)]` because it's async -- wasm-bindgen
    /// constructors can't be. JS calls this as a plain async function and
    /// gets a `Promise<WasmRenderer>`.
    #[wasm_bindgen(js_name = create)]
    pub async fn create(
        canvas: HtmlCanvasElement,
        width: u32,
        height: u32,
    ) -> Result<WasmRenderer, JsValue> {
        let inner =
            player_render::Renderer::new(wgpu::SurfaceTarget::Canvas(canvas), width, height)
                .await
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(WasmRenderer { inner })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.inner.resize(width, height);
    }

    /// `channels` is the interleaving width of `samples` -- the output's, since
    /// that is what the tap holds. Passing it rather than assuming stereo is
    /// M6's fix for a device that isn't.
    pub fn draw_frame(&mut self, samples: &[f32], channels: u16) -> Result<(), JsValue> {
        self.inner
            .draw_frame(samples, channels)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

/// How many interleaved samples one frame of the scope should be drawn from.
///
/// A fixed *duration* of history rather than a fixed sample count, so the
/// extrusion means the same thing at every output rate (M6 decision 1). Exposed
/// as a free function because the web's draw loop lives in TypeScript and has
/// to ask the same question `src-tauri`'s redraw thread does -- of the same
/// code, so the two shells cannot answer it differently.
#[wasm_bindgen(js_name = scopeWindowSamples)]
pub fn scope_window_samples(sample_rate: u32, channels: u16) -> usize {
    player_render::window_samples(sample_rate, channels)
}
