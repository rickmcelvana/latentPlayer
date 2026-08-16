//! The desktop shell's music library: walk folders, read tags, remember what
//! was found.
//!
//! Native only, and unavoidably so (M7 decision 3). A library is a *folder*
//! natively and a `FileSystemDirectoryHandle` in a browser; the two have no
//! common scanner, and inventing one would mean the desktop build could only
//! do what a browser can. What the two shells do share is everything above
//! this -- rows, search, playlists -- which lives in `app/src/library.ts`.
//!
//! The index is kept here rather than in `src-tauri` so it can be tested and
//! driven by `examples/library_smoke.rs` without booting a GUI, the same
//! argument the other smoke examples make. Nothing in this module knows what
//! Tauri is; it is handed a directory to keep its files in.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PlayerError;
use crate::source::LocalFile;
use crate::tags::read_tags;

/// Extensions the scan will try to read. Deliberately the same set the file
/// picker offers (`nativePlayer.ts`), plus the spellings the same containers
/// also go by, so a folder scan cannot find fewer tracks than opening files by
/// hand would.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "wave", "aac", "m4a", "m4b", "mp4", "ogg", "oga", "aiff", "aif", "alac",
    "mka",
];

/// How deep a scan will walk before giving up. A guard against a symlink loop
/// turning a scan into a hang, not a real limit -- no music library is nested
/// 32 folders deep.
const MAX_DEPTH: usize = 32;

/// One row of the library.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryTrack {
    /// Derived from the path, so rescanning a folder updates rows rather than
    /// duplicating them, and a playlist referring to this track survives the
    /// rescan (M7 decision 7).
    pub id: String,
    pub path: PathBuf,
    /// The file's own name, always present -- what the UI shows when a file
    /// carries no title at all.
    pub file_name: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<u32>,
    pub duration_seconds: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// Content hash of the embedded image, naming a file in `covers/`. Content
    /// rather than track keyed, so an album's twelve tracks share one cached
    /// image instead of writing the same JPEG twelve times.
    pub cover_id: Option<String>,
}

/// A saved playlist: a name and the rows it points at.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    /// `LibraryTrack::id`s, in order. References rather than copies, so fixing
    /// a tag fixes it everywhere it appears (M7 decision 7).
    pub track_ids: Vec<String>,
}

/// What a scan found. Reported rather than inferred from the row count,
/// because "nothing was added" and "nothing could be read" are different
/// problems and the UI should be able to say which happened.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    /// Files with a known audio extension that were found.
    pub found: usize,
    /// Of those, how many were read successfully.
    pub added: usize,
    /// Files whose tags could not be read at all -- a truncated download, a
    /// codec symphonia doesn't have. Skipped, never fatal: one bad file must
    /// not abandon a scan of ten thousand.
    pub failed: usize,
}

/// The on-disk shape of the index. Versioned so a later milestone can migrate
/// rather than silently misread an older file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    version: u32,
    folders: Vec<PathBuf>,
    tracks: Vec<LibraryTrack>,
    playlists: Vec<Playlist>,
}

const INDEX_VERSION: u32 = 1;

/// The library, backed by a directory of its own.
///
/// Holds the whole index in memory: a row is a couple of hundred bytes, so
/// even a 50,000-track library is a few megabytes, and every operation the UI
/// performs (search, sort, playlist edits) is then a plain iteration rather
/// than a query. Cover art is the part that would not fit, and it lives in
/// files rather than here.
pub struct Library {
    root: PathBuf,
    index: Index,
}

impl Library {
    /// Opens the library kept in `root`, creating it on first run.
    ///
    /// An index that fails to parse is replaced with an empty one rather than
    /// refusing to start: the library is a cache of what is on disk, so the
    /// worst case is a rescan, and refusing to open the app because a JSON
    /// file went bad would be a far worse trade.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PlayerError> {
        let root = root.into();
        fs::create_dir_all(root.join("covers")).map_err(|source| PlayerError::Open {
            path: root.clone(),
            source,
        })?;

        let index = fs::read(index_path(&root))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Index>(&bytes).ok())
            .filter(|index| index.version == INDEX_VERSION)
            .unwrap_or_default();

        Ok(Self { root, index })
    }

    pub fn tracks(&self) -> &[LibraryTrack] {
        &self.index.tracks
    }

    pub fn playlists(&self) -> &[Playlist] {
        &self.index.playlists
    }

    pub fn folders(&self) -> &[PathBuf] {
        &self.index.folders
    }

    /// Where a cover's bytes are cached. The caller serves this file rather
    /// than reading it through a command -- see M7 decision 5.
    pub fn cover_path(&self, cover_id: &str) -> PathBuf {
        self.root.join("covers").join(cover_id)
    }

    /// Walks `folder`, reads every audio file's tags, and merges the result
    /// into the index.
    ///
    /// Rescanning is the same call: rows are keyed by path, so a file that is
    /// still there is updated in place, one that has been deleted drops out,
    /// and one that has been added appears. That means a rescan cannot
    /// duplicate a library, which is the failure mode this being idempotent
    /// exists to prevent.
    pub fn scan(&mut self, folder: impl AsRef<Path>) -> Result<ScanSummary, PlayerError> {
        let folder = folder.as_ref();
        let mut files = Vec::new();
        collect_audio_files(folder, 0, &mut files);

        let mut summary = ScanSummary {
            found: files.len(),
            ..ScanSummary::default()
        };

        let mut scanned = Vec::with_capacity(files.len());
        for path in files {
            match self.read_track(&path) {
                Ok(track) => {
                    summary.added += 1;
                    scanned.push(track);
                }
                // One unreadable file skips, it does not abort: a scan of ten
                // thousand tracks will meet a truncated download eventually,
                // and losing the other 9,999 to it would be absurd.
                Err(_) => summary.failed += 1,
            }
        }

        // Everything previously found under this folder is replaced by what is
        // there now, and rows from other folders are left alone.
        self.index
            .tracks
            .retain(|track| !track.path.starts_with(folder));
        self.index.tracks.extend(scanned);
        self.index.tracks.sort_by(|a, b| a.path.cmp(&b.path));

        if !self.index.folders.iter().any(|f| f == folder) {
            self.index.folders.push(folder.to_path_buf());
        }

        self.save()?;
        Ok(summary)
    }

    /// Forgets a folder and every row under it. The files themselves are not
    /// touched -- this is a library, not a file manager.
    pub fn forget_folder(&mut self, folder: impl AsRef<Path>) -> Result<(), PlayerError> {
        let folder = folder.as_ref();
        self.index.folders.retain(|f| f != folder);
        self.index
            .tracks
            .retain(|track| !track.path.starts_with(folder));
        self.save()
    }

    /// Adds or replaces a playlist, keyed by id.
    pub fn save_playlist(&mut self, playlist: Playlist) -> Result<(), PlayerError> {
        match self
            .index
            .playlists
            .iter_mut()
            .find(|p| p.id == playlist.id)
        {
            Some(existing) => *existing = playlist,
            None => self.index.playlists.push(playlist),
        }
        self.save()
    }

    pub fn delete_playlist(&mut self, id: &str) -> Result<(), PlayerError> {
        self.index.playlists.retain(|p| p.id != id);
        self.save()
    }

    /// Writes the index out. Via a temporary file and a rename, so a crash
    /// mid-write leaves the previous index intact rather than a half-written
    /// one that `open` would then discard.
    pub fn save(&self) -> Result<(), PlayerError> {
        let path = index_path(&self.root);
        let temp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&Index {
            version: INDEX_VERSION,
            ..self.index.clone()
        })
        .map_err(|e| PlayerError::Library(e.to_string()))?;

        fs::write(&temp, bytes).map_err(|source| PlayerError::Open {
            path: temp.clone(),
            source,
        })?;
        fs::rename(&temp, &path).map_err(|source| PlayerError::Open { path, source })
    }

    /// Reads one file's tags and caches its cover art.
    fn read_track(&self, path: &Path) -> Result<LibraryTrack, PlayerError> {
        let tags = read_tags(&LocalFile::new(path))?;

        let cover_id = match tags.cover {
            Some(cover) if !cover.data.is_empty() => {
                let id = hash_hex(&cover.data);
                let target = self.cover_path(&id);
                // Content-addressed, so an album's other eleven tracks find
                // the file already written and skip the write entirely.
                if !target.exists() {
                    let _ = fs::write(&target, &cover.data);
                }
                Some(id)
            }
            _ => None,
        };

        Ok(LibraryTrack {
            id: hash_hex(path.to_string_lossy().as_bytes()),
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_path_buf(),
            title: tags.title,
            artist: tags.artist,
            album: tags.album,
            album_artist: tags.album_artist,
            genre: tags.genre,
            track_number: tags.track_number,
            disc_number: tags.disc_number,
            year: tags.year,
            duration_seconds: tags.duration_seconds,
            sample_rate: tags.sample_rate,
            channels: tags.channels,
            cover_id,
        })
    }
}

fn index_path(root: &Path) -> PathBuf {
    root.join("library.json")
}

/// Depth-first walk collecting audio files. Errors are skipped rather than
/// propagated -- an unreadable subdirectory (permissions, a dead symlink) is
/// a normal thing to meet halfway through a music folder and is not a reason
/// to fail the scan.
fn collect_audio_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => dirs.push(path),
            Ok(ft) if ft.is_file() && is_audio_file(&path) => out.push(path),
            _ => {}
        }
    }

    // Files before subdirectories, and each level sorted, so a scan produces
    // the same order twice running. An index whose row order depended on the
    // filesystem's would reshuffle the UI on every rescan for no reason.
    out.sort();
    dirs.sort();
    for dir in dirs {
        collect_audio_files(&dir, depth + 1, out);
    }
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.as_str()))
}

/// FNV-1a, rendered hex. Used for row ids (over the path) and cover names
/// (over the image bytes).
///
/// Not a cryptographic hash and not trying to be: nothing here defends against
/// a chosen collision, it only needs a short stable name for a byte string.
/// A real digest would mean a dependency for no gain.
fn hash_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Rows grouped by album, for a library view that shows albums rather than a
/// flat list. Keyed by album artist where there is one, so a compilation stays
/// one album instead of splintering into one per track.
pub fn group_by_album(tracks: &[LibraryTrack]) -> Vec<(String, Vec<&LibraryTrack>)> {
    let mut groups: HashMap<String, Vec<&LibraryTrack>> = HashMap::new();
    for track in tracks {
        let key = track.album.clone().unwrap_or_else(|| "Unknown".to_string());
        groups.entry(key).or_default().push(track);
    }
    let mut out: Vec<_> = groups.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, tracks) in &mut out {
        tracks.sort_by_key(|t| (t.disc_number.unwrap_or(1), t.track_number.unwrap_or(0)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans itself up. `std::env::temp_dir` plus a
    /// counter rather than a `tempfile` dependency for a handful of tests.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "latentplayer-{name}-{}-{unique}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A minimal silent WAV, same shape as tags.rs's.
    fn wav(frames: usize) -> Vec<u8> {
        let (channels, rate, bits) = (2u16, 44_100u32, 16u16);
        let block_align = channels * bits / 8;
        let data_len = frames * block_align as usize;

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        out.resize(out.len() + data_len, 0);
        out
    }

    fn write_wav(path: &Path, frames: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, wav(frames)).unwrap();
    }

    #[test]
    fn finds_audio_files_recursively_and_ignores_everything_else() {
        let music = TempDir::new("music");
        write_wav(&music.path().join("a.wav"), 4_410);
        write_wav(&music.path().join("album/b.wav"), 4_410);
        write_wav(&music.path().join("album/deep/c.wav"), 4_410);
        fs::write(music.path().join("cover.jpg"), [0u8; 8]).unwrap();
        fs::write(music.path().join("album/notes.txt"), b"hello").unwrap();

        let store = TempDir::new("store");
        let mut library = Library::open(store.path()).unwrap();
        let summary = library.scan(music.path()).unwrap();

        assert_eq!(summary.found, 3, "three audio files, not the jpg or txt");
        assert_eq!(summary.added, 3);
        assert_eq!(summary.failed, 0);
        assert_eq!(library.tracks().len(), 3);
    }

    #[test]
    fn a_rescan_updates_rows_rather_than_duplicating_them() {
        // The failure this guards against is the obvious one: scan twice, get
        // a library twice the size.
        let music = TempDir::new("music");
        write_wav(&music.path().join("a.wav"), 4_410);
        write_wav(&music.path().join("b.wav"), 4_410);

        let store = TempDir::new("store");
        let mut library = Library::open(store.path()).unwrap();
        library.scan(music.path()).unwrap();
        let first: Vec<String> = library.tracks().iter().map(|t| t.id.clone()).collect();

        // One file goes away, one appears.
        fs::remove_file(music.path().join("b.wav")).unwrap();
        write_wav(&music.path().join("c.wav"), 4_410);
        library.scan(music.path()).unwrap();

        let names: Vec<&str> = library
            .tracks()
            .iter()
            .map(|t| t.file_name.as_str())
            .collect();
        assert_eq!(names, ["a.wav", "c.wav"], "deleted out, added in");
        assert_eq!(
            library.tracks()[0].id,
            first[0],
            "a row that didn't move keeps its id, so playlists survive"
        );
    }

    #[test]
    fn a_bad_file_is_skipped_and_counted_rather_than_failing_the_scan() {
        let music = TempDir::new("music");
        write_wav(&music.path().join("good.wav"), 4_410);
        // Right extension, contents that are not a WAV at all.
        fs::write(music.path().join("truncated.wav"), b"RIFFnope").unwrap();

        let store = TempDir::new("store");
        let mut library = Library::open(store.path()).unwrap();
        let summary = library.scan(music.path()).unwrap();

        assert_eq!(summary.found, 2);
        assert_eq!(summary.added, 1, "the good one still lands");
        assert_eq!(summary.failed, 1, "and the bad one is reported, not hidden");
    }

    #[test]
    fn the_index_survives_being_reopened() {
        let music = TempDir::new("music");
        write_wav(&music.path().join("a.wav"), 4_410);

        let store = TempDir::new("store");
        {
            let mut library = Library::open(store.path()).unwrap();
            library.scan(music.path()).unwrap();
            library
                .save_playlist(Playlist {
                    id: "p1".into(),
                    name: "Evening".into(),
                    track_ids: vec![library.tracks()[0].id.clone()],
                })
                .unwrap();
        }

        let reopened = Library::open(store.path()).unwrap();
        assert_eq!(reopened.tracks().len(), 1);
        assert_eq!(reopened.playlists().len(), 1);
        assert_eq!(reopened.playlists()[0].name, "Evening");
        assert_eq!(reopened.folders(), [music.path().to_path_buf()]);
    }

    #[test]
    fn a_corrupt_index_opens_empty_instead_of_refusing_to_start() {
        let store = TempDir::new("store");
        fs::create_dir_all(store.path()).unwrap();
        fs::write(index_path(store.path()), b"{ this is not json").unwrap();

        let library = Library::open(store.path()).unwrap();
        assert!(library.tracks().is_empty());
    }

    #[test]
    fn forgetting_a_folder_drops_its_rows_and_leaves_the_others() {
        let one = TempDir::new("music-one");
        let two = TempDir::new("music-two");
        write_wav(&one.path().join("a.wav"), 4_410);
        write_wav(&two.path().join("b.wav"), 4_410);

        let store = TempDir::new("store");
        let mut library = Library::open(store.path()).unwrap();
        library.scan(one.path()).unwrap();
        library.scan(two.path()).unwrap();
        assert_eq!(library.tracks().len(), 2);

        library.forget_folder(one.path()).unwrap();
        assert_eq!(library.tracks().len(), 1);
        assert_eq!(library.tracks()[0].file_name, "b.wav");
        assert_eq!(library.folders().len(), 1);
    }

    #[test]
    fn playlists_are_replaced_by_id_not_appended() {
        let store = TempDir::new("store");
        let mut library = Library::open(store.path()).unwrap();
        library
            .save_playlist(Playlist {
                id: "p1".into(),
                name: "First".into(),
                track_ids: vec![],
            })
            .unwrap();
        library
            .save_playlist(Playlist {
                id: "p1".into(),
                name: "Renamed".into(),
                track_ids: vec!["t1".into()],
            })
            .unwrap();

        assert_eq!(library.playlists().len(), 1);
        assert_eq!(library.playlists()[0].name, "Renamed");

        library.delete_playlist("p1").unwrap();
        assert!(library.playlists().is_empty());
    }

    #[test]
    fn the_same_bytes_hash_to_the_same_name() {
        // What makes cover art deduplicate across an album.
        assert_eq!(hash_hex(b"cover bytes"), hash_hex(b"cover bytes"));
        assert_ne!(hash_hex(b"cover bytes"), hash_hex(b"other bytes"));
        assert_eq!(hash_hex(b"anything").len(), 16);
    }

    #[test]
    fn albums_group_and_sort_by_disc_then_track() {
        let track = |album: &str, disc, number| LibraryTrack {
            album: Some(album.to_string()),
            disc_number: Some(disc),
            track_number: Some(number),
            ..LibraryTrack::default()
        };
        let tracks = vec![
            track("B side", 1, 1),
            track("A side", 2, 1),
            track("A side", 1, 2),
            track("A side", 1, 1),
        ];

        let grouped = group_by_album(&tracks);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].0, "A side");
        let order: Vec<_> = grouped[0]
            .1
            .iter()
            .map(|t| (t.disc_number, t.track_number))
            .collect();
        assert_eq!(
            order,
            [(Some(1), Some(1)), (Some(1), Some(2)), (Some(2), Some(1))]
        );
    }
}
