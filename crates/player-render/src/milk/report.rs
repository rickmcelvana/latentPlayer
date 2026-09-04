//! A preset's report: what it asks for that this build does not do, what went
//! wrong compiling its equations, and what the shader translator made of it.
//!
//! One list, computed the same way whether a library scan is describing a
//! preset it has not selected or the renderer is describing the one it is
//! drawing -- so a row and a selection cannot disagree about what a preset
//! needs (decision 50, and the same rule `Renderer::preset_report` already
//! follows). The shader half is the same translation the MilkDrop visual runs
//! to build its pipelines, factored out so the two cannot drift.

use crate::milk::hlsl::{self, Pass};
use crate::milk::shaders::{self, MAX_USER_TEXTURES};
use crate::milk::state::Runtime;
use crate::milk::Preset;

/// Translates a preset's two shader passes, returning the WGSL sources and
/// appending one line per translation failure to `report`.
///
/// Per pass, not per preset: **605 presets in the corpus give one pass a
/// shader and leave the other fixed-function**, so a composite that will not
/// translate must not cost a preset its warp.
pub fn translate_shaders(
    preset: &Preset,
    report: &mut Vec<String>,
) -> (Option<String>, Option<String>) {
    let textures = preset.texture_names();
    let mut build = |source: &str, pass: Pass, name: &str| -> Option<String> {
        if source.trim().is_empty() {
            return None;
        }
        match hlsl::parse(source)
            .and_then(|shader| hlsl::emit_with_textures(&shader, pass, &textures))
        {
            Ok(out) => Some(out.source),
            Err(e) => {
                report.push(format!("{name} shader, line {}: {}", e.line, e.detail));
                None
            }
        }
    };
    let warp = build(&preset.warp, Pass::Warp, "warp");
    let comp = build(&preset.comp, Pass::Comp, "composite");
    (warp, comp)
}

/// The shader half of a preset's report: what the translator made of its two
/// passes, one line per thing that did not take.
pub fn shader_report(preset: &Preset) -> Vec<String> {
    let mut report = Vec::new();
    let textures = preset.texture_names();
    for overflow in textures.iter().skip(MAX_USER_TEXTURES) {
        report.push(format!(
            "texture:{overflow}: not loaded; more textures than one preset can name"
        ));
    }
    let (warp, comp) = translate_shaders(preset, &mut report);
    if warp.is_some() && !shaders::valid_wgsl(&shaders::warp_module(warp.as_deref().unwrap())) {
        report.push("warp shader: translated but would not compile".into());
    }
    if comp.is_some() && !shaders::valid_wgsl(&shaders::comp_module(comp.as_deref().unwrap())) {
        report.push("composite shader: translated but would not compile".into());
    }
    report
}

/// The full report for a preset, in the order `Renderer::preset_report` shows
/// it: what it asks for that this build does not do, what went wrong compiling
/// its equations, then what the shader translator made of it.
pub fn preset_report(preset: &Preset) -> Vec<String> {
    let mut report = Vec::new();
    report.extend(
        preset
            .unsupported
            .iter()
            .map(|u| format!("{}: {}", u.what, u.why)),
    );
    report.extend(Runtime::new(preset).faults().iter().map(|f| f.to_string()));
    report.extend(shader_report(preset));
    report
}
