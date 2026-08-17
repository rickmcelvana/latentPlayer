//! `player-core` -- decode + (native) output. Shared by the Tauri desktop
//! shell directly and by `player-rt-wasm` for the web build. See
//! `ARCHITECTURE.md` and PLAN.md §5.

pub mod analysis;
pub mod channels;
pub mod decode;
pub mod eq;
pub mod error;
/// The desktop shell's library. Native only: a browser has handles rather than
/// paths, so the two shells cannot share a scanner (M7 decision 3).
#[cfg(not(target_arch = "wasm32"))]
pub mod library;
pub mod loudness;
pub mod phase;
#[cfg(not(target_arch = "wasm32"))]
pub mod playback;
pub mod resample;
pub mod source;
pub mod spectrum;
pub mod stream;
/// The Subsonic/OpenSubsonic protocol (Phase 2). Cross-target and IO-free by
/// design -- the shells move the bytes, this decides what they mean.
pub mod subsonic;
pub mod tags;
pub mod tap;

pub use analysis::{MeterSnapshot, Meters, BAND_COUNT, FFT_SIZE, GONIOMETER_POINTS};
pub use channels::{adapt_channels, ChannelShape};
pub use decode::{decode_all, DecodedAudio};
pub use eq::{
    magnitude_response, EqCoefficients, EqProcessor, EqSettings, BAND_FREQUENCIES, EQ_BANDS,
};
pub use error::PlayerError;
#[cfg(not(target_arch = "wasm32"))]
pub use library::{Library, LibraryTrack, Playlist, ScanSummary};
pub use loudness::{LoudnessMeter, LoudnessReading};
pub use phase::{goniometer_points, CorrelationMeter};
#[cfg(not(target_arch = "wasm32"))]
pub use playback::Playback;
pub use source::{InMemorySource, LocalFile, PlaybackSource};
pub use spectrum::SpectrumAnalyzer;
pub use stream::StreamingDecoder;
pub use subsonic::{Credentials, Server, SubsonicError};
pub use tags::{read_tags, CoverArt, TrackTags};
pub use tap::{AnalysisTap, TapCursor, TapRead};
