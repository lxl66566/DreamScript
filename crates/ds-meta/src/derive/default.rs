//! `default` answers the receiverless construction (`fn default() -> Self` in
//! the type's own impl), one default per field
//! keyed by the field's type expression — a bare type name a `default`
//! dispatch, a tuple the per-element construction, an optional `none`,
//! an application over the collection heads (`list`, `map`) the empty
//! construction and every other application the instantiated `default`
//! dispatch — a part-less struct the
//! unit-word construction, an enum its first unit case, a tag itself.
//! A union's `#[default]` member annotation
//! names the one member the family synthesizes over: the annotated
//! enum defaults to that member's construction — a bare case the tag
//! itself, a payloaded case the call over per-part defaults — a
//! field's type spelling the annotated union answers the member's
//! construction, a union over one enum's cases member-wise; no
//! annotation keeps the ambiguity refusal, a second annotated member
//! refusing as a duplicate.

use super::{mint::*, *};

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
/// so the family reads it only inside the declaring file — a
/// foreign union answers through its own `default` member, whatever
/// derived it there.
pub(super) struct UnionDefaults {
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
    pub(super) fn collect(items: &[&Item], diags: &mut Vec<Diagnostic>) -> Self {
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
/// receiverless construction — the member lives
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
/// annotation, that member's construction, payloaded cases included —
/// and the tag body is the tag itself. A field whose type
/// spells no construction key refuses at the attribute, and so
/// does an enum whose first case carries a payload or an enum with no
/// cases; a field type the key reaches but whose construction the
/// language refuses (a bare name carrying no `default` member, a
/// one-argument head that is not the empty-construction shape) fails at
/// the synthesized call with the standard diagnostic, the family
/// precedent. Refuses otherwise exactly like `clone`: a generic
/// enum at the attribute (the body constructs a case), a non-data
/// declaration, a function-typed or `~Copyable` part.
pub(super) fn default_impl(
    item: &Item,
    attr: &Attribute,
    mint: &mut Mint,
    unions: &UnionDefaults,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Option<Item> {
    if matches!(&item.kind, ItemKind::Enum(decl) if !decl.generics.is_empty()) {
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
            let params: Vec<String> = decl.generics.iter().map(|p| p.name.name.clone()).collect();
            let body = match struct_default_body(&decl.name, &fields, mint, unions, &params) {
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
                    let params: Vec<String> =
                        decl.generics.iter().map(|p| p.name.name.clone()).collect();
                    match case_default(&case.name.name, &case.payload, unions, mint, &params) {
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
/// `none`; an application over the collection heads constructs empty
/// (`list[t]()`, `map[k, v]()`); every other application dispatches the
/// applied type's own `default` member through the instantiated
/// receiverless call; a parenthesized type keys by its inner shape; the
/// unit type answers the unit value. Every other shape refuses — a
/// union naming no annotated member, anything else the unkeyed shape.
fn default_value(
    ty: &Type,
    mint: &mut Mint,
    unions: &UnionDefaults,
    params: &[String],
) -> Result<Expr, NoDefault> {
    match ty {
        Type::Paren { inner, .. } => default_value(inner, mint, unions, params),
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return Err(NoDefault::Unkeyed);
            };
            match args.as_slice() {
                [] if params.contains(&only.name) => Err(NoDefault::Variable),
                [] => match unions.member(&only.name) {
                    // The bare name of a union this file annotates:
                    // the member's construction answers over the
                    // dispatch.
                    Err(_) => Err(NoDefault::AlreadyRefused),
                    Ok(None) => Ok(type_keyed_call(&only.name, "default", mint)),
                    Ok(Some(case)) => case_default(&case.name, &case.payload, unions, mint, params),
                },
                [arg] if only.name == "list" => {
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
                        selection: None,
                    })
                },
                [key, value] if only.name == "map" => {
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
                        selection: None,
                    })
                },
                // Every other application — a head the constructor
                // brackets do not serve, whatever its arity — defaults
                // through the applied type's own `default` member: the
                // instantiated receiverless call, whose substitution
                // and evidence the checker stages at the site. A head
                // carrying no member refuses at the synthesized call
                // with the standard missing-member diagnostic, the
                // bare-name precedent.
                _ => Ok(applied_default(ty, mint)),
            }
        },
        Type::Tuple { items, .. } => {
            let span = mint.span();
            let elements = items
                .iter()
                .map(|item| default_value(item, mint, unions, params))
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
            // member names the union's default; every other spelling keeps
            // the ambiguity refusal.
            let members = union_members(ty).ok_or(NoDefault::Union)?;
            let owner = unions.union_owner(&members).ok_or(NoDefault::Union)?;
            match unions.member(&unions.enums[owner].name) {
                Err(_) => Err(NoDefault::AlreadyRefused),
                // The annotated member must sit among the field's own
                // members: a choice outside the spelled union names no
                // canonical member of it.
                Ok(Some(case)) if members.contains(&case.name) => {
                    case_default(&case.name, &case.payload, unions, mint, params)
                },
                Ok(None | Some(_)) => Err(NoDefault::Union),
            }
        },
        _ => Err(NoDefault::Unkeyed),
    }
}

/// The annotated member's default construction: a bare case the case name
/// itself, a tuple payload the call
/// over per-part defaults, a struct payload the named-argument call —
/// every part keyed by the family's ordinary per-part rules, a part
/// spelling no construction refusing through them.
fn case_default(
    name: &str,
    payload: &EnumPayload,
    unions: &UnionDefaults,
    mint: &mut Mint,
    params: &[String],
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
                    let value = default_value(ty, mint, unions, params)?;
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
                selection: None,
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
                    let value = default_value(&field.ty, mint, unions, params)?;
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
                selection: None,
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
    params: &[String],
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
            let value =
                default_value(&field.ty, mint, unions, params).map_err(|why| RefusedField {
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

/// The instantiated type-keyed `default` call: the field's written
/// application type rides the fn-type node as the receiver — the one
/// expression form that holds a written type of any arity, including
/// the shapes the constructor brackets cannot spell — dispatching the
/// applied type's own `default` member. The carried type is the
/// structure-only minted copy (resolution records name uses by span,
/// and the written declaration already used the original's spans); the
/// member name and call mint their own.
fn applied_default(ty: &Type, mint: &mut Mint) -> Expr {
    let recv = Expr::FnType {
        span: mint.span(),
        ty: Box::new(minted_type(ty, mint)),
    };
    let name = Ident {
        span: mint.span(),
        name: "default".to_owned(),
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
        selection: None,
    }
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
        selection: None,
    }
}
