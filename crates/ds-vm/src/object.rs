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

impl Str {
    fn new(text: Box<str>) -> Self {
        Self {
            text,
            chars: None,
            hash: Cell::new(None),
        }
    }

    /// The content's filter hash, computed on the first ask.
    fn content_hash(&self) -> u32 {
        self.hash.get().unwrap_or_else(|| {
            let hash = text_hash(&self.text);
            self.hash.set(Some(hash));
            hash
        })
    }
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

impl CharIndex {
    /// One ASCII scan, then (multi-byte text only) one byte scan for the
    /// char count: a UTF-8 start byte is exactly a byte that is not a
    /// continuation (`b & 0xC0 != 0x80`), so no code point is decoded.
    fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Self::Ascii;
        }
        let count = text
            .as_bytes()
            .iter()
            .filter(|byte| **byte & 0xc0 != 0x80)
            .count();
        Self::Utf8 {
            count,
            offsets: None,
        }
    }

    /// The char count.
    fn count(&self, text: &str) -> usize {
        match self {
            Self::Ascii => text.len(),
            Self::Utf8 { count, .. } => *count,
        }
    }

    /// Whether the boundary offsets are present (pure ASCII needs none).
    fn has_offsets(&self) -> bool {
        match self {
            Self::Ascii => true,
            Self::Utf8 { offsets, .. } => offsets.is_some(),
        }
    }

    /// Records the boundary offsets a range query built.
    fn set_offsets(&mut self, built: Box<[u32]>) {
        match self {
            Self::Ascii => unreachable!("ASCII text indexes chars by byte"),
            Self::Utf8 { offsets, .. } => *offsets = Some(built),
        }
    }

    /// The byte offset of char boundary `index`, a valid char position
    /// (`index <= count`); a range query built the table.
    fn byte_at(&self, index: usize) -> usize {
        match self {
            Self::Ascii => index,
            Self::Utf8 { offsets, .. } => offsets
                .as_ref()
                .expect("a range query built the boundary offsets")[index]
                as usize,
        }
    }
}

/// The byte offsets of every char boundary of multi-byte `text` through
/// the end. A start byte is exactly a byte that is not a continuation,
/// so the table is one byte scan.
fn char_offsets(text: &str) -> Box<[u32]> {
    // byte offsets are u32 words: the length check bounds every entry
    let len = u32::try_from(text.len()).expect("string byte length fits u32");
    let mut offsets = Vec::with_capacity(text.len() / 2 + 1);
    for (at, &byte) in text.as_bytes().iter().enumerate() {
        if byte & 0xc0 != 0x80 {
            offsets.push(u32::try_from(at).expect("char offset below the byte length"));
        }
    }
    // the final entry is the end-of-text boundary a range end reaches
    offsets.push(len);
    offsets.into()
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

impl MapData {
    /// Records the row just pushed into `keys`, extending the index to
    /// the next power of two when the load factor would pass one half.
    fn index_push(&mut self) {
        let rows = self.keys.len();
        let capacity = if self.slots.len() >= rows * 2 {
            self.slots.len()
        } else {
            (rows * 2).next_power_of_two().max(8)
        };
        if capacity != self.slots.len() {
            self.slots.clear();
            self.slots.resize(capacity, ROW_NONE);
            // a rebuilt index re-places every row, the new one included
            for row in 0..self.hashes.len() {
                let hash = self.hashes[row];
                self.place(row, hash);
            }
            return;
        }
        self.place(rows - 1, self.hashes[rows - 1]);
    }

    /// Claims the first free slot at or past the hash of row `row`.
    fn place(&mut self, row: usize, hash: u32) {
        let row = u32::try_from(row).expect("a map row fits the index word");
        let mask = self.slots.len() - 1;
        let mut at = hash as usize & mask;
        while self.slots[at] != ROW_NONE {
            at = (at + 1) & mask;
        }
        self.slots[at] = row;
    }
}

/// FNV-1a over `bytes`, continuing `state` (one round per byte).
#[inline]
const fn fnv_step(mut state: u32, bytes: &[u8]) -> u32 {
    let mut at = 0;
    while at < bytes.len() {
        state ^= bytes[at] as u32;
        state = state.wrapping_mul(0x0100_0193);
        at += 1;
    }
    state
}

/// A key class's FNV seed: the offset basis folded once with the class
/// byte, so the classes stay apart without a round over that byte.
const fn class_seed(class: u8) -> u32 {
    (0x811c_9dc5 ^ class as u32).wrapping_mul(0x0100_0193)
}

/// Key equality classes, one seed each: a text hash, an identity hash, and
/// a scalar hash over the same bytes stay distinguishable.
const SEED_TEXT: u32 = class_seed(b't');
const SEED_REF: u32 = class_seed(b'r');
const SEED_SCALAR: u32 = class_seed(b's');
/// The float scalar class: a float key never equals an int key, so the two
/// kinds never share a hash.
const SEED_SCALAR_F64: u32 = class_seed(b'd');

/// The hash of a string key: its content, the text equality class.
fn text_hash(text: &str) -> u32 {
    fnv_step(SEED_TEXT, text.as_bytes())
}

/// The hash of an identity key: the reference word's own bits, one word.
fn ref_hash(word: RefId) -> u32 {
    let (fiber, id) = (word.fiber().0.to_le_bytes(), word.id().to_le_bytes());
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&fiber);
    bytes[4..].copy_from_slice(&id);
    fnv_step(SEED_REF, &bytes)
}

/// The one word-equality judge the structural `==` family rides [B9]:
/// floats compare bit-structurally (reflexive, total, `+0.0` and `-0.0`
/// distinct), every other word by its derived equality. Collection
/// content equality and map key matching answer through this judge, the
/// same one the scalar `==`/`in` chain serves.
fn word_eq(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::F64(x), Value::F64(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

/// The hash of a scalar key: bits that respect word equality — the float
/// word hashes its own bits, so word-equal (bit-structural) floats hash
/// equal and the filter never rejects an equal pair, while `+0.0` and
/// `-0.0` hash apart as the distinct keys they are (every other scalar
/// kind is bits-equal exactly when word-equal; a NaN key matches itself
/// alone, so its payload hashing with the bits is free).
fn scalar_hash(word: Value) -> u32 {
    // every scalar word is one 8-byte payload, so the class seed alone
    // carries the kind distinction
    let (payload, seed) = match word {
        Value::I64(n) => (n.to_le_bytes(), SEED_SCALAR),
        Value::F64(x) => (x.to_bits().to_le_bytes(), SEED_SCALAR_F64),
        Value::Bool(b) => (i64::from(b).to_le_bytes(), SEED_SCALAR),
        Value::Tag(t) => (u64::from(t).to_le_bytes(), SEED_SCALAR),
        Value::Arena(h) => (h.raw().to_le_bytes(), SEED_SCALAR),
        Value::Fn(f) => {
            let (module, func) = (f.module().to_le_bytes(), f.func().to_le_bytes());
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&module);
            bytes[4..].copy_from_slice(&func);
            (bytes, SEED_SCALAR)
        },
        // the probe resolution routes a Ref to the text or identity class
        // first; hashing its bits keeps this total for the dead case
        Value::Ref(r) => {
            let (fiber, id) = (r.fiber().0.to_le_bytes(), r.id().to_le_bytes());
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&fiber);
            bytes[4..].copy_from_slice(&id);
            (bytes, SEED_SCALAR)
        },
    };
    fnv_step(seed, &payload)
}

/// A map probe resolved once per lookup or store — the payload-local
/// form the row scan compares against. The probe word's payload is read
/// exactly here (a local word through this store, a foreign word through
/// the router's pre-resolution), so the per-candidate loop never walks
/// the probe's descriptor chain again; the classes mirror the key
/// equality rules exactly.
enum ProbeKey<'a> {
    /// A string probe: content equality, the text borrowed from its
    /// object (local) or the router's copy (foreign).
    Text { hash: u32, text: &'a str },
    /// A non-string reference probe: identity equality.
    Identity { hash: u32, word: RefId },
    /// A probe carrying no reference: word equality.
    Scalar { hash: u32, word: Value },
}

impl ProbeKey<'_> {
    /// The row filter hash: equal keys hash equal in every class, so a
    /// mismatch is a certain reject.
    fn hash(&self) -> u32 {
        match *self {
            Self::Text { hash, .. } | Self::Identity { hash, .. } | Self::Scalar { hash, .. } => {
                hash
            },
        }
    }
}

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

impl Objects {
    /// An empty heap owned by `owner`.
    #[must_use]
    pub fn new(owner: FiberId) -> Self {
        Self {
            owner,
            interned: InternMap::default(),
            table: Vec::new(),
            heap: Heap::default(),
            free_head: None,
            live: 0,
            pending: VecDeque::new(),
            scratch: DeathScratch::default(),
        }
    }

    /// The owning fiber (the pool index the store's words carry).
    #[must_use]
    pub fn owner(&self) -> FiberId {
        self.owner
    }

    /// Pops the oldest queued cross-heap operation, if any — the routing
    /// face's drain loop.
    pub(crate) fn pending_pop(&mut self) -> Option<ForeignOp> {
        self.pending.pop_front()
    }

    /// How many cross-heap operations wait on this store for the router.
    /// The length is the queue's own, so it cannot drift from it.
    #[must_use]
    pub(crate) fn queued(&self) -> usize {
        self.pending.len()
    }

    /// Acquires one count for a stored `Ref` word, routing by the word's
    /// owner: a local word incs here, a foreign word queues onto the
    /// pending list (the router drains it onto the owning heap).
    fn acquire_stored(&mut self, word: RefId) {
        if word.fiber() == self.owner {
            self.inc(word.id())
                .expect("a stored local reference is live");
        } else {
            self.pending.push_back(ForeignOp::Inc(word));
        }
    }

    /// Releases one count a stored `Ref` word carried, routing by the
    /// word's owner the same way [`Objects::acquire_stored`] acquires.
    fn release_stored(&mut self, word: RefId) {
        if word.fiber() == self.owner {
            self.dec(word.id())
                .expect("a stored local reference is live");
        } else {
            self.pending.push_back(ForeignOp::Dec(word));
        }
    }

    /// How many objects are live (a freed id counts only when reused).
    #[must_use]
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether the heap holds no live object.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The payload of the live object `obj` names, if it names one.
    fn payload_of(&self, obj: u32) -> Option<&Object> {
        match self.table.get(obj as usize) {
            Some(Slot::Live(header)) => Some(self.heap.payload::<Object>(*header)),
            _ => None,
        }
    }

    /// The block header of the live object `obj` names.
    fn header_of(&self, obj: u32) -> Result<NonNull<RcHeader>, Trap> {
        match self.table.get(obj as usize) {
            Some(Slot::Live(header)) => Ok(*header),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The live reference count of object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object.
    #[must_use = "the count is the witness"]
    pub fn rc_at(&self, obj: u32) -> Result<u32, Trap> {
        Ok(self.heap.rc_count(self.header_of(obj)?))
    }

    /// Claims one id for a fresh block of `object`: the free list first
    /// (the dec-to-alloc pairing), the table tail otherwise. The block's
    /// birth count is 1 — the caller owns the object.
    fn alloc(&mut self, object: Object) -> u32 {
        let header = self.heap.alloc_store(object, Some(drop_deinit::<Object>()));
        let id = if let Some(id) = self.free_head {
            let Slot::Free(next) = self.table[id as usize] else {
                unreachable!("the free list head is a freed slot");
            };
            self.free_head = (next != NO_FREE).then_some(next);
            self.table[id as usize] = Slot::Live(header);
            id
        } else {
            let id = u32::try_from(self.table.len()).expect("object table exceeds u32");
            self.table.push(Slot::Live(header));
            id
        };
        self.live += 1;
        id
    }

    /// Interns `text`: one live object per distinct content, so string
    /// constants and host-provided arguments share objects — and the
    /// claim acquires a count (each `KSTR` load owns its own reference).
    /// Concatenation (`alloc_str`) deliberately does not intern: its
    /// results are transient by construction and content equality is
    /// what EQSTR answers.
    pub fn intern(&mut self, text: &str) -> u32 {
        if let Some(&Interned { id, .. }) = self.interned.get(text) {
            self.inc(id).expect("an interned object is live");
            return id;
        }
        let id = self.alloc_str(text.to_owned());
        self.interned
            .insert(text.into(), Interned { id, pinned: false });
        id
    }

    /// Interns `text` for a borrow load (KSTR_BORROW): the caller claims
    /// no count, so the map owns the canonical one. A first intern of
    /// unpinned content allocates with the birth count staying the
    /// map's (the pin); already-interned content just gains the pin if
    /// it lacks one — an unpinned entry's remaining count belongs to
    /// its claimants and could dec away mid-borrow, so the pin inc keeps
    /// the object live whatever they do. A pinned object never decs to
    /// zero, so its row never unbinds and the id serves every later
    /// borrow load; the store's teardown frees it with the heap.
    pub fn intern_borrow(&mut self, text: &str) -> u32 {
        if let Some(&Interned { id, pinned }) = self.interned.get(text) {
            if !pinned {
                self.interned
                    .get_mut(text)
                    .expect("the row just read")
                    .pinned = true;
                self.inc(id).expect("an interned object is live");
            }
            return id;
        }
        let id = self.alloc_str(text.to_owned());
        self.interned
            .insert(text.into(), Interned { id, pinned: true });
        id
    }

    /// Releases the intern map's canonical pin counts: the owner's
    /// frames never run again (a death door or a completed drive), so
    /// no borrow word survives to need them. A pin whose object holds
    /// other claimants' counts survives their deaths; a last pin decs to
    /// zero, frees, and unbinds the row, so the next intern of the same
    /// content starts fresh. Idempotent — the pin bits clear as they
    /// collect, so the door and `die_current` may both fire.
    pub fn release_pins(&mut self) {
        let mut pinned = Vec::new();
        for row in self.interned.values_mut() {
            if row.pinned {
                row.pinned = false;
                pinned.push(row.id);
            }
        }
        for id in pinned {
            // a Str object carries no Ref payload, so the death cascade
            // never queues cross-heap work
            let _ = self.dec(id);
        }
    }

    /// Allocates a fresh string object (no interning), owned by the caller.
    pub fn alloc_str(&mut self, text: String) -> u32 {
        self.alloc(Object::Str(Str::new(text.into())))
    }

    /// Allocates a fresh string object holding `part`, owned by the
    /// caller. The empty answers of `substring` and `char_at` copy once
    /// through here instead of staging a `String` (which would copy
    /// twice).
    pub fn alloc_str_part(&mut self, part: &str) -> u32 {
        self.alloc(Object::Str(Str::new(part.into())))
    }

    /// Allocates a fresh string object holding the byte range
    /// `[begin, stop)` of string object `src` — a range on char
    /// boundaries, which every char-indexed query produces — owned by
    /// the caller. The `substring` and `char_at` results copy once
    /// through here instead of staging a `String` (which would copy
    /// twice).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object.
    pub fn alloc_substr(&mut self, src: u32, begin: usize, stop: usize) -> Result<u32, Trap> {
        let part: Box<str> = self.str_at(src)?[begin..stop].into();
        Ok(self.alloc(Object::Str(Str::new(part))))
    }

    /// Allocates a fresh list object over `elems`, owned by the caller.
    /// The element words are adopted as-is: a caller handing `Ref`
    /// elements must have acquired their counts itself (the producer
    /// natives over fresh objects). The stride follows the payload: 0 for
    /// the empty list (stride-polymorphic until the first push), 1 for
    /// every non-empty one-word-element list; a stride-carrying copy
    /// names its stride through [`Objects::alloc_list_strided`].
    pub fn alloc_list(&mut self, elems: Vec<Value>) -> u32 {
        let stride = usize::from(!elems.is_empty());
        self.alloc_list_strided(elems, stride)
    }

    /// Allocates a fresh list over `elems` at an explicit element stride
    /// (the copy natives carrying a source list's stride; `elems` empty
    /// keeps stride 0 whatever the caller passes).
    pub fn alloc_list_strided(&mut self, elems: Vec<Value>, stride: usize) -> u32 {
        let stride = if elems.is_empty() { 0 } else { stride };
        self.alloc(Object::List(ListData { elems, stride }))
    }

    /// `rc_inc` (C4): the reference in the slot naming `obj` gains one
    /// owner.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object:
    /// hand-assembled code or a host argument carried a bogus `Ref` word.
    pub fn inc(&mut self, obj: u32) -> Result<(), Trap> {
        self.heap.rc_inc_raw(self.header_of(obj)?);
        Ok(())
    }

    /// `rc_dec` (C4): one owner drops. A unique death frees the object —
    /// a string unbinds its intern entry, a list drops one count per
    /// `Ref` element it holds — and threads the id onto the free list
    /// (the next compatible allocation claims it) while the block's
    /// memory pools in ds-rt's size-class reuse.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object
    /// (a double drop of a freed object lands here: the id is `Free`).
    pub fn dec(&mut self, obj: u32) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if self.heap.rc_dec_count(header) {
            self.free(obj, header);
        }
        Ok(())
    }

    /// The unique death: unbinds the intern entry, threads the id onto
    /// the free list, extracts the payload's owned `Ref` words, then
    /// disposes the block (ds-rt runs the payload's death hook and pools
    /// the memory). The death cascade runs on an explicit work stack —
    /// a composite's owned counts drop in a loop instead of native
    /// recursion, so a deeply nested structure dies in bounded stack
    /// (the heap bounds the stack, and the free order stays the
    /// recursive depth-first one: each owner's children fully before
    /// the next sibling). The stack and the child set ride the store's
    /// scratch buffers.
    fn free(&mut self, obj: u32, header: NonNull<RcHeader>) {
        let mut dying = std::mem::take(&mut self.scratch.dying);
        let mut zeros = std::mem::take(&mut self.scratch.zeros);
        let mut spill = std::mem::take(&mut self.scratch.words);
        dying.push((obj, header));
        while let Some((id, header)) = dying.pop() {
            // an interned entry pointing here must not outlive the object:
            // the next intern of the same content allocates a fresh one
            if let Object::Str(s) = self.heap.payload::<Object>(header)
                && self.interned.get(&*s.text).is_some_and(|row| row.id == id)
            {
                self.interned.remove(&*s.text);
            }
            let next = self.free_head.unwrap_or(NO_FREE);
            self.free_head = Some(id);
            self.table[id as usize] = Slot::Free(next);
            self.live -= 1;
            // the owned words leave the payload before it dies, so the
            // block's death hook drops only empty containers; a list's or
            // a closure's element vector moves out whole, a map's
            // interleaved pairs drain into the spill buffer
            spill.clear();
            let owned = match self.heap.payload_mut::<Object>(header) {
                Object::List(data) => std::mem::take(&mut data.elems),
                Object::Closure(closure) => std::mem::take(&mut closure.captures),
                Object::Map(data) => {
                    let vstride = data.vstride;
                    for key in data.keys.drain(..) {
                        spill.push(key);
                        spill.extend(data.values.drain(..vstride));
                    }
                    Vec::new()
                },
                Object::Str(_) | Object::Bytes(_) => Vec::new(),
            };
            let words: &[Value] = if owned.is_empty() { &spill } else { &owned };
            // the block's death hook (the payload's own drop glue) runs,
            // then the memory pools for the next compatible allocation
            self.heap.rc_dispose::<Object>(header);
            // one owned count per Ref word drops here (C4): lists per
            // element, maps per key and value word, closures per capture.
            // The dec is split so a zero queues the object instead of
            // recursing; a stored reference is live by construction, and
            // the id check keeps the double-drop tripwire
            for word in words.iter().copied() {
                let Value::Ref(child) = word else {
                    continue;
                };
                if child.fiber() != self.owner {
                    // a foreign-held word: the count sits on the owning
                    // heap, so the dec queues for the router to drain
                    self.pending.push_back(ForeignOp::Dec(child));
                    continue;
                }
                let child_header = match self.table.get(child.id() as usize) {
                    Some(Slot::Live(header)) => *header,
                    _ => panic!("a stored element reference is live"),
                };
                if self.heap.rc_dec_count(child_header) {
                    zeros.push((child.id(), child_header));
                }
            }
            // reverse push pops in owned order: the first child's whole
            // subtree frees before the next sibling
            dying.extend(zeros.drain(..).rev());
        }
        self.scratch.dying = dying;
        self.scratch.zeros = zeros;
        self.scratch.words = spill;
    }

    /// Appends one element — its whole inline word span — onto a list
    /// object in place: the construction append behind list literals,
    /// where the list is freshly created and uniquely owned by the
    /// constructing frame. The first append onto an empty list fixes the
    /// list's element stride (`words.len()`); a later append must repeat
    /// it exactly, so a hand-assembled mismatch traps instead of
    /// misaligning the payload. Storing a `Ref` word acquires a count for
    /// the list, one per `Ref` word of the span — the staged words stay
    /// owned by their own frame too (the placement pass's native-borrow
    /// discipline). Every other in-place mutation stays the
    /// exclusive-window story.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list object;
    /// [`Trap::ElementStride`] when the staged span's width disagrees
    /// with the non-empty list's stride.
    pub fn push_list(&mut self, obj: u32, words: &[Value]) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::List(_)) || words.is_empty() {
            return Err(Trap::UnknownObject { obj });
        }
        if let Object::List(data) = self.heap.payload::<Object>(header)
            && !data.elems.is_empty()
            && data.stride != words.len()
        {
            return Err(Trap::ElementStride {
                obj,
                stride: data.stride,
                given: words.len(),
            });
        }
        // the acquire happens before the store: a bogus element reference
        // leaves the list untouched (a foreign element word queues, so
        // the owning heap takes the list's count)
        for value in words {
            if let Value::Ref(word) = value {
                if word.fiber() == self.owner {
                    self.inc(word.id())?;
                } else {
                    self.pending.push_back(ForeignOp::Inc(*word));
                }
            }
        }
        match self.heap.payload_mut::<Object>(header) {
            Object::List(data) => {
                if data.elems.is_empty() {
                    data.stride = words.len();
                }
                data.elems.extend_from_slice(words);
            },
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// Concatenates two lists into a fresh object: `lhs`'s elements then
    /// `rhs`'s, the joined length reserved up front (growth is amortized
    /// doubling; a join never re-grows mid-copy). The two strides must
    /// agree (an empty operand is stride-polymorphic and adopts the
    /// other side's); the joined list owns one count per `Ref` element
    /// word it copies; the operand lists keep theirs.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when either reference names no list;
    /// [`Trap::ElementStride`] when two non-empty operands' strides
    /// disagree.
    pub fn concat_lists(&mut self, lhs: u32, rhs: u32) -> Result<u32, Trap> {
        let ((front, front_s), (back, back_s)) = (self.list_parts(lhs)?, self.list_parts(rhs)?);
        let stride = match (front_s, back_s) {
            (0, s) | (s, 0) => s,
            (s, t) if s == t => s,
            (s, t) => {
                return Err(Trap::ElementStride {
                    obj: rhs,
                    // the named operand's own stride, the other side's
                    // width staged against it
                    stride: t,
                    given: s,
                })
            },
        };
        let mut elems = Vec::with_capacity(front.len() + back.len());
        let words: Vec<Value> = front.iter().chain(back.iter()).copied().collect();
        for value in words {
            if let Value::Ref(word) = value {
                self.acquire_stored(word);
            }
            elems.push(value);
        }
        Ok(self.alloc_list_strided(elems, stride))
    }

    /// Concatenates two strings into a fresh object, owned by the caller.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when either reference names no string.
    pub fn concat(&mut self, lhs: u32, rhs: u32) -> Result<u32, Trap> {
        let (left, right) = (self.str_at(lhs)?, self.str_at(rhs)?);
        let mut text = String::with_capacity(left.len() + right.len());
        text.push_str(left);
        text.push_str(right);
        Ok(self.alloc_str(text))
    }

    /// The string content of object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object: hand-assembled code or a host argument carried a bogus
    /// `Ref` word.
    pub fn str_at(&self, obj: u32) -> Result<&str, Trap> {
        match self.payload_of(obj) {
            Some(Object::Str(s)) => Ok(&s.text),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The char count of string object `obj`, building its char-boundary
    /// map on the first char-indexed query (see `[Str]`).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object.
    pub fn char_count(&mut self, obj: u32) -> Result<usize, Trap> {
        self.ensure_char_count(obj)?;
        let (text, chars) = self.str_with_chars(obj)?;
        Ok(chars.count(text))
    }

    /// The byte offsets of the char range `[start, end)` of string object
    /// `obj`, building its char-boundary map on the first char-indexed
    /// query. Both bounds are valid char positions with
    /// `start <= end <= char count` — the contract the clamping in
    /// `substring` establishes — so both offsets sit on char boundaries
    /// and the slice between them cannot cut a code point.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object.
    pub fn char_byte_range(
        &mut self,
        obj: u32,
        start: usize,
        end: usize,
    ) -> Result<(usize, usize), Trap> {
        self.ensure_char_offsets(obj)?;
        let (_, chars) = self.str_with_chars(obj)?;
        Ok((chars.byte_at(start), chars.byte_at(end)))
    }

    /// Builds the char count of string object `obj` unless it is already
    /// known: a count query leaves the boundary offsets unbuilt.
    fn ensure_char_count(&mut self, obj: u32) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if matches!(self.heap.payload::<Object>(header), Object::Str(s) if s.chars.is_some()) {
            return Ok(());
        }
        let built = CharIndex::new(self.str_at(obj)?);
        match self.heap.payload_mut::<Object>(header) {
            Object::Str(s) => s.chars = Some(built),
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// Builds the char-boundary offsets of string object `obj` unless they
    /// are already known: one table serves every later range query.
    fn ensure_char_offsets(&mut self, obj: u32) -> Result<(), Trap> {
        self.ensure_char_count(obj)?;
        let header = self.header_of(obj)?;
        if matches!(
            self.heap.payload::<Object>(header),
            Object::Str(s) if s.chars.as_ref().is_some_and(CharIndex::has_offsets)
        ) {
            return Ok(());
        }
        let built = char_offsets(self.str_at(obj)?);
        match self.heap.payload_mut::<Object>(header) {
            Object::Str(s) => s
                .chars
                .as_mut()
                .expect("the char count was just built")
                .set_offsets(built),
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// The text and its char-boundary map of string object `obj`; the
    /// caller built the map first (a char-indexed query's private
    /// invariant, not a promise to embedders).
    fn str_with_chars(&self, obj: u32) -> Result<(&str, &CharIndex), Trap> {
        match self.payload_of(obj) {
            Some(Object::Str(s)) => Ok((
                &s.text,
                s.chars
                    .as_ref()
                    .expect("a char-indexed query built the map"),
            )),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Allocates a fresh bytes object holding `payload`, owned by the
    /// caller — one counted block, `string`'s shape over raw bytes.
    pub fn alloc_bytes(&mut self, payload: Box<[u8]>) -> u32 {
        self.alloc(Object::Bytes(payload))
    }

    /// The byte payload of bytes object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live bytes
    /// object: hand-assembled code or a host argument carried a bogus
    /// `Ref` word.
    pub fn bytes_at(&self, obj: u32) -> Result<&[u8], Trap> {
        match self.payload_of(obj) {
            Some(Object::Bytes(data)) => Ok(data),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The element words of list object `obj` and its per-element stride:
    /// the payload holds `len * stride` words, element `i` at
    /// `i * stride .. (i + 1) * stride`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live list object.
    pub fn list_parts(&self, obj: u32) -> Result<(&[Value], usize), Trap> {
        match self.payload_of(obj) {
            Some(Object::List(data)) => Ok((&data.elems, data.stride)),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The element words of list object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live list object.
    pub fn list_at(&self, obj: u32) -> Result<&[Value], Trap> {
        Ok(self.list_parts(obj)?.0)
    }

    /// The list's per-element stride (0 for the empty list).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live list object.
    pub fn list_stride(&self, obj: u32) -> Result<usize, Trap> {
        Ok(self.list_parts(obj)?.1)
    }

    /// Allocates a fresh empty map object, owned by the caller.
    pub fn alloc_map(&mut self) -> u32 {
        self.alloc(Object::Map(MapData::default()))
    }

    /// Allocates a fresh closure object (the header FuncRef plus the
    /// capture block), owned by the caller. The capture words are adopted
    /// as-is: a moved-in `Ref` word keeps its count — the object owns it
    /// now — while copyable words are plain copies [F2].
    pub fn alloc_closure(&mut self, func: FuncRef, captures: Vec<Value>) -> u32 {
        self.alloc(Object::Closure(Closure { func, captures }))
    }

    /// The closure object's header word plus its capture block.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no closure object
    /// (out of range, or the object is not a closure).
    pub fn closure_at(&self, obj: u32) -> Result<(FuncRef, &[Value]), Trap> {
        match self.payload_of(obj) {
            Some(Object::Closure(closure)) => Ok((closure.func, &closure.captures)),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Visits every heap-held callable word — closure headers, closure
    /// captures, list elements, map keys and values — letting the caller
    /// inspect or rewrite each in place (the slot swap's staged-word
    /// re-check). The first `Err` short-circuits the walk.
    pub(crate) fn for_each_staged_fn<E>(
        &mut self,
        visit: &mut impl FnMut(StagedSite, &mut FuncRef) -> Result<(), E>,
    ) -> Result<(), E> {
        let owner = self.owner.0;
        for slot in &self.table {
            let Slot::Live(header) = *slot else {
                continue;
            };
            match self.heap.payload_mut::<Object>(header) {
                Object::Closure(closure) => {
                    visit(
                        StagedSite::ClosureHeader { fiber: owner },
                        &mut closure.func,
                    )?;
                    for word in &mut closure.captures {
                        if let Value::Fn(func) = word {
                            visit(StagedSite::ClosureCapture { fiber: owner }, func)?;
                        }
                    }
                },
                Object::List(list) => {
                    for word in &mut list.elems {
                        if let Value::Fn(func) = word {
                            visit(StagedSite::ListElement { fiber: owner }, func)?;
                        }
                    }
                },
                Object::Map(map) => {
                    for key in &mut map.keys {
                        if let Value::Fn(func) = key {
                            visit(StagedSite::MapWord { fiber: owner }, func)?;
                        }
                    }
                    for word in &mut map.values {
                        if let Value::Fn(func) = word {
                            visit(StagedSite::MapWord { fiber: owner }, func)?;
                        }
                    }
                },
                Object::Str(_) | Object::Bytes(_) => {},
            }
        }
        Ok(())
    }

    /// Whether `obj` names a live string object holding exactly `text`:
    /// the KSTR pool-const cache's hit validation — a cached id stays
    /// servable only while its content still matches, so a dead or
    /// recycled id never serves.
    pub(crate) fn is_live_str_with(&self, obj: u32, text: &str) -> bool {
        matches!(self.payload_of(obj), Some(Object::Str(s)) if &*s.text == text)
    }

    /// The string content of object `obj` with its cached filter hash, when
    /// `obj` names a live string — the map probe's foreign-resolution read,
    /// so the probe's own heap hashes and copies the text once.
    pub(crate) fn str_with_hash(&self, obj: u32) -> Option<(&str, u32)> {
        match self.payload_of(obj) {
            Some(Object::Str(s)) => Some((&s.text, s.content_hash())),
            _ => None,
        }
    }

    /// Resolves a probe word once per lookup or store, payload-locally:
    /// a local word resolves through this store, a foreign word through
    /// the router's pre-resolution (content and hash carried off the
    /// probe's own heap), a scalar word needs no payload at all. The scan
    /// then never re-reads the probe's payload per candidate.
    fn resolve_probe<'a>(&'a self, key: Value, res: &'a ResolvedKey) -> ProbeKey<'a> {
        match res {
            ResolvedKey::Text { text, hash } => ProbeKey::Text { hash: *hash, text },
            ResolvedKey::Opaque => match key {
                // the router builds Opaque only from a reference word
                Value::Ref(word) => ProbeKey::Identity {
                    hash: ref_hash(word),
                    word,
                },
                scalar => ProbeKey::Scalar {
                    hash: scalar_hash(scalar),
                    word: scalar,
                },
            },
            ResolvedKey::Word => match key {
                Value::Ref(word) => match self.payload_of(word.id()) {
                    Some(Object::Str(s)) => ProbeKey::Text {
                        hash: s.content_hash(),
                        text: &s.text,
                    },
                    _ => ProbeKey::Identity {
                        hash: ref_hash(word),
                        word,
                    },
                },
                scalar => ProbeKey::Scalar {
                    hash: scalar_hash(scalar),
                    word: scalar,
                },
            },
        }
    }

    /// Whether one stored key word equals the probe — the exact check a
    /// hash hit runs; the classes mirror the key equality rules with no
    /// probe payload read (the resolution already did it).
    fn stored_key_matches(&self, stored: Value, probe: &ProbeKey<'_>) -> bool {
        match probe {
            ProbeKey::Text { text, .. } => match stored {
                // the probe names a string: only a stored string key can
                // equal it (mixed kinds never match), by content
                Value::Ref(a) => matches!(
                    self.payload_of(a.id()),
                    Some(Object::Str(s)) if &*s.text == *text
                ),
                _ => false,
            },
            ProbeKey::Identity { word, .. } => match stored {
                // the probe names no string, and a stored string key is a
                // different object by construction, so the word compare
                // answers without reading the candidate's payload
                Value::Ref(a) => a == *word,
                _ => false,
            },
            ProbeKey::Scalar { word, .. } => word_eq(stored, *word),
        }
    }

    /// The row whose key equals the probe, if present: the index narrows
    /// the candidates to the rows sharing the probe's filter hash, so a miss
    /// reads a few dense index words and the exact check runs on the hash
    /// hits alone.
    fn map_row_at(&self, data: &MapData, probe: &ProbeKey<'_>) -> Option<usize> {
        if data.slots.is_empty() {
            return None;
        }
        let hash = probe.hash();
        let mask = data.slots.len() - 1;
        let mut at = hash as usize & mask;
        while data.slots[at] != ROW_NONE {
            let row = data.slots[at] as usize;
            if data.hashes[row] == hash && self.stored_key_matches(data.keys[row], probe) {
                return Some(row);
            }
            at = (at + 1) & mask;
        }
        None
    }

    /// Stores one key and its value span into a map object in place, the
    /// exclusive window's one mutation (the `map_set` native). The value
    /// is the value type's whole inline word span — one word for a
    /// scalar, a tag-plus-region span for a union value — and the first
    /// store onto an empty map fixes the value stride every later store
    /// must repeat. Every stored `Ref` word is acquired for the map
    /// before it enters a row, fresh or overwriting, so the caller's own
    /// count stays the caller's and a later death drops exactly what was
    /// stored. A content-equal key overwrites: the old value span's
    /// counts drop, and the row keeps its original key word (the map
    /// already owns that count; the caller keeps its own probe word), so
    /// the row index keeps its entry too.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object;
    /// [`Trap::ElementStride`] when the staged span's width disagrees
    /// with the non-empty map's value stride.
    pub fn set_map(
        &mut self,
        obj: u32,
        key: Value,
        res: &ResolvedKey,
        value: &[Value],
    ) -> Result<(), Trap> {
        let header = match self.table.get(obj as usize) {
            Some(Slot::Live(header))
                if matches!(self.heap.payload::<Object>(*header), Object::Map(_)) =>
            {
                *header
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        // one probe resolution serves the scan and the fresh row's hash;
        // the borrow ends before the mutation window opens
        let (found, hash, stride) = match self.payload_of(obj) {
            Some(Object::Map(data)) => {
                let probe = self.resolve_probe(key, res);
                (self.map_row_at(data, &probe), probe.hash(), data.vstride)
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        if stride != 0 && stride != value.len() {
            return Err(Trap::ElementStride {
                obj,
                stride,
                given: value.len(),
            });
        }
        if let Some(at) = found {
            // the store acquires for the map first, whatever it replaces:
            // a value word arriving with its caller's count must not ride
            // that count into the row (a dying caller would free it)
            for word in value {
                if let Value::Ref(word) = *word {
                    self.acquire_stored(word);
                }
            }
            let old = match self.heap.payload_mut::<Object>(header) {
                Object::Map(data) => {
                    let at = at * data.vstride;
                    let old = data.values[at..at + value.len()].to_vec();
                    data.values[at..at + value.len()].copy_from_slice(value);
                    old
                },
                _ => return Err(Trap::UnknownObject { obj }),
            };
            for word in old {
                if let Value::Ref(word) = word {
                    self.release_stored(word);
                }
            }
            return Ok(());
        }
        // the acquires happen before the store: a bogus reference word
        // leaves the map untouched
        if let Value::Ref(word) = key {
            self.acquire_stored(word);
        }
        for word in value {
            if let Value::Ref(word) = *word {
                self.acquire_stored(word);
            }
        }
        match self.heap.payload_mut::<Object>(header) {
            Object::Map(data) => {
                if data.keys.is_empty() {
                    data.vstride = value.len();
                }
                data.hashes.push(hash);
                data.keys.push(key);
                data.values.extend_from_slice(value);
                data.index_push();
            },
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// The value span stored under `key` in map object `obj`, if present.
    /// The read is a lookup, not a hand-out: the map keeps every count it
    /// owns, so the caller that wants the value acquires its own.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_at(
        &self,
        obj: u32,
        key: Value,
        res: &ResolvedKey,
    ) -> Result<Option<&[Value]>, Trap> {
        let Some(Object::Map(data)) = self.payload_of(obj) else {
            return Err(Trap::UnknownObject { obj });
        };
        // a rowless map has no candidate, so the probe needs no resolution
        if data.slots.is_empty() {
            return Ok(None);
        }
        let probe = self.resolve_probe(key, res);
        Ok(self.map_row_at(data, &probe).map(|at| {
            let at = at * data.vstride;
            &data.values[at..at + data.vstride]
        }))
    }

    /// The row count of map object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_len(&self, obj: u32) -> Result<usize, Trap> {
        match self.payload_of(obj) {
            Some(Object::Map(data)) => Ok(data.keys.len()),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The key words of map object `obj`, in entry order — the ordered
    /// key face (`map_keys`), the value spans read back through
    /// [`Objects::map_at`].
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_keys(&self, obj: u32) -> Result<&[Value], Trap> {
        match self.payload_of(obj) {
            Some(Object::Map(data)) => Ok(&data.keys),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }
}

/// The routing face over the pool's heaps: the store side of the native
/// calling convention. The running fiber's store is the default (fresh
/// objects are born there); every id-taking service routes a word to the
/// store its owner axis names, so a foreign word — a delivered, staged,
/// or host-held one — serves wherever it lands. Cross-heap count traffic
/// rides the stores' pending queues, drained after each routed call, so
/// the routing itself never skips a beat of the count discipline.
pub struct Heaps<'a> {
    /// The running fiber: its store sits in `current`, and its pool slot
    /// holds the scratch — never routed to.
    pub(crate) owner: FiberId,
    /// Whether `current` really is the running fiber's store: true under
    /// a live execution context, false when the drive is parked and the
    /// context store is the scratch — then every store is reached through
    /// the pool (the death walk's form).
    pub(crate) live: bool,
    pub(crate) current: &'a mut Objects,
    pub(crate) pool: &'a mut [Fiber],
    /// The per-fiber claim ledgers, parallel to the pool: the frame-claim
    /// bookkeeping the router's count mutations maintain.
    pub(crate) claims: &'a mut [ClaimTable],
}

impl<'a> Heaps<'a> {
    /// The router over the running fiber's store, the pool, and the claim
    /// ledgers.
    pub(crate) fn new(
        owner: FiberId,
        current: &'a mut Objects,
        pool: &'a mut [Fiber],
        claims: &'a mut [ClaimTable],
        live: bool,
    ) -> Self {
        Self {
            owner,
            live,
            current,
            pool,
            claims,
        }
    }

    /// The claim ledger of one fiber.
    fn ledger(&mut self, fiber: FiberId) -> &mut ClaimTable {
        &mut self.claims[fiber.index()]
    }

    /// The running fiber's store: the bare allocation target, for the
    /// fixtures' id-level assertions. Only ever this store — a
    /// word-bearing call goes through the routed mirrors below.
    #[cfg(test)]
    pub(crate) fn current(&mut self) -> &mut Objects {
        self.current
    }

    /// The `Ref` word of a fresh object `id` on the running fiber's store.
    pub(crate) fn word(&self, id: u32) -> Value {
        Value::Ref(RefId::new(self.owner, id))
    }

    /// The store owning `word`: the running fiber's when the owner axes
    /// agree under a live context, the named pool slot's otherwise.
    fn store(&mut self, word: RefId) -> Result<&mut Objects, Trap> {
        if self.live && word.fiber() == self.owner {
            Ok(self.current)
        } else {
            self.pool
                .get_mut(word.fiber().index())
                .map(|fiber| &mut fiber.objects)
                .ok_or(Trap::UnknownObject { obj: word.id() })
        }
    }

    /// The store owning `word`, shared: the routed reads' face. A read
    /// mutates no count and queues no cross-heap operation, so it owes no
    /// drain — the pool stays quiet between mutations exactly as before.
    fn store_shared(&self, word: RefId) -> Result<&Objects, Trap> {
        if self.live && word.fiber() == self.owner {
            Ok(&*self.current)
        } else {
            self.pool
                .get(word.fiber().index())
                .map(|fiber| &fiber.objects)
                .ok_or(Trap::UnknownObject { obj: word.id() })
        }
    }

    /// How many cross-heap operations wait on the store owning `fiber`.
    fn queued(&self, fiber: FiberId) -> usize {
        if self.live && fiber == self.owner {
            self.current.queued()
        } else {
            self.pool
                .get(fiber.index())
                .map_or(0, |fiber| fiber.objects.queued())
        }
    }

    /// Drains the cross-heap operations a routed mutation queued until none
    /// remain: a routed dec can free an object whose cascade queues more, on
    /// any store, so the loop runs to quiescence. Order within one queue is
    /// FIFO — a cascade's cross-heap decs fire in the traversal order the
    /// queueing store saw.
    ///
    /// `from` is the store the mutation routed to. Every queued operation
    /// lands on the store whose own mutation observed the foreign word, and
    /// every routed mutation ends in this drain, so the pool is quiet
    /// between mutations: `from`'s queue length is the whole pending count,
    /// and a zero answers before the pool is touched.
    fn settle(&mut self, from: FiberId) -> Result<(), Trap> {
        if self.queued(from) == 0 {
            #[cfg(feature = "diag")]
            // the invariant the fast path rests on, re-walked under the
            // tripwire layer: an operation queued anywhere else would
            // strand here
            assert!(
                self.current.queued() == 0
                    && self.pool.iter().all(|fiber| fiber.objects.queued() == 0),
                "ds-vm diag: a cross-heap op queued outside the mutated store"
            );
            return Ok(());
        }
        loop {
            let op = if let Some(op) = self.current.pending_pop() {
                op
            } else {
                let mut found = None;
                for fiber in self.pool.iter_mut() {
                    if let Some(op) = fiber.objects.pending_pop() {
                        found = Some(op);
                        break;
                    }
                }
                let Some(op) = found else {
                    return Ok(());
                };
                op
            };
            match op {
                ForeignOp::Inc(word) => self.store(word)?.inc(word.id())?,
                ForeignOp::Dec(word) => {
                    let store = self.store(word)?;
                    store.dec(word.id())?;
                },
            }
        }
    }

    /// Allocates a fresh string object on the running fiber's store and
    /// answers its word — the caller's frame claim.
    pub(crate) fn new_str(&mut self, text: String) -> Value {
        let id = self.current.alloc_str(text);
        self.claimed_alloc(id)
    }

    /// A fresh string object holding `part`, as its word — the caller's
    /// frame claim.
    pub(crate) fn new_str_part(&mut self, part: &str) -> Value {
        let id = self.current.alloc_str_part(part);
        self.claimed_alloc(id)
    }

    /// A fresh list object over `elems`, as its word — the caller's frame
    /// claim. The element words are adopted as-is, and each `Ref`
    /// element's count must already be the list's edge (an `inc_edge`
    /// where the caller's copy loop saw the word, or a fresh producer
    /// word's birth count with its frame claim released), so the free
    /// cascade owns those counts and no element claim stays on a ledger.
    pub(crate) fn new_list(&mut self, elems: Vec<Value>) -> Value {
        let id = self.current.alloc_list(elems);
        self.claimed_alloc(id)
    }

    /// A fresh list over `elems` at an explicit element stride (the copy
    /// natives carrying a source list's stride), as its word — the
    /// caller's frame claim, the edge-count contract of
    /// [`Heaps::new_list`].
    pub(crate) fn new_list_strided(&mut self, elems: Vec<Value>, stride: usize) -> Value {
        let id = self.current.alloc_list_strided(elems, stride);
        self.claimed_alloc(id)
    }

    /// A fresh list object over `elems`, as the raw word (the combinators'
    /// per-element accumulation list, carried across hosted calls).
    pub(crate) fn new_list_id(&mut self, elems: Vec<Value>) -> RefId {
        let id = self.current.alloc_list(elems);
        match self.claimed_alloc(id) {
            Value::Ref(word) => word,
            _ => unreachable!("a fresh list is its reference word"),
        }
    }

    /// A fresh empty map object, as its word — the caller's frame claim.
    pub(crate) fn new_map(&mut self) -> Value {
        let id = self.current.alloc_map();
        self.claimed_alloc(id)
    }

    /// A fresh bytes object holding `payload` on the running fiber's
    /// store, as its word — the caller's frame claim.
    pub(crate) fn new_bytes(&mut self, payload: Box<[u8]>) -> Value {
        let id = self.current.alloc_bytes(payload);
        self.claimed_alloc(id)
    }

    /// The byte payload of bytes object `word`, routed: a borrowed slice
    /// of the owning store — copy a byte out before another routed call.
    pub(crate) fn bytes_at(&mut self, word: RefId) -> Result<&[u8], Trap> {
        self.bytes_ref(word)
    }

    /// The interned word of `text` on the running fiber's store,
    /// acquiring one count — the caller's frame claim.
    pub(crate) fn new_intern(&mut self, text: &str) -> Value {
        let id = self.current.intern(text);
        self.claimed_alloc(id)
    }

    /// A fresh object's word with the caller's claim entered on the
    /// running fiber's ledger (the birth count is the caller's).
    fn claimed_alloc(&mut self, id: u32) -> Value {
        let word = RefId::new(self.owner, id);
        self.ledger(self.owner).acquire(word);
        Value::Ref(word)
    }

    /// Releases the running fiber's frame claim on `word` without
    /// touching the count: a fresh word's birth count (or a retained
    /// copy's count) becomes the adopting container's edge, so the frame
    /// no longer owns it and the container's free cascade will drop it.
    pub(crate) fn release_claim(&mut self, word: RefId) {
        self.ledger(self.owner).release(word);
    }

    /// `rc_inc` routed by the word's owner, the claim the running
    /// fiber's (a read-out's retain, the RCINC arm), queues drained.
    pub(crate) fn inc(&mut self, word: RefId) -> Result<(), Trap> {
        self.inc_for(self.owner, word)
    }

    /// `rc_inc` routed by the word's owner, the count claimed for
    /// `claimant`'s frames — the copy-in convention (the callee's
    /// parameters, the spawned child's entry frame).
    pub(crate) fn inc_for(&mut self, claimant: FiberId, word: RefId) -> Result<(), Trap> {
        self.store(word)?.inc(word.id())?;
        self.ledger(claimant).acquire(word);
        self.settle(word.fiber())
    }

    /// `rc_inc` routed by the word's owner for an object-graph edge (a
    /// closure's capture adoption, a list push): the count belongs to the
    /// holding object, released by its free cascade — no frame claim.
    pub(crate) fn inc_edge(&mut self, word: RefId) -> Result<(), Trap> {
        self.store(word)?.inc(word.id())?;
        self.settle(word.fiber())
    }

    /// `rc_dec` routed by the word's owner, the claim the running
    /// fiber's, queues drained. Cross-heap exact ownership: a foreign
    /// word's count releases only through this fiber's own claim (every
    /// honest crossing — a call or spawn copy-in, a delivery — acquires
    /// the row), so a never-claimed dec of a foreign word releases
    /// nothing while still refusing a freed id: the count it would drop
    /// is subscribed by its own heap's rows and edges, and stealing it
    /// freed a live object under a stored reference (the fuzz_vm
    /// spawn-drain crash). A same-heap dec keeps the one-store
    /// discipline: the host-borrowed gift counts (an entry body's owned
    /// parameters over root-staged words) ride no row by design, and the
    /// frame's own dec is their release.
    pub(crate) fn dec(&mut self, word: RefId) -> Result<(), Trap> {
        if word.fiber() != self.owner && self.claims[self.owner.index()].count_of(word) == 0 {
            return self.store(word)?.rc_at(word.id()).map(|_| ());
        }
        self.store(word)?.dec(word.id())?;
        self.ledger(self.owner).release(word);
        self.settle(word.fiber())
    }

    /// `rc_dec` routed for a death walk: no ledger traffic (the claims
    /// were taken before the walk began, so the gated [`Heaps::dec`] must
    /// not mediate). Routing follows the construction — the live door's
    /// router serves the current fiber's own words through its context
    /// store, the parked door's through the pool (a parked drive's
    /// context store is the scratch, never a walk's target) — and the
    /// word's own heap takes the dec either way, queues drained.
    pub(crate) fn dec_walk(&mut self, word: RefId) -> Result<(), Trap> {
        self.store(word)?.dec(word.id())?;
        self.settle(word.fiber())
    }

    /// The string content of `word`, routed as a shared borrow: the read
    /// allocates nothing, so the compare and hash natives read both
    /// operands without copying either out.
    pub(crate) fn str_ref(&self, word: RefId) -> Result<&str, Trap> {
        self.store_shared(word)?.str_at(word.id())
    }

    /// The byte payload of bytes object `word`, routed as a shared borrow,
    /// allocating nothing.
    pub(crate) fn bytes_ref(&self, word: RefId) -> Result<&[u8], Trap> {
        self.store_shared(word)?.bytes_at(word.id())
    }

    /// The string content of `word`, routed.
    pub(crate) fn str_at(&mut self, word: RefId) -> Result<&str, Trap> {
        self.str_ref(word)
    }

    /// Concatenates two strings into a fresh object on the running
    /// fiber's store, reading each operand from its own heap. The birth
    /// count is the calling frame's, both paths — the claim is what the
    /// frame's later dec rides, so a fast path without it strands the
    /// count on a word no ledger owns.
    pub(crate) fn concat(&mut self, x: RefId, y: RefId) -> Result<u32, Trap> {
        if x.fiber() == y.fiber() && x.fiber() == self.owner {
            let id = self.current.concat(x.id(), y.id())?;
            return Ok(self.claim_result(id));
        }
        let a = self.store(x)?.str_at(x.id())?.to_owned();
        let b = self.store(y)?.str_at(y.id())?.to_owned();
        let mut text = String::with_capacity(a.len() + b.len());
        text.push_str(&a);
        text.push_str(&b);
        let id = self.current.alloc_str(text);
        Ok(self.claim_result(id))
    }

    /// Enters the fresh result's claim on the running fiber's ledger
    /// (every producer native's birth count is the calling frame's).
    fn claim_result(&mut self, id: u32) -> u32 {
        let word = RefId::new(self.owner, id);
        self.ledger(self.owner).acquire(word);
        id
    }

    /// The char count of string object `word`, routed.
    pub(crate) fn char_count(&mut self, word: RefId) -> Result<usize, Trap> {
        self.store(word)?.char_count(word.id())
    }

    /// The byte offsets of a char range of string object `word`, routed.
    pub(crate) fn char_byte_range(
        &mut self,
        word: RefId,
        start: usize,
        end: usize,
    ) -> Result<(usize, usize), Trap> {
        self.store(word)?.char_byte_range(word.id(), start, end)
    }

    /// A fresh string object holding the byte range `[begin, stop)` of
    /// string object `src`, born on the running fiber's store; the range
    /// reads from the source's own heap.
    pub(crate) fn alloc_substr(
        &mut self,
        src: RefId,
        begin: usize,
        stop: usize,
    ) -> Result<u32, Trap> {
        let id = if self.live && src.fiber() == self.owner {
            self.current.alloc_substr(src.id(), begin, stop)?
        } else {
            let part: Box<str> = self.store(src)?.str_at(src.id())?[begin..stop].into();
            self.current.alloc_str_part(&part)
        };
        Ok(self.claim_result(id))
    }

    /// The element words of list object `word` and its per-element
    /// stride, routed: a borrowed slice of the owning store — copy a word
    /// out before another routed call.
    pub(crate) fn list_at(&mut self, word: RefId) -> Result<(&[Value], usize), Trap> {
        let store = self.store(word)?;
        store.list_parts(word.id())
    }

    /// Concatenates two lists into a fresh object on the running fiber's
    /// store: a same-store pair runs in place, a cross-store pair copies
    /// the element words out and acquires one count per `Ref` element on
    /// the element's own heap. The strides must agree (an empty operand
    /// adopts the other side's).
    pub(crate) fn concat_lists(&mut self, x: RefId, y: RefId) -> Result<u32, Trap> {
        if x.fiber() == y.fiber() && x.fiber() == self.owner {
            let id = self.current.concat_lists(x.id(), y.id())?;
            self.settle(x.fiber())?;
            return Ok(self.claim_result(id));
        }
        let (front, front_s) = {
            let (elems, stride) = self.list_at(x)?;
            (elems.to_vec(), stride)
        };
        let (back, back_s) = {
            let (elems, stride) = self.list_at(y)?;
            (elems.to_vec(), stride)
        };
        let stride = match (front_s, back_s) {
            (0, s) | (s, 0) => s,
            (s, t) if s == t => s,
            (s, t) => {
                return Err(Trap::ElementStride {
                    obj: y.id(),
                    // the named operand's own stride, the other side's
                    // width staged against it
                    stride: t,
                    given: s,
                })
            },
        };
        let mut elems = Vec::with_capacity(front.len() + back.len());
        for value in front.into_iter().chain(back) {
            if let Value::Ref(word) = value {
                self.inc_edge(word)?;
            }
            elems.push(value);
        }
        let id = self.current.alloc_list_strided(elems, stride);
        Ok(self.claim_result(id))
    }

    /// Appends one element's whole word span onto list object `list`,
    /// routed: the payload mutation lands on the list's store and each
    /// `Ref` word's acquired count on the element's own heap.
    pub(crate) fn push_list(&mut self, list: RefId, words: &[Value]) -> Result<(), Trap> {
        self.store(list)?.push_list(list.id(), words)?;
        self.settle(list.fiber())
    }

    /// Resolves a map probe word for the map's store: a local (or
    /// scalar) word compares through the store itself; a foreign word's
    /// string content and hash resolve here, on the probe's own heap, so
    /// the content compare never reads the wrong store's payload and the
    /// map's store never re-hashes the probe.
    fn resolve_key(&mut self, map: FiberId, key: Value) -> Result<ResolvedKey, Trap> {
        let Value::Ref(word) = key else {
            return Ok(ResolvedKey::Word);
        };
        if word.fiber() == map {
            return Ok(ResolvedKey::Word);
        }
        let probe = self.store(word)?.str_with_hash(word.id());
        Ok(match probe {
            Some((text, hash)) => ResolvedKey::Text {
                text: text.into(),
                hash,
            },
            None => ResolvedKey::Opaque,
        })
    }

    /// The value span stored under `key` in map object `map`, routed and
    /// copied out: the slice leaves the owning store as owned words, so
    /// the caller may route count mutations while holding it.
    pub(crate) fn map_at(&mut self, map: RefId, key: Value) -> Result<Option<Vec<Value>>, Trap> {
        let res = self.resolve_key(map.fiber(), key)?;
        match self.store(map)?.map_at(map.id(), key, &res)? {
            Some(span) => Ok(Some(span.to_vec())),
            None => Ok(None),
        }
    }

    /// The value span stored under `key` in map object `map`, routed into
    /// `scratch`: the row read for walks that pair one map's rows with
    /// another's. The span leaves the owning store as plain copied words
    /// (the map keeps every count it owns) in the caller's buffer, so a
    /// reused scratch holds a many-row walk at O(1) allocations.
    pub(crate) fn map_row_scratch(
        &mut self,
        map: RefId,
        key: Value,
        scratch: &mut Vec<Value>,
    ) -> Result<bool, Trap> {
        let res = self.resolve_key(map.fiber(), key)?;
        match self.store(map)?.map_at(map.id(), key, &res)? {
            Some(span) => {
                scratch.clear();
                scratch.extend_from_slice(span);
                Ok(true)
            },
            None => Ok(false),
        }
    }

    /// Stores one key and its value span into map object `map`, routed,
    /// with the queues drained.
    pub(crate) fn set_map(&mut self, map: RefId, key: Value, value: &[Value]) -> Result<(), Trap> {
        let res = self.resolve_key(map.fiber(), key)?;
        self.store(map)?.set_map(map.id(), key, &res, value)?;
        self.settle(map.fiber())
    }

    /// A closure word's header and capture block, routed and copied out:
    /// the block leaves the owning store as owned words (captures are
    /// plain value words), so the caller may route count mutations while
    /// holding it.
    pub(crate) fn closure_parts(&mut self, word: RefId) -> Result<(FuncRef, Vec<Value>), Trap> {
        let store = self.store(word)?;
        let (header, block) = store.closure_at(word.id())?;
        Ok((header, block.to_vec()))
    }

    /// The row count of map object `map`, routed.
    pub(crate) fn map_len(&mut self, map: RefId) -> Result<usize, Trap> {
        self.store(map)?.map_len(map.id())
    }

    /// The key words of map object `map`, in entry order, routed and
    /// copied out: the caller may route count mutations while holding
    /// them.
    pub(crate) fn map_keys(&mut self, map: RefId) -> Result<Vec<Value>, Trap> {
        Ok(self.store(map)?.map_keys(map.id())?.to_vec())
    }

    /// The content-compare face the object behind `word` presents, routed:
    /// the four kinds the structural `==` content compare reads, or `None`
    /// for a faceless kind (a closure), which compares by word identity
    /// alone.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object.
    pub(crate) fn content_face(&mut self, word: RefId) -> Result<Option<ContentFace>, Trap> {
        let store = self.store(word)?;
        Ok(match store.payload_of(word.id()) {
            Some(Object::Str(_)) => Some(ContentFace::Str),
            Some(Object::Bytes(_)) => Some(ContentFace::Bytes),
            Some(Object::List(_)) => Some(ContentFace::List),
            Some(Object::Map(_)) => Some(ContentFace::Map),
            _ => None,
        })
    }

    /// The structural content compare over the objects behind two
    /// reference words [B9]: strings compare text, bytes payloads, lists
    /// their element words pairwise, maps row by row through the same
    /// lookup face `map_get` serves (string keys by content, every other
    /// word by identity), and every nested reference pair compares the
    /// same way — the walk covers a list of lists and a map over string
    /// values without a static plan. Scalar words compare through the
    /// one bit-structural judge the scalar `==` chain serves (`word_eq`:
    /// floats by bits, a NaN reflexive, the two zeros distinct); mixed
    /// object kinds, and the faceless kinds against anything, never
    /// compare equal. Both operands stay borrowed: the walk only reads,
    /// so it owns no count and queues nothing. Iterative over a pending
    /// word stack, so depth costs heap words, not native frames, and an
    /// in-progress pair memo makes a cyclic structure answer instead of
    /// looping — a re-encountered in-progress pair assumes equal (the
    /// greatest fixed point), any real mismatch surfacing through some
    /// first-visit pair.
    pub(crate) fn content_eq(&mut self, x: RefId, y: RefId) -> Result<bool, Trap> {
        let mut pending = vec![(Value::Ref(x), Value::Ref(y))];
        let mut assumed: Vec<(RefId, RefId)> = Vec::new();
        // the two row buffers, reused across the walk so a many-row
        // compare allocates no per-row vector (the death scratch's spill
        // shape, walk-local: no store borrow survives a routed call)
        let (mut left, mut right) = (Vec::new(), Vec::new());
        while let Some((l, r)) = pending.pop() {
            // an equal word pair — one scalar (bit-structural, a float's
            // own judge), one shared object — needs no walk; unequal
            // non-reference words answer at once
            if word_eq(l, r) {
                continue;
            }
            let (Value::Ref(a), Value::Ref(b)) = (l, r) else {
                return Ok(false);
            };
            match (self.content_face(a)?, self.content_face(b)?) {
                (Some(ContentFace::Str), Some(ContentFace::Str)) => {
                    // both operands borrow shared, so the pair compares
                    // without a copy
                    if self.str_ref(a)? != self.str_ref(b)? {
                        return Ok(false);
                    }
                },
                (Some(ContentFace::Bytes), Some(ContentFace::Bytes)) => {
                    if self.bytes_ref(a)? != self.bytes_ref(b)? {
                        return Ok(false);
                    }
                },
                (Some(ContentFace::List), Some(ContentFace::List)) => {
                    if assumed.contains(&(a, b)) {
                        continue;
                    }
                    assumed.push((a, b));
                    let (front, _) = self.list_at(a)?;
                    left.clear();
                    left.extend_from_slice(front);
                    let (back, _) = self.list_at(b)?;
                    if left.len() != back.len() {
                        return Ok(false);
                    }
                    pending.extend(left.iter().copied().zip(back.iter().copied()));
                },
                (Some(ContentFace::Map), Some(ContentFace::Map)) => {
                    if assumed.contains(&(a, b)) {
                        continue;
                    }
                    assumed.push((a, b));
                    if self.map_len(a)? != self.map_len(b)? {
                        return Ok(false);
                    }
                    for key in self.map_keys(a)? {
                        // each row lands in a reused buffer, so the walk pays
                        // no per-row vector; the key is its own map's row,
                        // so only the right side can miss — a missing row
                        // answers unequal at once
                        if !self.map_row_scratch(a, key, &mut left)? {
                            return Ok(false);
                        }
                        if !self.map_row_scratch(b, key, &mut right)? {
                            return Ok(false);
                        }
                        if left.len() != right.len() {
                            return Ok(false);
                        }
                        pending.extend(left.iter().copied().zip(right.iter().copied()));
                    }
                },
                // mixed kinds, or a faceless kind against anything: no
                // content face, never equal
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
}

/// The content-compare face one heap object presents: which payload walk
/// [`Heaps::content_eq`] reads over it. The faceless kinds (a closure)
/// compare by word identity alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContentFace {
    /// A string's text.
    Str,
    /// A bytes payload.
    Bytes,
    /// A list's element words.
    List,
    /// A map's rows.
    Map,
}

/// A map key probe's resolution for [`Objects::map_at`] and
/// [`Objects::set_map`]: a local or scalar word resolves through the
/// map's own store; a foreign word arrives pre-resolved — its string
/// content, or the knowledge that it names no string (identity compare
/// then, like every non-string reference key).
pub enum ResolvedKey {
    /// The word is local to the map's store or carries no reference:
    /// the store resolves it itself.
    Word,
    /// A foreign string probe: its content copied off the probe's heap,
    /// plus that object's cached content hash, so the map's store compares
    /// without re-walking the probe.
    Text { text: Box<str>, hash: u32 },
    /// A foreign reference probe naming no string.
    Opaque,
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
                    text: foreign_text.clone(),
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
                    text: other_text,
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
