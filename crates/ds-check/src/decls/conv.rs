//! Written-type conversion: a declaration's written type solves into the
//! universe's expression while the written form stays beside it for diagnostics
//! and call-site substitution, and a written application is checked against its
//! head's declared parameters.

use super::*;

/// A written type application's shape refusal against its head
/// declaration: the applied argument count against the head's declared
/// parameter count, or a head whose kind never takes type arguments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ApplicationMismatch {
    /// A head applied with the wrong argument count.
    Arity {
        /// The head's declared type-parameter count.
        declared: usize,
        /// The argument count the application spells.
        given: usize,
    },
    /// A head that takes no type arguments at all.
    Kind { word: &'static str },
}

impl ApplicationMismatch {
    /// The refusal sentence, naming the declaration and both counts.
    pub(crate) fn message(&self, name: &str) -> String {
        match self {
            Self::Arity { declared, given } => format!(
                "`{name}` declares {declared} type parameter(s); the application spells {given}"
            ),
            Self::Kind { word } => format!("`{name}` is a {word} and takes no type arguments"),
        }
    }
}

/// Validates one written type application against its head declaration
/// by `DefKind`: struct, enum, alias, and trait heads carry their declared
/// parameter count, while tags, cases, value-namespace names, and every
/// non-generic head take zero arguments. The conversion seams call this
/// ahead of `apply_atom`, which internal structural application also
/// reuses.
pub(crate) fn check_application(
    package: &Package,
    def: DefId,
    given: usize,
    decls: &Decls,
) -> Result<(), ApplicationMismatch> {
    let declared = match package.def(def).kind {
        DefKind::Struct => decls
            .structs
            .get(&def)
            .map_or(0, |shape| shape.params.len()),
        DefKind::Enum => decls.enums.get(&def).map_or(0, |shape| shape.params.len()),
        DefKind::TypeAlias => decls
            .aliases
            .get(&def)
            .map_or(0, |(params, _)| params.len()),
        DefKind::Trait => decls.trait_generics.get(&def).map_or(0, Vec::len),
        kind => return Err(ApplicationMismatch::Kind { word: kind.word() }),
    };
    if declared == given { Ok(()) } else { Err(ApplicationMismatch::Arity { declared, given }) }
}

/// Converts a written type into the [`TypeExpr`] world for non-generic
/// universe shapes; `None` marks a type the core cannot express.
pub(super) fn conv_expr(
    package: &Package,
    ty: &Type,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    Some(match ty {
        Type::Path {
            span, path, args, ..
        } => {
            let [only] = path.segments.as_slice() else {
                return None
            };
            if args.is_empty() {
                path_expr(package, only, decls, diags)?
            } else {
                let Some(def) = package.lookup_type(&only.name) else {
                    // `none` applied with type arguments: the predeclared
                    // unit atom takes none, the same refusal a local
                    // tag's application hears, instead of the field
                    // silently missing its atom.
                    if only.name == "none" && !package.declares("none") {
                        diags.push(
                            Diagnostic::error(
                                ErrorCode::TypeApplicationMismatch,
                                ApplicationMismatch::Kind { word: "tag" }.message(&only.name),
                            )
                            .with_primary(*span, "type application"),
                        );
                    }
                    return None;
                };
                if let Err(mismatch) = check_application(package, def, args.len(), decls) {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::TypeApplicationMismatch,
                            mismatch.message(&only.name),
                        )
                        .with_primary(*span, "type application"),
                    );
                    return None;
                }
                let ctor = decls.atoms.get(&def).copied()?;
                let args = conv_args(package, args, &FxHashMap::default(), decls, diags)?;
                TypeExpr::Atom(decls.universe.apply_atom(ctor, &args).ok()?)
            }
        },
        Type::Optional { inner, .. } => {
            conv_expr(package, inner, decls, diags)?.union(TypeId::NONE.into())
        },
        Type::Negation { inner, .. } => TypeExpr::Not(trait_of_type(package, inner, decls)?),
        Type::Paren { inner, .. } => conv_expr(package, inner, decls, diags)?,
        Type::Union { left, right, .. } => {
            conv_expr(package, left, decls, diags)?.union(conv_expr(package, right, decls, diags)?)
        },
        Type::Intersect { left, right, .. } => {
            conv_expr(package, left, decls, diags)?.inter(conv_expr(package, right, decls, diags)?)
        },
        Type::Difference { left, right, .. } => {
            conv_expr(package, left, decls, diags)?.diff(conv_expr(package, right, decls, diags)?)
        },
        Type::Tuple { items, .. } => {
            let mut elements = Vec::new();
            for item in items {
                elements.push(conv_expr(package, item, decls, diags)?);
            }
            TypeExpr::Tuple(elements)
        },
        Type::Unit(_) => TypeExpr::Tuple(Vec::new()),
        // the comptime type-value atom: the `type` keyword's
        // conversion form, so a `comptime t: type` parameter's solved
        // binder type is the builtin atom
        Type::TypeKw(_) => TypeExpr::Atom(TypeId::TYPE),
        // a fn type converts to its hash-consed signature atom: a
        // fn-typed field's atom enters the declaration the shape walk
        // reads, the field's word the closure Ref the count traffic
        // covers. A bare fn-type parameter names a variable the
        // declaration-side conversion cannot resolve, so it drops.
        Type::Fn { params, ret, .. } => {
            let mut param_tys = Vec::with_capacity(params.len());
            for param in params {
                // a bare fn-type parameter (`fn(t) -> r`) names the type
                // the surface spelling carries, the same reading the
                // checker's own fn-type solve takes
                let bare;
                let written = match param.ty.as_ref() {
                    Some(ty) => ty,
                    None => {
                        bare = Type::Path {
                            span: param.span,
                            path: ds_parse::Path {
                                span: param.span,
                                segments: vec![Ident {
                                    span: param.span,
                                    name: param.name.name.clone(),
                                }],
                            },
                            args: Vec::new(),
                            delim: Delim::Bracket,
                        };
                        &bare
                    },
                };
                let expr = conv_expr(package, written, decls, diags)?;
                param_tys.push(decls.universe.solve(&expr).ok()?);
            }
            let ret_ty = match ret {
                Some(ret) => {
                    let expr = conv_expr(package, ret, decls, diags)?;
                    decls.universe.solve(&expr).ok()?
                },
                None => decls.universe.solve(&TypeExpr::Tuple(Vec::new())).ok()?,
            };
            let atom = decls.universe.fn_atom(&param_tys, ret_ty).ok()?;
            TypeExpr::Atom(atom)
        },
        Type::Ptr { .. } | Type::Foreign { .. } => return None,
    })
}

/// Solves written application arguments in conversion contexts.
pub(super) fn conv_args(
    package: &Package,
    args: &[Type],
    vars: &FxHashMap<String, TypeId>,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<Ty>> {
    let _ = vars;
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let expr = conv_expr(package, arg, decls, diags)?;
        out.push(decls.universe.solve(&expr).ok()?);
    }
    Some(out)
}

/// A single-segment type path in expression position: a declared atom, a
/// trait as its member set, or a non-generic alias expanded to its full
/// written union — never a set's single member.
pub(super) fn path_expr(
    package: &Package,
    name: &Ident,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    // `none` is the universe's predeclared unit atom with no def,
    // so both namespace lookups miss the name; the optional arm unions
    // this same atom for `t?`, and a package declaration claiming the
    // name keeps its own resolution below.
    if name.name == "none" && !package.declares("none") {
        return Some(TypeExpr::Atom(TypeId::NONE));
    }
    // An import-bound type the shape pass names ahead of the linked
    // mint loop: its def sits outside the local type namespace (the
    // link pass's registration), so the foreign table answers by
    // name, and the install forces forward — or the referencing
    // field would silently drop out of its declaration's universe
    // atom, a shape both this module's own lowering and the export
    // walk's link fact read.
    let Some(def) = package.lookup_type(&name.name) else {
        let def = package
            .foreign()
            .iter()
            .find(|(_, entry)| entry.name == name.name && entry.ty.is_some())
            .map(|(def, _)| *def)?;
        force_linked(package, decls, def)?;
        return path_def_expr(package, def, decls, diags);
    };
    path_def_expr(package, def, decls, diags)
}

/// One resolved def's universe form behind a written path segment: an
/// atom (a struct, a case, a tag), a trait reference, a non-generic
/// enum's case-atom union, or a non-generic alias's expanded
/// body. `None` marks a def the universe cannot express here.
pub(super) fn path_def_expr(
    package: &Package,
    def: DefId,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    if let Some(&atom) = decls.atoms.get(&def) {
        return Some(TypeExpr::Atom(atom));
    }
    if let Some(&trait_id) = decls.traits.get(&def) {
        return Some(TypeExpr::Trait(trait_id));
    }
    // A non-generic enum names the union of its minted case atoms.
    // The union's labels and an alias's body are the declaration's own
    // text: they resolve under the declaring file's shadow map, never
    // the use site's.
    if let Some(shape) = decls.enums.get(&def) {
        if shape.params.is_empty() {
            let saved = decls.current_file;
            decls.current_file = Some(package.def(def).span.file);
            let union = enum_case_union(package, &shape.cases, decls);
            decls.current_file = saved;
            return union;
        }
        return None;
    }
    if let Some((params, body)) = decls.aliases.get(&def).cloned()
        && params.is_empty()
    {
        let saved = decls.current_file;
        decls.current_file = Some(package.def(def).span.file);
        let expr = conv_expr(package, &body, decls, diags);
        decls.current_file = saved;
        return expr;
    }
    None
}

/// The union of one enum's minted case atoms; `None` when a case did
/// not mint. A zero-case enum is the canonical empty spelling — the
/// uninhabited union of zero atoms, so heads and fields over it convert.
/// A linked enum's case defs sit outside the package's own namespace, so
/// the name index the linked install filled answers them.
pub(super) fn enum_case_union(
    package: &Package,
    cases: &[(String, Span, EnumPayload)],
    decls: &Decls,
) -> Option<TypeExpr> {
    if cases.is_empty() {
        return Some(TypeExpr::Empty);
    }
    let mut union: Option<TypeExpr> = None;
    for (name, ..) in cases {
        let atom = type_def_for_use(package, decls, name)
            .or_else(|| decls.foreign_type(name))
            .and_then(|def| decls.atoms.get(&def).copied())?;
        let expr = TypeExpr::Atom(atom);
        union = Some(match union {
            Some(acc) => acc.union(expr),
            None => expr,
        });
    }
    union
}

/// The atom a single-segment type path names as a CONSTRUCTOR: declared
/// atoms only (aliases and traits are sets, not constructors).
pub(super) fn atom_of_path(package: &Package, name: &Ident, decls: &mut Decls) -> Option<TypeId> {
    let def = type_def_for_use(package, decls, &name.name)?;
    if decls.atoms.get(&def).is_none() && package.foreign().contains_key(&def) {
        // A written type naming a linked declaration shapes ahead of the
        // link install (an impl head, an annotation the local namespace
        // cannot answer): the same pull a written field and a trait
        // reference take, and the install replays idempotently.
        force_linked(package, decls, def)?;
    }
    decls.atoms.get(&def).copied()
}

/// Mints the unit atom for one local tag (or unit enum case): the
/// atom is the declaring declaration's own, so the mint never refuses and
/// two declarations may share a label as distinct atoms. The universe
/// predeclares `none` as its distinguished unit atom with no def, so
/// a `tag none` (or `enum e { none }`) restating the name maps onto the
/// predeclared atom instead of minting a second — a skipped mint would
/// silently orphan the def and every written `none` type or value would
/// stop solving.
pub(super) fn declare_unit_name(decls: &mut Decls, name: &str) -> TypeId {
    if name == "none" {
        return TypeId::NONE;
    }
    decls.universe.mint_tag(name)
}

/// The linked case's unit mint keeps the name-keyed contract: a label already declared in this
/// compile reuses that atom, `none` maps onto the predeclared atom, and
/// any other refused name leaves the def for the replayed atom.
pub(super) fn declare_foreign_unit_name(decls: &mut Decls, name: &str) -> Option<TypeId> {
    match decls.universe.declare_tag(name) {
        Ok(id) => Some(id),
        Err(_) if name == "none" => Some(TypeId::NONE),
        Err(_) => None,
    }
}

/// Records the solved signature of every application of a user-declared
/// generic struct or enum, after the whole package resolves: the walk
/// enumerates the universe's atoms in id order (growing as field and
/// slot solving mint nested applications and the enum applications'
/// own case atoms), reads the constructor's declared shape, and solves
/// each declared field or case slot under the parameter-to-argument
/// substitution — the same solving field access, construction, and
/// matching run at their sites, the types resolving under the
/// declaring file's shadow map. An application whose fields or slots
/// do not all solve records nothing, and its values keep the lowering
/// refusal they always carried; a self-containing application records
/// the entry its fields name, and the shape layer's cycle budget
/// refuses the value shape the entry describes.
pub(crate) fn record_applications(package: &Package, decls: &mut Decls) {
    let mut at = 0u32;
    while at < decls.universe.atom_count() {
        let id = TypeId::from_raw(at);
        at += 1;
        let kind = match decls.universe.atom_decl(id) {
            Ok(decl) => decl.kind.clone(),
            Err(_) => continue,
        };
        let crate::AtomKind::Apply(ctor, args) = kind else {
            continue;
        };
        let Some(&def) = decls.defs_of_atoms.get(&ctor) else {
            continue;
        };
        // An enum application's own walk derives the case atoms under
        // the application's arguments, growing the universe it walks.
        if decls.enums.contains_key(&def) {
            let _ = applied_enum_cases(package, decls, id);
            continue;
        }
        let Some(shape) = decls.structs.get(&def) else {
            continue;
        };
        if shape.params.is_empty() || shape.params.len() != args.len() {
            continue;
        }
        let mut vars: FxHashMap<String, Ty> = FxHashMap::default();
        for (param, arg) in shape.params.iter().zip(args.iter()) {
            vars.insert(param.name.clone(), *arg);
        }
        // the shape's own rows copy ahead of the solving, whose minting
        // borrows the tables mutably
        let written: Vec<Type> = shape.fields.iter().map(|field| field.ty.clone()).collect();
        let names: Vec<String> = shape
            .fields
            .iter()
            .map(|field| field.name.name.clone())
            .collect();
        let saved = decls.current_file;
        decls.current_file = Some(package.def(def).span.file);
        let mut fields = Vec::with_capacity(written.len());
        let mut solved_all = true;
        for (name, ty_written) in names.into_iter().zip(written.iter()) {
            match solve_written(package, ty_written, &vars, decls) {
                Some(ty) => fields.push(SolvedField { name, ty }),
                None => solved_all = false,
            }
        }
        decls.current_file = saved;
        if solved_all {
            decls.apply_structs.insert(id, fields.into());
        }
    }
}

/// The per-case facts of one application of a user-declared generic
/// enum, recorded once per application atom and minted on demand: each
/// case's def, the atom its values carry under the application's
/// arguments, and the case's solved payload slots. A bare case keeps
/// the shared unit atom it minted at reservation — a unit word carries
/// no substitution, so it tests over every application of the head. A
/// payloaded case mints its own atom per application (a struct-shaped
/// atom over the solved slots, the shape a nominal payload case
/// carries) and records the slots the construction and the arm binders
/// check against, the slot types solving under the parameter-to-argument
/// substitution exactly a struct application's fields do. A case whose
/// slots do not all solve, a payloaded case named `none` (the
/// predeclared unit atom takes no payload), and a case whose def never
/// reserved all refuse the whole application: the doors fall back to
/// the refusals they always carried. An empty record marks such a
/// refusal, so a repeated read never mints twice.
pub(crate) fn applied_enum_cases(
    package: &Package,
    decls: &mut Decls,
    app: TypeId,
) -> Option<std::rc::Rc<[AppliedCase]>> {
    if let Some(recorded) = decls.apply_enums.get(&app) {
        return Some(recorded.clone());
    }
    let derived = derive_apply_enum_cases(package, decls, app);
    let record = derived.unwrap_or_default();
    decls.apply_enums.insert(app, record.clone());
    if record.is_empty() { None } else { Some(record) }
}

/// One application's case derivation: `None` when any case refuses,
/// the empty list never answers (the caller records the refusal).
fn derive_apply_enum_cases(
    package: &Package,
    decls: &mut Decls,
    app: TypeId,
) -> Option<std::rc::Rc<[AppliedCase]>> {
    let kind = decls.universe.atom_decl(app).ok()?.kind.clone();
    let crate::AtomKind::Apply(ctor, args) = kind else {
        return None;
    };
    let def = decls.defs_of_atoms.get(&ctor).copied()?;
    let shape = decls.enums.get(&def)?.clone();
    if shape.params.is_empty() || shape.params.len() != args.len() {
        return None;
    }
    let mut vars: FxHashMap<String, Ty> = FxHashMap::default();
    for (param, arg) in shape.params.iter().zip(args.iter()) {
        vars.insert(param.name.clone(), *arg);
    }
    let saved = decls.current_file;
    decls.current_file = Some(package.def(def).span.file);
    let mut cases = Vec::with_capacity(shape.cases.len());
    let mut derived_all = true;
    for (name, span, payload) in &shape.cases {
        let Some(case_def) =
            type_def_for_use(package, decls, name).filter(|&d| package.def(d).span == *span)
        else {
            derived_all = false;
            continue;
        };
        match payload {
            EnumPayload::None => match decls.atoms.get(&case_def).copied() {
                Some(atom) => cases.push(AppliedCase {
                    def: case_def,
                    atom,
                    slots: None,
                }),
                None => derived_all = false,
            },
            payload @ (EnumPayload::Tuple(_) | EnumPayload::Struct(_)) => {
                // `none` is the predeclared unit atom with no payload of
                // its own: a payloaded case claiming the name keeps the
                // construction refusal it always carried.
                if name == "none" {
                    derived_all = false;
                    continue;
                }
                let written: Vec<(Option<String>, Type)> = match payload {
                    EnumPayload::Tuple(types) => types.iter().map(|t| (None, t.clone())).collect(),
                    EnumPayload::Struct(fields) => fields
                        .iter()
                        .map(|f| (Some(f.name.name.clone()), f.ty.clone()))
                        .collect(),
                    EnumPayload::None => unreachable!("matched above"),
                };
                let mut slots = Vec::with_capacity(written.len());
                let mut fields = Vec::with_capacity(written.len());
                for (slot_name, ty_written) in &written {
                    match solve_written(package, ty_written, &vars, decls) {
                        Some(ty) => {
                            slots.push(AppliedSlot {
                                name: slot_name.clone(),
                                ty,
                            });
                            fields.push(Field {
                                name: slot_name.clone(),
                                ty: expr_of_ty(decls, ty),
                            });
                        },
                        None => derived_all = false,
                    }
                }
                if !derived_all {
                    continue;
                }
                let atom = decls.universe.mint_payload_tag(name);
                let _ = decls.universe.set_struct_fields(atom, fields);
                cases.push(AppliedCase {
                    def: case_def,
                    atom,
                    slots: Some(slots.into()),
                });
            },
        }
    }
    decls.current_file = saved;
    (derived_all && cases.len() == shape.cases.len()).then(|| cases.into())
}

/// The solved type re-expressed as the union-of-atoms [`TypeExpr`] the
/// universe shapes carry: the minted case atom's field signature reads
/// it exactly like a declared field's written form.
fn expr_of_ty(decls: &Decls, ty: Ty) -> TypeExpr {
    let mut expr: Option<TypeExpr> = None;
    for atom in decls.universe.members(ty) {
        expr = Some(match expr {
            None => TypeExpr::atom(*atom),
            Some(e) => e.union((*atom).into()),
        });
    }
    expr.unwrap_or(TypeExpr::Empty)
}

/// The applications of one generic enum head inside a scrutinee's
/// members: the ctor-head route the constructions and the member arms
/// resolve through, applications staying opaque to the set algebra. A
/// caller over several applications refuses ambiguous; one over none
/// keeps its own refusal.
pub(crate) fn applications_of_head(decls: &Decls, scrut: Ty, head: TypeId) -> Vec<TypeId> {
    decls
        .universe
        .members(scrut)
        .iter()
        .copied()
        .filter(|&member| {
            matches!(
                decls.universe.atom_decl(member).ok().map(|d| &d.kind),
                Some(crate::AtomKind::Apply(ctor, _)) if *ctor == head
            )
        })
        .collect()
}

/// Converts one written type into a solved singleton [`TypeExpr`] for
/// instance heads and conditions: unions stay unions in [`conv_head`],
/// everything else becomes its solved atom set.
pub(super) fn conv_node(
    package: &Package,
    ty: &Type,
    vars: &FxHashMap<String, Ty>,
    decls: &mut Decls,
) -> Option<TypeExpr> {
    Some(match ty {
        Type::Union { left, right, .. } => {
            conv_node(package, left, vars, decls)?.union(conv_node(package, right, vars, decls)?)
        },
        Type::Paren { inner, .. } => conv_node(package, inner, vars, decls)?,
        other => {
            // Everything else: solve to a Ty, then re-express as a union of
            // atoms the TypeExpr world can carry — the empty set through
            // its canonical spelling, so a head naming a zero-case enum
            // (or any solved-empty shape) converts.
            let solved = solve_written(package, other, vars, decls)?;
            let mut expr = None::<TypeExpr>;
            for atom in decls.universe.members(solved) {
                expr = Some(match expr {
                    None => TypeExpr::atom(*atom),
                    Some(e) => e.union((*atom).into()),
                });
            }
            expr.unwrap_or(TypeExpr::Empty)
        },
    })
}

/// Solves one written type (path, optional, tuple, unit) to a [`Ty`] in a
/// conversion context; type variables resolve to their minted atoms.
pub(super) fn solve_written(
    package: &Package,
    ty: &Type,
    vars: &FxHashMap<String, Ty>,
    decls: &mut Decls,
) -> Option<Ty> {
    Some(match ty {
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return None
            };
            if let Some(&ty) = vars.get(&only.name) {
                ty
            } else if args.is_empty() {
                if let Some(atom) = atom_of_path(package, only, decls) {
                    decls.universe.atom_ty(atom).ok()?
                } else {
                    // A trait as a set, or a non-generic alias: solve its
                    // written expression in full. An alias body expanded
                    // here reports no application mismatch: an impl-head
                    // conversion degrades silently like its other
                    // unsolvable shapes, and the impl's own body checking
                    // reports the unsolved target.
                    path_expr(package, only, decls, &mut Vec::new())
                        .and_then(|expr| decls.universe.solve(&expr).ok())?
                }
            } else {
                let ctor = atom_of_path(package, only, decls)?;
                let mut arg_tys = Vec::new();
                for arg in args {
                    arg_tys.push(solve_written(package, arg, vars, decls)?);
                }
                let apply = decls.universe.apply_atom(ctor, &arg_tys).ok()?;
                decls.universe.atom_ty(apply).ok()?
            }
        },
        Type::Optional { inner, .. } => {
            let inner = solve_written(package, inner, vars, decls)?;
            decls.universe.optional(inner)
        },
        Type::Paren { inner, .. } => solve_written(package, inner, vars, decls)?,
        Type::Tuple { items, .. } => {
            let mut elements = Vec::new();
            for item in items {
                elements.push(solve_written(package, item, vars, decls)?);
            }
            decls.universe.product(&elements).ok()?
        },
        Type::Unit(_) => decls.universe.unit_ty(),
        Type::Union { left, right, .. } => {
            let (a, b) = (
                solve_written(package, left, vars, decls)?,
                solve_written(package, right, vars, decls)?,
            );
            decls.universe.union(a, b)
        },
        Type::Intersect { .. }
        | Type::Difference { .. }
        | Type::Negation { .. }
        | Type::Fn { .. }
        | Type::Ptr { .. }
        | Type::Foreign { .. } => return None,
        // the comptime type-value atom, in the written-variable
        // conversion context
        Type::TypeKw(_) => decls.universe.atom_ty(TypeId::TYPE).ok()?,
    })
}
