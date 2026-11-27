//! Id table lifecycle: allocation, refcounts, interning, and the staged-fn walk.

use super::*;

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
            fires: Vec::new(),
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

    /// Whether channel element deaths wait on this store for a fire door.
    #[must_use]
    pub(crate) fn fires_pending(&self) -> bool {
        !self.fires.is_empty()
    }

    /// Takes every queued channel fire: the drive doors' input. The
    /// cascade queued the deaths its own walk observed; the door fires
    /// each hook under a hosted drive and releases its held edge counts
    /// after the hook returns.
    pub(crate) fn take_fires(&mut self) -> Vec<ChanFire> {
        std::mem::take(&mut self.fires)
    }

    /// Whether any standing hook fact names `row`: the install sweep's
    /// judge, so a hot swap never reuses a version row a buffered
    /// element's fact still pins.
    #[must_use]
    pub(crate) fn names_version(&self, row: VersionIdx) -> bool {
        self.fires.iter().any(|fire| fire.hook.version == row)
            || self.table.iter().any(|slot| match slot {
                Slot::Live(header) => match self.heap.payload::<Object>(*header) {
                    Object::Chan(data) => {
                        data.hooks
                            .iter()
                            .any(|hook| hook.as_ref().is_some_and(|h| h.version == row))
                            || data.senders.iter().any(|sender| {
                                sender.hook.as_ref().is_some_and(|h| h.version == row)
                            })
                    },
                    _ => false,
                },
                Slot::Free(_) => false,
            })
    }

    /// Acquires one count for a stored `Ref` word, routing by the word's
    /// owner: a local word incs here, a foreign word queues onto the
    /// pending list (the router drains it onto the owning heap).
    pub(super) fn acquire_stored(&mut self, word: RefId) {
        if word.fiber() == self.owner {
            self.inc(word.id())
                .expect("a stored local reference is live");
        } else {
            self.pending.push_back(ForeignOp::Inc(word));
        }
    }

    /// Releases one count a stored `Ref` word carried, routing by the
    /// word's owner the same way [`Objects::acquire_stored`] acquires.
    pub(super) fn release_stored(&mut self, word: RefId) {
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
    pub(super) fn payload_of(&self, obj: u32) -> Option<&Object> {
        match self.table.get(obj as usize) {
            Some(Slot::Live(header)) => Some(self.heap.payload::<Object>(*header)),
            _ => None,
        }
    }

    /// The block header of the live object `obj` names.
    pub(super) fn header_of(&self, obj: u32) -> Result<NonNull<RcHeader>, Trap> {
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
    pub(super) fn alloc(&mut self, object: Object) -> u32 {
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

    /// `rc_inc`: the reference in the slot naming `obj` gains one
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

    /// `rc_dec`: one owner drops. A unique death frees the object —
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
    pub(super) fn free(&mut self, obj: u32, header: NonNull<RcHeader>) {
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
                Object::Cell(data) => std::mem::take(&mut data.words),
                Object::Map(data) => {
                    let vstride = data.vstride;
                    for key in data.keys.drain(..) {
                        spill.push(key);
                        spill.extend(data.values.drain(..vstride));
                    }
                    Vec::new()
                },
                Object::Chan(data) => {
                    // fact-carrying elements and parked-sender facts hold
                    // back from the cascade: their hooks fire under a
                    // hosted drive the cascade cannot host, each fire
                    // releasing its held edge counts after the hook
                    // returns (the fire-before-release door order). The
                    // fact-free elements dec exactly as before.
                    let stride = data.stride.max(1);
                    let hooks = std::mem::take(&mut data.hooks);
                    let elems: Vec<Value> = std::mem::take(&mut data.elems).into();
                    if hooks.is_empty() {
                        elems
                    } else {
                        let mut released: Vec<Value> = Vec::new();
                        for (index, words) in elems.chunks(stride).enumerate() {
                            match hooks.get(index).cloned().flatten() {
                                Some(hook) => {
                                    let held: Vec<Value> = words
                                        .iter()
                                        .filter(|word| matches!(word, Value::Ref(_)))
                                        .copied()
                                        .collect();
                                    self.fires.push(ChanFire {
                                        hook,
                                        words: words.to_vec(),
                                        held,
                                    });
                                },
                                None => released.extend_from_slice(words),
                            }
                        }
                        // the queue entries' facts die with the channel:
                        // their words ride no count (the parked frames
                        // kept their own), so only the fact queues
                        for sender in data.senders.drain(..) {
                            if let Some(hook) = sender.hook {
                                self.fires.push(ChanFire {
                                    hook,
                                    words: sender.payload,
                                    held: Vec::new(),
                                });
                            }
                        }
                        released
                    }
                },
                Object::Arena(data) => {
                    // the live element words leave the payload before it
                    // dies, one owned count per `Ref` word exactly like a
                    // list's; the retired slots hold nothing
                    for slot in &mut data.slots {
                        spill.extend(slot.words.take().into_iter().flatten());
                    }
                    Vec::new()
                },
                Object::Str(_) | Object::Bytes(_) => Vec::new(),
            };
            let words: &[Value] = if owned.is_empty() { &spill } else { &owned };
            // the block's death hook (the payload's own drop glue) runs,
            // then the memory pools for the next compatible allocation
            self.heap.rc_dispose::<Object>(header);
            // one owned count per Ref word drops here: lists per
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

    /// Allocates a fresh closure object (the header FuncRef plus the
    /// capture block), owned by the caller. The capture words are adopted
    /// as-is: a moved-in `Ref` word keeps its count — the object owns it
    /// now — while copyable words are plain copies.
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

    /// Replaces closure object `obj`'s capture word at `idx` with
    /// `word`, returning the word it displaced. The mut window a closure
    /// body's assignment to its own captured binding lowers onto: the
    /// caller owns both count moves around the swap (the incoming word's
    /// acquire, the displaced word's release).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no closure object;
    /// [`Trap::CaptureIndex`] when `idx` names no word of the block.
    pub fn replace_capture(&mut self, obj: u32, idx: usize, word: Value) -> Result<Value, Trap> {
        let header = self.header_of(obj)?;
        match self.heap.payload_mut::<Object>(header) {
            Object::Closure(closure) => closure
                .captures
                .get_mut(idx)
                .map(|slot| std::mem::replace(slot, word))
                .ok_or(Trap::CaptureIndex { obj, idx }),
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
                Object::Arena(arena) => {
                    for slot in &mut arena.slots {
                        for word in slot.words.iter_mut().flatten() {
                            if let Value::Fn(func) = word {
                                visit(StagedSite::ArenaElement { fiber: owner }, func)?;
                            }
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
                Object::Cell(data) => {
                    for word in &mut data.words {
                        if let Value::Fn(func) = word {
                            visit(StagedSite::CellWord { fiber: owner }, func)?;
                        }
                    }
                },
                // a channel's buffered payload words never stage a
                // callable in lowered code (the chan_send lowering
                // refuses a closure payload like a collection element)
                Object::Chan(_) | Object::Str(_) | Object::Bytes(_) => {},
            }
        }
        Ok(())
    }
}
