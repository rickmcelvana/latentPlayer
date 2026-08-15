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

    #[error("output device only offers an unsupported sample format: {0}")]
    UnsupportedOutputFormat(String),
}
