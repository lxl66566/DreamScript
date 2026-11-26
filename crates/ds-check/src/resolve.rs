//! Package-level name resolution over the parsed surface (G3, F3, F9).
//!
//! A [`Package`] collects the declarations of its files into TWO
//! namespaces (G3, F9): a type namespace (structs, enums, enum cases,
//! tags, aliases, traits) and a value namespace (functions, const/var
//! bindings). Files in
//! one package share both, there are zero intra-package imports, and
//! top-level definitions are order-independent — collection inserts every
//! declaration before any use is resolved, so intra-file and cross-file
//! definition cycles are legal by construction (resolution is name lookup,
//! not dependency ordering; the "whole-package fixpoint" of the record is
//! exactly this two-pass shape).
//!
//! # Namespace split (F9)
//!
//! Constructor positions resolve against the TYPE namespace only: brace
//! construction (`user { ... }`), bare tag references, pattern paths, and
//! generic-application constructors (`list[page]()`, whose head and type
//! arguments are type uses). A call-form name resolves against the VALUE
//! namespace (a function call), else the TYPE namespace (a constructor
//! call — tag or payload construction); present in BOTH it is an
//! ambiguity error (DS2014), never a priority search. A bare (non-call)
//! use resolves VALUE-first, then TYPE — a bare tag is a
//! value-producing constructor, while a struct/enum/trait name reached at
//! a bare use resolves so the checker can word its "type used as a value"
//! error. `struct map` and `fn map` therefore coexist, each resolving in
//! its own positions.
//!
//! # Method tier (F3)
//!
//! UFCS resolution is fixed: a dot-call `x.f(args)` feeds the receiver to
//! the first parameter of a FREE function named `f` if one exists (the
//! dot position is a function position — the VALUE namespace only), else
//! the registered method candidates (trait members, plus inherent impl
//! fns keyed by their target type — different owners may share one
//! method name); `Trait::f(x)` qualifies directly. Trait methods are
//! REGISTERED here only — selection among candidates is the checker's
//! type-directed work, and F3's true-ambiguity error therefore lives
//! there too, never as a priority search here. The fed parameter index is
//! recorded (always 0 today — capability parameters have no syntax yet,
//! so the first parameter IS the first data parameter; the checker
//! enforces the exclusion once they do).
//!
//! # Local scoping (F2)
//!
//! Params, lets, lambda parameters, match-arm binders, and for-patterns
//! bind into lexical scopes. Same-scope redefinition is an error (both
//! spans named); nested scopes may shadow outer bindings and package
//! names. An initializer is resolved BEFORE its own binders bind
//! (`const x = x` uses the outer `x`); the defined name of a const-arrow
//! function is in scope inside its own body via the package table
//! (recursion). Generic parameters bind in a separate type-variable scope
//! stack for type positions; a trait member's signatures additionally bind
//! `Self` as the self type (F2, record 6.1). Or-pattern alternatives rebind
//! identical names into one arm scope without error; whether alternatives
//! must bind the SAME set is the checker's (F4).
//!
//! # Positions that do not resolve here
//!
//! Bare field accesses (`x.f` without a call) are struct-owned and
//! type-directed — the checker resolves them. `.err(h)`/`.ok(h)` are
//! decided surface handler forms [section 10 item 2], not names, and are
//! recorded as [`Res::Handler`]. Imports (G4) declare no namespace names:
//! the resolver validates an `@`-sigil import's selected symbols against
//! the package's `pub` surface (a missing or non-`pub` selection
//! refuses), while every other import stays declarative for the link
//! walk, its condition a source-internal spelling until the target model
//! lands. Multi-segment paths beyond `Trait::f` are module paths, still
//! open. Enum case tails (`net_err::timeout`) resolve their head;
//! case-name existence is the checker's.
//!
//! # Example
//!
//! ```
//! use ds_check::{Package, Res, Resolutions};
//! use ds_diag::Files;
//! use ds_parse::parse;
//!
//! # fn main() -> Result<(), Vec<ds_diag::Diagnostic>> {
//! let mut files = Files::new();
//! let file = files.add_source(
//!     "m.ds",
//!     "tag timeout\nfn wait(t: timeout) -> timeout { t }\n",
//! );
//! let ast = parse(&files, file)?;
//!
//! let mut package = Package::new();
//! assert!(package.add_file(&ast).is_empty());
//! let (_, diags) = package.resolve(&ast);
//! assert!(diags.is_empty());
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use ds_diag::{Diagnostic, ErrorCode, Span, Suggestion};
use ds_parse::{
    Ast, EnumPayload, Expr, FnDecl, Ident, Item, ItemKind, Param, Path, Pattern, Stmt, StmtKind,
    StructMember, TraitMember, Type, TypeParam,
};
use rustc_hash::FxHashMap;

use crate::import::{ForeignEntry, ForeignKind, ImportGraph, ImportSurface, MemberSurface};

/// Identity of one package-level definition inside its [`Package`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct DefId(u32);

impl DefId {
    /// Builds an id from a raw table index (serialization, tests).
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index inside its package; also its declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// What a top-level declaration defines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DefKind {
    /// A named function or extern function.
    Fn,
    /// A `const` binding (const-arrow functions included — they are
    /// bindings whose value is a function (F2)).
    Const,
    /// A `var` binding.
    Var,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A unit tag (B1).
    Tag,
    /// A case minted by an enum block (B3): a member of the tag
    /// namespace like a tag, constructed per its payload.
    Case,
    /// A trait (B6).
    Trait,
    /// A type alias (F7).
    TypeAlias,
}

impl DefKind {
    /// Whether the kind declares into the type namespace (F9); the rest
    /// declare into the value namespace.
    #[must_use]
    pub const fn in_type_namespace(self) -> bool {
        matches!(
            self,
            Self::Struct | Self::Enum | Self::Tag | Self::Case | Self::Trait | Self::TypeAlias
        )
    }

    /// Whether the kind mints an atom a bare pattern can test: traits and
    /// aliases are sets, not constructors.
    #[must_use]
    pub const fn is_constructor(self) -> bool {
        matches!(self, Self::Struct | Self::Enum | Self::Tag | Self::Case)
    }

    /// The diagnostic word for the kind ("struct", "function", ...).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fn => "function",
            Self::Const | Self::Var => "binding",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Tag => "tag",
            Self::Case => "enum case",
            Self::Trait => "trait",
            Self::TypeAlias => "type alias",
        }
    }
}

/// The visibility tier of one package-level definition (G4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vis {
    /// Private: the defining file alone sees the name.
    Private,
    /// `pub(pkg)`: every file of the package sees it; the `.dsi` omits it.
    Pkg,
    /// `pub`: dependents see it through imports; the `.dsi` entry.
    Pub,
}

impl Vis {
    /// The tier a declaration's modifier spells: absent is private (G4).
    #[must_use]
    pub const fn of(vis: Option<ds_parse::Vis>) -> Self {
        match vis {
            None => Self::Private,
            Some(ds_parse::Vis::Pub(_)) => Self::Pub,
            Some(ds_parse::Vis::PubPkg(_)) => Self::Pkg,
        }
    }

    /// Whether the tier opens the name to dependents (the `.dsi` gate).
    #[must_use]
    pub const fn exported(self) -> bool {
        matches!(self, Self::Pub)
    }
}

/// One package-level definition: name, kind, visibility, and defining
/// name span.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Def {
    /// The declared name.
    pub name: String,
    /// What the declaration defines.
    pub kind: DefKind,
    /// The declared visibility tier (G4).
    pub vis: Vis,
    /// Span of the defining name occurrence; carries the file.
    pub span: Span,
}

/// Identity of one registered method inside its [`Package`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct MethodId(u32);

impl MethodId {
    /// Builds an id from a raw table index (serialization, tests).
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index inside its package; also its declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Identity of one registered module reference inside its [`Package`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ModuleId(u32);

impl ModuleId {
    /// Builds an id from a raw table index (serialization, tests).
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index inside its package.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Which declaration a registered method belongs to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MethodOwner {
    /// A trait member function; trait impls do not re-register — the
    /// trait's own signature already provides the candidate name.
    Trait(DefId),
    /// A function of an inherent impl (`impl t { fn ... }`), owned by the
    /// impl's target TYPE. The owner travels as its name, not its DefId:
    /// registration runs at collection, which may precede the target's
    /// declaring file (G3), and type names are package-unique (the first
    /// def wins), so the name IS the owner identity. The checker resolves
    /// the name to the owner's atom at call sites and selects by
    /// (owner, name) (F3).
    Inherent(String),
}

/// One registered method: a trait member or an inherent impl function.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MethodDecl {
    /// The method name as called after the dot.
    pub name: String,
    /// The owning trait or inherent impl.
    pub owner: MethodOwner,
    /// Span of the method's name; carries the file.
    pub span: Span,
}

/// What one name use resolved to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Res {
    /// A package-level definition.
    Def(DefId),
    /// A local binder (param, let, lambda, match arm, for pattern, or type
    /// variable); `bind` is the binder's span.
    Local {
        /// Span of the introducing binder.
        bind: Span,
    },
    /// The `Trait::f` escape hatch (F3): the named trait's member.
    TraitMethod(MethodId),
    /// The callee name of a UFCS dot-call (F3).
    Dot(DotRes),
    /// The `.err(h)`/`.ok(h)` handler surface form [section 10 item 2];
    /// no name sits behind it.
    Handler,
    /// A module alias an import binds (`import p as c`): the receiver of
    /// a module-qualified call, never a value. The module ref lives in
    /// the package's table the id indexes.
    Module(ModuleId),
}

/// The resolution of a dot-call's method position (F3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DotRes {
    /// The free-function tier won: the receiver feeds parameter
    /// `fed_param` (0-based). Always 0 today: capability parameters have
    /// no surface yet, so the first parameter is the first data parameter;
    /// the checker enforces the dot-position exclusion once they do.
    Free {
        /// The winning free function (or const binding of one).
        func: DefId,
        /// Index of the parameter the receiver feeds.
        fed_param: usize,
    },
    /// No free function; the registered method candidates, in declaration
    /// order. Selection is the checker's type-directed work.
    Methods {
        /// Candidate method ids: trait members and inherent impl fns.
        candidates: Vec<MethodId>,
    },
    /// The receiver names a type (the receiverless-member ruling): the
    /// candidates are that type's own inherent impl members, and the
    /// call is type-keyed — resolved at check time, dispatched with no
    /// instance word. A value shadowing the type name (a local or a
    /// value-namespace def) keeps the value path above, and the
    /// free-function tier never applies behind a type name.
    Static {
        /// Candidate method ids: the type's inherent impl members.
        candidates: Vec<MethodId>,
    },
    /// The receiver is a module alias an import bound: the named member
    /// of the linked package, called with the written arguments alone —
    /// no receiver word stages.
    Module {
        /// The linked member's function the call enters.
        func: DefId,
    },
}

/// The side table a resolution pass produces: use span to resolution.
/// Checker passes consume it; nothing in it mutates the package. The rows
/// sit behind one shared handle, so a table clones by refcount — the
/// checked-ambient serve parks its converted tables once per process and
/// hands every member compile a clone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Resolutions {
    /// One entry per name use, sorted by span so per-expression lookups
    /// binary search: `Span` carries no ordering, so the key is its raw
    /// file and offset triple.
    uses: Arc<[(Span, Res)]>,
}

impl Default for Resolutions {
    fn default() -> Self {
        Self {
            uses: Arc::from(Vec::new()),
        }
    }
}

impl Resolutions {
    /// Rebuilds one file's resolution table from its recorded uses (the
    /// checked-ambient artifact's served prefix): the caller's uses must be
    /// already sorted by span, the pass's own output order.
    #[must_use]
    pub fn from_uses(uses: Vec<(Span, Res)>) -> Self {
        debug_assert!(
            uses.windows(2)
                .all(|pair| span_key(pair[0].0) < span_key(pair[1].0)),
            "served uses must be sorted by span"
        );
        Self {
            uses: Arc::from(uses),
        }
    }

    /// Every recorded use with its resolution, in span order (the
    /// artifact generator's extraction view).
    #[must_use]
    pub fn uses(&self) -> &[(Span, Res)] {
        &self.uses
    }

    /// The resolution recorded at a use span, if any.
    #[must_use]
    pub fn get(&self, span: Span) -> Option<&Res> {
        let index = self
            .uses
            .binary_search_by_key(&span_key(span), |(use_span, _)| span_key(*use_span))
            .ok()?;
        Some(&self.uses[index].1)
    }

    /// Number of recorded uses.
    #[must_use]
    pub fn len(&self) -> usize {
        self.uses.len()
    }

    /// Whether no use was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.uses.is_empty()
    }

    /// The resolution whose recorded use span encloses `at`, if any.
    ///
    /// A point query (the LSP's go-to-definition): `at` names one position
    /// (its start; the end is ignored), and the answer is the covering use
    /// with the latest start. Use spans are identifier tokens and never
    /// nest; the coverage is inclusive at the end boundary (a use ending
    /// exactly at the position still answers, so a cursor right after a
    /// name finds it), and when one use ends at the position where
    /// another begins, the later-starting name wins. The table is sorted
    /// by start, so the walk back from the last use starting at or before
    /// the position meets the winner first; a walk that never covers
    /// answers `None`, and the walk is linear in the file's use count in
    /// the worst case (a position on whitespace between names).
    #[must_use]
    pub fn enclosing(&self, at: Span) -> Option<&Res> {
        self.covering(at).map(|(_, res)| res)
    }

    /// The covering use's span together with its resolution: the same
    /// lookup [`Self::enclosing`] answers, answering the span too — the
    /// tooling queries that edit or highlight the name at the position
    /// need the occurrence, not only its target.
    #[must_use]
    pub fn covering(&self, at: Span) -> Option<(Span, &Res)> {
        let mut index = self
            .uses
            .partition_point(|(span, _)| {
                (span.file.as_u32(), span.start) <= (at.file.as_u32(), at.start)
            })
            .checked_sub(1)?;
        loop {
            let (span, res) = &self.uses[index];
            if span.file == at.file && span.end >= at.start {
                return Some((*span, res));
            }
            index = index.checked_sub(1)?;
        }
    }
}

/// The sort key of one use span: file, then byte offsets.
fn span_key(span: Span) -> (u32, u32, u32) {
    (span.file.as_u32(), span.start, span.end)
}

/// Why one linked signature's type spelling refuses at the link walk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unspellable {
    /// The head names no type this compile or its surface can spell.
    Unknown,
    /// The head names a linked type whose registration a collision
    /// refused.
    Unbound,
}

/// The reverse edge of the resolution side table: for one resolved
/// target, every use that resolved to it. Package grain — a definition's
/// uses span every file, including the first-registered member's own.
/// Targets key on identity, never on name: two same-named locals of
/// different scopes answer disjoint sets, and a local shadowing a
/// package name never picks up the package definition's uses.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct UseIndex {
    /// Uses of each package-level definition, keyed by raw index.
    defs: FxHashMap<u32, Vec<Span>>,
    /// Uses of each local binder, keyed by the binder's own span.
    locals: FxHashMap<Span, Vec<Span>>,
    /// Qualified uses of each registered method (the `Trait::f` form),
    /// keyed by raw index.
    methods: FxHashMap<u32, Vec<Span>>,
}

impl UseIndex {
    /// Builds the index over every file's resolution table, the same
    /// compute the tables themselves belong to: uses are fully
    /// materialized there, so the index costs one walk instead of a
    /// per-request scan of the whole package.
    ///
    /// A dot-call's free-function tier names the definition the bare
    /// form does, so its use records under that definition; the method
    /// tier stays type-directed until the checker selects, so a
    /// methods dot-call names no single target and records nothing; the
    /// static tier carries no per-use target either, and the handler
    /// forms name nothing.
    #[must_use]
    pub fn build(resolutions: &[Resolutions]) -> Self {
        let mut index = Self::default();
        for table in resolutions {
            for &(use_span, ref res) in &table.uses[..] {
                match res {
                    Res::Def(id) => {
                        index.defs.entry(id.index()).or_default().push(use_span);
                    },
                    Res::Local { bind } => {
                        index.locals.entry(*bind).or_default().push(use_span);
                    },
                    Res::TraitMethod(id) => {
                        index.methods.entry(id.index()).or_default().push(use_span);
                    },
                    Res::Dot(DotRes::Free { func, .. } | DotRes::Module { func }) => {
                        index.defs.entry(func.index()).or_default().push(use_span);
                    },
                    Res::Dot(DotRes::Methods { .. } | DotRes::Static { .. })
                    | Res::Handler
                    | Res::Module(_) => {},
                }
            }
        }
        index
    }

    /// Every use of one package-level definition, in file then source
    /// order.
    #[must_use]
    pub fn def_uses(&self, id: DefId) -> &[Span] {
        self.defs.get(&id.index()).map_or(&[], Vec::as_slice)
    }

    /// Every use of one local binder.
    #[must_use]
    pub fn local_uses(&self, bind: Span) -> &[Span] {
        self.locals.get(&bind).map_or(&[], Vec::as_slice)
    }

    /// Every qualified use of one registered method.
    #[must_use]
    pub fn method_uses(&self, id: MethodId) -> &[Span] {
        self.methods.get(&id.index()).map_or(&[], Vec::as_slice)
    }
}

/// A package: the declarations of its files in the type and value
/// namespaces (G3, F9), plus the method-tier registries (F3). Collection
/// (`add_file`) is order-independent; use resolution (`resolve`) runs
/// against the complete tables, which is what makes intra-package
/// definition cycles legal.
///
/// The collected tables are two-tier, the same shape [`IdTable`](crate::IdTable)
/// carries: an optional shared base — a checked-ambient serve's prefix,
/// parked per process and installed by refcount per serve — plus the
/// per-compile growth the compiled files' own collection builds. The base
/// is a leading run of the def and method id spaces (a served prefix
/// precedes the fresh files), a growth insert never shadows a base row
/// (a duplicate against the base reports at collection like any other),
/// and the name tables answer growth-first with the base below.
#[derive(Clone, Default, Debug)]
pub struct Package {
    base: Option<Arc<PackageBase>>,
    files: Vec<ds_diag::FileId>,
    defs: Vec<Def>,
    type_names: FxHashMap<String, DefId>,
    value_names: FxHashMap<String, DefId>,
    methods: Vec<MethodDecl>,
    method_index: FxHashMap<String, Vec<MethodId>>,
    /// The import-bound functions, keyed by their def (G4): the
    /// signatures the declaration pass registers and the link facts the
    /// call lowering addresses. Foreign defs sit outside both name
    /// tables — only the importing file's bindings reach them.
    foreign: FxHashMap<DefId, ForeignEntry>,
    /// Each file's import bindings (G4): the selection names and module
    /// aliases the file's use sites resolve through.
    imports: FxHashMap<ds_diag::FileId, FileImports>,
    /// The module references the files' import aliases bound, indexed by
    /// [`ModuleId`].
    module_refs: Vec<ModuleRef>,
    /// Dedupes a foreign def per linked function: two imports of one
    /// package share the def the first registered.
    foreign_defs: FxHashMap<(u32, String), DefId>,
    /// Dedupes a foreign const def per linked binding: a literal const
    /// has no module table index, so its name is the fact's identity.
    foreign_const_defs: FxHashMap<(u32, String), DefId>,
    /// The registered foreign type defs, keyed by name alone (G4): one
    /// linked type owns a name across the compile — the universe mints
    /// by name, so a second linked type of the same name refuses its
    /// selection instead of mis-shaping the first's spellings. A local
    /// declaration of the name never appears here (the registration gate
    /// refuses it), keeping the shadowing discipline the value namespace
    /// already follows.
    foreign_type_defs: FxHashMap<String, DefId>,
    /// The selections a `pub import` row bound (G4's re-export): the
    /// import-bound defs that join this package's export surface, with
    /// the bound name (an alias's spelling included) and the row's own
    /// span — the definition site a dependent's diagnostics name, since
    /// the origin's declaring span lives in the origin package's file
    /// space. A def two pub rows alias twice rides twice, one export per
    /// bound name.
    reexports: Vec<(DefId, String, Span)>,
    /// The linked modules' tag-table rows the link pass collected (the
    /// shared tag mapping): a union-typed word crossing a module
    /// boundary carries the producing module's tag ids as immediates,
    /// so the consuming compile's lowering seeds its own table with
    /// these rows — identity from the linked module's artifact, never a
    /// positional guess. Duplicate rows (two imports of one package)
    /// ride twice; the seed is idempotent per name and id.
    linked_tags: Vec<(String, u8)>,
    /// The linked members' trait-impl facts the link pass collected
    /// (G4): the owner-keyed member links a consuming trait call site
    /// dispatches through, deduplicated per linked impl — two imports
    /// of one package share the row the first registered.
    linked_impls: Vec<crate::import::ImplFact>,
    /// The tag declarations a file re-declares beside an earlier file's
    /// same-label tag or case (F9.5): label to the shadowing def. The
    /// first declaration per label owns the type namespace's entry; a
    /// shadowing file's own uses resolve through here first, its atom
    /// a distinct declaration the directed probe reads beside the
    /// first's.
    file_tags: FxHashMap<ds_diag::FileId, FxHashMap<String, DefId>>,
    /// The file currently collecting (`add_file`): the gate that tells
    /// a same-file redeclaration (refused, both namespaces) from a
    /// cross-file tag shadow (F9.5, kept distinct).
    collecting_file: Option<ds_diag::FileId>,
}

/// The collected namespace tables a checked-ambient serve parks per
/// process: exactly the tables [`Package::add_file`] builds, with the
/// name tables as name/id maps (first-definition wins, the collection
/// rule). Shared behind one handle, so a serve installs it as a base and
/// every member compile's teardown drops a refcount, never the rows.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct PackageBase {
    /// The collected files, in addition order.
    files: Vec<ds_diag::FileId>,
    /// The definitions, in collection order.
    defs: Vec<Def>,
    /// The type namespace: name to definition.
    type_names: FxHashMap<String, DefId>,
    /// The value namespace: name to definition.
    value_names: FxHashMap<String, DefId>,
    /// The registered methods, in registration order.
    methods: Vec<MethodDecl>,
    /// The method-name index: name to candidates.
    method_index: FxHashMap<String, Vec<MethodId>>,
}

impl PackageBase {
    /// Builds a base from its collected parts (the checked-ambient
    /// artifact's converted namespace): the parts are exactly the tables
    /// [`Package::add_file`](Package::add_file) builds, with the name
    /// tables as name/id pairs.
    #[must_use]
    pub fn from_parts(
        files: Vec<ds_diag::FileId>,
        defs: Vec<Def>,
        type_names: Vec<(String, DefId)>,
        value_names: Vec<(String, DefId)>,
        methods: Vec<MethodDecl>,
        method_index: Vec<(String, Vec<MethodId>)>,
    ) -> Self {
        Self {
            files,
            defs,
            type_names: type_names.into_iter().collect(),
            value_names: value_names.into_iter().collect(),
            methods,
            method_index: method_index.into_iter().collect(),
        }
    }
}

/// Two packages are equal when their effective tables answer the same:
/// the base and growth tiers flatten to the same def, method, and file
/// runs and the same name resolutions (the drift gates' served-vs-fresh
/// comparison — one side carries the served base, the other a flat
/// walk).
impl PartialEq for Package {
    fn eq(&self, other: &Self) -> bool {
        self.defs().eq(other.defs())
            && self.methods().eq(other.methods())
            && self.files().eq(other.files())
            && self.namespace_eq(other, Self::type_names_of)
            && self.namespace_eq(other, Self::value_names_of)
            && self.method_index_eq(other)
    }
}

impl Eq for Package {}

/// A namespace tier pair: the package's growth tier plus the base tier
/// every growth insert leaves unshadowed.
type NamespaceTiers<'a> = (
    &'a FxHashMap<String, DefId>,
    Option<&'a FxHashMap<String, DefId>>,
);

impl Package {
    /// The type namespace's growth tier and base tier.
    fn type_names_of(pkg: &Self) -> NamespaceTiers<'_> {
        (
            &pkg.type_names,
            pkg.base.as_deref().map(|base| &base.type_names),
        )
    }

    /// The value namespace's growth tier and base tier.
    fn value_names_of(pkg: &Self) -> NamespaceTiers<'_> {
        (
            &pkg.value_names,
            pkg.base.as_deref().map(|base| &base.value_names),
        )
    }

    /// Whether both packages' selected namespace answers the same name/id
    /// pairs: the key counts match and every key of `self`'s tiers
    /// resolves the same through `other`'s (the tiers are disjoint — a
    /// growth insert never shadows a base row).
    fn namespace_eq(&self, other: &Self, of: fn(&Self) -> NamespaceTiers<'_>) -> bool {
        let count = |pkg: &Self| {
            let (growth, base) = of(pkg);
            growth.len() + base.map_or(0, FxHashMap::len)
        };
        if count(self) != count(other) {
            return false;
        }
        let (growth, base) = of(self);
        growth
            .iter()
            .chain(base.into_iter().flat_map(FxHashMap::iter))
            .all(|(name, id)| {
                let (other_growth, other_base) = of(other);
                other_growth
                    .get(name)
                    .copied()
                    .or_else(|| other_base.and_then(|names| names.get(name).copied()))
                    == Some(*id)
            })
    }

    /// Whether both packages' method-name indexes answer the same
    /// candidates per name over the same method count.
    fn method_index_eq(&self, other: &Self) -> bool {
        if self.method_len() != other.method_len() {
            return false;
        }
        let mut names = self.method_index.keys().chain(
            self.base
                .as_ref()
                .into_iter()
                .flat_map(|base| base.method_index.keys()),
        );
        names.all(|name| {
            self.method_candidates(name).collect::<Vec<_>>()
                == other.method_candidates(name).collect::<Vec<_>>()
        })
    }

    /// Number of registered methods across both tiers.
    fn method_len(&self) -> usize {
        self.methods.len() + self.base.as_ref().map_or(0, |base| base.methods.len())
    }

    /// The linked modules' tag-table rows the link pass collected (the
    /// shared tag mapping): the lowering seeds its own table with them
    /// before any body mints, so a union-typed word crossing a module
    /// boundary carries ids both modules agree on.
    #[must_use]
    pub fn linked_tag_rows(&self) -> &[(String, u8)] {
        &self.linked_tags
    }

    /// The linked members' trait-impl facts, deduplicated per linked
    /// impl: the declaration pass declares each row's instance and the
    /// lowering reads the member links a solved dispatch addresses.
    #[must_use]
    pub fn linked_impl_facts(&self) -> &[crate::import::ImplFact] {
        &self.linked_impls
    }

    /// One linked type name's def: the link table's row, which the
    /// local `type_names` walk never holds — a name resolves through
    /// both before a local declaration's own row wins.
    #[must_use]
    pub fn foreign_type_def(&self, name: &str) -> Option<DefId> {
        self.foreign_type_defs.get(name).copied()
    }
}

/// One file's import bindings (G4): the names its import declarations
/// selected (under their own or their per-symbol alias spelling), the
/// module aliases its `as` clauses bound, and the import declarations a
/// resolved graph served through a linked surface — the spans whose
/// selections the member surface validated, so the resolver's
/// merged-namespace check does not retry them. Imported names enter the
/// file's namespace alone — other files of the package see nothing.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FileImports {
    names: FxHashMap<String, (DefId, Span)>,
    modules: FxHashMap<String, (ModuleId, Span)>,
    linked: Vec<Span>,
}

/// One module reference an import's `as` clause bound: the alias's name
/// and the surface its qualified member calls resolve through.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModuleRef {
    /// The alias spelling, for diagnostics.
    alias: String,
    /// The alias's surface.
    surface: ModuleSurface,
}

/// The surface a module alias resolves through.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ModuleSurface {
    /// An `@std` alias: the merged member's own declarations, resolved
    /// through the package namespace with the member's file filter.
    Merged { member: ds_diag::FileId },
    /// A linked package member: its exported surface, with the def each
    /// bound function registered.
    Linked {
        surface: MemberSurface,
        defs: FxHashMap<String, DefId>,
    },
}

/// Resolves one package (G3): collects every file's declarations into one
/// namespace, then resolves every file's name uses against the complete
/// tables — collection is order-independent and resolution runs last,
/// which is what makes intra-package definition cycles legal. The
/// resolutions align with `asts` positionally. The diagnostics are the
/// collection class (duplicate definitions) and the resolution class; the
/// typed half ([`crate::type_of::analyze_resolved`]) adds the declaration
/// and body classes.
#[must_use]
pub fn resolve_package<T>(asts: &[T]) -> (Package, Vec<Resolutions>, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    let mut package = Package::new();
    let mut diags = Vec::new();
    for ast in asts {
        diags.extend(package.add_file(ast.borrow()));
    }
    let mut resolutions = Vec::with_capacity(asts.len());
    for ast in asts {
        let (uses, mut file_diags) = package.resolve(ast.borrow());
        resolutions.push(uses);
        diags.append(&mut file_diags);
    }
    (package, resolutions, diags)
}

/// [`resolve_package`] with import surfaces linked (G4): the same
/// collection and resolution, plus the link pass between them — one
/// pass over every file's import declarations that validates each
/// selection against its linked member's surface and binds what passes
/// (the selected names and module aliases the file's use sites resolve
/// through). A graph that names no import keeps the pass inert.
#[must_use]
pub fn resolve_package_linked<T>(
    asts: &[T],
    imports: &ImportGraph,
) -> (Package, Vec<Resolutions>, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    let mut package = Package::new();
    let mut diags = Vec::new();
    for ast in asts {
        diags.extend(package.add_file(ast.borrow()));
    }
    diags.extend(
        package.link_imports(
            &asts
                .iter()
                .map(std::borrow::Borrow::borrow)
                .collect::<Vec<_>>(),
            imports,
        ),
    );
    let mut resolutions = Vec::with_capacity(asts.len());
    for ast in asts {
        let (uses, mut file_diags) = package.resolve(ast.borrow());
        resolutions.push(uses);
        diags.append(&mut file_diags);
    }
    (package, resolutions, diags)
}

impl ModuleRef {
    /// The alias spelling the import bound.
    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Whether the alias references a linked package member: its
    /// qualified consts bind through the linked surface's literal
    /// facts, while a merged alias keeps the call-only reading.
    #[must_use]
    pub fn linked(&self) -> bool {
        matches!(self.surface, ModuleSurface::Linked { .. })
    }
}

impl Package {
    /// An empty package.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A package over one served prefix's collected tables: the base is
    /// the parked namespace (one [`Park`](crate::Park) cell's rows, an
    /// [`Arc`] refcount) a checked-ambient serve hands every member
    /// compile, and the growth the compile's own files build. A package
    /// installed this way continues collecting
    /// and resolving user files like any other — the base only replaces
    /// the collection walk over the served member's own items.
    #[must_use]
    pub fn with_base(base: Arc<PackageBase>) -> Self {
        Self {
            base: Some(base),
            ..Self::default()
        }
    }

    /// Collects one parsed file's declarations into the namespace,
    /// returning duplicate-definition diagnostics as they are met (both
    /// spans named; the first definition is the secondary label). Imports
    /// contribute nothing yet (G4, M7).
    pub fn add_file(&mut self, ast: &Ast) -> Vec<Diagnostic> {
        self.files.push(ast.file);
        self.collecting_file = Some(ast.file);
        let mut diags = Vec::new();
        for block in &ast.blocks {
            for item in &block.items {
                self.collect_item(item, &mut diags);
            }
        }
        self.collecting_file = None;
        diags
    }

    /// Resolves one parsed file's name uses against the complete package
    /// tables, returning the side table and the diagnostics in walk order.
    #[must_use]
    pub fn resolve(&self, ast: &Ast) -> (Resolutions, Vec<Diagnostic>) {
        let mut resolver = FileResolver {
            package: self,
            uses: Vec::new(),
            diags: Vec::new(),
            file: ast.file,
            imports: self.imports.get(&ast.file),
            scopes: vec![FxHashMap::default()],
            type_scopes: vec![FxHashMap::default()],
        };
        for block in &ast.blocks {
            for item in &block.items {
                resolver.item(item);
            }
        }
        let mut uses = resolver.uses;
        uses.sort_by_key(|(span, _)| span_key(*span));
        debug_assert!(uses.windows(2).all(|pair| pair[0].0 != pair[1].0));
        (
            Resolutions {
                uses: Arc::from(uses),
            },
            resolver.diags,
        )
    }

    /// The link pass (G4): after every file collected and before any
    /// use resolves, two passes over the import declarations the graph
    /// serves — every linked member's exported definitions register as
    /// package defs first (so no selection's validation depends on
    /// import order inside the file), then each selection validates
    /// against its linked member's surface (a missing name, a non-`pub`
    /// tier, or a shape this round cannot bind refusing at the import),
    /// what passes binds into the importing file's namespace, and an
    /// `as` alias binds a module reference the file's qualified calls
    /// resolve through. Running after collection keeps the pass
    /// order-independent (G3): a use before its import declaration in
    /// the same file still binds.
    pub fn link_imports(&mut self, asts: &[&Ast], imports: &ImportGraph) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        for ast in asts {
            for block in &ast.blocks {
                for item in &block.items {
                    let ItemKind::Import(imp) = &item.kind else {
                        continue;
                    };
                    let Some(surface) = imports.get(imp.span) else {
                        continue;
                    };
                    match surface {
                        ImportSurface::NoSuchMember { member } => {
                            diags.push(
                                Diagnostic::error(
                                    ErrorCode::ImportNameMissing,
                                    format!(
                                        "`{member}` names no member of the linked package [G4]"
                                    ),
                                )
                                .with_primary(imp.path.span, "no such member"),
                            );
                        },
                        ImportSurface::Merged { member } => {
                            if let Some(alias) = &imp.alias {
                                self.bind_module_alias(
                                    ast.file,
                                    alias,
                                    imp.span,
                                    ModuleSurface::Merged { member: *member },
                                    &mut diags,
                                );
                            }
                        },
                        ImportSurface::Linked(member_surface) => {
                            self.register_member(ast.file, imp, member_surface, &mut diags);
                            // the span the linked surface served: the
                            // resolver's merged-namespace check skips it (a
                            // linked selection validates against the member
                            // surface alone — the link pass above)
                            self.imports
                                .entry(ast.file)
                                .or_default()
                                .linked
                                .push(imp.span);
                            // the shared tag mapping's rows collect here:
                            // the lowering seeds its table with them, so
                            // duplicate rows (two imports of one package)
                            // ride twice and the seed stays idempotent
                            self.linked_tags.extend(member_surface.tags.iter().cloned());
                            // the trait-impl facts collect deduplicated:
                            // the declaration pass declares one instance
                            // per row, so a second registration of the
                            // same linked impl would trip coherence
                            // against itself
                            for fact in &member_surface.impls {
                                if !self.linked_impls.contains(fact) {
                                    self.linked_impls.push(fact.clone());
                                }
                            }
                        },
                    }
                }
            }
        }
        for ast in asts {
            for block in &ast.blocks {
                for item in &block.items {
                    let ItemKind::Import(imp) = &item.kind else {
                        continue;
                    };
                    // A `pub import` row re-exports: its bound selections
                    // join the export surface (validate_member records
                    // them), while a selectionless row binds nothing to
                    // re-export and the modifier names nothing. An `@std`
                    // path skips the selectionless check — the resolving
                    // pass's own `pub import` refusal owns the word there.
                    let reexport = matches!(item.vis, Some(ds_parse::Vis::Pub(_)));
                    let std_path = imp.path.at_sign
                        && imp.path.segments.first().is_some_and(|s| s.name == "std");
                    if let Some(ImportSurface::Linked(member_surface)) = imports.get(imp.span) {
                        self.validate_member(ast.file, imp, member_surface, reexport, &mut diags);
                        if reexport && imp.select.is_empty() && !std_path {
                            diags.push(
                                Diagnostic::error(
                                    ErrorCode::VisNoTarget,
                                    "a `pub import` with no selection list re-exports nothing [G4]"
                                        .to_owned(),
                                )
                                .with_primary(imp.span, "the import binds no name to re-export"),
                            );
                        }
                    }
                }
            }
        }
        diags
    }

    /// Registers one linked member's surface at its import: the
    /// exported definitions become package defs — callables and literal
    /// consts deduped per linked definition so two imports of one
    /// package share them, a type by its name alone (the universe mints
    /// by name, so a local declaration or another linked package owning
    /// the name gates the registration and the selection refuses) — and
    /// the alias binds a module reference the file's qualified calls
    /// resolve through.
    fn register_member(
        &mut self,
        file: ds_diag::FileId,
        imp: &ds_parse::ImportDecl,
        member_surface: &MemberSurface,
        diags: &mut Vec<Diagnostic>,
    ) {
        let mut callables: FxHashMap<String, DefId> = FxHashMap::default();
        for entry in &member_surface.entries {
            match entry.kind {
                ForeignKind::Fn => {
                    // the dedup key names the linked definition, not its
                    // table slot: two bodiless externs of one package
                    // share no func index, so the name is the identity
                    let key = (entry.link, entry.name.clone());
                    let def = if let Some(&def) = self.foreign_defs.get(&key) {
                        def
                    } else {
                        let def = DefId::from_raw(
                            u32::try_from(self.len()).expect("definition count fits u32"),
                        );
                        self.defs.push(Def {
                            name: entry.name.clone(),
                            kind: DefKind::Fn,
                            vis: entry.vis,
                            span: entry.decl_span,
                        });
                        self.foreign.insert(def, entry.clone());
                        self.foreign_defs.insert(key, def);
                        def
                    };
                    callables.insert(entry.name.clone(), def);
                },
                ForeignKind::Binding if entry.value.is_some() => {
                    let key = (entry.link, entry.name.clone());
                    if !self.foreign_const_defs.contains_key(&key) {
                        let def = DefId::from_raw(
                            u32::try_from(self.len()).expect("definition count fits u32"),
                        );
                        self.defs.push(Def {
                            name: entry.name.clone(),
                            kind: DefKind::Const,
                            vis: entry.vis,
                            span: entry.decl_span,
                        });
                        self.foreign.insert(def, entry.clone());
                        self.foreign_const_defs.insert(key, def);
                    }
                },
                // A fact-carrying type registers when its name is free:
                // the universe mints by name, so a local declaration (or
                // a universe-predeclared spelling) or another linked
                // package's type of the same name skips the registration
                // and the selection refuses naming the collision — a
                // silent skip here would mis-shape every signature the
                // local spelling answers instead. A case registers only
                // beside its enum's registration (the entries declare the
                // enum first), so an enum that refuses takes its cases'
                // selections down with it — the case's row maps back to
                // the enum's def.
                ForeignKind::Type if entry.ty.is_some() => {
                    let kind = match entry.ty.as_deref() {
                        Some(
                            crate::import::TypeFact::Struct { .. }
                            | crate::import::TypeFact::Opaque,
                        ) => Some(DefKind::Struct),
                        Some(crate::import::TypeFact::Enum { .. }) => Some(DefKind::Enum),
                        Some(crate::import::TypeFact::Case(fact)) => self
                            .foreign_type_defs
                            .contains_key(&fact.enum_name)
                            .then_some(DefKind::Case),
                        Some(crate::import::TypeFact::Alias(..)) => Some(DefKind::TypeAlias),
                        Some(crate::import::TypeFact::Tag) => Some(DefKind::Tag),
                        Some(crate::import::TypeFact::Trait(..)) => Some(DefKind::Trait),
                        None => None,
                    };
                    // A case may carry the reserved spellings `none`
                    // (and, where a producer could spell it, `type`):
                    // the mint's reserved-atom mapping owns both sides,
                    // mapping the colliding unit case onto the
                    // predeclared atom exactly like a local case's mint.
                    // Every other shape keeps the reserved-name refusal —
                    // its mint would silently skip the predeclared
                    // collision and mis-shape every use.
                    let reserved_ok =
                        matches!(entry.ty.as_deref(), Some(crate::import::TypeFact::Case(_)));
                    let free = (reserved_ok || (entry.name != "none" && entry.name != "type"))
                        && self.lookup_type(&entry.name).is_none()
                        && !self.foreign_type_defs.contains_key(&entry.name);
                    if let (Some(kind), true) = (kind, free) {
                        let def = DefId::from_raw(
                            u32::try_from(self.len()).expect("definition count fits u32"),
                        );
                        self.defs.push(Def {
                            name: entry.name.clone(),
                            kind,
                            vis: entry.vis,
                            span: entry.decl_span,
                        });
                        self.foreign.insert(def, entry.clone());
                        self.foreign_type_defs.insert(entry.name.clone(), def);
                        // a trait's members join the method registry as
                        // its own foreign rows: the candidates a
                        // consuming dot-call selects over, owner-keyed
                        // by the trait def the registration just minted
                        if let Some(crate::import::TypeFact::Trait(fact)) = entry.ty.as_deref() {
                            for member in &fact.members {
                                self.register_method(
                                    &Ident {
                                        span: member.span,
                                        name: member.name.clone(),
                                    },
                                    MethodOwner::Trait(def),
                                    diags,
                                );
                            }
                        }
                    }
                },
                ForeignKind::Binding | ForeignKind::Type => {},
            }
        }
        // the alias serves the callables alone: a qualified member use is
        // a call site, so only function defs reach its surface
        if let Some(alias) = &imp.alias {
            self.bind_module_alias(
                file,
                alias,
                imp.span,
                ModuleSurface::Linked {
                    surface: member_surface.clone(),
                    defs: callables.clone(),
                },
                diags,
            );
        }
    }

    /// Validates one linked member's selections: each names an exported
    /// definition whose shape binds this round, and what passes binds
    /// into the importing file's namespace (G4). A `pub import` row's
    /// bindings record as re-exports — the export surface's transitive
    /// closure — under the name the row bound (an alias's spelling
    /// included), the row's own span the definition site a dependent's
    /// diagnostics name.
    fn validate_member(
        &mut self,
        file: ds_diag::FileId,
        imp: &ds_parse::ImportDecl,
        member_surface: &MemberSurface,
        reexport: bool,
        diags: &mut Vec<Diagnostic>,
    ) {
        for sel in &imp.select {
            let Some(entry) = member_surface.entry(&sel.name.name) else {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportNameMissing,
                        format!(
                            "`{}` names no definition the imported member exports [G4]",
                            sel.name.name
                        ),
                    )
                    .with_primary(sel.span, "no such exported name"),
                );
                continue;
            };
            if !entry.vis.exported() {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportNotExported,
                        format!(
                            "`{}` is `pub(pkg)` or private in its package; only `pub` names \
                             export                              [G4]",
                            sel.name.name
                        ),
                    )
                    .with_primary(sel.span, "not exported to dependents")
                    .with_secondary(entry.decl_span, "declared here"),
                );
                continue;
            }
            // The bindable def this selection names, when its shape binds
            // this round: a function through its signature's spellability,
            // a literal const through its value fact, a flat struct
            // through its registered structural fact.
            let bindable = match entry.kind {
                ForeignKind::Fn if entry.generic || entry.defaulted => {
                    let what = if entry.generic {
                        "a generic function: its one body is a per-instantiation fact the linked \
                         module serves"
                    } else {
                        "a function with a defaulted parameter: the default stages at the linked \
                         module's own call sites"
                    };
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportUnsupportedShape,
                            format!(
                                "`{}` cannot bind across packages this round: {what} [G4]",
                                sel.name.name
                            ),
                        )
                        .with_primary(sel.span, "not bindable across packages")
                        .with_secondary(entry.decl_span, "declared here"),
                    );
                    continue;
                },
                ForeignKind::Fn => {
                    if let Some((bad, kind)) = self.unspellable_type(entry, member_surface) {
                        let what = match kind {
                            Unspellable::Unbound => format!(
                                "`{}`'s signature names `{}`, a linked type whose name this \
                                 package or another linked package already owns [G4]",
                                sel.name.name, bad
                            ),
                            Unspellable::Unknown => format!(
                                "`{}`'s signature names `{}`, which this package cannot spell; \
                                 cross-package types wait on the artifact round [G4]",
                                sel.name.name, bad
                            ),
                        };
                        diags.push(
                            Diagnostic::error(ErrorCode::ImportUnsupportedShape, what)
                                .with_primary(sel.span, "the signature names a foreign type")
                                .with_secondary(entry.decl_span, "declared here"),
                        );
                        continue;
                    }
                    self.foreign_defs
                        .get(&(entry.link, entry.name.clone()))
                        .copied()
                },
                ForeignKind::Binding if entry.value.is_some() => self
                    .foreign_const_defs
                    .get(&(entry.link, entry.name.clone()))
                    .copied(),
                ForeignKind::Binding => {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportUnsupportedShape,
                            format!(
                                "`{}` cannot bind across packages this round: its value is no \
                                 const fact — a `var` binding evaluates in the linked module and \
                                 no per-target value exists for it [G4]",
                                sel.name.name
                            ),
                        )
                        .with_primary(sel.span, "not bindable across packages")
                        .with_secondary(entry.decl_span, "declared here"),
                    );
                    continue;
                },
                ForeignKind::Type if entry.ty.is_some() => {
                    if let Some(&def) = self.foreign_type_defs.get(&entry.name) {
                        Some(def)
                    } else {
                        let what = if let Some(crate::import::TypeFact::Case(fact)) =
                            entry.ty.as_deref()
                            && !self.foreign_type_defs.contains_key(&fact.enum_name)
                        {
                            format!(
                                "its enum `{}` did not bind this round — the case registers only \
                                 beside its enum [G4]",
                                fact.enum_name
                            )
                        } else if self.lookup_type(&entry.name).is_some()
                            || (!matches!(
                                entry.ty.as_deref(),
                                Some(crate::import::TypeFact::Case(_))
                            ) && (entry.name == "none" || entry.name == "type"))
                        {
                            format!(
                                "this package already declares `{}`; the local declaration would \
                                 shadow the linked type and mis-shape every signature spelling it \
                                 [G4]",
                                entry.name
                            )
                        } else {
                            format!(
                                "another linked package already binds the type `{}` [G4]",
                                entry.name
                            )
                        };
                        diags.push(
                            Diagnostic::error(
                                ErrorCode::ImportUnsupportedShape,
                                format!(
                                    "`{}` cannot bind across packages this round: {what}",
                                    sel.name.name
                                ),
                            )
                            .with_primary(sel.span, "not bindable across packages")
                            .with_secondary(entry.decl_span, "declared here"),
                        );
                        continue;
                    }
                },
                ForeignKind::Type => {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportUnsupportedShape,
                            format!(
                                "`{}` cannot bind across packages this round: a cross-package \
                                 type's layout and tag rows are the artifact round's link facts \
                                 [G4]",
                                sel.name.name
                            ),
                        )
                        .with_primary(sel.span, "not bindable across packages")
                        .with_secondary(entry.decl_span, "declared here"),
                    );
                    continue;
                },
            };
            let Some(def) = bindable else {
                continue;
            };
            let name = sel.alias.as_ref().unwrap_or(&sel.name);
            let file_imports = self.imports.entry(file).or_default();
            if let Some((_, first)) = file_imports.names.get(&name.name) {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportDuplicate,
                        format!(
                            "two imports bind the name `{}` into this file [G4]",
                            name.name
                        ),
                    )
                    .with_primary(name.span, "bound again here")
                    .with_secondary(*first, "first bound here"),
                );
                continue;
            }
            if let Some(&(_, first)) = file_imports.modules.get(&name.name) {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportDuplicate,
                        format!(
                            "two imports bind the name `{}` into this file [G4]",
                            name.name
                        ),
                    )
                    .with_primary(name.span, "bound again here")
                    .with_secondary(first, "the module alias bound it here"),
                );
                continue;
            }
            file_imports
                .names
                .insert(name.name.clone(), (def, imp.span));
            if reexport {
                self.reexports.push((def, name.name.clone(), imp.span));
            }
        }
    }

    /// The first type-name head a linked function's written signature
    /// spells that this compile cannot spell as the linked package
    /// means it: a head naming a type the member itself exports must
    /// have registered (a fact-carrying type that bound), anything else
    /// must live in this package's own type namespace — the ambient's
    /// atoms included. A registered foreign type spells in every
    /// position, unions included: the shared tag mapping seeds this
    /// compile's tag table with the linked module's own rows at
    /// lowering, so a union's tag word carries ids both modules agree
    /// on.
    fn unspellable_type(
        &self,
        entry: &ForeignEntry,
        surface: &MemberSurface,
    ) -> Option<(String, Unspellable)> {
        let sig = entry.sig.as_ref()?;
        for param in &sig.params {
            if let Some(ty) = &param.ty
                && let Some(found) = self.type_refusal(ty, false, surface)
            {
                return Some(found);
            }
        }
        if let Some(ret) = &sig.ret {
            return self.type_refusal(ret, false, surface);
        }
        None
    }

    /// One written signature type's first refusal, walking the tree: a
    /// union or optional node marks everything under it union-carried,
    /// where the @std equivalence arm stays refused.
    fn type_refusal(
        &self,
        ty: &Type,
        in_union: bool,
        surface: &MemberSurface,
    ) -> Option<(String, Unspellable)> {
        match ty {
            Type::Path { path, args, .. } => {
                if let [only] = path.segments.as_slice() {
                    let head = &only.name;
                    let refusal = if let Some(entry) = surface.entry(head) {
                        // The linked package's own type spells when its
                        // fact registered; a local declaration of the same
                        // name would otherwise answer the spelling with
                        // the wrong shape. Unions spell too — the tag
                        // mapping owns the crossing words.
                        let registered = entry.kind != ForeignKind::Type
                            || self.foreign_type_defs.contains_key(head);
                        // The @std equivalence: the reserved package's
                        // first member is the ambient core itself (a
                        // declaring compile registers that same core as
                        // its ambient, the migrated members riding beside
                        // the core inside the package), so a word-shaped
                        // atom head the consuming ambient also declares is
                        // the same declaration — the spelling resolves
                        // through the consumer's own atom, the same
                        // base-aware lookup the registered foreign types
                        // read. Union positions keep the tag-seam refusal
                        // (the two modules' private tag tables), and every
                        // tag- or struct-bearing head keeps its Unbound
                        // refusal; the arm retires when the core's atoms
                        // stop crossing as plain words.
                        let std_equivalent = entry.std
                            && !in_union
                            && args.is_empty()
                            && self.word_shaped_ambient_atom(head);
                        (!registered && !std_equivalent).then_some(Unspellable::Unbound)
                    } else {
                        (head != "none" && self.lookup_type(head).is_none())
                            .then_some(Unspellable::Unknown)
                    };
                    if let Some(kind) = refusal {
                        return Some((head.clone(), kind));
                    }
                }
                args.iter()
                    .find_map(|arg| self.type_refusal(arg, false, surface))
            },
            // the optional marker adds `none` to the union, so everything
            // under it rides a tag word
            Type::Optional { inner, .. } => self.type_refusal(inner, true, surface),
            Type::Union { left, right, .. }
            | Type::Intersect { left, right, .. }
            | Type::Difference { left, right, .. } => self
                .type_refusal(left, true, surface)
                .or_else(|| self.type_refusal(right, true, surface)),
            Type::Paren { inner, .. } | Type::Negation { inner, .. } | Type::Ptr { inner, .. } => {
                self.type_refusal(inner, in_union, surface)
            },
            Type::Tuple { items, .. } => items
                .iter()
                .find_map(|item| self.type_refusal(item, in_union, surface)),
            Type::Fn { params, ret, .. } => params
                .iter()
                .filter_map(|param| param.ty.as_ref())
                .find_map(|ty| self.type_refusal(ty, in_union, surface))
                .or_else(|| {
                    ret.as_ref()
                        .and_then(|ty| self.type_refusal(ty, in_union, surface))
                }),
            Type::Unit(_) | Type::TypeKw(_) | Type::Foreign { .. } => None,
        }
    }

    /// Whether `name` is one of the ambient's word-shaped atoms — the
    /// scalar words plus the two Ref-word collections whose values cross
    /// modules as plain words with no tag id inside. The closed set the
    /// @std spelling equivalence trusts: a value of these types built in
    /// either module reads identically in the other, where a tag-bearing
    /// or struct value keeps its cross-package refusals. The lookup is
    /// the package's own base-aware one, so a parked serve's atoms and a
    /// fresh walk's answer alike.
    fn word_shaped_ambient_atom(&self, name: &str) -> bool {
        matches!(
            name,
            "int" | "float" | "bool" | "string" | "char" | "byte" | "u32" | "bytes"
        ) && self.lookup_type(name).is_some()
    }

    /// Binds one import's module alias, refusing a second binding of
    /// the same name into the file.
    fn bind_module_alias(
        &mut self,
        file: ds_diag::FileId,
        alias: &Ident,
        import_span: Span,
        surface: ModuleSurface,
        diags: &mut Vec<Diagnostic>,
    ) {
        let file_imports = self.imports.entry(file).or_default();
        if let Some((_, first)) = file_imports.names.get(&alias.name) {
            diags.push(
                Diagnostic::error(
                    ErrorCode::ImportDuplicate,
                    format!(
                        "two imports bind the name `{}` into this file [G4]",
                        alias.name
                    ),
                )
                .with_primary(alias.span, "bound again here")
                .with_secondary(*first, "first bound here"),
            );
            return;
        }
        // The duplicate check runs before the id exists: an insert that
        // overwrites first would leave the new id dangling over a
        // module_refs slot the early return never pushes.
        if file_imports.modules.contains_key(&alias.name) {
            diags.push(
                Diagnostic::error(
                    ErrorCode::ImportDuplicate,
                    format!(
                        "two imports bind the module alias `{}` into this file [G4]",
                        alias.name
                    ),
                )
                .with_primary(alias.span, "bound again here")
                .with_secondary(import_span, "the import bound it here"),
            );
            return;
        }
        let id = ModuleId::from_raw(
            u32::try_from(self.module_refs.len()).expect("module count fits u32"),
        );
        file_imports
            .modules
            .insert(alias.name.clone(), (id, import_span));
        self.module_refs.push(ModuleRef {
            alias: alias.name.clone(),
            surface,
        });
    }

    /// The import-bound functions, keyed by their def.
    #[must_use]
    pub fn foreign(&self) -> &FxHashMap<DefId, ForeignEntry> {
        &self.foreign
    }

    /// The selections a `pub import` row bound (G4's re-export): the
    /// def, the bound name it exports under, and the row's span — in
    /// binding order, a def bound by two pub rows riding twice.
    #[must_use]
    pub fn reexports(&self) -> &[(DefId, String, Span)] {
        &self.reexports
    }

    /// The module reference an id names.
    #[must_use]
    pub fn module_ref(&self, id: ModuleId) -> &ModuleRef {
        &self.module_refs[id.index() as usize]
    }

    /// The base tier's definition count: the boundary of the served
    /// prefix's leading run of the def id space (a compile's own files
    /// continue past it).
    #[must_use]
    pub fn base_def_count(&self) -> usize {
        self.base.as_ref().map_or(0, |base| base.defs.len())
    }

    /// The declaration of a definition: the base answers a served prefix
    /// id, the growth tier a compiled file's own.
    #[must_use]
    pub fn def(&self, id: DefId) -> &Def {
        let slot = id.index() as usize;
        match self.base.as_deref() {
            Some(base) if slot < base.defs.len() => &base.defs[slot],
            Some(base) => &self.defs[slot - base.defs.len()],
            None => &self.defs[slot],
        }
    }

    /// Every declaration, in collection order: the base tier's run first,
    /// the growth tier's after it.
    pub fn defs(&self) -> impl Iterator<Item = &Def> + '_ {
        self.base
            .as_ref()
            .into_iter()
            .flat_map(|base| base.defs.iter())
            .chain(self.defs.iter())
    }

    /// The declaration of a registered method: the base answers a served
    /// prefix id, the growth tier a compiled file's own.
    #[must_use]
    pub fn method(&self, id: MethodId) -> &MethodDecl {
        let slot = id.index() as usize;
        match self.base.as_deref() {
            Some(base) if slot < base.methods.len() => &base.methods[slot],
            Some(base) => &self.methods[slot - base.methods.len()],
            None => &self.methods[slot],
        }
    }

    /// The registered methods, in registration order: the base tier's run
    /// first, the growth tier's after it.
    pub fn methods(&self) -> impl Iterator<Item = &MethodDecl> + '_ {
        self.base
            .as_ref()
            .into_iter()
            .flat_map(|base| base.methods.iter())
            .chain(self.methods.iter())
    }

    /// The definition of a type-namespace name, if any (F9): the growth
    /// tier answers first, the shared base below.
    #[must_use]
    pub fn lookup_type(&self, name: &str) -> Option<DefId> {
        self.type_names.get(name).copied().or_else(|| {
            self.base
                .as_deref()
                .and_then(|base| base.type_names.get(name).copied())
        })
    }

    /// The type declaration a name carries for ONE FILE's uses (F9.5):
    /// the file's own shadowing tag declaration beats the first
    /// declaration per label, whose entry owns the shared namespace.
    #[must_use]
    pub fn lookup_type_in(&self, file: ds_diag::FileId, name: &str) -> Option<DefId> {
        if let Some(shadow) = self.file_tags.get(&file).and_then(|tags| tags.get(name)) {
            return Some(*shadow);
        }
        self.lookup_type(name)
    }

    /// The definition of a value-namespace name, if any (F9): the growth
    /// tier answers first, the shared base below.
    #[must_use]
    pub fn lookup_value(&self, name: &str) -> Option<DefId> {
        self.value_names.get(name).copied().or_else(|| {
            self.base
                .as_deref()
                .and_then(|base| base.value_names.get(name).copied())
        })
    }

    /// Whether either namespace declares the name — the sentinel checks
    /// (`none`, `self`) that must not shadow universe spellings.
    #[must_use]
    pub fn declares(&self, name: &str) -> bool {
        self.lookup_type(name).is_some() || self.lookup_value(name).is_some()
    }

    /// The namespace-agnostic lookup: the type declaration when the name
    /// has one, else the value declaration. For consumers that cannot say
    /// which namespace they mean (a name's atom, a legacy plan); call
    /// sites that know their position use [`Self::lookup_type`] or
    /// [`Self::lookup_value`].
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<DefId> {
        self.lookup_type(name).or_else(|| self.lookup_value(name))
    }

    /// The ids of the files collected so far, in addition order: the base
    /// tier's run first, the growth tier's after it.
    pub fn files(&self) -> impl Iterator<Item = ds_diag::FileId> + '_ {
        self.base
            .as_ref()
            .into_iter()
            .flat_map(|base| base.files.iter().copied())
            .chain(self.files.iter().copied())
    }

    /// The position of one file in the addition order (the resolutions'
    /// tree alignment).
    #[must_use]
    pub fn file_position(&self, file: ds_diag::FileId) -> Option<usize> {
        self.files().position(|candidate| candidate == file)
    }

    /// The registered candidates for a method name, in declaration order:
    /// the base tier's run first, the growth tier's after it (a growth
    /// entry for a name the base carries is seeded with the base's
    /// candidates, so its list alone is the complete one).
    pub fn method_candidates<'a>(&'a self, name: &str) -> impl Iterator<Item = MethodId> + 'a {
        let growth = self.method_index.get(name).map_or(&[][..], Vec::as_slice);
        let base = if self.method_index.contains_key(name) {
            &[][..]
        } else {
            self.base
                .as_deref()
                .and_then(|base| base.method_index.get(name))
                .map_or(&[][..], Vec::as_slice)
        };
        base.iter().chain(growth).copied()
    }

    /// Number of package-level definitions across both tiers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.defs.len() + self.base.as_ref().map_or(0, |base| base.defs.len())
    }

    /// Whether the package has no definitions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn collect_item(&mut self, item: &Item, diags: &mut Vec<Diagnostic>) {
        let vis = Vis::of(item.vis);
        if let Some(modifier) = item.vis.filter(|_| {
            matches!(item.kind, ItemKind::Impl(_) | ItemKind::Platform(_))
                || matches!(&item.kind, ItemKind::Import(_))
                    && matches!(item.vis, Some(ds_parse::Vis::PubPkg(_)))
        }) {
            // An impl or platform declaration names no package-level
            // name, and `pub(pkg)` on an import names nothing — imports
            // are already package-wide (one namespace per package, G3).
            // A `pub import`'s meaning is the link pass's: its bound
            // selections join the export surface there, so collection
            // alone cannot judge the modifier.
            let pkg = matches!(modifier, ds_parse::Vis::PubPkg(_))
                .then(|| "(pkg)")
                .unwrap_or("");
            diags.push(
                Diagnostic::error(
                    ErrorCode::VisNoTarget,
                    format!("`pub{pkg}` names no package-level definition here"),
                )
                .with_primary(modifier.span(), "the visibility modifier"),
            );
        }
        match &item.kind {
            ItemKind::Import(_) | ItemKind::Platform(_) => {},
            ItemKind::Local(local) => {
                let kind = if local.mutable { DefKind::Var } else { DefKind::Const };
                self.collect_pattern_defs(&local.pat, kind, vis, diags);
            },
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => {
                self.declare(&f.name, DefKind::Fn, vis, diags);
            },
            ItemKind::Struct(s) => {
                self.declare(&s.name, DefKind::Struct, vis, diags);
            },
            ItemKind::Enum(e) => {
                self.declare(&e.name, DefKind::Enum, vis, diags);
                // A case carries its enum's tier: the case is the enum's
                // own surface. A generic enum's unit cases collect beside
                // the others — a unit word carries no substitution, so its
                // name resolves over every application of the head — while
                // a payloaded case's shape is a per-instantiation fact,
                // so its name stays uncollected until case minting grows
                // substitution.
                for case in &e.cases {
                    if e.generics.is_empty() || matches!(case.payload, EnumPayload::None) {
                        self.declare(&case.name, DefKind::Case, vis, diags);
                    }
                }
            },
            ItemKind::Tag(t) => {
                self.declare(&t.name, DefKind::Tag, vis, diags);
            },
            ItemKind::TypeAlias(a) => {
                self.declare(&a.name, DefKind::TypeAlias, vis, diags);
            },
            ItemKind::Trait(t) => {
                let id = self.declare(&t.name, DefKind::Trait, vis, diags);
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.register_method(&f.name, MethodOwner::Trait(id), diags);
                    }
                }
            },
            ItemKind::Impl(imp) => {
                // Trait impls do not re-register: the trait's signatures
                // already provide the candidate names.
                if imp.trait_ref.is_none() {
                    let owner = inherent_owner(&imp.target);
                    for f in &imp.fns {
                        self.register_method(&f.name, MethodOwner::Inherent(owner.clone()), diags);
                    }
                }
            },
        }
    }

    /// Declares one name into its kind's namespace (F9); a duplicate
    /// WITHIN that namespace reports both spans and keeps the first
    /// definition — the same name in the other namespace coexists.
    fn declare(
        &mut self,
        name: &Ident,
        kind: DefKind,
        vis: Vis,
        diags: &mut Vec<Diagnostic>,
    ) -> DefId {
        self.declare_name(&name.name, name.span, kind, vis, diags)
    }

    fn declare_name(
        &mut self,
        name: &str,
        span: Span,
        kind: DefKind,
        vis: Vis,
        diags: &mut Vec<Diagnostic>,
    ) -> DefId {
        let existing =
            if kind.in_type_namespace() { self.lookup_type(name) } else { self.lookup_value(name) };
        if let Some(first) = existing {
            // Within one module a label names at most one tag
            // declaration: the file's own shadow is the duplicate the
            // refusal names (F9.5 keeps the same-file refusal).
            if matches!(kind, DefKind::Tag | DefKind::Case)
                && let Some(file) = self.collecting_file
                && let Some(prev) = self.file_tags.get(&file).and_then(|tags| tags.get(name))
            {
                let prev = *prev;
                let prev_span = self.def(prev).span;
                diags.push(
                    Diagnostic::error(
                        ErrorCode::DuplicateDefinition,
                        format!("`{name}` is already defined"),
                    )
                    .with_secondary(prev_span, "first defined here")
                    .with_primary(span, "redefined here"),
                );
                return prev;
            }
            // A tag family declaration shadowing an earlier FILE's
            // same-label declaration stays distinct (F9.5): the def
            // registers, the shared namespace keeps the first, and the
            // declaring file's own uses resolve through the shadow map.
            // Every other family keeps the refusal.
            let tag_shadow = matches!(kind, DefKind::Tag | DefKind::Case)
                && matches!(self.def(first).kind, DefKind::Tag | DefKind::Case)
                && self.collecting_file.is_some()
                && Some(self.def(first).span.file) != self.collecting_file;
            if !tag_shadow {
                let first_span = self.def(first).span;
                diags.push(
                    Diagnostic::error(
                        ErrorCode::DuplicateDefinition,
                        format!("`{name}` is already defined"),
                    )
                    .with_secondary(first_span, "first defined here")
                    .with_primary(span, "redefined here"),
                );
                return first;
            }
            // The shadow registers its def row beside the first's; the
            // shared namespace keeps the first.
            let id = DefId::from_raw(u32::try_from(self.len()).expect("table index fits u32"));
            self.defs.push(Def {
                name: name.to_owned(),
                kind,
                vis,
                span,
            });
            if let Some(file) = self.collecting_file {
                self.file_tags
                    .entry(file)
                    .or_default()
                    .insert(name.to_owned(), id);
            }
            return id;
        }
        // The growth path: a fresh name lands in its namespace. The
        // shadow map carries only cross-file tag shadows, never a
        // namespace's own entries.
        let id = DefId::from_raw(u32::try_from(self.len()).expect("table index fits u32"));
        self.defs.push(Def {
            name: name.to_owned(),
            kind,
            vis,
            span,
        });
        if kind.in_type_namespace() {
            self.type_names.insert(name.to_owned(), id);
        } else {
            self.value_names.insert(name.to_owned(), id);
        }
        id
    }

    /// Registers one method name; a same-name same-owner registration is a
    /// duplicate (two impls of one type, or one trait declaring a member
    /// twice) that reports both spans and keeps the first — the discipline
    /// of [`Self::declare`]. Different owners may share a method name —
    /// two traits, or two inherent impls on different target types; call
    /// sites disambiguate by receiver (F3).
    fn register_method(&mut self, name: &Ident, owner: MethodOwner, diags: &mut Vec<Diagnostic>) {
        if let Some(id) = self
            .method_candidates(&name.name)
            .find(|&id| self.method(id).owner == owner)
        {
            let first_span = self.method(id).span;
            diags.push(
                Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("method `{}` is already defined", name.name),
                )
                .with_secondary(first_span, "first defined here")
                .with_primary(name.span, "redefined here"),
            );
            return;
        }
        // A growth id continues the method space past the base tier's run,
        // and a growth entry for a name the base carries seeds with the
        // base's candidates so the growth list alone stays the complete
        // one in declaration order.
        let id =
            MethodId::from_raw(u32::try_from(self.method_len()).expect("table index fits u32"));
        let seeded = self
            .base
            .as_deref()
            .and_then(|base| base.method_index.get(&name.name))
            .is_some();
        if seeded {
            let mut candidates = self.method_candidates(&name.name).collect::<Vec<_>>();
            candidates.push(id);
            self.method_index.insert(name.name.clone(), candidates);
        } else {
            self.method_index
                .entry(name.name.clone())
                .or_default()
                .push(id);
        }
        self.methods.push(MethodDecl {
            name: name.name.clone(),
            owner,
            span: name.span,
        });
    }

    /// Top-level binding patterns declare one def per bound name, through
    /// the shared binder walker so pun fields and case sub-patterns
    /// declare like any other binder shape.
    fn collect_pattern_defs(
        &mut self,
        pat: &Pattern,
        kind: DefKind,
        vis: Vis,
        diags: &mut Vec<Diagnostic>,
    ) {
        for (span, name) in crate::pattern::pattern_binders(pat) {
            self.declare_name(name, span, kind, vis, diags);
        }
    }
}

/// One file's resolution pass: walks items with lexical scopes and records
/// every name use.
struct FileResolver<'a> {
    package: &'a Package,
    uses: Vec<(Span, Res)>,
    diags: Vec<Diagnostic>,
    /// The file being resolved: the shadowing tag lookup (F9.5) is
    /// per file, a file's own tag declaration beating the first's.
    file: ds_diag::FileId,
    /// The file's import bindings, when the link pass produced any.
    imports: Option<&'a FileImports>,
    /// Value scopes, innermost last: binder name to binder span.
    scopes: Vec<FxHashMap<String, Span>>,
    /// Type-variable scopes, innermost last.
    type_scopes: Vec<FxHashMap<String, Span>>,
}

/// What a bare identifier in a pattern names.
enum ArmTag {
    /// No constructor entry: the identifier binds.
    Binding,
    /// The universe's predeclared `none` atom, which has no def.
    BuiltinNone,
    /// A constructor type's definition.
    Def(DefId),
}

impl FileResolver<'_> {
    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Import(imp) => self.import(imp, item.vis),
            ItemKind::Tag(_) | ItemKind::Platform(_) => {},
            ItemKind::Local(local) => {
                // A top-level binding declares a PACKAGE def [G3], not a
                // local: the initializer and annotation resolve here, the
                // binders bind nowhere, and uses reach the name through
                // the package table.
                self.expr(&local.init);
                if let Some(ty) = &local.ty {
                    self.type_(ty);
                }
            },
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => self.fn_decl(f, false),
            ItemKind::Struct(s) => {
                self.type_params(&s.generics);
                for ty in &s.supers {
                    self.type_(ty);
                }
                for member in &s.members {
                    match member {
                        StructMember::Field(field) => self.type_(&field.ty),
                        // `self` inside a deinit body is the dying
                        // instance [C5], borrowed; the walk never types it,
                        // so binding the name here is all resolution owes.
                        StructMember::Deinit { span, body } => {
                            self.scopes.push(FxHashMap::default());
                            self.bind(
                                &Ident {
                                    span: *span,
                                    name: "self".to_owned(),
                                },
                                false,
                            );
                            self.block(body);
                            self.scopes.pop();
                        },
                    }
                }
                self.type_scopes.pop();
            },
            ItemKind::Enum(e) => {
                self.type_params(&e.generics);
                for ty in &e.supers {
                    self.type_(ty);
                }
                for case in &e.cases {
                    match &case.payload {
                        EnumPayload::None => {},
                        EnumPayload::Tuple(types) => {
                            for ty in types {
                                self.type_(ty);
                            }
                        },
                        EnumPayload::Struct(fields) => {
                            for field in fields {
                                self.type_(&field.ty);
                            }
                        },
                    }
                }
                self.type_scopes.pop();
            },
            ItemKind::TypeAlias(a) => {
                self.type_params(&a.generics);
                self.type_(&a.ty);
                self.type_scopes.pop();
            },
            ItemKind::Trait(t) => {
                self.type_params(&t.generics);
                for ty in &t.supers {
                    self.type_(ty);
                }
                // A trait member's signatures spell the self type `Self`
                // [F2, record 6.1].
                self.type_scopes
                    .push(FxHashMap::from_iter([("Self".to_owned(), t.name.span)]));
                for member in &t.members {
                    match member {
                        TraitMember::AssocType { .. } => {},
                        TraitMember::Fn(f) => self.fn_decl(f, true),
                    }
                }
                self.type_scopes.pop();
                self.type_scopes.pop();
            },
            ItemKind::Impl(imp) => {
                self.type_params(&imp.generics);
                if let Some(trait_ref) = &imp.trait_ref {
                    for arg in &trait_ref.args {
                        self.type_(arg);
                    }
                    // The trait reference is a type use: the type
                    // namespace owns it, so a same-named value (a free
                    // `fn show` beside `trait show`) never claims the
                    // head.
                    match trait_ref.path.segments.as_slice() {
                        [head] => self.type_name_use(head),
                        _ => self.path_use(&trait_ref.path),
                    }
                }
                // An impl's members spell the self type `Self` [F2, record
                // 6.1], trait and inherent alike.
                self.type_scopes
                    .push(FxHashMap::from_iter([("Self".to_owned(), imp.span)]));
                self.type_(&imp.target);
                for f in &imp.fns {
                    self.fn_decl(f, true);
                }
                self.type_scopes.pop();
                self.type_scopes.pop();
            },
        }
    }

    /// Binds generic type parameters in a fresh type scope.
    fn type_params(&mut self, params: &[TypeParam]) {
        let mut scope = FxHashMap::default();
        for param in params {
            self.record_dup(&mut scope, &param.name);
            if let Some(bound) = &param.bound {
                self.type_(bound);
            }
        }
        self.type_scopes.push(scope);
    }

    fn fn_decl(&mut self, f: &FnDecl, member: bool) {
        self.type_params(&f.generics);
        self.scopes.push(FxHashMap::default());
        for (index, param) in f.params.iter().enumerate() {
            self.param(param, member && index == 0);
        }
        if let Some(ret) = &f.ret {
            self.type_(ret);
        }
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.scopes.pop();
        self.type_scopes.pop();
    }

    fn param(&mut self, param: &Param, receiver: bool) {
        if let Some(ty) = &param.ty {
            // The annotated receiver's `self` annotation is the checker's
            // to report (the receiver ruling names the new spelling); a
            // plain `self` path here would double-report.
            let bare_self = matches!(
                ty,
                Type::Path { path, args, .. }
                    if args.is_empty()
                        && path.segments.len() == 1
                        && path.segments[0].name == "self"
            );
            if !(receiver && bare_self) {
                self.type_(ty);
            }
        }
        self.bind(&param.name, false);
    }

    fn local(&mut self, local: &ds_parse::LocalDecl) {
        // The initializer resolves before the binders bind.
        self.expr(&local.init);
        if let Some(ty) = &local.ty {
            self.type_(ty);
        }
        self.bind_pattern(&local.pat, false);
    }

    fn block(&mut self, block: &ds_parse::BlockExpr) {
        self.scopes.push(FxHashMap::default());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Local(local) => self.local(local),
            StmtKind::Expr(expr) => self.expr(expr),
        }
    }

    // ---- value positions ---------------------------------------------------

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal { .. }
            | Expr::RawStr { .. }
            | Expr::Break(_)
            | Expr::Continue(_)
            // A conversion placeholder uses no name.
            | Expr::Invalid(_) => {},
            Expr::Str { parts, .. } => {
                for part in parts {
                    match part {
                        ds_parse::StrPart::Text { .. } => {},
                        ds_parse::StrPart::Expr { expr, .. } => self.expr(expr),
                        // `$x` bare holes are a value use of the name.
                        ds_parse::StrPart::Ident { name, .. } => self.name_use(name),
                    }
                }
            },
            Expr::Lambda {
                generics,
                params,
                ret,
                body,
                ..
            } => {
                self.type_params(generics);
                self.scopes.push(FxHashMap::default());
                for param in params {
                    self.param(param, false);
                }
                if let Some(ret) = ret {
                    self.type_(ret);
                }
                self.expr(body);
                self.scopes.pop();
                self.type_scopes.pop();
            },
            Expr::Path { path, .. } => self.path_use(path),
            Expr::Struct { path, fields, .. } => {
                self.ctor_path_use(path);
                for (name, value) in fields {
                    match value {
                        // Punning: the field name is also a value use.
                        None => self.name_use(name),
                        Some(expr) => self.expr(expr),
                    }
                }
            },
            Expr::List { items, .. } | Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            },
            Expr::Paren { inner, .. } => self.expr(inner),
            Expr::Call {
                callee,
                args,
                trailing,
                ..
            } => {
                match callee.as_ref() {
                    // UFCS: a dot-callee gets tiered resolution [F3];
                    // a bare type name as the receiver takes the
                    // type-keyed tier instead (the receiverless ruling).
                    Expr::Field { recv, name, .. } => {
                        self.dot_recv_use(recv);
                        self.dot_use(recv, name);
                    },
                    // A call-form name resolves per the namespace split
                    // [F9]; a generic-application constructor head is a
                    // type use [F9].
                    Expr::Path { path, .. } if path.segments.len() == 1 => {
                        self.call_name_use(&path.segments[0]);
                    },
                    Expr::Index { recv, index, .. } => self.ctor_apply_use(recv, index),
                    Expr::MapApply {
                        head, key, value, ..
                    } => self.map_apply_use(head, key, value),
                    _ => self.expr(callee),
                }
                for arg in args {
                    self.expr(&arg.value);
                }
                if let Some(tail) = trailing {
                    self.expr(tail);
                }
            },
            Expr::Field { recv, name, .. } => {
                // Bare field access: struct-owned, type-directed later.
                // A module-alias receiver takes the qualified member's
                // value seam instead (G4).
                if let Some(module) = self.module_ref_receiver(recv) {
                    self.module_member_value_use(recv, module, name);
                } else {
                    self.expr(recv);
                }
            },
            Expr::Index { recv, index, .. } => {
                self.expr(recv);
                self.expr(index);
            },
            Expr::MapApply {
                head, key, value, ..
            } => self.map_apply_use(head, key, value),
            // A fn type in the constructor brackets resolves its param and
            // return types as type uses.
            Expr::FnType { ty, .. } => self.type_(ty),
            Expr::Try { expr, .. } | Expr::Unary { expr, .. } => self.expr(expr),
            Expr::Is { expr, ty, .. } => {
                self.expr(expr);
                self.type_(ty);
            },
            Expr::Binary { left, right, .. }
            | Expr::Range {
                start: left,
                end: right,
                ..
            }
            | Expr::Assign { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            },
            Expr::If {
                cond, then, else_, ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(else_expr) = else_ {
                    self.expr(else_expr);
                }
            },
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    // Each arm is its own scope nested in the enclosing one.
                    self.scopes.push(FxHashMap::default());
                    self.arm_pattern(&arm.pat);
                    if let Some(ann) = &arm.ann {
                        self.type_(ann);
                    }
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                    self.scopes.pop();
                }
            },
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            },
            Expr::For {
                pat, iter, body, ..
            } => {
                self.expr(iter);
                self.scopes.push(FxHashMap::default());
                self.bind_pattern(pat, false);
                self.expr(body);
                self.scopes.pop();
            },
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            },
            Expr::Nursery { body, .. } | Expr::Spawn { body, .. } | Expr::Defer { body, .. } => {
                self.block(body);
            },
            Expr::Block(block) => self.block(block),
        }
    }

    /// Refuses a use crossing the private tier's file boundary (G4): a
    /// private name is its defining file's alone. The resolution still
    /// records, keeping the refusal the single diagnostic the later
    /// passes see (the collision precedent).
    fn enforce_vis(&mut self, span: Span, def: DefId) {
        let def_decl = self.package.def(def);
        if def_decl.vis == Vis::Private && def_decl.span.file != span.file {
            // the retired collection spellings name their member
            // replacement in the message; every other private name keeps
            // the plain boundary refusal
            let hint = match def_decl.name.as_str() {
                "list_get" => "; the member spelling is `xs.get(i)`",
                "list_push" => "; the member spelling is `xs.push(x)`",
                "map_get" => "; the member spelling is `m.get(k)`",
                "map_set" => "; the member spelling is `m.set(k, v)`",
                "map_keys" => "; the member spelling is `m.keys()`",
                "list_len" => "; the member spelling is `xs.len()`",
                "list_concat" => "; the member spelling is `xs.concat(ys)`",
                "map_len" => "; the member spelling is `m.len()`",
                _ => "",
            };
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::PrivateUse,
                    format!(
                        "`{}` is private to its defining file [G4]{hint}",
                        def_decl.name
                    ),
                )
                .with_primary(span, "used outside the defining file")
                .with_secondary(def_decl.span, "defined private here"),
            );
        }
    }

    /// Records one use of a package definition behind the private tier's
    /// file boundary.
    fn def_use(&mut self, span: Span, def: DefId) {
        self.enforce_vis(span, def);
        self.uses.push((span, Res::Def(def)));
    }

    /// One import declaration (G4): an `@std` import's selections validate
    /// against the exported surface — the compile unit's merged namespace
    /// serving the undeclared reading — while a graph that resolved the
    /// reserved package serves the same span through its linked surface
    /// (the member surface's own validation, recorded by the link pass)
    /// and every other path (bare or sigiled) stays declarative for the
    /// link walk (the dep-list records it and the graph validates the
    /// link). A condition is a source-internal spelling the target model
    /// will evaluate; until then the import records and links as written.
    /// A `pub import` of `@std` refuses here: the merged-ambient reading
    /// binds the compile's own names, never import-bound defs, so no
    /// selection re-exports until the un-merge (item 53).
    fn import(&mut self, imp: &ds_parse::ImportDecl, vis: Option<ds_parse::Vis>) {
        if !imp.path.at_sign || imp.path.segments.first().is_none_or(|s| s.name != "std") {
            return;
        }
        if let Some(ds_parse::Vis::Pub(span)) = vis {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::VisNoTarget,
                    "`pub import` of `@std` re-exports nothing: the merged-ambient reading binds \
                     this compile's own names [G4]"
                        .to_owned(),
                )
                .with_primary(span, "the visibility modifier"),
            );
        }
        if self
            .imports
            .is_some_and(|imports| imports.linked.contains(&imp.span))
        {
            return;
        }
        for sel in &imp.select {
            let Some(def) = self.package.lookup(&sel.name.name) else {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportNameMissing,
                        format!(
                            "`{}` names no definition the imported surface exports [G4]",
                            sel.name.name
                        ),
                    )
                    .with_primary(sel.span, "no such exported name"),
                );
                continue;
            };
            if !self.package.def(def).vis.exported() {
                let def_span = self.package.def(def).span;
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportNotExported,
                        format!(
                            "`{}` is `pub(pkg)` or private in its package; only `pub` names \
                             export                              [G4]",
                            sel.name.name
                        ),
                    )
                    .with_primary(sel.span, "not exported to dependents")
                    .with_secondary(def_span, "declared here"),
                );
            }
        }
    }

    /// A plain identifier use: locals shadow package names, the value
    /// namespace comes next, and the type namespace last — a bare tag is a
    /// value-producing constructor, and a struct/enum/trait name reached
    /// here still resolves so the checker words its "type used as a value"
    /// error (F9). Bare `none` is the universe's distinguished unit atom
    /// (B4), not a package name; the checker types it directly, so it
    /// records nothing here.
    fn name_use(&mut self, name: &Ident) {
        if let Some(bind) = self.lookup_local(&name.name) {
            self.uses.push((name.span, Res::Local { bind }));
            return;
        }
        if let Some(def) = self
            .package
            .lookup_value(&name.name)
            .or_else(|| self.package.lookup_type_in(self.file, &name.name))
        {
            self.def_use(name.span, def);
        } else if let Some(def) = self.import_binding(&name.name) {
            self.uses.push((name.span, Res::Def(def)));
        } else if let Some(module) = self.module_alias(&name.name) {
            self.uses.push((name.span, Res::Module(module)));
        } else if name.name != "none" {
            self.unknown_name(name, self.shadowing_candidates());
        }
    }

    /// The import-bound function a name selects in the resolving file
    /// (G4): locals and package names shadow imported names, the
    /// resolver's own priority.
    fn import_binding(&self, name: &str) -> Option<DefId> {
        self.imports
            .and_then(|imports| imports.names.get(name))
            .map(|&(def, _)| def)
    }

    /// The module alias a name binds in the resolving file, when no
    /// local, package name, or imported name shadows it.
    fn module_alias(&self, name: &str) -> Option<ModuleId> {
        self.imports
            .and_then(|imports| imports.modules.get(name))
            .map(|&(module, _)| module)
    }

    /// The import-bound type a name selects in the resolving file (G4):
    /// a selection of a linked type's def, when the package's own type
    /// namespace does not declare the name — the same shadowing priority
    /// the value positions follow.
    fn import_type_binding(&self, name: &str) -> Option<DefId> {
        let def = self.import_binding(name)?;
        self.package
            .def(def)
            .kind
            .in_type_namespace()
            .then_some(def)
    }

    /// The callee of a call form (F9): locals first (an indirect call of a
    /// bound function value), then the value namespace, then the type
    /// namespace (a constructor call — tag or payload construction).
    /// Present in BOTH namespaces the call is ambiguous — an error naming
    /// both declarations, never a priority search.
    fn call_name_use(&mut self, name: &Ident) {
        if let Some(bind) = self.lookup_local(&name.name) {
            self.uses.push((name.span, Res::Local { bind }));
            return;
        }
        let type_def = self.package.lookup_type_in(self.file, &name.name);
        match (self.package.lookup_value(&name.name), type_def) {
            (Some(value), Some(ty)) => {
                // A tag family declaration yields to the value (F9.5):
                // tag names left the value namespace, so the call form
                // always means the fn or const. Every other family is
                // the constructor collision F9 keeps — DS2014, naming
                // both declarations.
                if matches!(self.package.def(ty).kind, DefKind::Tag | DefKind::Case) {
                    self.def_use(name.span, value);
                } else {
                    self.namespace_collision(name, value, ty);
                    // Still record the value declaration so the checker
                    // types the call through and the ambiguity stays the
                    // single diagnostic; the error is fatal.
                    self.def_use(name.span, value);
                }
            },
            (Some(value), None) => self.def_use(name.span, value),
            (None, Some(ty)) => self.def_use(name.span, ty),
            (None, None) => {
                if let Some(def) = self.import_binding(&name.name) {
                    self.uses.push((name.span, Res::Def(def)));
                } else if let Some(module) = self.module_alias(&name.name) {
                    self.uses.push((name.span, Res::Module(module)));
                } else if name.name != "none" {
                    self.unknown_name(name, self.callable_candidates());
                }
            },
        }
    }

    /// A call-form name present in both namespaces: DS2014, naming both
    /// declarations (F9).
    fn namespace_collision(&mut self, name: &Ident, value: DefId, ty: DefId) {
        let value_def = self.package.def(value);
        let ty_def = self.package.def(ty);
        self.diags.push(
            Diagnostic::error(
                ErrorCode::AmbiguousCall,
                format!(
                    "call-form `{}` is ambiguous between the value `{}` and the {} `{}` [F9]",
                    name.name,
                    value_def.name,
                    ty_def.kind.word(),
                    ty_def.name
                ),
            )
            .with_primary(name.span, "ambiguous call form")
            .with_secondary(value_def.span, "the value declaration")
            .with_secondary(ty_def.span, "the type declaration"),
        );
    }

    /// A generic-application constructor `list[page]()` (F9): the head
    /// resolves in the type namespace and the bracketed arguments are type
    /// uses. A head that names no type is an ordinary index expression
    /// (`xs[i]`), whose typing is the checker's.
    fn ctor_apply_use(&mut self, recv: &Expr, index: &Expr) {
        let head = single_path_ident(recv);
        if let Some(head) = head.filter(|id| self.package.lookup_type(&id.name).is_some()) {
            self.type_name_use(head);
            self.ctor_type_arg(index);
        } else {
            self.expr(recv);
            self.expr(index);
        }
    }

    /// The map constructor's type application `map[k, v]` (F9, B3): the
    /// head is a type use and the two bracketed arguments are type uses,
    /// the list application's own convention.
    fn map_apply_use(&mut self, head: &Ident, key: &Expr, value: &Expr) {
        self.type_name_use(head);
        self.ctor_type_arg(key);
        self.ctor_type_arg(value);
    }

    /// One bracketed constructor argument: a type spelled in expression
    /// position — a path is a type use, a tuple walks its items, a fn type
    /// walks its param and return types, anything else falls back to the
    /// expression walk.
    fn ctor_type_arg(&mut self, arg: &Expr) {
        match arg {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                self.type_name_use(&path.segments[0]);
            },
            Expr::Paren { inner, .. } => self.ctor_type_arg(inner),
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.ctor_type_arg(item);
                }
            },
            Expr::FnType { ty, .. } => self.type_(ty),
            _ => self.expr(arg),
        }
    }

    /// A path expression: one segment is a name use; `Trait::f` qualifies
    /// directly (F3); longer paths are module paths (G4, M7).
    fn path_use(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => self.name_use(only),
            [head, method] => {
                let Some(def) = self.package.lookup_type_in(self.file, &head.name) else {
                    // A value name at the qualifier position still gets
                    // the sharper "not a trait" word.
                    if self.package.lookup_value(&head.name).is_some() {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::UnresolvedName,
                                format!(
                                    "`{}` is not a trait; `A::f` is the trait-method escape hatch \
                                     [F3]",
                                    head.name
                                ),
                            )
                            .with_primary(head.span, "not a trait"),
                        );
                        return;
                    }
                    self.unknown_name(head, self.shadowing_candidates());
                    return;
                };
                if self.package.def(def).kind == DefKind::Trait {
                    let candidate = self
                        .package
                        .method_candidates(&method.name)
                        .find(|&id| self.package.method(id).owner == MethodOwner::Trait(def));
                    let Some(id) = candidate else {
                        let others = self
                            .package
                            .method_candidates(&method.name)
                            .filter(|&id| self.package.method(id).owner != MethodOwner::Trait(def))
                            .map(|id| self.package.method(id).owner_name(self.package))
                            .collect::<Vec<_>>();
                        let mut diag = Diagnostic::error(
                            ErrorCode::UnresolvedName,
                            format!("trait `{}` has no method `{}`", head.name, method.name),
                        )
                        .with_primary(method.span, "no such member");
                        if !others.is_empty() {
                            diag = diag.with_note(format!(
                                "method `{}` exists on: {}",
                                method.name,
                                others.join(", ")
                            ));
                        }
                        self.diags.push(diag);
                        return;
                    };
                    self.def_use(head.span, def);
                    self.uses.push((method.span, Res::TraitMethod(id)));
                } else if !self.qualified_label_use(def, head, method) {
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::UnresolvedName,
                            format!(
                                "`{}` is not a trait; `A::f` is the trait-method escape hatch [F3]",
                                head.name
                            ),
                        )
                        .with_primary(head.span, "not a trait"),
                    );
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "multi-segment paths resolve through imports, which land with modules [G4]",
                    )
                    .with_primary(path.span, "module path"),
                );
            },
        }
    }

    /// A brace-construction head `user { ... }` (F9): the type namespace
    /// only; longer paths are module paths (G4, M7).
    fn ctor_path_use(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => {
                if let Some(def) = self
                    .package
                    .lookup_type(&only.name)
                    .or_else(|| self.import_type_binding(&only.name))
                {
                    self.def_use(only.span, def);
                } else if only.name != "none" {
                    let candidates = self.type_candidates();
                    self.unknown_name_at(only, candidates, "struct");
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "qualified struct literals resolve through imports, which land with \
                         modules [G4]",
                    )
                    .with_primary(path.span, "qualified constructor path"),
                );
            },
        }
    }

    /// A UFCS dot-call's method position (F3): free functions first, then
    /// registered methods; the `.err`/`.ok` handler forms are surface
    /// syntax, not names. The dot is a function position, so the free
    /// tier searches the VALUE namespace only (F9). A receiver that is
    /// a bare name resolving through the type namespace takes the
    /// type-keyed tier instead (the receiverless-member ruling): the
    /// value tiers never apply behind a type name.
    fn dot_use(&mut self, recv: &Expr, name: &Ident) {
        if let Some(module) = self.module_ref_receiver(recv) {
            self.module_member_use(module, name);
            return;
        }
        if let Some(target) = self.type_keyed_receiver(recv) {
            self.static_use(target, name);
            return;
        }
        self.dot_use_value(name);
    }

    /// Whether a dot-call's receiver is a bare name this file's import
    /// aliases bound (G4): no local and no package name shadows it —
    /// the module reference is the weakest reading, exactly like the
    /// type-keyed tier's discipline.
    fn module_ref_receiver(&self, recv: &Expr) -> Option<ModuleId> {
        let only = single_path_ident(recv)?;
        if self.lookup_local(&only.name).is_some()
            || self.package.lookup_value(&only.name).is_some()
            || self.package.lookup_type(&only.name).is_some()
            || self.import_binding(&only.name).is_some()
        {
            return None;
        }
        self.module_alias(&only.name)
    }

    /// The member position behind a module alias: the alias's surface
    /// resolves the name — an `@std` alias through the merged member's
    /// own declarations, a linked alias through its member's exported
    /// entries — with the same tier refusals a selection meets.
    fn module_member_use(&mut self, module: ModuleId, name: &Ident) {
        let module_ref = self.package.module_ref(module);
        match &module_ref.surface {
            ModuleSurface::Merged { member } => {
                let found = self.package.lookup_value(&name.name).filter(|&def| {
                    let def_decl = self.package.def(def);
                    def_decl.span.file == *member && def_decl.vis.exported()
                });
                match found {
                    Some(def) => {
                        self.uses
                            .push((name.span, Res::Dot(DotRes::Module { func: def })));
                    },
                    None => {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::ImportNameMissing,
                                format!(
                                    "`{}` names no `pub` definition the module `{}` exports [G4]",
                                    name.name, module_ref.alias
                                ),
                            )
                            .with_primary(name.span, "no such exported name"),
                        );
                    },
                }
            },
            ModuleSurface::Linked { surface, defs } => {
                let Some(entry) = surface.entry(&name.name) else {
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportNameMissing,
                            format!(
                                "`{}` names no definition the module `{}` exports [G4]",
                                name.name, module_ref.alias
                            ),
                        )
                        .with_primary(name.span, "no such exported name"),
                    );
                    return;
                };
                if !entry.vis.exported() {
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportNotExported,
                            format!(
                                "`{}` is `pub(pkg)` or private in its package; only `pub` names \
                                 export                              [G4]",
                                name.name
                            ),
                        )
                        .with_primary(name.span, "not exported to dependents")
                        .with_secondary(entry.decl_span, "declared here"),
                    );
                    return;
                }
                let Some(&func) = defs.get(&name.name) else {
                    // a shape the qualified call cannot serve: the alias
                    // resolves call sites, so a literal const (a value
                    // member since the value seam) and a refused shape
                    // alike name the same boundary here
                    let what = if entry.kind == ForeignKind::Binding && entry.value.is_some() {
                        format!(
                            "the module alias serves callables at call sites; a linked const \
                             reads in value position (`{}.{}`) or binds through its selection \
                             import",
                            module_ref.alias, name.name
                        )
                    } else {
                        "it cannot bind across packages this round".to_owned()
                    };
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportUnsupportedShape,
                            format!(
                                "`{}` names no callable the module serves: {what} [G4]",
                                name.name
                            ),
                        )
                        .with_primary(name.span, "not bindable through the alias")
                        .with_secondary(entry.decl_span, "declared here"),
                    );
                    return;
                };
                self.uses
                    .push((name.span, Res::Dot(DotRes::Module { func })));
            },
        }
    }

    /// A module alias's qualified member in value position (G4): a
    /// linked package's fact-carrying const binds through the alias
    /// exactly as its selection spelling does — the same def, so every
    /// later pass embeds the literal — while every other member keeps
    /// its refusal at this seam. The merged-ambient alias keeps the
    /// call-only reading: its member file's names are the merged
    /// namespace's own, never import-bound defs.
    fn module_member_value_use(&mut self, recv: &Expr, module: ModuleId, name: &Ident) {
        let linked = match &self.package.module_ref(module).surface {
            ModuleSurface::Linked { surface, .. } => Some(surface.clone()),
            ModuleSurface::Merged { .. } => None,
        };
        self.expr(recv);
        let Some(surface) = linked else {
            return;
        };
        let module_alias = self.package.module_ref(module).alias().to_owned();
        let Some(entry) = surface.entry(&name.name) else {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ImportNameMissing,
                    format!(
                        "`{}` names no definition the module `{module_alias}` exports [G4]",
                        name.name
                    ),
                )
                .with_primary(name.span, "no such exported name"),
            );
            return;
        };
        if !entry.vis.exported() {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ImportNotExported,
                    format!(
                        "`{}` is `pub(pkg)` or private in its package; only `pub` names \
                         export                              [G4]",
                        name.name
                    ),
                )
                .with_primary(name.span, "not exported to dependents")
                .with_secondary(entry.decl_span, "declared here"),
            );
            return;
        }
        let Some(&def) = self
            .package
            .foreign_const_defs
            .get(&(entry.link, entry.name.clone()))
        else {
            let what = match entry.kind {
                ForeignKind::Binding => "its value is no const fact — a `var` binding evaluates \
                                         in the linked module and no per-target value exists for \
                                         it; its selection import keeps the same refusal"
                    .to_owned(),
                ForeignKind::Fn => "a linked function serves through the alias at its call sites \
                                    alone; a function value has no callable word this round"
                    .to_owned(),
                ForeignKind::Type => "a linked type is a type-namespace name; its selection \
                                      import binds the name in type position"
                    .to_owned(),
            };
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ImportUnsupportedShape,
                    format!(
                        "`{}` names no value member the module serves: {what} [G4]",
                        name.name
                    ),
                )
                .with_primary(name.span, "not bindable through the alias")
                .with_secondary(entry.decl_span, "declared here"),
            );
            return;
        };
        self.uses.push((name.span, Res::Def(def)));
    }

    /// Whether a dot-call's receiver is a bare name resolving through the
    /// type namespace: no local and no value-namespace def shadows it
    /// (the ordinary name-use priority), and a type-namespace def names
    /// it — the package's own declarations first, the file's import
    /// bindings below, the same priority a type position spells (G4).
    /// Such a receiver is a type, not a value — the type-keyed call tier
    /// owns it, so a linked type name refuses its member calls at the
    /// check instead of falling to the value tiers. An instantiated
    /// application takes the same tier: the receiver's head (a
    /// constructor-bracket application or a carried application type)
    /// names the type whose own impl members the call dispatches.
    fn type_keyed_receiver(&self, recv: &Expr) -> Option<DefId> {
        if let Some(only) = single_path_ident(recv) {
            if self.lookup_local(&only.name).is_some() {
                return None;
            }
            let def = self
                .package
                .lookup_type(&only.name)
                .or_else(|| self.import_type_binding(&only.name))?;
            return self
                .package
                .def(def)
                .kind
                .in_type_namespace()
                .then_some(def);
        }
        self.applied_receiver_head(recv)
    }

    /// The head's def of an instantiated type-keyed receiver — a type
    /// application in the receiver position: the one-argument bracket
    /// (`stack[int]`), the two-argument bracket (the grammar spells it
    /// `map[k, v]`; a synthesized tree carries any head), or a written
    /// application type carried by the fn-type node (the spelling the
    /// brackets cannot express: three or more arguments, an optional
    /// argument). The head names a type with no local shadow, the
    /// package's namespace first and the file's import bindings below —
    /// exactly the bare form's discipline.
    fn applied_receiver_head(&self, recv: &Expr) -> Option<DefId> {
        let head = match recv {
            Expr::Index { recv, .. } => single_path_ident(recv)?,
            Expr::MapApply { head, .. } => head,
            Expr::FnType { ty, .. } => {
                let Type::Path { path, args, .. } = ty.as_ref() else {
                    return None;
                };
                if path.segments.len() != 1 || args.is_empty() {
                    return None;
                }
                &path.segments[0]
            },
            _ => return None,
        };
        if self.lookup_local(&head.name).is_some() {
            return None;
        }
        let def = self
            .package
            .lookup_type(&head.name)
            .or_else(|| self.import_type_binding(&head.name))?;
        self.package
            .def(def)
            .kind
            .in_type_namespace()
            .then_some(def)
    }

    /// The receiver walk of a dot-call: an instantiated type-keyed
    /// receiver whose head names a type — the package's declarations or
    /// the file's import bindings — walks as type uses (the
    /// constructor-bracket convention), while every other shape — the
    /// map application and a carried fn type included, whose own arms
    /// already read as type uses — keeps the expression walk.
    fn dot_recv_use(&mut self, recv: &Expr) {
        let Expr::Index {
            recv: head, index, ..
        } = recv
        else {
            self.expr(recv);
            return;
        };
        let Some(head) = single_path_ident(head).filter(|id| {
            self.lookup_local(&id.name).is_none()
                && (self.package.lookup_type(&id.name).is_some()
                    || self.import_type_binding(&id.name).is_some())
        }) else {
            self.expr(recv);
            return;
        };
        self.type_name_use(head);
        self.ctor_type_arg(index);
    }

    /// The method position behind a type-name receiver: the type's own
    /// inherent impl members with that name claim the spelling — the
    /// type-keyed tier, resolved at check time with no instance word. A
    /// tag or case name that names no member of its own type keeps its
    /// other reading (the constructor value: `tone.show()` dispatches
    /// the trait impl over the tag's value), so the ordinary value
    /// tiers serve it; any other type names its members alone.
    fn static_use(&mut self, target: DefId, name: &Ident) {
        let type_name = self.package.def(target).name.clone();
        // one pass serves both halves: the owner's own members claim the
        // call, and the rest feed the "exists on" note when none do
        let (candidates, others): (Vec<MethodId>, Vec<MethodId>) =
            self.package.method_candidates(&name.name).partition(|&id| {
                matches!(
                    &self.package.method(id).owner,
                    MethodOwner::Inherent(owner) if *owner == type_name
                )
            });
        if candidates.is_empty() {
            if matches!(self.package.def(target).kind, DefKind::Tag | DefKind::Case) {
                self.dot_use_value(name);
                return;
            }
            let others = others
                .iter()
                .map(|&id| self.package.method(id).owner_name(self.package))
                .collect::<Vec<_>>();
            let mut diag = Diagnostic::error(
                ErrorCode::UnresolvedName,
                format!("type `{type_name}` carries no member `{}`", name.name),
            )
            .with_primary(name.span, "not a member of this type")
            .with_note(
                "a bare type name as the receiver calls that type's own receiverless impl \
                 members; other spellings need a value receiver",
            );
            if !others.is_empty() {
                diag = diag.with_note(format!(
                    "method `{}` exists on: {}",
                    name.name,
                    others.join(", ")
                ));
            }
            self.diags.push(diag);
            return;
        }
        self.uses
            .push((name.span, Res::Dot(DotRes::Static { candidates })));
    }

    /// The ordinary value tiers behind the dot [F3, F9]: the handler
    /// forms, then free functions, then every registered method
    /// candidate. A receiver that reaches here carries a value reading
    /// (a local, a value def, or a tag/case constructor value).
    fn dot_use_value(&mut self, name: &Ident) {
        if name.name == "err" || name.name == "ok" {
            self.uses.push((name.span, Res::Handler));
            return;
        }
        if let Some(func) = self.package.lookup_value(&name.name) {
            // Capability parameters have no syntax yet, so the receiver
            // feeds parameter 0 — the first data parameter [F3].
            self.enforce_vis(name.span, func);
            self.uses
                .push((name.span, Res::Dot(DotRes::Free { func, fed_param: 0 })));
            return;
        }
        let candidates: Vec<MethodId> = self.package.method_candidates(&name.name).collect();
        if candidates.is_empty() {
            let mut diag = Diagnostic::error(
                ErrorCode::UnresolvedName,
                format!("no name `{}` behind the dot", name.name),
            )
            .with_primary(name.span, "not a free function, not a method")
            .with_note("tried free functions first, then trait and inherent methods [F3]");
            if let Some(near) = nearest(&name.name, self.callable_candidates()) {
                diag = diag.with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: near,
                });
            }
            self.diags.push(diag);
            return;
        }
        self.uses
            .push((name.span, Res::Dot(DotRes::Methods { candidates })));
    }

    // ---- type positions ----------------------------------------------------

    fn type_(&mut self, ty: &Type) {
        match ty {
            Type::Path { path, args, .. } => {
                match path.segments.as_slice() {
                    [only] => self.type_name_use(only),
                    [head, tail] => {
                        let routed = self
                            .package
                            .lookup_type_in(self.file, &head.name)
                            .is_some_and(|def| self.qualified_label_use(def, head, tail));
                        if !routed {
                            self.diags.push(
                                Diagnostic::error(
                                    ErrorCode::UnresolvedName,
                                    "qualified type paths resolve through imports, which land \
                                     with modules [G4]",
                                )
                                .with_primary(path.span, "qualified type path"),
                            );
                        }
                    },
                    _ => {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::UnresolvedName,
                                "qualified type paths resolve through imports, which land with \
                                 modules [G4]",
                            )
                            .with_primary(path.span, "qualified type path"),
                        );
                    },
                }
                for arg in args {
                    self.type_(arg);
                }
            },
            Type::Optional { inner, .. }
            | Type::Negation { inner, .. }
            | Type::Paren { inner, .. }
            | Type::Ptr { inner, .. } => self.type_(inner),
            Type::Union { left, right, .. }
            | Type::Intersect { left, right, .. }
            | Type::Difference { left, right, .. } => {
                self.type_(left);
                self.type_(right);
            },
            Type::Tuple { items, .. } => {
                for item in items {
                    self.type_(item);
                }
            },
            Type::Fn { params, ret, .. } => {
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.type_(ty);
                    }
                }
                if let Some(ret) = ret {
                    self.type_(ret);
                }
            },
            Type::Unit(_) | Type::TypeKw(_) | Type::Foreign { .. } => {},
        }
    }

    fn type_name_use(&mut self, name: &Ident) {
        if let Some(bind) = self.lookup_type_var(&name.name) {
            self.uses.push((name.span, Res::Local { bind }));
        } else if let Some(def) = self.package.lookup_type_in(self.file, &name.name) {
            self.def_use(name.span, def);
        } else if name.name == "none" {
            // `none` is the universe's distinguished unit atom [B4], not a
            // package declaration; the checker solves it directly.
        } else if name.name == "self" {
            // `self` as a type name is retired; the receiver type is
            // spelled `Self` [F2, record 6.1].
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::LowercaseSelfType,
                    "`self` is retired as a type name; the receiver type is spelled `Self`",
                )
                .with_primary(name.span, "type name `self`")
                .with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: "Self".to_owned(),
                }),
            );
        } else if let Some(def) = self.import_type_binding(&name.name) {
            // a linked type's selection (G4): the sentinels above keep
            // their readings before any imported spelling
            self.def_use(name.span, def);
        } else {
            let candidates = self.type_candidates();
            self.unknown_name_at(name, candidates, "type");
        }
    }

    // ---- patterns ----------------------------------------------------------

    /// Binds a pattern's binders into the current scope and resolves its
    /// case paths. `or_rebind` allows identical rebinding across
    /// or-pattern alternatives (F4).
    fn bind_pattern(&mut self, pat: &Pattern, or_rebind: bool) {
        self.pattern_at(pat, or_rebind, false);
    }

    /// A match-arm pattern: sub-patterns are member tests first, bindings
    /// second (F9).
    fn arm_pattern(&mut self, pat: &Pattern) {
        self.pattern_at(pat, false, true);
    }

    /// One pattern walk shared by binding declarations (`arm == false`)
    /// and match-arm tests (`arm == true`): in a test position a single
    /// identifier that names a constructor type (or the predeclared
    /// `none`) is a member test that binds nothing — bare tags are
    /// constructor values, so the type namespace wins over any
    /// same-named local; identifiers with no constructor entry bind.
    fn pattern_at(&mut self, pat: &Pattern, or_rebind: bool, arm: bool) {
        match pat {
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. } => {},
            Pattern::Binding { name, .. } => match self.arm_tag(name, arm) {
                ArmTag::Binding => self.bind(name, or_rebind),
                ArmTag::BuiltinNone => {},
                ArmTag::Def(def) => {
                    self.def_use(name.span, def);
                },
            },
            // Pattern paths are item references, not locals.
            Pattern::Path { path, .. } => self.pattern_path(path),
            Pattern::Call { path, args, .. } => {
                self.pattern_path(path);
                for arg in args {
                    self.pattern_at(arg, or_rebind, arm);
                }
            },
            Pattern::Struct { path, fields, .. } => {
                self.pattern_path(path);
                for (name, field) in fields {
                    match field {
                        // Punning binds the field name.
                        None => self.bind(name, or_rebind),
                        Some(sub) => self.pattern_at(sub, or_rebind, arm),
                    }
                }
            },
            Pattern::Tuple { pats, .. } => {
                for sub in pats {
                    self.pattern_at(sub, or_rebind, arm);
                }
            },
            Pattern::Or { pats, .. } => {
                for sub in pats {
                    self.pattern_at(sub, true, arm);
                }
            },
        }
    }

    /// The type-namespace entry a bare arm identifier names (F9): the
    /// predeclared `none`, a constructor type's def, or nothing (a
    /// binding). Traits and aliases are sets, not constructors.
    fn arm_tag(&self, name: &Ident, arm: bool) -> ArmTag {
        if arm && name.name == "none" && !self.package.declares("none") {
            return ArmTag::BuiltinNone;
        }
        if !arm {
            return ArmTag::Binding;
        }
        self.package
            .lookup_type_in(self.file, &name.name)
            .or_else(|| self.import_type_binding(&name.name))
            .filter(|&def| self.package.def(def).kind.is_constructor())
            .map_or(ArmTag::Binding, ArmTag::Def)
    }

    /// Pattern paths are constructor references (F9): the type namespace
    /// only.
    fn pattern_path(&mut self, path: &Path) {
        let candidates = || {
            self.package
                .defs
                .iter()
                .filter(|def| def.kind.in_type_namespace())
                .map(|def| def.name.clone())
                .collect::<Vec<_>>()
        };
        match path.segments.as_slice() {
            [only] => {
                if let Some(def) = self
                    .package
                    .lookup_type_in(self.file, &only.name)
                    .or_else(|| self.import_type_binding(&only.name))
                {
                    self.def_use(only.span, def);
                } else {
                    let candidates = candidates();
                    self.unknown_name_at(only, candidates, "pattern");
                }
            },
            // The tail is a case name; case tables are the checker's.
            // A tag-family head routes its label through the qualified
            // spelling instead (F9.5).
            [head, tail] => {
                if let Some(def) = self
                    .package
                    .lookup_type_in(self.file, &head.name)
                    .or_else(|| self.import_type_binding(&head.name))
                {
                    if self.package.def(def).kind == DefKind::Enum {
                        self.def_use(head.span, def);
                    } else if !self.qualified_label_use(def, head, tail) {
                        let candidates = candidates();
                        self.unknown_name_at(head, candidates, "pattern");
                    }
                } else {
                    let candidates = candidates();
                    self.unknown_name_at(head, candidates, "pattern");
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "multi-segment pattern paths are not part of the surface [F4]",
                    )
                    .with_primary(path.span, "too many segments"),
                );
            },
        }
    }

    // ---- scopes ----------------------------------------------------------

    fn bind(&mut self, name: &Ident, rebind_ok: bool) {
        let scope = self.scopes.last_mut().expect("a scope is always open");
        match scope.get(&name.name) {
            Some(&first) if !rebind_ok => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::DuplicateDefinition,
                        format!("`{}` is already bound in this scope", name.name),
                    )
                    .with_secondary(first, "first bound here")
                    .with_primary(name.span, "bound again here"),
                );
            },
            _ => {
                scope.insert(name.name.clone(), name.span);
            },
        }
    }

    /// Records a binder in `scope`, reporting same-scope duplicates.
    fn record_dup(&mut self, scope: &mut FxHashMap<String, Span>, name: &Ident) {
        if let Some(&first) = scope.get(&name.name) {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("`{}` is already bound in this scope", name.name),
                )
                .with_secondary(first, "first bound here")
                .with_primary(name.span, "bound again here"),
            );
            return;
        }
        scope.insert(name.name.clone(), name.span);
    }

    fn lookup_local(&self, name: &str) -> Option<Span> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn lookup_type_var(&self, name: &str) -> Option<Span> {
        self.type_scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
    }

    // ---- diagnostics -------------------------------------------------------

    /// The qualified `type::label` routing (F9.5): a type-family head
    /// (tag, case, enum, alias) routes its label to the declaring
    /// declaration the HEAD's own file names — the escape hatch the bare
    /// label's ambiguity refusal points at. Membership in the head's set
    /// is the checker's universe work, never the resolver's. Returns
    /// whether the pair took the route (the caller keeps its own
    /// refusal otherwise).
    fn qualified_label_use(&mut self, head_def: DefId, head: &Ident, tail: &Ident) -> bool {
        if !matches!(
            self.package.def(head_def).kind,
            DefKind::Tag | DefKind::Case | DefKind::Enum | DefKind::TypeAlias
        ) {
            return false;
        }
        let head_file = self.package.def(head_def).span.file;
        self.def_use(head.span, head_def);
        if let Some(label_def) = self
            .package
            .lookup_type_in(head_file, &tail.name)
            .filter(|&d| matches!(self.package.def(d).kind, DefKind::Tag | DefKind::Case))
        {
            self.def_use(tail.span, label_def);
        } else {
            self.unknown_name(tail, self.shadowing_candidates());
        }
        true
    }

    fn unknown_name(&mut self, name: &Ident, candidates: Vec<String>) {
        self.unknown_name_at(name, candidates, "name");
    }

    fn unknown_name_at(&mut self, name: &Ident, candidates: Vec<String>, what: &str) {
        let mut diag = Diagnostic::error(
            ErrorCode::UnresolvedName,
            format!("unknown {} `{}`", what, name.name),
        )
        .with_primary(name.span, "not found");
        if let Some(near) = nearest(&name.name, candidates) {
            diag = diag
                .with_note(format!("did you mean `{near}`?"))
                .with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: near,
                });
        }
        self.diags.push(diag);
    }

    /// Names visible for suggestions at a value use: package names plus
    /// every local in scope.
    fn shadowing_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self.package.defs().map(|def| def.name.clone()).collect();
        for scope in &self.scopes {
            names.extend(scope.keys().cloned());
        }
        names
    }

    /// Names visible at a type use: package names plus type variables.
    fn type_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self.package.defs().map(|def| def.name.clone()).collect();
        for scope in &self.type_scopes {
            names.extend(scope.keys().cloned());
        }
        names
    }

    /// Callable-ish names for dot-call suggestions: value-namespace
    /// declarations plus registered method names.
    fn callable_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self.package.value_names.keys().cloned().collect();
        names.extend(self.package.method_index.keys().cloned());
        if let Some(base) = self.package.base.as_deref() {
            names.extend(base.value_names.keys().cloned());
            names.extend(base.method_index.keys().cloned());
        }
        names
    }
}

/// The identifier of a single-segment path expression, if it is one.
fn single_path_ident(expr: &Expr) -> Option<&Ident> {
    let Expr::Path { path, .. } = expr else {
        return None;
    };
    match path.segments.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

impl MethodDecl {
    /// The human-readable owner name for diagnostics: the trait's name or
    /// the inherent impl's target type.
    fn owner_name(&self, package: &Package) -> String {
        match &self.owner {
            MethodOwner::Trait(id) => package.def(*id).name.clone(),
            MethodOwner::Inherent(target) => format!("inherent impl `{target}`"),
        }
    }
}

/// The owner identity of an inherent impl: the target's head path name
/// (`impl stack[t]` owns `stack`). Any other written target (a union, a
/// qualified path) keys by its rendered spelling; such an owner names no
/// single declared type, so its methods match no receiver and the call
/// reports at the dot — the honest refusal.
fn inherent_owner(target: &Type) -> String {
    match target {
        Type::Paren { inner, .. } => inherent_owner(inner),
        Type::Path { path, .. } if path.segments.len() == 1 => path.segments[0].name.clone(),
        other => crate::decls::render_type(other),
    }
}

/// The nearest candidate by edit distance; ties break to the
/// lexicographically smallest name so the suggestion is deterministic.
fn nearest(name: &str, mut candidates: Vec<String>) -> Option<String> {
    candidates.sort();
    let threshold = (name.len() / 3).max(1);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(name, &candidate), candidate))
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
        .and_then(|(distance, candidate)| (distance <= threshold).then_some(candidate))
}

/// Levenshtein edit distance over bytes; small strings only (identifiers).
fn edit_distance(a: &str, b: &str) -> usize {
    let b_bytes = b.as_bytes();
    let mut previous: Vec<usize> = (0..=b_bytes.len()).collect();
    let mut current = vec![0; b_bytes.len() + 1];
    for (i, a_byte) in a.as_bytes().iter().enumerate() {
        current[0] = i + 1;
        for (j, b_byte) in b_bytes.iter().enumerate() {
            let cost = usize::from(a_byte != b_byte);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        core::mem::swap(&mut previous, &mut current);
    }
    previous[b_bytes.len()]
}
