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
//! Everything the artifact serves stays fresh somewhere by ruling or by
//! reach: the checked body facts stay fresh by G3.5's ruling, and the
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

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use ds_check::{
    Analyzed, AtomKind, Case, Condition, Def, DefEffects, DefId, DefKind, Effects, Field, FnSig,
    IdTable, InstanceId, MethodDecl, MethodId, MethodOwner, Package, PackageBase, ReplayMint, Res,
    Resolutions, ServedDecls, ServedInstance, ServedMethodRow, TraitId, TypeExpr, TypeId, Universe,
};
use ds_diag::{FileId, Span};
use ds_mod::Hash128;
use serde::{Deserialize, Serialize};

/// The artifact header: the 16-byte source hash followed by the
/// payload's 16-byte digest, so a stale artifact and a bit-rotten one
/// are both misses — the same discipline the dsmod format's IR-checksum
/// field carries.
const HEADER: usize = 32;

/// Why a checked-prefix artifact could not be served.
#[derive(Clone, Debug)]
pub enum PrefixDsiError {
    /// The artifact's source hash does not match the source the caller
    /// registered: the artifact predates an edit. Regenerate it.
    SourceDrift,
    /// The artifact addresses different files than the caller
    /// registered: its spans carry the file ids it was generated with,
    /// so only a matching registration can take the direct hit.
    FileMismatch,
    /// The payload failed to decode, or decoded to tables the conversion
    /// refuses (an unknown discriminant): a corrupt artifact, answered
    /// like every other miss.
    Decode(&'static str),
}

impl std::fmt::Display for PrefixDsiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceDrift => f.write_str("the checked-prefix artifact is stale"),
            Self::FileMismatch => {
                f.write_str("the checked-prefix artifact addresses other file ids")
            },
            Self::Decode(why) => write!(f, "the checked-prefix artifact is corrupt: {why}"),
        }
    }
}

impl std::error::Error for PrefixDsiError {}

/// The decoded artifact's native serve, ready for the analyze entries:
/// the namespace parts and per-file resolution tables
/// (`ds_meta::analyze_served`'s inputs), the
/// declaration-side prefix
/// ([`ServedDecls`]' replay input), and the solved effect rows
/// ([`effects_of_resolved_served`](ds_check::effects::effects_of_resolved_served)'s
/// seeds).
pub struct ServedPrefix {
    /// The package's collected namespace.
    pub package: Package,
    /// The served members' name resolutions, one table per served file,
    /// aligned with the leading trees the caller passes.
    pub resolutions: Vec<Resolutions>,
    /// The declaration-side prefix.
    pub decls: ServedDecls,
    /// The served definitions' solved rows, one shared handle the serve
    /// clones by refcount (the rows name definition ids alone).
    pub effect_defs: Arc<[DefEffects]>,
    /// The served methods' solved rows, cloned by refcount the same way.
    pub effect_methods: Arc<[ServedMethodRow]>,
}

/// The artifact's payload: everything one served run needs, all of it
/// plain data (spans as `ds_diag::Span`, written types as ds-parse trees,
/// solved references as raw ids into the mint log's spaces).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrefixDsi {
    /// The ambient member's namespace parts.
    pub package: PackageData,
    /// The members' recorded uses, one sorted-by-span table per file,
    /// in file order.
    pub uses: Vec<Vec<UseData>>,
    /// The universe's mint logs, memberships, and interned sets.
    pub universe: UniverseLog,
    /// The plain and signature declaration tables.
    pub decls: DeclsData,
    /// The instance environment's rows, in id order.
    pub instances: Vec<InstanceRowData>,
    /// The solved definition rows, in definition order.
    pub effects: Vec<DefRowData>,
    /// The solved method rows, in registration order.
    pub method_rows: Vec<MethodRowData>,
}

/// The namespace parts [`Package::add_file`] builds.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackageData {
    /// The collected files, in addition order.
    pub files: Vec<FileId>,
    /// The definitions, in collection order.
    pub defs: Vec<DefData>,
    /// The type namespace: name to definition index.
    pub type_names: Vec<(String, u32)>,
    /// The value namespace: name to definition index.
    pub value_names: Vec<(String, u32)>,
    /// The registered methods, in registration order.
    pub methods: Vec<MethodDeclData>,
    /// The method-name index: name to candidate indexes.
    pub method_index: Vec<(String, Vec<u32>)>,
}

/// One package definition.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DefData {
    /// The declared name.
    pub name: String,
    /// What the declaration defines, as a [`DefKind`] discriminant.
    pub kind: u8,
    /// The declared visibility tier (G4), as a [`ds_check::Vis`]
    /// discriminant.
    pub vis: u8,
    /// The defining name span.
    pub span: Span,
}

/// One registered method.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MethodDeclData {
    /// The method name.
    pub name: String,
    /// The owning trait or inherent impl target.
    pub owner: MethodOwnerData,
    /// The method's name span.
    pub span: Span,
}

/// The owner of a registered method.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MethodOwnerData {
    /// A trait member.
    Trait(u32),
    /// A function of an inherent impl over the named target type.
    Inherent(String),
}

/// One recorded name use.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UseData {
    /// The use's span.
    pub span: Span,
    /// What it resolved to.
    pub res: ResData,
}

/// The resolution of one name use.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ResData {
    /// A package-level definition.
    Def(u32),
    /// A local binder, by its introducing span.
    Local {
        /// The binder's span.
        bind: Span,
    },
    /// The `Trait::f` escape hatch.
    TraitMethod(u32),
    /// A dot-call's method position.
    Dot(DotResData),
    /// The `.err`/`.ok` handler surface form.
    Handler,
    /// A module alias an import bound (G4). The alias's module-reference
    /// table itself never serves — no linked compile persists a prefix —
    /// so a served entry here is a generation-side contract break, kept
    /// round-trip-faithful rather than refused at the format.
    Module(u32),
}

/// The resolution of a dot-call's method position.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum DotResData {
    /// The free-function tier won.
    Free {
        /// The winning function's definition index.
        func: u32,
        /// The parameter the receiver feeds.
        fed_param: u32,
    },
    /// The registered method candidates.
    Methods {
        /// Candidate method indexes, in declaration order.
        candidates: Vec<u32>,
    },
    /// The type-keyed tier behind a type-name receiver.
    Static {
        /// Candidate method indexes.
        candidates: Vec<u32>,
    },
    /// The module-qualified tier behind an import's alias (G4).
    Module {
        /// The linked function's definition index.
        func: u32,
    },
}

/// The universe's mint logs and interned sets: the reserve-phase atoms
/// in mint order (every def-named atom), the declared traits in order,
/// the trait memberships the skipped shape arms would have registered,
/// the shape-phase atom mints the skipped impl arms would have performed,
/// the interned member sets in intern order (the raw [`ds_check::Ty`]
/// indices every solved snapshot in the artifact names), and the shared
/// type-variable counter's settled value — all keyed by index against
/// the spaces the replays reproduce.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UniverseLog {
    /// The reserve-phase atoms, in mint order (the predeclared `none`
    /// excluded — `Universe::new` declares it).
    pub atoms: Vec<AtomEntry>,
    /// The declared traits, in declaration order.
    pub traits: Vec<String>,
    /// `(trait index, atom index)` membership records.
    pub memberships: Vec<(u32, u32)>,
    /// The shape-phase atom mints, in mint order, each with the
    /// generation atom id the prefix's recorded data names it by (the
    /// serve's landing map keys): impl type variables and the structural
    /// atoms their head solving minted.
    pub mints: Vec<(u32, MintData)>,
    /// The interned member sets, in intern order.
    pub sets: Vec<Vec<u32>>,
    /// The shared type-variable counter after the ambient's impl arms.
    pub var_counter: u32,
}

/// One shape-phase atom mint, the artifact's replay form of
/// [`ReplayMint`].
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MintData {
    /// A minted impl type variable, under its unique name.
    Var(String),
    /// A structural tuple atom over its element atoms.
    Tuple(Vec<u32>),
    /// A structural application atom: constructor atom plus argument
    /// sets (raw intern indices).
    Apply {
        /// The constructor atom.
        ctor: u32,
        /// The argument sets, in position order.
        args: Vec<u32>,
    },
    /// A minted linked struct (G4): an import-bound type's def-named
    /// atom, which the def-named log's rank replay could not place —
    /// the mint log's ids replay in order.
    Struct {
        /// The atom's declared name.
        name: String,
        /// Its field signatures, naming the ambient's def-named atoms
        /// (identities) and its own minted applications by generation id.
        fields: Vec<FieldData>,
    },
    /// A minted linked opaque struct (G4): a bodiless import-bound
    /// type's reserved atom — identity alone, its shape never set, so
    /// the replay reserves the name and every value use keeps the
    /// local spelling's own refusals.
    Reserved(String),
}

/// One minted atom and its final shape.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AtomEntry {
    /// The atom's declared name.
    pub name: String,
    /// Its final shape.
    pub shape: AtomShapeData,
}

/// An atom's final shape, the mint-log forms.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum AtomShapeData {
    /// A unit tag.
    Unit,
    /// Identity allocated, shape pending (generic declarations).
    Reserved,
    /// A struct with its solved field signature.
    Struct(Vec<FieldData>),
    /// An enum with its case signature.
    Enum(Vec<CaseData>),
}

/// One field of a solved shape, as a type expression over the minted
/// ids.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FieldData {
    /// The field name; `None` marks a positional slot.
    pub name: Option<String>,
    /// The field's type expression.
    pub ty: TypeExprData,
}

/// One case of a solved enum shape.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CaseData {
    /// The case name.
    pub name: String,
    /// The payload fields.
    pub fields: Vec<FieldData>,
}

/// A type expression over the minted ids.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum TypeExprData {
    /// A nominal atom.
    Atom(u32),
    /// A trait as a set.
    Trait(u32),
    /// A negated trait constraint.
    Not(u32),
    /// Union.
    Union(Box<Self>, Box<Self>),
    /// Intersection.
    Inter(Box<Self>, Box<Self>),
    /// Difference.
    Diff(Box<Self>, Box<Self>),
    /// A tuple.
    Tuple(Vec<Self>),
}

/// The plain and signature declaration tables, keyed by raw id.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeclsData {
    /// Definition to atom identity.
    pub atoms: Vec<(u32, u32)>,
    /// Atom identity back to its definition.
    pub defs_of_atoms: Vec<(u32, u32)>,
    /// Definition to trait identity.
    pub traits: Vec<(u32, u32)>,
    /// Struct shapes (enum-case payload shapes included).
    pub structs: Vec<(u32, StructShapeData)>,
    /// Enum shapes.
    pub enums: Vec<(u32, EnumShapeData)>,
    /// Minted cases of non-generic enums.
    pub cases: Vec<(u32, CaseDeclData)>,
    /// Signatures of named functions and extern functions, by def.
    pub fns: Vec<(u32, FnSigData)>,
    /// Trait member signatures, by method id.
    pub members: Vec<(u32, FnSigData)>,
    /// Inherent impl-function signatures, by method id.
    pub inherent: Vec<(u32, FnSigData)>,
    /// Member names of each trait, by trait def.
    pub trait_members: Vec<(u32, Vec<String>)>,
    /// The defaulted member names of each trait, by trait def.
    pub trait_member_defaults: Vec<(u32, Vec<String>)>,
    /// Generic parameter names of each trait, by trait def.
    pub trait_generics: Vec<(u32, Vec<String>)>,
    /// The declaring impl item's span per served instance.
    pub instance_impls: Vec<(u32, Span)>,
    /// The impl item each served inherent impl function lives in.
    pub inherent_impls: Vec<(u32, Span)>,
}

/// A stored function signature, the artifact's form of [`FnSig`]: the
/// embedded written types and defaults serialize like the AST blob's
/// trees (they are ds-parse subtrees).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FnSigData {
    /// Declared generic parameters, in order.
    pub generics: Vec<GenericParamData>,
    /// Parameters, in order.
    pub params: Vec<ParamSigData>,
    /// The written return type; absent means inferred or unit.
    pub ret: Option<ds_parse::Type>,
    /// The extern's bind target (H5), when the signature is a bound
    /// extern's.
    pub bind: Option<String>,
}

/// One parameter of a stored signature, as written.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ParamSigData {
    /// Parameter name.
    pub name: String,
    /// Span of the name.
    pub span: Span,
    /// Passing mode (C2).
    pub mode: ds_parse::ParamMode,
    /// Written type; absent for inferred lambda parameters.
    pub ty: Option<ds_parse::Type>,
    /// Literal-only default (F7), as written.
    pub default: Option<ds_parse::Expr>,
}

/// One stored instance row: the written and solved halves of a
/// per-branch instance, the artifact's form of [`ServedInstance`] with
/// every solved snapshot a raw intern index.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InstanceRowData {
    /// The trait implemented.
    pub trait_id: u32,
    /// The branch as written, over the minted ids.
    pub head_expr: TypeExprData,
    /// The solved head, a raw intern index.
    pub head: u32,
    /// The declaration's written head pre-decomposition, a raw intern
    /// index.
    pub written: u32,
    /// The conditions this branch kept, as written.
    pub condition_exprs: Vec<(u32, TypeExprData)>,
    /// The kept conditions solved: `(trait, intern index)`.
    pub conditions: Vec<(u32, u32)>,
}

/// A struct's declared shape: generic parameters as written plus the
/// named fields as written.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StructShapeData {
    /// Generic parameters, in order.
    pub params: Vec<GenericParamData>,
    /// Named fields, in declaration order.
    pub fields: Vec<ds_parse::FieldDecl>,
    /// Whether the declaration subtracts copyability.
    pub linear: bool,
}

/// An enum's declared shape.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnumShapeData {
    /// Generic parameters, in order.
    pub params: Vec<GenericParamData>,
    /// Cases, in declaration order.
    pub cases: Vec<EnumCaseData>,
    /// Whether the declaration subtracts copyability.
    pub linear: bool,
}

/// One declared generic parameter, as written.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GenericParamData {
    /// The parameter name.
    pub name: String,
    /// The name's span.
    pub span: Span,
    /// The declared bound, as written.
    pub bound: Option<ds_parse::Type>,
}

/// One enum case, as written.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnumCaseData {
    /// The case name.
    pub name: String,
    /// The name's span.
    pub span: Span,
    /// The written payload.
    pub payload: ds_parse::EnumPayload,
}

/// One minted case of a non-generic enum.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CaseDeclData {
    /// The owning enum's definition index.
    pub enum_def: u32,
    /// The written payload.
    pub payload: ds_parse::EnumPayload,
}

/// One solved definition row.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DefRowData {
    /// The definition's index.
    pub def: u32,
    /// The declared name.
    pub name: String,
    /// What the definition is, as a [`DefKind`] discriminant.
    pub kind: u8,
    /// The defining name span.
    pub span: Span,
    /// The solved row's bits.
    pub row: u8,
    /// The solved capability bits.
    pub caps: u8,
}

/// One solved method row.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MethodRowData {
    /// The method's index.
    pub method: u32,
    /// The solved row's bits.
    pub row: u8,
    /// The solved capability bits.
    pub caps: u8,
}

/// The [`DefKind`] discriminants, in declaration order.
const DEF_KINDS: [DefKind; 9] = [
    DefKind::Fn,
    DefKind::Const,
    DefKind::Var,
    DefKind::Struct,
    DefKind::Enum,
    DefKind::Tag,
    DefKind::Case,
    DefKind::Trait,
    DefKind::TypeAlias,
];

fn kind_bits(kind: DefKind) -> u8 {
    DEF_KINDS
        .iter()
        .position(|&k| k == kind)
        .and_then(|p| u8::try_from(p).ok())
        .unwrap_or(0)
}

fn kind_of(bits: u8) -> Result<DefKind, PrefixDsiError> {
    DEF_KINDS
        .get(bits as usize)
        .copied()
        .ok_or(PrefixDsiError::Decode("unknown def kind"))
}

const DEF_VIS: [ds_check::Vis; 3] = [
    ds_check::Vis::Private,
    ds_check::Vis::Pkg,
    ds_check::Vis::Pub,
];

fn vis_bits(vis: ds_check::Vis) -> u8 {
    DEF_VIS
        .iter()
        .position(|&v| v == vis)
        .and_then(|p| u8::try_from(p).ok())
        .unwrap_or(0)
}

fn vis_of(bits: u8) -> Result<ds_check::Vis, PrefixDsiError> {
    DEF_VIS
        .get(bits as usize)
        .copied()
        .ok_or(PrefixDsiError::Decode("unknown def visibility"))
}

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
    // The import-bound definitions (G4): excluded from the def-named
    // atom log and re-keyed as mints (see `universe_log`), like their
    // exclusion from the defs below.
    let foreign: HashSet<DefId> = package.foreign().keys().copied().collect();

    let mut type_names = Vec::new();
    let mut value_names = Vec::new();
    let defs: Vec<DefData> = package
        .defs()
        .enumerate()
        .filter_map(|(index, def)| {
            let id = DefId::from_raw(u32::try_from(index).expect("def count fits u32"));
            // The import-bound definitions (G4) never ride the artifact:
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

    PrefixDsi {
        package: PackageData {
            files: package.files().collect(),
            defs,
            type_names,
            value_names,
            methods,
            method_index,
        },
        uses,
        universe: universe_log(decls, &foreign),
        decls: decls_data(decls),
        instances: instance_rows_of(decls),
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

/// The universe's mint logs and interned sets from the declaration-phase
/// snapshot: the def-named atoms with final shapes (the import-bound
/// types' atoms excepted — they ride the mints below), the declared
/// traits in order, the trait memberships, the shape-phase mints, the
/// interned sets, and the settled variable counter.
fn universe_log(decls: &ds_check::Decls, foreign: &HashSet<DefId>) -> UniverseLog {
    let universe = &decls.universe;
    let mut log = Vec::new();
    for id in universe.atom_ids() {
        if id == TypeId::NONE || !decls.defs_of_atoms.contains_key(&id) {
            continue;
        }
        // An import-bound type's atom is def-named but minted late (the
        // link pass's products mint after the shape phase), so the
        // def-named log — which the serve replays by rank — cannot place
        // it: it rides the mints below, whose generation ids replay in
        // order.
        if foreign.contains(&decls.defs_of_atoms.get(&id).copied().unwrap()) {
            continue;
        }
        let decl = universe.atom_decl(id).expect("every id has a decl");
        log.push(AtomEntry {
            name: decl.name.clone(),
            shape: match &decl.kind {
                AtomKind::Unit => AtomShapeData::Unit,
                AtomKind::Reserved => AtomShapeData::Reserved,
                AtomKind::Struct(fields) => {
                    AtomShapeData::Struct(fields.iter().map(field_data).collect())
                },
                AtomKind::Enum(cases) => AtomShapeData::Enum(cases.iter().map(case_data).collect()),
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
        "the mint log must carry every def-named atom"
    );
    let mut memberships = Vec::new();
    for (_, &trait_id) in &decls.traits {
        let trait_decl = universe
            .trait_decl(trait_id)
            .expect("registered traits exist");
        for atom in &trait_decl.members {
            memberships.push((trait_id.index(), atom.index()));
        }
    }

    // The declared traits in declaration order (the serve replays them
    // before the memberships apply). The registry iterates by def id;
    // sorting by trait id restores the declaration order.
    let mut trait_rows: Vec<(u32, String)> = decls
        .traits
        .iter()
        .map(|(_, &trait_id)| {
            (
                trait_id.index(),
                universe
                    .trait_decl(trait_id)
                    .expect("registered traits exist")
                    .name
                    .clone(),
            )
        })
        .collect();
    trait_rows.sort_unstable();
    let traits: Vec<String> = trait_rows.into_iter().map(|(_, name)| name).collect();

    // The shape-phase mints: every atom the impl arms minted (variables
    // and structural atoms), in mint order, each with the generation id
    // the recorded data names it by (the merged run's reserve mints can
    // shift the landing, so the serve remaps through these). A linked
    // type's atom rides the same log's tail: its generation id replays
    // in order, and its fields name the ambient's identities and its
    // own later-minted applications, which the serve's remap rewrites.
    let mints: Vec<(u32, MintData)> = universe
        .atom_ids()
        .filter(|id| *id != TypeId::NONE)
        .filter_map(|id| {
            let decl = universe.atom_decl(id).expect("every id has a decl");
            if let Some(&def) = decls.defs_of_atoms.get(&id) {
                if !foreign.contains(&def) {
                    return None;
                }
                let linked = match &decl.kind {
                    // a linked struct, or a linked enum case's struct
                    // payload, rides its shape
                    AtomKind::Struct(fields) => MintData::Struct {
                        name: decl.name.clone(),
                        fields: fields.iter().map(field_data).collect(),
                    },
                    // a linked enum's unit case rides its bare name
                    AtomKind::Unit => MintData::Var(decl.name.clone()),
                    // a linked opaque struct rides its bare reserved
                    // identity
                    AtomKind::Reserved => MintData::Reserved(decl.name.clone()),
                    _ => panic!("a linked type's atom is a struct, a unit case, or reserved"),
                };
                return Some((id.index(), linked));
            }
            let mint = match &decl.kind {
                AtomKind::Unit => MintData::Var(decl.name.clone()),
                AtomKind::Tuple(elements) => {
                    MintData::Tuple(elements.iter().map(|e| e.index()).collect())
                },
                AtomKind::Apply(ctor, args) => MintData::Apply {
                    ctor: ctor.index(),
                    args: args.iter().map(|ty| ty.index()).collect(),
                },
                AtomKind::Reserved
                | AtomKind::Struct(_)
                | AtomKind::Enum(_)
                | AtomKind::FnSig(..) => {
                    panic!("a shape-phase mint is a variable or structural atom")
                },
            };
            Some((id.index(), mint))
        })
        .collect();

    // The interned sets in intern order: the raw Ty indices every solved
    // snapshot in the artifact names.
    let sets: Vec<Vec<u32>> = universe
        .set_log()
        .map(|members| members.iter().map(|atom| atom.index()).collect())
        .collect();

    UniverseLog {
        atoms: log,
        traits,
        memberships,
        mints,
        sets,
        var_counter: decls.var_counter,
    }
}

/// The plain and signature declaration tables from the declaration-phase
/// snapshot, every list in its table's id order.
fn decls_data(decls: &ds_check::Decls) -> DeclsData {
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
fn instance_rows_of(decls: &ds_check::Decls) -> Vec<InstanceRowData> {
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
            instance_row_data(&row)
        })
        .collect()
}

fn generic_param_data(param: &ds_check::GenericParam) -> GenericParamData {
    GenericParamData {
        name: param.name.clone(),
        span: param.span,
        bound: param.bound.clone(),
    }
}

fn generic_param(data: &GenericParamData) -> ds_check::GenericParam {
    ds_check::GenericParam {
        name: data.name.clone(),
        span: data.span,
        bound: data.bound.clone(),
    }
}

fn fn_sig_data(sig: &FnSig) -> FnSigData {
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

fn fn_sig(data: &FnSigData) -> FnSig {
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

fn instance_row_data(row: &ServedInstance) -> InstanceRowData {
    InstanceRowData {
        trait_id: row.trait_id.index(),
        head_expr: type_expr_data(&row.head_expr),
        head: row.head.index(),
        written: row.written.index(),
        condition_exprs: row
            .condition_exprs
            .iter()
            .map(|cond| (cond.trait_id.index(), type_expr_data(&cond.ty)))
            .collect(),
        conditions: row
            .conditions
            .iter()
            .map(|cond| (cond.trait_id.index(), cond.ty.index()))
            .collect(),
    }
}

/// Panics on a solved snapshot's written form naming a trait or a
/// negation: the only expressions whose solving membership growth widens,
/// which the served coherence skip cannot carry (the generator refuses
/// such an ambient until the serve learns to fall back).
fn assert_stable(expr: &TypeExpr) {
    match expr {
        TypeExpr::Trait(_) | TypeExpr::Not(_) => {
            panic!("a served instance head or condition names a trait-relative set")
        },
        TypeExpr::Union(a, b) | TypeExpr::Inter(a, b) | TypeExpr::Diff(a, b) => {
            assert_stable(a);
            assert_stable(b);
        },
        TypeExpr::Atom(_) => {},
        TypeExpr::Tuple(items) => {
            for item in items {
                assert_stable(item);
            }
        },
    }
}

fn field_data(field: &Field) -> FieldData {
    FieldData {
        name: field.name.clone(),
        ty: type_expr_data(&field.ty),
    }
}

fn case_data(case: &Case) -> CaseData {
    CaseData {
        name: case.name.clone(),
        fields: case.fields.iter().map(field_data).collect(),
    }
}

fn type_expr_data(expr: &TypeExpr) -> TypeExprData {
    match expr {
        TypeExpr::Atom(id) => TypeExprData::Atom(id.index()),
        TypeExpr::Trait(id) => TypeExprData::Trait(id.index()),
        TypeExpr::Not(id) => TypeExprData::Not(id.index()),
        TypeExpr::Union(a, b) => {
            TypeExprData::Union(Box::new(type_expr_data(a)), Box::new(type_expr_data(b)))
        },
        TypeExpr::Inter(a, b) => {
            TypeExprData::Inter(Box::new(type_expr_data(a)), Box::new(type_expr_data(b)))
        },
        TypeExpr::Diff(a, b) => {
            TypeExprData::Diff(Box::new(type_expr_data(a)), Box::new(type_expr_data(b)))
        },
        TypeExpr::Tuple(items) => TypeExprData::Tuple(items.iter().map(type_expr_data).collect()),
    }
}

fn res_data(res: &Res) -> ResData {
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
    serve(artifact, tables)
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
/// consults the universe replay — the instance rows and shape-phase
/// mints name intern indices, so they stay per-serve). Every table lands
/// behind one shared handle, so a process that serves the same artifact
/// repeatedly parks this behind a `ds_check::Park` cell and installs the
/// same bases per serve; a one-shot caller converts per serve through
/// [`serve_prefix`] and the conversion drops once with the bundle,
/// exactly what a per-serve rebuild always paid.
pub struct ServedTables {
    /// The converted namespace, installed as the package's shared base
    /// per serve: its rows name definition and method ids alone, which
    /// every serve mints identically, so a member compile's growth
    /// continues the id spaces and its teardown drops a refcount.
    pub package: Arc<PackageBase>,
    /// The converted per-file resolution tables, cloned per serve by
    /// refcount (each table's rows sit behind one shared handle).
    pub resolutions: Vec<Resolutions>,
    /// Definition to atom identity.
    pub atoms: Arc<IdTable<DefId, TypeId>>,
    /// Atom identity back to its definition.
    pub defs_of_atoms: Arc<IdTable<TypeId, DefId>>,
    /// Definition to trait identity.
    pub traits: Arc<IdTable<DefId, TraitId>>,
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
    let mut atoms = IdTable::default();
    for (def, atom) in &artifact.decls.atoms {
        atoms.insert(DefId::from_raw(*def), TypeId::from_raw(*atom));
    }
    let mut defs_of_atoms = IdTable::default();
    for (atom, def) in &artifact.decls.defs_of_atoms {
        defs_of_atoms.insert(TypeId::from_raw(*atom), DefId::from_raw(*def));
    }
    let mut traits = IdTable::default();
    for (def, trait_id) in &artifact.decls.traits {
        traits.insert(DefId::from_raw(*def), TraitId::from_raw(*trait_id));
    }
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
        atoms: Arc::new(atoms),
        defs_of_atoms: Arc::new(defs_of_atoms),
        traits: Arc::new(traits),
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

/// Converts the decoded artifact into the native serve over the shared
/// table bases: the universe replay and the solved rows that name it
/// (instances, mints, memberships) rebuild owned per serve — they name
/// the replayed universe's intern indices or shift with the compiled
/// files' mint landings, so a shared conversion cannot carry them. The
/// namespace, the per-file resolution tables, and the solved effect rows
/// clone from the shared conversion: refcount installs (the package's
/// base tier, the rows' shared handles), never a row rebuild.
fn serve(artifact: &PrefixDsi, tables: &ServedTables) -> Result<ServedPrefix, PrefixDsiError> {
    let package = Package::with_base(Arc::clone(&tables.package));
    let resolutions = tables.resolutions.clone();
    let (universe, tys) = serve_universe(artifact)?;
    let (decls, mints) = serve_tables(artifact, universe, &tys, tables)?;

    let effect_defs = Arc::clone(&tables.effect_defs);
    let effect_methods = Arc::clone(&tables.effect_methods);

    Ok(ServedPrefix {
        package,
        resolutions,
        decls: ServedDecls {
            decls,
            memberships: artifact
                .universe
                .memberships
                .iter()
                .map(|(trait_id, atom)| (TraitId::from_raw(*trait_id), TypeId::from_raw(*atom)))
                .collect(),
            mints,
            files: artifact.package.files.len(),
        },
        effect_defs,
        effect_methods,
    })
}

/// Converts the namespace parts into the shared base the serve installs:
/// every id it names (definitions, methods) is one every serve mints
/// identically, so the base is shareable across a process's serves.
fn package_base(artifact: &PrefixDsi) -> Result<PackageBase, PrefixDsiError> {
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
fn serve_resolutions(artifact: &PrefixDsi) -> Vec<Resolutions> {
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

/// Replays the universe — the atom mint log, the trait declarations, the
/// interned sets — answering the raw [`ds_check::Ty`] values the solved
/// snapshots name.
fn serve_universe(artifact: &PrefixDsi) -> Result<(Universe, Vec<ds_check::Ty>), PrefixDsiError> {
    let corrupt = || PrefixDsiError::Decode("a table references an id out of its space");
    let mut universe = Universe::new();
    for atom in &artifact.universe.atoms {
        match &atom.shape {
            AtomShapeData::Unit => {
                universe.declare_tag(&atom.name).map_err(|_| corrupt())?;
            },
            AtomShapeData::Reserved => {
                universe.declare_struct(&atom.name).map_err(|_| corrupt())?;
            },
            AtomShapeData::Struct(fields) => {
                let id = universe.declare_struct(&atom.name).map_err(|_| corrupt())?;
                let converted: Vec<Field> = fields
                    .iter()
                    .map(|field| Field {
                        name: field.name.clone(),
                        ty: type_expr(&field.ty),
                    })
                    .collect();
                universe
                    .set_struct_fields(id, converted)
                    .map_err(|_| corrupt())?;
            },
            AtomShapeData::Enum(cases) => {
                let id = universe.declare_enum(&atom.name).map_err(|_| corrupt())?;
                let converted: Vec<Case> = cases
                    .iter()
                    .map(|case| Case {
                        name: case.name.clone(),
                        fields: case
                            .fields
                            .iter()
                            .map(|field| Field {
                                name: field.name.clone(),
                                ty: type_expr(&field.ty),
                            })
                            .collect(),
                    })
                    .collect();
                universe
                    .set_enum_cases(id, converted)
                    .map_err(|_| corrupt())?;
            },
        }
    }
    for name in &artifact.universe.traits {
        universe.declare_trait(name).map_err(|_| corrupt())?;
    }
    // The interned sets replay first (nothing else has interned yet, so
    // every replayed handle keeps its index), giving the raw Ty values
    // the solved snapshots and application mints below name.
    let mut tys = Vec::with_capacity(artifact.universe.sets.len());
    for set in &artifact.universe.sets {
        let members: Vec<TypeId> = set.iter().map(|&id| TypeId::from_raw(id)).collect();
        tys.push(universe.replay_set(&members));
    }
    Ok((universe, tys))
}

/// Installs the artifact's shared table bases over the replayed
/// universe, converting the solved rows — the instance environment and
/// the shape-phase mints — per serve. Every raw intern index the rows
/// and mints name is bounds-checked against the replayed `tys` table: a
/// crafted artifact answers a [`PrefixDsiError::Decode`] miss, never an
/// indexing panic.
fn serve_tables(
    artifact: &PrefixDsi,
    universe: Universe,
    tys: &[ds_check::Ty],
    tables: &ServedTables,
) -> Result<(ds_check::Decls, Vec<(TypeId, ReplayMint)>), PrefixDsiError> {
    let corrupt = || PrefixDsiError::Decode("a solved snapshot names an intern index out of range");
    let mut decls = ds_check::Decls {
        universe,
        ..ds_check::Decls::default()
    };
    decls.atoms.install_base(Arc::clone(&tables.atoms));
    decls
        .defs_of_atoms
        .install_base(Arc::clone(&tables.defs_of_atoms));
    decls.traits.install_base(Arc::clone(&tables.traits));
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
    let rows: Result<Vec<ServedInstance>, PrefixDsiError> = artifact
        .instances
        .iter()
        .map(|row| {
            let solved = |set: &u32| tys.get(*set as usize).copied().ok_or_else(corrupt);
            Ok(ServedInstance {
                trait_id: TraitId::from_raw(row.trait_id),
                head_expr: type_expr(&row.head_expr),
                head: solved(&row.head)?,
                written: solved(&row.written)?,
                condition_exprs: row
                    .condition_exprs
                    .iter()
                    .map(|(trait_id, expr)| {
                        Condition::new(TraitId::from_raw(*trait_id), type_expr(expr))
                    })
                    .collect(),
                conditions: row
                    .conditions
                    .iter()
                    .map(|(trait_id, set)| {
                        solved(set).map(|ty| Condition::new(TraitId::from_raw(*trait_id), ty))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect();
    decls.instances.serve(rows?);
    let mints: Result<Vec<(TypeId, ReplayMint)>, PrefixDsiError> = artifact
        .universe
        .mints
        .iter()
        .map(|(gen_id, mint)| {
            let mint = match mint {
                MintData::Var(name) => ReplayMint::Var(name.clone()),
                MintData::Tuple(elements) => {
                    ReplayMint::Tuple(elements.iter().map(|&id| TypeId::from_raw(id)).collect())
                },
                MintData::Apply { ctor, args } => ReplayMint::Apply(
                    TypeId::from_raw(*ctor),
                    args.iter()
                        .map(|set| tys.get(*set as usize).copied().ok_or_else(corrupt))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                MintData::Struct { name, fields } => ReplayMint::Struct(
                    name.clone(),
                    fields
                        .iter()
                        .map(|field| Field {
                            name: field.name.clone(),
                            ty: type_expr(&field.ty),
                        })
                        .collect(),
                ),
                MintData::Reserved(name) => ReplayMint::Reserved(name.clone()),
            };
            Ok((TypeId::from_raw(*gen_id), mint))
        })
        .collect();
    decls.var_counter = artifact.universe.var_counter;
    decls.served_defs = u32::try_from(artifact.package.defs.len()).expect("def count fits u32");
    decls.served_methods =
        u32::try_from(artifact.package.methods.len()).expect("method count fits u32");
    Ok((decls, mints?))
}

fn type_expr(data: &TypeExprData) -> TypeExpr {
    match data {
        TypeExprData::Atom(id) => TypeExpr::Atom(TypeId::from_raw(*id)),
        TypeExprData::Trait(id) => TypeExpr::Trait(TraitId::from_raw(*id)),
        TypeExprData::Not(id) => TypeExpr::Not(TraitId::from_raw(*id)),
        TypeExprData::Union(a, b) => {
            TypeExpr::Union(Box::new(type_expr(a)), Box::new(type_expr(b)))
        },
        TypeExprData::Inter(a, b) => {
            TypeExpr::Inter(Box::new(type_expr(a)), Box::new(type_expr(b)))
        },
        TypeExprData::Diff(a, b) => TypeExpr::Diff(Box::new(type_expr(a)), Box::new(type_expr(b))),
        TypeExprData::Tuple(items) => TypeExpr::Tuple(items.iter().map(type_expr).collect()),
    }
}

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
        AtomKind, CaseDeclData, DeclsData, EnumCaseData, EnumShapeData, FnSigData,
        GenericParamData, InstanceId, InstanceRowData, StructShapeData, Universe,
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
    /// this view pins the plain tables alone.
    #[must_use]
    pub fn plain_tables(run: &Analyzed) -> Vec<u8> {
        let decls = &run.decls;
        let view = DeclsData {
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
            fns: Vec::new(),
            members: Vec::new(),
            inherent: Vec::new(),
            trait_members: Vec::new(),
            trait_member_defaults: Vec::new(),
            trait_generics: Vec::new(),
            instance_impls: Vec::new(),
            inherent_impls: Vec::new(),
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
        let rows: Vec<InstanceRowData> = (0..u32::try_from(run.decls.instances.len())
            .expect("instance count fits u32"))
            .map(|index| {
                super::instance_row_data(
                    &run.decls
                        .instances
                        .row(InstanceId::from_raw(index))
                        .expect("every id has a row"),
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
    use ds_diag::FileId;

    use super::{
        AtomEntry, AtomShapeData, DeclsData, InstanceRowData, MintData, PackageData, PrefixDsi,
        PrefixDsiError, TypeExprData, UniverseLog, artifact_bytes, decode_dsi,
    };

    /// A one-file, otherwise-empty artifact with `instances`/`mints` the
    /// caller overrides: everything else replays cleanly, so the crafted
    /// rows are what the serve must refuse.
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
            package: PackageData {
                files: vec![FileId::new(0)],
                defs: Vec::new(),
                type_names: Vec::new(),
                value_names: Vec::new(),
                methods: Vec::new(),
                method_index: Vec::new(),
            },
            uses: vec![Vec::new()],
            universe: UniverseLog {
                atoms: vec![AtomEntry {
                    name: "probe".to_owned(),
                    shape: AtomShapeData::Unit,
                }],
                traits: Vec::new(),
                memberships: Vec::new(),
                mints: Vec::new(),
                sets: vec![vec![0]],
                var_counter: 0,
            },
            decls: empty_decls,
            instances: Vec::new(),
            effects: Vec::new(),
            method_rows: Vec::new(),
        }
    }

    fn decode(artifact: &PrefixDsi) -> Result<super::ServedPrefix, PrefixDsiError> {
        let hash = ds_mod::Hash128::compute(b"probe source");
        let bytes = artifact_bytes(hash, artifact);
        decode_dsi(&bytes, hash, &[FileId::new(0)])
    }

    #[test]
    fn a_minimal_artifact_serves() {
        assert!(decode(&artifact()).is_ok());
    }

    /// An instance row naming an intern index past the replayed set table
    /// is a corrupt artifact answered as a decode miss, never an indexing
    /// panic (the header digest gates bit rot, not crafted bytes).
    #[test]
    fn an_instance_set_index_out_of_range_is_a_decode_miss() {
        let mut artifact = artifact();
        artifact.instances = vec![InstanceRowData {
            trait_id: 0,
            head_expr: TypeExprData::Atom(0),
            head: 9,
            written: 0,
            condition_exprs: Vec::new(),
            conditions: Vec::new(),
        }];
        let Err(PrefixDsiError::Decode(why)) = decode(&artifact) else {
            panic!("a crafted head index must be a decode miss");
        };
        assert!(why.contains("intern index"), "{why}");
    }

    /// An application mint whose argument names a set the universe never
    /// interned is the same corruption class.
    #[test]
    fn a_mint_set_index_out_of_range_is_a_decode_miss() {
        let mut artifact = artifact();
        artifact.universe.mints = vec![(0, MintData::Apply {
            ctor: 0,
            args: vec![7],
        })];
        let Err(PrefixDsiError::Decode(why)) = decode(&artifact) else {
            panic!("a crafted mint index must be a decode miss");
        };
        assert!(why.contains("intern index"), "{why}");
    }
}
