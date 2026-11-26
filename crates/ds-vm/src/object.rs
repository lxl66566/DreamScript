//! The VM-side heap object store [BL-2]: the objects raw `Ref` words name.
//!
//! Identity is the id table: a `Value::Ref` word's id part is an index
//! into `table`, and every live entry names one counted block of the ds-rt heap —
//! header plus payload in one allocation, the `repr(C)` layout a later JIT
//! can inline. The count lives in the block header, RCINC/RCDEC route
//! through ds-rt's rc surface, and a count reaching zero frees the object:
//! the id threads the table's free list (the next allocation claims it —
//! the dec-to-alloc pairing runs inside this table, so no alloc-at uop
//! exists in the ISA) while the block's memory pools in ds-rt's
//! size-class freelist (LIFO, depth-capped) for the next compatible
//! allocation, the `alloc_at` pattern. Free order is deterministic: drops
//! happen in program order, and both free lists are
//! last-freed-first-reused.
//!
//! A list is one object with its element words inline — the ruled shape;
//! see `[Object::List]`. The element words are plain [`Value`]s, so one
//! kind serves every element type. Ownership of `Ref` elements is exact:
//! storing one into a list (the `list_push` construction append, the
//! `list_concat` shared-element copies, the `list_get` checked read-out)
//! acquires a count, and freeing a list drops one count per `Ref`
//! element it holds. Producer natives that build lists of fresh objects
//! (`split`, `fs_list`) store without acquiring — the fresh object's
//! birth count is the list's. String interning acquires too: a `KSTR`
//! load claims a count of the interned object, and a dying interned
//! string removes its intern entry so the next load of the same content
//! reborns a fresh object. A `KSTR_BORROW` load claims no count: the
//! intern map pins its canonical copy for the store's life (see
//! [`Objects::intern_borrow`]), so the borrowed word outlives every
//! count churn around it.
//!
//! The store drives each block's count lifecycle through ds-rt's split
//! dec: a unique death observed at `rc_dec_count` runs the store's death
//! work first (unbind the intern entry, extract the owned `Ref` words,
//! thread the id) and owes exactly one `rc_dispose`, which drops the
//! payload's Rust memory and pools the block. The death cascade runs on
//! an explicit work stack — a composite's owned counts drop in a loop
//! instead of native recursion, so a deeply nested structure dies in
//! bounded stack (the heap bounds the stack, and the free order stays
//! the recursive depth-first one: each owner's children fully before
//! the next sibling).
//!
//! Counts are non-atomic: one `Objects` store belongs to one fiber on
//! one thread [C4]. Words self-describe their owner, so a store can
//! hold a foreign word — a list on one heap owning a count on another —
//! and the count mutations that reach past the store's own heap are
//! the bracketed delivery classes plus the death cascade, which route
//! through the pending-foreign queue: the store queues the cross-heap
//! inc/dec and the routing face (the `Heaps` router over the pool)
//! drains it, so no `Objects` method reaches another store. The `diag`
//! feature arms ds-rt's balance tripwires (rc underflow/overflow
//! asserts, the borrow-at-death check); the store holds no borrows, so
//! a firing tripwire is a balance bug.

use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    fmt,
    hash::{BuildHasherDefault, Hasher},
    ptr::NonNull,
};

use ds_rt::{Heap, RcHeader, drop_deinit};

use crate::{
    claims::ClaimTable,
    fiber::{Fiber, FiberId},
    loader::StagedSite,
    value::{FuncRef, RefId, Value},
    vm::Trap,
};

mod arena;
mod chan;
mod heaps;
mod list;
mod map;
mod rc;
mod text;

pub use heaps::*;
use map::*;

/// The free-list end sentinel: no `Free` word points past it.
const NO_FREE: u32 = u32::MAX;

/// A capturing lambda's closure object (F2): the header word is the
/// body's FuncRef, then the capture block inline. The object owns the
/// moved-in capture words' counts (affine words move at construction,
/// copyable words copy), so a unique death drops one count per `Ref`
/// capture — the same retain contract a list carries over its elements.
#[derive(Debug)]
struct Closure {
    /// The header word: the body's FuncRef, resolved through the
    /// two-slot module table at call time [H6 item 5].
    func: FuncRef,
    /// The capture block: the moved-in binding words.
    captures: Vec<Value>,
}

/// A string object's live payload: the text plus the char-boundary map
/// the char-indexed queries (`substring`, `char_at`, the string `len`)
/// build on their first access, and the content hash a map probe filters
/// on. Re-deriving char boundaries costs O(string length) per call, so one
/// map per object serves every later access; interned strings share one
/// object, hence one map and one hash.
#[derive(Debug)]
struct Str {
    text: Box<str>,
    /// `None` until the first char-indexed query, which records the char
    /// count and (a range query) the boundary offsets.
    chars: Option<CharIndex>,
    /// `None` until the first map probe or store hashes the content, which
    /// then answers every later one: the text never changes after
    /// allocation, so the cache cannot go stale. A cell because a probe
    /// holds the store shared.
    hash: Cell<Option<u32>>,
}

/// Char-boundary metadata: answers the char count, and maps a char
/// position to its byte offset in O(1) once a range query built the
/// boundary table.
#[derive(Debug)]
enum CharIndex {
    /// Pure ASCII: char index == byte index and the char count is the
    /// byte length, so nothing is stored.
    Ascii,
    /// Multi-byte text, where byte offsets diverge from char indexes:
    /// the char count, plus the byte offset of every char boundary
    /// through the end (count + 1 entries, the last at the byte length)
    /// once a range query built it. A count query allocates nothing.
    Utf8 {
        count: usize,
        offsets: Option<Box<[u32]>>,
    },
}

/// One heap object payload: the block body behind a live table id. One
/// payload type serves every kind, so every block shares one layout —
/// one payload offset, one class in ds-rt's reuse pool, one death hook
/// (the payload's own drop glue through [`drop_deinit`]).
#[derive(Debug)]
enum Object {
    /// A string: the text plus its lazily built char-boundary map.
    Str(Str),
    /// A bytes value: the immutable byte string, `string`'s shape over
    /// raw bytes — one counted block behind one `Ref` word carrying the
    /// whole payload, never a produced arena temporary. Owns no `Ref`
    /// words, so its death is the plain dec.
    Bytes(Box<[u8]>),
    /// A closure object: one heap object behind one `Ref` word (F2) —
    /// the value a capturing lambda's slot holds.
    Closure(Closure),
    /// A list: one heap object — len, capacity, and the element words
    /// inline in a single block payload, at a per-list element stride
    /// (one `Value` word per element word, a multi-word element stored as
    /// its whole inline span). The one in-place mutation is the append
    /// behind list literals ([`Objects::push_list`], the
    /// compiler-disciplined construction append over a freshly built
    /// list); every other mutation stays reserved for the exclusive
    /// windows, and growth is the backing buffer's amortized doubling.
    List(ListData),
    /// A map: one heap object behind one `Ref` word, the list object's
    /// conventions over key-value pairs. The internal shape is an
    /// association vector scanned front to back — the interim the
    /// builder-call construction story needs (no map literal yet), with
    /// no ordering promise and no iteration surface. Key matching:
    /// string keys compare by content (two equal-content string objects
    /// are one key), every other word by identity — so scalar and
    /// reference keys never collide across kinds. Each row carries a
    /// filter hash behind a row index ([`MapData`]) so a probe reads a few
    /// dense index words and loads a candidate key's payload only on a
    /// filter hit.
    Map(MapData),
    /// A bounded channel (the 2026-11-21 shared-state ruling): one heap
    /// object behind one `Ref` word on the creating fiber's store — the
    /// one owning heap the ruling names — whose payload is the FIFO
    /// buffer plus the scheduler's waiter queues. The buffered payload
    /// words own one edge count each exactly like list elements (a
    /// foreign element's inc posts through the pending queue); a parked
    /// sender's staged payload owns none (the parked frame keeps its own
    /// count, the parked-send discipline the net send wait set), so the
    /// death cascade decs the buffer alone.
    Chan(ChanData),
    /// The read-side aliasing container: one heap object behind one
    /// `Ref` word on the owning fiber's store, the slot array with its
    /// per-slot generation counters the whole payload. Live element
    /// words own one edge count each exactly like list elements (a
    /// foreign element's inc posts through the pending queue), released
    /// at the remove or by the death cascade.
    Arena(ArenaData),
}

/// A channel's payload: the bound, the FIFO buffer, and the fibers
/// parked on full and empty (the 2026-11-21 shared-state ruling). The
/// waiter queues are scheduler state the channel itself owns — a send
/// with room wakes the head receiver, a receive with stock wakes the
/// head sender — so a wake needs no reactor interest and no deadline:
/// the waking fiber is the parked one's resumer, and the executor's
/// run queue carries the hand-off.
#[derive(Debug)]
pub(crate) struct ChanData {
    /// The bound (`chan_new`'s capacity; zero is the rendezvous bound —
    /// the buffer holds nothing and every hand-off is a direct pairing
    /// of a parked sender's staged span with a receiving fiber).
    pub(crate) cap: usize,
    /// The buffered payload words, FIFO — `stride` consecutive words per
    /// buffered element.
    pub(crate) elems: VecDeque<Value>,
    /// How many consecutive words one payload element spans; 0 until
    /// the first deposit fixes it, sticky across drains (the buffer
    /// empties while the payload type's width stays the channel's own
    /// fact, so a drained channel keeps trapping a disagreeing width).
    /// The list element stride's exact sibling.
    pub(crate) stride: usize,
    /// The parked senders in park order, each with its staged payload
    /// span (the frame keeps the counts; the entry is a borrow).
    pub(crate) senders: VecDeque<ChanSender>,
    /// The parked receivers in park order.
    pub(crate) receivers: VecDeque<FiberId>,
}

/// The arena's slot payload (the read-side aliasing container): one
/// element word per live slot, a per-slot generation counter that
/// retires the handles of removed elements, and the retired slots'
/// free list. The slot array grows by amortized doubling; a removed
/// slot tombstones (its word leaves first) and its generation bumps at
/// reuse, so a stale handle — a copy taken before the removal — names
/// the slot's old generation and never the reused element.
#[derive(Debug, Default)]
pub(crate) struct ArenaData {
    /// One slot per allocated index: the generation plus the element
    /// word (`None` on a retired slot awaiting reuse).
    pub(crate) slots: Vec<ArenaSlot>,
    /// The retired slots' free list, last-retired-first-reused.
    pub(crate) free: Vec<u32>,
    /// The live element count.
    pub(crate) live: usize,
}

/// One arena slot: the generation counter plus the stored element word.
#[derive(Debug)]
pub(crate) struct ArenaSlot {
    /// The generation this slot's current occupant inserted under; the
    /// removal bumps it ahead of the tombstone, so every stale copy
    /// fails the check at once and the next reuse answers handles
    /// under the bumped one.
    pub(crate) generation: u32,
    /// The stored element word; `None` while the slot sits retired.
    pub(crate) word: Option<Value>,
}

/// One parked sender: the fiber and its staged payload span.
#[derive(Debug)]
pub(crate) struct ChanSender {
    /// The parked sending fiber.
    pub(crate) fiber: FiberId,
    /// The payload's whole word span staged at park time (each word's
    /// count stays the parked frame's).
    pub(crate) payload: Vec<Value>,
}

/// One popped channel element: the one-word element answers without
/// allocation, a wider element as its owned span.
#[derive(Debug, PartialEq)]
pub enum ChanPop {
    /// A one-word element.
    One(Value),
    /// A wider element's whole word span.
    Span(Vec<Value>),
}

/// A map's payload: the keys, the flattened value words at a per-map
/// value stride, and the row index over the per-row filter hashes. The
/// stride is the list element stride's exact sibling — the per-map
/// constant taken from `map[k, v]`'s compile-time value shape, fixed by
/// the first store and repeated by every later one — because a map value
/// spans as many words as its type's inline span (a two-word union value
/// rides two words per row). Keys stay one word: the probe resolution and
/// the row index hash whole words, and no faced key type spans words yet.
/// An empty map carries stride 0 (the invariant: stride 0 iff empty), so
/// an empty map is stride-polymorphic until its first store.
#[derive(Debug, Default)]
struct MapData {
    /// The filter hash of each row, parallel to `keys`.
    hashes: Vec<u32>,
    /// The row index: one row number per slot, [`ROW_NONE`] when free,
    /// probed linearly from the hash. Its length is a power of two and at
    /// least twice the row count, so a probe always meets a free slot; it
    /// is empty until the map holds a row.
    slots: Vec<u32>,
    /// The key words, in entry order.
    keys: Vec<Value>,
    /// The value words, flattened: row `i`'s value at
    /// `i * vstride .. (i + 1) * vstride`.
    values: Vec<Value>,
    /// How many consecutive words one row's value spans; 0 iff the map is
    /// empty.
    vstride: usize,
}

/// A list's payload: the element words inline plus the per-list element
/// stride — how many consecutive words one element spans. The stride is
/// a per-list constant taken from `list[t]`'s compile-time shape: the
/// first append onto an empty list fixes it, later appends must repeat
/// it, and every reader (len, the checked read) divides or slices by it.
/// An empty list carries stride 0 (the invariant: stride 0 iff empty),
/// so an empty list is stride-polymorphic until its first element.
#[derive(Debug)]
struct ListData {
    elems: Vec<Value>,
    stride: usize,
}

/// The free-slot marker of a map's row index: `u32::MAX` is past every
/// index's capacity, so it names no row.
const ROW_NONE: u32 = u32::MAX;

/// One id-table entry: a live id names the header of its ds-rt block
/// (the payload sits at the block's fixed payload offset, and the kind
/// lives in the payload), or a freed id threads the free list.
#[derive(Clone, Copy, Debug)]
enum Slot {
    Live(NonNull<RcHeader>),
    Free(u32),
}

/// One cross-heap count operation the store cannot apply itself: the
/// word names another fiber's heap, so the store queues it and the
/// routing face drains it (the order within one cascade is preserved,
/// FIFO).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ForeignOp {
    /// Acquire one count on the owning heap.
    Inc(RefId),
    /// Release one count on the owning heap.
    Dec(RefId),
}

/// The death cascade's working buffers, reused across deaths so a
/// steady-state death allocates nothing: `dying` is the depth-first work
/// stack, `zeros` one level's children, and `words` the spill buffer a
/// map's interleaved pairs drain into. A list's or a closure's element
/// vector needs no spill — it moves out of the payload whole.
#[derive(Debug, Default)]
struct DeathScratch {
    dying: Vec<(u32, NonNull<RcHeader>)>,
    zeros: Vec<(u32, NonNull<RcHeader>)>,
    words: Vec<Value>,
}

/// One intern row: the canonical object holding the content plus whether
/// the map's own pin holds a count on it. A pinned entry never decs to
/// zero, so its object never frees and its row never unbinds — the exact
/// lifetime a `KSTR_BORROW` word borrows.
#[derive(Clone, Copy, Debug)]
struct Interned {
    id: u32,
    pinned: bool,
}

/// The intern map: content → its canonical object. Hashed by content
/// through a fixed-seed word hasher, so a load never pays the default
/// SipHash.
type InternMap = HashMap<Box<str>, Interned, BuildHasherDefault<InternHasher>>;

/// The intern map's content hasher: the same multiply-rotate round the
/// claim ledger uses, one per 64-bit word of key bytes. The seed is
/// fixed, never per-process random, so a fuzz run replays exactly.
#[derive(Default)]
struct InternHasher(u64);

impl InternHasher {
    fn mix(&mut self, word: u64) {
        self.0 = self.0.rotate_left(13) ^ word;
        self.0 = self.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

impl Hasher for InternHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // the length rides its own round, so zero padding a short tail
        // never aliases a shorter key
        self.mix(bytes.len() as u64);
        let (words, tail) = bytes.as_chunks::<8>();
        for word in words {
            self.mix(u64::from_le_bytes(*word));
        }
        if !tail.is_empty() {
            let mut padded = [0u8; 8];
            padded[..tail.len()].copy_from_slice(tail);
            self.mix(u64::from_le_bytes(padded));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.mix(n);
    }
}

/// The heap: the id table over ds-rt's counted blocks, the table's free
/// list threading freed ids, and the string intern map.
///
/// `Send` (moves whole with its fiber under the fork-join ownership
/// transfer, E3's 2026-11-15 ruling) but not `Sync`: the block headers
/// and counts are confined state, sound only while exactly one thread
/// holds the store — the same contract ds-rt's `Heap` carries.
pub struct Objects {
    /// The owning fiber: the local/foreign test every count mutation on
    /// a stored word runs against (the pool index is the word's owner
    /// axis, stable across slot reuse).
    owner: FiberId,
    interned: InternMap,
    table: Vec<Slot>,
    heap: Heap,
    free_head: Option<u32>,
    live: usize,
    pending: VecDeque<ForeignOp>,
    scratch: DeathScratch,
}

// SAFETY: the store's raw pointers (the table's block headers, the death
// scratch) point into the `Heap` that moves with the store, and the
// fork-join pool transfers the whole fiber — store, heap, and ledger —
// exclusively: the host pool slot is vacated for the drive's duration,
// so no second thread can reach the store while it is away.
unsafe impl Send for Objects {}

impl fmt::Debug for Objects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Objects")
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

impl Drop for Objects {
    /// The store dies whole: every live block disposes exactly once (its
    /// payload's Rust memory drops, the block pools or releases), with no
    /// count moves — sibling blocks are dying in the same walk, so owned
    /// words owe nothing.
    fn drop(&mut self) {
        for slot in &mut self.table {
            if let Slot::Live(header) = *slot {
                self.heap.rc_dispose::<Object>(header);
                *slot = Slot::Free(NO_FREE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fiber::{FIBER_ROOT, Fiber};

    /// The cross-heap queue: a dying list whose element lives on another
    /// fiber's heap queues its release on the dying store, and the router's
    /// drain lands it on the element's own heap. Both queues end empty, so
    /// every later routed call answers on the zero-length fast path.
    #[test]
    fn a_cross_heaps_death_drains_the_queued_release() {
        let other = FiberId(1);
        let mut current = Objects::new(FIBER_ROOT);
        let mut pool = vec![Fiber::empty(FIBER_ROOT), Fiber::empty(other)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT), ClaimTable::new(other)];
        // the element's birth count is the adopting list's edge
        let elem = pool[other.index()].objects.alloc_str("elem".to_owned());
        let word = RefId::new(other, elem);
        let list = current.alloc_list(vec![Value::Ref(word)]);
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        heaps
            .dec(RefId::new(FIBER_ROOT, list))
            .expect("the list's owner drops");
        assert!(
            current.is_empty() && current.queued() == 0,
            "the dying store queued the release and drained it"
        );
        assert!(
            pool[other.index()].objects.is_empty() && pool[other.index()].objects.queued() == 0,
            "the element's own heap took the release"
        );
    }

    /// The structural content compare [B9]: equal content answers true
    /// behind distinct objects — nested lists, a map's rows through the
    /// lookup face (string keys by content), strings and bytes by payload
    /// — a mismatch at any depth answers false, mixed object kinds never
    /// compare equal, and a cyclic structure answers instead of looping
    /// (the in-progress pair memo's assumed-equal rule).
    #[test]
    fn the_content_compare_walks_nested_collections_and_cycles() {
        let mut current = Objects::new(FIBER_ROOT);
        let s1 = current.intern("a");
        let s2 = current.alloc_str("a".to_owned());
        let b1 = current.alloc_bytes(b"x".to_vec().into_boxed_slice());
        let b2 = current.alloc_bytes(b"x".to_vec().into_boxed_slice());
        let inner1 =
            current.alloc_list(vec![Value::I64(1), Value::Ref(RefId::new(FIBER_ROOT, s1))]);
        let inner2 =
            current.alloc_list(vec![Value::I64(1), Value::Ref(RefId::new(FIBER_ROOT, s2))]);
        let inner_short = current.alloc_list(vec![Value::I64(1)]);
        let nest1 = current.alloc_list(vec![
            Value::Ref(RefId::new(FIBER_ROOT, inner1)),
            Value::Ref(RefId::new(FIBER_ROOT, b1)),
        ]);
        let nest2 = current.alloc_list(vec![
            Value::Ref(RefId::new(FIBER_ROOT, inner2)),
            Value::Ref(RefId::new(FIBER_ROOT, b2)),
        ]);
        let map1 = current.alloc_map();
        let map2 = current.alloc_map();
        let map_miss = current.alloc_map();
        for (obj, key) in [(map1, s1), (map2, s2)] {
            current
                .set_map(
                    obj,
                    Value::Ref(RefId::new(FIBER_ROOT, key)),
                    &ResolvedKey::Word,
                    &[Value::I64(7), Value::Ref(RefId::new(FIBER_ROOT, inner1))],
                )
                .expect("the row stores");
        }
        current
            .set_map(
                map_miss,
                Value::Ref(RefId::new(FIBER_ROOT, s2)),
                &ResolvedKey::Word,
                &[Value::I64(8), Value::Ref(RefId::new(FIBER_ROOT, inner2))],
            )
            .expect("the row stores");
        let mut pool = vec![Fiber::empty(FIBER_ROOT)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT)];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        let (w, m) = (
            |id: u32| RefId::new(FIBER_ROOT, id),
            |heaps: &mut Heaps, x: RefId, y: RefId| heaps.content_eq(x, y),
        );
        assert_eq!(m(&mut heaps, w(nest1), w(nest2)), Ok(true));
        assert_eq!(m(&mut heaps, w(nest1), w(nest1)), Ok(true));
        assert_eq!(m(&mut heaps, w(inner1), w(inner_short)), Ok(false));
        assert_eq!(m(&mut heaps, w(s1), w(b1)), Ok(false), "mixed kinds");
        assert_eq!(m(&mut heaps, w(map1), w(map2)), Ok(true));
        assert_eq!(m(&mut heaps, w(map1), w(map_miss)), Ok(false));
        // a self-referential list in both operands: the pair memo assumes
        // the re-encountered pair equal, so the walk answers instead of
        // looping
        let Value::Ref(cyc1) = heaps.new_list(vec![]) else {
            unreachable!("a fresh list is its reference word")
        };
        heaps
            .push_list(cyc1, &[Value::Ref(cyc1)])
            .expect("the self edge stores");
        let Value::Ref(cyc2) = heaps.new_list(vec![]) else {
            unreachable!("a fresh list is its reference word")
        };
        heaps
            .push_list(cyc2, &[Value::Ref(cyc2)])
            .expect("the self edge stores");
        assert_eq!(m(&mut heaps, cyc1, cyc2), Ok(true));
        assert_eq!(
            m(&mut heaps, cyc1, w(inner1)),
            Ok(false),
            "a list and a one-word list of distinct content"
        );
    }

    /// Float words ride the scalar `==` chain's bit-structural judge
    /// through the content walk too: a NaN element equals its copy behind
    /// distinct objects (the judge is reflexive over floats), a distinct
    /// NaN payload never equals the canonical one, and the two zeros
    /// compare unequal — the same answers the scalar `==` prints.
    #[test]
    fn float_elements_compare_bit_structurally_through_the_content_walk() {
        let mut current = Objects::new(FIBER_ROOT);
        let nan = current.alloc_list(vec![Value::F64(f64::NAN)]);
        let nan_copy = current.alloc_list(vec![Value::F64(f64::NAN)]);
        let payloaded = current.alloc_list(vec![Value::F64(f64::from_bits(0x7ff8_0000_0000_0001))]);
        let zero = current.alloc_list(vec![Value::F64(0.0)]);
        let zero_copy = current.alloc_list(vec![Value::F64(0.0)]);
        let neg_zero = current.alloc_list(vec![Value::F64(-0.0)]);
        let mut pool = vec![Fiber::empty(FIBER_ROOT)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT)];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        let w = |id: u32| RefId::new(FIBER_ROOT, id);
        assert_eq!(
            heaps.content_eq(w(nan), w(nan_copy)),
            Ok(true),
            "a NaN element equals its copy"
        );
        assert_eq!(
            heaps.content_eq(w(nan), w(payloaded)),
            Ok(false),
            "a distinct NaN payload is a distinct word"
        );
        assert_eq!(
            heaps.content_eq(w(zero), w(zero_copy)),
            Ok(true),
            "equal zero spellings compare equal"
        );
        assert_eq!(
            heaps.content_eq(w(zero), w(neg_zero)),
            Ok(false),
            "the two zeros are distinct words"
        );
    }

    /// The shared-borrow reads route cross-heap: a string borrowed from
    /// one heap compares by content against another heap's through the
    /// live door, the parked door serves a pool-resident word, and the
    /// compare queues no cross-heap operation (every store ends the walk
    /// with a quiet pending queue).
    #[test]
    fn shared_string_reads_compare_cross_heap() {
        let other = FiberId(1);
        let mut current = Objects::new(FIBER_ROOT);
        let mut pool = vec![Fiber::empty(FIBER_ROOT), Fiber::empty(other)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT), ClaimTable::new(other)];
        let local = current.alloc_str("cross heap text".to_owned());
        let foreign = pool[other.index()]
            .objects
            .alloc_str("cross heap text".to_owned());
        let wrong = pool[other.index()]
            .objects
            .alloc_str("different".to_owned());
        let (lw, fw, ww) = (
            RefId::new(FIBER_ROOT, local),
            RefId::new(other, foreign),
            RefId::new(other, wrong),
        );
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        assert_eq!(heaps.str_ref(lw), Ok("cross heap text"));
        assert_eq!(heaps.str_ref(fw), Ok("cross heap text"));
        assert_eq!(heaps.str_ref(ww), Ok("different"));
        assert_eq!(heaps.content_eq(lw, fw), Ok(true));
        assert_eq!(heaps.content_eq(lw, ww), Ok(false));
        assert_eq!(heaps.content_eq(fw, ww), Ok(false));
        assert_eq!(
            current.queued() + pool[other.index()].objects.queued(),
            0,
            "the reads and the compare queue no cross-heap operation"
        );
        // the parked door: the context store is the scratch, so every
        // word routes through the pool, the local fiber's own slot
        // included
        let pooled = pool[FIBER_ROOT.index()]
            .objects
            .alloc_str("pooled text".to_owned());
        let mut scratch = Objects::new(FIBER_ROOT);
        let heaps = Heaps::new(FIBER_ROOT, &mut scratch, &mut pool, &mut claims, false);
        assert_eq!(
            heaps.str_ref(RefId::new(FIBER_ROOT, pooled)),
            Ok("pooled text")
        );
        assert_eq!(heaps.str_ref(fw), Ok("cross heap text"));
    }

    /// The map rows' scratch reuse: two content-equal maps over many
    /// rows — fresh string-key and list-value objects each, so no pair
    /// shares identity — compare true, and a value swapped in one row,
    /// a row missing on the right, and a longer value span each answer
    /// false.
    #[test]
    fn the_map_compare_walks_many_rows_through_the_scratch() {
        const ROWS: i64 = 100;
        let mut current = Objects::new(FIBER_ROOT);
        let fill = |objects: &mut Objects, rows: i64, swap_last: bool| -> u32 {
            let map = objects.alloc_map();
            for i in 0..rows {
                let key = objects.alloc_str(format!("key {i}"));
                let text = objects.alloc_str(format!("value {i}"));
                let word = RefId::new(FIBER_ROOT, text);
                let list = objects.alloc_list(vec![Value::I64(i), Value::Ref(word)]);
                let value = if swap_last && i + 1 == rows {
                    objects.alloc_list(vec![Value::I64(i), Value::Ref(word), Value::I64(9)])
                } else {
                    list
                };
                objects
                    .set_map(
                        map,
                        Value::Ref(RefId::new(FIBER_ROOT, key)),
                        &ResolvedKey::Word,
                        &[Value::Ref(RefId::new(FIBER_ROOT, value))],
                    )
                    .expect("the row stores");
            }
            map
        };
        let (map_a, map_b) = (
            fill(&mut current, ROWS, false),
            fill(&mut current, ROWS, false),
        );
        let short = fill(&mut current, ROWS - 1, false);
        let swapped = fill(&mut current, ROWS, true);
        let mut pool = vec![Fiber::empty(FIBER_ROOT)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT)];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        let w = |id: u32| RefId::new(FIBER_ROOT, id);
        assert_eq!(heaps.content_eq(w(map_a), w(map_b)), Ok(true));
        assert_eq!(heaps.content_eq(w(map_a), w(map_a)), Ok(true));
        assert_eq!(
            heaps.content_eq(w(map_a), w(short)),
            Ok(false),
            "a missing row"
        );
        assert_eq!(
            heaps.content_eq(w(map_a), w(swapped)),
            Ok(false),
            "one swapped value"
        );
    }

    /// The shared-borrow bytes reads route cross-heap: a payload
    /// borrowed from one heap compares by content against another heap's
    /// through the live door, the parked door serves a pool-resident
    /// word, and the reads queue no cross-heap operation.
    #[test]
    fn shared_bytes_reads_compare_cross_heap() {
        let other = FiberId(1);
        let mut current = Objects::new(FIBER_ROOT);
        let mut pool = vec![Fiber::empty(FIBER_ROOT), Fiber::empty(other)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT), ClaimTable::new(other)];
        let local = current.alloc_bytes(b"cross heap bytes".to_vec().into_boxed_slice());
        let foreign = pool[other.index()]
            .objects
            .alloc_bytes(b"cross heap bytes".to_vec().into_boxed_slice());
        let wrong = pool[other.index()]
            .objects
            .alloc_bytes(b"different".to_vec().into_boxed_slice());
        let (lw, fw, ww) = (
            RefId::new(FIBER_ROOT, local),
            RefId::new(other, foreign),
            RefId::new(other, wrong),
        );
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        assert_eq!(heaps.bytes_ref(lw), Ok(&b"cross heap bytes"[..]));
        assert_eq!(heaps.bytes_ref(fw), Ok(&b"cross heap bytes"[..]));
        assert_eq!(heaps.bytes_ref(ww), Ok(&b"different"[..]));
        assert_eq!(heaps.content_eq(lw, fw), Ok(true));
        assert_eq!(heaps.content_eq(lw, ww), Ok(false));
        assert_eq!(heaps.content_eq(fw, ww), Ok(false));
        assert_eq!(
            current.queued() + pool[other.index()].objects.queued(),
            0,
            "the reads and the compare queue no cross-heap operation"
        );
        // the parked door: the context store is the scratch, so every
        // word routes through the pool, the local fiber's own slot
        // included
        let pooled = pool[FIBER_ROOT.index()]
            .objects
            .alloc_bytes(b"pooled bytes".to_vec().into_boxed_slice());
        let mut scratch = Objects::new(FIBER_ROOT);
        let heaps = Heaps::new(FIBER_ROOT, &mut scratch, &mut pool, &mut claims, false);
        assert_eq!(
            heaps.bytes_ref(RefId::new(FIBER_ROOT, pooled)),
            Ok(&b"pooled bytes"[..])
        );
        assert_eq!(heaps.bytes_ref(fw), Ok(&b"cross heap bytes"[..]));
    }

    /// The map compare's cross-heap rows: two content-equal maps on
    /// different heaps, each keyed by its own local strings — every row
    /// of the walk resolves the left map's key against the right map's
    /// heap, borrowing the probe text (no owned copy per row) — answer
    /// true in both directions, a swapped value and a missing row
    /// answer false, the read funnel serves a foreign probe, and every
    /// queue ends the walk quiet.
    #[test]
    fn the_map_compare_resolves_foreign_keys_by_borrowing() {
        const ROWS: usize = 50;
        let other = FiberId(1);
        let mut current = Objects::new(FIBER_ROOT);
        let mut pool = vec![Fiber::empty(FIBER_ROOT), Fiber::empty(other)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT), ClaimTable::new(other)];
        let fill = |objects: &mut Objects, rows: usize, swap_last: bool| -> u32 {
            let map = objects.alloc_map();
            for i in 0..rows {
                let key = objects.alloc_str(format!("key {i}"));
                let last = swap_last && i + 1 == rows;
                let at = i64::try_from(i).expect("the row index fits");
                objects
                    .set_map(
                        map,
                        Value::Ref(RefId::new(objects.owner(), key)),
                        &ResolvedKey::Word,
                        &[Value::I64(if last { 9 } else { at })],
                    )
                    .expect("the row stores");
            }
            map
        };
        let (map_a, map_b) = (fill(&mut current, ROWS, false), {
            let store = &mut pool[other.index()].objects;
            fill(store, ROWS, false)
        });
        let foreign_key = pool[other.index()].objects.alloc_str("key 3".to_owned());
        let short = fill(&mut current, ROWS - 1, false);
        let swapped = fill(&mut current, ROWS, true);
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        let (a, b) = (RefId::new(FIBER_ROOT, map_a), RefId::new(other, map_b));
        assert_eq!(heaps.content_eq(a, b), Ok(true));
        assert_eq!(heaps.content_eq(b, a), Ok(true));
        assert_eq!(
            heaps.content_eq(a, RefId::new(FIBER_ROOT, short)),
            Ok(false),
            "a missing row"
        );
        assert_eq!(
            heaps.content_eq(a, RefId::new(FIBER_ROOT, swapped)),
            Ok(false),
            "one swapped value"
        );
        // the read funnel resolves a probe foreign to the map through
        // the same borrowed face: the left map's key finds the right
        // map's equal-content row
        assert_eq!(
            heaps.map_at(a, Value::Ref(RefId::new(other, foreign_key))),
            Ok(Some(vec![Value::I64(3)]))
        );
        assert_eq!(
            current.queued() + pool[other.index()].objects.queued(),
            0,
            "the walk queues no cross-heap operation"
        );
    }

    /// The store seam's key-localness gate: a fresh row under a word
    /// foreign to the map's heap refuses before any count moves — the
    /// stored key's payload is readable only on the map's own store, so
    /// the row would answer no content probe from any heap. Both foreign
    /// classes refuse (a string, the content class, and a list, the
    /// identity class), the map stays empty, the foreign words keep
    /// their birth counts, and no queue holds a pending operation.
    #[test]
    fn a_fresh_row_refuses_a_key_word_foreign_to_the_maps_heap() {
        let other = FiberId(1);
        let mut current = Objects::new(FIBER_ROOT);
        let mut pool = vec![Fiber::empty(FIBER_ROOT), Fiber::empty(other)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT), ClaimTable::new(other)];
        let map = current.alloc_map();
        let foreign_str = pool[other.index()].objects.alloc_str("key".to_owned());
        let foreign_list = pool[other.index()].objects.alloc_list(vec![Value::I64(1)]);
        let (str_word, list_word) = (
            RefId::new(other, foreign_str),
            RefId::new(other, foreign_list),
        );
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        for key in [Value::Ref(str_word), Value::Ref(list_word)] {
            assert_eq!(
                heaps.set_map(RefId::new(FIBER_ROOT, map), key, &[Value::I64(1)]),
                Err(Trap::ForeignMapKey {
                    obj: map,
                    key: match key {
                        Value::Ref(word) => word,
                        _ => unreachable!("the fixture stages reference words"),
                    },
                }),
                "the fresh row refuses the foreign key word"
            );
        }
        assert_eq!(
            heaps.map_len(RefId::new(FIBER_ROOT, map)),
            Ok(0),
            "no row landed"
        );
        assert_eq!(
            pool[other.index()].objects.rc_at(foreign_str),
            Ok(1),
            "the refused key kept its birth count"
        );
        assert_eq!(
            current.queued() + pool[other.index()].objects.queued(),
            0,
            "the refusals queue no cross-heap operation"
        );
    }

    /// What the gate leaves legal: a content-equal foreign probe
    /// overwrites an existing row (the row keeps its local key word, so
    /// every later probe finds it), a foreign value word stores into a
    /// fresh row (values carry no probe, counts route cross-heap), a
    /// local identity key's own word finds its row, and the read funnel
    /// keeps serving foreign probes by content.
    #[test]
    fn foreign_probes_overwrite_and_foreign_values_stay_storable() {
        let other = FiberId(1);
        let mut current = Objects::new(FIBER_ROOT);
        let mut pool = vec![Fiber::empty(FIBER_ROOT), Fiber::empty(other)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT), ClaimTable::new(other)];
        let map = current.alloc_map();
        let local_key = current.alloc_str("key 3".to_owned());
        let local_word = RefId::new(FIBER_ROOT, local_key);
        current
            .set_map(map, Value::Ref(local_word), &ResolvedKey::Word, &[
                Value::I64(3),
            ])
            .expect("the local row stores");
        let foreign_eq = pool[other.index()].objects.alloc_str("key 3".to_owned());
        let foreign_val = pool[other.index()].objects.alloc_list(vec![Value::I64(7)]);
        let second_key = current.alloc_str("row two".to_owned());
        let identity_key = current.alloc_list(vec![Value::I64(5)]);
        let identity_word = RefId::new(FIBER_ROOT, identity_key);
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        let map_word = RefId::new(FIBER_ROOT, map);
        // the foreign equal-content probe overwrites: the row keeps its
        // local key word
        heaps
            .set_map(map_word, Value::Ref(RefId::new(other, foreign_eq)), &[
                Value::I64(9),
            ])
            .expect("the content-equal foreign probe overwrites");
        assert_eq!(
            heaps.map_at(map_word, Value::Ref(local_word)),
            Ok(Some(vec![Value::I64(9)]))
        );
        assert_eq!(
            heaps.map_keys(map_word),
            Ok(vec![Value::Ref(local_word)]),
            "the row kept its original local key"
        );
        // a foreign value word stores into a fresh row under a local key
        heaps
            .set_map(map_word, Value::Ref(RefId::new(FIBER_ROOT, second_key)), &[
                Value::Ref(RefId::new(other, foreign_val)),
            ])
            .expect("a foreign value word stores");
        assert_eq!(heaps.map_len(map_word), Ok(2));
        // the read funnel keeps serving the foreign probe by content
        assert_eq!(
            heaps.map_at(map_word, Value::Ref(RefId::new(other, foreign_eq))),
            Ok(Some(vec![Value::I64(9)]))
        );
        // a local identity key's own word finds its row
        heaps
            .set_map(map_word, Value::Ref(identity_word), &[Value::Bool(true)])
            .expect("the local identity key stores");
        assert_eq!(
            heaps.map_at(map_word, Value::Ref(identity_word)),
            Ok(Some(vec![Value::Bool(true)]))
        );
        assert_eq!(heaps.map_len(map_word), Ok(3));
        assert_eq!(
            pool[other.index()].objects.rc_at(foreign_val),
            Ok(2),
            "the stored foreign value gained the map's count"
        );
    }

    #[test]
    fn intern_shares_by_content_and_alloc_does_not() {
        let mut objects = Objects::new(FIBER_ROOT);
        let a = objects.intern("localhost");
        let b = objects.intern("local");
        let c = objects.alloc_str("host".to_owned());
        let d = objects.concat(b, c).expect("both strings live");
        assert_eq!(
            objects.intern("localhost"),
            a,
            "interning shares one object and acquires a count"
        );
        assert_eq!(objects.rc_at(a), Ok(2));
        assert_ne!(
            d, a,
            "concatenation never interns: identity differs, content matches"
        );
        assert_eq!(objects.str_at(d), Ok("localhost"));
        assert_eq!(objects.len(), 4);
    }

    #[test]
    fn bogus_references_name_no_string() {
        let mut objects = Objects::new(FIBER_ROOT);
        let id = objects.intern("x");
        assert_eq!(
            objects.str_at(id + 1),
            Err(Trap::UnknownObject { obj: id + 1 })
        );
        assert_eq!(
            objects.concat(id, id + 1),
            Err(Trap::UnknownObject { obj: id + 1 })
        );
        assert_eq!(
            objects.inc(id + 1),
            Err(Trap::UnknownObject { obj: id + 1 })
        );
    }

    /// A count query records the char count alone; a range query then builds
    /// the boundary table the char indexes read through, and the table
    /// answers the multi-byte offsets.
    #[test]
    fn a_char_count_leaves_the_boundary_table_unbuilt() {
        let mut objects = Objects::new(FIBER_ROOT);
        let word = objects.alloc_str("héllo".to_owned());
        assert_eq!(objects.char_count(word), Ok(5));
        match objects.payload_of(word) {
            Some(Object::Str(s)) => match &s.chars {
                Some(CharIndex::Utf8 { count, offsets }) => {
                    assert_eq!(*count, 5);
                    assert!(offsets.is_none(), "the count query allocated no table");
                },
                other => panic!("a multi-byte index, got {other:?}"),
            },
            other => panic!("a string object, got {other:?}"),
        }
        assert_eq!(objects.char_byte_range(word, 1, 3), Ok((1, 4)));
        match objects.payload_of(word) {
            Some(Object::Str(s)) => match &s.chars {
                Some(CharIndex::Utf8 { offsets, .. }) => {
                    assert_eq!(offsets.as_deref(), Some([0, 1, 3, 4, 5, 6].as_slice()));
                },
                other => panic!("a multi-byte index, got {other:?}"),
            },
            other => panic!("a string object, got {other:?}"),
        }
        // pure ASCII needs no table at all, and both answers follow the bytes
        let ascii = objects.alloc_str("hello".to_owned());
        assert_eq!(objects.char_count(ascii), Ok(5));
        assert_eq!(objects.char_byte_range(ascii, 1, 3), Ok((1, 3)));
        match objects.payload_of(ascii) {
            Some(Object::Str(s)) => assert!(matches!(s.chars, Some(CharIndex::Ascii))),
            other => panic!("a string object, got {other:?}"),
        }
    }

    #[test]
    fn lists_hold_any_value_words_and_concat_shares_elements() {
        let mut objects = Objects::new(FIBER_ROOT);
        let a = objects.intern("a");
        let scalars = objects.alloc_list(vec![Value::I64(1), Value::Bool(false)]);
        let strings = objects.alloc_list(vec![Value::Ref(RefId::new(FIBER_ROOT, a))]);
        let joined = objects
            .concat_lists(scalars, strings)
            .expect("both lists live");
        assert_eq!(
            objects.list_at(joined),
            Ok(&[
                Value::I64(1),
                Value::Bool(false),
                Value::Ref(RefId::new(FIBER_ROOT, a))
            ][..]),
            "element words copied in order, any kind"
        );
        // the joined list owns one acquired count of its shared element
        assert_eq!(objects.rc_at(a), Ok(2));
        // a string read over a list object names the wrong kind
        assert_eq!(
            objects.str_at(joined),
            Err(Trap::UnknownObject { obj: joined })
        );
        // a list read over a string object does too
        assert_eq!(objects.list_at(a), Err(Trap::UnknownObject { obj: a }));
        // concat over a non-list reports the bogus side
        assert_eq!(
            objects.concat_lists(scalars, a),
            Err(Trap::UnknownObject { obj: a })
        );
    }

    /// The borrow intern's pin: KSTR_BORROW claims no count, so the map
    /// owns the canonical one — an acquiring claimant's dec-to-zero churn
    /// never frees the object, the intern row never unbinds, and both
    /// load spellings serve the one id.
    #[test]
    fn a_borrow_intern_pins_the_canonical_copy_through_claimant_churn() {
        let mut objects = Objects::new(FIBER_ROOT);
        let borrowed = objects.intern_borrow("held");
        assert_eq!(objects.rc_at(borrowed), Ok(1), "the map owns the pin alone");
        // an acquiring load of the same content serves the pinned object
        let acquired = objects.intern("held");
        assert_eq!(acquired, borrowed, "one object per distinct content");
        assert_eq!(objects.rc_at(borrowed), Ok(2), "the pin plus the claimant");
        // the claimant's death leaves the pin holding the object
        objects.dec(acquired).expect("the claimant drops");
        assert_eq!(
            objects.str_at(borrowed),
            Ok("held"),
            "the pin kept the object live"
        );
        assert_eq!(objects.len(), 1, "nothing else was born");
        // a second borrow load serves the same pinned id, claim-free
        let again = objects.intern_borrow("held");
        assert_eq!(again, borrowed);
        assert_eq!(
            objects.rc_at(borrowed),
            Ok(1),
            "a borrow load claims nothing"
        );
    }

    /// The pin over already-interned content: the first borrow of an
    /// unpinned entry incs once to become the map's canonical count, so
    /// the surviving claimants' decs can never strand a borrow word.
    #[test]
    fn a_first_borrow_pins_an_already_interned_entry() {
        let mut objects = Objects::new(FIBER_ROOT);
        let claimed = objects.intern("shared");
        assert_eq!(objects.rc_at(claimed), Ok(1), "the claimant alone");
        let borrowed = objects.intern_borrow("shared");
        assert_eq!(borrowed, claimed, "the borrow serves the live entry");
        assert_eq!(
            objects.rc_at(claimed),
            Ok(2),
            "the pin incs past the claimant's count"
        );
        // the claimant's dec leaves the pin: the object stays serving
        objects.dec(claimed).expect("the claimant drops");
        assert_eq!(objects.str_at(borrowed), Ok("shared"));
        assert_eq!(objects.rc_at(borrowed), Ok(1));
    }

    /// The death doors release the pins: a last pin decs to zero, frees,
    /// and unbinds the row (the next intern starts fresh), and a pin
    /// with a live claimant survives the release by exactly the
    /// claimant's count.
    #[test]
    fn release_pins_frees_last_pins_and_respects_claimants() {
        let mut objects = Objects::new(FIBER_ROOT);
        let a = objects.intern_borrow("solo");
        let b = objects.intern_borrow("kept");
        let claimant = objects.intern("kept");
        assert_eq!(objects.rc_at(b), Ok(2));
        objects.release_pins();
        assert_eq!(
            objects.str_at(a),
            Err(Trap::UnknownObject { obj: a }),
            "the last pin freed"
        );
        assert_eq!(
            objects.rc_at(b),
            Ok(1),
            "the claimant's count survives the release"
        );
        // the freed row unbound: a fresh borrow of the same content
        // allocates a new object — the freed id reuses for it (the
        // dec-to-alloc pairing), and the new pin holds the new block
        let fresh = objects.intern_borrow("solo");
        assert_eq!(fresh, a, "the freed slot is claimed by the next allocation");
        assert_eq!(objects.str_at(fresh), Ok("solo"));
        assert_eq!(
            objects.rc_at(fresh),
            Ok(1),
            "the fresh object carries the new pin"
        );
        // the claimant's death now frees the object and unbinds the row;
        // the re-interned pin needs its own door, and the drained store
        // proves the ledger of pins closed exactly
        objects.dec(claimant).expect("the claimant drops");
        assert!(
            !objects.is_empty(),
            "the re-interned pin still holds one object"
        );
        objects.release_pins();
        assert!(
            objects.is_empty(),
            "the store drains once every door released"
        );
    }

    /// A count reaching zero frees the object: the id reuses on the next
    /// allocation, a freed reference names no object, and a dying interned
    /// string unbinds its intern entry (the next load of the same content
    /// reborns fresh).
    #[test]
    fn a_unique_death_frees_and_the_slot_reuses() {
        let mut objects = Objects::new(FIBER_ROOT);
        let a = objects.intern("gone");
        assert_eq!(objects.rc_at(a), Ok(1));
        objects.dec(a).expect("the one owner drops");
        assert_eq!(objects.str_at(a), Err(Trap::UnknownObject { obj: a }));
        assert!(objects.is_empty(), "the heap drains");
        assert_eq!(objects.dec(a), Err(Trap::UnknownObject { obj: a }));
        // the reborn literal is a fresh object — but the freed id reuses
        // for it (the dec-to-alloc pairing)
        let b = objects.intern("gone");
        assert_eq!(a, b, "the freed id is claimed by the next allocation");
        assert_eq!(objects.str_at(b), Ok("gone"));
    }

    /// The death cascade runs on a work stack, not native recursion: a
    /// chain nested far deeper than any thread stack dies completely from
    /// one dropped count.
    #[test]
    fn a_deeply_nested_death_does_not_recurse_on_the_native_stack() {
        const DEPTH: usize = 500_000;
        let mut objects = Objects::new(FIBER_ROOT);
        // each level's birth count becomes its parent's stored count (the
        // list adopts the Ref word as-is), so the outermost's single count
        // owns the whole chain
        let mut node = objects.alloc_list(Vec::new());
        for _ in 1..DEPTH {
            node = objects.alloc_list(vec![Value::Ref(RefId::new(FIBER_ROOT, node))]);
        }
        objects.dec(node).expect("the outermost drops");
        assert!(objects.is_empty(), "the whole cascade died");
    }

    /// The cascade hands its working buffers back empty, so the next death
    /// reuses their capacity instead of allocating.
    #[test]
    fn the_death_cascade_hands_its_buffers_back_empty() {
        let mut objects = Objects::new(FIBER_ROOT);
        let map = objects.alloc_map();
        for i in 0..4 {
            let key = objects.alloc_str(format!("key{i}"));
            let value = objects.alloc_str(format!("value{i}"));
            objects
                .set_map(
                    map,
                    Value::Ref(RefId::new(FIBER_ROOT, key)),
                    &ResolvedKey::Word,
                    &[Value::Ref(RefId::new(FIBER_ROOT, value))],
                )
                .expect("the map accepts the pair");
            objects.dec(key).expect("the caller drops the key");
            objects.dec(value).expect("the caller drops the value");
        }
        let pairs = objects.map_len(map).expect("the map is live");
        objects.dec(map).expect("the map's owner drops");
        assert!(objects.is_empty(), "the pairs died with the map");
        assert!(
            objects.scratch.dying.is_empty()
                && objects.scratch.zeros.is_empty()
                && objects.scratch.words.is_empty(),
            "the cascade hands its buffers back empty"
        );
        assert!(
            objects.scratch.words.capacity() >= pairs * 2,
            "the spill buffer kept the drained pairs' capacity"
        );
    }

    /// A map stores and looks up by content keys: two equal-content
    /// string objects are one key, a scalar key never matches a string,
    /// and an overwrite replaces the value while keeping the entry count.
    #[test]
    fn maps_store_and_lookup_by_content_keys() {
        let mut objects = Objects::new(FIBER_ROOT);
        let interned = objects.intern("host");
        let fresh = objects.alloc_str("host".to_owned());
        assert_ne!(interned, fresh, "the fresh word is a distinct object");
        let map = objects.alloc_map();
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, interned)),
                &ResolvedKey::Word,
                &[Value::I64(80)],
            )
            .expect("the map object accepts the pair");
        // the fresh object's content names the interned key's entry
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, fresh)),
                &ResolvedKey::Word
            ),
            Ok(Some(&[Value::I64(80)][..])),
            "string keys match by content, not object identity"
        );
        assert_eq!(
            objects.map_at(map, Value::I64(80), &ResolvedKey::Word),
            Ok(None)
        );
        // a scalar key of its own
        objects
            .set_map(map, Value::I64(1), &ResolvedKey::Word, &[Value::Bool(true)])
            .expect("the map accepts scalars");
        assert_eq!(
            objects.map_at(map, Value::I64(1), &ResolvedKey::Word),
            Ok(Some(&[Value::Bool(true)][..]))
        );
        assert_eq!(objects.map_len(map), Ok(2));
        // the overwrite keeps one entry and drops the old value's count
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, fresh)),
                &ResolvedKey::Word,
                &[Value::I64(8080)],
            )
            .expect("the content-equal key overwrites");
        assert_eq!(objects.map_len(map), Ok(2));
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, interned)),
                &ResolvedKey::Word
            ),
            Ok(Some(&[Value::I64(8080)][..]))
        );
        // wrong-kind reads name no object
        assert_eq!(objects.list_at(map), Err(Trap::UnknownObject { obj: map }));
        assert_eq!(objects.str_at(map), Err(Trap::UnknownObject { obj: map }));
        // a set over a non-map reports the bogus side
        assert_eq!(
            objects.set_map(interned, Value::I64(1), &ResolvedKey::Word, &[Value::I64(
                2
            )]),
            Err(Trap::UnknownObject { obj: interned })
        );
    }

    /// The value channel carries whole word spans: a two-word value (the
    /// tag-plus-payload shape of a union value) stores, reads back, and
    /// overwrites one span per row, the first store fixing the stride a
    /// disagreeing later store traps on, and the map's death dropping one
    /// count per `Ref` word of every stored span.
    #[test]
    fn maps_carry_two_word_value_spans() {
        let mut objects = Objects::new(FIBER_ROOT);
        let map = objects.alloc_map();
        // a two-word union-shaped span: tag word plus one Ref payload
        let text = objects.intern("payload");
        let span = [Value::Tag(3), Value::Ref(RefId::new(FIBER_ROOT, text))];
        objects
            .set_map(map, Value::I64(1), &ResolvedKey::Word, &span)
            .expect("the first store fixes the two-word stride");
        assert_eq!(objects.map_len(map), Ok(1));
        assert_eq!(
            objects.map_at(map, Value::I64(1), &ResolvedKey::Word),
            Ok(Some(&span[..])),
            "the read hands back the whole span"
        );
        // the map's own count plus one per stored Ref word
        assert_eq!(objects.rc_at(text), Ok(2));
        // a disagreeing width traps, leaving the map untouched
        assert_eq!(
            objects.set_map(map, Value::I64(2), &ResolvedKey::Word, &[Value::I64(9)]),
            Err(Trap::ElementStride {
                obj: map,
                stride: 2,
                given: 1,
            })
        );
        assert_eq!(objects.map_len(map), Ok(1));
        // the overwrite trades the span's counts: the old payload drops,
        // the new one gains
        let other = objects.alloc_str("next".to_owned());
        let next = [Value::Tag(4), Value::Ref(RefId::new(FIBER_ROOT, other))];
        objects
            .set_map(map, Value::I64(1), &ResolvedKey::Word, &next)
            .expect("the stride agrees");
        assert_eq!(objects.rc_at(text), Ok(1), "the old span's count dropped");
        assert_eq!(objects.rc_at(other), Ok(2));
        // the death cascade drops one count per stored Ref word
        objects.dec(map).expect("the map's owner drops");
        assert_eq!(
            objects.rc_at(other),
            Ok(1),
            "only the caller's count remains"
        );
        objects
            .dec(text)
            .expect("the caller drops the first payload");
        objects
            .dec(other)
            .expect("the caller drops the stored payload");
        assert!(objects.is_empty(), "the span's payload died with the map");
    }

    /// The channel buffer carries whole word spans at a per-channel
    /// element stride: a multi-word payload deposits and pops as one
    /// element, the stride the first deposit fixes staying in force
    /// across the drain, a disagreeing width refusing before any count
    /// moves, and the death cascade dec'ing one count per stored `Ref`
    /// word of the buffer.
    #[test]
    fn channels_carry_multi_word_element_spans() {
        let mut objects = Objects::new(FIBER_ROOT);
        let text = objects.intern("payload");
        let chan = objects.alloc_chan(2);
        // the first deposit fixes the two-word stride and takes the
        // buffer's edge count per Ref word
        objects
            .chan_deposit(chan, &[
                Value::I64(1),
                Value::Ref(RefId::new(FIBER_ROOT, text)),
            ])
            .expect("the first deposit fixes the stride");
        assert_eq!(
            objects.rc_at(text),
            Ok(2),
            "the buffer's edge count per stored Ref word"
        );
        // a disagreeing width refuses before any count moves
        assert_eq!(
            objects.chan_deposit(chan, &[Value::I64(9)]),
            Err(Trap::ElementStride {
                obj: chan,
                stride: 2,
                given: 1
            })
        );
        assert_eq!(objects.rc_at(text), Ok(2));
        // the pop hands back the whole span, its count staying with the
        // words (the receiving frame's claim)
        assert_eq!(
            objects.chan_pop(chan),
            Ok(Some(ChanPop::Span(vec![
                Value::I64(1),
                Value::Ref(RefId::new(FIBER_ROOT, text))
            ])))
        );
        // the receiving frame consumes its claimed word: the buffer's
        // edge count became its backing, so its dec releases here
        objects
            .dec(text)
            .expect("the receiver consumes the claimed word");
        // the stride survives the drain: a narrower element still refuses
        assert_eq!(objects.chan_pop(chan), Ok(None));
        assert_eq!(
            objects.chan_deposit(chan, &[Value::I64(4)]),
            Err(Trap::ElementStride {
                obj: chan,
                stride: 2,
                given: 1
            })
        );
        // the one-word element rides the allocation-free pop arm
        let one = objects.alloc_chan(1);
        objects
            .chan_deposit(one, &[Value::I64(5)])
            .expect("a one-word channel takes its own stride");
        assert_eq!(objects.chan_pop(one), Ok(Some(ChanPop::One(Value::I64(5)))));
        objects
            .dec(one)
            .expect("the drained one-word channel drops");
        // the death cascade decs one count per stored Ref word
        objects
            .chan_deposit(chan, &[
                Value::I64(2),
                Value::Ref(RefId::new(FIBER_ROOT, text)),
            ])
            .expect("the same stride deposits again");
        assert_eq!(objects.rc_at(text), Ok(2));
        objects.dec(chan).expect("the channel's owner drops");
        assert_eq!(
            objects.rc_at(text),
            Ok(1),
            "only the caller's count remains"
        );
        objects.dec(text).expect("the caller drops its word");
        assert!(
            objects.is_empty(),
            "the span's payload died with the channel"
        );
    }

    /// The arena's slot discipline: insert answers packed
    /// `(index, generation)` handles, the read answers the stored word
    /// with no count move, remove retires the slot (its edge count
    /// releasing with it), reuse takes the retired slot first with the
    /// generation bumped — so a stale handle traps instead of naming the
    /// reused element — and the death cascade drops one count per live
    /// element word.
    #[test]
    fn arena_slots_retire_and_reuse_with_generation_bumps() {
        let mut objects = Objects::new(FIBER_ROOT);
        let text = objects.intern("payload");
        let arena = objects.alloc_arena();
        let first = objects
            .arena_insert(arena, Value::Ref(RefId::new(FIBER_ROOT, text)))
            .expect("the first element stores");
        assert_eq!(first, 0, "slot 0, generation 0");
        assert_eq!(
            objects.rc_at(text),
            Ok(2),
            "the arena's edge count beside the caller's word"
        );
        assert_eq!(objects.arena_len(arena), Ok(1), "one live element");
        assert_eq!(
            objects.arena_word(arena, first),
            Ok(Value::Ref(RefId::new(FIBER_ROOT, text)))
        );
        // a second insert takes a fresh slot, the index packing one past
        let second = objects
            .arena_insert(arena, Value::I64(9))
            .expect("the scalar element stores");
        assert_eq!(second >> 32, 0, "generation 0 again, a fresh slot");
        // the removal retires the first slot and releases its edge count
        objects
            .arena_remove(arena, first)
            .expect("the live handle removes");
        assert_eq!(objects.rc_at(text), Ok(1), "the edge count released");
        assert_eq!(objects.arena_len(arena), Ok(1));
        // the retired slot's stale handle traps with its own halves
        assert_eq!(
            objects.arena_word(arena, first),
            Err(Trap::DanglingArenaHandle {
                index: 0,
                generation: 0
            })
        );
        assert_eq!(
            objects.arena_remove(arena, first),
            Err(Trap::DanglingArenaHandle {
                index: 0,
                generation: 0
            })
        );
        // a never-held index traps the same deterministic trap
        assert_eq!(
            objects.arena_word(arena, (i64::from(7u32) << 32) | 0x63),
            Err(Trap::DanglingArenaHandle {
                index: 99,
                generation: 7
            })
        );
        // the reuse takes the retired slot first, its generation bumped
        let third = objects
            .arena_insert(arena, Value::I64(11))
            .expect("the reuse stores");
        assert_eq!(third, 1 << 32, "slot 0, generation 1");
        assert_eq!(objects.arena_word(arena, third), Ok(Value::I64(11)));
        // the death cascade drops one count per live element word, the
        // retired slot holding nothing
        objects.dec(arena).expect("the arena's owner drops");
        objects.dec(text).expect("the caller drops its word");
        assert!(
            objects.is_empty(),
            "the arena's payload died with its elements"
        );
    }

    /// Foreign probe resolution holds through the pre-resolved forms: a
    /// foreign string probe (the router resolved its content on the
    /// probe's own heap) finds a local string key by content and misses
    /// on different content, while a foreign non-string probe never
    /// matches a string or scalar key.
    #[test]
    fn foreign_probes_resolve_through_their_pre_resolution() {
        let mut objects = Objects::new(FIBER_ROOT);
        let key = objects.alloc_str("host".to_owned());
        let map = objects.alloc_map();
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Word,
                &[Value::I64(80)],
            )
            .expect("the local key stores");
        let foreign_text: Box<str> = "host".into();
        let hash = text_hash(&foreign_text);
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Text {
                    text: &foreign_text,
                    hash
                }
            ),
            Ok(Some(&[Value::I64(80)][..])),
            "a foreign string probe matches a local key by content"
        );
        let other_text: Box<str> = "port".into();
        let other_hash = text_hash(&other_text);
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Text {
                    text: &other_text,
                    hash: other_hash
                }
            ),
            Ok(None),
            "different content never matches"
        );
        let foreign_word = RefId::new(FiberId(7), key);
        assert_eq!(
            objects.map_at(map, Value::Ref(foreign_word), &ResolvedKey::Opaque),
            Ok(None),
            "an opaque foreign probe matches no string or scalar key"
        );
    }

    /// Word equality is the bit-structural scalar key rule, so `+0.0`
    /// and `-0.0` are two distinct keys: each spelling stores its own
    /// row, finds itself, and never finds the other, while an int and a
    /// float over equal-looking bytes stay distinct keys.
    #[test]
    fn signed_zero_scalar_keys_are_two_distinct_rows() {
        let mut objects = Objects::new(FIBER_ROOT);
        let map = objects.alloc_map();
        objects
            .set_map(map, Value::F64(0.0), &ResolvedKey::Word, &[Value::Bool(
                true,
            )])
            .expect("the positive zero stores");
        objects
            .set_map(map, Value::F64(-0.0), &ResolvedKey::Word, &[Value::Bool(
                false,
            )])
            .expect("the negative zero stores its own row");
        assert_eq!(objects.map_len(map), Ok(2), "the two zeros are two rows");
        assert_eq!(
            objects.map_at(map, Value::F64(0.0), &ResolvedKey::Word),
            Ok(Some(&[Value::Bool(true)][..])),
            "the positive zero finds its own row"
        );
        assert_eq!(
            objects.map_at(map, Value::F64(-0.0), &ResolvedKey::Word),
            Ok(Some(&[Value::Bool(false)][..])),
            "the negative zero finds its own row"
        );
        assert_eq!(
            objects.map_at(map, Value::I64(0), &ResolvedKey::Word),
            Ok(None),
            "an int key never matches a float key"
        );
    }

    /// A NaN scalar key is reflexive: one NaN payload stores one row, a
    /// second store over the same payload overwrites that row (the count
    /// stays one), the row reads back, and a distinct NaN payload is a
    /// different key with its own row.
    #[test]
    fn a_nan_scalar_key_round_trips_through_its_own_row() {
        let mut objects = Objects::new(FIBER_ROOT);
        let nan = f64::NAN;
        let payloaded = f64::from_bits(0x7ff8_0000_0000_0001);
        let map = objects.alloc_map();
        objects
            .set_map(map, Value::F64(nan), &ResolvedKey::Word, &[Value::I64(1)])
            .expect("the NaN key stores");
        objects
            .set_map(map, Value::F64(nan), &ResolvedKey::Word, &[Value::I64(2)])
            .expect("the same NaN payload overwrites its row");
        assert_eq!(objects.map_len(map), Ok(1), "the overwrite kept one row");
        assert_eq!(
            objects.map_at(map, Value::F64(nan), &ResolvedKey::Word),
            Ok(Some(&[Value::I64(2)][..])),
            "the NaN key finds its overwritten row"
        );
        objects
            .set_map(map, Value::F64(payloaded), &ResolvedKey::Word, &[
                Value::I64(3),
            ])
            .expect("the payloaded NaN stores its own row");
        assert_eq!(
            objects.map_len(map),
            Ok(2),
            "a distinct NaN payload is a distinct key"
        );
        assert_eq!(
            objects.map_at(map, Value::F64(payloaded), &ResolvedKey::Word),
            Ok(Some(&[Value::I64(3)][..])),
            "the payloaded NaN finds its own row"
        );
    }

    /// A string object hashes its content once for its whole life: the
    /// first probe builds the cache the later ones (and a store) answer
    /// from, and the cached value is the content's own hash, so a fresh
    /// equal-content object still finds the row.
    #[test]
    fn a_string_object_caches_its_content_hash() {
        let mut objects = Objects::new(FIBER_ROOT);
        let key = objects.alloc_str("cached".to_owned());
        let cached = |objects: &Objects| match objects.payload_of(key) {
            Some(Object::Str(s)) => s.hash.get(),
            other => panic!("a string object, got {other:?}"),
        };
        assert_eq!(cached(&objects), None, "nothing hashed the text yet");
        let map = objects.alloc_map();
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Word,
                &[Value::I64(1)],
            )
            .expect("the map accepts the string key");
        assert_eq!(
            cached(&objects),
            Some(text_hash("cached")),
            "the first probe cached the content hash"
        );
        // a second map answers from the same cache, and the fresh
        // equal-content object finds the row it stored
        let other = objects.alloc_map();
        let fresh = objects.alloc_str("cached".to_owned());
        objects
            .set_map(
                other,
                Value::Ref(RefId::new(FIBER_ROOT, fresh)),
                &ResolvedKey::Word,
                &[Value::I64(2)],
            )
            .expect("the second map accepts the same content");
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, fresh)),
                &ResolvedKey::Word
            ),
            Ok(Some(&[Value::I64(1)][..]))
        );
        assert_eq!(
            objects.map_at(
                other,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Word
            ),
            Ok(Some(&[Value::I64(2)][..]))
        );
        assert_eq!(cached(&objects), Some(text_hash("cached")));
    }

    /// A non-string reference key matches by identity, and the kinds never
    /// mix: the same word finds its row, another reference object misses,
    /// and neither class matches a key of the other.
    #[test]
    fn a_reference_key_matches_by_identity_alone() {
        let mut objects = Objects::new(FIBER_ROOT);
        let map = objects.alloc_map();
        let identity = objects.alloc_list(Vec::new());
        let text = objects.alloc_str("5".to_owned());
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, identity)),
                &ResolvedKey::Word,
                &[Value::I64(9)],
            )
            .expect("the map accepts a reference key");
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, text)),
                &ResolvedKey::Word,
                &[Value::I64(5)],
            )
            .expect("the map accepts a string key");
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, identity)),
                &ResolvedKey::Word
            ),
            Ok(Some(&[Value::I64(9)][..])),
            "the same reference word names its row"
        );
        let other_identity = objects.alloc_list(Vec::new());
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, other_identity)),
                &ResolvedKey::Word
            ),
            Ok(None),
            "another object of the same kind is a different key"
        );
        assert_eq!(
            objects.map_at(map, Value::I64(5), &ResolvedKey::Word),
            Ok(None),
            "a scalar key never matches a reference key"
        );
        // the identity probe over the string key misses: the classes never
        // mix, whatever the filter says
        let opaque = ResolvedKey::Opaque;
        assert_eq!(
            objects.map_at(map, Value::Ref(RefId::new(FIBER_ROOT, text)), &opaque),
            Ok(None),
            "an identity probe never matches a stored string key"
        );
    }

    /// The row index keeps every row reachable as the map grows: 80 rows
    /// pass the rebuilds, each answers through the index (a fresh
    /// equal-content string object included), the neighbouring keys miss,
    /// and a repeated key overwrites in place without a second row.
    #[test]
    fn the_row_index_holds_every_row_across_growth_and_overwrites() {
        let mut objects = Objects::new(FIBER_ROOT);
        let map = objects.alloc_map();
        for i in 0..40i64 {
            objects
                .set_map(map, Value::I64(i), &ResolvedKey::Word, &[Value::I64(i * 2)])
                .expect("the map accepts a scalar key");
            let key = objects.alloc_str(format!("key{i}"));
            objects
                .set_map(
                    map,
                    Value::Ref(RefId::new(FIBER_ROOT, key)),
                    &ResolvedKey::Word,
                    &[Value::I64(i)],
                )
                .expect("the map accepts a string key");
            objects.dec(key).expect("the caller drops its key count");
        }
        let shape = match objects.payload_of(map) {
            Some(Object::Map(data)) => {
                assert_eq!(data.keys.len(), 80);
                assert!(data.slots.len().is_power_of_two());
                assert!(
                    data.slots.len() >= data.keys.len() * 2,
                    "the load factor stays at most one half"
                );
                assert_eq!(
                    data.slots.iter().filter(|slot| **slot != ROW_NONE).count(),
                    data.keys.len(),
                    "every row claims exactly one index slot"
                );
                data.slots.len()
            },
            _ => panic!("the map is live"),
        };
        assert!(shape >= 160, "the index grew with the rows");
        for i in 0..40i64 {
            assert_eq!(
                objects.map_at(map, Value::I64(i), &ResolvedKey::Word),
                Ok(Some(&[Value::I64(i * 2)][..]))
            );
            assert_eq!(
                objects.map_at(map, Value::I64(i + 1000), &ResolvedKey::Word),
                Ok(None),
                "a key absent from the map misses"
            );
            // a fresh object of the same content names the stored row
            let hit = objects.alloc_str(format!("key{i}"));
            assert_eq!(
                objects.map_at(
                    map,
                    Value::Ref(RefId::new(FIBER_ROOT, hit)),
                    &ResolvedKey::Word
                ),
                Ok(Some(&[Value::I64(i)][..]))
            );
            objects.dec(hit).expect("the caller drops its probe count");
            let miss = objects.alloc_str(format!("missing{i}"));
            assert_eq!(
                objects.map_at(
                    map,
                    Value::Ref(RefId::new(FIBER_ROOT, miss)),
                    &ResolvedKey::Word
                ),
                Ok(None)
            );
            objects.dec(miss).expect("the caller drops its probe count");
        }
        // the repeated keys overwrite: the row count and the index hold
        for i in 0..40i64 {
            objects
                .set_map(map, Value::I64(i), &ResolvedKey::Word, &[Value::I64(-i)])
                .expect("the content-equal key overwrites");
        }
        assert_eq!(objects.map_len(map), Ok(80));
        assert_eq!(
            objects.map_at(map, Value::I64(9), &ResolvedKey::Word),
            Ok(Some(&[Value::I64(-9)][..])),
            "the overwrite answers under the original key"
        );
    }

    /// A shared list decs to zero without freeing; the unique death frees
    /// the list and drops its acquired element counts (a list of lists
    /// dies recursively).
    #[test]
    fn a_list_death_drops_its_element_counts() {
        let mut objects = Objects::new(FIBER_ROOT);
        // a fresh element whose birth count is the adopting list's (the
        // producer-native construction rule)
        let s = objects.alloc_str("elem".to_owned());
        let inner = objects.alloc_list(vec![Value::Ref(RefId::new(FIBER_ROOT, s))]);
        assert_eq!(objects.rc_at(s), Ok(1), "the adoption keeps the one count");
        let outer = objects.concat_lists(inner, inner).expect("both lists live");
        // the outer list acquired one count per copied element word
        assert_eq!(objects.rc_at(s), Ok(3));
        objects.dec(inner).expect("one owner of inner drops");
        assert_eq!(objects.rc_at(s), Ok(2), "inner still lives in outer");
        objects.dec(outer).expect("the outer's owner drops");
        assert!(objects.is_empty(), "the recursive death drains the heap");
    }

    /// A closure object carries its header FuncRef plus the moved-in
    /// capture block: a shared copy incs one count, the unique death
    /// drops one count per `Ref` capture, and wrong-kind reads name no
    /// object.
    #[test]
    fn closures_carry_the_header_and_retain_their_captures() {
        let mut objects = Objects::new(FIBER_ROOT);
        let id = objects.alloc_closure(FuncRef::new(3, 4), vec![Value::I64(9)]);
        assert_eq!(
            objects.closure_at(id),
            Ok((FuncRef::new(3, 4), &[Value::I64(9)][..])),
            "the header word and the capture block read back"
        );
        // a closure is not a string or a list
        assert_eq!(objects.str_at(id), Err(Trap::UnknownObject { obj: id }));
        assert_eq!(objects.list_at(id), Err(Trap::UnknownObject { obj: id }));
        // a string is not a closure
        let s = objects.intern("cap");
        assert_eq!(objects.closure_at(s), Err(Trap::UnknownObject { obj: s }));
        // the shared copy is one count on the closure itself [C4]
        objects.inc(id).expect("a shared lambda copy incs once");
        assert_eq!(objects.rc_at(id), Ok(2));
        objects.dec(id).expect("the shared copy drops");
        assert_eq!(objects.rc_at(id), Ok(1));
    }

    /// A closure's unique death frees the object and drops the moved-in
    /// capture counts: a `Ref` capture's object dies with it, the same
    /// retain contract a list carries.
    #[test]
    fn a_closure_death_drops_its_capture_counts() {
        let mut objects = Objects::new(FIBER_ROOT);
        let s = objects.alloc_str("cap".to_owned());
        // the moved-in word keeps its count: the closure owns it now
        let id = objects.alloc_closure(FuncRef::new(0, 0), vec![Value::Ref(RefId::new(
            FIBER_ROOT, s,
        ))]);
        assert_eq!(objects.rc_at(s), Ok(1));
        objects.dec(id).expect("the unique owner drops");
        assert_eq!(objects.rc_at(s), Err(Trap::UnknownObject { obj: s }));
        assert!(objects.is_empty(), "the capture died with the closure");
    }

    /// A map death drops one count per stored `Ref` word — keys and
    /// values both, the retain contract the list carries: the map's own
    /// acquired counts die with it, and the heap drains once the
    /// caller's counts drop too.
    #[test]
    fn a_map_death_drops_its_pair_counts() {
        let mut objects = Objects::new(FIBER_ROOT);
        let map = objects.alloc_map();
        let key = objects.alloc_str("host".to_owned());
        let value = objects.alloc_str("localhost".to_owned());
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Word,
                &[Value::Ref(RefId::new(FIBER_ROOT, value))],
            )
            .expect("the map accepts the pair");
        // the store acquired one count per stored Ref word, on top of the
        // caller's own
        assert_eq!(objects.rc_at(key), Ok(2));
        assert_eq!(objects.rc_at(value), Ok(2));
        // the overwrite drops the replaced value's acquired count and
        // keeps the key's
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Word,
                &[Value::I64(8080)],
            )
            .expect("the content-equal key overwrites");
        assert_eq!(objects.rc_at(value), Ok(1));
        assert_eq!(objects.rc_at(key), Ok(2));
        let other = objects.alloc_str("port".to_owned());
        objects
            .set_map(map, Value::I64(1), &ResolvedKey::Word, &[Value::Ref(
                RefId::new(FIBER_ROOT, other),
            )])
            .expect("the second entry adopts the scalar key");
        assert_eq!(objects.rc_at(other), Ok(2));
        // the death drops exactly the map's acquired counts, both pairs'
        // Ref words
        objects.dec(map).expect("the map's owner drops");
        assert_eq!(objects.rc_at(key), Ok(1));
        assert_eq!(objects.rc_at(value), Ok(1));
        assert_eq!(objects.rc_at(other), Ok(1));
        // the caller's own counts drop last: the heap drains
        objects.dec(key).expect("the caller drops the key");
        objects
            .dec(value)
            .expect("the caller drops the replaced value");
        objects
            .dec(other)
            .expect("the caller drops the second value");
        assert!(objects.is_empty(), "the pair words died with the map");
    }

    /// A concat over disagreeing non-empty strides names the right operand
    /// with its own stride and the width the left side staged -- lowered
    /// code cannot build the shape (the checker holds one element type
    /// per list value), so the trap is the hand-assembled frame's
    /// diagnostic and this pins what it says. The in-place pair runs
    /// first; the routed cross-store pair answers the same attribution.
    #[test]
    fn a_concat_of_disagreeing_strides_names_the_right_operand() {
        let mut objects = Objects::new(FIBER_ROOT);
        let one = objects.alloc_list(vec![Value::I64(1)]);
        let pair = objects.alloc_list_strided(vec![Value::I64(2), Value::I64(3)], 2);
        assert_eq!(
            objects.concat_lists(one, pair),
            Err(Trap::ElementStride {
                obj: pair,
                stride: 2,
                given: 1,
            })
        );

        let other = FiberId(1);
        let mut current = Objects::new(FIBER_ROOT);
        let mut pool = vec![Fiber::empty(FIBER_ROOT), Fiber::empty(other)];
        let mut claims = vec![ClaimTable::new(FIBER_ROOT), ClaimTable::new(other)];
        let front = current.alloc_list(vec![Value::I64(1)]);
        let back = pool[other.index()]
            .objects
            .alloc_list_strided(vec![Value::I64(2), Value::I64(3)], 2);
        let mut heaps = Heaps::new(FIBER_ROOT, &mut current, &mut pool, &mut claims, true);
        assert_eq!(
            heaps.concat_lists(RefId::new(FIBER_ROOT, front), RefId::new(other, back)),
            Err(Trap::ElementStride {
                obj: back,
                stride: 2,
                given: 1,
            })
        );
    }
}
