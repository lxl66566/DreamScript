//! List payloads: element spans at a per-list stride.

use super::*;

impl Objects {
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

    /// Replaces the element span at `index` with `words`, in place: the
    /// store acquires one count per incoming `Ref` word before the write
    /// (a bogus element reference leaves the list untouched), then the
    /// replaced span releases one count per `Ref` word. An out-of-range
    /// index has no extractable miss channel (the checked read answers
    /// `t | none` because the caller can extract; a store must resolve
    /// its place or halt), so it traps.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list or the
    /// staged span is empty; [`Trap::ElementStride`] when the staged
    /// width disagrees with the list's stride; [`Trap::ListIndex`] when
    /// `index` names no live element.
    pub fn set_list(&mut self, obj: u32, index: i64, words: &[Value]) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::List(_)) || words.is_empty() {
            return Err(Trap::UnknownObject { obj });
        }
        let (stride, len, at) = match self.payload_of(obj) {
            Some(Object::List(data)) if !data.elems.is_empty() => {
                let len = data.elems.len() / data.stride;
                (
                    data.stride,
                    len,
                    usize::try_from(index).ok().filter(|&at| at < len),
                )
            },
            _ => return Err(Trap::ListIndex { obj, index, len: 0 }),
        };
        let Some(at) = at else {
            return Err(Trap::ListIndex { obj, index, len });
        };
        if stride != words.len() {
            return Err(Trap::ElementStride {
                obj,
                stride,
                given: words.len(),
            });
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
            Object::List(data) => {
                let at = at * stride;
                let old = data.elems[at..at + stride].to_vec();
                data.elems[at..at + stride].copy_from_slice(words);
                old
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        for value in old {
            if let Value::Ref(word) = value {
                self.release_stored(word);
            }
        }
        Ok(())
    }

    /// Removes the element span at `index`, in place: the removed span
    /// releases one count per `Ref` word, and a list emptied by removals
    /// returns to the stride-0 polymorphism an empty list carries (the
    /// stride 0 iff empty invariant). An out-of-range index traps like
    /// [`Objects::set_list`]'s — the removal's miss has no extractable
    /// answer either.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list;
    /// [`Trap::ListIndex`] when `index` names no live element.
    pub fn remove_list(&mut self, obj: u32, index: i64) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::List(_)) {
            return Err(Trap::UnknownObject { obj });
        }
        let (stride, len, at) = match self.payload_of(obj) {
            Some(Object::List(data)) if !data.elems.is_empty() => {
                let len = data.elems.len() / data.stride;
                (
                    data.stride,
                    len,
                    usize::try_from(index).ok().filter(|&at| at < len),
                )
            },
            _ => return Err(Trap::ListIndex { obj, index, len: 0 }),
        };
        let Some(at) = at else {
            return Err(Trap::ListIndex { obj, index, len });
        };
        let removed = match self.heap.payload_mut::<Object>(header) {
            Object::List(data) => {
                let start = at * stride;
                let removed = data.elems.drain(start..start + stride).collect::<Vec<_>>();
                if data.elems.is_empty() {
                    data.stride = 0;
                }
                removed
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        for value in removed {
            if let Value::Ref(word) = value {
                self.release_stored(word);
            }
        }
        Ok(())
    }

    /// Inserts `words` as one fresh element span before `index`, in
    /// place: `index == len` appends (the `push` shape), an empty list
    /// adopts the staged width as its stride, and any other index in
    /// `0..=len` splices. The acquire happens before the store, exactly
    /// like [`Objects::push_list`]. An out-of-range index traps like
    /// [`Objects::set_list`]'s.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list or the
    /// staged span is empty; [`Trap::ElementStride`] when the staged
    /// width disagrees with a non-empty list's stride;
    /// [`Trap::ListIndex`] when `index` sits outside `0..=len`.
    pub fn insert_list(&mut self, obj: u32, index: i64, words: &[Value]) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::List(_)) || words.is_empty() {
            return Err(Trap::UnknownObject { obj });
        }
        let (stride, len, at) = match self.payload_of(obj) {
            Some(Object::List(data)) if data.elems.is_empty() => (0usize, 0usize, Some(0)),
            Some(Object::List(data)) => {
                let len = data.elems.len() / data.stride;
                (
                    data.stride,
                    len,
                    usize::try_from(index).ok().filter(|&at| at <= len),
                )
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        let Some(at) = at else {
            return Err(Trap::ListIndex { obj, index, len });
        };
        if stride != 0 && stride != words.len() {
            return Err(Trap::ElementStride {
                obj,
                stride,
                given: words.len(),
            });
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
        match self.heap.payload_mut::<Object>(header) {
            Object::List(data) => {
                if data.elems.is_empty() {
                    data.stride = words.len();
                }
                let start = at * data.stride;
                data.elems.splice(start..start, words.iter().copied());
            },
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// Empties the list, in place: every element word releases one
    /// count per `Ref` word and the list returns to the stride-0
    /// polymorphism an empty list carries.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list.
    pub fn clear_list(&mut self, obj: u32) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        let removed = match self.heap.payload_mut::<Object>(header) {
            Object::List(data) => {
                let elems = std::mem::take(&mut data.elems);
                data.stride = 0;
                elems
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        for value in removed {
            if let Value::Ref(word) = value {
                self.release_stored(word);
            }
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
}
