//! Arena payloads: generation-tagged slots and their handle words.

use super::*;

impl Objects {
    /// Allocates a fresh empty arena, owned by the caller: the birth
    /// count is the creating frame's, exactly a fresh list's.
    pub fn alloc_arena(&mut self) -> u32 {
        self.alloc(Object::Arena(ArenaData::default()))
    }

    /// Unpacks one handle word: the low half the slot index, the high
    /// half the generation the insert answered.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    pub(super) fn arena_unpack(handle: i64) -> (u32, u32) {
        ((handle & 0xffff_ffff) as u32, (handle >> 32) as u32)
    }

    /// The slot an insert takes: the free list's head (the removal
    /// already bumped its generation, so the next handle under it
    /// disagrees with every stale copy) or a fresh slot past the array's
    /// end (the backing buffer's amortized doubling).
    pub(super) fn arena_take_slot(&mut self, header: NonNull<RcHeader>) -> (u32, u32) {
        match self.heap.payload_mut::<Object>(header) {
            Object::Arena(data) => {
                if let Some(index) = data.free.pop() {
                    (index, data.slots[index as usize].generation)
                } else {
                    let index =
                        u32::try_from(data.slots.len()).expect("the slot index stays below 2^32");
                    data.slots.push(ArenaSlot {
                        generation: 0,
                        word: None,
                    });
                    (index, 0)
                }
            },
            _ => unreachable!("the kind check precedes the slot take"),
        }
    }

    /// Stores one element word into arena object `obj`, answering the
    /// packed `(index, generation)` handle: the store acquires one edge
    /// count for a `Ref` element exactly like a list element (a local
    /// word through the store's own inc, a foreign word by posting the
    /// inc to the pending queue), and the slot array's growth is the
    /// backing buffer's amortized doubling.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no arena.
    pub fn arena_insert(&mut self, obj: u32, word: Value) -> Result<i64, Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::Arena(_)) {
            return Err(Trap::UnknownObject { obj });
        }
        // the acquire happens before the store: a bogus element
        // reference leaves the arena untouched
        if let Value::Ref(word) = word {
            if word.fiber() == self.owner {
                self.inc(word.id())?;
            } else {
                self.pending.push_back(ForeignOp::Inc(word));
            }
        }
        let (index, generation) = self.arena_take_slot(header);
        match self.heap.payload_mut::<Object>(header) {
            Object::Arena(data) => {
                let slot = &mut data.slots[index as usize];
                debug_assert!(slot.word.is_none(), "a taken slot sits retired");
                slot.word = Some(word);
                data.live += 1;
            },
            _ => unreachable!("the kind check precedes the store"),
        }
        Ok((i64::from(generation) << 32) | i64::from(index))
    }

    /// The element word behind a live handle, shared: the slot's stored
    /// word without any count move — the caller (the routed read) owns
    /// the retain the answer needs, exactly a list's checked read-out.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no arena;
    /// [`Trap::DanglingArenaHandle`] when the handle names a retired
    /// slot, a slot the arena never held, or an old generation of a
    /// reused one.
    pub fn arena_word(&mut self, obj: u32, handle: i64) -> Result<Value, Trap> {
        let header = self.header_of(obj)?;
        let (index, generation) = Self::arena_unpack(handle);
        let slot = match self.heap.payload::<Object>(header) {
            Object::Arena(data) => data.slots.get(index as usize),
            _ => return Err(Trap::UnknownObject { obj }),
        };
        match slot {
            Some(ArenaSlot {
                generation: live,
                word: Some(word),
            }) if *live == generation => Ok(*word),
            _ => Err(Trap::DanglingArenaHandle { index, generation }),
        }
    }

    /// Removes the element behind a live handle: the stored word leaves
    /// the slot first, its edge count releasing at the store's own dec
    /// (a foreign word's count posting to the pending queue), the slot
    /// tombstones onto the free list with the generation bumping at the
    /// removal — so every handle copy taken before it goes stale at
    /// once, and the next reuse answers handles under the bumped one.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no arena;
    /// [`Trap::DanglingArenaHandle`] when the handle names no live
    /// element.
    pub fn arena_remove(&mut self, obj: u32, handle: i64) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        let (index, generation) = Self::arena_unpack(handle);
        let word = match self.heap.payload_mut::<Object>(header) {
            Object::Arena(data) => {
                let Some(ArenaSlot {
                    generation: live,
                    word,
                }) = data.slots.get_mut(index as usize)
                else {
                    return Err(Trap::DanglingArenaHandle { index, generation });
                };
                if *live != generation {
                    return Err(Trap::DanglingArenaHandle { index, generation });
                }
                *live = live.wrapping_add(1);
                data.live -= 1;
                data.free.push(index);
                word.take()
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        if let Some(Value::Ref(word)) = word {
            if word.fiber() == self.owner {
                self.dec(word.id())?;
            } else {
                self.pending.push_back(ForeignOp::Dec(word));
            }
        }
        Ok(())
    }

    /// The arena's live element count.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no arena.
    pub fn arena_len(&self, obj: u32) -> Result<usize, Trap> {
        let header = self.header_of(obj)?;
        match self.heap.payload::<Object>(header) {
            Object::Arena(data) => Ok(data.live),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }
}
