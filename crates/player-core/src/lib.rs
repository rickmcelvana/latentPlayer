//! `player-core` -- decode + (native) output. Shared by the Tauri desktop
//! shell directly and by `player-rt-wasm` for the web build. See
//! `ARCHITECTURE.md` and PLAN.md §5.

pub mod decode;
pub mod error;
#[cfg(not(target_arch = "wasm32"))]
pub mod playback;
/// Native-only: it's backed by `rubato`, and the web build has nothing to
/// convert (its `AudioContext` runs at the file's own rate).
#[cfg(not(target_arch = "wasm32"))]
pub mod resample;
pub mod source;

pub use decode::{decode_all, DecodedAudio};
pub use error::PlayerError;
#[cfg(not(target_arch = "wasm32"))]
pub use playback::Playback;
pub use source::{InMemorySource, LocalFile, PlaybackSource};
