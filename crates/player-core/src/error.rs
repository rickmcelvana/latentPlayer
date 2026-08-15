use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("failed to open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no supported audio track found in the source")]
    NoAudioTrack,

    #[error("unsupported/unregistered codec")]
    UnsupportedCodec,

    #[error("symphonia error: {0}")]
    Symphonia(#[from] symphonia::core::errors::Error),

    #[error("no usable audio output device")]
    NoOutputDevice,

    /// Anything the audio backend itself refused, carrying its own message.
    /// Previously these were all flattened into `NoOutputDevice`, which
    /// reported "no usable audio output device" for a device that was
    /// present and working fine and had merely rejected the requested
    /// format -- actively misleading, and it cost real debugging time on
    /// the first Windows desktop test (2026-08-15).
    #[error("audio output stream error: {0}")]
    OutputStream(String),

    #[error("sample-rate conversion failed: {0}")]
    Resample(String),

    #[error("output device only offers an unsupported sample format: {0}")]
    UnsupportedOutputFormat(String),
}
