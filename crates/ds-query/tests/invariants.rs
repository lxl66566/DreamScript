//! Randomized invariant harness: a seeded PRNG drives arbitrary edit
//! sequences over real multi-block `.ds` files against a `Db` wired like
//! tests/engine.rs (the real parse computes plus chained block-derived
//! downstream queries). After every op a full read sweep asserts:
//!
//! * (a) no op panics and every generated source parses clean (tree and no diagnostics — the scan
//!   fallback must never trigger here);
//! * (b) the revision is monotone and bumps exactly once per real change;
//! * (c) memo coherence: every read value equals what a from-scratch replay of the same inputs
//!   computes (memo state is never observable);
//! * (d) one sweep bumps any query's compute count by at most one beyond its first computation, and
//!   a read-only iteration bumps nothing (renewal actually avoids recomputation);
//! * (e) block-grain locality: an edit of file f never recomputes any query of another file, and
//!   never recomputes a downstream query of a block whose text held — layout shifts included,
//!   because a recomputed-equal block source renews its dependents.
//!
//! The oracle models the REAL tree-split semantics (see `FileModel`): the
//! newline terminating a separator line opens the following block, horizontal
//! whitespace before a separator closes the preceding block, and text after
//! `///|` belongs to no block. Every sweep pins the engine's block texts
//! against that model, so a parse/index divergence fails loudly.
//!
//! The cargo-fuzz target `fuzz_query_engine` drives the same op vocabulary
//! with coverage feedback; this file is the seeded, fully-checked mirror.

use std::{collections::HashMap, sync::Arc};

use ds_diag::FileId;
use ds_query::{
    BlockId, BlockIndex, BlockIndexOutput, BlockSource, Ctx, Db, DefId, ParseFile, ParseOutput,
    Query, QueryError, QueryKey, QueryKind, QueryValue,
};

/// Blocks per file ceiling: keeps the tracked key set small.
const MAX_BLOCKS: usize = 8;
/// Files driven by the harness.
const FILE_COUNT: usize = 4;

/// xorshift64* PRNG: deterministic, dependency-free, good enough for op
/// streams (mirrors crates/ds-mod/tests/fuzz_read.rs).
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("bound fits u64").max(1);
        usize::try_from(self.next_u64() % bound).expect("remainder fits usize")
    }
}

/// Per-seed iteration count; a CI fuzz job can crank it with
/// `FUZZ_RUNS=<n> cargo nextest run` (same knob as ds-mod's mirror). The
/// default keeps the four seeds under ~2 s: every real edit runs the real
/// parse, whose dedicated 8 MiB stack thread dominates the cost.
fn fuzz_runs() -> usize {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100)
}

/// Downstream marker over the placeholder TypeOf key; DefId packs
/// (file, block), so one compute serves every file's blocks.
struct TypeOf;

impl Query for TypeOf {
    type Key = DefId;
    type Value = Arc<str>;

    fn make_key(key: DefId) -> QueryKey {
        QueryKey::TypeOf(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::BlockSource(text) => Some(text),
            _ => None,
        }
    }
}

/// Second-level downstream marker over LinearityCheck: reads TypeOf, so the
/// dependency chain is input -> parse -> block source -> type -> linearity.
struct LinearityOf;

impl Query for LinearityOf {
    type Key = DefId;
    type Value = Arc<str>;

    fn make_key(key: DefId) -> QueryKey {
        QueryKey::LinearityCheck(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::BlockSource(text) => Some(text),
            _ => None,
        }
    }
}

/// Packs (file, block) into the placeholder DefId the downstream keys use.
fn def_id(file: u32, block: u32) -> DefId {
    DefId::new((file << 8) | block)
}

fn type_of_block(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::TypeOf(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let text = ctx.get::<BlockSource>(BlockId {
        file: FileId::new(def.as_u32() >> 8),
        index: def.as_u32() & 0xff,
    })?;
    Ok(QueryValue::BlockSource(text))
}

fn linearity_of_def(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::LinearityCheck(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let text = ctx.get::<TypeOf>(def)?;
    Ok(QueryValue::BlockSource(text))
}

/// Ground truth of one `.ds` file: item lines per block interleaved with
/// separator lines. `block_text` reproduces the engine's tree-split text
/// exactly, which is what renewal locality is measured against:
///
/// * the newline terminating a separator line opens the following block (every block but the first
///   starts with `\n`);
/// * horizontal whitespace before a separator closes the preceding block (indenting `///|` extends
///   the previous block's text);
/// * text after `///|` belongs to no block;
/// * the last block runs to end of file.
struct FileModel {
    bodies: Vec<String>,
    /// `seps[i]` sits between `bodies[i]` and `bodies[i + 1]`; each is
    /// optional indentation, `///|`, optional trailing text, and a newline.
    seps: Vec<String>,
}

impl FileModel {
    fn new(bodies: Vec<String>) -> Self {
        let seps = vec!["///|\n".to_owned(); bodies.len() - 1];
        Self { bodies, seps }
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        for (i, body) in self.bodies.iter().enumerate() {
            if i > 0 {
                out.push_str(&self.seps[i - 1]);
            }
            out.push_str(body);
        }
        out
    }

    fn block_count(&self) -> usize {
        self.bodies.len()
    }

    /// The engine's text of block `i` (tree spans; all sources parse clean).
    fn block_text(&self, i: usize) -> String {
        let mut text = if i == 0 {
            String::new()
        } else {
            // The newline of separator line i-1 opens this block.
            "\n".to_owned()
        };
        text.push_str(&self.bodies[i]);
        if let Some(sep) = self.seps.get(i) {
            // Horizontal whitespace before the next separator closes it —
            // except for the file's empty leading block, whose extent stays
            // empty because the grammar's top-level `sp` eats that
            // whitespace before the first `file_block` can.
            if i != 0 || !self.bodies[0].is_empty() {
                text.push_str(sep_indent(sep));
            }
        }
        text
    }

    fn texts(&self) -> Vec<String> {
        (0..self.block_count())
            .map(|i| self.block_text(i))
            .collect()
    }
}

/// The leading horizontal whitespace of a separator line (`sep` always ends
/// with `///|\n`, possibly after trailing separator text).
fn sep_indent(sep: &str) -> &str {
    let head = &sep[..sep.len() - "///|\n".len()];
    &head[..head.len() - head.trim_start().len()]
}

/// One tracked query: engine key plus the typed read used to observe it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Track {
    Parse(u32),
    Index(u32),
    Source(u32, u32),
    Type(u32, u32),
    Lin(u32, u32),
}

impl Track {
    fn file(self) -> u32 {
        match self {
            Self::Parse(f)
            | Self::Index(f)
            | Self::Source(f, _)
            | Self::Type(f, _)
            | Self::Lin(f, _) => f,
        }
    }

    fn raw(self) -> QueryKey {
        match self {
            Self::Parse(f) => QueryKey::ParseFile(FileId::new(f)),
            Self::Index(f) => QueryKey::BlockIndex(FileId::new(f)),
            Self::Source(f, b) => QueryKey::BlockSource(BlockId {
                file: FileId::new(f),
                index: b,
            }),
            Self::Type(f, b) => QueryKey::TypeOf(def_id(f, b)),
            Self::Lin(f, b) => QueryKey::LinearityCheck(def_id(f, b)),
        }
    }

    fn read(self, db: &mut Db) -> Result<Observed, QueryError> {
        match self {
            Self::Parse(f) => db.get::<ParseFile>(FileId::new(f)).map(Observed::Parse),
            Self::Index(f) => db.get::<BlockIndex>(FileId::new(f)).map(Observed::Index),
            Self::Source(f, b) => db
                .get::<BlockSource>(BlockId {
                    file: FileId::new(f),
                    index: b,
                })
                .map(Observed::Text),
            Self::Type(f, b) => db.get::<TypeOf>(def_id(f, b)).map(Observed::Text),
            Self::Lin(f, b) => db.get::<LinearityOf>(def_id(f, b)).map(Observed::Text),
        }
    }
}

/// A read result, normalized so values compare across databases.
#[derive(PartialEq, Debug)]
enum Observed {
    Parse(ParseOutput),
    Index(BlockIndexOutput),
    Text(Arc<str>),
}

/// A fresh db replaying the model: same files registered in the same order
/// (file ids are assigned by registration order), same downstream computes.
fn replay_db(files: &[FileModel]) -> Db {
    let mut db = Db::new();
    db.set_compute(QueryKind::TypeOf, type_of_block);
    db.set_compute(QueryKind::LinearityCheck, linearity_of_def);
    for (f, file) in files.iter().enumerate() {
        db.add_file(format!("file{f}.ds"), &file.serialize());
    }
    db
}

/// The harness state: the aged db, its ground-truth model, and the snapshot
/// (block texts, compute counts implied) the previous sweep verified
/// against.
struct Harness {
    db: Db,
    files: Vec<FileModel>,
    prev_texts: Vec<Vec<String>>,
    counter: u32,
}

impl Harness {
    fn new() -> Self {
        let files = (0..FILE_COUNT)
            .map(|f| {
                FileModel::new(
                    (0..3)
                        .map(|b| format!("const f{f}b{b} = {}\n", f * 10 + b))
                        .collect(),
                )
            })
            .collect::<Vec<_>>();
        let harness = Self {
            db: replay_db(&files),
            prev_texts: files.iter().map(FileModel::texts).collect(),
            counter: 0,
            files,
        };
        assert_eq!(
            harness.db.revision().as_u64(),
            0,
            "setup registers inputs but performs no changes"
        );
        harness
    }

    /// Every live query of every file, in sweep order.
    fn tracked(&self) -> Vec<Track> {
        let mut keys = Vec::new();
        for (idx, file) in self.files.iter().enumerate() {
            let f = u32::try_from(idx).expect("file index fits u32");
            keys.push(Track::Parse(f));
            keys.push(Track::Index(f));
            for b in 0..file.block_count() {
                let b = u32::try_from(b).expect("block index fits u32");
                keys.push(Track::Source(f, b));
                keys.push(Track::Type(f, b));
                keys.push(Track::Lin(f, b));
            }
        }
        keys
    }

    /// Applies one random edit; reports (file, whether the content bytes
    /// really changed) per the model — the db must agree via its revision.
    fn edit_op(&mut self, rng: &mut Rng) -> (usize, bool) {
        let file = rng.below(self.files.len());
        match rng.below(10) {
            0..=3 => self.edit_block(rng, file),
            4 => self.restyle_separator(rng, file),
            5..=6 => self.insert_separator(rng, file),
            7 => self.remove_separator(rng, file),
            // No model mutation: a pure same-content edit.
            8..=9 => (file, self.commit(file)),
            _ => unreachable!("rng.below(10) stays below 10"),
        }
    }

    /// Rewrites one block body (a 1-in-4 chance rewrites the same body: the
    /// edit must register as a no-op).
    fn edit_block(&mut self, rng: &mut Rng, file: usize) -> (usize, bool) {
        let block = rng.below(self.files[file].block_count());
        self.counter += 1;
        let body = if rng.below(4) == 0 {
            self.files[file].bodies[block].clone()
        } else {
            // One or two const lines, so separator splits stay meaningful.
            let head = format!("const c{}_a = {}\n", self.counter, rng.below(100));
            if rng.below(2) == 0 {
                head
            } else {
                format!("{head}const c{}_b = {}\n", self.counter, rng.below(100))
            }
        };
        self.files[file].bodies[block] = body;
        (file, self.commit(file))
    }

    /// Re-styles one separator line's surroundings: indentation (which joins
    /// the PREVIOUS block's text — the real tree-split semantics) and
    /// trailing text after `///|` (which belongs to no block, so every block
    /// text holds while all later spans shift).
    fn restyle_separator(&mut self, rng: &mut Rng, file: usize) -> (usize, bool) {
        if self.files[file].seps.is_empty() {
            return (file, self.commit(file)); // one block: nothing outside blocks
        }
        let at = rng.below(self.files[file].seps.len());
        let indent = if rng.below(2) == 0 { "  " } else { "" };
        let note = if rng.below(2) == 0 { " section" } else { "" };
        self.files[file].seps[at] = format!("{indent}///|{note}\n");
        (file, self.commit(file))
    }

    /// Splits one block at a line boundary: the block count grows and the
    /// layout shifts at and after the split.
    fn insert_separator(&mut self, rng: &mut Rng, file: usize) -> (usize, bool) {
        if self.files[file].block_count() >= MAX_BLOCKS {
            return (file, self.commit(file));
        }
        let at = rng.below(self.files[file].block_count());
        let body = self.files[file].bodies[at].clone();
        let lines = body.matches('\n').count();
        let split = rng.below(lines + 1);
        let mut head = String::new();
        let mut tail = String::new();
        for (line, chunk) in body.split_inclusive('\n').enumerate() {
            if line < split {
                head.push_str(chunk);
            } else {
                tail.push_str(chunk);
            }
        }
        self.files[file].bodies[at] = head;
        self.files[file].bodies.insert(at + 1, tail);
        self.files[file].seps.insert(at, "///|\n".to_owned());
        (file, self.commit(file))
    }

    /// Merges two adjacent blocks: the block count shrinks and blocks after
    /// the merge shift forward.
    fn remove_separator(&mut self, rng: &mut Rng, file: usize) -> (usize, bool) {
        if self.files[file].block_count() < 2 {
            return (file, self.commit(file));
        }
        let at = rng.below(self.files[file].seps.len());
        let merged = self.files[file].bodies[at].clone() + &self.files[file].bodies[at + 1];
        self.files[file].bodies[at] = merged;
        self.files[file].bodies.remove(at + 1);
        self.files[file].seps.remove(at);
        (file, self.commit(file))
    }

    /// Pushes the file's model content into the db; reports whether the
    /// bytes actually changed (ground truth for the revision bump).
    fn commit(&mut self, file: usize) -> bool {
        let id = FileId::new(u32::try_from(file).expect("file index fits u32"));
        let old = self.db.file_content(id).expect("registered file");
        let content = self.files[file].serialize();
        let changed = old != content;
        self.db
            .set_file_content(id, &content)
            .expect("registered file");
        changed
    }

    /// A read-only op: live get, ghost block, or ghost file edit. Must not
    /// move the revision or recompute anything (the sweep asserts both).
    fn read_op(&mut self, rng: &mut Rng) {
        match rng.below(4) {
            0 => {
                // Ghost block: a deterministic error, never memoized.
                let f = rng.below(self.files.len());
                let index = u32::try_from(self.files[f].block_count() + 1 + rng.below(3))
                    .expect("block index fits u32");
                let block = BlockId {
                    file: FileId::new(u32::try_from(f).expect("file index fits u32")),
                    index,
                };
                assert_eq!(
                    self.db.get::<BlockSource>(block),
                    Err(QueryError::UnknownBlock(block))
                );
                assert!(!self.db.is_fresh(&QueryKey::BlockSource(block)));
            },
            1 => {
                // Ghost file: rejected without touching any state.
                let ghost =
                    FileId::new(u32::try_from(self.files.len() + 9).expect("file id fits u32"));
                assert_eq!(
                    self.db.set_file_content(ghost, "const x = 1\n"),
                    Err(QueryError::UnknownFile(ghost))
                );
                assert!(self.db.file_content(ghost).is_none());
            },
            _ => {
                let key = self.random_track(rng);
                key.read(&mut self.db).expect("live key computes");
            },
        }
    }

    fn random_track(&mut self, rng: &mut Rng) -> Track {
        let f = rng.below(self.files.len());
        let file = u32::try_from(f).expect("file index fits u32");
        let block =
            u32::try_from(rng.below(self.files[f].block_count())).expect("block index fits u32");
        match rng.below(5) {
            0 => Track::Parse(file),
            1 => Track::Index(file),
            2 => Track::Source(file, block),
            3 => Track::Type(file, block),
            _ => Track::Lin(file, block),
        }
    }

    /// Reads every tracked query and asserts the invariant set against the
    /// previous sweep's snapshot. `changed` lists the files whose content
    /// really changed since then.
    fn sweep(&mut self, changed: &[usize], seed: u64, iteration: usize) {
        let keys = self.tracked();
        let before: HashMap<Track, usize> = keys
            .iter()
            .map(|key| (*key, self.db.compute_count(&key.raw())))
            .collect();
        let rev_before = self.db.revision();
        let results: Vec<(Track, Result<Observed, QueryError>)> = keys
            .iter()
            .map(|key| (*key, key.read(&mut self.db)))
            .collect();

        // (a) reaching here means the sweep did not panic; reads never bump
        // the revision, and only a successful read leaves a fresh memo.
        assert_eq!(
            self.db.revision(),
            rev_before,
            "seed {seed} iteration {iteration}: a read sweep bumped the revision"
        );
        for (key, result) in &results {
            assert_eq!(
                self.db.is_fresh(&key.raw()),
                result.is_ok(),
                "seed {seed} iteration {iteration} {key:?}: fresh iff the read succeeded"
            );
        }

        for (key, result) in &results {
            let fidx = usize::try_from(key.file()).expect("file index fits usize");
            let spot = format!("seed {seed} iteration {iteration} {key:?}");
            match result {
                // (a, oracle) generated sources parse clean: the tree path,
                // never the scan fallback, and no diagnostics.
                Ok(Observed::Parse(parse)) => {
                    assert!(
                        parse.ast.is_some(),
                        "{spot}: generated source failed to parse"
                    );
                    assert!(
                        parse.diagnostics.is_empty(),
                        "{spot}: unexpected diagnostics"
                    );
                },
                // (a, oracle) the engine's block text equals the model's
                // tree-split text — pins parse and index to the oracle.
                Ok(Observed::Text(text)) => {
                    if let Track::Source(_, b) = key {
                        let expected = self.files[fidx].block_text(*b as usize);
                        assert_eq!(
                            &**text, expected,
                            "{spot}: block text diverged from the model"
                        );
                    }
                },
                _ => {},
            }
            let after = self.db.compute_count(&key.raw());
            let before = before[key];
            assert!(after >= before, "{spot}: compute count went backwards");
            let bump = after - before;
            let first = before == 0;
            // (d) at most one recompute per key per invalidation cycle.
            assert!(
                bump <= 1,
                "{spot}: recomputed {bump} times within one sweep"
            );
            if !changed.contains(&fidx) {
                // (e, cross-file) nothing this key depends on changed; the
                // only allowed compute is a first computation.
                assert!(
                    first || bump == 0,
                    "{spot}: recomputed although its file did not change"
                );
            } else if let Track::Parse(_) = key {
                // The input dependency moved: the file chain must re-derive.
                assert_eq!(
                    bump, 1,
                    "{spot}: a changed file did not re-derive its parse"
                );
            } else if let Track::Type(_, b) | Track::Lin(_, b) = key {
                // (e, block grain) downstream of a block whose text held:
                // renewal, never a recompute — even when the layout
                // shifted, as long as the block's own text is unchanged.
                if !first && self.text_held(fidx, *b) {
                    assert_eq!(bump, 0, "{spot}: recomputed although the block text held");
                }
            }
        }

        // (c) memo coherence: the aged db matches a from-scratch replay.
        let mut replay = replay_db(&self.files);
        for (key, aged) in &results {
            assert_eq!(
                &key.read(&mut replay),
                aged,
                "seed {seed} iteration {iteration} {key:?}: aged value diverged from a replay"
            );
        }

        self.prev_texts = self.files.iter().map(FileModel::texts).collect();
    }

    /// Whether block `block` of file `file` carries the same text as at the
    /// previous sweep (a block born after it counts as changed).
    fn text_held(&self, file: usize, block: u32) -> bool {
        let b = usize::try_from(block).expect("block index fits usize");
        match (
            self.prev_texts[file].get(b),
            self.files[file].texts().get(b),
        ) {
            (Some(old), Some(new)) => old == new,
            _ => false,
        }
    }
}

/// Runs one seed's edit sequence through the harness.
fn run_seed(seed: u64) {
    let mut rng = Rng(seed | 1);
    let mut harness = Harness::new();
    for iteration in 0..fuzz_runs() {
        let rev_before = harness.db.revision().as_u64();
        let mut real_edits = 0_u64;
        let mut changed: Vec<usize> = Vec::new();
        let edits = rng.below(3);
        if edits == 0 {
            harness.read_op(&mut rng);
        } else {
            for _ in 0..edits {
                let (file, real) = harness.edit_op(&mut rng);
                if real {
                    real_edits += 1;
                    if !changed.contains(&file) {
                        changed.push(file);
                    }
                }
            }
        }

        // (b) the revision bumps exactly once per real content change.
        let rev_after = harness.db.revision().as_u64();
        assert!(
            rev_after >= rev_before,
            "seed {seed} iteration {iteration}: revision went backwards"
        );
        assert_eq!(
            rev_after - rev_before,
            real_edits,
            "seed {seed} iteration {iteration}: revision bumps must match real changes"
        );

        harness.sweep(&changed, seed, iteration);
    }
}

#[test]
fn random_edit_sequences_preserve_invariants() {
    for seed in [
        0x00c0_ffee_0000_0001_u64,
        0x9e37_79b9_7f4a_7c15,
        0xdead_beef_cafe_f00d,
        0x5eed_1234_abcd_8888,
    ] {
        run_seed(seed);
    }
}
