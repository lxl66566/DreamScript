//! Impl-member signature agreement (B6): a trait impl's member must
//! declare what its trait member declares — same arity, same receiver
//! spelling, same passing modes, same parameter types, same return, and
//! no parameter defaults of its own (a re-declared default would mint a
//! second source for the staged literal; an impl-only default is inert,
//! since the trait member's registered signature carries no default to
//! stage). The trait member's
//! registered signature is the call-site contract (call sites check
//! arguments against it, never against the impl's own signature), so a
//! divergent impl member would stage its body against a contract it
//! disagrees with and trap at the VM; agreement is proven at check time
//! instead.
//!
//! The comparison runs on the written signatures with the trait's
//! generic parameters substituted through the impl's trait reference;
//! both sides then solve in the impl's own scope, where the impl's
//! generic parameters mint the same atoms for both spellings, so solved
//! equality is agreement modulo spelling (aliases and union reorderings
//! solve equal). A member return left unannotated is agreed after
//! the body checks, through the body's own checked tail type
//! (`agree_inferred_return`). Impl completeness rides the same door
//! (`agree_impl_completeness`): every trait member without a default
//! body must arrive with a bodied impl member, because call sites check
//! the registered signature alone.

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{FnDecl, ImplDecl, ParamMode, Type};
use rustc_hash::FxHashMap;

use crate::{decls, decls::FnSig, resolve::DefId, type_of::Bodies, universe::Ty};

/// Compares one impl member's written signature with its trait member's
/// registered signature, reporting every disagreement at both sites.
pub(super) fn agree_member_signature(
    this: &mut Bodies<'_>,
    f: &FnDecl,
    sig: &FnSig,
    sig_span: Span,
    subst: &FxHashMap<String, Type>,
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
        // The trait declaration is the single authority for a defaulted
        // member parameter: call sites stage and check the trait's
        // declared default, so an impl body re-declaring it would mint a
        // second, never-checked source for the staged literal. Inherent
        // members own their defaults (no trait declaration exists). The
        // mirror asymmetry refuses too: an impl-only default is inert —
        // the trait member's registered signature carries no default, so
        // an omitted argument stays a missing-argument error at every
        // call site and the written literal never stages anywhere.
        match (declared.default.is_some(), written.default.as_ref()) {
            (true, Some(default)) => {
                this.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImplMemberDefaultRedeclared,
                        format!(
                            "parameter `{}` of impl member `{}` re-declares the default its trait \
                             member owns; the trait declaration is the default's single authority",
                            written.name.name, f.name.name
                        ),
                    )
                    .with_primary(default.span(), "re-declared default")
                    .with_secondary(declared.span, "trait member signature"),
                );
            },
            (false, Some(default)) => {
                this.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImplMemberDefaultWithoutTrait,
                        format!(
                            "parameter `{}` of impl member `{}` declares a default its trait \
                             member does not have; the trait member has no default to override",
                            written.name.name, f.name.name
                        ),
                    )
                    .with_primary(default.span(), "impl-only default")
                    .with_secondary(declared.span, "trait member signature"),
                );
            },
            _ => {},
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
fn substitute_type(ty: &Type, subst: &FxHashMap<String, Type>) -> Type {
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

/// A trait impl must provide a body for every trait member that
/// declares no default body: call sites check against the trait
/// member's registered signature alone, so a member with neither an
/// impl body nor a trait default would check clean at every call site
/// and trap at the dispatch seam. Both incomplete shapes refuse here
/// at the impl's own door — the member spelled bodiless, or omitted
/// entirely (the impl's trait reference is then the primary site, since
/// no member span exists) — with the trait member as the secondary.
pub(super) fn agree_impl_completeness(
    this: &mut Bodies<'_>,
    imp: &ImplDecl,
    trait_def: DefId,
    members: &[String],
) {
    for name in members {
        let defaulted = this
            .decls
            .trait_member_defaults
            .get(&trait_def)
            .is_some_and(|defaults| defaults.iter().any(|d| d == name));
        if defaulted {
            continue;
        }
        let spelled = imp.fns.iter().find(|f| f.name.name == *name);
        if spelled.is_some_and(|f| f.body.is_some()) {
            continue;
        }
        let Some((_, sig_span)) =
            decls::trait_member_sig(this.package, this.decls, trait_def, name)
        else {
            continue;
        };
        let (primary, message) = match spelled {
            Some(f) => (
                f.name.span,
                format!(
                    "impl member `{name}` declares no body, and its trait member declares no \
                     default body"
                ),
            ),
            None => (
                imp.trait_ref.as_ref().map_or(imp.span, |tr| tr.span),
                format!(
                    "the impl of `{}` implements no `{name}` member, and the trait member \
                     declares no default body",
                    decls::render_type(&imp.target)
                ),
            ),
        };
        this.diags.push(
            Diagnostic::error(ErrorCode::ImplMemberWithoutBody, message)
                .with_primary(primary, "no body for the trait member")
                .with_secondary(sig_span, "trait member with no default body"),
        );
    }
}

/// An impl member whose return is unannotated agrees with the trait
/// member's declared return through its body's checked tail type: the
/// registered signature is the call-site contract, so an inferred body
/// must produce exactly the declared return — a written divergence and an
/// inferred one are the same disagreement.
pub(super) fn agree_inferred_return(
    this: &mut Bodies<'_>,
    f: &FnDecl,
    sig: &FnSig,
    sig_span: Span,
    subst: &FxHashMap<String, Type>,
    inferred: Ty,
) {
    let Some(ret) = sig.ret.as_ref() else {
        return;
    };
    let Some(declared) = this.solve_here(&substitute_type(ret, subst)) else {
        return;
    };
    if declared == inferred {
        return;
    }
    this.diags.push(
        Diagnostic::error(
            ErrorCode::ImplMemberSignatureMismatch,
            format!(
                "impl member `{}`'s body returns `{}`, the trait member declares `{}`",
                f.name.name,
                this.decls.universe.render(inferred),
                this.decls.universe.render(declared),
            ),
        )
        .with_primary(
            f.body.as_ref().map_or(f.name.span, |body| body.span),
            "impl member body",
        )
        .with_secondary(sig_span, "trait member signature"),
    );
}
