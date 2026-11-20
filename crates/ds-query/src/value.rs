//! Values produced by queries.

use std::{fmt, sync::Arc};

use ds_check::{
    Analyzed, EffectRow, Effects, ScopeFrame,
    linearity::{DropSite, LastUse, Move},
    resolve::{Package, Resolutions},
};
use ds_diag::{Diagnostic, LabelStyle, Span};
use ds_parse::Ast;

use crate::{
    facts::{BlockFacts, primary_label_spans},
    key::BlockId,
};

/// Result of the [`ParseFile`](crate::QueryKey::ParseFile) query.
///
/// Parse failures are data, not engine errors: a syntax-broken file still
/// produces a value (with `ast: None` and the diagnostics), so dependents
/// such as the block index keep working for the rest of the file instead of
/// being poisoned by an error.
///
/// [`Ast`] implements neither `PartialEq` nor `Eq`, so [`ParseOutput`]
/// defines equality by source text. That is sound only because the parse
/// compute (`queries::parse`) is a pure function of exactly one input —
/// the file's `FileContent`, nothing else in [`Ctx`](crate::Ctx) — so
/// equal sources yield identical trees and diagnostics and renewal never
/// misses a change. The two facts are one contract split across two files:
/// if the parse compute ever reads more than the source, this equality
/// must widen with it.
#[derive(Clone, Debug)]
pub struct ParseOutput {
    /// The parsed tree, or `None` when the source had syntax errors.
    pub ast: Option<Arc<Ast>>,
    /// Syntax diagnostics; empty on a clean parse.
    pub diagnostics: Arc<[Diagnostic]>,
    /// The source the tree was parsed from; also the block-source substrate.
    pub source: Arc<str>,
}

impl PartialEq for ParseOutput {
    // Source equality stands in for whole-value equality; see the type docs
    // for the purity contract this leans on. Sharing the source arc is the
    // one case that answers without reading the text.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source) || self.source == other.source
    }
}

impl Eq for ParseOutput {}

/// One block of a file: identity plus the byte span of its source text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlockInfo {
    /// The block's identity.
    pub id: BlockId,
    /// Byte range of the block within the file. Tree-derived when the parse
    /// succeeded (item extents per the grammar's `file_block` spans); the
    /// `##` text-scan extents when it failed.
    pub span: Span,
}

/// Result of the [`BlockIndex`](crate::QueryKey::BlockIndex) query: the
/// file's blocks in source order, mapping byte spans to blocks.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockIndexOutput {
    /// Blocks in source order; every file has at least one (G3). Shared like
    /// the parse payload: a memo hit clones the arc, not the blocks.
    pub blocks: Arc<[BlockInfo]>,
}

/// Result of the [`ScopeIndex`](crate::QueryKey::ScopeIndex) query: one
/// file's local scope frames, shared so a memo hit clones the arc, not
/// the frames. Package-level declarations need no frames: they are the
/// [`Resolve`](crate::QueryValue::Resolve) value's own table.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScopeIndexOutput {
    /// The file's scope frames in walk order.
    pub frames: Arc<[ScopeFrame]>,
}

/// Result of the [`LinearityCheck`](crate::QueryKey::LinearityCheck)
/// query: the linearity facts of one definition — the diagnostics and
/// placements whose spans address its block, sliced out of the package
/// pass (C1, C5).
///
/// The slice is defined by primary spans: a diagnostic belongs to the
/// definition its primary label addresses, so the two-span `DS3001`
/// (moved here / used again) lands with the definition doing the second
/// use even when its first use was passed a value from elsewhere.
#[derive(Clone, Debug)]
pub struct LinearityOutput {
    /// Linearity diagnostics addressing this definition. Shared like the
    /// parse payload: memo hits clone the arc, not the diagnostics.
    pub diags: Arc<[Diagnostic]>,
    /// Consuming uses of owned non-copyable bindings in this definition.
    pub moves: Arc<[Move]>,
    /// Scope-exit deaths in this definition where deinit must run.
    pub drops: Arc<[DropSite]>,
    /// Final uses in this definition, borrowed or consuming.
    pub last_uses: Arc<[LastUse]>,
}

/// One label's comparison fact: file, byte range, primary-ness.
type LabelSig = (u32, u32, u32, bool);

/// Compare two diagnostic lists by the facts their identity rests on:
/// code plus, per label, file, span, and primary-ness. Labels' texts and
/// notes are derived, but the same byte range in another file — or a
/// primary/secondary swap, which moves a fact between blocks — is a
/// different fact, so a memo holding the old list must renew.
fn same_diags(left: &[Diagnostic], right: &[Diagnostic]) -> bool {
    fn sig(diags: &[Diagnostic]) -> Vec<(&str, Vec<LabelSig>)> {
        diags
            .iter()
            .map(|d| {
                (
                    d.code.as_str(),
                    d.labels
                        .iter()
                        .map(|l| {
                            (
                                l.span.file.as_u32(),
                                l.span.start,
                                l.span.end,
                                matches!(l.style, LabelStyle::Primary),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }
    sig(left) == sig(right)
}

impl PartialEq for LinearityOutput {
    fn eq(&self, other: &Self) -> bool {
        same_diags(&self.diags, &other.diags)
            && self.moves == other.moves
            && self.drops == other.drops
            && self.last_uses == other.last_uses
    }
}

impl Eq for LinearityOutput {}

/// Result of the [`PackageLinearity`](crate::QueryKey::PackageLinearity)
/// query: the M2 affine pass (C1, C5) over the package's derive-expanded
/// trees, the substrate every definition's [`LinearityOutput`] slices. The
/// fact lists are shared, so a memo hit clones arcs, not the facts.
///
/// Renewal identity: the fact lists themselves. The pass reads the analyzed
/// bundle for its tables, but the bundle may renew against an unchanged
/// identity while a span-keyed fact moved, so the dependency set also
/// carries the expansion — whose recompute always counts as changed — and
/// the pass re-derives on every parse move, renewing only when its own
/// facts compare equal.
#[derive(Clone, Debug)]
pub struct PackageLinearityOutput {
    /// Diagnostics in walk order.
    pub diags: Arc<[Diagnostic]>,
    /// Consuming uses of owned non-copyable bindings (C1).
    pub moves: Arc<[Move]>,
    /// Scope-exit deaths of live owned non-copyable bindings (C5).
    pub drops: Arc<[DropSite]>,
    /// The final use of every tracked binding.
    pub last_uses: Arc<[LastUse]>,
    /// The block-grain index of those four lists, built from the same walk:
    /// a definition's facts are one bucket, not a scan of the whole file.
    pub(crate) facts: Arc<LinearityFacts>,
}

/// The block-grain index of the package's affine pass.
#[derive(Clone, Debug, Default)]
pub(crate) struct LinearityFacts {
    /// Diagnostics, by index into [`PackageLinearityOutput::diags`].
    pub(crate) diags: BlockFacts,
    /// Moves, by index into [`PackageLinearityOutput::moves`].
    pub(crate) moves: BlockFacts,
    /// Drop sites, by index into [`PackageLinearityOutput::drops`].
    pub(crate) drops: BlockFacts,
    /// Final uses, by index into [`PackageLinearityOutput::last_uses`].
    pub(crate) last_uses: BlockFacts,
}

impl PartialEq for PackageLinearityOutput {
    fn eq(&self, other: &Self) -> bool {
        same_diags(&self.diags, &other.diags)
            && self.moves == other.moves
            && self.drops == other.drops
            && self.last_uses == other.last_uses
    }
}

impl Eq for PackageLinearityOutput {}

impl PackageLinearityOutput {
    /// The walk's rows as the checker's own pass result: the handoff an
    /// embedder that lowers from the queries passes to the lowerer's
    /// precomputed-facts seam, so a round that already paid the gate's
    /// walk never pays the lowerer's backstop re-walk.
    #[must_use]
    pub fn rows(&self) -> ds_check::Linearity {
        ds_check::Linearity {
            diags: self.diags.to_vec(),
            moves: self.moves.to_vec(),
            drops: self.drops.to_vec(),
            last_uses: self.last_uses.to_vec(),
        }
    }
}

/// Result of the [`Expanded`](crate::QueryKey::Expanded) query: the
/// package's parse memos' trees through the derive expansion (G1), shared
/// so every package-grain pass checks the same synthesized impls without
/// expanding per query.
///
/// Renewal identity: none — a recomputed expansion always counts as
/// changed. The trees implement no structural equality, so renewal
/// cannot prove the recompute equal; the conservative answer only costs
/// the recompute itself, since a parse value moves only when its source
/// text did, and each reader then renews through its own identity.
#[derive(Clone, Debug)]
pub struct ExpandedOutput {
    /// The expanded trees, position-aligned with the package's parseable
    /// files (a file whose parse failed contributes no tree). Shared
    /// handles one apiece, so the ambient's resident tree serves to
    /// every session and compute without a clone.
    pub asts: Arc<[Arc<Ast>]>,
    /// Derive-refusal diagnostics: unimplemented or unknown derive names,
    /// un-derivable parts, and the B9 equality refusal. Shared like the
    /// parse payload: memo hits clone the arc, not the diagnostics.
    pub diags: Arc<[Diagnostic]>,
}

impl PartialEq for ExpandedOutput {
    // Always changed; see the type docs for why the conservative answer
    // is the only sound one and what it costs.
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}

impl Eq for ExpandedOutput {}

/// Result of the [`Resolve`](crate::QueryKey::Resolve) query: one
/// package's collected namespace, per-file resolutions, reverse-edge
/// index, and the derive-refusal, collection, and resolution diagnostics
/// (G3) — the expansion's classes ride with the resolve half because the
/// expanded trees are its input.
#[derive(Clone, Debug)]
pub struct ResolveOutput {
    /// The package namespace every file of the package declared into.
    pub package: Arc<Package>,
    /// Each file's use table, aligning with the package's collected files
    /// (parseable ones only; a file whose parse failed contributes no
    /// declarations and no uses).
    pub resolutions: Arc<[Resolutions]>,
    /// The reverse edge of those tables: every use of each resolved
    /// target, package grain, built in the same compute the tables are
    /// (uses are fully materialized there, so a reference request is one
    /// lookup instead of a scan of the package). Shared like the parse
    /// payload: a memo hit clones the arc, not the index.
    pub uses: Arc<ds_check::UseIndex>,
    /// Derive-refusal, duplicate-definition, and unknown-name
    /// diagnostics. Shared like the parse payload: memo hits clone the
    /// arc, not the diagnostics.
    pub diags: Arc<[Diagnostic]>,
}

impl PartialEq for ResolveOutput {
    // The reverse-edge index is derived: it is a pure function of
    // `resolutions`, which the equality below compares, so comparing the
    // index too would walk the package twice.
    fn eq(&self, other: &Self) -> bool {
        self.package == other.package
            && self.resolutions == other.resolutions
            && self.diags == other.diags
    }
}

impl Eq for ResolveOutput {}

/// Result of the [`TypeOf`](crate::QueryKey::TypeOf) query: one
/// definition's typed story, sliced out of the package check (B2).
///
/// The slice is defined by primary spans, like
/// [`LinearityOutput`](crate::LinearityOutput): a diagnostic belongs to
/// the definition its primary label addresses. The types render through
/// the package universe because solved handles are meaningful only
/// inside it, and query values outlive the check that minted them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeOfOutput {
    /// The rendered solved type of every binding the definition's block
    /// holds: (definition name, type name). Shared like the diagnostics, so
    /// a memo hit clones the arc, not the pairs.
    pub types: Arc<[(String, String)]>,
    /// Declaration and body diagnostics addressing this definition.
    /// Shared like the parse payload: memo hits clone the arc, not the
    /// diagnostics.
    pub diags: Arc<[Diagnostic]>,
}

/// Result of the [`CheckedPackage`](crate::QueryKey::CheckedPackage) query:
/// the whole-package bidirectional check (B2), the substrate every
/// definition's [`TypeOfOutput`] and the
/// [`Analyzed`](crate::QueryKey::Analyzed) query's bundle both slice. The
/// payloads are shared, so a memo hit clones arcs, not the package.
#[derive(Clone)]
pub struct CheckedPackageOutput {
    /// The package namespace the slices key their definitions on: the same
    /// arc the `resolve` query carries.
    pub package: Arc<Package>,
    /// Declaration and body diagnostics of the whole package.
    pub diags: Arc<[Diagnostic]>,
    /// The rendered solved type of every top-level binding, keyed by
    /// definition id. Solved handles are meaningful only inside the
    /// universe that minted them, so the check renders them to names.
    pub type_names: Arc<rustc_hash::FxHashMap<ds_check::DefId, String>>,
    /// The block-grain index of the definitions and diagnostics, built from
    /// the same walk: a definition's story is one bucket, not a scan of
    /// every definition and diagnostic in the package.
    pub(crate) facts: Arc<CheckedPackageFacts>,
    /// The analyzer's bundle this value's own payloads were derived from:
    /// the one bidirectional run a round pays however many check-family
    /// queries it reads, handed to the `analyzed` query as its substrate.
    pub(crate) bundle: Analyzed,
}

impl CheckedPackageOutput {
    /// Derives the slice payloads off the freshly checked bundle: the
    /// check-family diagnostics split out of the merged list, the
    /// rendered type names, and the block-grain fact index over the
    /// bundle's own trees and namespace.
    pub(crate) fn new(bundle: Analyzed) -> Self {
        let blocks: Vec<(u32, Span)> = bundle
            .asts
            .iter()
            .flat_map(|ast| {
                ast.blocks
                    .iter()
                    .map(|block| (ast.file.as_u32(), block.span))
            })
            .collect();
        let defs = (0..bundle.package.len()).map(|raw| {
            let raw = u32::try_from(raw).expect("def count fits u32");
            let def = bundle.package.def(ds_check::DefId::from_raw(raw));
            (raw, def.span)
        });
        let diags: Arc<[Diagnostic]> = bundle.check_diags().to_vec().into();
        let facts = Arc::new(CheckedPackageFacts {
            defs: BlockFacts::build(blocks.iter().copied(), defs),
            diags: BlockFacts::build(blocks, primary_label_spans(&diags)),
        });
        Self {
            package: Arc::clone(&bundle.package),
            type_names: Arc::new(bundle.type_names()),
            diags,
            facts,
            bundle,
        }
    }
}

impl PartialEq for CheckedPackageOutput {
    // The fact index is derived from these three payloads: equal payloads
    // build equal buckets, so comparing them would walk the package twice.
    // The bundle is data, not identity: the engine stores the freshly
    // computed value even on a renewal, and the reader whose facts key on
    // spans (the analyzed query slicing it) anchors on the expansion
    // itself, so the substrate it slices is never staler than the last
    // compute.
    fn eq(&self, other: &Self) -> bool {
        self.package == other.package
            && self.diags == other.diags
            && self.type_names == other.type_names
    }
}

impl Eq for CheckedPackageOutput {}

impl fmt::Debug for CheckedPackageOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The bundle owns no renderable summary beyond counts, like the
        // diagnostics and definitions it carries.
        f.debug_struct("CheckedPackageOutput")
            .field("diags", &self.diags.len())
            .field("defs", &self.package.len())
            .field("type_names", &self.type_names.len())
            .finish_non_exhaustive()
    }
}

/// The block-grain index of one checked package.
#[derive(Clone, Debug, Default)]
pub(crate) struct CheckedPackageFacts {
    /// Definitions, by raw package index.
    pub(crate) defs: BlockFacts,
    /// Diagnostics, by index into [`CheckedPackageOutput::diags`].
    pub(crate) diags: BlockFacts,
}

/// Result of the [`PackageEffects`](crate::QueryKey::PackageEffects) query:
/// the whole-package effect rows (E1), the substrate the per-definition
/// rows and the platform check both read.
///
/// Equality compares the computed definitions (identity, name, row,
/// capability set): those are the only facts the pass's consumers observe,
/// so a recompute that moves none of them renews its readers.
#[derive(Clone)]
pub struct PackageEffectsOutput {
    /// The computed rows, in declaration order.
    pub effects: Arc<Effects>,
}

impl fmt::Debug for PackageEffectsOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Effects` keeps its index private; the computed definitions are
        // its whole observable surface.
        f.debug_struct("PackageEffectsOutput")
            .field("effects", &self.effects.defs())
            .finish()
    }
}

impl PartialEq for PackageEffectsOutput {
    fn eq(&self, other: &Self) -> bool {
        self.effects.defs() == other.effects.defs()
    }
}

impl Eq for PackageEffectsOutput {}

/// Result of the [`Analyzed`](crate::QueryKey::Analyzed) query: the full
/// check bundle (B2) — the derive-expanded trees, namespace, per-file
/// resolutions, declaration tables, and typed result — as shared handles
/// (the trees, namespace, resolutions, and resolution-time diagnostics
/// are the `Expanded` and `Resolve` memos' own arcs), so a memo hit
/// clones pointers, not the package, and a consumer holding the bundle
/// keeps the universe its `Ty`s were minted by alive.
#[derive(Clone)]
pub struct AnalyzedOutput {
    /// The bundle: everything the affine pass (C1) and the lowering pass
    /// (M3) read, the analyzer's own pass over the parse memos' trees.
    pub analyzed: Analyzed,
    /// Every top-level binding's id and rendered solved type, order-stable
    /// across the type map's iteration order. Solved handles are
    /// meaningless outside the universe that minted them, so renewal
    /// compares rendered names; the rows render once here at construction
    /// so the compare walks cached strings instead of re-rendering both
    /// sides through their universes.
    pub(crate) rendered_def_types: Arc<[(u32, String)]>,
}

impl AnalyzedOutput {
    /// Wraps a freshly checked bundle with its rendered type rows.
    #[must_use]
    pub(crate) fn new(analyzed: Analyzed) -> Self {
        let mut rows: Vec<_> = analyzed
            .checked
            .def_types
            .iter()
            .map(|(&def, &ty)| (def.index(), analyzed.decls.universe.render(ty)))
            .collect();
        rows.sort_unstable();
        Self {
            analyzed,
            rendered_def_types: rows.into(),
        }
    }
}

impl PartialEq for AnalyzedOutput {
    // Renewal identity: the facts consumers observe — the namespace, the
    // resolutions, the diagnostics, and every top-level type rendered
    // through the bundle's own universe (solved handles are meaningless
    // outside the universe that minted them), the same identity
    // `CheckedPackageOutput` carries. The expanded trees and the
    // span-keyed side tables (binder, expression, and call facts) are not
    // identity: they move only when the expansion value moves, and the
    // expansion recomputes only when some parse value moved — its source
    // text did — so a reader whose tables went stale re-derives through
    // its own dependency chain, never through this renewal alone.
    //
    // The package and the resolutions ride as the resolve memo's own
    // arcs, and `Arc` equality answers pointer-identical arcs without a
    // walk, so a bundle recomputed against a renewed resolve skips both
    // deep walks; the type rows are cached per value (see
    // [`AnalyzedOutput::new`]), so the compare never renders.
    fn eq(&self, other: &Self) -> bool {
        let (left, right) = (&self.analyzed, &other.analyzed);
        left.package == right.package
            && left.resolutions == right.resolutions
            && left.checked.diags == right.checked.diags
            && self.rendered_def_types == other.rendered_def_types
    }
}

impl Eq for AnalyzedOutput {}

impl fmt::Debug for AnalyzedOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The bundle owns no renderable summary beyond its counts; the
        // diagnostics and definitions are its whole observable surface.
        f.debug_struct("AnalyzedOutput")
            .field("diags", &self.analyzed.checked.diags.len())
            .field("defs", &self.analyzed.package.len())
            .field("def_type_rows", &self.rendered_def_types.len())
            .finish()
    }
}

/// Result of the [`PlatformCheck`](crate::QueryKey::PlatformCheck) query:
/// the platform-declaration and capability-subset diagnostics (H5, E1) of
/// one package, shared like the resolve payload so memo hits clone the
/// arc, not the diagnostics.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlatformCheckOutput {
    /// The package's platform-check diagnostics, source order not
    /// guaranteed (the caller sorts).
    pub diags: Arc<[Diagnostic]>,
}

/// Result of the [`PooledCheck`](crate::QueryKey::PooledCheck) query: the
/// pooled-body refusal diagnostics (E3) of one package, shared like the
/// resolve payload so memo hits clone the arc, not the diagnostics.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PooledCheckOutput {
    /// The package's pooled-body refusal diagnostics, source order not
    /// guaranteed (the caller sorts).
    pub diags: Arc<[Diagnostic]>,
}

/// Computed value of one query, stored in the memo table.
///
/// `non_exhaustive` because later milestones add their payload variants
/// when they land.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum QueryValue {
    /// Value of [`QueryKey::ParseFile`](crate::QueryKey::ParseFile).
    ParseFile(ParseOutput),
    /// Value of [`QueryKey::BlockIndex`](crate::QueryKey::BlockIndex).
    BlockIndex(BlockIndexOutput),
    /// Value of [`QueryKey::BlockSource`](crate::QueryKey::BlockSource): the
    /// block's source text.
    BlockSource(Arc<str>),
    /// Value of [`QueryKey::ScopeIndex`](crate::QueryKey::ScopeIndex).
    ScopeIndex(ScopeIndexOutput),
    /// Value of [`QueryKey::Expanded`](crate::QueryKey::Expanded).
    Expanded(ExpandedOutput),
    /// Value of [`QueryKey::Resolve`](crate::QueryKey::Resolve).
    Resolve(ResolveOutput),
    /// Value of [`QueryKey::TypeOf`](crate::QueryKey::TypeOf).
    TypeOf(TypeOfOutput),
    /// Value of [`QueryKey::LinearityCheck`](crate::QueryKey::LinearityCheck).
    LinearityCheck(LinearityOutput),
    /// Value of [`QueryKey::EffectsOf`](crate::QueryKey::EffectsOf): the
    /// definition's effect row.
    EffectRow(EffectRow),
    /// Value of [`QueryKey::CheckedPackage`](crate::QueryKey::CheckedPackage).
    CheckedPackage(CheckedPackageOutput),
    /// Value of [`QueryKey::Analyzed`](crate::QueryKey::Analyzed).
    Analyzed(AnalyzedOutput),
    /// Value of
    /// [`QueryKey::PackageLinearity`](crate::QueryKey::PackageLinearity).
    PackageLinearity(PackageLinearityOutput),
    /// Value of [`QueryKey::PackageEffects`](crate::QueryKey::PackageEffects).
    PackageEffects(PackageEffectsOutput),
    /// Value of [`QueryKey::PlatformCheck`](crate::QueryKey::PlatformCheck):
    /// the package's platform-check diagnostics, shared like the resolve
    /// payload.
    PlatformCheck(PlatformCheckOutput),
    /// Value of [`QueryKey::PooledCheck`](crate::QueryKey::PooledCheck):
    /// the package's pooled-body refusal diagnostics, shared like the
    /// resolve payload.
    PooledCheck(PooledCheckOutput),
}

#[cfg(test)]
mod tests {
    use ds_diag::{Diagnostic, ErrorCode, FileId, Span};

    use super::same_diags;

    fn span(file: u32, start: u32, end: u32) -> Span {
        Span::new(FileId::new(file), start, end)
    }

    fn moved(file: u32) -> Diagnostic {
        Diagnostic::error(ErrorCode::MovedThenUsed, "used after move".to_owned())
            .with_primary(span(file, 10, 11), "used again")
            .with_secondary(span(file, 0, 1), "moved here")
    }

    /// A diagnostic's byte offsets alone are not its identity: the same
    /// range in another file is another fact, so a memo comparing the old
    /// and new lists must renew instead of serving the stale label.
    #[test]
    fn a_same_range_diagnostic_in_another_file_differs() {
        assert!(same_diags(&[moved(0)], &[moved(0)]));
        assert!(!same_diags(&[moved(0)], &[moved(1)]));
    }

    /// The checked package and the analyzed query share one substrate: a
    /// round that reads both runs the bidirectional check once, and the
    /// bundle the analyzed query hands out is the very value the checked
    /// package derived its own payloads from — not a second check's
    /// equal-looking twin, whose universe would mint unrelated handles.
    #[test]
    fn the_analyzed_query_slices_the_checked_package_s_bundle() {
        let mut db = crate::Db::new();
        let _ = db.add_file("plain.ds", "tag int\n##\nconst answer = 42\n");
        let checked = db
            .get::<crate::query::CheckedPackage>(crate::PackageId::new(0))
            .unwrap();
        let analyzed = db
            .get::<crate::query::Analyzed>(crate::PackageId::new(0))
            .unwrap();
        assert!(std::rc::Rc::ptr_eq(
            &checked.bundle.decls,
            &analyzed.analyzed.decls
        ));
        assert!(std::sync::Arc::ptr_eq(
            &checked.bundle.checked,
            &analyzed.analyzed.checked
        ));
        assert!(std::sync::Arc::ptr_eq(
            &checked.bundle.asts,
            &analyzed.analyzed.asts
        ));
    }

    /// A primary/secondary swap moves the fact between blocks (the slice
    /// keys on primary spans), so the lists differ even at identical
    /// ranges in the same file.
    #[test]
    fn a_primary_secondary_swap_differs() {
        let swapped = Diagnostic::error(ErrorCode::MovedThenUsed, "used after move".to_owned())
            .with_primary(span(0, 0, 1), "moved here")
            .with_secondary(span(0, 10, 11), "used again");
        assert!(!same_diags(&[moved(0)], &[swapped]));
    }
}
