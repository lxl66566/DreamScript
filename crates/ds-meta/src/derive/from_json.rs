//! `from_json` answers the decode walk over the @std driver face —
//! a receiverless inherent member `fn from_json(v: json) -> Self |
//! parse_err` whose body reads through the `decoder` trait members
//! alone, never a comptime extension and never a json face name: the
//! struct body guards the object shape (`v.dec_object()?`, so a
//! non-object answers `parse_err` whatever its fields), reads one
//! member per field (`dec_member`, a required member none-testing into
//! `parse_err`, an optional member reading absence as `none`), and
//! answers the construction — a scalar field through its reader inline
//! (byte, u32, char, and bytes included: a
//! json number as byte or u32, a one-character json string as char, a
//! number array as bytes), a list
//! field through the array read plus
//! one element walk per position, a field whose type spells no reader
//! through that type's own `from_json` member (a type carrying none
//! fails at the synthesized call with the standard missing-member
//! diagnostic); the enum body probes the driver reads — the unit cases
//! matching the string read against their own names, the sole payloaded
//! case from its payload's bare shape (the object read for a struct
//! payload, the array read for a tuple payload), and several payloaded
//! cases each from the member its own name keys, any other shape answering `parse_err` at the
//! probe chain's tail; a zero-case enum hosts the member over its
//! name-solved head, the body answering `parse_err` unconditionally
//! (no member case can match). The walk refuses: a field whose type spells no read (a
//! tuple, a union, a shape the reader vocabulary does not name).

use super::{mint::*, *};

/// Synthesizes `impl <decl> { fn from_json(v: json) -> Self | parse_err { ... } }`:
/// the receiverless decode member over the landed typed readers
/// (`json_member` plus the `json_as_*` family, each read answering
/// `parse_err` on a wrong shape) — the per-target walk the ds-source
/// face pinned as a plain user function, now the engine's own
/// synthesis. The struct body is the object guard plus one member read
/// per field plus the construction; the enum body matches the value's
/// shape, the unit cases reading from their own names and the one
/// struct-payload case from its members. A field whose type spells no
/// reader dispatches that type's own `from_json` member (a type
/// carrying none fails at the synthesized call with the standard
/// missing-member diagnostic, the bare-name precedent); a shape the
/// walk cannot spell refuses at the attribute. Refuses otherwise like
/// the constructing families: a generic enum (the body constructs a
/// case), a non-data declaration, a function-typed or `~Copyable`
/// part; a tag carries no members to walk, so it refuses naming
/// itself.
pub(super) fn from_json_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    if matches!(&item.kind, ItemKind::Enum(decl) if !decl.generics.is_empty()) {
        diags.push(generic_refused(attr.span, "from_json"));
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
                    "from_json",
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
                if mentions_param(&field.ty, &params) {
                    diags.push(part_refused(
                        field.span,
                        "from_json",
                        "field",
                        &field.name.name,
                        "a type variable carries no json member (the members are type-keyed, \
                         never bound)",
                    ));
                    return None;
                }
            }
            // the shared failure value: one construction feeds every
            // none-test's early return (the per-site spelling would
            // mint a fresh payload span per field, past the frame slot
            // cap on wide targets); the seed beside it feeds every
            // element default the same way
            let fail = fresh_binder(&mut 0, "f");
            let mut stmts = vec![
                fail_local(mint, &fail),
                doc_seed_local(mint),
                guard_stmt(mint),
            ];
            let (rows, literal) = field_rows(&fields, "v", mint, &mut 0, &fail, diags)?;
            stmts.extend(rows);
            stmts.push(expr_stmt(Expr::Struct {
                span: mint.span(),
                path: minted_path(&decl.name.name, mint),
                fields: literal,
            }));
            Some(impl_item(
                attr,
                &decl.name,
                &decl.generics,
                from_json_member(mint),
                stmts,
                mint,
            ))
        },
        ItemKind::Enum(decl) => {
            // A zero-case enum hosts the decode member over the
            // name-solved head: no member case can match, so the body
            // answers `parse_err` unconditionally and never binds the
            // shared fail seed.
            if decl.cases.is_empty() {
                let body = vec![expr_stmt(parse_err_expr(mint))];
                return Some(impl_item(
                    attr,
                    &decl.name,
                    &decl.generics,
                    from_json_member(mint),
                    body,
                    mint,
                ));
            }
            // Every payload mix decodes by the discriminator: the sole case
            // from its bare shape, several from their own member keys.
            // member keys).
            let fail = fresh_binder(&mut 0, "f");
            let body = vec![
                fail_local(mint, &fail),
                doc_seed_local(mint),
                expr_stmt(enum_decode_body(decl, mint, &fail, diags)?),
            ];
            Some(impl_item(
                attr,
                &decl.name,
                &decl.generics,
                from_json_member(mint),
                body,
                mint,
            ))
        },
        ItemKind::Tag(decl) => {
            diags.push(part_refused(
                attr.span,
                "from_json",
                "declaration",
                &decl.name.name,
                "a tag declares no members, so no json shape names it",
            ));
            None
        },
        _ => {
            diags.push(non_data_refused(attr));
            None
        },
    }
}

/// `from_json`'s member: receiverless in the type's own impl (the
/// `default` shape), one `json` parameter, the target-or-`parse_err`
/// union back.
fn from_json_member(mint: &mut Mint) -> Member {
    let param = Param {
        span: mint.span(),
        mode: ParamMode::Borrow,
        name: Ident {
            span: mint.span(),
            name: "v".to_owned(),
        },
        ty: Some(name_type(mint, "json")),
        default: None,
    };
    let ret = {
        let span = mint.span();
        Type::Union {
            span,
            left: Box::new(name_type(mint, "Self")),
            right: Box::new(name_type(mint, "parse_err")),
        }
    };
    Member {
        trait_name: None,
        fn_name: "from_json",
        params: vec![param],
        ret,
    }
}

/// The struct walk's object guard: a bare `v.dec_object()?` statement
/// over the driver face, so a non-object document answers `parse_err`
/// whatever the fields read (an all-optional or empty target included).
fn guard_stmt(mint: &mut Mint) -> Stmt {
    let doc = path_expr("v", mint.span());
    let read = member_call(doc, "dec_object", Vec::new(), mint);
    expr_stmt(Expr::Try {
        span: mint.span(),
        expr: Box::new(read),
    })
}

/// One minted construction row: the field's name and its initializer.
type FieldRow = (Ident, Option<Expr>);

/// The per-field member reads (the staging statements) and the
/// construction's field literal: one `json_member` read per field over
/// `source` (the struct's `v`, the enum's narrowed `v`), a required
/// member none-testing into `parse_err` while an optional member leaves
/// absence to its value read, then the value read keyed by the field's
/// type. A field whose type spells no read refuses at the attribute,
/// naming the field.
fn field_rows(
    fields: &[&FieldDecl],
    source: &str,
    mint: &mut Mint,
    fresh: &mut usize,
    fail: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Vec<Stmt>, Vec<FieldRow>)> {
    let mut stmts = Vec::with_capacity(fields.len() * 2);
    let mut literal = Vec::with_capacity(fields.len());
    for field in fields {
        let member = fresh_binder(fresh, "m");
        let source_read = path_expr(source, mint.span());
        let key = key_literal(mint, &field.name.name);
        let read = member_call(source_read, "dec_member", vec![key], mint);
        stmts.push(local(mint, &member, read, false));
        if !matches!(field.ty, Type::Optional { .. }) {
            stmts.push(none_test(mint, &member, fail));
        }
        let value = value_read(
            &field.ty,
            &member,
            mint,
            fresh,
            field.span,
            &field.name.name,
            "field",
            fail,
            diags,
        )?;
        stmts.extend(value.0);
        literal.push((
            Ident {
                span: mint.span(),
                name: field.name.name.clone(),
            },
            Some(value.1),
        ));
    }
    Some((stmts, literal))
}

/// The value read keyed by one written type over the json value named
/// `source`: a scalar reads through its reader inline (byte, u32, char,
/// and bytes included, each through the json face's own width reader —
/// a json number as byte or u32, a
/// one-character json string as char, a number array as bytes, every
/// wrong shape answering `parse_err`); a bare-name or
/// application type dispatches its own `from_json` member (the
/// `is parse_err` narrow, since the `?` kept-set assumes word
/// payloads); a list reads the array then walks one element per
/// position into a fresh accumulator, a `t?` element narrowing at its
/// push (the presence narrow over the per-element binder, absence
/// contributing nothing — to_json's mirror); an optional reads
/// absence as `none` and presence through its inner read — a list inner through
/// the statement-position member row (to_json's mirror, since the
/// array walk's statements spell no value-position else that carries
/// them). Every other shape refuses at the attribute.
#[allow(clippy::too_many_arguments)]
fn value_read(
    ty: &Type,
    source: &str,
    mint: &mut Mint,
    fresh: &mut usize,
    span: Span,
    part: &str,
    what: &str,
    fail: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Vec<Stmt>, Expr)> {
    match ty {
        Type::Paren { inner, .. } => {
            value_read(inner, source, mint, fresh, span, part, what, fail, diags)
        },
        Type::Optional { inner, .. } => {
            if let Type::Path { path, args, .. } = paren_free(inner)
                && path.segments.len() == 1
                && path.segments[0].name == "list"
                && let [element] = args.as_slice()
            {
                return optional_list_row(
                    element, ty, source, mint, fresh, span, part, what, fail, diags,
                );
            }
            let binder = fresh_binder(fresh, "o");
            let (els, staged_narrow) = optional_else(inner, source, mint, span, part, what, diags)?;
            let source_read = path_expr(source, mint.span());
            let cond = Expr::Is {
                span: mint.span(),
                expr: Box::new(source_read),
                ty: Box::new(name_type(mint, "none")),
            };
            let absent = path_expr("none", mint.span());
            let then = block_of(mint, absent);
            let init = Expr::If {
                span: mint.span(),
                cond: Box::new(cond),
                then,
                else_: Some(Box::new(els)),
            };
            let mut stmts = vec![local(mint, &binder, init, false)];
            if staged_narrow {
                // The recursive call answers its own error union into
                // the if's union, so the wrong-shape narrow runs at
                // statement position over the bound optional.
                stmts.push(err_test(mint, &binder, fail));
            }
            Some((stmts, path_expr(&binder, mint.span())))
        },
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                diags.push(part_refused(
                    span,
                    "from_json",
                    what,
                    part,
                    "a multi-segment path spells no read",
                ));
                return None;
            };
            if args.is_empty() {
                let reader = match only.name.as_str() {
                    "int" => Some("dec_int"),
                    "float" => Some("dec_float"),
                    "bool" => Some("dec_bool"),
                    "string" => Some("dec_string"),
                    "byte" => Some("dec_byte"),
                    "u32" => Some("dec_u32"),
                    "char" => Some("dec_char"),
                    "bytes" => Some("dec_bytes"),
                    _ => None,
                };
                if let Some(reader) = reader {
                    let source_read = path_expr(source, mint.span());
                    let read = member_call(source_read, reader, Vec::new(), mint);
                    return Some((Vec::new(), Expr::Try {
                        span: mint.span(),
                        expr: Box::new(read),
                    }));
                }
                // The recursive composition: the field's own type
                // decodes through its `from_json` member, a type
                // carrying none failing at the synthesized call with
                // the standard missing-member diagnostic.
                let recv = path_expr(&only.name, mint.span());
                let call = type_keyed_decode(mint, &recv, source);
                return Some(member_decode(mint, fresh, call, fail));
            }
            if only.name == "list"
                && let [element] = args.as_slice()
            {
                return list_read(element, source, mint, fresh, span, part, what, fail, diags);
            }
            // Every other application decodes through the applied
            // type's own member over the fn-type carrier (the
            // `default` family's applied-receiver precedent).
            let recv = Expr::FnType {
                span: mint.span(),
                ty: Box::new(minted_type(ty, mint)),
            };
            let call = type_keyed_decode(mint, &recv, source);
            Some(member_decode(mint, fresh, call, fail))
        },
        _ => {
            diags.push(part_refused(
                span,
                "from_json",
                what,
                part,
                "this shape spells no json read",
            ));
            None
        },
    }
}

/// The optional read's else half: a scalar inner answers its reader's
/// `?`, a member-typed inner answers the raw recursive call (its
/// wrong-shape narrow running at statement position after the if — a
/// `return` inside a value-position block is outside the lowering
/// seam); every other inner shape spells no read the else can carry
/// (a list inner never reaches here — the member row owns it).
fn optional_else(
    inner: &Type,
    source: &str,
    mint: &mut Mint,
    span: Span,
    part: &str,
    what: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Expr, bool)> {
    let mut refused = |because: &str| {
        diags.push(part_refused(span, "from_json", what, part, because));
    };
    let inner = match inner {
        Type::Paren { inner, .. } => inner.as_ref(),
        other => other,
    };
    let Type::Path { path, args, .. } = inner else {
        refused("an optional of this shape spells no read");
        return None;
    };
    let [only] = path.segments.as_slice() else {
        refused("a multi-segment path spells no read");
        return None;
    };
    if args.is_empty() {
        let reader = match only.name.as_str() {
            "int" => Some("dec_int"),
            "float" => Some("dec_float"),
            "bool" => Some("dec_bool"),
            "string" => Some("dec_string"),
            "byte" => Some("dec_byte"),
            "u32" => Some("dec_u32"),
            "char" => Some("dec_char"),
            "bytes" => Some("dec_bytes"),
            _ => None,
        };
        if let Some(reader) = reader {
            let source_read = path_expr(source, mint.span());
            let read = member_call(source_read, reader, Vec::new(), mint);
            return Some((
                Expr::Try {
                    span: mint.span(),
                    expr: Box::new(read),
                },
                false,
            ));
        }
        let recv = path_expr(&only.name, mint.span());
        let call = type_keyed_decode(mint, &recv, source);
        return Some((call, true));
    }
    let recv = Expr::FnType {
        span: mint.span(),
        ty: Box::new(minted_type(inner, mint)),
    };
    let call = type_keyed_decode(mint, &recv, source);
    Some((call, true))
}

/// The optional list field's decode row, to_json's member row
/// mirrored: the field's value rides a mutable binder typed by the
/// field's own optional and initialized to `none`, and the presence
/// narrow's catchall arm runs the array walk and stores its
/// accumulator. The walk must run at statement position — a value
/// block whose tail reads the accumulator answers the var's slot, and
/// the block's scope end releases the reference before the join's
/// consumer claims it — so the rows run where to_json's member rows
/// run, and absence leaves the binder at `none`.
// Threads value_read's context plus the field's own type; the minting family shares this shape.
#[allow(clippy::too_many_arguments)]
fn optional_list_row(
    element: &Type,
    ty: &Type,
    source: &str,
    mint: &mut Mint,
    fresh: &mut usize,
    span: Span,
    part: &str,
    what: &str,
    fail: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Vec<Stmt>, Expr)> {
    let binder = fresh_binder(fresh, "o");
    let inner = fresh_binder(fresh, "j");
    let (rows, value) = list_read(element, &inner, mint, fresh, span, part, what, fail, diags)?;
    let mut arm = rows;
    arm.push(assign(mint, &binder, value));
    let narrow = presence_narrow(mint, source, &inner, arm);
    let ann = minted_type(ty, mint);
    let init = path_expr("none", mint.span());
    Some((
        vec![local_typed(mint, &binder, ann, init), narrow],
        path_expr(&binder, mint.span()),
    ))
}

/// One type-keyed `from_json` call: `<recv>.from_json(source)`.
fn type_keyed_decode(mint: &mut Mint, recv: &Expr, source: &str) -> Expr {
    let callee = Expr::Field {
        span: mint.span(),
        recv: Box::new(recv.clone()),
        name: Ident {
            span: mint.span(),
            name: "from_json".to_owned(),
        },
    };
    Expr::Call {
        span: mint.span(),
        callee: Box::new(callee),
        args: vec![CallArg {
            span: mint.span(),
            name: None,
            value: path_expr(source, mint.span()),
        }],
        trailing: None,
        selection: None,
    }
}

/// The array walk over a `list[t]` field: read the array, then one
/// element read per position into a fresh accumulator, the element read
/// keyed by the element's own type (an element shape spelling no read
/// refusing at the attribute). A `t?` element narrows at its push —
/// the presence narrow over the per-element binder, presence reading
/// and pushing, absence contributing nothing to the list (to_json's
/// mirror); the raw per-element read keeps its own `none` side for
/// the narrow to test (the tuple-payload item's discipline).
// the inputs are the field's element shape, the reads' source and
// binder counter, and the refusal vocabulary: one flat list where
// each parameter names a fact
#[allow(clippy::too_many_arguments)]
fn list_read(
    element: &Type,
    source: &str,
    mint: &mut Mint,
    fresh: &mut usize,
    span: Span,
    part: &str,
    what: &str,
    fail: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Vec<Stmt>, Expr)> {
    let array = fresh_binder(fresh, "a");
    let out = fresh_binder(fresh, "w");
    let index = fresh_binder(fresh, "i");
    let at = fresh_binder(fresh, "e");
    // The empty accumulator's element type rides the constructor
    // brackets, the ctor-arg vocabulary the language spells.
    let Ok(element_expr) = bracket_arg(element, mint) else {
        diags.push(part_refused(
            span,
            "from_json",
            what,
            part,
            "the element type spells no constructor-bracket argument",
        ));
        return None;
    };
    let array_source = path_expr(source, mint.span());
    let array_read = member_call(array_source, "dec_array", Vec::new(), mint);
    let read_array = Expr::Try {
        span: mint.span(),
        expr: Box::new(array_read),
    };
    let empty = Expr::Call {
        span: mint.span(),
        callee: Box::new(Expr::Index {
            span: mint.span(),
            recv: Box::new(path_expr("list", mint.span())),
            index: Box::new(element_expr),
        }),
        args: Vec::new(),
        trailing: None,
        selection: None,
    };
    let array_read = path_expr(&array, mint.span());
    let index_read = path_expr(&index, mint.span());
    let get = member_call(array_read, "get", vec![index_read], mint);
    // An optional element keeps the raw read's own `none` side: the
    // narrow below tests absence off it, so the unwrap to the seed
    // must not run first (a required element unwraps, a missing
    // element answering the seed document whose read fails into
    // `parse_err`); the tuple-payload item reads the same shape.
    let elem = if matches!(paren_free(element), Type::Optional { .. }) {
        get
    } else {
        Expr::Call {
            span: mint.span(),
            callee: Box::new(Expr::Field {
                span: mint.span(),
                recv: Box::new(get),
                name: Ident {
                    span: mint.span(),
                    name: "unwrap_or".to_owned(),
                },
            }),
            args: vec![CallArg {
                span: mint.span(),
                name: None,
                value: path_expr(SEED, mint.span()),
            }],
            trailing: None,
            selection: None,
        }
    };
    let mut loop_body = vec![local(mint, &at, elem, false)];
    if let Type::Optional { inner, .. } = paren_free(element) {
        // A `t?` element narrows at its push: the presence narrow
        // over the element binder, presence reading through the
        // element's own reader and pushing, absence contributing
        // nothing to the list (to_json's mirror); a present
        // wrong-shape element (a json null) answers `parse_err`
        // through its reader like the tuple-payload item.
        let inner_name = fresh_binder(fresh, "n");
        let (rows, value) = value_read(
            inner,
            &inner_name,
            mint,
            fresh,
            span,
            part,
            what,
            fail,
            diags,
        )?;
        let mut arm = rows;
        let out_read = path_expr(&out, mint.span());
        arm.push(expr_stmt(member_call(out_read, "push", vec![value], mint)));
        loop_body.push(presence_narrow(mint, &at, &inner_name, arm));
    } else {
        let (rows, value) = value_read(element, &at, mint, fresh, span, part, what, fail, diags)?;
        loop_body.extend(rows);
        let out_read = path_expr(&out, mint.span());
        loop_body.push(expr_stmt(member_call(out_read, "push", vec![value], mint)));
    }
    loop_body.push(assign_add(mint, &index));
    let index_cmp = path_expr(&index, mint.span());
    let array_len_arg = path_expr(&array, mint.span());
    let array_len = member_call(array_len_arg, "len", vec![], mint);
    let cond = Expr::Binary {
        span: mint.span(),
        op: BinaryOp::Lt,
        left: Box::new(index_cmp),
        right: Box::new(array_len),
    };
    let zero = int_answer(mint, "0");
    let stmts = vec![
        local(mint, &array, read_array, false),
        local(mint, &out, empty, true),
        local(mint, &index, zero, true),
        expr_stmt(Expr::While {
            span: mint.span(),
            cond: Box::new(cond),
            body: BlockExpr {
                span: mint.span(),
                stmts: loop_body,
                trailing_trivia: Vec::new(),
            },
        }),
    ];
    Some((stmts, path_expr(&out, mint.span())))
}

/// The enum walk: the value's shape names the arm — the unit cases read
/// from their own names (a json string), the sole payloaded case from
/// its payload's bare shape (a struct payload's members as a json
/// object, a tuple payload's positions as a json array), and several
/// payloaded cases each from the member its own name keys, any other
/// shape answering `parse_err`.
fn enum_decode_body(
    decl: &EnumDecl,
    mint: &mut Mint,
    fail: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<Expr> {
    debug_assert!(!decl.cases.is_empty(), "the empty enum hosts its own body");
    let payloaded: Vec<&EnumCase> = decl
        .cases
        .iter()
        .filter(|case| !matches!(case.payload, EnumPayload::None))
        .collect();
    let mut fresh = 0;
    match payloaded.as_slice() {
        [] => {
            let binder = fresh_binder(&mut fresh, "s");
            let chain = name_chain(decl, &binder, mint, fail)
                .expect("a non-empty enum with no payloaded case carries one");
            let tail = fail_block(mint, fail);
            Some(shape_probe(mint, "dec_string", &binder, chain, tail))
        },
        [only] => {
            let arm = Expr::Block(case_decode_arm(only, "v", mint, &mut fresh, fail, diags)?);
            // The sole payloaded case's bare shape names it: the object
            // read for a struct payload, the array read for a tuple
            // payload — a driver probe whose wrong shape falls to the
            // failure tail (the arm re-reads over `v`, so the probe's
            // binder serves the narrow alone).
            let read = match only.payload {
                EnumPayload::Struct(_) => "dec_object",
                EnumPayload::Tuple(_) => "dec_array",
                EnumPayload::None => unreachable!("the filter keeps payloaded cases alone"),
            };
            let binder = fresh_binder(&mut fresh, "g");
            let fail_tail = fail_block(mint, fail);
            let tail = shape_probe(mint, read, &binder, arm, fail_tail);
            let string_binder = fresh_binder(&mut fresh, "s");
            match name_chain(decl, &string_binder, mint, fail) {
                Some(chain) => Some(shape_probe(mint, "dec_string", &string_binder, chain, tail)),
                None => Some(tail),
            }
        },
        many => {
            let probe = payload_probe(many, mint, &mut fresh, fail, diags)?;
            let binder = fresh_binder(&mut fresh, "s");
            match name_chain(decl, &binder, mint, fail) {
                Some(chain) => Some(shape_probe(mint, "dec_string", &binder, chain, probe)),
                None => Some(probe),
            }
        },
    }
}

/// The probe chain's failure tail: a block answering the shared fail
/// binder.
fn fail_block(mint: &mut Mint, fail: &str) -> Expr {
    Expr::Block(BlockExpr {
        span: mint.span(),
        stmts: vec![expr_stmt(path_expr(fail, mint.span()))],
        trailing_trivia: Vec::new(),
    })
}

/// One driver probe over the document `v`: the read bound, a
/// `parse_err` narrow falling to `rest`, the arm over the success. The
/// decode's shape dispatch — the driver face's own reads carry it, so
/// no json case name appears here.
fn shape_probe(mint: &mut Mint, read: &str, binder: &str, arm: Expr, rest: Expr) -> Expr {
    let doc = path_expr("v", mint.span());
    let read = member_call(doc, read, Vec::new(), mint);
    let cond = Expr::Is {
        span: mint.span(),
        expr: Box::new(path_expr(binder, mint.span())),
        ty: Box::new(name_type(mint, "parse_err")),
    };
    let probe = Expr::If {
        span: mint.span(),
        cond: Box::new(cond),
        then: BlockExpr {
            span: mint.span(),
            stmts: vec![expr_stmt(rest)],
            trailing_trivia: Vec::new(),
        },
        else_: Some(Box::new(arm)),
    };
    Expr::Block(BlockExpr {
        span: mint.span(),
        stmts: vec![local(mint, binder, read, false), expr_stmt(probe)],
        trailing_trivia: Vec::new(),
    })
}

/// One payloaded case's decode block over the json value named
/// `source` (the whole document for a sole payloaded case, the member
/// its own name keys among several): a struct payload reads one member
/// per field and constructs through named arguments; a tuple payload
/// reads the array and one position per element, constructing through
/// positional arguments. The bare shape stays the same whatever names
/// the case — the wrap is the probe chain's, never the payload's.
fn case_decode_arm(
    case: &EnumCase,
    source: &str,
    mint: &mut Mint,
    fresh: &mut usize,
    fail: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<BlockExpr> {
    // One minted span shared by the construction's callee path and its
    // identifier: resolution records the case's name use at the
    // identifier's span and the lowerer's case-ctor probe looks it up
    // by the expression's, which must coincide.
    let case_span = mint.span();
    let construction = |args: Vec<CallArg>| -> Expr {
        Expr::Call {
            span: case_span,
            callee: Box::new(Expr::Path {
                span: case_span,
                path: Path {
                    span: case_span,
                    segments: vec![Ident {
                        span: case_span,
                        name: case.name.name.clone(),
                    }],
                },
            }),
            args,
            trailing: None,
            selection: None,
        }
    };
    let mut stmts = Vec::new();
    let args: Vec<CallArg> = match &case.payload {
        EnumPayload::None => unreachable!("the callers hand a payloaded case alone"),
        EnumPayload::Struct(fields) => {
            let (rows, literal) = field_rows(
                &fields.iter().collect::<Vec<_>>(),
                source,
                mint,
                fresh,
                fail,
                diags,
            )?;
            stmts.extend(rows);
            literal
                .into_iter()
                .map(|(name, value)| CallArg {
                    span: mint.span(),
                    name: Some(name),
                    value: value.expect("the literal always carries its value"),
                })
                .collect()
        },
        EnumPayload::Tuple(items) => {
            let array = fresh_binder(fresh, "a");
            let array_source = path_expr(source, mint.span());
            let array_call = member_call(array_source, "dec_array", Vec::new(), mint);
            let array_read = Expr::Try {
                span: mint.span(),
                expr: Box::new(array_call),
            };
            stmts.push(local(mint, &array, array_read, false));
            let mut values = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let at = fresh_binder(fresh, "e");
                let array_read = path_expr(&array, mint.span());
                let index_arg = int_answer(mint, &index.to_string());
                let get = member_call(array_read, "get", vec![index_arg], mint);
                // an optional item keeps its element read's own `none`
                // side: the optional test below reads absence off it,
                // so the unwrap to the seed must not run first (a
                // required item unwraps, a missing element answering
                // the seed document whose read fails into `parse_err`);
                // the field rows read this same shape over the member
                // probe
                let elem = if matches!(paren_free(item), Type::Optional { .. }) {
                    get
                } else {
                    Expr::Call {
                        span: mint.span(),
                        callee: Box::new(Expr::Field {
                            span: mint.span(),
                            recv: Box::new(get),
                            name: Ident {
                                span: mint.span(),
                                name: "unwrap_or".to_owned(),
                            },
                        }),
                        args: vec![CallArg {
                            span: mint.span(),
                            name: None,
                            value: path_expr(SEED, mint.span()),
                        }],
                        trailing: None,
                        selection: None,
                    }
                };
                stmts.push(local(mint, &at, elem, false));
                let read = value_read(
                    item,
                    &at,
                    mint,
                    fresh,
                    item.span(),
                    &case.name.name,
                    "enum case",
                    fail,
                    diags,
                )?;
                stmts.extend(read.0);
                values.push(read.1);
            }
            values
                .into_iter()
                .map(|value| CallArg {
                    span: mint.span(),
                    name: None,
                    value,
                })
                .collect()
        },
    };
    stmts.push(expr_stmt(construction(args)));
    Some(BlockExpr {
        span: mint.span(),
        stmts,
        trailing_trivia: Vec::new(),
    })
}

/// Several payloaded cases' probe chain (the discriminator's decode
/// half): one probe per case in declaration order, each binding
/// `json_member(v, "<case>")` and none-testing into the next probe, a
/// present member running that case's decode over it — the case's own
/// name is the only discriminator, so a wrong or missing key answers
/// `parse_err` at the chain's tail.
fn payload_probe(
    cases: &[&EnumCase],
    mint: &mut Mint,
    fresh: &mut usize,
    fail: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<Expr> {
    let mut chain = Expr::Block(BlockExpr {
        span: mint.span(),
        stmts: vec![expr_stmt(Expr::Return {
            span: mint.span(),
            expr: Some(Box::new(path_expr(fail, mint.span()))),
        })],
        trailing_trivia: Vec::new(),
    });
    for case in cases.iter().rev() {
        let member = fresh_binder(fresh, "k");
        let doc = path_expr("v", mint.span());
        let key = key_literal(mint, &case.name.name);
        let read = member_call(doc, "dec_member", vec![key], mint);
        let cond = Expr::Is {
            span: mint.span(),
            expr: Box::new(path_expr(&member, mint.span())),
            ty: Box::new(name_type(mint, "none")),
        };
        let decode = case_decode_arm(case, &member, mint, fresh, fail, diags)?;
        let probe = Expr::If {
            span: mint.span(),
            cond: Box::new(cond),
            then: block_of(mint, chain),
            else_: Some(Box::new(Expr::Block(decode))),
        };
        chain = Expr::Block(BlockExpr {
            span: mint.span(),
            stmts: vec![local(mint, &member, read, false), expr_stmt(probe)],
            trailing_trivia: Vec::new(),
        });
    }
    Some(chain)
}

/// The unit cases' name chain over the string probe's binder:
/// `s == "c1" { c1 } else if ... else { parse_err }`, `None` when the
/// enum carries no unit case.
fn name_chain(decl: &EnumDecl, binder: &str, mint: &mut Mint, fail: &str) -> Option<Expr> {
    let names: Vec<&str> = decl
        .cases
        .iter()
        .filter(|case| matches!(case.payload, EnumPayload::None))
        .map(|case| case.name.name.as_str())
        .collect();
    if names.is_empty() {
        return None;
    }
    let mut chain = path_expr(fail, mint.span());
    for name in names.iter().rev() {
        let cond = Expr::Binary {
            span: mint.span(),
            op: BinaryOp::Eq,
            left: Box::new(path_expr(binder, mint.span())),
            right: Box::new(key_literal(mint, name)),
        };
        let case = path_expr(name, mint.span());
        let then = block_of(mint, case);
        chain = Expr::If {
            span: mint.span(),
            cond: Box::new(cond),
            then,
            else_: Some(Box::new(chain)),
        };
    }
    Some(chain)
}

/// One member's recursive decode staged as statements: the call bound,
/// the `is parse_err` narrow returning early, the binder answering.
fn member_decode(mint: &mut Mint, fresh: &mut usize, call: Expr, fail: &str) -> (Vec<Stmt>, Expr) {
    let binder = fresh_binder(fresh, "r");
    let stmts = vec![
        local(mint, &binder, call, false),
        err_test(mint, &binder, fail),
    ];
    (stmts, path_expr(&binder, mint.span()))
}

/// `if <name> is none { return parse_err }` — the missing member's
/// refusal.
fn none_test(mint: &mut Mint, name: &str, fail: &str) -> Stmt {
    narrow_test(mint, name, "none", fail)
}

/// `if <name> is parse_err { return parse_err }` — the recursive
/// decode's wrong-shape refusal.
fn err_test(mint: &mut Mint, name: &str, fail: &str) -> Stmt {
    narrow_test(mint, name, "parse_err", fail)
}

fn narrow_test(mint: &mut Mint, name: &str, member: &str, fail: &str) -> Stmt {
    expr_stmt(Expr::If {
        span: mint.span(),
        cond: Box::new(Expr::Is {
            span: mint.span(),
            expr: Box::new(path_expr(name, mint.span())),
            ty: Box::new(name_type(mint, member)),
        }),
        then: BlockExpr {
            span: mint.span(),
            stmts: vec![expr_stmt(Expr::Return {
                span: mint.span(),
                expr: Some(Box::new(path_expr(fail, mint.span()))),
            })],
            trailing_trivia: Vec::new(),
        },
        else_: None,
    })
}

/// The decode walk's shared failure binding: one `parse_err` at its
/// zero position (the decode reads already-decoded values, so no source
/// position exists to name), read by every early return and tail.
fn fail_local(mint: &mut Mint, name: &str) -> Stmt {
    let init = parse_err_expr(mint);
    local(mint, name, init, false)
}
