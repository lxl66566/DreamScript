//! The routing face: claim ledgers, store routing, and cross-heap settling.

use super::*;

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
    /// context store is the scratch — then reads and decs route through the
    /// pool (the death walk's form), but allocation still lands in `current`,
    /// so a word a seam hands a parked fiber belongs in that fiber's own store
    /// swap and never through this router.
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
    pub(super) fn ledger(&mut self, fiber: FiberId) -> &mut ClaimTable {
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
    pub(super) fn store(&mut self, word: RefId) -> Result<&mut Objects, Trap> {
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
    pub(super) fn store_shared(&self, word: RefId) -> Result<&Objects, Trap> {
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
    pub(super) fn queued(&self, fiber: FiberId) -> usize {
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
    pub(super) fn settle(&mut self, from: FiberId) -> Result<(), Trap> {
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
    pub(super) fn claimed_alloc(&mut self, id: u32) -> Value {
        let word = RefId::new(self.owner, id);
        self.ledger(self.owner).acquire(word);
        Value::Ref(word)
    }

    /// Releases the running fiber's frame claim on `word` without
    /// touching the count: a fresh word's birth count (or a retained
    /// copy's count) becomes the adopting container's edge, so the frame
    /// stopped owning it and the container's free cascade will drop it.
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

    /// `rc_dec` routed by the word's owner for an object-graph edge: the
    /// replaced capture word a closure's mut store releases. The count
    /// belonged to the holding object (the store-acquire edge the window
    /// now spends), so no ledger row rides the release.
    pub(crate) fn dec_edge(&mut self, word: RefId) -> Result<(), Trap> {
        self.store(word)?.dec(word.id())?;
        self.settle(word.fiber())
    }

    /// Replaces one word of closure object `word`'s capture block,
    /// routed by the word's owner (the mut window over a foreign-held
    /// closure serves on its own heap).
    pub(crate) fn replace_capture(
        &mut self,
        word: RefId,
        idx: usize,
        value: Value,
    ) -> Result<Value, Trap> {
        self.store(word)?.replace_capture(word.id(), idx, value)
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
    pub(super) fn claim_result(&mut self, id: u32) -> u32 {
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

    /// Whether object `word`'s live count is one: the copy-before-write
    /// door's read, routed shared (a count read mutates no count and
    /// queues nothing).
    fn count_is_unique(&self, word: RefId) -> Result<bool, Trap> {
        Ok(self.store_shared(word)?.rc_at(word.id())? == 1)
    }

    /// The mutating list members' door: answers the receiver word the
    /// member's result owns (one count) beside the object to mutate —
    /// the receiver's own object at a unique count, a fresh copy at a
    /// shared one, the other holders keeping the original. The caller
    /// mutates the answered word's object in place either way: the copy
    /// is born unique, and the member's call site re-stores the word
    /// through the receiver's window.
    pub(crate) fn list_mutation_word(&mut self, list: RefId) -> Result<(RefId, Value), Trap> {
        if self.count_is_unique(list)? {
            self.inc(list)?;
            return Ok((list, Value::Ref(list)));
        }
        match self.copy_list(list)? {
            Value::Ref(word) => Ok((word, Value::Ref(word))),
            _ => unreachable!("a fresh list is its reference word"),
        }
    }

    /// The mutating map members' door, [`Heaps::list_mutation_word`]'s
    /// map shape.
    pub(crate) fn map_mutation_word(&mut self, map: RefId) -> Result<(RefId, Value), Trap> {
        if self.count_is_unique(map)? {
            self.inc(map)?;
            return Ok((map, Value::Ref(map)));
        }
        match self.copy_map(map)? {
            Value::Ref(word) => Ok((word, Value::Ref(word))),
            _ => unreachable!("a fresh map is its reference word"),
        }
    }

    /// The owning container's store door, [`Heaps::list_mutation_word`]'s
    /// cell shape.
    pub(crate) fn cell_mutation_word(&mut self, cell: RefId) -> Result<(RefId, Value), Trap> {
        if self.count_is_unique(cell)? {
            self.inc(cell)?;
            return Ok((cell, Value::Ref(cell)));
        }
        match self.copy_cell(cell)? {
            Value::Ref(word) => Ok((word, Value::Ref(word))),
            _ => unreachable!("a fresh cell is its reference word"),
        }
    }

    /// A fresh list over `list`'s element words on the running fiber's
    /// store: one edge count per `Ref` element acquired on the word's
    /// own heap, the source keeping its own. The stride carries; the
    /// birth count is the caller's claim.
    fn copy_list(&mut self, list: RefId) -> Result<Value, Trap> {
        let (elems, stride) = self.list_at(list)?;
        let elems = elems.to_vec();
        for value in &elems {
            if let Value::Ref(word) = *value {
                self.inc_edge(word)?;
            }
        }
        Ok(self.new_list_strided(elems, stride))
    }

    /// A fresh map over `map`'s rows on the map's own store: the rows'
    /// key words are local to it, so the store seam's fresh-row rule
    /// keeps holding (a copy on the running fiber's store would refuse
    /// every foreign key). Each row re-stores through the store seam's
    /// own acquires, one count per `Ref` word of key and value alike,
    /// and the entry order carries.
    fn copy_map(&mut self, map: RefId) -> Result<Value, Trap> {
        let id = self.store(map)?.alloc_map();
        let fresh = RefId::new(map.fiber(), id);
        self.ledger(self.owner).acquire(fresh);
        let keys = self.map_keys(map)?;
        let mut row = Vec::new();
        for key in keys {
            if self.map_row_scratch(map, key, &mut row)? {
                self.set_map(fresh, key, &row)?;
            }
        }
        Ok(Value::Ref(fresh))
    }

    /// A fresh owning container over `cell`'s held words on the running
    /// fiber's store: one edge count per `Ref` word acquired on the
    /// word's own heap, the source keeping its own. The birth count is
    /// the caller's claim.
    fn copy_cell(&mut self, cell: RefId) -> Result<Value, Trap> {
        let words = self.store(cell)?.cell_words(cell.id())?;
        self.alloc_cell(&words)
    }

    /// Allocates a fresh bounded channel of capacity `cap` on the
    /// running fiber's store — the creating fiber is its one owning heap —
    /// the birth count claimed for the caller's frame.
    pub(crate) fn alloc_chan(&mut self, cap: usize) -> Value {
        let id = self.current.alloc_chan(cap);
        self.claimed_alloc(id)
    }

    /// Allocates a fresh empty arena on the running fiber's store, the
    /// birth count claimed for the caller's frame — exactly a fresh
    /// list's.
    pub(crate) fn alloc_arena(&mut self) -> Value {
        let id = self.current.alloc_arena();
        self.claimed_alloc(id)
    }

    /// Allocates a fresh owning container over `words` on the running
    /// fiber's store, the birth count claimed for the caller's frame:
    /// one count per `Ref` word acquires here (a foreign word's inc
    /// posting through the pending funnel), the staged words' own
    /// counts dying at the call's window flush, so the fresh object's
    /// edges are its own.
    pub(crate) fn alloc_cell(&mut self, words: &[Value]) -> Result<Value, Trap> {
        let mut held = Vec::with_capacity(words.len());
        for value in words {
            if let Value::Ref(word) = value {
                self.inc_edge(*word)?;
            }
            held.push(*value);
        }
        let id = self.current.alloc_cell(held);
        Ok(self.claimed_alloc(id))
    }

    /// The held word span of owning container `cell`, one fresh count
    /// per `Ref` word retained for the reading frame (the container
    /// keeps its own) — the checked list read-out's contract.
    pub(crate) fn cell_get(&mut self, cell: RefId) -> Result<Vec<Value>, Trap> {
        let words = self.store(cell)?.cell_words(cell.id())?;
        for value in &words {
            if let Value::Ref(word) = value {
                self.inc(*word)?;
            }
        }
        Ok(words)
    }

    /// The consuming take: the held word span moves out as the answer.
    /// At a unique count the container's own edge counts become the
    /// answer's — the ledger rows adopted with no count mutation, the
    /// emptied container freeing at the caller's dying share (the
    /// window flush's dec) — and at a shared one the value copies out
    /// under fresh counts (the value door; every holder keeps the
    /// container whole).
    pub(crate) fn cell_take(&mut self, cell: RefId) -> Result<Vec<Value>, Trap> {
        if self.count_is_unique(cell)? {
            let words = self.store(cell)?.cell_take_words(cell.id())?;
            for value in &words {
                if let Value::Ref(word) = value {
                    self.adopt_claim(self.owner, *word);
                }
            }
            Ok(words)
        } else {
            self.cell_get(cell)
        }
    }

    /// Replaces owning container `cell`'s whole held span, routed: the
    /// mutation lands on the container's store, the incoming span's
    /// acquired counts beside it and the held span's releases routed
    /// after the swap.
    pub(crate) fn cell_set(&mut self, cell: RefId, words: &[Value]) -> Result<(), Trap> {
        self.store(cell)?.set_cell(cell.id(), words)?;
        self.settle(cell.fiber())
    }

    /// Stores one element word into arena `arena`, routed: the mutation
    /// and the element's edge count land on the arena's store (a
    /// foreign element's inc posting through the pending funnel).
    /// Answers the packed `(index, generation)` handle.
    pub(crate) fn arena_insert(&mut self, arena: RefId, word: Value) -> Result<i64, Trap> {
        let handle = self.store(arena)?.arena_insert(arena.id(), word)?;
        self.settle(arena.fiber())?;
        Ok(handle)
    }

    /// The element word behind a live handle in arena `arena`, routed:
    /// a `Ref` element answers as one retained count for the reading
    /// frame (the arena keeps its own), exactly a checked list
    /// read-out.
    pub(crate) fn arena_at(&mut self, arena: RefId, handle: i64) -> Result<Value, Trap> {
        let word = self.store(arena)?.arena_word(arena.id(), handle)?;
        if let Value::Ref(word) = word {
            self.inc(word)?;
        }
        Ok(word)
    }

    /// Removes the element behind a live handle in arena `arena`,
    /// routed: the stored word's edge count releases on the element's
    /// own heap, the queues draining with it.
    pub(crate) fn arena_remove(&mut self, arena: RefId, handle: i64) -> Result<(), Trap> {
        self.store(arena)?.arena_remove(arena.id(), handle)?;
        self.settle(arena.fiber())
    }

    /// The arena's live element count, routed as a shared read.
    pub(crate) fn arena_len(&self, arena: RefId) -> Result<usize, Trap> {
        self.store_shared(arena)?.arena_len(arena.id())
    }

    /// Whether channel `chan`'s buffer has room for one more payload
    /// element (the word count against the element stride), routed as a
    /// shared read.
    pub(crate) fn chan_room(&mut self, chan: RefId) -> Result<bool, Trap> {
        let store = self.store(chan)?;
        match store.payload_chan(chan.id())? {
            Some(data) => Ok(data.elems.len() / data.stride.max(1) < data.cap),
            None => Err(Trap::UnknownObject { obj: chan.id() }),
        }
    }

    /// Deposits one payload element's whole word span onto channel
    /// `chan`'s buffer tail, routed: the mutation lands on the
    /// channel's store and each `Ref` payload word's edge count on the
    /// word's own heap (the pending funnel draining here).
    pub(crate) fn chan_deposit(&mut self, chan: RefId, words: &[Value]) -> Result<(), Trap> {
        self.store(chan)?.chan_deposit(chan.id(), words)?;
        self.settle(chan.fiber())
    }

    /// Pops one payload element's whole word span off channel `chan`'s
    /// buffer head, routed. No count moves: the buffer's edge counts
    /// become the receiving frame's backing, one claim per `Ref` word
    /// taken through [`Heaps::adopt_claim`].
    pub(crate) fn chan_pop(&mut self, chan: RefId) -> Result<Option<ChanPop>, Trap> {
        self.store(chan)?.chan_pop(chan.id())
    }

    /// Stages one parked sender onto channel `chan`, routed: the fiber
    /// and its payload's whole word span enter the channel's sender
    /// queue with no count traffic (the parked frame keeps its own
    /// counts).
    pub(crate) fn chan_park_sender(
        &mut self,
        chan: RefId,
        fiber: FiberId,
        payload: Vec<Value>,
    ) -> Result<(), Trap> {
        let store = self.store(chan)?;
        match store.payload_chan_mut(chan.id())? {
            Some(data) => {
                data.senders.push_back(ChanSender { fiber, payload });
                Ok(())
            },
            None => Err(Trap::UnknownObject { obj: chan.id() }),
        }
    }

    /// Stages one parked receiver onto channel `chan`, routed.
    pub(crate) fn chan_park_receiver(&mut self, chan: RefId, fiber: FiberId) -> Result<(), Trap> {
        let store = self.store(chan)?;
        match store.payload_chan_mut(chan.id())? {
            Some(data) => {
                data.receivers.push_back(fiber);
                Ok(())
            },
            None => Err(Trap::UnknownObject { obj: chan.id() }),
        }
    }

    /// Pops the head parked sender off channel `chan`, routed — the
    /// caller checks the fiber's state before using the payload.
    pub(crate) fn chan_take_sender(&mut self, chan: RefId) -> Result<Option<ChanSender>, Trap> {
        let store = self.store(chan)?;
        match store.payload_chan_mut(chan.id())? {
            Some(data) => Ok(data.senders.pop_front()),
            None => Err(Trap::UnknownObject { obj: chan.id() }),
        }
    }

    /// Peeks the head parked receiver off channel `chan`, routed — the
    /// rendezvous send's pairing door reads it without consuming the
    /// entry (the release that follows performs the take).
    pub(crate) fn chan_peek_receiver(&mut self, chan: RefId) -> Result<Option<FiberId>, Trap> {
        let store = self.store(chan)?;
        match store.payload_chan(chan.id())? {
            Some(data) => Ok(data.receivers.front().copied()),
            None => Err(Trap::UnknownObject { obj: chan.id() }),
        }
    }

    /// Pops the head parked receiver off channel `chan`, routed — the
    /// caller checks the fiber's state before staging its delivery.
    pub(crate) fn chan_take_receiver(&mut self, chan: RefId) -> Result<Option<FiberId>, Trap> {
        let store = self.store(chan)?;
        match store.payload_chan_mut(chan.id())? {
            Some(data) => Ok(data.receivers.pop_front()),
            None => Err(Trap::UnknownObject { obj: chan.id() }),
        }
    }

    /// Removes one fiber's waiter entry from channel `chan`'s queue
    /// (`sender` names which), routed — the deadline and cancel doors'
    /// deregistration. The sender queue's staged span drops with the
    /// entry (its counts the parked frame's own); the entries behind it
    /// shift up, so a later peer still pairs in park order and no
    /// stale head can swallow a wake. Answers whether an entry went.
    pub(crate) fn chan_remove_waiter(
        &mut self,
        chan: RefId,
        fiber: FiberId,
        sender: bool,
    ) -> Result<bool, Trap> {
        let store = self.store(chan)?;
        match store.payload_chan_mut(chan.id())? {
            Some(data) => {
                let len = |data: &ChanData| {
                    if sender { data.senders.len() } else { data.receivers.len() }
                };
                let before = len(data);
                if sender {
                    data.senders.retain(|waiter| waiter.fiber != fiber);
                } else {
                    data.receivers.retain(|&waiter| waiter != fiber);
                }
                Ok(len(data) != before)
            },
            None => Err(Trap::UnknownObject { obj: chan.id() }),
        }
    }

    /// Adopts one existing count into `claimant`'s ledger with no count
    /// mutation: the channel receive's transfer — the buffer's edge
    /// count becomes the receiving frame's backing, so the frame's
    /// later dec (or its death walk) releases exactly the count the
    /// buffer held.
    pub(crate) fn adopt_claim(&mut self, claimant: FiberId, word: RefId) {
        self.ledger(claimant).acquire(word);
    }

    /// Resolves a map probe word for the map's store, the read funnels'
    /// shared face: a local (or scalar) word compares through the store
    /// itself; a foreign word's string content and hash resolve here, on
    /// the probe's own heap, so the content compare never reads the wrong
    /// store's payload and the map's store never re-hashes the probe. The
    /// foreign text borrows its own heap shared — no owned copy per probe
    /// — and the read queues no cross-heap operation, so it owes no drain.
    pub(super) fn resolve_key_shared(
        &self,
        map: FiberId,
        key: Value,
    ) -> Result<ResolvedKey<'_>, Trap> {
        let Value::Ref(word) = key else {
            return Ok(ResolvedKey::Word);
        };
        if word.fiber() == map {
            return Ok(ResolvedKey::Word);
        }
        let probe = self.store_shared(word)?.str_with_hash(word.id());
        Ok(match probe {
            Some((text, hash)) => ResolvedKey::Text { text, hash },
            None => ResolvedKey::Opaque,
        })
    }

    /// The same resolution for a mutating funnel: the foreign text lands
    /// in `scratch` (one copy per store op, the store's exclusive borrow
    /// window demanding a text the router does not hold), the resolution
    /// borrowing it.
    pub(super) fn resolve_key_staged<'s>(
        &mut self,
        map: FiberId,
        key: Value,
        scratch: &'s mut String,
    ) -> Result<ResolvedKey<'s>, Trap> {
        let Value::Ref(word) = key else {
            return Ok(ResolvedKey::Word);
        };
        if word.fiber() == map {
            return Ok(ResolvedKey::Word);
        }
        Ok(match self.store(word)?.str_with_hash(word.id()) {
            Some((text, hash)) => {
                scratch.clear();
                scratch.push_str(text);
                ResolvedKey::Text {
                    text: scratch.as_str(),
                    hash,
                }
            },
            None => ResolvedKey::Opaque,
        })
    }

    /// The value span stored under `key` in map object `map`, routed and
    /// copied out: the slice leaves the owning store as owned words, so
    /// the caller may route count mutations while holding it.
    pub(crate) fn map_at(&mut self, map: RefId, key: Value) -> Result<Option<Vec<Value>>, Trap> {
        let res = self.resolve_key_shared(map.fiber(), key)?;
        match self.store_shared(map)?.map_at(map.id(), key, &res)? {
            Some(span) => Ok(Some(span.to_vec())),
            None => Ok(None),
        }
    }

    /// The value span stored under `key` in map object `map`, routed into
    /// `scratch`: the row read for walks that pair one map's rows with
    /// another's. The span leaves the owning store as plain copied words
    /// (the map keeps every count it owns) in the caller's buffer, so a
    /// reused scratch holds a many-row walk at O(1) allocations; a key
    /// foreign to the map resolves by borrowing the probe's heap, so the
    /// walk copies no key text per row.
    pub(crate) fn map_row_scratch(
        &mut self,
        map: RefId,
        key: Value,
        scratch: &mut Vec<Value>,
    ) -> Result<bool, Trap> {
        let res = self.resolve_key_shared(map.fiber(), key)?;
        match self.store_shared(map)?.map_at(map.id(), key, &res)? {
            Some(span) => {
                scratch.clear();
                scratch.extend_from_slice(span);
                Ok(true)
            },
            None => Ok(false),
        }
    }

    /// Stores one key and its value span into map object `map`, routed,
    /// with the queues drained. A fresh row refuses a key word foreign
    /// to the map's heap at the store seam ([`Trap::ForeignMapKey`]);
    /// a content-equal foreign probe overwriting an existing row stays
    /// legal.
    pub(crate) fn set_map(&mut self, map: RefId, key: Value, value: &[Value]) -> Result<(), Trap> {
        let mut probe_text = String::new();
        let res = self.resolve_key_staged(map.fiber(), key, &mut probe_text)?;
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

    /// Removes the row stored under `key` in map object `map`, routed,
    /// with the queues drained: the removed key and value span release
    /// their counts on the words' own heaps, a missing key a no-op.
    pub(crate) fn remove_map(&mut self, map: RefId, key: Value) -> Result<(), Trap> {
        let mut probe_text = String::new();
        let res = self.resolve_key_staged(map.fiber(), key, &mut probe_text)?;
        self.store(map)?.remove_map(map.id(), key, &res)?;
        self.settle(map.fiber())
    }

    /// Whether a row is stored under `key` in map object `map`, routed
    /// as a shared read.
    pub(crate) fn contains_map(&mut self, map: RefId, key: Value) -> Result<bool, Trap> {
        let res = self.resolve_key_shared(map.fiber(), key)?;
        self.store_shared(map)?.contains_map(map.id(), key, &res)
    }

    /// The entry words of map object `map`, in entry order, routed and
    /// copied out: the map keeps every count it owns, so the caller that
    /// adopts the words acquires its own.
    pub(crate) fn map_entries(&mut self, map: RefId) -> Result<(Vec<Value>, usize), Trap> {
        self.store_shared(map)?.map_entries(map.id())
    }

    /// Replaces the element span at `index` in list object `list`,
    /// routed: the store mutation lands on the list's store and each
    /// incoming `Ref` word's acquired count on the element's own heap.
    pub(crate) fn set_list(
        &mut self,
        list: RefId,
        index: i64,
        words: &[Value],
    ) -> Result<(), Trap> {
        self.store(list)?.set_list(list.id(), index, words)?;
        self.settle(list.fiber())
    }

    /// Removes the element span at `index` in list object `list`,
    /// routed: the removed words' counts release on their own heaps.
    pub(crate) fn remove_list(&mut self, list: RefId, index: i64) -> Result<(), Trap> {
        self.store(list)?.remove_list(list.id(), index)?;
        self.settle(list.fiber())
    }

    /// Inserts one element span before `index` in list object `list`,
    /// routed.
    pub(crate) fn insert_list(
        &mut self,
        list: RefId,
        index: i64,
        words: &[Value],
    ) -> Result<(), Trap> {
        self.store(list)?.insert_list(list.id(), index, words)?;
        self.settle(list.fiber())
    }

    /// Empties list object `list`, routed: every element word's count
    /// releases on its own heap.
    pub(crate) fn clear_list(&mut self, list: RefId) -> Result<(), Trap> {
        self.store(list)?.clear_list(list.id())?;
        self.settle(list.fiber())
    }

    /// The content-compare face the object behind `word` presents, routed:
    /// the five kinds the structural `==` content compare reads, or `None`
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
            Some(Object::Cell(_)) => Some(ContentFace::Cell),
            _ => None,
        })
    }

    /// The structural content compare over the objects behind two
    /// reference words: strings compare text, bytes payloads, lists
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
        self.pairs_eq(vec![(Value::Ref(x), Value::Ref(y))])
    }

    /// Whether any element span of list object `list` equals the probe
    /// span, routed as a shared read: every pair compared through the
    /// same walk [`Heaps::content_eq`] serves, the probe staged variadic
    /// at the element's whole stride. Both operands stay borrowed, so
    /// the read owns no count and queues nothing.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object,
    /// [`Trap::ElementStride`] when the probe width misses the stride.
    pub(crate) fn list_contains(&mut self, list: RefId, probe: &[Value]) -> Result<bool, Trap> {
        let (elems, stride) = self.list_at(list)?;
        // the empty list carries no stride (stride 0 iff empty), so it
        // answers false whatever the probe's width
        if elems.is_empty() {
            return Ok(false);
        }
        if probe.len() != stride {
            return Err(Trap::ElementStride {
                obj: list.id(),
                stride,
                given: probe.len(),
            });
        }
        // the element words copy out of the owning store before the walk
        // routes: a borrowed slice dies at the next routed call
        let elems: Vec<Value> = elems.to_vec();
        let step = stride.max(1);
        for at in (0..elems.len()).step_by(step) {
            let pairs: Vec<(Value, Value)> = probe
                .iter()
                .copied()
                .zip(elems[at..at + stride].iter().copied())
                .collect();
            if self.pairs_eq(pairs)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The pairwise walk one content compare runs: the pending pair
    /// stack the entry site seeds (two reference words for
    /// [`Heaps::content_eq`], one probe-element pair per element for
    /// [`Heaps::list_contains`]).
    fn pairs_eq(&mut self, mut pending: Vec<(Value, Value)>) -> Result<bool, Trap> {
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
                        // no per-row vector, and a key foreign to either
                        // map resolves by borrowing (no key text copy per
                        // row); the key is its own map's row, so only the
                        // right side can miss — a missing row answers
                        // unequal at once
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
                (Some(ContentFace::Cell), Some(ContentFace::Cell)) => {
                    // the held spans compare word for word through the
                    // same walk; a held span cannot name its own
                    // container (the held value exists before the box),
                    // so no cycle memo rides. Both spans copy out of
                    // their owning stores before the walk routes on.
                    let front = self.store(a)?.cell_words(a.id())?;
                    let back = self.store(b)?.cell_words(b.id())?;
                    if front.len() != back.len() {
                        return Ok(false);
                    }
                    pending.extend(front.into_iter().zip(back));
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
    /// The owning container's held span.
    Cell,
}

/// A map key probe's resolution for [`Objects::map_at`] and
/// [`Objects::set_map`]: a local or scalar word resolves through the
/// map's own store; a foreign word arrives pre-resolved — its string
/// content, or the knowledge that it names no string (identity compare
/// then, like every non-string reference key). The content borrows: a
/// read funnel resolves it straight off the probe's own heap (no owned
/// copy per probe), while the mutating funnel stages its copy in the
/// caller's buffer.
pub enum ResolvedKey<'a> {
    /// The word is local to the map's store or carries no reference:
    /// the store resolves it itself.
    Word,
    /// A foreign string probe: its content, plus that object's cached
    /// content hash, so the map's store compares without re-walking the
    /// probe.
    Text { text: &'a str, hash: u32 },
    /// A foreign reference probe naming no string.
    Opaque,
}
