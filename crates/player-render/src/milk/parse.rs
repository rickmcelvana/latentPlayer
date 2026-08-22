//! Reading a `.milk` file into something structured.
//!
//! The format is an INI-ish list of `key=value` lines with three habits that
//! are easy to get subtly wrong, and all three are things a real corpus finds
//! rather than things a specification says:
//!
//! 1. **Code arrives one line per key**, numbered from 1 -- `per_frame_1`,
//!    `per_frame_2`, ... -- and the lines must be joined with a **newline**
//!    rather than concatenated. A preset in the shipped corpus writes
//!    `rx2 = gmegabuf((i2+o)%ni*8+5);//mirrored`, and concatenating would let
//!    that comment eat the next statement.
//! 2. **The numbering is not consistently spelled.** The top-level blocks put
//!    an underscore before the index (`per_frame_1`, `per_pixel_1`,
//!    `per_frame_init_1`) and the per-wave/per-shape blocks do not
//!    (`wave_0_per_frame1`, `wave_0_per_point1`, `shape_0_per_frame1`). Both
//!    spellings are in the corpus in `milkdrop/`; a parser that assumes one
//!    silently reads an empty program for half of a preset.
//! 3. **Shader lines carry a leading backtick** (`` warp_1=`sampler ... ``).
//!    MilkDrop 2 added it so leading whitespace survives a round trip through
//!    the INI writer. It is a marker, not content, and exactly one is stripped.
//!
//! Line *order* is file order, not numeric order. That is Butterchurn's
//! behaviour (it filters the file's lines by a prefix regex and joins them),
//! and it differs from MilkDrop's own reader, which asks for index 1, then 2,
//! and stops at the first index missing. The difference only shows on a preset
//! with a gap in its numbering, where MilkDrop truncates the block and
//! Butterchurn keeps it -- so file order is the forgiving reading, and
//! forgiving is the right default for a feature whose whole point is that a
//! listener can drop a thousand strangers' files into a folder.

use std::collections::BTreeMap;
use std::fmt;

/// Something in the file this build recognises but does not implement.
///
/// Collected rather than rejected. A preset is not a program this app wrote,
/// and "most of it works" beats "it refused" for every preset in a dumped
/// collection -- but a silently ignored feature is how an engine ends up with
/// a reputation for drawing the wrong thing, so each one is named here and the
/// caller decides.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unsupported {
    /// What was found, as a short stable id (`"sprite"`, `"texture:clouds"`).
    pub what: String,
    /// Why it is not handled, in one sentence, for a human reading a report.
    pub why: &'static str,
}

/// A parsed preset: the scalar state, the five code blocks, and the custom
/// waves and shapes with code blocks of their own.
#[derive(Clone, Debug, Default)]
pub struct Preset {
    /// `MILKDROP_PRESET_VERSION` -- 100/101/200/201 in the wild, 300 for the
    /// MilkDrop 3 / MilkDrop2077 fork.
    pub version: u32,
    /// `PSVERSION`, and the per-shader overrides where a preset gives them.
    /// 0 means "no pixel shaders, use the fixed-function path"; 2, 3 and 4
    /// select successive HLSL profiles.
    pub ps_version: u32,
    pub ps_version_warp: u32,
    pub ps_version_comp: u32,
    /// Every non-code `key=value` from `[preset00]`, verbatim. Interpreting
    /// these is the engine's job, not the parser's -- the parser's job is to
    /// lose nothing.
    pub base: BTreeMap<String, String>,
    /// `per_frame_init_N` -- run once when the preset is loaded.
    pub init: String,
    /// `per_frame_N` -- run once per frame.
    pub per_frame: String,
    /// `per_pixel_N` (spelled `per_vertex_N` by some tools) -- run per warp
    /// mesh vertex.
    pub per_pixel: String,
    /// `` warp_N=`... `` -- the HLSL body of the warp pass.
    pub warp: String,
    /// `` comp_N=`... `` -- the HLSL body of the composite pass.
    pub comp: String,
    pub waves: Vec<CustomWave>,
    pub shapes: Vec<CustomShape>,
    /// What was recognised and not implemented. See [`Unsupported`].
    pub unsupported: Vec<Unsupported>,
}

/// One of MilkDrop's four custom waveforms.
#[derive(Clone, Debug, Default)]
pub struct CustomWave {
    pub index: u32,
    /// `wavecode_N_*` scalars, with the `wavecode_N_` prefix removed.
    pub base: BTreeMap<String, String>,
    pub init: String,
    pub per_frame: String,
    pub per_point: String,
}

/// One of MilkDrop's four custom shapes.
#[derive(Clone, Debug, Default)]
pub struct CustomShape {
    pub index: u32,
    /// `shapecode_N_*` scalars, with the `shapecode_N_` prefix removed.
    pub base: BTreeMap<String, String>,
    pub init: String,
    pub per_frame: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// No `[preset00]` section header. Every `.milk` has one; a file without
    /// it is a different kind of file that happens to end in `.milk`.
    NoPresetSection,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NoPresetSection => write!(f, "no [preset00] section"),
        }
    }
}

impl std::error::Error for ParseError {}

/// How many custom waves and shapes MilkDrop has. Fixed by the format: the
/// keys are `wavecode_0_` through `wavecode_3_`.
const CUSTOM_SLOTS: u32 = 4;

impl Preset {
    /// Parses the text of a `.milk` file.
    ///
    /// Takes `&str` rather than a path: this crate does no IO on either shell
    /// (`player-render` is handed a surface and knows nothing about files or
    /// `fetch`), and a preset that arrives from a dropped `File` in a browser
    /// is the same bytes as one read from disk.
    pub fn parse(text: &str) -> Result<Preset, ParseError> {
        // A BOM would otherwise attach itself to the first key, and the first
        // key is the version -- so a BOM'd preset would parse as version 0 and
        // take the MilkDrop 1 path. Real files have them; Windows editors add
        // them.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);

        let mut preset = Preset::default();
        let mut in_preset = false;
        // Code lines are accumulated per block in file order (see the module
        // note on why file order rather than numeric).
        let mut blocks: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut wave_base: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
        let mut shape_base: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
        let mut in_sprite = false;

        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            let trimmed = line.trim();

            // Section headers. `[preset00]` opens the preset proper; the
            // MilkDrop 3 fork adds `[SPRITE1_BEGIN]`..`[SPRITE1_END]` blocks
            // that carry their own keys and their own code, which must not be
            // mistaken for the preset's.
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                let name = &trimmed[1..trimmed.len() - 1];
                if name.eq_ignore_ascii_case("preset00") {
                    in_preset = true;
                } else if name.to_ascii_uppercase().contains("SPRITE") {
                    let begins = name.to_ascii_uppercase().ends_with("_BEGIN");
                    in_sprite = begins;
                    if begins {
                        preset.note_unsupported(
                            "sprite",
                            "MilkDrop 3 sprites draw an external image file this app does not load",
                        );
                    }
                }
                continue;
            }
            if in_sprite {
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                continue;
            }

            if !in_preset {
                // The header, above `[preset00]`. Only the version keys mean
                // anything; `MD31` is the MilkDrop 3 fork's own marker.
                match key.to_ascii_uppercase().as_str() {
                    "MILKDROP_PRESET_VERSION" => preset.version = parse_u32(value),
                    "PSVERSION" => preset.ps_version = parse_u32(value),
                    "PSVERSION_WARP" => preset.ps_version_warp = parse_u32(value),
                    "PSVERSION_COMP" => preset.ps_version_comp = parse_u32(value),
                    _ => {}
                }
                continue;
            }

            if let Some((block, body)) = split_code_line(key, value) {
                blocks.entry(block).or_default().push(body);
                continue;
            }

            if let Some((slot, field)) = split_indexed(key, "wavecode_") {
                wave_base
                    .entry(slot)
                    .or_default()
                    .insert(field.to_string(), value.trim().to_string());
                continue;
            }
            if let Some((slot, field)) = split_indexed(key, "shapecode_") {
                shape_base
                    .entry(slot)
                    .or_default()
                    .insert(field.to_string(), value.trim().to_string());
                continue;
            }

            preset
                .base
                .insert(key.to_string(), value.trim().to_string());
        }

        if !in_preset {
            return Err(ParseError::NoPresetSection);
        }

        let take = |blocks: &mut BTreeMap<String, Vec<String>>, name: &str| -> String {
            blocks
                .remove(name)
                .map(|l| l.join("\n"))
                .unwrap_or_default()
        };

        preset.init = take(&mut blocks, "per_frame_init");
        preset.per_frame = take(&mut blocks, "per_frame");
        // `per_pixel` is MilkDrop's own spelling; `per_vertex` is what some
        // converters emit for the same block. Whichever is present wins.
        preset.per_pixel = {
            let a = take(&mut blocks, "per_pixel");
            if a.is_empty() {
                take(&mut blocks, "per_vertex")
            } else {
                a
            }
        };
        preset.warp = take(&mut blocks, "warp");
        preset.comp = take(&mut blocks, "comp");

        for slot in 0..CUSTOM_SLOTS {
            let base = wave_base.remove(&slot).unwrap_or_default();
            let init = take(&mut blocks, &format!("wave_{slot}_init"));
            let per_frame = take(&mut blocks, &format!("wave_{slot}_per_frame"));
            let per_point = take(&mut blocks, &format!("wave_{slot}_per_point"));
            if base.is_empty() && init.is_empty() && per_frame.is_empty() && per_point.is_empty() {
                continue;
            }
            preset.waves.push(CustomWave {
                index: slot,
                base,
                init,
                per_frame,
                per_point,
            });
        }

        for slot in 0..CUSTOM_SLOTS {
            let base = shape_base.remove(&slot).unwrap_or_default();
            let init = take(&mut blocks, &format!("shape_{slot}_init"));
            let per_frame = take(&mut blocks, &format!("shape_{slot}_per_frame"));
            if base.is_empty() && init.is_empty() && per_frame.is_empty() {
                continue;
            }
            preset.shapes.push(CustomShape {
                index: slot,
                base,
                init,
                per_frame,
            });
        }

        preset.note_external_textures();
        preset.unsupported.sort();
        preset.unsupported.dedup();
        Ok(preset)
    }

    fn note_unsupported(&mut self, what: &str, why: &'static str) {
        self.unsupported.push(Unsupported {
            what: what.to_string(),
            why,
        });
    }

    /// Custom `sampler_<name>` declarations name image files from a MilkDrop
    /// *texture pack*, which ships separately from the presets and is not in
    /// this repository. The two procedural noise textures MilkDrop generates
    /// for itself are excluded: those are ours to generate too.
    fn note_external_textures(&mut self) {
        let mut found: Vec<String> = Vec::new();
        for body in [self.warp.as_str(), self.comp.as_str()] {
            for name in sampler_names(body) {
                let base = name
                    .strip_prefix("fc_")
                    .or_else(|| name.strip_prefix("fw_"))
                    .or_else(|| name.strip_prefix("pc_"))
                    .or_else(|| name.strip_prefix("pw_"))
                    .unwrap_or(&name);
                if base == "main" || base.starts_with("noise") || base.starts_with("blur") {
                    continue;
                }
                found.push(base.to_string());
            }
        }
        found.sort();
        found.dedup();
        for name in found {
            self.note_unsupported(
                &format!("texture:{name}"),
                "names an image from a MilkDrop texture pack, which ships separately from presets",
            );
        }
    }

    /// True when the preset draws through HLSL pixel shaders rather than
    /// MilkDrop 1's fixed-function pipeline.
    pub fn uses_shaders(&self) -> bool {
        !self.warp.trim().is_empty() || !self.comp.trim().is_empty()
    }
}

/// Every `sampler_xyz` identifier appearing in a shader body.
fn sampler_names(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let needle = b"sampler_";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Not a match if it is the tail of a longer identifier.
            let preceded_by_ident =
                i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            let mut end = i + needle.len();
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if !preceded_by_ident && end > i + needle.len() {
                out.push(body[i + needle.len()..end].to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn parse_u32(value: &str) -> u32 {
    value.trim().parse().unwrap_or(0)
}

/// Splits `wavecode_2_enabled` into `(2, "enabled")`.
fn split_indexed<'a>(key: &'a str, prefix: &str) -> Option<(u32, &'a str)> {
    let rest = key.strip_prefix(prefix)?;
    let (index, field) = rest.split_once('_')?;
    Some((index.parse().ok()?, field))
}

/// Recognises a numbered code line and returns its block name and its body.
///
/// This is where the format's two numbering spellings are reconciled (see the
/// module note): `per_frame_1` and `wave_0_per_frame1` both belong to a block,
/// and the trailing index is dropped either way. The backtick a shader line
/// carries is stripped here too, exactly once -- a body that legitimately
/// starts with a backtick would be a syntax error in HLSL anyway, but stripping
/// greedily would eat a line of a preset that doubled it.
fn split_code_line(key: &str, value: &str) -> Option<(String, String)> {
    let stem = key.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() == key.len() {
        return None; // no trailing index: not a code line
    }
    let block = stem.strip_suffix('_').unwrap_or(stem);

    const BLOCKS: &[&str] = &[
        "per_frame_init",
        "per_frame",
        "per_pixel",
        "per_vertex",
        "warp",
        "comp",
    ];
    let known = BLOCKS.contains(&block)
        || (block.starts_with("wave_") || block.starts_with("shape_"))
            && (block.ends_with("_per_frame")
                || block.ends_with("_per_point")
                || block.ends_with("_init"));
    if !known {
        return None;
    }

    let body = value.strip_prefix('`').unwrap_or(value);
    Some((block.to_string(), body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_two_numbering_spellings() {
        let text = "MILKDROP_PRESET_VERSION=201\n[preset00]\n\
                    per_frame_1=a=1;\nper_frame_2=b=2;\n\
                    wave_0_per_point1=x=0;\nwave_0_per_point2=y=1;\n";
        let p = Preset::parse(text).unwrap();
        assert_eq!(p.per_frame, "a=1;\nb=2;");
        assert_eq!(p.waves.len(), 1);
        assert_eq!(p.waves[0].per_point, "x=0;\ny=1;");
    }

    #[test]
    fn joins_with_newlines_so_a_comment_cannot_eat_the_next_line() {
        let text = "[preset00]\nper_frame_1=a=1;//note\nper_frame_2=b=2;\n";
        let p = Preset::parse(text).unwrap();
        assert_eq!(p.per_frame, "a=1;//note\nb=2;");
    }

    #[test]
    fn strips_exactly_one_shader_backtick() {
        let text = "[preset00]\nwarp_1=`shader_body\nwarp_2=``nested\n";
        let p = Preset::parse(text).unwrap();
        assert_eq!(p.warp, "shader_body\n`nested");
    }

    #[test]
    fn keeps_ten_after_nine_by_reading_in_file_order() {
        let mut text = String::from("[preset00]\n");
        for i in 1..=12 {
            text.push_str(&format!("per_frame_{i}=x={i};\n"));
        }
        let p = Preset::parse(&text).unwrap();
        let lines: Vec<&str> = p.per_frame.lines().collect();
        assert_eq!(lines[8], "x=9;");
        assert_eq!(lines[9], "x=10;");
    }

    #[test]
    fn a_bom_does_not_hide_the_version() {
        let p =
            Preset::parse("\u{feff}MILKDROP_PRESET_VERSION=201\n[preset00]\nzoom=1;\n").unwrap();
        assert_eq!(p.version, 201);
    }

    #[test]
    fn a_file_with_no_preset_section_is_refused() {
        assert_eq!(
            Preset::parse("zoom=1\n").unwrap_err(),
            ParseError::NoPresetSection
        );
    }

    #[test]
    fn sprite_blocks_do_not_leak_into_the_preset() {
        let text = "[preset00]\nper_frame_1=a=1;\n[SPRITE1_BEGIN]\nSpriteName=x.png\n\
                    code_1=sx=2;\n[SPRITE1_END]\n";
        let p = Preset::parse(text).unwrap();
        assert_eq!(p.per_frame, "a=1;");
        assert!(!p.base.contains_key("SpriteName"));
        assert!(p.unsupported.iter().any(|u| u.what == "sprite"));
    }

    #[test]
    fn external_textures_are_named_and_built_ins_are_not() {
        let text = "[preset00]\nwarp_1=`sampler sampler_fw_clouds;\n\
                    warp_2=`ret = tex2D(sampler_main, uv).xyz + tex2D(sampler_noise_mq, uv).xyz;\n";
        let p = Preset::parse(text).unwrap();
        let names: Vec<&str> = p.unsupported.iter().map(|u| u.what.as_str()).collect();
        assert_eq!(names, vec!["texture:clouds"]);
    }

    #[test]
    fn scalars_survive_verbatim() {
        let p = Preset::parse("[preset00]\nfDecay=0.500\nnWaveMode=6\n").unwrap();
        assert_eq!(p.base.get("fDecay").map(String::as_str), Some("0.500"));
        assert_eq!(p.base.get("nWaveMode").map(String::as_str), Some("6"));
    }
}
