//! Keys and dependency edges of the query graph.

use ds_diag::FileId;

use crate::error::QueryError;

/// Placeholder identifier of one definition, to be replaced by the real id
/// type when the definition table lands. Carried by the [`QueryKey`]
/// def-keyed variants in two spellings: the M2 `type_of` and
/// `linearity_check` computes read it as the packed pair
/// `(file << 8) | block` — a definition is its `##` block until real
/// identity arrives, and the pair must be built by [`DefId::from_block`],
/// which rejects what the 8-bit block field cannot hold (a packed block
/// 256 would silently alias the next file's block 0) — while `effects_of`
/// reads it as the definition's package index.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct DefId(u32);

impl DefId {
    /// Wraps a raw identifier.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Packs one block's `(file << 8) | block` pair: the identity the
    /// `type_of` and `linearity_check` keys carry. The block field is 8
    /// bits and the file field 24, so a pair past either bound is rejected
    /// here rather than packed into the next file's block 0.
    ///
    /// # Errors
    ///
    /// [`QueryError::DefIdOverflow`] when `block.index` exceeds `0xff` or
    /// the file id exceeds `0xff_ffff`.
    pub const fn from_block(block: BlockId) -> Result<Self, QueryError> {
        if block.index > 0xff || block.file.as_u32() > 0xff_ffff {
            return Err(QueryError::DefIdOverflow(block));
        }
        Ok(Self((block.file.as_u32() << 8) | block.index))
    }

    /// Returns the raw identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Placeholder identifier of one package: the key grain of the `resolve`
/// query (a later milestone) and the package-grain invalidation input (M7).
/// Replaced by the real manifest identity when `ds.pkg` loading lands.
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

/// Identifies one `##`-separated block of one file (G3). A file without
/// separators is one block.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlockId {
    /// The file the block lies in.
    pub file: FileId,
    /// 0-based block position within the file, in source order.
    pub index: u32,
}

/// Key of one memoized compiler query (H6 item 1).
///
/// The enum is `non_exhaustive`: `Lower` is the remaining placeholder,
/// documented as such; asking the engine to compute it before its
/// milestone yields [`QueryError::Unavailable`](crate::QueryError::Unavailable).
/// Every other variant is a real M2 query: the package-grain passes
/// (`Expanded`, `Resolve`, `CheckedPackage`, `Analyzed`,
/// `PackageLinearity`, `PackageEffects`, `PlatformCheck`) and the
/// per-definition slices of them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum QueryKey {
    /// Parse one file's source into the syntax tree.
    ParseFile(FileId),
    /// Map a file's byte spans to its `##` blocks.
    BlockIndex(FileId),
    /// The source text of one block; the seam downstream block-keyed queries
    /// depend on so file edits invalidate at block grain.
    BlockSource(BlockId),
    /// One file's local scope frames: the value scopes the resolver walks
    /// (function and lambda parameters, block bindings, match-arm and
    /// for-pattern binders), each with its region and the offset each
    /// binding turns visible from — the substrate the position slice for
    /// completion reads, so the walk runs once per file instead of once
    /// per request. Package-level declarations need no frames: they are
    /// the [`QueryKey::Resolve`] value's own table.
    ScopeIndex(FileId),
    /// The derive-expanded trees of one package (G1): every parseable
    /// file's tree through the ds-check expansion, the substrate the
    /// package-grain passes below compute over — `Resolve`, the check
    /// family, the effects and platform passes, and `Analyzed`. The
    /// `PackageId` is the package grain, like [`QueryKey::Resolve`].
    Expanded(PackageId),
    /// Name resolution for one package (G3): collection and resolution
    /// over the [`QueryKey::Expanded`] value's derive-expanded trees, so
    /// a derive-synthesized member resolves like a hand-written one. The
    /// `PackageId` is the package grain; M2 has one package — every
    /// registered file — so the id is always 0 until M7's package
    /// loading mints real package identity.
    Resolve(PackageId),
    /// The type story of one definition (M2): a slice of the package's
    /// [`QueryKey::CheckedPackage`] value into the definition's block — the
    /// declaration and body diagnostics whose primary labels address it,
    /// plus the solved type of each binding it holds. `DefId` packs
    /// `(file << 8) | block` like [`QueryKey::LinearityCheck`].
    TypeOf(DefId),
    /// Effect row of one definition (E1): the definition's row out of the
    /// package's [`QueryKey::PackageEffects`] value.
    EffectsOf(DefId),
    /// The linearity check of one definition (M2): a slice of its
    /// package's [`QueryKey::PackageLinearity`] facts into the
    /// definition's block (C1, C5), with the slice taken by the same
    /// `DefId` packing as [`QueryKey::TypeOf`].
    LinearityCheck(DefId),
    /// The platform check of one package (H5, E1): the package's
    /// declarations collected against its platform declaration, the rows of
    /// [`QueryKey::PackageEffects`] verified inside what the platform
    /// provides. The `PackageId` is the package grain, like
    /// [`QueryKey::Resolve`].
    PlatformCheck(PackageId),
    /// The pooled-body check of one package (E3): every pooled call's
    /// (the @std `par_map`/`par_reduce` library fns, recognized by
    /// resolved member identity) callable hunted for the worker-diverging
    /// members over the [`QueryKey::Analyzed`] value's own bundle. The
    /// `PackageId` is the package grain, like [`QueryKey::Resolve`].
    PooledCheck(PackageId),
    /// The whole-package bidirectional check (B2): collection and
    /// resolution, then the declaration and body pass over the
    /// [`QueryKey::Expanded`] value's trees. The substrate every
    /// [`QueryKey::TypeOf`] slices, so the pass runs once per package
    /// instead of once per definition. The `PackageId` is the package
    /// grain, like [`QueryKey::Resolve`].
    CheckedPackage(PackageId),
    /// The analyzed package (B2): the full check bundle — the
    /// derive-expanded trees, namespace, per-file resolutions,
    /// declaration tables, and typed result — the analyzer's check half
    /// over the [`QueryKey::Expanded`] and [`QueryKey::Resolve`] values,
    /// so it never re-expands or re-resolves the queries' inputs. The
    /// seam the run path lowers from, over the expansion's own trees.
    /// The `PackageId` is the package grain, like
    /// [`QueryKey::Resolve`].
    Analyzed(PackageId),
    /// The M2 affine pass (C1, C5) over the package's derive-expanded trees
    /// through the [`QueryKey::Analyzed`] bundle — the same trees the
    /// check family checked — so a derive-synthesized impl body answers
    /// linearity facts like a hand-written one. The substrate every
    /// [`QueryKey::LinearityCheck`] slices, so the pass runs once per
    /// package instead of once per definition. The `PackageId` is the
    /// package grain, like [`QueryKey::Resolve`].
    PackageLinearity(PackageId),
    /// The effect rows (E1) of one package: every value-producing
    /// definition's row over the registered files. The substrate
    /// [`QueryKey::EffectsOf`] and [`QueryKey::PlatformCheck`] both read,
    /// so the pass runs once per package.
    PackageEffects(PackageId),
    /// Placeholder: lowering of one block (M3). Until it lands, consumers
    /// lower from the [`QueryKey::Analyzed`] value's own trees
    /// (`ds_lower::lower` over `Analyzed::asts`): the module's
    /// consumer-specific name and source hash and its consumed-by-install
    /// ownership have no place in the memo table.
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
            Self::ScopeIndex(_) => QueryKind::ScopeIndex,
            Self::Expanded(_) => QueryKind::Expanded,
            Self::Resolve(_) => QueryKind::Resolve,
            Self::TypeOf(_) => QueryKind::TypeOf,
            Self::EffectsOf(_) => QueryKind::EffectsOf,
            Self::LinearityCheck(_) => QueryKind::LinearityCheck,
            Self::PlatformCheck(_) => QueryKind::PlatformCheck,
            Self::PooledCheck(_) => QueryKind::PooledCheck,
            Self::CheckedPackage(_) => QueryKind::CheckedPackage,
            Self::Analyzed(_) => QueryKind::Analyzed,
            Self::PackageLinearity(_) => QueryKind::PackageLinearity,
            Self::PackageEffects(_) => QueryKind::PackageEffects,
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
    /// Kind of [`QueryKey::ScopeIndex`].
    ScopeIndex,
    /// Kind of [`QueryKey::Expanded`].
    Expanded,
    /// Kind of [`QueryKey::Resolve`].
    Resolve,
    /// Kind of [`QueryKey::TypeOf`].
    TypeOf,
    /// Kind of [`QueryKey::EffectsOf`].
    EffectsOf,
    /// Kind of [`QueryKey::LinearityCheck`].
    LinearityCheck,
    /// Kind of [`QueryKey::PlatformCheck`].
    PlatformCheck,
    /// Kind of [`QueryKey::PooledCheck`].
    PooledCheck,
    /// Kind of [`QueryKey::CheckedPackage`].
    CheckedPackage,
    /// Kind of [`QueryKey::Analyzed`].
    Analyzed,
    /// Kind of [`QueryKey::PackageLinearity`].
    PackageLinearity,
    /// Kind of [`QueryKey::PackageEffects`].
    PackageEffects,
    /// Kind of [`QueryKey::Lower`].
    Lower,
}

/// Key of one engine input.
///
/// M7 seam: package-grain invalidation adds a `PackageConfig(PackageId)`
/// variant here plus package-keyed queries reading it. The enum is
/// `non_exhaustive` so that addition breaks no consumer; the revision and
/// dependency machinery below is already per-input and needs no change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum InputKey {
    /// The full source text of one file.
    FileContent(FileId),
    /// The registered file set itself: the package-grain queries read its
    /// size through [`Ctx::file_count`](crate::Ctx::file_count), so
    /// registering a file invalidates them like any input change.
    FileSet,
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
