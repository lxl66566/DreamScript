//! `ord` answers the field-order lexicographic comparison (never
//! structural, float parts sorting through the explicit `total_compare`
//! native the ambient's float impl carries).

use super::{mint::*, *};

/// Synthesizes `impl ord for <decl> { fn compare(self, other: Self) -> int { ... } }`:
/// the field-order lexicographic comparison, never structural — the one
/// ordering the data itself never implies, and cases order by
/// declaration. Every part compares through a `compare` dispatch, so a
/// part whose type carries no impl fails at the synthesized call with
/// the standard missing-impl diagnostic over the part's own span: a
/// float part orders through the ambient's `total_compare`-backed impl,
/// a string, char, or byte part through its own compare-native-backed
/// impl. Refuses exactly like `show`: a payloaded case over a generic
/// head at the attribute, a non-data declaration, a function-typed or
/// `~Copyable` part.
pub(super) fn ord_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    if let Some(case) = payloaded_case_of_generic(item) {
        diags.push(payloaded_refused(case, "ord"));
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
                    "ord",
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
            }
            (
                &decl.name,
                decl.generics.as_slice(),
                struct_compare_body(&fields, mint),
            )
        },
        ItemKind::Enum(decl) => {
            for case in &decl.cases {
                let parts = payload_parts(&case.payload, &case.name.name);
                for (part_name, part, span) in parts {
                    check_part(
                        part,
                        span,
                        "ord",
                        &part_name,
                        "enum case payload",
                        linear,
                        diags,
                    )?;
                }
            }
            (&decl.name, decl.generics.as_slice(), vec![expr_stmt(
                enum_compare_body(decl, mint),
            )])
        },
        ItemKind::Tag(decl) => {
            let none: &[TypeParam] = &[];
            (&decl.name, none, vec![expr_stmt(zero_expr(mint))])
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
        ord_member(mint),
        body,
        mint,
    ))
}

/// `ord`'s member, spelled as its trait member spells it: the receiver,
/// then `other: Self`, both borrowed.
fn ord_member(mint: &mut Mint) -> Member {
    let other = Param {
        span: mint.span(),
        mode: ParamMode::Borrow,
        name: Ident {
            span: mint.span(),
            name: "other".to_owned(),
        },
        ty: Some(name_type(mint, "Self")),
        default: None,
    };
    Member {
        trait_name: Some("ord"),
        fn_name: "compare",
        params: vec![self_param(mint.span()), other],
        ret: name_type(mint, "int"),
    }
}

/// The compare member's struct body: the fields pair up in declaration
/// order, the first difference deciding.
fn struct_compare_body(fields: &[&FieldDecl], mint: &mut Mint) -> Vec<Stmt> {
    let pairs = fields
        .iter()
        .map(|field| {
            (
                field_read("self", &field.name, mint),
                field_read("other", &field.name, mint),
            )
        })
        .collect();
    compare_fold(pairs, mint)
}

/// The compare member's enum body: the outer match names the
/// receiver's case (binding its payload), and each arm matches the
/// other operand — the same case compares the payload pairs
/// lexicographically, any other case answers the case order's sign.
fn enum_compare_body(decl: &EnumDecl, mint: &mut Mint) -> Expr {
    if decl.cases.is_empty() {
        return zero_expr(mint);
    }
    let mut arms = Vec::with_capacity(decl.cases.len());
    for (index, case) in decl.cases.iter().enumerate() {
        let (pat, self_parts) = case_pattern(case, "s", mint);
        let mut inner_arms = Vec::with_capacity(decl.cases.len());
        for (other_index, other_case) in decl.cases.iter().enumerate() {
            let (other_pat, other_parts) = case_pattern(other_case, "o", mint);
            let body = if index == other_index {
                let pairs = self_parts
                    .iter()
                    .zip(other_parts.iter())
                    .map(|(one, other)| (binder_use(one, mint), binder_use(other, mint)))
                    .collect();
                Expr::Block(BlockExpr {
                    span: case.span,
                    stmts: compare_fold(pairs, mint),
                    trailing_trivia: Vec::new(),
                })
            } else {
                sign_expr(mint, index, other_index)
            };
            inner_arms.push(MatchArm {
                span: other_case.span,
                pat: other_pat,
                ann: None,
                guard: None,
                body,
            });
        }
        let inner = Expr::Match {
            span: mint.span(),
            scrutinee: Box::new(path_expr("other", mint.span())),
            arms: inner_arms,
        };
        arms.push(MatchArm {
            span: case.span,
            pat,
            ann: None,
            guard: None,
            body: inner,
        });
    }
    Expr::Match {
        span: mint.span(),
        scrutinee: Box::new(self_expr(mint)),
        arms,
    }
}

/// The lexicographic fold over one derived body's part pairs: one
/// `const` per pair (a `compare` dispatch over the pair), then an
/// if-chain answering the first non-zero result, the last pair's result
/// the tail.
fn compare_fold(pairs: Vec<(Expr, Expr)>, mint: &mut Mint) -> Vec<Stmt> {
    if pairs.is_empty() {
        return vec![expr_stmt(zero_expr(mint))];
    }
    let mut stmts = Vec::with_capacity(pairs.len() + 1);
    let mut binders = Vec::with_capacity(pairs.len());
    for (index, (lhs, rhs)) in pairs.into_iter().enumerate() {
        let init = pair_compare(lhs, rhs, mint);
        let binder = Ident {
            span: mint.span(),
            name: format!("c{index}"),
        };
        stmts.push(Stmt {
            span: binder.span,
            kind: StmtKind::Local(LocalDecl {
                span: binder.span,
                mutable: false,
                pat: Pattern::Binding {
                    span: binder.span,
                    name: binder.clone(),
                },
                ty: None,
                init,
            }),
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
        });
        binders.push(binder);
    }
    let last = binders.len() - 1;
    let mut chain = binder_use(&binders[last], mint);
    for binder in binders[..last].iter().rev() {
        let cond = Expr::Binary {
            span: mint.span(),
            op: BinaryOp::Ne,
            left: Box::new(binder_use(binder, mint)),
            right: Box::new(zero_expr(mint)),
        };
        let answer = expr_stmt(binder_use(binder, mint));
        chain = Expr::If {
            span: mint.span(),
            cond: Box::new(cond),
            then: BlockExpr {
                span: answer.span,
                stmts: vec![answer],
                trailing_trivia: Vec::new(),
            },
            else_: Some(Box::new(chain)),
        };
    }
    stmts.push(expr_stmt(chain));
    stmts
}

/// One part pair's compare: `lhs.compare(rhs)`, the uniform dispatch.
fn pair_compare(lhs: Expr, rhs: Expr, mint: &mut Mint) -> Expr {
    let callee = Expr::Field {
        span: mint.span(),
        recv: Box::new(lhs),
        name: Ident {
            span: mint.span(),
            name: "compare".to_owned(),
        },
    };
    Expr::Call {
        span: mint.span(),
        callee: Box::new(callee),
        args: vec![CallArg {
            span: rhs.span(),
            name: None,
            value: rhs,
        }],
        trailing: None,
        selection: None,
    }
}

/// The equal answer of a part-less shape.
fn zero_expr(mint: &mut Mint) -> Expr {
    int_answer(mint, "0")
}

/// The case-order sign between the receiver's case and the other
/// operand's: the later case compares greater.
fn sign_expr(mint: &mut Mint, index: usize, other_index: usize) -> Expr {
    if index > other_index {
        int_answer(mint, "1")
    } else {
        Expr::Unary {
            span: mint.span(),
            op: UnaryOp::Neg,
            expr: Box::new(int_answer(mint, "1")),
        }
    }
}
