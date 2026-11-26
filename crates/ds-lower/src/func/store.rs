//! The assignment sugar's store synthesis (the 2026-11-25 ruling): one
//! place chain — a field path over a `var`-bound struct, a subscript over
//! the ambient collections, or a nesting of both — lowers over the one
//! mutation mechanism the surface already had. A field-terminated chain
//! writes the leaf's frame sub-span in place (the synthesized mut-window
//! field store over the root binding, the exclusivity the checker's
//! var-spelling gate proved — no new mutation channel); a subscript step
//! stages the collection's `set` native exactly like the member call's
//! own spelling, so `xs[i] = v` and `xs.set(i, v)` emit identical code;
//! a nesting reads the element back through the checked read, mutates
//! the read copy's sub-span, then stores the element back through the
//! same `set` — the read's and the store's borrow windows composing over
//! the one root, with the out-of-range store trapping (the place must
//! resolve; the checked read's extractable `t | none` miss stays the
//! probe's own answer).

use super::*;

/// One pending element store-back: the collection the step addressed,
/// with its receiver and probe words and the element temp the chain's
/// remaining steps mutated.
struct PendingStore {
    /// The collection the receiver names.
    native: StoreIndex,
    /// The collection's receiver place (the list or map `Ref` word).
    recv: TypedPlace,
    /// The probe place (the list's index word, the map's key word).
    probe: TypedPlace,
    /// The element temp the remaining steps mutated.
    elem: TypedPlace,
}

impl FnLower<'_, '_, '_> {
    /// One assignment whose checked place chain the checker recorded at
    /// its own span: the root binder's place walks the recorded steps,
    /// the leaf store or the `set` staging synthesizing from the
    /// per-step solved types. The assignment answers unit.
    pub(super) fn assign_place_store(
        &mut self,
        e: &Expr,
        store: &PlaceStore,
    ) -> Result<Place, LowerError> {
        let Expr::Assign {
            left, right, span, ..
        } = e
        else {
            return Err(mismatch("a place assignment"));
        };
        let root = self
            .binders
            .get(&store.binder)
            .cloned()
            .ok_or_else(|| self.missing_binder(store.binder))?;
        let mut place = TypedPlace {
            place: root,
            ty: store.root_ty,
        };
        let mut pending: Vec<PendingStore> = Vec::new();
        for (index, step) in store.steps.iter().enumerate() {
            let last = index + 1 == store.steps.len();
            match step {
                PlaceStep::Field { name, ty } => {
                    place = self.place_field(&place, name, *ty, *span)?;
                },
                PlaceStep::Index { native, elem } => {
                    let probe = self.value(Self::place_probe(left, store.steps.len(), index)?)?;
                    let probe = self.probe_word(*native, &probe, !last, *span)?;
                    if last {
                        // the whole element is the assignment's target:
                        // the value evaluates, then the `set` member call
                        // stages — the exact code `xs.set(i, v)` takes
                        let value = self.value(right)?;
                        self.guard_list_element(&value, *span, "a subscript store element")?;
                        if self.deinit_claim(value.ty, *span)?.is_some() {
                            return Err(LowerError::unsupported(
                                *span,
                                "a deinit-carrying subscript element",
                                "the native store borrows staged words with no drop position of \
                                 their own; the value's deinit runs only at its binding's death",
                            ));
                        }
                        self.stage_index_store(*native, &place, &probe, &value)?;
                    } else {
                        // a nesting reads the element back through the
                        // checked read and continues the walk over the
                        // read copy; the store-back rides below
                        let read = self.stage_index_read(*native, &place, &probe, *elem)?;
                        pending.push(PendingStore {
                            native: *native,
                            recv: place,
                            probe,
                            elem: read,
                        });
                        place = pending.last().expect("just pushed").elem.clone();
                    }
                },
            }
        }
        // a field-terminated chain stores the value over the leaf's
        // frame sub-span: the old words' counts drop ahead of the store,
        // the value's words widen in — the whole-binding assignment's
        // own store tail, over the sub-span
        if matches!(store.steps.last(), Some(PlaceStep::Field { .. })) {
            let leaf_ty = place.ty;
            let assigned = self.value(right)?;
            for word in self.count_words(place.place.base, leaf_ty)? {
                self.rc.store_over(&mut self.code, &[word]);
            }
            self.refuse_closure_widen(&assigned, leaf_ty, *span)?;
            self.guard_deinit_union_member(assigned.ty, leaf_ty, *span)?;
            self.widen_into(&place.place, leaf_ty, &assigned, *span)?;
        }
        // the pending store-backs unwind innermost-first: the mutated
        // element returns to its own collection before the enclosing
        // element returns to its
        while let Some(store) = pending.pop() {
            self.stage_index_store(store.native, &store.recv, &store.probe, &store.elem)?;
        }
        Ok(self.unit_value(*span)?.place)
    }

    /// One receiver's field sub-place: the struct's (or the refined
    /// single member's) field word offset added to the receiver's base,
    /// the step's solved type carrying the shape. A word-shaped field
    /// takes the word's own slot kind; every wider field spans its type
    /// so a nested step addresses its words.
    fn place_field(
        &mut self,
        recv: &TypedPlace,
        name: &str,
        ty: Ty,
        span: Span,
    ) -> Result<TypedPlace, LowerError> {
        let shape = self.shapes.shape(recv.ty)?;
        let field = match &*shape {
            Shape::Union(_) => {
                let atom = self.only_atom(recv.ty, span)?;
                let fields = shape
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member field"))?;
                fields
                    .iter()
                    .find(|f| f.name.as_deref() == Some(name))
                    .ok_or_else(|| mismatch("a union member field"))?
            },
            Shape::Struct(s) => s
                .fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name))
                .ok_or_else(|| mismatch("a struct field"))?,
            Shape::Word(_) | Shape::Tuple(_) => return Err(mismatch("a field access")),
        };
        let word_shape = if matches!(&*self.shapes.shape(ty)?, Shape::Word(_)) {
            PlaceShape::Word(field.slot())
        } else {
            PlaceShape::Span(ty)
        };
        Ok(TypedPlace {
            place: Place {
                base: recv.place.base + field.word,
                shape: word_shape,
            },
            ty,
        })
    }

    /// The probe expression of one index step: the assignment's own
    /// left side re-walked from its outermost node (the last recorded
    /// step) down to the root, one step per node.
    fn place_probe(left: &Expr, steps: usize, index: usize) -> Result<&Expr, LowerError> {
        let mut recv = left;
        let mut pos = steps - 1;
        loop {
            match recv {
                Expr::Field { recv: inner, .. } => {
                    pos -= 1;
                    recv = inner;
                },
                Expr::Index {
                    recv: inner,
                    index: probe,
                    ..
                } => {
                    if pos == index {
                        return Ok(probe);
                    }
                    pos -= 1;
                    recv = inner;
                },
                _ => return Err(mismatch("a place assignment's probe")),
            }
        }
    }

    /// The probe's staging word: the source's own word when one staging
    /// reads it (the member call's discipline — a dying source's dec
    /// defers to the call's flush), a frame-owned copy when a second
    /// staging (a nesting's read and store-back) reads it too — the copy
    /// owns its count across the whole chain while the source's own dec
    /// lands wherever its binding dies.
    fn probe_word(
        &mut self,
        native: StoreIndex,
        probe: &TypedPlace,
        own: bool,
        span: Span,
    ) -> Result<TypedPlace, LowerError> {
        let (what, reason) = match native {
            StoreIndex::List => (
                "a subscript store index",
                "the index stages as one int word",
            ),
            StoreIndex::Map => (
                "a subscript store key",
                "map keys lower as one machine word (the probe vocabulary hashes whole words)",
            ),
        };
        let addrs = self.word_addrs(&probe.place, probe.ty)?;
        let [addr] = addrs.as_slice() else {
            return Err(LowerError::unsupported(span, what, reason));
        };
        if !own {
            return Ok(probe.clone());
        }
        let temp = self.temp_word(addr.slot_ty)?;
        self.emit(Op::Mov {
            dst: Slot::body(temp),
            src: Slot::body(addr.slot),
        });
        if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
            self.rc.copied(&mut self.code, temp, addr.slot);
        }
        Ok(TypedPlace {
            place: Place {
                base: temp,
                shape: PlaceShape::Word(addr.slot_ty),
            },
            ty: probe.ty,
        })
    }

    /// One checked element read (the nesting's front half): the generic
    /// `get` native's CALLG over the receiver and probe words, the ok
    /// payload's words copied into a fresh element temp under their own
    /// counts. The tag word is never tested — the store-back below
    /// traps on the same out-of-range probe, so a miss answer's payload
    /// never becomes observable.
    fn stage_index_read(
        &mut self,
        native: StoreIndex,
        place: &TypedPlace,
        probe: &TypedPlace,
        elem: Ty,
    ) -> Result<TypedPlace, LowerError> {
        let (get, instance) = match native {
            StoreIndex::List => (
                crate::native::lookup_generic("list_get")
                    .expect("the list surface is covered by the generic table"),
                BuiltinInstance::List,
            ),
            StoreIndex::Map => (
                crate::native::lookup_generic("map_get")
                    .expect("the map surface is covered by the generic table"),
                BuiltinInstance::Map,
            ),
        };
        let recv_addr = single(&self.word_addrs(&place.place, place.ty)?, no_span())?;
        let probe_addr = single(&self.word_addrs(&probe.place, probe.ty)?, no_span())?;
        let elem_words = self.shapes.words(elem)?;
        let mut result_words = vec![SlotType::Tag];
        result_words.extend_from_slice(&elem_words);
        self.guard_staged_count(result_words.len())?;
        self.overlay_site(
            &[SlotType::Ref, probe_addr.slot_ty, SlotType::I64],
            &result_words,
        );
        self.rc.native_open();
        self.rc.native_reads([recv_addr.slot, probe_addr.slot]);
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(recv_addr.slot),
        });
        self.emit(Op::Mov {
            dst: Slot::overlay(1),
            src: Slot::body(probe_addr.slot),
        });
        let kidx = self.pool(Constant::I64(instance as i64));
        self.emit(Op::Kimm {
            dst: Slot::overlay(2),
            kidx,
        });
        let callee = self.pool(self.funcref_const(Callee::Generic(get)));
        self.emit(Op::Callg { callee, nargs: 2 });
        self.rc.native_done(&mut self.code);
        // the payload words move into the element temp, each `Ref` word
        // under the count the read handed out (the collection keeps its
        // own, C4)
        let base = self.alloc(&elem_words)?;
        for (index, slot) in elem_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("span width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(index + 1).expect("result word fits u8")),
            });
            if *slot == SlotType::Ref || *slot == SlotType::Raw {
                self.rc.produced(word);
            }
        }
        Ok(TypedPlace {
            place: Place {
                base,
                shape: PlaceShape::Span(elem),
            },
            ty: elem,
        })
    }

    /// One collection element store: the `set` native's CALL staging —
    /// the receiver word, the probe word, then the element's whole
    /// inline word span (`list_push`'s variadic shape) — exactly the
    /// member spelling's own staging, the receiver keeping its count (no
    /// answer dup rides a statement-position assignment) and the
    /// element's words staging as transient borrows.
    fn stage_index_store(
        &mut self,
        native: StoreIndex,
        place: &TypedPlace,
        probe: &TypedPlace,
        elem: &TypedPlace,
    ) -> Result<(), LowerError> {
        let set = match native {
            StoreIndex::List => crate::native::lookup("list_set")
                .expect("the list surface is covered by the native table"),
            StoreIndex::Map => crate::native::lookup("map_set")
                .expect("the map surface is covered by the native table"),
        };
        let recv_addr = single(&self.word_addrs(&place.place, place.ty)?, no_span())?;
        let probe_addr = single(&self.word_addrs(&probe.place, probe.ty)?, no_span())?;
        let elem_addrs = self.word_addrs(&elem.place, elem.ty)?;
        let mut staged = vec![SlotType::Ref, probe_addr.slot_ty];
        let mut moves: Vec<(usize, u8, SlotType)> = vec![
            (0, recv_addr.slot, SlotType::Ref),
            (1, probe_addr.slot, probe_addr.slot_ty),
        ];
        for addr in &elem_addrs {
            moves.push((staged.len(), addr.slot, addr.slot_ty));
            staged.push(addr.slot_ty);
        }
        self.guard_staged_count(staged.len())?;
        self.overlay_site(&staged, &[SlotType::I64]);
        self.rc.native_open();
        self.rc.native_reads([recv_addr.slot, probe_addr.slot]);
        for (index, slot, slot_ty) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
            match (index, slot_ty) {
                // the receiver keeps its count: the window's flush never
                // decs it, and no answer dup rides the assignment
                (0, SlotType::Ref | SlotType::Raw) => {
                    self.rc.staged_native_keep(slot);
                },
                (_, SlotType::Ref | SlotType::Raw) => {
                    self.rc.staged_native(slot, false);
                },
                _ => {},
            }
        }
        let callee = self.pool(self.funcref_const(Callee::Builtin(set)));
        self.emit(Op::Call {
            callee,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        Ok(())
    }
}
