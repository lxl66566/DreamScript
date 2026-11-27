//! The emit primitives: slot allocation, temporary spans, the op buffer,
//! labels and their binding, and the constant pool. This family appends
//! every op, label, and pool entry; `finalize` patches those buffers.

use super::*;

impl FnLower<'_, '_, '_> {
    /// Allocates body slots, returning the base.
    pub(super) fn alloc(&mut self, words: &[SlotType]) -> Result<u8, LowerError> {
        let base = self.slots.len();
        self.slots.extend_from_slice(words);
        if self.slots.len() > usize::from(MAX_FRAME_SLOTS) {
            return Err(LowerError::FrameTooWide {
                name: self.name.clone(),
                need: self.slots.len(),
            });
        }
        // A zero-word span has no addressable slot, and a full frame's
        // base (256) is past u8; the nominal base clamps to slot 0, which
        // no op of the empty span ever references.
        Ok(u8::try_from(base).unwrap_or(0))
    }

    pub(super) fn temp_word(&mut self, slot: SlotType) -> Result<u8, LowerError> {
        self.alloc(&[slot])
    }

    /// A span temp holding one solved type.
    pub(super) fn temp_span(&mut self, ty: Ty) -> Result<Place, LowerError> {
        let words = self.shapes.words(ty)?;
        let base = self.alloc(&words)?;
        Ok(Place {
            base,
            shape: PlaceShape::Span(ty),
        })
    }

    /// The canonical place shape of one solved type at `base`: scalars are
    /// bare words, everything else a span (the same normalization params
    /// and call results get).
    pub(super) fn place_of_ty(&mut self, base: u8, ty: Ty) -> Result<Place, LowerError> {
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    pub(super) fn emit(&mut self, op: Op) {
        if self.last_span != Some(self.cur_span) {
            self.last_span = Some(self.cur_span);
            self.span_marks.push((self.code.len(), self.cur_span));
        }
        self.code.push(op);
    }

    /// One KSTR load into the body word `dst`: the word starts in the
    /// borrow classification and keeps it unless a count-moving site
    /// transits it before finalization (the placement pass owns the
    /// proof; the rewrite happens in `finalize`).
    pub(super) fn kstr(&mut self, dst: u8, kidx: u16) {
        self.emit(Op::Kstr {
            dst: Slot::body(dst),
            kidx,
        });
        self.rc.produced_kstr(dst);
    }

    pub(super) fn label(&mut self) -> usize {
        self.labels.push(usize::MAX);
        self.labels.len() - 1
    }

    pub(super) fn bind_label(&mut self, label: usize) {
        self.labels[label] = self.code.len();
    }

    /// Splices `ops` into the buffer at `at`: branch arms lower before
    /// the join's dying sets are known, so an arm's epilogue decs move in
    /// afterwards, right before the arm's jump to the join point. Labels
    /// bound past the splice shift with it; a label entering at the
    /// splice position runs the epilogue first — its path stays on the
    /// same arm.
    pub(super) fn splice(&mut self, ops: Vec<Op>, at: usize) {
        let len = ops.len();
        if len == 0 {
            return;
        }
        self.code.splice(at..at, ops);
        for pos in &mut self.labels {
            if *pos != usize::MAX && *pos > at {
                *pos += len;
            }
        }
        for span in &mut self.loop_spans {
            if span.head > at {
                span.head += len;
            }
            if span.back != usize::MAX && span.back > at {
                span.back += len;
            }
        }
        for (pos, _) in &mut self.span_marks {
            if *pos > at {
                *pos += len;
            }
        }
    }

    /// Records the innermost open loop's back edge at the next emission
    /// position. The spans persist through the walk (finalize pairs marks
    /// into the loops enclosing designated concatenations), so the
    /// innermost open one is the last span whose back edge is not yet
    /// recorded - inner loops close before their enclosing back edge
    /// runs.
    pub(super) fn loop_back_edge(&mut self) {
        let at = self.code.len();
        self.loop_spans
            .iter_mut()
            .rev()
            .find(|span| span.back == usize::MAX)
            .expect("a loop is open")
            .back = at;
    }

    /// One fall-through arm's epilogue: the words the join does not keep
    /// die inside their arm, spliced at the arm's code end (the other
    /// arm jumped past it).
    pub(super) fn arm_epilogue(&mut self, dying: &[(u8, Owned)]) {
        let mut epilogue = Vec::new();
        Placement::emit_drops(&mut epilogue, dying);
        let at = self.code.len();
        self.splice(epilogue, at);
    }

    /// Records one call site's overlay usage: the staged argument word
    /// types and the result words that land there.
    pub(super) fn overlay_site(&mut self, staged: &[SlotType], result: &[SlotType]) {
        self.overlay_len = self.overlay_len.max(staged.len()).max(result.len());
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        for (index, word) in staged.iter().enumerate().chain(result.iter().enumerate()) {
            self.overlay[index].insert(*word);
        }
    }

    /// Adds one pool constant (deduplicated by value), returning its index.
    pub(super) fn pool(&mut self, constant: Constant) -> u16 {
        if let Constant::Str(text) = constant {
            return self.pool_str(&text);
        }
        if let Some(&index) = self.pool_index.get(&constant) {
            return index;
        }
        let index = self.pool_len();
        self.pool_index.insert(constant.clone(), index);
        self.pool.push(constant);
        index
    }

    /// Adds one pool string constant (deduplicated by contents), returning
    /// its index.
    pub(super) fn pool_str(&mut self, text: &str) -> u16 {
        if let Some(&index) = self.pool_strings.get(text) {
            return index;
        }
        let index = self.pool_len();
        self.pool_strings.insert(text.to_owned(), index);
        self.pool.push(Constant::Str(text.to_owned()));
        index
    }

    pub(super) fn pool_len(&self) -> u16 {
        u16::try_from(self.pool.len()).expect("pool index fits u16")
    }
}

pub(super) fn resolve(slot: Slot, body_len: usize) -> u8 {
    if slot.overlay {
        u8::try_from(body_len + usize::from(slot.index)).expect("overlay slot fits u8")
    } else {
        slot.index
    }
}

pub(super) fn eq_op(slot: SlotType) -> Result<DataOp, LowerError> {
    Ok(match slot {
        SlotType::I64 => DataOp::EqI64,
        SlotType::F64 => DataOp::EqF64,
        SlotType::Bool => DataOp::EqBool,
        SlotType::Ref => DataOp::EqStr,
        _ => return Err(mismatch("a scalar equality")),
    })
}

pub(super) fn mismatch(what: &'static str) -> LowerError {
    LowerError::unsupported(
        no_span(),
        what,
        "the value's checked type and runtime representation disagree",
    )
}

pub(super) fn no_span() -> Span {
    Span::new(FileId::new(0), 0, 0)
}

/// The span of one pattern node (no `span()` method on the enum).
pub(super) fn pattern_span(pat: &Pattern) -> Span {
    match pat {
        Pattern::Wildcard(span)
        | Pattern::Literal { span, .. }
        | Pattern::Str { span, .. }
        | Pattern::RawStr { span, .. }
        | Pattern::Path { span, .. }
        | Pattern::Call { span, .. }
        | Pattern::Struct { span, .. }
        | Pattern::Tuple { span, .. }
        | Pattern::Binding { span, .. }
        | Pattern::Or { span, .. } => *span,
    }
}
