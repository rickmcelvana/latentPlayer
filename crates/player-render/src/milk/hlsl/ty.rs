//! HLSL's type lattice, which is the whole reason decision 54 says "typed
//! mini-compiler" rather than "substitution table".
//!
//! Two HLSL habits are illegal in WGSL, and both need the type of the
//! *surrounding expression* to resolve. Neither is exotic; both are in the
//! five-preset sample, so they are certainly in a collection of thousands:
//!
//! ```text
//! float2 water = tex2D(sampler_noise_mq, uv);   // float4 -> float2, dropping .zw
//! float  d     = texsize.zw;                    // float2 -> float,  dropping .y
//! float3 ret   = 1 - ret*(1-ret)*4;             // scalar promoted against a float3
//! float2 v     = 0.008;                         // scalar splatted into both lanes
//! ```
//!
//! HLSL is happy to silently drop components on the right of an assignment
//! (warning X3206, which nobody reads) and to splat a scalar into a vector.
//! WGSL does neither: `vec2<f32>` is not assignable from `vec4<f32>` and
//! `1.0 - v` is a type error. So every expression's type has to be *known*
//! before its WGSL can be written, and this module is where the rules for
//! knowing it live.
//!
//! # The rules, as HLSL actually has them
//!
//! - **A scalar against anything promotes**: `float * float3` is a `float3`.
//! - **Two vectors of different width truncate to the narrower**, silently.
//!   `float4 + float2` is a `float2`. This is the surprising one and it is
//!   real; it is why the sample's `float d = texsize.zw` compiles at all.
//! - **Assignment truncates or splats to the target**, by the same two rules.
//! - **Base types convert freely** among `float`/`int`/`bool`, with `float`
//!   the widest. WGSL requires each of those conversions written out.
//!
//! # What is deliberately not modelled
//!
//! `min16float`, `double`, arrays, `struct` and texture objects. None appears
//! in a MilkDrop preset -- the environment a preset is compiled in has no way
//! to give it one -- and a type this module cannot name is reported by the
//! parser as an unsupported construct with its own line number, which is the
//! behaviour a collection of strangers' files needs anyway.

use std::fmt;

/// The scalar kind underneath a type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Base {
    /// Widest, and what nearly every preset expression is.
    Float,
    Int,
    Bool,
}

impl Base {
    /// The base type an expression mixing these two produces.
    ///
    /// `Float` wins over `Int` wins over `Bool`, which is HLSL's promotion
    /// order and also the order that makes `bass > 1` usable as a multiplier
    /// -- something presets do.
    pub fn widest(self, other: Base) -> Base {
        self.min(other)
    }

    /// The WGSL spelling.
    pub fn wgsl(self) -> &'static str {
        match self {
            Base::Float => "f32",
            Base::Int => "i32",
            Base::Bool => "bool",
        }
    }
}

/// A type a preset's shader can name or an expression can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    /// The result of a function that returns nothing.
    Void,
    /// A scalar (`n == 1`) or a vector. `float` and `float1` are both `n == 1`
    /// and are the same type; the corpus writes both, sometimes in one file.
    Vec { base: Base, n: u8 },
    /// A matrix. Rare in presets and cheap to carry, so carried rather than
    /// reported as unsupported.
    Mat { base: Base, rows: u8, cols: u8 },
    /// A texture sampler. Only ever a parameter to `tex2D` and friends, never
    /// an operand, which is why it needs no arithmetic rules.
    Sampler,
}

impl Ty {
    pub const FLOAT: Ty = Ty::Vec {
        base: Base::Float,
        n: 1,
    };
    pub const FLOAT2: Ty = Ty::Vec {
        base: Base::Float,
        n: 2,
    };
    pub const FLOAT3: Ty = Ty::Vec {
        base: Base::Float,
        n: 3,
    };
    pub const FLOAT4: Ty = Ty::Vec {
        base: Base::Float,
        n: 4,
    };
    pub const BOOL: Ty = Ty::Vec {
        base: Base::Bool,
        n: 1,
    };

    /// Parse a type name as HLSL spells it: `float`, `float1`..`float4`,
    /// `float2x2`..`float4x4`, the `half`/`int`/`bool` families, `void` and
    /// the sampler names.
    ///
    /// `half` is `float`. Presets use it as a hint that a value is small, and
    /// no GPU this app targets has a distinct half pipeline through wgpu, so
    /// treating it as anything else would be inventing a difference.
    pub fn from_name(name: &str) -> Option<Ty> {
        if name == "void" {
            return Some(Ty::Void);
        }
        // `sampler`, `sampler2D`, `SamplerState`, and MilkDrop's own
        // `sampler_main` style names are declarations of samplers wherever
        // they appear as a type.
        if name == "sampler" || name == "sampler1D" || name == "sampler2D" || name == "sampler3D" {
            return Some(Ty::Sampler);
        }

        // Ordered longest-first where one name prefixes another, so that a
        // future `float16_t` cannot be read as `float` followed by `16_t`.
        const PREFIXES: &[(&str, Base)] = &[
            ("float", Base::Float),
            ("half", Base::Float),
            ("uint", Base::Int),
            ("int", Base::Int),
            ("bool", Base::Bool),
        ];
        let (base, rest) = PREFIXES
            .iter()
            .find_map(|(prefix, base)| name.strip_prefix(prefix).map(|rest| (*base, rest)))?;

        if rest.is_empty() {
            return Some(Ty::Vec { base, n: 1 });
        }
        let b = rest.as_bytes();
        let dim = |c: u8| (1..=4).contains(&(c.wrapping_sub(b'0'))).then(|| c - b'0');
        match b.len() {
            1 => dim(b[0]).map(|n| Ty::Vec { base, n }),
            3 if b[1] == b'x' => match (dim(b[0]), dim(b[2])) {
                (Some(rows), Some(cols)) => Some(Ty::Mat { base, rows, cols }),
                _ => None,
            },
            _ => None,
        }
    }

    /// The base type, where there is one.
    pub fn base(self) -> Option<Base> {
        match self {
            Ty::Vec { base, .. } | Ty::Mat { base, .. } => Some(base),
            Ty::Void | Ty::Sampler => None,
        }
    }

    /// How many scalars this type holds.
    pub fn components(self) -> u8 {
        match self {
            Ty::Vec { n, .. } => n,
            Ty::Mat { rows, cols, .. } => rows * cols,
            Ty::Void | Ty::Sampler => 0,
        }
    }

    /// True for `float`, `int`, `bool` -- the types that promote against a
    /// vector rather than having to match it.
    pub fn is_scalar(self) -> bool {
        matches!(self, Ty::Vec { n: 1, .. })
    }

    /// The same shape with a different base, for the conversions WGSL needs
    /// written out.
    pub fn with_base(self, base: Base) -> Ty {
        match self {
            Ty::Vec { n, .. } => Ty::Vec { base, n },
            Ty::Mat { rows, cols, .. } => Ty::Mat { base, rows, cols },
            other => other,
        }
    }

    /// The WGSL spelling, which is also the constructor name.
    pub fn wgsl(self) -> String {
        match self {
            Ty::Void => "void".into(),
            Ty::Sampler => "sampler".into(),
            Ty::Vec { base, n: 1 } => base.wgsl().into(),
            Ty::Vec { base, n } => format!("vec{n}<{}>", base.wgsl()),
            Ty::Mat { base, rows, cols } => {
                // HLSL writes rows x columns and WGSL writes columns x rows.
                // A `float3x4` is three rows of four, which WGSL spells
                // `mat4x3` -- four columns of three. Getting this backwards
                // compiles fine on square matrices and silently transposes
                // everything else.
                format!("mat{cols}x{rows}<{}>", base.wgsl())
            }
        }
    }

    /// The result type of `lhs op rhs` for an arithmetic operator.
    ///
    /// `None` where HLSL itself has no answer -- a sampler operand, `void`,
    /// or two matrices of different shape.
    pub fn binary(lhs: Ty, rhs: Ty) -> Option<Ty> {
        let base = lhs.base()?.widest(rhs.base()?);
        match (lhs, rhs) {
            // A scalar takes the other side's shape.
            (Ty::Vec { n: 1, .. }, other) | (other, Ty::Vec { n: 1, .. }) => {
                Some(other.with_base(base))
            }
            // Two vectors truncate to the narrower. Silent in HLSL, and the
            // reason `float d = texsize.zw` is legal rather than a typo.
            (Ty::Vec { n: a, .. }, Ty::Vec { n: b, .. }) => Some(Ty::Vec { base, n: a.min(b) }),
            (
                Ty::Mat {
                    rows: ar, cols: ac, ..
                },
                Ty::Mat {
                    rows: br, cols: bc, ..
                },
            ) if ar == br && ac == bc => Some(Ty::Mat {
                base,
                rows: ar,
                cols: ac,
            }),
            _ => None,
        }
    }

    /// How a value of type `self` reaches type `to`, or `None` if it cannot.
    ///
    /// This is the function the emitter asks before writing any assignment,
    /// argument or return, because every one of those is a place HLSL will
    /// have silently reshaped a value and WGSL will refuse to.
    pub fn fit_to(self, to: Ty) -> Option<Fit> {
        if self == to {
            return Some(Fit {
                shape: Shape::Same,
                base: None,
            });
        }
        let (from_base, to_base) = (self.base()?, to.base()?);
        let base = (from_base != to_base).then_some(to_base);
        let shape = match (self, to) {
            _ if self.components() == to.components() && self.is_scalar() == to.is_scalar() => {
                Shape::Same
            }
            // A scalar becomes every lane.
            (Ty::Vec { n: 1, .. }, _) => Shape::Splat,
            // A wider vector keeps its leading components.
            (Ty::Vec { n: a, .. }, Ty::Vec { n: b, .. }) if a > b => Shape::Truncate(b),
            // Anything else -- widening a float2 into a float4, a matrix into
            // a vector -- HLSL does not do either, so neither does this.
            _ => return None,
        };
        Some(Fit { shape, base })
    }
}

impl fmt::Display for Ty {
    /// HLSL's spelling, for reports a preset's author would recognise.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = |b: Base| match b {
            Base::Float => "float",
            Base::Int => "int",
            Base::Bool => "bool",
        };
        match *self {
            Ty::Void => f.write_str("void"),
            Ty::Sampler => f.write_str("sampler"),
            Ty::Vec { base: b, n: 1 } => f.write_str(base(b)),
            Ty::Vec { base: b, n } => write!(f, "{}{n}", base(b)),
            Ty::Mat {
                base: b,
                rows,
                cols,
            } => write!(f, "{}{rows}x{cols}", base(b)),
        }
    }
}

/// What has to happen to a value for it to reach a type. See [`Ty::fit_to`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    pub shape: Shape,
    /// `Some` when the base type also has to change, which WGSL needs written.
    pub base: Option<Base>,
}

/// The shape half of a [`Fit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Already the right shape.
    Same,
    /// A scalar filling every lane of the target.
    Splat,
    /// The leading `n` components kept, the rest dropped -- HLSL's implicit
    /// truncation, which is the construct decision 54 is named after.
    Truncate(u8),
}

/// The type of `value.name`, where `name` is a swizzle like `xy`, `zw` or
/// `rgba`.
///
/// Returns `None` for the three ways a swizzle can be wrong, all of which a
/// real collection contains: a component the source does not have (`.z` of a
/// `float2`), a mix of the two naming sets (`.xg`), and a name longer than
/// four. Reported by the parser with a line number rather than silently
/// producing something.
pub fn swizzle(ty: Ty, name: &str) -> Option<Ty> {
    let Ty::Vec { base, n } = ty else {
        return None;
    };
    if name.is_empty() || name.len() > 4 {
        return None;
    }
    const XYZW: &str = "xyzw";
    const RGBA: &str = "rgba";
    let set = if name.chars().all(|c| XYZW.contains(c)) {
        XYZW
    } else if name.chars().all(|c| RGBA.contains(c)) {
        RGBA
    } else {
        // `.xg` -- each character is a valid component name, in different
        // sets. HLSL rejects it and so does this.
        return None;
    };
    for c in name.chars() {
        let index = set.find(c)? as u8;
        if index >= n {
            return None;
        }
    }
    Some(Ty::Vec {
        base,
        n: name.len() as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_name_a_preset_writes_is_recognised() {
        assert_eq!(Ty::from_name("float"), Some(Ty::FLOAT));
        // The corpus writes `float1 d = 0.0039;` -- the same type, spelled the
        // long way, in the same file as plain `float`.
        assert_eq!(Ty::from_name("float1"), Some(Ty::FLOAT));
        assert_eq!(Ty::from_name("float3"), Some(Ty::FLOAT3));
        assert_eq!(Ty::from_name("half3"), Some(Ty::FLOAT3), "half is float");
        assert_eq!(Ty::from_name("bool"), Some(Ty::BOOL));
        assert_eq!(Ty::from_name("void"), Some(Ty::Void));
        assert_eq!(Ty::from_name("sampler2D"), Some(Ty::Sampler));
        assert_eq!(
            Ty::from_name("float3x3"),
            Some(Ty::Mat {
                base: Base::Float,
                rows: 3,
                cols: 3
            })
        );
        assert_eq!(Ty::from_name("float5"), None);
        assert_eq!(Ty::from_name("uv"), None);
    }

    #[test]
    fn a_matrix_is_transposed_between_the_two_spellings() {
        // HLSL's rows x columns against WGSL's columns x rows. Square
        // matrices hide this and the corpus's matrices are square, so the
        // test is the only thing standing between here and a silent
        // transpose the day one is not.
        let m = Ty::from_name("float3x4").unwrap();
        assert_eq!(m.to_string(), "float3x4");
        assert_eq!(m.wgsl(), "mat4x3<f32>");
        assert_eq!(Ty::from_name("float4x4").unwrap().wgsl(), "mat4x4<f32>");
    }

    #[test]
    fn wgsl_spelling_matches_what_the_emitter_will_write() {
        assert_eq!(Ty::FLOAT.wgsl(), "f32");
        assert_eq!(Ty::FLOAT2.wgsl(), "vec2<f32>");
        assert_eq!(Ty::BOOL.wgsl(), "bool");
    }

    #[test]
    fn a_scalar_takes_the_other_sides_shape() {
        // `1 - ret*(1-ret)*4` where ret is a float3, from the corpus.
        assert_eq!(Ty::binary(Ty::FLOAT, Ty::FLOAT3), Some(Ty::FLOAT3));
        assert_eq!(Ty::binary(Ty::FLOAT3, Ty::FLOAT), Some(Ty::FLOAT3));
    }

    #[test]
    fn two_vectors_truncate_to_the_narrower_which_is_hlsls_surprising_rule() {
        assert_eq!(Ty::binary(Ty::FLOAT4, Ty::FLOAT2), Some(Ty::FLOAT2));
        assert_eq!(Ty::binary(Ty::FLOAT2, Ty::FLOAT4), Some(Ty::FLOAT2));
        // `float2 d = texsize.zw*12;` -- a float2 against a scalar stays a
        // float2, and it is the *assignment* that would truncate, not this.
        assert_eq!(Ty::binary(Ty::FLOAT2, Ty::FLOAT), Some(Ty::FLOAT2));
    }

    #[test]
    fn a_base_type_widens_towards_float() {
        let int3 = Ty::Vec {
            base: Base::Int,
            n: 3,
        };
        assert_eq!(Ty::binary(Ty::FLOAT, int3), Some(Ty::FLOAT3));
        assert_eq!(Base::Bool.widest(Base::Int), Base::Int);
        assert_eq!(Base::Int.widest(Base::Float), Base::Float);
    }

    #[test]
    fn a_sampler_has_no_arithmetic() {
        assert_eq!(Ty::binary(Ty::Sampler, Ty::FLOAT), None);
        assert_eq!(Ty::binary(Ty::Void, Ty::FLOAT), None);
    }

    #[test]
    fn the_two_constructs_decision_54_names_are_the_two_fits() {
        // `float2 water = tex2D(...)` -- a float4 keeping its leading two.
        assert_eq!(
            Ty::FLOAT4.fit_to(Ty::FLOAT2),
            Some(Fit {
                shape: Shape::Truncate(2),
                base: None
            })
        );
        // `float2 v = 0.008;` -- a scalar in both lanes.
        assert_eq!(
            Ty::FLOAT.fit_to(Ty::FLOAT2),
            Some(Fit {
                shape: Shape::Splat,
                base: None
            })
        );
        // `float d = texsize.zw;` -- all the way down to a scalar.
        assert_eq!(
            Ty::FLOAT2.fit_to(Ty::FLOAT),
            Some(Fit {
                shape: Shape::Truncate(1),
                base: None
            })
        );
    }

    #[test]
    fn a_narrower_value_never_widens_itself() {
        // HLSL will not turn a float2 into a float4 implicitly, and neither
        // will this -- a preset relying on it would be relying on something
        // that never compiled in MilkDrop either.
        assert_eq!(Ty::FLOAT2.fit_to(Ty::FLOAT4), None);
        assert_eq!(Ty::FLOAT3.fit_to(Ty::Sampler), None);
    }

    #[test]
    fn a_base_change_is_reported_alongside_the_shape() {
        let int2 = Ty::Vec {
            base: Base::Int,
            n: 2,
        };
        assert_eq!(
            Ty::FLOAT.fit_to(int2),
            Some(Fit {
                shape: Shape::Splat,
                base: Some(Base::Int)
            })
        );
        assert_eq!(
            Ty::FLOAT.fit_to(Ty::BOOL),
            Some(Fit {
                shape: Shape::Same,
                base: Some(Base::Bool)
            })
        );
    }

    #[test]
    fn a_swizzle_types_by_its_length_and_the_sources_width() {
        assert_eq!(swizzle(Ty::FLOAT4, "zw"), Some(Ty::FLOAT2));
        assert_eq!(swizzle(Ty::FLOAT4, "xyz"), Some(Ty::FLOAT3));
        assert_eq!(swizzle(Ty::FLOAT3, "x"), Some(Ty::FLOAT));
        assert_eq!(swizzle(Ty::FLOAT4, "rgb"), Some(Ty::FLOAT3));
        // Repeats are legal and widen: `.xxx` of a float is a float3, which
        // presets use to make a grey.
        assert_eq!(swizzle(Ty::FLOAT, "xxx"), Some(Ty::FLOAT3));
    }

    #[test]
    fn the_three_ways_a_swizzle_can_be_wrong_are_all_refused() {
        assert_eq!(swizzle(Ty::FLOAT2, "z"), None, "a lane it does not have");
        assert_eq!(swizzle(Ty::FLOAT4, "xg"), None, "two naming sets at once");
        assert_eq!(swizzle(Ty::FLOAT4, "xyzwx"), None, "longer than four");
        assert_eq!(swizzle(Ty::Sampler, "x"), None);
    }
}
