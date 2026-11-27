//! The AST-minting vocabulary every derive family shares: the span
//! allocator (`Mint`), the impl/member/parameter skeletons, and the
//! expression and statement builders a synthesized body composes.

use super::*;

/// Allocates the synthesized nodes' spans inside the attributed item's
/// own byte range. The declaration carries no expressions, but its
/// written types record name uses of their own (a field type's
/// `int`, a `list[t]` head), so the mint skips the declaration's
/// written identifier and type spans: a minted span colliding with a
/// written use would key two resolutions into one span. Distinct
/// slices keep the checker's span-keyed tables (name uses, expression
/// types, trait calls, binder facts, member fns' solved returns)
/// collision-free. A pass spends one span per byte; a spent pass is
/// followed by one over one-byte-longer spans, so the budget grows
/// with the square of the item's length while every synthesized node —
/// the member fn declarations included — mints only a few spans per
/// part (the `from_json` walk's few dozen against a part's few bytes),
/// leaving the budget better than two spans per span spent on every
/// reachable declaration: the wrap never runs out, its cycle start-over
/// degrading a diagnostic's span only.
pub(super) struct Mint {
    pub(super) file: ds_diag::FileId,
    pub(super) start: u32,
    pub(super) next: u32,
    pub(super) end: u32,
    pub(super) len: u32,
    /// The declaration's written identifier and type spans, skipped by
    /// every mint: the resolver records a use per one of them.
    pub(super) forbidden: std::collections::HashSet<Span>,
}

impl Mint {
    pub(super) fn new(item: &Item) -> Self {
        let mut forbidden = std::collections::HashSet::new();
        forbidden.insert(item.span);
        written_spans(item, &mut forbidden);
        Self {
            file: item.span.file,
            start: item.span.start,
            next: item.span.start,
            end: item.span.end.saturating_sub(1),
            len: 1,
            forbidden,
        }
    }

    pub(super) fn span(&mut self) -> Span {
        // Every candidate is a subspan of `[start, end)` (one pass per
        // length), so the space holds at most `range*(range+1)/2` spans; a
        // search that exhausts it means `forbidden` covers the whole item
        // (a length-1 declaration, unreachable from real derives). The
        // fallback spends the item's first byte, colliding with a written
        // span -- the degradation the header names -- rather than looping.
        let span_budget = u64::from(self.end.saturating_sub(self.start));
        let candidates = span_budget * (span_budget + 1) / 2 + 1;
        for _ in 0..candidates {
            let range = self.end.saturating_sub(self.start);
            if self.len > range || self.next + self.len > self.end {
                self.next = self.start;
                self.len = if self.len >= range { 1 } else { self.len + 1 };
            }
            let span = Span::new(self.file, self.next, self.next + self.len);
            self.next += 1;
            if !self.forbidden.contains(&span) {
                return span;
            }
        }
        Span::new(self.file, self.start, self.start + 1)
    }
}

/// The assembled impl item:
/// `impl <trait> for <target> { fn <member> ... }`.
pub(super) fn impl_item(
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
    // declaration wrote, and the target applies them in order. The
    // written bound rides the structure-only minted copy — resolution
    // records name uses by span, and the written declaration already
    // used the original's. An inherent member carries no trait to add,
    // so its parameters pass through plainly — the target still applies
    // them.
    let impl_generics: Vec<TypeParam> = match member.trait_name {
        Some(trait_name) => generics
            .iter()
            .map(|p| {
                let bound = match &p.bound {
                    Some(written) => Some(Type::Intersect {
                        span: p.span,
                        left: Box::new(minted_type(written, mint)),
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
        None => generics.to_vec(),
    };
    // The target is a name use, so it mints its own span: two families
    // expanding one item each synthesize an impl, and the declaration's
    // own name span cannot serve both targets' use records. The applied
    // arguments mint their own spans for the same reason — a generic
    // declaration's written parameter span already carries the
    // declaration's own use, so one shared span would key two impls'
    // resolutions into one use record.
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
            .map(|p| name_type(mint, &p.name.name))
            .collect(),
        delim: Delim::Bracket,
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
        bind: None,
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
            name: None,
            fns: vec![fn_decl],
        }),
        leading_trivia: Vec::new(),
        doc: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

/// The synthesized impl's member: the trait it implements, the fn's
/// name, its parameters (the receiver first, spelled as its trait
/// member spells it), and its return type.
pub(super) struct Member {
    /// The trait the synthesized impl implements; `None` is the
    /// receiverless inherent member (`default`), whose impl carries no
    /// trait reference.
    pub(super) trait_name: Option<&'static str>,
    pub(super) fn_name: &'static str,
    pub(super) params: Vec<Param>,
    pub(super) ret: Type,
}

/// The receiver parameter, a declaration keeping the attribute's span.
pub(super) fn self_param(span: Span) -> Param {
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
pub(super) fn name_type(mint: &mut Mint, name: &str) -> Type {
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
        delim: Delim::Bracket,
    }
}

/// One synthesized statement over an expression, the span shared.
pub(super) fn expr_stmt(expr: Expr) -> Stmt {
    Stmt {
        span: expr.span(),
        kind: StmtKind::Expr(expr),
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

/// A binding statement at minted spans.
pub(super) fn local(mint: &mut Mint, name: &str, init: Expr, mutable: bool) -> Stmt {
    let span = mint.span();
    Stmt {
        span,
        kind: StmtKind::Local(LocalDecl {
            span,
            mutable,
            pat: Pattern::Binding {
                span,
                name: Ident {
                    span,
                    name: name.to_owned(),
                },
            },
            ty: None,
            init,
        }),
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

/// A typed mutable binding statement at minted spans: the var's slot
/// span is sized by its declared type, so a union-typed var stores
/// whichever member a later assignment widens into it.
pub(super) fn local_typed(mint: &mut Mint, name: &str, ty: Type, init: Expr) -> Stmt {
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
                    name: name.to_owned(),
                },
            },
            ty: Some(ty),
            init,
        }),
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

/// `name = value` at minted spans.
pub(super) fn assign(mint: &mut Mint, name: &str, value: Expr) -> Stmt {
    expr_stmt(Expr::Assign {
        span: mint.span(),
        op: AssignOp::Plain,
        left: Box::new(path_expr(name, mint.span())),
        right: Box::new(value),
    })
}

/// `name += 1` at minted spans.
pub(super) fn assign_add(mint: &mut Mint, name: &str) -> Stmt {
    expr_stmt(Expr::Assign {
        span: mint.span(),
        op: AssignOp::Add,
        left: Box::new(path_expr(name, mint.span())),
        right: Box::new(int_answer(mint, "1")),
    })
}

/// A block expression over one value statement.
pub(super) fn block_of(mint: &mut Mint, value: Expr) -> BlockExpr {
    BlockExpr {
        span: mint.span(),
        stmts: vec![expr_stmt(value)],
        trailing_trivia: Vec::new(),
    }
}

/// A minted one-segment path over a declaration name, the path and its
/// identifier sharing one span (resolution records the name use at the
/// identifier's span and lowering looks it up by the path's).
pub(super) fn minted_path(name: &str, mint: &mut Mint) -> Path {
    let span = mint.span();
    Path {
        span,
        segments: vec![Ident {
            span,
            name: name.to_owned(),
        }],
    }
}

pub(super) fn text(span: Span, s: &str) -> StrPart {
    StrPart::Text {
        span,
        text: s.to_owned(),
    }
}

pub(super) fn str_expr(span: Span, parts: Vec<StrPart>) -> Expr {
    Expr::Str { span, parts }
}

/// `self` as an expression; each occurrence mints one span shared by the
/// expression, the path, and the identifier — resolution records the
/// use at the identifier's span and lowering looks it up by the
/// expression's, which coincide for a one-segment path.
pub(super) fn self_expr(mint: &mut Mint) -> Expr {
    path_expr("self", mint.span())
}

/// A one-segment path expression over one minted span.
pub(super) fn path_expr(name: &str, span: Span) -> Expr {
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

/// The derived trait as a written bound, at one fresh minted span
/// shared by the type, the path, and the identifier.
pub(super) fn trait_type(mint: &mut Mint, name: &str) -> Type {
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
        delim: Delim::Bracket,
    }
}

/// A `parse_err` construction at its zero position: the decode walks
/// read already-decoded values, so no source position exists to name.
pub(super) fn parse_err_expr(mint: &mut Mint) -> Expr {
    Expr::Call {
        span: mint.span(),
        callee: Box::new(path_expr("parse_err", mint.span())),
        args: vec![CallArg {
            span: mint.span(),
            name: Some(Ident {
                span: mint.span(),
                name: "pos".to_owned(),
            }),
            value: int_answer(mint, "0"),
        }],
        trailing: None,
        selection: None,
    }
}

/// A string literal at a minted span.
pub(super) fn key_literal(mint: &mut Mint, key: &str) -> Expr {
    str_expr(mint.span(), vec![text(mint.span(), key)])
}

/// The written spans of one attributed declaration: every identifier it
/// names and every type node it spells, the spans the resolver records
/// uses at (a field type's head and arguments, a supers bound, a
/// payload's types).
pub(super) fn written_spans(item: &Item, out: &mut std::collections::HashSet<Span>) {
    match &item.kind {
        ItemKind::Struct(decl) => {
            out.insert(decl.name.span);
            for param in &decl.generics {
                out.insert(param.name.span);
                if let Some(bound) = &param.bound {
                    type_spans(bound, out);
                }
            }
            for ty in &decl.supers {
                type_spans(ty, out);
            }
            for member in &decl.members {
                if let StructMember::Field(field) = member {
                    out.insert(field.name.span);
                    type_spans(&field.ty, out);
                }
            }
        },
        ItemKind::Enum(decl) => {
            out.insert(decl.name.span);
            for param in &decl.generics {
                out.insert(param.name.span);
                if let Some(bound) = &param.bound {
                    type_spans(bound, out);
                }
            }
            for ty in &decl.supers {
                type_spans(ty, out);
            }
            for case in &decl.cases {
                out.insert(case.name.span);
                match &case.payload {
                    EnumPayload::None => {},
                    EnumPayload::Tuple(items) => {
                        for ty in items {
                            type_spans(ty, out);
                        }
                    },
                    EnumPayload::Struct(fields) => {
                        for field in fields {
                            out.insert(field.name.span);
                            type_spans(&field.ty, out);
                        }
                    },
                }
            }
        },
        ItemKind::Tag(decl) => {
            out.insert(decl.name.span);
        },
        _ => {},
    }
}

/// One written type's spans: its own node plus every identifier inside.
pub(super) fn type_spans(ty: &Type, out: &mut std::collections::HashSet<Span>) {
    out.insert(ty.span());
    match ty {
        Type::Path { path, args, .. } => {
            out.insert(path.span);
            for seg in &path.segments {
                out.insert(seg.span);
            }
            for arg in args {
                type_spans(arg, out);
            }
        },
        Type::Tuple { items, .. } => {
            for item in items {
                type_spans(item, out);
            }
        },
        Type::Optional { inner, .. } | Type::Paren { inner, .. } => {
            type_spans(inner, out);
        },
        Type::Fn { params, ret, .. } => {
            for param in params {
                if let Some(ty) = &param.ty {
                    type_spans(ty, out);
                }
            }
            if let Some(ret) = ret {
                type_spans(ret, out);
            }
        },
        Type::Negation { inner, .. } | Type::Ptr { inner, .. } => type_spans(inner, out),
        Type::Union { left, right, .. }
        | Type::Intersect { left, right, .. }
        | Type::Difference { left, right, .. } => {
            type_spans(left, out);
            type_spans(right, out);
        },
        Type::Unit(_) | Type::TypeKw(_) | Type::Foreign { .. } => {},
    }
}

/// One enum payload's element types with their names and spans. A tuple
/// element is named by its case; a struct payload names its fields.
pub(super) fn payload_parts<'a>(
    payload: &'a EnumPayload,
    case: &str,
) -> Vec<(String, &'a Type, Span)> {
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

/// The part carve-outs: a function-typed part never shows, a written
/// negation (`~Copyable`) is the linear layer, and a part headed by a linear-declared nominal is a
/// resource. layer, and a part headed by a linear-declared nominal is a resource.
/// Every other part flows through the ordinary checker: a part without
/// `show` fails at the synthesized call with the standard missing-impl
/// diagnostic over the part's own span.
pub(super) fn check_part(
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
            if let [only] = path.segments.as_slice()
                && linear.iter().any(|head| head == &only.name)
            {
                diags.push(part_refused(
                    span,
                    family,
                    what,
                    part,
                    &format!("`{}` declares the `~Copyable` linear layer", only.name),
                ));
                return None;
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

/// Whether a part's type mentions one of the declaration's own type
/// parameters: every json read or write over such a part stages the
/// variable's own type-keyed member, and the json members are never
/// bound, so the json families refuse the part at the attribute — the
/// trait families dispatch through the added bound instead.
pub(super) fn mentions_param(ty: &Type, params: &[String]) -> bool {
    match ty {
        Type::Path { path, args, .. } => {
            let named = match path.segments.as_slice() {
                [only] => params.contains(&only.name),
                _ => false,
            };
            named || args.iter().any(|arg| mentions_param(arg, params))
        },
        Type::Optional { inner, .. } | Type::Paren { inner, .. } => mentions_param(inner, params),
        Type::Tuple { items, .. } => items.iter().any(|item| mentions_param(item, params)),
        _ => false,
    }
}

/// One case's pattern with its payload bound to `prefix`-numbered
/// binders: a tuple element binds by position, a struct field through a
/// renamed binder (so the outer and the inner match never collide on a
/// name), a part-less case carries none.
pub(super) fn case_pattern(
    case: &EnumCase,
    prefix: &str,
    mint: &mut Mint,
) -> (Pattern, Vec<Ident>) {
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

/// One inherent-member call over a receiver expression:
/// `recv.name(args)` — the collection face's member spellings.
pub(super) fn member_call(recv: Expr, name: &str, args: Vec<Expr>, mint: &mut Mint) -> Expr {
    Expr::Call {
        span: mint.span(),
        callee: Box::new(Expr::Field {
            span: mint.span(),
            recv: Box::new(recv),
            name: Ident {
                span: mint.span(),
                name: name.to_owned(),
            },
        }),
        args: args
            .into_iter()
            .map(|value| CallArg {
                span: value.span(),
                name: None,
                value,
            })
            .collect(),
        trailing: None,
        selection: None,
    }
}

/// A field read of the receiver or the other operand: `base.name`.
pub(super) fn field_read(base: &str, field: &Ident, mint: &mut Mint) -> Expr {
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
pub(super) fn binder_use(binder: &Ident, mint: &mut Mint) -> Expr {
    path_expr(&binder.name, mint.span())
}

/// An integer-literal answer.
pub(super) fn int_answer(mint: &mut Mint, text: &str) -> Expr {
    Expr::Literal {
        span: mint.span(),
        lit: Literal::Int {
            suffix: String::new(),
            text: text.to_owned(),
        },
    }
}

/// One part's trait-member dispatch: `part.<member>()`, the uniform call
/// every part takes, so a missing impl names the part's own span.
pub(super) fn trait_call(part: Expr, member: &str, mint: &mut Mint) -> Expr {
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
        selection: None,
    }
}

/// Why a written field type spells no default construction: the
/// type-expression key refuses the shape at the attribute, never at the
/// synthesized call.
pub(super) enum NoDefault {
    /// `a | b` naming no annotated member: no member is canonical, so
    /// the family refuses naming the ambiguity rather than inventing
    /// one — the `#[default]` member annotation names the choice.
    Union,
    /// A shape the key spells no construction for: a multi-segment
    /// path, an intersection, a difference, a pointer, a foreign atom,
    /// or the `type` keyword.
    Unkeyed,
    /// A type argument the constructor brackets spell no key for (a
    /// union inside the brackets; an optional rides the fn-type
    /// carrier).
    Arg,
    /// A bare type variable of the declaration being derived: no
    /// default value of a variable exists (the type-keyed surface
    /// dispatches no bound), so the key refuses naming the part.
    Variable,
    /// A duplicate member annotation the collection already refused:
    /// the reading site refuses over it with no second diagnostic.
    AlreadyRefused,
}

impl NoDefault {
    /// The refusal's because-clause, `None` when the collection
    /// already reported the duplicate this reading site refuses over.
    pub(super) fn refusal(self) -> Option<String> {
        match self {
            Self::AlreadyRefused => None,
            _ => Some(self.because()),
        }
    }

    /// The refusal's because-clause.
    pub(super) fn because(self) -> String {
        match self {
            Self::Union => "no union member is canonical; annotate one member with `#[default]` \
                            or write the impl"
                .to_owned(),
            Self::Unkeyed => {
                "the type-expression key spells no default construction for this shape".to_owned()
            },
            Self::Arg => "the constructor brackets spell no key for this type argument".to_owned(),
            Self::Variable => "a type variable carries no default value".to_owned(),
            // The collection reported the duplicate; the because-clause
            // never renders.
            Self::AlreadyRefused => "a duplicate `#[default]` member annotation".to_owned(),
        }
    }
}

/// One constructor-bracket type argument (the `t` of `list[t]`), the
/// expression-position type spellings the brackets admit: a bare name,
/// a nested one-argument application, the `map[k, v]` application, a
/// parenthesized or tuple type, or a fn type — the checker's
/// `ctor_type_arg` acceptance mirrored, so an empty construction the
/// language cannot spell refuses here naming the argument.
pub(super) fn bracket_arg(ty: &Type, mint: &mut Mint) -> Result<Expr, NoDefault> {
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
        // A fn type or an optional rides the fn-type node — the one
        // expression form that holds a written type the bracket
        // spellings cannot express; the checker's bracket read unwraps
        // either way. The carried type takes the minted copy (the
        // written declaration already used the original's spans). The
        // empty construction never builds an element, so an optional
        // element's own default never runs; a union keeps the argument
        // refusal (no clause lifts it).
        Type::Fn { .. } | Type::Optional { .. } => Ok(Expr::FnType {
            span: mint.span(),
            ty: Box::new(minted_type(ty, mint)),
        }),
        _ => Err(NoDefault::Arg),
    }
}

/// A structure-only copy of one written type with every span freshly
/// minted: a synthesized expression's type uses must key by spans the
/// written declaration never used (resolution records one name use per
/// span). A fn type's parameter defaults drop on the copy — a type
/// spelling's parameter carries none.
pub(super) fn minted_type(ty: &Type, mint: &mut Mint) -> Type {
    let span = mint.span();
    match ty {
        Type::Path {
            path, args, delim, ..
        } => Type::Path {
            span,
            path: Path {
                span,
                segments: path
                    .segments
                    .iter()
                    .map(|seg| Ident {
                        span: mint.span(),
                        name: seg.name.clone(),
                    })
                    .collect(),
            },
            args: args.iter().map(|arg| minted_type(arg, mint)).collect(),
            delim: *delim,
        },
        Type::Optional { inner, .. } => Type::Optional {
            span,
            inner: Box::new(minted_type(inner, mint)),
        },
        Type::Paren { inner, .. } => Type::Paren {
            span,
            inner: Box::new(minted_type(inner, mint)),
        },
        Type::Tuple { items, .. } => Type::Tuple {
            span,
            items: items.iter().map(|item| minted_type(item, mint)).collect(),
        },
        Type::Union { left, right, .. } => Type::Union {
            span,
            left: Box::new(minted_type(left, mint)),
            right: Box::new(minted_type(right, mint)),
        },
        Type::Fn { params, ret, .. } => Type::Fn {
            span,
            params: params
                .iter()
                .map(|param| Param {
                    span: mint.span(),
                    mode: param.mode,
                    name: Ident {
                        span: mint.span(),
                        name: param.name.name.clone(),
                    },
                    ty: param.ty.as_ref().map(|t| minted_type(t, mint)),
                    default: None,
                })
                .collect(),
            ret: ret.as_ref().map(|r| Box::new(minted_type(r, mint))),
        },
        Type::Unit(_) => Type::Unit(span),
        other => other.clone(),
    }
}

/// The optional narrow: `match <name> { none => { } <inner> => { <rows>
/// } }` — the catchall binder binds the inner type as its own word (a
/// narrowed read of the optional's own word has no lowering), the
/// empty none arm contributing nothing, so absence leaves the member
/// out.
pub(super) fn presence_narrow(mint: &mut Mint, name: &str, inner: &str, rows: Vec<Stmt>) -> Stmt {
    // The pattern, its path, and its identifier share one span (the
    // use records key by the identifier's); the arm's own span mints
    // separately, since the arm's facts key by theirs.
    let none_span = mint.span();
    let binder_span = mint.span();
    let arms = vec![
        MatchArm {
            span: mint.span(),
            // The bare `none` arm is a binding pattern the checker's
            // arm tag reads as the builtin none member, never a path
            // pattern (an item reference).
            pat: Pattern::Binding {
                span: none_span,
                name: Ident {
                    span: none_span,
                    name: "none".to_owned(),
                },
            },
            ann: None,
            guard: None,
            body: Expr::Block(BlockExpr {
                span: mint.span(),
                stmts: Vec::new(),
                trailing_trivia: Vec::new(),
            }),
        },
        MatchArm {
            span: mint.span(),
            pat: Pattern::Binding {
                span: binder_span,
                name: Ident {
                    span: binder_span,
                    name: inner.to_owned(),
                },
            },
            ann: None,
            guard: None,
            body: Expr::Block(BlockExpr {
                span: mint.span(),
                stmts: rows,
                trailing_trivia: Vec::new(),
            }),
        },
    ];
    let scrutinee = path_expr(name, mint.span());
    expr_stmt(Expr::Match {
        span: mint.span(),
        scrutinee: Box::new(scrutinee),
        arms,
    })
}

/// The written type beneath any paren wrappers.
pub(super) fn paren_free(ty: &Type) -> &Type {
    match ty {
        Type::Paren { inner, .. } => paren_free(inner),
        other => other,
    }
}

/// A fresh binder name under one body-wide counter.
pub(super) fn fresh_binder(fresh: &mut usize, prefix: &str) -> String {
    let name = format!("{prefix}{fresh}");
    *fresh += 1;
    name
}

/// The driver's empty document, the seed every encode construction
/// consumes as its receiver: the doc type's own `default()` through the
/// type-keyed static tier (the driver face's convention — a driver's
/// document type answers `default()` with its empty document).
pub(super) fn doc_seed(mint: &mut Mint) -> Expr {
    let field_span = mint.span();
    let recv = path_expr("json", mint.span());
    let name_span = mint.span();
    let callee = Expr::Field {
        span: field_span,
        recv: Box::new(recv),
        name: Ident {
            span: name_span,
            name: "default".to_owned(),
        },
    };
    Expr::Call {
        span: mint.span(),
        callee: Box::new(callee),
        args: Vec::new(),
        trailing: None,
        selection: None,
    }
}

/// The encode walk's seed binding at the member's top: one
/// `const d0 = json.default()` every construction site borrows.
pub(super) const SEED: &str = "d0";

pub(super) fn doc_seed_local(mint: &mut Mint) -> Stmt {
    let init = doc_seed(mint);
    local(mint, SEED, init, false)
}

#[cfg(test)]
mod tests {
    use ds_diag::FileId;

    use super::{Mint, Span};

    /// A length-1 item span is fully forbidden (the whole-span row
    /// `Mint::new` inserts), so the candidate space holds nothing: the
    /// search must degrade to the item's first byte rather than cycle
    /// its one rejected candidate forever. Real derives never mint over
    /// a declaration shorter than the derive's own spelling, so only
    /// this hand-built mint reaches the bound.
    #[test]
    fn a_fully_covered_item_span_degrades_instead_of_looping() {
        let mut mint = Mint {
            file: FileId::new(0),
            start: 7,
            next: 7,
            end: 7,
            len: 1,
            forbidden: [Span::new(FileId::new(0), 7, 8)].into_iter().collect(),
        };
        assert_eq!(mint.span(), Span::new(FileId::new(0), 7, 8));
        // a follow-up mint answers the same fallback, never a later byte
        assert_eq!(mint.span(), Span::new(FileId::new(0), 7, 8));
    }

    /// The ordinary mint walks the item's bytes in order, skipping the
    /// forbidden rows: the first free subspan answers and the next call
    /// continues from where the last one spent.
    #[test]
    fn the_mint_spends_free_spans_in_order() {
        let file = FileId::new(0);
        let mut mint = Mint {
            file,
            start: 3,
            next: 3,
            end: 9,
            len: 1,
            forbidden: [Span::new(file, 5, 6), Span::new(file, 7, 8)]
                .into_iter()
                .collect(),
        };
        assert_eq!(mint.span(), Span::new(file, 3, 4));
        assert_eq!(mint.span(), Span::new(file, 4, 5));
        assert_eq!(mint.span(), Span::new(file, 6, 7));
        assert_eq!(mint.span(), Span::new(file, 8, 9));
    }
}
