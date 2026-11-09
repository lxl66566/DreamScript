//! Type-expression rendering with precedence-aware parentheses.
//!
//! Levels mirror the grammar: union (1) < intersection (2) < difference (3)
//! < postfix `?` (4) < prefix `~` (5) < atom (6). Slot minimums equal the
//! grammar level of each slot's sub-rule, so grammar-conformant trees render
//! flat exactly where they were parsed flat, and re-parse to the same tree.

use ds_parse::{Param, Type, TypeParam, TypeRef};

use crate::ctx::Ctx;

pub(crate) fn write_ty(ctx: &Ctx, out: &mut String, ty: &Type) {
    write_ty_min(ctx, out, ty, 1);
}

pub(crate) fn write_ty_min(ctx: &Ctx, out: &mut String, ty: &Type, min: u8) {
    if ty_level(ty) < min {
        out.push('(');
        write_ty(ctx, out, ty);
        out.push(')');
    } else {
        write_ty_inner(ctx, out, ty);
    }
}

fn ty_level(ty: &Type) -> u8 {
    match ty {
        Type::Union { .. } => 1,
        Type::Intersect { .. } => 2,
        Type::Difference { .. } => 3,
        Type::Optional { .. } => 4,
        Type::Negation { .. } => 5,
        Type::Path { .. }
        | Type::Paren { .. }
        | Type::Tuple { .. }
        | Type::Unit(_)
        | Type::Fn { .. }
        | Type::Ptr { .. }
        | Type::TypeKw(_)
        | Type::Foreign { .. } => 6,
    }
}

fn write_ty_inner(ctx: &Ctx, out: &mut String, ty: &Type) {
    match ty {
        Type::Path {
            span, path, args, ..
        } => {
            out.push_str(&path.text());
            if !args.is_empty() {
                // Case-type refs (`bad_number(string)`) keep their parens;
                // generic application renders square whatever the source
                // spelled (bracket canonical, angle tolerated). The AST
                // models both as `Path { args }`, so the delimiter is read
                // from the source span's closing byte.
                let paren = ctx.text(*span).ends_with(')');
                out.push(if paren { '(' } else { '[' });
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_ty(ctx, out, a);
                }
                out.push(if paren { ')' } else { ']' });
            }
        },
        Type::Optional { inner, .. } => {
            write_ty_min(ctx, out, inner, 4);
            out.push('?');
        },
        Type::Negation { inner, .. } => {
            out.push('~');
            write_ty_min(ctx, out, inner, 5);
        },
        Type::Union { left, right, .. } => {
            write_ty_min(ctx, out, left, 1);
            out.push_str(" | ");
            write_ty_min(ctx, out, right, 2);
        },
        Type::Intersect { left, right, .. } => {
            write_ty_min(ctx, out, left, 2);
            out.push_str(" & ");
            write_ty_min(ctx, out, right, 3);
        },
        Type::Difference { left, right, .. } => {
            write_ty_min(ctx, out, left, 3);
            out.push_str(" - ");
            write_ty_min(ctx, out, right, 4);
        },
        Type::Paren { inner, .. } => {
            out.push('(');
            write_ty(ctx, out, inner);
            out.push(')');
        },
        Type::Tuple { items, .. } => {
            out.push('(');
            for (i, t) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_ty(ctx, out, t);
            }
            out.push(')');
        },
        Type::Unit(_) => out.push_str("()"),
        Type::Fn { params, ret, .. } => {
            out.push_str("fn");
            write_params(ctx, out, params);
            if let Some(r) = ret {
                out.push_str(" -> ");
                write_ty(ctx, out, r);
            }
        },
        Type::Ptr { mutable, inner, .. } => {
            out.push_str(if *mutable { "*mut " } else { "*const " });
            write_ty_min(ctx, out, inner, 5);
        },
        Type::TypeKw(_) => out.push_str("type"),
        // Verbatim from the span: escape-free re-rendering of `c["sqlite3"]`.
        Type::Foreign { span, .. } => out.push_str(ctx.text(*span)),
    }
}

/// `[T: bound]`-style generic parameter list, always square brackets.
pub(crate) fn write_generics(ctx: &Ctx, out: &mut String, generics: &[TypeParam]) {
    if generics.is_empty() {
        return;
    }
    out.push('[');
    for (i, g) in generics.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&g.name.name);
        if let Some(b) = &g.bound {
            out.push_str(": ");
            write_ty(ctx, out, b);
        }
    }
    out.push(']');
}

/// `(mut self, x: t)` — modes always render before the name.
pub(crate) fn write_params(ctx: &Ctx, out: &mut String, params: &[Param]) {
    out.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_param(ctx, out, p);
    }
    out.push(')');
}

fn write_param(ctx: &Ctx, out: &mut String, p: &Param) {
    if let ds_parse::ParamMode::Mut(_)
    | ds_parse::ParamMode::Consume(_)
    | ds_parse::ParamMode::Comptime(_) = p.mode
    {
        out.push_str(mode_str(p.mode));
        out.push(' ');
    }
    out.push_str(&p.name.name);
    if let Some(t) = &p.ty {
        out.push_str(": ");
        write_ty(ctx, out, t);
    }
    // Literal-only signature defaults [F7]; conversion guarantees a `Some`
    // value is a literal shape, so the base indent never matters.
    if let Some(d) = &p.default {
        out.push_str(" = ");
        crate::expr::write_expr(ctx, out, 0, d);
    }
}

fn mode_str(mode: ds_parse::ParamMode) -> &'static str {
    match mode {
        ds_parse::ParamMode::Mut(_) => "mut",
        ds_parse::ParamMode::Consume(_) => "move",
        ds_parse::ParamMode::Comptime(_) => "comptime",
        ds_parse::ParamMode::Borrow => "",
    }
}

/// A trait reference in an impl head: `json`, `ord[k]`.
pub(crate) fn write_type_ref(ctx: &Ctx, out: &mut String, r: &TypeRef) {
    out.push_str(&r.path.text());
    if !r.args.is_empty() {
        out.push('[');
        for (i, a) in r.args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_ty(ctx, out, a);
        }
        out.push(']');
    }
}
