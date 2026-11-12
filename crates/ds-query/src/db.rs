//! The database: inputs, memo table, dependency edges, and the
//! verify-or-recompute cycle that drives them.

use std::{collections::VecDeque, sync::Arc};

use ds_diag::{FileId, Files};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    error::QueryError,
    key::{Dependency, InputKey, QueryKey, QueryKind},
    query::Query,
    value::QueryValue,
};

/// Global revision counter: bumped once per actual input change and never
/// decreased. Memo timestamps refer to it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Revision(u64);

impl Revision {
    /// The raw counter value.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Compute function of one query kind: a pure function of the context (which
/// exposes inputs and other queries) and its key. Computes run synchronously
/// on the caller's thread; queries that need a bigger stack (the parse
/// grammar runs its recursive descent on a dedicated 8 MiB stack thread
/// internally) arrange it themselves.
pub type ComputeFn = fn(&mut Ctx<'_>, QueryKey) -> Result<QueryValue, QueryError>;

/// One memoized query result. `dirty` means some dependency may have changed
/// since `verified_at`; a dirty memo is deep-verified on the next read and
/// recomputed only if a dependency's `changed_at` moved past `verified_at`.
#[derive(Clone, Debug)]
struct Memo {
    value: QueryValue,
    deps: Vec<Dependency>,
    verified_at: Revision,
    changed_at: Revision,
    dirty: bool,
}

/// One active-stack entry: a key being computed (dependencies accumulate in
/// `deps`) or being deep-verified (`deps` stays empty).
struct Frame {
    key: QueryKey,
    deps: Deps,
}

/// How many recorded dependencies a frame dedupes by linear scan before it
/// upgrades to a hash set: a package-grain compute records one dependency
/// per registered file, so a linear `contains` over every recorded read went
/// quadratic in the file count.
const SEEN_LINEAR_MAX: usize = 16;

/// The dependency set of one computing frame: recorded once each, in read
/// order. Duplicate suppression is a linear scan while the set is small and
/// a hash set past [`SEEN_LINEAR_MAX`].
#[derive(Debug, Default)]
struct Deps {
    recorded: Vec<Dependency>,
    index: Option<FxHashSet<Dependency>>,
}

impl Deps {
    /// Records `dep` unless an equal one is already recorded.
    fn insert(&mut self, dep: Dependency) {
        if let Some(index) = &mut self.index {
            if index.insert(dep) {
                self.recorded.push(dep);
            }
            return;
        }
        if self.recorded.contains(&dep) {
            return;
        }
        if self.recorded.len() >= SEEN_LINEAR_MAX {
            let index = self.index.get_or_insert_with(FxHashSet::default);
            index.extend(self.recorded.iter().copied());
            index.insert(dep);
        }
        self.recorded.push(dep);
    }
}

/// The query database: the only owner of inputs and memos, and the only path
/// to compiler data for every consumer (CLI, LSP, REPL, hot reload,
/// formatter) (H6 item 1).
///
/// File inputs live in a [`Files`] registry whose ids are the engine's
/// `FileId`s, so every span produced by a query addresses this registry
/// (retrieve it with [`Db::files`] to render diagnostics). Editing a file
/// swaps that file's content in place via [`Files::set_source`]; ids stay
/// stable and other files are untouched, so edits stay cheap as the LSP
/// hammers this path.
///
/// Determinism: computes are pure functions of inputs, so the same inputs
/// yield the same results; whether a value comes from a memo hit or a fresh
/// compute is never observable in returned values.
#[derive(Clone, Debug, Default)]
pub struct Db {
    revision: Revision,
    files: Files,
    /// Per-file revision of the last content change, indexed by file id.
    changed_at: Vec<Revision>,
    /// Revision of the last change to the registered file set (a file
    /// registration), read as the [`InputKey::FileSet`] input.
    file_set_changed_at: Revision,
    memos: FxHashMap<QueryKey, Memo>,
    /// Reverse dependency edges: dependency -> keys whose memos read it. A
    /// dependency's reader list is short (every pass is memoized at its own
    /// grain, so a parse is read by a handful of keys), so a flat vector
    /// beats a set; a duplicate reader is harmless because invalidation and
    /// the renewal marking are idempotent.
    reverse: FxHashMap<Dependency, Vec<QueryKey>>,
    computes: FxHashMap<QueryKind, ComputeFn>,
    /// Per-key compute counters. Written in debug builds only: they serve
    /// test introspection, so a release build does not pay a map write per
    /// compute and reads zero from an empty table.
    compute_counts: FxHashMap<QueryKey, usize>,
    /// The declared effect rows the effects pass reads (E1): the ambient
    /// stdlib table. Fixed for the database's life, so no memo can go
    /// stale against it.
    declared_rows: ds_check::DeclaredRows,
    /// The world functions' capability attribution the effects pass reads
    /// beside the rows (H5): the ambient stdlib table, licensing the
    /// package's world usage per capability. Fixed like the rows.
    world_caps: ds_check::WorldCaps,
}

impl Db {
    /// Creates an empty database with the computes for
    /// [`QueryKey::ParseFile`], [`QueryKey::BlockIndex`],
    /// [`QueryKey::BlockSource`], [`QueryKey::Resolve`],
    /// [`QueryKey::CheckedPackage`], [`QueryKey::FileLinearity`],
    /// [`QueryKey::PackageEffects`], [`QueryKey::TypeOf`],
    /// [`QueryKey::LinearityCheck`], [`QueryKey::EffectsOf`], and
    /// [`QueryKey::PlatformCheck`] registered: `ds-parse` for the tree and
    /// diagnostics, the tree's block spans for the index, the block-text
    /// seam for sources, the ds-check collection and resolution passes
    /// over the registered files (G3), the bidirectional check (B2), the M2
    /// affine pass (C1, C5) per file, the ds-check effects pass over the
    /// registered files (E1), and the per-definition slices of those three
    /// (see the crate docs).
    #[must_use]
    pub fn new() -> Self {
        let mut db = Self::default();
        db.set_compute(QueryKind::ParseFile, crate::queries::parse);
        db.set_compute(QueryKind::BlockIndex, crate::queries::block_index);
        db.set_compute(QueryKind::BlockSource, crate::queries::block_source);
        db.set_compute(QueryKind::Resolve, crate::queries::resolve);
        db.set_compute(QueryKind::CheckedPackage, crate::queries::checked_package);
        db.set_compute(QueryKind::FileLinearity, crate::queries::file_linearity);
        db.set_compute(QueryKind::PackageEffects, crate::queries::package_effects);
        db.set_compute(QueryKind::TypeOf, crate::queries::type_of);
        db.set_compute(QueryKind::LinearityCheck, crate::queries::linearity_check);
        db.set_compute(QueryKind::EffectsOf, crate::queries::effects_of);
        db.set_compute(QueryKind::PlatformCheck, crate::queries::platform_check);
        db
    }

    /// A database whose effects pass reads `rows` (E1): the stdlib ambient
    /// registers its world-touching functions' rows here. The table is an
    /// immutable input — set it before the first query; [`Db::new`] gives
    /// every ambient name the pure row.
    #[must_use]
    pub fn with_declared_rows(rows: ds_check::DeclaredRows) -> Self {
        let mut db = Self::new();
        db.declared_rows = rows;
        db
    }

    /// A database whose effects pass also reads `caps` (H5): the ambient's
    /// world functions' capability attribution, licensing world usage per
    /// capability against the package's platform declaration. Set before
    /// the first query, like [`Db::with_declared_rows`].
    #[must_use]
    pub fn with_world_caps(mut self, caps: ds_check::WorldCaps) -> Self {
        self.world_caps = caps;
        self
    }

    /// Registers `content` under `name` and returns its file id. File ids
    /// are never reused; names are not checked for duplicates.
    ///
    /// Registration is a change of the file-set input: the revision bumps
    /// and every memo that read it (the package-grain queries) is marked
    /// dirty, so a file added after a resolve/type_of memo exists is
    /// visible on the next read.
    ///
    /// # Panics
    ///
    /// Panics if the file count overflows `u32`.
    pub fn add_file(&mut self, name: impl Into<String>, content: &str) -> FileId {
        let file = self.files.add_source(name, content);
        self.revision = Revision(self.revision.as_u64() + 1);
        self.file_set_changed_at = self.revision;
        self.changed_at.push(self.revision);
        self.invalidate_from(Dependency::Input(InputKey::FileSet));
        file
    }

    /// Replaces a file's content. Setting the same content is a no-op: no
    /// revision bump, nothing invalidated. A real change bumps the global
    /// revision and marks every memo transitively depending on the file
    /// (via the reverse edges) as dirty; recomputation happens lazily on the
    /// next read.
    ///
    /// # Errors
    ///
    /// [`QueryError::UnknownFile`] if no such file was registered.
    pub fn set_file_content(&mut self, file: FileId, content: &str) -> Result<(), QueryError> {
        if !self.is_registered(file) {
            return Err(QueryError::UnknownFile(file));
        }
        if self.files.source(file) == content {
            return Ok(());
        }
        self.revision = Revision(self.revision.as_u64() + 1);
        let revision = self.revision;
        self.changed_at[file.as_u32() as usize] = revision;
        self.files.set_source(file, content);
        self.invalidate_from(Dependency::Input(InputKey::FileContent(file)));
        Ok(())
    }

    /// Runs (or reuses) the query and returns its typed value.
    ///
    /// # Errors
    ///
    /// Any [`QueryError`] raised while computing the query or its transitive
    /// dependencies; errors are not memoized, so a retried get recomputes.
    pub fn get<Q: Query>(&mut self, key: Q::Key) -> Result<Q::Value, QueryError> {
        let query_key = Q::make_key(key);
        let mut frames = Vec::new();
        let value = ensure(self, query_key, &mut frames)?;
        Q::extract(value).ok_or(QueryError::KindMismatch(query_key))
    }

    /// The current global revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// The source registry all query spans address; hand this to diagnostic
    /// renderers.
    #[must_use]
    pub fn files(&self) -> &Files {
        &self.files
    }

    /// The registered content of `file`, if any.
    #[must_use]
    pub fn file_content(&self, file: FileId) -> Option<&str> {
        self.is_registered(file).then(|| self.files.source(file))
    }

    /// The name `file` was registered under, if it exists.
    #[must_use]
    pub fn file_name(&self, file: FileId) -> Option<&str> {
        self.is_registered(file).then(|| self.files.name(file))
    }

    /// Replaces the compute function of a query kind: the hook tests and
    /// embedders use to model not-yet-landed queries.
    ///
    /// Existing memos of the kind do not survive the swap: each is dropped
    /// and its transitive dependents are marked dirty, and the revision
    /// bumps once when any memo was dropped, so the next read observes the
    /// new compute instead of a stale memo. Swapping before the first
    /// query of the kind (the usual shape) changes nothing observable.
    pub fn set_compute(&mut self, kind: QueryKind, compute: ComputeFn) {
        self.computes.insert(kind, compute);
        let affected: Vec<QueryKey> = self
            .memos
            .keys()
            .filter(|key| key.kind() == kind)
            .copied()
            .collect();
        if affected.is_empty() {
            return;
        }
        // Dropping the memos forces a recompute (a dirty memo would
        // deep-verify against unchanged deps and wrongly renew); the bump
        // makes every dependent's verification fail, so readers re-derive.
        self.revision = Revision(self.revision.as_u64() + 1);
        for key in &affected {
            self.memos.remove(key);
        }
        for key in affected {
            self.invalidate_from(Dependency::Query(key));
        }
    }

    /// How many times the query's compute function has actually run (debug
    /// and test introspection; memo hits are invisible here by design).
    /// Release builds count nothing and answer zero.
    #[must_use]
    pub fn compute_count(&self, key: &QueryKey) -> usize {
        self.compute_counts.get(key).copied().unwrap_or(0)
    }

    /// Whether the query has a memo that is not marked dirty (debug and
    /// test introspection).
    #[must_use]
    pub fn is_fresh(&self, key: &QueryKey) -> bool {
        self.memos.get(key).is_some_and(|memo| !memo.dirty)
    }

    fn is_registered(&self, file: FileId) -> bool {
        (file.as_u32() as usize) < self.changed_at.len()
    }

    fn input_changed_at(&self, input: InputKey) -> Revision {
        match input {
            InputKey::FileContent(file) => self.changed_at[file.as_u32() as usize],
            InputKey::FileSet => self.file_set_changed_at,
        }
    }

    /// Transitive invalidation: marks every memo reachable from `dep`
    /// through the reverse edges dirty. Marking is conservative (all
    /// blocks of a file are reached); the per-block freshness decision
    /// happens later, in deep verification.
    fn invalidate_from(&mut self, dep: Dependency) {
        let mut queue = VecDeque::from([dep]);
        let mut seen = FxHashSet::default();
        while let Some(dep) = queue.pop_front() {
            let dependents = self.reverse.get(&dep).cloned().unwrap_or_default();
            for dependent in dependents {
                if let Some(memo) = self.memos.get_mut(&dependent) {
                    memo.dirty = true;
                }
                let next = Dependency::Query(dependent);
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
    }
}

/// Context handed to a compute function: the only way a query reads inputs
/// or other queries. Every read records a dependency edge.
pub struct Ctx<'a> {
    db: &'a mut Db,
    frames: &'a mut Vec<Frame>,
}

impl Ctx<'_> {
    /// Reads a file's content, recording an input dependency.
    ///
    /// # Errors
    ///
    /// [`QueryError::UnknownFile`] if no such file was registered.
    pub fn file_content(&mut self, file: FileId) -> Result<Arc<str>, QueryError> {
        if !self.db.is_registered(file) {
            return Err(QueryError::UnknownFile(file));
        }
        let content = Arc::from(self.db.files.source(file));
        self.record(Dependency::Input(InputKey::FileContent(file)));
        Ok(content)
    }

    /// Reads another query's value, recording a query dependency.
    ///
    /// # Errors
    ///
    /// Any error from computing that query, including
    /// [`QueryError::Cycle`] if the read closes a cycle.
    pub fn get<Q: Query>(&mut self, key: Q::Key) -> Result<Q::Value, QueryError> {
        let query_key = Q::make_key(key);
        let value = ensure(self.db, query_key, &mut *self.frames)?;
        self.record(Dependency::Query(query_key));
        Q::extract(value).ok_or(QueryError::KindMismatch(query_key))
    }

    /// Reads the file-set input — the number of registered files, the
    /// package grain of the M2 checker queries (G3) — recording a
    /// dependency like every input read: registering another file
    /// invalidates the caller.
    pub fn file_count(&mut self) -> usize {
        self.record(Dependency::Input(InputKey::FileSet));
        self.db.changed_at.len()
    }

    /// The declared effect rows the effects pass reads (E1): the
    /// database's ambient row table.
    #[must_use]
    pub fn declared_rows(&self) -> &ds_check::DeclaredRows {
        &self.db.declared_rows
    }

    /// The world functions' capability attribution the effects pass reads
    /// beside the rows (H5): the database's ambient table.
    #[must_use]
    pub fn world_caps(&self) -> &ds_check::WorldCaps {
        &self.db.world_caps
    }

    /// The source registry all query spans address (the parse compute hands
    /// it to `ds-parse`; renderers get theirs from [`Db::files`]).
    #[must_use]
    pub fn files(&self) -> &Files {
        &self.db.files
    }

    /// The current global revision.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.db.revision
    }

    fn record(&mut self, dep: Dependency) {
        let frame = self
            .frames
            .last_mut()
            .expect("reads happen only inside a computation");
        frame.deps.insert(dep);
    }
}

/// Returns the key's value, memoizing: fresh memo hit, else deep-verify the
/// dirty memo, else recompute.
fn ensure(db: &mut Db, key: QueryKey, frames: &mut Vec<Frame>) -> Result<QueryValue, QueryError> {
    // Reading a key whose computation or verification is in flight closes a
    // cycle; report it with the full path before touching any memo state.
    if let Some(pos) = frames.iter().position(|frame| frame.key == key) {
        let mut path: Vec<QueryKey> = frames[pos..].iter().map(|frame| frame.key).collect();
        path.push(key);
        return Err(QueryError::Cycle { path });
    }

    // Fresh memo hit.
    if let Some(value) = db
        .memos
        .get(&key)
        .filter(|memo| !memo.dirty)
        .map(|memo| memo.value.clone())
    {
        return Ok(value);
    }
    // Dirty memo: deep-verify before recomputing. Taking the memo out of the
    // table hands verification its dependency list without a clone; it goes
    // back before any recompute needs the old value for the renewal compare,
    // and a read that closes a cycle through this key is caught by the
    // frame check above either way.
    if let Some(mut memo) = db.memos.remove(&key).filter(|memo| memo.dirty) {
        if verify(db, key, &memo.deps, memo.verified_at, frames)? {
            memo.dirty = false;
            memo.verified_at = db.revision;
            let value = memo.value.clone();
            db.memos.insert(key, memo);
            return Ok(value);
        }
        // A dependency genuinely moved: fall through and recompute.
        db.memos.insert(key, memo);
    }

    let Some(&compute) = db.computes.get(&key.kind()) else {
        return Err(QueryError::Unavailable(key));
    };
    frames.push(Frame {
        key,
        deps: Deps::default(),
    });
    let mut ctx = Ctx { db, frames };
    let result = compute(&mut ctx, key);
    let Ctx { db, frames } = ctx;
    let frame = frames.pop().expect("computing frame was just pushed above");

    let value = result?;
    // A recomputed value equal to the old one renews the memo: changed_at
    // stays put, so readers of this key stay valid. That renewal is what
    // makes invalidation block-grain.
    let (value_changed, changed_at) = match db.memos.get(&key) {
        Some(memo) if memo.value == value => (false, memo.changed_at),
        _ => (true, db.revision),
    };

    // Re-sync the reverse edges to the dependency set just recorded.
    if let Some(memo) = db.memos.get(&key) {
        for dep in &memo.deps {
            if let Some(dependents) = db.reverse.get_mut(dep) {
                dependents.retain(|reader| *reader != key);
            }
        }
    }
    for dep in &frame.deps.recorded {
        db.reverse.entry(*dep).or_default().push(key);
    }
    // If this value actually moved, everything reading it must re-verify.
    // Input-change marking usually covered these; this guards interleavings
    // where a reader verified against an older value of this key.
    if value_changed {
        let dependents = db
            .reverse
            .get(&Dependency::Query(key))
            .cloned()
            .unwrap_or_default();
        for dependent in dependents {
            if let Some(memo) = db.memos.get_mut(&dependent) {
                memo.dirty = true;
            }
        }
    }

    #[cfg(debug_assertions)]
    {
        *db.compute_counts.entry(key).or_insert(0) += 1;
    }
    let out = value.clone();
    db.memos.insert(key, Memo {
        value,
        deps: frame.deps.recorded,
        verified_at: db.revision,
        changed_at,
        dirty: false,
    });
    Ok(out)
}

/// Deep-verifies a dirty memo: walks its dependencies (computing them as
/// needed) and reports whether every one of them still carries a `changed_at`
/// at or before `verified_at`. Reads of in-flight keys surface as cycle
/// errors, since verification depends on values just like computation does.
fn verify(
    db: &mut Db,
    key: QueryKey,
    deps: &[Dependency],
    verified_at: Revision,
    frames: &mut Vec<Frame>,
) -> Result<bool, QueryError> {
    fn walk(
        db: &mut Db,
        deps: &[Dependency],
        verified_at: Revision,
        frames: &mut Vec<Frame>,
    ) -> Result<bool, QueryError> {
        for dep in deps {
            match *dep {
                Dependency::Input(input) => {
                    if db.input_changed_at(input) > verified_at {
                        return Ok(false);
                    }
                },
                Dependency::Query(dep_key) => {
                    ensure(db, dep_key, frames)?;
                    let dep_changed = db
                        .memos
                        .get(&dep_key)
                        .is_some_and(|memo| memo.changed_at > verified_at);
                    if dep_changed {
                        return Ok(false);
                    }
                },
            }
        }
        Ok(true)
    }

    frames.push(Frame {
        key,
        deps: Deps::default(),
    });
    let result = walk(db, deps, verified_at, frames);
    frames.pop();
    result
}
