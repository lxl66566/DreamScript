//! Keys and dependency edges of the query graph.

use ds_diag::FileId;

/// Placeholder identifier of one definition, to be replaced by the real id
/// type when the checker lands (M2). Carried today only by the placeholder
/// [`QueryKey`] variants so consumers can handle the key space ahead of time.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct DefId(u32);

impl DefId {
    /// Wraps a raw identifier.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Placeholder identifier of one package: the key grain of the `resolve`
/// query (M2) and the package-grain invalidation input (M7). Replaced by the
/// real manifest identity when `ds.pkg` loading lands.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PackageId(u32);

impl PackageId {
    /// Wraps a raw identifier.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Identifies one `///|`-separated block of one file [G3]. A file without
/// separators is one block.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlockId {
    /// The file the block lies in.
    pub file: FileId,
    /// 0-based block position within the file, in source order.
    pub index: u32,
}

/// Key of one memoized compiler query [H6 item 1].
///
/// The enum is [`non_exhaustive`]: variants for the M2+ queries
/// (`Resolve`, `TypeOf`, `EffectsOf`, `LinearityCheck`, `Lower`) are
/// placeholders documented as such; asking the engine to compute one before
/// its milestone yields [`QueryError::Unavailable`](crate::QueryError::Unavailable).
/// Their key payloads are placeholder ids swapped for the real checker ids
/// when those crates land.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum QueryKey {
    /// Parse one file's source into the syntax tree.
    ParseFile(FileId),
    /// Map a file's byte spans to its `///|` blocks.
    BlockIndex(FileId),
    /// The source text of one block; the seam downstream block-keyed queries
    /// depend on so file edits invalidate at block grain.
    BlockSource(BlockId),
    /// Placeholder: name resolution for one package (M2).
    Resolve(PackageId),
    /// Placeholder: type of one definition (M2).
    TypeOf(DefId),
    /// Placeholder: effect row of one definition (M2).
    EffectsOf(DefId),
    /// Placeholder: linearity check of one definition (M2).
    LinearityCheck(DefId),
    /// Placeholder: lowering of one block (M3).
    Lower(BlockId),
}

impl QueryKey {
    /// The kind discriminant, used to dispatch compute functions.
    #[must_use]
    pub const fn kind(&self) -> QueryKind {
        match self {
            Self::ParseFile(_) => QueryKind::ParseFile,
            Self::BlockIndex(_) => QueryKind::BlockIndex,
            Self::BlockSource(_) => QueryKind::BlockSource,
            Self::Resolve(_) => QueryKind::Resolve,
            Self::TypeOf(_) => QueryKind::TypeOf,
            Self::EffectsOf(_) => QueryKind::EffectsOf,
            Self::LinearityCheck(_) => QueryKind::LinearityCheck,
            Self::Lower(_) => QueryKind::Lower,
        }
    }
}

/// Discriminant of a [`QueryKey`] variant, used as the registration handle
/// for compute functions ([`Db::set_compute`](crate::Db::set_compute)).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum QueryKind {
    /// Kind of [`QueryKey::ParseFile`].
    ParseFile,
    /// Kind of [`QueryKey::BlockIndex`].
    BlockIndex,
    /// Kind of [`QueryKey::BlockSource`].
    BlockSource,
    /// Kind of [`QueryKey::Resolve`].
    Resolve,
    /// Kind of [`QueryKey::TypeOf`].
    TypeOf,
    /// Kind of [`QueryKey::EffectsOf`].
    EffectsOf,
    /// Kind of [`QueryKey::LinearityCheck`].
    LinearityCheck,
    /// Kind of [`QueryKey::Lower`].
    Lower,
}

/// Key of one engine input.
///
/// M7 seam: package-grain invalidation adds a `PackageConfig(PackageId)`
/// variant here plus package-keyed queries reading it. The enum is
/// [`non_exhaustive`] so that addition breaks no consumer; the revision and
/// dependency machinery below is already per-input and needs no change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum InputKey {
    /// The full source text of one file.
    FileContent(FileId),
}

/// One edge in the dependency graph: a query read an input or another
/// query's result while computing, so it must be re-derived when that
/// dependency changes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Dependency {
    /// The computing query read an engine input.
    Input(InputKey),
    /// The computing query read another query's memoized result.
    Query(QueryKey),
}
