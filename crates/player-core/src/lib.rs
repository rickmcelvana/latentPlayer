//! `player-core` -- decode + (native) output. Shared by the Tauri desktop
//! shell directly and by `player-rt-wasm` for the web build. See
//! `ARCHITECTURE.md` and PLAN.md §5.

pub mod analysis;
pub mod channels;
pub mod decode;
pub mod error;
#[cfg(not(target_arch = "wasm32"))]
pub mod playback;
pub mod resample;
pub mod source;
pub mod spectrum;
pub mod stream;
pub mod tap;

pub use analysis::{Meters, BAND_COUNT, FFT_SIZE};
pub use channels::{adapt_channels, ChannelShape};
pub use decode::{decode_all, DecodedAudio};
pub use error::PlayerError;
#[cfg(not(target_arch = "wasm32"))]
pub use playback::Playback;
pub use source::{InMemorySource, LocalFile, PlaybackSource};
pub use spectrum::SpectrumAnalyzer;
pub use stream::StreamingDecoder;
pub use tap::{AnalysisTap, TapCursor, TapRead};
