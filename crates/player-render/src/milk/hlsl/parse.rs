//! Reading a preset's shader body into an [`ast::Shader`].
//!
//! The grammar is the corpus survey's table, in its order. Branches, loops,
//! preset-defined functions and `static`/`const` globals are in because they
//! are 25.1%, 10.2%, 5.7% and 6.0% of eleven thousand real presets. `struct`,
//! the bitwise operators and the preprocessor are out because they are zero,
//! zero and 3.1% -- and each of them is refused **by name and line number**
//! rather than by falling over, so a report can say which construct cost which
//! file.
//!
//! # It never panics, and it always says where
//!
//! Every failure is an [`HlslError`] carrying a line and a stable `construct`
//! slug. The slug is what makes the count tallyable across a collection: "347
//! presets need the preprocessor" is a plan, "347 presets failed" is not.
//!
//! Recursion is bounded ([`MAX_DEPTH`]) for the same reason decision 51 bounds
//! the equation interpreter -- a preset is a stranger's file, and a thousand
//! nested parentheses should end that file's translation rather than the
//! process.
//!
//! # Integer literals stay integers
//!
//! `1/2` is `0` in HLSL, and in WGSL, and would be `0.5` in a translator that
//! made every literal a float. So [`ExprKind::Number`] keeps `is_int`, and the
//! emitter's type rules do the widening HLSL would have done.

use super::ast::*;
use super::lex::{lex, Tok, Token};
use super::ty::Ty;

/// How deep an expression or block may nest before this gives up on the file.
pub const MAX_DEPTH: u32 = 128;

/// Why a shader could not be read, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlslError {
    /// 1-based line within the shader body.
    pub line: u32,
    /// A stable short id for the thing that was not readable, for tallying
    /// across a collection: `"preprocessor"`, `"struct"`, `"bitwise"`,
    /// `"unknown-type"`, `"syntax"`, `"too-deep"`, `"no-shader-body"`.
    pub construct: &'static str,
    /// One sentence for a person.
    pub detail: String,
}

impl HlslError {
    fn new(line: u32, construct: &'static str, detail: impl Into<String>) -> HlslError {
        HlslError {
            line,
            construct,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for HlslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.detail)
    }
}

type R<T> = Result<T, HlslError>;

/// Read a shader body: lex, preprocess, then parse.
pub fn parse(src: &str) -> R<Shader> {
    let tokens = super::pre::preprocess(lex(src))?;
    Parser {
        toks: tokens,
        pos: 0,
        depth: 0,
    }
    .shader()
}

struct Parser<'a> {
    toks: Vec<Token<'a>>,
    pos: usize,
    depth: u32,
}

/// Keywords that qualify a declaration rather than naming a type.
const QUALIFIERS: &[&str] = &["static", "const", "uniform"];

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token<'a>> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, n: usize) -> Option<&Token<'a>> {
        self.toks.get(self.pos + n)
    }

    /// The line to blame when something goes wrong here.
    fn line(&self) -> u32 {
        self.peek()
            .or_else(|| self.toks.last())
            .map_or(1, |t| t.line)
    }

    fn bump(&mut self) -> Option<Token<'a>> {
        let t = self.toks.get(self.pos).copied();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn is_op(&self, text: &str) -> bool {
        self.peek()
            .is_some_and(|t| t.kind == Tok::Op && t.text == text)
    }

    fn is_word(&self, text: &str) -> bool {
        self.peek()
            .is_some_and(|t| t.kind == Tok::Ident && t.text == text)
    }

    fn eat_op(&mut self, text: &str) -> bool {
        if self.is_op(text) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn eat_word(&mut self, text: &str) -> bool {
        if self.is_word(text) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn expect_op(&mut self, text: &str) -> R<()> {
        if self.eat_op(text) {
            return Ok(());
        }
        let found = self.peek().map_or("end of shader", |t| t.text);
        Err(HlslError::new(
            self.line(),
            "syntax",
            format!("expected `{text}`, found `{found}`"),
        ))
    }

    fn ident(&mut self) -> R<(String, u32)> {
        match self.peek() {
            Some(t) if t.kind == Tok::Ident => {
                let (name, line) = (t.text.to_owned(), t.line);
                self.pos += 1;
                Ok((name, line))
            }
            _ => Err(HlslError::new(
                self.line(),
                "syntax",
                "expected a name here",
            )),
        }
    }

    fn deeper(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(HlslError::new(
                self.line(),
                "too-deep",
                format!("nested more than {MAX_DEPTH} deep"),
            ));
        }
        Ok(())
    }

    /// Refuse the constructs this grammar deliberately does not read, by name.
    fn refuse_unreadable(&self) -> R<()> {
        let Some(t) = self.peek() else {
            return Ok(());
        };
        match t.kind {
            // The preprocessor consumes every directive before this runs, so
            // one reaching here is a bug in that pass rather than in the file.
            Tok::Directive => Err(HlslError::new(
                t.line,
                "preprocessor",
                format!("`{}` survived preprocessing", t.text),
            )),
            Tok::Unknown => Err(HlslError::new(
                t.line,
                "syntax",
                format!("`{}` is not part of HLSL", t.text),
            )),
            Tok::Ident if t.text == "struct" => Err(HlslError::new(
                t.line,
                "struct",
                "`struct` is not supported",
            )),
            Tok::Op if matches!(t.text, "&" | "|" | "^" | "~" | "<<" | ">>") => Err(
                HlslError::new(t.line, "bitwise", format!("`{}` is not supported", t.text)),
            ),
            _ => Ok(()),
        }
    }

    // ---- top level -------------------------------------------------------

    fn shader(&mut self) -> R<Shader> {
        let mut out = Shader::default();
        loop {
            while self.eat_op(";") {}
            if self.peek().is_none() {
                break;
            }
            self.refuse_unreadable()?;

            // `shader_body { ... }` -- the wrapper MilkDrop replaces with an
            // entry point. The word is a marker in the source rather than
            // something `include.fx` defines; see `env::BODY_MARKER`.
            if self.is_word(super::env::BODY_MARKER) {
                self.pos += 1;
                let block = self.block()?;
                // A second one is not a thing MilkDrop supports -- it seeks
                // the *first* occurrence -- so the first wins and the rest is
                // read as ordinary items.
                if out.body.is_none() {
                    out.body = Some(block);
                }
                // Everything after the body is discarded, because MilkDrop
                // discards it: `LoadShaderFromMemory` finds the *last* `}`
                // and overwrites from there to the end of the text. Files in
                // the corpus sign off after the brace -- "written by martin",
                // "END" -- and compile in MilkDrop for exactly this reason.
                break;
            }

            let (quals, ty, ty_line) = self.decl_head()?;
            let (name, line) = self.ident()?;

            if self.is_op("(") {
                out.functions.push(self.function(ty, name, line)?);
            } else {
                for var in self.declarators(quals, ty, name, line)? {
                    out.globals.push(var);
                }
                self.expect_op(";").map_err(|mut e| {
                    e.line = ty_line.max(e.line);
                    e
                })?;
            }
        }
        Ok(out)
    }

    /// Qualifiers and a type name, which every declaration starts with.
    fn decl_head(&mut self) -> R<(Qualifiers, Ty, u32)> {
        let mut quals = Qualifiers::default();
        loop {
            if self.eat_word("static") {
                quals.is_static = true;
            } else if self.eat_word("const") {
                quals.is_const = true;
            } else if self.eat_word("uniform") {
                quals.is_uniform = true;
            } else {
                break;
            }
        }
        let line = self.line();
        let (name, _) = self.ident()?;
        let ty = Ty::from_name(&name).ok_or_else(|| {
            HlslError::new(
                line,
                "unknown-type",
                format!("`{name}` is not a type this build knows"),
            )
        })?;
        Ok((quals, ty, line))
    }

    fn function(&mut self, ret: Ty, name: String, line: u32) -> R<Function> {
        self.expect_op("(")?;
        let mut params = Vec::new();
        if !self.is_op(")") {
            loop {
                let mut passing = Passing::default();
                loop {
                    if self.eat_word("out") {
                        passing.out = true;
                    } else if self.eat_word("inout") {
                        passing.inout = true;
                    } else if self.eat_word("in") {
                        // The default, and writing it changes nothing.
                    } else {
                        break;
                    }
                }
                let (_, ty, ty_line) = self.decl_head()?;
                if ty == Ty::Void {
                    // `f(void)` is an empty parameter list, not a parameter.
                    let _ = ty_line;
                    break;
                }
                let (pname, _) = self.ident()?;
                params.push(Param {
                    ty,
                    name: pname,
                    passing,
                });
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        let body = self.block()?;
        Ok(Function {
            ret,
            name,
            params,
            body,
            line,
        })
    }

    /// The comma-separated names in one declaration, the first already read.
    fn declarators(&mut self, quals: Qualifiers, ty: Ty, first: String, line: u32) -> R<Vec<Var>> {
        let mut out = Vec::new();
        let mut name = first;
        let mut name_line = line;
        loop {
            let mut array = None;
            if self.eat_op("[") {
                let len = match self.peek() {
                    Some(t) if t.kind == Tok::Number => {
                        let n = number_of(t.text).0;
                        self.pos += 1;
                        Some(n as u32)
                    }
                    _ => {
                        return Err(HlslError::new(
                            self.line(),
                            "array-length",
                            "an array length must be a plain number here",
                        ))
                    }
                };
                self.expect_op("]")?;
                array = len;
            }
            let mut sampler_state = Vec::new();
            let init = if self.eat_op("=") {
                if self.is_word("sampler_state") {
                    self.pos += 1;
                    sampler_state = self.sampler_state()?;
                    None
                } else if self.is_op("{") {
                    Some(self.init_list()?)
                } else {
                    Some(self.assignment()?)
                }
            } else {
                None
            };
            out.push(Var {
                qualifiers: quals,
                ty,
                name,
                array,
                init,
                sampler_state,
                line: name_line,
            });
            if !self.eat_op(",") {
                break;
            }
            let next = self.ident()?;
            name = next.0;
            name_line = next.1;
        }
        Ok(out)
    }

    /// `sampler_state { AddressU = WRAP; MagFilter = LINEAR; }`.
    ///
    /// A settings block rather than an expression, and read as one: names on
    /// the left, one token's worth of value on the right. Presets use it to
    /// make a texture wrap, which is a real difference in the picture rather
    /// than a formality.
    fn sampler_state(&mut self) -> R<Vec<(String, String)>> {
        self.expect_op("{")?;
        let mut out = Vec::new();
        while !self.is_op("}") {
            let Some(key) = self.peek().copied() else {
                return Err(HlslError::new(
                    self.line(),
                    "syntax",
                    "the shader ends inside a sampler_state block",
                ));
            };
            self.pos += 1;
            if key.kind == Tok::Op && key.text == ";" {
                continue;
            }
            self.expect_op("=")?;
            let mut value = String::new();
            while !self.is_op(";") && !self.is_op("}") {
                let Some(t) = self.bump() else {
                    return Err(HlslError::new(
                        key.line,
                        "syntax",
                        "the shader ends inside a sampler_state block",
                    ));
                };
                value.push_str(t.text);
            }
            self.eat_op(";");
            out.push((key.text.to_owned(), value));
        }
        self.expect_op("}")?;
        Ok(out)
    }

    /// `{ a, b, { c, d } }` -- an aggregate initializer, nesting and
    /// tolerating a trailing comma the way HLSL does.
    fn init_list(&mut self) -> R<Expr> {
        self.deeper()?;
        let line = self.line();
        self.expect_op("{")?;
        let mut items = Vec::new();
        while !self.is_op("}") {
            if self.peek().is_none() {
                self.depth -= 1;
                return Err(HlslError::new(
                    line,
                    "syntax",
                    "the shader ends inside an initializer",
                ));
            }
            items.push(if self.is_op("{") {
                self.init_list()?
            } else {
                self.assignment()?
            });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op("}")?;
        self.depth -= 1;
        Ok(Expr::new(ExprKind::InitList(items), line))
    }

    // ---- statements ------------------------------------------------------

    fn block(&mut self) -> R<Block> {
        self.deeper()?;
        self.expect_op("{")?;
        let mut stmts = Vec::new();
        while !self.is_op("}") {
            if self.peek().is_none() {
                self.depth -= 1;
                return Err(HlslError::new(
                    self.line(),
                    "syntax",
                    "the shader ends inside a block",
                ));
            }
            stmts.push(self.stmt()?);
        }
        self.pos += 1;
        self.depth -= 1;
        Ok(Block(stmts))
    }

    /// A statement, or a block wrapped as one -- so `if (x) y = 1;` and
    /// `if (x) { y = 1; }` produce the same shape.
    fn body_block(&mut self) -> R<Block> {
        if self.is_op("{") {
            return self.block();
        }
        Ok(Block(vec![self.stmt()?]))
    }

    fn stmt(&mut self) -> R<Stmt> {
        self.refuse_unreadable()?;
        if self.is_op("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.eat_op(";") {
            return Ok(Stmt::Empty);
        }
        if self.eat_word("if") {
            self.deeper()?;
            self.expect_op("(")?;
            let cond = self.expr()?;
            self.expect_op(")")?;
            let then = self.body_block()?;
            let els = if self.eat_word("else") {
                Some(self.body_block()?)
            } else {
                None
            };
            self.depth -= 1;
            return Ok(Stmt::If { cond, then, els });
        }
        if self.eat_word("while") {
            self.deeper()?;
            self.expect_op("(")?;
            let cond = self.expr()?;
            self.expect_op(")")?;
            let body = self.body_block()?;
            self.depth -= 1;
            return Ok(Stmt::While { cond, body });
        }
        if self.eat_word("do") {
            self.deeper()?;
            let body = self.body_block()?;
            if !self.eat_word("while") {
                self.depth -= 1;
                return Err(HlslError::new(self.line(), "syntax", "expected `while`"));
            }
            self.expect_op("(")?;
            let cond = self.expr()?;
            self.expect_op(")")?;
            self.eat_op(";");
            self.depth -= 1;
            return Ok(Stmt::DoWhile { body, cond });
        }
        if self.eat_word("for") {
            self.deeper()?;
            self.expect_op("(")?;
            let init = if self.is_op(";") {
                self.pos += 1;
                None
            } else if self.starts_declaration() {
                Some(Box::new(self.declaration_stmt()?))
            } else {
                let e = self.expr()?;
                self.expect_op(";")?;
                Some(Box::new(Stmt::Expr(e)))
            };
            let cond = if self.is_op(";") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect_op(";")?;
            let step = if self.is_op(")") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect_op(")")?;
            let body = self.body_block()?;
            self.depth -= 1;
            return Ok(Stmt::For {
                init,
                cond,
                step,
                body,
            });
        }
        if self.eat_word("return") {
            let value = if self.is_op(";") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect_op(";")?;
            return Ok(Stmt::Return(value));
        }
        if self.eat_word("break") {
            self.expect_op(";")?;
            return Ok(Stmt::Break);
        }
        if self.eat_word("continue") {
            self.expect_op(";")?;
            return Ok(Stmt::Continue);
        }
        if self.eat_word("discard") {
            self.eat_op(";");
            return Ok(Stmt::Discard);
        }
        if self.starts_declaration() {
            return self.declaration_stmt();
        }
        let e = self.expr()?;
        self.expect_op(";")?;
        Ok(Stmt::Expr(e))
    }

    /// Whether what follows declares a name rather than evaluating one.
    ///
    /// The only ambiguity is a type name at the start of an expression:
    /// `float2 v = ...` declares and `float2(x, y)` constructs. A declaration
    /// is a type *followed by a name*, and there are no user-defined types
    /// here to widen that test.
    fn starts_declaration(&self) -> bool {
        let Some(t) = self.peek() else {
            return false;
        };
        if t.kind != Tok::Ident {
            return false;
        }
        if QUALIFIERS.contains(&t.text) {
            return true;
        }
        Ty::from_name(t.text).is_some() && self.peek_at(1).is_some_and(|n| n.kind == Tok::Ident)
    }

    fn declaration_stmt(&mut self) -> R<Stmt> {
        let (quals, ty, _) = self.decl_head()?;
        let (name, line) = self.ident()?;
        let vars = self.declarators(quals, ty, name, line)?;
        self.expect_op(";")?;
        Ok(Stmt::Decl(vars))
    }

    // ---- expressions -----------------------------------------------------

    /// A full expression, including C's comma operator.
    ///
    /// Only the contexts where a comma *separates* rather than sequences --
    /// argument lists, declarator lists, initializer lists -- call
    /// [`Self::assignment`] directly instead. Getting that split wrong turns
    /// `f(a, b)` into a call with one sequenced argument.
    fn expr(&mut self) -> R<Expr> {
        let first = self.assignment()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let line = first.line;
        let mut items = vec![first];
        while self.eat_op(",") {
            items.push(self.assignment()?);
        }
        Ok(Expr::new(ExprKind::Sequence(items), line))
    }

    fn assignment(&mut self) -> R<Expr> {
        self.deeper()?;
        let lhs = self.ternary()?;
        let op = match self.peek() {
            Some(t) if t.kind == Tok::Op => match t.text {
                "=" => Some(None),
                "+=" => Some(Some(BinOp::Add)),
                "-=" => Some(Some(BinOp::Sub)),
                "*=" => Some(Some(BinOp::Mul)),
                "/=" => Some(Some(BinOp::Div)),
                "%=" => Some(Some(BinOp::Rem)),
                _ => None,
            },
            _ => None,
        };
        let out = match op {
            Some(op) => {
                let line = self.line();
                self.pos += 1;
                // Right-associative, so `a = b = c` groups as `a = (b = c)`.
                let value = self.assignment()?;
                Expr::new(
                    ExprKind::Assign {
                        op,
                        target: Box::new(lhs),
                        value: Box::new(value),
                    },
                    line,
                )
            }
            None => lhs,
        };
        self.depth -= 1;
        Ok(out)
    }

    fn ternary(&mut self) -> R<Expr> {
        let cond = self.binary(1)?;
        if !self.is_op("?") {
            return Ok(cond);
        }
        let line = self.line();
        self.pos += 1;
        let then = self.assignment()?;
        self.expect_op(":")?;
        let els = self.assignment()?;
        Ok(Expr::new(
            ExprKind::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            },
            line,
        ))
    }

    fn binary(&mut self, min_prec: u8) -> R<Expr> {
        self.deeper()?;
        let mut lhs = self.unary()?;
        loop {
            let Some((op, prec)) = self.peek().and_then(|t| binary_op(t)) else {
                // Whatever stopped the loop might be a construct this grammar
                // refuses rather than the end of the expression -- `a & b`
                // would otherwise be reported as a missing semicolon, which
                // names the wrong thing and tallies as the wrong construct.
                self.refuse_unreadable()?;
                break;
            };
            if prec < min_prec {
                break;
            }
            let line = self.line();
            self.pos += 1;
            // Every one of these is left-associative, so the right side binds
            // only tighter operators.
            let rhs = self.binary(prec + 1)?;
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                line,
            );
        }
        self.depth -= 1;
        Ok(lhs)
    }

    fn unary(&mut self) -> R<Expr> {
        self.refuse_unreadable()?;
        let line = self.line();
        let op = match self.peek() {
            Some(t) if t.kind == Tok::Op => match t.text {
                "-" => Some(UnOp::Neg),
                "+" => Some(UnOp::Plus),
                "!" => Some(UnOp::Not),
                "++" => Some(UnOp::PreInc),
                "--" => Some(UnOp::PreDec),
                _ => None,
            },
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            self.deeper()?;
            let expr = self.unary()?;
            self.depth -= 1;
            return Ok(Expr::new(
                ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                },
                line,
            ));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            let line = self.line();
            if self.eat_op(".") {
                let (name, _) = self.ident()?;
                e = Expr::new(
                    ExprKind::Member {
                        base: Box::new(e),
                        name,
                    },
                    line,
                );
            } else if self.eat_op("[") {
                let index = self.expr()?;
                self.expect_op("]")?;
                e = Expr::new(
                    ExprKind::Index {
                        base: Box::new(e),
                        index: Box::new(index),
                    },
                    line,
                );
            } else if self.is_op("++") || self.is_op("--") {
                let inc = self.is_op("++");
                self.pos += 1;
                e = Expr::new(
                    ExprKind::PostIncDec {
                        expr: Box::new(e),
                        inc,
                    },
                    line,
                );
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> R<Expr> {
        self.refuse_unreadable()?;
        let line = self.line();
        if self.eat_op("(") {
            self.deeper()?;
            let e = self.expr()?;
            self.depth -= 1;
            self.expect_op(")")?;
            return Ok(e);
        }
        match self.bump() {
            Some(t) if t.kind == Tok::Number => {
                let (value, is_int) = number_of(t.text);
                Ok(Expr::new(ExprKind::Number { value, is_int }, t.line))
            }
            Some(t) if t.kind == Tok::Ident => {
                if t.text == "true" || t.text == "false" {
                    return Ok(Expr::new(ExprKind::Bool(t.text == "true"), t.line));
                }
                if self.is_op("(") {
                    self.pos += 1;
                    self.deeper()?;
                    let mut args = Vec::new();
                    if !self.is_op(")") {
                        loop {
                            args.push(self.assignment()?);
                            if !self.eat_op(",") {
                                break;
                            }
                        }
                    }
                    self.depth -= 1;
                    self.expect_op(")")?;
                    return Ok(Expr::new(
                        ExprKind::Call {
                            name: t.text.to_owned(),
                            args,
                        },
                        t.line,
                    ));
                }
                Ok(Expr::new(ExprKind::Name(t.text.to_owned()), t.line))
            }
            Some(t) => Err(HlslError::new(
                t.line,
                "syntax",
                format!("`{}` cannot start an expression", t.text),
            )),
            None => Err(HlslError::new(
                line,
                "syntax",
                "the shader ends in the middle of an expression",
            )),
        }
    }
}

/// The binary operator a token spells, and how tightly it binds.
fn binary_op(t: &Token) -> Option<(BinOp, u8)> {
    if t.kind != Tok::Op {
        return None;
    }
    Some(match t.text {
        "||" => (BinOp::Or, 1),
        "&&" => (BinOp::And, 2),
        "==" => (BinOp::Eq, 3),
        "!=" => (BinOp::Ne, 3),
        "<" => (BinOp::Lt, 4),
        "<=" => (BinOp::Le, 4),
        ">" => (BinOp::Gt, 4),
        ">=" => (BinOp::Ge, 4),
        "+" => (BinOp::Add, 5),
        "-" => (BinOp::Sub, 5),
        "*" => (BinOp::Mul, 6),
        "/" => (BinOp::Div, 6),
        "%" => (BinOp::Rem, 6),
        _ => return None,
    })
}

/// A literal's value, and whether HLSL reads it as an integer.
///
/// Integer-ness survives because `1/2` is `0` and `1.0/2` is `0.5`, in HLSL
/// and in WGSL alike.
fn number_of(text: &str) -> (f64, bool) {
    let trimmed = text.trim_end_matches(['f', 'F', 'h', 'H']);
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return (
            u64::from_str_radix(hex, 16).unwrap_or(0) as f64,
            // A suffixed hex literal is still an integer value; nothing in the
            // corpus writes one, and reading it as a float would be worse.
            true,
        );
    }
    let is_int =
        trimmed.len() == text.len() && !trimmed.contains('.') && !trimmed.contains(['e', 'E']);
    (trimmed.parse::<f64>().unwrap_or(0.0), is_int)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shader(src: &str) -> Shader {
        parse(src).unwrap_or_else(|e| panic!("{e}"))
    }

    fn body(src: &str) -> Vec<Stmt> {
        shader(&format!("shader_body {{ {src} }}")).body.unwrap().0
    }

    fn err(src: &str) -> HlslError {
        parse(src).unwrap_err()
    }

    #[test]
    fn a_shader_is_globals_then_functions_then_the_body() {
        // The shape the survey found, and the shape every one of the corpus's
        // shader blocks has.
        let s = shader(
            "sampler sampler_fw_clouds;\n\
             float sigmoid(float t, float c){ return 1 / ( 1 + exp(t*c)); }\n\
             shader_body\n{\n ret = GetPixel(uv);\n}",
        );
        assert_eq!(s.globals.len(), 1);
        assert_eq!(s.globals[0].ty, Ty::Sampler);
        assert_eq!(s.globals[0].name, "sampler_fw_clouds");
        assert_eq!(s.functions.len(), 1);
        assert_eq!(s.functions[0].name, "sigmoid");
        assert_eq!(s.functions[0].params.len(), 2);
        assert_eq!(s.functions[0].ret, Ty::FLOAT);
        assert_eq!(s.body.unwrap().0.len(), 1);
    }

    #[test]
    fn a_missing_body_is_a_state_rather_than_an_error() {
        // MilkDrop refuses such a file; saying *which* file and why is the
        // caller's job, so the parser reports the shape it found.
        let s = shader("float k = 3;");
        assert!(s.body.is_none());
        assert_eq!(s.globals.len(), 1);
    }

    #[test]
    fn precedence_is_cs_and_the_tree_says_so() {
        // `a + b * c` must group as `a + (b * c)`, not `(a + b) * c` -- the
        // one bug in a hand-written parser that changes every picture subtly
        // and no single frame reveals.
        let stmts = body("x = a + b * c;");
        let Stmt::Expr(Expr {
            kind: ExprKind::Assign { value, .. },
            ..
        }) = &stmts[0]
        else {
            panic!("expected an assignment")
        };
        let ExprKind::Binary { op, rhs, .. } = &value.kind else {
            panic!("expected a binary")
        };
        assert_eq!(*op, BinOp::Add);
        assert!(matches!(rhs.kind, ExprKind::Binary { op: BinOp::Mul, .. }));
    }

    #[test]
    fn subtraction_is_left_associative() {
        // `a - b - c` is `(a-b)-c`. Right-associating it is the other silent
        // arithmetic bug of the same family.
        let stmts = body("x = a - b - c;");
        let Stmt::Expr(Expr {
            kind: ExprKind::Assign { value, .. },
            ..
        }) = &stmts[0]
        else {
            panic!()
        };
        let ExprKind::Binary { lhs, rhs, .. } = &value.kind else {
            panic!()
        };
        assert!(matches!(lhs.kind, ExprKind::Binary { .. }), "left-nested");
        assert!(matches!(rhs.kind, ExprKind::Name(_)));
    }

    #[test]
    fn a_comparison_binds_looser_than_arithmetic_and_tighter_than_and() {
        // From the corpus: `-(lum(ret)<sin(time*90))*ret*blur`.
        let stmts = body("x = a < b + 1 && c > d;");
        let Stmt::Expr(Expr {
            kind: ExprKind::Assign { value, .. },
            ..
        }) = &stmts[0]
        else {
            panic!()
        };
        let ExprKind::Binary { op, lhs, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(*op, BinOp::And, "&& is the outermost");
        assert!(matches!(lhs.kind, ExprKind::Binary { op: BinOp::Lt, .. }));
    }

    #[test]
    fn an_integer_literal_stays_an_integer() {
        // `1/2` is 0 in HLSL and in WGSL. A parser that floats every literal
        // silently turns it into 0.5.
        let stmts = body("x = 1; y = 1.0; z = .5; w = 1e3; v = 2.0f;");
        let kinds: Vec<bool> = stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Expr(Expr {
                    kind: ExprKind::Assign { value, .. },
                    ..
                }) => match value.kind {
                    ExprKind::Number { is_int, .. } => Some(is_int),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec![true, false, false, false, false]);
    }

    #[test]
    fn a_type_name_starts_a_declaration_only_when_a_name_follows() {
        // `float2 v = ...` declares; `float2(x,y)` constructs. The whole
        // ambiguity in this grammar, and it is one token wide.
        assert!(matches!(body("float2 v = 0.008;")[0], Stmt::Decl(_)));
        assert!(matches!(body("ret = float2(1,2).xyy;")[0], Stmt::Expr(_)));
    }

    #[test]
    fn one_declaration_can_name_several_things() {
        let Stmt::Decl(vars) = &body("float a = 1, b, c = a*2;")[0] else {
            panic!()
        };
        assert_eq!(vars.len(), 3);
        assert_eq!(vars[1].name, "b");
        assert!(vars[1].init.is_none());
        assert!(vars[2].init.is_some());
    }

    #[test]
    fn the_four_control_flow_shapes_the_corpus_uses_all_parse() {
        // 25.1%, 10.2% and 10.2% of eleven thousand presets.
        assert!(matches!(body("if (a > 1) ret = 0;")[0], Stmt::If { .. }));
        let Stmt::If { els, .. } = &body("if (a) { x=1; } else { x=2; }")[0] else {
            panic!()
        };
        assert!(els.is_some());
        assert!(matches!(
            body("for (int i=0; i<4; i++) ret += i;")[0],
            Stmt::For { .. }
        ));
        assert!(matches!(
            body("while (n < 8) { n++; }")[0],
            Stmt::While { .. }
        ));
        assert!(matches!(
            body("do { n++; } while (n < 8);")[0],
            Stmt::DoWhile { .. }
        ));
    }

    #[test]
    fn a_bare_statement_body_and_a_braced_one_have_the_same_shape() {
        let Stmt::If { then: a, .. } = &body("if (x) y = 1;")[0] else {
            panic!()
        };
        let Stmt::If { then: b, .. } = &body("if (x) { y = 1; }")[0] else {
            panic!()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn swizzles_calls_and_indices_chain_in_the_order_written() {
        let stmts = body("ret = a[i].xy.x;");
        let Stmt::Expr(Expr {
            kind: ExprKind::Assign { value, .. },
            ..
        }) = &stmts[0]
        else {
            panic!()
        };
        // Outermost is the last accessor written.
        let ExprKind::Member { base, name } = &value.kind else {
            panic!()
        };
        assert_eq!(name, "x");
        let ExprKind::Member { base, name } = &base.kind else {
            panic!()
        };
        assert_eq!(name, "xy");
        assert!(matches!(base.kind, ExprKind::Index { .. }));
    }

    #[test]
    fn the_ternary_and_the_increments_parse() {
        assert!(matches!(
            body("x = a > 0 ? b : c;")[0],
            Stmt::Expr(Expr {
                kind: ExprKind::Assign { .. },
                ..
            })
        ));
        let stmts = body("i++; ++j;");
        assert!(matches!(
            stmts[0],
            Stmt::Expr(Expr {
                kind: ExprKind::PostIncDec { inc: true, .. },
                ..
            })
        ));
        assert!(matches!(
            stmts[1],
            Stmt::Expr(Expr {
                kind: ExprKind::Unary {
                    op: UnOp::PreInc,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn the_comma_operator_sequences_but_a_call_still_separates() {
        // `for (...; ...; i++, j++)` is the corpus's usual reason, and the
        // split matters: read the wrong way, `f(a, b)` becomes a one-argument
        // call whose argument happens to be a sequence.
        let Stmt::For { step, .. } = &body("for (int i=0; i<4; i++, j++) x=1;")[0] else {
            panic!()
        };
        let ExprKind::Sequence(items) = &step.as_ref().unwrap().kind else {
            panic!("expected a sequence")
        };
        assert_eq!(items.len(), 2);

        let Stmt::Expr(Expr {
            kind: ExprKind::Assign { value, .. },
            ..
        }) = &body("x = lerp(a, b, t);")[0]
        else {
            panic!()
        };
        let ExprKind::Call { args, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(args.len(), 3, "three arguments, not one sequence");
    }

    #[test]
    fn a_sampler_state_block_is_read_rather_than_skipped() {
        // These carry the address and filter modes a wgpu sampler needs, so a
        // preset asking for a wrapping texture can be given one.
        let s = shader(
            "sampler sampler_grad = sampler_state {
 AddressU = WRAP;
 AddressV = CLAMP;
};
             shader_body { ret = 0; }",
        );
        assert_eq!(
            s.globals[0].sampler_state,
            vec![
                ("AddressU".to_owned(), "WRAP".to_owned()),
                ("AddressV".to_owned(), "CLAMP".to_owned())
            ]
        );
    }

    #[test]
    fn an_initializer_list_is_read_and_may_nest() {
        // `static const float2 offsets[10] = { sin(..), cos(..), ... };` --
        // twenty scalars for ten pairs, which is how the corpus writes it.
        let Stmt::Decl(vars) = &body("float2 o[2] = { 1, 2, {3, 4} };")[0] else {
            panic!()
        };
        let ExprKind::InitList(items) = &vars[0].init.as_ref().unwrap().kind else {
            panic!()
        };
        assert_eq!(items.len(), 3);
        assert!(matches!(items[2].kind, ExprKind::InitList(_)));
    }

    #[test]
    fn anything_after_the_body_is_discarded_the_way_milkdrop_discards_it() {
        // `LoadShaderFromMemory` overwrites from the last `}` to the end, so
        // a preset that signs off after the brace still compiles there.
        let s = shader(
            "shader_body { ret = 1; }

written by martin
END",
        );
        assert!(s.body.is_some());
        assert!(s.globals.is_empty());
    }

    #[test]
    fn compound_assignment_keeps_its_operator() {
        let Stmt::Expr(Expr {
            kind: ExprKind::Assign { op, .. },
            ..
        }) = &body("ret *= 0.94;")[0]
        else {
            panic!()
        };
        assert_eq!(*op, Some(BinOp::Mul));
    }

    #[test]
    fn static_and_const_globals_keep_their_qualifiers() {
        let s = shader("static const float k = 3; shader_body { ret = k; }");
        assert!(s.globals[0].qualifiers.is_static);
        assert!(s.globals[0].qualifiers.is_const);
    }

    #[test]
    fn an_array_declaration_carries_its_length() {
        let Stmt::Decl(vars) = &body("float w[4];")[0] else {
            panic!()
        };
        assert_eq!(vars[0].array, Some(4));
    }

    #[test]
    fn every_construct_this_grammar_refuses_names_itself_and_its_line() {
        // The point of refusing by name: a report can say "347 presets need
        // the preprocessor", which is a plan, rather than "347 failed".
        let e = err("shader_body {\n#include \"x\"\nret = 0;\n}");
        assert_eq!(e.construct, "preprocessor");
        assert_eq!(e.line, 2);

        let e = err("struct S { float a; };");
        assert_eq!(e.construct, "struct");

        let e = err("shader_body { ret = a & b; }");
        assert_eq!(e.construct, "bitwise");

        let e = err("float9 x;");
        assert_eq!(e.construct, "unknown-type");

        let e = err("shader_body { ret = ; }");
        assert_eq!(e.construct, "syntax");
    }

    #[test]
    fn a_runaway_nesting_ends_the_file_rather_than_the_process() {
        // A stranger's file, and decision 51's reasoning one level up.
        let deep = format!(
            "shader_body {{ ret = {}1{}; }}",
            "(".repeat(400),
            ")".repeat(400)
        );
        assert_eq!(err(&deep).construct, "too-deep");
    }

    #[test]
    fn an_unterminated_block_is_reported_rather_than_looping() {
        let e = err("shader_body { ret = 1;");
        assert_eq!(e.construct, "syntax");
    }

    #[test]
    fn a_real_corpus_shader_parses_whole() {
        // `MilkDrop2077 vs martin - riding the wave`, warp pass, verbatim.
        let s = shader(
            "sampler sampler_fw_clouds;\n\
             shader_body\n\
             {// sample previous frame\n\
             ret = tex2D( sampler_main, uv ).xyz;\n\
             float blur =sin(bass*90)*rad;\n\
             ret=0.125*(\n\
             GetPixel(float2(uv.x-0.01*blur,uv.y-0.01*blur)) +\n\
             GetPixel(float2(uv.x+0.01*blur,uv.y+0.01*blur)) +\n\
             GetPixel(float2(uv.x,uv.y-0.01*blur)) +\n\
             GetPixel(float2(uv.x+0.01*blur,uv.y)));\n\
             ret-=-(lum(ret)<sin(time*90))*ret*blur+(lum(ret)>sin(-time*90))*ret*blur;\n\
             }",
        );
        let stmts = s.body.unwrap().0;
        assert_eq!(stmts.len(), 4);
        assert!(matches!(stmts[1], Stmt::Decl(_)), "the `float blur` line");
    }
}
