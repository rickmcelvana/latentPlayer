//! The preprocessor, which the corpus said to build last and then made the
//! whole of what was left.
//!
//! The survey put `#define` at 3.1% of presets and conditional compilation at
//! 1.3%, so the plan put this after the grammar. That was right, and by the
//! time the grammar had read everything else the preprocessor was **523 of the
//! remaining 528 refusals** -- effectively the entire gap between 95.7% of the
//! corpus and all of it.
//!
//! It is also smaller than it sounds, because of what presets actually use it
//! for. Ranked, from eleven thousand files:
//!
//! ```text
//! #define sat saturate                    an alias for a builtin
//! #if 1                                   a switch the author left on
//! #define MyGet GetPixel                  an alias for a MilkDrop macro
//! #define sampler_pic sampler_prayerwheel  choosing which texture to use
//! #define PI 3.14159265359                a constant
//! ```
//!
//! Object-like macros standing for one name or one number, and conditionals on
//! constants. So this is C's preprocessor in the shape the corpus needs it:
//! `#define`/`#undef`, the `#if`/`#ifdef`/`#ifndef`/`#elif`/`#else`/`#endif`
//! family with a constant-expression evaluator, and function-like macros
//! because they cost little once object-like ones exist.
//!
//! # It works on tokens, not on text
//!
//! C's preprocessor is textual, and doing it textually here would mean lexing
//! twice and re-deciding whether every `.` is a swizzle. Working on the token
//! stream the lexer already produced is one pass, and every token stays
//! borrowed from the original source -- including the ones inside a macro
//! body, since a directive's text is itself a slice of the shader.
//!
//! # A stranger's macro cannot run away with the process
//!
//! `#define A A` is a fixed point, `#define A B` / `#define B A` is a cycle,
//! and `#define A A A` doubles for as long as it is allowed to. So expansion
//! refuses to re-enter a macro already being expanded (C's own rule), bounds
//! its depth, and caps the total token count. Decision 51's reasoning, one
//! layer further out.

use std::collections::{HashMap, HashSet};

use super::lex::{lex, Tok, Token};
use super::parse::HlslError;

/// The most tokens preprocessing may produce before it gives up on the file.
///
/// The corpus's largest shader is a few thousand tokens, so this is roughly a
/// thousandfold headroom and still catches a macro bomb in well under a
/// second.
pub const MAX_TOKENS: usize = 2_000_000;

/// How deep macro expansion may nest.
pub const MAX_EXPANSION: u32 = 64;

/// An item on the expansion work stack: a token to emit, or the end of a
/// macro's expansion, at which point its name may be expanded again.
#[derive(Clone, Copy, Debug)]
enum Item<'a> {
    Tok(Token<'a>),
    Leave(&'a str),
}

#[derive(Clone, Debug)]
struct Macro<'a> {
    /// `None` for an object-like macro, `Some` for `#define F(a, b) ...`.
    params: Option<Vec<&'a str>>,
    body: Vec<Token<'a>>,
}

/// One `#if` group's state.
struct Cond {
    /// Whether this group's tokens are being kept.
    active: bool,
    /// Whether any branch of this group has been taken, so `#elif` and
    /// `#else` know to stay off.
    taken: bool,
    seen_else: bool,
}

/// Run the preprocessor over a lexed shader.
pub fn preprocess<'a>(tokens: Vec<Token<'a>>) -> Result<Vec<Token<'a>>, HlslError> {
    let mut macros: HashMap<&'a str, Macro<'a>> = HashMap::new();
    let mut stack: Vec<Cond> = Vec::new();
    let mut out: Vec<Token<'a>> = Vec::new();
    let mut i = 0usize;

    while i < tokens.len() {
        let t = tokens[i];
        if t.kind == Tok::Directive {
            i += 1;
            directive(t, &mut macros, &mut stack)?;
            continue;
        }
        let live = stack.iter().all(|c| c.active);
        if !live {
            i += 1;
            continue;
        }
        // Not a directive and not skipped: expand it, which may consume the
        // argument list of a function-like macro.
        expand(&tokens, &mut i, &macros, &mut out)?;
        if out.len() > MAX_TOKENS {
            return Err(HlslError {
                line: t.line,
                construct: "macro-runaway",
                detail: format!("macro expansion passed {MAX_TOKENS} tokens"),
            });
        }
    }

    if let Some(_unclosed) = stack.pop() {
        return Err(HlslError {
            line: tokens.last().map_or(1, |t| t.line),
            construct: "preprocessor",
            detail: "a `#if` is never closed by an `#endif`".into(),
        });
    }
    Ok(out)
}

/// Read one `#...` line and act on it.
fn directive<'a>(
    t: Token<'a>,
    macros: &mut HashMap<&'a str, Macro<'a>>,
    stack: &mut Vec<Cond>,
) -> Result<(), HlslError> {
    let body = t.text.strip_prefix('#').unwrap_or(t.text).trim_start();
    let (word, rest) = split_word(body);
    let live = stack.iter().all(|c| c.active);
    let err = |construct: &'static str, detail: String| HlslError {
        line: t.line,
        construct,
        detail,
    };

    match word {
        // A bare `#`, which three files in the corpus contain. C calls this a
        // null directive and ignores it.
        "" => Ok(()),
        "endif" => {
            if stack.pop().is_none() {
                return Err(err(
                    "preprocessor",
                    "an `#endif` with no `#if` before it".into(),
                ));
            }
            Ok(())
        }
        "else" => match stack.last_mut() {
            Some(c) if !c.seen_else => {
                c.seen_else = true;
                c.active = !c.taken;
                c.taken = true;
                Ok(())
            }
            Some(_) => Err(err("preprocessor", "a second `#else` in one group".into())),
            None => Err(err(
                "preprocessor",
                "an `#else` with no `#if` before it".into(),
            )),
        },
        "elif" => {
            // Evaluated only when an outer group is live and no branch here
            // has been taken -- otherwise its condition may name macros that
            // were never defined, which is exactly why it is guarded.
            let outer = stack.len() > 1 && !stack[..stack.len() - 1].iter().all(|c| c.active);
            let Some(c) = stack.last_mut() else {
                return Err(err(
                    "preprocessor",
                    "an `#elif` with no `#if` before it".into(),
                ));
            };
            if c.taken || outer {
                c.active = false;
                return Ok(());
            }
            let value = eval(rest, t.line, macros)?;
            c.active = value != 0;
            c.taken |= c.active;
            Ok(())
        }
        "if" | "ifdef" | "ifndef" => {
            if !live {
                // Inside a dead branch, only the nesting matters.
                stack.push(Cond {
                    active: false,
                    taken: true,
                    seen_else: false,
                });
                return Ok(());
            }
            let value = match word {
                "ifdef" => i64::from(macros.contains_key(split_word(rest).0)),
                "ifndef" => i64::from(!macros.contains_key(split_word(rest).0)),
                _ => eval(rest, t.line, macros)?,
            };
            stack.push(Cond {
                active: value != 0,
                taken: value != 0,
                seen_else: false,
            });
            Ok(())
        }
        "define" if live => {
            let (name, after) = split_word(rest);
            if name.is_empty() {
                return Err(err("preprocessor", "a `#define` with no name".into()));
            }
            // Function-like only when the `(` touches the name, which is the
            // rule C uses and the only thing separating `#define F(x) x*2`
            // from `#define K (1+2)`.
            let (params, body_text) = if after.starts_with('(') {
                let close = after.find(')').ok_or_else(|| {
                    err(
                        "preprocessor",
                        "a function-like `#define` with no closing `)`".into(),
                    )
                })?;
                let names: Vec<&str> = after[1..close]
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                (Some(names), &after[close + 1..])
            } else {
                (None, after)
            };
            let mut body = lex(body_text);
            for tok in &mut body {
                tok.line = t.line;
            }
            macros.insert(name, Macro { params, body });
            Ok(())
        }
        "undef" if live => {
            macros.remove(split_word(rest).0);
            Ok(())
        }
        "define" | "undef" => Ok(()),
        // `#include`, `#pragma`, `#error` and anything else. Named rather
        // than ignored: a preset needing an include is a preset needing a
        // file this build has no way to find.
        other if live => Err(err(
            "preprocessor",
            format!("`#{other}` is not a directive this build handles"),
        )),
        _ => Ok(()),
    }
}

/// Copy one token to the output, expanding it if it names a macro.
fn expand<'a>(
    tokens: &[Token<'a>],
    i: &mut usize,
    macros: &HashMap<&'a str, Macro<'a>>,
    out: &mut Vec<Token<'a>>,
) -> Result<(), HlslError> {
    // A work stack rather than recursion over the input, so a macro whose
    // body names another macro is handled by the same loop.
    //
    // `Leave` is what makes the "currently expanding" set a *region* rather
    // than a one-shot: it is pushed under a macro's body, so the name comes
    // back into play the moment that body is finished. Without it, `SQ(A)`
    // with `#define SQ(x) x*x` would expand the first `A` and leave the
    // second one standing -- and a macro bomb would look like a fixed point
    // instead of the runaway it is.
    let mut pending: Vec<Item<'a>> = vec![Item::Tok(tokens[*i])];
    *i += 1;
    let mut active: HashSet<&str> = HashSet::new();
    let mut depth = 0u32;

    while let Some(item) = pending.pop() {
        let t = match item {
            Item::Tok(t) => t,
            Item::Leave(name) => {
                active.remove(name);
                depth = depth.saturating_sub(1);
                continue;
            }
        };
        let name = t.text;
        let Some(mac) = (t.kind == Tok::Ident)
            .then(|| macros.get(name))
            .flatten()
            .filter(|_| !active.contains(name))
        else {
            out.push(t);
            continue;
        };
        depth += 1;
        if depth > MAX_EXPANSION {
            return Err(HlslError {
                line: t.line,
                construct: "macro-runaway",
                detail: format!("`{name}` expands more than {MAX_EXPANSION} deep"),
            });
        }

        let body: Vec<Token<'a>> = match &mac.params {
            None => mac.body.clone(),
            Some(params) => {
                // A function-like macro without a following `(` is just a
                // name, which is C's rule and keeps `#define F(x) ...` from
                // eating an unrelated `F`.
                let next_is_paren = match pending.last() {
                    Some(Item::Tok(n)) => n.kind == Tok::Op && n.text == "(",
                    Some(Item::Leave(_)) => false,
                    None => tokens
                        .get(*i)
                        .is_some_and(|n| n.kind == Tok::Op && n.text == "("),
                };
                if !next_is_paren {
                    out.push(t);
                    continue;
                }
                let args = arguments(tokens, i, &mut pending, t.line)?;
                substitute(&mac.body, params, &args)
            }
        };

        // `active` keeps a macro from re-entering itself, which is what makes
        // `#define A A` a fixed point rather than a hang.
        active.insert(name);
        pending.push(Item::Leave(name));
        for tok in body.into_iter().rev() {
            pending.push(Item::Tok(tok));
        }
        if out.len() + pending.len() > MAX_TOKENS {
            return Err(HlslError {
                line: t.line,
                construct: "macro-runaway",
                detail: format!("macro expansion passed {MAX_TOKENS} tokens"),
            });
        }
    }
    Ok(())
}

/// Read `( a, b )` for a function-like macro, from the pending stack first and
/// the input after it.
fn arguments<'a>(
    tokens: &[Token<'a>],
    i: &mut usize,
    pending: &mut Vec<Item<'a>>,
    line: u32,
) -> Result<Vec<Vec<Token<'a>>>, HlslError> {
    let mut next = || -> Option<Token<'a>> {
        while let Some(item) = pending.pop() {
            match item {
                Item::Tok(t) => return Some(t),
                // A macro whose expansion ends exactly at an argument list
                // boundary: the region closes here rather than being skipped.
                Item::Leave(_) => continue,
            }
        }
        let t = tokens.get(*i).copied();
        if t.is_some() {
            *i += 1;
        }
        t
    };
    let open = next();
    debug_assert!(open.is_some_and(|t| t.text == "("));

    let mut args: Vec<Vec<Token<'a>>> = vec![Vec::new()];
    let mut depth = 1i32;
    loop {
        let Some(t) = next() else {
            return Err(HlslError {
                line,
                construct: "preprocessor",
                detail: "a macro's argument list is never closed".into(),
            });
        };
        if t.kind == Tok::Op {
            match t.text {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                "," if depth == 1 => {
                    args.push(Vec::new());
                    continue;
                }
                _ => {}
            }
        }
        args.last_mut().expect("always one argument").push(t);
    }
    if args.len() == 1 && args[0].is_empty() {
        args.clear();
    }
    Ok(args)
}

/// Replace a macro body's parameter names with the arguments given.
fn substitute<'a>(body: &[Token<'a>], params: &[&str], args: &[Vec<Token<'a>>]) -> Vec<Token<'a>> {
    let mut out = Vec::with_capacity(body.len());
    for t in body {
        match params.iter().position(|p| *p == t.text) {
            Some(n) if t.kind == Tok::Ident => {
                // A parameter with no argument expands to nothing, which is
                // what C does for `F()`.
                if let Some(arg) = args.get(n) {
                    out.extend(arg.iter().copied());
                }
            }
            _ => out.push(*t),
        }
    }
    out
}

/// Evaluate a `#if` condition.
///
/// Integers only, which is all C promises and more than the corpus uses: the
/// dominant condition in eleven thousand presets is `1`.
fn eval(text: &str, line: u32, macros: &HashMap<&str, Macro<'_>>) -> Result<i64, HlslError> {
    let mut toks = lex(text);
    // `defined X` and `defined(X)` are resolved before macros expand, because
    // the whole point is to ask about a name rather than to use it.
    let mut resolved: Vec<Cell> = Vec::new();
    let mut k = 0usize;
    while k < toks.len() {
        let t = toks[k];
        if t.kind == Tok::Ident && t.text == "defined" {
            k += 1;
            let paren = toks.get(k).is_some_and(|n| n.text == "(");
            if paren {
                k += 1;
            }
            let name = toks.get(k).map_or("", |n| n.text);
            k += 1;
            if paren && toks.get(k).is_some_and(|n| n.text == ")") {
                k += 1;
            }
            resolved.push(Cell::Num(i64::from(macros.contains_key(name))));
            continue;
        }
        if t.kind == Tok::Ident {
            // An undefined name is 0, and a defined one stands for its body
            // if that body is a plain number. Anything else is 0 too, which
            // matches C closely enough for conditions on constants.
            let value = macros
                .get(t.text)
                .and_then(|m| match m.body.as_slice() {
                    [one] if one.kind == Tok::Number => one.text.parse::<i64>().ok(),
                    _ => None,
                })
                .unwrap_or(0);
            resolved.push(Cell::Num(value));
            k += 1;
            continue;
        }
        resolved.push(match t.kind {
            Tok::Number => Cell::Num(
                t.text
                    .parse::<i64>()
                    .unwrap_or_else(|_| t.text.parse::<f64>().map(|f| f as i64).unwrap_or(0)),
            ),
            _ => Cell::Op(t.text),
        });
        k += 1;
    }
    toks.clear();

    let mut at = 0usize;
    let value = cond_expr(&resolved, &mut at, 0).ok_or(HlslError {
        line,
        construct: "preprocessor",
        detail: format!(
            "`#if {}` is not a constant this build can evaluate",
            text.trim()
        ),
    })?;
    Ok(value)
}

/// One item of a `#if` condition after names have been resolved.
#[derive(Clone, Copy, Debug)]
enum Cell<'a> {
    Num(i64),
    Op(&'a str),
}

/// Precedence-climbing over the integer operators, mirroring the shader
/// grammar's own so the two cannot disagree about `a + b * c`.
fn cond_expr(cells: &[Cell], at: &mut usize, min_prec: u8) -> Option<i64> {
    let mut lhs = cond_unary(cells, at)?;
    while let Some(Cell::Op(op)) = cells.get(*at) {
        let prec = match *op {
            "||" => 1,
            "&&" => 2,
            "==" | "!=" => 3,
            "<" | "<=" | ">" | ">=" => 4,
            "+" | "-" => 5,
            "*" | "/" | "%" => 6,
            _ => break,
        };
        if prec < min_prec {
            break;
        }
        let op = *op;
        *at += 1;
        let rhs = cond_expr(cells, at, prec + 1)?;
        lhs = match op {
            "||" => i64::from(lhs != 0 || rhs != 0),
            "&&" => i64::from(lhs != 0 && rhs != 0),
            "==" => i64::from(lhs == rhs),
            "!=" => i64::from(lhs != rhs),
            "<" => i64::from(lhs < rhs),
            "<=" => i64::from(lhs <= rhs),
            ">" => i64::from(lhs > rhs),
            ">=" => i64::from(lhs >= rhs),
            "+" => lhs.wrapping_add(rhs),
            "-" => lhs.wrapping_sub(rhs),
            "*" => lhs.wrapping_mul(rhs),
            // A constant expression that divides by zero is a broken
            // condition, not a crash.
            "/" => lhs.checked_div(rhs).unwrap_or(0),
            "%" => lhs.checked_rem(rhs).unwrap_or(0),
            _ => return None,
        };
    }
    Some(lhs)
}

fn cond_unary(cells: &[Cell], at: &mut usize) -> Option<i64> {
    match cells.get(*at)? {
        Cell::Num(n) => {
            *at += 1;
            Some(*n)
        }
        Cell::Op("(") => {
            *at += 1;
            let v = cond_expr(cells, at, 0)?;
            match cells.get(*at) {
                Some(Cell::Op(")")) => {
                    *at += 1;
                    Some(v)
                }
                _ => None,
            }
        }
        Cell::Op("!") => {
            *at += 1;
            Some(i64::from(cond_unary(cells, at)? == 0))
        }
        Cell::Op("-") => {
            *at += 1;
            Some(-cond_unary(cells, at)?)
        }
        Cell::Op("+") => {
            *at += 1;
            cond_unary(cells, at)
        }
        Cell::Op(_) => None,
    }
}

/// The first whitespace-delimited word, and everything after it.
fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    (&s[..end], &s[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> String {
        let out = preprocess(lex(src)).unwrap_or_else(|e| panic!("{e}"));
        out.iter().map(|t| t.text).collect::<Vec<_>>().join(" ")
    }

    fn fails(src: &str) -> HlslError {
        preprocess(lex(src)).unwrap_err()
    }

    #[test]
    fn an_object_like_define_is_the_corpuss_common_case() {
        // `#define sat saturate` is the single commonest directive in eleven
        // thousand presets.
        assert_eq!(
            run("#define sat saturate\nret = sat(x);"),
            "ret = saturate ( x ) ;"
        );
        assert_eq!(run("#define PI 3.14159\nret = PI;"), "ret = 3.14159 ;");
    }

    #[test]
    fn a_defines_trailing_comment_is_not_part_of_its_body() {
        // `#define MyGet GetPixel //GetBlur1` -- 55 shader bodies, and the
        // comment would otherwise expand into the code.
        assert_eq!(
            run("#define MyGet GetPixel //GetBlur1\nMyGet(uv)"),
            "GetPixel ( uv )"
        );
    }

    #[test]
    fn a_function_like_macro_substitutes_its_arguments() {
        assert_eq!(
            run("#define SQ(x) ((x)*(x))\nret = SQ(a+1);"),
            "ret = ( ( a + 1 ) * ( a + 1 ) ) ;"
        );
        // Nested parentheses in an argument do not end it early.
        assert_eq!(run("#define F(a,b) a+b\nF(g(1,2),3)"), "g ( 1 , 2 ) + 3");
    }

    #[test]
    fn a_function_like_macro_without_a_call_is_just_a_name() {
        // C's rule, and it keeps `#define F(x) ...` from eating an unrelated
        // `F` used as a variable.
        assert_eq!(run("#define F(x) x\nfloat F;"), "float F ;");
    }

    #[test]
    fn only_a_paren_touching_the_name_makes_it_function_like() {
        // `#define K (1+2)` is an object-like macro whose body is a
        // parenthesised expression, not a macro of one parameter.
        assert_eq!(run("#define K (1+2)\nret = K;"), "ret = ( 1 + 2 ) ;");
    }

    #[test]
    fn if_one_keeps_its_branch_and_drops_the_other() {
        // 141 shader bodies say exactly `#if 1`.
        assert_eq!(run("#if 1\na\n#else\nb\n#endif"), "a");
        assert_eq!(run("#if 0\na\n#else\nb\n#endif"), "b");
    }

    #[test]
    fn a_condition_can_be_an_expression_over_defined_names() {
        assert_eq!(run("#define N 3\n#if N > 2 && !0\na\n#endif"), "a");
        assert_eq!(run("#define N 1\n#if N*4 == 4\na\n#else\nb\n#endif"), "a");
        assert_eq!(run("#ifdef N\na\n#else\nb\n#endif"), "b");
        assert_eq!(run("#define N\n#ifdef N\na\n#endif"), "a");
        assert_eq!(run("#if defined(X)\na\n#else\nb\n#endif"), "b");
    }

    #[test]
    fn elif_chains_take_exactly_one_branch() {
        assert_eq!(
            run("#if 0\na\n#elif 1\nb\n#elif 1\nc\n#else\nd\n#endif"),
            "b"
        );
        assert_eq!(run("#if 0\na\n#elif 0\nb\n#else\nd\n#endif"), "d");
    }

    #[test]
    fn a_dead_branch_does_not_define_anything_or_evaluate_anything() {
        // The condition inside a dead group may name things that do not
        // exist; evaluating it anyway is how a preprocessor invents errors.
        assert_eq!(
            run("#if 0\n#define X 1\n#if UNKNOWN(\n#endif\n#endif\nX"),
            "X"
        );
    }

    #[test]
    fn undef_takes_a_name_back() {
        assert_eq!(run("#define A 1\n#undef A\nA"), "A");
    }

    #[test]
    fn a_bare_hash_is_ignored_the_way_c_ignores_it() {
        // Three files in the corpus contain one.
        assert_eq!(run("#\nret = 1;"), "ret = 1 ;");
    }

    #[test]
    fn a_macro_used_twice_in_one_expansion_expands_twice() {
        // The bug the bomb test found: with a flat "currently expanding" set,
        // the first `A` expands and the second is left standing, silently.
        assert_eq!(
            run("#define A 7
#define SQ(x) x*x
SQ(A)"),
            "7 * 7"
        );
    }

    #[test]
    fn a_self_referential_macro_is_a_fixed_point_rather_than_a_hang() {
        // C's rule: a macro may not re-enter itself.
        assert_eq!(run("#define A A\nA"), "A");
        assert_eq!(run("#define A B\n#define B A\nA"), "A");
        assert_eq!(run("#define A A+1\nA"), "A + 1");
    }

    #[test]
    fn a_macro_bomb_ends_the_file_rather_than_the_process() {
        let bomb = "#define A0 x\n".to_owned()
            + &(1..24)
                .map(|n| format!("#define A{n} A{} A{}\n", n - 1, n - 1))
                .collect::<String>()
            + "A23";
        let e = fails(&bomb);
        assert_eq!(e.construct, "macro-runaway");
    }

    #[test]
    fn the_directives_this_build_does_not_handle_say_so() {
        assert_eq!(fails("#include \"x\"\n").construct, "preprocessor");
        assert_eq!(
            fails("#if 1\n").detail,
            "a `#if` is never closed by an `#endif`"
        );
        assert_eq!(
            fails("#endif\n").detail,
            "an `#endif` with no `#if` before it"
        );
    }

    #[test]
    fn a_line_number_survives_expansion_for_the_sake_of_reports() {
        let out = preprocess(lex("#define A 1\n\n\nret = A;")).unwrap();
        // `A`'s replacement carries the `#define`'s line, and everything
        // around it carries its own -- either way a report can point at a
        // line the author would recognise.
        assert_eq!(out.first().unwrap().line, 4);
    }
}
