//! Item and declaration rendering: imports, bindings, functions, structs,
//! enums, tags, aliases, traits, impls.
//!
//! Canonical-form decisions taken here (see also the crate docs):
//! - named functions render as `fn name(params) -> ret { ... }`: trait/impl members are FnDecls,
//!   and a top-level `fn` converts onto the const-declaration node, whose converted shape (one
//!   immutable name bound to one block-bodied lambda) renders back as the fn spelling (F2).
//! - generic application renders with square brackets (`list[int]`), never the angle form the
//!   parser tolerates.
//! - block-bodied members (struct/enum/trait/impl) render newline-separated without commas when
//!   multiline; match arms drop commas only where the grammar's newline separation allows
//!   (block-bodied followers).
//! - an attached member/field `///` doc run renders immediately above its member at the member's
//!   indent (member-grain placement, the parse layer's doc slots as the placement input); every
//!   other interior comment keeps the item-trailing migration.

use ds_diag::Span;
use ds_parse::{
    Attribute, EnumCase, EnumDecl, EnumPayload, Expr, FieldDecl, FnDecl, ImplDecl, ImportDecl,
    Item, ItemKind, LocalDecl, Pattern, PlatformDecl, StructDecl, StructMember, TagDecl, TraitDecl,
    TraitMember, Trivia, TypeAliasDecl, Vis,
};

use crate::{
    ctx::Ctx,
    expr::{push_indent, write_block, write_expr, write_local},
    ty::{write_generics, write_params, write_path, write_ty, write_type_ref},
};

/// Renders one top-level item as one or more lines (no trailing newline);
/// attributes render as their own lines above the declaration.
pub(crate) fn write_item(ctx: &Ctx, out: &mut String, base: usize, item: &Item) {
    for a in &item.attrs {
        push_indent(out, base);
        write_attr(out, a);
        out.push('\n');
    }
    push_indent(out, base);
    if let Some(vis) = &item.vis {
        write_vis(out, *vis);
        out.push(' ');
    }
    match &item.kind {
        ItemKind::Import(d) => write_import(ctx, out, base, d),
        ItemKind::Local(l) => write_top_local(ctx, out, base, l),
        ItemKind::Fn(f) | ItemKind::ExternFn(f) => {
            if matches!(item.kind, ItemKind::ExternFn(_)) {
                out.push_str("extern ");
            }
            write_fn(ctx, out, base, f);
        },
        ItemKind::Struct(s) => write_struct(ctx, out, base, s),
        ItemKind::Enum(e) => write_enum(ctx, out, base, e),
        ItemKind::Tag(t) => write_tag(out, t),
        ItemKind::TypeAlias(t) => write_type_alias(ctx, out, t),
        ItemKind::Trait(t) => write_trait(ctx, out, base, t),
        ItemKind::Impl(i) => write_impl(ctx, out, base, i),
        ItemKind::Platform(d) => write_platform(out, d),
    }
}

/// One platform declaration: the name's literal is re-quoted, the
/// provides list comma-separated inside angle brackets.
fn write_platform(out: &mut String, d: &PlatformDecl) {
    out.push_str("platform \"");
    out.push_str(&d.name.name);
    out.push('"');
    if !d.provides.is_empty() {
        out.push_str(" provides <");
        for (i, cap) in d.provides.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&cap.name);
        }
        out.push('>');
    }
}

fn write_vis(out: &mut String, vis: Vis) {
    out.push_str(match vis {
        Vis::Pub(_) => "pub",
        Vis::PubPkg(_) => "pub(pkg)",
    });
}

/// A top-level binding: the converted `fn` shape — one immutable name,
/// no binding annotation, one block-bodied lambda — renders back as the
/// fn declaration it spells (F2); every other binding keeps its
/// const/var form.
fn write_top_local(ctx: &Ctx, out: &mut String, base: usize, l: &LocalDecl) {
    if let (
        Pattern::Binding { name, .. },
        false,
        None,
        Expr::Lambda {
            generics,
            params,
            ret,
            arrow: false,
            body,
            ..
        },
    ) = (&l.pat, l.mutable, &l.ty, &l.init)
    {
        out.push_str("fn ");
        out.push_str(&name.name);
        write_generics(ctx, out, generics);
        write_params(ctx, out, params);
        if let Some(ret) = ret {
            out.push_str(" -> ");
            write_ty(ctx, out, ret);
        }
        out.push(' ');
        if let Expr::Block(b) = &**body {
            write_block(ctx, out, base, b);
        } else {
            write_expr(ctx, out, base, body);
        }
        return;
    }
    write_local(ctx, out, base, l);
}

fn write_attr(out: &mut String, a: &Attribute) {
    out.push_str("#[");
    out.push_str(&a.name.name);
    if !a.args.is_empty() {
        out.push('(');
        for (i, p) in a.args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_path(out, p);
        }
        out.push(')');
    }
    out.push(']');
}

fn write_import(ctx: &Ctx, out: &mut String, base: usize, d: &ImportDecl) {
    out.push_str("import ");
    if d.path.at_sign {
        out.push('@');
    }
    for (i, seg) in d.path.segments.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&seg.name);
    }
    if !d.select.is_empty() {
        out.push_str(" (");
        for (i, s) in d.select.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&s.name.name);
            if let Some(a) = &s.alias {
                out.push_str(" as ");
                out.push_str(&a.name);
            }
        }
        out.push(')');
    }
    if let Some(a) = &d.alias {
        out.push_str(" as ");
        out.push_str(&a.name);
    }
    if let Some(cond) = &d.cond {
        out.push_str(" if ");
        write_expr(ctx, out, base, cond);
    }
}

fn write_fn(ctx: &Ctx, out: &mut String, base: usize, f: &FnDecl) {
    out.push_str("fn ");
    out.push_str(&f.name.name);
    write_generics(ctx, out, &f.generics);
    write_params(ctx, out, &f.params);
    if let Some(ret) = &f.ret {
        out.push_str(" -> ");
        write_ty(ctx, out, ret);
    }
    if let Some(body) = &f.body {
        out.push(' ');
        write_block(ctx, out, base, body);
    }
}

fn write_struct(ctx: &Ctx, out: &mut String, base: usize, s: &StructDecl) {
    out.push_str("struct ");
    out.push_str(&s.name.name);
    write_generics(ctx, out, &s.generics);
    write_supers(ctx, out, &s.supers);
    if !s.has_body {
        return;
    }
    if s.members.is_empty() {
        out.push_str(" {}");
        return;
    }
    if ctx.one_line(s.span) {
        out.push_str(" { ");
        for (i, m) in s.members.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_struct_member(ctx, out, base, m);
        }
        out.push_str(" }");
    } else {
        out.push_str(" {\n");
        for m in &s.members {
            if let StructMember::Field(f) = m {
                push_doc(ctx, out, base + 1, &f.doc);
            }
            push_indent(out, base + 1);
            write_struct_member(ctx, out, base + 1, m);
            out.push('\n');
        }
        push_indent(out, base);
        out.push('}');
    }
}

/// One attached member/field doc run, printed where it stood: each line
/// verbatim at the member's own indent, immediately above the member.
fn push_doc(ctx: &Ctx, out: &mut String, indent: usize, doc: &[Trivia]) {
    for t in doc {
        push_indent(out, indent);
        out.push_str(ctx.comment_text(t));
        out.push('\n');
    }
}

/// Whether `span` is a trivia of an attached member/field doc run in the
/// item: the trivia the item's trailing migration must skip, because the
/// member sites above print them at their members instead.
pub(crate) fn is_member_doc(kind: &ItemKind, span: Span) -> bool {
    let hit = |doc: &[Trivia]| doc.iter().any(|t| t.span == span);
    match kind {
        ItemKind::Trait(t) => t.members.iter().any(|m| match m {
            TraitMember::Fn(f) => hit(&f.doc),
            TraitMember::AssocType { .. } => false,
        }),
        ItemKind::Impl(i) => i.fns.iter().any(|f| hit(&f.doc)),
        ItemKind::Struct(s) => s.members.iter().any(|m| match m {
            StructMember::Field(f) => hit(&f.doc),
            StructMember::Deinit { .. } => false,
        }),
        ItemKind::Enum(e) => e.cases.iter().any(|c| match &c.payload {
            EnumPayload::Struct(fields) => fields.iter().any(|f| hit(&f.doc)),
            _ => false,
        }),
        _ => false,
    }
}

fn write_struct_member(ctx: &Ctx, out: &mut String, base: usize, m: &StructMember) {
    match m {
        StructMember::Field(f) => write_field(ctx, out, f),
        StructMember::Deinit { body, .. } => {
            out.push_str("deinit ");
            write_block(ctx, out, base, body);
        },
    }
}

fn write_field(ctx: &Ctx, out: &mut String, f: &FieldDecl) {
    out.push_str(&f.name.name);
    out.push_str(": ");
    write_ty(ctx, out, &f.ty);
}

fn write_supers(ctx: &Ctx, out: &mut String, supers: &[ds_parse::Type]) {
    if supers.is_empty() {
        return;
    }
    out.push_str(": ");
    for (i, t) in supers.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_ty(ctx, out, t);
    }
}

fn write_enum(ctx: &Ctx, out: &mut String, base: usize, e: &EnumDecl) {
    out.push_str("enum ");
    out.push_str(&e.name.name);
    write_generics(ctx, out, &e.generics);
    write_supers(ctx, out, &e.supers);
    if e.cases.is_empty() {
        out.push_str(" {}");
        return;
    }
    if ctx.one_line(e.span) {
        out.push_str(" { ");
        for (i, c) in e.cases.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_enum_case(ctx, out, base, c);
        }
        out.push_str(" }");
    } else {
        out.push_str(" {\n");
        for c in &e.cases {
            push_indent(out, base + 1);
            write_enum_case(ctx, out, base + 1, c);
            out.push('\n');
        }
        push_indent(out, base);
        out.push('}');
    }
}

fn write_enum_case(ctx: &Ctx, out: &mut String, base: usize, c: &EnumCase) {
    if let Some(span) = c.default {
        out.push_str("#[default]");
        if ctx.one_line(span) {
            out.push(' ');
        } else {
            out.push('\n');
            push_indent(out, base);
        }
    }
    out.push_str(&c.name.name);
    match &c.payload {
        EnumPayload::None => {},
        EnumPayload::Tuple(types) => {
            out.push('(');
            for (i, t) in types.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_ty(ctx, out, t);
            }
            out.push(')');
        },
        // Struct payloads are comma-only in the grammar (no decl_sep), so a
        // multiline payload keeps its commas.
        EnumPayload::Struct(fields) => {
            if ctx.one_line(c.span) {
                out.push_str(" { ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_field(ctx, out, f);
                }
                out.push_str(" }");
            } else {
                out.push_str(" {\n");
                for (i, f) in fields.iter().enumerate() {
                    push_doc(ctx, out, base + 1, &f.doc);
                    push_indent(out, base + 1);
                    write_field(ctx, out, f);
                    if i + 1 < fields.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(out, base);
                out.push('}');
            }
        },
    }
}

fn write_tag(out: &mut String, t: &TagDecl) {
    out.push_str("tag ");
    out.push_str(&t.name.name);
}

fn write_type_alias(ctx: &Ctx, out: &mut String, t: &TypeAliasDecl) {
    out.push_str("type ");
    out.push_str(&t.name.name);
    write_generics(ctx, out, &t.generics);
    out.push_str(" = ");
    write_ty(ctx, out, &t.ty);
}

fn write_trait(ctx: &Ctx, out: &mut String, base: usize, t: &TraitDecl) {
    out.push_str("trait ");
    out.push_str(&t.name.name);
    write_generics(ctx, out, &t.generics);
    write_supers(ctx, out, &t.supers);
    if t.members.is_empty() {
        out.push_str(" {}");
        return;
    }
    // member_list is newline-separated; more than one member can never be
    // inline, so only the single-member case could match one_line.
    if ctx.one_line(t.span) && t.members.len() == 1 {
        out.push_str(" { ");
        write_trait_member(ctx, out, base + 1, &t.members[0]);
        out.push_str(" }");
    } else {
        out.push_str(" {\n");
        for m in &t.members {
            if let TraitMember::Fn(f) = m {
                push_doc(ctx, out, base + 1, &f.doc);
            }
            push_indent(out, base + 1);
            write_trait_member(ctx, out, base + 1, m);
            out.push('\n');
        }
        push_indent(out, base);
        out.push('}');
    }
}

fn write_trait_member(ctx: &Ctx, out: &mut String, base: usize, m: &TraitMember) {
    match m {
        TraitMember::AssocType { name, .. } => {
            out.push_str("type ");
            out.push_str(&name.name);
        },
        TraitMember::Fn(f) => write_fn(ctx, out, base, f),
    }
}

fn write_impl(ctx: &Ctx, out: &mut String, base: usize, i: &ImplDecl) {
    out.push_str("impl");
    write_generics(ctx, out, &i.generics);
    out.push(' ');
    if let Some(tr) = &i.trait_ref {
        write_type_ref(ctx, out, tr);
        out.push_str(" for ");
    }
    write_ty(ctx, out, &i.target);
    if i.fns.is_empty() {
        out.push_str(" {}");
        return;
    }
    if ctx.one_line(i.span) && i.fns.len() == 1 {
        out.push_str(" { ");
        write_fn(ctx, out, base + 1, &i.fns[0]);
        out.push_str(" }");
    } else {
        out.push_str(" {\n");
        for f in &i.fns {
            push_doc(ctx, out, base + 1, &f.doc);
            push_indent(out, base + 1);
            write_fn(ctx, out, base + 1, f);
            out.push('\n');
        }
        push_indent(out, base);
        out.push('}');
    }
}
