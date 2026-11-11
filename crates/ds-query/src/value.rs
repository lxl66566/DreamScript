//! Values produced by queries.

use std::sync::Arc;

use ds_check::{
    EffectRow,
    linearity::{DropSite, LastUse, Move},
    resolve::{Package, Resolutions},
};
use ds_diag::{Diagnostic, Span};
use ds_parse::Ast;

use crate::key::BlockId;

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
    // for the purity contract this leans on.
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
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
    /// Blocks in source order; every file has at least one (G3).
    pub blocks: Box<[BlockInfo]>,
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
    pub moves: Box<[Move]>,
    /// Scope-exit deaths in this definition where deinit must run.
    pub drops: Box<[DropSite]>,
    /// Final uses in this definition, borrowed or consuming.
    pub last_uses: Box<[LastUse]>,
}

impl PartialEq for LinearityOutput {
    fn eq(&self, other: &Self) -> bool {
        // Diagnostic equality is code plus span set; labels and notes are
        // derived, so two values with equal code and spans are equal.
        fn sig(out: &LinearityOutput) -> Vec<(String, Vec<(u32, u32)>)> {
            out.diags
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
        sig(self) == sig(other)
            && self.moves == other.moves
            && self.drops == other.drops
            && self.last_uses == other.last_uses
    }
}

impl Eq for LinearityOutput {}

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
    /// holds: (definition name, type name).
    pub types: Box<[(String, String)]>,
    /// Declaration and body diagnostics addressing this definition.
    /// Shared like the parse payload: memo hits clone the arc, not the
    /// diagnostics.
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
}
