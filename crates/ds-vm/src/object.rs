//! The VM-side heap object store [BL-2]: the objects raw `Ref` words name.
//!
//! Identity is the id table: a `Value::Ref(u32)` word is an index into
//! `table`, and every live entry names one counted block of the ds-rt heap —
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
//! Counts are non-atomic: one `Objects` store belongs to one VM on one
//! thread [C4]. The `diag` feature arms ds-rt's balance tripwires (rc
//! underflow/overflow asserts, the borrow-at-death check); the store
//! holds no borrows, so a firing tripwire is a balance bug.

use std::{collections::HashMap, fmt, ptr::NonNull, time::Duration};

use ds_rt::{Heap, RcHeader, drop_deinit};

use crate::{
    net::Connections,
    value::{FuncRef, Value},
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
    /// reference keys never collide across kinds.
    Map(Vec<(Value, Value)>),
}

/// One id-table entry: a live id names the header of its ds-rt block
/// (the payload sits at the block's fixed payload offset, and the kind
/// lives in the payload), or a freed id threads the free list.
#[derive(Clone, Copy, Debug)]
enum Slot {
    Live(NonNull<RcHeader>),
    Free(u32),
}

/// The heap: the id table over ds-rt's counted blocks, the table's free
/// list threading freed ids, and the string intern map.
#[derive(Default)]
pub struct Objects {
    interned: HashMap<Box<str>, u32>,
    table: Vec<Slot>,
    heap: Heap,
    free_head: Option<u32>,
    live: usize,
    /// The connection table of the net natives ([`crate::net`]): sockets
    /// are VM-lifetime host state like heap objects, and the native
    /// calling convention hands a body exactly two host references (this
    /// store, the output sink), so the table rides the store the VM owns
    /// and drops — closing every open socket when the VM goes away.
    net: Connections,
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
    /// An empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
                Object::Map(pairs) => pairs
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
                let child_header = match self.table.get(child as usize) {
                    Some(Slot::Live(header)) => *header,
                    _ => panic!("a stored element reference is live"),
                };
                if self.heap.rc_dec_count(child_header) {
                    zeros.push((child, child_header));
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
        // leaves the list untouched
        if let Value::Ref(id) = value {
            self.inc(id)?;
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
            if let Value::Ref(id) = value {
                self.inc(id)?;
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
        self.alloc(Object::Map(Vec::new()))
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

    /// Whether two key words name the same key: string references compare
    /// by content (one key per content, whatever object carries it),
    /// every other word by identity — mixed kinds never match.
    fn key_matches(&self, stored: Value, probe: Value) -> bool {
        match (stored, probe) {
            (Value::Ref(a), Value::Ref(b)) => match (self.payload_of(a), self.payload_of(b)) {
                (Some(Object::Str(x)), Some(Object::Str(y))) => x.text == y.text,
                _ => a == b,
            },
            _ => stored == probe,
        }
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
    pub fn set_map(&mut self, obj: u32, key: Value, value: Value) -> Result<(), Trap> {
        let header = match self.table.get(obj as usize) {
            Some(Slot::Live(header))
                if matches!(self.heap.payload::<Object>(*header), Object::Map(_)) =>
            {
                *header
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        if let Some(at) = self.map_pair_at(obj, key)? {
            // the store acquires for the map first, whatever it replaces:
            // a value word arriving with its caller's count must not ride
            // that count into the entry (a dying caller would free it)
            if let Value::Ref(id) = value {
                self.inc(id)?;
            }
            let old = match self.heap.payload_mut::<Object>(header) {
                Object::Map(pairs) => std::mem::replace(&mut pairs[at].1, value),
                _ => return Err(Trap::UnknownObject { obj }),
            };
            if let Value::Ref(id) = old {
                self.dec(id).expect("a stored pair reference is live");
            }
            return Ok(());
        }
        // the acquires happen before the store: a bogus reference word
        // leaves the map untouched
        if let Value::Ref(id) = key {
            self.inc(id)?;
        }
        if let Value::Ref(id) = value {
            self.inc(id)?;
        }
        match self.heap.payload_mut::<Object>(header) {
            Object::Map(pairs) => pairs.push((key, value)),
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// The entry position of `key` in map object `obj`, if present.
    fn map_pair_at(&self, obj: u32, key: Value) -> Result<Option<usize>, Trap> {
        match self.payload_of(obj) {
            Some(Object::Map(pairs)) => Ok(pairs
                .iter()
                .position(|(stored, _)| self.key_matches(*stored, key))),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The value word stored under `key` in map object `obj`, if present.
    /// The read is a lookup, not a hand-out: the map keeps every count it
    /// owns, so the caller that wants the value acquires its own.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_at(&self, obj: u32, key: Value) -> Result<Option<Value>, Trap> {
        match self.payload_of(obj) {
            Some(Object::Map(pairs)) => Ok(pairs
                .iter()
                .find(|(stored, _)| self.key_matches(*stored, key))
                .map(|(_, value)| *value)),
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
            Some(Object::Map(pairs)) => Ok(pairs),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The connection table of the net natives.
    pub(crate) fn net(&mut self) -> &mut Connections {
        &mut self.net
    }

    /// Bounds every blocking net wait ([`crate::net`]).
    pub(crate) fn set_net_timeout(&mut self, timeout: Duration) {
        self.net.set_timeout(timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_shares_by_content_and_alloc_does_not() {
        let mut objects = Objects::new();
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
        let mut objects = Objects::new();
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
        let mut objects = Objects::new();
        let a = objects.intern("a");
        let scalars = objects.alloc_list(vec![Value::I64(1), Value::Bool(false)]);
        let strings = objects.alloc_list(vec![Value::Ref(a)]);
        let joined = objects
            .concat_lists(scalars, strings)
            .expect("both lists live");
        assert_eq!(
            objects.list_at(joined),
            Ok(&[Value::I64(1), Value::Bool(false), Value::Ref(a)][..]),
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
        let mut objects = Objects::new();
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
        let mut objects = Objects::new();
        // each level's birth count becomes its parent's stored count (the
        // list adopts the Ref word as-is), so the outermost's single count
        // owns the whole chain
        let mut node = objects.alloc_list(Vec::new());
        for _ in 1..DEPTH {
            node = objects.alloc_list(vec![Value::Ref(node)]);
        }
        objects.dec(node).expect("the outermost drops");
        assert!(objects.is_empty(), "the whole cascade died");
    }

    /// A map stores and looks up by content keys: two equal-content
    /// string objects are one key, a scalar key never matches a string,
    /// and an overwrite replaces the value while keeping the entry count.
    #[test]
    fn maps_store_and_lookup_by_content_keys() {
        let mut objects = Objects::new();
        let interned = objects.intern("host");
        let fresh = objects.alloc_str("host".to_owned());
        assert_ne!(interned, fresh, "the fresh word is a distinct object");
        let map = objects.alloc_map();
        objects
            .set_map(map, Value::Ref(interned), Value::I64(80))
            .expect("the map object accepts the pair");
        // the fresh object's content names the interned key's entry
        assert_eq!(
            objects.map_at(map, Value::Ref(fresh)),
            Ok(Some(Value::I64(80))),
            "string keys match by content, not object identity"
        );
        assert_eq!(objects.map_at(map, Value::I64(80)), Ok(None));
        // a scalar key of its own
        objects
            .set_map(map, Value::I64(1), Value::Bool(true))
            .expect("the map accepts scalars");
        assert_eq!(
            objects.map_at(map, Value::I64(1)),
            Ok(Some(Value::Bool(true)))
        );
        assert_eq!(objects.map_pairs(map).map(<[_]>::len), Ok(2));
        // the overwrite keeps one entry and drops the old value's count
        objects
            .set_map(map, Value::Ref(fresh), Value::I64(8080))
            .expect("the content-equal key overwrites");
        assert_eq!(objects.map_pairs(map).map(<[_]>::len), Ok(2));
        assert_eq!(
            objects.map_at(map, Value::Ref(interned)),
            Ok(Some(Value::I64(8080)))
        );
        // wrong-kind reads name no object
        assert_eq!(objects.list_at(map), Err(Trap::UnknownObject { obj: map }));
        assert_eq!(objects.str_at(map), Err(Trap::UnknownObject { obj: map }));
        // a set over a non-map reports the bogus side
        assert_eq!(
            objects.set_map(interned, Value::I64(1), Value::I64(2)),
            Err(Trap::UnknownObject { obj: interned })
        );
    }

    /// A shared list decs to zero without freeing; the unique death frees
    /// the list and drops its acquired element counts (a list of lists
    /// dies recursively).
    #[test]
    fn a_list_death_drops_its_element_counts() {
        let mut objects = Objects::new();
        // a fresh element whose birth count is the adopting list's (the
        // producer-native construction rule)
        let s = objects.alloc_str("elem".to_owned());
        let inner = objects.alloc_list(vec![Value::Ref(s)]);
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
        let mut objects = Objects::new();
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
        let mut objects = Objects::new();
        let s = objects.alloc_str("cap".to_owned());
        // the moved-in word keeps its count: the closure owns it now
        let id = objects.alloc_closure(FuncRef::new(0, 0), vec![Value::Ref(s)]);
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
        let mut objects = Objects::new();
        let map = objects.alloc_map();
        let key = objects.alloc_str("host".to_owned());
        let value = objects.alloc_str("localhost".to_owned());
        objects
            .set_map(map, Value::Ref(key), Value::Ref(value))
            .expect("the map accepts the pair");
        // the store acquired one count per stored Ref word, on top of the
        // caller's own
        assert_eq!(objects.rc_at(key), Ok(2));
        assert_eq!(objects.rc_at(value), Ok(2));
        // the overwrite drops the replaced value's acquired count and
        // keeps the key's
        objects
            .set_map(map, Value::Ref(key), Value::I64(8080))
            .expect("the content-equal key overwrites");
        assert_eq!(objects.rc_at(value), Ok(1));
        assert_eq!(objects.rc_at(key), Ok(2));
        let other = objects.alloc_str("port".to_owned());
        objects
            .set_map(map, Value::I64(1), Value::Ref(other))
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
