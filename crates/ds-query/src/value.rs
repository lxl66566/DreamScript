//! Values produced by queries.

use std::{fmt, sync::Arc};

use ds_check::{
    EffectRow, Effects,
    linearity::{DropSite, LastUse, Move},
    resolve::{Package, Resolutions},
};
use ds_diag::{Diagnostic, Span};
use ds_parse::Ast;

use crate::{facts::BlockFacts, key::BlockId};

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

/// Compare two diagnostic lists by the facts their identity rests on:
/// code plus span set. Labels and notes are derived, so equal codes and
/// spans mean equal values.
fn same_diags(left: &[Diagnostic], right: &[Diagnostic]) -> bool {
    fn sig(diags: &[Diagnostic]) -> Vec<(String, Vec<(u32, u32)>)> {
        diags
            .iter()
            .map(|d| {
                (
                    d.code.to_string(),
                    d.labels
                        .iter()
                        .map(|l| (l.span.start, l.span.end))
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

/// Result of the [`FileLinearity`](crate::QueryKey::FileLinearity) query:
/// the M2 affine pass (C1, C5) over one file, the substrate every
/// definition's [`LinearityOutput`] slices. The fact lists are shared, so
/// a memo hit clones arcs, not the facts.
#[derive(Clone, Debug)]
pub struct FileLinearityOutput {
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

/// The block-grain index of one file's affine pass.
#[derive(Clone, Debug, Default)]
pub(crate) struct LinearityFacts {
    /// Diagnostics, by index into [`FileLinearityOutput::diags`].
    pub(crate) diags: BlockFacts,
    /// Moves, by index into [`FileLinearityOutput::moves`].
    pub(crate) moves: BlockFacts,
    /// Drop sites, by index into [`FileLinearityOutput::drops`].
    pub(crate) drops: BlockFacts,
    /// Final uses, by index into [`FileLinearityOutput::last_uses`].
    pub(crate) last_uses: BlockFacts,
}

impl PartialEq for FileLinearityOutput {
    fn eq(&self, other: &Self) -> bool {
        same_diags(&self.diags, &other.diags)
            && self.moves == other.moves
            && self.drops == other.drops
            && self.last_uses == other.last_uses
    }
}

impl Eq for FileLinearityOutput {}

/// Result of the [`Resolve`](crate::QueryKey::Resolve) query: one
/// package's collected namespace, per-file resolutions, and the
/// collection and resolution diagnostics (G3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolveOutput {
    /// The package namespace every file of the package declared into.
    pub package: Arc<Package>,
    /// Each file's use table, aligning with the package's collected files
    /// (parseable ones only; a file whose parse failed contributes no
    /// declarations and no uses).
    pub resolutions: Arc<[Resolutions]>,
    /// Duplicate-definition and unknown-name diagnostics. Shared like the
    /// parse payload: memo hits clone the arc, not the diagnostics.
    pub diags: Arc<[Diagnostic]>,
}

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
/// definition's [`TypeOfOutput`] slices. The payloads are shared, so a
/// memo hit clones arcs, not the package.
#[derive(Clone, Debug)]
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
}

impl PartialEq for CheckedPackageOutput {
    // The fact index is derived from these three payloads: equal payloads
    // build equal buckets, so comparing them would walk the package twice.
    fn eq(&self, other: &Self) -> bool {
        self.package == other.package
            && self.diags == other.diags
            && self.type_names == other.type_names
    }
}

impl Eq for CheckedPackageOutput {}

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
    /// Value of [`QueryKey::FileLinearity`](crate::QueryKey::FileLinearity).
    FileLinearity(FileLinearityOutput),
    /// Value of [`QueryKey::PackageEffects`](crate::QueryKey::PackageEffects).
    PackageEffects(PackageEffectsOutput),
    /// Value of [`QueryKey::PlatformCheck`](crate::QueryKey::PlatformCheck):
    /// the package's platform-check diagnostics, shared like the resolve
    /// payload.
    PlatformCheck(PlatformCheckOutput),
}
