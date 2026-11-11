//! Impl-member signature agreement (B6): a trait impl's member must
//! declare what its trait member declares — same arity, same receiver
//! spelling, same passing modes, same parameter types, same return.
//! The trait member's registered signature is the call-site contract
//! (call sites check arguments against it, never against the impl's own
//! signature), so a divergent impl member would stage its body against a
//! contract it disagrees with and trap at the VM; agreement is proven at
//! check time instead.
//!
//! The comparison runs on the written signatures with the trait's
//! generic parameters substituted through the impl's trait reference;
//! both sides then solve in the impl's own scope, where the impl's
//! generic parameters mint the same atoms for both spellings, so solved
//! equality is agreement modulo spelling (aliases and union reorderings
//! solve equal). A member return left unannotated stays unchecked here:
//! its inference is the body's, and the trait's declared return against
//! an inferred impl return is a body-check seam.

use std::collections::HashMap;

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{FnDecl, ParamMode, Type};

use crate::{decls::FnSig, type_of::Bodies};

/// Compares one impl member's written signature with its trait member's
/// registered signature, reporting every disagreement at both sites.
pub(super) fn agree_member_signature(
    this: &mut Bodies<'_>,
    f: &FnDecl,
    sig: &FnSig,
    sig_span: Span,
    subst: &HashMap<String, Type>,
) {
    let trait_recv = sig.params.first().is_some_and(|p| p.name == "self");
    let impl_recv = f.params.first().is_some_and(|p| p.name.name == "self");
    if trait_recv != impl_recv {
        this.diags.push(
            Diagnostic::error(
                ErrorCode::ImplMemberSignatureMismatch,
                if trait_recv {
                    format!(
                        "impl member `{}` must spell its receiver `self` like the trait member",
                        f.name.name
                    )
                } else {
                    format!(
                        "the trait member `{}` takes no receiver; the impl member must not spell \
                         `self`",
                        f.name.name
                    )
                },
            )
            .with_primary(
                f.params.first().map_or(f.name.span, |p| p.name.span),
                "impl member signature",
            )
            .with_secondary(sig_span, "trait member signature"),
        );
        return;
    }
    if sig.params.len() != f.params.len() {
        this.diags.push(
            Diagnostic::error(
                ErrorCode::ImplMemberSignatureMismatch,
                format!(
                    "impl member `{}` takes {} parameters, the trait member takes {}",
                    f.name.name,
                    f.params.len(),
                    sig.params.len()
                ),
            )
            .with_primary(f.name.span, "impl member signature")
            .with_secondary(sig_span, "trait member signature"),
        );
        return;
    }
    for (index, (declared, written)) in sig.params.iter().zip(f.params.iter()).enumerate() {
        if !modes_agree(declared.mode, written.mode) {
            this.diags.push(
                Diagnostic::error(
                    ErrorCode::ImplMemberSignatureMismatch,
                    format!(
                        "parameter `{}` of impl member `{}` passes differently than the trait \
                         member declares",
                        written.name.name, f.name.name
                    ),
                )
                .with_primary(written.name.span, "impl member signature")
                .with_secondary(declared.span, "trait member signature"),
            );
        }
        if index == 0 && trait_recv {
            continue;
        }
        let declared_ty = declared
            .ty
            .as_ref()
            .map(|t| substitute_type(t, subst))
            .and_then(|t| this.solve_here(&t));
        let written_ty = written.ty.as_ref().and_then(|t| this.solve_here(t));
        if let (Some(a), Some(b)) = (declared_ty, written_ty) {
            if a != b {
                this.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImplMemberSignatureMismatch,
                        format!(
                            "impl member `{}`'s parameter `{}` has type `{}`, the trait member \
                             declares `{}`",
                            f.name.name,
                            written.name.name,
                            this.decls.universe.render(b),
                            this.decls.universe.render(a),
                        ),
                    )
                    .with_primary(
                        written.ty.as_ref().map_or(written.name.span, Type::span),
                        "impl member signature",
                    )
                    .with_secondary(declared.span, "trait member signature"),
                );
            }
        }
    }
    let declared_ret = sig
        .ret
        .as_ref()
        .map(|r| substitute_type(r, subst))
        .and_then(|r| this.solve_here(&r))
        .or_else(|| Some(this.unit()));
    let written_ret = f.ret.as_ref().and_then(|r| this.solve_here(r));
    if let (Some(a), Some(b)) = (declared_ret, written_ret) {
        if a != b {
            this.diags.push(
                Diagnostic::error(
                    ErrorCode::ImplMemberSignatureMismatch,
                    format!(
                        "impl member `{}` returns `{}`, the trait member declares `{}`",
                        f.name.name,
                        this.decls.universe.render(b),
                        this.decls.universe.render(a),
                    ),
                )
                .with_primary(
                    f.ret.as_ref().map_or(f.name.span, Type::span),
                    "impl member signature",
                )
                .with_secondary(sig_span, "trait member signature"),
            );
        }
    }
}

/// Whether two parameter modes are the same mode, ignoring their spans.
fn modes_agree(a: ParamMode, b: ParamMode) -> bool {
    matches!(
        (a, b),
        (ParamMode::Borrow, ParamMode::Borrow)
            | (ParamMode::Mut(_), ParamMode::Mut(_))
            | (ParamMode::Consume(_), ParamMode::Consume(_))
            | (ParamMode::Comptime(_), ParamMode::Comptime(_))
    )
}

/// Rewrites a written type with the impl's trait-reference arguments in
/// place of the trait's generic-parameter names: every bare path naming a
/// substituted parameter takes the argument's written type.
fn substitute_type(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Path { span, path, args } => {
            if args.is_empty() {
                if let [only] = path.segments.as_slice() {
                    if let Some(replacement) = subst.get(&only.name) {
                        return replacement.clone();
                    }
                }
            }
            Type::Path {
                span: *span,
                path: path.clone(),
                args: args.iter().map(|a| substitute_type(a, subst)).collect(),
            }
        },
        Type::Optional { span, inner } => Type::Optional {
            span: *span,
            inner: Box::new(substitute_type(inner, subst)),
        },
        Type::Negation { span, inner } => Type::Negation {
            span: *span,
            inner: Box::new(substitute_type(inner, subst)),
        },
        Type::Union { span, left, right } => Type::Union {
            span: *span,
            left: Box::new(substitute_type(left, subst)),
            right: Box::new(substitute_type(right, subst)),
        },
        Type::Intersect { span, left, right } => Type::Intersect {
            span: *span,
            left: Box::new(substitute_type(left, subst)),
            right: Box::new(substitute_type(right, subst)),
        },
        Type::Difference { span, left, right } => Type::Difference {
            span: *span,
            left: Box::new(substitute_type(left, subst)),
            right: Box::new(substitute_type(right, subst)),
        },
        Type::Paren { span, inner } => Type::Paren {
            span: *span,
            inner: Box::new(substitute_type(inner, subst)),
        },
        Type::Tuple { span, items } => Type::Tuple {
            span: *span,
            items: items.iter().map(|i| substitute_type(i, subst)).collect(),
        },
        Type::Unit(span) => Type::Unit(*span),
        Type::Fn { span, params, ret } => Type::Fn {
            span: *span,
            params: params.clone(),
            ret: ret.as_ref().map(|r| Box::new(substitute_type(r, subst))),
        },
        Type::Ptr {
            span,
            mutable,
            inner,
        } => Type::Ptr {
            span: *span,
            mutable: *mutable,
            inner: Box::new(substitute_type(inner, subst)),
        },
        Type::Foreign { span, name } => Type::Foreign {
            span: *span,
            name: name.clone(),
        },
        Type::TypeKw(span) => Type::TypeKw(*span),
    }
}
