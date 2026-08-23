//! Turning a preset's shader body into tokens.
//!
//! The first component of decision 54's typed mini-compiler, and the one the
//! corpus survey runs on its own: before a grammar is designed, the tokens a
//! real collection actually contains can be counted, and the grammar drawn
//! from the measurement rather than from a sample of five presets.
//!
//! Three rules shape it.
//!
//! **It never fails.** A preset is a stranger's file, and against a collection
//! of thousands there is no useful sense in which the lexer "rejects" one --
//! a character this build does not know becomes a [`Tok::Unknown`] token
//! carrying the character, and the decision about what to do with it belongs
//! to a caller that can name the preset in a report. The same reasoning as
//! [`Unsupported`](super::super::parse::Unsupported), one layer down.
//!
//! **A `.` is two different things and only context tells them apart.**
//! `texsize.zw` is a swizzle and `ret.y += .11` ends in a number, and the
//! character is the same. The rule is the one every C-family lexer uses: a `.`
//! directly after something that can *end* an expression -- an identifier, a
//! number, a `)` or a `]` -- is an accessor, and anywhere else it begins a
//! number. Getting this backwards is silent: `.11` lexed as accessor-then-11
//! parses as a swizzle with a numeric name and reports as an unknown
//! construct, in a file that is perfectly ordinary.
//!
//! **Comments are dropped but their newlines are not.** Line numbers are how a
//! report tells a preset's author which line this build could not read, and a
//! body with a hundred lines of commented-out shader in it -- which the corpus
//! has -- would otherwise report every position wrong.

/// What a token is, as far as the lexer can tell.
///
/// Deliberately coarse. Keywords are [`Tok::Ident`] here because whether
/// `float2` is a type name, a constructor or a variable someone declared is a
/// question about the grammar, and answering it in the lexer means the answer
/// exists in two places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    /// An identifier or keyword: `uv`, `float3`, `lerp`, `return`, `GetBlur1`.
    Ident,
    /// A numeric literal, including `.5`, `1.`, `1e-5`, `1.0f` and `0x1f`.
    Number,
    /// An operator or a piece of punctuation, longest match, exact spelling.
    Op,
    /// A `#...` preprocessor line, kept whole including the `#`.
    ///
    /// MilkDrop's own shader header is prepended by the engine rather than
    /// written by the preset, so a `#` in a preset body is unusual -- which is
    /// precisely why it is worth counting rather than skipping.
    Directive,
    /// A character this lexer has no rule for. Never an error, always counted.
    Unknown,
}

/// One token, and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: Tok,
    /// The exact source text, borrowed. For [`Tok::Directive`] this is the
    /// whole line; for everything else it is the token itself.
    pub text: &'a str,
    /// 1-based line within the shader body, for reports a person reads.
    pub line: u32,
}

/// Every operator this lexer knows, longest first so that `<=` never lexes as
/// `<` then `=`.
///
/// `++`/`--`/`<<`/`>>` and the bitwise family are here because HLSL has them
/// rather than because a preset was seen using them: the survey's job is to
/// report what the corpus contains, and an operator lexed as two tokens would
/// be reported as two ordinary ones rather than as the unusual thing it is.
const OPERATORS: &[&str] = &[
    "<<=", ">>=", "&&", "||", "==", "!=", "<=", ">=", "++", "--", "+=", "-=", "*=", "/=", "%=",
    "&=", "|=", "^=", "<<", ">>", "+", "-", "*", "/", "%", "=", "<", ">", "!", "?", ":", ",", ";",
    "(", ")", "[", "]", "{", "}", ".", "&", "|", "^", "~",
];

fn at(src: &str, i: usize) -> Option<char> {
    src[i..].chars().next()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Words that are never values, so a `.` after one begins a number.
///
/// Without this, `return .8*x;` reads as `return` with a member called `8`.
/// The corpus has exactly that line, and the failure is quiet: it is a
/// perfectly ordinary file that reports as using an unsupported construct.
const NOT_VALUES: &[&str] = &[
    "return", "else", "do", "case", "if", "while", "for", "in", "out", "inout", "discard",
];

/// True when a token can end an expression, which is what makes a following
/// `.` an accessor rather than the start of a number.
fn ends_an_expression(t: &Token) -> bool {
    match t.kind {
        Tok::Ident => !NOT_VALUES.contains(&t.text),
        Tok::Number => true,
        Tok::Op => t.text == ")" || t.text == "]",
        _ => false,
    }
}

/// Tokenize a shader body. Never fails; see the module note.
pub fn lex(src: &str) -> Vec<Token<'_>> {
    let mut out: Vec<Token<'_>> = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;

    while let Some(c) = at(src, i) {
        // Whitespace, counting lines.
        if c.is_whitespace() {
            if c == '\n' {
                line += 1;
            }
            i += c.len_utf8();
            continue;
        }

        // Comments. Their newlines still count.
        if c == '/' {
            if at(src, i + 1) == Some('/') {
                while let Some(c) = at(src, i) {
                    if c == '\n' {
                        break;
                    }
                    i += c.len_utf8();
                }
                continue;
            }
            if at(src, i + 1) == Some('*') {
                i += 2;
                loop {
                    match at(src, i) {
                        // An unterminated block comment eats the rest of the
                        // body rather than ending the run. A real file in a
                        // real collection will do this eventually.
                        None => break,
                        Some('*') if at(src, i + 1) == Some('/') => {
                            i += 2;
                            break;
                        }
                        Some(c) => {
                            if c == '\n' {
                                line += 1;
                            }
                            i += c.len_utf8();
                        }
                    }
                }
                continue;
            }
        }

        // A preprocessor line, kept whole.
        if c == '#' {
            let start = i;
            while let Some(c) = at(src, i) {
                if c == '\n' {
                    break;
                }
                i += c.len_utf8();
            }
            out.push(Token {
                kind: Tok::Directive,
                text: src[start..i].trim_end(),
                line,
            });
            continue;
        }

        // Identifiers and keywords.
        if is_ident_start(c) {
            let start = i;
            while let Some(c) = at(src, i) {
                if !is_ident_continue(c) {
                    break;
                }
                i += c.len_utf8();
            }
            out.push(Token {
                kind: Tok::Ident,
                text: &src[start..i],
                line,
            });
            continue;
        }

        // Numbers, including the leading-dot form -- but only where a `.`
        // cannot be an accessor. See the module note.
        let dot_starts_a_number = c == '.'
            && at(src, i + 1).is_some_and(|d| d.is_ascii_digit())
            && !out.last().is_some_and(ends_an_expression);
        if c.is_ascii_digit() || dot_starts_a_number {
            let start = i;
            i += lex_number(&src[i..]);
            out.push(Token {
                kind: Tok::Number,
                text: &src[start..i],
                line,
            });
            continue;
        }

        // Operators, longest match.
        if let Some(op) = OPERATORS.iter().find(|op| src[i..].starts_with(**op)) {
            out.push(Token {
                kind: Tok::Op,
                text: &src[i..i + op.len()],
                line,
            });
            i += op.len();
            continue;
        }

        // Anything else, named rather than refused.
        let start = i;
        i += c.len_utf8();
        out.push(Token {
            kind: Tok::Unknown,
            text: &src[start..i],
            line,
        });
    }

    out
}

/// Length in bytes of the numeric literal at the start of `s`.
///
/// Permissive on purpose: `1.0e` and `0x` are malformed and are still lexed as
/// one number token, so the *parser* reports a bad literal with a line number
/// rather than the lexer splitting it into pieces that each look reasonable.
fn lex_number(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0usize;

    if b.len() >= 2 && b[0] == b'0' && (b[1] | 32) == b'x' {
        i = 2;
        while i < b.len() && (b[i] as char).is_ascii_hexdigit() {
            i += 1;
        }
        return i;
    }

    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < b.len() && (b[i] | 32) == b'e' {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    // `f`/`h` suffixes, as in `1.0f`. Not consumed when what follows is more
    // identifier, because `1.0fake` is a number and a name in any reading and
    // the parser should see both.
    if i < b.len()
        && matches!(b[i] | 32, b'f' | b'h')
        && !(i + 1 < b.len() && is_ident_continue(b[i + 1] as char))
    {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Tok, &str)> {
        lex(src).into_iter().map(|t| (t.kind, t.text)).collect()
    }

    fn texts(src: &str) -> Vec<&str> {
        lex(src).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn a_swizzle_and_a_leading_dot_number_are_told_apart_by_what_precedes_them() {
        assert_eq!(
            kinds("texsize.zw"),
            vec![(Tok::Ident, "texsize"), (Tok::Op, "."), (Tok::Ident, "zw")]
        );
        // The corpus writes exactly this: `ret.y += (...)*.11 + 0.034;`
        assert_eq!(
            kinds("a += .11"),
            vec![(Tok::Ident, "a"), (Tok::Op, "+="), (Tok::Number, ".11")]
        );
        // And after a `)`, which is the case a naive "previous is a letter"
        // rule gets wrong.
        assert_eq!(
            kinds("f(x).xy"),
            vec![
                (Tok::Ident, "f"),
                (Tok::Op, "("),
                (Tok::Ident, "x"),
                (Tok::Op, ")"),
                (Tok::Op, "."),
                (Tok::Ident, "xy"),
            ]
        );
    }

    #[test]
    fn a_keyword_is_not_a_value_so_a_dot_after_one_starts_a_number() {
        // `float dist (float2 uvi) {return .8*(uvi.y-.4);}` is in the corpus,
        // and reading `.8` as a member of `return` turns an ordinary file
        // into one that reports an unsupported construct.
        assert_eq!(
            kinds("return .8"),
            vec![(Tok::Ident, "return"), (Tok::Number, ".8")]
        );
        // A name, by contrast, really can be swizzled.
        assert_eq!(
            kinds("uv .8"),
            vec![(Tok::Ident, "uv"), (Tok::Op, "."), (Tok::Number, "8")]
        );
    }

    #[test]
    fn numbers_come_in_every_shape_the_corpus_writes() {
        for src in [
            "1", "1.0", ".5", "1.", "1e-5", "2E+3", "1.0f", "0x1F", "0.0039",
        ] {
            let out = lex(src);
            assert_eq!(out.len(), 1, "{src} should lex as one token");
            assert_eq!(out[0].kind, Tok::Number, "{src}");
            assert_eq!(out[0].text, src, "{src}");
        }
    }

    #[test]
    fn a_suffix_is_only_a_suffix_when_nothing_more_follows_it() {
        // `1.0f` is a float literal; `1.0fake` is a literal and a name, and
        // the parser is the thing that should complain about it.
        assert_eq!(kinds("1.0f"), vec![(Tok::Number, "1.0f")]);
        assert_eq!(
            kinds("1.0fake"),
            vec![(Tok::Number, "1.0"), (Tok::Ident, "fake")]
        );
    }

    #[test]
    fn operators_take_the_longest_match() {
        assert_eq!(
            texts("a<=b&&c!=d"),
            vec!["a", "<=", "b", "&&", "c", "!=", "d"]
        );
        assert_eq!(texts("x*=2"), vec!["x", "*=", "2"]);
    }

    #[test]
    fn comments_are_dropped_and_their_lines_are_kept() {
        let src = "a // one\n/* two\nthree */ b";
        let out = lex(src);
        assert_eq!(texts(src), vec!["a", "b"]);
        assert_eq!(out[0].line, 1);
        // `b` is on the third line, and would report as the first if block
        // comments did not count their newlines.
        assert_eq!(out[1].line, 3);
    }

    #[test]
    fn an_unterminated_block_comment_ends_the_body_rather_than_the_run() {
        assert_eq!(texts("a /* and then nothing"), vec!["a"]);
    }

    #[test]
    fn a_division_is_not_the_start_of_a_comment() {
        assert_eq!(texts("a/b"), vec!["a", "/", "b"]);
    }

    #[test]
    fn a_directive_is_kept_whole() {
        let out = lex("#define X 1\nfloat y;");
        assert_eq!(out[0].kind, Tok::Directive);
        assert_eq!(out[0].text, "#define X 1");
        assert_eq!(out[1].text, "float");
        assert_eq!(out[1].line, 2);
    }

    #[test]
    fn an_unknown_character_is_a_token_rather_than_a_failure() {
        let out = lex("a $ b");
        assert_eq!(out[1].kind, Tok::Unknown);
        assert_eq!(out[1].text, "$");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn a_non_ascii_comment_does_not_split_a_character_in_half() {
        // Preset authors sign their work, and not always in ASCII.
        let src = "// par l\u{e9}o \u{2014} \u{4e2d}\nfloat x;";
        assert_eq!(texts(src), vec!["float", "x", ";"]);
        assert_eq!(lex(src)[0].line, 2);
    }

    #[test]
    fn a_real_corpus_line_lexes_the_way_the_translator_needs_it_to() {
        // From `martin - Gin Tonic on Ice 003`: a `float1` declaration, an
        // implicit truncation by swizzle, and two nested calls.
        let src =
            "float1 dx = ( GetBlur2(uv + float2(d,0)) - GetBlur2(uv-float2(d,0)) ).x*scale.x;";
        let out = lex(src);
        assert!(out.iter().all(|t| t.kind != Tok::Unknown));
        assert_eq!(out[0].text, "float1");
        assert_eq!(out.last().unwrap().text, ";");
        assert_eq!(
            out.iter().filter(|t| t.text == "GetBlur2").count(),
            2,
            "both calls are one identifier each"
        );
    }
}
