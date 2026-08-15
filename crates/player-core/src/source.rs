//! §5's `PlaybackSource` abstraction (PLAN.md). Phase 0 only implements the
//! `Local`/in-memory half -- Subsonic is Phase 2 (PLAN.md §8.2) and
//! deliberately not stubbed here yet.

use std::io::Cursor;
use std::path::PathBuf;

use symphonia::core::io::MediaSource;

use crate::error::PlayerError;

/// A source of encoded audio bytes the player can decode. `open()` hands
/// back a Symphonia `MediaSource` rather than raw bytes so large local files
/// don't have to be read into memory just to find out what codec they are.
pub trait PlaybackSource: Send {
    fn open(&self) -> Result<Box<dyn MediaSource>, PlayerError>;

    /// Hint for Symphonia's format-probe (usually the file extension).
    /// Without this, some containers (e.g. bare `.aac`) can't be
    /// disambiguated from their contents alone.
    fn extension_hint(&self) -> Option<&str> {
        None
    }
}

/// A file already on disk. The native (Tauri) source for Phase 0.
pub struct LocalFile {
    pub path: PathBuf,
}

impl LocalFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl PlaybackSource for LocalFile {
    fn open(&self) -> Result<Box<dyn MediaSource>, PlayerError> {
        let file = std::fs::File::open(&self.path).map_err(|source| PlayerError::Open {
            path: self.path.clone(),
            source,
        })?;
        Ok(Box::new(file))
    }

    fn extension_hint(&self) -> Option<&str> {
        self.path.extension().and_then(|ext| ext.to_str())
    }
}

/// A byte buffer already in memory, e.g. bytes the browser's file picker
/// handed over on the web build (§5 -- `player-rt-wasm` has no filesystem
/// to read from). The Phase-0 wasm source.
pub struct InMemorySource {
    bytes: Vec<u8>,
    extension_hint: Option<String>,
}

impl InMemorySource {
    pub fn new(bytes: Vec<u8>, extension_hint: Option<String>) -> Self {
        Self {
            bytes,
            extension_hint,
        }
    }
}

impl PlaybackSource for InMemorySource {
    fn open(&self) -> Result<Box<dyn MediaSource>, PlayerError> {
        Ok(Box::new(CursorSource(Cursor::new(self.bytes.clone()))))
    }

    fn extension_hint(&self) -> Option<&str> {
        self.extension_hint.as_deref()
    }
}

/// `Cursor<Vec<u8>>` already satisfies `Read + Seek`; `MediaSource` just
/// needs the two extra queries below, which this newtype supplies.
struct CursorSource(Cursor<Vec<u8>>);

impl std::io::Read for CursorSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl std::io::Seek for CursorSource {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

impl MediaSource for CursorSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_extension_hint_reads_the_real_extension() {
        let source = LocalFile::new("/tmp/example.flac");
        assert_eq!(source.extension_hint(), Some("flac"));
    }

    #[test]
    fn in_memory_source_reports_its_stored_length() {
        let bytes = vec![0u8; 128];
        let source = InMemorySource::new(bytes, Some("wav".to_string()));
        let opened = source
            .open()
            .expect("open should succeed for in-memory bytes");
        assert_eq!(opened.byte_len(), Some(128));
        assert_eq!(source.extension_hint(), Some("wav"));
    }
}
