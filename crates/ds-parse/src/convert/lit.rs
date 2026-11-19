//! Literal conversion: numbers, chars, and structured strings.

use ds_diag::{Diagnostic, ErrorCode, Span};
use pest::iterators::Pair;

use super::{
    Cooked, CookedByte, Ctx, cook_byte, cook_escape, inner, split_float_suffix, split_int_suffix,
};
use crate::{Rule, ast::*};

impl Ctx {
    // ===== literals =====

    pub(super) fn int_lit(&mut self, p: Pair<'_, Rule>, neg: bool) -> Literal {
        let text = p.as_str().replace('_', "");
        let (digits, suffix) = split_int_suffix(&text);
        let text = if neg { format!("-{digits}") } else { digits.to_owned() };
        Literal::Int {
            suffix: suffix.to_owned(),
            text,
        }
    }

    pub(super) fn float_lit(&mut self, p: Pair<'_, Rule>, neg: bool) -> Literal {
        let text = p.as_str().replace('_', "");
        let (digits, suffix) = split_float_suffix(&text);
        let text = if neg { format!("-{digits}") } else { digits.to_owned() };
        Literal::Float {
            suffix: suffix.to_owned(),
            text,
        }
    }

    pub(super) fn char_lit(&mut self, p: Pair<'_, Rule>) -> Literal {
        let span = self.span(&p);
        let raw = inner(&p)
            .next()
            .map(|c| c.as_str().to_owned())
            .unwrap_or_default();
        match cook_escape(&raw) {
            Cooked::Char(ch) => Literal::Char(ch),
            Cooked::Invalid(off) => {
                self.err(
                    Span::new(self.file, span.start + off, span.start + off + 1),
                    ErrorCode::InvalidLiteral,
                    "escape is not a valid Unicode scalar value",
                );
                Literal::Char('\0')
            },
            Cooked::Empty => Literal::Char('\0'),
        }
    }

    pub(super) fn byte_lit(&mut self, p: Pair<'_, Rule>) -> Literal {
        let span = self.span(&p);
        let raw = inner(&p)
            .next()
            .map(|c| c.as_str().to_owned())
            .unwrap_or_default();
        match cook_byte(&raw) {
            CookedByte::Byte(b) => Literal::Byte(b),
            CookedByte::NotAscii(ch) => {
                self.err(
                    span,
                    ErrorCode::InvalidLiteral,
                    format!(
                        "`{}` is not an ASCII byte; a byte literal takes one ASCII char or a \
                         `\\u{{..}}` escape naming 0-255",
                        ch.escape_debug()
                    ),
                );
                Literal::Byte(0)
            },
            CookedByte::PastRange(value) => {
                self.err(
                    span,
                    ErrorCode::InvalidLiteral,
                    format!("`\\u{{{value:x}}}` names {value}, past the byte range 0-255"),
                );
                Literal::Byte(0)
            },
            CookedByte::Invalid(off) => {
                self.err(
                    Span::new(self.file, span.start + off, span.start + off + 1),
                    ErrorCode::InvalidLiteral,
                    "escape is not a valid Unicode scalar value",
                );
                Literal::Byte(0)
            },
            CookedByte::Empty => Literal::Byte(0),
        }
    }

    pub(super) fn string_lit(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let mut parts: Vec<StrPart> = Vec::new();
        let mut text = String::new();
        let mut text_start: Option<u32> = None;
        for c in p.clone().into_inner() {
            match c.as_rule() {
                Rule::str_text => {
                    if text_start.is_none() {
                        text_start = Some(self.span(&c).start);
                    }
                    text.push_str(c.as_str());
                },
                Rule::esc => {
                    if text_start.is_none() {
                        text_start = Some(self.span(&c).start);
                    }
                    match cook_escape(c.as_str()) {
                        Cooked::Char(ch) => text.push(ch),
                        Cooked::Invalid(off) => {
                            let s = self.span(&c);
                            self.err(
                                Span::new(self.file, s.start + off, s.start + off + 1),
                                ErrorCode::InvalidEscape,
                                "escape is not a valid Unicode scalar value",
                            );
                        },
                        Cooked::Empty => {},
                    }
                },
                Rule::interp_brace => {
                    let hspan = self.span(&c);
                    if let Some(start) = text_start.take() {
                        parts.push(StrPart::Text {
                            span: Span::new(self.file, start, hspan.start),
                            text: std::mem::take(&mut text),
                        });
                    }
                    let hole_expr = inner(&c)
                        .find(|e| e.as_rule() == Rule::expr)
                        .map(|e| self.expr(e));
                    parts.push(StrPart::Expr {
                        span: hspan,
                        expr: Box::new(hole_expr.unwrap_or(Expr::Break(hspan))),
                    });
                },
                Rule::interp_simple => {
                    let hspan = self.span(&c);
                    if let Some(start) = text_start.take() {
                        parts.push(StrPart::Text {
                            span: Span::new(self.file, start, hspan.start),
                            text: std::mem::take(&mut text),
                        });
                    }
                    parts.push(StrPart::Ident {
                        span: hspan,
                        name: Ident {
                            span: Span::new(self.file, hspan.start + 1, hspan.end),
                            name: c.as_str()[1..].to_owned(),
                        },
                    });
                },
                _ => {},
            }
        }
        if let Some(start) = text_start.take() {
            parts.push(StrPart::Text {
                span: Span::new(self.file, start, span.end - 1),
                text,
            });
        }
        Expr::Str { span, parts }
    }

    pub(super) fn raw_str(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let text = p.as_str();
        if p.as_rule() == Rule::raw_zero {
            return Expr::RawStr {
                span,
                hashes: 0,
                text: text[2..text.len() - 1].to_owned(),
            };
        }
        let bytes = text.as_bytes();
        let mut open = 0usize;
        while bytes.get(1 + open) == Some(&b'#') {
            open += 1;
        }
        let mut close = 0usize;
        while bytes.get(text.len().wrapping_sub(1 + close)) == Some(&b'#') {
            close += 1;
        }
        let body_start = 1 + open + 1;
        let body_end = text.len() - close - 1;
        if open != close {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::InvalidLiteral,
                    format!("raw string opened with {open} hashes but closed with {close}"),
                )
                .with_primary(
                    Span::new(
                        self.file,
                        span.start + (text.len() - close) as u32,
                        span.end,
                    ),
                    format!("{close} closing hashes"),
                )
                .with_secondary(
                    Span::new(self.file, span.start + 1, span.start + 1 + open as u32),
                    format!("{open} opening hashes"),
                ),
            );
        }
        let body = if body_end > body_start {
            text[body_start..body_end].to_owned()
        } else {
            String::new()
        };
        Expr::RawStr {
            span,
            hashes: open as u32,
            text: body,
        }
    }
}
