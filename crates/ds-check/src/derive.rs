//! The derive engine's first slice (G1): the meta-language is the
//! language, so a derive attribute expands into an ordinary impl
//! declaration — AST-level synthesis after parse, before resolution —
//! and the synthesized impl flows through the normal checker (trait
//! coherence, signature agreement) and the normal lowering. Nothing in
//! the emission paths knows a derive exists.
//!
//! `show` is the landed family: one member answering the construction
//! spelling of the data. Every other name in the ruled set (`hash`,
//! `ord`, `clone`, `to_json`, `from_json`, `default`) parses and
//! refuses with a checklist diagnostic naming it unimplemented; `==`
//! refuses with the B9 diagnostic (equality is universal and never
//! derived); unknown names refuse naming the ruled set; a
//! function-typed or `~Copyable` part refuses naming the part; a
//! declaration carrying its own type parameters refuses (the runtime
//! seam lowers one member body per impl, so a receiver whose shape
//! spans those parameters waits on the JIT's per-instantiation frames).

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    Ast, Attribute, BlockExpr, EnumCase, EnumDecl, EnumPayload, Expr, FieldDecl, FnDecl, Ident,
    ImplDecl, Item, ItemKind, MatchArm, Param, ParamMode, Path, Pattern, Stmt, StmtKind, StrPart,
    StructMember, Type, TypeParam, TypeRef,
};

/// The ruled derive vocabulary (G1), in ruling order.
pub const RULED_SET: &str = "hash, show, ord, clone, to_json, from_json, default";

/// The families this round has not implemented yet, in ruling order.
const UNIMPLEMENTED: &[&str] = &["hash", "ord", "clone", "to_json", "from_json", "default"];

/// The written type names the interpolation context prints itself (F6);
/// every other part derives through a `show` dispatch.
const PRINTABLE: &[&str] = &["int", "float", "bool", "char", "string"];

/// Expands every derive attribute in the package: consumed attributes
/// are stripped and their impls appended beside the attributed item,
/// refused names keep the attribute and contribute a diagnostic. Files
/// without derive attributes come back byte-identical (a plain clone).
#[must_use]
pub fn expand(asts: &[Ast]) -> (Vec<Ast>, Vec<Diagnostic>) {
    if !asts.iter().any(has_derive) {
        return (asts.to_vec(), Vec::new());
    }
    let linear = linear_heads(asts);
    let mut diags = Vec::new();
    let mut out = Vec::with_capacity(asts.len());
    for ast in asts {
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
fn linear_heads(asts: &[Ast]) -> Vec<String> {
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
                "show" => match show_impl(item, attr, linear, diags) {
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
fn generic_refused(span: Span) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveGenericRefused,
        "derive `show` refuses a generic declaration [G1]",
    )
    .with_primary(span, "this declaration carries its own type parameters")
    .with_note(
        "the synthesized member reads its parts through the declaration's type variables; the \
         runtime seam lowers one body per impl, and per-instantiation frames are the JIT \
         specialization tier's",
    )
}

/// A part (field or payload element) that refuses structural derivation.
fn part_refused(span: Span, what: &str, part: &str, because: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DerivePartRefused,
        format!("derive `show` refuses the {what} `{part}`: {because} [G1, B9]"),
    )
    .with_primary(span, "this part carries no structural show")
}

/// Synthesizes `impl show for <decl> { fn show(self) -> string { ... } }`.
/// Refuses (returning `None`, diagnostic pushed) when the item is not a
/// data declaration or a part refuses structural derivation.
fn show_impl(
    item: &Item,
    attr: &Attribute,
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
        diags.push(generic_refused(attr.span));
        return None;
    }
    let mut mint = Mint::new(item.span);
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
                    &field.name.name,
                    "field",
                    linear,
                    diags,
                )?;
            }
            let body = struct_body(&decl.name.name, &fields, &mut mint);
            Some(impl_item(attr, &decl.name, &decl.generics, body, &mut mint))
        },
        ItemKind::Enum(decl) => {
            for case in &decl.cases {
                let parts = payload_parts(&case.payload, &case.name.name);
                for (part_name, part, span) in parts {
                    check_part(part, span, &part_name, "enum case payload", linear, diags)?;
                }
            }
            let body = enum_body(decl, &mut mint);
            Some(impl_item(attr, &decl.name, &decl.generics, body, &mut mint))
        },
        ItemKind::Tag(decl) => {
            let body = name_body(&decl.name.name, &mut mint);
            Some(impl_item(attr, &decl.name, &[], body, &mut mint))
        },
        _ => {
            diags.push(
                Diagnostic::error(
                    ErrorCode::DeriveNonDataItem,
                    "a derive applies to a struct, enum, or tag declaration [G1]",
                )
                .with_primary(attr.span, "this declaration derives nothing"),
            );
            None
        },
    }
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
    part: &str,
    what: &str,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<()> {
    match ty {
        Type::Fn { .. } => {
            diags.push(part_refused(
                span,
                what,
                part,
                "a function-typed part compares and shows as no structural value",
            ));
            None
        },
        Type::Negation { .. } => {
            diags.push(part_refused(
                span,
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
                check_part(item, span, part, what, linear, diags)?;
            }
            Some(())
        },
        Type::Optional { inner, .. } => check_part(inner, span, part, what, linear, diags),
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

/// The assembled impl item: `impl show for <target> { fn show ... }`.
fn impl_item(
    attr: &Attribute,
    name: &Ident,
    generics: &[TypeParam],
    body: Expr,
    mint: &mut Mint,
) -> Item {
    let span = attr.span;
    // A generic declaration derives per instantiation: the impl declares
    // the same parameters with `show` added to whatever bound the
    // declaration wrote, and the target applies them in order.
    let impl_generics: Vec<TypeParam> = generics
        .iter()
        .map(|p| {
            let bound = match &p.bound {
                Some(written) => Some(Type::Intersect {
                    span: p.span,
                    left: Box::new(written.clone()),
                    right: Box::new(show_type(mint)),
                }),
                None => Some(show_type(mint)),
            };
            TypeParam {
                span: p.span,
                name: p.name.clone(),
                bound,
            }
        })
        .collect();
    let target = Type::Path {
        span: name.span,
        path: Path {
            span: name.span,
            segments: vec![name.clone()],
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
    // The trait reference and the return type are name uses, so each
    // mints its own span; the fn name and the receiver parameter are
    // declarations and keep the attribute's span.
    let show = Ident {
        span: mint.span(),
        name: "show".to_owned(),
    };
    let fn_decl = FnDecl {
        span,
        name: Ident {
            span,
            name: "show".to_owned(),
        },
        generics: Vec::new(),
        params: vec![Param {
            span,
            mode: ParamMode::Borrow,
            name: Ident {
                span,
                name: "self".to_owned(),
            },
            ty: None,
            default: None,
        }],
        ret: Some(Type::Path {
            span: mint.span(),
            path: Path {
                span: mint.span(),
                segments: vec![Ident {
                    span: mint.span(),
                    name: "string".to_owned(),
                }],
            },
            args: Vec::new(),
        }),
        body: Some(BlockExpr {
            span,
            stmts: vec![Stmt {
                span: body.span(),
                kind: StmtKind::Expr(body),
                leading_trivia: Vec::new(),
                trailing_trivia: Vec::new(),
            }],
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
                span: show.span,
                path: Path {
                    span: show.span,
                    segments: vec![show],
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

/// The `show` trait as a written type, at a fresh minted span.
fn show_type(mint: &mut Mint) -> Type {
    let span = mint.span();
    Type::Path {
        span,
        path: Path {
            span,
            segments: vec![Ident {
                span,
                name: "show".to_owned(),
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
/// bytes collide with no real expression span, and distinct one-byte
/// slices keep the checker's span-keyed tables (name uses, expression
/// types, trait calls, binder facts) collision-free. The budget is the
/// item's whole text — several bytes per part against three minted
/// spans per part — so exhaustion needs a part shorter than its own
/// minted spans; the cycle start-over is the degenerate guard,
/// degrading a diagnostic's span only.
struct Mint {
    file: ds_diag::FileId,
    start: u32,
    next: u32,
    end: u32,
}

impl Mint {
    fn new(item: Span) -> Self {
        Self {
            file: item.file,
            start: item.start,
            next: item.start,
            end: item.end.saturating_sub(1),
        }
    }

    fn span(&mut self) -> Span {
        if self.next >= self.end {
            self.next = self.start;
        }
        let span = Span::new(self.file, self.next, self.next + 1);
        self.next += 1;
        span
    }
}
