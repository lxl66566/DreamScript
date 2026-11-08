//! Values produced by queries.

use std::sync::Arc;

use ds_diag::Span;

use crate::key::BlockId;

/// Result of the [`ParseFile`](crate::QueryKey::ParseFile) query.
///
/// Placeholder payload: until `ds-parse` lands it echoes the file source.
/// The follow-up replaces it with the green syntax tree (AST plus trivia,
/// with spans), which is what downstream queries will inspect.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseOutput {
    /// The file source the tree was parsed from.
    pub source: Arc<str>,
}

/// One block of a file: identity plus the byte span of its source text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlockInfo {
    /// The block's identity.
    pub id: BlockId,
    /// Byte range of the block's source text within the file. Separator
    /// lines belong to no block.
    pub span: Span,
}

/// Result of the [`BlockIndex`](crate::QueryKey::BlockIndex) query: the
/// file's blocks in source order, mapping byte spans to blocks.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockIndexOutput {
    /// Blocks in source order; every file has at least one [G3].
    pub blocks: Box<[BlockInfo]>,
}

/// Computed value of one query, stored in the memo table.
///
/// [`non_exhaustive`] because the M2+ queries add their payload variants
/// when their milestones land.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum QueryValue {
    /// Value of [`QueryKey::ParseFile`].
    ParseFile(ParseOutput),
    /// Value of [`QueryKey::BlockIndex`].
    BlockIndex(BlockIndexOutput),
    /// Value of [`QueryKey::BlockSource`]: the block's source text.
    BlockSource(Arc<str>),
}
