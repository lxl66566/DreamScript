//! The serve half: the shared bases installed per serve and the name-keyed rows resolved against
//! the merged compile's own universe.

use std::sync::Arc;

use ds_check::{
    Case, Condition, Def, DefId, Field, MethodDecl, MethodId, MethodOwner, Package, PackageBase,
    Res, Resolutions, ServedDecls, ServedInstance, TraitId, TypeExpr, TypeId, Universe,
};
use ds_diag::{FileId, Span};

use super::*;

/// One decoded artifact's native serve: the decoded tables must address
/// exactly the file ids the caller registered (the registration the
/// spans carry), and every table rebuilds owned per call — the tables
/// move into the caller's bundle, so a shared decode serves a private
/// conversion each time. A caller serving the same artifact repeatedly
/// converts once through [`convert_tables`] and serves through
/// [`serve_prefix_tables`] instead, installing the shared bases per
/// serve. Any miss is an error the caller answers with the fresh
/// compute.
///
/// # Errors
///
/// [`PrefixDsiError`]: a foreign registration, or tables the conversion
/// refuses — both cache misses, never source problems.
pub fn serve_prefix(
    artifact: &PrefixDsi,
    files: &[FileId],
) -> Result<ServedPrefix, PrefixDsiError> {
    let tables = convert_tables(artifact)?;
    serve_prefix_tables(artifact, files, &tables)
}

/// [`serve_prefix`] over a caller-parked conversion: a process that
/// serves the same artifact repeatedly (the ambient's member compiles,
/// a query session) converts once and hands every serve the same shared
/// table bases, so each serve's install is a refcount and its teardown
/// never drops the rows.
///
/// # Errors
///
/// [`PrefixDsiError`]: a foreign registration, or tables the conversion
/// refuses — both cache misses, never source problems.
pub fn serve_prefix_tables(
    artifact: &PrefixDsi,
    files: &[FileId],
    tables: &ServedTables,
) -> Result<ServedPrefix, PrefixDsiError> {
    if artifact.package.files.as_slice() != files {
        return Err(PrefixDsiError::FileMismatch);
    }
    Ok(serve(artifact, tables))
}

/// Converts the decoded artifact into the native serve over the shared
/// table bases: the def-keyed tables install as the pre-built
/// [`Decls`]'s shared bases (the merged compile's reserve pass reads
/// them and skips their rows' inserts), the universe starts empty, and
/// the name-keyed rows — the def-named identity checks, the shapes, the
/// memberships, the mints, the interned sets, the instance rows, and
/// the solved span-keyed records — move into one resolve step the
/// declaration build runs once its own reserve pass has minted the
/// prefix's atoms. A drift refusal there is a cache miss the build
/// falls back from to the fresh path, never a served wrong fact.
pub(super) fn serve(artifact: &PrefixDsi, tables: &ServedTables) -> ServedPrefix {
    let package = Package::with_base(Arc::clone(&tables.package));
    let resolutions = tables.resolutions.clone();

    let mut decls = ds_check::Decls::default();
    decls.structs.install_base(Arc::clone(&tables.structs));
    decls.enums.install_base(Arc::clone(&tables.enums));
    decls.cases.install_base(Arc::clone(&tables.cases));
    decls.fns.install_base(Arc::clone(&tables.fns));
    decls.members.install_base(Arc::clone(&tables.members));
    decls.inherent.install_base(Arc::clone(&tables.inherent));
    decls
        .trait_members
        .install_base(Arc::clone(&tables.trait_members));
    decls
        .trait_member_defaults
        .install_base(Arc::clone(&tables.trait_member_defaults));
    decls
        .trait_generics
        .install_base(Arc::clone(&tables.trait_generics));
    decls
        .instance_impls
        .install_base(Arc::clone(&tables.instance_impls));
    decls
        .inherent_impls
        .install_base(Arc::clone(&tables.inherent_impls));
    // `sealed_traits` installs no base row, deliberately: the reserve
    // pass re-derives it from the declaring text of every tree, served
    // or fresh, so a served row would be dead weight. Every serve
    // consumer reaches the tables through that reserve walk; a reader
    // of the seal outside it answers an empty table.
    decls.served_defs = u32::try_from(artifact.package.defs.len()).expect("def count fits u32");
    decls.served_methods =
        u32::try_from(artifact.package.methods.len()).expect("method count fits u32");

    // The def-named identity range: `none` and `type` plus the atom
    // log's length — every raw id below it is one the merged walk must
    // reproduce, every raw id at or above it names a mint.
    let identity_limit = 2 + u32::try_from(artifact.universe.atoms.len()).expect("count fits u32");
    let rows = ServedRows {
        names: Arc::clone(&tables.names),
        identity_limit,
        atoms: artifact.universe.atoms.clone(),
        def_atoms: artifact.decls.atoms.clone(),
        atom_defs: artifact.decls.defs_of_atoms.clone(),
        traits: artifact.universe.traits.clone(),
        memberships: artifact.universe.memberships.clone(),
        mints: artifact.universe.mints.clone(),
        sets: artifact.universe.sets.clone(),
        instances: artifact.instances.clone(),
        impl_args: artifact.universe.impl_args.clone(),
        inherent_bounds: artifact.universe.inherent_bounds.clone(),
        var_counter: artifact.universe.var_counter,
    };
    let resolve = Box::new(move |decls: &mut ds_check::Decls| resolve_rows(decls, &rows));

    let effect_defs = Arc::clone(&tables.effect_defs);
    let effect_methods = Arc::clone(&tables.effect_methods);

    ServedPrefix {
        package,
        resolutions,
        decls: ServedDecls {
            decls,
            resolve,
            files: artifact.package.files.len(),
        },
        effect_defs,
        effect_methods,
    }
}

/// One artifact's name-keyed rows, moved into the per-serve resolve
/// step: everything that resolves against the merged compile's own
/// universe.
pub(super) struct ServedRows {
    /// The interned name table.
    pub(super) names: Arc<[String]>,
    /// The raw ids the merged walk must reproduce: `none`, `type`, and
    /// the atom log's length — one past the def-named identity range.
    pub(super) identity_limit: u32,
    /// The def-named atoms with their final shapes.
    pub(super) atoms: Vec<AtomEntry>,
    /// The def-to-atom rows: the defs this walk does not re-mint (a
    /// linked type this compile's own link pass registers) keep their
    /// table rows from the artifact, so the late linked mint reuses the
    /// replayed atom instead of declaring a duplicate.
    pub(super) def_atoms: Vec<(u32, AtomRef)>,
    /// The atom-to-def rows, the reverse of `def_atoms`.
    pub(super) atom_defs: Vec<(AtomRef, u32)>,
    /// The declared traits.
    pub(super) traits: Vec<TraitRef>,
    /// The trait memberships.
    pub(super) memberships: Vec<(TraitRef, AtomRef)>,
    /// The shape-phase mints, in mint order.
    pub(super) mints: Vec<MintEntry>,
    /// The interned member sets, in intern order.
    pub(super) sets: Vec<Vec<AtomRef>>,
    /// The instance rows, in id order.
    pub(super) instances: Vec<InstanceRowData>,
    /// The solved trait-reference arguments per impl span.
    pub(super) impl_args: Vec<(Span, Vec<u32>)>,
    /// The solved generic inherent bounds per impl span.
    pub(super) inherent_bounds: Vec<(Span, InherentBoundData)>,
    /// The settled type-variable counter.
    pub(super) var_counter: u32,
}

/// One name reference's failure, for the drift gates' output.
pub(super) fn drift(detail: String) -> ds_check::ServedDrift {
    ds_check::ServedDrift { detail }
}

/// One name reference's string, bounds-checked against the table.
pub(super) fn name_of(names: &[String], index: u32) -> Result<&str, ds_check::ServedDrift> {
    names
        .get(index as usize)
        .map(String::as_str)
        .ok_or_else(|| drift(format!("a name index {index} past the table")))
}

/// One atom reference over the merged universe: the name resolves (a
/// name that does not is a corrupt or foreign artifact), and inside the
/// def-named identity range the landing must equal the recorded raw id
/// — the merged walk reproduced the generation's ids or the serve
/// refuses, never serving a wrong fact.
pub(super) fn resolve_atom(
    universe: &Universe,
    names: &[String],
    identity_limit: u32,
    reference: AtomRef,
) -> Result<TypeId, ds_check::ServedDrift> {
    let name = name_of(names, reference.name)?;
    let id = universe.atom_by_name(name).ok_or_else(|| {
        drift(format!(
            "the atom name `{name}` does not resolve in this compile"
        ))
    })?;
    if id.index() != reference.id && reference.id < identity_limit {
        return Err(drift(format!(
            "the atom `{name}` minted at {} where the artifact records {}",
            id.index(),
            reference.id
        )));
    }
    Ok(id)
}

/// One trait reference: the name resolves onto the recorded id (every
/// trait is def-declared in walk order, so the reference is always an
/// identity check).
pub(super) fn resolve_trait(
    universe: &Universe,
    names: &[String],
    reference: TraitRef,
) -> Result<TraitId, ds_check::ServedDrift> {
    let name = name_of(names, reference.name)?;
    match universe.trait_by_name(name) {
        Some(id) if id.index() == reference.id => Ok(id),
        _ => Err(drift(format!(
            "the trait name `{name}` does not resolve at the artifact's id {}",
            reference.id
        ))),
    }
}

/// One name-referenced type expression over the merged universe.
pub(super) fn resolve_expr(
    universe: &Universe,
    names: &[String],
    identity_limit: u32,
    expr: &TypeExprData,
) -> Result<TypeExpr, ds_check::ServedDrift> {
    Ok(match expr {
        TypeExprData::Atom(reference) => {
            TypeExpr::Atom(resolve_atom(universe, names, identity_limit, *reference)?)
        },
        TypeExprData::Trait(reference) => {
            TypeExpr::Trait(resolve_trait(universe, names, *reference)?)
        },
        TypeExprData::Not(reference) => TypeExpr::Not(resolve_trait(universe, names, *reference)?),
        TypeExprData::Union(a, b) => TypeExpr::Union(
            Box::new(resolve_expr(universe, names, identity_limit, a)?),
            Box::new(resolve_expr(universe, names, identity_limit, b)?),
        ),
        TypeExprData::Inter(a, b) => TypeExpr::Inter(
            Box::new(resolve_expr(universe, names, identity_limit, a)?),
            Box::new(resolve_expr(universe, names, identity_limit, b)?),
        ),
        TypeExprData::Diff(a, b) => TypeExpr::Diff(
            Box::new(resolve_expr(universe, names, identity_limit, a)?),
            Box::new(resolve_expr(universe, names, identity_limit, b)?),
        ),
        TypeExprData::Tuple(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(resolve_expr(universe, names, identity_limit, item)?);
            }
            TypeExpr::Tuple(out)
        },
        TypeExprData::Empty => TypeExpr::Empty,
    })
}

/// Re-interns every set whose members resolve, in intern order, ahead
/// of the mint that first reads one: the generation interned each set
/// before the apply minting over it, and every member minted before the
/// set's intern, so the sets the mint log reaches are exactly the ones
/// this cursor has landed. A set landing off its recorded index is a
/// duplicate content or a misordered log — both drift.
pub(super) fn drain_sets(
    universe: &mut Universe,
    rows: &ServedRows,
    cursor: &mut usize,
    tys: &mut Vec<ds_check::Ty>,
) -> Result<(), ds_check::ServedDrift> {
    while *cursor < rows.sets.len() {
        let mut members = Vec::with_capacity(rows.sets[*cursor].len());
        for reference in &rows.sets[*cursor] {
            match resolve_atom(universe, &rows.names, rows.identity_limit, *reference) {
                Ok(atom) => members.push(atom),
                Err(_) => return Ok(()),
            }
        }
        let ty = universe.set_ty(&members);
        if ty.index() != u32::try_from(*cursor).expect("set count fits u32") {
            return Err(drift(format!(
                "an interned set landed at {} where the artifact records {cursor}",
                ty.index()
            )));
        }
        tys.push(ty);
        *cursor += 1;
    }
    Ok(())
}

/// Resolves one artifact's name-keyed rows over the merged compile's
/// universe — the validating read. The mints replay through the
/// declaring pass's own operations in mint order (each landing one
/// constant shift off its recorded raw id, checked), the interned sets
/// re-intern over name-resolved members in the generation's intern
/// order, the def-named atoms take their shapes and their identity
/// checks, the memberships apply by name (a minted member — a tuple
/// head — resolves onto its replay landing), and the instance rows and
/// solved span-keyed records install over the re-interned handles.
/// Every refusal is a [`ServedDrift`] the declaration build's caller
/// answers with the fresh path.
pub(super) fn resolve_rows(
    decls: &mut ds_check::Decls,
    rows: &ServedRows,
) -> Result<(), ds_check::ServedDrift> {
    let universe = &mut decls.universe;
    let names = &rows.names[..];

    // The shape-phase mints, in mint order, with the sets they read
    // interning ahead of them. A linked struct declares now and takes
    // its fields below, once every mint has landed.
    let mut tys: Vec<ds_check::Ty> = Vec::new();
    let mut set_cursor = 0usize;
    let mut shift: Option<i64> = None;
    let mut landed_structs: Vec<(AtomRef, &Vec<FieldData>)> = Vec::new();
    for entry in &rows.mints {
        drain_sets(universe, rows, &mut set_cursor, &mut tys)?;
        let landed = match &entry.data {
            MintData::Var(reference) => {
                let name = name_of(names, reference.name)?;
                universe
                    .declare_tag(name)
                    .map_err(|_| drift(format!("the minted variable `{name}` refuses to replay")))?
            },
            MintData::Tuple(references) => {
                let mut elements = Vec::with_capacity(references.len());
                for reference in references {
                    elements.push(resolve_atom(
                        universe,
                        names,
                        rows.identity_limit,
                        *reference,
                    )?);
                }
                universe
                    .tuple_atom(&elements)
                    .map_err(|_| drift("a minted tuple atom refuses to replay".to_owned()))?
            },
            MintData::Apply { ctor, args } => {
                let ctor = resolve_atom(universe, names, rows.identity_limit, *ctor)?;
                let mut sets = Vec::with_capacity(args.len());
                for &index in args {
                    let ty = tys.get(index as usize).copied().ok_or_else(|| {
                        drift(format!(
                            "a mint names the intern set {index} before it lands"
                        ))
                    })?;
                    sets.push(ty);
                }
                universe
                    .apply_atom(ctor, &sets)
                    .map_err(|_| drift("a minted application atom refuses to replay".to_owned()))?
            },
            MintData::Struct { name, fields } => {
                let reference = *name;
                let spelled = name_of(names, reference.name)?;
                let id = universe.declare_struct(spelled).map_err(|_| {
                    drift(format!("the linked struct `{spelled}` refuses to replay"))
                })?;
                landed_structs.push((reference, fields));
                id
            },
            MintData::Reserved(reference) => {
                let name = name_of(names, reference.name)?;
                universe
                    .declare_struct(name)
                    .map_err(|_| drift(format!("the linked opaque `{name}` refuses to replay")))?
            },
        };
        // The mint log is one contiguous tail of the generation's atom
        // space, so every landing must sit one constant shift off the
        // recorded id — the merged walk's own atoms are the shift.
        let landing = i64::from(landed.index()) - i64::from(entry.id);
        if shift.is_some_and(|seen| seen != landing) {
            return Err(drift(format!(
                "a mint landed at a shift of {landing} where the log's is {}",
                shift.unwrap_or_default()
            )));
        }
        shift = Some(landing);
    }
    drain_sets(universe, rows, &mut set_cursor, &mut tys)?;

    // The linked structs' fields: every mint has landed, so the ambient
    // identities and their own applications resolve by name.
    for (reference, fields) in landed_structs {
        let id = resolve_atom(universe, names, rows.identity_limit, reference)?;
        let mut converted = Vec::with_capacity(fields.len());
        for field in fields {
            converted.push(Field {
                name: field.name.clone(),
                ty: resolve_expr(universe, names, rows.identity_limit, &field.ty)?,
            });
        }
        universe.set_struct_fields(id, converted).map_err(|_| {
            drift(format!(
                "the linked struct `{}` refuses its shape",
                name_of(names, reference.name).unwrap_or("?")
            ))
        })?;
    }

    // The def-named atoms: each name must resolve onto its recorded id —
    // the walk divergence check — and each shape applies over the
    // merged walk's own atom.
    for entry in &rows.atoms {
        let id = resolve_atom(universe, names, rows.identity_limit, entry.name)?;
        match &entry.shape {
            AtomShapeData::Unit | AtomShapeData::Reserved => {},
            AtomShapeData::Struct(fields) => {
                let mut converted = Vec::with_capacity(fields.len());
                for field in fields {
                    converted.push(Field {
                        name: field.name.clone(),
                        ty: resolve_expr(universe, names, rows.identity_limit, &field.ty)?,
                    });
                }
                universe
                    .set_struct_fields(id, converted)
                    .map_err(|_| drift("a served shape refuses to apply".to_owned()))?;
            },
            AtomShapeData::Enum(cases) => {
                let mut converted = Vec::with_capacity(cases.len());
                for case in cases {
                    let mut fields = Vec::with_capacity(case.fields.len());
                    for field in &case.fields {
                        fields.push(Field {
                            name: field.name.clone(),
                            ty: resolve_expr(universe, names, rows.identity_limit, &field.ty)?,
                        });
                    }
                    converted.push(Case {
                        name: case.name.clone(),
                        fields,
                    });
                }
                universe
                    .set_enum_cases(id, converted)
                    .map_err(|_| drift("a served shape refuses to apply".to_owned()))?;
            },
        }
    }

    // The def-atom table rows: the defs this walk's own reserve pass
    // does not mint (a linked type this compile's link pass registered
    // at the package tail) take their rows from the artifact, so the
    // late linked mint reuses the replayed atom instead of declaring a
    // duplicate — and a def the walk did mint re-installs the same
    // pair, its identity the rows' checks already proved.
    for (def, reference) in &rows.def_atoms {
        let atom = resolve_atom(universe, names, rows.identity_limit, *reference)?;
        decls.atoms.insert(DefId::from_raw(*def), atom);
    }
    for (reference, def) in &rows.atom_defs {
        let atom = resolve_atom(universe, names, rows.identity_limit, *reference)?;
        decls.defs_of_atoms.insert(atom, DefId::from_raw(*def));
    }

    // The trait identity checks, then the memberships by name — a
    // member minted by the shape phase (a tuple head) resolves onto its
    // replay landing.
    for reference in &rows.traits {
        resolve_trait(universe, names, *reference)?;
    }
    for (trait_ref, atom_ref) in &rows.memberships {
        let trait_id = resolve_trait(universe, names, *trait_ref)?;
        let atom = resolve_atom(universe, names, rows.identity_limit, *atom_ref)?;
        universe
            .impl_trait(trait_id, atom)
            .map_err(|_| drift("a served membership names no atom".to_owned()))?;
    }

    // The instance rows, their solved snapshots the re-interned handles
    // the sets table landed.
    let solved =
        |index: &u32, tys: &Vec<ds_check::Ty>| -> Result<ds_check::Ty, ds_check::ServedDrift> {
            tys.get(*index as usize).copied().ok_or_else(|| {
                drift(format!(
                    "a solved snapshot names the intern set {index} past the table"
                ))
            })
        };
    let instances: Result<Vec<ServedInstance>, ds_check::ServedDrift> = rows
        .instances
        .iter()
        .map(|row| {
            Ok(ServedInstance {
                trait_id: resolve_trait(universe, names, row.trait_id)?,
                name: row.name.clone().map(Arc::from),
                head_expr: resolve_expr(universe, names, rows.identity_limit, &row.head_expr)?,
                head: solved(&row.head, &tys)?,
                written: solved(&row.written, &tys)?,
                condition_exprs: row
                    .condition_exprs
                    .iter()
                    .map(|(trait_ref, expr)| {
                        Ok(Condition::new(
                            resolve_trait(universe, names, *trait_ref)?,
                            resolve_expr(universe, names, rows.identity_limit, expr)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                conditions: row
                    .conditions
                    .iter()
                    .map(|(trait_ref, index)| {
                        Ok(Condition::new(
                            resolve_trait(universe, names, *trait_ref)?,
                            solved(index, &tys)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect();
    decls.instances.serve(instances?);

    // The solved span-keyed records, over the same handles.
    for (span, args) in &rows.impl_args {
        let mut solved_args = Vec::with_capacity(args.len());
        for index in args {
            solved_args.push(solved(index, &tys)?);
        }
        decls.trait_impl_args.insert(*span, solved_args);
    }
    for (span, bound) in &rows.inherent_bounds {
        let mut conditions = Vec::with_capacity(bound.conditions.len());
        for (trait_ref, index) in &bound.conditions {
            conditions.push(Condition::new(
                resolve_trait(universe, names, *trait_ref)?,
                solved(index, &tys)?,
            ));
        }
        decls
            .inherent_impl_bounds
            .insert(*span, ds_check::InherentBounds {
                head: solved(&bound.head, &tys)?,
                conditions,
            });
    }
    decls.var_counter = rows.var_counter;
    Ok(())
}

/// Converts the namespace parts into the shared base the serve installs:
/// every id it names (definitions, methods) is one every serve mints
/// identically, so the base is shareable across a process's serves.
pub(super) fn package_base(artifact: &PrefixDsi) -> Result<PackageBase, PrefixDsiError> {
    let defs = artifact
        .package
        .defs
        .iter()
        .map(|def| {
            Ok(Def {
                name: def.name.clone(),
                kind: kind_of(def.kind)?,
                vis: vis_of(def.vis)?,
                span: def.span,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let methods = artifact
        .package
        .methods
        .iter()
        .map(|method| MethodDecl {
            name: method.name.clone(),
            owner: match &method.owner {
                MethodOwnerData::Trait(def) => MethodOwner::Trait(DefId::from_raw(*def)),
                MethodOwnerData::Inherent(target) => MethodOwner::Inherent(target.clone()),
            },
            span: method.span,
        })
        .collect();
    Ok(PackageBase::from_parts(
        artifact.package.files.clone(),
        defs,
        artifact
            .package
            .type_names
            .iter()
            .map(|(name, id)| (name.clone(), DefId::from_raw(*id)))
            .collect(),
        artifact
            .package
            .value_names
            .iter()
            .map(|(name, id)| (name.clone(), DefId::from_raw(*id)))
            .collect(),
        methods,
        artifact
            .package
            .method_index
            .iter()
            .map(|(name, ids)| {
                (
                    name.clone(),
                    ids.iter()
                        .map(|&id| MethodId::from_raw(id))
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
    ))
}

/// Rebuilds the member's resolution table.
pub(super) fn serve_resolutions(artifact: &PrefixDsi) -> Vec<Resolutions> {
    artifact
        .uses
        .iter()
        .map(|file| {
            Resolutions::from_uses(
                file.iter()
                    .map(|use_| {
                        let res = match &use_.res {
                            ResData::Def(def) => Res::Def(DefId::from_raw(*def)),
                            ResData::Local { bind } => Res::Local { bind: *bind },
                            ResData::TraitMethod(method) => {
                                Res::TraitMethod(MethodId::from_raw(*method))
                            },
                            ResData::Dot(dot) => Res::Dot(match dot {
                                DotResData::Free { func, fed_param } => ds_check::DotRes::Free {
                                    func: DefId::from_raw(*func),
                                    fed_param: *fed_param as usize,
                                },
                                DotResData::Methods { candidates } => ds_check::DotRes::Methods {
                                    candidates: candidates
                                        .iter()
                                        .map(|&id| MethodId::from_raw(id))
                                        .collect(),
                                },
                                DotResData::Static { candidates } => ds_check::DotRes::Static {
                                    candidates: candidates
                                        .iter()
                                        .map(|&id| MethodId::from_raw(id))
                                        .collect(),
                                },
                                DotResData::Module { func } => ds_check::DotRes::Module {
                                    func: DefId::from_raw(*func),
                                },
                            }),
                            ResData::Handler => Res::Handler,
                            ResData::Module(module) => {
                                Res::Module(ds_check::ModuleId::from_raw(*module))
                            },
                        };
                        (use_.span, res)
                    })
                    .collect(),
            )
        })
        .collect()
}
