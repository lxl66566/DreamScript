//! `hash` answers the field-order content fold over the wrap arithmetic
//! (every part through a `hash` dispatch, the seed and mix the FNV-1a pair
//! the content-hash natives hash under).

use super::{mint::*, *};

/// Synthesizes `impl hash for <decl> { fn hash(self) -> int { ... } }`:
/// the field-order content fold over the wrap arithmetic (`+%`/`*%`,
/// the operators the hash fold uses), one fixed seed and mix
/// constant — the FNV-1a pair the content-hash natives hash under — so
/// structurally equal data answers equal hashes. Every part folds
/// through a `hash` dispatch, so a part whose type carries no impl fails
/// at the synthesized call with the standard missing-impl diagnostic
/// over the part's own span: a float part through the ambient's
/// `float_hash`-backed impl, a string or char part through its own
/// content-hash-native-backed impl. An enum folds its case index first,
/// a tag answers the seed constant. Refuses exactly like `show` and
/// `ord`: a payloaded case over a generic head at the attribute, a non-data
/// declaration, a function-typed or `~Copyable` part.
pub(super) fn hash_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    if let Some(case) = payloaded_case_of_generic(item) {
        diags.push(payloaded_refused(case, "hash"));
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
                    "hash",
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
            }
            (
                &decl.name,
                decl.generics.as_slice(),
                struct_hash_body(&fields, mint),
            )
        },
        ItemKind::Enum(decl) => {
            for case in &decl.cases {
                let parts = payload_parts(&case.payload, &case.name.name);
                for (part_name, part, span) in parts {
                    check_part(
                        part,
                        span,
                        "hash",
                        &part_name,
                        "enum case payload",
                        linear,
                        diags,
                    )?;
                }
            }
            (
                &decl.name,
                decl.generics.as_slice(),
                enum_hash_body(decl, mint),
            )
        },
        ItemKind::Tag(decl) => {
            let none: &[TypeParam] = &[];
            (&decl.name, none, vec![expr_stmt(seed_answer(mint))])
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
        hash_member(mint),
        body,
        mint,
    ))
}

/// `hash`'s member, spelled as its trait member spells it: the borrowed
/// receiver alone.
fn hash_member(mint: &mut Mint) -> Member {
    Member {
        trait_name: Some("hash"),
        fn_name: "hash",
        params: vec![self_param(mint.span())],
        ret: name_type(mint, "int"),
    }
}

/// The hash member's struct body: the seed binding, then every field's
/// fold step, then the accumulator's answer.
fn struct_hash_body(fields: &[&FieldDecl], mint: &mut Mint) -> Vec<Stmt> {
    let mut stmts = vec![seed_local(mint)];
    for field in fields {
        let part = field_read("self", &field.name, mint);
        stmts.push(mix_stmt(part, mint));
    }
    stmts.push(expr_stmt(h_use(mint)));
    stmts
}

/// The hash member's enum body: the seed binding, one match over the
/// receiver folding each case's index then its payload parts, then the
/// accumulator's answer.
fn enum_hash_body(decl: &EnumDecl, mint: &mut Mint) -> Vec<Stmt> {
    if decl.cases.is_empty() {
        return vec![expr_stmt(seed_answer(mint))];
    }
    let mut stmts = vec![seed_local(mint)];
    let arms = decl
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let (pat, binders) = case_pattern(case, "p", mint);
            let mut fold = vec![mix_stmt(int_answer(mint, &index.to_string()), mint)];
            for binder in &binders {
                fold.push(mix_stmt(binder_use(binder, mint), mint));
            }
            // every arm answers the accumulator's word, so the discarded
            // match joins over one int shape
            fold.push(expr_stmt(h_use(mint)));
            MatchArm {
                span: case.span,
                pat,
                ann: None,
                guard: None,
                body: Expr::Block(BlockExpr {
                    span: case.span,
                    stmts: fold,
                    trailing_trivia: Vec::new(),
                }),
            }
        })
        .collect::<Vec<_>>();
    let match_span = mint.span();
    stmts.push(Stmt {
        span: match_span,
        kind: StmtKind::Expr(Expr::Match {
            span: match_span,
            scrutinee: Box::new(self_expr(mint)),
            arms,
        }),
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
    });
    stmts.push(expr_stmt(h_use(mint)));
    stmts
}

/// The seed binding every fold accumulates through: `var h = <seed>`.
fn seed_local(mint: &mut Mint) -> Stmt {
    let span = mint.span();
    Stmt {
        span,
        kind: StmtKind::Local(LocalDecl {
            span,
            mutable: true,
            pat: Pattern::Binding {
                span,
                name: Ident {
                    span,
                    name: "h".to_owned(),
                },
            },
            ty: None,
            init: seed_answer(mint),
        }),
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

/// The part-less answer: the seed constant itself, the fold of no parts.
fn seed_answer(mint: &mut Mint) -> Expr {
    Expr::Unary {
        span: mint.span(),
        op: UnaryOp::Neg,
        expr: Box::new(int_answer(mint, HASH_SEED)),
    }
}

/// The accumulator's one use: `h`, at a fresh minted span.
fn h_use(mint: &mut Mint) -> Expr {
    path_expr("h", mint.span())
}

/// One part's fold step: `h = h *% <mix> +% part.hash()`, both operators
/// wrapping, the part through the uniform `hash` dispatch.
fn mix_stmt(part: Expr, mint: &mut Mint) -> Stmt {
    let value = Expr::Binary {
        span: mint.span(),
        op: BinaryOp::WrapAdd,
        left: Box::new(Expr::Binary {
            span: mint.span(),
            op: BinaryOp::WrapMul,
            left: Box::new(h_use(mint)),
            right: Box::new(int_answer(mint, HASH_MIX)),
        }),
        right: Box::new(trait_call(part, "hash", mint)),
    };
    let span = mint.span();
    Stmt {
        span,
        kind: StmtKind::Expr(Expr::Assign {
            span,
            op: AssignOp::Plain,
            left: Box::new(h_use(mint)),
            right: Box::new(value),
        }),
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}
