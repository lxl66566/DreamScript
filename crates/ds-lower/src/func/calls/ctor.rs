//! Call construction: the argument-staging guards, the direct literal and
//! computed answers, list and map construction, `ctor_call` and
//! `map_ctor_call`, the embedded comptime answers, the case construction,
//! and the written defaults a call materializes.

use super::*;

impl<'a> FnLower<'a, '_, '_> {
    /// The element guard both list construction paths share: a pushed
    /// element stages its whole inline word span — one scalar word (int,
    /// float, bool, tag, a heap Ref, a callable's scalar FuncRef) or a
    /// struct's flattened field words at the per-list element stride the
    /// first push fixes (the native channel's staged-count and read-out
    /// span seam). Tuple elements ride the same flat span, one word per
    /// element, gathered positionally. A union element stages its whole
    /// tag-plus-region span: the checked read hands the span back
    /// verbatim when the call's own result union carries the element's
    /// members (the enum-in-optional shape), so the element's own tag
    /// already names a result member. A deinit-carrying element refuses
    /// (the list's edge retention owns counts, never claims; per-span
    /// deinit claims own the seam), and a union member cannot carry one
    /// (the union's own shape refuses the declaration).
    /// A callable element must prove scalar provenance: a closure word (or
    /// a callable-defaulted parameter that may hold one at runtime) would
    /// leave its count invisible to the element staging and read back as a
    /// Ref word behind a Fn-classified payload slot.
    /// The channel-carried element type an element-slot argument stages
    /// into (a list's element, a map's value, a channel's payload):
    /// read off the receiver's own type, since the native callee
    /// carries no static parameter type. `None` for any other callee or
    /// receiver — a union of collections, a non-application.
    pub(in crate::func) fn element_slot_ty(
        &self,
        target: &Callee,
        receiver: Option<Ty>,
    ) -> Option<Ty> {
        let ty = receiver?;
        match target {
            Callee::Builtin(native) if native.name == "list_push" => self.shapes.list_element(ty),
            Callee::Builtin(native) if native.name == "list_contains" => {
                self.shapes.list_element(ty)
            },
            Callee::Builtin(native) if native.name == "list_set" => self.shapes.list_element(ty),
            Callee::Builtin(native) if native.name == "list_insert" => self.shapes.list_element(ty),
            Callee::Builtin(native) if native.name == "map_set" => self.shapes.map_value(ty),
            Callee::Builtin(native) if native.name == "chan_send" => self.shapes.chan_payload(ty),
            Callee::Builtin(native) if native.name == "chan_send_deadline" => {
                self.shapes.chan_payload(ty)
            },
            Callee::Builtin(native) if native.name == "arena_insert" => {
                self.shapes.arena_element(ty)
            },
            Callee::Builtin(native) if native.name == "box_set" => self.shapes.box_element(ty),
            _ => None,
        }
    }

    pub(in crate::func) fn stage_heap_element(
        &mut self,
        val: TypedPlace,
        span: Span,
        what: &'static str,
    ) -> Result<TypedPlace, LowerError> {
        match &*self.shapes.shape(val.ty)? {
            Shape::Struct(_) => {
                // the list's edge retention owns counts, never claims
                if self.deinit_hook(val.ty, span)?.is_some() {
                    return Err(LowerError::unsupported(
                        span,
                        what,
                        "a deinit-carrying element's hook has no claim inside the list's edge \
                         retention; per-span deinit claims own the seam",
                    ));
                }
                Ok(val)
            },
            // a fn-typed element stages through the heap classification:
            // the word is the closure Ref the channel's count traffic
            // covers, a proven scalar fn wrapping into its static closure
            // object and an unproven word staging as itself
            Shape::Word(SlotType::Fn) => {
                let addr = self.stage_heap_fn(&val, span)?;
                Ok(TypedPlace {
                    place: Place {
                        base: addr.slot,
                        shape: PlaceShape::Word(addr.slot_ty),
                    },
                    ty: val.ty,
                })
            },
            _ => Ok(val),
        }
    }

    /// The staged-span shape guards a call refuses before dispatch.
    /// `map_set` stages the map's `Ref`, the key's one word (the probe
    /// vocabulary hashes whole words), then the value's whole inline span
    /// — one word per map value word, the `list_push` staging shape — so
    /// a site with fewer than three staged words mis-stages the native's
    /// positional contract. A local callee's slide contract is
    /// its parameter span: the widening family re-spans a member or
    /// sub-union argument into the parameter's whole region before the
    /// staging moves, so a remaining width disagreement is a lowerer
    /// seam the VM's slide validation would trap on. A linked member
    /// dispatch carries the same contract over the checker's per-site
    /// solved record (the receiver word of a default-body dispatch
    /// included, since its one-word receiver guard ran ahead of here).
    pub(in crate::func) fn guard_staged_width(
        &mut self,
        target: Callee,
        staged: &[SlotType],
        span: Span,
        member_params: Option<&[Ty]>,
    ) -> Result<(), LowerError> {
        if let Callee::Builtin(native) = target
            && native.name == "map_set"
            && staged.len() < 3
        {
            return Err(LowerError::unsupported(
                span,
                "a map_set argument",
                "map_set stages the map, one key word, then the value's whole word span; a \
                 narrower staging mis-names the value channel",
            ));
        }
        if let Callee::Builtin(native) = target
            && (native.name == "list_set" || native.name == "list_insert")
            && staged.len() < 3
        {
            return Err(LowerError::unsupported(
                span,
                "a list element store argument",
                "the store stages the list, one index word, then the element's whole word span; a \
                 narrower staging mis-names the element channel",
            ));
        }
        if let Callee::Local(index) = target
            && let Some(expected) = self.callee_param_words(index)
            && staged.len() != expected
        {
            return Err(LowerError::unsupported(
                span,
                "a call argument",
                "the staged words disagree with the callee's parameter span; a union-typed \
                 parameter re-spans its member argument into the whole region at the call, so a \
                 disagreement here is a lowerer seam",
            ));
        }
        if let Callee::Foreign { def: None, .. } = target
            && let Some(params) = member_params
        {
            let mut expected = 0;
            for ty in params {
                let Some(words) = self.shapes.words(*ty).ok() else {
                    return Ok(());
                };
                expected += words.len();
            }
            if staged.len() != expected {
                return Err(LowerError::unsupported(
                    span,
                    "a call argument",
                    "the staged words disagree with the linked member's parameter span; a \
                     union-typed parameter re-spans its member argument into the whole region at \
                     the call, so a disagreement here is a lowerer seam",
                ));
            }
        }
        Ok(())
    }

    /// Guards a staged word count against the one-byte staging width:
    /// the overlay area and every call's nargs (a closure's capture
    /// count) address words by u8, so a staged span wider than 255
    /// words is a typed refusal at the point the span is built — the
    /// finalize-time frame check would come only after the staging
    /// moves lowered, and their conversions panic first.
    pub(in crate::func) fn guard_staged_count(&self, count: usize) -> Result<(), LowerError> {
        if count > usize::from(u8::MAX) {
            return Err(LowerError::FrameTooWide {
                name: self.name.clone(),
                need: self.slots.len() + count,
            });
        }
        Ok(())
    }

    /// One call argument's single scalar word kind, when its checked
    /// type is one I64/F64 word and its parameter neither re-spans the
    /// value (a union parameter takes the member's whole region) nor
    /// erases the staging kind (a type variable stages Raw).
    pub(in crate::func) fn scalar_arg_slot(
        &mut self,
        arg: &Expr,
        param_ty: Option<Ty>,
    ) -> Result<Option<SlotType>, LowerError> {
        let ty = self.val(arg.span())?.ty;
        if let Some(param) = param_ty
            && (param != ty && self.is_union_ty(param)? || self.is_var_ty(param))
        {
            return Ok(None);
        }
        Ok(match &*self.shapes.shape(ty)? {
            Shape::Word(slot @ (SlotType::I64 | SlotType::F64)) => Some(*slot),
            _ => None,
        })
    }

    /// A literal scalar argument's word kind and constant, when it may
    /// stage directly: the load emits at the staging position with
    /// every argument already evaluated, so a later argument's nested
    /// call cannot clobber the word, and the temp the ordinary
    /// spelling allocates is single-use by construction.
    pub(in crate::func) fn direct_literal(
        &mut self,
        arg: &Expr,
        param_ty: Option<Ty>,
    ) -> Result<Option<(SlotType, ScalarConst)>, LowerError> {
        let Expr::Literal { lit, .. } = peel_parens(arg) else {
            return Ok(None);
        };
        let Some(slot) = self.scalar_arg_slot(arg, param_ty)? else {
            return Ok(None);
        };
        Ok(scalar_const(lit, slot).map(|constant| (slot, constant)))
    }

    /// A computed scalar argument's word kind, when its outermost data
    /// op may write its overlay word directly: the argument itself must
    /// be the arithmetic binary that takes the offer, and every later
    /// argument must evaluate without touching the overlay — a later
    /// argument's nested call would stage over the word first. The call
    /// site additionally keeps tail calls on the evaluated temp (a tail
    /// transfer stages its arguments past the exit claims, which stage
    /// through the overlay over them).
    pub(in crate::func) fn direct_compute(
        &mut self,
        arg: &Expr,
        param_ty: Option<Ty>,
        later: &[&Expr],
    ) -> Result<Option<SlotType>, LowerError> {
        if !arith_shape(arg) || !later.iter().all(|expr| trivially_staged(expr)) {
            return Ok(None);
        }
        self.scalar_arg_slot(arg, param_ty)
    }

    /// One list literal `[a, b, c]`: the empty `list_new` call, then one
    /// `list_push` per element — the list word staged first, the
    /// element's words after it (an element expression may itself
    /// call). The list is a freshly created Ref its frame uniquely
    /// owns, so the in-place appends carry no window discipline.
    /// The staged element borrows or transfers by its own death: `list_push`
    /// acquires its own count when storing, so a dying element's post-call
    /// drop hands the count to the list (the push window's flush); a live
    /// element
    /// keeps its frame count and the list holds the acquired one.
    pub(in crate::func) fn list_literal(
        &mut self,
        e: &Expr,
        list_ty: Ty,
    ) -> Result<Place, LowerError> {
        let Expr::List { items, .. } = e else {
            return Err(mismatch("a list literal"));
        };
        // A union element stages its whole tag-plus-region span (the
        // element-slot argument's own rule): the literal's bare member
        // items re-span into the channel's element type first, or the
        // first push fixes the stride at one word and every later read
        // mis-stages the member's payload as the tag.
        let element = self.shapes.list_element(list_ty);
        let list = self.list_new_word()?;
        self.list_push_each(list, items.iter(), |this, item| {
            let mut arg = this.value(item)?;
            if let Some(element) = element
                && element != arg.ty
                && !this.diverges_at(item.span())
                && this.is_union_ty(element)?
            {
                let temp = this.temp_span(element)?;
                this.widen_into(&temp, element, &arg, item.span())?;
                arg = TypedPlace {
                    place: temp,
                    ty: element,
                };
            }
            Ok((arg, item.span()))
        })?;
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// Every element staged and pushed onto a list under construction:
    /// the shared per-element discipline — the push's borrow window
    /// opened ahead of the element's own staging, the list word
    /// read-pinned per push — the staging closure naming the staged
    /// element and its own diagnostic span.
    pub(in crate::func) fn list_push_each<E>(
        &mut self,
        list: u8,
        items: impl IntoIterator<Item = E>,
        mut stage: impl FnMut(&mut Self, E) -> Result<(TypedPlace, Span), LowerError>,
    ) -> Result<(), LowerError> {
        for item in items {
            // the push's borrow window opens before the element
            // staging (an element expression may itself call) and pins
            // the list word plus every word the push stages
            self.rc.native_open();
            self.rc.native_reads([list]);
            let (item_val, span) = stage(self, item)?;
            self.list_push_word(list, &item_val, span)?;
        }
        Ok(())
    }

    /// One element's push onto a list under construction: the guarded
    /// element staged through the `list_push` native's overlay. The
    /// literal construction and the embedded comptime answer share the
    /// spelling; the caller owns the borrow window and the element's own
    /// lowering.
    pub(in crate::func) fn list_push_word(
        &mut self,
        list: u8,
        item_val: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        let staged_val =
            self.stage_heap_element(item_val.clone(), span, "a list literal element")?;
        let push = crate::native::lookup("list_push")
            .expect("the list surface is covered by the native table");
        let mut staged = vec![SlotType::Ref];
        let mut moves = vec![(0usize, list)];
        for addr in self.word_addrs(&staged_val.place, staged_val.ty)? {
            moves.push((staged.len(), addr.slot));
            staged.push(addr.slot_ty);
        }
        self.guard_staged_count(staged.len())?;
        for (index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
            // index 0 is the construction target itself: the list is
            // the frame's own unique word, so its staging is a borrow
            // that never drops mid-construction — the value's real
            // death (a binder's scope end or the frame exit) owns the
            // count
            if index > 0 && staged[index] == SlotType::Ref {
                self.rc.staged_native(slot, false);
            }
        }
        self.overlay_site(&staged, &[SlotType::Ref]);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(push)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        // the push answers the list's post-mutation word under one
        // count: the constructing frame's list is unique, so the answer
        // is the list's own word and the slot adopts its claim — the
        // replaced word's count dies as the value the list already was
        self.rc.store_over(&mut self.code, &[list]);
        self.emit(Op::Mov {
            dst: Slot::body(list),
            src: Slot::overlay(0),
        });
        self.rc.produced(list);
        Ok(())
    }

    /// The empty-list word: the zero-argument `list_new` call whose one
    /// Ref result lands on the overlay (the empty `list_new` call still
    /// needs its one-word overlay area — the result must land
    /// somewhere), copied into a fresh body word the frame owns.
    pub(in crate::func) fn list_new_word(&mut self) -> Result<u8, LowerError> {
        let native = crate::native::lookup("list_new")
            .expect("the list surface is covered by the native table");
        self.overlay_site(&[], &[SlotType::Ref]);
        self.rc.native_open();
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 0,
        });
        self.rc.native_done(&mut self.code);
        let list = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(list),
            src: Slot::overlay(0),
        });
        self.rc.produced(list);
        Ok(list)
    }

    /// One generic-application constructor call `list[t]()`:
    /// the head resolved in the type namespace and the checker typed
    /// the result `list<arg>`; the runtime half is `list_new` — the
    /// element type argument never reaches the code, every list being
    /// the one Ref word.
    pub(in crate::func) fn ctor_call(
        &mut self,
        recv: &Expr,
        args: &[CallArg],
        span: Span,
    ) -> Result<Place, LowerError> {
        let Some(head) = single_path_ident(recv) else {
            return Err(mismatch("a type application"));
        };
        let Some(def) = self.analyzed.package.lookup_type(&head.name) else {
            return Err(LowerError::unsupported(
                span,
                "an indexed callee",
                "the head names no type; indexing is outside the lowered surface",
            ));
        };
        let one_param_fieldless = self
            .analyzed
            .decls
            .structs
            .get(&def)
            .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty());
        // the channel's construction: the one type application carrying a
        // value argument — the bound staged to `chan_new`, whose Ref answer
        // the frame owns
        if one_param_fieldless && head.name.as_str() == "chan" {
            let [cap] = args else {
                return Err(mismatch("the channel constructor's bound"));
            };
            // the checker contained the bound to int, so the value is
            // the one i64 word its place names
            let bound = self.value(&cap.value)?.place.base;
            let native = crate::native::lookup("chan_new")
                .expect("the channel surface is covered by the native table");
            self.overlay_site(&[SlotType::I64], &[SlotType::Ref]);
            self.rc.native_open();
            self.emit(Op::Mov {
                dst: Slot::overlay(0),
                src: Slot::body(bound),
            });
            self.rc.staged_native(bound, false);
            let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
            self.emit(Op::Call {
                callee: kidx,
                nargs: 1,
            });
            self.rc.native_done(&mut self.code);
            let chan = self.temp_word(SlotType::Ref)?;
            self.emit(Op::Mov {
                dst: Slot::body(chan),
                src: Slot::overlay(0),
            });
            self.rc.produced(chan);
            return Ok(Place {
                base: chan,
                shape: PlaceShape::Word(SlotType::Ref),
            });
        }
        // the arena's construction (the read-side aliasing container):
        // the empty-arena spelling `arena[t]()`, the element type never
        // reaching the code (every arena one Ref word) — the runtime
        // half is `arena_new`, whose Ref answer the frame owns
        if one_param_fieldless && head.name.as_str() == "arena" {
            if !args.is_empty() {
                return Err(mismatch("the empty-arena constructor's arguments"));
            }
            let native = crate::native::lookup("arena_new")
                .expect("the arena surface is covered by the native table");
            self.overlay_site(&[], &[SlotType::Ref]);
            self.rc.native_open();
            let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
            self.emit(Op::Call {
                callee: kidx,
                nargs: 0,
            });
            self.rc.native_done(&mut self.code);
            let arena = self.temp_word(SlotType::Ref)?;
            self.emit(Op::Mov {
                dst: Slot::body(arena),
                src: Slot::overlay(0),
            });
            self.rc.produced(arena);
            return Ok(Place {
                base: arena,
                shape: PlaceShape::Word(SlotType::Ref),
            });
        }
        // the owning container's construction `box[t](v)`: the held
        // value's whole word span stages to `box_new`, whose Ref answer
        // the frame owns — the element type never reaches the code,
        // every box one Ref word
        if one_param_fieldless && head.name.as_str() == "box" {
            let [value] = args else {
                return Err(mismatch("the box constructor's value"));
            };
            let elem = self.val(span)?.ty;
            let elem = self
                .shapes
                .box_element(elem)
                .ok_or_else(|| mismatch("the box constructor's element type"))?;
            let held = self.value(&value.value)?;
            let claim = self.deinit_claim(held.ty, value.value.span())?;
            if claim.is_some() {
                return Err(LowerError::unsupported(
                    value.value.span(),
                    "a deinit-carrying box value",
                    "the container's death owns counts, never claims, so the value's hook has no \
                     firing position; per-span deinit claims own the seam",
                ));
            }
            let mut held = held;
            if held.ty != elem {
                let temp = self.temp_span(elem)?;
                self.widen_into(&temp, elem, &held, value.value.span())?;
                held = TypedPlace {
                    place: temp,
                    ty: elem,
                };
            }
            let elem_slots: Vec<SlotType> = self.shapes.words(elem)?.to_vec();
            self.overlay_site(&elem_slots, &[SlotType::Ref]);
            let addrs = self.word_addrs(&held.place, held.ty)?;
            for (index, word) in addrs.iter().enumerate() {
                let at = u8::try_from(index).expect("the element span fits the overlay");
                self.emit(Op::Mov {
                    dst: Slot::overlay(at),
                    src: Slot::body(word.slot),
                });
                self.rc.staged_native(word.slot, false);
            }
            let native = crate::native::lookup("box_new")
                .expect("the box surface is covered by the native table");
            self.rc.native_open();
            let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(elem_slots.len()).expect("the element span fits a call"),
            });
            self.rc.native_done(&mut self.code);
            let boxed = self.temp_word(SlotType::Ref)?;
            self.emit(Op::Mov {
                dst: Slot::body(boxed),
                src: Slot::overlay(0),
            });
            self.rc.produced(boxed);
            return Ok(Place {
                base: boxed,
                shape: PlaceShape::Word(SlotType::Ref),
            });
        }
        if !one_param_fieldless {
            return Err(LowerError::unsupported(
                span,
                "a type-application constructor",
                "only the collection types construct by type application [B3]",
            ));
        }
        if !args.is_empty() {
            return Err(mismatch("the empty-list constructor's arguments"));
        }
        let list = self.list_new_word()?;
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// One map constructor call `map[k, v]()`: the checker typed
    /// the result `map<key, value>`; the runtime half is `map_new` — the
    /// type arguments never reach the code, every map being the one Ref
    /// word. The empty-map word's overlay discipline is `list_new`'s: the
    /// one-word overlay area exists because the result must land
    /// somewhere.
    pub(in crate::func) fn map_ctor_call(&mut self, args: &[CallArg]) -> Result<Place, LowerError> {
        if !args.is_empty() {
            return Err(mismatch("the empty-map constructor's arguments"));
        }
        let native = crate::native::lookup("map_new")
            .expect("the map surface is covered by the native table");
        self.overlay_site(&[], &[SlotType::Ref]);
        self.rc.native_open();
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 0,
        });
        self.rc.native_done(&mut self.code);
        let map = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(map),
            src: Slot::overlay(0),
        });
        self.rc.produced(map);
        Ok(Place {
            base: map,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// One embedded comptime answer: the evaluator's recorded constant
    /// lowered exactly like the literal a user would have written at the
    /// call's own span (whose checked type is the call's result type, so
    /// the literal arm reads a sound fact).
    pub(in crate::func) fn embedded_value(
        &mut self,
        constant: &crate::ComptimeConst,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        if let crate::ComptimeConst::List(items) = constant {
            return self.embedded_list(items, span);
        }
        if let crate::ComptimeConst::Tuple(items) = constant {
            return self.embedded_tuple(items, ty, span);
        }
        let synth = embedded_const_expr(constant, span)?;
        let typed = self.value(&synth)?;
        Ok(typed.place)
    }

    /// A tuple answer: the tuple literal a user would have written at
    /// the call's span — one word per element staged into the tuple's
    /// own span from the call's checked tuple shape, each element's
    /// literal keyed by its position's field (the evaluator refused
    /// every answer whose element is not one embeddable scalar over its
    /// position's element type before recording it, so an element whose
    /// word disagrees with its field is the belt refusal).
    pub(in crate::func) fn embedded_tuple(
        &mut self,
        items: &[crate::ComptimeConst],
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(LowerError::unsupported(
                span,
                "an embedded tuple answer",
                "the call's checked type is not a tuple; the evaluator's gate disagrees with                  the recorded answer",
            ));
        };
        if s.fields.len() != items.len() {
            return Err(LowerError::unsupported(
                span,
                "an embedded tuple answer",
                "the answer's arity disagrees with the call's checked tuple; the evaluator's                  gate disagrees with the recorded answer",
            ));
        }
        let target = self.temp_span(ty)?;
        for (field, item) in s.fields.iter().zip(items) {
            let place = self.embedded_tuple_word(item, field.slot(), span)?;
            self.emit(Op::Mov {
                dst: Slot::body(target.base + field.word),
                src: Slot::body(place.base),
            });
            if field.slot() == SlotType::Ref {
                self.rc
                    .copied(&mut self.code, target.base + field.word, place.base);
            }
        }
        Ok(target)
    }

    /// One embedded tuple element's literal word: the scalar atoms stage
    /// their own literal and a string its interned word; the literal's
    /// intrinsic classification must agree with the tuple field's.
    pub(in crate::func) fn embedded_tuple_word(
        &mut self,
        item: &crate::ComptimeConst,
        field_slot: SlotType,
        span: Span,
    ) -> Result<Place, LowerError> {
        let disagrees = "the element's word disagrees with its tuple field; the evaluator's                          gate disagrees with the recorded answer";
        match item {
            crate::ComptimeConst::Str(text) => {
                if field_slot != SlotType::Ref {
                    return Err(LowerError::unsupported(
                        span,
                        "an embedded tuple element",
                        disagrees,
                    ));
                }
                self.string_expr(&[StrPart::Text {
                    span,
                    text: text.clone(),
                }])
            },
            crate::ComptimeConst::Int(value) => {
                if field_slot != SlotType::I64 {
                    return Err(LowerError::unsupported(
                        span,
                        "an embedded tuple element",
                        disagrees,
                    ));
                }
                self.literal(
                    &Literal::Int {
                        suffix: String::new(),
                        text: value.to_string(),
                    },
                    span,
                )
            },
            crate::ComptimeConst::Float(value) => {
                if field_slot != SlotType::F64 {
                    return Err(LowerError::unsupported(
                        span,
                        "an embedded tuple element",
                        disagrees,
                    ));
                }
                self.literal(
                    &Literal::Float {
                        suffix: String::new(),
                        text: value.to_string(),
                    },
                    span,
                )
            },
            crate::ComptimeConst::Bool(value) => {
                if field_slot != SlotType::Bool {
                    return Err(LowerError::unsupported(
                        span,
                        "an embedded tuple element",
                        disagrees,
                    ));
                }
                self.literal(&Literal::Bool(*value), span)
            },
            crate::ComptimeConst::List(_) | crate::ComptimeConst::Tuple(_) => {
                Err(LowerError::unsupported(
                    span,
                    "an embedded tuple element",
                    "a nested compound answer refuses at the evaluator",
                ))
            },
        }
    }

    /// A list answer: the list literal a user would have written at the
    /// call's span — one `list_push` per element, each element's literal
    /// staged from the element type the call's own checked fact names
    /// (the evaluator refused every answer whose call type is not the
    /// list application before recording it).
    pub(in crate::func) fn embedded_list(
        &mut self,
        items: &[crate::ComptimeConst],
        span: Span,
    ) -> Result<Place, LowerError> {
        let elem_ty = self.embedded_list_element_ty(span)?;
        let list = self.list_new_word()?;
        self.list_push_each(list, items, |this, item| {
            Ok((this.embedded_scalar(item, elem_ty, span)?, span))
        })?;
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// The element type of one embedded list answer's call site: the one
    /// application argument of the singleton apply the checker recorded
    /// at the span (the evaluator's embed gate and this read agree; a
    /// disagreement is the belt refusal, never a silent mis-lowering).
    pub(in crate::func) fn embedded_list_element_ty(&self, span: Span) -> Result<Ty, LowerError> {
        let ty = self.val(span)?.ty;
        let elem = self
            .analyzed
            .decls
            .universe
            .members(ty)
            .first()
            .and_then(|&atom| {
                let decl = self.universe().atom_decl(atom).ok()?;
                match &decl.kind {
                    AtomKind::Apply(_, args) => args.first().copied(),
                    _ => None,
                }
            });
        elem.ok_or_else(|| {
            LowerError::unsupported(
                span,
                "an embedded list answer",
                "the call's checked type is not a list application; the evaluator's gate \
                 disagrees with the recorded answer",
            )
        })
    }

    /// One embedded element's literal place: the element type comes from
    /// the list's own construction because the synthesized node carries
    /// no checked fact of its own.
    pub(in crate::func) fn embedded_scalar(
        &mut self,
        constant: &crate::ComptimeConst,
        ty: Ty,
        span: Span,
    ) -> Result<TypedPlace, LowerError> {
        let place = match constant {
            crate::ComptimeConst::Str(text) => self.string_expr(&[StrPart::Text {
                span,
                text: text.clone(),
            }])?,
            crate::ComptimeConst::Int(value) => self.literal(
                &Literal::Int {
                    suffix: String::new(),
                    text: value.to_string(),
                },
                span,
            )?,
            crate::ComptimeConst::Float(value) => self.literal(
                &Literal::Float {
                    suffix: String::new(),
                    text: value.to_string(),
                },
                span,
            )?,
            crate::ComptimeConst::Bool(value) => self.literal(&Literal::Bool(*value), span)?,
            crate::ComptimeConst::List(_) => {
                return Err(LowerError::unsupported(
                    span,
                    "an embedded list element",
                    "a nested list answer refuses at the evaluator",
                ));
            },
            crate::ComptimeConst::Tuple(_) => {
                return Err(LowerError::unsupported(
                    span,
                    "an embedded list element",
                    "a nested tuple answer refuses at the evaluator",
                ));
            },
        };
        Ok(TypedPlace { place, ty })
    }

    /// Constructs one minted enum case by call form: the case's own
    /// struct span, fields filled positionally or by name, exactly like a
    /// struct literal with positional slots admitted.
    pub(in crate::func) fn case_value(
        &mut self,
        args: &[CallArg],
        ty: Ty,
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Struct(s) = &*shape else {
            return Err(mismatch("a case construction"));
        };
        let target = self.temp_span(ty)?;
        for (index, arg) in args.iter().enumerate() {
            let position = match &arg.name {
                Some(name) => s
                    .fields
                    .iter()
                    .position(|f| f.name.as_deref() == Some(name.name.as_str()))
                    .ok_or_else(|| mismatch("a case construction field"))?,
                None => index,
            };
            let Some(field) = s.fields.get(position) else {
                return Err(mismatch("a case construction slot"));
            };
            let v = self.value(&arg.value)?;
            let addrs = self
                .word_addrs(&v.place, v.ty)
                .map_err(|err| err.respanned(arg.value.span()))?;
            if addrs.len() != field.width() {
                // a member value into a union-typed payload field re-spans
                // through the field's own region, the literal store's rule
                let members = self.field_members(ty, position);
                self.store_union_member_field(
                    target.base + field.word,
                    members.as_deref(),
                    &v,
                    arg.span,
                )?;
                continue;
            }
            // a fn-typed payload field holds the closure Ref word: the
            // struct literal's own store rule (the proven scalar wrapping,
            // the unproven word refusing)
            let addrs = if field.slot() == SlotType::Ref && addrs[0].slot_ty == SlotType::Fn {
                vec![self.stage_heap_fn(&v, arg.span)?]
            } else {
                addrs
            };
            self.store_field_span(target.base, field, &addrs);
        }
        Ok(target)
    }

    /// The case-construction door both spellings share: a payloaded case
    /// of a user-declared generic enum constructs over the call's own
    /// checked application (the per-application case the checker's
    /// construction record names), every other case or tag over its own
    /// atom.
    pub(in crate::func) fn case_value_for(
        &mut self,
        args: &[CallArg],
        ty: Ty,
        case_def: DefId,
        span: Span,
    ) -> Result<Place, LowerError> {
        let generic_enum = self
            .analyzed
            .decls
            .cases
            .get(&case_def)
            .and_then(|case| self.analyzed.decls.enums.get(&case.enum_def))
            .is_some_and(|shape| !shape.params.is_empty());
        if generic_enum {
            return self.applied_case_value(args, ty, case_def, span);
        }
        self.case_value(args, ty)
    }

    /// Constructs one payloaded case of a user-declared generic enum's
    /// application: the call's checked type is the application itself,
    /// so the temp spans the application's whole union — the case's
    /// tag minted at the tag word, the dead region zeroed, and each
    /// payload slot stored at the case's claimed region offsets.
    pub(in crate::func) fn applied_case_value(
        &mut self,
        args: &[CallArg],
        ty: Ty,
        case_def: DefId,
        span: Span,
    ) -> Result<Place, LowerError> {
        let app = self.only_atom(ty, span)?;
        let Some(cases) = self.shapes.apply_cases_of(app) else {
            return Err(LowerError::unsupported(
                span,
                "a case construction",
                "the application's case atoms never minted; a payload that never solved carries \
                 no value shape",
            ));
        };
        let Some(case) = cases.iter().find(|case| case.def == case_def) else {
            return Err(mismatch("a case construction"));
        };
        let Some(slots) = &case.slots else {
            return Err(mismatch("a case construction"));
        };
        let atom = case.atom;
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(union) = &*shape else {
            return Err(mismatch("a case construction"));
        };
        let member = union
            .member(atom)
            .cloned()
            .ok_or_else(|| mismatch("a case construction's member"))?;
        let target = self.temp_span(ty)?;
        let tag = self.shapes.tag_id(atom)?;
        self.emit(Op::TagNew {
            dst: Slot::body(target.base),
            tag,
        });
        self.zero_init_member(target.base, &union.words, &member.fields)?;
        let mut positional = 0usize;
        for arg in args {
            let position = if let Some(name) = &arg.name {
                member
                    .fields
                    .iter()
                    .position(|field| field.name.as_deref() == Some(name.name.as_str()))
                    .ok_or_else(|| mismatch("a case construction field"))?
            } else {
                let index = positional;
                positional += 1;
                index
            };
            let Some(field) = member.fields.get(position) else {
                return Err(mismatch("a case construction slot"));
            };
            let Some(slot) = slots.get(position) else {
                return Err(mismatch("a case construction slot"));
            };
            let v = self.value(&arg.value)?;
            let addrs = self
                .word_addrs(&v.place, v.ty)
                .map_err(|err| err.respanned(arg.value.span()))?;
            if addrs.len() != field.width() {
                // a member value into a union-typed payload slot re-spans
                // through the slot's own region, the literal store's rule
                let members = self.universe().members(slot.ty).to_vec();
                self.store_union_member_field(
                    target.base + field.word,
                    Some(members.as_slice()),
                    &v,
                    arg.span,
                )?;
                continue;
            }
            // a fn-typed payload slot holds the closure Ref word: the
            // struct literal's own store rule
            let addrs = if field.slot() == SlotType::Ref && addrs[0].slot_ty == SlotType::Fn {
                vec![self.stage_heap_fn(&v, arg.span)?]
            } else {
                addrs
            };
            self.store_field_span(target.base, field, &addrs);
        }
        Ok(target)
    }

    /// The literal-only parameter defaults a local call stages
    /// for its omitted trailing parameters: the checker proved each
    /// default's literal against its parameter type, so the call lowers
    /// them exactly like written arguments. A non-defaulted omission is
    /// the checker's to reject; the call-site span guard names anything
    /// that still slips through.
    pub(in crate::func) fn defaulted_args<'e>(
        &self,
        def: DefId,
        target: Callee,
        written: Vec<&'e Expr>,
    ) -> Vec<&'e Expr>
    where
        'a: 'e,
    {
        if !matches!(target, Callee::Local(_)) {
            return written;
        }
        let Some(sig) = self.analyzed.decls.fns.get(&def) else {
            return written;
        };
        let Some(missing) = sig.params.get(written.len()..) else {
            return written;
        };
        if !missing.iter().all(|param| param.default.is_some()) {
            return written;
        }
        let mut staged = written;
        staged.extend(
            missing
                .iter()
                .map(|param| param.default.as_ref().expect("checked directly above")),
        );
        staged
    }

    /// The literal-only parameter defaults a trait-method dispatch
    /// call stages at the call site for its omitted trailing parameters:
    /// the trait declaration is a defaulted member parameter's single
    /// authority, so the dot spelling and the qualified
    /// `Trait::method` spelling stage the trait member's registered
    /// defaults — the same literals the checker proved against the
    /// parameter types at this call site — exactly like written
    /// arguments. The staged exprs are span-preserving clones of the
    /// trait's declared literals, so the checker's span-keyed tables
    /// resolve them unchanged. A non-defaulted omission is the
    /// checker's to reject; the staged-width guard at the call names
    /// anything that slips through.
    pub(in crate::func) fn trait_default_exprs(
        &self,
        method: MethodId,
        written: usize,
    ) -> Vec<Expr> {
        let Some(sig) = self.analyzed.decls.members.get(&method) else {
            return Vec::new();
        };
        let Some(missing) = sig.params.get(written..) else {
            return Vec::new();
        };
        if !missing.iter().all(|param| param.default.is_some()) {
            return Vec::new();
        }
        missing
            .iter()
            .map(|param| param.default.clone().expect("checked directly above"))
            .collect()
    }

    /// The literal-only parameter defaults an inherent-member call
    /// stages at the call site for its omitted trailing parameters: the
    /// checker proves each default's literal against the parameter type
    /// at the call (inherent members own their body defaults — no trait
    /// declaration exists), so the call lowers them exactly like written
    /// arguments — the same staging the free-fn spelling applies, over
    /// the impl body's own declaration (the plan entry the dispatch
    /// resolved). The staged exprs are span-preserving clones, so the
    /// checker's span-keyed tables resolve them unchanged. A default the
    /// checker never typed refuses naming the seam; a non-defaulted
    /// omission is the checker's to reject, and the staged-width guard
    /// at the call names anything that slips through.
    pub(in crate::func) fn member_default_exprs(
        &self,
        index: u32,
        written: usize,
        span: Span,
    ) -> Result<Vec<Expr>, LowerError> {
        let Some(decl) = &self.plan.functions()[index as usize].decl else {
            return Ok(Vec::new());
        };
        let Some(missing) = decl.params.get(written..) else {
            return Ok(Vec::new());
        };
        if !missing.iter().all(|param| param.default.is_some()) {
            return Ok(Vec::new());
        }
        for param in missing {
            let default = param.default.as_ref().expect("checked directly above");
            if !self
                .analyzed
                .checked
                .expr_types
                .contains_key(&default.span())
            {
                return Err(LowerError::unsupported(
                    span,
                    "a defaulted trait-member parameter",
                    "the call site checks the trait's declared default while the staging lowers \
                     the impl body's own re-declared one; the two spellings disagree this round",
                ));
            }
        }
        Ok(missing
            .iter()
            .map(|param| {
                param
                    .default
                    .as_ref()
                    .expect("checked directly above")
                    .clone()
            })
            .collect())
    }
}

/// One embedded scalar answer's literal expression, the node a user
/// would have written at `span` (the compound answers lower through
/// their own paths in `embedded_value`, so a compound here is the belt
/// refusal).
pub(in crate::func) fn embedded_const_expr(
    constant: &crate::ComptimeConst,
    span: Span,
) -> Result<Expr, LowerError> {
    Ok(match constant {
        crate::ComptimeConst::Str(text) => Expr::Str {
            span,
            parts: vec![StrPart::Text {
                span,
                text: text.clone(),
            }],
        },
        crate::ComptimeConst::Int(value) => Expr::Literal {
            span,
            lit: Literal::Int {
                suffix: String::new(),
                text: value.to_string(),
            },
        },
        crate::ComptimeConst::Float(value) => Expr::Literal {
            span,
            lit: Literal::Float {
                suffix: String::new(),
                text: value.to_string(),
            },
        },
        crate::ComptimeConst::Bool(value) => Expr::Literal {
            span,
            lit: Literal::Bool(*value),
        },
        // the evaluator's gate refuses a compound inside a tuple, so this
        // is the belt refusal, never a silent mis-lowering
        crate::ComptimeConst::List(_) => {
            return Err(LowerError::unsupported(
                span,
                "an embedded tuple element",
                "a nested list answer refuses at the evaluator",
            ));
        },
        crate::ComptimeConst::Tuple(_) => {
            return Err(LowerError::unsupported(
                span,
                "an embedded tuple element",
                "a nested tuple answer refuses at the evaluator",
            ));
        },
    })
}
