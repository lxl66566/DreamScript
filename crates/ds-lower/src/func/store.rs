use super::*;

/// One element store-back: the collection the step addressed, with its
/// receiver and probe words and the element temp the chain's remaining
/// steps mutated (a compound leaf's read copy holding the folded result).
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
    /// per-step solved types. A compound operator folds the right side
    /// into the leaf's read copy and stores the result back. The
    /// assignment answers unit.
    pub(super) fn assign_place_store(
        &mut self,
        e: &Expr,
        store: &PlaceStore,
    ) -> Result<Place, LowerError> {
        let Expr::Assign {
            op,
            left,
            right,
            span,
            ..
        } = e
        else {
            return Err(mismatch("a place assignment"));
        };
        let compound = *op != ds_parse::AssignOp::Plain;
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
        // A compound store whose leaf is a whole element: the element's
        // collection, receiver, and the probe its read and store share.
        let mut element: Option<PendingStore> = None;
        for (index, step) in store.steps.iter().enumerate() {
            let last = index + 1 == store.steps.len();
            match step {
                PlaceStep::Field { name, ty } => {
                    place = self.place_field(&place, name, *ty, *span)?;
                },
                PlaceStep::Index { native, elem } => {
                    let probe = self.value(Self::place_probe(left, store.steps.len(), index)?)?;
                    // two stagings read the probe when a read precedes the
                    // store (a nesting, a compound store): one owned copy
                    // serves both
                    let probe = self.probe_word(*native, &probe, !last || compound, *span)?;
                    if last && !compound {
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
                        self.release_place(&probe)?;
                        self.release_place(&value)?;
                    } else {
                        // the element reads back through the checked read
                        // and the walk continues over the read copy; the
                        // store-back rides below
                        let elem = self.stage_index_read(
                            *native,
                            &place,
                            &probe,
                            *elem,
                            "an assignment place",
                        )?;
                        let step = PendingStore {
                            native: *native,
                            recv: place,
                            probe,
                            elem: elem.clone(),
                        };
                        place = elem;
                        if last {
                            element = Some(step);
                        } else {
                            pending.push(step);
                        }
                    }
                },
            }
        }
        if let Some(step) = element {
            // `xs[i] op= e`: the read copy is the old value, the result
            // is written back through the read's own probe and receiver
            let result = self.compound_value(*op, &place, right, left.span())?;
            self.stage_index_store(step.native, &step.recv, &step.probe, &result)?;
            self.release_place(&step.elem)?;
            self.release_place(&result)?;
            self.release_place(&step.probe)?;
        } else if matches!(store.steps.last(), Some(PlaceStep::Field { .. })) {
            // a field-terminated chain stores over the leaf's frame
            // sub-span: the old words' counts drop ahead of the store,
            // the value's words widen in — the whole-binding assignment's
            // own store tail, over the sub-span. A compound form computes
            // into the leaf word itself when the leaf is a scalar, so the
            // store tail degenerates to the in-place no-op there
            let leaf_ty = place.ty;
            let assigned = if compound {
                let result = self.compound_value(*op, &place, right, left.span())?;
                for word in self.count_words(place.place.base, leaf_ty)? {
                    self.rc.store_over(&mut self.code, &[word]);
                }
                result
            } else {
                let value = self.value(right)?;
                // the replaced leaf's counts die before the store threads:
                // the value's words widen in over the released sub-span
                // (the whole-binding assignment's own store tail)
                for word in self.count_words(place.place.base, leaf_ty)? {
                    self.rc.store_over(&mut self.code, &[word]);
                }
                value
            };
            self.refuse_closure_widen(&assigned, leaf_ty, *span)?;
            self.guard_deinit_union_member(assigned.ty, leaf_ty, *span)?;
            self.widen_into(&place.place, leaf_ty, &assigned, *span)?;
        }
        // the pending store-backs unwind innermost-first: the mutated
        // element returns to its own collection before the enclosing
        // element returns to its, each read copy and probe copy dying at
        // its own store
        while let Some(step) = pending.pop() {
            self.stage_index_store(step.native, &step.recv, &step.probe, &step.elem)?;
            self.release_place(&step.elem)?;
            self.release_place(&step.probe)?;
        }
        Ok(self.unit_value(*span)?.place)
    }

    /// Releases a chain operand its store was the last read of (an
    /// element read copy, a frame-owned probe copy, a stored value): its
    /// anonymous reference words die here, once per evaluation of the
    /// assignment, so a store inside a loop leaks no count per
    /// iteration. A named binding's word keeps its own death.
    fn release_place(&mut self, value: &TypedPlace) -> Result<(), LowerError> {
        let words: Vec<u8> = self
            .shapes
            .words(value.ty)?
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot, SlotType::Ref | SlotType::Raw))
            .map(|(index, _)| value.place.base + u8::try_from(index).expect("span width fits u8"))
            .collect();
        self.rc.drop_unnamed(&mut self.code, &words);
        Ok(())
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
    /// staging (a nesting's or a compound store's read and store-back)
    /// reads it too — the copy owns its count across the whole chain
    /// while the source's own dec lands wherever its binding dies.
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

    /// One value-position subscript read: the checker routed the
    /// receiver over the ambient collections with the assignment place's
    /// own resolution, so the read stages the generic `get` native's
    /// CALLG over the receiver and probe words and answers the ok
    /// payload's whole span. The miss arm carries the assignment path's
    /// semantics at the read: a list's out-of-range index traps, a
    /// map's absent key halts through the bare trap primitive — the
    /// checked `get` members alone keep the `t | none` spelling. An
    /// anonymous receiver's or probe's count dies after the read, once
    /// per evaluation; a named binding keeps its own death.
    pub(super) fn index_read_value(
        &mut self,
        recv: &Expr,
        probe: &Expr,
        elem: Ty,
    ) -> Result<TypedPlace, LowerError> {
        let recv_tp = self.value(recv)?;
        let probe_tp = self.value(probe)?;
        let native = match self.shapes.builtin_instance(recv_tp.ty) {
            Some(BuiltinInstance::List) => StoreIndex::List,
            Some(BuiltinInstance::Map) => StoreIndex::Map,
            _ => {
                return Err(LowerError::unsupported(
                    recv.span(),
                    "a subscript read receiver",
                    "the read's collection has no instance row the lowerer covers",
                ));
            },
        };
        let recv_addr = single(&self.word_addrs(&recv_tp.place, recv_tp.ty)?, no_span())?;
        let probe_addr = single(&self.word_addrs(&probe_tp.place, probe_tp.ty)?, no_span())?;
        let temp = self.stage_index_read(native, &recv_tp, &probe_tp, elem, "a subscript read")?;
        self.rc
            .drop_unnamed(&mut self.code, &[recv_addr.slot, probe_addr.slot]);
        Ok(temp)
    }

    /// One checked element read (the nesting's front half): the generic
    /// `get` native's CALLG over the receiver and probe words, the ok
    /// payload's words copied into a fresh element temp under their own
    /// counts. The tag word decides: a miss answer's payload is the
    /// probe, so the miss arm halts before any word of it lands in the
    /// temp.
    fn stage_index_read(
        &mut self,
        native: StoreIndex,
        place: &TypedPlace,
        probe: &TypedPlace,
        elem: Ty,
        place_kind: &str,
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
        // the staged span ends in the generic's evidence words, one per
        // type parameter behind the instance — the area reserves them
        // all, or a dispatch's riders overrun a frame whose widest call
        // is this one
        let evidence = usize::from(get.evidence);
        let mut staged = vec![SlotType::Ref, probe_addr.slot_ty];
        staged.resize(2 + evidence, SlotType::I64);
        self.guard_staged_count(staged.len().max(result_words.len()))?;
        self.overlay_site(&staged, &result_words);
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
        for index in 0..evidence {
            self.emit(Op::Kimm {
                dst: Slot::overlay(u8::try_from(2 + index).expect("overlay word fits u8")),
                kidx,
            });
        }
        let callee = self.pool(self.funcref_const(Callee::Generic(get)));
        self.emit(Op::Callg { callee, nargs: 2 });
        self.rc.native_done(&mut self.code);
        let base = self.alloc(&elem_words)?;
        let temp = TypedPlace {
            place: Place {
                base,
                shape: PlaceShape::Span(elem),
            },
            ty: elem,
        };
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        let ok = self.label();
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok,
        });
        // the miss arm halts inside its own call, so nothing it owns
        // flows past the fork
        let fork = self.rc.save();
        self.place_miss(native, place, probe, &temp, &elem_words, place_kind)?;
        self.rc.restore(fork);
        self.bind_label(ok);
        // the payload words move into the element temp, each `Ref` word
        // under the count the read handed out (the collection keeps its
        // own)
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
        Ok(temp)
    }

    /// The miss arm of a checked collection read: the place must resolve
    /// or stop. A list's own store refuses the out-of-range probe — the
    /// element temp first takes kind-true dead words so the staging
    /// stays well-typed — answering the store's own trap; a map's store
    /// would insert instead, so an absent key halts through the bare
    /// trap primitive. `place_kind` names the miss in the halt line (an
    /// assignment place, a subscript read).
    fn place_miss(
        &mut self,
        native: StoreIndex,
        place: &TypedPlace,
        probe: &TypedPlace,
        temp: &TypedPlace,
        elem_words: &[SlotType],
        place_kind: &str,
    ) -> Result<(), LowerError> {
        match native {
            StoreIndex::List => {
                for (index, slot) in elem_words.iter().enumerate() {
                    let word = temp.place.base + u8::try_from(index).expect("span width fits u8");
                    self.zero_word(word, *slot);
                }
                self.stage_index_store(native, place, probe, temp)
            },
            StoreIndex::Map => {
                let trap =
                    crate::native::lookup("trap").ok_or_else(|| mismatch("the trap primitive"))?;
                let k = self.pool_str(&format!(
                    "the map holds no value under the key; {place_kind} must resolve, it carries \
                     no miss answer"
                ));
                let msg = self.temp_word(SlotType::Ref)?;
                self.kstr(msg, k);
                self.seam_call(trap, &[(msg, SlotType::Ref)], &[SlotType::I64])
            },
        }
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
