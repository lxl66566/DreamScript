//! Guard-tail collision recovery: the grammar reads a bare identifier
//! directly before `=>` as a bare-parameter lambda, so a match guard whose
//! expression ends in one loses the arm arrow to that lambda and the file
//! fails with a generic token error. This module re-diagnoses that failure:
//! it finds candidate `if ... ident =>` shapes, confirms a candidate by
//! re-parsing a parenthesized repair (exactly the fix the diagnostic
//! suggests), and reports the collision with precise spans.
//!
//! Confirmation is the repaired file parsing, so the diagnostic fires only
//! when parenthesizing that guard fixes the syntax: a bare lambda in a
//! non-guard position never confirms (the file was not failing through it),
//! and a file with two collisions or another syntax error alongside keeps
//! the generic error. Shapes that stay on the generic error with it: the
//! paren-parameter family (`x if x > (m) => ...`) and a one-name guard
//! (`if m => ...`, whose `(m)` repair is itself a paren-parameter lambda)
//! per the recorded quirk ruling, and a guard whose tail is reached across
//! a newline continuation or through a comment before the arrow.

// Offsets come from the token walk and are u32 by the Files contract, so
// narrowing casts are total.
#![allow(clippy::cast_possible_truncation)]

use ds_diag::{Diagnostic, ErrorCode, FileId, Span, Suggestion};
use pest::Parser;

use crate::{DsParser, Rule};

/// Cap on repair re-parses per failed parse. Real files need one; the cap
/// only bounds the broken-file path on adversarial `if`/`=>` counts.
const MAX_REPARSES: usize = 64;

/// If the failure is the guard-tail collision, its diagnostic.
pub(crate) fn detect(file: FileId, src: &str) -> Option<Diagnostic> {
    let (ifs, arrows) = tokens(src);
    let mut reparses = 0;
    'ifs: for &(if_end, if_depth) in &ifs {
        for &(arrow, arrow_depth, barrier) in arrows.iter().filter(|a| a.0 > if_end) {
            // The stolen arrow sits in the same match arm as the `if`: same
            // bracket depth, with no arm separator (a depth-matching comma
            // or newline) between them. This is what keeps a later arm's
            // own `_ =>` from confirming as this guard's tail.
            if arrow_depth != if_depth || barrier >= if_end {
                continue;
            }
            let Some((ident_start, ident_end)) = bare_ident_before(src.as_bytes(), arrow) else {
                continue;
            };
            // The identifier must sit after the `if` (not be it) with guard
            // text between: `if m => ...` has no parenthesizable guard (a
            // lone `(m)` is the paren-lambda shape) and stays generic.
            if ident_start <= if_end {
                continue;
            }
            let guard_start = guard_expr_start(src.as_bytes(), if_end);
            let mut repaired = String::with_capacity(src.len() + 2);
            repaired.push_str(&src[..guard_start]);
            repaired.push('(');
            repaired.push_str(&src[guard_start..ident_end]);
            repaired.push(')');
            repaired.push_str(&src[ident_end..]);
            reparses += 1;
            if DsParser::parse(Rule::file, &repaired).is_ok() {
                return Some(collision(
                    file,
                    src,
                    guard_start,
                    ident_start,
                    ident_end,
                    arrow,
                ));
            }
            if reparses >= MAX_REPARSES {
                break 'ifs;
            }
        }
    }
    None
}

/// The collision diagnostic: the identifier read as the lambda's parameter
/// (primary), the stolen arm arrow (secondary), and the verified
/// parenthesization as two insert suggestions.
fn collision(
    file: FileId,
    src: &str,
    guard_start: usize,
    ident_start: usize,
    ident_end: usize,
    arrow: usize,
) -> Diagnostic {
    let name = &src[ident_start..ident_end];
    Diagnostic::error(
        ErrorCode::GuardTailLambda,
        format!(
            "match guard collides with the arm arrow: `{name}` before `=>` is read as a \
             bare-parameter lambda, so the guard swallows the arm body"
        ),
    )
    .with_primary(
        Span::new(file, ident_start as u32, ident_end as u32),
        format!("`{name}` is read as the lambda's parameter"),
    )
    .with_secondary(
        Span::new(file, arrow as u32, arrow as u32 + 2),
        "this `=>` becomes the lambda's arrow, leaving the arm without one",
    )
    .with_note(
        "the grammar reads a bare identifier directly before `=>` as a lambda parameter; \
         parenthesize the guard expression so the arm keeps its arrow",
    )
    .with_suggestion(Suggestion::Insert {
        span: Span::new(file, guard_start as u32, guard_start as u32),
        text: "(".to_owned(),
    })
    .with_suggestion(Suggestion::Insert {
        span: Span::new(file, ident_end as u32, ident_end as u32),
        text: ")".to_owned(),
    })
}

/// An `if` keyword token: (end offset, bracket depth).
type IfTok = (usize, usize);
/// An `=>` token: (start offset, bracket depth, position of the last arm
/// separator at that depth before it, 0 meaning none).
type ArrowTok = (usize, usize, usize);

/// End offsets (with bracket depth) of `if` keyword tokens, and start
/// offsets of `=>` tokens in code, each with its bracket depth and the
/// position of the last arm separator (a depth-matching `,` or newline)
/// before it. Comments and literals are skipped; interpolation-hole
/// interiors are skipped with their string (a collision there keeps the
/// generic error).
fn tokens(src: &str) -> (Vec<IfTok>, Vec<ArrowTok>) {
    #[derive(Clone, Copy)]
    enum St {
        Code,
        Line,
        Block,
        Str,
        Char,
        Raw(usize),
    }
    let b = src.as_bytes();
    let mut st = St::Code;
    let mut i = 0;
    let mut depth = 0usize;
    // Last separator position seen at each depth; 0 means none.
    let mut last_sep = vec![0usize];
    let mut ifs = Vec::new();
    let mut arrows = Vec::new();
    let ident_ish = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        let prev = i.checked_sub(1).is_some_and(|p| ident_ish(b[p]));
        match st {
            St::Code => match b[i] {
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    st = St::Line;
                    i += 2;
                },
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    st = St::Block;
                    i += 2;
                },
                b'"' => {
                    st = St::Str;
                    i += 1;
                },
                b'\'' => {
                    st = St::Char;
                    i += 1;
                },
                b'r' if !prev && matches!(b.get(i + 1), Some(b'"' | b'#')) => {
                    let mut k = 0;
                    while b.get(i + 1 + k) == Some(&b'#') {
                        k += 1;
                    }
                    st = St::Raw(k);
                    // Skip `r`, the hashes, and the opening quote.
                    i += 1 + k + 1;
                },
                b'b' if !prev && b.get(i + 1) == Some(&b'\'') => {
                    st = St::Char;
                    i += 2;
                },
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    while last_sep.len() <= depth {
                        last_sep.push(0);
                    }
                    i += 1;
                },
                // The lexical pre-scan already rejected unbalanced brackets,
                // so this cannot underflow on real input.
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    i += 1;
                },
                b',' | b'\n' => {
                    last_sep[depth] = i;
                    i += 1;
                },
                b'i' if !prev
                    && b.get(i + 1) == Some(&b'f')
                    && !b.get(i + 2).copied().is_some_and(ident_ish) =>
                {
                    ifs.push((i + 2, depth));
                    i += 2;
                },
                b'=' if b.get(i + 1) == Some(&b'>') => {
                    arrows.push((i, depth, last_sep[depth]));
                    i += 2;
                },
                _ => i += 1,
            },
            St::Line => {
                if b[i] == b'\n' {
                    st = St::Code;
                }
                i += 1;
            },
            St::Block => {
                if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    st = St::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            },
            St::Str => match b[i] {
                b'\\' => i += 2,
                // A newline inside is a defensive bail, not a real case: the
                // lexical pre-scan already rejected unterminated strings.
                b'"' | b'\n' => {
                    st = St::Code;
                    i += 1;
                },
                _ => i += 1,
            },
            St::Char => match b[i] {
                b'\\' => i += 2,
                b'\'' | b'\n' => {
                    st = St::Code;
                    i += 1;
                },
                _ => i += 1,
            },
            St::Raw(hashes) => {
                if b[i] == b'"' {
                    let mut k = 0;
                    while b.get(i + 1 + k) == Some(&b'#') {
                        k += 1;
                    }
                    if k == hashes {
                        st = St::Code;
                    }
                    i += 1 + k;
                } else {
                    i += 1;
                }
            },
        }
    }
    (ifs, arrows)
}

/// The identifier ending right before `=>` (same line, horizontal spacing
/// only), if any: `(start, end)` of the name. A comment between the
/// identifier and the arrow is not skipped; that shape keeps the generic
/// error.
fn bare_ident_before(b: &[u8], arrow: usize) -> Option<(usize, usize)> {
    let mut end = arrow;
    while end > 0 && matches!(b[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    if !end
        .checked_sub(1)
        .is_some_and(|p| b[p].is_ascii_alphanumeric() || b[p] == b'_')
    {
        return None;
    }
    let mut start = end;
    while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
        start -= 1;
    }
    let first = b[start];
    (first.is_ascii_alphabetic() || first == b'_').then_some((start, end))
}

/// First byte at or after the `if` that is not horizontal spacing: where the
/// guard expression starts and the suggested `(` goes.
fn guard_expr_start(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    i
}
