//! The derive engine (G1): the meta-language is the language, so a
//! derive attribute expands into an ordinary impl declaration —
//! AST-level synthesis after parse, before resolution — and the
//! synthesized impl flows through the normal checker (trait coherence,
//! signature agreement) and the normal lowering. Nothing in the
//! emission paths knows a derive exists.
//!
//! `show`, `ord`, `hash`, `clone`, and `default` are the landed
//! families: `show` answers the construction spelling of the data;
//! `ord` answers the field-order lexicographic comparison (never
//! structural, float parts sorting through the explicit
//! `total_compare` native the ambient's float impl carries); `hash`
//! answers the field-order content fold over the wrap arithmetic
//! (every part through a `hash` dispatch, the seed and mix the FNV-1a
//! pair the content-hash natives hash under); `clone` answers the
//! shallow duplicate, the construction over the borrowed receiver
//! whose Ref parts dup through the placement pass — the one family
//! whose body adds no runtime work beyond the count traffic; `default`
//! answers the receiverless construction (`fn default() -> Self` in
//! the type's own impl, the 2026-11-15 ruling), one default per field
//! keyed by the field's type expression — a bare type name a `default`
//! dispatch, a tuple the per-element construction, an optional `none`,
//! an application the empty construction — a part-less struct the
//! unit-word construction, an enum its first unit case, a tag itself.
//! A union's `#[default]` member annotation (the 2026-11-15 ruling)
//! names the one member the family synthesizes over: the annotated
//! enum defaults to that member's construction — a bare case the tag
//! itself, a payloaded case the call over per-part defaults — a
//! field's type spelling the annotated union answers the member's
//! construction, a union over one enum's cases member-wise; no
//! annotation keeps the ambiguity refusal, a second annotated member
//! refusing as a duplicate.
//! Every other name in the ruled set (`to_json`, `from_json`) parses
//! and refuses with a checklist diagnostic naming it unimplemented; `==`
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
const UNIMPLEMENTED: &[&str] = &["to_json", "from_json"];

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
/// [`check_resolved`](ds_check::type_of::check_resolved)): the query layer
/// expands its parse memos without cloning the trees.
#[must_use]
pub fn expand<T>(asts: &[T]) -> (Vec<Ast>, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    let trees: Vec<&Ast> = asts.iter().map(std::borrow::Borrow::borrow).collect();
    expand_trees(&trees)
}

/// Expands the package's caller-owned trees by moving each one
/// through: nothing clones, so the cold run path never pays the deep
/// copy [`expand`] pays over its borrowed inputs. `preexpanded` marks,
/// aligned with `asts`, the trees already derive-expanded (the ambient
/// blob's decode); a marked tree must carry no derive attribute and
/// passes through untouched — the mark asserts the artifact held its
/// end of the contract, it never gates behavior, because expansion
/// consumes derive attributes and a second expansion over a
/// pre-expanded tree is the identity either way. Linear heads are
/// collected over the whole package first, marked trees included, so a
/// user derive over a linear ambient part refuses identically to the
/// borrowed expansion.
#[must_use]
pub fn expand_owned(mut asts: Vec<Ast>, preexpanded: &[bool]) -> (Vec<Ast>, Vec<Diagnostic>) {
    let trees: Vec<&Ast> = asts.iter().collect();
    let linear = linear_heads(&trees);
    let mut diags = Vec::new();
    for (index, ast) in asts.iter_mut().enumerate() {
        if preexpanded.get(index).copied().unwrap_or(false) {
            debug_assert!(
                !has_expansion(ast),
                "a pre-expanded tree carries no expansion work"
            );
        } else if has_expansion(ast) {
            expand_tree_in_place(ast, &linear, &mut diags);
        }
    }
    (asts, diags)
}

/// The expansion over the collected trees; see [`expand`].
fn expand_trees(asts: &[&Ast]) -> (Vec<Ast>, Vec<Diagnostic>) {
    if !asts.iter().any(|ast| has_expansion(ast)) {
        let cloned: Vec<Ast> = asts.iter().map(|ast| (*ast).clone()).collect();
        return (cloned, Vec::new());
    }
    let linear = linear_heads(asts);
    let mut diags = Vec::new();
    let mut out = Vec::with_capacity(asts.len());
    for &ast in asts {
        if !has_expansion(ast) {
            out.push(ast.clone());
            continue;
        }
        let mut ast = ast.clone();
        expand_tree_in_place(&mut ast, &linear, &mut diags);
        out.push(ast);
    }
    (out, diags)
}

/// The in-place expansion of one tree: consumed attributes stripped,
/// their impls appended beside the attributed item.
fn expand_tree_in_place(ast: &mut Ast, linear: &[String], diags: &mut Vec<Diagnostic>) {
    // The `#[default]` member annotations are declaration data of this
    // file's enums, so the context collects once per tree ahead of the
    // walk: a field key reads an annotation only when the annotated
    // enum is declared in the same file (G4).
    let unions = UnionDefaults::collect(
        &ast.blocks
            .iter()
            .flat_map(|b| b.items.iter())
            .collect::<Vec<_>>(),
        diags,
    );
    for block in &mut ast.blocks {
        // Drain and rebuild: synthesized impls insert beside their
        // attributed item without index arithmetic, so a derive late
        // in the block is never shifted past the walk.
        let mut rebuilt = Vec::with_capacity(block.items.len());
        for item in block.items.drain(..) {
            let (extra, strip) = expand_item(&item, &unions, linear, diags);
            let mut item = item;
            if strip {
                item.attrs.retain(|a| a.name.name != "derive");
            }
            item.attrs.retain(|a| a.name.name != "default");
            rebuilt.push(item);
            rebuilt.extend(extra);
        }
        block.items = rebuilt;
    }
}

/// Whether any item of the file carries work the expansion owns: a
/// derive attribute, a `#[default]` member annotation on an enum case,
/// or a `#[default]` attribute on an item (which the expansion refuses
/// — the annotation belongs inside an enum).
fn has_expansion(ast: &Ast) -> bool {
    ast.blocks.iter().flat_map(|b| b.items.iter()).any(|i| {
        i.attrs.iter().any(|a| a.name.name == "derive" || a.name.name == "default")
            || matches!(&i.kind, ItemKind::Enum(decl) if decl.cases.iter().any(|c| c.default.is_some()))
    })
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
fn expand_item(
    item: &Item,
    unions: &UnionDefaults,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> (Vec<Item>, bool) {
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
        if attr.name.name == "default" {
            diags.push(misplaced_default(attr));
            continue;
        }
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
                "default" => match default_impl(item, attr, &mut mint, unions, linear, diags) {
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

/// A `#[default]` attribute on an item: the annotation belongs to an
/// enum case, the union member a default derivation reads.
fn misplaced_default(attr: &Attribute) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveMemberAnnotation,
        "`#[default]` annotates an enum case, the union member a default derives over [G1]",
    )
    .with_primary(
        attr.span,
        "place the annotation on one case inside the enum",
    )
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
/// struct answers the empty construction — its `Self` spans the one
/// reserved unit word, so the construction carries zero dispatches and
/// the member's return keeps the ABI's one-word minimum. Refuses
/// otherwise exactly like `hash`: a generic declaration at the
/// attribute, a non-data declaration, a function-typed or `~Copyable`
/// part.
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

// ---- the union-default member annotation (the 2026-11-15 ruling) ----

/// One case of a collected enum, the `#[default]` annotation carried
/// with it.
struct AnnotatedCase {
    /// Case name.
    name: String,
    /// The case's payload.
    payload: EnumPayload,
    /// The annotation's span, `None` when the case carries none.
    default: Option<Span>,
}

/// One enum of the expanded file: its cases with their annotations and
/// whether a default derivation names the union.
struct AnnotatedEnum {
    /// The enum's name.
    name: String,
    /// One entry per case, in declaration order.
    cases: Vec<AnnotatedCase>,
    /// A default derivation names this union: the enum derives
    /// `default` itself, or a defaulting struct field or annotated
    /// member's part spells it (transitively). An annotation nothing
    /// reads refuses.
    targeted: bool,
}

/// The per-file union-default context: the `#[default]` member
/// annotations of this file's enums and the default targets a
/// derivation reads them through. The annotation is declaration data,
/// so the family reads it only inside the declaring file (G4) — a
/// foreign union answers through its own `default` member, whatever
/// derived it there.
struct UnionDefaults {
    /// One entry per declared enum, in declaration order.
    enums: Vec<AnnotatedEnum>,
}

/// The members of a union type expression flattened, `None` when a
/// side spells other than a bare single-segment name.
fn union_members(ty: &Type) -> Option<Vec<String>> {
    match ty {
        Type::Paren { inner, .. } => union_members(inner),
        Type::Union { left, right, .. } => {
            let mut members = union_members(left)?;
            members.extend(union_members(right)?);
            Some(members)
        },
        Type::Path { path, args, .. } if args.is_empty() => match path.segments.as_slice() {
            [only] => Some(vec![only.name.clone()]),
            _ => None,
        },
        _ => None,
    }
}

/// Whether the item's derive attributes name the family.
fn derives(item: &Item, family: &str) -> bool {
    item.attrs.iter().any(|attr| {
        attr.name.name == "derive"
            && attr
                .args
                .iter()
                .any(|arg| single_segment(arg) == Some(family))
    })
}

impl UnionDefaults {
    /// Collects the file's enum annotations and walks the default
    /// targets, refusing the classes the annotation admits nowhere: a
    /// second annotated member of one union, an annotation no default
    /// derivation reads.
    fn collect(items: &[&Item], diags: &mut Vec<Diagnostic>) -> Self {
        let mut unions = Self {
            enums: items
                .iter()
                .filter_map(|item| match &item.kind {
                    ItemKind::Enum(decl) => Some(AnnotatedEnum {
                        name: decl.name.name.clone(),
                        cases: decl
                            .cases
                            .iter()
                            .map(|case| AnnotatedCase {
                                name: case.name.name.clone(),
                                payload: case.payload.clone(),
                                default: case.default,
                            })
                            .collect(),
                        targeted: false,
                    }),
                    _ => None,
                })
                .collect(),
        };
        // The target walk seeds from what a `default` derivation reads:
        // a deriving enum names itself, a deriving struct's fields key
        // their type expressions, and a newly reached union seeds its
        // annotated member's payload parts — the parts the family
        // inlines the same way.
        let mut work: Vec<Type> = Vec::new();
        for &item in items {
            if !derives(item, "default") {
                continue;
            }
            match &item.kind {
                ItemKind::Enum(decl) => unions.reach(&decl.name.name, &mut work),
                ItemKind::Struct(decl) => {
                    for member in &decl.members {
                        if let StructMember::Field(field) = member {
                            work.push(field.ty.clone());
                        }
                    }
                },
                _ => {},
            }
        }
        while let Some(ty) = work.pop() {
            match &ty {
                Type::Paren { inner, .. } => work.push((**inner).clone()),
                Type::Tuple { items, .. } => work.extend(items.iter().cloned()),
                Type::Path { path, args, .. } if args.is_empty() => {
                    if let [only] = path.segments.as_slice() {
                        unions.reach(&only.name, &mut work);
                    }
                },
                Type::Union { .. } => {
                    if let Some(members) = union_members(&ty)
                        && let Some(owner) = unions.union_owner(&members)
                    {
                        unions.reach_index(owner, &mut work);
                    }
                },
                _ => {},
            }
        }
        unions.refuse_misuse(diags);
        unions
    }

    /// Marks the named union a default target, seeding its annotated
    /// member's payload parts when the mark is new.
    fn reach(&mut self, name: &str, work: &mut Vec<Type>) {
        if let Some(index) = self.enums.iter().position(|e| e.name == name) {
            self.reach_index(index, work);
        }
    }

    fn reach_index(&mut self, index: usize, work: &mut Vec<Type>) {
        if self.enums[index].targeted {
            return;
        }
        self.enums[index].targeted = true;
        for case in &self.enums[index].cases {
            if case.default.is_some() {
                match &case.payload {
                    EnumPayload::Tuple(types) => work.extend(types.iter().cloned()),
                    EnumPayload::Struct(fields) => {
                        work.extend(fields.iter().map(|field| field.ty.clone()));
                    },
                    EnumPayload::None => {},
                }
            }
        }
    }

    /// The unique enum whose cases name every member, `None` when a
    /// member spells no case or two enums claim one name.
    fn union_owner(&self, members: &[String]) -> Option<usize> {
        let mut owner: Option<usize> = None;
        for member in members {
            let mut found = None;
            for (index, decl) in self.enums.iter().enumerate() {
                if decl.cases.iter().any(|case| case.name == *member) {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(index);
                }
            }
            let found = found?;
            match owner {
                None => owner = Some(found),
                Some(previous) if previous == found => {},
                Some(_) => return None,
            }
        }
        owner
    }

    /// The union's default member: `Err` on the duplicate the
    /// collection refused, `Ok(None)` when the union carries no
    /// annotation (the ordinary dispatch answers), `Ok(Some)` the one
    /// annotated member.
    fn member(&self, name: &str) -> Result<Option<&AnnotatedCase>, NoDefault> {
        let Some(index) = self.enums.iter().position(|e| e.name == name) else {
            return Ok(None);
        };
        let annotated = self.annotated(index);
        match annotated.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one)),
            _ => Err(NoDefault::AlreadyRefused),
        }
    }

    /// The union's annotated members: one entry per annotated case.
    fn annotated(&self, index: usize) -> Vec<&AnnotatedCase> {
        self.enums[index]
            .cases
            .iter()
            .filter(|case| case.default.is_some())
            .collect()
    }

    /// The annotation misuse the collection refuses: a duplicated
    /// choice (two members of one union annotated) and an annotation
    /// no default derivation reads.
    fn refuse_misuse(&self, diags: &mut Vec<Diagnostic>) {
        for (index, decl) in self.enums.iter().enumerate() {
            let annotated = self.annotated(index);
            if annotated.len() > 1 {
                diags.push(duplicate_annotation(&decl.name, &annotated));
            } else if let [one] = annotated.as_slice()
                && !decl.targeted
            {
                diags.push(unread_annotation(
                    one.default.expect("an annotated case carries the span"),
                    &decl.name,
                    &one.name,
                ));
            }
        }
    }
}

/// The second `#[default]` member annotation of one union: one member
/// names the union's default, so a second choice is the duplicate the
/// family refuses.
fn duplicate_annotation(union: &str, annotated: &[&AnnotatedCase]) -> Diagnostic {
    let first = annotated[0];
    let second = annotated[1];
    Diagnostic::error(
        ErrorCode::DeriveMemberAnnotation,
        format!(
            "the union `{union}` carries {} `#[default]` member annotations; one member names the \
             union's default [G1]",
            annotated.len()
        ),
    )
    .with_primary(
        second.default.expect("an annotated case carries the span"),
        "a second default member",
    )
    .with_secondary(
        first.default.expect("an annotated case carries the span"),
        format!("`{}` already carries the annotation", first.name),
    )
}

/// An annotation no default derivation reads: the `#[default]` choice
/// only a `default` family expansion consults, so a union nothing
/// defaults over refuses it.
fn unread_annotation(span: Span, union: &str, case: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveMemberAnnotation,
        format!(
            "`#[default]` on the member `{case}` of the union `{union}` annotates a default \
             derivation; none names the union [G1]"
        ),
    )
    .with_primary(
        span,
        "derive `default` over the union or remove the annotation",
    )
    .with_note(
        "the annotation reads inside its declaring file: a `#[derive(default)]` on the union, or \
         a defaulting struct field spelling the union or its members",
    )
}

/// Synthesizes `impl <decl> { fn default() -> Self { ... } }`: the
/// receiverless construction (the 2026-11-15 ruling) — the member lives
/// in the type's own impl with no receiver parameter, callable as
/// `<type>.default()` through the type-keyed surface with no instance
/// word. The struct body is the construction with one default per field
/// keyed by the field's type expression — a bare type name the
/// `<name>.default()` dispatch (the primitives carry their own ambient
/// `default` members), a tuple the per-element construction, an
/// optional `none`, an application the empty construction
/// (`list[int]()`, `map[k, v]()`) — a part-less struct the unit-word
/// construction `X { }`; the enum body is the first case, which must be
/// a unit case — or, when one case carries the `#[default]` member
/// annotation, that member's construction, payloaded cases included
/// (the union-default ruling) — and the tag body is the tag itself. A field whose type
/// spells no construction key refuses at the attribute (a union naming
/// the open ruling, a shape the key cannot reach naming itself), and so
/// does an enum whose first case carries a payload or an enum with no
/// cases; a field type the key reaches but whose construction the
/// language refuses (a bare name carrying no `default` member, a
/// one-argument head that is not the empty-construction shape) fails at
/// the synthesized call with the standard diagnostic, the family
/// precedent. Refuses otherwise exactly like `clone`: a generic
/// declaration at the attribute, a non-data declaration, a
/// function-typed or `~Copyable` part.
fn default_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    unions: &UnionDefaults,
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
        diags.push(generic_refused(attr.span, "default"));
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
                    "default",
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
            }
            let body = match struct_default_body(&decl.name, &fields, mint, unions) {
                Ok(body) => body,
                Err(refused) => {
                    // A `None` because-clause rides the duplicate the
                    // collection already refused; the refusal needs no
                    // second diagnostic.
                    if let Some(because) = refused.because {
                        diags.push(part_refused(
                            refused.span,
                            "default",
                            "field",
                            &refused.name,
                            &because,
                        ));
                    }
                    return None;
                },
            };
            (&decl.name, decl.generics.as_slice(), vec![expr_stmt(body)])
        },
        ItemKind::Enum(decl) => {
            let annotated: Vec<usize> = decl
                .cases
                .iter()
                .enumerate()
                .filter_map(|(index, case)| case.default.map(|_| index))
                .collect();
            let body = match annotated.as_slice() {
                // No annotation keeps the first-case rule.
                [] => {
                    let Some(case) = decl.cases.first() else {
                        diags.push(part_refused(
                            attr.span,
                            "default",
                            "enum",
                            &decl.name.name,
                            "an enum with no cases has no default value",
                        ));
                        return None;
                    };
                    if !matches!(case.payload, EnumPayload::None) {
                        diags.push(part_refused(
                            case.span,
                            "default",
                            "enum case",
                            &case.name.name,
                            "the family defaults to the first case, which must be a unit case",
                        ));
                        return None;
                    }
                    path_expr(&case.name.name, mint.span())
                },
                // The annotated member: its construction answers
                // through the receiverless surface, the parts keyed by
                // the family's ordinary per-part rules.
                [index] => {
                    let case = &decl.cases[*index];
                    for (part, ty, span) in payload_parts(&case.payload, &case.name.name) {
                        check_part(ty, span, "default", &part, "case part", linear, diags)?;
                    }
                    match case_default(&case.name.name, &case.payload, unions, mint) {
                        Ok(body) => body,
                        Err(why) => {
                            if let Some(because) = why.refusal() {
                                diags.push(part_refused(
                                    case.span,
                                    "default",
                                    "case",
                                    &case.name.name,
                                    &because,
                                ));
                            }
                            return None;
                        },
                    }
                },
                // The duplicate member annotation the collection
                // refused; its diagnostic is the collection's.
                _ => return None,
            };
            (&decl.name, decl.generics.as_slice(), vec![expr_stmt(body)])
        },
        ItemKind::Tag(decl) => {
            let none: &[TypeParam] = &[];
            (&decl.name, none, vec![expr_stmt(path_expr(
                &decl.name.name,
                mint.span(),
            ))])
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
        default_member(mint),
        body,
        mint,
    ))
}

/// Why a written field type spells no default construction: the
/// type-expression key refuses the shape at the attribute, never at the
/// synthesized call.
enum NoDefault {
    /// `a | b` naming no annotated member: no member is canonical, so
    /// the family refuses naming the ambiguity rather than inventing
    /// one — the `#[default]` member annotation names the choice.
    Union,
    /// A shape the key spells no construction for: a multi-segment
    /// path, an intersection, a difference, a pointer, a foreign atom,
    /// or the `type` keyword.
    Unkeyed,
    /// An application of more type arguments than any empty
    /// construction spells.
    Arity(usize),
    /// A two-argument application whose head is not `map`: the
    /// two-argument empty construction is the `map[k, v]()` spelling
    /// alone (the grammar pins the form to that head).
    NonMapPair,
    /// A type argument the constructor brackets spell no key for (an
    /// optional or a union inside the brackets).
    Arg,
    /// A duplicate member annotation the collection already refused:
    /// the reading site refuses over it with no second diagnostic.
    AlreadyRefused,
}

impl NoDefault {
    /// The refusal's because-clause, `None` when the collection
    /// already reported the duplicate this reading site refuses over.
    fn refusal(self) -> Option<String> {
        match self {
            Self::AlreadyRefused => None,
            _ => Some(self.because()),
        }
    }

    /// The refusal's because-clause.
    fn because(self) -> String {
        match self {
            Self::Union => "no union member is canonical; annotate one member with `#[default]` \
                            or write the impl"
                .to_owned(),
            Self::Unkeyed => {
                "the type-expression key spells no default construction for this shape".to_owned()
            },
            Self::Arity(count) => {
                format!("the empty construction spells no application of {count} type arguments")
            },
            Self::NonMapPair => "a two-argument application constructs empty only through the \
                                 `map[k, v]()` spelling"
                .to_owned(),
            Self::Arg => "the constructor brackets spell no key for this type argument".to_owned(),
            // The collection reported the duplicate; the because-clause
            // never renders.
            Self::AlreadyRefused => "a duplicate `#[default]` member annotation".to_owned(),
        }
    }
}

/// One struct field whose type expression spells no default
/// construction: the refusal carries the field and the rendered reason
/// back to the attribute.
struct RefusedField {
    span: Span,
    name: String,
    because: Option<String>,
}

/// `default`'s member: receiverless, `Self` back — the type-keyed
/// surface's own shape.
fn default_member(mint: &mut Mint) -> Member {
    Member {
        trait_name: None,
        fn_name: "default",
        params: Vec::new(),
        ret: name_type(mint, "Self"),
    }
}

/// One field's default construction keyed by its type expression's
/// shape: a bare type name dispatches `<name>.default()` through the
/// receiverless surface; a tuple constructs per element; `t?` answers
/// `none`; a one-argument application constructs empty (`list[int]`)
/// and the two-argument `map` application likewise; a parenthesized
/// type keys by its inner shape; the unit type answers the unit value.
/// Every other shape refuses — a union names the open ruling, anything
/// else names the unkeyed shape.
fn default_value(ty: &Type, mint: &mut Mint, unions: &UnionDefaults) -> Result<Expr, NoDefault> {
    match ty {
        Type::Paren { inner, .. } => default_value(inner, mint, unions),
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return Err(NoDefault::Unkeyed);
            };
            match args.as_slice() {
                [] => match unions.member(&only.name) {
                    // The bare name of a union this file annotates:
                    // the member's construction answers over the
                    // dispatch (the union-default ruling).
                    Err(_) => Err(NoDefault::AlreadyRefused),
                    Ok(None) => Ok(type_keyed_call(&only.name, "default", mint)),
                    Ok(Some(case)) => case_default(&case.name, &case.payload, unions, mint),
                },
                [arg] => {
                    let index = bracket_arg(arg, mint)?;
                    let recv_span = mint.span();
                    let recv = path_expr(&only.name, recv_span);
                    let callee = Expr::Index {
                        span: mint.span(),
                        recv: Box::new(recv),
                        index: Box::new(index),
                    };
                    Ok(Expr::Call {
                        span: mint.span(),
                        callee: Box::new(callee),
                        args: Vec::new(),
                        trailing: None,
                    })
                },
                [key, value] => {
                    if only.name != "map" {
                        return Err(NoDefault::NonMapPair);
                    }
                    let key = bracket_arg(key, mint)?;
                    let value = bracket_arg(value, mint)?;
                    let apply_span = mint.span();
                    let head = Ident {
                        span: mint.span(),
                        name: only.name.clone(),
                    };
                    let callee = Expr::MapApply {
                        span: apply_span,
                        head,
                        key: Box::new(key),
                        value: Box::new(value),
                    };
                    Ok(Expr::Call {
                        span: mint.span(),
                        callee: Box::new(callee),
                        args: Vec::new(),
                        trailing: None,
                    })
                },
                many => Err(NoDefault::Arity(many.len())),
            }
        },
        Type::Tuple { items, .. } => {
            let span = mint.span();
            let elements = items
                .iter()
                .map(|item| default_value(item, mint, unions))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Tuple {
                span,
                items: elements,
                trailing_comma: false,
            })
        },
        Type::Unit(_) => Ok(Expr::Tuple {
            span: mint.span(),
            items: Vec::new(),
            trailing_comma: false,
        }),
        Type::Optional { .. } => Ok(path_expr("none", mint.span())),
        Type::Union { .. } => {
            // A union spelled over one enum's cases: the one annotated
            // member names the union's default (the union-default
            // ruling); every other spelling keeps the ambiguity
            // refusal.
            let members = union_members(ty).ok_or(NoDefault::Union)?;
            let owner = unions.union_owner(&members).ok_or(NoDefault::Union)?;
            match unions.member(&unions.enums[owner].name) {
                Err(_) => Err(NoDefault::AlreadyRefused),
                // The annotated member must sit among the field's own
                // members: a choice outside the spelled union names no
                // canonical member of it.
                Ok(Some(case)) if members.contains(&case.name) => {
                    case_default(&case.name, &case.payload, unions, mint)
                },
                Ok(None | Some(_)) => Err(NoDefault::Union),
            }
        },
        _ => Err(NoDefault::Unkeyed),
    }
}

/// One constructor-bracket type argument (the `t` of `list[t]`), the
/// expression-position type spellings the brackets admit: a bare name,
/// a nested one-argument application, the `map[k, v]` application, a
/// parenthesized or tuple type, or a fn type — the checker's
/// `ctor_type_arg` acceptance mirrored, so an empty construction the
/// language cannot spell refuses here naming the argument.
fn bracket_arg(ty: &Type, mint: &mut Mint) -> Result<Expr, NoDefault> {
    match ty {
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return Err(NoDefault::Arg);
            };
            match args.as_slice() {
                [] => Ok(path_expr(&only.name, mint.span())),
                [arg] => {
                    let index = bracket_arg(arg, mint)?;
                    let recv_span = mint.span();
                    let recv = path_expr(&only.name, recv_span);
                    Ok(Expr::Index {
                        span: mint.span(),
                        recv: Box::new(recv),
                        index: Box::new(index),
                    })
                },
                [key, value] if only.name == "map" => {
                    let key = bracket_arg(key, mint)?;
                    let value = bracket_arg(value, mint)?;
                    let span = mint.span();
                    let head = Ident {
                        span: mint.span(),
                        name: only.name.clone(),
                    };
                    Ok(Expr::MapApply {
                        span,
                        head,
                        key: Box::new(key),
                        value: Box::new(value),
                    })
                },
                _ => Err(NoDefault::Arg),
            }
        },
        Type::Paren { inner, .. } => Ok(Expr::Paren {
            span: mint.span(),
            inner: Box::new(bracket_arg(inner, mint)?),
        }),
        Type::Tuple { items, .. } => {
            let span = mint.span();
            let elements = items
                .iter()
                .map(|item| bracket_arg(item, mint))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Tuple {
                span,
                items: elements,
                trailing_comma: false,
            })
        },
        // The written type's own spans ride along, the hand-written
        // spelling's discipline: the resolver walks them as type uses
        // either way.
        Type::Fn { .. } => Ok(Expr::FnType {
            span: mint.span(),
            ty: Box::new(ty.clone()),
        }),
        _ => Err(NoDefault::Arg),
    }
}

/// The annotated member's default construction (the union-default
/// ruling): a bare case the case name itself, a tuple payload the call
/// over per-part defaults, a struct payload the named-argument call —
/// every part keyed by the family's ordinary per-part rules, a part
/// spelling no construction refusing through them.
fn case_default(
    name: &str,
    payload: &EnumPayload,
    unions: &UnionDefaults,
    mint: &mut Mint,
) -> Result<Expr, NoDefault> {
    // One minted span shared by the construction's path and its
    // identifier: resolution records the case's name use at the
    // identifier's span and lowering looks it up by the expression's.
    let span = mint.span();
    let callee = Path {
        span,
        segments: vec![Ident {
            span,
            name: name.to_owned(),
        }],
    };
    match payload {
        EnumPayload::None => Ok(Expr::Path { span, path: callee }),
        EnumPayload::Tuple(items) => {
            let args = items
                .iter()
                .map(|ty| {
                    let value = default_value(ty, mint, unions)?;
                    Ok(CallArg {
                        span: mint.span(),
                        name: None,
                        value,
                    })
                })
                .collect::<Result<Vec<_>, NoDefault>>()?;
            Ok(Expr::Call {
                span: mint.span(),
                callee: Box::new(Expr::Path { span, path: callee }),
                args,
                trailing: None,
            })
        },
        EnumPayload::Struct(fields) => {
            let args = fields
                .iter()
                .map(|field| {
                    let name = Ident {
                        span: mint.span(),
                        name: field.name.name.clone(),
                    };
                    let value = default_value(&field.ty, mint, unions)?;
                    Ok(CallArg {
                        span: mint.span(),
                        name: Some(name),
                        value,
                    })
                })
                .collect::<Result<Vec<_>, NoDefault>>()?;
            Ok(Expr::Call {
                span: mint.span(),
                callee: Box::new(Expr::Path { span, path: callee }),
                args,
                trailing: None,
            })
        },
    }
}

/// The default member's struct body: the construction with one default
/// per field keyed by the field's type expression; a part-less struct
/// is the unit-word construction `X { }`. A field whose type spells no
/// construction key refuses, naming the field and the shape.
fn struct_default_body(
    name: &Ident,
    fields: &[&FieldDecl],
    mint: &mut Mint,
    unions: &UnionDefaults,
) -> Result<Expr, RefusedField> {
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
            let value = default_value(&field.ty, mint, unions).map_err(|why| RefusedField {
                span: field.span,
                name: field.name.name.clone(),
                because: why.refusal(),
            })?;
            Ok((field_name, Some(value)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Expr::Struct {
        span: mint.span(),
        path,
        fields: literal,
    })
}

/// One type-keyed member call `<type>.<member>()`: the receiver is the
/// bare type name at one minted span shared by the expression, the
/// path, and the identifier (the use records key by the identifier's),
/// and the call's own span mints after it.
fn type_keyed_call(type_name: &str, member: &str, mint: &mut Mint) -> Expr {
    let recv = path_expr(type_name, mint.span());
    let name = Ident {
        span: mint.span(),
        name: member.to_owned(),
    };
    Expr::Call {
        span: mint.span(),
        callee: Box::new(Expr::Field {
            span: mint.span(),
            recv: Box::new(recv),
            name,
        }),
        args: Vec::new(),
        trailing: None,
    }
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
    /// The trait the synthesized impl implements; `None` is the
    /// receiverless inherent member (`default`), whose impl carries no
    /// trait reference.
    trait_name: Option<&'static str>,
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
    let impl_generics: Vec<TypeParam> = match member.trait_name {
        Some(trait_name) => generics
            .iter()
            .map(|p| {
                let bound = match &p.bound {
                    Some(written) => Some(Type::Intersect {
                        span: p.span,
                        left: Box::new(written.clone()),
                        right: Box::new(trait_type(mint, trait_name)),
                    }),
                    None => Some(trait_type(mint, trait_name)),
                };
                TypeParam {
                    span: p.span,
                    name: p.name.clone(),
                    bound,
                }
            })
            .collect(),
        None => Vec::new(),
    };
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
    // The trait reference is a name use, so it mints its own span; an
    // inherent member's impl carries none.
    let trait_ref = member.trait_name.map(|trait_name| {
        let trait_ident = Ident {
            span: mint.span(),
            name: trait_name.to_owned(),
        };
        TypeRef {
            span: trait_ident.span,
            path: Path {
                span: trait_ident.span,
                segments: vec![trait_ident],
            },
            args: Vec::new(),
        }
    });
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
        // The minted span rides the attributed declaration's bytes, so
        // the synthesized mark keeps position queries out of it.
        synthesized: true,
        kind: ItemKind::Impl(ImplDecl {
            span,
            generics: impl_generics,
            trait_ref,
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
