//! Memoized query engine for DreamScript: the compiler as a query library
//! (H6 item 1). Every consumer — CLI, LSP, REPL, hot reload, formatter —
//! reads compiler data through [`Db`] and nowhere else.
//!
//! # Model
//!
//! Inputs are typed ([`InputKey`], today file contents and the registered
//! file set; package config at M7) and stamped with a global [`Revision`]
//! when they change. Queries are
//! keyed by the [`non_exhaustive`] [`QueryKey`] enum and
//! memoized: each memo stores its value, the dependency edges recorded while
//! computing (reading an input or another query creates an edge), and the
//! revisions it was verified and last changed at.
//!
//! When an input changes — a file edit through [`Db::set_file_content`] or
//! a registration through [`Db::add_file`] — the revision bumps and the
//! reverse dependency edges mark every transitive dependent memo dirty.
//! Dirty memos are re-examined lazily on the next read: a memo
//! is deep-verified dependency by dependency and recomputed only if some
//! dependency's change-revision moved past the memo's verify-revision. A
//! recomputed value equal to the old one renews the memo — its
//! change-revision stays put, so its own readers stay valid. That renewal is
//! what turns file-grain marking into block-grain invalidation.
//!
//! # Block grain and the ds-parse seam
//!
//! [`QueryKey::ParseFile`] runs `ds-parse` and returns tree plus diagnostics
//! as one value — syntax errors are data, not engine errors, so a broken
//! file does not poison dependents. [`QueryKey::BlockIndex`] maps the tree's
//! block spans to block ids (degrading to a `##` text scan when the parse
//! failed). [`QueryKey::BlockSource`] is the renewal seam: it returns one
//! block's source text, and downstream block-keyed queries (the M2+
//! resolve/type_of family) depend on it instead of on the file — so editing
//! block 3 of 7 leaves the other six blocks' dependents valid: their
//! dependency's value — the block text — recomputes to an equal value and
//! renews. See the `queries` module docs for the exact dependency shape.
//!
//! # Package grain and the per-definition slices
//!
//! A checker pass is memoized at its own grain, and the per-definition
//! queries slice its value instead of re-running it: `CheckedPackage` runs
//! the bidirectional check (B2) once per package, `PackageLinearity` the
//! affine pass (C1, C5) once per package over the expansion's own trees,
//! and `PackageEffects` the effects pass (E1) once per package. `TypeOf`,
//! `LinearityCheck`, and `EffectsOf` therefore depend on one package-grain
//! key each (plus the block index for the definition's block), so a
//! definition's cost is one bucket lookup in the pass's block-grain fact
//! index plus that block's facts, and
//! deep verification walks a handful of keys however many definitions the
//! package holds. The memoed payloads are shared arcs, so answering a read
//! from a memo clones pointers. `PlatformCheck` reads the same effects
//! memo, so the effects pass runs once for both. `Expanded` runs the derive
//! expansion (G1) once over the parse set, and `Resolve` with the check
//! family computes over its trees, so a derived member's call resolves and
//! checks at the ordinary grain — `ds check` and a REPL session's check
//! no longer refuse it. `Analyzed` slices the checked package's own
//! bundle — the one bidirectional run that value paid — so a round that
//! reads both check-family queries (an editor's diagnostics plus a hover)
//! checks the package once; the bundle carries the expansion's own trees,
//! so the run path lowers from it instead of re-expanding or re-resolving
//! the queries' inputs.
//!
//! # Cycle policy
//!
//! Two distinct concepts:
//!
//! * Query-level cycles — a query transitively reading its own in-flight result — mean
//!   non-termination and are an engine error, detected and reported as [`QueryError::Cycle`] with
//!   the full path.
//! * Intra-package definition cycles are legal language semantics (G3), resolved by whole-package
//!   fixpoint at M2. They are the queries' values' business, not the engine's; nothing here
//!   prevents them.
//!
//! # Package grain (M7 seam)
//!
//! Package-grain invalidation arrives with package loading: a
//! `PackageConfig` input in [`InputKey`] plus package-keyed queries reading
//! it. The revision and dependency machinery is already per-input; no
//! engine change is needed beyond the new variant.
//!
//! # Example
//!
//! ```
//! use ds_query::{BlockIndex, BlockSource, Db, ParseFile};
//!
//! let mut db = Db::new();
//! let file = db.add_file("main.ds", "const a = 1\n##\nconst b = 2\n");
//!
//! let parse = db.get::<ParseFile>(file).unwrap();
//! assert!(parse.ast.is_some());
//!
//! let blocks = db.get::<BlockIndex>(file).unwrap();
//! assert_eq!(blocks.blocks.len(), 2);
//! let second = db.get::<BlockSource>(blocks.blocks[1].id).unwrap();
//! // The newline ending the separator line opens the following block,
//! // whose extent ends at its last content byte.
//! assert_eq!(&*second, "\nconst b = 2");
//!
//! // Editing the first block re-derives the file's parse and index, but the
//! // second block's source renews and its readers never recompute.
//! db.set_file_content(file, "const a = 3\n##\nconst b = 2\n")
//!     .unwrap();
//! assert_eq!(
//!     &*db.get::<BlockSource>(blocks.blocks[1].id).unwrap(),
//!     "\nconst b = 2"
//! );
//! ```
//!
//! [`non_exhaustive`]: https://doc.rust-lang.org/reference/attributes/type_system.html#the-non_exhaustive_attribute

#![warn(missing_docs)]

mod db;
mod error;
mod facts;
mod key;
mod queries;
mod query;
mod value;

pub use db::{ComputeFn, Ctx, Db, Revision};
pub use ds_check::{Effect, EffectRow};
pub use error::QueryError;
pub use key::{BlockId, DefId, Dependency, InputKey, PackageId, QueryKey, QueryKind};
pub use query::{
    Analyzed, BlockIndex, BlockSource, CheckedPackage, EffectsOf, Expanded, LinearityCheck,
    PackageEffects, PackageLinearity, ParseFile, PlatformCheck, PooledCheck, Query, Resolve,
    ScopeIndex, TypeOf,
};
pub use value::{
    AnalyzedOutput, BlockIndexOutput, BlockInfo, CheckedPackageOutput, ExpandedOutput,
    LinearityOutput, PackageEffectsOutput, PackageLinearityOutput, ParseOutput,
    PlatformCheckOutput, PooledCheckOutput, QueryValue, ResolveOutput, ScopeIndexOutput,
    TypeOfOutput,
};
