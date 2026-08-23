//! Print the WGSL a preset's shaders translate to (Phase 3 M7b).
//!
//! ```text
//! cargo run -p player-render --example milk_wgsl -- <preset.milk> [warp|comp]
//! ```
//!
//! `milk_survey` says how many of a collection translate and compile;
//! this says what one of them *became*, which is the only way to find out why
//! a particular file did not. It validates with naga and prints the error
//! against the generated line, because a translator's bugs are almost always
//! visible in one line of its output and invisible in its input.

use std::fs;

use player_render::milk::hlsl::{emit, parse, Pass};
use player_render::milk::Preset;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: milk_wgsl <preset.milk> [warp|comp]");
        std::process::exit(2);
    };
    let which = args.next().unwrap_or_else(|| "warp".into());
    let pass = if which.starts_with('c') {
        Pass::Comp
    } else {
        Pass::Warp
    };

    let bytes = fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(2);
    });
    let preset = match Preset::parse(&String::from_utf8_lossy(&bytes)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let src = match pass {
        Pass::Warp => &preset.warp,
        Pass::Comp => &preset.comp,
    };
    if src.trim().is_empty() {
        eprintln!("this preset has no {which} shader");
        std::process::exit(1);
    }

    let shader = match parse(src) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("parse: {e} ({})", e.construct);
            std::process::exit(1);
        }
    };
    let out = match emit(&shader, pass) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("emit: {e} ({})", e.construct);
            std::process::exit(1);
        }
    };

    // Only the preset's own half is printed. The prelude is fixed, long, and
    // never the thing being debugged.
    let body = out
        .source
        .split_once("// ---- preset ----")
        .map_or(out.source.as_str(), |(_, rest)| rest);
    for (n, line) in body.lines().enumerate() {
        println!("{:>4}  {line}", n + 1);
    }
    if !out.wants_textures.is_empty() {
        eprintln!("\nwants textures: {}", out.wants_textures.join(", "));
    }

    match naga::front::wgsl::parse_str(&out.source) {
        Ok(module) => {
            let mut v = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            );
            match v.validate(&module) {
                Ok(_) => eprintln!("\nnaga: ok"),
                Err(e) => eprintln!("\nnaga: {}", e.emit_to_string(&out.source)),
            }
        }
        Err(e) => eprintln!("\nnaga: {}", e.emit_to_string(&out.source)),
    }
}
