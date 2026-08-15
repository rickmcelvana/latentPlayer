//! Web glue for `player-core` (decode) and `player-render` (the wgpu
//! scope). Mirrors `../latent-mixing/crates/mixer-rt-wasm`'s role, but
//! smaller: a plain player has no per-block DSP to run in wasm the way a
//! mixer does, so the only wasm entry points are "decode this file" (once,
//! not per audio block) and "drive the renderer" (once per animation
//! frame, not per audio block either). The AudioWorkletProcessor itself
//! (app/src/worklet/player-worklet.ts) is plain TS reading directly off the
//! SharedArrayBuffer -- there's no per-block wasm call on the audio path at
//! all, deliberately simpler than the mixer's ring-buffer+wasm design
//! because there's no real-time mixing/DSP happening here.

use js_sys::Float32Array;
use player_core::{decode_all, InMemorySource};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// Result of decoding a file, handed back to JS so it can size and fill the
/// shared playback buffer (see playerEngine.ts).
#[wasm_bindgen]
pub struct DecodedTrack {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
}

#[wasm_bindgen]
impl DecodedTrack {
    #[wasm_bindgen(getter)]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    #[wasm_bindgen(getter)]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[wasm_bindgen(getter)]
    pub fn frame_count(&self) -> u32 {
        (self.samples.len() / self.channels.max(1) as usize) as u32
    }

    /// Copies the interleaved samples into a caller-provided `Float32Array`
    /// view (in practice, a view over a `SharedArrayBuffer` -- see
    /// playerEngine.ts) starting at `offset`.
    pub fn copy_into(&self, dst: &Float32Array, offset: u32) {
        let view = dst.subarray(offset, offset + self.samples.len() as u32);
        view.copy_from(&self.samples);
    }
}

/// Decodes `bytes` (a whole file, already read into memory by the browser's
/// file picker) into interleaved f32 PCM. Runs inside a Web Worker
/// (worker.ts), off the main thread -- mirrors the sibling repos' rule that
/// heavy/blocking work never runs on the UI thread.
#[wasm_bindgen]
pub fn decode(bytes: Vec<u8>, extension_hint: Option<String>) -> Result<DecodedTrack, JsValue> {
    let source = InMemorySource::new(bytes, extension_hint);
    let audio = decode_all(&source).map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(DecodedTrack {
        samples: audio.samples,
        channels: audio.channels,
        sample_rate: audio.sample_rate,
    })
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
