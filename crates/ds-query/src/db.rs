//! The database: inputs, memo table, dependency edges, and the
//! verify-or-recompute cycle that drives them.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use ds_diag::FileId;

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
/// exposes inputs and other queries) and its key.
pub type ComputeFn = fn(&mut Ctx<'_>, QueryKey) -> Result<QueryValue, QueryError>;

/// One registered input, with the revision it last changed at.
#[derive(Clone, Debug)]
struct InputEntry {
    name: String,
    content: Arc<str>,
    changed_at: Revision,
}

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
    deps: Vec<Dependency>,
}

/// The query database: the only owner of inputs and memos, and the only path
/// to compiler data for every consumer (CLI, LSP, REPL, hot reload,
/// formatter) [H6 item 1].
///
/// Determinism: computes are pure functions of inputs, so the same inputs
/// yield the same results; whether a value comes from a memo hit or a fresh
/// compute is never observable in returned values.
#[derive(Clone, Debug, Default)]
pub struct Db {
    revision: Revision,
    inputs: HashMap<FileId, InputEntry>,
    memos: HashMap<QueryKey, Memo>,
    /// Reverse dependency edges: dependency -> keys whose memos read it.
    reverse: HashMap<Dependency, HashSet<QueryKey>>,
    computes: HashMap<QueryKind, ComputeFn>,
    compute_counts: HashMap<QueryKey, usize>,
}

impl Db {
    /// Creates an empty database with the placeholder computes for
    /// [`QueryKey::ParseFile`], [`QueryKey::BlockIndex`], and
    /// [`QueryKey::BlockSource`] registered (see the crate docs for the
    /// `ds-parse` seam they model).
    #[must_use]
    pub fn new() -> Self {
        let mut db = Self::default();
        db.set_compute(QueryKind::ParseFile, crate::placeholder::parse);
        db.set_compute(QueryKind::BlockIndex, crate::placeholder::block_index);
        db.set_compute(QueryKind::BlockSource, crate::placeholder::block_source);
        db
    }

    /// Registers `content` under `name` and returns its file id. File ids
    /// are never reused; names are not checked for duplicates.
    ///
    /// # Panics
    ///
    /// Panics if the file count overflows `u32`.
    pub fn add_file(&mut self, name: impl Into<String>, content: &str) -> FileId {
        let file = FileId::new(u32::try_from(self.inputs.len()).expect("file count overflows u32"));
        self.inputs.insert(file, InputEntry {
            name: name.into(),
            content: Arc::from(content),
            changed_at: self.revision,
        });
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
        let Some(entry) = self.inputs.get_mut(&file) else {
            return Err(QueryError::UnknownFile(file));
        };
        if entry.content.as_ref() == content {
            return Ok(());
        }
        self.revision = Revision(self.revision.as_u64() + 1);
        let revision = self.revision;
        entry.content = Arc::from(content);
        entry.changed_at = revision;

        // Transitive invalidation: mark every memo reachable from the
        // changed input through the reverse edges. Marking is conservative
        // (all blocks of the file are reached); the per-block freshness
        // decision happens later, in deep verification.
        let mut queue = VecDeque::from([Dependency::Input(InputKey::FileContent(file))]);
        let mut seen = HashSet::new();
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

    /// The registered content of `file`, if any.
    #[must_use]
    pub fn file_content(&self, file: FileId) -> Option<&str> {
        self.inputs.get(&file).map(|e| e.content.as_ref())
    }

    /// The name `file` was registered under, if it exists.
    #[must_use]
    pub fn file_name(&self, file: FileId) -> Option<&str> {
        self.inputs.get(&file).map(|e| e.name.as_str())
    }

    /// Replaces the compute function of a query kind. This is the seam the
    /// `ds-parse` follow-up uses to install the real parse pipeline, and the
    /// hook tests and embedders use to model not-yet-landed queries.
    pub fn set_compute(&mut self, kind: QueryKind, compute: ComputeFn) {
        self.computes.insert(kind, compute);
    }

    /// How many times the query's compute function has actually run (debug
    /// and test introspection; memo hits are invisible here by design).
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

    fn input_changed_at(&self, input: InputKey) -> Revision {
        match input {
            InputKey::FileContent(file) => {
                self.inputs
                    .get(&file)
                    .expect("input deps only reference registered files")
                    .changed_at
            },
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
        let Some(entry) = self.db.inputs.get(&file) else {
            return Err(QueryError::UnknownFile(file));
        };
        let content = Arc::clone(&entry.content);
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
        if !frame.deps.contains(&dep) {
            frame.deps.push(dep);
        }
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
    // Dirty memo: deep-verify before recomputing.
    if let Some((deps, verified_at)) = db
        .memos
        .get(&key)
        .filter(|memo| memo.dirty)
        .map(|memo| (memo.deps.clone(), memo.verified_at))
    {
        if verify(db, key, &deps, verified_at, frames)? {
            let revision = db.revision;
            let memo = db
                .memos
                .get_mut(&key)
                .expect("memo existed when its deps were cloned");
            memo.dirty = false;
            memo.verified_at = revision;
            return Ok(memo.value.clone());
        }
        // A dependency genuinely moved: fall through and recompute.
    }

    let Some(&compute) = db.computes.get(&key.kind()) else {
        return Err(QueryError::Unavailable(key));
    };
    frames.push(Frame {
        key,
        deps: Vec::new(),
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
                dependents.remove(&key);
            }
        }
    }
    for dep in &frame.deps {
        db.reverse.entry(*dep).or_default().insert(key);
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

    *db.compute_counts.entry(key).or_insert(0) += 1;
    let out = value.clone();
    db.memos.insert(key, Memo {
        value,
        deps: frame.deps,
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
        deps: Vec::new(),
    });
    let result = walk(db, deps, verified_at, frames);
    frames.pop();
    result
}
