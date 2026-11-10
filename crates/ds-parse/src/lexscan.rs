//! Lexical pre-scan: a single iterative pass that never panics on arbitrary
//! input. It runs before the pest parse for two jobs:
//!
//! 1. a nesting guard — pest's recursive descent (and this crate's own conversion pass) would
//!    overflow the stack on adversarial nesting, which aborts rather than panics, so [`MAX_NEST`]
//!    rejects it as a diagnostic;
//! 2. precise two-span diagnostics for the lexical class where pest's furthest-failure position is
//!    useless: unterminated strings and interpolation holes, raw-string hash counting, unbalanced
//!    or mismatched bracket pairs, stray semicolons, and characters valid in no token.

// Span offsets are u32 by contract (Files::add_source rejects longer
// sources), so narrowing casts from usize are total.
#![allow(clippy::cast_possible_truncation)]

use ds_diag::{Diagnostic, ErrorCode, FileId, Span, Suggestion};

/// Maximum bracket nesting depth. One nesting level costs roughly 12 KiB of
/// pest stack (the precedence chain is ~19 rules deep per level), so 40
/// levels stays well inside a 1 MiB main-thread stack with margin for the
/// converter pass.
pub(crate) const MAX_NEST: u32 = 40;

/// What the scanner is inside of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Frame {
    /// An expression bracket `( [ {` opened at a byte offset.
    Open(char, u32),
    /// A `${` interpolation hole opened inside a string.
    Hole(u32),
    /// A normal string opened at the `"` byte offset.
    Str(u32),
    /// A char or byte literal opened at the quote offset.
    Char(u32),
    /// A raw string with the given hash count, opened at the `r` offset.
    Raw(usize, u32),
    /// A line comment.
    LineComment,
    /// A block comment.
    BlockComment,
}

/// Outcome of pushing an opening bracket.
enum Pushed {
    Ok,
    TooDeep(Diagnostic),
}

/// Scans `src`; returns diagnostics if anything is lexically broken.
pub(crate) fn scan(file: FileId, src: &str) -> Option<Vec<Diagnostic>> {
    let b = src.as_bytes();
    let mut diags = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;

    while i < b.len() {
        let prev = if i > 0 { Some(b[i - 1]) } else { None };
        let at = |n: usize| -> u8 { b.get(i + n).copied().unwrap_or(0) };
        let ident_ish = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let top = stack.last().copied();
        match top {
            Some(Frame::LineComment) => {
                if b[i] == b'\n' {
                    stack.pop();
                }
                i += 1;
            },
            Some(Frame::BlockComment) => {
                if b[i] == b'*' && at(1) == b'/' {
                    stack.pop();
                    i += 2;
                } else {
                    i += 1;
                }
            },
            Some(Frame::Str(open)) => match b[i] {
                b'\n' => {
                    diags.push(unterminated_string(file, open, i as u32, None));
                    stack.pop();
                },
                b'\r' if at(1) != b'\n' => {
                    diags.push(lone_cr(file, i));
                    i += 1;
                },
                b'\\' => {
                    if let Some(step) = escape_len(b, i) {
                        i += step;
                    } else {
                        diags.push(
                            Diagnostic::error(
                                ErrorCode::InvalidEscape,
                                "invalid escape sequence in string literal",
                            )
                            .with_primary(
                                Span::new(file, i as u32, (i + 2).min(b.len()) as u32),
                                "invalid escape",
                            ),
                        );
                        i += 1;
                    }
                },
                b'"' => {
                    stack.pop();
                    i += 1;
                },
                b'$' => {
                    if at(1) == b'{' {
                        stack.push(Frame::Hole(i as u32));
                        i += 2;
                    } else if ident_ish(at(1)) && !at(1).is_ascii_digit() {
                        i += 2;
                        while i < b.len() && ident_ish(b[i]) {
                            i += 1;
                        }
                    } else {
                        diags.push(
                            Diagnostic::error(
                                ErrorCode::InvalidInterpolation,
                                "interpolation hole must be `$name` or `${expr}`",
                            )
                            .with_primary(
                                Span::new(file, i as u32, i as u32 + 1),
                                "`$` starts a hole",
                            ),
                        );
                        i += 1;
                    }
                },
                _ => i += 1,
            },
            Some(Frame::Char(open)) => match b[i] {
                b'\n' => {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::UnterminatedString,
                            "unterminated character literal",
                        )
                        .with_primary(Span::new(file, i as u32, i as u32 + 1), "line ended")
                        .with_secondary(Span::new(file, open, open + 1), "literal starts here"),
                    );
                    stack.pop();
                },
                b'\\' => {
                    if let Some(step) = escape_len(b, i) {
                        i += step;
                    } else {
                        diags.push(
                            Diagnostic::error(
                                ErrorCode::InvalidEscape,
                                "invalid escape sequence in character literal",
                            )
                            .with_primary(
                                Span::new(file, i as u32, (i + 2).min(b.len()) as u32),
                                "invalid escape",
                            ),
                        );
                        i += 1;
                    }
                },
                b'\r' if at(1) != b'\n' => {
                    diags.push(lone_cr(file, i));
                    i += 1;
                },
                b'\'' => {
                    stack.pop();
                    i += 1;
                },
                _ => i += 1,
            },
            Some(Frame::Raw(hashes, _open)) => {
                if b[i] == b'"' {
                    let mut k = 0;
                    while i + 1 + k < b.len() && b[i + 1 + k] == b'#' {
                        k += 1;
                    }
                    if k == hashes {
                        stack.pop();
                    }
                    i += 1 + k;
                } else {
                    i += 1;
                }
            },
            Some(Frame::Hole(_)) => match b[i] {
                b'"' => {
                    stack.push(Frame::Str(i as u32));
                    i += 1;
                },
                b'\'' => {
                    stack.push(Frame::Char(i as u32));
                    i += 1;
                },
                b'r' if boundary_prev(prev) && (at(1) == b'"' || at(1) == b'#') => {
                    let mut k = 0;
                    while at(1 + k) == b'#' {
                        k += 1;
                    }
                    stack.push(Frame::Raw(k, i as u32));
                    // Skip `r`, the hashes, and the opening quote.
                    i += 1 + k + 1;
                },
                b'b' if boundary_prev(prev) && at(1) == b'\'' => {
                    stack.push(Frame::Char(i as u32 + 1));
                    // Skip `b` and the opening quote.
                    i += 2;
                },
                b'/' if at(1) == b'/' => {
                    stack.push(Frame::LineComment);
                    i += 2;
                },
                b'/' if at(1) == b'*' => {
                    stack.push(Frame::BlockComment);
                    i += 2;
                },
                b'{' | b'(' | b'[' => match push_open(file, &mut stack, b[i], i) {
                    Pushed::Ok => i += 1,
                    Pushed::TooDeep(d) => return Some(vec![d]),
                },
                b'}' => {
                    // Top of stack is the hole (an open `{` would have matched
                    // the Open arm below), so this closes the hole.
                    stack.pop();
                    i += 1;
                },
                b')' | b']' => {
                    report_close(file, &mut stack, b[i], i, &mut diags);
                    i += 1;
                },
                b';' => {
                    diags.push(stray_semicolon(file, i));
                    i += 1;
                },
                b'$' | b'`' | b'\\' => {
                    diags.push(invalid_char(file, i));
                    i += 1;
                },
                b'\r' if at(1) != b'\n' => {
                    diags.push(lone_cr(file, i));
                    i += 1;
                },
                _ => i += 1,
            },
            None | Some(Frame::Open(..)) => match b[i] {
                b'/' if at(1) == b'/' => {
                    stack.push(Frame::LineComment);
                    i += 2;
                },
                b'/' if at(1) == b'*' => {
                    stack.push(Frame::BlockComment);
                    i += 2;
                },
                b'"' => {
                    stack.push(Frame::Str(i as u32));
                    i += 1;
                },
                b'\'' => {
                    stack.push(Frame::Char(i as u32));
                    i += 1;
                },
                b'r' if boundary_prev(prev) && (at(1) == b'"' || at(1) == b'#') => {
                    let mut k = 0;
                    while at(1 + k) == b'#' {
                        k += 1;
                    }
                    stack.push(Frame::Raw(k, i as u32));
                    // Skip `r`, the hashes, and the opening quote.
                    i += 1 + k + 1;
                },
                b'b' if boundary_prev(prev) && at(1) == b'\'' => {
                    stack.push(Frame::Char(i as u32 + 1));
                    // Skip `b` and the opening quote.
                    i += 2;
                },
                b'{' | b'(' | b'[' => match push_open(file, &mut stack, b[i], i) {
                    Pushed::Ok => i += 1,
                    Pushed::TooDeep(d) => return Some(vec![d]),
                },
                b')' | b']' | b'}' => {
                    report_close(file, &mut stack, b[i], i, &mut diags);
                    i += 1;
                },
                b';' => {
                    diags.push(stray_semicolon(file, i));
                    i += 1;
                },
                b'$' | b'`' | b'\\' => {
                    diags.push(invalid_char(file, i));
                    i += 1;
                },
                b'#' if stack.is_empty() && at(1) == b'#' && matches!(prev, None | Some(b'\n')) => {
                    // A line-initial `##` opens a separator line (G3); the
                    // rest of the line is separator text, invisible here.
                    i = match b[i..].iter().position(|&c| c == b'\n') {
                        Some(p) => i + p + 1,
                        None => b.len(),
                    };
                },
                b'#' if at(1) != b'[' => {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::InvalidChar,
                            "`#` only starts an attribute (`#[...]`) or a line-initial separator \
                             (`##`)",
                        )
                        .with_primary(Span::new(file, i as u32, i as u32 + 1), "invalid character"),
                    );
                    i += 1;
                },
                b'\r' if at(1) != b'\n' => {
                    diags.push(lone_cr(file, i));
                    i += 1;
                },
                _ => i += 1,
            },
        }
    }

    report_unclosed(file, src, &stack, &mut diags);
    (!diags.is_empty()).then_some(diags)
}

/// Reports every frame still open at EOF. The innermost terminator frame
/// (string, hole, comment) wins; otherwise every unclosed opener is
/// reported. Never panics: impossible frames fall through silently.
fn report_unclosed(file: FileId, src: &str, stack: &[Frame], diags: &mut Vec<Diagnostic>) {
    let eof = src.len() as u32;
    if let Some(frame) = stack
        .iter()
        .rev()
        .find(|f| !matches!(f, Frame::Open(..) | Frame::LineComment))
    {
        match *frame {
            Frame::Str(open) | Frame::Raw(_, open) => {
                diags.push(unterminated_string(file, open, eof, None));
            },
            Frame::Char(open) => diags.push(
                Diagnostic::error(
                    ErrorCode::UnterminatedString,
                    "unterminated character literal",
                )
                .with_primary(Span::new(file, eof, eof), "end of input")
                .with_secondary(Span::new(file, open, open + 1), "literal starts here"),
            ),
            Frame::BlockComment => diags.push(
                Diagnostic::error(ErrorCode::UnexpectedEof, "unterminated block comment")
                    .with_primary(Span::new(file, eof, eof), "end of input"),
            ),
            Frame::Hole(hole_open) => {
                let str_open = stack
                    .iter()
                    .rev()
                    .find_map(|f| match f {
                        Frame::Str(o) => Some(*o),
                        _ => None,
                    })
                    .unwrap_or(hole_open);
                diags.push(unterminated_string(file, str_open, eof, Some(hole_open)));
            },
            Frame::Open(..) | Frame::LineComment => {},
        }
    } else {
        for &frame in stack.iter().rev() {
            if let Frame::Open(ch, open) = frame {
                diags.push(
                    Diagnostic::error(ErrorCode::UnbalancedDelimiter, format!("unclosed `{ch}`"))
                        .with_primary(
                            Span::new(file, open, open + 1),
                            format!("`{ch}` opens here"),
                        )
                        .with_secondary(Span::new(file, eof, eof), "input ended here"),
                );
            }
        }
    }
}

fn boundary_prev(prev: Option<u8>) -> bool {
    !matches!(prev, Some(c) if c.is_ascii_alphanumeric() || c == b'_')
}

/// Length of a valid escape sequence starting at `b[i] == '\\'`, if any.
fn escape_len(b: &[u8], i: usize) -> Option<usize> {
    let simple = |c: u8| matches!(c, b'n' | b't' | b'r' | b'0' | b'\\' | b'"' | b'$' | b'\'');
    match b.get(i + 1) {
        Some(&c) if simple(c) => Some(2),
        Some(b'u') if b.get(i + 2) == Some(&b'{') => {
            let mut j = i + 3;
            let mut n = 0;
            while let Some(&c) = b.get(j) {
                if c.is_ascii_hexdigit() {
                    n += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            (n > 0 && n <= 6 && b.get(j) == Some(&b'}')).then_some(j + 1 - i)
        },
        _ => None,
    }
}

/// Pushes an opening bracket, guarding depth.
fn push_open(file: FileId, stack: &mut Vec<Frame>, ch: u8, i: usize) -> Pushed {
    let opens = stack
        .iter()
        .filter(|f| matches!(f, Frame::Open(..)))
        .count() as u32;
    if opens >= MAX_NEST {
        return Pushed::TooDeep(
            Diagnostic::error(
                ErrorCode::NestingTooDeep,
                format!("bracket nesting exceeds the parser limit of {MAX_NEST}"),
            )
            .with_primary(
                Span::new(file, i as u32, i as u32 + 1),
                format!("`{}` nested here", ch as char),
            ),
        );
    }
    stack.push(Frame::Open(ch as char, i as u32));
    Pushed::Ok
}

/// Reports a closing bracket against the stack top.
fn report_close(
    file: FileId,
    stack: &mut Vec<Frame>,
    ch: u8,
    i: usize,
    diags: &mut Vec<Diagnostic>,
) {
    let partner = match ch {
        b')' => '(',
        b']' => '[',
        _ => '{',
    };
    match stack.last().copied() {
        Some(Frame::Open(top, _)) if top == partner => {
            stack.pop();
        },
        Some(Frame::Open(top, open)) => {
            diags.push(
                Diagnostic::error(
                    ErrorCode::UnbalancedDelimiter,
                    format!("mismatched pairing: `{top}` closed by `{}`", ch as char),
                )
                .with_primary(
                    Span::new(file, i as u32, i as u32 + 1),
                    format!("unexpected `{}`", ch as char),
                )
                .with_secondary(
                    Span::new(file, open, open + 1),
                    format!("`{top}` opens here"),
                )
                .with_suggestion(Suggestion::Replace {
                    span: Span::new(file, i as u32, i as u32 + 1),
                    with: closing_of(top).to_string(),
                }),
            );
            // Pop the mismatched opener so later closes re-sync.
            stack.pop();
        },
        _ => {
            diags.push(
                Diagnostic::error(
                    ErrorCode::UnbalancedDelimiter,
                    format!("stray `{}`", ch as char),
                )
                .with_primary(
                    Span::new(file, i as u32, i as u32 + 1),
                    "does not close anything",
                ),
            );
        },
    }
}

fn closing_of(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        _ => '}',
    }
}

fn unterminated_string(file: FileId, open: u32, end: u32, hole: Option<u32>) -> Diagnostic {
    let mut d = Diagnostic::error(ErrorCode::UnterminatedString, "string literal never closed")
        .with_primary(Span::new(file, end, end), "input ended here")
        .with_secondary(Span::new(file, open, open + 1), "string starts here");
    if let Some(hole_open) = hole {
        d = d
            .with_secondary(
                Span::new(file, hole_open, hole_open + 2),
                "hole `${` opens here",
            )
            .with_note("interpolation hole was never closed");
    }
    d
}

fn invalid_char(file: FileId, i: usize) -> Diagnostic {
    Diagnostic::error(ErrorCode::InvalidChar, "character not valid in any token")
        .with_primary(Span::new(file, i as u32, i as u32 + 1), "invalid character")
}

/// A `\r` not followed by `\n`: rejected in code, in strings and holes, and
/// in char literals alike (raw strings alone may carry it, as multiline
/// data) — the same rule everywhere, so `nl` never splits on a lone `\r`.
fn lone_cr(file: FileId, i: usize) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::InvalidChar,
        "a lone `\\r` is not a line terminator",
    )
    .with_primary(Span::new(file, i as u32, i as u32 + 1), "invalid character")
}

fn stray_semicolon(file: FileId, i: usize) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::UnexpectedToken,
        "semicolons are not part of the surface; statements end at newlines",
    )
    .with_primary(Span::new(file, i as u32, i as u32 + 1), "stray `;`")
    .with_suggestion(Suggestion::Remove {
        span: Span::new(file, i as u32, i as u32 + 1),
    })
}

#[cfg(test)]
mod tests {
    use ds_diag::Files;

    use super::*;

    fn first_diag(src: &str) -> Diagnostic {
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        scan(f, src)
            .and_then(|d| d.into_iter().next())
            .expect("a diagnostic")
    }

    #[test]
    fn clean_source_produces_nothing() {
        let src = "const s = \"a ${f(1)} b\"\nconst y = r#\"raw\"#\nprint(s, y)\n";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
    }

    #[test]
    fn clean_nested_strings_in_holes() {
        let src = "const s = \"a ${ \"b ${c}\" } d\"\n";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
    }

    #[test]
    fn unterminated_string_is_two_span() {
        let d = first_diag("const s = \"abc");
        assert_eq!(d.code, ErrorCode::UnterminatedString);
        assert_eq!(d.labels.len(), 2);
    }

    #[test]
    fn unclosed_interpolation_names_the_hole() {
        let d = first_diag("\"a ${x");
        assert_eq!(d.code, ErrorCode::UnterminatedString);
        assert_eq!(d.labels.len(), 3);
    }

    #[test]
    fn mismatched_pair_reports_both_spans() {
        let d = first_diag("(a]");
        assert_eq!(d.code, ErrorCode::UnbalancedDelimiter);
        assert_eq!(d.labels.len(), 2);
        assert_eq!(d.suggestions.len(), 1);
    }

    #[test]
    fn deep_nesting_is_rejected_not_crashing() {
        let src = format!("{}x{})", "(".repeat(300), ")".repeat(300));
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        let diags = scan(f, &src).expect("diagnostics");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ErrorCode::NestingTooDeep);
    }

    #[test]
    fn semicolon_gets_a_removal_suggestion() {
        let d = first_diag("const a = 1;");
        assert_eq!(d.code, ErrorCode::UnexpectedToken);
        assert_eq!(d.suggestions.len(), 1);
    }

    #[test]
    fn escape_errors_are_positioned() {
        let d = first_diag("\"a\\q\"");
        assert_eq!(d.code, ErrorCode::InvalidEscape);
    }
}
