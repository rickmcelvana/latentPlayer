//! Phase 3 M7's de-risk spike (`tasks/phase-3.md` decision 7): parse a real
//! preset, run its equations, and report what an engine would have to be.
//!
//! Run it against the presets in the repository's `milkdrop/` folder:
//!
//! ```text
//! cargo run -p player-render --example milk_spike
//! cargo run -p player-render --example milk_spike -- path/to/presets
//! ```
//!
//! This is a *report*, not a check that passes -- the milestone has not been
//! designed yet, and the point of a spike is to find out what to design. What
//! it prints is the three unknowns the milestone plan named: how much of the
//! `.milk` surface real presets use, whether the equation language runs fast
//! enough on the thread that draws, and how far the shaders are from WGSL.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use player_render::milk::{eel, MilkFrame, Preset, WarpParams};

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
        "MilkDrop spike -- {} preset(s) in {}\n",
        files.len(),
        root.display()
    );

    let mut totals = Totals::default();
    for path in &files {
        report(path, &mut totals);
    }
    totals.print();
    render_leg(&files);
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

/// The spike's third leg: does MilkDrop's frame -- a feedback loop resampled
/// through a warped mesh -- actually run under wgpu, and does a preset's own
/// `per_pixel` block move the picture the way its arithmetic says?
///
/// Checked by readback rather than by looking, per decision 6. A bright square
/// is written into the feedback pair and then advanced under the preset's warp;
/// if the loop works, the square's lit area and its centre move, and they move
/// where the equations put them.
fn render_leg(files: &[PathBuf]) {
    println!("\n== Render leg: MilkDrop's frame under wgpu ==");

    let instance = wgpu::Instance::default();
    let adapter =
        match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        {
            Ok(a) => a,
            Err(e) => {
                println!("no GPU adapter: {e}");
                return;
            }
        };
    let (device, queue) =
        match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())) {
            Ok(pair) => pair,
            Err(e) => {
                println!("no GPU device: {e}");
                return;
            }
        };

    const SIZE: u32 = 128;
    const OUTPUT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("milk-spike-output"),
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

    // Every preset in the corpus, plus one synthetic control whose motion is
    // known exactly -- the control is what says a null result belongs to the
    // preset rather than to the pipeline.
    let mut cases: Vec<(String, Option<PathBuf>)> =
        vec![("(control: zoom 1.05, no preset)".to_string(), None)];
    for path in files {
        cases.push((
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            Some(path.clone()),
        ));
    }

    for (label, path) in cases {
        let mut frame = MilkFrame::new(&device, OUTPUT, SIZE, SIZE, 32, 24);
        frame.seed(&queue, &seed_square(SIZE));

        let mut symbols = eel::Symbols::new();
        let mut per_frame = None;
        let mut per_pixel = None;
        let mut ctx_holder = None;
        if let Some(path) = &path {
            if let Ok(bytes) = std::fs::read(path) {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if let Ok(preset) = Preset::parse(&text) {
                    per_frame = eel::compile(&preset.per_frame, &mut symbols).ok();
                    per_pixel = eel::compile(&preset.per_pixel, &mut symbols).ok();
                    if let Ok(init) = eel::compile(&preset.init, &mut symbols) {
                        let mut ctx = eel::Context::new(&symbols);
                        ctx.sync(&symbols);
                        seed_inputs(&mut ctx, &symbols, 0.0);
                        let _ = ctx.run(&init);
                        ctx_holder = Some(ctx);
                    }
                }
            }
        }
        let mut ctx = ctx_holder.unwrap_or_else(|| eel::Context::new(&symbols));
        ctx.sync(&symbols);

        const FRAMES: usize = 30;
        for f in 0..FRAMES {
            let time = f as f64 / 60.0;
            seed_inputs(&mut ctx, &symbols, time);
            if let Some(p) = &per_frame {
                let _ = ctx.run(p);
            }
            frame.set_mesh(&queue, |x, y| {
                let Some(p) = &per_pixel else {
                    return WarpParams {
                        zoom: 1.05,
                        ..Default::default()
                    };
                };
                // The per-vertex inputs MilkDrop hands `per_pixel`.
                let (dx, dy) = (f64::from(x) - 0.5, f64::from(y) - 0.5);
                for (name, value) in [
                    ("x", f64::from(x)),
                    ("y", f64::from(y)),
                    ("rad", (dx * dx + dy * dy).sqrt() * 2.0),
                    ("ang", dy.atan2(dx)),
                ] {
                    if let Some(slot) = symbols.existing(name) {
                        ctx.set(slot, value);
                    }
                }
                let _ = ctx.run(p);
                WarpParams::from_context(&ctx, &symbols)
            });
            // A decay just under 1 -- MilkDrop's own `fDecay`, and what makes a
            // feedback loop fade rather than saturate.
            frame.advance(&device, &queue, 0.97);
        }

        let commands = frame.composite(&device, &output_view);
        queue.submit(Some(commands));
        let Some(pixels) = read_back(&device, &queue, &output, SIZE) else {
            println!("  {label}: readback failed");
            continue;
        };
        let (lit, cx, cy, brightest) = measure(&pixels, SIZE);
        println!(
            "  {label}\n     per_pixel {}: lit {lit}px, centre ({cx:.1},{cy:.1}), brightest {brightest}",
            if per_pixel.is_some() { "ran" } else { "absent" }
        );
    }
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
