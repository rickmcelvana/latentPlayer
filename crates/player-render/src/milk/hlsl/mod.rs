//! Translating a preset's HLSL pixel shaders into WGSL (Phase 3 M7b).
//!
//! M7a draws a preset through MilkDrop's fixed-function pipeline, which is
//! what a `PSVERSION 0` preset asks for and an approximation of what every
//! later one does. This is the part that makes a preset draw *as itself*.
//!
//! Decision 54 settles the shape: **a typed mini-compiler, never text
//! substitution**. Two HLSL constructs in the five-preset sample force it, and
//! both need the type of the surrounding expression to resolve -- see
//! [`ty`]'s module note, which is where the rules are written down. Text
//! substitution can move `float2` to `vec2<f32>`; it cannot decide that
//! `tex2D(...)` needs `.xy` after it in one line and not the next.
//!
//! # The pieces, in the order they run
//!
//! - [`lex`] -- tokens. Never fails; an unknown character is a token.
//! - [`ty`] -- HLSL's type lattice, its promotion and truncation rules, and
//!   swizzle typing.
//! - [`env`] -- decision 55's environment: MilkDrop's `include.fx` written
//!   down, which is what `uv`, `time`, `q17` and `GetBlur2` actually are.
//!
//! Still to come in M7b: the parser producing a typed tree, the WGSL emitter,
//! decision 55's prelude (MilkDrop's `GetPixel`/`GetBlur1..3`/`lum`, the
//! `texsize`/`aspect` uniforms, `q1..q32`), and the blur chain the helpers
//! need -- which is a real render pass and lands only when a preset asks for
//! one.
//!
//! # Why the corpus is surveyed before the grammar is written
//!
//! The spike measured 244 shader lines across five presets using 15 distinct
//! constructs. That is a sample of five files from one source and one era, and
//! the user's own collection -- the MilkDrop3 set plus collections gathered
//! over years, still growing -- spans every version of the format. Designing a
//! grammar against the sample means finding the sixteenth construct after the
//! emitter's shape is settled.
//!
//! So `examples/milk_survey.rs` runs [`lex`] over a whole collection and
//! reports what is actually in it: every call, every type, every operator,
//! every swizzle, every unknown character, and the `PSVERSION` distribution
//! that says which era the files come from. The tokenizer was the translator's
//! own first component either way; it just runs before the parser rather than
//! after it.
//!
//! # What the survey found on its first run, which the spike's list had not
//!
//! Three constructs, all in the same five presets the spike measured, none of
//! them in its inventory of fifteen -- which is the argument for surveying a
//! real collection before fixing a grammar, made by a sample of five.
//!
//! **A shader body is not a list of statements.** It is
//!
//! ```text
//! sampler sampler_fw_clouds;
//! shader_body
//! {
//!     ret = tex2D(sampler_main, uv).xyz;
//! }
//! ```
//!
//! `shader_body` is a macro MilkDrop's own prepended header defines as the
//! entry point's signature, and `ret` is an output variable that header
//! declares rather than the preset. Every one of the ten shader blocks in the
//! sample is shaped this way. So the grammar's top level is *global
//! declarations then a wrapper block*, and the free-standing `sampler`
//! declaration before it is how a preset asks for a texture by name -- which
//! is decision 56's texture packs arriving through the shader rather than
//! through a key.
//!
//! **A preset defines functions of its own.** `float sigmoid(float t, float
//! c){ return 1 / (1 + exp(t*c)); }` and a `vortex` beside it, in two of five
//! files. The translator needs function definitions, parameters and `return`,
//! not just a statement list.
//!
//! **A bool is a number.** `ret -= -(lum(ret)<sin(time*90))*ret*blur + ...`
//! multiplies a comparison by a `float3`. HLSL promotes `bool` to `float`
//! silently; WGSL has no implicit conversion at all and no arithmetic on
//! `bool`. [`ty`] already carries the rule ([`Base::widest`] puts `float`
//! above `int` above `bool`), and it is here because a preset in the sample
//! depends on it, not because HLSL's specification mentions it.
//!
//! # An unknown construct is a normal Tuesday
//!
//! Against thousands of strangers' files written across two decades, "this
//! shader uses something this build does not know" is the ordinary case rather
//! than the exceptional one. It lands where decision 56 already put its
//! siblings -- named per preset, shown in the library UI -- and the preset
//! falls back to M7a's fixed-function path rather than drawing nothing. The
//! figure that matters is the count that draws, and that it never falls.

pub mod env;
pub mod lex;
pub mod ty;

pub use env::{Alias, Pass};
pub use lex::{lex, Tok, Token};
pub use ty::{swizzle, Base, Fit, Shape, Ty};
