//! The decode half: the header and digest check, the payload decode, and the declaration tables'
//! conversion to shared bases.

use std::sync::Arc;

use ds_check::{
    DefEffects, DefId, FnSig, IdTable, InstanceId, MethodId, PackageBase, Resolutions,
    ServedMethodRow,
};
use ds_diag::{FileId, Span};
use ds_mod::Hash128;

use super::*;

/// The artifact's decoded payload: the source-hash header must match the
/// source text the caller registered, the payload must pass its digest,
/// and the bytes must decode — the format half of [`decode_dsi`] alone,
/// so a process that serves the same artifact repeatedly decodes once
/// and re-`serve_prefix`es per call. Any miss is an error the caller
/// answers with the fresh compute.
///
/// # Errors
///
/// [`PrefixDsiError`]: a stale header or a corrupt payload — both cache
/// misses, never source problems.
pub fn decode_prefix(bytes: &[u8], source_hash: Hash128) -> Result<PrefixDsi, PrefixDsiError> {
    let Some((header, payload)) = bytes.split_first_chunk::<HEADER>() else {
        return Err(PrefixDsiError::SourceDrift);
    };
    let Some((source, digest)) = header.split_first_chunk::<16>() else {
        return Err(PrefixDsiError::SourceDrift);
    };
    if source != source_hash.as_bytes() {
        return Err(PrefixDsiError::SourceDrift);
    }
    if digest != Hash128::compute(payload).as_bytes() {
        return Err(PrefixDsiError::Decode(
            "the payload does not match its digest",
        ));
    }
    postcard::from_bytes(payload).map_err(|_| PrefixDsiError::Decode("postcard refused"))
}

/// The verified artifact's native serve over its bytes: the source-hash
/// header must match the source text the caller registered, the payload
/// must decode, and the decoded tables must address exactly the file ids
/// the caller registered (the registration the spans carry). Any miss is
/// an error the caller answers with the fresh compute.
///
/// # Errors
///
/// [`PrefixDsiError`]: a stale header, a foreign registration, or a
/// corrupt payload — all of them cache misses, never source problems.
pub fn decode_dsi(
    bytes: &[u8],
    source_hash: Hash128,
    files: &[FileId],
) -> Result<ServedPrefix, PrefixDsiError> {
    serve_prefix(&decode_prefix(bytes, source_hash)?, files)
}

/// The artifact's plain and signature declaration tables converted once,
/// beside the converted namespace, per-file resolution tables, and
/// solved effect rows: the shareable half of the serve (none of it
/// consults the merged universe). The atom, def-atom, and trait identity
/// tables are absent on purpose — the merged compile mints those spaces
/// in its own walk and the rows validate them — while the name-keyed
/// rows (shapes, memberships, mints, sets, instances, records) resolve
/// per serve against that walk, so they stay outside the shared
/// conversion. Every table lands behind one shared handle, so a process
/// that serves the same artifact repeatedly parks this behind a
/// `ds_check::Park` cell and installs the same bases per serve; a
/// one-shot caller converts per serve through [`serve_prefix`] and the
/// conversion drops once with the bundle, exactly what a per-serve
/// rebuild always paid.
pub struct ServedTables {
    /// The interned name table the rows reference.
    pub names: Arc<[String]>,
    /// The converted namespace, installed as the package's shared base
    /// per serve: its rows name definition and method ids alone, which
    /// every serve mints identically, so a member compile's growth
    /// continues the id spaces and its teardown drops a refcount.
    pub package: Arc<PackageBase>,
    /// The converted per-file resolution tables, cloned per serve by
    /// refcount (each table's rows sit behind one shared handle).
    pub resolutions: Vec<Resolutions>,
    /// Struct shapes (enum-case payload shapes included).
    pub structs: Arc<IdTable<DefId, ds_check::decls::StructShape>>,
    /// Enum shapes.
    pub enums: Arc<IdTable<DefId, ds_check::decls::EnumShape>>,
    /// Minted cases of non-generic enums.
    pub cases: Arc<IdTable<DefId, ds_check::decls::CaseDecl>>,
    /// Signatures of named functions and const-arrow function bindings.
    pub fns: Arc<IdTable<DefId, Arc<FnSig>>>,
    /// Trait member signatures, by method id.
    pub members: Arc<IdTable<MethodId, Arc<FnSig>>>,
    /// Inherent impl-function signatures, by method id.
    pub inherent: Arc<IdTable<MethodId, Arc<FnSig>>>,
    /// Member names of each trait, by trait def.
    pub trait_members: Arc<IdTable<DefId, Vec<String>>>,
    /// The defaulted member names of each trait, by trait def.
    pub trait_member_defaults: Arc<IdTable<DefId, Vec<String>>>,
    /// Generic parameter names of each trait, by trait def.
    pub trait_generics: Arc<IdTable<DefId, Vec<String>>>,
    /// The declaring impl item's span per served instance.
    pub instance_impls: Arc<IdTable<InstanceId, Span>>,
    /// The impl item each served inherent impl function lives in.
    pub inherent_impls: Arc<IdTable<MethodId, Span>>,
    /// The solved definition rows, one shared handle cloned per serve by
    /// refcount: the rows name definition ids alone, which every serve
    /// mints identically, so the fill's name rows park with the
    /// conversion instead of re-collecting per serve.
    pub effect_defs: Arc<[DefEffects]>,
    /// The solved method rows, cloned per serve the same way: method ids
    /// alone, the same share the namespace base rides.
    pub effect_methods: Arc<[ServedMethodRow]>,
}

/// Converts the artifact's namespace and declaration tables into shared
/// bases.
///
/// # Errors
///
/// [`PrefixDsiError`]: tables the conversion refuses (an unknown
/// discriminant) — a cache miss, never a source problem.
pub fn convert_tables(artifact: &PrefixDsi) -> Result<ServedTables, PrefixDsiError> {
    let package = Arc::new(package_base(artifact)?);
    let resolutions = serve_resolutions(artifact);
    let names: Arc<[String]> = Arc::from(artifact.names.clone());
    let mut structs = IdTable::default();
    for (def, shape) in &artifact.decls.structs {
        structs.insert(DefId::from_raw(*def), ds_check::decls::StructShape {
            params: shape.params.iter().map(generic_param).collect(),
            fields: shape.fields.clone(),
            linear: shape.linear,
        });
    }
    let mut enums = IdTable::default();
    for (def, shape) in &artifact.decls.enums {
        enums.insert(DefId::from_raw(*def), ds_check::decls::EnumShape {
            params: shape.params.iter().map(generic_param).collect(),
            cases: shape
                .cases
                .iter()
                .map(|case| (case.name.clone(), case.span, case.payload.clone()))
                .collect(),
            linear: shape.linear,
        });
    }
    let mut cases = IdTable::default();
    for (def, case) in &artifact.decls.cases {
        cases.insert(DefId::from_raw(*def), ds_check::decls::CaseDecl {
            enum_def: DefId::from_raw(case.enum_def),
            payload: case.payload.clone(),
        });
    }
    let mut fns = IdTable::default();
    for (def, sig) in &artifact.decls.fns {
        fns.insert(DefId::from_raw(*def), Arc::new(fn_sig(sig)));
    }
    let mut members = IdTable::default();
    for (method, sig) in &artifact.decls.members {
        members.insert(MethodId::from_raw(*method), Arc::new(fn_sig(sig)));
    }
    let mut inherent = IdTable::default();
    for (method, sig) in &artifact.decls.inherent {
        inherent.insert(MethodId::from_raw(*method), Arc::new(fn_sig(sig)));
    }
    let mut trait_members = IdTable::default();
    for (def, names) in &artifact.decls.trait_members {
        trait_members.insert(DefId::from_raw(*def), names.clone());
    }
    let mut trait_member_defaults = IdTable::default();
    for (def, names) in &artifact.decls.trait_member_defaults {
        trait_member_defaults.insert(DefId::from_raw(*def), names.clone());
    }
    let mut trait_generics = IdTable::default();
    for (def, names) in &artifact.decls.trait_generics {
        trait_generics.insert(DefId::from_raw(*def), names.clone());
    }
    let mut instance_impls = IdTable::default();
    for (instance, span) in &artifact.decls.instance_impls {
        instance_impls.insert(InstanceId::from_raw(*instance), *span);
    }
    let mut inherent_impls = IdTable::default();
    for (method, span) in &artifact.decls.inherent_impls {
        inherent_impls.insert(MethodId::from_raw(*method), *span);
    }
    let effect_defs = artifact
        .effects
        .iter()
        .map(|row| {
            Ok(DefEffects {
                def: DefId::from_raw(row.def),
                name: row.name.clone(),
                kind: kind_of(row.kind)?,
                span: row.span,
                row: ds_check::EffectRow::from_bits(row.row),
                caps: row.caps,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let effect_methods = artifact
        .method_rows
        .iter()
        .map(|row| ServedMethodRow {
            method: MethodId::from_raw(row.method),
            row: ds_check::EffectRow::from_bits(row.row),
            caps: row.caps,
        })
        .collect::<Vec<_>>();
    Ok(ServedTables {
        package,
        resolutions,
        names,
        structs: Arc::new(structs),
        enums: Arc::new(enums),
        cases: Arc::new(cases),
        fns: Arc::new(fns),
        members: Arc::new(members),
        inherent: Arc::new(inherent),
        trait_members: Arc::new(trait_members),
        trait_member_defaults: Arc::new(trait_member_defaults),
        trait_generics: Arc::new(trait_generics),
        instance_impls: Arc::new(instance_impls),
        inherent_impls: Arc::new(inherent_impls),
        effect_defs: effect_defs.into(),
        effect_methods: effect_methods.into(),
    })
}
