//! The checked-prefix artifact (`.dsi`): one package's pre-resolved,
//! pre-declared, pre-solved front-end half, beside the pre-expanded
//! trees an AST blob serves. The artifact is a postcard payload behind
//! a 16-byte source-hash header — a stale header or a foreign file id
//! is a cache miss, and every caller falls back to the fresh compute,
//! so the artifact is a pure cache, never correctness-load-bearing.
//!
//! Two consumers share one format: the checked-in stdlib ambient
//! (ds-stdlib embeds its artifact and serves it on every cold compile)
//! and a user package's own compile, which persists its prefix into
//! `$DS_CACHE` keyed on the source hash and the build identity, a miss
//! falling back to the fresh compute exactly as the ambient does. The
//! payload carries, for the package's members alone:
//!
//! * the resolve half — the package namespace parts and the per-file resolution tables — so a
//!   served run collects and resolves only the fresh files, continuing the package's [`DefId`] and
//!   [`MethodId`] spaces exactly as a fresh run mints them (the mint-determinism pin);
//! * the declaration half's plain tables — the universe's atom and trait mint logs (replayed
//!   through the declare APIs) plus the atoms/structs/enums/cases tables and the trait memberships
//!   — so the reserve and shape passes skip the served defs; the shape-phase mint log carries each
//!   mint's generation atom id beside it, because the replay lands past the fresh files' reserve
//!   mints and a post-replay remap rewrites every recorded datum naming a minted atom onto its
//!   landing id (a def-named reference replays first, at its own id, and never moves);
//! * the hard declaration tables — every stored signature (`fns`, trait `members`, `inherent`; the
//!   embedded written types serialize like an AST blob's trees), the trait member/generics tables,
//!   the instance environment's rows with their solved heads and conditions (raw
//!   [`Ty`](ds_check::Ty) indices valid because the interned-set log replays in intern order and
//!   the mint log pins the atom space), and the impl-item ties — so a served run declares only its
//!   fresh files' items and the closure coherence pass probes only the fresh rows (the artifact's
//!   clean generation proved the served prefix coherent against a byte-identical universe, and a
//!   head or condition naming a trait-relative set refuses the generation);
//! * the effects half's solved rows — the definitions' and registered methods' fixpoint rows — so a
//!   served run walks only the fresh trees and seeds the served subgraph as fixed inputs.
//!
//! Everything the artifact serves stays fresh somewhere by scope or by
//! reach: the checked body facts stay fresh by the format contract, and the
//! solved span-keyed records (`trait_impl_args`, `inherent_impl_bounds`,
//! `impl_fn_rets`) rebuild over the served trees (ds-check's
//! declaration-pass rebuild; `impl_fn_rets` in the body pass the fresh
//! trees always run).
//!
//! The drift gates pin an artifact three ways: a byte gate regenerates
//! from the current source and compares postcard bytes; an equivalence
//! gate runs the served analyze and the fresh analyze over the same
//! program and deep-compares the bundles through the [`gates`] views;
//! and the consuming path's own suite exercises the serve end to end.

use ds_check::{AtomKind, InstanceId, Universe};

mod data;
mod decode;
mod encode;
mod serve;

pub use data::*;
pub use decode::*;
pub use encode::*;
pub use serve::*;

/// The artifact header: the 16-byte source hash followed by the
/// payload's 16-byte digest, so a stale artifact and a bit-rotten one
/// are both misses — the same discipline the dsmod format's IR-checksum
/// field carries.
const HEADER: usize = 32;

/// The drift gates' comparable views: one run's facts rendered to
/// plain data, so a served run and a fresh run deep-compare without
/// depending on identity between universes (the raw solved-handle
/// comparisons are exact where the two runs mint identical sequences —
/// the pin the gates enforce).
/// The package-surface conversion: one served prefix's own members as
/// the `.dsi` document `ds info` prints and the LSP hover spells.
pub mod surface;

pub mod gates {
    use ds_check::Analyzed;
    use ds_diag::Span;
    use serde::Serialize;

    use super::{
        AtomKind, CaseDeclData, EnumCaseData, EnumShapeData, FnSigData, GenericParamData,
        InstanceId, InstanceRowData, StructShapeData, Universe,
    };

    /// The atom mint sequence of one finished universe, name plus shape
    /// per index.
    #[must_use]
    pub fn mint_facts(universe: &Universe) -> Vec<(String, String)> {
        universe
            .atom_ids()
            .enumerate()
            .map(|(index, id)| {
                let decl = universe.atom_decl(id).expect("every id has a decl");
                let shape = match &decl.kind {
                    AtomKind::Unit => "unit".to_owned(),
                    AtomKind::Reserved => "reserved".to_owned(),
                    AtomKind::Struct(fields) => {
                        let parts: Vec<String> = fields
                            .iter()
                            .map(|field| {
                                format!("{}={:?}", field.name.clone().unwrap_or_default(), field.ty)
                            })
                            .collect();
                        format!("struct({})", parts.join(","))
                    },
                    AtomKind::Enum(cases) => {
                        let parts: Vec<String> =
                            cases.iter().map(|case| case.name.clone()).collect();
                        format!("enum({})", parts.join(","))
                    },
                    AtomKind::Tuple(_) | AtomKind::Apply(..) | AtomKind::FnSig(..) => {
                        format!("{:?}", decl.kind)
                    },
                };
                (format!("{index}:{}", decl.name), shape)
            })
            .collect()
    }

    /// The plain declaration tables of one run, postcard bytes (the
    /// embedded written types carry no equality, so the bytes pin
    /// them). The signature tables ride in [`signature_tables`], so
    /// this view pins the plain tables alone; the id-keyed rows render
    /// raw pairs, the two runs minting identical sequences being the
    /// pin this view enforces.
    #[must_use]
    pub fn plain_tables(run: &Analyzed) -> Vec<u8> {
        #[derive(Serialize)]
        struct View {
            atoms: Vec<(u32, u32)>,
            defs_of_atoms: Vec<(u32, u32)>,
            traits: Vec<(u32, u32)>,
            structs: Vec<(u32, StructShapeData)>,
            enums: Vec<(u32, EnumShapeData)>,
            cases: Vec<(u32, CaseDeclData)>,
        }
        let decls = &run.decls;
        let view = View {
            atoms: decls
                .atoms
                .iter()
                .map(|(def, atom)| (def.index(), atom.index()))
                .collect(),
            defs_of_atoms: decls
                .defs_of_atoms
                .iter()
                .map(|(atom, def)| (atom.index(), def.index()))
                .collect(),
            traits: decls
                .traits
                .iter()
                .map(|(def, trait_id)| (def.index(), trait_id.index()))
                .collect(),
            structs: decls
                .structs
                .iter()
                .map(|(def, shape)| {
                    (def.index(), StructShapeData {
                        params: shape.params.iter().map(generic_param).collect(),
                        fields: shape.fields.clone(),
                        linear: shape.linear,
                    })
                })
                .collect(),
            enums: decls
                .enums
                .iter()
                .map(|(def, shape)| {
                    (def.index(), EnumShapeData {
                        params: shape.params.iter().map(generic_param).collect(),
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
                .collect(),
            cases: decls
                .cases
                .iter()
                .map(|(def, case)| {
                    (def.index(), CaseDeclData {
                        enum_def: case.enum_def.index(),
                        payload: case.payload.clone(),
                    })
                })
                .collect(),
        };
        postcard::to_allocvec(&view).expect("the plain tables serialize")
    }

    /// The signature and trait-member tables of one run, in id order,
    /// postcard bytes.
    #[must_use]
    pub fn signature_tables(run: &Analyzed) -> Vec<u8> {
        #[derive(Serialize)]
        struct View {
            fns: Vec<(u32, FnSigData)>,
            members: Vec<(u32, FnSigData)>,
            inherent: Vec<(u32, FnSigData)>,
            trait_members: Vec<(u32, Vec<String>)>,
            trait_member_defaults: Vec<(u32, Vec<String>)>,
            trait_generics: Vec<(u32, Vec<String>)>,
            instance_impls: Vec<(u32, Span)>,
            inherent_impls: Vec<(u32, Span)>,
        }
        let decls = &run.decls;
        let view = View {
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
        };
        postcard::to_allocvec(&view).expect("the signature tables serialize")
    }

    /// The top-level binding rows of one run: one postcard blob per def,
    /// in id order — the initializers carry no equality, so the bytes pin
    /// them. The rows arrive shared from the served base where the serve
    /// installs one, so this view is what pins the base's content to the
    /// fresh walk's clones.
    #[must_use]
    pub fn const_rows(run: &Analyzed) -> Vec<(u32, Vec<u8>)> {
        run.decls
            .consts
            .iter()
            .map(|(def, local)| {
                (
                    def.index(),
                    postcard::to_allocvec(local.as_ref()).expect("the binding row serializes"),
                )
            })
            .collect()
    }

    /// Every stored instance row of one run, serialized in id order (the
    /// written halves are the artifact's data form, so postcard bytes are
    /// the comparable shape).
    #[must_use]
    pub fn instance_rows(run: &Analyzed) -> Vec<u8> {
        let mut names = super::Names::default();
        let rows: Vec<InstanceRowData> = (0..u32::try_from(run.decls.instances.len())
            .expect("instance count fits u32"))
            .map(|index| {
                super::instance_row_data(
                    &run.decls
                        .instances
                        .row(InstanceId::from_raw(index))
                        .expect("every id has a row"),
                    &run.decls.universe,
                    &mut names,
                )
            })
            .collect();
        postcard::to_allocvec(&rows).expect("the instance rows serialize")
    }

    /// The trait registry of one run: index, name, member atoms.
    #[must_use]
    pub fn trait_facts(run: &Analyzed) -> Vec<(u32, String, Vec<u32>)> {
        let universe = &run.decls.universe;
        run.decls
            .traits
            .iter()
            .map(|(_, &trait_id)| {
                let decl = universe
                    .trait_decl(trait_id)
                    .expect("registered traits exist");
                (
                    trait_id.index(),
                    decl.name.clone(),
                    decl.members.iter().map(|atom| atom.index()).collect(),
                )
            })
            .collect()
    }

    /// Every checked expression fact rendered through its run's own
    /// universe: one line per span.
    #[must_use]
    pub fn rendered_exprs(run: &Analyzed) -> Vec<String> {
        let universe = &run.decls.universe;
        let mut facts: Vec<_> = run
            .checked
            .expr_types
            .iter()
            .map(|(span, val)| {
                format!(
                    "{}/{}/{} {} diverges={} poisoned={}",
                    span.file.as_u32(),
                    span.start,
                    span.end,
                    universe.render(val.ty),
                    val.diverges,
                    val.poisoned
                )
            })
            .collect();
        facts.sort_unstable();
        facts
    }

    /// One generic parameter's data form.
    fn generic_param(param: &ds_check::GenericParam) -> GenericParamData {
        super::generic_param_data(param)
    }

    /// One stored signature's data form.
    fn fn_sig_data(sig: &ds_check::FnSig) -> FnSigData {
        super::fn_sig_data(sig)
    }
}

#[cfg(test)]
mod tests {
    use ds_check::{Decls, ServedDrift};
    use ds_diag::FileId;

    use super::{
        AtomRef, DeclsData, InstanceRowData, MintData, MintEntry, PackageData, PrefixDsi,
        PrefixDsiError, ServedRows, TraitRef, TypeExprData, UniverseLog, artifact_bytes,
        decode_dsi, resolve_rows,
    };

    /// A one-file, otherwise-empty artifact with `instances`/`mints` the
    /// caller overrides: everything else resolves cleanly, so the crafted
    /// rows are what the resolve step must refuse.
    fn artifact() -> PrefixDsi {
        let empty_decls = DeclsData {
            atoms: Vec::new(),
            defs_of_atoms: Vec::new(),
            traits: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            cases: Vec::new(),
            fns: Vec::new(),
            members: Vec::new(),
            inherent: Vec::new(),
            trait_members: Vec::new(),
            trait_member_defaults: Vec::new(),
            trait_generics: Vec::new(),
            instance_impls: Vec::new(),
            inherent_impls: Vec::new(),
        };
        PrefixDsi {
            names: vec!["probe".to_owned()],
            package: PackageData {
                files: vec![FileId::new(0)],
                defs: Vec::new(),
                type_names: Vec::new(),
                value_names: Vec::new(),
                case_names: Vec::new(),
                methods: Vec::new(),
                method_index: Vec::new(),
            },
            docs: Vec::new(),
            uses: vec![Vec::new()],
            universe: UniverseLog {
                atoms: Vec::new(),
                traits: Vec::new(),
                memberships: Vec::new(),
                mints: Vec::new(),
                sets: vec![vec![AtomRef { name: 0, id: 0 }]],
                impl_args: Vec::new(),
                inherent_bounds: Vec::new(),
                var_counter: 0,
            },
            decls: empty_decls,
            instances: Vec::new(),
            effects: Vec::new(),
            method_rows: Vec::new(),
        }
    }

    /// The crafted artifact's resolve step over a fresh declaration set:
    /// the rows resolve against the universe the caller's own walk
    /// minted, so a one-atom walk (the `probe` tag below) is the
    /// universe the resolve step must accept or refuse.
    fn resolve(artifact: &PrefixDsi) -> Result<(), ServedDrift> {
        let mut decls = Decls::default();
        let probe = decls
            .universe
            .declare_tag("probe")
            .expect("the probe tag mints");
        decls
            .universe
            .declare_trait("tells")
            .expect("the probe trait mints");
        let rows = ServedRows {
            names: std::sync::Arc::from(artifact.names.clone()),
            identity_limit: 2 + u32::try_from(artifact.universe.atoms.len()).expect("fits"),
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
        let result = resolve_rows(&mut decls, &rows);
        let _ = probe;
        result
    }

    fn decode(artifact: &PrefixDsi) -> Result<super::ServedPrefix, PrefixDsiError> {
        let hash = ds_mod::Hash128::compute(b"probe source");
        let bytes = artifact_bytes(hash, artifact);
        decode_dsi(&bytes, hash, &[FileId::new(0)])
    }

    #[test]
    fn a_minimal_artifact_serves() {
        let artifact = artifact();
        assert!(decode(&artifact).is_ok());
        resolve(&artifact).expect("the minimal rows resolve over a one-atom walk");
    }

    /// An instance row naming an intern index past the set table is a
    /// corrupt artifact the resolve step refuses loudly, never an
    /// indexing panic (the header digest gates bit rot, not crafted
    /// bytes).
    #[test]
    fn an_instance_set_index_out_of_range_is_a_drift_refusal() {
        let mut artifact = artifact();
        artifact.names = vec!["probe".to_owned(), "tells".to_owned()];
        artifact.instances = vec![InstanceRowData {
            // `tells` at trait id 0 resolves over the fixture's own
            // walk, so the crafted head index is the refusal under test
            trait_id: TraitRef { name: 1, id: 0 },
            name: None,
            head_expr: TypeExprData::Atom(AtomRef { name: 0, id: 100 }),
            head: 9,
            written: 0,
            condition_exprs: Vec::new(),
            conditions: Vec::new(),
        }];
        let Err(drift) = resolve(&artifact) else {
            panic!("a crafted head index must be a drift refusal");
        };
        assert!(drift.detail.contains("intern set"), "{}", drift.detail);
    }

    /// An application mint whose argument names a set the universe never
    /// interned is the same corruption class.
    #[test]
    fn a_mint_set_index_out_of_range_is_a_drift_refusal() {
        let mut artifact = artifact();
        artifact.universe.mints = vec![MintEntry {
            id: 0,
            data: MintData::Apply {
                // a raw id past the identity range names a mint, so the
                // constructor resolves and the crafted set index is the
                // refusal under test
                ctor: AtomRef { name: 0, id: 100 },
                args: vec![7],
            },
        }];
        let Err(drift) = resolve(&artifact) else {
            panic!("a crafted mint index must be a drift refusal");
        };
        assert!(drift.detail.contains("intern set"), "{}", drift.detail);
    }

    /// A row naming an atom the walk never minted is the drift refusal
    /// proper: the name does not resolve, and the serve never serves a
    /// wrong fact.
    #[test]
    fn an_unresolved_atom_name_is_a_drift_refusal() {
        let mut artifact = artifact();
        artifact.names = vec!["elsewhere".to_owned()];
        artifact.universe.traits = vec![TraitRef { name: 0, id: 0 }];
        let Err(drift) = resolve(&artifact) else {
            panic!("an unresolved name must be a drift refusal");
        };
        assert!(
            drift.detail.contains("does not resolve"),
            "{}",
            drift.detail
        );
    }
}
