//! Live check of the preset library scan, parse and index (Phase 3 M7c item 3).
//! Synthesises its own preset files, so it needs no fixture and no arguments:
//!
//!   cargo run -p player-render --example preset_library_smoke
//!
//! The native counterpart of `/preset.html` in the web build, running the same
//! checks against the same claims so the two shells can be held to one standard.
//!
//! Like the music library's smoke it opens no GPU device: the library never
//! touches the renderer, which is itself one of the milestone's claims. What it
//! exercises is the whole of the real thing -- a real directory tree, the real
//! parser, a real index written to a real cache.

use std::fs;
use std::path::PathBuf;

use player_render::milk::PresetLibrary;

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "latentplayer-preset-smoke-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn write(path: &PathBuf, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn main() {
    let presets = scratch("presets");
    let store = scratch("store");

    // One good preset, one that names a texture pack (which the report tracks),
    // one whose shader calls a function this build does not know, and one that
    // is not a preset at all -- the four cases a row has to render.
    write(&presets.join("good.milk"), "[preset00]\nfDecay=0.9\n");
    write(
        &presets.join("textured.milk"),
        "[preset00]\nwarp_1=`sampler sampler_fw_clouds;\nwarp_2=`shader_body\nwarp_3=`{\nwarp_4=`ret = tex2D(sampler_fw_clouds, uv).xyz;\nwarp_5=`}\n",
    );
    write(
        &presets.join("needy.milk"),
        "[preset00]\nwarp_1=`shader_body\nwarp_2=`{\nwarp_3=`ret = nosuchfunction(uv);\nwarp_4=`}\n",
    );
    write(&presets.join("not-a-preset.milk"), "zoom=1\n");
    // Two files the scan must ignore.
    fs::write(presets.join("notes.txt"), b"not a preset").unwrap();
    fs::write(presets.join("clouds.jpg"), [0u8; 16]).unwrap();

    println!("preset library smoke -- scanning {}", presets.display());

    let mut library = PresetLibrary::open(&store).unwrap();
    let summary = library.scan(&presets).unwrap();

    check(
        "the scan finds .milk files and only .milk files",
        summary.found == 4 && summary.added == 4 && summary.failed == 0,
        format!(
            "found {}, added {}, failed {} (a .txt and a .jpg were present)",
            summary.found, summary.added, summary.failed
        ),
    );

    let good = library.rows().iter().find(|r| r.file_name == "good.milk");
    check(
        "a clean preset has an empty report",
        good.is_some_and(|r| r.report.is_empty()),
        match good {
            Some(r) => format!("{} line(s): {:?}", r.report.len(), r.report),
            None => "no good.milk row".to_string(),
        },
    );

    let needy = library.rows().iter().find(|r| r.file_name == "needy.milk");
    check(
        "a preset with an unknown function names it in its report",
        needy.is_some_and(|r| r.report.iter().any(|l| l.contains("nosuchfunction"))),
        match needy {
            Some(r) => format!("{:?}", r.report),
            None => "no needy.milk row".to_string(),
        },
    );

    let not_a_preset = library
        .rows()
        .iter()
        .find(|r| r.file_name == "not-a-preset.milk");
    check(
        "a file that is not a preset is listed as failing rather than hidden",
        not_a_preset.is_some_and(|r| r.report.iter().any(|l| l.contains("not a MilkDrop preset"))),
        match not_a_preset {
            Some(r) => format!("{:?}", r.report),
            None => "no not-a-preset.milk row".to_string(),
        },
    );

    // A rescan must be idempotent -- scan twice, do not get a library twice the
    // size. The same claim the music library's smoke makes.
    let first_ids: Vec<String> = library.rows().iter().map(|r| r.id.clone()).collect();
    library.scan(&presets).unwrap();
    let second_ids: Vec<String> = library.rows().iter().map(|r| r.id.clone()).collect();
    check(
        "rescanning updates rows rather than duplicating them",
        first_ids == second_ids,
        format!(
            "{} rows before, {} after",
            first_ids.len(),
            second_ids.len()
        ),
    );

    // Reopening is the whole point of persisting: a library that has to be
    // rescanned on every launch is not a library.
    drop(library);
    let reopened = PresetLibrary::open(&store).unwrap();
    check(
        "the index survives a restart",
        reopened.rows().len() == 4 && reopened.folders().len() == 1,
        format!(
            "{} rows and {} folder(s) read back from disk",
            reopened.rows().len(),
            reopened.folders().len()
        ),
    );

    // Forgetting a folder drops its rows.
    let mut reopened = reopened;
    reopened.forget_folder(&presets).unwrap();
    check(
        "forgetting a folder drops its rows",
        reopened.rows().is_empty() && reopened.folders().is_empty(),
        format!("{} rows left", reopened.rows().len()),
    );

    let _ = fs::remove_dir_all(&presets);
    let _ = fs::remove_dir_all(&store);
}
