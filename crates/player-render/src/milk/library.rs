//! The desktop shell's preset library: walk folders, parse presets, remember
//! what was found (Phase 3 M7c item 3, decision 50).
//!
//! Native only, for the same reason `player-core::library` is: a library is a
//! *folder* natively and a `FileSystemDirectoryHandle` in a browser, and the
//! two have no common scanner. What the two shells share is everything above
//! this -- the row shape, the report, the selection -- which lives in
//! `app/src/presetLibrary.ts`.
//!
//! It lives in `player-render` rather than `player-core` because the parser it
//! depends on ([`crate::milk::Preset`]) is this crate's, and `player-core`
//! must not grow a dependency on the renderer. The index is kept here rather
//! than in `src-tauri` so it can be tested and driven by
//! `examples/preset_library_smoke.rs` without booting a GUI, the same argument
//! the music library's smoke makes.
//!
//! The `.milk` text is **not** held in the index -- it is read on selection,
//! the way the music library reads tags on scan but audio on play. A row is a
//! couple of hundred bytes; a thousand presets is a few hundred kilobytes of
//! index.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::milk::report::preset_report;
use crate::milk::Preset;

/// How deep a scan will walk before giving up. A guard against a symlink loop
/// turning a scan into a hang, not a real limit -- no preset collection is
/// nested 32 folders deep.
const MAX_DEPTH: usize = 32;

/// One row of the preset library.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PresetRow {
    /// Derived from the path, so rescanning a folder updates rows rather than
    /// duplicating them.
    pub id: String,
    pub path: PathBuf,
    /// The file's own name, always present -- what the UI shows.
    pub file_name: String,
    /// What this preset asks for that this build does not do, and what went
    /// wrong compiling it -- one line each, ready to show beside its name
    /// (decision 56). A preset that fails to parse is *listed as failing*
    /// rather than hidden (decision 50), and its report says so.
    pub report: Vec<String>,
}

/// What a scan found. Reported rather than inferred from the row count,
/// because "nothing was added" and "nothing could be read" are different
/// problems and the UI should be able to say which happened.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PresetScanSummary {
    /// Files with a `.milk` extension that were found.
    pub found: usize,
    /// Of those, how many were read successfully.
    pub added: usize,
    /// Files that could not be read at all.
    pub failed: usize,
}

/// The on-disk shape of the index. Versioned so a later milestone can migrate
/// rather than silently misread an older file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    version: u32,
    folders: Vec<PathBuf>,
    presets: Vec<PresetRow>,
}

const INDEX_VERSION: u32 = 1;

/// The preset library, backed by a directory of its own.
///
/// Holds the whole index in memory: a row is a couple of hundred bytes, so even
/// a ten-thousand-preset collection is a few megabytes, and every operation the
/// UI performs (list, search) is then a plain iteration rather than a query.
pub struct PresetLibrary {
    root: PathBuf,
    index: Index,
}

impl PresetLibrary {
    /// Opens the library kept in `root`, creating it on first run.
    ///
    /// An index that fails to parse is replaced with an empty one rather than
    /// refusing to start: the library is a cache of what is on disk, so the
    /// worst case is a rescan.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let root = root.into();
        fs::create_dir_all(&root)?;

        let index = fs::read(index_path(&root))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Index>(&bytes).ok())
            .filter(|index| index.version == INDEX_VERSION)
            .unwrap_or_default();

        Ok(Self { root, index })
    }

    pub fn rows(&self) -> &[PresetRow] {
        &self.index.presets
    }

    pub fn folders(&self) -> &[PathBuf] {
        &self.index.folders
    }

    /// Walks `folder`, parses every `.milk` file, and merges the result into
    /// the index.
    ///
    /// Rescanning is the same call: rows are keyed by path, so a file that is
    /// still there is updated in place, one that has been deleted drops out,
    /// and one that has been added appears. That means a rescan cannot
    /// duplicate a library.
    pub fn scan(&mut self, folder: impl AsRef<Path>) -> Result<PresetScanSummary, std::io::Error> {
        let folder = folder.as_ref();
        let mut files = Vec::new();
        collect_preset_files(folder, 0, &mut files);

        let mut summary = PresetScanSummary {
            found: files.len(),
            ..PresetScanSummary::default()
        };

        let mut scanned = Vec::with_capacity(files.len());
        for path in files {
            match self.read_row(&path) {
                Ok(row) => {
                    summary.added += 1;
                    scanned.push(row);
                }
                // One unreadable file skips, it does not abort: a collection of
                // a thousand presets will meet a file in an eccentric encoding
                // eventually, and losing the other 999 to it would be absurd.
                Err(_) => summary.failed += 1,
            }
        }

        // Everything previously found under this folder is replaced by what is
        // there now, and rows from other folders are left alone.
        self.index
            .presets
            .retain(|row| !row.path.starts_with(folder));
        self.index.presets.extend(scanned);
        self.index.presets.sort_by(|a, b| a.path.cmp(&b.path));

        if !self.index.folders.iter().any(|f| f == folder) {
            self.index.folders.push(folder.to_path_buf());
        }

        self.save()?;
        Ok(summary)
    }

    /// Forgets a folder and every row under it. The files themselves are not
    /// touched -- this is a library, not a file manager.
    pub fn forget_folder(&mut self, folder: impl AsRef<Path>) -> Result<(), std::io::Error> {
        let folder = folder.as_ref();
        self.index.folders.retain(|f| f != folder);
        self.index
            .presets
            .retain(|row| !row.path.starts_with(folder));
        self.save()
    }

    /// Writes the index out. Via a temporary file and a rename, so a crash
    /// mid-write leaves the previous index intact rather than a half-written
    /// one that `open` would then discard.
    fn save(&self) -> Result<(), std::io::Error> {
        let path = index_path(&self.root);
        let temp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&Index {
            version: INDEX_VERSION,
            ..self.index.clone()
        })
        .map_err(std::io::Error::other)?;

        fs::write(&temp, bytes)?;
        fs::rename(&temp, &path)
    }

    /// Reads one file and builds its row. A file that is not a preset is still
    /// a row -- listed as failing, with the reason, rather than hidden.
    fn read_row(&self, path: &Path) -> Result<PresetRow, std::io::Error> {
        let bytes = fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let report = match Preset::parse(&text) {
            Ok(preset) => preset_report(&preset),
            Err(e) => vec![format!("not a MilkDrop preset: {e}")],
        };
        Ok(PresetRow {
            id: hash_hex(path.to_string_lossy().as_bytes()),
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_path_buf(),
            report,
        })
    }
}

fn index_path(root: &Path) -> PathBuf {
    root.join("presets.json")
}

/// Depth-first walk collecting `.milk` files. Errors are skipped rather than
/// propagated -- an unreadable subdirectory is a normal thing to meet halfway
/// through a collection and is not a reason to fail the scan.
fn collect_preset_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
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
            Ok(ft) if ft.is_file() && is_preset_file(&path) => out.push(path),
            _ => {}
        }
    }

    // Files before subdirectories, and each level sorted, so a scan produces
    // the same order twice running.
    out.sort();
    dirs.sort();
    for dir in dirs {
        collect_preset_files(&dir, depth + 1, out);
    }
}

fn is_preset_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("milk"))
}

/// FNV-1a, rendered hex. Used for row ids over the path.
///
/// Not a cryptographic hash and not trying to be: nothing here defends against
/// a chosen collision, it only needs a short stable name for a byte string.
fn hash_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans itself up.
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

    fn write_preset(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_preset_files_recursively_and_ignores_everything_else() {
        let presets = TempDir::new("presets");
        write_preset(&presets.path().join("a.milk"), "[preset00]\n");
        write_preset(&presets.path().join("sub/b.milk"), "[preset00]\n");
        write_preset(&presets.path().join("sub/deep/c.milk"), "[preset00]\n");
        fs::write(presets.path().join("notes.txt"), b"hello").unwrap();
        fs::write(presets.path().join("clouds.jpg"), [0u8; 8]).unwrap();

        let store = TempDir::new("store");
        let mut library = PresetLibrary::open(store.path()).unwrap();
        let summary = library.scan(presets.path()).unwrap();

        assert_eq!(summary.found, 3, "three .milk files, not the txt or jpg");
        assert_eq!(summary.added, 3);
        assert_eq!(summary.failed, 0);
        assert_eq!(library.rows().len(), 3);
    }

    #[test]
    fn a_rescan_updates_rows_rather_than_duplicating_them() {
        let presets = TempDir::new("presets");
        write_preset(&presets.path().join("a.milk"), "[preset00]\n");
        write_preset(&presets.path().join("b.milk"), "[preset00]\n");

        let store = TempDir::new("store");
        let mut library = PresetLibrary::open(store.path()).unwrap();
        library.scan(presets.path()).unwrap();
        let first: Vec<String> = library.rows().iter().map(|r| r.id.clone()).collect();

        fs::remove_file(presets.path().join("b.milk")).unwrap();
        write_preset(&presets.path().join("c.milk"), "[preset00]\n");
        library.scan(presets.path()).unwrap();

        let names: Vec<&str> = library
            .rows()
            .iter()
            .map(|r| r.file_name.as_str())
            .collect();
        assert_eq!(names, ["a.milk", "c.milk"], "deleted out, added in");
        assert_eq!(
            library.rows()[0].id,
            first[0],
            "a row that didn't move keeps its id"
        );
    }

    #[test]
    fn a_file_that_is_not_a_preset_is_listed_as_failing_rather_than_hidden() {
        let presets = TempDir::new("presets");
        write_preset(&presets.path().join("good.milk"), "[preset00]\n");
        fs::write(presets.path().join("bad.milk"), b"zoom=1\n").unwrap();

        let store = TempDir::new("store");
        let mut library = PresetLibrary::open(store.path()).unwrap();
        let summary = library.scan(presets.path()).unwrap();

        assert_eq!(summary.found, 2);
        assert_eq!(summary.added, 2, "a non-preset is still a row");
        let bad = library
            .rows()
            .iter()
            .find(|r| r.file_name == "bad.milk")
            .unwrap();
        assert!(
            bad.report
                .iter()
                .any(|l| l.contains("not a MilkDrop preset")),
            "the reason is in the report: {:?}",
            bad.report
        );
    }

    #[test]
    fn a_preset_with_an_unsupported_feature_carries_it_in_its_report() {
        let presets = TempDir::new("presets");
        // A shader that names a texture pack this build would load, plus a
        // function the translator does not know -- both land in the report.
        write_preset(
            &presets.path().join("needy.milk"),
            "[preset00]\nwarp_1=`shader_body\nwarp_2=`{\nwarp_3=`ret = nosuchfunction(uv);\nwarp_4=`}\n",
        );

        let store = TempDir::new("store");
        let mut library = PresetLibrary::open(store.path()).unwrap();
        library.scan(presets.path()).unwrap();

        let row = &library.rows()[0];
        assert!(
            row.report.iter().any(|l| l.contains("nosuchfunction")),
            "the unknown function is named: {:?}",
            row.report
        );
    }

    #[test]
    fn the_index_survives_being_reopened() {
        let presets = TempDir::new("presets");
        write_preset(&presets.path().join("a.milk"), "[preset00]\n");

        let store = TempDir::new("store");
        {
            let mut library = PresetLibrary::open(store.path()).unwrap();
            library.scan(presets.path()).unwrap();
        }

        let reopened = PresetLibrary::open(store.path()).unwrap();
        assert_eq!(reopened.rows().len(), 1);
        assert_eq!(reopened.folders(), [presets.path().to_path_buf()]);
    }

    #[test]
    fn a_corrupt_index_opens_empty_instead_of_refusing_to_start() {
        let store = TempDir::new("store");
        fs::write(index_path(store.path()), b"{ this is not json").unwrap();

        let library = PresetLibrary::open(store.path()).unwrap();
        assert!(library.rows().is_empty());
    }

    #[test]
    fn forgetting_a_folder_drops_its_rows_and_leaves_the_others() {
        let one = TempDir::new("presets-one");
        let two = TempDir::new("presets-two");
        write_preset(&one.path().join("a.milk"), "[preset00]\n");
        write_preset(&two.path().join("b.milk"), "[preset00]\n");

        let store = TempDir::new("store");
        let mut library = PresetLibrary::open(store.path()).unwrap();
        library.scan(one.path()).unwrap();
        library.scan(two.path()).unwrap();
        assert_eq!(library.rows().len(), 2);

        library.forget_folder(one.path()).unwrap();
        assert_eq!(library.rows().len(), 1);
        assert_eq!(library.rows()[0].file_name, "b.milk");
        assert_eq!(library.folders().len(), 1);
    }

    #[test]
    fn the_same_bytes_hash_to_the_same_name() {
        assert_eq!(hash_hex(b"a path"), hash_hex(b"a path"));
        assert_ne!(hash_hex(b"a path"), hash_hex(b"b path"));
        assert_eq!(hash_hex(b"anything").len(), 16);
    }
}
