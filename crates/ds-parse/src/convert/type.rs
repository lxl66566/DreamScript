//! Type and pattern conversion.

use pest::iterators::Pair;

use super::{Ctx, inner, is_type_rule, join};
use crate::{Rule, ast::*};

impl Ctx<'_> {
    // ===== types =====

    pub(super) fn type_expr(&mut self, p: Pair<'_, Rule>) -> Type {
        match p.as_rule() {
            Rule::type_union => {
                let span = self.span(&p);
                let mut iter = inner(&p).filter(|c| is_type_rule(c));
                let first = iter
                    .next()
                    .map_or(Type::TypeKw(span), |c| self.type_expr(c));
                iter.map(|c| self.type_expr(c)).fold(first, |acc, t| {
                    let s = join(acc.span(), t.span());
                    Type::Union {
                        span: s,
                        left: Box::new(acc),
                        right: Box::new(t),
                    }
                })
            },
            Rule::type_inter => {
                let span = self.span(&p);
                let mut iter = inner(&p).filter(|c| is_type_rule(c));
                let first = iter
                    .next()
                    .map_or(Type::TypeKw(span), |c| self.type_expr(c));
                iter.map(|c| self.type_expr(c)).fold(first, |acc, t| {
                    let s = join(acc.span(), t.span());
                    Type::Intersect {
                        span: s,
                        left: Box::new(acc),
                        right: Box::new(t),
                    }
                })
            },
            Rule::type_diff => {
                let span = self.span(&p);
                let mut iter = inner(&p).filter(|c| is_type_rule(c));
                let first = iter
                    .next()
                    .map_or(Type::TypeKw(span), |c| self.type_expr(c));
                iter.map(|c| self.type_expr(c)).fold(first, |acc, t| {
                    let s = join(acc.span(), t.span());
                    Type::Difference {
                        span: s,
                        left: Box::new(acc),
                        right: Box::new(t),
                    }
                })
            },
            Rule::type_postfix => {
                let span = self.span(&p);
                let mut inner_ty = Type::TypeKw(span);
                let mut marks = 0u32;
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::opt_mark => marks += 1,
                        _ => inner_ty = self.type_expr(c),
                    }
                }
                (0..marks).fold(inner_ty, |acc, _| Type::Optional {
                    span,
                    inner: Box::new(acc),
                })
            },
            Rule::type_unary => {
                let span = self.span(&p);
                let mut atom = None;
                let mut negs = 0u32;
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::neg_prefix => negs += 1,
                        _ => atom = Some(self.type_expr(c)),
                    }
                }
                let inner_ty = atom.unwrap_or(Type::TypeKw(span));
                (0..negs).fold(inner_ty, |acc, _| Type::Negation {
                    span,
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
                Some(c) if is_type_rule(&c) => self.type_expr(c),
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
                            let span = self.span(&c);
                            if let Expr::Str { parts, .. } = self.string_lit(c) {
                                return Pattern::Str { span, parts };
                            }
                            return Pattern::Wildcard(span);
                        },
                        Rule::raw_hashed | Rule::raw_zero => {
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
                Pattern::Literal { span, lit }
            },
            Rule::pat_call => {
                let mut path = None;
                let mut args = Vec::new();
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::path => path = Some(self.conv_path(c)),
                        Rule::pattern => args.push(self.pattern(c)),
                        _ => {},
                    }
                }
                Pattern::Call {
                    span,
                    path: path.unwrap_or(Path {
                        span,
                        segments: Vec::new(),
                    }),
                    args,
                }
            },
            Rule::pat_struct => {
                let mut path = None;
                let mut fields = Vec::new();
                for c in inner(&p) {
                    match c.as_rule() {
                        Rule::path => path = Some(self.conv_path(c)),
                        Rule::pat_field => {
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
                        },
                        _ => {},
                    }
                }
                Pattern::Struct {
                    span,
                    path: path.unwrap_or(Path {
                        span,
                        segments: Vec::new(),
                    }),
                    fields,
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
}
