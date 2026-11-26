//! `clone` answers the shallow duplicate, the construction over the
//! borrowed receiver whose Ref parts dup through the placement pass — the
//! one family whose body adds no runtime work beyond the count traffic.

use super::{mint::*, *};

/// Synthesizes `impl clone for <decl> { fn clone(self) -> Self { ... } }`:
/// the shallow duplicate (the body is a construction over
/// the live borrowed receiver, and the placement pass dups each Ref
/// word the construction reads — a count bump per reference, never a
/// deep copy — so the family adds no uop and no native, the one whose
/// body is zero runtime work beyond the count traffic). The struct body
/// is the construction with one `clone` dispatch per field; the enum
/// body matches the receiver and constructs each case from per-part
/// dispatches (every payload shape); the tag body is the receiver
/// itself. A part whose type carries no impl fails at the synthesized
/// call with the standard missing-impl diagnostic over the part's own
/// span; a hand-written part impl answers through it. A part-less
/// struct answers the empty construction — its `Self` spans the one
/// reserved unit word, so the construction carries zero dispatches and
/// the member's return keeps the ABI's one-word minimum. Refuses
/// otherwise exactly like `hash` save the enum gate: a generic enum
/// at the attribute (the synthesized body constructs a case, which
/// types as its own atom, never the applied head), a non-data
/// declaration, a function-typed or `~Copyable` part.
pub(super) fn clone_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    if matches!(&item.kind, ItemKind::Enum(decl) if !decl.generics.is_empty()) {
        diags.push(generic_refused(attr.span, "clone"));
        return None;
    }
    let (name, generics, body) = match &item.kind {
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
                    "clone",
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
            }
            (&decl.name, decl.generics.as_slice(), vec![expr_stmt(
                struct_clone_body(&decl.name, &fields, mint),
            )])
        },
        ItemKind::Enum(decl) => {
            for case in &decl.cases {
                let parts = payload_parts(&case.payload, &case.name.name);
                for (part_name, part, span) in parts {
                    check_part(
                        part,
                        span,
                        "clone",
                        &part_name,
                        "enum case payload",
                        linear,
                        diags,
                    )?;
                }
            }
            (&decl.name, decl.generics.as_slice(), vec![expr_stmt(
                enum_clone_body(decl, mint),
            )])
        },
        ItemKind::Tag(decl) => {
            let none: &[TypeParam] = &[];
            (&decl.name, none, vec![expr_stmt(self_expr(mint))])
        },
        _ => {
            diags.push(non_data_refused(attr));
            return None;
        },
    };
    Some(impl_item(
        attr,
        name,
        generics,
        clone_member(mint),
        body,
        mint,
    ))
}

/// `clone`'s member, spelled as its trait member spells it: the borrowed
/// receiver alone, `Self` back.
fn clone_member(mint: &mut Mint) -> Member {
    Member {
        trait_name: Some("clone"),
        fn_name: "clone",
        params: vec![self_param(mint.span())],
        ret: name_type(mint, "Self"),
    }
}

/// The clone member's struct body: the construction with one `clone`
/// dispatch per field, `X { f: self.f.clone(), ... }`; a part-less
/// struct's construction carries no field, so the duplicate is the
/// fresh value over the receiver's reserved unit word alone. Every
/// name the literal carries mints its own span (the struct path, each
/// field name), because resolution records name uses by span.
fn struct_clone_body(name: &Ident, fields: &[&FieldDecl], mint: &mut Mint) -> Expr {
    let path_span = mint.span();
    let path = Path {
        span: path_span,
        segments: vec![Ident {
            span: path_span,
            name: name.name.clone(),
        }],
    };
    let literal = fields
        .iter()
        .map(|field| {
            let field_name = Ident {
                span: mint.span(),
                name: field.name.name.clone(),
            };
            let part = field_read("self", &field.name, mint);
            (field_name, Some(trait_call(part, "clone", mint)))
        })
        .collect();
    Expr::Struct {
        span: mint.span(),
        path,
        fields: literal,
    }
}

/// The clone member's enum body: one match arm per case constructing
/// the case from per-part `clone` dispatches — a unit case names
/// itself, a tuple case calls with the bound parts in order, a struct
/// case calls with named arguments over them. An empty enum never
/// inhabits a value, so its arms never run and the body is the
/// receiver (the checker refuses the impl head itself).
fn enum_clone_body(decl: &EnumDecl, mint: &mut Mint) -> Expr {
    if decl.cases.is_empty() {
        return self_expr(mint);
    }
    let arms = decl
        .cases
        .iter()
        .map(|case| {
            let (pat, binders) = case_pattern(case, "p", mint);
            // One minted span shared by the construction's path and its
            // identifier, whatever the payload shape: resolution records
            // the case's name use at the identifier's span and lowering
            // looks it up by the expression's, which must coincide.
            let span = mint.span();
            let callee = Path {
                span,
                segments: vec![Ident {
                    span,
                    name: case.name.name.clone(),
                }],
            };
            let value = match &case.payload {
                EnumPayload::None => Expr::Path { span, path: callee },
                EnumPayload::Tuple(items) => {
                    let args = binders
                        .iter()
                        .zip(items.iter())
                        .map(|(binder, _)| CallArg {
                            span: mint.span(),
                            name: None,
                            value: trait_call(binder_use(binder, mint), "clone", mint),
                        })
                        .collect();
                    Expr::Call {
                        span: mint.span(),
                        callee: Box::new(Expr::Path { span, path: callee }),
                        args,
                        trailing: None,
                        selection: None,
                    }
                },
                EnumPayload::Struct(fields) => {
                    let args = binders
                        .iter()
                        .zip(fields.iter())
                        .map(|(binder, field)| CallArg {
                            span: mint.span(),
                            name: Some(Ident {
                                span: mint.span(),
                                name: field.name.name.clone(),
                            }),
                            value: trait_call(binder_use(binder, mint), "clone", mint),
                        })
                        .collect();
                    Expr::Call {
                        span: mint.span(),
                        callee: Box::new(Expr::Path { span, path: callee }),
                        args,
                        trailing: None,
                        selection: None,
                    }
                },
            };
            MatchArm {
                span: case.span,
                pat,
                ann: None,
                guard: None,
                body: value,
            }
        })
        .collect::<Vec<_>>();
    Expr::Match {
        span: mint.span(),
        scrutinee: Box::new(self_expr(mint)),
        arms,
    }
}
