//! Fibers as interpreter data [E2]: the VM pool, per-fiber segments, and
//! the switching vocabulary. A fiber is not an OS thread — the pool is
//! single-threaded, spawn allocates a pool entry with a fresh segment, and
//! a switch swaps segment, call-chain, and heap-store ownership between
//! the running fiber's pool slot and the VM's execution context: three
//! buffer swaps, no word or object ever copied [E2: switch is O(1)].
//!
//! # Segments: chunk-append growth [H2]
//!
//! A fiber's value-stack segment is a list of fixed-capacity chunks. Growth
//! appends a chunk and never reallocates the ones before it, so a word's
//! address (chunk base + offset) survives any later growth — the stability
//! guarantee deep non-tail recursion and captured frame addresses rely on.
//! This is the same chunk-append discipline the runtime's per-fiber arenas
//! use [H4]: arena marks are per-fiber and fiber switching needs no arena
//! action; here the segment is per-fiber and switching needs no segment
//! action beyond the ownership swap.
//!
//! Truncation (a frame pop) only moves the logical length cursor; chunks
//! stay allocated, so a parked fiber keeps its high-water footprint and its
//! addresses. Reclamation is pool-slot reuse: a dead fiber's chunks are
//! handed to the next spawn.
//!
//! # Resume points and resumers
//!
//! A parked fiber records where to continue (its module version, function,
//! and the instruction after the call that parked it) plus the fiber (or
//! the host) that resumed it. The switching mechanics — park, enter,
//! complete — live in the dispatcher (`crate::vm`); this module is the data
//! representation and the per-fiber TCO/stack-limit accounting surface.

use std::sync::Arc;

use ds_abi::CodePc;
use ds_rt::Arena;

use crate::{
    module::ModuleVersion, nursery::NurseryId, object::Objects, value::Word, vm::CallInfo,
};

/// Log2 of [`CHUNK_WORDS`]: chunk lookup is a shift and a mask.
const CHUNK_SHIFT: usize = 8;

/// Words per segment chunk: 256 words (4 KiB on 64-bit targets), small
/// enough that fixture-scale recursion crosses chunk boundaries.
pub const CHUNK_WORDS: usize = 1 << CHUNK_SHIFT;

/// The mask for a word's offset inside its chunk.
const CHUNK_MASK: usize = CHUNK_WORDS - 1;

/// A fiber's identity in the VM pool. Index 0 is reserved for the root
/// fiber a [`crate::Vm::run`] entry drives ([`FIBER_ROOT`]); spawns take
/// the slots above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FiberId(pub u32);

/// The root fiber: the entry function a `run` call drives. Its resumer is
/// always the host.
pub const FIBER_ROOT: FiberId = FiberId(0);

impl FiberId {
    /// The pool index this id names.
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

/// Where a fiber stands in the switch lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiberState {
    /// Spawned, never entered: its resume point is its entry function.
    Ready,
    /// Executing; only one fiber at a time holds this state.
    Running,
    /// Parked at a `fiber_yield`/`fiber_resume` call; its resume point is
    /// the instruction after that call.
    Suspended,
    /// Ran to completion (entry-frame RET or HALT); its pool slot is
    /// spawn-reusable.
    Dead,
}

/// Where a parked fiber continues: the module version and function it was
/// executing, and the instruction after the call that parked it.
#[derive(Debug)]
pub(crate) struct ResumePoint {
    pub(crate) version: Arc<ModuleVersion>,
    pub(crate) func: u32,
    pub(crate) pc: CodePc,
}

/// One pooled fiber: its value-stack segment, its call chain, its heap
/// object store, and its switch state. While the fiber runs, segment,
/// call chain, and store live in the VM's execution context and the slot
/// holds scratch; every other time they live here.
#[derive(Debug)]
pub(crate) struct Fiber {
    /// The fiber's value-stack segment [H2: chunk-append growth].
    pub(crate) segment: Segment,
    /// The fiber's frame arena: the bytes behind its designated
    /// concatenation temporaries. It lives in the pool slot and never
    /// swaps into the drive context (only the running fiber's own ops
    /// touch it; a death door is the only cross-fiber access), so a
    /// fiber switch performs no arena action.
    pub(crate) arena: Arena,
    /// The fiber's heap object store: the id table, the free list, the
    /// intern map, and ds-rt's block pools — one store per fiber, so a
    /// block and its count are only ever touched by the thread driving
    /// that fiber [C4]. The slot's store persists across pool reuse: a
    /// foreign-held object outlives its dead owner, and the next spawn
    /// continues the id space.
    pub(crate) objects: Objects,
    /// The fiber's frame chain (the running fiber's is swapped into the VM).
    pub(crate) callinfo: Vec<CallInfo>,
    /// High-water mark of this fiber's segment, the per-fiber TCO criterion.
    pub(crate) max_len: usize,
    /// Where a Ready/Suspended fiber continues.
    pub(crate) resume: Option<ResumePoint>,
    /// Lifecycle state.
    pub(crate) state: FiberState,
    /// The fiber that resumed this one; `None` = the host.
    pub(crate) resumer: Option<FiberId>,
    /// The nursery this fiber was spawned into (the child-of link [E2]);
    /// `None` = the root fiber or a detached spawn (no live root nursery —
    /// the host is the supervisor).
    pub(crate) nursery: Option<NurseryId>,
}

impl Fiber {
    /// An empty fiber: no segment, no frames, nothing resumable. The pool's
    /// root slot starts here and every spawn overwrites its slot wholesale
    /// (a reused slot keeps its chunks and its store — the objects stay).
    pub(crate) fn empty(id: FiberId) -> Self {
        Self {
            segment: Segment::new(),
            arena: Arena::new(),
            callinfo: Vec::new(),
            objects: Objects::new(id),
            max_len: 0,
            resume: None,
            state: FiberState::Ready,
            resumer: None,
            nursery: None,
        }
    }

    /// Whether a switch may enter this fiber: never started or parked at a
    /// call, with a continuation to enter. A trapped fiber (left `Running`
    /// with no resume point when its drive ended in a `Trap`) and a dead
    /// one answer false.
    pub(crate) fn resumable(&self) -> bool {
        matches!(self.state, FiberState::Ready | FiberState::Suspended) && self.resume.is_some()
    }
}

/// A per-fiber value-stack segment: fixed-capacity chunks appended on
/// growth, never reallocated, so existing words keep their addresses across
/// any later growth [H2]. Frames may straddle chunk boundaries — indexing
/// maps each word to its own chunk. Each chunk is one fixed-length array
/// behind its own box, so a slot access is one chunk-base load and the
/// offset add: the shift-and-mask split proves the in-chunk index in
/// range, which leaves one bounds check on the chunk list.
#[derive(Debug)]
pub(crate) struct Segment {
    chunks: Vec<Box<[Word; CHUNK_WORDS]>>,
    len: usize,
}

impl Segment {
    /// An empty segment; the first growth allocates the first chunk.
    pub(crate) fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    /// Logical word count.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Grows to `new_len`, appending chunks of [`CHUNK_WORDS`] words
    /// pre-filled with the inert zero word. Never shrinks.
    pub(crate) fn grow_to(&mut self, new_len: usize) {
        if new_len <= self.len {
            return;
        }
        let needed = new_len.div_ceil(CHUNK_WORDS);
        while self.chunks.len() < needed {
            self.chunks.push(Box::new(
                [Word::Slot(crate::value::Value::I64(0)); CHUNK_WORDS],
            ));
        }
        self.len = new_len;
    }

    /// Appends one word.
    pub(crate) fn push(&mut self, word: Word) {
        let at = self.len;
        self.grow_to(at + 1);
        self[at] = word;
    }

    /// Drops the words past `new_len` (a frame pop). Chunks stay allocated:
    /// parked fibers keep their footprint and their addresses; only pool
    /// reuse or [`Segment::clear`] frees them.
    pub(crate) fn truncate(&mut self, new_len: usize) {
        self.len = self.len.min(new_len);
    }

    /// Frees every chunk (a fresh run's root frame starts from nothing).
    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.len = 0;
    }

    /// How many chunks the segment owns — the growth witness.
    pub(crate) fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Words physically allocated across the chunks: the address space a
    /// stability assertion can name, including words a frame pop truncated
    /// past (chunks are retained, so their addresses stay meaningful).
    pub(crate) fn capacity(&self) -> usize {
        self.chunks.len() * CHUNK_WORDS
    }

    /// The address of word `i`, for stability assertions across growth and
    /// switch round-trips.
    pub(crate) fn word_addr(&self, i: usize) -> usize {
        (&raw const self[i]) as usize
    }

    /// The word at `i` when the logical segment covers it: the teardown
    /// pass reads dying frames' words this way, and a frame region past
    /// the current length (never written, or truncated away) answers
    /// `None` instead of indexing stale chunks.
    pub(crate) fn get(&self, i: usize) -> Option<&Word> {
        (i < self.len()).then(|| &self[i])
    }
}

impl std::ops::Index<usize> for Segment {
    type Output = Word;

    fn index(&self, i: usize) -> &Word {
        &self.chunks[i >> CHUNK_SHIFT][i & CHUNK_MASK]
    }
}

impl std::ops::IndexMut<usize> for Segment {
    fn index_mut(&mut self, i: usize) -> &mut Word {
        &mut self.chunks[i >> CHUNK_SHIFT][i & CHUNK_MASK]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(i: usize) -> Word {
        Word::Slot(crate::value::Value::I64(
            i64::try_from(i).expect("test index fits"),
        ))
    }

    #[test]
    fn growth_appends_chunks_and_keeps_earlier_addresses_stable() {
        let mut seg = Segment::new();
        seg.grow_to(10);
        assert_eq!(seg.chunk_count(), 1);
        seg[3] = word(3);
        let addr = seg.word_addr(3);
        // grow past the first chunk: chunk 0 must not move
        seg.grow_to(10 * CHUNK_WORDS);
        assert!(seg.chunk_count() > 1);
        assert_eq!(seg.word_addr(3), addr, "chunk append never reallocates");
        assert_eq!(seg[3], word(3), "the word itself survives");
        // a word in the second chunk maps through its own chunk
        let mid = CHUNK_WORDS + 1;
        seg[mid] = word(mid);
        assert_eq!(seg[mid], word(mid));
    }

    #[test]
    fn frames_may_straddle_chunk_boundaries() {
        let mut seg = Segment::new();
        seg.grow_to(2 * CHUNK_WORDS);
        // write a frame-sized span across the boundary and read it back
        for i in (CHUNK_WORDS - 2)..(CHUNK_WORDS + 2) {
            seg[i] = word(i);
        }
        for i in (CHUNK_WORDS - 2)..(CHUNK_WORDS + 2) {
            assert_eq!(seg[i], word(i));
        }
    }

    #[test]
    fn truncate_keeps_chunks_and_regrow_reuses_them() {
        let mut seg = Segment::new();
        seg.grow_to(2 * CHUNK_WORDS);
        let chunks = seg.chunk_count();
        seg.truncate(4);
        assert_eq!(seg.len(), 4);
        assert_eq!(seg.chunk_count(), chunks, "a pop frees no chunks");
        seg.grow_to(2 * CHUNK_WORDS);
        assert_eq!(
            seg.chunk_count(),
            chunks,
            "regrow within capacity appends nothing"
        );
        seg.clear();
        assert_eq!(seg.chunk_count(), 0);
        assert_eq!(seg.len(), 0);
    }
}
