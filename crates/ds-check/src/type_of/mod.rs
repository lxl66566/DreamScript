//! The `type_of` pass: bidirectional checking with monotone flow refinement.
//!
//! # Engine shape
//!
//! Boundary anchoring: top-level signatures are REQUIRED and the
//! set algebra checks against them by containment; locals, lambdas, and
//! match arms infer. Inference failure is a compile error — there is no
//! `any` fallback anywhere. Every type is a solved [`Ty`] from the
//! package universe; nothing unsolved ever enters an environment.
//!
//! The engine is one expression walk `expr(e, expected) -> Val`. The
//! `expected` hint is consumed exactly where inference cannot proceed
//! without it (lambda parameters, `?` splits, generic construction) and
//! ignored everywhere else; containment is checked explicitly at anchors
//! (call arguments, field values, returns, annotated bindings). `Val`
//! carries a `diverges` flag (early exits) so `if`/`match` unify over the
//! arms that actually produce a value.
//!
//! # Flow refinement
//!
//! Refinement is a per-binder overlay `refined: binder span -> Ty` over
//! declared types, narrowed monotonically by intersection: `is` tests,
//! none-comparisons, and match-arm patterns (through the scrutinee binder
//! and the arm's pattern coverage). The overlay resets at assignment
//! and at loop entry (sound: refined is always contained in declared, so
//! the declared type is a legal widening). Known limit: no narrowing
//! through mutable fields of union-typed values.
//!
//! # Union errors
//!
//! `?` splits its operand against the success context: at an expected type
//! the members outside it propagate, elsewhere the members INSIDE the
//! enclosing return union propagate — either way every propagated member
//! must be contained in the written return union (DS2008). `or` extracts
//! the wanted members (the right operand's type for a plain tail, the
//! return union for `or return`); `.err(h)`/`.ok(h)` are difference-typed:
//! the handled set is the handler parameter when annotated, otherwise
//! everything but what the handler's own result keeps — and that set is
//! the handler parameter's expected type.
//!
//! # Out of scope here
//!
//! Effect rows; the affine/move pass (assumption bounds like `[t: json]`
//! feed method selection, `~Copyable` belongs to it); ranges, indexing, and
//! `in`-containment (they need the ambient stdlib's type vocabulary).

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use ds_diag::{Diagnostic, ErrorCode, FileId, Files, LabelStyle, Span};
use ds_parse::{
    Ast, BinaryOp, BlockExpr, Delim, EnumPayload, Expr, FnDecl, Ident, Item, ItemKind, Literal,
    LocalDecl, MatchArm, Param, Path, Pattern, Stmt, StrPart, TraitMember, Type, TypeParam,
    UnaryOp,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    TypeExpr,
    atom::{AtomKind, TraitId, TypeId},
    decls::{self, Decls, FnSig, GenericParam, ParamSig},
    error::Error,
    extraction::Extraction,
    instance::{Condition, InstanceId},
    instance_solve::Resolution,
    resolve::{DefId, DefKind, DotRes, MethodId, MethodOwner, Package, Res, Resolutions, nearest},
    universe::Ty,
};

/// A typed expression: its solved type, whether evaluation always exits
/// early (return/break/continue), so arm unification can skip it, and
/// whether its own subtree already reported a diagnostic.
#[derive(Clone, Copy, Debug)]
pub struct Val {
    /// The solved type of the expression.
    pub ty: Ty,
    /// Whether every path through the expression exits early.
    pub diverges: bool,
    /// Whether any diagnostic was reported inside this expression: the
    /// expression-error epoch. Enclosing positions consume a poisoned
    /// value without re-reporting, so the root cause stands alone;
    /// sibling positions check their own subtree and stay honest.
    pub poisoned: bool,
}

/// One local binding: declared type plus assignability.
#[derive(Clone, Copy, Debug)]
struct Binding {
    ty: Ty,
    mutable: bool,
}

/// One enclosing for-in's iterated binding: its resolved identity plus
/// the iterable's own use in the loop head. The loop froze its element
/// count at entry, so a body `mut` window over the same binding refuses.
#[derive(Clone, Copy)]
struct IterWindow {
    bind: IterBind,
    iter_at: Span,
}

/// A binding's resolved identity, shared by the loop head and the body's
/// `mut` window site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IterBind {
    /// A local binder by its introducing span.
    Local(Span),
    /// A top-level binding by its definition.
    Def(DefId),
}

/// One enclosing nursery block's scope entry: the local binder spans
/// live when the block opened — exactly the bindings whose lifetime
/// outlives the block, since every binder the block itself introduces
/// dies with it after the join.
#[derive(Clone)]
struct NurseryScope {
    outlived: Vec<Span>,
    at: Span,
}

/// One scope boundary's rollback point: both binder tables' undo-log
/// marks, the assumption stack's length, and the enclosing return union.
/// Cheap by construction — restoring replays only what the scope changed.
#[derive(Clone, Copy)]
struct ScopeMark {
    locals: usize,
    refined: usize,
    assumptions: usize,
    ret: Option<Ty>,
}

/// One narrowing fact harvested from a condition.
struct Narrow {
    /// The narrowed binder.
    bind: Span,
    /// Narrow into `ty` (true) or away from it (false).
    keep: bool,
    /// The narrowing type.
    ty: Ty,
}

/// The outcome of one interpolation hole's `show` proof: dispatched
/// (the solved instance records at the hole's own span), refused (no
/// candidate serves, the diagnostic answers), or reported (an
/// ambiguity or bound seam already answered in its own words).
enum HoleShow {
    Dispatched,
    Refused,
    Reported,
}

/// The per-call-site runtime-evidence record of an inherent call
/// entering a generic impl's body: one entry per declared bound — the
/// bound's variable atom, its trait, and the unique instance serving the
/// substituted type at the site (`None` when none serves it uniquely).
pub type InherentCallBounds = Vec<(TypeId, TraitId, Option<InstanceId>)>;

/// Every interpolation hole that staged an implicit `show` dispatch,
/// keyed by the hole's own span: the dispatched member (the solved trait
/// member, the inherent method, or the trait member an in-scope
/// assumption answers — the runtime-evidence form). The effects pass and
/// the lowering's demand walk take the table by this name.
pub type HoleShowDispatches = FxHashMap<Span, MethodId>;

/// The per-call-site runtime-evidence record of a call into a free
/// generic fn's body: one entry per trait-bound generic parameter whose
/// substitution solved — the parameter's minted variable atom, its
/// trait, the unique instance serving the solved type at the site
/// (`None` when none serves it uniquely: the runtime-resolved form whose
/// words read the calling frame's own evidence slots), and the solved
/// type itself (the follow chain's substitution start and the runtime
/// form's receiver fact).
pub type GenericCallBounds = Vec<(TypeId, TraitId, Option<InstanceId>, Ty)>;

/// One step of a place-store chain (the assignment sugar): a field
/// projection or an index over the ambient
/// collections. The step carries its solved type so the lowering walks
/// the chain without re-deriving sub-place types no span records.
#[derive(Clone, Debug)]
pub enum PlaceStep {
    /// A field of the receiver struct, by its declared name; `ty` the
    /// field's solved type at this position of the chain.
    Field {
        /// The field's declared name.
        name: String,
        /// The field's solved type.
        ty: Ty,
    },
    /// An index over a list or map: the subscript assignment lowers to
    /// the collection's `set` member, and a nested chain reads the
    /// element back through the checked read first. `elem` is the
    /// element (a list's) or value (a map's) type.
    Index {
        /// The collection the receiver names.
        native: StoreIndex,
        /// The element (a list's) or value (a map's) type.
        elem: Ty,
    },
}

/// The collection a subscript store addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreIndex {
    /// A list receiver: `list_set`, the probe an int index.
    List,
    /// A map receiver: `map_set`, the probe a key word.
    Map,
}

/// The `for comptime` loop's projected iterable: the written struct
/// type's fields (names and nominal tag strings), or the fn's own type
/// variable the iterable names — the per-instance marker.
enum Projection {
    /// A written concrete struct's field names and their tag strings.
    Written(Vec<String>, Vec<String>),
    /// The fn's own type variable the iterable names.
    PerInstance(Ty),
}

/// One copy's checked records of a `for comptime` body (the comptime
/// field seam): the checker unrolls the loop over its projected fields,
/// and the copies share their source spans, so every fact a copy's walk
/// records lands keyed by the copy's path and the span. A body-wide
/// table cannot hold them; the lowering reads a span through the active
/// copy's record first, the body-wide table second. A field stays
/// `None` when the walk recorded none there.
#[derive(Clone, Debug, Default)]
pub struct CopyRecords {
    /// The expression's checked value.
    pub val: Option<Val>,
    /// The binder fact a pattern inside the copy recorded.
    pub binder: Option<crate::linearity::BinderFact>,
    /// The solved generic-argument types a call inside the copy carries.
    pub generic_call: Option<Vec<Ty>>,
    /// The trait-method dispatch a call inside the copy solved.
    pub trait_call: Option<TraitCall>,
    /// The solved tested type of an `is` inside the copy.
    pub is_test: Option<Ty>,
    /// The place chain an assignment inside the copy recorded.
    pub place_store: Option<PlaceStore>,
    /// The solved parameter types of a member call inside the copy.
    pub member_params: Option<Vec<Ty>>,
    /// The handled member set of a handler site inside the copy.
    pub handler_handled: Option<Ty>,
    /// The inherent method a dot-call inside the copy selected.
    pub inherent_call: Option<MethodId>,
    /// The interpolation hole's implicit show dispatch inside the copy.
    pub hole_show: Option<MethodId>,
    /// The atom a directed label use inside the copy resolved to.
    pub directed_tag: Option<TypeId>,
    /// The bound answers a generic-body call inside the copy carries.
    pub generic_bounds: Option<GenericCallBounds>,
    /// The bound answers an inherent generic-impl call inside the copy.
    pub inherent_bounds: Option<InherentCallBounds>,
}

/// The projected fields of one `for comptime` loop: the declaring
/// struct's field names in declaration order, and the pattern binder's
/// span. The lowering and the affine walk read the count and the
/// per-copy field names from it. A loop over the fn's own type variable
/// carries no fields of its own — the expansion is a per-instance fact
/// the entering sites' walks project, keyed by the walked fn's
/// definition and solved arguments in the instance tables.
#[derive(Clone, Debug)]
pub struct ComptimeLoop {
    /// The declaring struct's field names, in declaration order: one
    /// copy per name, the copy's own constants derived from it. Empty
    /// exactly on a per-instance loop.
    pub fields: Vec<String>,
    /// Each field's nominal tag name (the type's own name when the
    /// field type is one bare atom, empty when composite), aligned with
    /// `fields`.
    pub tags: Vec<String>,
    /// The fn's own type variable the iterable names, on a per-instance
    /// loop; `None` on a written one.
    pub per_instance: Option<Ty>,
    /// The pattern binder's span the record projects through.
    pub binder: Span,
}

/// The solved generic arguments one per-instance expansion walks under:
/// the walked fn's definition plus the entering site's solved argument
/// types, in declaration order. Two sites solving the same arguments
/// share one expansion, exactly as they share one instance body.
pub type InstanceScope = (DefId, Rc<Vec<Ty>>);

/// Where one record write lands.
enum RecordScope {
    /// The body-wide table.
    Body,
    /// A written `for comptime` loop's copy, by copy path.
    Written(Vec<u32>),
    /// A per-instance expansion's copy, by the walked fn's definition,
    /// the solved arguments, and the copy path.
    Instance(DefId, Rc<Vec<Ty>>, Vec<u32>),
}

/// The per-instance expansion one body walk runs under: the walked fn's
/// definition and the entering site's solved argument types. The walk
/// pushes these as the fn's generic scope instead of minting fresh
/// variables, so a loop over the fn's own type variable projects the
/// instance's fields, and every record write routes to the instance
/// tables.
#[derive(Clone)]
struct InstanceRun {
    def: DefId,
    args: Rc<Vec<Ty>>,
}

/// One checked assignment place chain (the assignment sugar): the root
/// binder plus its projection steps, recorded at the assignment's own
/// span for the lowering to synthesize the store.
#[derive(Clone, Debug)]
pub struct PlaceStore {
    /// The root's introducing binder span.
    pub binder: Span,
    /// The root's solved type at the assignment (the flow-refined type,
    /// a narrowing's narrowed member included).
    pub root_ty: Ty,
    /// The projection steps, outermost first.
    pub steps: Vec<PlaceStep>,
}

/// The result of one package check: diagnostics in walk order plus the
/// memoized type of every top-level binding.
#[derive(Clone, Default)]
pub struct Checked {
    /// All diagnostics of collection, resolution, declaration, and bodies.
    pub diags: Vec<Diagnostic>,
    /// The solved type of every top-level const/var binding.
    pub def_types: FxHashMap<DefId, Ty>,
    /// Affine facts of every local binder, keyed by binder span: the
    /// copyability its type had at the binding site. The linearity
    /// pass consumes this; top-level bindings derive theirs from
    /// `def_types` directly.
    pub binders: HashMap<Span, crate::linearity::BinderFact>,
    /// The binder spans of every block-local `fn` declaration's own
    /// name: the name is in scope inside the declaration's body alone,
    /// so the lowering answers a use of one with the fn's own callable
    /// word and never collects it as a capture of the fn's own frame.
    pub self_fns: FxHashSet<Span>,
    /// The checked `Val` of every expression, keyed by its span: the solved
    /// type AT THAT USE SITE (flow-refined inside `is` tests and match arms)
    /// plus the early-exit flag. The lowering pass consumes this
    /// side table instead of re-deriving types; entries are recorded in
    /// walk order and a re-walk overwrites them.
    pub expr_types: FxHashMap<Span, Val>,
    /// The instantiation of every generic call, keyed by the call's span:
    /// the solved type of each generic parameter, in declaration order.
    /// Recorded only when every parameter solves (no `any` fallback); the
    /// lowering pass mints the CALLG evidence words from
    /// it.
    pub generic_calls: FxHashMap<Span, Vec<Ty>>,
    /// The solved dispatch of every trait-method call whose instance the
    /// environment statically resolved, keyed by the call's span.
    /// The lowering pass mints the CALLG evidence word from it.
    pub trait_calls: FxHashMap<Span, TraitCall>,
    /// Every interpolation hole that staged an implicit `show` dispatch,
    /// keyed by the hole's own span: the dispatched member (the
    /// solved trait member, the inherent method, or the trait member an
    /// in-scope assumption answers — the runtime-evidence form). The
    /// affine pass reads the receiver mode here and the effects pass
    /// unions the dispatched member's row; the lowering's synthetic call
    /// resolves through this record when no solved instance sits beside
    /// it.
    pub hole_show_dispatches: HoleShowDispatches,
    /// The selected inherent method of every inherent-impl dot-call whose
    /// owner covers the receiver, keyed by the call's own span. The
    /// lowering pass resolves the impl item through the declaration
    /// tables' method-to-impl-item map; no instance exists for an
    /// inherent impl, so nothing else records at the site.
    pub inherent_calls: FxHashMap<Span, MethodId>,
    /// Every dot-call the field tier answered, keyed by the call's own
    /// span: the spelling lowered as the indirect call of the receiver's
    /// fn-typed field word, the written arguments alone (the receiver no
    /// argument). The lowering routes on this record before any
    /// resolution tier.
    pub field_calls: FxHashSet<Span>,
    /// The runtime evidence a call into a generic inherent impl's body
    /// needs: one entry per declared bound — the bound's variable
    /// atom, its trait, and the unique instance serving the substituted
    /// type at this call site (`None` when none serves it uniquely, the
    /// shape the lowerer's staging refusal owns). Keyed by the call's own
    /// span, mirroring [`TraitCall::bounds`] minus the instance word an
    /// inherent impl never has. Recorded only at sites entering a generic
    /// body; monomorphic inherent calls need no evidence.
    pub inherent_call_bounds: FxHashMap<Span, InherentCallBounds>,
    /// The runtime evidence a call into a free generic fn's body needs:
    /// one entry per trait-bound generic parameter whose
    /// substitution solved, mirroring [`InherentCallBounds`] plus the
    /// solved type the substitution names. Keyed by the call's own span;
    /// recorded only when the fn declares trait bounds, and read by the
    /// lowering pass to stage the body's evidence demands.
    pub generic_call_bounds: FxHashMap<Span, GenericCallBounds>,
    /// The solved tested type of every `is` expression that type-solves,
    /// keyed by the expression's span. The lowering pass reads the tested
    /// atom from it when the written type is not a nominal single-segment
    /// path (a written fn type is the read-back call-through spelling).
    pub is_tests: FxHashMap<Span, Ty>,
    /// Every label use the directed probe resolved to an expected
    /// member's atom, keyed by the label's own span: a bare label in a
    /// directed position names the member of the expected union, which
    /// may be another declaration's atom than the one name resolution
    /// bound. The lowering pass reads the atom to emit here and never
    /// re-derives the redirection.
    pub directed_tags: FxHashMap<Span, TypeId>,
    /// The handled member set of every `.err`/`.ok` handler site, keyed
    /// by the call's own span: the checker's difference-rule record —
    /// the result type alone cannot re-derive it once the handler's
    /// answer re-adds a handled member, so the lowering pass dispatches
    /// the tag tests from this record and never re-derives.
    pub handler_handled: FxHashMap<Span, Ty>,
    /// The member one dot-call's type-directed selection settled on,
    /// keyed by the callee name's own span: the single survivor of
    /// the value-receiver tier — a trait member a solved instance or an
    /// in-scope assumption answers, or an inherent fn whose owner covers
    /// the receiver — or the member the type-keyed tier claimed. The
    /// dispatch records live at the call's span; this one names the
    /// member behind the name occurrence itself, so a name-position
    /// consumer reads the selection without re-deriving it.
    pub method_selections: FxHashMap<Span, MethodId>,
    /// The solved parameter types of every member call site, keyed by
    /// the call's own span: the site's `Self` and trait-generic
    /// substitution already applied, the receiver slot typed by the
    /// call's own receiver binding. A member owns no package def, so
    /// no `def_types` row serves its signature the way an import-bound
    /// free fn's does; the lowerer's call-site staging (the widening
    /// re-span, the width guard) reads this record where a dispatch
    /// reaches a body no planned def names.
    pub member_call_params: FxHashMap<Span, Vec<Ty>>,
    /// The solved parameter types of every non-generic free fn call,
    /// keyed by the call's own span: recorded only when every parameter
    /// solves, so the call-site staging (the native widening family's
    /// union re-span) reads the site's own types instead of a
    /// `def_types` row no free fn declaration carries.
    pub call_params: FxHashMap<Span, Vec<Ty>>,
    /// Every checked assignment place chain (the assignment sugar),
    /// keyed by the assignment's own span: the root binder plus its
    /// field/index steps, the lowering synthesizing the mut-window
    /// field store or the `set` member call from it.
    pub place_stores: FxHashMap<Span, PlaceStore>,
    /// The per-copy records of every `for comptime` body, keyed by the
    /// copy's path (outer-to-inner copy indices) and the span.
    pub comptime_records: FxHashMap<(Vec<u32>, Span), CopyRecords>,
    /// The projected fields of every `for comptime` loop, keyed by the
    /// loop's own span. A per-instance loop carries no fields here —
    /// its expansions live in `comptime_instance_loops`.
    pub comptime_loops: FxHashMap<Span, ComptimeLoop>,
    /// The per-copy records of every per-instance `for comptime`
    /// expansion, keyed by the walked fn's definition and solved
    /// arguments, the copy path, and the span. The once-check cannot
    /// walk a loop over the fn's own type variable; each entering
    /// site's re-walk of the fn body under its own substitution lands
    /// its copies' facts here.
    pub comptime_instance_records: FxHashMap<(DefId, Rc<Vec<Ty>>, Vec<u32>, Span), CopyRecords>,
    /// The projected fields of every per-instance expansion, keyed by
    /// the walked fn's definition, the solved arguments, and the loop's
    /// own span.
    pub comptime_instance_loops: FxHashMap<(DefId, Rc<Vec<Ty>>, Span), ComptimeLoop>,
}

/// One trait-method call's solved dispatch: the selected trait member
/// plus the unique instance whose head contains the receiver's type,
/// recorded at the call's own span. A site satisfied only through an
/// in-scope assumption (the receiver's type is a bound type variable)
/// records nothing — that site is the runtime-resolved evidence form, and
/// its dispatch waits for the runtime-tag shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TraitCall {
    /// The selected trait member.
    pub method: MethodId,
    /// The trait the member belongs to.
    pub trait_id: TraitId,
    /// The unique containing instance for the receiver's type.
    pub instance: InstanceId,
    /// The declaring impl item's span, when the package recorded one; the
    /// impl's bodies live in that item.
    pub impl_site: Option<Span>,
    /// The receiver's solved type at the call site.
    pub recv: Ty,
    /// The runtime evidence a call into a generic impl's body needs:
    /// one entry per declared bound of the solved instance — the bound's
    /// trait, the type variable it constrains, and the unique instance
    /// serving the substituted type at this call site (`None` when none
    /// serves it uniquely). Empty for monomorphic instances, whose
    /// bodies need no evidence.
    pub bounds: Vec<(TypeId, TraitId, Option<InstanceId>)>,
}

mod impl_sig;
mod ops;
use ops::compound_op;
mod pattern;
mod scope;
use scope::ScopedMap;

mod ctor;
mod expr;
mod item;
mod lambda;
mod name;
mod place;
mod record;

mod call;
mod expect;
mod flow;
mod query;
mod solve;

/// Checks one package: collects and resolves every file, builds the
/// declaration tables, then body-checks every item. The files registry
/// supplies source text for machine-readable suggestions.
#[must_use]
pub fn type_check(files: &Files, asts: &[Ast]) -> Checked {
    // The bundle built above is the arc's only holder, so this moves the
    // tables out instead of cloning them.
    Arc::unwrap_or_clone(analyze(files, asts).checked)
}

/// The full bidirectional check bundled with the tables later passes read:
/// the package namespace, per-file resolutions, declaration tables, and
/// the typed result. Every `Ty` inside `Checked` was minted by
/// `decls.universe`, so consumers must pass the bundle along unchanged —
/// a second pass over the same sources mints an unrelated universe. The
/// affine pass and the lowering pass consume exactly these.
///
/// Every field of the bundle is a shared handle, so cloning it clones
/// pointers — the query layer hands the bundle out of a memo without
/// copying the namespace — and a consumer holding the bundle keeps the
/// universe its `Ty`s were minted by alive. The trees are shared handles
/// too (`Arc<Ast>` per tree): a process that compiles many members over
/// one ambient (a package graph's member compiles, a query session)
/// serves the ambient's resident tree to every compile without cloning
/// it, and a compile's teardown drops a refcount, never the tree itself.
///
/// The check runs over the derive-expanded trees: the trees the
/// caller hands in are the ones every table is built over, and
/// [`Analyzed::asts`] carries them back, so the lowering pass consumes
/// those, never the caller's originals. Expansion precedes resolution
/// and belongs to the meta layer: a caller with
/// raw trees expands through the derive engine's analyze entries
/// (`ds_meta::analyze`, over shared trees) and hands the expanded trees
/// in. A caller that already resolved through a memo hands that memo's
/// own expanded trees and resolution in through [`analyze_resolved`]
/// instead of resolving again.
pub fn analyze<T>(files: &Files, asts: &[T]) -> Analyzed
where
    T: std::borrow::Borrow<Ast>,
{
    // The trees arrive borrowed, so they copy once into the shared
    // shape; a caller holding the trees by value takes `analyze_owned`
    // and skips this copy.
    let owned: Vec<Arc<Ast>> = asts
        .iter()
        .map(|ast| Arc::new(std::borrow::Borrow::borrow(ast).clone()))
        .collect();
    analyze_owned(files, owned)
}

/// The ownership variant of [`analyze`] for the cold run path: the
/// caller hands the package's derive-expanded trees as shared handles
/// (`Arc::new` moves an owned tree in without copying it), so the
/// bundle adopts them. The expansion itself belongs to the meta layer:
/// a caller with raw trees expands through `ds_meta::analyze_owned` (the
/// derive engine and the expanding analyze entries live there) and
/// hands the result here unchanged.
pub fn analyze_owned(files: &Files, asts: Vec<Arc<Ast>>) -> Analyzed {
    let (package, resolutions, resolve_diags) = crate::resolve_package(&asts);
    analyze_resolved(
        files,
        asts.into(),
        package.into(),
        resolutions.into(),
        resolve_diags,
        Arc::default(),
    )
}

/// The check half of [`analyze`] over pre-resolved trees: builds the
/// declaration tables and body-checks every item, then bundles the
/// caller's own trees, namespace, and resolutions (the shared handles,
/// adopted as-is) with `resolve_diags` merged ahead of the declaration
/// and body classes (the expansion's refusal classes ride with them).
/// The trees must be the ones `package` and `resolutions` were built
/// over — the derive-expanded ones — so a caller holding a resolve
/// memo's value passes its parts here unchanged.
pub fn analyze_resolved(
    files: &Files,
    asts: Arc<[Arc<Ast>]>,
    package: Arc<Package>,
    resolutions: Arc<[Resolutions]>,
    resolve_diags: Vec<Diagnostic>,
    artifacts: Arc<ArtifactSymbols>,
) -> Analyzed {
    let (decls, checked) = check_bodies(files, &asts[..], &package, &resolutions[..], None)
        .expect("a fresh build carries no serve to drift");
    let mut diags = resolve_diags;
    let resolve_diag_count = u32::try_from(diags.len()).expect("diagnostic count fits u32");
    diags.extend(checked.diags);
    let checked = Checked {
        diags,
        def_types: checked.def_types,
        binders: checked.binders,
        self_fns: checked.self_fns,
        expr_types: checked.expr_types,
        generic_calls: checked.generic_calls,
        trait_calls: checked.trait_calls,
        hole_show_dispatches: checked.hole_show_dispatches,
        inherent_calls: checked.inherent_calls,
        field_calls: checked.field_calls,
        inherent_call_bounds: checked.inherent_call_bounds,
        generic_call_bounds: checked.generic_call_bounds,
        is_tests: checked.is_tests,
        directed_tags: checked.directed_tags,
        handler_handled: checked.handler_handled,
        method_selections: checked.method_selections,
        member_call_params: checked.member_call_params,
        call_params: checked.call_params,
        place_stores: checked.place_stores,
        comptime_records: checked.comptime_records,
        comptime_loops: checked.comptime_loops,
        comptime_instance_records: checked.comptime_instance_records,
        comptime_instance_loops: checked.comptime_instance_loops,
    };
    Analyzed {
        asts,
        package,
        resolutions,
        decls: Rc::new(decls),
        artifacts,
        checked: Arc::new(checked),
        resolve_diag_count,
        span_files: span_files_of(files),
    }
}

/// [`analyze_owned`] behind a checked-ambient artifact: the ambient
/// member's tree arrives first (already derive-expanded, the blob
/// discipline), its namespace, per-file resolutions, and declaration-side
/// prefix pre-served from the artifact, and the user trees after it,
/// every tree pre-expanded by the caller. `diags` seeds the
/// diagnostics collected ahead of this analyze — the derive
/// expansion's over the trees, when the caller expanded through the
/// meta layer — ahead of the same walk order [`analyze_owned`]
/// produces. The user trees collect and resolve against the served
/// namespace exactly as [`analyze_owned`] would, and every check pass
/// then runs fresh over the full tree list: this artifact slice serves
/// none of the hard tables or body facts, so the ambient's signatures,
/// instances, and bodies check against the blob-served trees as always,
/// only against the served universe and namespace. The served prefix's
/// ids hold by the mint-determinism pin, so user ids continue after it
/// exactly as in a fresh analyze.
pub fn analyze_served(
    files: &Files,
    asts: Vec<Arc<Ast>>,
    package: Package,
    resolutions: Vec<Resolutions>,
    served: decls::ServedDecls,
    diags: Vec<Diagnostic>,
) -> Analyzed {
    analyze_served_over(
        files,
        asts,
        package,
        resolutions,
        served,
        diags,
        None,
        Arc::default(),
    )
}

/// [`analyze_served`] with import surfaces linked: the link pass
/// runs over every tree of the unit between collection and resolution,
/// so an import the graph serves validates and binds exactly as the
/// fresh path's does.
pub fn analyze_served_linked(
    files: &Files,
    asts: Vec<Arc<Ast>>,
    package: Package,
    resolutions: Vec<Resolutions>,
    served: decls::ServedDecls,
    diags: Vec<Diagnostic>,
    imports: &crate::import::ImportGraph,
) -> Analyzed {
    analyze_served_over(
        files,
        asts,
        package,
        resolutions,
        served,
        diags,
        Some(imports),
        Arc::new(imports.artifacts().clone()),
    )
}

fn analyze_served_over(
    files: &Files,
    asts: Vec<Arc<Ast>>,
    package: Package,
    resolutions: Vec<Resolutions>,
    served: decls::ServedDecls,
    diags: Vec<Diagnostic>,
    imports: Option<&crate::import::ImportGraph>,
    artifacts: Arc<ArtifactSymbols>,
) -> Analyzed {
    let mut diags = diags;
    // The diagnostics seeded ahead of this call (the derive expansion's
    // over the trees) outlive the fallback below; the collection,
    // linking, and resolution diags the serve path produces rebuild
    // over the same trees when the serve refuses.
    let seed_len = diags.len();
    let mut package = package;
    assert!(
        resolutions.len() <= asts.len(),
        "one served resolution table per leading served tree, at most one per tree"
    );
    for ast in &asts[resolutions.len()..] {
        diags.extend(package.add_file(ast));
    }
    if let Some(imports) = imports {
        // The link pass runs over every tree of the unit, the served
        // trees too: the artifact carries no link-pass state (the
        // foreign map, the per-file bindings, the module refs are all
        // re-derived from the caller's fresh graph), and a served tree's
        // import-bound defs register at the same tail ids its served
        // resolutions name, so the re-run re-lands them and every later
        // pass sees the compile's own link facts exactly like the fresh
        // path's. A served tree with no imports (the ambient member) the
        // pass scans and skips.
        let trees: Vec<&Ast> = asts.iter().map(|ast| &**ast).collect();
        diags.extend(package.link_imports(&trees, imports));
    }
    let mut tables = resolutions;
    for ast in &asts[tables.len()..] {
        let (uses, mut file_diags) = package.resolve(ast);
        tables.push(uses);
        diags.append(&mut file_diags);
    }
    let checked = check_bodies(files, &asts[..], &package, &tables[..], Some(served));
    let (decls, checked) = if let Ok(pair) = checked {
        pair
    } else {
        // The serve drifted: a name-addressed row refused to resolve
        // over this compile's own universe (a corrupt or foreign
        // artifact, a walk the rows' raw ids did not reproduce). The
        // artifact is a pure cache, so the analyze falls back to the
        // fully fresh path — re-collect, re-link, re-resolve, and
        // rebuild every declaration over the same trees — paying
        // the fresh cost once instead of serving a wrong fact.
        let mut fresh_package = Package::new();
        let trees: Vec<&Ast> = asts.iter().map(|ast| &**ast).collect();
        diags.truncate(seed_len);
        for ast in &trees {
            diags.extend(fresh_package.add_file(ast));
        }
        if let Some(imports) = imports {
            diags.extend(fresh_package.link_imports(&trees, imports));
        }
        let mut fresh_tables = Vec::new();
        for ast in &trees {
            let (uses, mut file_diags) = fresh_package.resolve(ast);
            fresh_tables.push(uses);
            diags.append(&mut file_diags);
        }
        package = fresh_package;
        tables = fresh_tables;
        let (decls, checked) = check_bodies(files, &asts[..], &package, &tables[..], None)
            .expect("a fresh build carries no serve to drift");
        (decls, checked)
    };
    let resolve_diag_count = u32::try_from(diags.len()).expect("diagnostic count fits u32");
    diags.extend(checked.diags);
    let checked = Checked {
        diags,
        def_types: checked.def_types,
        binders: checked.binders,
        self_fns: checked.self_fns,
        expr_types: checked.expr_types,
        generic_calls: checked.generic_calls,
        trait_calls: checked.trait_calls,
        hole_show_dispatches: checked.hole_show_dispatches,
        inherent_calls: checked.inherent_calls,
        field_calls: checked.field_calls,
        inherent_call_bounds: checked.inherent_call_bounds,
        generic_call_bounds: checked.generic_call_bounds,
        is_tests: checked.is_tests,
        directed_tags: checked.directed_tags,
        handler_handled: checked.handler_handled,
        method_selections: checked.method_selections,
        member_call_params: checked.member_call_params,
        call_params: checked.call_params,
        place_stores: checked.place_stores,
        comptime_records: checked.comptime_records,
        comptime_loops: checked.comptime_loops,
        comptime_instance_records: checked.comptime_instance_records,
        comptime_instance_loops: checked.comptime_instance_loops,
    };
    Analyzed {
        asts: asts.into(),
        package: package.into(),
        resolutions: tables.into(),
        decls: Rc::new(decls),
        artifacts,
        checked: Arc::new(checked),
        resolve_diag_count,
        span_files: span_files_of(files),
    }
}

/// The declaration and body half of the check the analyze entries
/// bundle: the declaration tables the affine pass reads beside the
/// checked result.
/// Iterates `asts` and `resolutions` positionally: the files a caller
/// registers may leave holes in the file-id space (unparseable files
/// contribute no tree), so positions — not file ids — align the two.
/// `served` carries the checked-ambient prefix's declaration tables when
/// the caller analyzed through [`analyze_served`]. A drift refusal
/// answers [`Err`] with the count of diagnostics the serve-path's own
/// collection produced ahead of this pass, so the caller can fall back
/// to the fresh collection over the same trees — the artifact is a
/// pure cache, and a refused serve never serves a wrong fact.
fn check_bodies<T>(
    files: &Files,
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
    served: Option<decls::ServedDecls>,
) -> Result<(Decls, Checked), ()>
where
    T: std::borrow::Borrow<Ast>,
{
    let trees: Vec<&Ast> = asts.iter().map(std::borrow::Borrow::borrow).collect();
    let (mut decls, mut diags) =
        decls::build_over(package, &trees, resolutions, served).map_err(|_| ())?;
    let res_by_file = FilePositions::new(&trees);
    let mut def_types = FxHashMap::default();
    let mut binders = HashMap::default();
    let mut self_fns = FxHashSet::default();
    let mut expr_types = FxHashMap::default();
    let mut generic_calls = FxHashMap::default();
    let mut trait_calls = FxHashMap::default();
    let mut inherent_calls = FxHashMap::default();
    let mut field_calls = FxHashSet::default();
    let mut method_selections: FxHashMap<Span, MethodId> = FxHashMap::default();
    let mut member_call_params: FxHashMap<Span, Vec<Ty>> = FxHashMap::default();
    let mut call_params: FxHashMap<Span, Vec<Ty>> = FxHashMap::default();
    let mut place_stores: FxHashMap<Span, PlaceStore> = FxHashMap::default();
    let mut inherent_call_bounds: FxHashMap<Span, InherentCallBounds> = FxHashMap::default();
    let mut generic_call_bounds: FxHashMap<Span, GenericCallBounds> = FxHashMap::default();
    let mut is_tests = FxHashMap::default();
    let mut directed_tags: FxHashMap<Span, TypeId> = FxHashMap::default();
    let mut handler_handled: FxHashMap<Span, Ty> = FxHashMap::default();
    let mut hole_show_dispatches: FxHashMap<Span, MethodId> = FxHashMap::default();
    let mut comptime_records: FxHashMap<(Vec<u32>, Span), CopyRecords> = FxHashMap::default();
    let mut comptime_loops: FxHashMap<Span, ComptimeLoop> = FxHashMap::default();
    let mut instance_records: FxHashMap<(DefId, Rc<Vec<Ty>>, Vec<u32>, Span), CopyRecords> =
        FxHashMap::default();
    let mut instance_loops: FxHashMap<(DefId, Rc<Vec<Ty>>, Span), ComptimeLoop> =
        FxHashMap::default();
    let mut comptime_sites: FxHashMap<Span, (DefId, Vec<Ty>)> = FxHashMap::default();
    for (ast, res) in trees.iter().zip(resolutions) {
        let mut bodies = Bodies {
            files,
            package,
            res,
            resolutions,
            res_by_file: &res_by_file,
            file: Some(ast.file),
            decls: &mut decls,
            def_types: &mut def_types,
            binders: &mut binders,
            self_fns: &mut self_fns,
            expr_types: &mut expr_types,
            generic_calls: &mut generic_calls,
            trait_calls: &mut trait_calls,
            inherent_calls: &mut inherent_calls,
            field_calls: &mut field_calls,
            inherent_call_bounds: &mut inherent_call_bounds,
            generic_call_bounds: &mut generic_call_bounds,
            is_tests: &mut is_tests,
            directed_tags: &mut directed_tags,
            handler_handled: &mut handler_handled,
            hole_show_dispatches: &mut hole_show_dispatches,
            comptime_records: &mut comptime_records,
            comptime_loops: &mut comptime_loops,
            comptime_instance_records: &mut instance_records,
            comptime_instance_loops: &mut instance_loops,
            comptime_sites: &mut comptime_sites,
            instance: None,
            method_selections: &mut method_selections,
            member_call_params: &mut member_call_params,
            call_params: &mut call_params,
            place_stores: &mut place_stores,
            diags: Vec::new(),
            linear_vars: FxHashMap::default(),
            locals: ScopedMap::default(),
            for_iters: Vec::new(),
            nursery_scopes: Vec::new(),
            spawn_answers: Vec::new(),
            spawn_depth: 0,
            comptime_path: Vec::new(),
            comptime_stack: Vec::new(),
            refined: ScopedMap::default(),
            type_vars: Vec::new(),
            assumptions: Vec::new(),
            assumption_marks: Vec::new(),
            ret: None,
            loop_depth: 0,
            loop_breaks: Vec::new(),
            frame_depth: 0,
            cond_depth: 0,
            visiting: Vec::new(),
            solve_reported: false,
            pattern_label_reported: false,
            value_pos: true,
            expected_ty_span: None,
            comptime: ComptimeCtx::default(),
        };
        bodies.file(ast);
        diags.append(&mut bodies.diags);
    }
    // The import-bound functions' checked types: a foreign def has
    // no tree to walk, so its fn type mints here from the surface's
    // written signature — the same fn typing every later consumer reads
    // (the lowerer's parameter-word reads, a value use's fn typing). The
    // link pass already refused a signature naming a type this package
    // cannot spell, and a foreign def no use reaches mints quietly.
    {
        let empty_res = Resolutions::from_uses(Vec::new());
        let empty_tables: Vec<Resolutions> = Vec::new();
        let no_files = FilePositions(Vec::new());
        let mut bodies = Bodies {
            files,
            package,
            res: &empty_res,
            resolutions: &empty_tables,
            res_by_file: &no_files,
            file: None,
            decls: &mut decls,
            def_types: &mut def_types,
            binders: &mut binders,
            self_fns: &mut self_fns,
            expr_types: &mut expr_types,
            generic_calls: &mut generic_calls,
            trait_calls: &mut trait_calls,
            inherent_calls: &mut inherent_calls,
            field_calls: &mut field_calls,
            inherent_call_bounds: &mut inherent_call_bounds,
            generic_call_bounds: &mut generic_call_bounds,
            is_tests: &mut is_tests,
            directed_tags: &mut directed_tags,
            handler_handled: &mut handler_handled,
            hole_show_dispatches: &mut hole_show_dispatches,
            comptime_records: &mut comptime_records,
            comptime_loops: &mut comptime_loops,
            comptime_instance_records: &mut instance_records,
            comptime_instance_loops: &mut instance_loops,
            comptime_sites: &mut comptime_sites,
            instance: None,
            method_selections: &mut method_selections,
            member_call_params: &mut member_call_params,
            call_params: &mut call_params,
            place_stores: &mut place_stores,
            diags: Vec::new(),
            linear_vars: FxHashMap::default(),
            locals: ScopedMap::default(),
            for_iters: Vec::new(),
            nursery_scopes: Vec::new(),
            spawn_answers: Vec::new(),
            spawn_depth: 0,
            comptime_path: Vec::new(),
            comptime_stack: Vec::new(),
            refined: ScopedMap::default(),
            type_vars: Vec::new(),
            assumptions: Vec::new(),
            assumption_marks: Vec::new(),
            ret: None,
            loop_depth: 0,
            loop_breaks: Vec::new(),
            frame_depth: 0,
            cond_depth: 0,
            visiting: Vec::new(),
            solve_reported: false,
            pattern_label_reported: false,
            value_pos: true,
            expected_ty_span: None,
            comptime: ComptimeCtx::default(),
        };
        let mut linked: Vec<DefId> = package.foreign().keys().copied().collect();
        linked.sort_by_key(|def| def.index());
        let mut linked_types = Vec::with_capacity(linked.len());
        for def in linked {
            let Some(entry) = package.foreign().get(&def) else {
                continue;
            };
            let Some(sig) = &entry.sig else {
                continue;
            };
            linked_types.push((def, bodies.fn_atom_of(sig).ty));
        }
        def_types.extend(linked_types);
    }
    // The solved signatures of every struct application and the per-case
    // facts of every enum application: the whole package's resolution is
    // done, so every minted application atom is final and its
    // constructor's shape is installed. The lowering pass shapes the
    // applications' values through these tables.
    decls::record_applications(package, &mut decls);
    // The per-instance `for comptime` expansions: every solved call site
    // entering a fn whose body holds a loop over the fn's own type
    // variable re-walks that body under its own substitution, the
    // copies' facts landing in the instance tables. The worklist takes
    // the nested sites each walk re-records (a generic callee entered
    // through the walked body), so instances compose.
    let mut instance_records =
        FxHashMap::<(DefId, Rc<Vec<Ty>>, Vec<u32>, Span), CopyRecords>::default();
    let mut instance_loops: FxHashMap<(DefId, Rc<Vec<Ty>>, Span), ComptimeLoop> =
        FxHashMap::default();
    let instance_diags = expand_comptime_instances(
        files,
        &trees[..],
        package,
        resolutions,
        &mut decls,
        &def_types,
        &mut comptime_loops,
        &mut comptime_sites,
        &mut instance_records,
        &mut instance_loops,
        &mut self_fns,
    );
    diags.extend(instance_diags);
    Ok((decls, Checked {
        diags,
        def_types,
        binders,
        self_fns,
        expr_types,
        generic_calls,
        trait_calls,
        hole_show_dispatches,
        inherent_calls,
        field_calls,
        inherent_call_bounds,
        generic_call_bounds,
        is_tests,
        directed_tags,
        handler_handled,
        comptime_records,
        comptime_loops,
        comptime_instance_records: instance_records,
        comptime_instance_loops: instance_loops,
        method_selections,
        member_call_params,
        call_params,
        place_stores,
    }))
}

/// The instance expansions one free generic body plans. The cap mirrors
/// the lowering's own per-body instance bound: a self-entering recursion
/// over a growing shape refuses typed instead of expanding without end.
const MAX_INSTANCE_EXPANSIONS: usize = 256;

/// Whether `ty` names no minted type variable anywhere in its shape: a
/// solved argument the expansion worklist can walk.
fn names_no_variable(universe: &crate::Universe, ty: Ty) -> bool {
    universe
        .members(ty)
        .iter()
        .all(|&atom| atom_is_concrete(universe, atom))
}

fn atom_is_concrete(universe: &crate::Universe, atom: TypeId) -> bool {
    let Ok(decl) = universe.atom_decl(atom) else {
        return false;
    };
    if decl.name.starts_with('$') {
        return false;
    }
    match &decl.kind {
        AtomKind::Apply(_, args) => args.iter().all(|&ty| names_no_variable(universe, ty)),
        AtomKind::Tuple(elements) => elements
            .iter()
            .all(|&atom| atom_is_concrete(universe, atom)),
        AtomKind::FnSig(params, ret) => {
            params.iter().all(|&ty| names_no_variable(universe, ty))
                && names_no_variable(universe, *ret)
        },
        AtomKind::Unit | AtomKind::Struct(_) | AtomKind::Enum(_) | AtomKind::Reserved => true,
    }
}

/// The lambda body one declared free fn's definition names: the
/// top-level Local item whose binder spans the definition's own name
/// occurrence (the same occurrence collection registered).
fn fn_lambda<'a>(trees: &[&'a Ast], package: &Package, def: DefId) -> Option<(&'a Ast, &'a Expr)> {
    let entry = package.def(def);
    let positions = FilePositions::new(trees);
    let ast = trees[positions.get(entry.span.file)?];
    let items = ast.blocks.iter().flat_map(|block| &block.items);
    let mut by_name = None;
    for item in items {
        let ItemKind::Local(local) = &item.kind else {
            continue;
        };
        let Pattern::Binding { span, name, .. } = &local.pat else {
            continue;
        };
        if *span == entry.span {
            return match &local.init {
                Expr::Lambda { .. } => Some((ast, &local.init)),
                _ => None,
            };
        }
        if name.name == entry.name && matches!(local.init, Expr::Lambda { .. }) {
            by_name = Some(&local.init);
        }
    }
    by_name.map(|lambda| (ast, lambda))
}

/// The per-instance `for comptime` expansions: every solved call site
/// entering a fn whose body holds a loop over the fn's own type
/// variable re-walks that body under the site's substitution, the
/// copies' checked facts landing in the instance tables keyed by the
/// walked fn's definition and solved arguments. The worklist takes the
/// nested sites each walk re-records (a generic callee entered through
/// the walked body), so instances compose. A walk's body-wide facts run
/// to scratch tables — the once-check's variable-typed rows are the
/// ones every instance reads through the lowering's lens — and a
/// written loop inside the walked body records its copies into the
/// instance tables too, its projection never having depended on the
/// walked variable. The walks' diagnostics dedupe by (code, primary
/// span) across sites, first wins: two instances checking the same
/// source line re-derive the same refusal.
#[allow(clippy::too_many_arguments)]
fn expand_comptime_instances(
    files: &Files,
    trees: &[&Ast],
    package: &Package,
    resolutions: &[Resolutions],
    decls: &mut Decls,
    def_types: &FxHashMap<DefId, Ty>,
    comptime_loops: &mut FxHashMap<Span, ComptimeLoop>,
    sites: &mut FxHashMap<Span, (DefId, Vec<Ty>)>,
    instance_records: &mut FxHashMap<(DefId, Rc<Vec<Ty>>, Vec<u32>, Span), CopyRecords>,
    instance_loops: &mut FxHashMap<(DefId, Rc<Vec<Ty>>, Span), ComptimeLoop>,
    self_fns: &mut FxHashSet<Span>,
) -> Vec<Diagnostic> {
    let per_instance: Vec<Span> = comptime_loops
        .iter()
        .filter(|(_, info)| info.per_instance.is_some())
        .map(|(&span, _)| span)
        .collect();
    if per_instance.is_empty() {
        return Vec::new();
    }
    // Sites seeded in walk order; each walk's own nested sites join in
    // span order, so the expansions, their atom mints, and the merged
    // diagnostics stay deterministic run to run.
    let mut seen: HashSet<(DefId, Vec<Ty>)> = HashSet::new();
    let mut queue: Vec<(DefId, Rc<Vec<Ty>>)> = Vec::new();
    let mut seeded: Vec<Span> = sites.keys().copied().collect();
    seeded.sort_by_key(|span| (span.file.as_u32(), span.start));
    for span in seeded {
        let (def, args) = &sites[&span];
        if seen.insert((*def, args.clone()))
            && args
                .iter()
                .all(|&ty| names_no_variable(&decls.universe, ty))
        {
            queue.push((*def, Rc::new(args.clone())));
        }
    }
    let positions = FilePositions::new(trees);
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut seen_diags: HashSet<(ErrorCode, Option<Span>)> = HashSet::new();
    let mut walked = 0usize;
    let mut at = 0usize;
    while at < queue.len() {
        let (def, args) = queue[at].clone();
        at += 1;
        let Some((ast, lambda)) = fn_lambda(trees, package, def) else {
            continue;
        };
        let body_span = lambda.span();
        // Only fns whose own body holds a per-instance loop expand.
        if !per_instance.iter().any(|&span| {
            span.file == body_span.file
                && span.start >= body_span.start
                && span.end <= body_span.end
        }) {
            continue;
        }
        walked += 1;
        if walked > MAX_INSTANCE_EXPANSIONS {
            diags.push(
                Diagnostic::error(
                    ErrorCode::InferenceFailed,
                    "a generic fn instantiates itself without end: its per-instance `fields` loop \
                     keeps entering new shapes"
                        .to_owned(),
                )
                .with_primary(body_span, "the expansion never reaches a fixed point"),
            );
            break;
        }
        let Some(position) = positions.get(ast.file) else {
            continue;
        };
        let res = &resolutions[position];
        // Scratch body-wide tables: reads seed from the once-check's
        // rows, writes die with the walk. `comptime_records` joins them
        // (the instance walk's written-loop copies land under the
        // instance key), `comptime_loops` stays the real table (the
        // walk re-projects written loops to identical rows), and the
        // per-instance tables plus the site table are the real ones the
        // lowering and the worklist read.
        let mut scratch_def_types = def_types.clone();
        let mut scratch_binders = HashMap::default();
        let mut scratch_expr_types = FxHashMap::default();
        let mut scratch_generic_calls = FxHashMap::default();
        let mut scratch_trait_calls = FxHashMap::default();
        let mut scratch_hole_show = FxHashMap::default();
        let mut scratch_inherent_calls = FxHashMap::default();
        let mut scratch_field_calls = FxHashSet::default();
        let mut scratch_inherent_bounds = FxHashMap::default();
        let mut scratch_generic_bounds = FxHashMap::default();
        let mut scratch_is_tests = FxHashMap::default();
        let mut scratch_directed_tags = FxHashMap::default();
        let mut scratch_handler = FxHashMap::default();
        let mut scratch_member_params = FxHashMap::default();
        let mut scratch_method_selections = FxHashMap::default();
        let mut scratch_place_stores = FxHashMap::default();
        let mut scratch_call_params: FxHashMap<Span, Vec<Ty>> = FxHashMap::default();
        let mut scratch_comptime_records: FxHashMap<(Vec<u32>, Span), CopyRecords> =
            FxHashMap::default();
        let sites_before: HashSet<Span> = sites.keys().copied().collect();
        let mut bodies = Bodies {
            files,
            package,
            res,
            resolutions,
            res_by_file: &positions,
            file: Some(ast.file),
            decls,
            def_types: &mut scratch_def_types,
            binders: &mut scratch_binders,
            self_fns,
            expr_types: &mut scratch_expr_types,
            generic_calls: &mut scratch_generic_calls,
            trait_calls: &mut scratch_trait_calls,
            inherent_calls: &mut scratch_inherent_calls,
            field_calls: &mut scratch_field_calls,
            inherent_call_bounds: &mut scratch_inherent_bounds,
            generic_call_bounds: &mut scratch_generic_bounds,
            is_tests: &mut scratch_is_tests,
            directed_tags: &mut scratch_directed_tags,
            handler_handled: &mut scratch_handler,
            hole_show_dispatches: &mut scratch_hole_show,
            comptime_records: &mut scratch_comptime_records,
            comptime_loops,
            comptime_instance_records: instance_records,
            comptime_instance_loops: instance_loops,
            comptime_sites: sites,
            method_selections: &mut scratch_method_selections,
            member_call_params: &mut scratch_member_params,
            call_params: &mut scratch_call_params,
            place_stores: &mut scratch_place_stores,
            diags: Vec::new(),
            linear_vars: FxHashMap::default(),
            locals: ScopedMap::default(),
            for_iters: Vec::new(),
            nursery_scopes: Vec::new(),
            spawn_answers: Vec::new(),
            spawn_depth: 0,
            comptime_path: Vec::new(),
            comptime_stack: Vec::new(),
            refined: ScopedMap::default(),
            type_vars: Vec::new(),
            assumptions: Vec::new(),
            assumption_marks: Vec::new(),
            ret: None,
            loop_depth: 0,
            loop_breaks: Vec::new(),
            frame_depth: 0,
            cond_depth: 0,
            visiting: Vec::new(),
            solve_reported: false,
            pattern_label_reported: false,
            value_pos: true,
            expected_ty_span: None,
            comptime: ComptimeCtx::default(),
            instance: Some(InstanceRun {
                def,
                args: Rc::clone(&args),
            }),
        };
        bodies.instance_fn_body(lambda, &args);
        let mut walk_diags = std::mem::take(&mut bodies.diags);
        // The nested sites the walk re- recorded, in walk order.
        let mut fresh: Vec<Span> = sites
            .keys()
            .filter(|span| !sites_before.contains(span))
            .copied()
            .collect();
        fresh.sort_by_key(|span| (span.file.as_u32(), span.start));
        for span in fresh {
            let (callee, callee_args) = &sites[&span];
            if seen.insert((*callee, callee_args.clone()))
                && callee_args
                    .iter()
                    .all(|&ty| names_no_variable(&decls.universe, ty))
            {
                queue.push((*callee, Rc::new(callee_args.clone())));
            }
        }
        for diag in walk_diags.drain(..) {
            let key = (diag.code, diag.labels.first().map(|label| label.span));
            if seen_diags.insert(key) {
                diags.push(diag);
            }
        }
    }
    diags
}

/// The resolved graph's bound artifacts: artifact name to its
/// installed library's exported C symbols, in enumeration order.
pub type ArtifactSymbols = std::collections::BTreeMap<String, Arc<Vec<String>>>;

/// The bundle's span facts: one row per registered file, indexed by raw
/// file id.
fn span_files_of(files: &Files) -> Arc<[ds_diag::FileSpans]> {
    (0..files.len())
        .map(|at| files.file_spans(FileId::new(u32::try_from(at).expect("file count fits u32"))))
        .collect()
}

/// The tables of one analyzed package; see [`analyze`]. Every field is a
/// shared handle, so cloning the bundle clones pointers — the query layer
/// hands it out of a memo without copying the namespace — and a consumer
/// holding the bundle keeps the universe its `Ty`s were minted by alive.
#[must_use]
#[derive(Clone)]
pub struct Analyzed {
    /// The derive-expanded trees the tables were built over:
    /// consumed derive attributes became ordinary impl items here. The
    /// lowering pass consumes these trees, never the caller's originals.
    /// Shared handles one apiece, so a process's resident ambient tree
    /// serves to every member compile without a clone and a compile's
    /// teardown drops a refcount, never the tree.
    pub asts: Arc<[Arc<Ast>]>,
    /// The package namespace: definitions, name tables, method registry.
    pub package: Arc<Package>,
    /// Per-file name resolutions, aligning with the package's collected
    /// (parseable) files.
    pub resolutions: Arc<[Resolutions]>,
    /// The declaration tables, owning the universe that minted every
    /// `Ty`. Shared without atomics: the tables hold `Rc` sets (see
    /// `Universe`), so the bundle is not `Send` however it is shared.
    pub decls: Rc<Decls>,
    /// The resolved graph's bound artifacts: each `[artifacts]`
    /// name the run's manifest graph resolved mapped to its installed
    /// library's exported C symbols, in enumeration order — the fold a
    /// bound extern's declaration resolves through and the table the
    /// run installs as the artifact's native module. Empty on every
    /// graphless path (a plain script, the REPL), where a bound extern
    /// refuses at lowering naming the missing package fold.
    pub artifacts: Arc<ArtifactSymbols>,
    /// The typed result: diagnostics (resolve classes merged ahead of the
    /// declaration and body classes, like [`analyze`]), top-level types,
    /// binder facts, per-expression types.
    pub checked: Arc<Checked>,
    /// Per registered file, the span facts (name, text, line index) the
    /// bundle's spans resolve against, indexed by raw file id. The trees'
    /// byte offsets mean nothing without it, so the bundle carries it:
    /// the lowering pass builds the module's pc-to-span table from the
    /// same facts, and the registry itself never crosses this seam.
    pub span_files: Arc<[ds_diag::FileSpans]>,
    /// The count of leading resolve-half diagnostics in `checked.diags`
    /// (see [`check_diags`](Self::check_diags)): the merge order both
    /// analyze constructors build, kept private so the split stays this
    /// module's own contract.
    resolve_diag_count: u32,
}

impl Analyzed {
    /// The declaration and body diagnostics alone: the merged list in
    /// `checked.diags` minus its resolve-half prefix (expansion
    /// refusals, collection, and resolution — the classes the resolve
    /// half owns and reports through its own value). A consumer
    /// publishing the check family's classes reads them off the one
    /// bundle instead of re-running the check for the split.
    #[must_use]
    pub fn check_diags(&self) -> &[Diagnostic] {
        &self.checked.diags[self.resolve_diag_count as usize..]
    }

    /// Every top-level binding's solved type rendered through the
    /// bundle's own universe, keyed by definition id. Solved handles
    /// are meaningful only inside the universe that minted them, and
    /// consumers store values across computes, so the render belongs to
    /// the bundle's own surface.
    #[must_use]
    pub fn type_names(&self) -> FxHashMap<DefId, String> {
        self.checked
            .def_types
            .iter()
            .map(|(&id, &ty)| (id, self.decls.universe.render(ty)))
            .collect()
    }
}

/// The tree position of each file id: file ids index a `Files` registry
/// densely, and a file that contributed no tree holds no position.
struct FilePositions(Vec<Option<usize>>);

impl FilePositions {
    fn new(trees: &[&Ast]) -> Self {
        let mut positions: Vec<Option<usize>> = Vec::new();
        for (index, ast) in trees.iter().enumerate() {
            let file = ast.file.as_u32() as usize;
            if positions.len() <= file {
                positions.resize(file + 1, None);
            }
            positions[file] = Some(index);
        }
        Self(positions)
    }

    fn get(&self, file: FileId) -> Option<usize> {
        self.0.get(file.as_u32() as usize).copied().flatten()
    }
}

/// One bare label's directed outcome against an expected union.
enum DirectedLabel {
    /// Exactly one expected member carries the label: its atom.
    Directed(TypeId),
    /// None or several carried it, the diagnostic reported.
    Refused,
}

/// The body checker over one file.
struct Bodies<'a> {
    files: &'a Files,
    package: &'a Package,
    res: &'a Resolutions,
    /// Every file's resolutions, aligned with the trees by position; a
    /// lazily typed top-level binding walks under its OWN file's table,
    /// no matter which file's body triggered the memo fill.
    resolutions: &'a [Resolutions],
    res_by_file: &'a FilePositions,
    /// The file whose bodies this walk checks: a tag label resolves
    /// through the declaring file's shadow entry first, `None`
    /// in the foreign-defs pass where only linked names answer.
    file: Option<FileId>,
    decls: &'a mut Decls,
    def_types: &'a mut FxHashMap<DefId, Ty>,
    binders: &'a mut HashMap<Span, crate::linearity::BinderFact>,
    self_fns: &'a mut FxHashSet<Span>,
    expr_types: &'a mut FxHashMap<Span, Val>,
    generic_calls: &'a mut FxHashMap<Span, Vec<Ty>>,
    trait_calls: &'a mut FxHashMap<Span, TraitCall>,
    inherent_calls: &'a mut FxHashMap<Span, MethodId>,
    /// Every dot-call the field tier answered, keyed by the call's own
    /// span: the lowering's authority for routing the spelling to the
    /// computed-callee path (the record, not the resolution tier).
    field_calls: &'a mut FxHashSet<Span>,
    method_selections: &'a mut FxHashMap<Span, MethodId>,
    member_call_params: &'a mut FxHashMap<Span, Vec<Ty>>,
    call_params: &'a mut FxHashMap<Span, Vec<Ty>>,
    place_stores: &'a mut FxHashMap<Span, PlaceStore>,
    inherent_call_bounds: &'a mut FxHashMap<Span, InherentCallBounds>,
    generic_call_bounds: &'a mut FxHashMap<Span, GenericCallBounds>,
    is_tests: &'a mut FxHashMap<Span, Ty>,
    /// The member atom the directed probe resolved each directed
    /// label use to, keyed by the label's own span; exported through
    /// [`Checked::directed_tags`] for the lowering pass, which never
    /// re-derives the redirection.
    directed_tags: &'a mut FxHashMap<Span, TypeId>,
    handler_handled: &'a mut FxHashMap<Span, Ty>,
    /// Whether a qualified pattern label already reported its refusal,
    /// so the caller's unknown-pattern word stays quiet.
    pattern_label_reported: bool,
    hole_show_dispatches: &'a mut FxHashMap<Span, MethodId>,
    comptime_records: &'a mut FxHashMap<(Vec<u32>, Span), CopyRecords>,
    comptime_loops: &'a mut FxHashMap<Span, ComptimeLoop>,
    /// The per-instance expansions' per-copy records; the phase-1 walk
    /// never writes them (its copies are written-loop copies), so an
    /// empty table rides every construction but the expansion walks.
    comptime_instance_records: &'a mut FxHashMap<(DefId, Rc<Vec<Ty>>, Vec<u32>, Span), CopyRecords>,
    /// The per-instance expansions' projected fields, same keying.
    comptime_instance_loops: &'a mut FxHashMap<(DefId, Rc<Vec<Ty>>, Span), ComptimeLoop>,
    /// Every solved free-fn call site: the callee definition and the
    /// solved argument types, the expansion worklist's source.
    comptime_sites: &'a mut FxHashMap<Span, (DefId, Vec<Ty>)>,
    /// The active per-instance expansion walk, if this walk is one.
    instance: Option<InstanceRun>,
    diags: Vec<Diagnostic>,
    /// Minted type variables whose generic parameter subtracts the
    /// copyability constraint (`[t: ~Copyable]`); the binder facts
    /// consult them while recording.
    linear_vars: FxHashMap<TypeId, bool>,
    /// Declared types of locals, keyed by binder span; snapshot/restore
    /// at scope boundaries runs through its undo log, never a clone.
    locals: ScopedMap<Binding>,
    /// The enclosing for-ins' iterated bindings, innermost last: the
    /// body's `mut` window over one of them refuses. Pushed and
    /// popped around each body walk, so the stack is empty at every
    /// frame boundary.
    for_iters: Vec<IterWindow>,
    /// The enclosing nursery blocks' scope entries, innermost last: the
    /// locals live at each block's entry are the bindings that outlive
    /// it, so a spawned body's `mut` window over one of them refuses.
    nursery_scopes: Vec<NurseryScope>,
    /// Each enclosing nursery block's spawned-body answers, innermost
    /// last: the block's value is the collected answers' list, so every
    /// `spawn` inside it records its body's answer here. A lambda frame
    /// clears the stack for its body — a `spawn` inside a lambda names
    /// no nursery.
    spawn_answers: Vec<Vec<Val>>,
    /// Depth of spawned-body frames being walked: gates the
    /// spawn-scoped refinements (a `cancel` arm never answers inside a
    /// spawned body) and the outlived-binding refusal.
    spawn_depth: usize,
    /// The active `for comptime` copy path while a copy's body walks,
    /// outer-to-inner copy indices; empty outside one. Every record
    /// write routes through it.
    comptime_path: Vec<u32>,
    /// The enclosing `for comptime` loops' spans, innermost last; the
    /// loop's projected fields and binder span read from
    /// `comptime_loops` through it.
    comptime_stack: Vec<Span>,
    /// The refinement overlay, keyed by binder span.
    refined: ScopedMap<Ty>,
    /// Type-variable scopes, innermost last (generic parameters, `Self`).
    type_vars: Vec<FxHashMap<String, Ty>>,
    /// In-scope trait assumptions from generic bounds (`[t: json]`), with
    /// per-scope truncation marks.
    assumptions: Vec<(TraitId, Ty)>,
    assumption_marks: Vec<usize>,
    /// The enclosing return union; `None` outside function bodies.
    ret: Option<Ty>,
    loop_depth: usize,
    /// Break counts per enclosing loop, innermost last: index
    /// `loop_depth - 1` is the count for the loop being walked. The stack
    /// follows `loop_depth` at every seam (frame entry resets both), so a
    /// `while true` body with a zero top can never break out of its loop.
    loop_breaks: Vec<usize>,
    /// Depth of enclosing lambda and spawned-body frames. Loop exits
    /// never cross such a frame, so a stray exit inside one reports the
    /// crossed boundary instead of a missing loop.
    frame_depth: usize,
    /// Depth of condition positions being walked (if/match-guard/while
    /// conditions). An `is` test there refines legitimately — a disjoint
    /// test narrows a branch to empty, which is sound dead code — so the
    /// always-false diagnostic fires only at value/assert depth.
    cond_depth: usize,
    /// Cycle guard for order-independent top-level typing.
    visiting: Vec<DefId>,
    /// Whether the failed solve just below already reported the tuple
    /// distribution cap precisely; the generic fallbacks around
    /// `solve_here` consume it so DS2012 never layers under another code.
    solve_reported: bool,
    /// Whether the expression currently being checked has its value
    /// consumed (a binding's initializer, an argument, an operand, an
    /// if/match arm's answer). A `return` there is refused: the early
    /// exit is a statement, and the lowerer would emit the exit's
    /// dead answer into the consuming slot.
    value_pos: bool,
    /// Where the expectation currently fed to an expression was written,
    /// when the feeding site knows it (a callee's declared fn-typed
    /// parameter). A lambda-literal arity refusal cites it as the
    /// expected fn type's site; set immediately around the one `expr`
    /// call that carries the expectation, cleared after.
    expected_ty_span: Option<Span>,
    /// The comptime evaluation context flags, kept as one field
    /// so the walk's snapshot discipline treats them as a unit.
    comptime: ComptimeCtx,
}

/// The comptime evaluation context of one checker walk position.
///
/// `ctx` marks the walk inside a comptime function's body — the one
/// context where a type name is a value of the builtin `type` atom and
/// the reflection builtins answer. `args` marks the argument positions
/// of a comptime call (`schema(point)`), which open the same type-value
/// reading outside a comptime body. `declaring_fn` marks a top-level fn
/// declaration's initializer — the only surface a `comptime` parameter
/// takes here — consumed by the first lambda the walk enters.
#[derive(Clone, Copy, Default)]
struct ComptimeCtx {
    ctx: bool,
    args: bool,
    declaring_fn: bool,
}

// ---- free helpers -----------------------------------------------------------

/// The single identifier of a one-segment path expression, if it is one.
fn single_path_ident(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Path { path, .. } if path.segments.len() == 1 => path.segments.first(),
        _ => None,
    }
}

fn generic_of(param: &TypeParam) -> GenericParam {
    GenericParam {
        name: param.name.name.clone(),
        span: param.name.span,
        bound: param.bound.clone(),
    }
}

/// Whether a parameter binds a re-assignable place: only the `mut`
/// window re-binds — `move` transfers once, `comptime` is a compile-time
/// value, and a plain borrow is immutable — so assignment accepts a
/// `mut` parameter alone, exactly like a `var` local.
fn param_is_mut(param: &Param) -> bool {
    matches!(param.mode, ds_parse::ParamMode::Mut(_))
}

/// One comptime parameter's annotation: the
/// builtin `type` atom (the evaluator stages a reflection index) or one
/// of the four scalar atoms by name (it stages a plain literal word).
/// Anything else has no comptime staging and refuses at the declaration.
fn comptime_annotation(ty: Option<&Type>) -> bool {
    match ty {
        Some(Type::TypeKw(_)) => true,
        Some(Type::Path { path, args, .. }) if args.is_empty() => {
            let [only] = path.segments.as_slice() else {
                return false;
            };
            matches!(only.name.as_str(), "int" | "float" | "bool" | "string")
        },
        _ => false,
    }
}

/// A synthetic path type naming one identifier — the written form of a
/// bare fn-type parameter (`fn(t) -> r`), resolved through scopes like any
/// written type.
fn name_type(name: &str) -> Type {
    let span = Span::new(FileId::new(0), 0, 0);
    Type::Path {
        span,
        path: Path {
            span,
            segments: vec![Ident {
                span,
                name: name.to_owned(),
            }],
        },
        args: Vec::new(),
        delim: Delim::Bracket,
    }
}

/// One integer literal's i64 value with its written sign folded in:
/// the digit text (radix prefixes kept, separators already stripped by
/// the parser) must fit the two's-complement range at that sign, so a
/// leading `-` widens the bound to `9223372036854775808` while the bare
/// magnitude refuses. `None` when the digits are out of range.
fn int_literal_value(text: &str, neg: bool) -> Option<i64> {
    let (radix, digits) = match text.as_bytes() {
        [b'0', b'x' | b'X', ..] => (16, &text[2..]),
        [b'0', b'b' | b'B', ..] => (2, &text[2..]),
        [b'0', b'o' | b'O', ..] => (8, &text[2..]),
        _ => (10, text),
    };
    let magnitude = u64::from_str_radix(digits, radix).ok()?;
    if neg {
        // The min magnitude alone wraps under negation; every smaller
        // one negates in range.
        if magnitude == 1u64 << 63 { Some(i64::MIN) } else { Some(-i64::try_from(magnitude).ok()?) }
    } else {
        i64::try_from(magnitude).ok()
    }
}

fn peel_paren(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => peel_paren(inner),
        other => other,
    }
}

/// Whether a diagnostic's primary label sits inside `range`: a walk that
/// typed exactly those sources owns the diagnostic, so a re-walk replaces
/// it; anything else (a lazily typed binding's initializer reached from
/// inside) reports from elsewhere and stays.
fn reported_in(diag: &Diagnostic, range: Span) -> bool {
    diag.labels
        .iter()
        .find(|label| label.style == LabelStyle::Primary)
        .is_some_and(|label| {
            label.span.file == range.file
                && label.span.start >= range.start
                && label.span.end <= range.end
        })
}

/// Whether a written type names `name` anywhere a path segment sits.
fn type_mentions(ty: &Type, name: &str) -> bool {
    match ty {
        Type::Path { path, args, .. } => {
            path.segments.iter().any(|segment| segment.name == name)
                || args.iter().any(|arg| type_mentions(arg, name))
        },
        Type::Optional { inner, .. } | Type::Negation { inner, .. } | Type::Paren { inner, .. } => {
            type_mentions(inner, name)
        },
        Type::Union { left, right, .. }
        | Type::Intersect { left, right, .. }
        | Type::Difference { left, right, .. } => {
            type_mentions(left, name) || type_mentions(right, name)
        },
        Type::Tuple { items, .. } => items.iter().any(|item| type_mentions(item, name)),
        Type::Fn { params, ret, .. } => {
            params.iter().any(|param| match &param.ty {
                // A bare fn-type parameter (`fn(t) -> r`) is the
                // in-scope variable its name spells.
                None => param.name.name == name,
                Some(ty) => type_mentions(ty, name),
            }) || ret.as_ref().is_some_and(|r| type_mentions(r, name))
        },
        _ => false,
    }
}

fn is_none_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Path { path, .. }
            if path.segments.len() == 1 && path.segments[0].name == "none"
    )
}

/// Whether an expression is the literal `true` (a paren-wrapped spelling
/// included): the one condition a `while` never re-tests false.
fn is_true_expr(expr: &Expr) -> bool {
    matches!(peel_paren(expr), Expr::Literal {
        lit: Literal::Bool(true),
        ..
    })
}
