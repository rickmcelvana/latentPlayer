//! Watches every preset in a folder for a few seconds and says which ones end
//! on a picture that is plainly wrong -- a white screen, or a black one.
//!
//! ```text
//! cargo run --release -p player-render --example milk_look -- <folder> [limit] [out.tsv]
//! ```
//!
//! `milk_smoke` proves a preset parses, compiles and draws a frame without
//! taking the app with it. That is not the same as drawing *right*: a live look
//! at a real collection found a lot of presets that pass every one of those and
//! still fill the window with white. This is the check that sees that, through
//! the app's own path -- the offscreen renderer, the preset loaded the way the
//! desktop shell loads it, its images found by the same rules
//! (`milk::files::load_images`), and a spectrum measured by `player-core`'s own
//! analyser over synthesised music.
//!
//! The folder is walked recursively. `limit` takes every Nth preset so a first
//! pass over eleven thousand files can be a sample. The TSV has one row per
//! preset, so the white ones can be grouped by what they have in common.
//!
//! `root` may also be a single `.milk` file, or a `.txt` list of preset paths
//! (one per line -- presets stay where they are, so their images are still
//! found). With `MILK_LOOK_DUMP=<dir>` set every measured frame is written
//! there as a PNG; with `MILK_LOOK_SHEET=<file.png>` every preset's last frame
//! is tiled, at half size, into one contact sheet in run order -- tile N is
//! TSV row N.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use player_core::analysis::{band_centres, Meters};
use player_render::milk::{files, report, Preset};
use player_render::{OffscreenRenderer, VisualFrame, VisualKind};

/// The frame size, overridable with `MILK_LOOK_SIZE=WxH`. Small by default for
/// speed -- but a one-pixel line covers more of a small frame, so a preset of
/// thousands of additive lines saturates here sooner than at a real window
/// size. Check a suspect at a real size before believing it.
fn size() -> (u32, u32) {
    std::env::var("MILK_LOOK_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((320, 240))
}
const RATE: u32 = 48_000;
const FPS: f32 = 60.0;
/// Four seconds: long enough for a feedback loop to run away or settle, which
/// is the failure being looked for. Most white-outs are complete inside one.
const FRAMES: usize = 240;
/// The frames that are read back and measured.
const MEASURE_AT: [usize; 3] = [30, 120, 240];

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next().map(PathBuf::from) else {
        eprintln!("usage: milk_look <folder> [every-nth] [out.tsv]");
        std::process::exit(2);
    };
    let step: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let out = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("milk_look.tsv"));

    let mut files_found = Vec::new();
    let is_list = root
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
    if is_list {
        let list = std::fs::read_to_string(&root).expect("read the list");
        files_found.extend(
            list.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(PathBuf::from),
        );
    } else if root.is_file() {
        files_found.push(root.clone());
    } else {
        walk(&root, &mut files_found);
        files_found.sort();
    }
    let files_found: Vec<PathBuf> = files_found.into_iter().step_by(step).collect();
    println!(
        "milk_look -- {} preset(s) under {}",
        files_found.len(),
        root.display()
    );

    let music = Music::new();
    let mut renderer = new_renderer();
    let mut tsv = std::fs::File::create(&out).expect("create the TSV");
    writeln!(
        tsv,
        "class\twhite30\twhite120\twhite240\tblack240\tmean240\tversion\tps_warp\tps_comp\twarp_shader\tcomp_shader\twaves\tshapes\tsprites\ttextures\tmissing_images\treport\tms\tpath"
    )
    .unwrap();

    let want_sheet = std::env::var("MILK_LOOK_SHEET").ok();
    let mut sheet: Vec<Vec<u8>> = Vec::new();
    let mut counts = std::collections::BTreeMap::<&'static str, usize>::new();
    let started = Instant::now();
    for (i, path) in files_found.iter().enumerate() {
        let t0 = Instant::now();
        let row = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            look(&mut renderer, path, &music)
        }));
        let row = match row {
            Ok(row) => row,
            Err(_) => {
                // A panic inside wgpu leaves the device in an unknown state.
                renderer = new_renderer();
                Row::failed("PANIC")
            }
        };
        if want_sheet.is_some() {
            sheet.push(renderer.read_pixels().unwrap_or_default());
        }
        *counts.entry(row.class).or_default() += 1;
        writeln!(
            tsv,
            "{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.1}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.class,
            row.white[0],
            row.white[1],
            row.white[2],
            row.black,
            row.mean,
            row.version,
            row.ps_warp,
            row.ps_comp,
            row.warp_shader,
            row.comp_shader,
            row.waves,
            row.shapes,
            row.sprites,
            row.textures,
            row.missing_images,
            row.report.join(" | ").replace(['\t', '\n'], " "),
            t0.elapsed().as_millis(),
            path.display()
        )
        .unwrap();
        if (i + 1) % 100 == 0 {
            println!(
                "  {}/{} in {:.0}s  {:?}",
                i + 1,
                files_found.len(),
                started.elapsed().as_secs_f32(),
                counts
            );
        }
    }
    if let Some(file) = want_sheet {
        write_sheet(&file, &sheet);
    }
    println!("\n{counts:?}\nwrote {}", out.display());
}

/// Tiles each frame at half size, eight to a row, with a grey gutter so a
/// black tile is still visibly a tile.
fn write_sheet(file: &str, frames: &[Vec<u8>]) {
    const PER_ROW: usize = 8;
    let (width, height) = size();
    // Tiles are always 160x120, whatever the render size.
    let step = (width / 160).max(1) as usize;
    let (tw, th) = (width as usize / step, height as usize / step);
    let rows = frames.len().div_ceil(PER_ROW).max(1);
    let (w, h) = (PER_ROW * (tw + 2), rows * (th + 2));
    let mut out = vec![90u8; w * h * 4];
    for (n, frame) in frames.iter().enumerate() {
        if frame.len() < (width * height * 4) as usize {
            continue;
        }
        let (ox, oy) = ((n % PER_ROW) * (tw + 2), (n / PER_ROW) * (th + 2));
        for y in 0..th {
            for x in 0..tw {
                let src = (y * step * width as usize + x * step) * 4;
                let dst = ((oy + y) * w + ox + x) * 4;
                out[dst..dst + 4].copy_from_slice(&frame[src..src + 4]);
            }
        }
    }
    let _ = image::save_buffer(file, &out, w as u32, h as u32, image::ColorType::Rgba8);
}

fn new_renderer() -> OffscreenRenderer {
    let (w, h) = size();
    pollster::block_on(OffscreenRenderer::new(w, h, VisualKind::MilkDrop))
        .expect("an offscreen MilkDrop renderer")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("milk"))
        {
            out.push(path);
        }
    }
}

struct Row {
    class: &'static str,
    white: [f32; 3],
    black: f32,
    mean: f32,
    version: u32,
    ps_warp: u32,
    ps_comp: u32,
    warp_shader: &'static str,
    comp_shader: &'static str,
    waves: usize,
    shapes: usize,
    sprites: usize,
    textures: usize,
    missing_images: usize,
    report: Vec<String>,
}

impl Row {
    fn failed(class: &'static str) -> Self {
        Self {
            class,
            white: [0.0; 3],
            black: 0.0,
            mean: 0.0,
            version: 0,
            ps_warp: 0,
            ps_comp: 0,
            warp_shader: "-",
            comp_shader: "-",
            waves: 0,
            shapes: 0,
            sprites: 0,
            textures: 0,
            missing_images: 0,
            report: Vec::new(),
        }
    }
}

fn look(renderer: &mut OffscreenRenderer, path: &Path, music: &Music) -> Row {
    let Ok(bytes) = std::fs::read(path) else {
        return Row::failed("UNREADABLE");
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let Ok(preset) = Preset::parse(&text) else {
        return Row::failed("PARSE");
    };
    let folder = path.parent().unwrap_or(Path::new("."));
    let (images, missing) = files::load_images(&preset, folder);
    let mut lines = report::preset_report(&preset);
    lines.extend(missing.iter().cloned());

    let shader_state = |body: &str, name: &str| -> &'static str {
        if body.trim().is_empty() {
            "none"
        } else if lines.iter().any(|l| l.starts_with(name)) {
            "FAILED"
        } else {
            "ok"
        }
    };
    let enabled = |base: &std::collections::BTreeMap<String, String>| {
        base.get("enabled")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .is_some_and(|v| v != 0.0)
    };

    let mut row = Row {
        class: "OK",
        white: [0.0; 3],
        black: 0.0,
        mean: 0.0,
        version: preset.version,
        ps_warp: preset.ps_version_warp,
        ps_comp: preset.ps_version_comp,
        warp_shader: shader_state(&preset.warp, "warp shader"),
        comp_shader: shader_state(&preset.comp, "composite shader"),
        waves: preset.waves.iter().filter(|w| enabled(&w.base)).count(),
        shapes: preset.shapes.iter().filter(|s| enabled(&s.base)).count(),
        sprites: preset.sprites.len(),
        textures: preset.texture_names().len(),
        missing_images: missing.len(),
        report: lines,
    };

    if renderer.set_preset(Some(&text)).is_err() {
        row.class = "PARSE";
        return row;
    }
    renderer.set_textures(&images);

    let mut measured = 0;
    for n in 1..=FRAMES {
        let chunk = music.chunk(n);
        let frame = VisualFrame::new(&chunk.samples, 2, RATE, 1.0 / FPS)
            .with_spectrum(&chunk.levels, &music.centres);
        renderer.draw(&frame);
        if MEASURE_AT.contains(&n) {
            let Ok(pixels) = renderer.read_pixels() else {
                row.class = "READBACK";
                return row;
            };
            if let Ok(dir) = std::env::var("MILK_LOOK_DUMP") {
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let out = Path::new(&dir).join(format!("{stem}-{n:03}.png"));
                let (w, h) = size();
                let _ = image::save_buffer(&out, &pixels, w, h, image::ColorType::Rgba8);
            }
            let stats = Stats::of(&pixels);
            row.white[measured] = stats.white;
            if n == FRAMES {
                row.black = stats.black;
                row.mean = stats.mean;
            }
            measured += 1;
        }
    }
    row.class = if row.white[2] > 0.6 {
        "WHITE"
    } else if row.black > 0.98 {
        "BLACK"
    } else {
        "OK"
    };
    row
}

struct Stats {
    /// Share of pixels with every channel at or above 235.
    white: f32,
    /// Share of pixels with every channel at or below 12.
    black: f32,
    mean: f32,
}

impl Stats {
    fn of(pixels: &[u8]) -> Self {
        let (mut white, mut black, mut sum) = (0usize, 0usize, 0u64);
        let n = pixels.len() / 4;
        for p in pixels.chunks_exact(4) {
            let lo = p[0].min(p[1]).min(p[2]);
            let hi = p[0].max(p[1]).max(p[2]);
            if lo >= 235 {
                white += 1;
            }
            if hi <= 12 {
                black += 1;
            }
            sum += u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]);
        }
        Self {
            white: white as f32 / n as f32,
            black: black as f32 / n as f32,
            mean: sum as f32 / (n * 3) as f32,
        }
    }
}

/// Four seconds of something shaped like music, pre-measured: a kick on every
/// beat at 120 BPM, a hat on the offbeat, and a sustained chord. Each frame is
/// handed the last 2048 frames of it and the spectrum `player-core` measured
/// over that frame's new audio.
struct Music {
    signal: Vec<f32>,
    levels: Vec<Vec<f32>>,
    centres: Vec<f32>,
}

struct Chunk {
    samples: Vec<f32>,
    levels: Vec<f32>,
}

impl Music {
    fn new() -> Self {
        let per_frame = (RATE as f32 / FPS) as usize;
        let total = per_frame * (FRAMES + 1) + 2048;
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let beat = RATE as f32 * 0.5;
        let mut signal = Vec::with_capacity(total * 2);
        for n in 0..total {
            let t = n as f32 / RATE as f32;
            let since_beat = (n as f32 % beat) / RATE as f32;
            let since_off = ((n as f32 + beat * 0.5) % beat) / RATE as f32;
            let kick = (-since_beat * 18.0).exp()
                * (std::f32::consts::TAU * (50.0 + 90.0 * (-since_beat * 30.0).exp()) * t).sin();
            let hat = (-since_off * 60.0).exp() * noise() * 0.3;
            let chord = [220.0f32, 277.2, 329.6]
                .iter()
                .map(|f| (std::f32::consts::TAU * f * t).sin())
                .sum::<f32>()
                * 0.08;
            let l = 0.6 * kick + hat + chord;
            let r = 0.6 * kick + hat * 0.7 + chord * 1.1;
            signal.push(l);
            signal.push(r);
        }
        let mut meters = Meters::new(RATE, 2);
        let mut levels = Vec::with_capacity(FRAMES + 1);
        levels.push(Vec::new());
        for f in 1..=FRAMES {
            let start = (2048 + (f - 1) * per_frame) * 2;
            let end = start + per_frame * 2;
            meters.ingest(&signal[start..end], false);
            levels.push(meters.update(1.0 / FPS).to_vec());
        }
        Self {
            signal,
            levels,
            centres: band_centres(RATE),
        }
    }

    fn chunk(&self, frame: usize) -> Chunk {
        let per_frame = (RATE as f32 / FPS) as usize;
        let end = (2048 + frame * per_frame) * 2;
        Chunk {
            samples: self.signal[end - 4096..end].to_vec(),
            levels: self.levels[frame].clone(),
        }
    }
}
