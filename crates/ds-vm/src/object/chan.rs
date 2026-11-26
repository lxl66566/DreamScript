//! Channel payloads: the bounded FIFO and the parked-fiber queues.

use super::*;

impl Objects {
    /// Allocates a fresh bounded channel of capacity `cap` (zero the
    /// rendezvous bound, the constructor native refusing a negative
    /// bound ahead of the allocation), owned by the caller: the birth
    /// count is the creating frame's, exactly a fresh list's.
    pub fn alloc_chan(&mut self, cap: usize) -> u32 {
        self.alloc(Object::Chan(ChanData {
            cap,
            elems: VecDeque::new(),
            stride: 0,
            senders: VecDeque::new(),
            receivers: VecDeque::new(),
        }))
    }

    /// Deposits one payload element — its whole inline word span — onto
    /// channel object `obj`'s buffer tail, the `list_push` staging
    /// shape at the channel's hand-off: the buffer's store takes the
    /// words and acquires one edge count per `Ref` word exactly like
    /// list elements — a local word through the store's own inc, a
    /// foreign word by posting the inc to the pending queue for the
    /// router to drain (the foreign-post funnel, the channel's
    /// cross-heap seam). The first deposit fixes the buffer's element
    /// stride; a later deposit must repeat it exactly, so a
    /// hand-assembled mismatch traps instead of misaligning the FIFO.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no channel;
    /// [`Trap::ElementStride`] when the staged span's width disagrees
    /// with the stride the first deposit fixed.
    pub fn chan_deposit(&mut self, obj: u32, words: &[Value]) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::Chan(_)) {
            return Err(Trap::UnknownObject { obj });
        }
        if let Object::Chan(data) = self.heap.payload::<Object>(header)
            && data.stride != 0
            && data.stride != words.len()
        {
            return Err(Trap::ElementStride {
                obj,
                stride: data.stride,
                given: words.len(),
            });
        }
        // the acquire happens before the store: a bogus element
        // reference leaves the buffer untouched (a foreign element word
        // queues, so the owning heap takes the buffer's count)
        for value in words {
            if let Value::Ref(id) = value {
                if id.fiber() == self.owner {
                    self.inc(id.id())?;
                } else {
                    self.pending.push_back(ForeignOp::Inc(*id));
                }
            }
        }
        match self.heap.payload_mut::<Object>(header) {
            Object::Chan(data) => {
                if data.stride == 0 {
                    data.stride = words.len();
                }
                data.elems.extend(words);
                Ok(())
            },
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Pops one payload element off channel object `obj`'s buffer head
    /// — its whole word span. The popped words keep the counts they
    /// hold — the buffer's edge counts become the receiving frame's
    /// backing — so no count moves here; the receiver's ledger takes
    /// one claim per `Ref` word through the routed face.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no channel.
    pub fn chan_pop(&mut self, obj: u32) -> Result<Option<ChanPop>, Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::Chan(_)) {
            return Err(Trap::UnknownObject { obj });
        }
        match self.heap.payload_mut::<Object>(header) {
            // stride 0 names the never-deposited channel, whose buffer
            // is empty; the one-word element rides the allocation-free
            // arm, a wider one its span
            Object::Chan(data) => Ok(match data.stride {
                0 | 1 => data.elems.pop_front().map(ChanPop::One),
                stride => (data.elems.len() >= stride)
                    .then(|| ChanPop::Span(data.elems.drain(..stride).collect())),
            }),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The channel payload behind `obj`, shared, when the object is a
    /// channel: `None` hands the caller a structured refusal for a
    /// non-channel object, `Err` a freed id.
    pub(super) fn payload_chan(&self, obj: u32) -> Result<Option<&ChanData>, Trap> {
        let header = self.header_of(obj)?;
        match self.heap.payload::<Object>(header) {
            Object::Chan(data) => Ok(Some(data)),
            _ => Ok(None),
        }
    }

    /// The channel payload behind `obj`, exclusive.
    pub(super) fn payload_chan_mut(&mut self, obj: u32) -> Result<Option<&mut ChanData>, Trap> {
        let header = self.header_of(obj)?;
        match self.heap.payload_mut::<Object>(header) {
            Object::Chan(data) => Ok(Some(data)),
            _ => Ok(None),
        }
    }
}
