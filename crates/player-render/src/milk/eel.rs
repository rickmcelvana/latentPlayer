//! EEL2 -- the expression language a `.milk` preset is written in.
//!
//! This is the risk `tasks/phase-3.md` named ("The EEL language ... an
//! expression evaluator that is fast (per-vertex, per-frame), safe, and
//! faithful to quirks decades of presets depend on"), so the three words are
//! worth taking one at a time.
//!
//! **Faithful.** EEL2 is not "C with fewer types", and two of its habits are
//! load-bearing for real presets. Verified against Cockos' own WDL source
//! rather than from memory, per CONVENTIONS.md:
//!
//! - Equality and truthiness are *approximate*. `NSEEL_CLOSEFACTOR` is
//!   `0.00001`; `a == b` is `fabs(a - b) < 0.00001` and a value counts as true
//!   when `fabs(v) > 0.00001`. A preset written against exact comparison would
//!   behave differently under one, and presets are written by trying things
//!   until they look right, which means the quirk is in the corpus whether the
//!   authors knew it or not.
//! - Memory is block-allocated: `NSEEL_RAM_BLOCKS` (128) blocks of
//!   `NSEEL_RAM_ITEMSPERBLOCK` (65536) doubles, i.e. 8.4M addressable slots,
//!   reached lazily. The shipped corpus opens with
//!   `loop(10000, megabuf(n)=gmegabuf(n)=0; n+=1;)`, so a preset really does
//!   walk this memory, and allocating it eagerly would cost 64MB a preset.
//! - Names are case-insensitive. The corpus proves it needs to be: one preset
//!   writes `MINBPM = 70` in its init and reads `minbpm` in the loop three
//!   lines later.
//!
//! **Safe.** A preset is a *program from a stranger* -- the whole point of the
//! milestone is that a listener drops a thousand of them into a folder -- and
//! this evaluator runs on the thread that draws. So evaluation carries a
//! budget ([`Context::budget`]) and `loop`/`while` are bounded, and running out
//! is an ordinary [`EvalError`] rather than a hang. EEL2's own bound is
//! `NSEEL_LOOPFUNC_SUPPORT_MAXLEN` (1048576) per loop, which caps one loop and
//! does nothing about ten nested ones.
//!
//! **Fast.** Names are interned to slot indices at compile time so evaluation
//! never touches a hash map, and the tree is walked rather than compiled. The
//! spike measures whether walking is enough
//! (`crates/player-render/examples/milk_spike.rs`).
//!
//! One deliberate divergence: `rand` draws from a seeded generator owned by the
//! [`Context`] rather than from the process. MilkDrop's presets are not
//! reproducible frame to frame; ours have to be, or no preset could ever get a
//! pixel-readback check of the kind every other visual in this crate ships
//! with (decision 6).

use std::collections::HashMap;
use std::fmt;

/// EEL2's `NSEEL_CLOSEFACTOR`. See the module note -- this is the value from
/// WDL's `ns-eel-int.h`, not an epsilon chosen here.
pub const CLOSE_FACTOR: f64 = 0.00001;

/// `NSEEL_RAM_BLOCKS` x `NSEEL_RAM_ITEMSPERBLOCK`, the addressable size of
/// `megabuf`/`gmegabuf`.
pub const RAM_ITEMS_PER_BLOCK: usize = 65536;
pub const RAM_BLOCKS: usize = 128;

/// EEL2's own per-loop bound (`NSEEL_LOOPFUNC_SUPPORT_MAXLEN`).
pub const MAX_LOOP_ITERATIONS: i64 = 1_048_576;

#[derive(Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The tokenizer met a character that starts no token.
    UnexpectedChar(char, usize),
    /// The parser wanted something and found something else.
    Expected(&'static str, usize),
    /// A call to a name this build does not implement, or with the wrong
    /// number of arguments. Named rather than ignored: silently returning 0
    /// for an unknown function is how a preset ends up drawing a still frame
    /// and nobody knows why.
    UnknownFunction(String, usize),
    WrongArity {
        name: &'static str,
        wanted: &'static str,
        got: usize,
    },
    /// `x + 1 = 2` -- the left of an assignment must be a variable or a
    /// `megabuf`/`gmegabuf` slot.
    NotAssignable(usize),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::UnexpectedChar(c, at) => write!(f, "unexpected {c:?} at byte {at}"),
            CompileError::Expected(what, at) => write!(f, "expected {what} at byte {at}"),
            CompileError::UnknownFunction(name, at) => {
                write!(f, "unknown function {name}() at byte {at}")
            }
            CompileError::WrongArity { name, wanted, got } => {
                write!(f, "{name}() takes {wanted} arguments, got {got}")
            }
            CompileError::NotAssignable(at) => write!(f, "not assignable at byte {at}"),
        }
    }
}

impl std::error::Error for CompileError {}

#[derive(Debug, PartialEq, Eq)]
pub enum EvalError {
    /// The budget ran out. See the module note on safety -- this is the
    /// difference between a bad preset and a hung visualizer.
    BudgetExhausted,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::BudgetExhausted => write!(f, "evaluation budget exhausted"),
        }
    }
}

impl std::error::Error for EvalError {}

/// The names a set of programs share.
///
/// MilkDrop's per-frame and per-vertex code see one pool of variables, so the
/// blocks of a preset are compiled against one table and evaluated against one
/// [`Context`]. Interning happens here, once, so evaluation is an index rather
/// than a lookup.
#[derive(Debug, Default)]
pub struct Symbols {
    slots: HashMap<String, usize>,
    names: Vec<String>,
}

impl Symbols {
    pub fn new() -> Self {
        Self::default()
    }

    /// The slot for a name, creating it if new. Case-insensitive, per the
    /// module note.
    pub fn slot(&mut self, name: &str) -> usize {
        let key = name.to_ascii_lowercase();
        if let Some(&slot) = self.slots.get(&key) {
            return slot;
        }
        let slot = self.names.len();
        self.names.push(key.clone());
        self.slots.insert(key, slot);
        slot
    }

    /// The slot for a name that must already exist.
    pub fn existing(&self, name: &str) -> Option<usize> {
        self.slots.get(&name.to_ascii_lowercase()).copied()
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn name(&self, slot: usize) -> Option<&str> {
        self.names.get(slot).map(String::as_str)
    }
}

/// Which of EEL2's two memories a subscript reads.
///
/// `megabuf` is per-program state; `gmegabuf` is shared between every code
/// block in the preset. The shipped corpus uses the distinction for real: it
/// computes a resonator bank in `megabuf` in `per_frame` and publishes the two
/// rows a custom wave reads through `gmegabuf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bank {
    Mega,
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    BitAnd,
    BitOr,
}

#[derive(Clone, Debug)]
enum Target {
    Var(usize),
    Mem(Bank, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Func1 {
    Abs,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sqrt,
    Log,
    Log10,
    Exp,
    Floor,
    Ceil,
    Int,
    Sign,
    Sqr,
    InvSqrt,
    Rand,
    BNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Func2 {
    Min,
    Max,
    Pow,
    Atan2,
    Above,
    Below,
    Equal,
    BAnd,
    BOr,
    Sigmoid,
}

#[derive(Clone, Debug)]
enum Expr {
    Num(f64),
    Var(usize),
    Mem(Bank, Box<Expr>),
    Assign(Target, Box<Expr>),
    /// `a += b` and friends. Kept distinct from `a = a + b` because the target
    /// of a compound assignment to `megabuf(i)` must have its subscript
    /// evaluated once, not twice -- and a subscript can have side effects.
    OpAssign(Target, BinOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Call1(Func1, Box<Expr>),
    Call2(Func2, Box<Expr>, Box<Expr>),
    /// `if(cond, then, else)` -- lazy, and it must be. The corpus writes
    /// `if (beat, vol2=vol*5;t0=time, 0)`, where evaluating both arms would
    /// set `t0` on every frame rather than on beats.
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `loop(count, body)` -- count evaluated once, then truncated.
    Loop(Box<Expr>, Box<Expr>),
    /// `while(body)` -- repeats while the body's own value is true.
    While(Box<Expr>),
    /// A `;`-separated statement list; its value is the last statement's.
    Seq(Vec<Expr>),
}

/// One compiled code block.
#[derive(Debug)]
pub struct Program {
    body: Expr,
}

/// The state a compiled block reads and writes.
pub struct Context {
    vars: Vec<f64>,
    mega: Ram,
    global: Ram,
    /// Nodes left to evaluate before [`EvalError::BudgetExhausted`]. Reset per
    /// call by [`Context::run`]; see the module note on safety.
    pub budget: u64,
    remaining: u64,
    rng: u64,
}

/// EEL2's block-allocated memory, allocated on the block a preset touches
/// rather than up front (see the module note).
struct Ram {
    blocks: Vec<Option<Box<[f64]>>>,
}

impl Ram {
    fn new() -> Self {
        Self {
            blocks: (0..RAM_BLOCKS).map(|_| None).collect(),
        }
    }

    fn address(index: f64) -> Option<(usize, usize)> {
        if !index.is_finite() || index < 0.0 {
            return None;
        }
        let i = index as usize;
        let block = i / RAM_ITEMS_PER_BLOCK;
        if block >= RAM_BLOCKS {
            return None;
        }
        Some((block, i % RAM_ITEMS_PER_BLOCK))
    }

    fn get(&self, index: f64) -> f64 {
        match Self::address(index) {
            Some((b, o)) => self.blocks[b].as_ref().map_or(0.0, |block| block[o]),
            None => 0.0,
        }
    }

    fn set(&mut self, index: f64, value: f64) {
        let Some((b, o)) = Self::address(index) else {
            return;
        };
        let block =
            self.blocks[b].get_or_insert_with(|| vec![0.0; RAM_ITEMS_PER_BLOCK].into_boxed_slice());
        block[o] = value;
    }
}

/// The default evaluation budget: generous enough for the heaviest preset in
/// the shipped corpus (a 256-resonator bank with a nested integrator, measured
/// at roughly 90k nodes a frame) with an order of magnitude spare, and small
/// enough that hitting it costs a fraction of a frame rather than a hang.
pub const DEFAULT_BUDGET: u64 = 4_000_000;

impl Context {
    pub fn new(symbols: &Symbols) -> Self {
        Self {
            vars: vec![0.0; symbols.len()],
            mega: Ram::new(),
            global: Ram::new(),
            budget: DEFAULT_BUDGET,
            remaining: DEFAULT_BUDGET,
            // Any odd constant; the point is only that it is ours and it is
            // the same on both shells.
            rng: 0x2545_F491_4F6C_DD1D,
        }
    }

    /// Grows the variable pool to match a table that has gained names since
    /// this context was made -- compiling a preset's later blocks can intern
    /// names the earlier ones never mentioned.
    pub fn sync(&mut self, symbols: &Symbols) {
        self.vars.resize(symbols.len(), 0.0);
    }

    pub fn get(&self, slot: usize) -> f64 {
        self.vars.get(slot).copied().unwrap_or(0.0)
    }

    pub fn set(&mut self, slot: usize, value: f64) {
        if slot >= self.vars.len() {
            self.vars.resize(slot + 1, 0.0);
        }
        self.vars[slot] = value;
    }

    pub fn megabuf(&self, index: f64) -> f64 {
        self.mega.get(index)
    }

    pub fn gmegabuf(&self, index: f64) -> f64 {
        self.global.get(index)
    }

    /// Nodes evaluated by the most recent [`Context::run`]. The spike reports
    /// this per preset -- it is the figure that says whether a tree walk is
    /// fast enough, independent of how fast this machine happens to be.
    pub fn last_cost(&self) -> u64 {
        self.budget - self.remaining
    }

    /// Runs a compiled block. The budget is restored first, so a block that
    /// exhausted it last frame gets a fresh one this frame rather than being
    /// dead for the rest of the session.
    pub fn run(&mut self, program: &Program) -> Result<f64, EvalError> {
        self.remaining = self.budget;
        self.eval(&program.body)
    }

    fn next_rand(&mut self) -> f64 {
        // xorshift64*, which is small, has no dependency, and gives the same
        // stream on both shells.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // Top 53 bits to a double in [0, 1).
        ((v >> 11) as f64) / ((1u64 << 53) as f64)
    }

    fn eval(&mut self, expr: &Expr) -> Result<f64, EvalError> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(EvalError::BudgetExhausted)?;
        Ok(match expr {
            Expr::Num(v) => *v,
            Expr::Var(slot) => self.get(*slot),
            Expr::Mem(bank, index) => {
                let i = self.eval(index)?;
                match bank {
                    Bank::Mega => self.mega.get(i),
                    Bank::Global => self.global.get(i),
                }
            }
            Expr::Assign(target, value) => {
                // The subscript is evaluated before the value, matching C's
                // usual left-to-right and EEL2's own emitted order.
                let place = self.place(target)?;
                let v = self.eval(value)?;
                self.store(place, v);
                v
            }
            Expr::OpAssign(target, op, value) => {
                let place = self.place(target)?;
                let current = self.load(place);
                let v = self.eval(value)?;
                let result = binary(*op, current, v);
                self.store(place, result);
                result
            }
            Expr::Binary(BinOp::And, l, r) => {
                // Short-circuit, like C and like EEL2's emitted code.
                if truthy(self.eval(l)?) && truthy(self.eval(r)?) {
                    1.0
                } else {
                    0.0
                }
            }
            Expr::Binary(BinOp::Or, l, r) => {
                if truthy(self.eval(l)?) || truthy(self.eval(r)?) {
                    1.0
                } else {
                    0.0
                }
            }
            Expr::Binary(op, l, r) => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                binary(*op, a, b)
            }
            Expr::Neg(e) => -self.eval(e)?,
            Expr::Not(e) => {
                if truthy(self.eval(e)?) {
                    0.0
                } else {
                    1.0
                }
            }
            Expr::Call1(f, a) => {
                let x = self.eval(a)?;
                match f {
                    Func1::Rand => {
                        let r = self.next_rand();
                        r * x
                    }
                    _ => call1(*f, x),
                }
            }
            Expr::Call2(f, a, b) => {
                let x = self.eval(a)?;
                let y = self.eval(b)?;
                call2(*f, x, y)
            }
            Expr::If(cond, then, other) => {
                if truthy(self.eval(cond)?) {
                    self.eval(then)?
                } else {
                    self.eval(other)?
                }
            }
            Expr::Loop(count, body) => {
                let n = self.eval(count)?;
                let n = if n.is_finite() { n as i64 } else { 0 };
                let n = n.clamp(0, MAX_LOOP_ITERATIONS);
                let mut last = 0.0;
                for _ in 0..n {
                    last = self.eval(body)?;
                }
                last
            }
            Expr::While(body) => {
                let mut last;
                let mut spins = 0i64;
                loop {
                    last = self.eval(body)?;
                    spins += 1;
                    if !truthy(last) || spins >= MAX_LOOP_ITERATIONS {
                        break;
                    }
                }
                last
            }
            Expr::Seq(items) => {
                let mut last = 0.0;
                for item in items {
                    last = self.eval(item)?;
                }
                last
            }
        })
    }

    /// Resolves an assignment target to somewhere concrete, evaluating a
    /// subscript exactly once.
    fn place(&mut self, target: &Target) -> Result<Place, EvalError> {
        Ok(match target {
            Target::Var(slot) => Place::Var(*slot),
            Target::Mem(bank, index) => Place::Mem(*bank, self.eval(index)?),
        })
    }

    fn load(&self, place: Place) -> f64 {
        match place {
            Place::Var(slot) => self.get(slot),
            Place::Mem(Bank::Mega, i) => self.mega.get(i),
            Place::Mem(Bank::Global, i) => self.global.get(i),
        }
    }

    fn store(&mut self, place: Place, value: f64) {
        match place {
            Place::Var(slot) => self.set(slot, value),
            Place::Mem(Bank::Mega, i) => self.mega.set(i, value),
            Place::Mem(Bank::Global, i) => self.global.set(i, value),
        }
    }
}

#[derive(Clone, Copy)]
enum Place {
    Var(usize),
    Mem(Bank, f64),
}

/// EEL2's truthiness: `fabs(v) > NSEEL_CLOSEFACTOR`, not `v != 0`.
pub fn truthy(v: f64) -> bool {
    v.abs() > CLOSE_FACTOR
}

fn bool_val(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
    }
}

fn binary(op: BinOp, a: f64, b: f64) -> f64 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::Mod => {
            // EEL2's `%` is an integer remainder, not `fmod` -- the corpus
            // uses it to wrap a sample index (`(i+o)%ni`), where the two
            // differ for negative operands and for fractional ones.
            let d = b as i64;
            if d == 0 {
                0.0
            } else {
                ((a as i64) % d) as f64
            }
        }
        BinOp::Pow => a.powf(b),
        BinOp::Lt => bool_val(a < b),
        BinOp::Gt => bool_val(a > b),
        BinOp::Le => bool_val(a <= b),
        BinOp::Ge => bool_val(a >= b),
        BinOp::Eq => bool_val((a - b).abs() < CLOSE_FACTOR),
        BinOp::Ne => bool_val((a - b).abs() >= CLOSE_FACTOR),
        BinOp::And => bool_val(truthy(a) && truthy(b)),
        BinOp::Or => bool_val(truthy(a) || truthy(b)),
        BinOp::BitAnd => ((a as i64) & (b as i64)) as f64,
        BinOp::BitOr => ((a as i64) | (b as i64)) as f64,
    }
}

fn call1(f: Func1, x: f64) -> f64 {
    match f {
        Func1::Abs => x.abs(),
        Func1::Sin => x.sin(),
        Func1::Cos => x.cos(),
        Func1::Tan => x.tan(),
        Func1::Asin => x.asin(),
        Func1::Acos => x.acos(),
        Func1::Atan => x.atan(),
        Func1::Sqrt => x.abs().sqrt(),
        Func1::Log => x.ln(),
        Func1::Log10 => x.log10(),
        Func1::Exp => x.exp(),
        Func1::Floor => x.floor(),
        Func1::Ceil => x.ceil(),
        Func1::Int => x.trunc(),
        Func1::Sign => {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        Func1::Sqr => x * x,
        Func1::InvSqrt => 1.0 / x.sqrt(),
        Func1::BNot => bool_val(!truthy(x)),
        // Handled by the evaluator, which owns the generator.
        Func1::Rand => 0.0,
    }
}

fn call2(f: Func2, x: f64, y: f64) -> f64 {
    match f {
        Func2::Min => x.min(y),
        Func2::Max => x.max(y),
        Func2::Pow => x.powf(y),
        Func2::Atan2 => x.atan2(y),
        Func2::Above => bool_val(x > y),
        Func2::Below => bool_val(x < y),
        Func2::Equal => bool_val((x - y).abs() < CLOSE_FACTOR),
        Func2::BAnd => bool_val(truthy(x) && truthy(y)),
        Func2::BOr => bool_val(truthy(x) || truthy(y)),
        Func2::Sigmoid => {
            let t = 1.0 + (-x * y).exp();
            if t.abs() > CLOSE_FACTOR {
                1.0 / t
            } else {
                0.0
            }
        }
    }
}

// ---------------------------------------------------------------- tokenizer

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Semi,
    End,
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            src: text.as_bytes(),
            text,
            pos: 0,
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.src[self.pos..].starts_with(b"//") {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            if self.src[self.pos..].starts_with(b"/*") {
                self.pos += 2;
                while self.pos < self.src.len() && !self.src[self.pos..].starts_with(b"*/") {
                    self.pos += 1;
                }
                self.pos = (self.pos + 2).min(self.src.len());
                continue;
            }
            return;
        }
    }

    fn next(&mut self) -> Result<(Tok, usize), CompileError> {
        self.skip_trivia();
        let at = self.pos;
        if self.pos >= self.src.len() {
            return Ok((Tok::End, at));
        }
        let c = self.src[self.pos];

        if c.is_ascii_digit() || (c == b'.' && self.peek_digit(1)) {
            let start = self.pos;
            // `$xhh` hex literals exist in EEL2 but not in any preset here;
            // decimal and exponent notation are what the corpus writes.
            while self.pos < self.src.len()
                && (self.src[self.pos].is_ascii_digit() || self.src[self.pos] == b'.')
            {
                self.pos += 1;
            }
            if self.pos < self.src.len() && (self.src[self.pos] | 0x20) == b'e' {
                let save = self.pos;
                self.pos += 1;
                if self.pos < self.src.len()
                    && (self.src[self.pos] == b'+' || self.src[self.pos] == b'-')
                {
                    self.pos += 1;
                }
                if self.peek_digit(0) {
                    while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                } else {
                    self.pos = save;
                }
            }
            let value = self.text[start..self.pos].parse::<f64>().unwrap_or(0.0);
            return Ok((Tok::Num(value), at));
        }

        if c.is_ascii_alphabetic() || c == b'_' {
            let start = self.pos;
            while self.pos < self.src.len()
                && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'_')
            {
                self.pos += 1;
            }
            return Ok((Tok::Ident(self.text[start..self.pos].to_string()), at));
        }

        // Two-character operators first, so `<=` never lexes as `<` then `=`.
        const TWO: &[&str] = &[
            "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=",
        ];
        if self.pos + 1 < self.src.len() {
            let pair = &self.text[self.pos..self.pos + 2];
            if let Some(op) = TWO.iter().find(|o| **o == pair) {
                self.pos += 2;
                return Ok((Tok::Op(op), at));
            }
        }

        const ONE: &[&str] = &["+", "-", "*", "/", "%", "^", "<", ">", "=", "!", "&", "|"];
        let single = &self.text[self.pos..self.pos + 1];
        if let Some(op) = ONE.iter().find(|o| **o == single) {
            self.pos += 1;
            return Ok((Tok::Op(op), at));
        }

        self.pos += 1;
        Ok((
            match c {
                b'(' => Tok::LParen,
                b')' => Tok::RParen,
                b',' => Tok::Comma,
                b';' => Tok::Semi,
                _ => return Err(CompileError::UnexpectedChar(c as char, at)),
            },
            at,
        ))
    }

    fn peek_digit(&self, ahead: usize) -> bool {
        self.src
            .get(self.pos + ahead)
            .is_some_and(u8::is_ascii_digit)
    }
}

// ------------------------------------------------------------------ parser

struct Parser<'a, 's> {
    lexer: Lexer<'a>,
    tok: Tok,
    at: usize,
    symbols: &'s mut Symbols,
}

/// Compiles one code block against a shared symbol table.
///
/// An empty block compiles to an expression that evaluates to 0 -- presets
/// routinely leave `per_pixel` out entirely, and that is not an error.
pub fn compile(source: &str, symbols: &mut Symbols) -> Result<Program, CompileError> {
    let mut parser = Parser::new(source, symbols)?;
    let body = parser.parse_sequence(&[Tok::End])?;
    parser.expect_end()?;
    Ok(Program { body })
}

impl<'a, 's> Parser<'a, 's> {
    fn new(text: &'a str, symbols: &'s mut Symbols) -> Result<Self, CompileError> {
        let mut lexer = Lexer::new(text);
        let (tok, at) = lexer.next()?;
        Ok(Self {
            lexer,
            tok,
            at,
            symbols,
        })
    }

    fn bump(&mut self) -> Result<(), CompileError> {
        let (tok, at) = self.lexer.next()?;
        self.tok = tok;
        self.at = at;
        Ok(())
    }

    fn expect_end(&self) -> Result<(), CompileError> {
        if self.tok == Tok::End {
            Ok(())
        } else {
            Err(CompileError::Expected("end of block", self.at))
        }
    }

    fn eat_op(&mut self, op: &str) -> Result<bool, CompileError> {
        if self.tok == Tok::Op(unsafe_static(op)) {
            self.bump()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// `a; b; c` -- also what sits inside parentheses and inside a call's
    /// arguments, which is why `if(c, x=1;y=2, 0)` parses.
    fn parse_sequence(&mut self, stop: &[Tok]) -> Result<Expr, CompileError> {
        let mut items: Vec<Expr> = Vec::new();
        loop {
            while self.tok == Tok::Semi {
                self.bump()?;
            }
            if stop.contains(&self.tok) || self.tok == Tok::RParen || self.tok == Tok::Comma {
                break;
            }
            items.push(self.parse_assign()?);
            if self.tok == Tok::Semi {
                continue;
            }
            break;
        }
        Ok(match items.len() {
            0 => Expr::Num(0.0),
            1 => items.pop().unwrap_or(Expr::Num(0.0)),
            _ => Expr::Seq(items),
        })
    }

    fn parse_assign(&mut self) -> Result<Expr, CompileError> {
        let at = self.at;
        let left = self.parse_binary(0)?;
        let op = match &self.tok {
            Tok::Op("=") => None,
            Tok::Op("+=") => Some(BinOp::Add),
            Tok::Op("-=") => Some(BinOp::Sub),
            Tok::Op("*=") => Some(BinOp::Mul),
            Tok::Op("/=") => Some(BinOp::Div),
            Tok::Op("%=") => Some(BinOp::Mod),
            _ => return Ok(left),
        };
        self.bump()?;
        let target = match left {
            Expr::Var(slot) => Target::Var(slot),
            Expr::Mem(bank, index) => Target::Mem(bank, index),
            _ => return Err(CompileError::NotAssignable(at)),
        };
        // Right-associative, which is what makes `r1=r2=0` (in the corpus)
        // assign zero to both rather than assigning a comparison to `r1`.
        let value = self.parse_assign()?;
        Ok(match op {
            None => Expr::Assign(target, Box::new(value)),
            Some(op) => Expr::OpAssign(target, op, Box::new(value)),
        })
    }

    /// Precedence climbing. The table is EEL2's, lowest binding first.
    fn parse_binary(&mut self, min_level: u8) -> Result<Expr, CompileError> {
        let mut left = self.parse_unary()?;
        while let Some((op, level)) = self.binop() {
            if level < min_level {
                break;
            }
            self.bump()?;
            let right = self.parse_binary(level + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn binop(&self) -> Option<(BinOp, u8)> {
        Some(match &self.tok {
            Tok::Op("||") => (BinOp::Or, 1),
            Tok::Op("&&") => (BinOp::And, 2),
            Tok::Op("|") => (BinOp::BitOr, 3),
            Tok::Op("&") => (BinOp::BitAnd, 4),
            Tok::Op("==") => (BinOp::Eq, 5),
            Tok::Op("!=") => (BinOp::Ne, 5),
            Tok::Op("<") => (BinOp::Lt, 6),
            Tok::Op(">") => (BinOp::Gt, 6),
            Tok::Op("<=") => (BinOp::Le, 6),
            Tok::Op(">=") => (BinOp::Ge, 6),
            Tok::Op("+") => (BinOp::Add, 7),
            Tok::Op("-") => (BinOp::Sub, 7),
            Tok::Op("*") => (BinOp::Mul, 8),
            Tok::Op("/") => (BinOp::Div, 8),
            Tok::Op("%") => (BinOp::Mod, 8),
            Tok::Op("^") => (BinOp::Pow, 9),
            _ => return None,
        })
    }

    fn parse_unary(&mut self) -> Result<Expr, CompileError> {
        if self.eat_op("-")? {
            return Ok(Expr::Neg(Box::new(self.parse_unary()?)));
        }
        if self.eat_op("+")? {
            return self.parse_unary();
        }
        if self.eat_op("!")? {
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<Expr, CompileError> {
        let at = self.at;
        match self.tok.clone() {
            Tok::Num(v) => {
                self.bump()?;
                Ok(Expr::Num(v))
            }
            Tok::LParen => {
                self.bump()?;
                let inner = self.parse_sequence(&[Tok::End])?;
                if self.tok != Tok::RParen {
                    return Err(CompileError::Expected(")", self.at));
                }
                self.bump()?;
                Ok(inner)
            }
            Tok::Ident(name) => {
                self.bump()?;
                if self.tok == Tok::LParen {
                    self.bump()?;
                    let args = self.parse_args()?;
                    return self.build_call(&name, args, at);
                }
                Ok(Expr::Var(self.symbols.slot(&name)))
            }
            _ => Err(CompileError::Expected("an expression", at)),
        }
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, CompileError> {
        let mut args = Vec::new();
        if self.tok == Tok::RParen {
            self.bump()?;
            return Ok(args);
        }
        loop {
            args.push(self.parse_sequence(&[Tok::End])?);
            match self.tok {
                Tok::Comma => {
                    self.bump()?;
                }
                Tok::RParen => {
                    self.bump()?;
                    break;
                }
                _ => return Err(CompileError::Expected(", or )", self.at)),
            }
        }
        Ok(args)
    }

    fn build_call(
        &mut self,
        name: &str,
        mut args: Vec<Expr>,
        at: usize,
    ) -> Result<Expr, CompileError> {
        let lower = name.to_ascii_lowercase();
        let arity = |wanted: &'static str, got: usize, name: &'static str| {
            CompileError::WrongArity { name, wanted, got }
        };

        // Special forms first: these control evaluation and cannot be a
        // function taking values.
        match lower.as_str() {
            "if" => {
                if args.len() != 3 {
                    return Err(arity("3", args.len(), "if"));
                }
                let other = args.pop().unwrap_or(Expr::Num(0.0));
                let then = args.pop().unwrap_or(Expr::Num(0.0));
                let cond = args.pop().unwrap_or(Expr::Num(0.0));
                return Ok(Expr::If(Box::new(cond), Box::new(then), Box::new(other)));
            }
            "loop" => {
                if args.len() != 2 {
                    return Err(arity("2", args.len(), "loop"));
                }
                let body = args.pop().unwrap_or(Expr::Num(0.0));
                let count = args.pop().unwrap_or(Expr::Num(0.0));
                return Ok(Expr::Loop(Box::new(count), Box::new(body)));
            }
            "while" => {
                if args.len() != 1 {
                    return Err(arity("1", args.len(), "while"));
                }
                return Ok(Expr::While(Box::new(args.pop().unwrap_or(Expr::Num(0.0)))));
            }
            "exec2" | "exec3" => return Ok(Expr::Seq(args)),
            "megabuf" | "gmegabuf" => {
                if args.len() != 1 {
                    return Err(arity("1", args.len(), "megabuf"));
                }
                let bank = if lower == "megabuf" {
                    Bank::Mega
                } else {
                    Bank::Global
                };
                return Ok(Expr::Mem(
                    bank,
                    Box::new(args.pop().unwrap_or(Expr::Num(0.0))),
                ));
            }
            _ => {}
        }

        const ONE_ARG: &[(&str, Func1)] = &[
            ("abs", Func1::Abs),
            ("sin", Func1::Sin),
            ("cos", Func1::Cos),
            ("tan", Func1::Tan),
            ("asin", Func1::Asin),
            ("acos", Func1::Acos),
            ("atan", Func1::Atan),
            ("sqrt", Func1::Sqrt),
            ("log", Func1::Log),
            ("log10", Func1::Log10),
            ("exp", Func1::Exp),
            ("floor", Func1::Floor),
            ("ceil", Func1::Ceil),
            ("int", Func1::Int),
            ("sign", Func1::Sign),
            ("sqr", Func1::Sqr),
            ("invsqrt", Func1::InvSqrt),
            ("rand", Func1::Rand),
            ("bnot", Func1::BNot),
        ];
        const TWO_ARG: &[(&str, Func2)] = &[
            ("min", Func2::Min),
            ("max", Func2::Max),
            ("pow", Func2::Pow),
            ("atan2", Func2::Atan2),
            ("above", Func2::Above),
            ("below", Func2::Below),
            ("equal", Func2::Equal),
            ("band", Func2::BAnd),
            ("bor", Func2::BOr),
            ("sigmoid", Func2::Sigmoid),
        ];

        if let Some((_, f)) = ONE_ARG.iter().find(|(n, _)| *n == lower) {
            if args.len() != 1 {
                return Err(CompileError::WrongArity {
                    name: "a one-argument function",
                    wanted: "1",
                    got: args.len(),
                });
            }
            return Ok(Expr::Call1(
                *f,
                Box::new(args.pop().unwrap_or(Expr::Num(0.0))),
            ));
        }
        if let Some((_, f)) = TWO_ARG.iter().find(|(n, _)| *n == lower) {
            if args.len() != 2 {
                return Err(CompileError::WrongArity {
                    name: "a two-argument function",
                    wanted: "2",
                    got: args.len(),
                });
            }
            let b = args.pop().unwrap_or(Expr::Num(0.0));
            let a = args.pop().unwrap_or(Expr::Num(0.0));
            return Ok(Expr::Call2(*f, Box::new(a), Box::new(b)));
        }

        Err(CompileError::UnknownFunction(name.to_string(), at))
    }
}

/// `Tok::Op` holds a `&'static str` from the lexer's own tables, so comparing
/// against a caller's string needs one of those same entries.
fn unsafe_static(op: &str) -> &'static str {
    const ALL: &[&str] = &[
        "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "+", "-", "*", "/", "%",
        "^", "<", ">", "=", "!", "&", "|",
    ];
    ALL.iter().find(|o| **o == op).copied().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> (Context, Symbols, f64) {
        let mut symbols = Symbols::new();
        let program = compile(source, &mut symbols).expect("compiles");
        let mut ctx = Context::new(&symbols);
        let value = ctx.run(&program).expect("evaluates");
        (ctx, symbols, value)
    }

    fn var(source: &str, name: &str) -> f64 {
        let (ctx, symbols, _) = run(source);
        ctx.get(symbols.existing(name).expect("name exists"))
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(run("1 + 2 * 3").2, 7.0);
        assert_eq!(run("(1 + 2) * 3").2, 9.0);
        assert_eq!(run("2 ^ 3 * 2").2, 16.0);
        assert_eq!(run("-2 + 1").2, -1.0);
    }

    #[test]
    fn names_are_case_insensitive() {
        // The corpus depends on this: one preset sets MINBPM and reads minbpm.
        assert_eq!(var("MINBPM = 70; x = minbpm * 2;", "x"), 140.0);
    }

    #[test]
    fn equality_uses_the_close_factor() {
        // Verified against WDL: `==` is |a-b| < 0.00001, not exact.
        assert_eq!(run("1.0 == 1.000001").2, 1.0);
        assert_eq!(run("1.0 == 1.001").2, 0.0);
    }

    #[test]
    fn truthiness_uses_the_close_factor() {
        // A tiny non-zero value is false in EEL2, which is not what `!= 0`
        // would say.
        assert_eq!(run("if(0.000001, 5, 7)").2, 7.0);
        assert_eq!(run("if(0.1, 5, 7)").2, 5.0);
    }

    #[test]
    fn if_is_lazy() {
        // From the corpus's shape: `if(cond, side_effect, 0)`.
        assert_eq!(var("hit = 0; if(0, hit = 1, 0);", "hit"), 0.0);
        assert_eq!(var("hit = 0; if(1, hit = 1, 0);", "hit"), 1.0);
    }

    #[test]
    fn statement_lists_live_inside_call_arguments() {
        assert_eq!(var("if(1, a=2;b=3, 0);", "b"), 3.0);
    }

    #[test]
    fn assignment_chains_right_to_left() {
        let (ctx, symbols, _) = run("r1=r2=4;");
        assert_eq!(ctx.get(symbols.existing("r1").unwrap()), 4.0);
        assert_eq!(ctx.get(symbols.existing("r2").unwrap()), 4.0);
    }

    #[test]
    fn megabuf_reads_writes_and_chains() {
        // Exactly the corpus's init line shape.
        assert_eq!(
            var("megabuf(3) = gmegabuf(3) = 9; x = megabuf(3);", "x"),
            9.0
        );
        assert_eq!(
            var("megabuf(3) = gmegabuf(3) = 9; y = gmegabuf(3);", "y"),
            9.0
        );
    }

    #[test]
    fn megabuf_is_two_separate_memories() {
        assert_eq!(var("megabuf(1) = 5; x = gmegabuf(1);", "x"), 0.0);
    }

    #[test]
    fn unwritten_memory_reads_zero_including_out_of_range() {
        assert_eq!(run("megabuf(500)").2, 0.0);
        assert_eq!(run("megabuf(-1)").2, 0.0);
        assert_eq!(run("megabuf(1e12)").2, 0.0);
    }

    #[test]
    fn compound_assignment_to_memory_evaluates_its_subscript_once() {
        // `n` advances once, so the write lands at index 0 and `n` ends at 1 --
        // not index 1 with `n` at 2.
        let (ctx, symbols, _) = run("n = 0; megabuf(n; n += 1; n - 1) += 7;");
        assert_eq!(ctx.megabuf(0.0), 7.0);
        assert_eq!(ctx.get(symbols.existing("n").unwrap()), 1.0);
    }

    #[test]
    fn loop_runs_a_counted_body() {
        assert_eq!(var("n=0; loop(10, n += 1;);", "n"), 10.0);
        assert_eq!(var("n=5; loop(0, n += 1;);", "n"), 5.0);
        assert_eq!(var("n=5; loop(-3, n += 1;);", "n"), 5.0);
    }

    #[test]
    fn modulo_is_an_integer_remainder() {
        // The corpus wraps a sample index with it: (i+o)%ni.
        assert_eq!(run("7 % 3").2, 1.0);
        assert_eq!(run("7.9 % 3").2, 1.0);
        assert_eq!(run("5 % 0").2, 0.0);
    }

    #[test]
    fn comments_do_not_eat_the_next_statement() {
        assert_eq!(var("a = 1; //mirrored\nb = a + 1;", "b"), 2.0);
        assert_eq!(var("a = 1; /* note */ b = a + 1;", "b"), 2.0);
    }

    #[test]
    fn rand_is_bounded_and_reproducible() {
        let (_, _, first) = run("x=0; loop(50, x = max(x, rand(1)));");
        let (_, _, again) = run("x=0; loop(50, x = max(x, rand(1)));");
        assert_eq!(first, again, "the same program must draw the same numbers");
        assert!(
            (0.0..1.0).contains(&first),
            "rand(1) stays in [0,1): {first}"
        );
    }

    #[test]
    fn a_runaway_loop_is_an_error_rather_than_a_hang() {
        let mut symbols = Symbols::new();
        let program = compile("loop(1000000, x += 1);", &mut symbols).unwrap();
        let mut ctx = Context::new(&symbols);
        ctx.budget = 1000;
        assert_eq!(ctx.run(&program), Err(EvalError::BudgetExhausted));
    }

    #[test]
    fn an_unknown_function_is_named_rather_than_ignored() {
        let mut symbols = Symbols::new();
        let err = compile("x = frobnicate(1);", &mut symbols).unwrap_err();
        assert!(matches!(err, CompileError::UnknownFunction(ref n, _) if n == "frobnicate"));
    }

    #[test]
    fn an_empty_block_compiles() {
        assert_eq!(run("").2, 0.0);
        assert_eq!(run("   \n // nothing \n ").2, 0.0);
    }

    #[test]
    fn context_state_survives_between_runs() {
        let mut symbols = Symbols::new();
        let init = compile("n = 10;", &mut symbols).unwrap();
        let step = compile("n += 1;", &mut symbols).unwrap();
        let mut ctx = Context::new(&symbols);
        ctx.run(&init).unwrap();
        ctx.run(&step).unwrap();
        ctx.run(&step).unwrap();
        assert_eq!(ctx.get(symbols.existing("n").unwrap()), 12.0);
    }
}
