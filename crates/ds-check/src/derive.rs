//! The derive engine's first slice (G1): the meta-language is the
//! language, so a derive attribute expands into an ordinary impl
//! declaration — AST-level synthesis after parse, before resolution —
//! and the synthesized impl flows through the normal checker (trait
//! coherence, signature agreement) and the normal lowering. Nothing in
//! the emission paths knows a derive exists.
//!
//! `show`, `ord`, `hash`, and `clone` are the landed families: `show`
//! answers the construction spelling of the data; `ord` answers the
//! field-order lexicographic comparison (never structural, float parts
//! sorting through the explicit `total_compare` native the ambient's
//! float impl carries); `hash` answers the field-order content fold
//! over the wrap arithmetic (every part through a `hash` dispatch, the
//! seed and mix the FNV-1a pair the content-hash natives hash under);
//! `clone` answers the shallow duplicate, the construction over the
//! borrowed receiver whose Ref parts dup through the placement pass —
//! the one family whose body adds no runtime work beyond the count
//! traffic. Every other name in the ruled set (`to_json`, `from_json`,
//! `default`) parses and refuses with a checklist diagnostic naming it
//! unimplemented; `==`
//! refuses with the B9 diagnostic (equality is universal and never
//! derived); unknown names refuse naming the ruled set; a
//! function-typed or `~Copyable` part refuses naming the part; a
//! declaration carrying its own type parameters refuses (the runtime
//! seam lowers one member body per impl, so a receiver whose shape
//! spans those parameters waits on the JIT's per-instantiation frames).

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    AssignOp, Ast, Attribute, BinaryOp, BlockExpr, CallArg, EnumCase, EnumDecl, EnumPayload, Expr,
    FieldDecl, FnDecl, Ident, ImplDecl, Item, ItemKind, Literal, LocalDecl, MatchArm, Param,
    ParamMode, Path, Pattern, Stmt, StmtKind, StrPart, StructMember, Type, TypeParam, TypeRef,
    UnaryOp,
};

/// The ruled derive vocabulary (G1), in ruling order.
pub const RULED_SET: &str = "hash, show, ord, clone, to_json, from_json, default";

/// The families this round has not implemented yet, in ruling order.
const UNIMPLEMENTED: &[&str] = &["to_json", "from_json", "default"];

/// The hash family's fold constants: FNV-1a's offset basis and prime,
/// the same pair the builtin str/float/char hash natives hash under —
/// the basis exceeds i64's range, so the fold spells its 2^64 wrap as a
/// negated literal (the fold is mod-2^64 arithmetic either way).
const HASH_SEED: &str = "3750763034362895579";
const HASH_MIX: &str = "1099511628211";

/// The written type names the interpolation context prints itself (F6);
/// every other part derives through a `show` dispatch.
const PRINTABLE: &[&str] = &["int", "float", "bool", "char", "string"];

/// Expands every derive attribute in the package: consumed attributes
/// are stripped and their impls appended beside the attributed item,
/// refused names keep the attribute and contribute a diagnostic. Files
/// without derive attributes come back byte-identical (a plain clone).
/// `asts` may hold shared trees (`Borrow<Ast>`, like
/// [`check_resolved`](crate::type_of::check_resolved)): the query layer
/// expands its parse memos without cloning the trees.
#[must_use]
pub fn expand<T>(asts: &[T]) -> (Vec<Ast>, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    let trees: Vec<&Ast> = asts.iter().map(std::borrow::Borrow::borrow).collect();
    expand_trees(&trees)
}

/// The expansion over the collected trees; see [`expand`].
fn expand_trees(asts: &[&Ast]) -> (Vec<Ast>, Vec<Diagnostic>) {
    if !asts.iter().any(|ast| has_derive(ast)) {
        let cloned: Vec<Ast> = asts.iter().map(|ast| (*ast).clone()).collect();
        return (cloned, Vec::new());
    }
    let linear = linear_heads(asts);
    let mut diags = Vec::new();
    let mut out = Vec::with_capacity(asts.len());
    for &ast in asts {
        if !has_derive(ast) {
            out.push(ast.clone());
            continue;
        }
        let mut ast = ast.clone();
        for block in &mut ast.blocks {
            // Drain and rebuild: synthesized impls insert beside their
            // attributed item without index arithmetic, so a derive late
            // in the block is never shifted past the walk.
            let mut rebuilt = Vec::with_capacity(block.items.len());
            for item in block.items.drain(..) {
                let (extra, strip) = expand_item(&item, &linear, &mut diags);
                let mut item = item;
                if strip {
                    item.attrs.retain(|a| a.name.name != "derive");
                }
                rebuilt.push(item);
                rebuilt.extend(extra);
            }
            block.items = rebuilt;
        }
        out.push(ast);
    }
    (out, diags)
}

/// Whether any item of the file carries a derive attribute.
fn has_derive(ast: &Ast) -> bool {
    ast.blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .any(|i| i.attrs.iter().any(|a| a.name.name == "derive"))
}

/// The names of struct/enum declarations whose supers carry a negated
/// constraint (the `~Copyable` linear layer, C1): a part of such a type
/// refuses structural derivation.
fn linear_heads(asts: &[&Ast]) -> Vec<String> {
    let mut heads = Vec::new();
    for item in asts
        .iter()
        .flat_map(|a| a.blocks.iter())
        .flat_map(|b| b.items.iter())
    {
        let (name, supers) = match &item.kind {
            ItemKind::Struct(s) => (&s.name, &s.supers),
            ItemKind::Enum(e) => (&e.name, &e.supers),
            _ => continue,
        };
        if supers.iter().any(|t| matches!(t, Type::Negation { .. })) {
            heads.push(name.name.clone());
        }
    }
    heads
}

/// Expands one item's derive attributes. Returns the synthesized impl
/// items to insert after it and whether every derive argument was
/// consumed (the attribute is then stripped).
fn expand_item(item: &Item, linear: &[String], diags: &mut Vec<Diagnostic>) -> (Vec<Item>, bool) {
    let mut extra = Vec::new();
    let mut consumed = 0;
    let mut derive_attrs = 0;
    // One mint per attributed item, shared by every family expanding
    // it: two families minting from scratch would hand the same byte
    // range's spans to two different name uses, and the member fn
    // declarations mint here too, so two families' fns never share one
    // span.
    let mut mint = Mint::new(item.span);
    for attr in &item.attrs {
        if attr.name.name != "derive" {
            continue;
        }
        derive_attrs += 1;
        let mut attr_consumed = true;
        for arg in &attr.args {
            let Some(name) = single_segment(arg) else {
                diags.push(unknown(arg.span, &arg.text()));
                attr_consumed = false;
                continue;
            };
            match name {
                "==" => {
                    diags.push(eq_refused(attr.span));
                    attr_consumed = false;
                },
                "show" => match show_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "ord" => match ord_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "hash" => match hash_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "clone" => match clone_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                ruled if UNIMPLEMENTED.contains(&ruled) => {
                    diags.push(unimplemented(attr.span, ruled));
                    attr_consumed = false;
                },
                _ => {
                    diags.push(unknown(arg.span, name));
                    attr_consumed = false;
                },
            }
        }
        if attr_consumed {
            consumed += 1;
        }
    }
    (extra, consumed == derive_attrs && derive_attrs > 0)
}

/// The derive name: a single-segment path argument.
fn single_segment(path: &Path) -> Option<&str> {
    match path.segments.as_slice() {
        [only] => Some(only.name.as_str()),
        _ => None,
    }
}

fn unimplemented(span: Span, name: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveUnimplemented,
        format!("derive `{name}` is ruled but not implemented yet [G1]"),
    )
    .with_primary(span, format!("the `{name}` family is queued"))
    .with_note(format!("the ruled set is: {RULED_SET}"))
}

fn unknown(span: Span, name: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveUnknown,
        format!("`{name}` is not a derive name [G1]"),
    )
    .with_primary(span, "not in the ruled set")
    .with_note(format!("the ruled set is: {RULED_SET}"))
}

fn eq_refused(span: Span) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveEqRefused,
        "`==` is universal structural equality and is never derived [B9]",
    )
    .with_primary(span, "remove `==` from the derive list")
    .with_note(
        "equality compares structurally over all data already; hash stays consistent with it",
    )
}

/// A generic declaration refuses: the synthesized member's body reads
/// its parts through the declaration's own type variables, while the
/// runtime seam lowers one member body per impl — a receiver whose
/// shape spans those variables needs the JIT's per-instantiation
/// frames, never the interpreter's.
fn generic_refused(span: Span, family: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveGenericRefused,
        format!("derive `{family}` refuses a generic declaration [G1]"),
    )
    .with_primary(span, "this declaration carries its own type parameters")
    .with_note(
        "the synthesized member reads its parts through the declaration's type variables; the \
         runtime seam lowers one body per impl, and per-instantiation frames are the JIT \
         specialization tier's",
    )
}

/// A part (field or payload element) that refuses structural derivation.
fn part_refused(span: Span, family: &str, what: &str, part: &str, because: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DerivePartRefused,
        format!("derive `{family}` refuses the {what} `{part}`: {because} [G1, B9]"),
    )
    .with_primary(span, format!("this part carries no structural {family}"))
}

/// Synthesizes `impl show for <decl> { fn show(self) -> string { ... } }`.
/// Refuses (returning `None`, diagnostic pushed) when the item is not a
/// data declaration or a part refuses structural derivation.
fn show_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    let generics = match &item.kind {
        ItemKind::Struct(decl) => Some(&decl.generics),
        ItemKind::Enum(decl) => Some(&decl.generics),
        _ => None,
    };
    if let Some(generics) = generics
        && !generics.is_empty()
    {
        diags.push(generic_refused(attr.span, "show"));
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

/// The non-data refusal every family shares.
fn non_data_refused(attr: &Attribute) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveNonDataItem,
        "a derive applies to a struct, enum, or tag declaration [G1]",
    )
    .with_primary(attr.span, "this declaration derives nothing")
}

/// `show`'s member: the receiver, no other parameter, `string` back.
fn show_member(span: Span, mint: &mut Mint) -> Member {
    Member {
        trait_name: "show",
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

/// One enum payload's element types with their names and spans. A tuple
/// element is named by its case; a struct payload names its fields.
fn payload_parts<'a>(payload: &'a EnumPayload, case: &str) -> Vec<(String, &'a Type, Span)> {
    match payload {
        EnumPayload::None => Vec::new(),
        EnumPayload::Tuple(items) => items
            .iter()
            .map(|t| (case.to_owned(), t, t.span()))
            .collect(),
        EnumPayload::Struct(fields) => fields
            .iter()
            .map(|f| (f.name.name.clone(), &f.ty, f.span))
            .collect(),
    }
}

/// The ruled part carve-outs: a function-typed part never shows (B9's
/// static refusal), a written negation (`~Copyable`) is the linear
/// layer, and a part headed by a linear-declared nominal is a resource.
/// Every other part flows through the ordinary checker: a part without
/// `show` fails at the synthesized call with the standard missing-impl
/// diagnostic over the part's own span.
fn check_part(
    ty: &Type,
    span: Span,
    family: &str,
    part: &str,
    what: &str,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<()> {
    match ty {
        Type::Fn { .. } => {
            diags.push(part_refused(
                span,
                family,
                what,
                part,
                "a function-typed part compares and shows as no structural value",
            ));
            None
        },
        Type::Negation { .. } => {
            diags.push(part_refused(
                span,
                family,
                what,
                part,
                "a `~`-typed part is a linear resource",
            ));
            None
        },
        Type::Path { path, .. } => {
            if let [only] = path.segments.as_slice() {
                if linear.iter().any(|head| head == &only.name) {
                    diags.push(part_refused(
                        span,
                        family,
                        what,
                        part,
                        &format!("`{}` declares the `~Copyable` linear layer", only.name),
                    ));
                    return None;
                }
            }
            Some(())
        },
        Type::Tuple { items, .. } => {
            for item in items {
                check_part(item, span, family, part, what, linear, diags)?;
            }
            Some(())
        },
        Type::Optional { inner, .. } => check_part(inner, span, family, part, what, linear, diags),
        _ => Some(()),
    }
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
/// `rect { w, h } => "rect { w: ${w}, h: ${h} }"`.
fn arm(case: &EnumCase, mint: &mut Mint) -> MatchArm {
    let path = Path {
        span: case.name.span,
        segments: vec![case.name.clone()],
    };
    let mut parts = vec![text(case.span, &case.name.name)];
    let pat = match &case.payload {
        EnumPayload::None => Pattern::Path {
            span: case.name.span,
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
/// itself (F6); every other type dispatches through `show`, so a part
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

/// Synthesizes `impl ord for <decl> { fn compare(self, other: Self) -> int { ... } }`:
/// the field-order lexicographic comparison, never structural — the one
/// ordering the data itself never implies, and cases order by
/// declaration. Every part compares through a `compare` dispatch, so a
/// part whose type carries no impl fails at the synthesized call with
/// the standard missing-impl diagnostic over the part's own span: a
/// float part orders through the ambient's `total_compare`-backed impl,
/// a string, char, or byte part through its own compare-native-backed
/// impl. Refuses exactly like `show`: a generic declaration at the
/// attribute, a non-data declaration, a function-typed or `~Copyable`
/// part.
fn ord_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    let generics = match &item.kind {
        ItemKind::Struct(decl) => Some(&decl.generics),
        ItemKind::Enum(decl) => Some(&decl.generics),
        _ => None,
    };
    if let Some(generics) = generics
        && !generics.is_empty()
    {
        diags.push(generic_refused(attr.span, "ord"));
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
        trait_name: "ord",
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

/// One case's pattern with its payload bound to `prefix`-numbered
/// binders: a tuple element binds by position, a struct field through a
/// renamed binder (so the outer and the inner match never collide on a
/// name), a part-less case carries none.
fn case_pattern(case: &EnumCase, prefix: &str, mint: &mut Mint) -> (Pattern, Vec<Ident>) {
    // The case path is a name use, so each pattern occurrence mints
    // its own span (one compare names every case path several times:
    // once in the outer match, once per inner arm).
    let span = mint.span();
    let path = Path {
        span,
        segments: vec![Ident {
            span,
            name: case.name.name.clone(),
        }],
    };
    match &case.payload {
        EnumPayload::None => (Pattern::Path { span, path }, Vec::new()),
        EnumPayload::Tuple(items) => {
            let mut binders = Vec::with_capacity(items.len());
            let mut args = Vec::with_capacity(items.len());
            for index in 0..items.len() {
                let binder = mint_binder(mint, prefix, index);
                args.push(Pattern::Binding {
                    span: binder.span,
                    name: binder.clone(),
                });
                binders.push(binder);
            }
            (
                Pattern::Call {
                    span: case.span,
                    path,
                    args,
                },
                binders,
            )
        },
        EnumPayload::Struct(fields) => {
            let mut binders = Vec::with_capacity(fields.len());
            let mut pat_fields = Vec::with_capacity(fields.len());
            for (index, field) in fields.iter().enumerate() {
                let binder = mint_binder(mint, prefix, index);
                pat_fields.push((
                    field.name.clone(),
                    Some(Pattern::Binding {
                        span: binder.span,
                        name: binder.clone(),
                    }),
                ));
                binders.push(binder);
            }
            (
                Pattern::Struct {
                    span: case.span,
                    path,
                    fields: pat_fields,
                },
                binders,
            )
        },
    }
}

fn mint_binder(mint: &mut Mint, prefix: &str, index: usize) -> Ident {
    Ident {
        span: mint.span(),
        name: format!("{prefix}{index}"),
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
    }
}

/// A field read of the receiver or the other operand: `base.name`.
fn field_read(base: &str, field: &Ident, mint: &mut Mint) -> Expr {
    let recv = path_expr(base, mint.span());
    Expr::Field {
        span: mint.span(),
        recv: Box::new(recv),
        name: Ident {
            span: mint.span(),
            name: field.name.clone(),
        },
    }
}

/// One use of a payload binder or a fold const.
fn binder_use(binder: &Ident, mint: &mut Mint) -> Expr {
    path_expr(&binder.name, mint.span())
}

/// An integer-literal answer.
fn int_answer(mint: &mut Mint, text: &str) -> Expr {
    Expr::Literal {
        span: mint.span(),
        lit: Literal::Int {
            suffix: String::new(),
            text: text.to_owned(),
        },
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

/// Synthesizes `impl hash for <decl> { fn hash(self) -> int { ... } }`:
/// the field-order content fold over the wrap arithmetic (B7's `+%`/`*%`,
/// the operators the record names for hash use), one fixed seed and mix
/// constant — the FNV-1a pair the content-hash natives hash under — so
/// structurally equal data answers equal hashes. Every part folds
/// through a `hash` dispatch, so a part whose type carries no impl fails
/// at the synthesized call with the standard missing-impl diagnostic
/// over the part's own span: a float part through the ambient's
/// `float_hash`-backed impl, a string or char part through its own
/// content-hash-native-backed impl. An enum folds its case index first,
/// a tag answers the seed constant. Refuses exactly like `show` and
/// `ord`: a generic declaration at the attribute, a non-data
/// declaration, a function-typed or `~Copyable` part.
fn hash_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    let generics = match &item.kind {
        ItemKind::Struct(decl) => Some(&decl.generics),
        ItemKind::Enum(decl) => Some(&decl.generics),
        _ => None,
    };
    if let Some(generics) = generics
        && !generics.is_empty()
    {
        diags.push(generic_refused(attr.span, "hash"));
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
        trait_name: "hash",
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
/// wrapping (B7), the part through the uniform `hash` dispatch.
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

/// One part's trait-member dispatch: `part.<member>()`, the uniform call
/// every part takes, so a missing impl names the part's own span.
fn trait_call(part: Expr, member: &str, mint: &mut Mint) -> Expr {
    let callee = Expr::Field {
        span: mint.span(),
        recv: Box::new(part),
        name: Ident {
            span: mint.span(),
            name: member.to_owned(),
        },
    };
    Expr::Call {
        span: mint.span(),
        callee: Box::new(callee),
        args: Vec::new(),
        trailing: None,
    }
}

/// Synthesizes `impl clone for <decl> { fn clone(self) -> Self { ... } }`:
/// the shallow duplicate (C4's contract: the body is a construction over
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
/// struct refuses: its `Self` carries no word, so the member's return
/// would lower to a zero-width RETSPAN, which the ABI refuses by
/// design (plain fns returning one share the hole). Refuses otherwise
/// exactly like `hash`: a generic declaration at the attribute, a
/// non-data declaration, a function-typed or `~Copyable` part.
fn clone_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    let generics = match &item.kind {
        ItemKind::Struct(decl) => Some(&decl.generics),
        ItemKind::Enum(decl) => Some(&decl.generics),
        _ => None,
    };
    if let Some(generics) = generics
        && !generics.is_empty()
    {
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
            if fields.is_empty() {
                diags.push(partless_refused(attr.span));
                return None;
            }
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

/// The part-less struct refusal: the member's `Self` return carries no
/// word, so its RETSPAN would be zero-width, which the ABI refuses.
fn partless_refused(span: Span) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveClonePartless,
        "derive `clone` refuses a part-less struct [G1]",
    )
    .with_primary(span, "this struct's `Self` carries no word")
    .with_note(
        "the member's Self return would lower to a zero-width RETSPAN, which the ABI refuses by \
         design; a value must occupy at least one word",
    )
}

/// `clone`'s member, spelled as its trait member spells it: the borrowed
/// receiver alone, `Self` back.
fn clone_member(mint: &mut Mint) -> Member {
    Member {
        trait_name: "clone",
        fn_name: "clone",
        params: vec![self_param(mint.span())],
        ret: name_type(mint, "Self"),
    }
}

/// The clone member's struct body: the construction with one `clone`
/// dispatch per field, `X { f: self.f.clone(), ... }`. Every name the
/// literal carries mints its own span (the struct path, each field
/// name), because resolution records name uses by span.
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

/// The synthesized impl's member: the trait it implements, the fn's
/// name, its parameters (the receiver first, spelled as its trait
/// member spells it), and its return type.
struct Member {
    trait_name: &'static str,
    fn_name: &'static str,
    params: Vec<Param>,
    ret: Type,
}

/// The receiver parameter, a declaration keeping the attribute's span.
fn self_param(span: Span) -> Param {
    Param {
        span,
        mode: ParamMode::Borrow,
        name: Ident {
            span,
            name: "self".to_owned(),
        },
        ty: None,
        default: None,
    }
}

/// A one-name written type at fresh minted spans: a name use, so the
/// type, the path, and the identifier each mint their own.
fn name_type(mint: &mut Mint, name: &str) -> Type {
    let span = mint.span();
    let path = Path {
        span: mint.span(),
        segments: vec![Ident {
            span: mint.span(),
            name: name.to_owned(),
        }],
    };
    Type::Path {
        span,
        path,
        args: Vec::new(),
    }
}

/// One synthesized statement over an expression, the span shared.
fn expr_stmt(expr: Expr) -> Stmt {
    Stmt {
        span: expr.span(),
        kind: StmtKind::Expr(expr),
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

/// The assembled impl item:
/// `impl <trait> for <target> { fn <member> ... }`.
fn impl_item(
    attr: &Attribute,
    name: &Ident,
    generics: &[TypeParam],
    member: Member,
    body: Vec<Stmt>,
    mint: &mut Mint,
) -> Item {
    let span = attr.span;
    // A generic declaration derives per instantiation: the impl declares
    // the same parameters with the trait added to whatever bound the
    // declaration wrote, and the target applies them in order.
    let impl_generics: Vec<TypeParam> = generics
        .iter()
        .map(|p| {
            let bound = match &p.bound {
                Some(written) => Some(Type::Intersect {
                    span: p.span,
                    left: Box::new(written.clone()),
                    right: Box::new(trait_type(mint, member.trait_name)),
                }),
                None => Some(trait_type(mint, member.trait_name)),
            };
            TypeParam {
                span: p.span,
                name: p.name.clone(),
                bound,
            }
        })
        .collect();
    // The target is a name use, so it mints its own span: two families
    // expanding one item each synthesize an impl, and the declaration's
    // own name span cannot serve both targets' use records.
    let target_span = mint.span();
    let target = Type::Path {
        span: target_span,
        path: Path {
            span: target_span,
            segments: vec![Ident {
                span: target_span,
                name: name.name.clone(),
            }],
        },
        args: generics
            .iter()
            .map(|p| Type::Path {
                span: p.span,
                path: Path {
                    span: p.span,
                    segments: vec![p.name.clone()],
                },
                args: Vec::new(),
            })
            .collect(),
    };
    // The trait reference is a name use, so it mints its own span.
    let trait_ident = Ident {
        span: mint.span(),
        name: member.trait_name.to_owned(),
    };
    // The member fn declaration mints one span for the declaration, the
    // fn name, and the body: two families expanding one attribute each
    // synthesize a member fn, the trait-member match is by name, and the
    // solved returns ride the span-keyed impl table — the attribute's
    // shared span would keep only the later family's return.
    let fn_span = mint.span();
    let fn_decl = FnDecl {
        span: fn_span,
        name: Ident {
            span: fn_span,
            name: member.fn_name.to_owned(),
        },
        generics: Vec::new(),
        params: member.params,
        ret: Some(member.ret),
        body: Some(BlockExpr {
            span: fn_span,
            stmts: body,
            trailing_trivia: Vec::new(),
        }),
        doc: Vec::new(),
    };
    Item {
        span,
        attrs: Vec::new(),
        vis: None,
        kind: ItemKind::Impl(ImplDecl {
            span,
            generics: impl_generics,
            trait_ref: Some(TypeRef {
                span: trait_ident.span,
                path: Path {
                    span: trait_ident.span,
                    segments: vec![trait_ident],
                },
                args: Vec::new(),
            }),
            target,
            fns: vec![fn_decl],
        }),
        leading_trivia: Vec::new(),
        doc: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

/// The derived trait as a written bound, at one fresh minted span
/// shared by the type, the path, and the identifier.
fn trait_type(mint: &mut Mint, name: &str) -> Type {
    let span = mint.span();
    Type::Path {
        span,
        path: Path {
            span,
            segments: vec![Ident {
                span,
                name: name.to_owned(),
            }],
        },
        args: Vec::new(),
    }
}

fn text(span: Span, s: &str) -> StrPart {
    StrPart::Text {
        span,
        text: s.to_owned(),
    }
}

fn str_expr(span: Span, parts: Vec<StrPart>) -> Expr {
    Expr::Str { span, parts }
}

/// `self` as an expression; each occurrence mints one span shared by the
/// expression, the path, and the identifier — resolution records the
/// use at the identifier's span and lowering looks it up by the
/// expression's, which coincide for a one-segment path.
fn self_expr(mint: &mut Mint) -> Expr {
    path_expr("self", mint.span())
}

/// A one-segment path expression over one minted span.
fn path_expr(name: &str, span: Span) -> Expr {
    Expr::Path {
        span,
        path: Path {
            span,
            segments: vec![Ident {
                span,
                name: name.to_owned(),
            }],
        },
    }
}

/// Allocates the synthesized nodes' spans inside the attributed item's
/// own byte range: the declaration contains no expressions, so its
/// bytes collide with no real expression span, and distinct slices keep
/// the checker's span-keyed tables (name uses, expression types, trait
/// calls, binder facts, member fns' solved returns) collision-free. A
/// pass spends one span per byte; a spent pass is followed by one over
/// one-byte-longer spans, so the budget grows with the square of the
/// item's length while every synthesized node — the member fn
/// declarations included — mints only a few spans per part (the `ord`
/// fold's dozen against a part's few bytes), leaving the budget better
/// than two spans per span spent on every reachable declaration: the
/// wrap never runs out, its cycle start-over degrading a diagnostic's
/// span only.
struct Mint {
    file: ds_diag::FileId,
    start: u32,
    next: u32,
    end: u32,
    len: u32,
}

impl Mint {
    fn new(item: Span) -> Self {
        Self {
            file: item.file,
            start: item.start,
            next: item.start,
            end: item.end.saturating_sub(1),
            len: 1,
        }
    }

    fn span(&mut self) -> Span {
        let range = self.end.saturating_sub(self.start);
        if self.len > range || self.next + self.len > self.end {
            self.next = self.start;
            self.len = if self.len >= range { 1 } else { self.len + 1 };
        }
        let span = Span::new(self.file, self.next, self.next + self.len);
        self.next += 1;
        span
    }
}
