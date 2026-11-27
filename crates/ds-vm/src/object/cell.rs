//! The owning container's store methods: `box[t]`'s object kind over the
//! shared block layout.

use super::*;

impl Objects {
    /// Allocates a fresh owning container over `words`, the counts
    /// adopted as-is — the caller acquired one count per `Ref` word
    /// (a moved-in word keeps its count, the object owns it now).
    pub(super) fn alloc_cell(&mut self, words: Vec<Value>) -> u32 {
        self.alloc(Object::Cell(CellData { words }))
    }

    /// The held word span of owning container `obj`, as a borrowed
    /// copy — the routed read acquires its own counts.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live owning
    /// container.
    pub fn cell_words(&mut self, obj: u32) -> Result<Vec<Value>, Trap> {
        let header = self.header_of(obj)?;
        match self.heap.payload::<Object>(header) {
            Object::Cell(data) => Ok(data.words.clone()),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Moves owning container `obj`'s whole held span out, leaving it
    /// empty: the counts ride with the words (the caller adopts them as
    /// its answer's), so the emptied container's death cascade releases
    /// nothing twice.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live owning
    /// container.
    pub fn cell_take_words(&mut self, obj: u32) -> Result<Vec<Value>, Trap> {
        let header = self.header_of(obj)?;
        match self.heap.payload_mut::<Object>(header) {
            Object::Cell(data) => Ok(std::mem::take(&mut data.words)),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Replaces owning container `obj`'s whole held span, in place: the
    /// incoming span stores under acquired counts (one per `Ref` word)
    /// and the held span's counts release after the swap — a bogus
    /// incoming reference leaves the container untouched.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live owning
    /// container.
    pub fn set_cell(&mut self, obj: u32, words: &[Value]) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::Cell(_)) {
            return Err(Trap::UnknownObject { obj });
        }
        for value in words {
            if let Value::Ref(word) = value {
                if word.fiber() == self.owner {
                    self.inc(word.id())?;
                } else {
                    self.pending.push_back(ForeignOp::Inc(*word));
                }
            }
        }
        let old = match self.heap.payload_mut::<Object>(header) {
            Object::Cell(data) => std::mem::replace(&mut data.words, words.to_vec()),
            _ => return Err(Trap::UnknownObject { obj }),
        };
        for value in old {
            if let Value::Ref(word) = value {
                self.release_stored(word);
            }
        }
        Ok(())
    }
}
