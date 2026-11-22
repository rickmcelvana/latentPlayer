//! Finding the images a preset names, beside the preset on disk (decision 59).
//!
//! Native only: a browser has folder handles rather than paths, so the web
//! shell walks its handle in `presetLibrary.ts` instead. Lives here rather than
//! in `src-tauri` so the desktop shell and the corpus checks load a preset's
//! images by the *same* rules -- a check that found images more generously than
//! the app would pass presets the app draws wrong.

use std::path::{Path, PathBuf};

use super::Preset;

/// Image formats a texture pack or sprite is read from. PNG and JPEG are the
/// whole of what the corpus ships (`jfif` is JPEG under another name).
const EXTS: &[&str] = &["png", "jpg", "jpeg", "jfif"];

/// How many folders up from the preset's own to look: the preset's folder, its
/// parent, and its grandparent. A collection that keeps presets in `presets/`
/// has its images as a sibling; one that nests a level deeper, an ancestor up.
const ANCESTORS: usize = 3;

/// Every image `preset` names, read from beside `folder` (the preset file's
/// own folder), plus one report line per name that was not found.
///
/// The pairs are what [`crate::Renderer::set_textures`] takes: a texture-pack
/// name (`clouds`) or a sprite's `SpriteName` path (`sprites/x1.png`, slashes
/// normalised) with the file's bytes.
pub fn load_images(preset: &Preset, folder: &Path) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
    let mut images = Vec::new();
    let mut report = Vec::new();
    for name in preset.texture_names() {
        match find_image(folder, &name).and_then(|p| std::fs::read(p).ok()) {
            Some(bytes) => images.push((name, bytes)),
            None => report.push(format!("texture:{name}: image not found beside the preset")),
        }
    }
    for sprite in &preset.sprites {
        let name = sprite
            .base
            .get("Name")
            .map(String::as_str)
            .unwrap_or("")
            .replace('\\', "/");
        if name.is_empty() {
            continue;
        }
        match find_sprite_image(folder, &name).and_then(|p| std::fs::read(p).ok()) {
            Some(bytes) => images.push((name, bytes)),
            None => report.push(format!("sprite:{name}: image not found beside the preset")),
        }
    }
    (images, report)
}

/// The folders searched for a texture-pack image, nearest first: `textures/`
/// and `sprites/` (MilkDrop 3 treats them as interchangeable) beside the
/// preset and beside each of its ancestors.
fn search_dirs(folder: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut ancestor = Some(folder);
    for _ in 0..ANCESTORS {
        if let Some(dir) = ancestor {
            dirs.push(dir.join("textures"));
            dirs.push(dir.join("sprites"));
            ancestor = dir.parent();
        }
    }
    dirs
}

/// Finds the image file for a texture-pack name, by stem: `clouds` finds
/// `textures/clouds.jpg`. Case-insensitive, because the corpus was written on
/// Windows and names its files however it likes.
pub fn find_image(folder: &Path, name: &str) -> Option<PathBuf> {
    for dir in search_dirs(folder) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stem_matches = path
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name));
            let known_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()));
            if stem_matches && known_ext {
                return Some(path);
            }
        }
    }
    None
}

/// Finds a sprite's image file. `name` is the path the preset wrote in
/// `SpriteName` (`sprites/x1.png`), resolved against the preset's folder and its
/// ancestors -- the same walk as [`find_image`], but the name already carries
/// its folder, so it is tried as a path rather than a stem.
pub fn find_sprite_image(folder: &Path, name: &str) -> Option<PathBuf> {
    let rel = Path::new(name);
    let mut ancestor = Some(folder);
    for _ in 0..ANCESTORS {
        if let Some(dir) = ancestor {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
            ancestor = dir.parent();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `base/presets/p.milk`, `base/textures/clouds.jpg` and answers
    /// with the `presets` folder, the shape a MilkDrop collection takes.
    fn make_collection(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("milk-files-{tag}-{}", std::process::id()));
        let presets = base.join("presets");
        let textures = base.join("textures");
        std::fs::create_dir_all(&presets).unwrap();
        std::fs::create_dir_all(&textures).unwrap();
        std::fs::write(presets.join("p.milk"), b"unused").unwrap();
        std::fs::write(textures.join("Clouds.JPG"), b"unused").unwrap();
        presets
    }

    #[test]
    fn finds_a_texture_pack_image_in_the_sibling_folder() {
        let presets = make_collection("sibling");
        let found = find_image(&presets, "clouds").expect("Clouds.JPG is found");
        assert!(found.ends_with("Clouds.JPG"), "{found:?}");
        assert!(find_image(&presets, "missing").is_none());
    }

    #[test]
    fn finds_a_sprite_by_its_path_from_an_ancestor() {
        let presets = make_collection("sprite");
        let found = find_sprite_image(&presets, "textures/Clouds.JPG");
        assert!(found.is_some());
        assert!(find_sprite_image(&presets, "textures/none.png").is_none());
    }
}
