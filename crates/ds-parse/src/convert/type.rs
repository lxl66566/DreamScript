//! Type and pattern conversion.

use ds_diag::{ErrorCode, Span};
use pest::iterators::Pair;

use super::{Ctx, inner, is_type_rule, join};
use crate::{Rule, ast::*};

impl Ctx {
    // ===== types =====

    /// Folds one flat n-ary type chain (`type_union`, `type_inter`,
    /// `type_diff`) left-associatively into `ctor` nodes, guarded by the
    /// chain budget. The three grammar rules are the same repetition with a
    /// different connector, so one fold serves all three.
    fn type_chain(
        &mut self,
        p: Pair<'_, Rule>,
        ctor: impl Fn(Span, Box<Type>, Box<Type>) -> Type,
    ) -> Type {
        let span = self.span(&p);
        let terms: Vec<Pair<'_, Rule>> = inner(&p).filter(|c| is_type_rule(c)).collect();
        if self.chain_guard(terms.len(), span) {
            return Type::TypeKw(span);
        }
        let mut iter = terms.into_iter();
        let first = iter
            .next()
            .map_or(Type::TypeKw(span), |c| self.type_expr(c));
        iter.map(|c| self.type_expr(c)).fold(first, |acc, t| {
            ctor(join(acc.span(), t.span()), Box::new(acc), Box::new(t))
        })
    }

    pub(super) fn type_expr(&mut self, p: Pair<'_, Rule>) -> Type {
        match p.as_rule() {
            Rule::type_union => {
                self.type_chain(p, |span, left, right| Type::Union { span, left, right })
            },
            Rule::type_inter => {
                self.type_chain(p, |span, left, right| Type::Intersect { span, left, right })
            },
            Rule::type_diff => self.type_chain(p, |span, left, right| Type::Difference {
                span,
                left,
                right,
            }),
            Rule::type_postfix => {
                let span = self.span(&p);
                let mut inner_ty = Type::TypeKw(span);
                let mut marks: Vec<Span> = Vec::new();
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::opt_mark => marks.push(self.span(&c)),
                        _ => inner_ty = self.type_expr(c),
                    }
                }
                if self.chain_guard(marks.len() + 1, span) {
                    return Type::TypeKw(span);
                }
                // Fold in source order so every node spans the operand's
                // start through its own mark (`int?` inside `int??`), not
                // the whole level.
                marks
                    .into_iter()
                    .fold(inner_ty, |acc, mspan| Type::Optional {
                        span: join(acc.span(), mspan),
                        inner: Box::new(acc),
                    })
            },
            Rule::type_unary => {
                let span = self.span(&p);
                let mut atom = None;
                let mut negs: Vec<Span> = Vec::new();
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::neg_prefix => negs.push(self.span(&c)),
                        _ => atom = Some(self.type_expr(c)),
                    }
                }
                let inner_ty = atom.unwrap_or(Type::TypeKw(span));
                if self.chain_guard(negs.len() + 1, span) {
                    return Type::TypeKw(span);
                }
                // Fold innermost-first (the rightmost prefix binds the
                // atom), each node spanning its own prefix token through
                // the operand's end, like the expr-side unary fold.
                negs.into_iter()
                    .rev()
                    .fold(inner_ty, |acc, nspan| Type::Negation {
                        span: join(nspan, acc.span()),
                        inner: Box::new(acc),
                    })
            },
            Rule::case_type => {
                let span = self.span(&p);
                let mut path = None;
                let mut payload = Vec::new();
                for c in inner(&p) {
                    match c.as_rule() {
                        // `case_type` spells its head with the plain `path`
                        // rule, not `type_path`.
                        Rule::type_path | Rule::path => path = Some(self.conv_path(c)),
                        Rule::type_expr => payload.push(self.type_expr(c)),
                        _ => {},
                    }
                }
                let path = path.unwrap_or(Path {
                    span,
                    segments: Vec::new(),
                });
                Type::Path {
                    span,
                    path,
                    args: payload,
                    // Case-type applications spell parens.
                    delim: Delim::Paren,
                }
            },
            Rule::path_type => {
                let span = self.span(&p);
                let mut path = None;
                let mut args = Vec::new();
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::type_path => path = Some(self.conv_path(c)),
                        Rule::type_args_bracket | Rule::type_args_angle => args = self.type_args(c),
                        _ => {},
                    }
                }
                Type::Path {
                    span,
                    path: path.unwrap_or(Path {
                        span,
                        segments: Vec::new(),
                    }),
                    args,
                    // Bracket is canonical; the angle spelling folds here.
                    delim: Delim::Bracket,
                }
            },
            Rule::tuple_type => {
                let span = self.span(&p);
                let mut items = Vec::new();
                let mut trailing = false;
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::type_expr => items.push(self.type_expr(c)),
                        _ => trailing = true,
                    }
                }
                if items.is_empty() {
                    Type::Unit(span)
                } else if items.len() == 1 && !trailing {
                    let inner_t = items.pop().unwrap_or(Type::TypeKw(span));
                    Type::Paren {
                        span,
                        inner: Box::new(inner_t),
                    }
                } else {
                    Type::Tuple { span, items }
                }
            },
            Rule::fn_type => {
                let span = self.span(&p);
                let mut params = Vec::new();
                let mut ret = None;
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::params => params = self.params(c),
                        Rule::ret_ann => {
                            if let Some(t) = inner(&c).next() {
                                ret = Some(Box::new(self.type_expr(t)));
                            }
                        },
                        _ => {},
                    }
                }
                Type::Fn { span, params, ret }
            },
            Rule::ptr_type => {
                let span = self.span(&p);
                let mut mutable = false;
                let mut pointee = Type::TypeKw(span);
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::kw_mut => mutable = true,
                        _ => pointee = self.type_expr(c),
                    }
                }
                Type::Ptr {
                    span,
                    mutable,
                    inner: Box::new(pointee),
                }
            },
            Rule::type_kw => Type::TypeKw(self.span(&p)),
            // type_expr / type_arg wrappers and anything unexpected.
            _ => match inner(&p).next() {
                Some(c) => self.type_expr(c),
                None => Type::TypeKw(self.span(&p)),
            },
        }
    }

    // ===== patterns =====

    pub(super) fn pattern(&mut self, p: Pair<'_, Rule>) -> Pattern {
        let inner_pat = if p.as_rule() == Rule::pattern {
            match inner(&p).next() {
                Some(c) => c,
                None => return Pattern::Wildcard(self.span(&p)),
            }
        } else {
            p
        };
        if inner_pat.as_rule() == Rule::pat_or {
            let span = self.span(&inner_pat);
            let mut pats = Vec::new();
            // pat_single is silent, so pat_or's inner pairs are the atoms
            // themselves; pat_atom's fallback covers anything unexpected.
            for c in inner(&inner_pat) {
                pats.push(self.pat_atom(c));
            }
            if pats.len() > 1 {
                Pattern::Or { span, pats }
            } else {
                pats.pop().unwrap_or(Pattern::Wildcard(span))
            }
        } else {
            self.pat_atom(inner_pat)
        }
    }

    /// A pattern's `-` sign applies only to int and float literals; on any
    /// other literal the grammar accepts the shape but the convert layer
    /// could only drop the sign silently, so it is a diagnostic instead.
    fn pat_neg_on_non_numeric(&mut self, span: Span) {
        self.err(
            span,
            ErrorCode::InvalidLiteral,
            "a negative sign in a pattern applies only to int and float literals",
        );
    }

    pub(super) fn pat_atom(&mut self, p: Pair<'_, Rule>) -> Pattern {
        let span = self.span(&p);
        match p.as_rule() {
            Rule::pat_ident => match inner(&p).next() {
                Some(i) => {
                    let name = self.ident(i);
                    Pattern::Binding { span, name }
                },
                None => Pattern::Wildcard(span),
            },
            Rule::pat_literal => {
                let mut neg = false;
                let mut lit = Literal::Bool(false);
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::pat_neg => neg = true,
                        Rule::kw_true => lit = Literal::Bool(true),
                        Rule::kw_false => lit = Literal::Bool(false),
                        Rule::int_lit => lit = self.int_lit(c, neg),
                        Rule::float_lit => lit = self.float_lit(c, neg),
                        Rule::string_lit => {
                            // `pat_neg` is a prefix, so the sign is already
                            // known here.
                            if neg {
                                self.pat_neg_on_non_numeric(span);
                            }
                            let span = self.span(&c);
                            if let Expr::Str { parts, .. } = self.string_lit(c) {
                                return Pattern::Str { span, parts };
                            }
                            return Pattern::Wildcard(span);
                        },
                        Rule::raw_hashed | Rule::raw_zero => {
                            if neg {
                                self.pat_neg_on_non_numeric(span);
                            }
                            let span = self.span(&c);
                            if let Expr::RawStr { hashes, text, .. } = self.raw_str(c) {
                                return Pattern::RawStr { span, hashes, text };
                            }
                            return Pattern::Wildcard(span);
                        },
                        Rule::char_lit => lit = self.char_lit(c),
                        Rule::byte_lit => lit = self.byte_lit(c),
                        _ => {},
                    }
                }
                if neg && !matches!(lit, Literal::Int { .. } | Literal::Float { .. }) {
                    self.pat_neg_on_non_numeric(span);
                }
                Pattern::Literal { span, lit }
            },
            Rule::pat_head => {
                let mut path = None;
                let mut tail = None;
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::path => path = Some(self.conv_path(c)),
                        _ => tail = Some(c),
                    }
                }
                let path = path.unwrap_or(Path {
                    span,
                    segments: Vec::new(),
                });
                match tail {
                    Some(c) if c.as_rule() == Rule::pat_call => self.pat_call(c, span, path),
                    Some(c) => self.pat_struct(c, span, path),
                    // a bare qualified label pattern (F9.5): the path
                    // alone is the test
                    None => Pattern::Path { span, path },
                }
            },
            Rule::tuple_pattern => {
                let mut pats = Vec::new();
                let mut trailing = false;
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::pattern => pats.push(self.pattern(c)),
                        Rule::pt_trail => trailing = true,
                        _ => {},
                    }
                }
                Pattern::Tuple {
                    span,
                    pats,
                    trailing_comma: trailing,
                }
            },
            _ => Pattern::Wildcard(span),
        }
    }

    /// A call pattern's arguments; the head pair carries the span and path.
    fn pat_call(&mut self, p: Pair<'_, Rule>, span: Span, path: Path) -> Pattern {
        let args = inner(&p)
            .filter(|c| c.as_rule() == Rule::pattern)
            .map(|c| self.pattern(c))
            .collect();
        Pattern::Call { span, path, args }
    }

    /// A struct pattern's fields; the head pair carries the span and path.
    fn pat_struct(&mut self, p: Pair<'_, Rule>, span: Span, path: Path) -> Pattern {
        let mut fields = Vec::new();
        for c in inner(&p).filter(|c| c.as_rule() == Rule::pat_field) {
            let mut name = None;
            let mut pat = None;
            for fc in inner(&c) {
                match fc.as_rule() {
                    Rule::ident => name = Some(self.ident(fc)),
                    Rule::pattern => pat = Some(self.pattern(fc)),
                    _ => {},
                }
            }
            if let Some(name) = name {
                fields.push((name, pat));
            }
        }
        Pattern::Struct { span, path, fields }
    }
}
