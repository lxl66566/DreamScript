//! The `type_of` pass: bidirectional checking with monotone flow refinement
//! (B2).
//!
//! # Engine shape
//!
//! Boundary anchoring per (B2): top-level signatures are REQUIRED and the
//! set algebra checks against them by containment; locals, lambdas, and
//! match arms infer. Inference failure is a compile error — there is no
//! `any` fallback anywhere (B1). Every type is a solved [`Ty`] from the
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
//! and the arm's pattern coverage) (B2). The overlay resets at assignment
//! and at loop entry (sound: refined is always contained in declared, so
//! the declared type is a legal widening). Known limit, from the record:
//! no narrowing through mutable fields of union-typed values.
//!
//! # E4 union errors
//!
//! `?` splits its operand against the success context: at an expected type
//! the members outside it propagate, elsewhere the members INSIDE the
//! enclosing return union propagate — either way every propagated member
//! must be contained in the written return union (DS2008). `or` extracts
//! the wanted members (the right operand's type for a plain tail, the
//! return union for `or return`); `.err(h)`/`.ok(h)` are difference-typed:
//! the handled set is the handler parameter when annotated, otherwise
//! everything but what the handler's own result keeps.
//!
//! # Out of scope here
//!
//! Effect rows; the affine/move pass (assumption bounds like `[t: json]`
//! feed method selection, `~Copyable` is M4); ranges, indexing, and
//! `in`-containment (they need the M3 stdlib's type vocabulary).

use std::{collections::HashMap, rc::Rc, sync::Arc};

use ds_diag::{Diagnostic, ErrorCode, Files, Span};
use ds_parse::{
    Ast, BinaryOp, BlockExpr, EnumPayload, Expr, FnDecl, Ident, Item, ItemKind, Literal, LocalDecl,
    MatchArm, Param, Path, Pattern, Stmt, StrPart, TraitMember, Type, TypeParam, UnaryOp,
};
use rustc_hash::FxHashMap;

use crate::{
    TypeExpr,
    atom::{AtomKind, TraitId, TypeId},
    decls::{self, Decls, FnSig, GenericParam, ParamSig},
    error::Error,
    extraction::Extraction,
    instance::{Condition, InstanceId},
    instance_solve::Resolution,
    resolve::{DefId, DefKind, DotRes, MethodId, MethodOwner, Package, Res, Resolutions},
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

/// One local binding: declared type plus assignability (C3).
#[derive(Clone, Copy, Debug)]
struct Binding {
    ty: Ty,
    mutable: bool,
}

/// One enclosing for-in's iterated binding: its resolved identity plus
/// the iterable's own use in the loop head. The loop froze its element
/// count at entry, so a body `mut` window over the same binding refuses
/// [D3].
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

/// The outcome of one interpolation hole's `show` proof (F6): dispatched
/// (the solved instance records at the hole's own span), refused (no
/// candidate serves, the F6 diagnostic answers), or reported (an
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

/// The result of one package check: diagnostics in walk order plus the
/// memoized type of every top-level binding (G3).
#[derive(Clone, Default)]
pub struct Checked {
    /// All diagnostics of collection, resolution, declaration, and bodies.
    pub diags: Vec<Diagnostic>,
    /// The solved type of every top-level const/var binding.
    pub def_types: FxHashMap<DefId, Ty>,
    /// Affine facts of every local binder, keyed by binder span: the
    /// copyability its type had at the binding site (C1). The linearity
    /// pass consumes this; top-level bindings derive theirs from
    /// `def_types` directly.
    pub binders: HashMap<Span, crate::linearity::BinderFact>,
    /// The checked `Val` of every expression, keyed by its span: the solved
    /// type AT THAT USE SITE (flow-refined inside `is` tests and match arms
    /// (B2)) plus the early-exit flag. The lowering pass (M3) consumes this
    /// side table instead of re-deriving types; entries are recorded in
    /// walk order and a re-walk overwrites them.
    pub expr_types: FxHashMap<Span, Val>,
    /// The instantiation of every generic call, keyed by the call's span:
    /// the solved type of each generic parameter, in declaration order
    /// (H3). Recorded only when every parameter solves (no `any` fallback
    /// (B1)); the lowering pass (M3) mints the CALLG evidence words from
    /// it.
    pub generic_calls: FxHashMap<Span, Vec<Ty>>,
    /// The solved dispatch of every trait-method call whose instance the
    /// environment statically resolved, keyed by the call's span (B6).
    /// The lowering pass mints the CALLG evidence word from it.
    pub trait_calls: FxHashMap<Span, TraitCall>,
    /// Every interpolation hole that staged an implicit `show` dispatch,
    /// keyed by the hole's own span (F6): the dispatched member (the
    /// solved trait member, the inherent method, or the trait member an
    /// in-scope assumption answers — the runtime-evidence form). The
    /// affine pass reads the receiver mode here and the effects pass
    /// unions the dispatched member's row; the lowering's synthetic call
    /// resolves through this record when no solved instance sits beside
    /// it.
    pub hole_show_dispatches: HoleShowDispatches,
    /// The selected inherent method of every inherent-impl dot-call whose
    /// owner covers the receiver, keyed by the call's own span (F3). The
    /// lowering pass resolves the impl item through the declaration
    /// tables' method-to-impl-item map; no instance exists for an
    /// inherent impl, so nothing else records at the site.
    pub inherent_calls: FxHashMap<Span, MethodId>,
    /// The runtime evidence a call into a generic inherent impl's body
    /// needs (H3): one entry per declared bound — the bound's variable
    /// atom, its trait, and the unique instance serving the substituted
    /// type at this call site (`None` when none serves it uniquely, the
    /// shape the lowerer's staging refusal owns). Keyed by the call's own
    /// span, mirroring [`TraitCall::bounds`] minus the instance word an
    /// inherent impl never has. Recorded only at sites entering a generic
    /// body; monomorphic inherent calls need no evidence.
    pub inherent_call_bounds: FxHashMap<Span, InherentCallBounds>,
    /// The runtime evidence a call into a free generic fn's body needs
    /// (H3): one entry per trait-bound generic parameter whose
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
    /// The member one dot-call's type-directed selection settled on,
    /// keyed by the callee name's own span (F3): the single survivor of
    /// the value-receiver tier — a trait member a solved instance or an
    /// in-scope assumption answers, or an inherent fn whose owner covers
    /// the receiver — or the member the type-keyed tier claimed. The
    /// dispatch records live at the call's span; this one names the
    /// member behind the name occurrence itself, so a name-position
    /// consumer reads the selection without re-deriving it.
    pub method_selections: FxHashMap<Span, MethodId>,
}

/// One trait-method call's solved dispatch (B6): the selected trait member
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
    /// The runtime evidence a call into a generic impl's body needs (H3):
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
/// affine pass (C1) and the lowering pass (M3) consume exactly these.
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
/// The check runs over the derive-expanded trees (G1): the trees the
/// caller hands in are the ones every table is built over, and
/// [`Analyzed::asts`] carries them back, so the lowering pass consumes
/// those, never the caller's originals. Expansion precedes resolution
/// (the landed ruling) and belongs to the meta layer: a caller with
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
        expr_types: checked.expr_types,
        generic_calls: checked.generic_calls,
        trait_calls: checked.trait_calls,
        hole_show_dispatches: checked.hole_show_dispatches,
        inherent_calls: checked.inherent_calls,
        inherent_call_bounds: checked.inherent_call_bounds,
        generic_call_bounds: checked.generic_call_bounds,
        is_tests: checked.is_tests,
        method_selections: checked.method_selections,
    };
    Analyzed {
        asts,
        package,
        resolutions,
        decls: Rc::new(decls),
        artifacts,
        checked: Arc::new(checked),
        resolve_diag_count,
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

/// [`analyze_served`] with import surfaces linked (G4): the link pass
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
        expr_types: checked.expr_types,
        generic_calls: checked.generic_calls,
        trait_calls: checked.trait_calls,
        hole_show_dispatches: checked.hole_show_dispatches,
        inherent_calls: checked.inherent_calls,
        inherent_call_bounds: checked.inherent_call_bounds,
        generic_call_bounds: checked.generic_call_bounds,
        is_tests: checked.is_tests,
        method_selections: checked.method_selections,
    };
    Analyzed {
        asts: asts.into(),
        package: package.into(),
        resolutions: tables.into(),
        decls: Rc::new(decls),
        artifacts,
        checked: Arc::new(checked),
        resolve_diag_count,
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
    let mut expr_types = FxHashMap::default();
    let mut generic_calls = FxHashMap::default();
    let mut trait_calls = FxHashMap::default();
    let mut inherent_calls = FxHashMap::default();
    let mut method_selections: FxHashMap<Span, MethodId> = FxHashMap::default();
    let mut inherent_call_bounds: FxHashMap<Span, InherentCallBounds> = FxHashMap::default();
    let mut generic_call_bounds: FxHashMap<Span, GenericCallBounds> = FxHashMap::default();
    let mut is_tests = FxHashMap::default();
    let mut hole_show_dispatches: FxHashMap<Span, MethodId> = FxHashMap::default();
    for (ast, res) in trees.iter().zip(resolutions) {
        let mut bodies = Bodies {
            files,
            package,
            res,
            resolutions,
            res_by_file: &res_by_file,
            decls: &mut decls,
            def_types: &mut def_types,
            binders: &mut binders,
            expr_types: &mut expr_types,
            generic_calls: &mut generic_calls,
            trait_calls: &mut trait_calls,
            inherent_calls: &mut inherent_calls,
            inherent_call_bounds: &mut inherent_call_bounds,
            generic_call_bounds: &mut generic_call_bounds,
            is_tests: &mut is_tests,
            hole_show_dispatches: &mut hole_show_dispatches,
            method_selections: &mut method_selections,
            diags: Vec::new(),
            linear_vars: FxHashMap::default(),
            locals: ScopedMap::default(),
            for_iters: Vec::new(),
            nursery_scopes: Vec::new(),
            spawn_answers: Vec::new(),
            spawn_depth: 0,
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
            value_pos: true,
            expected_ty_span: None,
            comptime: ComptimeCtx::default(),
        };
        bodies.file(ast);
        diags.append(&mut bodies.diags);
    }
    // The import-bound functions' checked types (G4): a foreign def has
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
            decls: &mut decls,
            def_types: &mut def_types,
            binders: &mut binders,
            expr_types: &mut expr_types,
            generic_calls: &mut generic_calls,
            trait_calls: &mut trait_calls,
            inherent_calls: &mut inherent_calls,
            inherent_call_bounds: &mut inherent_call_bounds,
            generic_call_bounds: &mut generic_call_bounds,
            is_tests: &mut is_tests,
            hole_show_dispatches: &mut hole_show_dispatches,
            method_selections: &mut method_selections,
            diags: Vec::new(),
            linear_vars: FxHashMap::default(),
            locals: ScopedMap::default(),
            for_iters: Vec::new(),
            nursery_scopes: Vec::new(),
            spawn_answers: Vec::new(),
            spawn_depth: 0,
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
    Ok((decls, Checked {
        diags,
        def_types,
        binders,
        expr_types,
        generic_calls,
        trait_calls,
        hole_show_dispatches,
        inherent_calls,
        inherent_call_bounds,
        generic_call_bounds,
        is_tests,
        method_selections,
    }))
}

/// The resolved graph's bound artifacts (H5): artifact name to its
/// installed library's exported C symbols, in enumeration order.
pub type ArtifactSymbols = std::collections::BTreeMap<String, Arc<Vec<String>>>;

/// The tables of one analyzed package; see [`analyze`]. Every field is a
/// shared handle, so cloning the bundle clones pointers — the query layer
/// hands it out of a memo without copying the namespace — and a consumer
/// holding the bundle keeps the universe its `Ty`s were minted by alive.
#[must_use]
#[derive(Clone)]
pub struct Analyzed {
    /// The derive-expanded trees the tables were built over (G1):
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
    /// The resolved graph's bound artifacts (H5): each `[artifacts]`
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

    fn get(&self, file: ds_diag::FileId) -> Option<usize> {
        self.0.get(file.as_u32() as usize).copied().flatten()
    }
}

/// The body checker over one file.
struct Bodies<'a> {
    files: &'a Files,
    package: &'a Package,
    res: &'a Resolutions,
    /// Every file's resolutions, aligned with the trees by position; a
    /// lazily typed top-level binding walks under its OWN file's table
    /// (G3), no matter which file's body triggered the memo fill.
    resolutions: &'a [Resolutions],
    res_by_file: &'a FilePositions,
    decls: &'a mut Decls,
    def_types: &'a mut FxHashMap<DefId, Ty>,
    binders: &'a mut HashMap<Span, crate::linearity::BinderFact>,
    expr_types: &'a mut FxHashMap<Span, Val>,
    generic_calls: &'a mut FxHashMap<Span, Vec<Ty>>,
    trait_calls: &'a mut FxHashMap<Span, TraitCall>,
    inherent_calls: &'a mut FxHashMap<Span, MethodId>,
    method_selections: &'a mut FxHashMap<Span, MethodId>,
    inherent_call_bounds: &'a mut FxHashMap<Span, InherentCallBounds>,
    generic_call_bounds: &'a mut FxHashMap<Span, GenericCallBounds>,
    is_tests: &'a mut FxHashMap<Span, Ty>,
    hole_show_dispatches: &'a mut FxHashMap<Span, MethodId>,
    diags: Vec<Diagnostic>,
    /// Minted type variables whose generic parameter subtracts the
    /// copyability constraint (`[t: ~Copyable]`) (C1); the binder facts
    /// consult them while recording.
    linear_vars: FxHashMap<TypeId, bool>,
    /// Declared types of locals, keyed by binder span; snapshot/restore
    /// at scope boundaries runs through its undo log, never a clone.
    locals: ScopedMap<Binding>,
    /// The enclosing for-ins' iterated bindings, innermost last: the
    /// body's `mut` window over one of them refuses [D3]. Pushed and
    /// popped around each body walk, so the stack is empty at every
    /// frame boundary.
    for_iters: Vec<IterWindow>,
    /// The enclosing nursery blocks' scope entries, innermost last: the
    /// locals live at each block's entry are the bindings that outlive
    /// it, so a spawned body's `mut` window over one of them refuses
    /// [E2].
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
    /// The refinement overlay, keyed by binder span (B2).
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
    /// Cycle guard for order-independent top-level typing (G3).
    visiting: Vec<DefId>,
    /// Whether the failed solve just below already reported the tuple
    /// distribution cap precisely; the generic fallbacks around
    /// `solve_here` consume it so DS2012 never layers under another code.
    solve_reported: bool,
    /// Whether the expression currently being checked has its value
    /// consumed (a binding's initializer, an argument, an operand, an
    /// if/match arm's answer). A `return` there is refused: the early
    /// exit is a statement [F2], and the lowerer would emit the exit's
    /// dead answer into the consuming slot.
    value_pos: bool,
    /// Where the expectation currently fed to an expression was written,
    /// when the feeding site knows it (a callee's declared fn-typed
    /// parameter). A lambda-literal arity refusal cites it as the
    /// expected fn type's site; set immediately around the one `expr`
    /// call that carries the expectation, cleared after.
    expected_ty_span: Option<Span>,
    /// The comptime evaluation context flags (G1, G2), kept as one field
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
/// takes this round — consumed by the first lambda the walk enters.
#[derive(Clone, Copy, Default)]
struct ComptimeCtx {
    ctx: bool,
    args: bool,
    declaring_fn: bool,
}

impl Bodies<'_> {
    /// Binds one local and records its affine facts (C1): copyability of
    /// the solved type under the in-scope generic bounds.
    fn bind_local(&mut self, span: Span, binding: Binding) {
        let copyable = crate::linearity::ty_copyable(self.decls, binding.ty, &self.linear_vars);
        self.binders.insert(span, crate::linearity::BinderFact {
            copyable,
            ty: binding.ty,
        });
        self.locals.insert(span, binding);
    }

    fn file(&mut self, ast: &Ast) {
        for block in &ast.blocks {
            for item in &block.items {
                self.item(item);
            }
        }
    }

    // ---- items ------------------------------------------------------------

    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(f) => {
                if f.body.is_some() {
                    self.fn_decl(f, None, false);
                }
            },
            ItemKind::Trait(t) => {
                self.push_generics(&t.generics.iter().map(generic_of).collect::<Vec<_>>());
                let self_ty = self.fresh_var("Self");
                // A trait's members see `Self` as satisfying their own
                // trait: a default body calls sibling members on `self`
                // without an instance — every eventual impl proves the
                // whole member set together. The assumption is scoped to
                // these bodies, never global, and bound recursion stays
                // depth-capped through it.
                let self_trait = self
                    .package
                    .lookup_type(&t.name.name)
                    .and_then(|def| self.decls.traits.get(&def).copied());
                if let Some(trait_id) = self_trait {
                    self.assumptions.push((trait_id, self_ty));
                }
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.fn_decl(f, Some(self_ty), true);
                    }
                }
                if self_trait.is_some() {
                    self.assumptions.pop();
                }
                self.pop_generics();
            },
            ItemKind::Impl(imp) => {
                self.impl_item(item, imp);
            },
            ItemKind::Local(local) => self.top_level_local(local),
            ItemKind::Struct(s) => self.struct_item(s),
            ItemKind::ExternFn(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Import(_)
            | ItemKind::Platform(_) => {},
        }
    }

    /// Type-checks every declared `deinit` body (C5): `self` is the
    /// dying instance, borrowed, typed the struct's own atom; the body
    /// types as a unit-returning function body. The recorded facts —
    /// the `self` binder at the member's span, the body's expression
    /// types — are what the deinit's runtime half consumes.
    fn struct_item(&mut self, s: &ds_parse::StructDecl) {
        let Some(def) = self.package.lookup_type(&s.name.name) else {
            return;
        };
        let Some(atom) = self.decls.atoms.get(&def).copied() else {
            return;
        };
        let generics: Vec<GenericParam> = s.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        for member in &s.members {
            let ds_parse::StructMember::Deinit { span, body } = member else {
                continue;
            };
            let Ok(self_ty) = self.decls.universe.atom_ty(atom) else {
                continue;
            };
            let saved = self.snapshot();
            self.locals.clear();
            self.refined.clear();
            self.bind_local(*span, Binding {
                ty: self_ty,
                mutable: false,
            });
            let unit = self.unit();
            let old_ret = self.ret;
            self.ret = Some(unit);
            let block = Expr::Block(body.clone());
            self.value_pos = false;
            let val = self.expr(&block, Some(unit));
            if !val.diverges {
                self.expect_contains(val, unit, body.span, "a deinit body");
            }
            self.ret = old_ret;
            self.restore(saved);
        }
        self.pop_generics();
    }

    fn impl_item(&mut self, item: &Item, imp: &ds_parse::ImplDecl) {
        let _ = item;
        let generics: Vec<GenericParam> = imp.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        let target = self.solve_here(&imp.target);
        let Some(target) = target else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "impl target `{}` does not solve against the package declarations",
                        decls::render_type(&imp.target)
                    ),
                    imp.target.span(),
                    "unsolvable target",
                );
            }
            self.pop_generics();
            return;
        };
        let trait_def = imp.trait_ref.as_ref().and_then(|tr| {
            let [only] = tr.path.segments.as_slice() else {
                return None
            };
            self.package.lookup_type(&only.name)
        });
        let member_names: Option<Vec<String>> =
            trait_def.and_then(|def| self.decls.trait_members.get(&def).cloned());
        // `Self` resolves to the impl's target while the written member
        // signatures compare (B6).
        self.type_vars
            .push(FxHashMap::from_iter([("Self".to_owned(), target)]));
        let subst = self.trait_arg_subst(imp, trait_def);
        for f in &imp.fns {
            if let Some(members) = &member_names {
                if !members.iter().any(|m| m == &f.name.name) {
                    self.error(
                        ErrorCode::UnresolvedName,
                        format!("the implemented trait has no member `{}`", f.name.name),
                        f.name.span,
                        "not a trait member",
                    );
                    continue;
                }
                if let (Some(def), Some(subst)) = (trait_def, &subst)
                    && let Some((sig, sig_span)) =
                        decls::trait_member_sig(self.package, self.decls, def, &f.name.name)
                {
                    impl_sig::agree_member_signature(self, f, &sig, sig_span, subst);
                }
            }
            let inferred = self.fn_decl(f, Some(target), false);
            // An inferred return is agreed through the body's checked tail
            // type, exactly as a written one is: the trait member's
            // registered signature stays the call-site contract.
            if f.ret.is_none()
                && let (Some(def), Some(subst)) = (trait_def, &subst)
                && let (Some((sig, sig_span)), Some(inferred)) = (
                    decls::trait_member_sig(self.package, self.decls, def, &f.name.name),
                    inferred,
                )
            {
                impl_sig::agree_inferred_return(self, f, &sig, sig_span, subst, inferred);
            }
        }
        // Every trait member without a default body needs a bodied impl
        // member: the registered signature checks call sites alone, so
        // the missing body would surface only at the dispatch seam.
        if let (Some(def), Some(members)) = (trait_def, &member_names) {
            impl_sig::agree_impl_completeness(self, imp, def, members);
        }
        self.type_vars.pop();
        self.pop_generics();
    }

    /// The trait's generic parameters substituted by the impl's trait
    /// reference, positionally (B6): every name the trait declares maps
    /// to the argument the reference spells. A trait declaring generics
    /// the reference does not spell reports once — its member signatures
    /// could not be spelled in the impl's scope at all.
    fn trait_arg_subst(
        &mut self,
        imp: &ds_parse::ImplDecl,
        trait_def: Option<DefId>,
    ) -> Option<FxHashMap<String, Type>> {
        let reference = imp.trait_ref.as_ref()?;
        let def = trait_def?;
        let names = self.decls.trait_generics.get(&def)?.clone();
        if names.len() != reference.args.len() {
            self.error(
                ErrorCode::ImplMemberSignatureMismatch,
                format!(
                    "the trait declares {} generic parameter(s); the impl's trait reference \
                     spells {} argument(s)",
                    names.len(),
                    reference.args.len()
                ),
                reference.span,
                "unsubstituted trait generics",
            );
            return None;
        }
        Some(
            names
                .into_iter()
                .zip(reference.args.iter().cloned())
                .collect(),
        )
    }

    /// A top-level binding: its initializer is typed (memoized for
    /// order-independent uses (G3)); a written annotation anchors the check.
    /// A destructuring pattern declares many defs — the memo and the cycle
    /// guard cover them all, so tuple/or/struct binders receive types like
    /// any other top-level name.
    fn top_level_local(&mut self, local: &LocalDecl) {
        let defs: Vec<DefId> = crate::pattern::pattern_binders(&local.pat)
            .into_iter()
            .filter_map(|(_, name)| self.package.lookup_value(name))
            .collect();
        if defs.is_empty() {
            return;
        }
        if defs.iter().all(|def| self.def_types.contains_key(def))
            || defs.iter().any(|def| self.visiting.contains(def))
        {
            return;
        }
        let visiting = self.visiting.len();
        self.visiting.extend(defs.iter().copied());
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;
        // The initializer is its own frame: a lazily triggered fill
        // must not inherit the triggering walk's loop and frame depths.
        let (outer_loops, outer_frames) = (self.loop_depth, self.frame_depth);
        let outer_breaks = std::mem::take(&mut self.loop_breaks);
        self.loop_depth = 0;
        self.frame_depth = 0;
        // A use in another file's body can trigger this lazy fill; the
        // initializer's spans resolve under the binding's OWN file table,
        // never the triggering file's [G3].
        let home_res = self.res;
        if let Some(index) = self.res_by_file.get(self.package.def(defs[0]).span.file)
            && let Some(res) = self.resolutions.get(index)
        {
            self.res = res;
        }

        // A top-level fn declaration's initializer is its lambda: mark it
        // so the lambda's own check knows a `comptime` parameter sits on
        // the round-one surface (a free fn), never on a nested lambda.
        let was = self.comptime;
        self.comptime.declaring_fn = matches!(local.init, Expr::Lambda { .. });
        self.value_pos = true;
        let mut val = self.expr(&local.init, None);
        self.comptime = was;
        if let Some(ann) = &local.ty {
            if let Some(expected) = self.solve_here(ann) {
                self.expect_contains(val, expected, local.init.span(), "annotated binding");
                val.ty = expected;
            } else if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "annotation `{}` does not solve against the package declarations",
                        decls::render_type(ann)
                    ),
                    ann.span(),
                    "unsolvable annotation",
                );
            }
        }
        for (name, def, ty) in self.pattern_defs(&local.pat, val.ty) {
            let _ = name;
            if let Some(def) = def {
                self.def_types.insert(def, ty);
            }
        }
        self.restore(saved);
        self.loop_depth = outer_loops;
        self.frame_depth = outer_frames;
        self.loop_breaks = outer_breaks;
        self.res = home_res;
        self.visiting.truncate(visiting);
    }

    /// One named function declaration: boundary-anchored (B2). `self_ty`
    /// binds `Self` inside trait/impl scopes, where the first parameter
    /// named `self` is the receiver — exempt from annotation and typed
    /// `Self` [record 6.1]. A trait member must spell the receiver
    /// (dynamic dispatch is receiver-carried, so DS2021 fires there);
    /// an inherent impl member may omit it — the receiverless member is
    /// the type-keyed static surface (the 2026-11-15 ruling). Returns
    /// the member's solved return — the written return, or the body's
    /// checked tail type when inferred — for the impl-member agreement
    /// checks.
    fn fn_decl(&mut self, f: &FnDecl, self_ty: Option<Ty>, trait_member: bool) -> Option<Ty> {
        let generics: Vec<GenericParam> = f.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        if let Some(target) = self_ty {
            self.type_vars
                .push(FxHashMap::from_iter([("Self".to_owned(), target)]));
        }
        // A trait member's signature declares its receiver as its first
        // parameter [F3]: both call forms feed the receiver first and
        // the trait dispatch keys on its type, so a trait signature
        // with no `self` has no reachable call form. An inherent impl
        // member without one is the type-keyed static surface instead.
        if trait_member && f.params.first().is_none_or(|p| p.name.name != "self") {
            self.error(
                ErrorCode::MemberWithoutReceiver,
                format!(
                    "member `{}` declares its receiver as its first parameter (`self` or `mut \
                     self`, typed `Self`)",
                    f.name.name
                ),
                f.name.span,
                "member without a receiver",
            );
        }
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;
        for (index, param) in f.params.iter().enumerate() {
            if index == 0 && self_ty.is_some() && param.name.name == "self" {
                if let Some(ann) = &param.ty {
                    self.error(
                        ErrorCode::AnnotatedReceiver,
                        "the receiver is spelled `self` or `mut self` with no annotation; it is \
                         typed `Self`"
                            .to_owned(),
                        ann.span(),
                        "annotated receiver",
                    );
                }
                let recv_ty = self_ty.unwrap_or_else(|| self.empty());
                self.bind_local(param.name.span, Binding {
                    ty: recv_ty,
                    mutable: param_is_mut(param),
                });
                continue;
            }
            if let Some(solved) = param.ty.as_ref().and_then(|t| self.solve_here(t)) {
                self.bind_local(param.name.span, Binding {
                    ty: solved,
                    mutable: param_is_mut(param),
                });
            } else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "parameter `{}` needs a type annotation at this boundary [B2]",
                            param.name.name
                        ),
                        param.name.span,
                        "unannotated boundary parameter",
                    );
                }
                let empty = self.empty();
                self.bind_local(param.name.span, Binding {
                    ty: empty,
                    mutable: param_is_mut(param),
                });
            }
        }
        let ret = f.ret.as_ref().and_then(|r| self.solve_here(r)).or_else(|| {
            f.ret.as_ref().map(|r| {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "return type `{}` does not solve against the package declarations",
                            decls::render_type(r)
                        ),
                        r.span(),
                        "unsolvable return type",
                    );
                }
                self.empty()
            })
        });
        let old_ret = self.ret;
        self.ret = ret;
        let mut body_val: Option<Val> = None;
        if let Some(body) = &f.body {
            self.value_pos = false;
            let val = self.block_expected(body, ret);
            if let Some(ret) = ret
                && !val.diverges
            {
                self.expect_contains(val, ret, body.span, "function body");
            }
            body_val = Some(val);
        }
        // An impl-member body's solved return rides the declaration
        // tables for the lowering pass: a member function has no package
        // DefId, so its solved signature lives here — the written return
        // against `Self`, or the body's own checked type when inferred.
        let solved = ret.or_else(|| body_val.as_ref().filter(|v| !v.poisoned).map(|v| v.ty));
        if self_ty.is_some()
            && let Some(ty) = solved
        {
            self.decls.impl_fn_rets.insert(f.span, ty);
        }
        self.ret = old_ret;
        self.restore(saved);
        if self_ty.is_some() {
            self.type_vars.pop();
        }
        self.pop_generics();
        solved
    }

    // ---- statements ---------------------------------------------------------

    fn block(&mut self, block: &BlockExpr) -> Val {
        self.block_expected(block, None)
    }

    /// A block whose final expression carries an expected type — the
    /// function-body anchor: a tail `?` splits its operand against the
    /// return union there, so a fully-contained operand yields itself
    /// instead of propagating everything (E4).
    fn block_expected(&mut self, block: &BlockExpr, expected: Option<Ty>) -> Val {
        // The tail expression occupies the block's own position (a frame
        // body's tail is a statement slot, a consumed block's tail feeds
        // its consumer); head statements are statements either way.
        let tail_pos = self.value_pos;
        let saved = self.snapshot();
        let mut val = Val {
            ty: self.unit(),
            diverges: false,
            poisoned: false,
        };
        let (head, last) = match block.stmts.split_last() {
            Some((last, head)) => (head, Some(last)),
            None => (&block.stmts[..], None),
        };
        for stmt in head {
            val = self.stmt(stmt);
        }
        match last {
            // Only an expression tail carries the expected type (the
            // function-body anchor); a binding tail still checks, its
            // block value stays unit [F2].
            Some(Stmt {
                kind: ds_parse::StmtKind::Expr(expr),
                ..
            }) => {
                self.value_pos = tail_pos;
                val = self.expr(expr, expected);
            },
            Some(other) => val = self.stmt(other),
            None => {},
        }
        self.restore(saved);
        val
    }

    fn stmt(&mut self, stmt: &Stmt) -> Val {
        match &stmt.kind {
            ds_parse::StmtKind::Local(local) => {
                self.local_stmt(local);
                Val {
                    ty: self.unit(),
                    diverges: false,
                    poisoned: false,
                }
            },
            ds_parse::StmtKind::Expr(expr) => {
                self.value_pos = false;
                self.expr(expr, None)
            },
        }
    }

    /// A statement-level binding: locals infer (B2).
    fn local_stmt(&mut self, local: &LocalDecl) {
        self.value_pos = true;
        let mut val = self.expr(&local.init, None);
        if let Some(ann) = &local.ty {
            if let Some(expected) = self.solve_here(ann) {
                self.expect_contains(val, expected, local.init.span(), "annotated binding");
                val.ty = expected;
            } else if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "annotation `{}` does not solve against the package declarations",
                        decls::render_type(ann)
                    ),
                    ann.span(),
                    "unsolvable annotation",
                );
            }
        }
        // A poisoned initializer already reported why its type is empty;
        // only a clean-but-empty initializer is its own root cause.
        if self.decls.universe.is_empty(val.ty)
            && !val.diverges
            && !val.poisoned
            && !self.take_solve_reported()
        {
            self.error(
                ErrorCode::InferenceFailed,
                "the initializer's type is empty; inference found no inhabitant [B1]".to_owned(),
                local.init.span(),
                "empty initializer type",
            );
        }
        self.bind_pattern(&local.pat, val.ty, local.mutable);
    }

    // ---- expressions ----------------------------------------------------------

    fn expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        // Expression-error epoch: a diagnostic pushed anywhere inside this
        // expression poisons its value — every enclosing position consumes
        // it without re-reporting (the containment helpers and the
        // operand-shape checks), so a root cause stands alone instead of
        // cascading into "empty"/wrapper-type mismatches above it.
        let before = self.diags.len();
        let mut val = self.expr_inner(e, expected);
        val.poisoned |= self.diags.len() > before;
        self.expr_types.insert(e.span(), val);
        val
    }

    fn expr_inner(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        // The position this expression occupies (statement or
        // value-consumed) was set by the parent walk; every sub-expression
        // defaults back to value-consumed, and the frame-like bodies (a
        // lambda's, a loop's, a spawned fiber's) and the arm inheritance
        // below override it.
        let pos = self.value_pos;
        self.value_pos = true;
        match e {
            Expr::Literal { span, lit } => self.literal(*span, lit),
            Expr::Str { span, parts } => self.string_expr(*span, parts),
            Expr::RawStr { span, .. } => self.named_ty(*span, "string", "raw string"),
            Expr::Lambda { .. } => self.lambda(e, expected),
            Expr::Path { path, .. } => self.path_expr(path, expected),
            Expr::Struct { .. } => self.struct_expr(e, expected),
            Expr::Tuple {
                items,
                trailing_comma,
                ..
            } => self.tuple_expr(e, items, *trailing_comma, expected),
            Expr::List { .. } => self.list_expr(e, expected),
            Expr::Paren { inner, .. } => {
                self.value_pos = pos;
                self.expr(inner, expected)
            },
            Expr::Call { .. } => self.call(e),
            Expr::Field { recv, name, .. } => self.field_access(recv, name),
            Expr::Index { span, recv, .. } => {
                if let Some(head) = single_path_ident(recv)
                    && self.package.lookup_type(&head.name).is_some()
                {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`{}` is a type; the application `{}[...]` constructs only in call \
                             position — `{name}[t]()` [F9, B3]",
                            head.name,
                            head.name,
                            name = head.name
                        ),
                        *span,
                        "type application outside a call",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                }
                self.error(
                    ErrorCode::InferenceFailed,
                    "index typing arrives with the stdlib collection types [F5, M3]".to_owned(),
                    *span,
                    "index",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            Expr::MapApply { span, .. } => {
                self.error(
                    ErrorCode::TypeMismatch,
                    "`map` is a type; the application `map[...]` constructs only in call position \
                     — `map[k, v]()` [F9, B3]"
                        .to_owned(),
                    *span,
                    "type application outside a call",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            // A fn type is a type spelling, never a value; the constructor
            // brackets consume it before value typing, so this arm holds
            // the boundary for any other position.
            Expr::FnType { span, .. } => {
                self.error(
                    ErrorCode::TypeMismatch,
                    "a fn type is a type spelling, not a value; it belongs in a constructor's \
                     brackets — `list[fn(int) -> int]()` [F2, F9]"
                        .to_owned(),
                    *span,
                    "fn type outside a type application",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                }
            },
            Expr::Try { expr, span } => self.try_expr(expr, expected, *span),
            Expr::Unary { op, expr, span } => self.unary(*op, expr, *span),
            Expr::Is { expr, ty, span, .. } => {
                let operand = self.expr(expr, None);
                let Some(solved) = self.solve_here(ty) else {
                    if !self.take_solve_reported() {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "the `is` test against `{}` does not solve",
                                decls::render_type(ty)
                            ),
                            ty.span(),
                            "unsolvable test type",
                        );
                    }
                    return Val {
                        ty: self.bool_ty(),
                        diverges: false,
                        poisoned: false,
                    };
                };
                self.is_tests.insert(*span, solved);
                // A provably-disjoint test computes a constant false, so in
                // value/assert position it is a compile error — the same
                // ruling as the constant `==` [B9 carve-out 2]. In
                // condition position (if/match guard/while, any depth) the
                // test refines legitimately: the kept branch narrows to
                // empty, which is sound dead code, and a match arm covering
                // nothing is the exhaustiveness story.
                if self.cond_depth == 0
                    && !operand.poisoned
                    && !self.decls.universe.is_empty(operand.ty)
                    && !self.decls.universe.is_empty(solved)
                    && self.decls.universe.is_disjoint(operand.ty, solved)
                {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the `is` test against `{}` is always false over `{}` [B2]",
                            self.decls.universe.render(solved),
                            self.decls.universe.render(operand.ty)
                        ),
                        ty.span(),
                        "always false",
                    );
                }
                Val {
                    ty: self.bool_ty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => self.binary(*op, left, right, *span),
            Expr::Range { span, .. } => {
                self.error(
                    ErrorCode::InferenceFailed,
                    "a range is not a storable value; ranges are `for`-in and `in` sugar [F7]"
                        .to_owned(),
                    *span,
                    "range value",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            Expr::Assign {
                op,
                left,
                right,
                span,
            } => self.assign(*op, left, right, *span),
            Expr::If {
                cond, then, else_, ..
            } => self.if_expr(cond, then, else_.as_deref(), expected, pos),
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.match_expr(scrutinee, arms, *span, expected, pos),
            Expr::While { cond, body, .. } => self.while_expr(cond, body),
            Expr::For {
                pat,
                iter,
                body,
                span,
            } => self.for_expr(pat, iter, body, *span),
            Expr::Return { expr, span } => {
                // The early exit is a statement [F2]: a block whose value
                // feeds a binding, an argument, or a join arm has no
                // lowering for it — the exit's answer is dead at the point
                // the consuming slot reads — so the position refuses
                // instead of trapping past a clean check.
                if pos {
                    self.error(
                        ErrorCode::ReturnInValuePosition,
                        "`return` is a statement; this position consumes the block's value, so \
                         the exit has no answer to feed it"
                            .to_owned(),
                        *span,
                        "return in a value position",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: true,
                        poisoned: true,
                    };
                }
                // The returned expression carries the declared return as
                // its expected type, matching the final-expression anchor:
                // a `?` operand splits against the return union here too
                // [E4].
                let value = match expr {
                    Some(expr) => self.expr(expr, self.ret),
                    None => Val {
                        ty: self.unit(),
                        diverges: false,
                        poisoned: false,
                    },
                };
                match self.ret {
                    Some(ret) => {
                        self.expect_contains(value, ret, *span, "return");
                    },
                    None => self.error(
                        ErrorCode::PropagationNotContained,
                        "`return` outside a function body".to_owned(),
                        *span,
                        "stray return",
                    ),
                }
                Val {
                    ty: self.empty(),
                    diverges: true,
                    poisoned: false,
                }
            },
            Expr::Break(span) | Expr::Continue(span) => {
                if self.loop_depth == 0 {
                    // The loop depth is frame-scoped: inside a lambda or
                    // spawned body it counts only that frame's own loops.
                    let (message, label) = if self.frame_depth > 0 {
                        (
                            "loop exits never cross a lambda or spawned-body frame boundary",
                            "frame-crossing loop exit",
                        )
                    } else {
                        ("loop exit outside a loop", "stray loop exit")
                    };
                    self.error(ErrorCode::InferenceFailed, message.to_owned(), *span, label);
                } else if matches!(e, Expr::Break(..)) {
                    // A break leaves its loop's value reachable, which a
                    // `while true` diverge decision below must see.
                    if let Some(count) = self.loop_breaks.last_mut() {
                        *count += 1;
                    }
                }
                Val {
                    ty: self.empty(),
                    diverges: true,
                    poisoned: false,
                }
            },
            Expr::Nursery { body, span } => self.nursery_block(body, *span),
            // A spawned body runs as its own fiber: its loop exits stay
            // inside its frame, and its answer rides the join's
            // collected list.
            Expr::Spawn { body, .. } => self.spawned_block(body),
            Expr::Defer { body, .. } => {
                self.value_pos = false;
                self.block(body);
                Val {
                    ty: self.unit(),
                    diverges: false,
                    poisoned: false,
                }
            },
            // A bare block inherits its consumer's position: statement
            // blocks keep statement exits, consumed blocks refuse them in
            // their tail.
            Expr::Block(block) => {
                self.value_pos = pos;
                self.block_expected(block, expected)
            },
        }
    }

    /// A while loop: the condition types in condition position (an
    /// always-false `is` test refines legitimately there), the body in
    /// statement position inside its own loop frame.
    fn while_expr(&mut self, cond: &Expr, body: &BlockExpr) -> Val {
        self.cond_depth += 1;
        let cond_val = self.expr(cond, None);
        self.cond_depth -= 1;
        self.expect_bool(cond_val, cond.span());
        let saved = self.snapshot();
        self.refined.clear();
        self.loop_depth += 1;
        self.loop_breaks.push(0);
        self.value_pos = false;
        let body_val = self.block(body);
        let breaks = self.loop_breaks.pop().unwrap_or(0);
        self.loop_depth -= 1;
        self.restore(saved);
        // A literal-`true` condition with no break in the body
        // never completes the loop normally, so the while keeps
        // the body's own diverge flag (`while true { return 1 }`
        // leaves through the return). Any other condition — or a
        // break — leaves the loop's unit value reachable, and a
        // `for` body's early return never diverges the `for`
        // itself: zero iterations complete it.
        Val {
            ty: self.unit(),
            diverges: is_true_expr(cond) && breaks == 0 && body_val.diverges,
            poisoned: false,
        }
    }

    /// A tuple literal: one element without a trailing comma is a
    /// parenthesized expression; the product type otherwise.
    fn tuple_expr(
        &mut self,
        e: &Expr,
        items: &[Expr],
        trailing_comma: bool,
        expected: Option<Ty>,
    ) -> Val {
        if items.len() == 1 && !trailing_comma {
            return self.expr(&items[0], expected);
        }
        // Bidirectional against an expected tuple type: a
        // single-member expectation of the pattern's arity feeds
        // each element its expected type, so the
        // expected-consuming forms (the empty list literal, a
        // struct literal, a nested tuple, `?`) infer inside a
        // tuple the way they do in a return position.
        let expected_elems: Option<Vec<Ty>> =
            expected.and_then(|exp| self.expected_tuple_elems(exp, items.len()));
        let mut elements = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let item_expected = expected_elems
                .as_ref()
                .and_then(|elems| elems.get(index).copied());
            elements.push(self.expr(item, item_expected).ty);
        }
        let ty = match self.decls.universe.product(&elements) {
            Ok(ty) => ty,
            Err(err) => {
                // The distribution cap is a real rejection, not an
                // inference fallback: render DS2012 instead of
                // degrading the whole expression to the empty type.
                self.error(
                    ErrorCode::TupleDistributionCap,
                    err.to_string(),
                    e.span(),
                    "tuple distribution cap",
                );
                // Downstream empty-type fallbacks must not re-report
                // this rejection as an inference hole.
                self.solve_reported = true;
                self.empty()
            },
        };
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    fn literal(&mut self, span: Span, lit: &Literal) -> Val {
        if let Literal::Int { suffix, text } = lit
            && suffix.is_empty()
            && int_literal_value(text, false).is_none()
        {
            self.error(
                ErrorCode::InvalidLiteral,
                format!("integer literal `{text}` does not fit the i64 range"),
                span,
                "out of range",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let (name, what) = match lit {
            Literal::Int { suffix, .. } => {
                if suffix.is_empty() {
                    ("int", "integer literal")
                } else {
                    (suffix.as_str(), "integer literal suffix")
                }
            },
            Literal::Float { suffix, .. } => {
                if suffix.is_empty() {
                    ("float", "float literal")
                } else {
                    (suffix.as_str(), "float literal suffix")
                }
            },
            Literal::Bool(_) => ("bool", "boolean literal"),
            Literal::Char(_) => ("char", "char literal"),
            Literal::Byte(_) => ("byte", "byte literal"),
        };
        self.named_ty(span, name, what)
    }

    /// The atom a literal names, by name, through the package universe
    /// (B7): the numeric vocabulary is declared data, never builtin
    /// identity — inference never falls back.
    fn named_ty(&mut self, span: Span, name: &str, what: &str) -> Val {
        if let Some(ty) = self
            .decls
            .universe
            .atom_by_name(name)
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
        {
            Val {
                ty,
                diverges: false,
                poisoned: false,
            }
        } else {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`{what}` needs a declared `{name}` type; inference never falls back [B1, B7]"
                ),
                span,
                "no such type",
            );
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        }
    }

    /// Interpolated strings: every hole is an explicit string context
    /// (F6) — a printable primitive (bool, char, float, int, string)
    /// converts through `str(e)` implicitly; every other operand is
    /// legal exactly when its type carries `show`, the same
    /// solved-instance proof a written `.show()` runs, the dispatch
    /// recorded at the hole's own span for the lowering's staging.
    fn string_expr(&mut self, span: Span, parts: &[StrPart]) -> Val {
        let string_ty = self.named_atom_ty(span, "string", "string literal");
        let Some(string_ty) = string_ty else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        for part in parts {
            let (hole_span, hole_val) = match part {
                StrPart::Text { .. } => continue,
                StrPart::Expr { span, expr } => (*span, self.expr(expr, Some(string_ty))),
                StrPart::Ident { span, name } => {
                    let val = self.ident_hole_value(name);
                    self.expr_types.insert(name.span, val);
                    // the hole's own span (`$x`, dollar included) is the
                    // dispatch record's key — the lowerer's synthetic
                    // member call carries it, and the name's own span
                    // keeps recording the operand's type
                    (*span, val)
                },
            };
            if self.printable_hole(hole_val.ty) {
                continue;
            }
            // An empty or poisoned hole is an already-reported operand,
            // never a new mismatch: the cascade stays with its root cause.
            if self.decls.universe.is_empty(hole_val.ty) || hole_val.poisoned {
                continue;
            }
            match self.hole_show_dispatch(hole_span, hole_val.ty, string_ty) {
                HoleShow::Dispatched | HoleShow::Reported => continue,
                HoleShow::Refused => {},
            }
            let hole_ty = hole_val.ty;
            let note = if self.decls.universe.members(hole_ty).len() == 1 {
                "derive `show` (`#[derive(show)]`) or implement `show` for this type"
            } else {
                "extract a member first [B4, E4], or implement `show` over the whole union"
            };
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "interpolation hole has type `{}`; the printable primitives (bool, char, \
                         float, int, string) convert implicitly, every other type carries `show` \
                         [F6]",
                        self.decls.universe.render(hole_ty)
                    ),
                )
                .with_primary(hole_span, "not a printable operand")
                .with_note(note),
            );
        }
        Val {
            ty: string_ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// One hole's `show` proof (F6): the candidate selection a written
    /// `.show()` runs — trait owners through the solved-instance
    /// environment (a union receiver through its covering sibling group),
    /// inherent owners by coverage, an in-scope assumption through the
    /// frame's runtime-evidence form — with the survivor's dispatch
    /// recorded at the hole's own span, exactly where a written call's
    /// span records it, so the lowering stages the ordinary member call.
    /// The survivor's registered signature must be the family's shape:
    /// the receiver alone, `string` back. Every dispatched hole records
    /// its member in `hole_show_dispatches` (the affine and effects
    /// passes' seam); a solved instance or an inherent owner records
    /// beside it as a written call does, while the assumption form
    /// records alone — its dispatch resolves at runtime through the
    /// evidence slot the entering site stages.
    fn hole_show_dispatch(&mut self, span: Span, ty: Ty, string_ty: Ty) -> HoleShow {
        let mut survivors: Vec<MethodId> = Vec::new();
        let mut runtime: Vec<MethodId> = Vec::new();
        let mut ambiguous = None;
        for candidate in self.package.method_candidates("show") {
            let Some(sig) = self.member_sig(candidate) else {
                continue;
            };
            // the family's call shape: the receiver alone, `string` back —
            // the hole stages no argument and concatenates one string word
            let receiver_only = sig.params.len() == 1 && sig.params[0].name == "self";
            let ret_string =
                sig.ret.as_ref().and_then(|ret| self.solve_here(ret)) == Some(string_ty);
            if !receiver_only || !ret_string {
                continue;
            }
            match &self.package.method(candidate).owner {
                MethodOwner::Trait(def) => {
                    let Some(trait_id) = self.decls.traits.get(def).copied() else {
                        continue;
                    };
                    if self.assumption_covers(trait_id, ty) {
                        runtime.push(candidate);
                        continue;
                    }
                    match self.instance_resolution(trait_id, ty) {
                        Resolution::Unique(_) | Resolution::Covered(_) => {
                            survivors.push(candidate);
                        },
                        Resolution::Ambiguous(candidates) => {
                            ambiguous = Some((trait_id, candidates));
                        },
                        Resolution::None | Resolution::BoundUnsat { .. } => {},
                    }
                },
                MethodOwner::Inherent(target) => {
                    if self.inherent_accepts(target, ty) {
                        survivors.push(candidate);
                    }
                },
            }
        }
        if let Some((trait_id, candidates)) = ambiguous {
            self.instance_ambiguity(trait_id, ty, &candidates, span);
            return HoleShow::Reported;
        }
        match (survivors.as_slice(), runtime.as_slice()) {
            ([only], []) => {
                self.record_solved_dispatch(*only, ty, None, span);
                self.hole_show_dispatches.insert(span, *only);
                self.expr_types.insert(span, Val {
                    ty: string_ty,
                    diverges: false,
                    poisoned: false,
                });
                HoleShow::Dispatched
            },
            ([], [only]) => {
                self.hole_show_dispatches.insert(span, *only);
                self.expr_types.insert(span, Val {
                    ty: string_ty,
                    diverges: false,
                    poisoned: false,
                });
                HoleShow::Dispatched
            },
            ([], []) => HoleShow::Refused,
            (survivors, runtime) => {
                self.error(
                    ErrorCode::AmbiguousCall,
                    format!(
                        "the interpolation hole's `show` is ambiguous between {} owners; write \
                         the dispatch inside the hole and qualify it [F3, F6]",
                        survivors.len() + runtime.len()
                    ),
                    span,
                    "ambiguous implicit dispatch",
                );
                HoleShow::Reported
            },
        }
    }

    /// One hole operand under the printable rule (F6): the string
    /// context itself, or exactly one printable primitive atom.
    fn printable_hole(&self, ty: Ty) -> bool {
        let [atom] = self.decls.universe.members(ty) else {
            return false;
        };
        let name = self
            .decls
            .universe
            .atom_decl(*atom)
            .map_or("", |decl| decl.name.as_str());
        matches!(name, "bool" | "char" | "float" | "int" | "string")
    }

    /// The value type of a bare `$name` hole: the single-segment path
    /// resolution, recorded at the name's own span so the lowerer reads
    /// it there (F6).
    fn ident_hole_value(&mut self, name: &Ident) -> Val {
        if let Some(res) = self.res.get(name.span) {
            match res {
                Res::Local { bind } => Val {
                    ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                    diverges: false,
                    poisoned: false,
                },
                Res::Def(def) => self.def_value(*def, name.span),
                Res::TraitMethod(_) | Res::Dot(_) | Res::Handler | Res::Module(_) => Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                },
            }
        } else if name.name == "none" && !self.package.declares("none") {
            Val {
                ty: self.none_ty(),
                diverges: false,
                poisoned: false,
            }
        } else {
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{}` does not resolve to a value", name.name),
                name.span,
                "not a value",
            );
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        }
    }

    fn path_expr(&mut self, path: &Path, expected: Option<Ty>) -> Val {
        if let [only] = path.segments.as_slice() {
            if let Some(res) = self.res.get(only.span) {
                match res {
                    Res::Local { bind } => Val {
                        ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                        diverges: false,
                        poisoned: false,
                    },
                    Res::Def(def) => {
                        // A type name where a `type` value is expected is
                        // the type-value reading (G1, G2): legal only in a
                        // comptime evaluation context — inside a comptime
                        // function's body, or feeding a comptime call's
                        // argument. A struct, enum, tag, or alias names a
                        // declared type; the reflection index it denotes is
                        // the evaluator's, never a runtime word.
                        if expected.is_some_and(|ty| self.is_type_atom(ty))
                            && self.names_declared_type(*def)
                        {
                            if self.comptime.ctx || self.comptime.args {
                                return Val {
                                    ty: self
                                        .decls
                                        .universe
                                        .atom_ty(TypeId::TYPE)
                                        .unwrap_or_else(|_| self.empty()),
                                    diverges: false,
                                    poisoned: false,
                                };
                            }
                            self.error(
                                ErrorCode::ComptimeValueOutside,
                                format!(
                                    "`{}` names a type; type values exist only inside comptime \
                                     evaluation [G2]",
                                    only.name
                                ),
                                only.span,
                                "type value outside comptime",
                            );
                            return Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: false,
                            };
                        }
                        self.def_value(*def, only.span)
                    },
                    Res::TraitMethod(_) | Res::Dot(_) | Res::Handler => Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    },
                    Res::Module(module) => {
                        // A module alias alone is not a value: only the
                        // qualified call behind it lowers [G4].
                        let alias = self.package.module_ref(*module).alias().to_owned();
                        self.error(
                            ErrorCode::InferenceFailed,
                            format!(
                                "`{alias}` is a module alias; call its members through it, as \
                                 `{alias}.name(args)` [G4]",
                            ),
                            only.span,
                            "a module reference is callable only",
                        );
                        Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                }
            } else if only.name == "none" && !self.package.declares("none") {
                Val {
                    ty: self.none_ty(),
                    diverges: false,
                    poisoned: false,
                }
            } else {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` does not resolve to a value", only.name),
                    only.span,
                    "not a value",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            }
        } else {
            self.error(
                ErrorCode::InferenceFailed,
                "qualified paths are callable method references, not values [F3]".to_owned(),
                path.span,
                "method reference",
            );
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        }
    }

    /// The value type of a package-level definition used as a value.
    fn def_value(&mut self, def: DefId, span: Span) -> Val {
        match self.package.def(def).kind {
            DefKind::Fn => match self.decls.fns.get(&def).cloned() {
                Some(sig) if sig.generics.is_empty() => self.fn_atom_of(&sig),
                Some(_) => {
                    self.error(
                        ErrorCode::InferenceFailed,
                        "a generic function used as a value needs its type arguments [H3]"
                            .to_owned(),
                        span,
                        "unapplied generic function",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                },
                None => Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                },
            },
            DefKind::Tag => {
                // A payloaded tag constructs by call form only, exactly a
                // minted payload case; the bare name is the cross-spelling.
                if self.decls.structs.contains_key(&def) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "`{}` is a tag with a payload; construct it by call form: `{}(...)`",
                            self.package.def(def).name,
                            self.package.def(def).name
                        ),
                        span,
                        "tag with a payload used bare",
                    );
                }
                match self.decls.atoms.get(&def).copied() {
                    Some(atom) => Val {
                        ty: self
                            .decls
                            .universe
                            .atom_ty(atom)
                            .unwrap_or_else(|_| self.empty()),
                        diverges: false,
                        poisoned: false,
                    },
                    None => Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    },
                }
            },
            DefKind::Case => {
                // A bare case is the unit form, like a tag; a payload
                // case constructs by call form only [B3], so a bare use
                // is the cross-spelling.
                let payload = self
                    .decls
                    .cases
                    .get(&def)
                    .map_or(EnumPayload::None, |c| c.payload.clone());
                if !matches!(payload, EnumPayload::None) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "`{}` is an enum case with a payload; construct it by call form: \
                             `{}(...)`",
                            self.package.def(def).name,
                            self.package.def(def).name
                        ),
                        span,
                        "payload case used bare",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                }
                match self.decls.atoms.get(&def).copied() {
                    Some(atom) => Val {
                        ty: self
                            .decls
                            .universe
                            .atom_ty(atom)
                            .unwrap_or_else(|_| self.empty()),
                        diverges: false,
                        poisoned: false,
                    },
                    None => Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    },
                }
            },
            DefKind::Const | DefKind::Var => self.const_ty(def, span),
            DefKind::Struct | DefKind::Enum | DefKind::TypeAlias | DefKind::Trait => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "`{}` is a type, not a value; construct structs with `name {{ ... }}`",
                        self.package.def(def).name
                    ),
                    span,
                    "type used as a value",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// The memoized type of a top-level binding, typed lazily so
    /// order-independent uses work (G3). A linked const has no tree here:
    /// its literal fact is the whole type story — the fact's own mint in
    /// this universe (a scalar's atom, a list's applied constructor),
    /// memoized like a solved initializer.
    fn const_ty(&mut self, def: DefId, span: Span) -> Val {
        if let Some(&ty) = self.def_types.get(&def) {
            return Val {
                ty,
                diverges: false,
                poisoned: false,
            };
        }
        if let Some(fact) = self.decls.foreign_consts.get(&def).cloned() {
            let ty = fact
                .ty(&mut self.decls.universe)
                .unwrap_or_else(|| self.empty());
            self.def_types.insert(def, ty);
            return Val {
                ty,
                diverges: false,
                poisoned: false,
            };
        }
        let Some(local) = self.decls.consts.get(&def).cloned() else {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "binding `{}` has no initializer to infer from",
                    self.package.def(def).name
                ),
                span,
                "no initializer",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        if self.visiting.contains(&def) {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "binding `{}` participates in a value cycle [G3]",
                    self.package.def(def).name
                ),
                span,
                "cycle",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        self.top_level_local(&local);
        let ty = self
            .def_types
            .get(&def)
            .copied()
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    fn lambda(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Lambda {
            span,
            generics,
            params,
            ret,
            body,
            ..
        } = e
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        // The script-entry fold's synthesized `main` reaches here as a
        // lambda the user never wrote; its own span is empty (no real node
        // owns an empty span — the synthesis's alias guard), so the
        // boundary label names the script entry, not a lambda.
        let body_label = if span.start == span.end { "script main body" } else { "lambda body" };
        let expected_sig = expected.and_then(|ty| self.decompose_fn(ty));
        // A lambda literal's own arity matches the expected fn type's
        // exactly: the call ABI stages the declared parameter count, so a
        // lambda taking fewer parameters would trap at the arity
        // validation the moment it is called — the mismatch refuses at
        // the literal's own check, never at the call.
        let arity_reported = expected_sig
            .as_ref()
            .is_some_and(|(expected_params, _)| expected_params.len() != params.len());
        if arity_reported {
            let list_span = match (params.first(), params.last()) {
                (Some(first), Some(last)) => {
                    Span::new(first.span.file, first.span.start, last.span.end)
                },
                _ => *span,
            };
            let expected_len = expected_sig
                .as_ref()
                .map_or(0, |(expected_params, _)| expected_params.len());
            let mut diagnostic = Diagnostic::error(
                ErrorCode::ArityMismatch,
                format!(
                    "the lambda takes {} parameters, the expected fn type takes {}",
                    params.len(),
                    expected_len
                ),
            )
            .with_primary(list_span, "lambda parameter list");
            // The feeding call site knows where its expectation was
            // written (the callee's declared fn type); an expectation
            // arriving by propagation (an arm, a tuple element) has no
            // written site and the message alone carries the fact.
            if let Some(ty_span) = self.expected_ty_span.take() {
                diagnostic = diagnostic.with_secondary(ty_span, "expected fn type");
            }
            self.diags.push(diagnostic);
        }
        // The body is a fresh expression world: a stale citation from
        // this lambda's own feeding site must not reach a nested lambda.
        self.expected_ty_span = None;
        // The comptime parameter surface (G1): round two admits it on a
        // free fn only, with every parameter comptime and annotated
        // `type` (the evaluator stages one reflection index per
        // parameter) or one of the four scalar atoms — the evaluator
        // stages a plain literal of the annotation — so any other shape
        // refuses at the declaration instead of at each call site.
        let on_fn_decl = self.comptime.declaring_fn;
        self.comptime.declaring_fn = false;
        let comptime_fn = params
            .iter()
            .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)));
        if comptime_fn {
            let broken = if !on_fn_decl {
                Some("a comptime parameter is a free-fn surface; nested lambdas take none")
            } else if !generics.is_empty() {
                Some("a comptime function declares no type parameters this round")
            } else if params
                .iter()
                .any(|p| !matches!(p.mode, ds_parse::ParamMode::Comptime(_)))
            {
                Some("every parameter of a comptime function is comptime this round")
            } else if params.iter().any(|p| !comptime_annotation(p.ty.as_ref())) {
                Some(
                    "a comptime parameter is annotated `type` or one of the scalar atoms int, \
                     float, bool, string this round",
                )
            } else {
                None
            };
            if let Some(broken) = broken {
                self.error(
                    ErrorCode::ComptimeParamShape,
                    broken.to_owned(),
                    params
                        .iter()
                        .find_map(|p| match p.mode {
                            ds_parse::ParamMode::Comptime(span) => Some(span),
                            _ => None,
                        })
                        .unwrap_or(*span),
                    "comptime parameter",
                );
            }
        }
        self.push_generics(&generics.iter().map(generic_of).collect::<Vec<_>>());
        let saved = self.snapshot();
        let mut param_tys = Vec::with_capacity(params.len());
        for (index, param) in params.iter().enumerate() {
            let solved = param
                .ty
                .as_ref()
                .and_then(|t| self.solve_here(t))
                .or_else(|| {
                    expected_sig
                        .as_ref()
                        .and_then(|(ps, _)| ps.get(index).copied())
                });
            let Some(solved) = solved else {
                // A parameter past a mismatched expectation's length
                // already has its root cause (the arity refusal); an
                // uninferable-parameter report would be a follow-on.
                if !arity_reported && !self.take_solve_reported() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "parameter `{}` has no expected type to infer from; annotate it [B2]",
                            param.name.name
                        ),
                        param.name.span,
                        "uninferable parameter",
                    );
                }
                let empty = self.empty();
                param_tys.push(empty);
                self.bind_local(param.name.span, Binding {
                    ty: empty,
                    mutable: false,
                });
                continue;
            };
            param_tys.push(solved);
            // A `type`-annotated parameter without the comptime mode has
            // no runtime spelling: nothing constructs the builtin atom's
            // values outside comptime evaluation (G2), so the annotation
            // refuses here rather than at every call.
            if self.is_type_atom(solved) && !matches!(param.mode, ds_parse::ParamMode::Comptime(_))
            {
                self.error(
                    ErrorCode::TypeParamNotComptime,
                    format!(
                        "parameter `{}` is annotated `type`; a type value exists only inside \
                         comptime evaluation, so spell the parameter `comptime {}`",
                        param.name.name, param.name.name
                    ),
                    param.name.span,
                    "type parameter without the comptime mode",
                );
            }
            self.bind_local(param.name.span, Binding {
                ty: solved,
                mutable: param_is_mut(param),
            });
        }
        let declared_ret = ret.as_ref().and_then(|r| self.solve_here(r));
        let fn_ret_expected = declared_ret.or_else(|| expected_sig.as_ref().map(|(_, r)| *r));
        // A still-unbound variable as the expected return is the
        // instantiation's inference hole (a generic callee's `r`): the
        // lambda's own body type solves it, checked by the caller's
        // match against the fn-typed parameter.
        let expected_is_var = fn_ret_expected.is_some_and(|r| self.is_var_ty(r));
        let old_ret = self.ret;
        self.ret = if expected_is_var { None } else { fn_ret_expected };
        // A lambda body starts its own frame: the lexical loop depth
        // stays with the enclosing frame. The synthesized script entry
        // keeps the file walk's depth; its stray exits have no loop, and
        // the boundary message would name a lambda the user never wrote.
        let frame = (span.start != span.end).then(|| self.enter_frame());
        // A comptime function's body checks under the comptime context
        // (G1): type names are values of the builtin atom there and the
        // reflection builtins answer. Nested lambdas inside the body keep
        // the context — they evaluate inside the same compile-time run.
        let outer_ctx = self.comptime;
        self.comptime.ctx |= comptime_fn;
        // A real lambda's body is its own frame: a `spawn` lexically
        // inside it names no nursery (the entry lowers as its own
        // function), so the answer collector clears for the body and
        // restores after. The script entry's synthesized `main` keeps
        // the stack — its body is the file's top level, where a nursery
        // stands exactly like one inside a declared fn.
        let outer_answers =
            (span.start != span.end).then(|| std::mem::take(&mut self.spawn_answers));
        // The lambda's body is its own frame's statement stream: its tail
        // may exit early like any function body's.
        self.value_pos = false;
        let body_val = self.expr(body, self.ret);
        if let Some(outer_answers) = outer_answers {
            self.spawn_answers = outer_answers;
        }
        self.comptime = outer_ctx;
        if let Some(frame) = frame {
            self.exit_frame(frame);
        }
        let fn_ret = match fn_ret_expected {
            Some(r) if !expected_is_var => {
                if !body_val.diverges {
                    self.expect_contains(body_val, r, body.span(), body_label);
                }
                r
            },
            _ => body_val.ty,
        };
        self.ret = old_ret;
        self.restore(saved);
        self.pop_generics();
        let params_boxed: Box<[Ty]> = param_tys.into();
        let ty = self
            .decls
            .universe
            .fn_atom(&params_boxed, fn_ret)
            .ok()
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            // The arity refusal is the root cause; enclosing containment
            // checks consume the poisoned value without re-reporting.
            poisoned: arity_reported,
        }
    }

    fn struct_expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Struct { path, fields, span } = e else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let [only] = path.segments.as_slice() else {
            self.error(
                ErrorCode::UnresolvedName,
                "qualified struct literals resolve through imports [G4]".to_owned(),
                path.span,
                "qualified path",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let Some(def) = self
            .package
            .lookup_type(&only.name)
            .or_else(|| self.decls.foreign_type(&only.name))
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }; // resolver reported
        };
        // An enum case constructs by call form only [B3]; the brace form
        // is the struct construction, so this is the cross-spelling. A
        // payloaded tag keeps the same discipline.
        if self.decls.cases.contains_key(&def)
            || (self.package.def(def).kind == DefKind::Tag && self.decls.structs.contains_key(&def))
        {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` is an enum case; construct it by call form: `{}(...)`",
                    only.name, only.name
                ),
                only.span,
                "brace form on an enum case",
            );
            for (_, value) in fields {
                if let Some(expr) = value {
                    self.expr(expr, None);
                }
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let Some(shape) = self.decls.structs.get(&def).cloned() else {
            self.error(
                ErrorCode::TypeMismatch,
                format!("`{}` does not construct a struct", only.name),
                only.span,
                "not a struct",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        // Substitute the constructor's parameters: from an expected
        // application of the same constructor, else from in-scope variables
        // named like the parameters (generic construction inside its own
        // scope) [B2].
        let mut subst = FxHashMap::default();
        if !shape.params.is_empty() {
            if let Some(expected) = expected
                && let Some((ctor, args)) = self.single_apply(expected)
                && self.decls.defs_of_atoms.get(&ctor) == Some(&def)
            {
                for (param, arg) in shape.params.iter().zip(args.iter()) {
                    subst.insert(param.name.clone(), *arg);
                }
            }
            let scoped: Vec<(String, Ty)> = shape
                .params
                .iter()
                .filter_map(|p| {
                    self.type_vars
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(&p.name).copied())
                        .map(|ty| (p.name.clone(), ty))
                })
                .collect();
            for (name, ty) in scoped {
                subst.entry(name).or_insert(ty);
            }
        }
        self.type_vars.push(subst);
        let mut written = vec![false; shape.fields.len()];
        for (name, value) in fields {
            let Some(index) = shape.fields.iter().position(|f| f.name.name == name.name) else {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!("`{}` has no field `{}`", only.name, name.name),
                    name.span,
                    "no such field",
                );
                continue;
            };
            written[index] = true;
            let field_ty = self.solve_here(&shape.fields[index].ty);
            let Some(field_ty) = field_ty else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("field `{}` of `{}` does not solve", name.name, only.name),
                        name.span,
                        "unsolvable field type",
                    );
                }
                continue;
            };
            match value {
                Some(expr) => {
                    let val = self.expr(expr, Some(field_ty));
                    self.expect_contains(val, field_ty, expr.span(), "field value");
                },
                None => {
                    // Punning: the field name is itself a value use.
                    if let Some(res) = self.res.get(name.span) {
                        let val = match res {
                            Res::Local { bind } => Val {
                                ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                                diverges: false,
                                poisoned: false,
                            },
                            Res::Def(def) => self.def_value(*def, name.span),
                            _ => Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: false,
                            },
                        };
                        self.expect_contains(val, field_ty, name.span, "field value");
                    }
                },
            }
        }
        for (index, &is_written) in written.iter().enumerate() {
            if !is_written {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "`{}` literal misses field `{}`",
                        only.name, shape.fields[index].name.name
                    ),
                    *span,
                    "missing field",
                );
            }
        }
        let ty = if shape.params.is_empty() {
            self.decls
                .atoms
                .get(&def)
                .and_then(|&a| self.decls.universe.atom_ty(a).ok())
                .unwrap_or_else(|| self.empty())
        } else {
            let bound: Option<Vec<Ty>> = shape
                .params
                .iter()
                .map(|p| self.type_vars.last().and_then(|s| s.get(&p.name).copied()))
                .collect();
            if let Some(args) = bound {
                let ctor = self.decls.atoms.get(&def).copied().unwrap_or(TypeId::NONE);
                self.decls
                    .universe
                    .apply_atom(ctor, &args)
                    .ok()
                    .and_then(|a| self.decls.universe.atom_ty(a).ok())
                    .unwrap_or_else(|| self.empty())
            } else {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "constructing generic `{}` needs an expected type or in-scope parameters \
                         [B2]",
                        only.name
                    ),
                    *span,
                    "unbound generic construction",
                );
                self.empty()
            }
        };
        self.type_vars.pop();
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// One list literal `[a, b, c]` (F7, B3). Bidirectional: an expected
    /// `list[t]` fixes the element type and every item checks against it;
    /// without one, the first item fixes the type and a later item
    /// outside it is the mixed-element error. The empty literal has no
    /// element evidence — it needs the expected type, and `list[t]()`
    /// is the always-spellable empty form.
    fn list_expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::List { span, items, .. } = e else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let Some(list_def) = self.package.lookup_type("list") else {
            for item in items {
                self.expr(item, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }; // the resolver reported the unknown type
        };
        let expected_elem = expected.and_then(|exp| {
            let (ctor, args) = self.single_apply(exp)?;
            let [elem] = args.as_slice() else {
                return None;
            };
            (self.decls.defs_of_atoms.get(&ctor) == Some(&list_def)).then_some(*elem)
        });
        let elem = if items.is_empty() {
            let Some(elem) = expected_elem else {
                self.error(
                    ErrorCode::InferenceFailed,
                    "the empty list literal needs an expected `list[t]`; write `list[t]()` for \
                     the explicit empty list [B3]"
                        .to_owned(),
                    *span,
                    "empty list literal",
                );
                // downstream empty-type fallbacks must not re-report
                self.solve_reported = true;
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            };
            elem
        } else if let Some(elem) = expected_elem {
            elem
        } else {
            self.expr(&items[0], None).ty
        };
        for (index, item) in items.iter().enumerate() {
            if index == 0 && expected_elem.is_none() {
                continue; // walked above for the element evidence
            }
            let val = self.expr(item, Some(elem));
            if val.diverges || self.decls.universe.contains(elem, val.ty) {
                continue;
            }
            match expected_elem {
                Some(_) => {
                    self.expect_contains(val, elem, item.span(), "list element");
                },
                None => {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "list literal mixes element types: item {} has type `{}`, the first \
                             item `{}` [B2]",
                            index + 1,
                            self.decls.universe.render(val.ty),
                            self.decls.universe.render(elem)
                        ),
                        item.span(),
                        "mixed element types",
                    );
                },
            }
        }
        let ty = self.applied_list_ty(elem).unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// One generic-application constructor call `list[page]()` (F9, B3):
    /// the head resolved in the type namespace, so the bracketed form is
    /// a type application, never an index. Returns `None` when the head
    /// names no type — the ordinary index error reports instead.
    fn ctor_apply_call(
        &mut self,
        recv: &Expr,
        index: &Expr,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Option<Val> {
        let head = single_path_ident(recv)?;
        let def = self.package.lookup_type(&head.name)?;
        let walk_args = |this: &mut Self| {
            for arg in args {
                this.expr(&arg.value, None);
            }
            if let Some(tail) = trailing {
                this.expr(tail, None);
            }
        };
        let is_list = |this: &Self| {
            this.decls
                .structs
                .get(&def)
                .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty())
        };
        // the channel's construction (the 2026-11-21 shared-state
        // ruling): the one type application that carries a value
        // argument — the bound, `chan[int](4)` — because the payload
        // type never reaches the code (every channel one Ref word) while
        // the bound does
        let is_chan = |this: &Self| {
            head.name.as_str() == "chan"
                && this
                    .decls
                    .structs
                    .get(&def)
                    .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty())
        };
        if !is_list(self) && !is_chan(self) {
            // Construction is fixed per declaration [B3]; every type
            // application besides the collection surface is the
            // cross-spelling error.
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` constructs per its declaration, not by type application [B3]",
                    head.name
                ),
                span,
                "cross-spelling construction",
            );
            walk_args(self);
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        let chan_bound = if is_chan(self) {
            if trailing.is_some() {
                self.error(
                    ErrorCode::TypeMismatch,
                    "the channel constructor takes no trailing block".to_owned(),
                    span,
                    "trailing block",
                );
                self.solve_reported = true;
                return Some(self.empty_val());
            }
            let [cap] = args else {
                self.error(
                    ErrorCode::TypeMismatch,
                    "the channel constructor takes the bound alone: `chan[t](n)`".to_owned(),
                    span,
                    "constructor arguments",
                );
                walk_args(self);
                self.solve_reported = true;
                return Some(self.empty_val());
            };
            let cap_val = self.expr(&cap.value, None);
            let bound = self.named_ty(cap.value.span(), "int", "the channel bound");
            self.expect_contains(cap_val, bound.ty, cap.value.span(), "the channel bound");
            true
        } else {
            false
        };
        if trailing.is_some() {
            self.error(
                ErrorCode::TypeMismatch,
                "a type application takes no trailing block".to_owned(),
                span,
                "trailing block",
            );
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        if !args.is_empty() && !chan_bound {
            self.error(
                ErrorCode::TypeMismatch,
                "the empty-list constructor takes no arguments; build with a list literal `[...]` \
                 [B3]"
                    .to_owned(),
                span,
                "constructor arguments",
            );
            walk_args(self);
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        let Some(arg_ty) = Self::ctor_type_arg(index) else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "the bracketed argument of `{}[...]` is a type, not a value [F9]",
                    head.name
                ),
                index.span(),
                "not a type",
            );
            self.solve_reported = true;
            return Some(self.empty_val());
        };
        let Some(elem) = self.solve_here(&arg_ty) else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "the type argument `{}` does not solve against the package declarations",
                        decls::render_type(&arg_ty)
                    ),
                    arg_ty.span(),
                    "unsolvable type argument",
                );
            }
            self.solve_reported = true;
            return Some(self.empty_val());
        };
        let ty = if chan_bound { self.applied_chan_ty(elem)? } else { self.applied_list_ty(elem)? };
        Some(Val {
            ty,
            diverges: false,
            poisoned: false,
        })
    }

    /// The map constructor call `map[k, v]()` (F9, B3): the mirror of the
    /// list constructor over the two-argument application of the map
    /// struct. Construction takes no value arguments — the builder call
    /// (`map_set`) fills the map, the interim the record pins while the
    /// map literal rides the anonymous-record-literal reservation.
    fn map_ctor_call(
        &mut self,
        head: &Ident,
        key: &Expr,
        value: &Expr,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let walk_args = |this: &mut Self| {
            for arg in args {
                this.expr(&arg.value, None);
            }
            if let Some(tail) = trailing {
                this.expr(tail, None);
            }
        };
        let Some(def) = self.package.lookup_type(&head.name) else {
            // the resolver already reported the unknown type name
            self.solve_reported = true;
            return self.empty_val();
        };
        let is_map = self
            .decls
            .structs
            .get(&def)
            .is_some_and(|shape| shape.params.len() == 2 && shape.fields.is_empty());
        if !is_map {
            // Construction is fixed per declaration [B3]; the map
            // constructs by type application, any other head is the
            // cross-spelling error.
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` constructs per its declaration, not by type application [B3]",
                    head.name
                ),
                span,
                "cross-spelling construction",
            );
            walk_args(self);
            self.solve_reported = true;
            return self.empty_val();
        }
        if trailing.is_some() {
            self.error(
                ErrorCode::TypeMismatch,
                "a type application takes no trailing block".to_owned(),
                span,
                "trailing block",
            );
            self.solve_reported = true;
            return self.empty_val();
        }
        if !args.is_empty() {
            self.error(
                ErrorCode::TypeMismatch,
                "the empty-map constructor takes no arguments; build by `map_set` calls [B3]"
                    .to_owned(),
                span,
                "constructor arguments",
            );
            walk_args(self);
            self.solve_reported = true;
            return self.empty_val();
        }
        let arg_ty = |this: &mut Self, arg: &Expr| -> Option<Ty> {
            let Some(ty) = Self::ctor_type_arg(arg) else {
                this.error(
                    ErrorCode::TypeMismatch,
                    "the bracketed arguments of `map[...]` are types, not values [F9]".to_owned(),
                    arg.span(),
                    "not a type",
                );
                this.solve_reported = true;
                return None;
            };
            let Some(solved) = this.solve_here(&ty) else {
                if !this.take_solve_reported() {
                    this.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the type argument `{}` does not solve against the package \
                             declarations",
                            decls::render_type(&ty)
                        ),
                        ty.span(),
                        "unsolvable type argument",
                    );
                }
                this.solve_reported = true;
                return None;
            };
            Some(solved)
        };
        let (Some(key_ty), Some(value_ty)) = (arg_ty(self, key), arg_ty(self, value)) else {
            return self.empty_val();
        };
        let Some(ty) = self.applied_map_ty(key_ty, value_ty) else {
            self.solve_reported = true;
            return self.empty_val();
        };
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// The bracketed argument of a type application: a type spelled in
    /// expression position (F9) — a path, a parenthesized form, a tuple of
    /// the same, a fn type (`fn(int) -> int`), or a nested generic
    /// application (`list[string]`, `map[string, int]`) whose own bracketed
    /// arguments recurse.
    fn ctor_type_arg(arg: &Expr) -> Option<Type> {
        match arg {
            Expr::Path { span, path, .. } if path.segments.len() == 1 => Some(Type::Path {
                span: *span,
                path: path.clone(),
                args: Vec::new(),
            }),
            Expr::FnType { ty, .. } => Some((**ty).clone()),
            Expr::Paren { inner, .. } => Self::ctor_type_arg(inner),
            Expr::Tuple { items, span, .. } => Some(Type::Tuple {
                span: *span,
                items: items.iter().filter_map(Self::ctor_type_arg).collect(),
            }),
            Expr::Index { recv, index, .. } => {
                let head = single_path_ident(recv)?;
                let arg = Self::ctor_type_arg(index)?;
                Some(Type::Path {
                    span: recv.span(),
                    path: Path {
                        span: head.span,
                        segments: vec![head.clone()],
                    },
                    args: vec![arg],
                })
            },
            Expr::MapApply {
                head, key, value, ..
            } => {
                let key = Self::ctor_type_arg(key)?;
                let value = Self::ctor_type_arg(value)?;
                Some(Type::Path {
                    span: head.span,
                    path: Path {
                        span: head.span,
                        segments: vec![head.clone()],
                    },
                    args: vec![key, value],
                })
            },
            _ => None,
        }
    }

    /// The applied `list[t]` type over one element type.
    fn applied_list_ty(&mut self, elem: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("list")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[elem]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    /// The applied `chan[t]` type over the payload type.
    fn applied_chan_ty(&mut self, elem: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("chan")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[elem]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    /// The applied `map[k, v]` type over the key and value types.
    fn applied_map_ty(&mut self, key: Ty, value: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("map")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[key, value]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    fn call(&mut self, e: &Expr) -> Val {
        let Expr::Call {
            callee,
            args,
            trailing,
            span,
        } = e
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let callee = peel_paren(callee);
        if let Expr::Field { recv, name, .. } = callee {
            return self.dot_call(recv, name, args, trailing.as_deref(), *span);
        }
        // A generic-application constructor `list[t]()` [F9]: the callee
        // head resolves in the type namespace.
        if let Expr::Index { recv, index, .. } = callee
            && let Some(val) = self.ctor_apply_call(recv, index, args, trailing.as_deref(), *span)
        {
            return val;
        }
        // The map constructor `map[k, v]()` [F9, B3]: the head resolved
        // in the type namespace.
        if let Expr::MapApply {
            head, key, value, ..
        } = callee
        {
            return self.map_ctor_call(head, key, value, args, trailing.as_deref(), *span);
        }
        if let Expr::Path { path, .. } = callee {
            match path.segments.as_slice() {
                [only] => {
                    if let Some(Res::Def(def)) = self.res.get(only.span) {
                        let def = *def;
                        if self.decls.fns.contains_key(&def) {
                            let val = self.call_def(def, None, args, trailing.as_deref(), *span);
                            return self.spawn_cancel_refines(def, val);
                        }
                        // A minted case constructs by call form [B3]; a
                        // bare case falls through to the tag error below.
                        if let Some(case) = self.decls.cases.get(&def)
                            && !matches!(case.payload, EnumPayload::None)
                        {
                            return self.case_call(def, only, args, trailing.as_deref(), *span);
                        }
                        // A payloaded tag constructs by call form too: the
                        // same named-field slots over its own struct shape.
                        if self.package.def(def).kind == DefKind::Tag
                            && self.decls.structs.contains_key(&def)
                        {
                            return self.case_call(def, only, args, trailing.as_deref(), *span);
                        }
                        if self.decls.structs.contains_key(&def)
                            || self.decls.atoms.contains_key(&def)
                        {
                            self.error(
                                ErrorCode::InferenceFailed,
                                format!(
                                    "`{}` is a type; construct structs with `name {{ ... }}` and \
                                     tags by name",
                                    only.name
                                ),
                                only.span,
                                "type called as a function",
                            );
                            for arg in args {
                                self.expr(&arg.value, None);
                            }
                            return Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: false,
                            };
                        }
                        let callee_val = self.def_value(def, only.span);
                        return self.indirect_call(callee_val, args, trailing.as_deref(), *span);
                    }
                },
                [_head, method] => {
                    if let Some(Res::TraitMethod(method_id)) = self.res.get(method.span) {
                        return self.qualified_call(*method_id, args, trailing.as_deref(), *span);
                    }
                },
                _ => {},
            }
        }
        let callee_val = self.expr(callee, None);
        self.indirect_call(callee_val, args, trailing.as_deref(), *span)
    }

    /// Constructs one minted enum case by call form (B3): positional
    /// arguments fill tuple-payload slots and named arguments fill
    /// named-payload fields; the result is the case's own atom set.
    fn case_call(
        &mut self,
        def: DefId,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let payload = if let Some(case) = self.decls.cases.get(&def) {
            case.payload.clone()
        } else if let Some(shape) = self.decls.structs.get(&def) {
            // a payloaded tag's slots: its own struct shape
            EnumPayload::Struct(shape.fields.clone())
        } else {
            EnumPayload::None
        };
        if trailing.is_some() {
            self.error(
                ErrorCode::ArityMismatch,
                format!("enum case `{}` takes no trailing block", name.name),
                span,
                "trailing block on a case construction",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let case_name = self.package.def(def).name.clone();
        let slots: Vec<(Option<String>, Span, Type)> = match &payload {
            EnumPayload::Tuple(types) => {
                types.iter().map(|t| (None, name.span, t.clone())).collect()
            },
            EnumPayload::Struct(fields) => fields
                .iter()
                .map(|f| (Some(f.name.name.clone()), f.name.span, f.ty.clone()))
                .collect(),
            // A bare case constructs by name like a unit tag; the call
            // site routes here only after the payload check, so this is
            // the belt behind it.
            EnumPayload::None => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` is a bare case; it constructs by name", name.name),
                    name.span,
                    "unit case called as a function",
                );
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            },
        };
        let mut written = vec![false; slots.len()];
        let mut broken = false;
        let mut positional = 0usize;
        for arg in args {
            let slot = if let Some(arg_name) = &arg.name {
                let index = slots
                    .iter()
                    .position(|(field, ..)| field.as_deref().is_some_and(|f| f == arg_name.name));
                let Some(index) = index else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("enum case `{case_name}` has no field `{}`", arg_name.name),
                        arg_name.span,
                        "no such field",
                    );
                    self.expr(&arg.value, None);
                    broken = true;
                    continue;
                };
                index
            } else {
                let index = positional;
                positional += 1;
                let Some(_slot) = slots.get(index) else {
                    self.error(
                        ErrorCode::ArityMismatch,
                        format!("enum case `{case_name}` takes {} argument(s)", slots.len()),
                        arg.span,
                        "extra argument",
                    );
                    self.expr(&arg.value, None);
                    continue;
                };
                index
            };
            if written[slot] {
                self.error(
                    ErrorCode::DuplicateDefinition,
                    format!(
                        "field `{}` of `{case_name}` is written twice",
                        slots[slot].0.clone().unwrap_or_default()
                    ),
                    arg.span,
                    "duplicate field",
                );
            }
            written[slot] = true;
            let Some(field_ty) = self.solve_here(&slots[slot].2) else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("a field of `{case_name}` does not solve"),
                        arg.span,
                        "unsolvable field type",
                    );
                }
                self.expr(&arg.value, None);
                continue;
            };
            let val = self.expr(&arg.value, Some(field_ty));
            self.expect_contains(val, field_ty, arg.value.span(), "case field value");
        }
        if !broken {
            for (index, &is_written) in written.iter().enumerate() {
                if is_written {
                    continue;
                }
                match &slots[index].0 {
                    Some(field) => self.error(
                        ErrorCode::TypeMismatch,
                        format!("enum case `{case_name}` misses field `{field}`"),
                        span,
                        "missing field",
                    ),
                    None => self.error(
                        ErrorCode::ArityMismatch,
                        format!("enum case `{case_name}` takes {} argument(s)", slots.len()),
                        span,
                        "missing argument",
                    ),
                }
            }
        }
        // A minted case constructs into its own atom [B3]. `none` is the
        // predeclared unit atom [B4], so a payloaded case named `none`
        // claims no atom of its own and the construction refuses at this
        // seam instead of typing the binding empty.
        let Some(ty) = self
            .decls
            .atoms
            .get(&def)
            .and_then(|&a| self.decls.universe.atom_ty(a).ok())
        else {
            self.error(
                ErrorCode::NonePayloadCase,
                "`none` is the predeclared unit atom; a case named `none` cannot carry a payload"
                    .to_owned(),
                name.span,
                "payload on the unit atom",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    fn indirect_call(
        &mut self,
        callee: Val,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some((param_tys, ret)) = self.decompose_fn(callee.ty) else {
            // A poisoned callee already reported; "not callable" on top of
            // it would be a cascade. The arguments still walk.
            if callee.poisoned {
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`{}` is not callable; expected one function type",
                    self.decls.universe.render(callee.ty)
                ),
                span,
                "not a function",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let mut all: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        if let Some(tail) = trailing {
            all.push(tail);
        }
        let params: Vec<Option<Ty>> = param_tys.iter().copied().map(Some).collect();
        self.check_args(&all, &params, span);
        Val {
            ty: ret,
            diverges: false,
            poisoned: false,
        }
    }

    /// Calls one package function by definition: instantiates generics by
    /// structural argument matching (the M2 half of one-body generics
    /// (H3)).
    /// Calls one package function by definition: instantiates generics by
    /// structural argument matching (the M2 half of one-body generics
    /// (H3)); named arguments fill their parameter's slot and literal-only
    /// defaults cover omissions (F7).
    fn call_def(
        &mut self,
        def: DefId,
        fed: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some(sig) = self.decls.fns.get(&def).cloned() else {
            // The callee resolved in the value namespace [F9], so it can
            // be a plain const or var binding (a dot-call's free tier); a
            // call against it is a call-shape error with the receiver and
            // every argument walked, never a silently empty type.
            let name = self.package.def(def).name.clone();
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{name}` is a value binding, not a function"),
                span,
                "not callable",
            );
            if let Some(recv) = fed {
                self.expr(recv, None);
            }
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        // The E4 extraction family types by rule against the checked
        // receiver, never through generic inference: an inferred `t?`
        // parameter would bind a plain `t` receiver, and the fallback's
        // width meeting must be diagnosed, not assumed.
        if let Some(kind) = self.extraction_kind(def, &sig) {
            return self.extraction_call(kind, fed, args, trailing, span);
        }
        // The reflection builtins are comptime-context-only (G2): their
        // answers read the compile-time reflection tables, so a call
        // outside a comptime function's body refuses here — the wiring
        // is by name, the builtin admission criterion (H5.5).
        let callee_name = self.package.def(def).name.clone();
        if matches!(callee_name.as_str(), "fields" | "type_name") && !self.comptime.ctx {
            self.error(
                ErrorCode::ComptimeValueOutside,
                format!(
                    "`{callee_name}` answers reflection at compile time only; call it inside a \
                     comptime function [G2]"
                ),
                span,
                "reflection outside comptime",
            );
            if let Some(recv) = fed {
                self.expr(recv, None);
            }
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let slots = self.arg_slots(fed, 0, args, trailing, &sig.params, span);
        let Some(slots) = slots else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        self.check_mut_window_args(&slots, &sig.params);
        // A comptime call's arguments name types (G1): the type-value
        // reading opens for exactly these argument positions.
        let outer = self.comptime;
        self.comptime.args = sig
            .params
            .iter()
            .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)));
        if sig.generics.is_empty() {
            let mut param_tys = Vec::with_capacity(sig.params.len());
            for param in &sig.params {
                if let Some(solved) = param.ty.as_ref().and_then(|t| self.solve_here(t)) {
                    param_tys.push(Some(solved));
                } else {
                    if !self.take_solve_reported() {
                        self.error(
                            ErrorCode::InferenceFailed,
                            "a parameter type does not solve; annotate it [B2]".to_owned(),
                            span,
                            "unannotated parameter",
                        );
                    }
                    param_tys.push(None);
                }
            }
            let ret = sig
                .ret
                .as_ref()
                .and_then(|r| self.solve_here(r))
                .unwrap_or_else(|| self.unit());
            self.check_slots(&slots, &sig.params, &param_tys);
            self.comptime = outer;
            return Val {
                ty: ret,
                diverges: false,
                poisoned: false,
            };
        }
        self.comptime = outer;
        // Generic: mint fresh variables, bind non-lambda arguments by
        // structural matching, check lambda arguments against their
        // fn-typed parameters (binding return variables), then solve the
        // return under the substitution. The substitution (not a scope)
        // carries the variables, so the caller's in-scope variables never
        // capture the callee's.
        let var_atoms: Vec<TypeId> = sig
            .generics
            .iter()
            .map(|p| self.mint_var(&p.name))
            .collect();
        let mut subst: FxHashMap<String, Ty> = FxHashMap::default();
        let assumption_mark = self.assumptions.len();
        for (index, param) in sig.generics.iter().enumerate() {
            if let Ok(ty) = self.decls.universe.atom_ty(var_atoms[index]) {
                if param.bound.as_ref().is_some_and(decls::linear_marker) {
                    self.linear_vars.insert(var_atoms[index], false);
                }
                if let Some(bound) = &param.bound {
                    let mut arms = Vec::new();
                    self.bound_traits(bound, &mut arms);
                    for trait_id in arms {
                        self.assumptions.push((trait_id, ty));
                    }
                }
            }
        }
        // Pass one: non-lambda arguments bind variables structurally,
        // then check against the solved parameter type [B2]: a
        // constructor mismatch binds nothing silently otherwise.
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            let (Some(arg), Some(param_ty)) = (slot, param.ty.as_ref()) else {
                continue;
            };
            if matches!(peel_paren(arg), Expr::Lambda { .. }) && matches!(param_ty, Type::Fn { .. })
            {
                continue;
            }
            let arg_val = self.expr(arg, None);
            self.match_param(param_ty, arg_val.ty, &sig.generics, &mut subst);
            if let Some(expected) = self.solve_with(param_ty, &subst, &sig.generics) {
                self.expect_param_contains(arg_val, param_ty, expected, arg.span());
            }
        }
        // Pass two: lambda arguments check against the fn-typed parameter,
        // and their result type binds the parameter's return variables.
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            let Some(Type::Fn {
                params: fn_params,
                ret: fn_ret,
                ..
            }) = &param.ty
            else {
                continue;
            };
            let Some(arg) = slot else { continue };
            if !matches!(peel_paren(arg), Expr::Lambda { .. }) {
                continue;
            }
            let mut param_tys = Vec::with_capacity(fn_params.len());
            for p in fn_params {
                let written = p.ty.clone().unwrap_or_else(|| name_type(&p.name.name));
                let solved = self.solve_with(&written, &subst, &sig.generics);
                param_tys.push(match solved {
                    Some(ty) => ty,
                    None => self.fresh_var(&p.name.name),
                });
            }
            let expected_ret = fn_ret
                .as_ref()
                .and_then(|r| self.solve_with(r, &subst, &sig.generics))
                .unwrap_or_else(|| self.empty());
            let params_boxed: Box<[Ty]> = param_tys.into();
            let expected_ty = self
                .decls
                .universe
                .fn_atom(&params_boxed, expected_ret)
                .ok()
                .and_then(|a| self.decls.universe.atom_ty(a).ok())
                .unwrap_or_else(|| self.empty());
            // The lambda's expectation cites the callee's written fn-typed
            // parameter when the lambda's own arity refuses.
            self.expected_ty_span = param.ty.as_ref().map(Type::span);
            let val = self.expr(arg, Some(expected_ty));
            self.expected_ty_span = None;
            if let Some(fn_ret) = fn_ret {
                // The lambda's own result type binds the parameter's
                // return variables, not the lambda's function type.
                let result = self.decompose_fn(val.ty).map_or(val.ty, |(_, ret)| ret);
                self.match_param(fn_ret, result, &sig.generics, &mut subst);
            }
        }
        let ret = match sig.ret.as_ref() {
            Some(ret_ty) => self
                .solve_with(ret_ty, &subst, &sig.generics)
                .unwrap_or_else(|| self.empty()),
            None => self.unit(),
        };
        // Record the instantiation when every generic parameter solved
        // [H3]: the lowering pass mints the CALLG evidence words from it.
        // A partially-solved call is an inference failure below and the
        // lowerer rejects the missing record, never a silent `any`.
        let mut solved = Vec::with_capacity(sig.generics.len());
        for param in &sig.generics {
            let Some(ty) = subst.get(&param.name) else {
                solved.clear();
                break;
            };
            solved.push(*ty);
        }
        // A solved instantiation answers for its declared bounds [B6]:
        // every bound must hold for the solved type, through the
        // instance environment or an in-scope assumption (a caller
        // bound propagates that way). An empty solved type rides a
        // cascading argument error and skips the check. Each answered
        // bound records beside the call: the unique instance serving the
        // solved type (`None` when only an in-scope assumption covers
        // it — the runtime-resolved evidence form) plus the solved type
        // itself, the record the lowering pass stages the body's
        // evidence demands from (H3).
        let mut call_bounds: GenericCallBounds = Vec::new();
        for ((index, param), &ty) in sig.generics.iter().enumerate().zip(solved.iter()) {
            let mut arms = Vec::new();
            if let Some(bound) = &param.bound {
                self.bound_traits(bound, &mut arms);
            }
            for trait_id in arms {
                if self.decls.universe.is_empty(ty) {
                    continue;
                }
                let served = if self.trait_satisfied(trait_id, ty, span) {
                    self.instance_resolution(trait_id, ty).solved()
                } else {
                    None
                };
                call_bounds.push((var_atoms[index], trait_id, served, ty));
            }
        }
        if !call_bounds.is_empty() {
            self.generic_call_bounds.insert(span, call_bounds);
        }
        if !solved.is_empty() {
            self.generic_calls.insert(span, solved);
        }
        // Unbound variables leaking into the result are inference
        // failures: no any fallback [B1]. The member set is read by index:
        // reporting mutates the checker, so no borrow of it stays open.
        let ret_len = self.decls.universe.members(ret).len();
        for index in 0..ret_len {
            let atom = self.decls.universe.members(ret)[index];
            if let Some(pos) = var_atoms.iter().position(|v| *v == atom) {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "cannot infer `{}` from this call's arguments [B2]",
                        sig.generics[pos].name
                    ),
                    span,
                    "unbound type variable",
                );
            }
        }
        self.assumptions.truncate(assumption_mark);
        Val {
            ty: ret,
            diverges: false,
            poisoned: false,
        }
    }

    /// Maps call arguments onto signature parameter slots (F7): positional
    /// arguments in order, named arguments by parameter name, the trailing
    /// lambda at the next open position; omissions must carry a literal
    /// default. `None` returns after diagnosing.
    fn arg_slots<'x>(
        &mut self,
        fed: Option<&'x Expr>,
        skip: usize,
        args: &'x [ds_parse::CallArg],
        trailing: Option<&'x Expr>,
        params: &[ParamSig],
        span: Span,
    ) -> Option<Vec<Option<&'x Expr>>> {
        let mut slots: Vec<Option<&'x Expr>> = vec![None; params.len()];
        let mut next_positional = skip;
        let mut ok = true;
        // The UFCS-fed receiver occupies the first positional slot [F3].
        if let Some(fed) = fed {
            if slots.is_empty() {
                self.error(
                    ErrorCode::ArityMismatch,
                    "the dot-call receiver has no parameter to feed".to_owned(),
                    span,
                    "extra argument",
                );
                ok = false;
            } else {
                slots[0] = Some(fed);
                next_positional = 1;
            }
        }
        // Leading parameters the caller already handled (a method's
        // receiver) count as filled.
        for arg in args {
            match &arg.name {
                Some(name) => {
                    let value: &'x Expr = &arg.value;
                    match params.iter().position(|p| p.name == name.name) {
                        Some(index) if slots[index].is_none() => slots[index] = Some(value),
                        Some(_) => {
                            self.error(
                                ErrorCode::ArityMismatch,
                                format!("argument `{}` is given twice", name.name),
                                name.span,
                                "duplicate argument",
                            );
                            ok = false;
                        },
                        None => {
                            self.error(
                                ErrorCode::UnresolvedName,
                                format!("no parameter `{}`", name.name),
                                name.span,
                                "no such parameter",
                            );
                            ok = false;
                        },
                    }
                },
                None => {
                    if next_positional < slots.len() {
                        let value: &'x Expr = &arg.value;
                        slots[next_positional] = Some(value);
                        next_positional += 1;
                    } else {
                        self.error(
                            ErrorCode::ArityMismatch,
                            "more arguments than parameters".to_owned(),
                            arg.span,
                            "extra argument",
                        );
                        ok = false;
                    }
                },
            }
        }
        if let Some(tail) = trailing {
            if next_positional < slots.len() && slots[next_positional].is_none() {
                let value: &'x Expr = tail;
                slots[next_positional] = Some(value);
            } else {
                self.error(
                    ErrorCode::ArityMismatch,
                    "the trailing lambda has no parameter to feed".to_owned(),
                    span,
                    "extra argument",
                );
                ok = false;
            }
        }
        let missing: Vec<&str> = slots
            .iter()
            .zip(params.iter())
            .enumerate()
            .filter(|(index, (slot, param))| {
                slot.is_none() && param.default.is_none() && *index >= skip
            })
            .map(|(_, (_, param))| param.name.as_str())
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ArityMismatch,
                    format!("missing argument(s): {names}"),
                )
                .with_primary(span, "missing arguments"),
            );
            ok = false;
        }
        if ok { Some(slots) } else { None }
    }

    /// Checks filled slots against their parameter types and omitted ones
    /// against their written literal defaults (F7).
    fn check_slots(
        &mut self,
        slots: &[Option<&Expr>],
        params: &[ParamSig],
        param_tys: &[Option<Ty>],
    ) {
        for ((slot, param), param_ty) in slots.iter().zip(params.iter()).zip(param_tys.iter()) {
            match (slot, param_ty) {
                (Some(arg), Some(param_ty)) => {
                    // A lambda argument's expectation can cite the
                    // callee's written parameter type when the lambda's
                    // arity refuses; any other argument clears it.
                    self.expected_ty_span = match peel_paren(arg) {
                        Expr::Lambda { .. } => param.ty.as_ref().map(Type::span),
                        _ => None,
                    };
                    let val = self.expr(arg, Some(*param_ty));
                    self.expected_ty_span = None;
                    self.expect_contains(val, *param_ty, arg.span(), "argument");
                },
                (Some(arg), None) => {
                    self.expr(arg, None);
                },
                (None, Some(param_ty)) => {
                    if let Some(default) = &param.default {
                        let val = self.expr(default, Some(*param_ty));
                        self.expect_contains(val, *param_ty, default.span(), "default value");
                    }
                },
                (None, None) => {},
            }
        }
    }

    /// The iterated iterable's binding identity when it is a bare name
    /// [D3]: only a mutable binding (a local `var` or a top-level `var`)
    /// can open a `mut` window over it later, so a const or computed
    /// iterable records nothing and every body read stays free.
    fn iter_window(&self, iter: &Expr) -> Option<IterWindow> {
        let Expr::Path { path, .. } = iter else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        let bind = match self.res.get(only.span) {
            Some(Res::Local { bind }) => self
                .locals
                .get(*bind)
                .is_some_and(|binding| binding.mutable)
                .then_some(IterBind::Local(*bind)),
            Some(Res::Def(def)) => {
                (self.package.def(*def).kind == DefKind::Var).then_some(IterBind::Def(*def))
            },
            _ => None,
        }?;
        Some(IterWindow {
            bind,
            iter_at: only.span,
        })
    }

    /// The innermost enclosing for-in iterating `bind`, by its loop
    /// head's iterable use.
    fn iterated_at(&self, bind: IterBind) -> Option<Span> {
        self.for_iters
            .iter()
            .rev()
            .find(|window| window.bind == bind)
            .map(|window| window.iter_at)
    }

    /// The spawn-scoped `cancel` refinement [E2]: inside a spawned body
    /// a `cancel` call is the child's deliberate failure — the fiber
    /// dies at the call, so it never answers, and the staging
    /// signature's unit drops out of the body's own unions (a match's
    /// `cancel` arm contributes nothing to the answer the join
    /// collects). Outside spawned bodies the declared signature stands
    /// unchanged. The spelling is recognized like the ambient's other
    /// compiler-required names (`none`): by its name — the ambient's
    /// `cancel` is the only declaration honest code reaches inside a
    /// spawned body.
    fn spawn_cancel_refines(&mut self, def: DefId, mut val: Val) -> Val {
        if self.spawn_depth > 0 && self.package.def(def).name == "cancel" {
            val.diverges = true;
        }
        val
    }

    /// A `mut` window parameter over a copyable binding takes only a
    /// `var` receiver (D2, C3): copyable Ref words alias with no
    /// checker-visible trace, so exclusivity is proven by spelling the
    /// binder `var`. `~Copyable` receivers keep their linearity proof and
    /// fresh rvalues are uniquely owned by construction, so only a const
    /// binding of a copyable type is rejected. A hidden copy behind an
    /// alias would keep mutation invisible at the call site, so the
    /// spelling is corrected, never coerced.
    fn check_mut_window_args(&mut self, slots: &[Option<&Expr>], params: &[ParamSig]) {
        for (slot, param) in slots.iter().zip(params.iter()) {
            if !matches!(param.mode, ds_parse::ParamMode::Mut(_)) {
                continue;
            }
            let Some(arg) = slot else { continue };
            let Expr::Path { path, .. } = peel_paren(arg) else {
                continue;
            };
            let [only] = path.segments.as_slice() else {
                continue
            };
            let Some(res) = self.res.get(only.span).cloned() else {
                continue;
            };
            // A `mut` window over an enclosing for-in's iterated binding
            // refuses [D3]: the loop's element count froze at entry, so
            // the mid-loop mutation never joins this loop's walk — a
            // diagnostic refinement over the frozen-count semantics,
            // which stay the runtime guarantee. Only a mutable binding
            // records a window, so the const-receiver refusal below
            // never doubles with this one.
            let windowed = match &res {
                Res::Local { bind } => Some(IterBind::Local(*bind)),
                Res::Def(def) => Some(IterBind::Def(*def)),
                _ => None,
            };
            if let Some(bind) = windowed
                && let Some(iter_at) = self.iterated_at(bind)
            {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::ForIterMutWindow,
                        format!(
                            "`{}` is this loop's iterated binding; its element count froze at \
                             loop entry, so a `mut` window over it inside the body never joins \
                             this loop's walk [D3]",
                            only.name
                        ),
                    )
                    .with_primary(only.span, "mut window over the iterated binding")
                    .with_secondary(iter_at, "iterated here"),
                );
                continue;
            }
            // A `mut` window over a binding outliving the enclosing
            // nursery block refuses inside a spawned body [E2, the
            // 2026-11-21 scheduler ruling]: once a child can park on a
            // world op the drive interleaves siblings, so a mutation of
            // a binding that survives the join has no sequenced
            // observer — the refusal permanent and drive-independent.
            // Bindings the block itself introduced die with it after
            // the join, so a window over one of those stays legal, and
            // the parent's own statements in the block sequence with
            // its spawns by construction, so only the spawned body is
            // refused. A top-level `var` always outlives the block.
            if self.spawn_depth > 0
                && let Some((outlives, nursery_at)) = self.nursery_scopes.last().map(|scope| {
                    let outlives = match &res {
                        Res::Local { bind } => scope.outlived.contains(bind),
                        Res::Def(def) => self.package.def(*def).kind == DefKind::Var,
                        _ => false,
                    };
                    (outlives, scope.at)
                })
                && outlives
            {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::SpawnOutlivedMutWindow,
                        format!(
                            "`{}` outlives this nursery block; a spawned body may never hold a \
                             `mut` window over it [E2]",
                            only.name
                        ),
                    )
                    .with_primary(only.span, "mut window over an outlived binding")
                    .with_secondary(nursery_at, "the nursery block joins here"),
                );
                continue;
            }
            let (binder, mutable, ty) = match res {
                Res::Local { bind } => {
                    let Some(binding) = self.locals.get(bind) else {
                        continue
                    };
                    (bind, binding.mutable, binding.ty)
                },
                Res::Def(def) => {
                    let def_info = self.package.def(def);
                    if def_info.kind != DefKind::Const {
                        continue;
                    }
                    let Some(ty) = self.def_types.get(&def).copied() else {
                        continue;
                    };
                    (def_info.span, false, ty)
                },
                _ => continue,
            };
            // An empty type is an inference failure the walk already
            // reported; the ruling is about copyable shared-heap words.
            if mutable
                || self.decls.universe.is_empty(ty)
                || !crate::linearity::ty_copyable(self.decls, ty, &self.linear_vars)
            {
                continue;
            }
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::MutWindowConstReceiver,
                    format!(
                        "`{}` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]",
                        only.name
                    ),
                )
                .with_secondary(binder, "bound here")
                .with_primary(only.span, "const receiver"),
            );
        }
    }

    fn qualified_call(
        &mut self,
        method: MethodId,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some(sig) = self.member_sig(method) else {
            self.error(
                ErrorCode::UnresolvedName,
                "the qualified member has no registered signature".to_owned(),
                span,
                "no signature",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let trait_id = match &self.package.method(method).owner {
            MethodOwner::Trait(def) => self.decls.traits.get(def).copied(),
            MethodOwner::Inherent(_) => None,
        };
        let Some(recv) = args.first() else {
            self.error(
                ErrorCode::ArityMismatch,
                "the qualified member needs its receiver as the first argument".to_owned(),
                span,
                "missing receiver",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let recv_val = self.expr(&recv.value, None);
        if let Some(trait_id) = trait_id {
            if !self.trait_satisfied(trait_id, recv_val.ty, span) {
                for arg in args.iter().skip(1) {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            }
            self.record_solved_dispatch(method, recv_val.ty, None, span);
        }
        self.member_call(
            method,
            &sig,
            recv_val.ty,
            Some(&recv.value),
            &args[1..],
            trailing,
            span,
            None,
        )
    }

    fn dot_call(
        &mut self,
        recv: &Expr,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        // A bare type name as the receiver takes the type-keyed tier: its
        // own impl members claim the spelling, and no value form feeds the
        // receiver. A tag or case name with no recorded member keeps its
        // constructor-value reading, so the ordinary tiers below type it;
        // every other type has no value reading even when the resolver
        // already reported the missing member, so the static path owns the
        // receiver and the poison silences the cascade.
        if let Some((def, recv_span, written)) = self.type_keyed_receiver(recv) {
            let recorded_static = matches!(
                self.res.get(name.span),
                Some(Res::Dot(DotRes::Static { .. }))
            );
            let value_reading = matches!(self.package.def(def).kind, DefKind::Tag | DefKind::Case);
            if recorded_static || !value_reading {
                return self.static_member_call(
                    def,
                    recv_span,
                    written.as_ref(),
                    name,
                    args,
                    trailing,
                    span,
                );
            }
        }
        let Some(res) = self.res.get(name.span).cloned() else {
            // The resolution pass already reported the root cause (no
            // name behind the dot); the empty result must not cascade
            // into downstream empty-intersection errors. The flag sets
            // last — a nested solve_here would wipe it (solve_here
            // clears the flag it reads).
            self.expr(recv, None);
            for arg in args {
                self.expr(&arg.value, None);
            }
            self.solve_reported = true;
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        match res {
            Res::Handler => self.err_ok(recv, &name.name, args, span),
            Res::Dot(DotRes::Free { func, fed_param }) => {
                // The receiver feeds the recorded parameter [F3]; only
                // parameter 0 exists today (capability parameters have no
                // syntax yet). The free tier keeps F3's preference, but a
                // free fn whose first parameter cannot contain the
                // receiver's type never claims the call (the 2026-11-22
                // amendment): the registered method candidates serve it
                // instead, so a member and a same-named free fn coexist
                // over disjoint receivers.
                if fed_param == 0 {
                    let candidates: Vec<MethodId> =
                        self.package.method_candidates(&name.name).collect();
                    if !candidates.is_empty() {
                        let recv_val = self.expr(recv, None);
                        if !self.free_fn_takes_receiver(func, recv_val.ty) {
                            return self.method_select_typed(
                                recv,
                                recv_val,
                                name,
                                &candidates,
                                args,
                                trailing,
                                span,
                            );
                        }
                        return self.call_def(func, Some(recv), args, trailing, span);
                    }
                }
                let fed = (fed_param == 0).then_some(recv);
                self.call_def(func, fed, args, trailing, span)
            },
            Res::Dot(DotRes::Module { func }) => {
                // A module-qualified call (G4): the linked function takes
                // the written arguments alone — no receiver word stages —
                // and types exactly like the bare call of the bound name.
                self.call_def(func, None, args, trailing, span)
            },
            Res::Dot(DotRes::Methods { candidates }) => {
                self.method_select(recv, name, &candidates, args, trailing, span)
            },
            _ => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` does not name a callable", name.name),
                    name.span,
                    "not callable",
                );
                self.expr(recv, None);
                for arg in args {
                    self.expr(&arg.value, None);
                }
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// The receiver of a type-keyed dot-call: a bare single-segment path
    /// (parentheses break the spelling, mirroring the resolver's side)
    /// whose recorded use names a type-namespace def, or an instantiated
    /// application — a one-argument bracket, a two-argument bracket, or
    /// a written application type carried by the fn-type node — whose
    /// head use names the type. The resolver made the namespace decision
    /// (locals and value defs shadow the type), so this only reads it
    /// back. The written application rides along when the receiver
    /// spells one, so the static tier solves the applied type the call
    /// dispatches.
    fn type_keyed_receiver(&self, recv: &Expr) -> Option<(DefId, Span, Option<Type>)> {
        if let Expr::Path { path, .. } = recv {
            let [only] = path.segments.as_slice() else {
                return None;
            };
            return match self.res.get(only.span) {
                Some(Res::Def(def)) if self.package.def(*def).kind.in_type_namespace() => {
                    Some((*def, only.span, None))
                },
                _ => None,
            };
        }
        self.applied_receiver(recv)
    }

    /// The instantiated form of a type-keyed receiver: the head's
    /// recorded def plus the written application it dispatches. The
    /// bracket forms carry expression-position type spellings the
    /// checker rereads as types (the constructor-bracket convention);
    /// the fn-type node carries the written application itself — the
    /// synthesized spelling for shapes the brackets cannot express.
    fn applied_receiver(&self, recv: &Expr) -> Option<(DefId, Span, Option<Type>)> {
        let (head_span, def, written): (Span, DefId, Option<Type>) = match recv {
            Expr::Index { recv, index, .. } => {
                let Expr::Path { span, path, .. } = recv.as_ref() else {
                    return None;
                };
                let [only] = path.segments.as_slice() else {
                    return None;
                };
                let def = self.recorded_type_def(only.span)?;
                let arg = Self::ctor_type_arg(index)?;
                (
                    only.span,
                    def,
                    Some(Type::Path {
                        span: *span,
                        path: path.clone(),
                        args: vec![arg],
                    }),
                )
            },
            Expr::MapApply {
                span,
                head,
                key,
                value,
                ..
            } => {
                let def = self.recorded_type_def(head.span)?;
                let key = Self::ctor_type_arg(key)?;
                let value = Self::ctor_type_arg(value)?;
                (
                    head.span,
                    def,
                    Some(Type::Path {
                        span: *span,
                        path: Path {
                            span: head.span,
                            segments: vec![head.clone()],
                        },
                        args: vec![key, value],
                    }),
                )
            },
            Expr::FnType { ty, .. } => {
                let Type::Path { path, args, .. } = ty.as_ref() else {
                    return None;
                };
                let [only] = path.segments.as_slice() else {
                    return None;
                };
                if args.is_empty() {
                    return None;
                }
                (
                    only.span,
                    self.recorded_type_def(only.span)?,
                    Some((**ty).clone()),
                )
            },
            _ => return None,
        };
        Some((def, head_span, written))
    }

    /// The type-namespace def a resolver-recorded use names, `None` when
    /// the span carries no type-namespace def.
    fn recorded_type_def(&self, span: Span) -> Option<DefId> {
        match self.res.get(span) {
            Some(Res::Def(def)) if self.package.def(*def).kind.in_type_namespace() => Some(*def),
            _ => None,
        }
    }

    /// Refusal epilogue shared by `static_member_call`'s dead ends: the
    /// argument words are still walked so their own checks run, then the
    /// empty-typed value returns. `poisoned` is that value's cascade flag;
    /// `reported` raises `solve_reported` before the walk, as the drained
    /// words may observe it.
    fn refuse_static_member_call(
        &mut self,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        poisoned: bool,
        reported: bool,
    ) -> Val {
        if reported {
            self.solve_reported = true;
        }
        for arg in args {
            self.expr(&arg.value, None);
        }
        if let Some(tail) = trailing {
            self.expr(tail, None);
        }
        Val {
            ty: self.empty(),
            diverges: false,
            poisoned,
        }
    }

    /// The type-keyed static member call (the 2026-11-15 ruling): the
    /// receiver names a type, the member lives in that type's own impl,
    /// and the call carries no instance — resolution is a check-time
    /// fact keyed by the type alone. The resolver registered the
    /// type's members with that name as the candidates, so at most one
    /// exists (registration dedups by name and owner); the member must
    /// be receiverless, its owner's `Self` binds the call, and a
    /// generic impl's member refuses behind the bare type name alone —
    /// an instantiated receiver carries the type arguments, so it
    /// proceeds and stages the body's evidence exactly like a solved
    /// inherent dot-call.
    fn static_member_call(
        &mut self,
        def: DefId,
        recv_span: Span,
        written: Option<&Type>,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let type_name = self.package.def(def).name.clone();
        // A missing record means the resolver reported the missing member at
        // the method position; the empty result must not cascade.
        let Some(Res::Dot(DotRes::Static { candidates })) = self.res.get(name.span) else {
            return self.refuse_static_member_call(args, trailing, true, true);
        };
        let Some(&method) = candidates.first() else {
            return self.refuse_static_member_call(args, trailing, true, true);
        };
        let Some(sig) = self.member_sig(method) else {
            self.error(
                ErrorCode::UnresolvedName,
                format!("member `{}` has no registered signature", name.name),
                span,
                "no signature",
            );
            return self.refuse_static_member_call(args, trailing, false, false);
        };
        // A receiver-taking member is not reachable through the type name
        // — the type-keyed surface serves receiverless members only.
        if sig.params.first().is_some_and(|p| p.name == "self") {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "member `{}` of `{}` takes a receiver; call it on a value of `{}`",
                    name.name, type_name, type_name
                ),
                span,
                "receiverless call of a receiver member",
            );
            return self.refuse_static_member_call(args, trailing, false, false);
        }
        // A generic impl's member cannot be spelled through the bare type
        // name: the call would need the impl's type arguments, and the
        // bare receiver position carries none. An instantiated receiver
        // carries them, so the refusal names the applied spelling.
        if written.is_none()
            && let Some(&impl_site) = self.decls.inherent_impls.get(&method)
            && self.decls.inherent_impl_bounds.contains_key(&impl_site)
        {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "member `{}` of `{}` lives in a generic impl; the type-keyed call needs the \
                     applied receiver `{}[...]`, which the bare type name cannot spell [H3]",
                    name.name, type_name, type_name
                ),
                span,
                "generic impl member",
            );
            return self.refuse_static_member_call(args, trailing, false, false);
        }
        // The owner's own type binds `Self` at the call: solved through
        // the normal name solver over the receiver's own spelling, so an
        // enum is the union of its cases and an alias its body — and an
        // instantiated receiver solves the applied type, whose
        // substitution the member's signature reads below.
        let owner_written = written.cloned().unwrap_or_else(|| Type::Path {
            span: recv_span,
            path: Path {
                span: recv_span,
                segments: vec![Ident {
                    span: recv_span,
                    name: type_name.clone(),
                }],
            },
            args: Vec::new(),
        });
        let Some(owner_ty) = self.solve_here(&owner_written) else {
            if let Some(applied) = &written {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the type-keyed receiver `{}` does not solve against the package \
                             declarations",
                            decls::render_type(applied)
                        ),
                        span,
                        "unsolvable application",
                    );
                }
                self.solve_reported = true;
            } else {
                self.error(
                    ErrorCode::UnresolvedName,
                    format!("`{type_name}` names no value-bearing type"),
                    span,
                    "no owner type",
                );
            }
            return self.refuse_static_member_call(args, trailing, false, false);
        };
        // The static dispatch record: the method id at the call's own
        // span, the same table a solved inherent dot-call records into —
        // the lowerer resolves the member's body through it and stages no
        // receiver word. A generic impl's body dispatches on runtime
        // evidence (H3): the instantiated receiver records the serving
        // instance per declared bound under the application's
        // substitution, the value-receiver record's own mirror.
        self.inherent_calls.insert(span, method);
        self.method_selections.insert(name.span, method);
        if let Some(&impl_site) = self.decls.inherent_impls.get(&method)
            && let Some(record) = self.decls.inherent_impl_bounds.get(&impl_site).cloned()
        {
            let bounds = self.inherent_evidence(record.head, &record.conditions, owner_ty, span);
            if !bounds.is_empty() {
                self.inherent_call_bounds.insert(span, bounds);
            }
        }
        self.member_call(method, &sig, owner_ty, None, args, trailing, span, None)
    }

    /// Method-candidate selection (F3, B6): candidates narrow by receiver
    /// type — trait members through the instance environment (or an
    /// in-scope assumption), inherent members by owner type; survivors
    /// beyond one are true ambiguity, never a priority order.
    fn method_select(
        &mut self,
        recv: &Expr,
        name: &Ident,
        candidates: &[MethodId],
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let recv_val = self.expr(recv, None);
        self.method_select_typed(recv, recv_val, name, candidates, args, trailing, span)
    }

    /// The candidate selection over a receiver already typed once (the
    /// free-tier fallback's entry): identical to [`Self::method_select`]
    /// apart from the pre-typed receiver, whose second typing would
    /// double every diagnostic an erroneous receiver already reported.
    fn method_select_typed(
        &mut self,
        recv: &Expr,
        recv_val: Val,
        name: &Ident,
        candidates: &[MethodId],
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        // Each survivor carries the resolution its trait branch solved:
        // the selected survivor hands its resolution to the dispatch
        // record and the member call, so the pair solves once.
        let mut survivors: Vec<(MethodId, Arc<FnSig>, Option<Resolution>)> = Vec::new();
        let mut miss = None;
        for &candidate in candidates {
            let Some(sig) = self.member_sig(candidate) else {
                continue
            };
            match &self.package.method(candidate).owner {
                MethodOwner::Trait(def) => {
                    let Some(trait_id) = self.decls.traits.get(def).copied() else {
                        continue;
                    };
                    if self.assumption_covers(trait_id, recv_val.ty) {
                        survivors.push((candidate, sig, None));
                        continue;
                    }
                    match self.instance_resolution(trait_id, recv_val.ty) {
                        resolution @ (Resolution::Unique(_)
                        | Resolution::Covered(_)
                        | Resolution::Ambiguous(_)) => {
                            survivors.push((candidate, sig, Some(resolution)));
                        },
                        // A head that matched while its bound failed is
                        // the nearest miss: the diagnostic below names the
                        // unsatisfied bound instead of the receiver alone.
                        Resolution::BoundUnsat { bound, target, .. } => {
                            if miss.is_none() {
                                miss = Some((trait_id, bound, target));
                            }
                        },
                        Resolution::None => {},
                    }
                },
                MethodOwner::Inherent(target) => {
                    // A receiverless member answers through the type-keyed
                    // surface alone; a value receiver never selects it.
                    if sig.params.first().is_none_or(|p| p.name != "self") {
                        continue;
                    }
                    if self.inherent_accepts(target, recv_val.ty) {
                        survivors.push((candidate, sig, None));
                    }
                },
            }
        }
        if survivors.is_empty() {
            // A poisoned receiver already reported; "no method accepts
            // `<empty>`" on top of it would be a cascade. The arguments
            // still walk so their own errors surface.
            if recv_val.poisoned {
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
            if let Some((trait_id, bound, target)) = miss {
                self.report_bound_miss(trait_id, bound, &target, span);
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            }
            let trait_names: Vec<String> = candidates
                .iter()
                .filter_map(|c| match &self.package.method(*c).owner {
                    MethodOwner::Trait(def) => Some(self.package.def(*def).name.clone()),
                    MethodOwner::Inherent(_) => None,
                })
                .collect();
            self.error(
                ErrorCode::UnsatisfiedConstraint,
                if trait_names.is_empty() {
                    format!(
                        "no method `{}` accepts receiver `{}`",
                        name.name,
                        self.decls.universe.render(recv_val.ty)
                    )
                } else {
                    format!(
                        "no instance of {} for `{}`",
                        trait_names
                            .iter()
                            .map(|t| format!("`{t}`"))
                            .collect::<Vec<_>>()
                            .join(" or "),
                        self.decls.universe.render(recv_val.ty)
                    )
                },
                span,
                if trait_names.is_empty() { "no accepting owner" } else { "unimplemented trait" },
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        if survivors.len() > 1 {
            let names: Vec<String> = candidates
                .iter()
                .filter(|c| self.member_sig(**c).is_some())
                .map(|c| match &self.package.method(*c).owner {
                    MethodOwner::Trait(def) => format!("`{}`", self.package.def(*def).name),
                    MethodOwner::Inherent(target) => format!("inherent `{target}`"),
                })
                .collect();
            self.error(
                ErrorCode::AmbiguousCall,
                format!(
                    "dot-call `{}` is truly ambiguous between {}; qualify with `Trait::{}` [F3]",
                    name.name,
                    names.join(" and "),
                    name.name
                ),
                span,
                "ambiguous method",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let (method, sig, resolution) = survivors.remove(0);
        self.method_selections.insert(name.span, method);
        self.record_solved_dispatch(method, recv_val.ty, resolution.as_ref(), span);
        self.member_call(
            method,
            &sig,
            recv_val.ty,
            Some(recv),
            args,
            trailing,
            span,
            resolution.as_ref(),
        )
    }

    /// Whether a dot-call's free tier can take the receiver (the F3
    /// amendment, 2026-11-22): the free fn keeps the preference when its
    /// first parameter's solved type contains the receiver's type. A
    /// missing signature or parameter, an unsolvable parameter type, or
    /// an already-failed receiver keeps the free reading — the fallback
    /// never widens an existing refusal.
    fn free_fn_takes_receiver(&mut self, func: DefId, recv: Ty) -> bool {
        let Some(sig) = self.decls.fns.get(&func).cloned() else {
            return true;
        };
        let Some(param) = sig.params.first() else {
            return true;
        };
        let Some(written) = param.ty.as_ref() else {
            return true;
        };
        let Some(expected) = self.solve_here(written) else {
            return true;
        };
        if self.decls.universe.is_empty(recv) {
            return true;
        }
        self.decls.universe.contains(expected, recv)
    }

    /// Whether an inherent impl's owner accepts the receiver (F3): every
    /// member of the receiver's type must be the owner's atom or an
    /// application of it (a generic impl's constructor). The receiver is
    /// contained in what the owner covers — the instance check's inherent
    /// mirror, never the converse — so a union receiver narrows first and
    /// a receiver the owner does not cover reports at the dot. An owner
    /// naming no declared atom (an alias, a union target) accepts nothing.
    fn inherent_accepts(&self, target: &str, recv: Ty) -> bool {
        let Some(def) = self
            .package
            .lookup_type(target)
            .or_else(|| self.decls.foreign_type(target))
        else {
            return false;
        };
        let Some(&owner) = self.decls.atoms.get(&def) else {
            return false;
        };
        let universe = &self.decls.universe;
        universe.members(recv).iter().all(|&member| {
            member == owner
                || matches!(
                    universe.atom_decl(member),
                    Ok(decl) if matches!(decl.kind, AtomKind::Apply(ctor, _) if ctor == owner)
                )
        })
    }

    /// Records one selected method's solved dispatch at the call's own
    /// span. A trait survivor records the instance the environment
    /// resolved for the receiver's type — a containing head, a generic
    /// head that unifies under provable bounds (B6), or a covering
    /// group of sibling branches whose representative names the shared
    /// impl item — where the solved instance is the declared one and
    /// the recorded receiver type carries the substitution. An inherent survivor records its
    /// method id (the lowerer resolves the impl item through it, and no
    /// instance exists). A site satisfied only through an in-scope
    /// assumption with no resolving instance records nothing — its
    /// dispatch is the runtime-resolved evidence form.
    fn record_solved_dispatch(
        &mut self,
        method: MethodId,
        recv: Ty,
        resolution: Option<&Resolution>,
        span: Span,
    ) {
        if matches!(&self.package.method(method).owner, MethodOwner::Inherent(_)) {
            self.inherent_calls.insert(span, method);
            // A generic inherent impl's body dispatches on runtime
            // evidence (H3): the site records the serving instance per
            // declared bound under the receiver's substitution — the
            // trait arm's per-bound record minus the instance word an
            // inherent impl never has.
            if let Some(&impl_site) = self.decls.inherent_impls.get(&method)
                && let Some(record) = self.decls.inherent_impl_bounds.get(&impl_site).cloned()
            {
                let bounds = self.inherent_evidence(record.head, &record.conditions, recv, span);
                if !bounds.is_empty() {
                    self.inherent_call_bounds.insert(span, bounds);
                }
            }
            return;
        }
        let MethodOwner::Trait(def) = &self.package.method(method).owner else {
            return;
        };
        let Some(&trait_id) = self.decls.traits.get(def) else {
            return;
        };
        // The dot-call's selected resolution arrives from selection; the
        // other call spellings solve the pair here.
        let resolved;
        let resolution = if let Some(resolution) = resolution {
            resolution
        } else {
            resolved = self.instance_resolution(trait_id, recv);
            &resolved
        };
        match resolution {
            Resolution::Unique(instance) | Resolution::Covered(instance) => {
                let instance = *instance;
                // a generic impl's body dispatches on runtime evidence:
                // record the per-bound instances the substitution serves,
                // so the lowering side stages them without re-deriving types
                let bounds = crate::instance_solve::evidence_bounds(
                    &mut self.decls.universe,
                    &self.decls.instances,
                    instance,
                    recv,
                );
                self.trait_calls.insert(span, TraitCall {
                    method,
                    trait_id,
                    instance,
                    impl_site: self.decls.instance_impls.get(&instance).copied(),
                    recv,
                    bounds,
                });
            },
            Resolution::Ambiguous(candidates) => {
                self.instance_ambiguity(trait_id, recv, candidates, span);
            },
            Resolution::None | Resolution::BoundUnsat { .. } => {},
        }
    }

    /// The `Self` a member call binds: the receiver's type, widened to the
    /// solving instance's written head when that head is closed and
    /// contains the receiver — the dispatched member body was checked
    /// over the written head, so its `Self`-typed parameters accept the
    /// head's whole member set and a case-narrowed receiver no longer
    /// pins them to its own case. A generic written head keeps the
    /// receiver, which carries the call's substitution. The dot-call's
    /// selected resolution arrives from selection; the other call
    /// spellings solve the pair here.
    fn self_binding(&self, method: MethodId, recv: Ty, resolution: Option<&Resolution>) -> Ty {
        let MethodOwner::Trait(def) = &self.package.method(method).owner else {
            return recv;
        };
        let Some(&trait_id) = self.decls.traits.get(def) else {
            return recv;
        };
        let resolved;
        let resolution = if let Some(resolution) = resolution {
            resolution
        } else {
            resolved = self.instance_resolution(trait_id, recv);
            &resolved
        };
        let (&Resolution::Unique(id) | &Resolution::Covered(id)) = resolution else {
            return recv;
        };
        let Ok(written) = self.decls.instances.written(id) else {
            return recv;
        };
        if self.decls.universe.contains(written, recv)
            && !crate::instance_solve::head_carries_vars(&self.decls.universe, written)
        {
            written
        } else {
            recv
        }
    }

    /// One selected method's call check: the member signature with `Self`
    /// bound through [`Self::self_binding`] and the trait's own generics
    /// bound through the dispatch's solved instance (the impl's trait
    /// reference rewritten against the receiver), so arguments check
    /// against the site's substitution — a generic parameter left free
    /// bound itself to any argument, accepting types the substitution
    /// does not cover [record 6.1]. A value receiver passes through the
    /// same mut-window exclusivity gate as a free call's fed argument
    /// (D2, C3). The dot-call's selected resolution arrives from
    /// selection (the pair solved once there); the other call spellings
    /// pass `None` and solve here.
    #[allow(clippy::too_many_arguments)]
    fn member_call(
        &mut self,
        method: MethodId,
        sig: &FnSig,
        recv_ty: Ty,
        recv_expr: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
        resolution: Option<&Resolution>,
    ) -> Val {
        let self_ty = self.self_binding(method, recv_ty, resolution);
        let mut vars = FxHashMap::from_iter([("Self".to_owned(), self_ty)]);
        if let MethodOwner::Trait(def) = &self.package.method(method).owner
            && let Some(&trait_id) = self.decls.traits.get(def)
            && let Some(instance) = match resolution {
                Some(resolution) => Resolution::solved(resolution),
                None => self.instance_resolution(trait_id, recv_ty).solved(),
            }
            && let Some(&site) = self.decls.instance_impls.get(&instance)
            && let Some(decl_args) = self.decls.trait_impl_args.get(&site)
            && let Some(names) = self.decls.trait_generics.get(def)
            && let Some(substituted) = crate::instance_solve::substituted_trait_args(
                &mut self.decls.universe,
                &self.decls.instances,
                instance,
                decl_args,
                recv_ty,
            )
        {
            for (name, ty) in names.iter().zip(substituted) {
                if let Some(ty) = ty {
                    vars.insert(name.clone(), ty);
                }
            }
        }
        // A generic inherent impl's head pattern binds its variables at
        // the call: the receiver unifies with the head, and each
        // variable's solved type substitutes into the member's written
        // signature (the trait branch's instance substitution mirrored
        // on the inherent side — a variable the head does not bind, or a
        // receiver it cannot unify with, leaves the written name to the
        // ordinary solver).
        if let Some(&impl_site) = self.decls.inherent_impls.get(&method)
            && let Some(record) = self.decls.inherent_impl_bounds.get(&impl_site).cloned()
            && let Some(pairs) = crate::instance_solve::pattern_substitution(
                &self.decls.universe,
                record.head,
                self_ty,
            )
        {
            for (var, members) in pairs {
                let Some(name) = crate::instance_solve::var_param_name(&self.decls.universe, var)
                else {
                    continue;
                };
                let ty = self.decls.universe.set_ty(&members);
                vars.insert(name, ty);
            }
        }
        self.type_vars.push(vars);
        let mut param_tys = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            param_tys.push(param.ty.as_ref().and_then(|t| self.solve_here(t)));
        }
        // The receiver already filled the `self` parameter [record 6.1];
        // the remaining arguments feed the rest. A receiverless member
        // (the type-keyed surface) has no filled slot, so the written
        // arguments feed the whole signature.
        let skip = usize::from(sig.params.first().is_some_and(|p| p.name == "self"));
        // The receiver expression fills the `self` slot, so a `mut self`
        // member checks its exclusivity exactly like a free call's fed
        // `mut` argument [D2, C3] — a dot-call on a `const` binding of a
        // copyable type rejects identically to the free-function
        // spelling. The static tier passes no receiver, so it skips.
        if skip == 1
            && let Some(recv) = recv_expr
        {
            self.check_mut_window_args(&[Some(recv)], &sig.params[..1]);
        }
        if let Some(slots) = self.arg_slots(None, skip, args, trailing, &sig.params, span) {
            self.check_slots(&slots[skip..], &sig.params[skip..], &param_tys[skip..]);
        }
        let ret = sig
            .ret
            .as_ref()
            .and_then(|r| self.solve_here(r))
            .unwrap_or_else(|| self.unit());
        self.type_vars.pop();
        Val {
            ty: ret,
            diverges: false,
            poisoned: false,
        }
    }

    /// `.err(h)`/`.ok(h)`: difference-typed handlers (E4). The handled set
    /// is the handler's annotated parameter, or — unannotated — everything
    /// except what the handler's own result keeps of the operand.
    fn err_ok(&mut self, recv: &Expr, name: &str, args: &[ds_parse::CallArg], span: Span) -> Val {
        let recv_val = self.expr(recv, None);
        let total = recv_val.ty;
        if args.len() != 1 {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ArityMismatch,
                    format!("`.{name}` takes the handler lambda as its one argument"),
                )
                .with_primary(span, "wrong arity"),
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let Expr::Lambda {
            params: handler_params,
            body,
            ..
        } = peel_paren(&args[0].value)
        else {
            self.error(
                ErrorCode::TypeMismatch,
                format!("`.{name}` handles through a lambda `(e) => ...`"),
                args[0].span,
                "not a handler lambda",
            );
            self.expr(&args[0].value, None);
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let annotated = handler_params
            .first()
            .and_then(|p| p.ty.as_ref())
            .and_then(|t| self.solve_here(t));
        let saved = self.snapshot();
        let param_ty = annotated.unwrap_or(total);
        if let Some(first) = handler_params.first() {
            self.bind_local(first.name.span, Binding {
                ty: param_ty,
                mutable: false,
            });
        }
        let body_val = self.expr(body, None);
        let body_ty = body_val.ty;
        self.restore(saved);
        let handled_set = if let Some(written) = annotated {
            written
        } else {
            let kept = self.decls.universe.inter(total, body_ty);
            self.decls.universe.diff(total, kept)
        };
        let overlap = self.decls.universe.inter(total, handled_set);
        if self.decls.universe.is_empty(overlap)
            && !self.decls.universe.is_empty(total)
            && !recv_val.poisoned
        {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "the handler's `{}` handles `{}`, which `{}` does not carry",
                    name,
                    self.decls.universe.render(handled_set),
                    self.decls.universe.render(total)
                ),
                span,
                "disjoint handler",
            );
        }
        let remainder = self.decls.universe.diff(total, handled_set);
        // the result carries the handler's own answer beside the kept
        // side (the MapErr contract `fn(e) -> e2` answers `t | e2`): a
        // handler mapping the error onto a member the receiver never
        // carried must widen the answer in, or the recorded type omits
        // the only member the err arm ever writes. An answer that never
        // solves at the site (the `die` idiom: a generic extern whose
        // return variable no expectation feeds) names no knowable
        // member — a minted variable's atom name is the only `$` a
        // solved set can carry, surface names cannot spell it — and
        // contributes nothing.
        let answered = self.decls.universe.members(body_ty).iter().any(|m| {
            self.decls
                .universe
                .atom_decl(*m)
                .is_ok_and(|d| d.name.contains('$'))
        });
        let result =
            if answered { remainder } else { self.decls.universe.union(remainder, body_ty) };
        if self.decls.universe.is_empty(result)
            && !self.decls.universe.is_empty(total)
            && !recv_val.poisoned
        {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`.{name}` leaves nothing: the handler consumed all of `{}`",
                    self.decls.universe.render(total)
                ),
                span,
                "empty remainder",
            );
        }
        Val {
            ty: result,
            diverges: false,
            poisoned: false,
        }
    }

    /// The family member a call addresses, when the callee's shape could
    /// be the ambient's: a generic function of the family name and
    /// two-parameter arity. Without the ambient, a same-named generic
    /// function rides the family rules (the natives' name-routing
    /// hazard, unchanged).
    fn extraction_kind(&self, def: DefId, sig: &FnSig) -> Option<Extraction> {
        let kind = Extraction::of_name(&self.package.def(def).name)?;
        (!sig.generics.is_empty() && sig.params.len() == 2).then_some(kind)
    }

    /// One extraction-family call (E4): the receiver and one argument,
    /// typed locally against the checked receiver's members. The eager
    /// `unwrap_or` binds its fallback to the extracted type, the lazy
    /// `unwrap_or_else` checks a zero-parameter lambda against it, `ok_or`
    /// unions the new member in, and `map_err` mirrors the `.err`/`.ok`
    /// handler rules — the annotated parameter handles, or everything
    /// but what the lambda's own result keeps.
    fn extraction_call(
        &mut self,
        kind: Extraction,
        fed: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        if trailing.is_some() {
            self.error(
                ErrorCode::ArityMismatch,
                format!(
                    "`{}` takes no trailing block argument; write the lambda inline",
                    kind.name()
                ),
                span,
                "extra argument",
            );
            return self.empty_val();
        }
        let (recv, arg): (&Expr, &Expr) = match (fed, args) {
            (Some(recv), [only]) => (recv, &only.value),
            (None, [recv, arg]) => (&recv.value, &arg.value),
            _ => {
                self.error(
                    ErrorCode::ArityMismatch,
                    format!("`{}` takes the receiver and one argument", kind.name()),
                    span,
                    "wrong arity",
                );
                return self.empty_val();
            },
        };
        if args.iter().any(|arg| arg.name.is_some()) {
            self.error(
                ErrorCode::ArityMismatch,
                format!("`{}` takes positional arguments only", kind.name()),
                span,
                "named argument",
            );
            return self.empty_val();
        }
        match kind {
            Extraction::UnwrapOr | Extraction::UnwrapOrElse | Extraction::OkOr => {
                let recv_val = self.expr(recv, None);
                let total = recv_val.ty;
                let name = kind.name();
                let none = self.none_ty();
                if !self.decls.universe.contains(total, none) {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`{name}` extracts a `t | none` receiver; `{}` carries no `none` \
                             member [E4]",
                            self.decls.universe.render(total)
                        ),
                        span,
                        "receiver is not optional",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                }
                let extracted = self.decls.universe.diff(total, none);
                if self.decls.universe.is_empty(extracted) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!("`{name}` over `none` leaves nothing to extract [E4]"),
                        span,
                        "empty extraction",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                }
                match kind {
                    Extraction::UnwrapOr => {
                        let arg_val = self.expr(arg, None);
                        if !self.decls.universe.contains(extracted, arg_val.ty) {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "the fallback `{}` does not meet the extracted type `{}` [E4]",
                                    self.decls.universe.render(arg_val.ty),
                                    self.decls.universe.render(extracted)
                                ),
                                span,
                                "fallback mismatch",
                            );
                            return self.empty_val();
                        }
                        Val {
                            ty: extracted,
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::UnwrapOrElse => {
                        let Expr::Lambda { params, body, .. } = peel_paren(arg) else {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "`{name}` takes a `fn() -> t` lambda that inlines at the call \
                                     site"
                                ),
                                arg.span(),
                                "not a fallback lambda",
                            );
                            self.expr(arg, None);
                            return self.empty_val();
                        };
                        if !params.is_empty() {
                            self.error(
                                ErrorCode::ArityMismatch,
                                "the fallback lambda takes no parameters".to_owned(),
                                arg.span(),
                                "wrong lambda arity",
                            );
                            return self.empty_val();
                        }
                        let saved = self.snapshot();
                        let body_val = self.expr(body, Some(extracted));
                        self.restore(saved);
                        if !self.decls.universe.contains(extracted, body_val.ty) {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "the fallback lambda's `{}` does not meet the extracted type \
                                     `{}` [E4]",
                                    self.decls.universe.render(body_val.ty),
                                    self.decls.universe.render(extracted)
                                ),
                                span,
                                "fallback mismatch",
                            );
                            return self.empty_val();
                        }
                        Val {
                            ty: extracted,
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::OkOr => {
                        let arg_val = self.expr(arg, None);
                        Val {
                            ty: self.decls.universe.union(extracted, arg_val.ty),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::MapErr => unreachable!("the optional arm owns three of four"),
                }
            },
            Extraction::MapErr => {
                let Expr::Lambda {
                    params: handler_params,
                    body,
                    ..
                } = peel_paren(arg)
                else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        "`map_err` takes a one-parameter lambda `(e) => ...`".to_owned(),
                        arg.span(),
                        "not a handler lambda",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                };
                let [param] = handler_params.as_slice() else {
                    self.error(
                        ErrorCode::ArityMismatch,
                        "`map_err` takes a one-parameter lambda".to_owned(),
                        arg.span(),
                        "wrong lambda arity",
                    );
                    return self.empty_val();
                };
                let recv_val = self.expr(recv, None);
                let total = recv_val.ty;
                let annotated = param.ty.as_ref().and_then(|t| self.solve_here(t));
                let saved = self.snapshot();
                let param_ty = annotated.unwrap_or(total);
                self.bind_local(param.name.span, Binding {
                    ty: param_ty,
                    mutable: false,
                });
                let body_val = self.expr(body, None);
                self.restore(saved);
                let handled_set = if let Some(written) = annotated {
                    written
                } else {
                    let kept = self.decls.universe.inter(total, body_val.ty);
                    self.decls.universe.diff(total, kept)
                };
                let overlap = self.decls.universe.inter(total, handled_set);
                if self.decls.universe.is_empty(overlap) && !self.decls.universe.is_empty(total) {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`map_err` handles `{}`, which `{}` does not carry",
                            self.decls.universe.render(handled_set),
                            self.decls.universe.render(total)
                        ),
                        span,
                        "disjoint handler",
                    );
                    return self.empty_val();
                }
                let remainder = self.decls.universe.diff(total, handled_set);
                let result = self.decls.universe.union(remainder, body_val.ty);
                if self.decls.universe.is_empty(result) && !self.decls.universe.is_empty(total) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "`map_err` leaves nothing: the handler consumed all of `{}`",
                            self.decls.universe.render(total)
                        ),
                        span,
                        "empty remainder",
                    );
                    return self.empty_val();
                }
                Val {
                    ty: result,
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// Bare field access `x.f`: struct-owned, type-directed (F3); on a
    /// generic application through the constructor's shape and the
    /// application's argument substitution.
    fn field_access(&mut self, recv: &Expr, name: &Ident) -> Val {
        // A module alias's qualified member in value position (G4): the
        // resolver bound a linked const's def at the member's span —
        // the same res its selection spelling carries at its own use —
        // so the use types as the bound name's value and the receiver
        // is no value of its own. A member that refused at the seam
        // reports nothing here: the resolver named the boundary, and
        // the receiver's module-alias word would only cascade over
        // it.
        if let Some(head) = single_path_ident(recv)
            && let Some(Res::Module(module)) = self.res.get(head.span)
            && self.package.module_ref(*module).linked()
        {
            match self.res.get(name.span) {
                Some(Res::Def(def)) if self.decls.foreign_consts.contains_key(def) => {
                    let def = *def;
                    return self.def_value(def, name.span);
                },
                _ => {
                    self.solve_reported = true;
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                },
            }
        }
        let recv_val = self.expr(recv, None);
        // A poisoned receiver already reported; a field-access complaint
        // over its (empty or wrapper) type would be a cascade.
        if recv_val.poisoned {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        }
        let single = {
            let members = self.decls.universe.members(recv_val.ty);
            members.first().filter(|_| members.len() == 1).copied()
        };
        let Some(single) = single else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "field access needs one struct, `{}` given; narrow with `is` first [B2]",
                    self.decls.universe.render(recv_val.ty)
                ),
                name.span,
                "not a single struct",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let kind = self.decls.universe.atom_decl(single).ok().map(|d| &d.kind);
        let (def, subst): (DefId, FxHashMap<String, Ty>) = match kind {
            Some(AtomKind::Apply(ctor, args)) => {
                if let Some(def) = self.decls.defs_of_atoms.get(ctor).copied() {
                    let mut subst = FxHashMap::default();
                    if let Some(shape) = self.decls.structs.get(&def) {
                        for (param, arg) in shape.params.iter().zip(args.iter()) {
                            subst.insert(param.name.clone(), *arg);
                        }
                    }
                    (def, subst)
                } else {
                    self.no_field(recv_val.ty, name);
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                }
            },
            _ => match self.decls.defs_of_atoms.get(&single).copied() {
                Some(def) if self.decls.structs.contains_key(&def) => (def, FxHashMap::default()),
                _ => {
                    self.no_field(recv_val.ty, name);
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                },
            },
        };
        let field_ty = self
            .decls
            .structs
            .get(&def)
            .and_then(|shape| {
                shape
                    .fields
                    .iter()
                    .find(|field| field.name.name == name.name)
            })
            .map(|field| field.ty.clone());
        let Some(field_ty) = field_ty else {
            self.no_field(recv_val.ty, name);
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        self.type_vars.push(subst);
        let field_ty = self.solve_here(&field_ty);
        self.type_vars.pop();
        if let Some(ty) = field_ty {
            Val {
                ty,
                diverges: false,
                poisoned: false,
            }
        } else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!("field `{}` does not solve", name.name),
                    name.span,
                    "unsolvable field type",
                );
            }
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        }
    }

    fn no_field(&mut self, recv_ty: Ty, name: &Ident) {
        self.error(
            ErrorCode::TypeMismatch,
            format!(
                "`{}` has no field `{}`",
                self.decls.universe.render(recv_ty),
                name.name
            ),
            name.span,
            "no such field",
        );
    }

    /// `e?` propagation: the success part survives, every propagated member
    /// must be contained in the written return union (B2, E4).
    fn try_expr(&mut self, operand: &Expr, expected: Option<Ty>, span: Span) -> Val {
        let val = self.expr(operand, None);
        let total = val.ty;
        let ret = self.ret;
        let (ok, propagated) = match (expected, ret) {
            (Some(expected), _) => {
                let ok = self.decls.universe.inter(total, expected);
                let propagated = self.decls.universe.diff(total, expected);
                (ok, propagated)
            },
            (None, Some(ret)) => {
                let ok = self.decls.universe.diff(total, ret);
                let propagated = self.decls.universe.inter(total, ret);
                (ok, propagated)
            },
            (None, None) => {
                // No return union to split against: a top-level
                // initializer (no enclosing function), a fn without a
                // declared return, or a lambda whose return is inferred
                // (no annotation, or a bare type-variable expectation
                // from a generic callee) — the split needs a known
                // destination, never a silent whole-union bind.
                self.error(
                    ErrorCode::PropagationNotContained,
                    "`?` needs an enclosing return union to propagate into [E4]".to_owned(),
                    span,
                    "stray propagation",
                );
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            },
        };
        if let Some(ret) = ret {
            let escape = self.decls.universe.diff(propagated, ret);
            if !self.decls.universe.is_empty(escape) && !val.poisoned {
                self.error(
                    ErrorCode::PropagationNotContained,
                    format!(
                        "`?` propagates `{}`, which the return union `{}` does not contain [E4]",
                        self.decls.universe.render(escape),
                        self.decls.universe.render(ret)
                    ),
                    span,
                    "not contained",
                );
            }
        }
        if self.decls.universe.is_empty(ok) && !self.decls.universe.is_empty(total) && !val.poisoned
        {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`?` leaves nothing: every member of `{}` propagates [E4]",
                    self.decls.universe.render(total)
                ),
                span,
                "empty success part",
            );
        }
        Val {
            ty: ok,
            diverges: false,
            poisoned: val.poisoned,
        }
    }

    fn assign(&mut self, op: ds_parse::AssignOp, left: &Expr, right: &Expr, span: Span) -> Val {
        let unit = self.unit();
        let place = match peel_paren(left) {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                self.res.get(path.segments[0].span).cloned()
            },
            _ => None,
        };
        let Some(res) = place else {
            self.error(
                ErrorCode::InferenceFailed,
                "assignment places are bindings; field assignment is undecided surface [D2]"
                    .to_owned(),
                left.span(),
                "unsupported place",
            );
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        };
        let (declared, bind_span, mutable) = match &res {
            Res::Local { bind } => match self.locals.get(*bind) {
                Some(binding) => (binding.ty, Some(*bind), binding.mutable),
                None => (self.empty(), None, false),
            },
            Res::Def(def) => {
                let mutable = self.package.def(*def).kind == DefKind::Var;
                let val = self.const_ty(*def, left.span());
                (val.ty, None, mutable)
            },
            _ => (self.empty(), None, false),
        };
        if !mutable {
            self.error(
                ErrorCode::TypeMismatch,
                "assignment needs a `var` binding; `const` bindings are immutable [C3]".to_owned(),
                left.span(),
                "immutable place",
            );
        }
        if op == ds_parse::AssignOp::Plain {
            let val = self.expr(right, Some(declared));
            self.expect_contains(val, declared, right.span(), "assignment");
        } else {
            let current = bind_span
                .and_then(|span| self.refined.get(span).copied())
                .unwrap_or(declared);
            let right_val = self.expr(right, Some(current));
            let result = self.binary_val(
                compound_op(op),
                current,
                right_val.ty,
                left.span(),
                right.span(),
                span,
                right_val.poisoned,
            );
            self.expect_contains(result, declared, span, "compound assignment");
        }
        // Refinement resets at assignment [B2].
        if let Some(bind_span) = bind_span {
            self.refined.remove(bind_span);
        }
        Val {
            ty: unit,
            diverges: false,
            poisoned: false,
        }
    }

    /// `?` in a branch-final position splits against the expected type
    /// when one flows in (E4): a branch-final expression is the enclosing
    /// function's final expression when the if/match itself is last.
    fn if_expr(
        &mut self,
        cond: &Expr,
        then: &BlockExpr,
        else_: Option<&Expr>,
        expected: Option<Ty>,
        pos: bool,
    ) -> Val {
        self.cond_depth += 1;
        let cond_val = self.expr(cond, None);
        self.cond_depth -= 1;
        self.expect_bool(cond_val, cond.span());
        let narrows = self.harvest(cond);
        // The else path runs on ¬(a∧b∧…) = ¬a∨¬b∨…: when the condition is
        // an `&&` chain, no single conjunct is guaranteed false, so none
        // of its narrows inverts soundly there. A single-conjunct
        // condition still inverts (¬a is exactly the else).
        let else_narrows: &[Narrow] = if matches!(peel_paren(cond), Expr::Binary {
            op: BinaryOp::And,
            ..
        }) {
            &[]
        } else {
            &narrows
        };
        let unit = self.unit();
        let saved = self.snapshot();
        for narrow in &narrows {
            self.apply_narrow(narrow);
        }
        let then_val = {
            self.value_pos = pos;
            let val = self.block_expected(then, expected);
            self.value_pos = true;
            val
        };
        self.restore(saved);
        let else_val = if let Some(else_expr) = else_ {
            let saved = self.snapshot();
            for narrow in else_narrows {
                self.apply_inverse(narrow);
            }
            self.value_pos = pos;
            let val = self.expr(else_expr, expected);
            self.restore(saved);
            val
        } else {
            if then_val.diverges {
                // The then arm never falls through, so the continuation
                // is exactly the implicit else path: its inverse
                // narrowing stays live after the if.
                for narrow in else_narrows {
                    self.apply_inverse(narrow);
                }
            }
            Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            }
        };
        if then_val.diverges {
            return else_val;
        }
        if else_val.diverges {
            return then_val;
        }
        Val {
            ty: self.decls.universe.union(then_val.ty, else_val.ty),
            diverges: false,
            poisoned: false,
        }
    }

    fn match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        expected: Option<Ty>,
        pos: bool,
    ) -> Val {
        let scrut_val = self.expr(scrutinee, None);
        let scrut_ty = scrut_val.ty;
        let scrut_bind = self.path_local(scrutinee);
        let mut covered: Vec<TypeId> = Vec::new();
        let mut arm_vals = Vec::with_capacity(arms.len());
        // Arms type and cover sequentially: each arm sees the members
        // earlier arms left, so `s: string, i => ...` binds `i` the
        // remainder [F4 set semantics].
        let mut remaining = scrut_ty;
        for arm in arms {
            let saved = self.snapshot();
            let (arm_cover, arm_binders) =
                self.pattern_cover(&arm.pat, remaining, arm.ann.as_ref());
            if let Some(bind) = scrut_bind {
                // An arm covering no member atom (a literal arm) refines
                // nothing: narrowing the scrutinee to the empty type
                // would poison the arm body.
                if !arm_cover.is_empty() {
                    let refined = self.cover_ty(&arm_cover);
                    self.refined.insert(bind, refined);
                }
            }
            for (_, span, ty) in arm_binders {
                self.bind_local(span, Binding { ty, mutable: false });
            }
            if let Some(guard) = &arm.guard {
                self.cond_depth += 1;
                self.value_pos = true;
                let guard_val = self.expr(guard, None);
                self.cond_depth -= 1;
                self.expect_bool(guard_val, guard.span());
                for narrow in self.harvest(guard) {
                    self.apply_narrow(&narrow);
                }
            }
            self.value_pos = pos;
            let body_val = self.expr(&arm.body, expected);
            self.restore(saved);
            arm_vals.push(body_val);
            if arm.guard.is_none() {
                covered.extend(arm_cover.iter().copied());
                // a value-level arm (any literal test at any depth)
                // subtracts nothing from the remainder: the test may
                // fail, so later arms check over the full scrutinee
                // type — a branch checked on a superset may be dead,
                // never wrong. The exhaustiveness accounting above
                // keeps the arm's own cover regardless.
                if !Self::carries_value_tests(&arm.pat) {
                    let cover_ty = self.cover_ty(&arm_cover);
                    remaining = self.decls.universe.diff(remaining, cover_ty);
                }
            }
        }
        // Exhaustiveness by set difference [F4]; guards prove nothing,
        // conservatively, and neither do literal patterns. An applied
        // enum head stays opaque to the algebra, so its case atoms
        // exhaust it case-wise: a catchall that covered the application
        // itself or every minted case atom empties the remainder, while
        // a case minting no atom (a payloaded case over a generic head)
        // keeps the match honestly non-exhaustive.
        let missing = match self.applied_enum_head(scrut_ty) {
            Some((head_atom, case_atoms, unminted)) => {
                if covered.contains(&head_atom)
                    || (!unminted && case_atoms.iter().all(|atom| covered.contains(atom)))
                {
                    self.empty()
                } else {
                    scrut_ty
                }
            },
            None => self.missing_ty(scrut_ty, &covered),
        };
        if !self.decls.universe.members(missing).is_empty() {
            let scrut_text = self.decls.universe.render(scrut_ty);
            let missing_text = format!("`{}`", self.decls.universe.render(missing));
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::NonExhaustiveMatch,
                    format!("match on `{scrut_text}` is not exhaustive: missing: {missing_text}"),
                )
                .with_primary(span, "missing members"),
            );
        }
        let producing: Vec<&Val> = arm_vals.iter().filter(|v| !v.diverges).collect();
        if producing.is_empty() {
            return Val {
                ty: self.empty(),
                diverges: true,
                poisoned: false,
            };
        }
        let mut ty = self.empty();
        for val in producing {
            ty = self.decls.universe.union(ty, val.ty);
        }
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    fn for_expr(&mut self, pat: &Pattern, iter: &Expr, body: &Expr, span: Span) -> Val {
        let unit = self.unit();
        // A range is the surface's one pinned iterable: integer
        // endpoints, the pattern binds the element [F7]. A storable
        // range value is not in the record; the value-position stub
        // below stands.
        if let Expr::Range { start, end, .. } = peel_paren(iter) {
            if let Some(element_ty) = self.range_element(start, end, span) {
                let saved = self.snapshot();
                self.refined.clear();
                self.loop_depth += 1;
                self.loop_breaks.push(0);
                self.bind_pattern(pat, element_ty, false);
                self.value_pos = false;
                self.expr(body, None);
                self.loop_breaks.pop();
                self.loop_depth -= 1;
                self.restore(saved);
            }
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        }
        let iter_val = self.expr(iter, None);
        let single = {
            let members = self.decls.universe.members(iter_val.ty);
            members.first().filter(|_| members.len() == 1).copied()
        };
        let element_ty = single.and_then(|single| {
            let (_, args) = self.single_apply_of(single)?;
            args.first().copied()
        });
        let Some(element_ty) = element_ty else {
            // A poisoned iterable already reported; the collection-shape
            // complaint on top of it would be a cascade.
            if !iter_val.poisoned {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "iteration needs one collection type, `{}` given; the stdlib traits \
                         arrive at M3",
                        self.decls.universe.render(iter_val.ty)
                    ),
                    span,
                    "not iterable",
                );
            }
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        };
        // A bare-name iterable of a mutable binding records its window:
        // the body's own `mut` window over it refuses below, since the
        // entry count froze [D3]. A range head or a computed iterable
        // records nothing.
        let iter_window = self.iter_window(peel_paren(iter));
        if let Some(window) = iter_window {
            self.for_iters.push(window);
        }
        let saved = self.snapshot();
        self.refined.clear();
        self.loop_depth += 1;
        self.loop_breaks.push(0);
        self.bind_pattern(pat, element_ty, false);
        self.value_pos = false;
        self.expr(body, None);
        self.loop_breaks.pop();
        self.loop_depth -= 1;
        self.restore(saved);
        if iter_window.is_some() {
            self.for_iters.pop();
        }
        Val {
            ty: unit,
            diverges: false,
            poisoned: false,
        }
    }

    // ---- patterns ---------------------------------------------------------

    // ---- refinement ---------------------------------------------------------

    /// Narrows harvested from a condition: `is` tests and none-comparisons
    /// over local paths (B2).
    fn harvest(&mut self, cond: &Expr) -> Vec<Narrow> {
        let mut out = Vec::new();
        self.harvest_into(peel_paren(cond), &mut out);
        out
    }

    fn harvest_into(&mut self, cond: &Expr, out: &mut Vec<Narrow>) {
        match cond {
            Expr::Binary {
                op: BinaryOp::And,
                left,
                right,
                ..
            } => {
                self.harvest_into(peel_paren(left), out);
                self.harvest_into(peel_paren(right), out);
            },
            Expr::Binary {
                op, left, right, ..
            } if matches!(op, BinaryOp::Eq | BinaryOp::Ne) => {
                let (path_side, other) = (peel_paren(left), peel_paren(right));
                let bind = self
                    .path_local(path_side)
                    .or_else(|| self.path_local(other));
                let Some(bind) = bind else { return };
                if !is_none_expr(other) && !is_none_expr(path_side) {
                    return;
                }
                let none_ty = self.none_ty();
                out.push(Narrow {
                    bind,
                    keep: *op == BinaryOp::Eq,
                    ty: none_ty,
                });
            },
            Expr::Is { expr, ty, .. } => {
                let Some(bind) = self.path_local(peel_paren(expr)) else {
                    return
                };
                // The test type solved once at its own site; a re-solve
                // here only feeds the narrowing, so a repeated refusal
                // is dropped instead of reported twice.
                let before = self.diags.len();
                let solved = self.solve_here(ty);
                self.diags.truncate(before);
                let Some(solved) = solved else { return };
                out.push(Narrow {
                    bind,
                    keep: true,
                    ty: solved,
                });
            },
            _ => {},
        }
    }

    /// Monotone narrowing: intersect the binder's current type.
    fn apply_narrow(&mut self, narrow: &Narrow) {
        let Some(declared) = self.locals.get(narrow.bind).map(|b| b.ty) else {
            return;
        };
        let current = self.refined.get(narrow.bind).copied().unwrap_or(declared);
        let next = if narrow.keep {
            self.decls.universe.inter(current, narrow.ty)
        } else {
            self.decls.universe.diff(current, narrow.ty)
        };
        self.refined.insert(narrow.bind, next);
    }

    fn apply_inverse(&mut self, narrow: &Narrow) {
        let inverted = Narrow {
            bind: narrow.bind,
            keep: !narrow.keep,
            ty: narrow.ty,
        };
        self.apply_narrow(&inverted);
    }

    // ---- helpers ---------------------------------------------------------

    /// The method's registered signature, shared (the handle clones; the
    /// AST-shaped signature never deep-copies at call sites).
    fn member_sig(&self, method: MethodId) -> Option<Arc<FnSig>> {
        self.decls
            .members
            .get(&method)
            .or_else(|| self.decls.inherent.get(&method))
            .cloned()
    }

    fn error(&mut self, code: ErrorCode, message: String, span: Span, label: &str) {
        self.diags
            .push(Diagnostic::error(code, message).with_primary(span, label));
    }

    fn expect_contains(&mut self, val: Val, expected: Ty, span: Span, what: &str) -> bool {
        // A poisoned operand already reported its root cause; a boundary
        // mismatch on top of it would be a cascade.
        if val.diverges || val.poisoned || self.decls.universe.contains(expected, val.ty) {
            true
        } else {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "`{}` has type `{}`, not contained in `{}` [B2]",
                        what,
                        self.decls.universe.render(val.ty),
                        self.decls.universe.render(expected)
                    ),
                )
                .with_primary(span, "not contained"),
            );
            false
        }
    }

    /// One generic call's argument against its solved parameter type (B2):
    /// the structural binder stays silent on a constructor mismatch (it
    /// binds nothing for `list_len("words")`), so the solved parameter has
    /// to contain the argument or the mismatch reports. A parameter that
    /// still carries an unbound variable renders its written spelling,
    /// never the internal `$t` name.
    fn expect_param_contains(
        &mut self,
        val: Val,
        written: &Type,
        expected: Ty,
        span: Span,
    ) -> bool {
        // An empty side is an already-reported operand, and a poisoned one
        // reported inside its own subtree — never a new mismatch: the
        // cascade stays with its root cause.
        if val.diverges
            || val.poisoned
            || self.decls.universe.is_empty(val.ty)
            || self.decls.universe.is_empty(expected)
            || self.decls.universe.contains(expected, val.ty)
        {
            return true;
        }
        let expected_text = if self.has_free_vars(expected) {
            decls::render_type(written)
        } else {
            self.decls.universe.render(expected)
        };
        self.diags.push(
            Diagnostic::error(
                ErrorCode::TypeMismatch,
                format!(
                    "`argument` has type `{}`, not contained in `{}` [B2]",
                    self.decls.universe.render(val.ty),
                    expected_text
                ),
            )
            .with_primary(span, "not contained"),
        );
        false
    }

    /// Whether a solved type still carries an inference variable: a bare
    /// minted variable, or a structural shape applied over one.
    fn has_free_vars(&self, ty: Ty) -> bool {
        self.decls
            .universe
            .members(ty)
            .iter()
            .any(|atom| self.atom_has_free_vars(*atom))
    }

    fn atom_has_free_vars(&self, atom: TypeId) -> bool {
        match self.decls.universe.atom_decl(atom) {
            Ok(decl) => {
                decl.name.starts_with('$')
                    || match &decl.kind {
                        AtomKind::Apply(_, args) => args.iter().any(|arg| self.has_free_vars(*arg)),
                        AtomKind::Tuple(elements) => {
                            elements.iter().any(|elem| self.atom_has_free_vars(*elem))
                        },
                        AtomKind::FnSig(params, ret) => {
                            params.iter().any(|param| self.has_free_vars(*param))
                                || self.has_free_vars(*ret)
                        },
                        _ => false,
                    }
            },
            Err(_) => false,
        }
    }

    fn expect_bool(&mut self, val: Val, span: Span) -> bool {
        let bool_ty = self.bool_ty();
        if val.diverges || val.poisoned || self.decls.universe.contains(bool_ty, val.ty) {
            true
        } else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "condition has type `{}`, expected `bool`",
                    self.decls.universe.render(val.ty)
                ),
                span,
                "not a boolean",
            );
            false
        }
    }

    fn check_args(&mut self, args: &[&Expr], param_tys: &[Option<Ty>], span: Span) {
        if args.len() > param_tys.len() {
            // The direct path reports extra arguments where they appear;
            // the fn-typed-value path matches it, and the dropped
            // expressions still walk so inner errors surface.
            self.error(
                ErrorCode::ArityMismatch,
                "more arguments than the function type takes".to_owned(),
                args[param_tys.len()].span(),
                "extra argument",
            );
            for arg in &args[param_tys.len()..] {
                self.expr(arg, None);
            }
        } else if args.len() < param_tys.len() {
            self.error(
                ErrorCode::ArityMismatch,
                format!(
                    "missing argument(s): the function type takes {}",
                    param_tys.len()
                ),
                span,
                "missing arguments",
            );
        }
        for (arg, param) in args.iter().zip(param_tys.iter()) {
            match param {
                Some(param) => {
                    let val = self.expr(arg, Some(*param));
                    self.expect_contains(val, *param, arg.span(), "argument");
                },
                None => {
                    self.expr(arg, None);
                },
            }
        }
    }

    /// Solves a written type in the current scopes: type variables
    /// innermost-first, then package atoms, aliases, and traits.
    fn solve_here(&mut self, ty: &Type) -> Option<Ty> {
        self.solve_reported = false;
        self.solve_with(ty, &FxHashMap::default(), &[])
    }

    /// Whether the solve that just failed already reported its precise
    /// diagnostic (the tuple distribution cap); a generic fallback then
    /// stays silent so the golden carries one code, not two.
    fn take_solve_reported(&mut self) -> bool {
        std::mem::take(&mut self.solve_reported)
    }

    fn solve_with(
        &mut self,
        ty: &Type,
        subst: &FxHashMap<String, Ty>,
        generics: &[GenericParam],
    ) -> Option<Ty> {
        match ty {
            Type::Path { span, path, args } => {
                let [only] = path.segments.as_slice() else {
                    return None
                };
                // The callee's substitution binds first: a called function's
                // own variables must not be captured by the caller's
                // in-scope variables of the same name.
                if let Some(ty) = subst.get(&only.name) {
                    return Some(*ty);
                }
                for scope in self.type_vars.iter().rev() {
                    if let Some(ty) = scope.get(&only.name) {
                        return Some(*ty);
                    }
                }
                if generics.iter().any(|g| g.name == only.name) {
                    // An unbound callee variable: mint a fresh atom so the
                    // solve still produces a Ty; leaks are diagnosed at
                    // the call.
                    let var = self.mint_var(&only.name);
                    return self.decls.universe.atom_ty(var).ok();
                }
                if only.name == "none" && !self.package.declares("none") {
                    return Some(self.none_ty());
                }
                if only.name == "self" && !self.package.declares("self") {
                    // `self` as a type name retired [record 6.1]; resolve
                    // reported DS2015, so suppress the generic fallbacks.
                    self.solve_reported = true;
                    return None;
                }
                let def = self
                    .package
                    .lookup_type(&only.name)
                    .or_else(|| self.decls.foreign_type(&only.name))?;
                if !args.is_empty() {
                    if let Err(mismatch) =
                        decls::check_application(self.package, def, args.len(), self.decls)
                    {
                        self.error(
                            ErrorCode::TypeApplicationMismatch,
                            mismatch.message(&only.name),
                            *span,
                            "type application",
                        );
                        self.solve_reported = true;
                        return None;
                    }
                    let mut arg_tys = Vec::with_capacity(args.len());
                    for arg in args {
                        arg_tys.push(self.solve_with(arg, subst, generics)?);
                    }
                    let ctor = self.decls.atoms.get(&def).copied()?;
                    let apply = self.decls.universe.apply_atom(ctor, &arg_tys).ok()?;
                    return self.decls.universe.atom_ty(apply).ok();
                }
                match self.package.def(def).kind {
                    DefKind::Tag | DefKind::Struct | DefKind::Case => {
                        let atom = self.decls.atoms.get(&def).copied()?;
                        self.decls.universe.atom_ty(atom).ok()
                    },
                    DefKind::Enum => {
                        // A generic enum keeps its opaque atom; a
                        // non-generic one names the union of its minted
                        // case atoms [B3] — a linked enum's case defs
                        // answer through the name index the linked
                        // install filled. A zero-case enum names the
                        // empty set: uninhabited, still its own type, so
                        // impl heads and annotations over it check.
                        if let Some(&atom) = self.decls.atoms.get(&def) {
                            return self.decls.universe.atom_ty(atom).ok();
                        }
                        let shape = self.decls.enums.get(&def)?.clone();
                        if !shape.params.is_empty() {
                            return None;
                        }
                        let mut union: Option<Ty> = None;
                        for (name, ..) in &shape.cases {
                            let case_def = self
                                .package
                                .lookup_type(name)
                                .or_else(|| self.decls.foreign_type(name))?;
                            let atom = self.decls.atoms.get(&case_def).copied()?;
                            let ty = self.decls.universe.atom_ty(atom).ok()?;
                            union = Some(match union {
                                Some(acc) => self.decls.universe.union(acc, ty),
                                None => ty,
                            });
                        }
                        // A zero-case enum names the empty set.
                        Some(union.unwrap_or_else(|| self.empty()))
                    },
                    DefKind::TypeAlias => {
                        let (params, body) = self.decls.aliases.get(&def)?.clone();
                        if params.is_empty() {
                            self.solve_with(&body, subst, generics)
                        } else {
                            None // generic aliases resolve through use-site args
                        }
                    },
                    DefKind::Trait => {
                        let trait_id = self.decls.traits.get(&def).copied()?;
                        self.decls
                            .universe
                            .solve(&TypeExpr::trait_ref(trait_id))
                            .ok()
                    },
                    DefKind::Fn | DefKind::Const | DefKind::Var => None,
                }
            },
            Type::Optional { inner, .. } => {
                let inner = self.solve_with(inner, subst, generics)?;
                Some(self.decls.universe.optional(inner))
            },
            Type::Negation { inner, .. } => {
                let Type::Path { path, .. } = inner.as_ref() else {
                    return None
                };
                let [only] = path.segments.as_slice() else {
                    return None
                };
                let def = self.package.lookup_type(&only.name)?;
                let trait_id = self.decls.traits.get(&def).copied()?;
                self.decls.universe.solve(&TypeExpr::not(trait_id)).ok()
            },
            Type::Paren { inner, .. } => self.solve_with(inner, subst, generics),
            Type::Union { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.union(a, b))
            },
            Type::Intersect { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.inter(a, b))
            },
            Type::Difference { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.diff(a, b))
            },
            Type::Tuple { items, .. } => {
                let mut elements = Vec::with_capacity(items.len());
                for item in items {
                    elements.push(self.solve_with(item, subst, generics)?);
                }
                match self.decls.universe.product(&elements) {
                    Ok(ty) => Some(ty),
                    Err(err @ Error::TupleDistributionCap { .. }) => {
                        self.error(
                            ErrorCode::TupleDistributionCap,
                            err.to_string(),
                            ty.span(),
                            "tuple distribution cap",
                        );
                        self.solve_reported = true;
                        None
                    },
                    Err(_) => None,
                }
            },
            Type::Unit(_) => Some(self.unit()),
            Type::Fn { params, ret, .. } => {
                let mut param_tys = Vec::with_capacity(params.len());
                for param in params {
                    // A bare fn-type parameter (`fn(t) -> r`) names the
                    // in-scope variable, per the surface spelling.
                    let written = param
                        .ty
                        .clone()
                        .unwrap_or_else(|| name_type(&param.name.name));
                    param_tys.push(self.solve_with(&written, subst, generics)?);
                }
                let ret_ty = match ret {
                    Some(ret) => self.solve_with(ret, subst, generics)?,
                    None => self.unit(),
                };
                let atom = self.decls.universe.fn_atom(&param_tys, ret_ty).ok()?;
                self.decls.universe.atom_ty(atom).ok()
            },
            // The `type` keyword solves to the builtin comptime type-value
            // atom (G1, G2): the comptime evaluator's staged reflection
            // indices carry it, and nothing constructs its values at
            // runtime.
            Type::TypeKw(_) => self.decls.universe.atom_ty(TypeId::TYPE).ok(),
            Type::Ptr { .. } | Type::Foreign { .. } => None,
        }
    }

    /// Structural parameter-to-argument matching for generic
    /// instantiation: binds callee variables by walking parameter shapes
    /// against the argument's solved type.
    fn match_param(
        &mut self,
        param: &Type,
        arg: Ty,
        generics: &[GenericParam],
        subst: &mut FxHashMap<String, Ty>,
    ) {
        match param {
            Type::Paren { inner, .. } => self.match_param(inner, arg, generics, subst),
            Type::Path { path, args, .. } => {
                let [only] = path.segments.as_slice() else {
                    return
                };
                if args.is_empty() {
                    if generics.iter().any(|g| g.name == only.name) {
                        subst.entry(only.name.clone()).or_insert(arg);
                    }
                    return;
                }
                let members = self.decls.universe.members(arg);
                let Some(single) = members.first().filter(|_| members.len() == 1) else {
                    return;
                };
                let Some((ctor, apply_args)) = self.single_apply_of(*single) else {
                    return
                };
                // The parameter's constructor must be the argument's.
                let Some(def) = self.package.lookup_type(&only.name) else {
                    return
                };
                if self.decls.atoms.get(&def) != Some(&ctor) {
                    return;
                }
                for (param_arg, arg_ty) in args.iter().zip(apply_args.iter()) {
                    self.match_param(param_arg, *arg_ty, generics, subst);
                }
            },
            Type::Optional { inner, .. } => {
                let none = self.none_ty();
                let without_none = self.decls.universe.diff(arg, none);
                self.match_param(inner, without_none, generics, subst);
            },
            Type::Tuple { items, .. } => {
                // Tuple elements solve and mint through the universe, so the
                // member set is read by index across the walk.
                let len = self.decls.universe.members(arg).len();
                for index in 0..len {
                    let member = self.decls.universe.members(arg)[index];
                    if let Some(elements) = self.tuple_elements(member) {
                        if elements.len() != items.len() {
                            continue;
                        }
                        for (item, element) in items.iter().zip(elements.iter()) {
                            if let Ok(ty) = self.decls.universe.atom_ty(*element) {
                                self.match_param(item, ty, generics, subst);
                            }
                        }
                    }
                }
            },
            Type::Fn { params, ret, .. } => {
                let members = self.decls.universe.members(arg);
                let Some(single) = members.first().filter(|_| members.len() == 1) else {
                    return
                };
                let Some((param_tys, ret_ty)) = self.decompose_fn_atom(*single) else {
                    return
                };
                for (param, arg_ty) in params.iter().zip(param_tys.iter()) {
                    if let Some(ty) = &param.ty {
                        self.match_param(ty, *arg_ty, generics, subst);
                    }
                }
                if let Some(ret) = ret {
                    self.match_param(ret, ret_ty, generics, subst);
                }
            },
            _ => {},
        }
    }

    fn trait_of_type(&self, ty: &Type) -> Option<TraitId> {
        let Type::Path { path, .. } = ty else {
            return None
        };
        let [only] = path.segments.as_slice() else {
            return None
        };
        let def = self.package.lookup_type(&only.name)?;
        self.decls.traits.get(&def).copied()
    }

    /// The trait arms one written bound carries: a bare trait path, or
    /// every arm of an intersection — a derive's synthesized bound
    /// intersects the family's trait with whatever the declaration
    /// wrote, and a hand-written intersection spells the same shape.
    fn bound_traits(&self, ty: &Type, out: &mut Vec<TraitId>) {
        match ty {
            Type::Intersect { left, right, .. } => {
                self.bound_traits(left, out);
                self.bound_traits(right, out);
            },
            other => {
                if let Some(trait_id) = self.trait_of_type(other) {
                    out.push(trait_id);
                }
            },
        }
    }

    /// Whether a type is exactly one minted type variable (the checker's
    /// `$name` atoms): an inference hole, not a data type.
    fn is_var_ty(&self, ty: Ty) -> bool {
        match self.decls.universe.members(ty) {
            [only] => self
                .decls
                .universe
                .atom_decl(*only)
                .is_ok_and(|decl| decl.name.starts_with('$')),
            _ => false,
        }
    }

    /// The runtime evidence one inherent call site must stage for the
    /// impl's declared bounds (H3): the head pattern substitutes against
    /// the receiver, and every bound must hold for the substituted type
    /// exactly as the trait form's instance bounds do [B6] — an in-scope
    /// assumption satisfies the checker while the record keeps `None`
    /// (the lowerer refuses that site naming the staging seam). A head
    /// that does not unify (a union receiver solving the variables
    /// differently) records nothing.
    fn inherent_evidence(
        &mut self,
        head: Ty,
        conditions: &[Condition<Ty>],
        recv: Ty,
        span: Span,
    ) -> InherentCallBounds {
        let targets = crate::instance_solve::inherent_bound_targets(
            &mut self.decls.universe,
            head,
            conditions,
            recv,
        );
        let mut out = Vec::new();
        for (var, trait_id, target) in targets {
            if self.decls.universe.is_empty(target) {
                continue; // a cascading argument error's empty type
            }
            let served = if self.trait_satisfied(trait_id, target, span) {
                self.instance_resolution(trait_id, target).solved()
            } else {
                None
            };
            out.push((var, trait_id, served));
        }
        out
    }

    /// Whether a trait holds for a type: an in-scope assumption covers
    /// it, or the environment resolves an instance — a containing head
    /// for concrete receivers, or a generic head that unifies with the
    /// receiver under provable bounds (B6). Diagnoses on failure.
    fn trait_satisfied(&mut self, trait_id: TraitId, ty: Ty, span: Span) -> bool {
        if self.assumption_covers(trait_id, ty) {
            return true;
        }
        match self.instance_resolution(trait_id, ty) {
            Resolution::Unique(_) | Resolution::Covered(_) => true,
            Resolution::Ambiguous(candidates) => {
                self.instance_ambiguity(trait_id, ty, &candidates, span);
                false
            },
            Resolution::None => {
                self.error(
                    ErrorCode::UnsatisfiedConstraint,
                    format!(
                        "no instance of `{}` for `{}`",
                        self.trait_name(trait_id),
                        self.decls.universe.render(ty)
                    ),
                    span,
                    "unimplemented trait",
                );
                false
            },
            Resolution::BoundUnsat { bound, target, .. } => {
                self.report_bound_miss(trait_id, bound, &target, span);
                false
            },
        }
    }

    /// The bound-miss diagnostic both call spellings share: the head
    /// matched and the substituted bound is the missing proof, so the
    /// message names the bound's trait over the substituted type.
    fn report_bound_miss(
        &mut self,
        trait_id: TraitId,
        bound: TraitId,
        target: &[TypeId],
        span: Span,
    ) {
        let miss = self.render_members(target);
        self.error(
            ErrorCode::UnsatisfiedConstraint,
            format!(
                "no instance of `{}` for {}: the `{}` impl's bound is unsatisfied there",
                self.trait_name(bound),
                miss,
                self.trait_name(trait_id)
            ),
            span,
            "unsatisfied bound",
        );
    }

    /// Whether an in-scope generic bound (`[t: json]`) assumes the trait
    /// for the type.
    fn assumption_covers(&self, trait_id: TraitId, ty: Ty) -> bool {
        self.assumptions
            .iter()
            .rev()
            .any(|(t, assumed)| *t == trait_id && self.decls.universe.contains(*assumed, ty))
    }

    /// The environment's call-site resolution (B6): the instance whose
    /// head contains the receiver, or — for a generic impl's head — the
    /// one whose pattern unifies with it under provable bounds. The
    /// solved instance is the declared instance; a per-substitution
    /// instance is never minted, because the call's own receiver type
    /// (recorded beside it) carries the substitution the dispatch seam
    /// needs.
    fn instance_resolution(&self, trait_id: TraitId, ty: Ty) -> Resolution {
        crate::instance_solve::resolve(
            &self.decls.universe,
            &self.decls.instances,
            &self.assumptions,
            trait_id,
            ty,
        )
    }

    /// Several impls serve one call site (B6): declared heads are
    /// pairwise disjoint, but a generic head is opaque to the set
    /// algebra, so two generic impls of one trait can both serve one
    /// receiver. A compile error, never a priority order.
    fn instance_ambiguity(
        &mut self,
        trait_id: TraitId,
        recv: Ty,
        candidates: &[InstanceId],
        span: Span,
    ) {
        let mut diag = Diagnostic::error(
            ErrorCode::AmbiguousInstance,
            format!(
                "{} impls of `{}` accept receiver `{}`; dispatch must pick one [B6]",
                candidates.len(),
                self.trait_name(trait_id),
                self.decls.universe.render(recv)
            ),
        )
        .with_primary(span, "ambiguous dispatch");
        for &id in candidates {
            if let (Ok(head), Some(site)) = (
                self.decls.instances.head(id),
                self.decls.instance_impls.get(&id),
            ) {
                diag = diag.with_secondary(
                    *site,
                    format!("instance head `{}`", self.decls.universe.render(head)),
                );
            }
        }
        self.diags.push(diag);
    }

    fn trait_name(&self, trait_id: TraitId) -> String {
        self.decls
            .universe
            .trait_decl(trait_id)
            .map_or_else(|_| "<unknown>".to_owned(), |d| d.name.clone())
    }

    fn push_generics(&mut self, generics: &[GenericParam]) {
        let mut scope = FxHashMap::default();
        self.assumption_marks.push(self.assumptions.len());
        for param in generics {
            let var = self.mint_var(&param.name);
            if let Ok(ty) = self.decls.universe.atom_ty(var) {
                scope.insert(param.name.clone(), ty);
                if param.bound.as_ref().is_some_and(decls::linear_marker) {
                    self.linear_vars.insert(var, false);
                }
                if let Some(bound) = &param.bound {
                    let mut arms = Vec::new();
                    self.bound_traits(bound, &mut arms);
                    for trait_id in arms {
                        self.assumptions.push((trait_id, ty));
                    }
                }
            }
        }
        self.type_vars.push(scope);
    }

    fn pop_generics(&mut self) {
        self.type_vars.pop();
        if let Some(mark) = self.assumption_marks.pop() {
            self.assumptions.truncate(mark);
        }
    }

    fn mint_var(&mut self, name: &str) -> TypeId {
        self.decls.var_counter += 1;
        let unique = format!("${name}#{}", self.decls.var_counter);
        // Impossible by construction: surface names cannot contain `$` or
        // `#`, and both minting sites (instance declaration and body
        // checking) draw from the shared var_counter, so the minted name
        // is always fresh — a failure would be a silently wrong type, so
        // it panics instead of falling back to NONE.
        self.decls
            .universe
            .declare_tag(&unique)
            .expect("minted variable names are unique by the shared counter")
    }

    fn fresh_var(&mut self, name: &str) -> Ty {
        let var = self.mint_var(name);
        self.decls
            .universe
            .atom_ty(var)
            .unwrap_or_else(|_| self.empty())
    }

    /// A nursery block [E2]: the body checks in a fresh scope over the
    /// block's own nursery scope entry, every `spawn` inside it records
    /// its body's answer, and the block's value is the collected
    /// answers' list — the join channel's checker face. The element
    /// type is the union of the spawned bodies' answers (a body that
    /// only cancels never answers, so it contributes nothing); a block
    /// whose spawns never answer at all keeps the unit element, the
    /// empty list's honest inhabitant.
    fn nursery_block(&mut self, body: &BlockExpr, span: Span) -> Val {
        self.nursery_scopes.push(NurseryScope {
            outlived: self.locals.live_keys(),
            at: span,
        });
        self.spawn_answers.push(Vec::new());
        let saved = self.snapshot();
        self.refined.clear();
        self.value_pos = false;
        self.block(body);
        self.restore(saved);
        self.nursery_scopes.pop();
        let answers = self
            .spawn_answers
            .pop()
            .expect("the nursery pushed its collector");
        let mut elem = self.empty();
        let mut answering = false;
        for answer in answers {
            if !answer.diverges {
                elem = self.decls.universe.union(elem, answer.ty);
                answering = true;
            }
        }
        // a block whose spawns never answer at all (every body cancels,
        // or no spawn stands) keeps the unit element — the empty list's
        // honest inhabitant
        if !answering {
            elem = self.unit();
        }
        let ty = self.applied_list_ty(elem).unwrap_or_else(|| self.unit());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// A spawned body [E2]: its own frame, its tail checked as the
    /// child's answer, and the spawn-scoped `cancel` refinement — a
    /// `cancel` arm inside a spawned body never answers, so it drops
    /// out of the body's own unions instead of polluting the answer
    /// channel with the staging signature's unit. The expression itself
    /// stays unit: the answers collect at the enclosing nursery's join.
    fn spawned_block(&mut self, body: &BlockExpr) -> Val {
        let saved = self.snapshot();
        self.refined.clear();
        let frame = self.enter_frame();
        self.spawn_depth += 1;
        self.value_pos = true;
        let val = self.block(body);
        self.spawn_depth -= 1;
        self.exit_frame(frame);
        self.restore(saved);
        // the body's answer lands on the span the lowerer plans the
        // child function's return from
        self.expr_types.insert(body.span, val);
        if let Some(answers) = self.spawn_answers.last_mut() {
            answers.push(val);
        }
        Val {
            ty: self.unit(),
            diverges: false,
            poisoned: false,
        }
    }

    /// Enters one lambda or spawned-body frame: the lexical loop depth
    /// stays with the enclosing frame because loop exits never cross a
    /// function or lambda frame boundary. Returns the saved depth for the
    /// paired [`Self::exit_frame`].
    fn enter_frame(&mut self) -> (usize, Vec<usize>) {
        self.frame_depth += 1;
        (
            std::mem::replace(&mut self.loop_depth, 0),
            std::mem::take(&mut self.loop_breaks),
        )
    }

    /// The paired exit of [`Self::enter_frame`].
    fn exit_frame(&mut self, saved: (usize, Vec<usize>)) {
        self.frame_depth -= 1;
        self.loop_depth = saved.0;
        self.loop_breaks = saved.1;
    }

    fn snapshot(&self) -> ScopeMark {
        ScopeMark {
            locals: self.locals.mark(),
            refined: self.refined.mark(),
            assumptions: self.assumptions.len(),
            ret: self.ret,
        }
    }

    fn restore(&mut self, saved: ScopeMark) {
        self.locals.restore_to(saved.locals);
        self.refined.restore_to(saved.refined);
        self.assumptions.truncate(saved.assumptions);
        self.ret = saved.ret;
    }

    fn local_ty(&self, bind: Span) -> Option<Ty> {
        self.refined
            .get(bind)
            .copied()
            .or_else(|| self.locals.get(bind).map(|b| b.ty))
    }

    /// Whether a solved type is exactly the builtin `type` atom (G1,
    /// G2): the comptime type-value type.
    fn is_type_atom(&self, ty: Ty) -> bool {
        matches!(self.decls.universe.members(ty), [atom] if *atom == TypeId::TYPE)
    }

    /// Whether a package definition names a declared type — the type-
    /// namespace kinds a type value can denote (a struct, enum, tag, or
    /// alias; a case keeps its constructor-value reading).
    fn names_declared_type(&self, def: DefId) -> bool {
        matches!(
            self.package.def(def).kind,
            DefKind::Struct | DefKind::Enum | DefKind::Tag | DefKind::TypeAlias
        )
    }

    /// The binder span when an expression is a local path.
    fn path_local(&self, expr: &Expr) -> Option<Span> {
        let Expr::Path { path, .. } = peel_paren(expr) else {
            return None
        };
        let [only] = path.segments.as_slice() else {
            return None
        };
        match self.res.get(only.span) {
            Some(Res::Local { bind }) => Some(*bind),
            _ => None,
        }
    }

    fn cover_ty(&mut self, covered: &[TypeId]) -> Ty {
        let mut ty = self.empty();
        for atom in covered {
            if let Ok(atom_ty) = self.decls.universe.atom_ty(*atom) {
                ty = self.decls.universe.union(ty, atom_ty);
            }
        }
        ty
    }

    /// The scrutinee members no arm covers.
    fn missing_ty(&mut self, scrut: Ty, covered: &[TypeId]) -> Ty {
        let covered_ty = self.cover_ty(covered);
        self.decls.universe.diff(scrut, covered_ty)
    }

    fn render_members(&mut self, members: &[TypeId]) -> String {
        let ty = self.cover_ty(members);
        format!("`{}`", self.decls.universe.render(ty))
    }

    fn struct_shape_of(&self, atom: TypeId) -> Option<&decls::StructShape> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        self.decls.structs.get(&def)
    }

    /// Payload types of a struct (named fields, positionally matched) or an
    /// enum case, for call patterns.
    fn payload_tys(&mut self, _path: &Path, atom: TypeId) -> Option<Vec<Ty>> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        if let Some(shape) = self.decls.structs.get(&def).cloned() {
            let mut out = Vec::with_capacity(shape.fields.len());
            for field in &shape.fields {
                out.push(self.solve_here(&field.ty)?);
            }
            return Some(out);
        }
        // A minted tuple-payload case carries no struct shape; its
        // payload types come from the case table in declaration order.
        let tuple_payload = match self.decls.cases.get(&def).map(|c| &c.payload) {
            Some(EnumPayload::Tuple(types)) => Some(types.clone()),
            _ => None,
        };
        if let Some(types) = tuple_payload {
            let mut out = Vec::with_capacity(types.len());
            for ty in &types {
                out.push(self.solve_here(ty)?);
            }
            return Some(out);
        }
        None
    }

    fn atom_name(&self, atom: TypeId) -> String {
        self.decls
            .universe
            .atom_decl(atom)
            .map_or_else(|_| "<unknown>".to_owned(), |d| d.name.clone())
    }

    /// The application shape of a singleton type, as constructor plus
    /// argument types.
    fn single_apply_of(&self, atom: TypeId) -> Option<(TypeId, Vec<Ty>)> {
        match self.decls.universe.atom_decl(atom).ok()?.kind {
            AtomKind::Apply(ctor, ref args) => Some((ctor, args.to_vec())),
            _ => None,
        }
    }

    /// The application shape of a whole type, when it is one application.
    fn single_apply(&mut self, ty: Ty) -> Option<(TypeId, Vec<Ty>)> {
        let [only] = self.decls.universe.members(ty) else {
            return None
        };
        self.single_apply_of(*only)
    }

    /// The applied enum head of a scrutinee type with its minted case
    /// atoms: `(head atom, case atoms, any unminted case)` — the head
    /// atom is the application itself, the member a catchall covers.
    /// Applications stay opaque to the set algebra, so case patterns
    /// over an enum application cover it case-wise — a unit case's atom
    /// carries no substitution, while a case minting no atom (a
    /// payloaded case over a generic head) can never be covered. `None`
    /// when the scrutinee is not one application of a declared enum
    /// head.
    fn applied_enum_head(&mut self, scrut: Ty) -> Option<(TypeId, Vec<TypeId>, bool)> {
        let [only] = self.decls.universe.members(scrut) else {
            return None
        };
        let (ctor, _) = self.single_apply_of(*only)?;
        let enum_def = self.decls.defs_of_atoms.get(&ctor).copied()?;
        let shape = self.decls.enums.get(&enum_def)?.clone();
        let mut atoms = Vec::with_capacity(shape.cases.len());
        let mut unminted = false;
        for (name, ..) in &shape.cases {
            match self
                .package
                .lookup_type(name)
                .or_else(|| self.decls.foreign_type(name))
                .and_then(|def| self.decls.atoms.get(&def).copied())
            {
                Some(atom) => atoms.push(atom),
                None => unminted = true,
            }
        }
        Some((*only, atoms, unminted))
    }

    fn decompose_fn(&mut self, ty: Ty) -> Option<(Vec<Ty>, Ty)> {
        let [only] = self.decls.universe.members(ty) else {
            return None
        };
        self.decompose_fn_atom(*only)
    }

    fn decompose_fn_atom(&self, atom: TypeId) -> Option<(Vec<Ty>, Ty)> {
        let (params, ret) = self.decls.universe.fn_sig(atom)?;
        Some((params.to_vec(), ret))
    }

    fn fn_typed(&self, ty: Ty) -> bool {
        self.decls
            .universe
            .members(ty)
            .iter()
            .any(|atom| self.decls.universe.fn_sig(*atom).is_some())
    }

    /// The per-element expected types of one expected tuple type: the
    /// expectation must be a single tuple atom of the construction's
    /// arity (a union of tuples or a wider member set carries no
    /// element-wise expectation).
    fn expected_tuple_elems(&mut self, expected: Ty, arity: usize) -> Option<Vec<Ty>> {
        let [only] = self.decls.universe.members(expected) else {
            return None;
        };
        let elements = self.tuple_elements(*only)?;
        if elements.len() != arity {
            return None;
        }
        elements
            .iter()
            .map(|e| self.decls.universe.atom_ty(*e).ok())
            .collect()
    }

    fn tuple_elements(&self, atom: TypeId) -> Option<Vec<TypeId>> {
        match self.decls.universe.atom_decl(atom).ok()?.kind {
            AtomKind::Tuple(ref elements) => Some(elements.clone()),
            _ => None,
        }
    }

    fn named_atom_ty(&mut self, span: Span, name: &str, what: &str) -> Option<Ty> {
        let ty = self
            .decls
            .universe
            .atom_by_name(name)
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok());
        if ty.is_none() {
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{what}` needs a declared `{name}` type [B1]"),
                span,
                "no such type",
            );
        }
        ty
    }

    fn is_string_ty(&self, ty: Ty) -> bool {
        // By name through the atom table: `atom_ty` interns and needs
        // exclusivity this shared-context check does not have.
        match self.decls.universe.members(ty) {
            [only] => self
                .decls
                .universe
                .atom_decl(*only)
                .is_ok_and(|decl| decl.name == "string"),
            _ => false,
        }
    }

    fn none_ty(&mut self) -> Ty {
        self.decls
            .universe
            .atom_ty(TypeId::NONE)
            .unwrap_or_else(|_| self.empty())
    }

    fn bool_ty(&mut self) -> Ty {
        self.decls
            .universe
            .atom_by_name("bool")
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty())
    }

    fn unit(&mut self) -> Ty {
        self.decls.universe.unit_ty()
    }

    fn empty(&mut self) -> Ty {
        self.decls.universe.empty_ty()
    }

    /// The empty-typed value of an errored expression.
    fn empty_val(&mut self) -> Val {
        Val {
            ty: self.empty(),
            diverges: false,
            poisoned: false,
        }
    }

    fn fn_atom_of(&mut self, sig: &FnSig) -> Val {
        let mut param_tys = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            param_tys.push(
                param
                    .ty
                    .as_ref()
                    .and_then(|t| self.solve_here(t))
                    .unwrap_or_else(|| self.empty()),
            );
        }
        let ret = sig
            .ret
            .as_ref()
            .and_then(|r| self.solve_here(r))
            .unwrap_or_else(|| self.unit());
        let params_boxed: Box<[Ty]> = param_tys.into();
        let ty = self
            .decls
            .universe
            .fn_atom(&params_boxed, ret)
            .ok()
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    fn span_text(&self, span: Span) -> String {
        self.files
            .source(span.file)
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
            .to_owned()
    }
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

/// Whether a parameter binds a re-assignable place (C3): only the `mut`
/// window re-binds — `move` transfers once, `comptime` is a compile-time
/// value, and a plain borrow is immutable — so assignment accepts a
/// `mut` parameter alone, exactly like a `var` local.
fn param_is_mut(param: &Param) -> bool {
    matches!(param.mode, ds_parse::ParamMode::Mut(_))
}

/// One comptime parameter's annotation inside the round's shape: the
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
    let span = Span::new(ds_diag::FileId::new(0), 0, 0);
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
