//! `show` answers the construction spelling of the data.

use super::{mint::*, *};

/// Synthesizes `impl show for <decl> { fn show(self) -> string { ... } }`.
/// Refuses (returning `None`, diagnostic pushed) when the item is not a
/// data declaration, a payloaded case rides a generic head, or a part
/// refuses structural derivation.
pub(super) fn show_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    if let Some(case) = payloaded_case_of_generic(item) {
        diags.push(payloaded_refused(case, "show"));
        return None;
    }
    match &item.kind {
        ItemKind::Struct(decl) => {
            let fields: Vec<&FieldDecl> = decl
                .members
                .iter()
                .filter_map(|m| match m {
                    StructMember::Field(f) => Some(f),
                    StructMember::Deinit { .. } => None,
                })
                .collect();
            for field in &fields {
                check_part(
                    &field.ty,
                    field.span,
                    "show",
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
            }
            let body = struct_body(&decl.name.name, &fields, mint);
            Some(show_impl_item(attr, &decl.name, &decl.generics, body, mint))
        },
        ItemKind::Enum(decl) => {
            for case in &decl.cases {
                let parts = payload_parts(&case.payload, &case.name.name);
                for (part_name, part, span) in parts {
                    check_part(
                        part,
                        span,
                        "show",
                        &part_name,
                        "enum case payload",
                        linear,
                        diags,
                    )?;
                }
            }
            let body = enum_body(decl, mint);
            Some(show_impl_item(attr, &decl.name, &decl.generics, body, mint))
        },
        ItemKind::Tag(decl) => {
            let body = name_body(&decl.name.name, mint);
            Some(show_impl_item(attr, &decl.name, &[], body, mint))
        },
        _ => {
            diags.push(non_data_refused(attr));
            None
        },
    }
}

/// `show`'s member: the receiver, no other parameter, `string` back.
fn show_member(span: Span, mint: &mut Mint) -> Member {
    Member {
        trait_name: Some("show"),
        fn_name: "show",
        params: vec![self_param(span)],
        ret: name_type(mint, "string"),
    }
}

/// The `show` impl over one already-checked body.
fn show_impl_item(
    attr: &Attribute,
    name: &Ident,
    generics: &[TypeParam],
    body: Expr,
    mint: &mut Mint,
) -> Item {
    let stmts = vec![expr_stmt(body)];
    impl_item(
        attr,
        name,
        generics,
        show_member(attr.span, mint),
        stmts,
        mint,
    )
}

/// The member body of a struct derive: `"{ x: ${self.x}, y: ${self.y} }"`
/// over the printable parts, a `.show()` dispatch over the rest.
fn struct_body(name: &str, fields: &[&FieldDecl], mint: &mut Mint) -> Expr {
    if fields.is_empty() {
        return name_body(name, mint);
    }
    let mut parts = vec![text(fields[0].span, &format!("{name} {{ "))];
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            parts.push(text(field.span, ", "));
        }
        parts.push(text(field.span, &format!("{}: ", field.name.name)));
        parts.push(field_hole(&field.name, &field.ty, mint));
    }
    parts.push(text(fields[fields.len() - 1].span, " }"));
    str_expr(mint.span(), parts)
}

/// The member body of an enum derive: one arm per case printing the
/// construction spelling of its payload.
fn enum_body(decl: &EnumDecl, mint: &mut Mint) -> Expr {
    if decl.cases.is_empty() {
        return name_body(&decl.name.name, mint);
    }
    let scrutinee = self_expr(mint);
    let arms = decl
        .cases
        .iter()
        .map(|case| arm(case, mint))
        .collect::<Vec<_>>();
    Expr::Match {
        span: mint.span(),
        scrutinee: Box::new(scrutinee),
        arms,
    }
}

/// One match arm: `circle(r0) => "circle(${r0})"`, `dot => "dot"`, or
/// `rect { w, h } => "rect { w: ${w}, h: ${h} }"`. The case path is a
/// name use, so each arm mints its own span — the written case spans
/// stay the declaration's uses (resolution records one use per span).
fn arm(case: &EnumCase, mint: &mut Mint) -> MatchArm {
    let pat_span = mint.span();
    let path = Path {
        span: pat_span,
        segments: vec![Ident {
            span: pat_span,
            name: case.name.name.clone(),
        }],
    };
    let mut parts = vec![text(case.span, &case.name.name)];
    let pat = match &case.payload {
        EnumPayload::None => Pattern::Path {
            span: pat_span,
            path,
        },
        EnumPayload::Tuple(items) => {
            parts.push(text(case.span, "("));
            let mut args = Vec::new();
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    parts.push(text(case.span, ", "));
                }
                let binder = Ident {
                    span: mint.span(),
                    name: format!("p{index}"),
                };
                args.push(Pattern::Binding {
                    span: binder.span,
                    name: binder.clone(),
                });
                parts.push(binder_hole(&binder, item, mint));
            }
            parts.push(text(case.span, ")"));
            Pattern::Call {
                span: case.span,
                path,
                args,
            }
        },
        EnumPayload::Struct(fields) => {
            parts.push(text(case.span, " { "));
            let mut pat_fields = Vec::new();
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    parts.push(text(field.span, ", "));
                }
                parts.push(text(field.span, &format!("{}: ", field.name.name)));
                pat_fields.push((field.name.clone(), None));
                parts.push(binder_hole(&field.name, &field.ty, mint));
            }
            let closer = fields.last().map_or(case.span, |f| f.span);
            parts.push(text(closer, " }"));
            Pattern::Struct {
                span: case.span,
                path,
                fields: pat_fields,
            }
        },
    };
    MatchArm {
        span: case.span,
        pat,
        ann: None,
        guard: None,
        body: str_expr(mint.span(), parts),
    }
}

/// The body of a derive whose declaration has no parts: the bare name.
fn name_body(name: &str, mint: &mut Mint) -> Expr {
    str_expr(mint.span(), vec![text(mint.span(), name)])
}

/// One interpolation hole over a part whose value expression is already
/// assembled (a field read in a struct body, a bound binder in an enum
/// arm): a printable primitive converts through the string context
/// itself; every other type dispatches through `show`, so a part
/// without an impl fails at its own span. Every name the hole carries
/// gets its own minted span, because resolution records name uses by
/// span.
fn part_hole(part: Expr, ty: &Type, mint: &mut Mint) -> StrPart {
    let expr = if printable(ty) {
        part
    } else {
        Expr::Call {
            span: mint.span(),
            callee: Box::new(Expr::Field {
                span: mint.span(),
                recv: Box::new(part),
                name: Ident {
                    span: mint.span(),
                    name: "show".to_owned(),
                },
            }),
            args: Vec::new(),
            trailing: None,
            selection: None,
        }
    };
    StrPart::Expr {
        span: expr.span(),
        expr: Box::new(expr),
    }
}

/// The struct-body hole: the part is a field of the receiver.
fn field_hole(field: &Ident, ty: &Type, mint: &mut Mint) -> StrPart {
    let part = Expr::Field {
        span: mint.span(),
        recv: Box::new(self_expr(mint)),
        name: Ident {
            span: mint.span(),
            name: field.name.clone(),
        },
    };
    part_hole(part, ty, mint)
}

/// The enum-arm hole: the part is the arm's bound binder, one minted
/// span shared by the path expression and its identifier.
fn binder_hole(binder: &Ident, ty: &Type, mint: &mut Mint) -> StrPart {
    let part = path_expr(&binder.name, mint.span());
    part_hole(part, ty, mint)
}

/// Whether the written type is one of the interpolation primitives.
fn printable(ty: &Type) -> bool {
    match ty {
        Type::Path { path, .. } => {
            matches!(path.segments.as_slice(), [only] if PRINTABLE.contains(&only.name.as_str()))
        },
        _ => false,
    }
}
