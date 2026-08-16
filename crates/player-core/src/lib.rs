//! `player-core` -- decode + (native) output. Shared by the Tauri desktop
//! shell directly and by `player-rt-wasm` for the web build. See
//! `ARCHITECTURE.md` and PLAN.md §5.

pub mod analysis;
pub mod channels;
pub mod decode;
pub mod error;
pub mod loudness;
pub mod phase;
#[cfg(not(target_arch = "wasm32"))]
pub mod playback;
pub mod resample;
pub mod source;
pub mod spectrum;
pub mod stream;
pub mod tap;

pub use analysis::{MeterSnapshot, Meters, BAND_COUNT, FFT_SIZE, GONIOMETER_POINTS};
pub use channels::{adapt_channels, ChannelShape};
pub use decode::{decode_all, DecodedAudio};
pub use error::PlayerError;
pub use loudness::{LoudnessMeter, LoudnessReading};
pub use phase::{goniometer_points, CorrelationMeter};
#[cfg(not(target_arch = "wasm32"))]
pub use playback::Playback;
pub use source::{InMemorySource, LocalFile, PlaybackSource};
pub use spectrum::SpectrumAnalyzer;
pub use stream::StreamingDecoder;
pub use tap::{AnalysisTap, TapCursor, TapRead};
