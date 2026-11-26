//! `to_json` answers the encode walk, from_json's mirror — the
//! receiverless inherent member `fn to_json(v: Self) -> json` building
//! the document through the `encoder` trait members alone over one
//! seed (`json.default()`, the driver face's empty-document
//! convention), the plain `to_json: (json) -> string` writer staying
//! the composition point: the struct body fills one object map with
//! one member per field — a scalar field through its `enc_*`
//! construction inline (the four widths), a list field
//! through one document element per position, a field whose type
//! spells no construction through that type's own `to_json` member (a
//! type carrying none failing at the synthesized call with the
//! standard missing-member diagnostic) — an optional field's presence
//! setting its member and its absence leaving the member out (the
//! decode mirror: the absent member reads back as `none`); the enum
//! body matches the receiver, the unit cases answering their names
//! through `enc_string`, the sole payloaded case its bare payload
//! shape, and several payloaded cases one-member objects keyed by
//! their own names; a zero-case
//! enum hosts the member over its name-solved head, the body vacuous
//! over the impossible receiver and answering the driver's empty
//! document. The walk refuses: a field whose
//! type spells no write (a tuple, a union, a shape the case vocabulary
//! does not name). An optional nested inside
//! a part (a `t?` list element, an optional payload field) narrows at
//! its consumer — the presence narrow over the element binder or the
//! payload binder, absence contributing nothing.

use super::{mint::*, *};

/// Synthesizes `impl <decl> { fn to_json(v: Self) -> json { ... } }`:
/// the receiverless encode member over the json enum's own case
/// constructions (the constructed-value seam this engine alone owns),
/// the plain `to_json: (json) -> string` writer staying the composition
/// point. The struct body fills one object map with one member per
/// field; the enum body matches the receiver, the unit cases answering
/// their json_string names and the one struct-payload case its object.
/// A field whose type spells no case construction dispatches that
/// type's own `to_json` member (a type carrying none failing at the
/// synthesized call with the standard missing-member diagnostic, the
/// bare-name precedent); a shape the walk cannot spell refuses at the
/// attribute. An optional nested inside a part (a `t?` list element,
/// an optional payload field) narrows at its consumer: the presence
/// narrow over the element or payload binder, absence contributing
/// nothing (the decode mirror). Refuses
/// otherwise like the matching families: a payloaded case over a
/// generic head, a non-data declaration, a function-typed or
/// `~Copyable` part; a tag carries no members to walk, so it refuses
/// naming itself.
pub(super) fn to_json_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    if let Some(case) = payloaded_case_of_generic(item) {
        diags.push(payloaded_refused(case, "to_json"));
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
            let params: Vec<String> = decl.generics.iter().map(|p| p.name.name.clone()).collect();
            for field in &fields {
                check_part(
                    &field.ty,
                    field.span,
                    "to_json",
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
                if mentions_param(&field.ty, &params) {
                    diags.push(part_refused(
                        field.span,
                        "to_json",
                        "field",
                        &field.name.name,
                        "a type variable carries no json member (the members are type-keyed, \
                         never bound)",
                    ));
                    return None;
                }
            }
            let mut fresh = 0;
            let seed = doc_seed_local(mint);
            let map = fresh_binder(&mut fresh, "m");
            let empty = map_empty(mint);
            let mut stmts = vec![seed, local(mint, &map, empty, true)];
            let rows = member_rows(
                &fields
                    .iter()
                    .map(|field| (*field, field_read("v", &field.name, mint)))
                    .collect::<Vec<_>>(),
                &map,
                mint,
                &mut fresh,
                diags,
            )?;
            stmts.extend(rows);
            let map_read = path_expr(&map, mint.span());
            let seed_read = path_expr(SEED, mint.span());
            stmts.push(expr_stmt(member_call(
                seed_read,
                "enc_object",
                vec![map_read],
                mint,
            )));
            Some(impl_item(
                attr,
                &decl.name,
                &decl.generics,
                to_json_member(mint),
                stmts,
                mint,
            ))
        },
        ItemKind::Enum(decl) => {
            // A zero-case enum hosts the encode member over the
            // name-solved head: the receiver cannot exist, so the body
            // is vacuous and answers the driver's empty document.
            if decl.cases.is_empty() {
                let body = vec![expr_stmt(doc_seed(mint))];
                return Some(impl_item(
                    attr,
                    &decl.name,
                    &decl.generics,
                    to_json_member(mint),
                    body,
                    mint,
                ));
            }
            // Every payload mix encodes by the discriminator; the body's own
            // unlowerable single-arm member test.
            let body = vec![
                doc_seed_local(mint),
                expr_stmt(enum_encode_body(decl, mint, diags)?),
            ];
            Some(impl_item(
                attr,
                &decl.name,
                &decl.generics,
                to_json_member(mint),
                body,
                mint,
            ))
        },
        ItemKind::Tag(decl) => {
            diags.push(part_refused(
                attr.span,
                "to_json",
                "declaration",
                &decl.name.name,
                "a tag declares no members, so no json shape answers it",
            ));
            None
        },
        _ => {
            diags.push(non_data_refused(attr));
            None
        },
    }
}

/// `to_json`'s member: receiverless in the type's own impl (the
/// `default` shape), one borrowed `Self` parameter, the json value
/// back.
fn to_json_member(mint: &mut Mint) -> Member {
    let param = Param {
        span: mint.span(),
        mode: ParamMode::Borrow,
        name: Ident {
            span: mint.span(),
            name: "v".to_owned(),
        },
        ty: Some(name_type(mint, "Self")),
        default: None,
    };
    Member {
        trait_name: None,
        fn_name: "to_json",
        params: vec![param],
        ret: name_type(mint, "json"),
    }
}

/// The empty object map the member rows fill: `map[string, json]()`,
/// the collection face's empty construction.
fn map_empty(mint: &mut Mint) -> Expr {
    let apply_span = mint.span();
    let head = Ident {
        span: mint.span(),
        name: "map".to_owned(),
    };
    let key = path_expr("string", mint.span());
    let value = path_expr("json", mint.span());
    let callee = Expr::MapApply {
        span: apply_span,
        head,
        key: Box::new(key),
        value: Box::new(value),
    };
    Expr::Call {
        span: mint.span(),
        callee: Box::new(callee),
        args: Vec::new(),
        trailing: None,
        selection: None,
    }
}

/// The object walk's member rows: per field the staged value write then
/// one `map_set` — a required field's write directly, an optional
/// field's inside the presence narrow, absence leaving the member out
/// (the decode mirror: the absent member reads back as `none`). The
/// source expression is the field read (a struct target) or the arm's
/// bound binder (an enum payload field).
fn member_rows(
    fields: &[(&FieldDecl, Expr)],
    map: &str,
    mint: &mut Mint,
    fresh: &mut usize,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<Stmt>> {
    let mut stmts = Vec::with_capacity(fields.len() * 2);
    for &(field, ref source) in fields {
        let key = key_literal(mint, &field.name.name);
        let ty = paren_free(&field.ty);
        if let Type::Optional { inner, .. } = ty {
            let binder = fresh_binder(fresh, "o");
            let inner_name = fresh_binder(fresh, "n");
            let narrowed = path_expr(&inner_name, mint.span());
            let (rows, value) = json_value(
                inner,
                narrowed,
                mint,
                fresh,
                field.span,
                &field.name.name,
                "field",
                diags,
            )?;
            let mut set = rows;
            set.push(member_set(mint, map, key, value));
            stmts.push(local(mint, &binder, source.clone(), false));
            stmts.push(presence_narrow(mint, &binder, &inner_name, set));
        } else {
            let (rows, value) = json_value(
                ty,
                source.clone(),
                mint,
                fresh,
                field.span,
                &field.name.name,
                "field",
                diags,
            )?;
            stmts.extend(rows);
            stmts.push(member_set(mint, map, key, value));
        }
    }
    Some(stmts)
}

/// One member's `set`-member statement at minted spans.
fn member_set(mint: &mut Mint, map: &str, key: Expr, value: Expr) -> Stmt {
    let map_read = path_expr(map, mint.span());
    expr_stmt(member_call(map_read, "set", vec![key, value], mint))
}

/// The value write keyed by one written type over the value expression
/// already assembled (a field read, an arm's bound binder, a for-bound
/// element): a scalar writes its case construction inline, the four
/// widths the case vocabulary cannot spell from (byte, u32, char,
/// bytes) through the json face's own encode helpers;
/// or application type dispatches its own `to_json` member (a type
/// carrying none failing at the synthesized call with the standard
/// missing-member diagnostic); a list walks one json element per
/// position into a fresh array — a `t?` element narrowing at its
/// consumer, absence skipping the element (the decode mirror). Every
/// other shape refuses at the attribute; an optional reaching here
/// directly (a nested optional its consumer sites do not narrow)
/// keeps the refusal, since the narrow wraps the consumer's own
/// statement and this walk cannot spell it twice.
fn json_value(
    ty: &Type,
    source: Expr,
    mint: &mut Mint,
    fresh: &mut usize,
    span: Span,
    part: &str,
    what: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Vec<Stmt>, Expr)> {
    let ty = paren_free(ty);
    match ty {
        Type::Optional { .. } => {
            diags.push(part_refused(
                span,
                "to_json",
                what,
                part,
                "a nested optional the consumer sites do not narrow spells no second presence \
                 narrow at this walk",
            ));
            None
        },
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                diags.push(part_refused(
                    span,
                    "to_json",
                    what,
                    part,
                    "a multi-segment path spells no write",
                ));
                return None;
            };
            if args.is_empty() {
                // The driver face's constructions: every scalar and
                // width writes through one `enc_*` member over the walk's
                // seed (byte/u32 as numbers, char as its one-character string, bytes as
                // the number array), so the walk names no json case.
                let enc = match only.name.as_str() {
                    "int" => Some("enc_int"),
                    "float" => Some("enc_float"),
                    "bool" => Some("enc_bool"),
                    "string" => Some("enc_string"),
                    "byte" => Some("enc_byte"),
                    "u32" => Some("enc_u32"),
                    "char" => Some("enc_char"),
                    "bytes" => Some("enc_bytes"),
                    _ => None,
                };
                if let Some(enc) = enc {
                    let seed = path_expr(SEED, mint.span());
                    return Some((Vec::new(), member_call(seed, enc, vec![source], mint)));
                }
                // The recursive composition: the part's own type encodes
                // through its `to_json` member.
                let recv = path_expr(&only.name, mint.span());
                return Some((Vec::new(), type_keyed_encode(mint, &recv, source)));
            }
            if only.name == "list"
                && let [element] = args.as_slice()
            {
                return list_write(element, source, mint, fresh, span, part, what, diags);
            }
            // Every other application encodes through the applied
            // type's own member over the fn-type carrier (the `default`
            // family's applied-receiver precedent).
            let recv = Expr::FnType {
                span: mint.span(),
                ty: Box::new(minted_type(ty, mint)),
            };
            Some((Vec::new(), type_keyed_encode(mint, &recv, source)))
        },
        _ => {
            diags.push(part_refused(
                span,
                "to_json",
                what,
                part,
                "this shape spells no json write",
            ));
            None
        },
    }
}

/// One type-keyed `to_json` call: `<recv>.to_json(source)`.
fn type_keyed_encode(mint: &mut Mint, recv: &Expr, source: Expr) -> Expr {
    let callee = Expr::Field {
        span: mint.span(),
        recv: Box::new(recv.clone()),
        name: Ident {
            span: mint.span(),
            name: "to_json".to_owned(),
        },
    };
    Expr::Call {
        span: mint.span(),
        callee: Box::new(callee),
        args: vec![CallArg {
            span: mint.span(),
            name: None,
            value: source,
        }],
        trailing: None,
        selection: None,
    }
}

/// The array walk over a `list[t]` part: one json element per position
/// into a fresh accumulator, the element write keyed by the element's
/// own type (an element shape spelling no write refusing at the
/// attribute). A `t?` element narrows at the push — the presence
/// narrow over the for-bound element, absence skipping the element
/// (the decode mirror: the element never reads back).
fn list_write(
    element: &Type,
    source: Expr,
    mint: &mut Mint,
    fresh: &mut usize,
    span: Span,
    part: &str,
    what: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Vec<Stmt>, Expr)> {
    let out = fresh_binder(fresh, "w");
    let at = fresh_binder(fresh, "e");
    let empty = Expr::Call {
        span: mint.span(),
        callee: Box::new(Expr::Index {
            span: mint.span(),
            recv: Box::new(path_expr("list", mint.span())),
            index: Box::new(path_expr("json", mint.span())),
        }),
        args: Vec::new(),
        trailing: None,
        selection: None,
    };
    let body = if let Type::Optional { inner, .. } = paren_free(element) {
        let inner_name = fresh_binder(fresh, "n");
        let narrowed = path_expr(&inner_name, mint.span());
        let (rows, value) = json_value(inner, narrowed, mint, fresh, span, part, what, diags)?;
        let mut arm = rows;
        let out_read = path_expr(&out, mint.span());
        arm.push(expr_stmt(member_call(out_read, "push", vec![value], mint)));
        vec![presence_narrow(mint, &at, &inner_name, arm)]
    } else {
        let (rows, value) = json_value(
            element,
            path_expr(&at, mint.span()),
            mint,
            fresh,
            span,
            part,
            what,
            diags,
        )?;
        let mut body = rows;
        let out_read = path_expr(&out, mint.span());
        body.push(expr_stmt(member_call(out_read, "push", vec![value], mint)));
        body
    };
    let pat_span = mint.span();
    let walk = Expr::For {
        span: mint.span(),
        pat: Pattern::Binding {
            span: pat_span,
            name: Ident {
                span: pat_span,
                name: at,
            },
        },
        iter: Box::new(source),
        body: Box::new(Expr::Block(BlockExpr {
            span: mint.span(),
            stmts: body,
            trailing_trivia: Vec::new(),
        })),
        comptime: false,
    };
    let stmts = vec![local(mint, &out, empty, true), expr_stmt(walk)];
    let out_read = path_expr(&out, mint.span());
    let seed = path_expr(SEED, mint.span());
    Some((stmts, member_call(seed, "enc_array", vec![out_read], mint)))
}

/// The enum walk: the receiver's case names the arm — the unit cases
/// answering their json_string names, the sole payloaded case its bare
/// payload shape (a struct payload's members as a json object, a tuple
/// payload's positions as a json array), and several payloaded cases
/// one-member objects keyed by their own names.
fn enum_encode_body(decl: &EnumDecl, mint: &mut Mint, diags: &mut Vec<Diagnostic>) -> Option<Expr> {
    debug_assert!(!decl.cases.is_empty(), "the empty enum hosts its own body");
    // Several payloaded cases wrap; the sole
    // payloaded case answers its bare shape.
    let wrap = decl
        .cases
        .iter()
        .filter(|case| !matches!(case.payload, EnumPayload::None))
        .count()
        >= 2;
    let mut fresh = 0;
    let arms = decl
        .cases
        .iter()
        .map(|case| {
            let (pat, binders) = case_pattern(case, "p", mint);
            let body = match &case.payload {
                EnumPayload::None => {
                    let name = key_literal(mint, &case.name.name);
                    let seed = path_expr(SEED, mint.span());
                    member_call(seed, "enc_string", vec![name], mint)
                },
                EnumPayload::Struct(fields) => {
                    let map = fresh_binder(&mut fresh, "m");
                    let rows = member_rows(
                        &fields
                            .iter()
                            .zip(binders.iter())
                            .map(|(field, binder)| (field, binder_use(binder, mint)))
                            .collect::<Vec<_>>(),
                        &map,
                        mint,
                        &mut fresh,
                        diags,
                    )?;
                    let empty = map_empty(mint);
                    let mut stmts = vec![local(mint, &map, empty, true)];
                    stmts.extend(rows);
                    let map_read = path_expr(&map, mint.span());
                    let seed = path_expr(SEED, mint.span());
                    let value = member_call(seed, "enc_object", vec![map_read], mint);
                    let (mut stmts, value) = if wrap {
                        wrapped_case(mint, &mut fresh, &case.name.name, stmts, value)
                    } else {
                        (stmts, value)
                    };
                    stmts.push(expr_stmt(value));
                    Expr::Block(BlockExpr {
                        span: mint.span(),
                        stmts,
                        trailing_trivia: Vec::new(),
                    })
                },
                EnumPayload::Tuple(items) => {
                    let out = fresh_binder(&mut fresh, "w");
                    let empty = Expr::Call {
                        span: mint.span(),
                        callee: Box::new(Expr::Index {
                            span: mint.span(),
                            recv: Box::new(path_expr("list", mint.span())),
                            index: Box::new(path_expr("json", mint.span())),
                        }),
                        args: Vec::new(),
                        trailing: None,
                        selection: None,
                    };
                    let mut stmts = vec![local(mint, &out, empty, true)];
                    for (index, item) in items.iter().enumerate() {
                        // a `t?` tuple-payload item narrows at its push —
                        // the presence narrow over the payload binder,
                        // absence skipping the position (the decode
                        // mirror: the element never reads back)
                        let position = if let Type::Optional { inner, .. } = paren_free(item) {
                            let inner_name = fresh_binder(&mut fresh, "n");
                            let narrowed = path_expr(&inner_name, mint.span());
                            let (rows, value) = json_value(
                                inner,
                                narrowed,
                                mint,
                                &mut fresh,
                                case.span,
                                &case.name.name,
                                "enum case",
                                diags,
                            )?;
                            let mut arm = rows;
                            let out_read = path_expr(&out, mint.span());
                            arm.push(expr_stmt(member_call(out_read, "push", vec![value], mint)));
                            vec![presence_narrow(
                                mint,
                                &binders[index].name,
                                &inner_name,
                                arm,
                            )]
                        } else {
                            let (rows, value) = json_value(
                                item,
                                binder_use(&binders[index], mint),
                                mint,
                                &mut fresh,
                                case.span,
                                &case.name.name,
                                "enum case",
                                diags,
                            )?;
                            let mut set = rows;
                            let out_read = path_expr(&out, mint.span());
                            set.push(expr_stmt(member_call(out_read, "push", vec![value], mint)));
                            set
                        };
                        stmts.extend(position);
                    }
                    let out_read = path_expr(&out, mint.span());
                    let seed = path_expr(SEED, mint.span());
                    let value = member_call(seed, "enc_array", vec![out_read], mint);
                    let (mut stmts, value) = if wrap {
                        wrapped_case(mint, &mut fresh, &case.name.name, stmts, value)
                    } else {
                        (stmts, value)
                    };
                    stmts.push(expr_stmt(value));
                    Expr::Block(BlockExpr {
                        span: mint.span(),
                        stmts,
                        trailing_trivia: Vec::new(),
                    })
                },
            };
            Some(MatchArm {
                span: case.span,
                pat,
                ann: None,
                guard: None,
                body,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Expr::Match {
        span: mint.span(),
        scrutinee: Box::new(path_expr("v", mint.span())),
        arms,
    })
}

/// The discriminator's encode half: the case's bare payload value set
/// as the one member its own name keys, the wrap answered as the
/// object carrying that member.
fn wrapped_case(
    mint: &mut Mint,
    fresh: &mut usize,
    case_name: &str,
    rows: Vec<Stmt>,
    value: Expr,
) -> (Vec<Stmt>, Expr) {
    let map = fresh_binder(fresh, "m");
    let key = key_literal(mint, case_name);
    let mut stmts = Vec::with_capacity(rows.len() + 2);
    let empty = map_empty(mint);
    stmts.push(local(mint, &map, empty, true));
    stmts.extend(rows);
    stmts.push(member_set(mint, &map, key, value));
    let map_read = path_expr(&map, mint.span());
    let seed = path_expr(SEED, mint.span());
    (stmts, member_call(seed, "enc_object", vec![map_read], mint))
}
