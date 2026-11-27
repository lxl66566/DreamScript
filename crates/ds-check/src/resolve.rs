//! Package-level name resolution over the parsed surface.
//!
//! A [`Package`] collects the declarations of its files into TWO
//! namespaces: a type namespace (structs, enums, enum cases,
//! tags, aliases, traits) and a value namespace (functions, const/var
//! bindings). Files in
//! one package share both, there are zero intra-package imports, and
//! top-level definitions are order-independent — collection inserts every
//! declaration before any use is resolved, so intra-file and cross-file
//! definition cycles are legal by construction (resolution is name lookup,
//! not dependency ordering; the "whole-package fixpoint" is exactly this
//! two-pass shape).
//!
//! # Positions that do not resolve here
//!
//! Bare field accesses (`x.f` without a call) are struct-owned and
//! type-directed — the checker resolves them. `.err(h)`/`.ok(h)` are
//! decided surface handler forms, not names, and are
//! recorded as [`Res::Handler`]. Imports declare no namespace names:
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

mod file;
mod package;
mod query;

use file::*;
use package::*;
pub use package::{
    FileImports, ModuleRef, Package, PackageBase, resolve_package, resolve_package_linked,
};

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
    /// bindings whose value is a function).
    Const,
    /// A `var` binding.
    Var,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A unit tag.
    Tag,
    /// A case minted by an enum block: a member of its enum's
    /// case set, constructed per its payload. The label claims no
    /// entry of the package's shared type namespace — its def lives
    /// in the case namespace, so a struct, trait, or alias of the
    /// same label coexists beside it.
    Case,
    /// A trait.
    Trait,
    /// A type alias.
    TypeAlias,
}

impl DefKind {
    /// Whether the kind declares into the package's shared type
    /// namespace; the rest declare into the value namespace or, for a
    /// case, the case namespace beside it. Cases left the shared
    /// namespace: a bare case label resolves
    /// type-directed in arms and `is` tests, and every other spelling
    /// rides the qualified `enum::case` form.
    #[must_use]
    pub const fn in_type_namespace(self) -> bool {
        matches!(
            self,
            Self::Struct | Self::Enum | Self::Tag | Self::Trait | Self::TypeAlias
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

/// The visibility tier of one package-level definition.
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
    /// The tier a declaration's modifier spells: absent is private.
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
    /// The declared visibility tier.
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
    /// declaring file, and type names are package-unique (the first
    /// def wins), so the name IS the owner identity. The checker resolves
    /// the name to the owner's atom at call sites and selects by
    /// (owner, name).
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
    /// The `Trait::f` escape hatch: the named trait's member.
    TraitMethod(MethodId),
    /// The callee name of a UFCS dot-call.
    Dot(DotRes),
    /// The `.err(h)`/`.ok(h)` handler surface form;
    /// no name sits behind it.
    Handler,
    /// A module alias an import binds (`import p as c`): the receiver of
    /// a module-qualified call, never a value. The module ref lives in
    /// the package's table the id indexes.
    Module(ModuleId),
}

/// The resolution of a dot-call's method position.
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
    /// The receiver names a type: the
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
    /// No free function and no registered member answers the name: the
    /// call may still be the receiver's fn-typed field called through
    /// the dot. The resolver cannot type the receiver, so this row is
    /// the marker the checker decides on — the receiver's single-struct
    /// type carrying a fn-typed field of the name turns the spelling
    /// into an indirect call of the field's word, and every other
    /// receiver takes the refused reading there (the diagnostic lives
    /// with the decision, never here).
    Field,
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
                    // The field tier's name is the receiver's field
                    // label, no value-namespace target.
                    Res::Dot(DotRes::Methods { .. } | DotRes::Static { .. } | DotRes::Field)
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

/// The nearest candidate by edit distance; ties break to the
/// lexicographically smallest name so the suggestion is deterministic.
pub(crate) fn nearest(name: &str, mut candidates: Vec<String>) -> Option<String> {
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
