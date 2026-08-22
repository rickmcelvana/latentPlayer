//! What a real preset collection actually contains (Phase 3 M7b).
//!
//! ```text
//! cargo run --release -p player-render --example milk_survey -- <folder>
//! ```
//!
//! `milk_smoke` asserts and must not regress. This one **reports and asserts
//! nothing**, because it is the measurement M7b's grammar is designed from
//! rather than a check on a grammar already written.
//!
//! The reason it exists: the M7 spike measured 244 shader lines across five
//! presets using 15 distinct constructs, and that is a sample of five files
//! from one source and one era. A real collection -- the MilkDrop3 set plus
//! packs gathered over twenty years, still growing -- spans every version of
//! the format. Designing a translator against the sample means meeting the
//! sixteenth construct after the emitter's shape is settled, which is the
//! expensive time to meet it.
//!
//! It recurses, because collections arrive as folders of folders.
//!
//! What it reports, and why each figure is worth having:
//!
//! - **The era distribution** (`MILKDROP_PRESET_VERSION`, `PSVERSION`). Says
//!   where the value is. A large `PSVERSION 0` share means M7a is already
//!   worth more than five presets suggested; a cluster in one shader model is
//!   the model to be exact about first.
//! - **Every call, by frequency.** The standard library of decision 55 is
//!   whatever this list says it is, and the tail is where the surprises live.
//! - **Every free identifier.** A name that is used and never declared is a
//!   uniform this build has to provide.
//! - **Every type, operator, swizzle and statement keyword.** The grammar's
//!   actual surface, counted rather than assumed.
//! - **Unknown characters and preprocessor lines.** Two things a translator
//!   plan tends to forget until a file has one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use player_render::milk::hlsl::{lex, Tok, Ty};
use player_render::milk::Preset;

/// Statement and declaration keywords, which are `Tok::Ident` to the lexer.
const KEYWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "break", "continue", "return", "discard", "struct",
    "const", "static", "inline", "uniform", "in", "out", "inout", "true", "false", "switch",
    "case", "default",
];

#[derive(Default)]
struct Tally(BTreeMap<String, usize>);

impl Tally {
    fn add(&mut self, key: impl Into<String>) {
        *self.0.entry(key.into()).or_default() += 1;
    }

    /// Sorted by frequency, commonest first.
    fn ranked(&self) -> Vec<(&str, usize)> {
        let mut v: Vec<(&str, usize)> = self.0.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v
    }

    fn report(&self, title: &str, limit: usize) {
        let ranked = self.ranked();
        println!("\n{title} -- {} distinct", ranked.len());
        for (name, n) in ranked.iter().take(limit) {
            println!("  {n:>7}  {name}");
        }
        if ranked.len() > limit {
            let rest: usize = ranked[limit..].iter().map(|(_, n)| n).sum();
            println!("  {rest:>7}  ... and {} more", ranked.len() - limit);
        }
    }
}

#[derive(Default)]
struct Survey {
    files: usize,
    unreadable: Vec<(PathBuf, String)>,
    parse_failed: Vec<(PathBuf, String)>,
    with_warp: usize,
    with_comp: usize,
    with_neither: usize,
    shader_lines: usize,
    tokens: usize,
    preset_version: Tally,
    ps_version: Tally,
    calls: Tally,
    constructors: Tally,
    free_idents: Tally,
    declared_types: Tally,
    operators: Tally,
    swizzles: Tally,
    keywords: Tally,
    directives: Tally,
    unknown_chars: Tally,
    defines_functions: usize,
    unsupported: Tally,
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("milk"))
        {
            found.push(path);
        }
    }
    found.sort();
    out.extend(found);
}

impl Survey {
    fn shader(&mut self, src: &str) {
        if src.trim().is_empty() {
            return;
        }
        self.shader_lines += src.lines().count();
        let tokens = lex(src);
        self.tokens += tokens.len();

        for (i, t) in tokens.iter().enumerate() {
            let prev = i.checked_sub(1).map(|j| &tokens[j]);
            let next = tokens.get(i + 1);
            let is_call = next.is_some_and(|n| n.kind == Tok::Op && n.text == "(");
            let after_dot = prev.is_some_and(|p| p.kind == Tok::Op && p.text == ".");

            match t.kind {
                Tok::Op => self.operators.add(t.text),
                Tok::Directive => {
                    // The first word only: `#define X 1` and `#define Y 2` are
                    // one construct, not two.
                    self.directives
                        .add(t.text.split_whitespace().next().unwrap_or("#"))
                }
                Tok::Unknown => self.unknown_chars.add(t.text),
                Tok::Number => {}
                Tok::Ident => {
                    if after_dot {
                        self.swizzles.add(t.text);
                    } else if is_call {
                        if Ty::from_name(t.text).is_some() {
                            self.constructors.add(t.text);
                        } else {
                            self.calls.add(t.text);
                            // `float sigmoid(float t, float c){` -- a preset
                            // defining a function of its own, which the
                            // spike's construct list did not include and the
                            // corpus sample already contains.
                            if prev.is_some_and(|p| Ty::from_name(p.text).is_some()) {
                                self.defines_functions += 1;
                            }
                        }
                    } else if Ty::from_name(t.text).is_some() {
                        self.declared_types.add(t.text);
                    } else if KEYWORDS.contains(&t.text) {
                        self.keywords.add(t.text);
                    } else if prev.is_some_and(|p| Ty::from_name(p.text).is_some()) {
                        // A name being declared, not a name being used.
                    } else {
                        // Used and not declared here: a uniform, or a local
                        // declared earlier. Frequency sorts the two apart --
                        // `uv` and `texsize` appear in every file, a local
                        // called `my_uv` in one.
                        self.free_idents.add(t.text);
                    }
                }
            }
        }
    }

    fn preset(&mut self, path: &Path) {
        self.files += 1;
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            // A collection downloaded from anywhere has at least one file in
            // an eccentric encoding. Read it lossily rather than skipping it.
            Err(_) => match fs::read(path) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) => {
                    self.unreadable.push((path.to_owned(), e.to_string()));
                    return;
                }
            },
        };

        let preset = match Preset::parse(&text) {
            Ok(p) => p,
            Err(e) => {
                self.parse_failed.push((path.to_owned(), e.to_string()));
                return;
            }
        };

        self.preset_version.add(preset.version.to_string());
        self.ps_version.add(
            match (
                preset.ps_version,
                preset.ps_version_warp,
                preset.ps_version_comp,
            ) {
                (v, w, c) if w == v && c == v => format!("{v}"),
                (v, w, c) => format!("{v} (warp {w}, comp {c})"),
            },
        );
        for u in &preset.unsupported {
            self.unsupported.add(&u.what);
        }

        let has_warp = !preset.warp.trim().is_empty();
        let has_comp = !preset.comp.trim().is_empty();
        self.with_warp += usize::from(has_warp);
        self.with_comp += usize::from(has_comp);
        self.with_neither += usize::from(!has_warp && !has_comp);

        self.shader(&preset.warp);
        self.shader(&preset.comp);
    }

    fn report(&self) {
        println!("\n=== {} presets ===", self.files);
        if !self.unreadable.is_empty() {
            println!("  {} unreadable", self.unreadable.len());
        }
        println!(
            "  {} parsed, {} failed to parse",
            self.files - self.parse_failed.len() - self.unreadable.len(),
            self.parse_failed.len()
        );
        println!(
            "  {} have a warp shader, {} a comp shader, {} neither (M7a draws these as themselves)",
            self.with_warp, self.with_comp, self.with_neither
        );
        println!(
            "  {} shader lines, {} tokens, {} functions defined by presets themselves",
            self.shader_lines, self.tokens, self.defines_functions
        );

        for (path, why) in self.parse_failed.iter().take(10) {
            println!("  parse failed: {} -- {why}", path.display());
        }

        self.preset_version.report("MILKDROP_PRESET_VERSION", 12);
        self.ps_version.report("PSVERSION", 12);
        self.calls.report("Function calls", 60);
        self.constructors.report("Type constructors", 20);
        self.free_idents
            .report("Identifiers used but not declared", 60);
        self.declared_types.report("Types named", 20);
        self.operators.report("Operators", 40);
        self.swizzles.report("Swizzles and members", 30);
        self.keywords.report("Statement keywords", 20);
        self.directives.report("Preprocessor directives", 10);
        self.unknown_chars
            .report("Characters the lexer has no rule for", 20);
        self.unsupported
            .report("Unsupported features named by the parser", 20);
    }
}

fn main() {
    let folder = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!(
            "usage: cargo run --release -p player-render --example milk_survey -- <folder>\n\
             \n\
             No preset ships with this app (Phase 3 decision 57) and none is committed,\n\
             so there is no default corpus -- point it at a collection of your own."
        );
        std::process::exit(2);
    });

    let mut paths = Vec::new();
    collect(Path::new(&folder), &mut paths);
    if paths.is_empty() {
        eprintln!("no .milk files under {folder}");
        std::process::exit(2);
    }

    let mut survey = Survey::default();
    for path in &paths {
        survey.preset(path);
    }
    survey.report();
}
