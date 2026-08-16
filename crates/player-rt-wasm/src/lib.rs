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
use player_core::resample::StreamingResampler;
use player_core::{adapt_channels, ChannelShape, InMemorySource, StreamingDecoder};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

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

    pub fn draw_frame(&mut self, samples: &[f32]) -> Result<(), JsValue> {
        self.inner
            .draw_frame(samples)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
