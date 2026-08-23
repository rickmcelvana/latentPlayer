//! The shape of a preset's shader, once it has been read.
//!
//! Deliberately untyped. [`ty`](super::ty) knows HLSL's rules and the emitter
//! applies them, but the *tree* records only what was written -- so "this file
//! uses a construct we cannot read" and "this file uses a name we do not have"
//! stay separate answers, which is what a report over eleven thousand
//! strangers' files needs them to be.
//!
//! Shaped by what the corpus survey measured rather than by what HLSL permits.
//! Branches are in because a quarter of presets branch; `struct` is not,
//! because none does. Everything the grammar refuses refuses *by name and line
//! number* rather than by falling over.

use super::ty::Ty;

/// A whole shader body: its globals, its functions, and the one block
/// MilkDrop wraps in an entry point.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shader {
    /// `sampler sampler_fw_clouds;` and `static const float k = 3;` -- the
    /// declarations a preset puts *before* its `shader_body`.
    pub globals: Vec<Var>,
    /// Functions the preset declares for itself. 5.7% of the corpus does.
    pub functions: Vec<Function>,
    /// The `shader_body { ... }` block. `None` is a real state and not an
    /// error here -- MilkDrop refuses such a file, and saying which file and
    /// why is the caller's job.
    pub body: Option<Block>,
}

/// Storage qualifiers, kept because 6% of the corpus writes them and because
/// `static const` at file scope means something different from a local.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Qualifiers {
    pub is_static: bool,
    pub is_const: bool,
    pub is_uniform: bool,
}

/// One declared name: a global, a local, or a `for`'s loop counter.
#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub qualifiers: Qualifiers,
    pub ty: Ty,
    pub name: String,
    /// `float weights[4];` -- the declared length, where there is one.
    pub array: Option<u32>,
    pub init: Option<Expr>,
    /// `sampler foo = sampler_state { AddressU = WRAP; MagFilter = LINEAR; };`
    ///
    /// Kept as written rather than skipped: these are the address and filter
    /// modes a wgpu sampler descriptor needs, so a preset that asks for a
    /// wrapping texture can get one. Keys are upper-cased by convention but
    /// not by rule, so the reader lower-cases neither.
    pub sampler_state: Vec<(String, String)>,
    pub line: u32,
}

/// How a parameter is passed. `out`/`inout` are rare and cheap to carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Passing {
    pub out: bool,
    pub inout: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub ty: Ty,
    pub name: String,
    pub passing: Passing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub ret: Ty,
    pub name: String,
    pub params: Vec<Param>,
    pub body: Block,
    pub line: u32,
}

/// A braced sequence of statements.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block(pub Vec<Stmt>);

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    /// `float a = 1, b;` -- one statement, however many names.
    Decl(Vec<Var>),
    /// An expression evaluated for its effect, which in a shader means an
    /// assignment or a call.
    Expr(Expr),
    If {
        cond: Expr,
        then: Block,
        els: Option<Block>,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Block,
    },
    While {
        cond: Expr,
        body: Block,
    },
    DoWhile {
        body: Block,
        cond: Expr,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    /// `discard` -- legal HLSL, and something a preset could write even though
    /// none in the corpus does.
    Discard,
    Block(Block),
    /// A stray `;`.
    Empty,
}

impl Stmt {
    /// The line this statement starts on, where it has an expression to ask.
    pub fn line(&self) -> Option<u32> {
        match self {
            Stmt::Decl(vars) => vars.first().map(|v| v.line),
            Stmt::Expr(e) | Stmt::If { cond: e, .. } | Stmt::While { cond: e, .. } => Some(e.line),
            Stmt::Return(Some(e)) => Some(e.line),
            _ => None,
        }
    }
}

/// A unary operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    /// `++x` and `--x`. 10.1% of the corpus increments something.
    PreInc,
    PreDec,
}

/// A binary operator, including the comparisons -- whose result HLSL will
/// happily multiply by a float3, which is why [`ty`](super::ty) treats `bool`
/// as a base type rather than as a separate world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl BinOp {
    /// True for the operators whose result is a boolean rather than a number.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
        )
    }

    /// True for `&&` and `||`, whose operands are booleans too.
    pub fn is_logical(self) -> bool {
        matches!(self, BinOp::And | BinOp::Or)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    /// For reports. An unsupported construct without a line number is a
    /// message nobody can act on.
    pub line: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// A numeric literal, keeping whether it was written as an integer.
    ///
    /// That distinction is load-bearing rather than pedantic: HLSL's `1/2` is
    /// integer division and is `0`, and so is WGSL's. A translator that made
    /// every literal a float would quietly turn it into `0.5` and change the
    /// picture.
    Number {
        value: f64,
        is_int: bool,
    },
    Bool(bool),
    /// A name: a local, a global, a parameter, or something from MilkDrop's
    /// environment. Which one is a question for the emitter's scope.
    Name(String),
    /// A call or a constructor -- `lerp(a,b,t)` and `float2(x,y)` are the same
    /// syntax, and only the name tells them apart.
    Call {
        name: String,
        args: Vec<Expr>,
    },
    /// `uv.xy`, `texsize.zw`, `ret.x`.
    Member {
        base: Box<Expr>,
        name: String,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    /// `x++` and `x--`, which differ from the prefix forms only as values.
    PostIncDec {
        expr: Box<Expr>,
        inc: bool,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `a = b` and the compound forms. `op` is `None` for a plain assignment.
    Assign {
        op: Option<BinOp>,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// `= { 1, 2, 3 }` -- an aggregate initializer, which the corpus uses to
    /// build lookup tables: `static const float2 offsets[10] = { ... };`.
    ///
    /// Nests, and is flat in HLSL's reading: a `float2[10]` may be written as
    /// twenty scalars rather than as ten pairs, and the corpus does exactly
    /// that. So the emitter regroups by the target's shape rather than
    /// trusting the writer's grouping.
    InitList(Vec<Expr>),
    /// `a, b` -- C's comma operator: evaluate each, take the last one's value.
    ///
    /// Almost always a `for`'s update clause (`i++, j++`) or a declaration
    /// list, and worth 457 of the corpus's shader bodies on its own. WGSL has
    /// no such operator, so the emitter has to unfold it rather than spell it.
    Sequence(Vec<Expr>),
}

impl Expr {
    pub fn new(kind: ExprKind, line: u32) -> Expr {
        Expr { kind, line }
    }
}
