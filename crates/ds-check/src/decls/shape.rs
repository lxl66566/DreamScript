//! The shape pass: the reserved atoms take their solved shapes — a struct's
//! field signature, an enum's case rows, a trait's supers and bounds — and the
//! universe's field/case tables fill.

use super::*;

pub(super) fn struct_fields_ast(members: &[StructMember]) -> Vec<FieldDecl> {
    members
        .iter()
        .filter_map(|m| match m {
            StructMember::Field(f) => Some(f.clone()),
            StructMember::Deinit { .. } => None,
        })
        .collect()
}

/// `served_item` marks an item of the checked-ambient member itself: its
/// impl items' instance declarations arrived installed with the artifact
/// (rows, spans, memberships, mints), so the impl arm skips.
pub(super) fn shape_item(
    package: &Package,
    item: &Item,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
    served_item: bool,
) {
    match &item.kind {
        ItemKind::Struct(s) => {
            let Some(def) = package.lookup_type(&s.name.name) else {
                return
            };
            if is_served(decls, def) {
                return;
            }
            let Some(&atom) = decls.atoms.get(&def) else {
                return
            };
            register_supers(package, &s.supers, atom, decls);
            if s.generics.is_empty() && s.has_body {
                let fields = universe_fields(package, &struct_fields_ast(&s.members), decls, diags);
                let _ = decls.universe.set_struct_fields(atom, fields);
            }
        },
        ItemKind::Enum(e) => {
            let Some(def) = type_def_for_use(package, decls, &e.name.name)
                .filter(|&d| package.def(d).span == e.name.span)
            else {
                return
            };
            if is_served(decls, def) {
                return;
            }
            if e.generics.is_empty() {
                // Minted cases: each case's atom takes its payload as the
                // struct field signature (positional slots for tuple
                // payloads), so a minted payload case IS a struct atom for
                // the set algebra and the lowerer.
                for case in &e.cases {
                    let Some(case_def) = type_def_for_use(package, decls, &case.name.name)
                        .filter(|&d| package.def(d).span == case.name.span)
                    else {
                        continue
                    };
                    let Some(&atom) = decls.atoms.get(&case_def) else {
                        continue
                    };
                    register_supers(package, &e.supers, atom, decls);
                    let fields = match &case.payload {
                        EnumPayload::None => Vec::new(),
                        EnumPayload::Tuple(types) => types
                            .iter()
                            .filter_map(|t| {
                                let subject = format!("case `{}`'s payload slot", case.name.name);
                                field_expr(package, t, t.span(), &subject, decls, diags)
                            })
                            .map(Field::positional)
                            .collect(),
                        EnumPayload::Struct(fields) => {
                            universe_fields(package, fields, decls, diags)
                        },
                    };
                    if !fields.is_empty() {
                        let _ = decls.universe.set_struct_fields(atom, fields);
                    }
                }
            } else {
                // A generic enum keeps the opaque atom with its case
                // signatures until case minting grows substitution.
                let Some(&atom) = decls.atoms.get(&def) else {
                    return
                };
                register_supers(package, &e.supers, atom, decls);
                let mut cases = Vec::new();
                for case in &e.cases {
                    let fields = match &case.payload {
                        EnumPayload::None => Vec::new(),
                        EnumPayload::Tuple(types) => types
                            .iter()
                            .filter_map(|t| {
                                let subject = format!("case `{}`'s payload slot", case.name.name);
                                field_expr(package, t, t.span(), &subject, decls, diags)
                            })
                            .map(Field::positional)
                            .collect(),
                        EnumPayload::Struct(fields) => {
                            universe_fields(package, fields, decls, diags)
                        },
                    };
                    cases.push(Case::new(&case.name.name, fields));
                }
                let _ = decls.universe.set_enum_cases(atom, cases);
            }
        },
        ItemKind::Impl(imp) => {
            if !served_item {
                declare_instance(package, item, imp, decls, diags);
            }
        },
        ItemKind::Tag(t) => {
            let Some(def) = type_def_for_use(package, decls, &t.name.name)
                .filter(|&d| package.def(d).span == t.name.span)
            else {
                return
            };
            if is_served(decls, def) {
                return;
            }
            match &t.payload {
                // the tuple form refused before any mint: a payloaded
                // tag's atom is the struct shape alone
                EnumPayload::Tuple(_) => {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::TagPayloadTuple,
                            "a tag payload spells named fields".to_owned(),
                        )
                        .with_primary(
                            t.span,
                            "a tuple payload is not a tag form; write `tag name { field: type }`",
                        ),
                    );
                },
                EnumPayload::None => {},
                EnumPayload::Struct(fields) => {
                    let Some(&atom) = decls.atoms.get(&def) else {
                        return
                    };
                    let solved = universe_fields(package, fields, decls, diags);
                    let _ = decls.universe.set_struct_fields(atom, solved);
                },
            }
        },
        ItemKind::TypeAlias(_)
        | ItemKind::Trait(_)
        | ItemKind::Fn(_)
        | ItemKind::ExternFn(_)
        | ItemKind::Local(_)
        | ItemKind::Import(_)
        | ItemKind::Platform(_) => {},
    }
}

/// Whether a written type is the `~Copyable` marker: the negation of
/// the fixed `Copyable` trait name. The marker subtracts the default
/// copyability constraint, so a declaration carrying it is linear.
pub(crate) fn linear_marker(ty: &Type) -> bool {
    let Type::Negation { inner, .. } = ty else {
        return false;
    };
    let Type::Path { path, .. } = inner.as_ref() else {
        return false;
    };
    path.segments.len() == 1 && path.segments[0].name == "Copyable"
}

/// `struct dog: animal` nominal subtyping: every non-negated supertrait
/// path registers the atom as a trait member.
pub(super) fn register_supers(package: &Package, supers: &[Type], atom: TypeId, decls: &mut Decls) {
    for super_ty in supers {
        if let Some(trait_id) = trait_of_type(package, super_ty, decls) {
            let _ = decls.universe.impl_trait(trait_id, atom);
        }
    }
}

pub(super) fn trait_of_type(package: &Package, ty: &Type, decls: &Decls) -> Option<TraitId> {
    let Type::Path { path, .. } = ty else {
        return None;
    };
    let [only] = path.segments.as_slice() else {
        return None;
    };
    let def = package.lookup_type(&only.name)?;
    decls.traits.get(&def).copied()
}

/// The traits one written bound names: a path its trait, an
/// intersection every arm's traits — an intersect bound's every arm
/// reads as trait evidence, the same rule the admission's check runs,
/// so a synthesized impl's bound keeps its condition and the member's
/// dispatch stages its evidence word.
pub(super) fn bound_traits(package: &Package, ty: &Type, decls: &Decls) -> Vec<TraitId> {
    match ty {
        Type::Intersect { left, right, .. } => {
            let mut out = bound_traits(package, left, decls);
            out.extend(bound_traits(package, right, decls));
            out
        },
        _ => trait_of_type(package, ty, decls).into_iter().collect(),
    }
}

/// Converts annotated struct/enum fields into solved universe fields; a
/// field whose type does not live in the [`TypeExpr`] world (function
/// types, foreign types) contributes nothing — the checker works from the
/// AST shape regardless, only witnesses degrade.
pub(super) fn universe_fields(
    package: &Package,
    fields: &[FieldDecl],
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Field> {
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let subject = format!("field `{}`", field.name.name);
        if let Some(expr) = field_expr(package, &field.ty, field.span, &subject, decls, diags) {
            out.push(Field::named(&field.name.name, expr));
        }
    }
    out
}

/// One field type's conversion behind the none door: a written type
/// whose solution is the `none` atom alone refuses at its own span —
/// `none` is the optional's absent member, never a field's whole
/// type, and a field that enters no atom would surface later as a
/// lowering refusal over the wrong vocabulary.
pub(super) fn field_expr(
    package: &Package,
    ty: &Type,
    span: Span,
    subject: &str,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    let expr = conv_expr(package, ty, decls, diags)?;
    let none_alone = decls
        .universe
        .solve(&expr)
        .is_ok_and(|solved| decls.universe.members(solved) == [TypeId::NONE]);
    if none_alone {
        diags.push(
            Diagnostic::error(
                ErrorCode::NoneTypedField,
                format!(
                    "{subject} cannot be typed `none`: `none` is the optional's absent member, a \
                     member of `t?` and `t | none` spellings, never a standalone field type"
                ),
            )
            .with_primary(span, "none-typed field"),
        );
        return None;
    }
    let uninhabited = decls
        .universe
        .solve(&expr)
        .is_ok_and(|solved| decls.universe.is_empty(solved));
    if uninhabited {
        diags.push(
            Diagnostic::error(
                ErrorCode::NeverTypedPosition,
                format!(
                    "{subject} is typed `never`: no value inhabits the type, so the field holds \
                     nothing"
                ),
            )
            .with_primary(span, "field typed never"),
        );
        return None;
    }
    Some(expr)
}
