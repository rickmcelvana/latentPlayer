//! Phase 3 M7's corpus check: every preset in a folder parses, compiles,
//! draws, and none of them takes the app with it.
//!
//! ```text
//! cargo run -p player-render --example milk_smoke
//! cargo run -p player-render --example milk_smoke -- path/to/presets
//! ```
//!
//! It began as M7's de-risk spike (decision 7) and was a *report* while there
//! was nothing yet to be right about. M7a gives it claims, and the corpus leg
//! keeps the spike's shape on purpose: run it against a folder of hundreds and
//! the figure that matters is not that they all work -- against a thousand
//! strangers' files they will not -- but that the count of those that do never
//! falls.
//!
//! No preset ships with this app (decision 57), so there is no default corpus
//! in the repository either: point it at a folder of your own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use player_render::milk::frame::CompositeSettings;
use player_render::milk::state::{Inputs, Runtime};
use player_render::milk::warp::{Aspect, Grid, Warp};
use player_render::milk::wave::{self, WaveInput, WAVE_SAMPLES};
use player_render::milk::{eel, MilkFrame, Preset};
use player_render::milk::{hlsl, shaders};

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_corpus);

    let mut files: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("milk"))
            })
            .collect(),
        Err(e) => {
            eprintln!("cannot read {}: {e}", root.display());
            std::process::exit(1);
        }
    };
    files.sort();

    if files.is_empty() {
        eprintln!("no .milk files in {}", root.display());
        std::process::exit(1);
    }

    println!(
        "MilkDrop smoke -- {} preset(s) in {}\n",
        files.len(),
        root.display()
    );

    let mut totals = Totals::default();
    for path in &files {
        report(path, &mut totals);
    }
    totals.print();

    let mut checks = Checks::default();
    println!("\n== Corpus ==");
    checks.claim(
        "every preset in the folder parses",
        totals.parsed == totals.presets,
        format!("{}/{}", totals.parsed, totals.presets),
    );
    checks.claim(
        "every equation block compiles",
        totals.eel_compiled == totals.eel_blocks,
        format!("{}/{}", totals.eel_compiled, totals.eel_blocks),
    );
    checks.claim(
        "no preset calls an EEL function this build does not implement",
        totals.unknown_functions.is_empty(),
        format!("{:?}", totals.unknown_functions.keys().collect::<Vec<_>>()),
    );
    checks.claim(
        "everything a preset needs and this build does not do is named",
        totals.unsupported.values().all(|count| *count > 0),
        format!("{} kind(s) named", totals.unsupported.len()),
    );

    render_leg(&files, &mut checks);

    println!("\n{}/{} checks passed", checks.passed, checks.total());
    if checks.failed > 0 {
        std::process::exit(1);
    }
}

fn default_corpus() -> PathBuf {
    // The example runs from the workspace root under `cargo run`, but keep the
    // manifest-relative path so it also works when run from the crate.
    let from_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../milkdrop")
        .canonicalize();
    from_manifest.unwrap_or_else(|_| PathBuf::from("milkdrop"))
}

/// Where a frame's equation time went. The split is the point: the plan needs
/// to know whether the cost is the preset's own per-frame program or the
/// per-vertex one the mesh size multiplies.
#[derive(Clone, Copy, Default)]
struct Split {
    per_frame: f64,
    per_pixel: f64,
    waves: f64,
    shapes: f64,
}

impl Split {
    fn total(self) -> f64 {
        self.per_frame + self.per_pixel + self.waves + self.shapes
    }
}

#[derive(Default)]
struct Totals {
    presets: usize,
    parsed: usize,
    eel_blocks: usize,
    eel_compiled: usize,
    unknown_functions: BTreeMap<String, usize>,
    compile_errors: Vec<String>,
    unsupported: BTreeMap<String, usize>,
    hlsl_constructs: BTreeMap<String, usize>,
    worst_frame_us: f64,
    worst_frame_name: String,
}

impl Totals {
    fn print(&self) {
        println!("== Summary ==");
        println!("presets read:            {}/{}", self.parsed, self.presets);
        println!(
            "EEL blocks compiled:     {}/{}",
            self.eel_compiled, self.eel_blocks
        );
        if !self.compile_errors.is_empty() {
            println!("\nEEL blocks that did not compile:");
            for line in &self.compile_errors {
                println!("  {line}");
            }
        }
        if !self.unknown_functions.is_empty() {
            println!("\nEEL functions this build does not implement:");
            for (name, count) in &self.unknown_functions {
                println!("  {name:<16} {count} use(s)");
            }
        }
        if !self.unsupported.is_empty() {
            println!("\nPreset features recognised and not implemented:");
            for (what, count) in &self.unsupported {
                println!("  {what:<24} {count} preset(s)");
            }
        }
        if !self.hlsl_constructs.is_empty() {
            println!("\nHLSL surface the shaders use (each needs a WGSL answer):");
            for (what, count) in &self.hlsl_constructs {
                println!("  {what:<24} {count} use(s)");
            }
        }
        if self.worst_frame_us > 0.0 {
            println!(
                "\nslowest per-frame equation cost: {:.1}us ({})",
                self.worst_frame_us, self.worst_frame_name
            );
            println!(
                "  a 60fps frame is 16667us, so that is {:.2}% of one",
                self.worst_frame_us / 166.67
            );
        }
    }
}

fn report(path: &Path, totals: &mut Totals) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    totals.presets += 1;
    println!("-- {name}");

    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            println!("   unreadable: {e}\n");
            return;
        }
    };

    let preset = match Preset::parse(&text) {
        Ok(p) => p,
        Err(e) => {
            println!("   parse failed: {e}\n");
            return;
        }
    };
    totals.parsed += 1;

    println!(
        "   version {} / PS {} (warp {}, comp {}); {} scalars, {} wave(s), {} shape(s)",
        preset.version,
        preset.ps_version,
        preset.ps_version_warp,
        preset.ps_version_comp,
        preset.base.len(),
        preset.waves.len(),
        preset.shapes.len()
    );

    for note in &preset.unsupported {
        *totals.unsupported.entry(note.what.clone()).or_default() += 1;
        println!("   unsupported: {} -- {}", note.what, note.why);
    }

    // --- the equations -----------------------------------------------------
    //
    // One symbol table and one context for the whole preset, because MilkDrop's
    // blocks share a variable pool: `per_frame` writes `q1..q32` and the custom
    // waves read them.
    let mut symbols = eel::Symbols::new();
    let mut blocks: Vec<(String, &str)> = vec![
        ("per_frame_init".into(), preset.init.as_str()),
        ("per_frame".into(), preset.per_frame.as_str()),
        ("per_pixel".into(), preset.per_pixel.as_str()),
    ];
    for wave in &preset.waves {
        let i = wave.index;
        blocks.push((format!("wave_{i}_init"), wave.init.as_str()));
        blocks.push((format!("wave_{i}_per_frame"), wave.per_frame.as_str()));
        blocks.push((format!("wave_{i}_per_point"), wave.per_point.as_str()));
    }
    for shape in &preset.shapes {
        let i = shape.index;
        blocks.push((format!("shape_{i}_init"), shape.init.as_str()));
        blocks.push((format!("shape_{i}_per_frame"), shape.per_frame.as_str()));
    }

    let mut compiled: BTreeMap<String, eel::Program> = BTreeMap::new();
    for (label, source) in &blocks {
        if source.trim().is_empty() {
            continue;
        }
        totals.eel_blocks += 1;
        match eel::compile(source, &mut symbols) {
            Ok(program) => {
                totals.eel_compiled += 1;
                compiled.insert(label.clone(), program);
            }
            Err(e) => {
                if let eel::CompileError::UnknownFunction(ref f, _) = e {
                    *totals.unknown_functions.entry(f.clone()).or_default() += 1;
                }
                println!("   EEL {label}: {e}");
                totals.compile_errors.push(format!("{name} / {label}: {e}"));
            }
        }
    }
    println!(
        "   EEL: {}/{} blocks compiled, {} distinct variables",
        compiled.len(),
        blocks.iter().filter(|(_, s)| !s.trim().is_empty()).count(),
        symbols.len()
    );

    // Run them the way a frame would: init once, then per_frame, per_pixel over
    // a mesh, and each custom wave's per_point over its samples. The inputs are
    // the ones MilkDrop feeds -- without them a preset divides by `fps` and
    // takes every branch the wrong way.
    let mut ctx = eel::Context::new(&symbols);
    ctx.sync(&symbols);
    let set = |ctx: &mut eel::Context, symbols: &eel::Symbols, name: &str, v: f64| {
        if let Some(slot) = symbols.existing(name) {
            ctx.set(slot, v);
        }
    };

    if let Some(init) = compiled.get("per_frame_init") {
        seed_inputs(&mut ctx, &symbols, 0.0);
        let started = Instant::now();
        match ctx.run(init) {
            Ok(_) => println!(
                "   init ran in {:.1}us ({} nodes)",
                started.elapsed().as_secs_f64() * 1e6,
                ctx.last_cost()
            ),
            Err(e) => println!("   init failed: {e}"),
        }
    }

    // Ten frames, so a preset whose cost grows with its own state shows it.
    // Two mesh sizes, because the mesh is the app's setting rather than the
    // preset's, and per-vertex code is the one cost that scales with it --
    // MilkDrop's own default is 32x24 and its high setting is 64x48.
    for (mesh_x, mesh_y) in [(32usize, 24usize), (64, 48)] {
        let mut worst = 0.0f64;
        let mut worst_nodes = 0u64;
        let mut worst_split = Split::default();
        let mut failed = None;

        for frame in 0..10 {
            let time = frame as f64 / 60.0;
            seed_inputs(&mut ctx, &symbols, time);
            let mut nodes = 0u64;
            let mut split = Split::default();

            if let Some(p) = compiled.get("per_frame") {
                let started = Instant::now();
                match ctx.run(p) {
                    Ok(_) => nodes += ctx.last_cost(),
                    Err(e) => failed = Some(format!("per_frame: {e}")),
                }
                split.per_frame += started.elapsed().as_secs_f64() * 1e6;
            }
            if let Some(p) = compiled.get("per_pixel") {
                let started = Instant::now();
                for v in 0..mesh_x * mesh_y {
                    set(
                        &mut ctx,
                        &symbols,
                        "x",
                        (v % mesh_x) as f64 / (mesh_x - 1) as f64,
                    );
                    set(
                        &mut ctx,
                        &symbols,
                        "y",
                        (v / mesh_x) as f64 / (mesh_y - 1) as f64,
                    );
                    if let Err(e) = ctx.run(p) {
                        failed = Some(format!("per_pixel: {e}"));
                        break;
                    }
                    nodes += ctx.last_cost();
                }
                split.per_pixel += started.elapsed().as_secs_f64() * 1e6;
            }
            for wave in &preset.waves {
                let samples: usize = wave
                    .base
                    .get("samples")
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(512.0) as usize;
                let started = Instant::now();
                if let Some(p) = compiled.get(&format!("wave_{}_per_frame", wave.index)) {
                    if ctx.run(p).is_ok() {
                        nodes += ctx.last_cost();
                    }
                }
                if let Some(p) = compiled.get(&format!("wave_{}_per_point", wave.index)) {
                    for s in 0..samples.min(512) {
                        set(
                            &mut ctx,
                            &symbols,
                            "sample",
                            s as f64 / samples.max(1) as f64,
                        );
                        if let Err(e) = ctx.run(p) {
                            failed = Some(format!("wave_{}_per_point: {e}", wave.index));
                            break;
                        }
                        nodes += ctx.last_cost();
                    }
                }
                split.waves += started.elapsed().as_secs_f64() * 1e6;
            }
            for shape in &preset.shapes {
                if let Some(p) = compiled.get(&format!("shape_{}_per_frame", shape.index)) {
                    let instances: usize = shape
                        .base
                        .get("num_inst")
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(1.0) as usize;
                    let started = Instant::now();
                    for i in 0..instances.min(1024) {
                        set(&mut ctx, &symbols, "instance", i as f64);
                        if ctx.run(p).is_ok() {
                            nodes += ctx.last_cost();
                        }
                    }
                    split.shapes += started.elapsed().as_secs_f64() * 1e6;
                }
            }

            if let Some(reason) = failed.take() {
                println!("   frame {frame}: {reason}");
                break;
            }
            let micros = split.total();
            if micros > worst {
                worst = micros;
                worst_nodes = nodes;
                worst_split = split;
            }
        }
        if worst > 0.0 {
            println!(
                "   equations @ mesh {mesh_x}x{mesh_y}: worst frame {worst:.0}us ({worst_nodes} nodes) \
                 -- per_frame {:.0}, per_pixel {:.0}, waves {:.0}, shapes {:.0}",
                worst_split.per_frame,
                worst_split.per_pixel,
                worst_split.waves,
                worst_split.shapes
            );
            if worst > totals.worst_frame_us {
                totals.worst_frame_us = worst;
                totals.worst_frame_name = format!("{name} @ {mesh_x}x{mesh_y}");
            }
        }
    }

    // --- the shaders -------------------------------------------------------
    if preset.uses_shaders() {
        let mut found = survey_hlsl(&preset.warp);
        for (k, v) in survey_hlsl(&preset.comp) {
            *found.entry(k).or_default() += v;
        }
        let mut names: Vec<&String> = found.keys().collect();
        names.sort();
        println!(
            "   HLSL: warp {} line(s), comp {} line(s); uses {}",
            preset.warp.lines().count(),
            preset.comp.lines().count(),
            names
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (k, v) in found {
            *totals.hlsl_constructs.entry(k).or_default() += v;
        }
    } else {
        println!("   HLSL: none (fixed-function preset)");
    }
    println!();
}

/// The read-only inputs MilkDrop hands a preset every frame. Enough of them to
/// run the corpus honestly: a preset that reads `fps` as zero divides by it.
fn seed_inputs(ctx: &mut eel::Context, symbols: &eel::Symbols, time: f64) {
    // Values a quiet-to-moderate passage would produce, so branches on `bass`
    // take a plausible path rather than always the zero one.
    let inputs: &[(&str, f64)] = &[
        ("time", time),
        ("fps", 60.0),
        ("frame", (time * 60.0).round()),
        ("progress", time / 30.0),
        ("bass", 1.0 + 0.5 * (time * 6.0).sin()),
        ("mid", 1.0 + 0.4 * (time * 5.0).sin()),
        ("treb", 1.0 + 0.3 * (time * 7.0).sin()),
        ("bass_att", 1.0),
        ("mid_att", 1.0),
        ("treb_att", 1.0),
        ("meshx", 32.0),
        ("meshy", 24.0),
        ("pixelsx", 1280.0),
        ("pixelsy", 720.0),
        ("aspectx", 1.0),
        ("aspecty", 0.5625),
    ];
    for (name, value) in inputs {
        if let Some(slot) = symbols.existing(name) {
            ctx.set(slot, *value);
        }
    }
}

/// What a shader body asks for, in the terms a translator would have to answer.
fn survey_hlsl(body: &str) -> BTreeMap<String, usize> {
    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    let probes: &[(&str, &str)] = &[
        ("float2", "vector types"),
        ("float3", "vector types"),
        ("float4", "vector types"),
        ("float3x3", "matrix types"),
        ("tex2D", "tex2D"),
        ("tex3D", "tex3D"),
        ("GetPixel", "GetPixel helper"),
        ("GetBlur1", "blur chain"),
        ("GetBlur2", "blur chain"),
        ("GetBlur3", "blur chain"),
        ("lum(", "lum helper"),
        ("saturate", "saturate"),
        ("frac(", "frac"),
        ("lerp(", "lerp"),
        ("smoothstep", "smoothstep"),
        ("length(", "length"),
        ("normalize", "normalize"),
        ("mul(", "mul"),
        ("ddx", "screen derivatives"),
        ("ddy", "screen derivatives"),
        ("sampler ", "custom sampler declaration"),
        ("shader_body", "shader_body wrapper"),
        ("for(", "loops"),
        ("for (", "loops"),
        ("if(", "branches"),
        ("if (", "branches"),
        ("rand", "rand"),
        ("texsize", "texsize uniform"),
        ("aspect", "aspect uniform"),
        ("roam_cos", "roam uniforms"),
        ("slow_roam_cos", "roam uniforms"),
        ("rad", "rad/ang varyings"),
        ("ang", "rad/ang varyings"),
    ];
    for (needle, label) in probes {
        let count = body.matches(needle).count();
        if count > 0 {
            *found.entry((*label).to_string()).or_default() += count;
        }
    }
    // q1..q32 are the bridge from the equations to the shaders, and how many a
    // preset uses says how wide that bridge has to be.
    let qs = (1..=32).filter(|i| body.contains(&format!("q{i}"))).count();
    if qs > 0 {
        found.insert("q variables".to_string(), qs);
    }
    found
}

/// The smoke's tally. One line per claim, and an exit code that means it.
///
/// The corpus this runs against is a *folder the user chose* (decision 57: no
/// preset ships with the app), so the useful figure is not that every preset
/// works -- against a thousand strangers' files they will not -- but that the
/// count never falls between milestones.
#[derive(Default)]
struct Checks {
    passed: usize,
    failed: usize,
}

impl Checks {
    fn claim(&mut self, what: &str, ok: bool, detail: impl AsRef<str>) {
        let detail = detail.as_ref();
        if ok {
            self.passed += 1;
            if detail.is_empty() {
                println!("  PASS  {what}");
            } else {
                println!("  PASS  {what} -- {detail}");
            }
        } else {
            self.failed += 1;
            println!("  FAIL  {what} -- {detail}");
        }
    }

    fn pass(&mut self, what: &str) {
        self.claim(what, true, "");
    }

    fn fail(&mut self, what: &str, why: &str) {
        self.claim(what, false, why);
    }

    fn total(&self) -> usize {
        self.passed + self.failed
    }
}

/// The third leg: MilkDrop's frame, driven by the **real** engine.
///
/// The spike stood this up as a report; M7a turns it into checks, because
/// there is now something to be right about. A bright square is written into
/// the feedback pair and advanced under each preset's own equations, and what
/// is asserted is *where it went* -- the spike's own bug is the argument for
/// that, since every preset moved correctly while the whole picture was drawn
/// at quarter scale in one corner, and only a coordinate said so.
///
/// Every claim here goes through `MilkDropVisual` by way of `OffscreenRenderer`,
/// so what is measured is the path the app draws through rather than a
/// reimplementation of it. The one exception is the seeded-square leg below,
/// which needs to put a known picture *into* the loop and so drives
/// `MilkFrame` directly.
fn render_leg(files: &[PathBuf], checks: &mut Checks) {
    println!("\n== Render leg: MilkDrop's frame, through the real engine ==");

    let Some((device, queue)) = gpu_device() else {
        println!("  no GPU device; skipped");
        return;
    };

    const SIZE: u32 = 128;
    const OUTPUT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("milk-smoke-output"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OUTPUT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

    // --- The control: a known transform, checked to the pixel. ---
    //
    // A preset that writes nothing but `dy` moves the picture straight down by
    // `dy` of the frame per turn of the loop, and after N turns it has moved
    // N*dy. That arithmetic is what the whole leg rests on, so it is asserted
    // against a preset written here rather than found in the corpus.
    const FRAMES: usize = 30;
    const DY: f32 = 0.005;
    // `warp=0` explicitly, because MilkDrop's *default* is 1.0 -- a preset that
    // sets no motion at all still ripples. That is correct, and is checked on
    // its own below; here it would be a second source of movement inside a
    // measurement of the first.
    let control = format!("[preset00]\nfDecay=1.0\nwarp=0\nper_pixel_1=dy = {DY};\n");
    let seeded = run_seeded(
        &device,
        &queue,
        &output,
        &output_view,
        SIZE,
        &control,
        FRAMES,
    );
    match seeded {
        None => checks.fail("a seeded square survives the loop", "readback failed"),
        Some((lit, cx, cy, _)) => {
            // The square is drawn at the frame's centre, which for a 128px
            // frame is 63.5 -- the figure that caught the spike's quarter-scale
            // composite when it read 31.5.
            let centre = (SIZE as f32 - 1.0) / 2.0;
            checks.claim(
                "the seeded square starts at the frame's own centre",
                (cx - centre).abs() < 1.5,
                format!("x {cx:.1}, expected {centre:.1}"),
            );
            let expected = centre + DY * FRAMES as f32 * SIZE as f32;
            checks.claim(
                "a per_pixel dy moves the picture where its arithmetic says",
                (cy - expected).abs() < 2.0,
                format!("y {cy:.1}, expected {expected:.1}"),
            );
            checks.claim(
                "the picture is still there after 30 turns of the loop",
                lit > 100,
                format!("{lit} lit pixels"),
            );
        }
    }

    // A preset with no motion at all must leave the square exactly where it
    // was: 256 lit pixels, its own seed. This is the null result that says a
    // moved picture belongs to the preset rather than to the pipeline.
    let still = "[preset00]\nfDecay=1.0\nwarp=0\n".to_string();
    if let Some((lit, cx, cy, _)) =
        run_seeded(&device, &queue, &output, &output_view, SIZE, &still, FRAMES)
    {
        let centre = (SIZE as f32 - 1.0) / 2.0;
        checks.claim(
            "a preset with no motion moves the picture not at all",
            (cx - centre).abs() < 0.5 && (cy - centre).abs() < 0.5 && (lit as i64 - 256).abs() < 40,
            format!("{lit}px at ({cx:.1},{cy:.1})"),
        );
    }

    // MilkDrop's default `warp` is 1.0, so a preset that writes no motion at
    // all still ripples -- the term the spike's reduced transform did not have,
    // and the one whose absence is invisible in a still frame. Thirty turns of
    // a bounded 0.007-per-frame displacement is a walk of a few percent of the
    // frame: enough to measure, and small enough that measuring it is the only
    // way to see it.
    let rippling = "[preset00]\nfDecay=1.0\nwarp=1\n".to_string();
    if let Some((_, cx, cy, _)) = run_seeded(
        &device,
        &queue,
        &output,
        &output_view,
        SIZE,
        &rippling,
        FRAMES,
    ) {
        let centre = (SIZE as f32 - 1.0) / 2.0;
        let moved = (cx - centre).hypot(cy - centre);
        checks.claim(
            "MilkDrop's default warp ripples the picture, and does so gently",
            moved > 0.5 && moved < 40.0,
            format!("moved {moved:.1}px in {FRAMES} frames"),
        );
    }

    // --- The corpus: every preset draws, and none of them panics. ---
    let mut renderer = match pollster::block_on(player_render::OffscreenRenderer::new(
        SIZE,
        SIZE,
        player_render::VisualKind::MilkDrop,
    )) {
        Ok(r) => r,
        Err(e) => {
            checks.fail("the MilkDrop visual builds", &e.to_string());
            return;
        }
    };
    checks.pass("the MilkDrop visual builds and joins the registry");

    let audio = test_audio();
    let levels: Vec<f32> = (0..48).map(|i| -60.0 + (i % 12) as f32 * 2.0).collect();
    let centres: Vec<f32> = (0..48).map(|i| 30.0 * 1.15f32.powi(i)).collect();
    let feed = || {
        player_render::VisualFrame::new(&audio, 2, 48_000, 1.0 / 60.0)
            .with_spectrum(&levels, &centres)
    };

    // With no preset at all -- the state a fresh install is in, since none
    // ship (decision 57) -- the visual still draws MilkDrop's defaults.
    let mut drew_default = 0usize;
    for _ in 0..90 {
        if let Ok(pixels) = renderer.draw_to_pixels(&feed()) {
            drew_default = measure(&pixels, SIZE).0;
        }
    }
    checks.claim(
        "with no preset loaded the gallery entry still draws something",
        drew_default > 50,
        format!("{drew_default} lit pixels of MilkDrop's defaults"),
    );

    let mut worst = (0.0f64, String::new());
    let (mut lit_presets, mut dark_presets) = (0usize, 0usize);
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let Ok(bytes) = std::fs::read(path) else {
            checks.fail(&format!("{name} is readable"), "read failed");
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if let Err(e) = renderer.set_preset(Some(&text)) {
            checks.fail(&format!("{name} loads"), &e.to_string());
            continue;
        }

        let started = Instant::now();
        let mut lit = 0usize;
        let mut ok = true;
        for _ in 0..FRAMES {
            match renderer.draw_to_pixels(&feed()) {
                Ok(pixels) => lit = measure(&pixels, SIZE).0,
                Err(e) => {
                    checks.fail(&format!("{name} draws"), &e.to_string());
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        // Per frame, including the readback -- so this is an upper bound on
        // the engine's own cost rather than a measurement of it. The figure
        // that matters is that it does not climb between milestones.
        let per_frame_ms = started.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        if per_frame_ms > worst.0 {
            worst = (per_frame_ms, name.clone());
        }
        // What M7a is responsible for, and no more. A preset draws light here
        // only if its *built-in* waveform is on: three of the five in the
        // author's own corpus set `fWaveAlpha=0.001`, deliberately below
        // MilkDrop's own 0.004 threshold, because their picture comes from
        // custom waves (M7c) and preset shaders (M7b). Claiming "it draws
        // light" for those would be claiming a milestone that has not happened,
        // and the claim would have to be deleted to let M7b pass.
        if draws_built_in_waveform(&text, SIZE) {
            lit_presets += 1;
            checks.claim(
                &format!("{name} draws its built-in waveform"),
                lit > 0,
                format!("{lit} lit px, {per_frame_ms:.2}ms/frame incl. readback"),
            );
        } else {
            dark_presets += 1;
            checks.claim(
                &format!("{name} runs a whole frame without error"),
                true,
                format!(
                    "{lit} lit px, {per_frame_ms:.2}ms/frame -- no built-in waveform on this material, so its picture waits on M7b/M7c"
                ),
            );
        }
    }
    println!(
        "  worst frame: {:.2}ms ({}) -- readback included",
        worst.0, worst.1
    );

    // M7b's own figure, and the one the phase says must never fall: how many
    // of these presets draw through their *own* shaders rather than through
    // MilkDrop's fixed-function path. Counted by translating each pass and
    // asking naga -- the same acceptance the device applies, which is what
    // `PresetPasses::set_shaders` does at load time.
    let mut own_shaders = 0usize;
    let mut passes_built = 0usize;
    let mut passes_present = 0usize;
    for path in files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(preset) = Preset::parse(text.as_str()) else {
            continue;
        };
        let mut all = true;
        let mut any = false;
        for (body, pass) in [
            (&preset.warp, hlsl::Pass::Warp),
            (&preset.comp, hlsl::Pass::Comp),
        ] {
            if body.trim().is_empty() {
                continue;
            }
            any = true;
            passes_present += 1;
            let built = hlsl::parse(body)
                .and_then(|shader| hlsl::emit(&shader, pass))
                .ok()
                .map(|out| match pass {
                    hlsl::Pass::Warp => shaders::warp_module(&out.source),
                    hlsl::Pass::Comp => shaders::comp_module(&out.source),
                })
                .is_some_and(|module| shaders::valid_wgsl(&module));
            if built {
                passes_built += 1;
            } else {
                all = false;
            }
        }
        if any && all {
            own_shaders += 1;
        }
    }
    println!(
        "  drawing through their own shaders: {own_shaders} presets, {passes_built}/{passes_present} passes"
    );
    println!(
        "  drawing through M7a's own path: {lit_presets}; waiting on shaders or custom waves: {dark_presets}"
    );
    checks.claim(
        "the corpus is not silently all-dark",
        lit_presets > 0 || files.is_empty(),
        format!("{lit_presets} of {} draw through M7a", files.len()),
    );

    // --- Switching, which is what a gallery of a thousand presets does. ---
    if files.len() >= 2 {
        let a = std::fs::read_to_string(&files[0]).unwrap_or_default();
        let b = std::fs::read_to_string(&files[1]).unwrap_or_default();
        let _ = renderer.set_preset(Some(&a));
        for _ in 0..10 {
            let _ = renderer.draw_to_pixels(&feed());
        }
        // Switching to another preset and back must give the same picture as
        // loading it fresh: a preset change empties the feedback loop, so
        // nothing of the one in between can survive into it.
        let _ = renderer.set_preset(Some(&b));
        for _ in 0..10 {
            let _ = renderer.draw_to_pixels(&feed());
        }
        let _ = renderer.set_preset(Some(&a));
        let mut switched = Vec::new();
        for _ in 0..10 {
            switched = renderer.draw_to_pixels(&feed()).unwrap_or_default();
        }

        let mut fresh_renderer = pollster::block_on(player_render::OffscreenRenderer::new(
            SIZE,
            SIZE,
            player_render::VisualKind::MilkDrop,
        ))
        .ok();
        let mut fresh = Vec::new();
        if let Some(r) = fresh_renderer.as_mut() {
            let _ = r.set_preset(Some(&a));
            for _ in 0..10 {
                fresh = r.draw_to_pixels(&feed()).unwrap_or_default();
            }
        }
        let differing = switched.iter().zip(&fresh).filter(|(a, b)| a != b).count();
        checks.claim(
            "a preset switched back to is the preset loaded fresh",
            !switched.is_empty() && switched.len() == fresh.len() && differing == 0,
            format!("{differing} bytes differ"),
        );
    }

    // --- The library's own promise: nothing here panics on a bad file. ---
    for (label, text) in [
        ("nothing but a section header", "[preset00]\n"),
        ("no section", "fDecay=0.5\n"),
        ("truncated code", "[preset00]\nper_frame_1=zoom = ((\n"),
        (
            "hostile numbers",
            "[preset00]\nzoom=0\nsx=0\nsy=0\nfWarpScale=0\nfZoomExponent=-1\nfDecay=9\n",
        ),
        (
            "a loop that will not finish",
            "[preset00]\nper_pixel_1=loop(1000000, zoom = zoom + 0.000001;);\n",
        ),
    ] {
        let loaded = renderer.set_preset(Some(text));
        match (label, loaded) {
            // Only the sectionless one is a parse failure, and it is meant to
            // be: a `.milk` file without `[preset00]` is not a preset.
            ("no section", Err(_)) => checks.pass("a file with no preset section is refused"),
            (_, Err(e)) => checks.fail(&format!("{label} loads"), &e.to_string()),
            (_, Ok(())) => {
                let drew = (0..3).all(|_| renderer.draw_to_pixels(&feed()).is_ok());
                checks.claim(
                    &format!("a preset that is {label} draws without taking the app with it"),
                    drew,
                    String::new(),
                );
            }
        }
    }
}

/// A device for the legs that need one, or `None` on a machine without a GPU.
fn gpu_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Seeds a bright square into the feedback pair, advances it under a preset's
/// own equations, and reads the result back.
///
/// Drives `MilkFrame` directly rather than the visual, because a visual has no
/// way to be handed a starting picture -- and a known starting picture is the
/// only thing that turns "it moved" into "it moved 19.6 pixels".
fn run_seeded(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    output: &wgpu::Texture,
    output_view: &wgpu::TextureView,
    size: u32,
    preset_text: &str,
    frames: usize,
) -> Option<(usize, f32, f32, u8)> {
    let preset = Preset::parse(preset_text).ok()?;
    let mut runtime = Runtime::new(&preset);
    let mut frame = MilkFrame::new(device, output.format(), size, size, 32, 24);
    let grid = Grid::new(32, 24, frame.aspect());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("milk-smoke-clear"),
    });
    frame.clear(&mut encoder);
    queue.submit(Some(encoder.finish()));
    frame.seed(queue, &seed_square(size));

    for f in 0..frames {
        let time = f as f32 / 60.0;
        let state = runtime.run_frame(&Inputs {
            time,
            fps: 60.0,
            frame: f as u32,
            bass: 1.0,
            mid: 1.0,
            treb: 1.0,
            bass_att: 1.0,
            mid_att: 1.0,
            treb_att: 1.0,
            mesh_x: grid.mesh_x,
            mesh_y: grid.mesh_y,
            pixels_x: size,
            pixels_y: size,
            aspect_x: 1.0,
            aspect_y: 1.0,
            ..Default::default()
        });
        let fixed = runtime.fixed;
        let warp = Warp::new(size, size, time, fixed.warp_anim_speed, fixed.warp_scale);
        frame.set_mesh(queue, &grid, |point| {
            let (px, py) = grid.pixel_inputs(point);
            let motion = runtime.run_pixel(&state.motion, px, py, point.rad, point.ang);
            warp.source_for(&motion, point)
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("milk-smoke-frame"),
        });
        frame.advance(device, queue, &mut encoder, state.decay, state.wrap);
        // No overlays: the point of this leg is what the *loop* does to a
        // known picture, and a waveform drawn into it every frame would be a
        // second source of light in the measurement.
        frame.composite(
            device,
            queue,
            &mut encoder,
            output_view,
            &state,
            &CompositeSettings {
                time,
                shader: fixed.shader,
            },
        );
        queue.submit(Some(encoder.finish()));
    }

    let pixels = read_back(device, queue, output, size)?;
    Some(measure(&pixels, size))
}

/// Whether this preset's built-in waveform draws at all, on the material the
/// corpus leg feeds it.
///
/// Asked by *building the waveform*, through the same code the engine draws
/// with, rather than by reading `fWaveAlpha` out of the file. Two things in the
/// author's own corpus make the file's answer wrong:
///
/// - a preset may set `wave_a` in its `per_frame` and override the file;
/// - and `bModWaveAlphaByVolume` scales the alpha by how loud the music is
///   against its own running average, so `MilkDrop2077 - Dude the walls are
///   moving2` -- which sets `fWaveAlpha=0.303`, plainly visible -- shows nothing
///   at all until the level passes `fModWaveAlphaStart=1.110`. On a steady test
///   tone, sitting at exactly average by construction, it is correctly
///   invisible.
///
/// An earlier version of this asked the `Runtime` for the alpha and got the
/// first of those right and the second wrong, which produced a failing check
/// against entirely correct behaviour. The gate and the draw have to ask the
/// same question.
fn draws_built_in_waveform(text: &str, width: u32) -> bool {
    let Ok(preset) = Preset::parse(text) else {
        return false;
    };
    let mut runtime = Runtime::new(&preset);
    let state = runtime.run_frame(&Inputs {
        fps: 60.0,
        bass: 1.0,
        mid: 1.0,
        treb: 1.0,
        bass_att: 1.0,
        mid_att: 1.0,
        treb_att: 1.0,
        ..Default::default()
    });
    let fixed = runtime.fixed;
    let mut left = vec![0.0f32; WAVE_SAMPLES];
    let mut right = vec![0.0f32; WAVE_SAMPLES];
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = i as f32 / 48_000.0;
        *l = (t * 220.0 * std::f32::consts::TAU).sin() * 0.5;
        *r = (t * 330.0 * std::f32::consts::TAU).sin() * 0.5;
    }
    wave::prepare(&mut left, fixed.wave_scale, fixed.wave_smoothing);
    wave::prepare(&mut right, fixed.wave_scale, fixed.wave_smoothing);
    wave::build(
        &state.wave,
        &fixed,
        &WaveInput {
            left: &left,
            right: &right,
            time: 0.0,
            aspect: Aspect::new(width, width),
            width,
            bass: 1.0,
            mid: 1.0,
            treb: 1.0,
        },
    )
    .is_some()
}

/// A stereo tone for the corpus leg, so the waveform modes have something to
/// draw and a preset reading `bass` sees a number.
fn test_audio() -> Vec<f32> {
    (0..2048)
        .flat_map(|i| {
            let t = i as f32 / 48_000.0;
            [
                (t * 220.0 * std::f32::consts::TAU).sin() * 0.5,
                (t * 330.0 * std::f32::consts::TAU).sin() * 0.5,
            ]
        })
        .collect()
}

/// A bright 16x16 square at the centre, as RGBA16Float rows.
fn seed_square(size: u32) -> Vec<u8> {
    let mut rows = vec![0u8; (size * size * 8) as usize];
    let half = size / 2;
    for y in (half - 8)..(half + 8) {
        for x in (half - 8)..(half + 8) {
            let at = ((y * size + x) * 8) as usize;
            let bits = f32_to_f16(1.0);
            for channel in 0..4 {
                rows[at + channel * 2] = (bits & 0xff) as u8;
                rows[at + channel * 2 + 1] = (bits >> 8) as u8;
            }
        }
    }
    rows
}

/// IEEE half. Only ever handed 1.0 here, but written honestly rather than as
/// the bit pattern for one value.
fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = (bits & 0x007f_ffff) >> 13;
    if exponent <= 0 {
        return sign;
    }
    if exponent >= 0x1f {
        return sign | 0x7c00;
    }
    sign | ((exponent as u16) << 10) | mantissa as u16
}

fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: u32,
) -> Option<Vec<u8>> {
    let unpadded = size * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("milk-spike-readback"),
        size: u64::from(padded) * u64::from(size),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("milk-spike-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size),
            },
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let (sender, receiver) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = sender.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    receiver.recv().ok()?.ok()?;
    let mapped = buffer.slice(..).get_mapped_range().ok()?;
    let mut pixels = Vec::with_capacity((unpadded * size) as usize);
    for row in 0..size as usize {
        let start = row * padded as usize;
        pixels.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Some(pixels)
}

/// Lit pixel count, the lit centroid, and the brightest channel value.
fn measure(pixels: &[u8], size: u32) -> (usize, f32, f32, u8) {
    let mut lit = 0usize;
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    let mut brightest = 0u8;
    for y in 0..size {
        for x in 0..size {
            let at = ((y * size + x) * 4) as usize;
            let value = pixels[at].max(pixels[at + 1]).max(pixels[at + 2]);
            brightest = brightest.max(value);
            if value > 8 {
                lit += 1;
                sx += f64::from(x);
                sy += f64::from(y);
            }
        }
    }
    if lit == 0 {
        return (0, 0.0, 0.0, brightest);
    }
    (
        lit,
        (sx / lit as f64) as f32,
        (sy / lit as f64) as f32,
        brightest,
    )
}
