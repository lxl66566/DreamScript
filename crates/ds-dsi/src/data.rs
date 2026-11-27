//! The payload as data: the postcard DTOs, whose derived field order is the wire format, with the
//! def kind and visibility bit mapping.

use std::sync::Arc;

use ds_check::{DefEffects, DefKind, Package, Resolutions, ServedDecls, ServedMethodRow};
use ds_diag::{FileId, Span};
use serde::{Deserialize, Serialize};

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
    /// One interned name table every serve-carried row references atoms
    /// and traits through: the boundary's names-authoritative address
    /// (the rows keep their raw generation ids beside the names as a
    /// checked fast path — see [`AtomRef`]).
    pub names: Vec<String>,
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
    /// The declared visibility tier, as a [`ds_check::Vis`]
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
    /// A module alias an import bound. The alias's module-reference
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
    /// The module-qualified tier behind an import's alias.
    Module {
        /// The linked function's definition index.
        func: u32,
    },
    /// No free function and no registered member answered: the checker
    /// decides between the receiver's fn-typed field and the refusal.
    Field,
}

/// The universe's mint logs and interned sets: the reserve-phase atoms
/// in mint order (every def-named atom), the declared traits in order,
/// the trait memberships the skipped shape arms would have registered,
/// the shape-phase atom mints the skipped impl arms would have performed,
/// The universe's declaration-phase facts, every atom and trait
/// reference through the name table: the def-named atoms with their
/// final shapes, the declared traits, the trait memberships, the
/// shape-phase mints, the interned member sets, the solved span-keyed
/// records, and the shared type-variable counter's settled value.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UniverseLog {
    /// The reserve-phase atoms, in mint order (the predeclared `none`
    /// excluded — `Universe::new` declares it): the merged compile
    /// mints these itself in its own walk order, and each entry's name
    /// resolving onto its recorded id is the identity check that pins
    /// the merged walk to the generation's.
    pub atoms: Vec<AtomEntry>,
    /// The declared traits, in declaration order: the same identity
    /// check over the trait space.
    pub traits: Vec<TraitRef>,
    /// `(trait, atom)` membership records, both sides name-referenced.
    pub memberships: Vec<(TraitRef, AtomRef)>,
    /// The shape-phase atom mints, in mint order: impl type variables,
    /// the structural atoms their head solving minted, and the linked
    /// types' late-minted atoms. Each entry keeps its raw
    /// generation id — the mint log is a contiguous tail of the
    /// generation's atom space, so every entry landing at one
    /// constant shift off that id is the replay's own drift check.
    pub mints: Vec<MintEntry>,
    /// The interned member sets, in intern order: the members by name,
    /// the landing at the row's own index the re-interning check.
    pub sets: Vec<Vec<AtomRef>>,
    /// The solved trait-reference arguments per impl item span, the
    /// values raw intern indices into the sets table.
    pub impl_args: Vec<(Span, Vec<u32>)>,
    /// The solved generic inherent bounds per impl item span, the head
    /// and conditions raw intern indices.
    pub inherent_bounds: Vec<(Span, InherentBoundData)>,
    /// The shared type-variable counter after the ambient's impl arms.
    pub var_counter: u32,
}

/// One shape-phase atom mint with its raw generation id beside it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MintEntry {
    /// The mint's raw generation id.
    pub id: u32,
    /// The mint itself.
    pub data: MintData,
}

/// One shape-phase atom mint.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MintData {
    /// A minted impl type variable, under its unique name.
    Var(AtomRef),
    /// A structural tuple atom over its element atoms.
    Tuple(Vec<AtomRef>),
    /// A structural application atom: constructor atom plus argument
    /// sets (raw intern indices, validated against the sets table).
    Apply {
        /// The constructor atom.
        ctor: AtomRef,
        /// The argument sets, in position order.
        args: Vec<u32>,
    },
    /// A minted linked struct: an import-bound type's def-named
    /// atom, minted past the shape phase — the merged compile resolves
    /// its fields by name once every mint has landed.
    Struct {
        /// The atom's declared name.
        name: AtomRef,
        /// Its field signatures, naming the ambient's def-named atoms
        /// and its own minted applications.
        fields: Vec<FieldData>,
    },
    /// A minted linked opaque struct: a bodiless import-bound
    /// type's reserved atom — identity alone, its shape never set, so
    /// the serve reserves the name and every value use keeps the
    /// local spelling's own refusals.
    Reserved(AtomRef),
}

/// One solved generic inherent bound: the solved head and one condition
/// per declared bound, the solved values raw intern indices.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InherentBoundData {
    /// The solved head pattern.
    pub head: u32,
    /// The kept conditions solved: `(trait, intern index)`.
    pub conditions: Vec<(TraitRef, u32)>,
}

/// One minted atom and its final shape.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AtomEntry {
    /// The atom's declared name with its raw generation id.
    pub name: AtomRef,
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

/// One atom reference a serve-carried row holds: the interned name
/// table's index with the generation's raw id beside it. The name is
/// authoritative — the merged compile resolves it in its own universe —
/// and the raw id is the checked fast path: inside the def-named
/// identity range the merged walk must reproduce the generation's ids,
/// so a name resolving off its recorded id is a walk divergence the
/// validating read refuses loudly; outside that range the row names a
/// minted atom, and the name's landing is the fact.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct AtomRef {
    /// The name-table index of the atom's declared name.
    pub name: u32,
    /// The generation's raw atom id.
    pub id: u32,
}

/// One trait reference, the same discipline over the trait space (every
/// trait is def-declared in walk order, so the reference is always an
/// identity check).
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct TraitRef {
    /// The name-table index of the trait's declared name.
    pub name: u32,
    /// The generation's raw trait id.
    pub id: u32,
}

/// A type expression over name-referenced atoms and traits.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum TypeExprData {
    /// A nominal atom.
    Atom(AtomRef),
    /// A trait as a set.
    Trait(TraitRef),
    /// A negated trait constraint.
    Not(TraitRef),
    /// Union.
    Union(Box<Self>, Box<Self>),
    /// Intersection.
    Inter(Box<Self>, Box<Self>),
    /// Difference.
    Diff(Box<Self>, Box<Self>),
    /// A tuple.
    Tuple(Vec<Self>),
    /// The empty set (a zero-case enum's name).
    Empty,
}

/// The plain and signature declaration tables, keyed by raw def and
/// method id (collection spaces the merged compile continues from the
/// shared namespace); the atom- and trait-naming rows reference through
/// the name table with the raw id beside, the def-table identity check.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeclsData {
    /// Definition to atom identity, the atom name-referenced.
    pub atoms: Vec<(u32, AtomRef)>,
    /// Atom identity back to its definition, the atom name-referenced.
    pub defs_of_atoms: Vec<(AtomRef, u32)>,
    /// Definition to trait identity, the trait name-referenced.
    pub traits: Vec<(u32, TraitRef)>,
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
    /// The extern's bind target, when the signature is a bound
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
    /// Passing mode.
    pub mode: ds_parse::ParamMode,
    /// Written type; absent for inferred lambda parameters.
    pub ty: Option<ds_parse::Type>,
    /// Literal-only default, as written.
    pub default: Option<ds_parse::Expr>,
}

/// One stored instance row: the written and solved halves of a
/// per-branch instance, the artifact's form of [`ServedInstance`] with
/// every solved snapshot a raw intern index.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InstanceRowData {
    /// The trait implemented, name-referenced.
    pub trait_id: TraitRef,
    /// The impl's vocabulary name; `None` is the default vocabulary.
    pub name: Option<String>,
    /// The branch as written, over name-referenced atoms.
    pub head_expr: TypeExprData,
    /// The solved head, a raw intern index into the sets table.
    pub head: u32,
    /// The declaration's written head pre-decomposition, a raw intern
    /// index.
    pub written: u32,
    /// The conditions this branch kept, as written, the traits
    /// name-referenced.
    pub condition_exprs: Vec<(TraitRef, TypeExprData)>,
    /// The kept conditions solved: `(trait, intern index)`.
    pub conditions: Vec<(TraitRef, u32)>,
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
pub(super) const DEF_KINDS: [DefKind; 9] = [
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

pub(super) fn kind_bits(kind: DefKind) -> u8 {
    DEF_KINDS
        .iter()
        .position(|&k| k == kind)
        .and_then(|p| u8::try_from(p).ok())
        .unwrap_or(0)
}

pub(super) fn kind_of(bits: u8) -> Result<DefKind, PrefixDsiError> {
    DEF_KINDS
        .get(bits as usize)
        .copied()
        .ok_or(PrefixDsiError::Decode("unknown def kind"))
}

pub(super) const DEF_VIS: [ds_check::Vis; 3] = [
    ds_check::Vis::Private,
    ds_check::Vis::Pkg,
    ds_check::Vis::Pub,
];

pub(super) fn vis_bits(vis: ds_check::Vis) -> u8 {
    DEF_VIS
        .iter()
        .position(|&v| v == vis)
        .and_then(|p| u8::try_from(p).ok())
        .unwrap_or(0)
}

pub(super) fn vis_of(bits: u8) -> Result<ds_check::Vis, PrefixDsiError> {
    DEF_VIS
        .get(bits as usize)
        .copied()
        .ok_or(PrefixDsiError::Decode("unknown def visibility"))
}
