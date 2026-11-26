//! The extraction half: one finished analyze and its declaration tables to the payload's data form.

use std::collections::{HashMap, HashSet};

use ds_check::{
    Analyzed, AtomKind, Case, DefId, Effects, Field, FnSig, InstanceId, MethodOwner, Res,
    ServedInstance, ServedMethodRow, TraitId, TypeExpr, TypeId, Universe,
};
use ds_diag::Span;
use ds_mod::Hash128;

use super::*;

/// The artifact's bytes: source-hash header plus the postcard payload.
#[must_use]
pub fn artifact_bytes(source_hash: Hash128, artifact: &PrefixDsi) -> Vec<u8> {
    let payload = postcard::to_allocvec(artifact).expect("the artifact serializes");
    let mut bytes = source_hash.as_bytes().to_vec();
    bytes.extend(Hash128::compute(&payload).as_bytes());
    bytes.extend(payload);
    bytes
}

/// Extracts the artifact's tables from one package's declaration-phase
/// tables plus its finished analyze (the namespace, uses, and effect
/// rows come from the analyze; every declaration table and log from the
/// phase snapshot — the body pass interns more sets, so the snapshot
/// the caller passes is the declaration phase alone). Deterministic by
/// construction: every list follows its table's id order or the walk's
/// own order.
#[must_use]
pub fn extract(
    decls: &ds_check::Decls,
    analyzed: &Analyzed,
    effects: &Effects,
    method_rows: &[ServedMethodRow],
) -> PrefixDsi {
    let package = &analyzed.package;
    // The import-bound definitions: excluded from the def-named
    // atom log and re-keyed as mints (see `universe_log`), like their
    // exclusion from the defs below.
    let foreign: HashSet<DefId> = package.foreign().keys().copied().collect();
    let mut names = Names::default();

    let mut type_names = Vec::new();
    let mut value_names = Vec::new();
    let defs: Vec<DefData> = package
        .defs()
        .enumerate()
        .filter_map(|(index, def)| {
            let id = DefId::from_raw(u32::try_from(index).expect("def count fits u32"));
            // The import-bound definitions never ride the artifact:
            // their link-pass products (the foreign map, the per-file
            // bindings, the module refs) are compile-time state a serve
            // re-derives from its own fresh graph, and the defs themselves
            // sit strictly after every collected definition, so the
            // serve's link re-run re-registers them at the same tail ids
            // the served resolutions and tables name.
            if foreign.contains(&id) {
                return None;
            }
            if package.lookup_type(&def.name) == Some(id) {
                type_names.push((def.name.clone(), id.index()));
            }
            if package.lookup_value(&def.name) == Some(id) {
                value_names.push((def.name.clone(), id.index()));
            }
            Some(DefData {
                name: def.name.clone(),
                kind: kind_bits(def.kind),
                vis: vis_bits(def.vis),
                span: def.span,
            })
        })
        .collect();
    let mut method_index: Vec<(String, Vec<u32>)> = Vec::new();
    // the name's row in `method_index`, so one pass builds the index
    // without a linear find per method
    let mut method_slots: HashMap<&str, usize> = HashMap::new();
    let methods: Vec<MethodDeclData> = package
        .methods()
        .enumerate()
        .map(|(index, method)| {
            let id = u32::try_from(index).expect("method count fits u32");
            let slot = *method_slots.entry(method.name.as_str()).or_insert_with(|| {
                method_index.push((method.name.clone(), vec![]));
                method_index.len() - 1
            });
            method_index[slot].1.push(id);
            MethodDeclData {
                name: method.name.clone(),
                owner: match &method.owner {
                    MethodOwner::Trait(def) => MethodOwnerData::Trait(def.index()),
                    MethodOwner::Inherent(target) => MethodOwnerData::Inherent(target.clone()),
                },
                span: method.span,
            }
        })
        .collect();

    let uses = analyzed
        .resolutions
        .iter()
        .map(|table| {
            table
                .uses()
                .iter()
                .map(|(span, res)| UseData {
                    span: *span,
                    res: res_data(res),
                })
                .collect()
        })
        .collect();

    let universe = universe_log(decls, &foreign, &mut names);
    let tables = decls_data(decls, &mut names);
    let instances = instance_rows_of(decls, &mut names);
    PrefixDsi {
        names: names.finish(),
        package: PackageData {
            files: package.files().collect(),
            defs,
            type_names,
            value_names,
            methods,
            method_index,
        },
        uses,
        universe,
        decls: tables,
        instances,
        effects: effects
            .defs()
            .iter()
            .map(|row| DefRowData {
                def: row.def.index(),
                name: row.name.clone(),
                kind: kind_bits(row.kind),
                span: row.span,
                row: row.row.bits(),
                caps: row.caps,
            })
            .collect(),
        method_rows: method_rows
            .iter()
            .map(|row| MethodRowData {
                method: row.method.index(),
                row: row.row.bits(),
                caps: row.caps,
            })
            .collect(),
    }
}

/// The extraction's name interner: one table the artifact's rows
/// share, filled in the extraction's deterministic walk order.
#[derive(Default)]
pub(super) struct Names {
    names: Vec<String>,
    seen: HashMap<String, u32>,
}

impl Names {
    /// One name's table index, interning on first sight.
    pub(super) fn intern(&mut self, name: &str) -> u32 {
        if let Some(&index) = self.seen.get(name) {
            return index;
        }
        let index = u32::try_from(self.names.len()).expect("name count fits u32");
        self.names.push(name.to_owned());
        self.seen.insert(name.to_owned(), index);
        index
    }

    /// One atom's name reference.
    pub(super) fn atom(&mut self, universe: &Universe, id: TypeId) -> AtomRef {
        let name = universe
            .atom_decl(id)
            .expect("every id has a decl")
            .name
            .clone();
        AtomRef {
            name: self.intern(&name),
            id: id.index(),
        }
    }

    /// One trait's name reference.
    pub(super) fn trait_ref(&mut self, universe: &Universe, id: TraitId) -> TraitRef {
        let name = universe
            .trait_decl(id)
            .expect("registered traits exist")
            .name
            .clone();
        TraitRef {
            name: self.intern(&name),
            id: id.index(),
        }
    }

    /// The finished table.
    pub(super) fn finish(self) -> Vec<String> {
        self.names
    }
}

/// The universe's declaration-phase facts from the snapshot: the
/// def-named atoms with final shapes (the import-bound types' atoms
/// excepted — they ride the mints below), the declared traits in order,
/// the trait memberships, the shape-phase mints, the interned sets, the
/// solved span-keyed records, and the settled variable counter — every
/// atom and trait reference through the name table.
pub(super) fn universe_log(
    decls: &ds_check::Decls,
    foreign: &HashSet<DefId>,
    names: &mut Names,
) -> UniverseLog {
    let universe = &decls.universe;
    let mut log = Vec::new();
    for id in universe.atom_ids() {
        if id == TypeId::NONE || !decls.defs_of_atoms.contains_key(&id) {
            continue;
        }
        // An import-bound type's atom is def-named but minted late (the
        // link pass's products mint after the shape phase), so the
        // def-named log — whose ids the merged walk must reproduce —
        // cannot place it: it rides the mints below.
        if foreign.contains(&decls.defs_of_atoms.get(&id).copied().unwrap()) {
            continue;
        }
        let decl = universe.atom_decl(id).expect("every id has a decl");
        log.push(AtomEntry {
            name: names.atom(universe, id),
            shape: match &decl.kind {
                AtomKind::Unit => AtomShapeData::Unit,
                AtomKind::Reserved => AtomShapeData::Reserved,
                AtomKind::Struct(fields) => AtomShapeData::Struct(
                    fields
                        .iter()
                        .map(|f| field_data(f, universe, names))
                        .collect(),
                ),
                AtomKind::Enum(cases) => AtomShapeData::Enum(
                    cases
                        .iter()
                        .map(|c| case_data(c, universe, names))
                        .collect(),
                ),
                AtomKind::Tuple(_) | AtomKind::Apply(..) | AtomKind::FnSig(..) => {
                    panic!("a def-named atom never carries a structural shape")
                },
            },
        });
    }
    let linked_atoms = decls
        .defs_of_atoms
        .iter()
        .filter(|(_, def)| foreign.contains(def))
        .count();
    assert_eq!(
        log.len(),
        decls.defs_of_atoms.len() - linked_atoms,
        "the atom log must carry every def-named atom"
    );
    let mut memberships = Vec::new();
    for (_, &trait_id) in &decls.traits {
        let trait_decl = universe
            .trait_decl(trait_id)
            .expect("registered traits exist");
        let trait_ref = names.trait_ref(universe, trait_id);
        for atom in &trait_decl.members {
            memberships.push((trait_ref, names.atom(universe, *atom)));
        }
    }

    // The declared traits in declaration order. The registry iterates by
    // def id; sorting by trait id restores the declaration order.
    let mut trait_rows: Vec<(u32, TraitId)> = decls
        .traits
        .iter()
        .map(|(_, &id)| (id.index(), id))
        .collect();
    trait_rows.sort_unstable();
    let traits: Vec<TraitRef> = trait_rows
        .into_iter()
        .map(|(_, id)| names.trait_ref(universe, id))
        .collect();

    // The shape-phase mints: every atom the impl arms minted (variables
    // and structural atoms), in mint order, each with its raw
    // generation id beside its name-referenced content. A linked type's
    // atom rides the same log's tail; its fields name the ambient's
    // def-named atoms and its own later-minted applications, resolved
    // by name once every mint has landed.
    let mints: Vec<MintEntry> = universe
        .atom_ids()
        .filter(|id| *id != TypeId::NONE && *id != TypeId::TYPE)
        .filter_map(|id| {
            let decl = universe.atom_decl(id).expect("every id has a decl");
            let raw = id.index();
            if let Some(&def) = decls.defs_of_atoms.get(&id) {
                if !foreign.contains(&def) {
                    return None;
                }
                let linked = match &decl.kind {
                    // a linked struct, or a linked enum case's struct
                    // payload, rides its shape
                    AtomKind::Struct(fields) => MintData::Struct {
                        name: names.atom(universe, id),
                        fields: fields
                            .iter()
                            .map(|f| field_data(f, universe, names))
                            .collect(),
                    },
                    // a linked enum's unit case rides its bare name
                    AtomKind::Unit => MintData::Var(names.atom(universe, id)),
                    // a linked opaque struct rides its bare reserved
                    // identity
                    AtomKind::Reserved => MintData::Reserved(names.atom(universe, id)),
                    _ => panic!("a linked type's atom is a struct, a unit case, or reserved"),
                };
                return Some(MintEntry {
                    id: raw,
                    data: linked,
                });
            }
            let mint = match &decl.kind {
                AtomKind::Unit => MintData::Var(names.atom(universe, id)),
                AtomKind::Tuple(elements) => {
                    MintData::Tuple(elements.iter().map(|&e| names.atom(universe, e)).collect())
                },
                AtomKind::Apply(ctor, args) => MintData::Apply {
                    ctor: names.atom(universe, *ctor),
                    args: args.iter().map(|ty| ty.index()).collect(),
                },
                AtomKind::Reserved
                | AtomKind::Struct(_)
                | AtomKind::Enum(_)
                | AtomKind::FnSig(..) => {
                    panic!("a shape-phase mint is a variable or structural atom")
                },
            };
            Some(MintEntry {
                id: raw,
                data: mint,
            })
        })
        .collect();

    // The interned sets in intern order: the members by name, the raw
    // Ty indices every solved snapshot in the artifact names.
    let sets: Vec<Vec<AtomRef>> = universe
        .set_log()
        .map(|members| {
            members
                .iter()
                .map(|&atom| names.atom(universe, atom))
                .collect()
        })
        .collect();

    // The solved span-keyed records of the declaration phase, the spans
    // sorted for the byte gate's determinism.
    let mut impl_args: Vec<(Span, Vec<ds_check::Ty>)> = decls
        .trait_impl_args
        .iter()
        .map(|(span, args)| (*span, args.clone()))
        .collect();
    impl_args.sort_unstable_by_key(|(span, _)| (span.file.as_u32(), span.start, span.end));
    let impl_args: Vec<(Span, Vec<u32>)> = impl_args
        .into_iter()
        .map(|(span, args)| (span, args.iter().map(|ty| ty.index()).collect()))
        .collect();
    let mut bounds: Vec<(Span, &ds_check::InherentBounds)> = decls
        .inherent_impl_bounds
        .iter()
        .map(|(s, b)| (*s, b))
        .collect();
    bounds.sort_unstable_by_key(|(span, _)| (span.file.as_u32(), span.start, span.end));
    let inherent_bounds: Vec<(Span, InherentBoundData)> = bounds
        .into_iter()
        .map(|(span, bound)| {
            (span, InherentBoundData {
                head: bound.head.index(),
                conditions: bound
                    .conditions
                    .iter()
                    .map(|cond| (names.trait_ref(universe, cond.trait_id), cond.ty.index()))
                    .collect(),
            })
        })
        .collect();

    UniverseLog {
        atoms: log,
        traits,
        memberships,
        mints,
        sets,
        impl_args,
        inherent_bounds,
        var_counter: decls.var_counter,
    }
}

/// The plain and signature declaration tables from the declaration-phase
/// snapshot, every list in its table's id order.
pub(super) fn decls_data(decls: &ds_check::Decls, names: &mut Names) -> DeclsData {
    let structs = decls
        .structs
        .iter()
        .map(|(def, shape)| {
            (def.index(), StructShapeData {
                params: shape.params.iter().map(generic_param_data).collect(),
                fields: shape.fields.clone(),
                linear: shape.linear,
            })
        })
        .collect();
    let enums = decls
        .enums
        .iter()
        .map(|(def, shape)| {
            (def.index(), EnumShapeData {
                params: shape.params.iter().map(generic_param_data).collect(),
                cases: shape
                    .cases
                    .iter()
                    .map(|(name, span, payload)| EnumCaseData {
                        name: name.clone(),
                        span: *span,
                        payload: payload.clone(),
                    })
                    .collect(),
                linear: shape.linear,
            })
        })
        .collect();
    let cases = decls
        .cases
        .iter()
        .map(|(def, case)| {
            (def.index(), CaseDeclData {
                enum_def: case.enum_def.index(),
                payload: case.payload.clone(),
            })
        })
        .collect();
    DeclsData {
        atoms: decls
            .atoms
            .iter()
            .map(|(def, atom)| (def.index(), names.atom(&decls.universe, *atom)))
            .collect(),
        defs_of_atoms: decls
            .defs_of_atoms
            .iter()
            .map(|(atom, def)| (names.atom(&decls.universe, *atom), def.index()))
            .collect(),
        traits: decls
            .traits
            .iter()
            .map(|(def, trait_id)| (def.index(), names.trait_ref(&decls.universe, *trait_id)))
            .collect(),
        structs,
        enums,
        cases,
        fns: decls
            .fns
            .iter()
            .map(|(def, sig)| (def.index(), fn_sig_data(sig)))
            .collect(),
        members: decls
            .members
            .iter()
            .map(|(method, sig)| (method.index(), fn_sig_data(sig)))
            .collect(),
        inherent: decls
            .inherent
            .iter()
            .map(|(method, sig)| (method.index(), fn_sig_data(sig)))
            .collect(),
        trait_members: decls
            .trait_members
            .iter()
            .map(|(def, names)| (def.index(), names.clone()))
            .collect(),
        trait_member_defaults: decls
            .trait_member_defaults
            .iter()
            .map(|(def, names)| (def.index(), names.clone()))
            .collect(),
        trait_generics: decls
            .trait_generics
            .iter()
            .map(|(def, names)| (def.index(), names.clone()))
            .collect(),
        instance_impls: decls
            .instance_impls
            .iter()
            .map(|(id, span)| (id.index(), *span))
            .collect(),
        inherent_impls: decls
            .inherent_impls
            .iter()
            .map(|(method, span)| (method.index(), *span))
            .collect(),
    }
}

/// The instance rows, gated by the coherence skip's stability rule: a
/// head or condition naming a trait-relative set refuses the generation
/// (the serve's skip could not carry it).
pub(super) fn instance_rows_of(decls: &ds_check::Decls, names: &mut Names) -> Vec<InstanceRowData> {
    (0..u32::try_from(decls.instances.len()).expect("instance count fits u32"))
        .map(|index| {
            let row = decls
                .instances
                .row(InstanceId::from_raw(index))
                .expect("every id has a row");
            assert_stable(&row.head_expr);
            for cond in &row.condition_exprs {
                assert_stable(&cond.ty);
            }
            instance_row_data(&row, &decls.universe, names)
        })
        .collect()
}

pub(super) fn generic_param_data(param: &ds_check::GenericParam) -> GenericParamData {
    GenericParamData {
        name: param.name.clone(),
        span: param.span,
        bound: param.bound.clone(),
    }
}

pub(super) fn generic_param(data: &GenericParamData) -> ds_check::GenericParam {
    ds_check::GenericParam {
        name: data.name.clone(),
        span: data.span,
        bound: data.bound.clone(),
    }
}

pub(super) fn fn_sig_data(sig: &FnSig) -> FnSigData {
    FnSigData {
        generics: sig.generics.iter().map(generic_param_data).collect(),
        params: sig
            .params
            .iter()
            .map(|param| ParamSigData {
                name: param.name.clone(),
                span: param.span,
                mode: param.mode,
                ty: param.ty.clone(),
                default: param.default.clone(),
            })
            .collect(),
        ret: sig.ret.clone(),
        bind: sig.bind.clone(),
    }
}

pub(super) fn fn_sig(data: &FnSigData) -> FnSig {
    FnSig {
        generics: data.generics.iter().map(generic_param).collect(),
        params: data
            .params
            .iter()
            .map(|param| ds_check::decls::ParamSig {
                name: param.name.clone(),
                span: param.span,
                mode: param.mode,
                ty: param.ty.clone(),
                default: param.default.clone(),
            })
            .collect(),
        ret: data.ret.clone(),
        bind: data.bind.clone(),
    }
}

pub(super) fn instance_row_data(
    row: &ServedInstance,
    universe: &Universe,
    names: &mut Names,
) -> InstanceRowData {
    InstanceRowData {
        trait_id: names.trait_ref(universe, row.trait_id),
        name: row.name.clone().map(|name| name.to_string()),
        head_expr: type_expr_data(&row.head_expr, universe, names),
        head: row.head.index(),
        written: row.written.index(),
        condition_exprs: row
            .condition_exprs
            .iter()
            .map(|cond| {
                (
                    names.trait_ref(universe, cond.trait_id),
                    type_expr_data(&cond.ty, universe, names),
                )
            })
            .collect(),
        conditions: row
            .conditions
            .iter()
            .map(|cond| (names.trait_ref(universe, cond.trait_id), cond.ty.index()))
            .collect(),
    }
}

/// Panics on a solved snapshot's written form naming a trait or a
/// negation: the only expressions whose solving membership growth widens,
/// which the served coherence skip cannot carry (the generator refuses
/// such an ambient until the serve learns to fall back).
pub(super) fn assert_stable(expr: &TypeExpr) {
    match expr {
        TypeExpr::Trait(_) | TypeExpr::Not(_) => {
            panic!("a served instance head or condition names a trait-relative set")
        },
        TypeExpr::Union(a, b) | TypeExpr::Inter(a, b) | TypeExpr::Diff(a, b) => {
            assert_stable(a);
            assert_stable(b);
        },
        TypeExpr::Atom(_) | TypeExpr::Empty => {},
        TypeExpr::Tuple(items) => {
            for item in items {
                assert_stable(item);
            }
        },
    }
}

pub(super) fn field_data(field: &Field, universe: &Universe, names: &mut Names) -> FieldData {
    FieldData {
        name: field.name.clone(),
        ty: type_expr_data(&field.ty, universe, names),
    }
}

pub(super) fn case_data(case: &Case, universe: &Universe, names: &mut Names) -> CaseData {
    CaseData {
        name: case.name.clone(),
        fields: case
            .fields
            .iter()
            .map(|f| field_data(f, universe, names))
            .collect(),
    }
}

pub(super) fn type_expr_data(
    expr: &TypeExpr,
    universe: &Universe,
    names: &mut Names,
) -> TypeExprData {
    match expr {
        TypeExpr::Atom(id) => TypeExprData::Atom(names.atom(universe, *id)),
        TypeExpr::Trait(id) => TypeExprData::Trait(names.trait_ref(universe, *id)),
        TypeExpr::Not(id) => TypeExprData::Not(names.trait_ref(universe, *id)),
        TypeExpr::Union(a, b) => TypeExprData::Union(
            Box::new(type_expr_data(a, universe, names)),
            Box::new(type_expr_data(b, universe, names)),
        ),
        TypeExpr::Inter(a, b) => TypeExprData::Inter(
            Box::new(type_expr_data(a, universe, names)),
            Box::new(type_expr_data(b, universe, names)),
        ),
        TypeExpr::Diff(a, b) => TypeExprData::Diff(
            Box::new(type_expr_data(a, universe, names)),
            Box::new(type_expr_data(b, universe, names)),
        ),
        TypeExpr::Tuple(items) => TypeExprData::Tuple(
            items
                .iter()
                .map(|item| type_expr_data(item, universe, names))
                .collect(),
        ),
        TypeExpr::Empty => TypeExprData::Empty,
    }
}

pub(super) fn res_data(res: &Res) -> ResData {
    match res {
        Res::Def(def) => ResData::Def(def.index()),
        Res::Local { bind } => ResData::Local { bind: *bind },
        Res::TraitMethod(method) => ResData::TraitMethod(method.index()),
        Res::Dot(dot) => ResData::Dot(match dot {
            ds_check::DotRes::Free { func, fed_param } => DotResData::Free {
                func: func.index(),
                fed_param: u32::try_from(*fed_param).expect("parameter index fits u32"),
            },
            ds_check::DotRes::Methods { candidates } => DotResData::Methods {
                candidates: candidates.iter().map(|m| m.index()).collect(),
            },
            ds_check::DotRes::Static { candidates } => DotResData::Static {
                candidates: candidates.iter().map(|m| m.index()).collect(),
            },
            ds_check::DotRes::Module { func } => DotResData::Module { func: func.index() },
        }),
        Res::Handler => ResData::Handler,
        Res::Module(module) => ResData::Module(module.index()),
    }
}
