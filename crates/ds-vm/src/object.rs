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
//! see [`Object::List`]. The element words are plain [`Value`]s, so one
//! kind serves every element type. Ownership of `Ref` elements is exact:
//! storing one into a list (the `list_push` construction append, the
//! `list_concat` shared-element copies, the `list_get` checked read-out)
//! acquires a count, and freeing a list drops one count per `Ref`
//! element it holds. Producer natives that build lists of fresh objects
//! (`split`, `fs_list`) store without acquiring — the fresh object's
//! birth count is the list's. String interning acquires too: a `KSTR`
//! load claims a count of the interned object, and a dying interned
//! string removes its intern entry so the next load of the same content
//! reborns a fresh object.
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
    collections::{HashMap, VecDeque},
    fmt,
    ptr::NonNull,
};

use ds_rt::{Heap, RcHeader, drop_deinit};

use crate::{
    claims::ClaimTable,
    fiber::{Fiber, FiberId},
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
/// build on their first access. Re-deriving char boundaries per call
/// costs O(string length) per call, so one map per object serves every
/// later access; interned strings share one object, hence one map.
#[derive(Debug)]
struct Str {
    text: Box<str>,
    /// `None` until the first char-indexed query builds it.
    chars: Option<CharIndex>,
}

impl Str {
    fn new(text: Box<str>) -> Self {
        Self { text, chars: None }
    }
}

/// Char-boundary metadata: maps a char position to its byte offset in
/// O(1), and answers the char count.
#[derive(Debug)]
enum CharIndex {
    /// Pure ASCII: char index == byte index and the char count is the
    /// byte length, so nothing is stored.
    Ascii,
    /// The byte offset of every char boundary through the end: char
    /// count + 1 entries, the last at the byte length. Stored only for
    /// multi-byte text, where byte offsets diverge from char indexes.
    Offsets(Box<[u32]>),
}

impl CharIndex {
    /// One scan of `text`: pure-ASCII text answers from its length,
    /// multi-byte text records the boundary table.
    fn build(text: &str) -> Self {
        if text.is_ascii() {
            return Self::Ascii;
        }
        // byte offsets are u32 words: the length check bounds every entry
        let len = u32::try_from(text.len()).expect("string byte length fits u32");
        let mut offsets = Vec::with_capacity(text.len() + 1);
        for (o, _) in text.char_indices() {
            offsets.push(u32::try_from(o).expect("char offset below the byte length"));
        }
        // the final entry is the end-of-text boundary a range end reaches
        offsets.push(len);
        Self::Offsets(offsets.into())
    }

    /// The char count.
    fn count(&self, text: &str) -> usize {
        match self {
            Self::Ascii => text.len(),
            Self::Offsets(offsets) => offsets.len() - 1,
        }
    }

    /// The byte offset of char boundary `index`, a valid char position
    /// (`index <= count`).
    fn byte_at(&self, index: usize) -> usize {
        match self {
            Self::Ascii => index,
            Self::Offsets(offsets) => offsets[index] as usize,
        }
    }
}

/// One heap object payload: the block body behind a live table id. One
/// payload type serves every kind, so every block shares one layout —
/// one payload offset, one class in ds-rt's reuse pool, one death hook
/// (the payload's own drop glue through [`drop_deinit`]).
#[derive(Debug)]
enum Object {
    /// A string: the text plus its lazily built char-boundary map.
    Str(Str),
    /// A closure object: one heap object behind one `Ref` word (F2) —
    /// the value a capturing lambda's slot holds.
    Closure(Closure),
    /// A list: one heap object — len, capacity, and the element words
    /// inline in a single block payload. The one in-place mutation is the
    /// append behind list literals ([`Objects::push_list`], the
    /// compiler-disciplined construction append over a freshly built
    /// list); every other mutation stays reserved for the exclusive
    /// windows, and growth is the backing buffer's amortized doubling.
    List(Vec<Value>),
    /// A map: one heap object behind one `Ref` word, the list object's
    /// conventions over key-value pairs. The internal shape is an
    /// association vector scanned front to back — the interim the
    /// builder-call construction story needs (no map literal yet), with
    /// no ordering promise and no iteration surface. Key matching:
    /// string keys compare by content (two equal-content string objects
    /// are one key), every other word by identity — so scalar and
    /// reference keys never collide across kinds. Each row carries a
    /// filter hash ([`MapData`]) so a probe's reject scan walks dense
    /// u32 words and never loads a candidate key's payload.
    Map(MapData),
}

/// A map's payload: the association rows plus one filter hash per row.
/// The hash is a reject filter, never the answer — every hash hit runs
/// the exact key check, so a collision costs one compare and nothing
/// else, while a miss answers from the dense u32 scan alone.
#[derive(Debug, Default)]
struct MapData {
    hashes: Vec<u32>,
    pairs: Vec<(Value, Value)>,
}

/// FNV-1a over `bytes`, continuing `state` (one round per byte; the
/// strings hashed here are short, and the map scan amortizes the hash
/// over thousands of compares).
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

/// Key equality classes, folded into each hash's first round so a text
/// hash and an identity hash over the same bytes stay distinguishable.
const KEY_CLASS_TEXT: u32 = b't' as u32;
const KEY_CLASS_REF: u32 = b'r' as u32;
const KEY_CLASS_SCALAR: u32 = b's' as u32;

/// The hash of a string key: its content, the text equality class.
fn text_hash(text: &str) -> u32 {
    fnv_step(KEY_CLASS_TEXT, text.as_bytes())
}

/// The hash of an identity key: the reference word's own bits.
fn ref_hash(word: RefId) -> u32 {
    let state = fnv_step(KEY_CLASS_REF, &word.fiber().0.to_le_bytes());
    fnv_step(state, &word.id().to_le_bytes())
}

/// The hash of a scalar key: bits that respect word equality — `+0.0`
/// and `-0.0` compare equal as words, so both fold to one bits form and
/// the filter never rejects an equal pair (every other scalar kind is
/// bits-equal exactly when word-equal; a NaN probe never matches, so its
/// hash is free).
fn scalar_hash(word: Value) -> u32 {
    let (payload, is_f64) = match word {
        Value::I64(n) => (n.to_le_bytes(), false),
        Value::F64(x) => (
            (if x == 0.0 { 0.0 } else { x }).to_bits().to_le_bytes(),
            true,
        ),
        Value::Bool(b) => ([u8::from(b), 0, 0, 0, 0, 0, 0, 0], false),
        Value::Tag(t) => ([t, 0, 0, 0, 0, 0, 0, 0], false),
        Value::Fn(f) => {
            let (module, func) = (f.module().to_le_bytes(), f.func().to_le_bytes());
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&module);
            bytes[4..].copy_from_slice(&func);
            (bytes, false)
        },
        // the probe resolution routes a Ref to the text or identity class
        // first; hashing its bits keeps this total for the dead case
        Value::Ref(r) => {
            let (fiber, id) = (r.fiber().0.to_le_bytes(), r.id().to_le_bytes());
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&fiber);
            bytes[4..].copy_from_slice(&id);
            (bytes, false)
        },
    };
    let state = fnv_step(KEY_CLASS_SCALAR, &payload);
    fnv_step(state, &[u8::from(is_f64)])
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

/// The heap: the id table over ds-rt's counted blocks, the table's free
/// list threading freed ids, and the string intern map.
pub struct Objects {
    /// The owning fiber: the local/foreign test every count mutation on
    /// a stored word runs against (the pool index is the word's owner
    /// axis, stable across slot reuse).
    owner: FiberId,
    interned: HashMap<Box<str>, u32>,
    table: Vec<Slot>,
    heap: Heap,
    free_head: Option<u32>,
    live: usize,
    pending: VecDeque<ForeignOp>,
}

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
            interned: HashMap::new(),
            table: Vec::new(),
            heap: Heap::default(),
            free_head: None,
            live: 0,
            pending: VecDeque::new(),
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
        if let Some(&id) = self.interned.get(text) {
            self.inc(id).expect("an interned object is live");
            return id;
        }
        let id = self.alloc_str(text.to_owned());
        self.interned.insert(text.into(), id);
        id
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
    /// natives over fresh objects).
    pub fn alloc_list(&mut self, elems: Vec<Value>) -> u32 {
        self.alloc(Object::List(elems))
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
    /// the next sibling).
    fn free(&mut self, obj: u32, header: NonNull<RcHeader>) {
        let mut dying: Vec<(u32, NonNull<RcHeader>)> = vec![(obj, header)];
        while let Some((id, header)) = dying.pop() {
            // an interned entry pointing here must not outlive the object:
            // the next intern of the same content allocates a fresh one
            if let Object::Str(s) = self.heap.payload::<Object>(header) {
                if self.interned.get(&*s.text) == Some(&id) {
                    self.interned.remove(&*s.text);
                }
            }
            let next = self.free_head.unwrap_or(NO_FREE);
            self.free_head = Some(id);
            self.table[id as usize] = Slot::Free(next);
            self.live -= 1;
            // the owned words leave the payload before it dies, so the
            // block's death hook drops only empty containers
            let words: Vec<Value> = match self.heap.payload_mut::<Object>(header) {
                Object::List(elems) => std::mem::take(elems),
                Object::Map(data) => data
                    .pairs
                    .drain(..)
                    .flat_map(|(key, value)| [key, value])
                    .collect(),
                Object::Closure(closure) => std::mem::take(&mut closure.captures),
                Object::Str(_) => Vec::new(),
            };
            // the block's death hook (the payload's own drop glue) runs,
            // then the memory pools for the next compatible allocation
            self.heap.rc_dispose::<Object>(header);
            // one owned count per Ref word drops here (C4): lists per
            // element, maps per key and value word, closures per capture.
            // The dec is split so a zero queues the object instead of
            // recursing; a stored reference is live by construction, and
            // the id check keeps the double-drop tripwire
            let mut zeros: Vec<(u32, NonNull<RcHeader>)> = Vec::new();
            for word in words {
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
            dying.extend(zeros.into_iter().rev());
        }
    }

    /// Appends one element word onto a list object in place: the
    /// construction append behind list literals, where the list is
    /// freshly created and uniquely owned by the constructing frame.
    /// Storing a `Ref` element acquires a count for the list — the
    /// staged word stays owned by its own frame too (the placement pass's
    /// native-borrow discipline). Every other in-place mutation stays the
    /// exclusive-window story.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list object.
    pub fn push_list(&mut self, obj: u32, value: Value) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::List(_)) {
            return Err(Trap::UnknownObject { obj });
        }
        // the acquire happens before the store: a bogus element reference
        // leaves the list untouched (a foreign element word queues, so
        // the owning heap takes the list's count)
        if let Value::Ref(word) = value {
            if word.fiber() == self.owner {
                self.inc(word.id())?;
            } else {
                self.pending.push_back(ForeignOp::Inc(word));
            }
        }
        match self.heap.payload_mut::<Object>(header) {
            Object::List(elems) => elems.push(value),
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// Concatenates two lists into a fresh object: `lhs`'s elements then
    /// `rhs`'s, the joined length reserved up front (growth is amortized
    /// doubling; a join never re-grows mid-copy). The joined list owns
    /// one count per `Ref` element word it copies; the operand lists keep
    /// theirs.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when either reference names no list.
    pub fn concat_lists(&mut self, lhs: u32, rhs: u32) -> Result<u32, Trap> {
        let (front, back) = (self.list_at(lhs)?, self.list_at(rhs)?);
        let mut elems = Vec::with_capacity(front.len() + back.len());
        let words: Vec<Value> = front.iter().chain(back.iter()).copied().collect();
        for value in words {
            if let Value::Ref(word) = value {
                self.acquire_stored(word);
            }
            elems.push(value);
        }
        Ok(self.alloc_list(elems))
    }

    /// Concatenates two strings into a fresh object, owned by the caller.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when either reference names no string.
    pub fn concat(&mut self, lhs: u32, rhs: u32) -> Result<u32, Trap> {
        let mut text = String::with_capacity(self.str_at(lhs)?.len() + self.str_at(rhs)?.len());
        text.push_str(self.str_at(lhs)?);
        text.push_str(self.str_at(rhs)?);
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
    /// map on the first char-indexed query (see [`Str`]).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object.
    pub fn char_count(&mut self, obj: u32) -> Result<usize, Trap> {
        self.ensure_char_index(obj)?;
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
        self.ensure_char_index(obj)?;
        let (_, chars) = self.str_with_chars(obj)?;
        Ok((chars.byte_at(start), chars.byte_at(end)))
    }

    /// Builds the char-boundary map of string object `obj` unless it
    /// already exists.
    fn ensure_char_index(&mut self, obj: u32) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if matches!(self.heap.payload::<Object>(header), Object::Str(s) if s.chars.is_some()) {
            return Ok(());
        }
        let built = CharIndex::build(self.str_at(obj)?);
        match self.heap.payload_mut::<Object>(header) {
            Object::Str(s) => s.chars = Some(built),
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

    /// The element words of list object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live list object.
    pub fn list_at(&self, obj: u32) -> Result<&[Value], Trap> {
        match self.payload_of(obj) {
            Some(Object::List(elems)) => Ok(elems),
            _ => Err(Trap::UnknownObject { obj }),
        }
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

    /// Whether `obj` names a live string object holding exactly `text`:
    /// the KSTR pool-const cache's hit validation — a cached id stays
    /// servable only while its content still matches, so a dead or
    /// recycled id never serves.
    pub(crate) fn is_live_str_with(&self, obj: u32, text: &str) -> bool {
        matches!(self.payload_of(obj), Some(Object::Str(s)) if &*s.text == text)
    }

    /// The string content of object `obj`, when it names a live string —
    /// the map probe's foreign-resolution read.
    pub(crate) fn str_if(&self, obj: u32) -> Option<&str> {
        match self.payload_of(obj) {
            Some(Object::Str(s)) => Some(&s.text),
            _ => None,
        }
    }

    /// Resolves a probe word once per lookup or store, payload-locally:
    /// a local word resolves through this store, a foreign word through
    /// the router's pre-resolution, a scalar word needs no payload at
    /// all. The scan then never re-reads the probe's payload per
    /// candidate.
    fn resolve_probe<'a>(&'a self, key: Value, res: &'a ResolvedKey) -> ProbeKey<'a> {
        match res {
            ResolvedKey::Text(text) => ProbeKey::Text {
                hash: text_hash(text),
                text,
            },
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
                        hash: text_hash(&s.text),
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
                // the probe names no string: a local stored string key
                // is a different object by construction (same id would
                // have resolved the probe to text), so identity answers
                Value::Ref(a) => match self.payload_of(a.id()) {
                    Some(Object::Str(_)) => false,
                    _ => a == *word,
                },
                _ => false,
            },
            ProbeKey::Scalar { word, .. } => stored == *word,
        }
    }

    /// The first row whose key equals the probe, if present: the dense
    /// hash scan rejects most candidates without any payload load, and
    /// every hash hit runs the exact check.
    fn map_row_at(&self, data: &MapData, probe: &ProbeKey<'_>) -> Option<usize> {
        let hash = probe.hash();
        for (at, &row_hash) in data.hashes.iter().enumerate() {
            if row_hash == hash {
                let (stored, _) = data.pairs[at];
                if self.stored_key_matches(stored, probe) {
                    return Some(at);
                }
            }
        }
        None
    }

    /// Stores one key-value pair into a map object in place, the exclusive
    /// window's one mutation (the `map_set` native). Every stored `Ref`
    /// word is acquired for the map before it enters an entry, fresh or
    /// overwriting, so the caller's own count stays the caller's and a
    /// later death drops exactly what was stored. A content-equal key
    /// overwrites: the old value's count drops, and the entry keeps its
    /// original key word (the map already owns that count; the caller
    /// keeps its own probe word).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn set_map(
        &mut self,
        obj: u32,
        key: Value,
        res: &ResolvedKey,
        value: Value,
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
        let (found, hash) = match self.payload_of(obj) {
            Some(Object::Map(data)) => {
                let probe = self.resolve_probe(key, res);
                (self.map_row_at(data, &probe), probe.hash())
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        if let Some(at) = found {
            // the store acquires for the map first, whatever it replaces:
            // a value word arriving with its caller's count must not ride
            // that count into the entry (a dying caller would free it)
            if let Value::Ref(word) = value {
                self.acquire_stored(word);
            }
            let old = match self.heap.payload_mut::<Object>(header) {
                Object::Map(data) => std::mem::replace(&mut data.pairs[at].1, value),
                _ => return Err(Trap::UnknownObject { obj }),
            };
            if let Value::Ref(word) = old {
                self.release_stored(word);
            }
            return Ok(());
        }
        // the acquires happen before the store: a bogus reference word
        // leaves the map untouched
        if let Value::Ref(word) = key {
            self.acquire_stored(word);
        }
        if let Value::Ref(word) = value {
            self.acquire_stored(word);
        }
        match self.heap.payload_mut::<Object>(header) {
            Object::Map(data) => {
                data.hashes.push(hash);
                data.pairs.push((key, value));
            },
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// The value word stored under `key` in map object `obj`, if present.
    /// The read is a lookup, not a hand-out: the map keeps every count it
    /// owns, so the caller that wants the value acquires its own.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_at(&self, obj: u32, key: Value, res: &ResolvedKey) -> Result<Option<Value>, Trap> {
        let probe = self.resolve_probe(key, res);
        match self.payload_of(obj) {
            Some(Object::Map(data)) => Ok(self.map_row_at(data, &probe).map(|at| data.pairs[at].1)),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The pairs of map object `obj`, in entry order.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_pairs(&self, obj: u32) -> Result<&[(Value, Value)], Trap> {
        match self.payload_of(obj) {
            Some(Object::Map(data)) => Ok(&data.pairs),
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

    /// Drains every store's queued cross-heap operations until none
    /// remain: a routed dec can free an object whose cascade queues
    /// more, on any store, so the loop runs to quiescence. Order within
    /// one queue is FIFO — a cascade's cross-heap decs fire in the
    /// traversal order the queueing store saw.
    fn settle(&mut self) -> Result<(), Trap> {
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
        self.settle()
    }

    /// `rc_inc` routed by the word's owner for an object-graph edge (a
    /// closure's capture adoption, a list push): the count belongs to the
    /// holding object, released by its free cascade — no frame claim.
    pub(crate) fn inc_edge(&mut self, word: RefId) -> Result<(), Trap> {
        self.store(word)?.inc(word.id())?;
        self.settle()
    }

    /// `rc_dec` routed by the word's owner, the claim the running
    /// fiber's, queues drained.
    pub(crate) fn dec(&mut self, word: RefId) -> Result<(), Trap> {
        self.store(word)?.dec(word.id())?;
        self.ledger(self.owner).release(word);
        self.settle()
    }

    /// `rc_dec` routed for a death walk: no ledger (the claims were
    /// taken before the walk began) and no live-context assumption (a
    /// parked drive's context store is the scratch) — the word's own
    /// heap takes the dec through the pool, queues drained.
    pub(crate) fn dec_walk(&mut self, word: RefId) -> Result<(), Trap> {
        self.store(word)?.dec(word.id())?;
        self.settle()
    }

    /// The string content of `word`, routed.
    pub(crate) fn str_at(&mut self, word: RefId) -> Result<&str, Trap> {
        let store = self.store(word)?;
        let text = store.str_at(word.id())?;
        Ok(text)
    }

    /// Whether two reference words name content-equal strings: a
    /// same-store pair compares in place, a cross-store pair copies one
    /// side (string equality is content equality whatever heap each
    /// side lives on).
    pub(crate) fn str_eq(&mut self, x: RefId, y: RefId) -> Result<bool, Trap> {
        if x.fiber() == y.fiber() {
            let store = self.store(x)?;
            let a = store.str_at(x.id())?;
            let b = store.str_at(y.id())?;
            return Ok(a == b);
        }
        let a = self.store(x)?.str_at(x.id())?.to_owned();
        let b = self.store(y)?.str_at(y.id())?;
        Ok(a == b)
    }

    /// Concatenates two strings into a fresh object on the running
    /// fiber's store, reading each operand from its own heap.
    pub(crate) fn concat(&mut self, x: RefId, y: RefId) -> Result<u32, Trap> {
        if x.fiber() == y.fiber() && x.fiber() == self.owner {
            let id = self.current.concat(x.id(), y.id())?;
            return Ok(id);
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

    /// The element words of list object `word`, routed: a borrowed slice
    /// of the owning store — copy a word out before another routed call.
    pub(crate) fn list_at(&mut self, word: RefId) -> Result<&[Value], Trap> {
        let store = self.store(word)?;
        let elems = store.list_at(word.id())?;
        Ok(elems)
    }

    /// Concatenates two lists into a fresh object on the running fiber's
    /// store: a same-store pair runs in place, a cross-store pair copies
    /// the element words out and acquires one count per `Ref` element on
    /// the element's own heap.
    pub(crate) fn concat_lists(&mut self, x: RefId, y: RefId) -> Result<u32, Trap> {
        if x.fiber() == y.fiber() && x.fiber() == self.owner {
            let id = self.current.concat_lists(x.id(), y.id())?;
            self.settle()?;
            return Ok(self.claim_result(id));
        }
        let front: Vec<Value> = self.list_at(x)?.to_vec();
        let back: Vec<Value> = self.list_at(y)?.to_vec();
        let mut elems = Vec::with_capacity(front.len() + back.len());
        for value in front.into_iter().chain(back) {
            if let Value::Ref(word) = value {
                self.inc_edge(word)?;
            }
            elems.push(value);
        }
        let id = self.current.alloc_list(elems);
        Ok(self.claim_result(id))
    }

    /// Appends one element word onto list object `list`, routed: the
    /// payload mutation lands on the list's store and a `Ref` element's
    /// acquired count on the element's own heap.
    pub(crate) fn push_list(&mut self, list: RefId, value: Value) -> Result<(), Trap> {
        self.store(list)?.push_list(list.id(), value)?;
        self.settle()
    }

    /// Resolves a map probe word for the map's store: a local (or
    /// scalar) word compares through the store itself; a foreign word's
    /// string content resolves here, on the probe's own heap, so the
    /// content compare never reads the wrong store's payload.
    fn resolve_key(&mut self, map: FiberId, key: Value) -> Result<ResolvedKey, Trap> {
        let Value::Ref(word) = key else {
            return Ok(ResolvedKey::Word);
        };
        if word.fiber() == map {
            return Ok(ResolvedKey::Word);
        }
        let text = self.store(word)?.str_if(word.id()).map(str::to_owned);
        Ok(match text {
            Some(text) => ResolvedKey::Text(text.into()),
            None => ResolvedKey::Opaque,
        })
    }

    /// The value word stored under `key` in map object `map`, routed.
    pub(crate) fn map_at(&mut self, map: RefId, key: Value) -> Result<Option<Value>, Trap> {
        let res = self.resolve_key(map.fiber(), key)?;
        self.store(map)?.map_at(map.id(), key, &res)
    }

    /// Stores one key-value pair into map object `map`, routed, with the
    /// queues drained.
    pub(crate) fn set_map(&mut self, map: RefId, key: Value, value: Value) -> Result<(), Trap> {
        let res = self.resolve_key(map.fiber(), key)?;
        self.store(map)?.set_map(map.id(), key, &res, value)?;
        self.settle()
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

    /// The pairs of map object `map`, in entry order, routed.
    pub(crate) fn map_pairs(&mut self, map: RefId) -> Result<&[(Value, Value)], Trap> {
        let store = self.store(map)?;
        let pairs = store.map_pairs(map.id())?;
        Ok(pairs)
    }
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
    /// A foreign string probe's content, resolved on the probe's heap.
    Text(Box<str>),
    /// A foreign reference probe naming no string.
    Opaque,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fiber::FIBER_ROOT;

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
                Value::I64(80),
            )
            .expect("the map object accepts the pair");
        // the fresh object's content names the interned key's entry
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, fresh)),
                &ResolvedKey::Word
            ),
            Ok(Some(Value::I64(80))),
            "string keys match by content, not object identity"
        );
        assert_eq!(
            objects.map_at(map, Value::I64(80), &ResolvedKey::Word),
            Ok(None)
        );
        // a scalar key of its own
        objects
            .set_map(map, Value::I64(1), &ResolvedKey::Word, Value::Bool(true))
            .expect("the map accepts scalars");
        assert_eq!(
            objects.map_at(map, Value::I64(1), &ResolvedKey::Word),
            Ok(Some(Value::Bool(true)))
        );
        assert_eq!(objects.map_pairs(map).map(<[_]>::len), Ok(2));
        // the overwrite keeps one entry and drops the old value's count
        objects
            .set_map(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, fresh)),
                &ResolvedKey::Word,
                Value::I64(8080),
            )
            .expect("the content-equal key overwrites");
        assert_eq!(objects.map_pairs(map).map(<[_]>::len), Ok(2));
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, interned)),
                &ResolvedKey::Word
            ),
            Ok(Some(Value::I64(8080)))
        );
        // wrong-kind reads name no object
        assert_eq!(objects.list_at(map), Err(Trap::UnknownObject { obj: map }));
        assert_eq!(objects.str_at(map), Err(Trap::UnknownObject { obj: map }));
        // a set over a non-map reports the bogus side
        assert_eq!(
            objects.set_map(interned, Value::I64(1), &ResolvedKey::Word, Value::I64(2)),
            Err(Trap::UnknownObject { obj: interned })
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
                Value::I64(80),
            )
            .expect("the local key stores");
        let foreign_text: Box<str> = "host".into();
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Text(foreign_text.clone())
            ),
            Ok(Some(Value::I64(80))),
            "a foreign string probe matches a local key by content"
        );
        let other_text: Box<str> = "port".into();
        assert_eq!(
            objects.map_at(
                map,
                Value::Ref(RefId::new(FIBER_ROOT, key)),
                &ResolvedKey::Text(other_text)
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

    /// Word equality is the scalar key rule, so the hash filter folds
    /// `+0.0` and `-0.0` to one form: either spelling finds the other's
    /// entry, while an int and a float over equal-looking bytes stay
    /// distinct keys.
    #[test]
    fn signed_zero_scalar_keys_match_through_the_filter() {
        let mut objects = Objects::new(FIBER_ROOT);
        let map = objects.alloc_map();
        objects
            .set_map(map, Value::F64(0.0), &ResolvedKey::Word, Value::Bool(true))
            .expect("the positive zero stores");
        assert_eq!(
            objects.map_at(map, Value::F64(-0.0), &ResolvedKey::Word),
            Ok(Some(Value::Bool(true))),
            "the two zero spellings are one scalar key"
        );
        assert_eq!(
            objects.map_at(map, Value::I64(0), &ResolvedKey::Word),
            Ok(None),
            "an int key never matches a float key"
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
                Value::Ref(RefId::new(FIBER_ROOT, value)),
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
                Value::I64(8080),
            )
            .expect("the content-equal key overwrites");
        assert_eq!(objects.rc_at(value), Ok(1));
        assert_eq!(objects.rc_at(key), Ok(2));
        let other = objects.alloc_str("port".to_owned());
        objects
            .set_map(
                map,
                Value::I64(1),
                &ResolvedKey::Word,
                Value::Ref(RefId::new(FIBER_ROOT, other)),
            )
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
}
