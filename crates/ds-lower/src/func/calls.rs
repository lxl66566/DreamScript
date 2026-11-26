//! The call-staging vocabulary: the argument-staging guards, list/map
//! construction, embedded comptime answers, the one staging walk over every
//! callee class (`call_impl`), the trait-dispatch evidence chains, the
//! fallible native results, and the `ok`/`err` result arms.

use super::*;

/// A bodiless ambient collection member's native routing: the extern
/// def the call lowers through, and whether the member re-answers the
/// receiver after the native's in-place mutation (`push`/`set`).
#[derive(Clone, Copy)]
struct NativeMember {
    def: DefId,
    window: bool,
}

impl<'a> FnLower<'a, '_, '_> {
    // ---- calls ------------------------------------------------------------------

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
    fn element_slot_ty(&self, target: &Callee, receiver: Option<Ty>) -> Option<Ty> {
        let ty = receiver?;
        match target {
            Callee::Builtin(native) if native.name == "list_push" => self.shapes.list_element(ty),
            Callee::Builtin(native) if native.name == "list_set" => self.shapes.list_element(ty),
            Callee::Builtin(native) if native.name == "list_insert" => self.shapes.list_element(ty),
            Callee::Builtin(native) if native.name == "map_set" => self.shapes.map_value(ty),
            Callee::Builtin(native) if native.name == "chan_send" => self.shapes.chan_payload(ty),
            Callee::Builtin(native) if native.name == "arena_insert" => {
                self.shapes.arena_element(ty)
            },
            _ => None,
        }
    }

    pub(super) fn guard_list_element(
        &mut self,
        val: &TypedPlace,
        span: Span,
        what: &'static str,
    ) -> Result<(), LowerError> {
        match &*self.shapes.shape(val.ty)? {
            Shape::Struct(_) => {
                if self.deinit_hook(val.ty, span)?.is_some() {
                    return Err(LowerError::unsupported(
                        span,
                        what,
                        "a deinit-carrying element's hook has no claim inside the list's edge \
                         retention; per-span deinit claims own the seam",
                    ));
                }
                Ok(())
            },
            Shape::Tuple(s) => {
                // the tuple's one-word elements ride the channel like a
                // struct's flattened span, gathered positionally; a
                // callable element word keeps the scalar-provenance rule
                // (a closure word would read back as a Ref behind a
                // Fn-classified payload slot)
                let addrs = self.word_addrs(&val.place, val.ty)?;
                for (field, addr) in s.fields.iter().zip(addrs.iter()) {
                    if field.slot() == SlotType::Fn && !self.scalar_fn_words.contains(&addr.slot) {
                        return Err(LowerError::unsupported(
                            span,
                            what,
                            "a callable element must prove a non-capturing scalar word; a closure \
                             or an unproven callable parameter cannot inhabit a collection element",
                        ));
                    }
                }
                Ok(())
            },
            Shape::Union(u) => {
                // a callable word inside the region keeps the
                // scalar-provenance rule (the tuple arm's own contract,
                // one address per span word)
                if u.words.contains(&SlotType::Fn) {
                    let addrs = self.word_addrs(&val.place, val.ty)?;
                    for (slot, addr) in u.words.iter().zip(addrs.iter()) {
                        if *slot == SlotType::Fn && !self.scalar_fn_words.contains(&addr.slot) {
                            return Err(LowerError::unsupported(
                                span,
                                what,
                                "a callable element must prove a non-capturing scalar word; a \
                                 closure or an unproven callable parameter cannot inhabit a \
                                 collection element",
                            ));
                        }
                    }
                }
                Ok(())
            },
            Shape::Word(_) => {
                if matches!(&*self.shapes.shape(val.ty)?, Shape::Word(SlotType::Fn)) {
                    let addrs = self.word_addrs(&val.place, val.ty)?;
                    let proven = addrs.len() == 1 && self.scalar_fn_words.contains(&addrs[0].slot);
                    if !proven {
                        return Err(LowerError::unsupported(
                            span,
                            what,
                            "a callable element must prove a non-capturing scalar word; a closure \
                             or an unproven callable parameter cannot inhabit a collection element",
                        ));
                    }
                }
                Ok(())
            },
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
    fn guard_staged_width(
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
    pub(super) fn guard_staged_count(&self, count: usize) -> Result<(), LowerError> {
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
    fn scalar_arg_slot(
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
    fn direct_literal(
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
    fn direct_compute(
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
    /// owns, so the in-place appends carry no window discipline [B3].
    /// The staged element borrows or transfers by its own death: `list_push`
    /// acquires its own count when storing, so a dying element's post-call
    /// drop hands the count to the list (the push window's flush); a live
    /// element
    /// keeps its frame count and the list holds the acquired one.
    pub(super) fn list_literal(&mut self, e: &Expr, list_ty: Ty) -> Result<Place, LowerError> {
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
    pub(super) fn list_push_each<E>(
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
    pub(super) fn list_push_word(
        &mut self,
        list: u8,
        item_val: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.guard_list_element(item_val, span, "a list literal element")?;
        let push = crate::native::lookup("list_push")
            .expect("the list surface is covered by the native table");
        let mut staged = vec![SlotType::Ref];
        let mut moves = vec![(0usize, list)];
        for addr in self.word_addrs(&item_val.place, item_val.ty)? {
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
        self.overlay_site(&staged, &[SlotType::I64]); // the unit word
        let kidx = self.pool(self.funcref_const(Callee::Builtin(push)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        Ok(())
    }

    /// The empty-list word: the zero-argument `list_new` call whose one
    /// Ref result lands on the overlay (the empty `list_new` call still
    /// needs its one-word overlay area — the result must land
    /// somewhere), copied into a fresh body word the frame owns.
    pub(super) fn list_new_word(&mut self) -> Result<u8, LowerError> {
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

    /// One generic-application constructor call `list[t]()` [F9, B3]:
    /// the head resolved in the type namespace and the checker typed
    /// the result `list<arg>`; the runtime half is `list_new` — the
    /// element type argument never reaches the code, every list being
    /// the one Ref word.
    fn ctor_call(
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
        // the channel's construction (the 2026-11-21 shared-state
        // ruling): the one type application carrying a value argument —
        // the bound staged to `chan_new`, whose Ref answer the frame owns
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

    /// One map constructor call `map[k, v]()` [F9, B3]: the checker typed
    /// the result `map<key, value>`; the runtime half is `map_new` — the
    /// type arguments never reach the code, every map being the one Ref
    /// word. The empty-map word's overlay discipline is `list_new`'s: the
    /// one-word overlay area exists because the result must land
    /// somewhere.
    fn map_ctor_call(&mut self, args: &[CallArg]) -> Result<Place, LowerError> {
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

    pub(super) fn call_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        self.call_impl(e, ty, false)
    }

    /// One embedded comptime answer: the evaluator's recorded constant
    /// lowered exactly like the literal a user would have written at the
    /// call's own span (whose checked type is the call's result type, so
    /// the literal arm reads a sound fact).
    fn embedded_value(
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
    fn embedded_tuple(
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
    fn embedded_tuple_word(
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
    fn embedded_list(
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
    fn embedded_list_element_ty(&self, span: Span) -> Result<Ty, LowerError> {
        let ty = self.val(span)?.ty;
        let elem = self
            .analyzed
            .decls
            .universe
            .members(ty)
            .first()
            .and_then(|&atom| {
                let decl = self.analyzed.decls.universe.atom_decl(atom).ok()?;
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
    fn embedded_scalar(
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

    /// Constructs one minted enum case by call form [B3]: the case's own
    /// struct span, fields filled positionally or by name, exactly like a
    /// struct literal with positional slots admitted.
    fn case_value(&mut self, args: &[CallArg], ty: Ty) -> Result<Place, LowerError> {
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
                let decl_expr = self.field_decl_expr(ty, position);
                self.store_union_member_field(target.base + field.word, decl_expr, &v, arg.span)?;
                continue;
            }
            self.store_field_span(target.base, field, &addrs);
        }
        Ok(target)
    }

    // One staging walk covers every callee class; extracting the tail or
    // native arms would re-thread the staged span and evidence state.
    #[allow(clippy::too_many_lines)]
    pub(super) fn call_impl(&mut self, e: &Expr, ty: Ty, tail: bool) -> Result<Place, LowerError> {
        let Expr::Call {
            callee,
            args,
            trailing,
            span,
        } = e
        else {
            return Err(mismatch("a call"));
        };
        // the call's own span is the trait-dispatch table's key (the
        // checker records the solved instance there); the callee-shape
        // destructure below rebinds `span` to the callee path's own
        let call_span = *span;
        // A minted case constructor takes named arguments [B3]; named
        // arguments on functions stay outside the M3 contract. A
        // payloaded tag constructs through the same call form.
        let case_ctor = match callee.as_ref() {
            Expr::Path { span, .. } => match self.res.get(*span) {
                Some(Res::Def(def)) => {
                    if let Some(case) = self.analyzed.decls.cases.get(def)
                        && !matches!(case.payload, EnumPayload::None)
                    {
                        Some(*def)
                    } else if self.analyzed.package.def(*def).kind == ds_check::DefKind::Tag
                        && self.analyzed.decls.structs.contains_key(def)
                    {
                        Some(*def)
                    } else {
                        None
                    }
                },
                _ => None,
            },
            _ => None,
        };
        for arg in args {
            if arg.name.is_some() && case_ctor.is_none() {
                return Err(LowerError::unsupported(
                    arg.span,
                    "a named argument",
                    "named arguments are not in the M3 contract",
                ));
            }
        }
        // A generic-application constructor `list[t]()` [F9]: the
        // bracketed callee head resolved in the type namespace.
        if let Expr::Index { recv, .. } = &**callee {
            return self.ctor_call(recv, args, *span);
        }
        // The map constructor `map[k, v]()` [F9, B3]: the two-argument
        // application of the map struct.
        if let Expr::MapApply { .. } = &**callee {
            return self.map_ctor_call(args);
        }
        // A bodiless ambient collection member (`xs.get(i)`, `xs.push(x)`,
        // `xs.concat(ys)`, `m.get(k)`, `m.set(k, v)`) routes to its native
        // seam through the
        // free-callee path below: the receiver feeds the first argument,
        // exactly like the retired free spelling's own call. `push`/`set`
        // additionally re-answer the receiver after the native's unit word
        // (the member's answer is the collection itself).
        let native_member = self.native_member_def(call_span);
        // the callee and its argument expressions in call order: a plain
        // name call feeds the written arguments; a UFCS dot-call `x.f(a)`
        // resolves to the free function and stages the receiver as its
        // first argument — `f(x, a)`, zero runtime cost [F3]. A
        // trait-method site carries no def: the solved instance names
        // the impl body (the dispatch side channel below), and `None`
        // marks the trait path.
        let (def, callee_span, mut arg_exprs, trait_dispatch): (
            Option<DefId>,
            Span,
            Vec<&Expr>,
            Option<TraitDispatch>,
        ) = if let Expr::Field { recv, name, .. } = &**callee {
            match self.res.get(name.span) {
                Some(Res::Handler) => {
                    // the `.err`/`.ok` handlers: tag dispatch plus an
                    // inlined handler body
                    let [handler] = args.as_slice() else {
                        return Err(mismatch("a handler argument"));
                    };
                    match name.name.as_str() {
                        "err" => return self.err_value(recv, &handler.value, ty),
                        "ok" => return self.ok_value(recv, &handler.value, ty),
                        _ => {
                            return Err(LowerError::unsupported(
                                name.span,
                                "a handler",
                                "only `.err` and `.ok` are lowerable this round",
                            ))
                        },
                    }
                },
                Some(Res::Dot(DotRes::Free { func, fed_param })) => {
                    if *fed_param != 0 {
                        return Err(LowerError::unsupported(
                            name.span,
                            "a dot-call past a capability parameter",
                            "only the first data parameter feeds the dot position [F3]",
                        ));
                    }
                    // The F3 receiver fallback (2026-11-22): a free fn
                    // that cannot take the receiver typed through the
                    // method tier, so the checker's dispatch record at
                    // the call span owns the lowering — the native
                    // member seam first, then the resolved method body.
                    let member_routed = native_member.is_some()
                        || self
                            .analyzed
                            .checked
                            .inherent_calls
                            .contains_key(&call_span)
                        || self.analyzed.checked.trait_calls.contains_key(&call_span);
                    if member_routed {
                        if let Some(member) = native_member {
                            (
                                Some(member.def),
                                name.span,
                                std::iter::once(&**recv)
                                    .chain(args.iter().map(|arg| &arg.value))
                                    .collect(),
                                None,
                            )
                        } else {
                            let candidates: Vec<_> = self
                                .analyzed
                                .package
                                .method_candidates(&name.name)
                                .collect();
                            let dispatch = self.resolve_method_dispatch(
                                call_span,
                                name.span,
                                Some(recv),
                                &candidates,
                            )?;
                            (
                                None,
                                name.span,
                                std::iter::once(&**recv)
                                    .chain(args.iter().map(|arg| &arg.value))
                                    .collect(),
                                Some(dispatch),
                            )
                        }
                    } else {
                        (
                            Some(*func),
                            name.span,
                            std::iter::once(&**recv)
                                .chain(args.iter().map(|arg| &arg.value))
                                .collect(),
                            None,
                        )
                    }
                },
                Some(Res::Dot(DotRes::Methods { candidates })) => {
                    if let Some(member) = native_member {
                        (
                            Some(member.def),
                            name.span,
                            std::iter::once(&**recv)
                                .chain(args.iter().map(|arg| &arg.value))
                                .collect(),
                            None,
                        )
                    } else {
                        let dispatch = self.resolve_method_dispatch(
                            call_span,
                            name.span,
                            Some(recv),
                            candidates,
                        )?;
                        (
                            None,
                            name.span,
                            std::iter::once(&**recv)
                                .chain(args.iter().map(|arg| &arg.value))
                                .collect(),
                            Some(dispatch),
                        )
                    }
                },
                // The type-keyed member call: no receiver word stages, so
                // the written arguments alone feed the body; the dispatch
                // resolves through the checker's inherent record like any
                // solved inherent site (a plain local callee — no instance
                // word, no evidence words, no dispatch uop).
                Some(Res::Dot(DotRes::Static { candidates })) => {
                    let dispatch =
                        self.resolve_method_dispatch(call_span, name.span, None, candidates)?;
                    (
                        None,
                        name.span,
                        args.iter().map(|arg| &arg.value).collect(),
                        Some(dispatch),
                    )
                },
                // A module alias's qualified call (G4): the receiver is
                // the alias itself — no word stages — and the dot's
                // recorded def is the linked package's function, a plain
                // def call through the import's link facts.
                Some(Res::Dot(DotRes::Module { func })) => (
                    Some(*func),
                    name.span,
                    args.iter().map(|arg| &arg.value).collect(),
                    None,
                ),
                // An implicit dispatch site (an interpolation hole's
                // `show`, F6): no surface name sits behind the dot, so
                // the resolutions carry nothing here — the checker's
                // span-keyed dispatch records ARE the site's resolution
                // (an assumption-answered hole records only the hole
                // table, its dispatch resolving through the frame's
                // runtime evidence below), and the method-name
                // candidates come from the registry by the synthetic
                // name.
                _ if self.analyzed.checked.trait_calls.contains_key(&call_span)
                    || self
                        .analyzed
                        .checked
                        .inherent_calls
                        .contains_key(&call_span)
                    || self
                        .analyzed
                        .checked
                        .hole_show_dispatches
                        .contains_key(&call_span) =>
                {
                    let candidates: Vec<_> = self
                        .analyzed
                        .package
                        .method_candidates(&name.name)
                        .collect();
                    let dispatch = self.resolve_method_dispatch(
                        call_span,
                        name.span,
                        Some(recv),
                        &candidates,
                    )?;
                    (
                        None,
                        name.span,
                        std::iter::once(&**recv)
                            .chain(args.iter().map(|arg| &arg.value))
                            .collect(),
                        Some(dispatch),
                    )
                },
                _ => {
                    return Err(LowerError::unsupported(
                        name.span,
                        "a method call",
                        "the dot name resolves to neither a free function nor a handler",
                    ))
                },
            }
        } else {
            let Expr::Path { path, span, .. } = &**callee else {
                return Err(LowerError::unsupported(
                    callee.span(),
                    "a computed callee",
                    "only name and dot callees are lowerable this round",
                ));
            };
            // `Trait::f(x)` qualification (F3): the checker records the
            // solved instance at the call's own span like the dot form,
            // and the receiver is the first written argument
            let qualified = match path.segments.as_slice() {
                [.., last] if matches!(self.res.get(last.span), Some(Res::TraitMethod(_))) => {
                    Some(last.span)
                },
                _ => None,
            };
            if let Some(what) = qualified {
                let recv = args.first().map(|arg| &arg.value);
                let dispatch = self.resolve_method_dispatch(call_span, what, recv, &[])?;
                (
                    None,
                    what,
                    args.iter().map(|arg| &arg.value).collect(),
                    Some(dispatch),
                )
            } else if let Some(Res::Def(def)) = self.res.get(*span) {
                (
                    Some(*def),
                    *span,
                    args.iter().map(|arg| &arg.value).collect(),
                    None,
                )
            } else if let Some(Res::Local { bind }) = self.res.get(*span) {
                // a clean check makes this a fn-typed binding: the
                // call dispatches through its Fn word [F2]
                let mut arg_exprs: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
                if let Some(block) = trailing {
                    arg_exprs.push(&**block);
                }
                return self.calls_value(ty, *bind, *span, &arg_exprs, tail);
            } else {
                return Err(LowerError::unsupported(
                    *span,
                    "a callee name",
                    "it does not resolve to a package function",
                ));
            }
        };
        // The block-argument sugar [F7]: the checker fed the parser's
        // zero-parameter trailing block at the next open positional slot,
        // and with named arguments refused above that slot is exactly the
        // written arguments' end, so the block stages below like a
        // written trailing lambda argument — pure sugar, no calling-
        // convention surface of its own.
        if let Some(block) = trailing {
            arg_exprs.push(&**block);
        }
        if case_ctor.is_some() && case_ctor == def {
            return self.case_value(args, ty);
        }
        // A comptime call's answer embeds at the call site (G1): the
        // evaluator ran the callee on the fenced VM and recorded the
        // result on this call's span, so lowering emits it exactly like
        // a written literal and the call never reaches the runtime
        // module. A comptime callee with no recorded answer is the
        // evaluator's contract broken, not a shape the round owes a
        // lowering for — the site refuses naming the seam.
        if let ComptimeRun::Runtime(values) = &self.comptime {
            if let Some(constant) = values.get(call_span) {
                return self.embedded_value(constant, ty, call_span);
            }
            let is_comptime = def.is_some_and(|def| self.comptime_def(def));
            if is_comptime {
                return Err(LowerError::unsupported(
                    call_span,
                    "a comptime call",
                    "the evaluator recorded no answer for this site; run the comptime evaluator \
                     before lowering",
                ));
            }
        }
        // The E4 extraction family inlines at the call site over the
        // checked receiver's own union shape — no callee, no control
        // transfer, word kinds from the call site like every covered
        // generic-safe native.
        if let Some(def) = def {
            if let Some(kind) = self.extraction_def(def) {
                return self.extraction_value(kind, &arg_exprs, ty, callee_span);
            }
            if let Some(place) = self.ambient_str_place(def, &arg_exprs, callee_span)? {
                return Ok(place);
            }
            if self.ambient_extern(def, AmbientExtern::Cancel) {
                return self.cancel_call(&arg_exprs, callee_span);
            }
            if self.ambient_extern(def, AmbientExtern::Die) {
                return self.die_call(&arg_exprs, ty, callee_span);
            }
            if self.ambient_extern(def, AmbientExtern::Exit) {
                return self.exit_call(&arg_exprs, ty, callee_span);
            }
            if self.ambient_extern(def, AmbientExtern::Sort) {
                return self.sort_call(&arg_exprs, ty, call_span, callee_span);
            }
        }
        // a trait-method dispatch names its impl body directly: the call
        // stages and transfers like a local callee, with the solved
        // instance riding as the evidence word after the arguments. An
        // inherent dispatch is a plain local callee — the receiver's own
        // type picked the body, so no instance word exists; a generic
        // body's evidence words stage below like ordinary parameters
        let target = match &trait_dispatch {
            Some(TraitDispatch::Static { index, .. } | TraitDispatch::Inherent { index, .. }) => {
                Callee::Local(*index)
            },
            Some(TraitDispatch::Foreign { position, func, .. }) => Callee::Foreign {
                def: None,
                position: *position,
                func: *func,
            },
            Some(TraitDispatch::Runtime { slot, .. }) => Callee::Evidence(*slot),
            None => {
                self.resolve_callee(def.expect("a named callee resolves to a def"), callee_span)?
            },
        };
        // a member call stages its omitted literal defaults at the call
        // site like the free-fn spelling [F7]: a trait dispatch stages
        // the trait's declared defaults (the declaration is the single
        // authority — call sites check those literals), while an
        // inherent dispatch stages the impl body's own
        let member_defaults: Vec<Expr>;
        let arg_exprs = match (def, &trait_dispatch) {
            (Some(def), _) => self.defaulted_args(def, target, arg_exprs),
            (_, Some(TraitDispatch::Static { call, .. } | TraitDispatch::Foreign { call, .. })) => {
                member_defaults = self.trait_default_exprs(call.method, arg_exprs.len());
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            (_, Some(TraitDispatch::Runtime { method, .. })) => {
                member_defaults = self.trait_default_exprs(*method, arg_exprs.len());
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            (None, Some(TraitDispatch::Inherent { index, .. })) => {
                member_defaults =
                    self.member_default_exprs(*index, arg_exprs.len(), callee_span)?;
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            _ => arg_exprs,
        };
        // A direct `list_push` stages its (list, value) pair with the
        // value's whole word span — the same element guard the literal
        // path applies (a struct element stages its flattened words at
        // the stride the first push fixes). `cons` mirrors it with the
        // element's whole span in the receiver position (the stride-aware
        // prepend's head). `map_set` stages its value the same way (the
        // map value channel's whole span at the value stride the first
        // store fixes) while its key stays one word — the probe
        // vocabulary hashes whole words. `chan_send` stages its payload
        // the same way (the channel's element stride at the hand-off:
        // the receiver's `chan[t]` names `t`, the payload's whole span
        // crossing at the stride the first deposit fixes, the recv's
        // answer the tag-less total of the same words). `reduce`'s init
        // needs no element
        // guard: the checker proves it the fold's own type, so any
        // element that could inhabit the list already passed this guard,
        // and the fold's interior span carries the init's whole width.
        let element_slot = match target {
            Callee::Builtin(native) if native.name == "list_push" => {
                Some((1usize, "a list_push element"))
            },
            Callee::Builtin(native) if native.name == "list_set" => {
                Some((2usize, "a list_set element"))
            },
            Callee::Builtin(native) if native.name == "list_insert" => {
                Some((2usize, "a list_insert element"))
            },
            Callee::Builtin(native) if native.name == "map_set" => {
                Some((2usize, "a map_set value"))
            },
            Callee::Builtin(native) if native.name == "chan_send" => {
                Some((1usize, "a chan_send payload"))
            },
            Callee::Builtin(native) if native.name == "arena_insert" => {
                Some((1usize, "an arena_insert element"))
            },
            Callee::Generic(generic) if generic.name == "cons" => Some((0usize, "a cons element")),
            _ => None,
        };
        // whether the call is a `map_set` whose key argument (position 1)
        // must stage exactly one word
        let map_set_key = matches!(target, Callee::Builtin(native) if native.name == "map_set");
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call)
        let mut staged: Vec<SlotType> = Vec::new();
        // which staged words carry a count at runtime: Ref words, plus
        // the one Raw word of a type-variable value (the VM's RC ops
        // check the kind, so a scalar instantiation is a no-op)
        let mut counted: Vec<bool> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // the written argument each staged word belongs to: a consuming
        // (`move`) parameter's words stage as a hand-off (C2)
        let mut word_arg: Vec<usize> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter (the closure-capable callable surface): the callee
        // reads the word and never owns its count, so the staging is a
        // borrow window, never a dup or a forward
        let mut defaulted: Vec<usize> = Vec::new();
        let default_dispatch = matches!(
            &trait_dispatch,
            Some(TraitDispatch::Foreign {
                default_body: true,
                ..
            })
        );
        let expected_words = self.expected_param_words(target, default_dispatch);
        // an interior-span generic stages one dynamic span between its
        // leading and trailing named words, so a staged closure word's
        // position is dynamic and the callable-defaulted slot is the
        // trailing named word alone
        let interior_span = matches!(target, Callee::Generic(g) if g.staging == Staging::Interior);
        // a linked member dispatch's solved parameter types, from the
        // checker's per-site record: the registered member signature
        // rides the decls table keyed by its method id, never a def
        // whose fn type the free-fn seam reads, so the staging below
        // reads the call's own solved record instead
        let member_params: Option<&[Ty]> = match target {
            Callee::Foreign { def: None, .. } => self
                .analyzed
                .checked
                .member_call_params
                .get(&call_span)
                .map(Vec::as_slice),
            _ => None,
        };
        // a local callee's per-parameter checked types, when its
        // signature is static: a union-typed parameter re-spans a bare
        // member or a strict sub-union argument into its whole region
        // here (the widening family)
        let param_types = match target {
            Callee::Local(index) => self.callee_param_types(index),
            Callee::Foreign { def: Some(def), .. } => self
                .analyzed
                .checked
                .def_types
                .get(&def)
                .copied()
                .and_then(|fn_ty| fn_sig_of(&self.analyzed.decls.universe, fn_ty))
                .map(|(params, _)| params.to_vec()),
            Callee::Foreign { def: None, .. } => member_params.map(<[Ty]>::to_vec),
            _ => None,
        };
        // whether each written argument position feeds a consuming
        // (`move`) parameter: the staging then forwards the source's
        // count and any deinit claim — the checker's forced move makes
        // the death test true at exactly this use (C2/C1)
        let consuming = self.consuming_args(target, trait_dispatch.as_ref(), arg_exprs.len());
        // the receiver's checked type mints a generic call's evidence
        // words (the builtin instance it addresses) [H3]
        let mut receiver_ty: Option<Ty> = None;
        // a `push`/`set` member call's receiver place: the native window
        // mutates it in place, and the member re-answers a duplicate of
        // its word after the call
        let window_answer = native_member.is_some_and(|member| member.window);
        let mut window_receiver: Option<TypedPlace> = None;
        // the call's borrow window opens before operand evaluation: a
        // later operand's own use can fire an earlier word's last-use
        // dec there, and the window pins every word this CALL reads
        // (native borrows never dup), deferring the dec to the flush
        // after the CALL
        self.rc.native_open();
        let mut consts: Vec<(usize, ScalarConst)> = Vec::new();
        for (index, arg) in arg_exprs.iter().enumerate() {
            let param_ty = param_types
                .as_ref()
                .and_then(|types| types.get(index).copied());
            // an element-slot argument stages its channel's whole
            // element span, so the direct staging paths read the
            // channel's element type as the position's parameter type:
            // a union element suppresses them exactly a union
            // parameter's own staging does (a bare literal would
            // otherwise stage one word where the element stride needs
            // the member's whole region)
            let element_ty = match element_slot {
                Some((slot, _)) if slot == index => self.element_slot_ty(&target, receiver_ty),
                _ => None,
            };
            // A literal scalar argument stages its constant directly on
            // its overlay word. A tail transfer defers the staging past
            // the exit claims, so it keeps the evaluated temp. A word
            // whose callee-side formal declares Raw (a default body's
            // `Self` receiver) records the wildcard kind, mirroring the
            // moved-word path below — the ABI slide admits the pairing
            // whatever the literal's own static kind.
            if !tail
                && let Some((slot, constant)) = self.direct_literal(arg, element_ty.or(param_ty))?
            {
                if index == 0 {
                    receiver_ty = Some(self.val(arg.span())?.ty);
                }
                let expected_raw = expected_words
                    .as_deref()
                    .is_some_and(|words| words.get(staged.len()) == Some(&SlotType::Raw));
                let at = staged.len();
                staged.push(if expected_raw { SlotType::Raw } else { slot });
                counted.push(false);
                word_arg.push(index);
                consts.push((at, constant));
                continue;
            }
            // A single-use computed scalar argument writes its overlay
            // word directly: the offer goes to the argument's outermost
            // data op, taken only when every later argument evaluates
            // without touching the overlay.
            let offered = if !tail
                && let Some(slot) =
                    self.direct_compute(arg, element_ty.or(param_ty), &arg_exprs[index + 1..])?
            {
                self.scalar_dst = Some(ScalarDst::Arg {
                    index: u8::try_from(staged.len()).expect("overlay word fits u8"),
                    slot,
                });
                true
            } else {
                false
            };
            let arg_val = self.value(arg)?;
            let direct = offered && self.scalar_dst.take().is_none();
            if index == 0 {
                receiver_ty = Some(arg_val.ty);
                if window_answer {
                    window_receiver = Some(arg_val.clone());
                }
            }
            let deinit = self.deinit_claim(arg_val.ty, arg.span())?;
            if deinit.is_some() {
                // A channel payload names its own refusal seam: the buffer's
                // edge retention owns counts, never claims, so the payload's
                // hook has no door at either wait-ending edge — a buffered
                // element the channel never delivers dies at the send with
                // the sender's claim spent, and a parked sender cut with
                // its span queued fires no hook either way
                if let Some((slot, _)) = element_slot
                    && slot == index
                    && matches!(target, Callee::Builtin(native) if native.name == "chan_send")
                {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying channel payload",
                        "the payload's hook has no claim inside the channel's edge retention; \
                         per-span deinit claims own the seam",
                    ));
                }
                // An arena element names its own refusal seam: the slot's
                // edge retention owns counts exactly a channel buffer's,
                // so the element's hook has no door at the remove or the
                // arena's death
                if let Some((slot, _)) = element_slot
                    && slot == index
                    && matches!(target, Callee::Builtin(native) if native.name == "arena_insert")
                {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying arena element",
                        "the element's hook has no claim inside the arena's edge retention; \
                         per-span deinit claims own the seam",
                    ));
                }
                if matches!(
                    target,
                    Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. }
                ) {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying argument",
                        "a native call borrows staged words with no drop position of their own; \
                         the value's deinit runs only at its binding's death",
                    ));
                }
                if tail
                    && matches!(target, Callee::Local(_) | Callee::Foreign { .. })
                    && !consuming[index]
                {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying argument in tail position",
                        "the tail call reuses this frame, so the callee's borrow would outlive \
                         the frame that owns the value's deinit; keep the call out of tail \
                         position",
                    ));
                }
            }
            let mut arg_val = arg_val;
            if let Some((slot, what)) = element_slot
                && slot == index
            {
                // A bare member value (a case construction, a
                // narrowed binding) re-spans into the receiver's
                // element or value span first — the generic callee
                // has no static parameter type to drive the widening
                // family, and the staged words must be the whole
                // span the channel carries
                let channel_ty = self.element_slot_ty(&target, receiver_ty);
                if let Some(channel_ty) = channel_ty.filter(|ty| *ty != arg_val.ty) {
                    let temp = self.temp_span(channel_ty)?;
                    self.widen_into(&temp, channel_ty, &arg_val, arg.span())?;
                    arg_val = TypedPlace {
                        place: temp,
                        ty: channel_ty,
                    };
                }
                self.guard_list_element(&arg_val, arg.span(), what)?;
                // an arena element stages exactly one word this round:
                // the slot array stores one element word per handle, so
                // a wider element (a multi-word record or tuple, a
                // union's region) refuses at the staging with the
                // per-instantiation seam its own residual
                if matches!(target, Callee::Builtin(native) if native.name == "arena_insert") {
                    let addrs = self.word_addrs(&arg_val.place, arg_val.ty)?;
                    if addrs.len() != 1 {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "an arena_insert element",
                            "arena elements lower as one machine word per slot this round; a \
                             wider element waits on the per-instantiation element-span seam",
                        ));
                    }
                }
            }
            // a map_set's key stages exactly one word: the probe
            // resolution and the row index hash whole words, so a wider
            // key type has no channel yet
            if map_set_key && index == 1 {
                let addrs = self.word_addrs(&arg_val.place, arg_val.ty)?;
                if addrs.len() != 1 {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a map_set key",
                        "map keys lower as one machine word (the probe vocabulary hashes whole \
                         words); wider keys arrive when the native channel carries key spans",
                    ));
                }
            }
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter; its one Ref word marks the borrow-window index.
            // An interior-span generic's callable word rides at the end
            // of the staged span, so the static expected list cannot
            // name its position there
            if self.is_closure_word(&arg_val.place, arg_val.ty)? {
                if interior_span {
                    Self::guard_interior_closure_slot(
                        expected_words.as_deref(),
                        index + 1 == arg_exprs.len(),
                        arg.span(),
                    )?;
                } else {
                    Self::guard_callable_staged(
                        expected_words.as_deref(),
                        staged.len(),
                        arg.span(),
                    )?;
                }
                defaulted.push(staged.len());
            }
            // a union-typed parameter takes its member's whole region: a
            // bare member (a literal or an un-annotated binding), a
            // strict sub-union argument, or an equal-typed argument
            // whose place keeps a wider union's physical layout (an
            // is-narrowed binder over the answer union) re-spans into a
            // staging temp of the parameter's own region here, mirroring
            // the annotated binding's widening write, so the staged
            // words match the callee's parameter span
            let arg_val = match param_types.as_ref().and_then(|types| types.get(index)) {
                Some(&param_ty)
                    if self.is_union_ty(param_ty)?
                        && (param_ty != arg_val.ty
                            || !self.place_layout_matches(&arg_val.place, param_ty)?) =>
                {
                    // a union member cannot carry a deinit yet (the
                    // callee's own parameter shapes refuse first), so
                    // this guard is the staging temp's backstop for the
                    // deinit arc's landing
                    if deinit.is_some() {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument into a union parameter",
                            "the re-span's staging temp is anonymous, so it carries no claim \
                             contract of its own; the deinit arc owns the seam",
                        ));
                    }
                    let temp = self.temp_span(param_ty)?;
                    self.widen_into(&temp, param_ty, &arg_val, arg.span())?;
                    TypedPlace {
                        place: temp,
                        ty: param_ty,
                    }
                },
                _ => arg_val,
            };
            let var_word = self.is_var_ty(arg_val.ty);
            // a free generic fn's parameter over its own type variable
            // claims the argument as the variable's single Raw word, so
            // an argument spanning more than one word refuses here —
            // the one-word instantiation boundary, at the site where
            // the type is concrete
            let var_param = param_ty.is_some_and(|param| self.is_var_ty(param));
            if var_param {
                self.guard_one_word_instantiation(
                    self.members(arg_val.ty),
                    "a generic call's argument",
                    arg.span(),
                )?;
            }
            if direct {
                // the argument's outermost data op wrote its overlay
                // word: the staging records the word's kind and the
                // argument's ownership; no Mov and no window read ride
                // it (a scalar word carries no count). A Raw-declaring
                // callee formal takes the wildcard kind, exactly like
                // the literal path above and the moved-word path below
                let PlaceShape::Word(slot) = arg_val.place.shape else {
                    return Err(mismatch("a directly staged scalar argument"));
                };
                let expected_raw = expected_words
                    .as_deref()
                    .is_some_and(|words| words.get(staged.len()) == Some(&SlotType::Raw));
                staged.push(if expected_raw { SlotType::Raw } else { slot });
                counted.push(false);
                word_arg.push(index);
            } else {
                let addrs = self
                    .word_addrs(&arg_val.place, arg_val.ty)
                    .map_err(|err| err.respanned(arg.span()))?;
                for addr in addrs {
                    self.rc.native_reads([addr.slot]);
                    moves.push((staged.len(), addr.slot));
                    // the variable's single word stages as the wildcard, and
                    // so does any word whose callee-side formal declares Raw
                    // (a multi-word parameter over type variables, the
                    // argument's own words carrying their concrete kinds):
                    // the overlay word records Raw (the runtime kind is the
                    // callee's fact), so the ABI slide admits the pairing
                    // whatever the source's static kind — a scalar staged at
                    // an overlay index no other type visits would otherwise
                    // fail the slide's static check against the Raw parameter.
                    // A Raw-classified staged word always records the count
                    // hand-off: the RC ops check the runtime kind, so a
                    // scalar instantiation is a no-op (a variable-keyed union
                    // member's region claim holds a counted reference under a
                    // reference substitution)
                    let expected_raw = expected_words
                        .as_deref()
                        .is_some_and(|words| words.get(staged.len()) == Some(&SlotType::Raw));
                    staged.push(if var_param || expected_raw {
                        SlotType::Raw
                    } else {
                        addr.slot_ty
                    });
                    counted.push(matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) || var_word);
                    word_arg.push(index);
                }
            }
        }
        // a closure word in a callable-defaulted parameter has no CALLT
        // path: the transfer leaves no post-call position for the borrow
        // flush, and the pre-transfer exit drops would free the object
        // under the callee's read. Its tail lowers in place instead (CALL
        // plus flush plus return, the R13 native-tail shape) —
        // stack-identical and count-exact; the frame-reuse guarantee
        // [H2] keeps every scalar-staging tail site, a linked callee's
        // included
        let closure_tail = !defaulted.is_empty()
            && tail
            && matches!(target, Callee::Local(_) | Callee::Foreign { .. });
        // a trait-method tail transfers through CALLTG now (the dispatch
        // tail form, H2's guarantee closure): a Static site's evidence and
        // instance words and a Runtime site's sentinel FuncRef ride the
        // same frame-reuse slide a local tail's arguments take, staged
        // after the exit claims. An inherent dispatch stays excluded — it
        // is a plain local callee taking the CALLT transfer below
        let native_tail_return = tail
            && !self.root
            && matches!(
                target,
                Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. }
            );
        // a closure-staging local tail lowers in place for the root frame
        // too: the callee reads the word during the call, so the count
        // cannot ride the abandoned corner nor die at the pre-transfer
        // exit drops
        // a linked package's tail call takes the same CALLT frame-reuse
        // transfer a local's does (H2): the pool FuncRef carries the
        // module boundary, the slide a pure fact of the two layouts, so
        // the transfer crosses the dsmod seam with the stack flat
        // a collection-mutating member (`push`/`set`) answers the receiver
        // after the native's unit word, so it always lowers in place: the
        // transfer would hand the caller the unit word where the member's
        // answer is the collection
        let in_place_tail = native_tail_return || closure_tail || (tail && window_answer);
        // the frame-ending transfer: the exit drops and every claim hook
        // stage through the overlay as their own argument windows, so the
        // call's argument span stages after them, ahead of the CALLT
        let tail_transfer = tail && !in_place_tail;
        self.guard_staged_count(staged.len())?;
        // the literal arguments' constant loads land on their overlay
        // words at the staging position, in staged order
        for &(at, constant) in &consts {
            self.scalar_const_into(
                constant,
                Slot::overlay(u8::try_from(at).expect("overlay word fits u8")),
            );
        }
        for &(index, slot) in &moves {
            if !tail_transfer {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    src: Slot::body(slot),
                });
            }
            // a staged word of a consuming (`move`) parameter: the callee
            // owns the value's death, so the count and any deinit claim
            // ride the staging as a hand-off — the checker's forced move
            // spent the source's death at exactly this use. A plain
            // count keeps the dup-or-forward death test outside the tail
            // transfer (a live copyable source dups), while a word a
            // claim covers always rides (a borrowed inc would leave the
            // caller's hook armed beside the callee's own)
            let consumes = word_arg
                .get(index)
                .is_some_and(|&arg| consuming.get(arg).copied().unwrap_or(false));
            // a staged Ref: a local callee's borrow parameter owns the
            // handed word (forward on the source's death, dup otherwise);
            // a native is a transient borrow window that never dups. A
            // tail-position staging abandons only when the frame's tail
            // transfer really ends the program (the root frame's named
            // corner); a non-root frame lowers its native tail call in
            // place below, so its staged borrows stay pending for the
            // post-call flush. A callable-defaulted closure word is a
            // borrow window against every callee kind: the callee reads it
            // (CALLS never consumes) and never owns its count, and a
            // closure-staging tail lowers in place below, so its flush
            // always has a post-call position
            if counted[index] {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
                } else if window_answer && word_arg.get(index) == Some(&0) {
                    // a collection-mutating member's receiver: the
                    // member re-answers a duplicate of the word after
                    // the call, so the window's flush never decs a
                    // dying receiver — the answer's own count keeps
                    // the object
                    self.rc.staged_native_keep(slot);
                } else {
                    match target {
                        Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. } => {
                            self.rc.staged_native(slot, tail && self.root);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. }
                            if tail_transfer && consumes =>
                        {
                            self.rc.staged_move(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. }
                            if tail_transfer =>
                        {
                            self.rc.staged_tail_local(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. }
                            if consumes && self.rc.claim_base_of(slot).is_some() =>
                        {
                            self.rc.staged_move(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. } => {
                            self.rc.staged_local(&mut self.code, slot);
                        },
                    }
                }
            } else if consumes && self.rc.claim_base_of(slot).is_some() {
                // a consuming scalar claim anchor (no Ref count of its
                // own): the hook rides the staging silently, or this
                // frame's scope end would fire it beside the callee's
                self.rc.staged_move(&mut self.code, slot);
            }
        }
        self.guard_staged_width(target, &staged, callee_span, member_params)?;
        if let Callee::Builtin(native) = target
            && matches!(
                native.shape,
                NativeShape::Fallible { .. } | NativeShape::Checked
            )
        {
            return self.native_span_result(native, ty, tail, callee_span, &staged);
        }
        // a generic call site [H3]: stage the instantiated types as
        // evidence words after the real arguments and dispatch the
        // callee's ONE body through CALLG — or, from the root frame's
        // tail position, through the CALLTG transfer (a non-root frame
        // lowers the native tail in place below, the R13 shape, because
        // the body runs inside the instruction and no frame is pushed)
        let generic_target = match target {
            Callee::Generic(generic) => Some(generic),
            _ => None,
        };
        let mut generic_instance: Option<BuiltinInstance> = None;
        let generic_tail_at = staged.len();
        if let Some(generic) = generic_target {
            if tail && matches!(generic.shape, NativeShape::Checked) {
                return Err(LowerError::unsupported(
                    callee_span,
                    "a tail call to a checked-read generic",
                    "the ok/none span re-spans into the call's union at the call site, and the \
                     tail transfer leaves no position for it; keep the call out of tail position",
                ));
            }
            generic_instance = Some(self.stage_generic_evidence(
                generic,
                def.expect("a generic callee resolved to a def"),
                callee_span,
                *span,
                &mut staged,
                receiver_ty,
                tail_transfer,
                native_member.is_some(),
            )?);
        }
        // a trait-method dispatch site [H3]: a call into a generic impl
        // body stages the runtime-evidence words its substitution
        // resolves (one evidence-chain object index per demand, in body
        // order) and then the solved declared-instance word; a call into
        // a monomorphic body stages the instance word alone. A nested
        // bound-variable site stages none of these — its slot word rides
        // below — and a nested site whose serving body is itself generic
        // stages the callee's words from this frame's own evidence area.
        // The staged types register here (the overlay span sizes from
        // them); on the tail path the stores defer past the exit claims,
        // staged after them like every tail-staged word (a claim's
        // overlay window would clobber words staged before it)
        let mut trait_evidence: Vec<EvidenceSrc> = Vec::new();
        let trait_evidence_at = staged.len();
        if let Some(TraitDispatch::Foreign { call, .. }) = &trait_dispatch {
            // a linked body rides no evidence pairs (the walk refuses
            // the generic and conditional impls that demand them, and a
            // trait's default body crosses only when it stages none),
            // so the solved instance word rides alone — the same
            // trailing word a local monomorphic body's site stages.
            // A default body is the exception on the receiver: its
            // `Self` variable claims the receiver as its single Raw
            // word, so a receiver spanning more than one word keeps
            // the local one-word instantiation boundary's own refusal,
            // mirrored at the linked site
            if default_dispatch {
                self.guard_one_word_instantiation(
                    self.members(call.recv),
                    "a linked default body's receiver",
                    callee_span,
                )?;
            }
            self.guard_staged_count(staged.len() + 1)?;
            trait_evidence.push(EvidenceSrc::Const(call.instance.index()));
            staged.push(SlotType::I64);
            if !tail_transfer {
                self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            }
        }
        if let Some(TraitDispatch::Static { index, call }) = &trait_dispatch {
            let pairs = self.plan.functions()[*index as usize].evidence.clone();
            self.guard_staged_count(staged.len() + pairs.len() + 1)?;
            for pair in pairs.iter() {
                let words = if self.plan.is_default_body(*index) {
                    // the entered body is the trait's declared default:
                    // every demand keys by the trait's own variables —
                    // `Self` names the receiver (the chain starts at its
                    // member set under the site's own solved instance),
                    // every other names a trait generic the impl behind
                    // that instance substitutes through its trait
                    // reference — the declared bounds record answers
                    // neither shape
                    vec![EvidenceSrc::Const(self.default_pair_chain(
                        call.method,
                        call.instance,
                        Some(self.members(call.recv)),
                        pair,
                        "a trait-method call into a declared default body",
                        callee_span,
                    )?)]
                } else {
                    self.evidence_words(
                        pair,
                        &call.bounds,
                        Some(call.instance),
                        None,
                        Some(call.recv),
                        "a trait-method call into a generic impl body",
                        callee_span,
                    )?
                };
                trait_evidence.extend(words);
            }
            // The instance word is a per-branch id: a union-receiver
            // dispatch stages the representative branch of the covering
            // group, whose written head names the one impl item the word
            // stands for (the inline-cache story keys on the written
            // head, never on this branch id).
            trait_evidence.push(EvidenceSrc::Const(call.instance.index()));
            for _ in 0..=pairs.len() {
                staged.push(SlotType::I64);
            }
            if !tail_transfer {
                self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            }
        }
        // a generic inherent member call stages the same per-pair
        // runtime-evidence words the trait path stages, minus the
        // instance word an inherent impl never has: the words trail the
        // staged arguments in the overlay, and the body claims them as
        // its trailing evidence parameters, so the ordinary CALL below —
        // and a tail site's CALLT — names the whole staged count; no
        // dispatch uop exists. The staged types register here (the
        // overlay span sizes from them), while the stores defer past the
        // exit claims on the tail path (a claim's overlay window would
        // clobber words staged before it)
        let mut member_evidence: Vec<EvidenceSrc> = Vec::new();
        let member_evidence_at = staged.len();
        if let Some(TraitDispatch::Inherent {
            index,
            bounds,
            method,
        }) = &trait_dispatch
        {
            let pairs = self.plan.functions()[*index as usize].evidence.clone();
            if !pairs.is_empty() {
                self.guard_staged_count(staged.len() + pairs.len())?;
                let inherent_site = self.analyzed.decls.inherent_impls.get(method).copied();
                for pair in pairs.iter() {
                    let words = self.evidence_words(
                        pair,
                        bounds,
                        None,
                        inherent_site,
                        receiver_ty,
                        "an inherent method call into a generic impl body",
                        callee_span,
                    )?;
                    member_evidence.extend(words);
                }
                for _ in 0..pairs.len() {
                    staged.push(SlotType::I64);
                }
                if !tail_transfer {
                    self.stage_member_evidence(&member_evidence, member_evidence_at);
                }
            }
        }
        // a free generic fn's call site [H3]: the callee's ONE body
        // claims its runtime-evidence demands as trailing parameters,
        // so the entering site stages them after the true arguments in
        // this outgoing-argument area — one evidence-chain object index
        // per demand, the checker's per-call record answering each bound
        // under this site's own substitution. The ordinary CALL below (a
        // tail site's CALLT) names the whole staged count, like the
        // inherent generic body's entering site. The staged types
        // register here (the overlay span sizes from them), while the
        // stores defer past the exit claims on the tail path (a claim's
        // overlay window would clobber words staged before it)
        let mut free_evidence: Vec<EvidenceSrc> = Vec::new();
        let free_evidence_at = staged.len();
        if let Callee::Local(index) = target {
            let pairs = self.plan.functions()[index as usize].evidence.clone();
            // a dispatch target (a trait's declared default body) stages
            // its evidence words through the dispatch branch above; this
            // free-fn staging serves named callees alone
            if trait_dispatch.is_none()
                && self.plan.functions()[index as usize].free_generic
                && !pairs.is_empty()
            {
                let Some(record) = self
                    .analyzed
                    .checked
                    .generic_call_bounds
                    .get(&call_span)
                    .cloned()
                else {
                    return Err(LowerError::unsupported(
                        callee_span,
                        "a call into a free generic fn's body",
                        "the checker recorded no bound answer at this site; report the construct",
                    ));
                };
                self.guard_staged_count(staged.len() + pairs.len())?;
                for pair in pairs.iter() {
                    let words = self.free_evidence_words(
                        pair,
                        &record,
                        "a call into a free generic fn's body",
                        callee_span,
                    )?;
                    free_evidence.extend(words);
                }
                for _ in 0..pairs.len() {
                    staged.push(SlotType::I64);
                }
                if !tail_transfer {
                    self.stage_member_evidence(&free_evidence, free_evidence_at);
                }
            }
        }
        // a nested runtime-resolved site: the target rides the frame's
        // evidence slot, whose word names an evidence-chain object. The
        // DISPATCH stages the chain's tail objects' indices as the
        // serving body's trailing evidence parameters (a static table
        // read per hop), so this site stages no words itself — it only
        // reserves the overlay: the runtime-resolved target's demand
        // count is bounded by the widest planned body's, and the
        // dispatch writes exactly what its chain object carries
        let mut runtime_riders = 0usize;
        if let Some(TraitDispatch::Runtime { .. }) = &trait_dispatch {
            runtime_riders = self.plan.max_evidence_pairs();
            self.guard_staged_count(staged.len() + runtime_riders)?;
            for _ in 0..runtime_riders {
                staged.push(SlotType::I64);
            }
        }
        // a checked generic read (`list_get`, `map_get`): the staged span
        // ends in its evidence words and the ok/none span re-spans
        // through the checked path, which emits the CALLG with the
        // real-argument count
        if let Callee::Generic(generic) = target
            && matches!(generic.shape, NativeShape::Checked)
        {
            let real_args = staged.len() - usize::from(generic.evidence);
            return self.checked_native_result(target, ty, &staged, real_args);
        }
        let result_words = self
            .callee_result_words(target, generic_instance, ty)
            .map_err(|err| err.respanned(callee_span))?;
        self.overlay_site(&staged, &result_words);
        let kidx = self.pool(self.funcref_const(target));
        // A native tail call transfers control only from the root frame:
        // CALLT-to-native (and CALLTG-to-generic) pops the frame inside
        // the instruction, so the staged borrows' decs have no code
        // position there — sound solely because the root frame's transfer
        // ends the program (the heap dies whole). From any other frame the
        // same tail position lowers in place: CALL + RET is
        // stack-identical (the native runs inside the CALL instruction,
        // no frame is pushed), and the post-call flush plus the return's
        // exit drops balance every count.
        // a dispatch tail (a trait site, or the root frame's generic
        // call) transfers through CALLTG: the dispatch resolution is the
        // CALLG emission's own and the transfer is the CALLT slide's, so
        // the guarantee admits no callee-class exception [H2]
        let dispatch_tail = generic_target.is_some()
            || matches!(
                trait_dispatch,
                Some(
                    TraitDispatch::Static { .. }
                        | TraitDispatch::Runtime { .. }
                        | TraitDispatch::Foreign { .. }
                )
            );
        if tail_transfer {
            // control transfers to the callee; the stack never grows [H2].
            // A local or linked callee owns its handed words (their counts
            // ride the transfer, its own exit drops them); every other
            // owned reference drops before the transfer, and a root-frame
            // native's staged borrows are the abandoned corner above. The
            // exit drops and every claim hook staged through the overlay
            // as their own argument windows, so the call's argument span
            // and the dispatch tail's evidence words stage after them —
            // the transfer hands the callee its words intact.
            self.rc.native_tail_transfer();
            self.rc.exit(&mut self.code, &[]);
            self.stage_member_evidence(&member_evidence, member_evidence_at);
            self.stage_member_evidence(&free_evidence, free_evidence_at);
            if let (Some(generic), Some(instance)) = (generic_target, generic_instance) {
                // the deferred evidence stores: one instance word per
                // constraint-list entry, staged after the exit claims
                let word = u32::try_from(instance as i64).expect("instance ids fit u32");
                let words = vec![EvidenceSrc::Const(word); usize::from(generic.evidence)];
                self.stage_member_evidence(&words, generic_tail_at);
            }
            self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            self.stage_moves(&moves);
            if dispatch_tail {
                // the dispatch tail's uop names the real arguments; the
                // evidence/instance words trailing them ride the transfer
                let real_args = if let Some(generic) = generic_target {
                    staged.len() - usize::from(generic.evidence)
                } else if let Some(TraitDispatch::Static { index, .. }) = &trait_dispatch {
                    staged.len() - 1 - self.plan.functions()[*index as usize].evidence.len()
                } else if matches!(trait_dispatch, Some(TraitDispatch::Foreign { .. })) {
                    staged.len() - 1
                } else {
                    staged.len() - runtime_riders
                };
                self.emit(Op::Calltg {
                    callee: kidx,
                    nargs: u8::try_from(real_args).expect("argument count fits u8"),
                });
            } else {
                self.emit(Op::Callt {
                    callee: kidx,
                    nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
                });
            }
            // an unreachable marker place; no code follows a tail call
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        // the staged span of a generic call ends in its evidence words;
        // nargs names the real arguments, and the callee's own constraint
        // list carries the count the dispatch reads. A trait-method call
        // stages the same shape: one instance evidence word trails the
        // arguments, and the impl body the FuncRef names runs to
        // completion inside the instruction
        if generic_target.is_some() {
            let real_args = staged.len() - usize::from(generic_target.map_or(0, |g| g.evidence));
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if let Some(TraitDispatch::Static { index, .. }) = &trait_dispatch {
            // the evidence words trail the real arguments (one per the
            // body's runtime demands, then the solved instance), so the
            // uop names the real-argument count and the callee claims
            // the rest as its trailing evidence parameters
            let real_args =
                staged.len() - 1 - self.plan.functions()[*index as usize].evidence.len();
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if matches!(trait_dispatch, Some(TraitDispatch::Foreign { .. })) {
            // the linked body claims no evidence pairs this round, so
            // only the solved instance word trails the real arguments
            let real_args = staged.len() - 1;
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if matches!(trait_dispatch, Some(TraitDispatch::Runtime { .. })) {
            // a nested runtime-resolved site: the sentinel FuncRef names
            // the frame's evidence slot and dispatches from it; the
            // chain's tail words trail the real arguments for the
            // serving body the slot may name, staged by the dispatch
            // itself from the chain object
            let real_args = staged.len() - runtime_riders;
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else {
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
            });
        }
        self.rc.native_done(&mut self.code);
        let place = if let Some(recv) = window_receiver {
            // a collection-mutating member (`xs.push(x)`, `m.set(k, v)`)
            // answers the receiver: the native window appended in place
            // and answered the unit word on the overlay, so the member's
            // result is a duplicate of the receiver's own word
            let word = single(
                &self
                    .word_addrs(&recv.place, recv.ty)
                    .map_err(|err| err.respanned(callee_span))?,
                callee_span,
            )?;
            let base = self.alloc(&[SlotType::Ref])?;
            self.emit(Op::Mov {
                dst: Slot::body(base),
                src: Slot::body(word.slot),
            });
            self.rc.duped(&mut self.code, base, word.slot);
            Place {
                base,
                shape: PlaceShape::Word(SlotType::Ref),
            }
        } else {
            let place = self.result_place(ty, &result_words, callee_span)?;
            // a known-function callee's fn-typed result is the scalar its own
            // provenance proved (a factory's result classifies Ref instead),
            // so the result word joins the provenance oracle and widens into
            // a re-storable slot like a named fn value
            if *result_words == [SlotType::Fn] && matches!(target, Callee::Local(_)) {
                self.scalar_fn_words.insert(place.base);
            }
            place
        };
        if in_place_tail {
            // the in-place tail call (a non-root native or generic tail,
            // or a local tail staging a closure into a callable-defaulted
            // parameter): the tail position returns the call's result,
            // the post-call flush settles the borrow windows, and the
            // return's exit drops flush every other count the frame owes
            self.emit_return(&place, ty, e.span())?;
            // an unreachable marker place; no code follows the return
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        Ok(place)
    }

    /// Stages one call's argument span: the recorded body words move into
    /// the overlay in argument order.
    fn stage_moves(&mut self, moves: &[(usize, u8)]) {
        for &(index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
        }
    }

    /// Copies one call's result span out of the overlay into a fresh temp:
    /// the callee transferred its result references out with the return.
    pub(super) fn result_place(
        &mut self,
        ty: Ty,
        result_words: &[SlotType],
        span: Span,
    ) -> Result<Place, LowerError> {
        let base = self.alloc(result_words)?;
        for (index, slot) in result_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("result width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(index).expect("result word fits u8")),
            });
            // a Raw result word is a type variable's runtime word: the
            // callee transferred its count, and the kind-checked drop
            // balances it when the value is a reference
            if *slot == SlotType::Ref || *slot == SlotType::Raw {
                self.rc.produced(word);
            }
        }
        // a deinit-carrying result owns its hook wherever it lands (C5):
        // the temp holds the value until a binder aliases or discards it
        if let Some(claim) = self.deinit_claim(ty, span)? {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let words = self.ref_words(base, ty)?;
            let band = self.claim_band_word(base)?;
            self.rc
                .claimed(&mut self.code, base, claim.hook, band, &words);
        }
        let shape = match &*self.shapes.shape(ty)? {
            // the copy-out word's own kind leads: a factory's fn-typed
            // result classifies Ref (the closure its tail constructs),
            // so the place names the physical word the call slide
            // validated — a Fn classification would hide the count from
            // every use and the binder
            Shape::Word(slot) => PlaceShape::Word(result_words.first().copied().unwrap_or(*slot)),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    /// CALLS (the callable-word call form, F2): the callee is a local
    /// binding whose single word names the function — a scalar Fn word,
    /// or a Ref word naming a closure object (the read dispatches on the
    /// word's kind; the emitted op is the same). Arguments stage on the
    /// overlay exactly like a pool call, the callee frame overlays it,
    /// and the result copies out of the overlay. A tail position runs
    /// the maybe-Ref callee tracking [H2's closure ruling]: the callee
    /// word is maybe-Ref by classification, and the provenance tables
    /// (`scalar_fn_words` naming the function through `fn_words`) prove
    /// some words count-free scalars naming exactly one planned function
    /// — those tails transfer through CALLT with that function's own
    /// pool FuncRef, and every unprovable word (a closure's Ref, a
    /// callable-defaulted parameter, an unproven alias) keeps the
    /// in-place CALLS plus return.
    fn calls_value(
        &mut self,
        ty: Ty,
        bind: Span,
        callee_span: Span,
        arg_exprs: &[&Expr],
        tail: bool,
    ) -> Result<Place, LowerError> {
        let callee_ty = self.val(callee_span)?.ty;
        let Some((params, ret)) = fn_sig_of(&self.analyzed.decls.universe, callee_ty) else {
            return Err(LowerError::unsupported(
                callee_span,
                "a callable callee",
                "its checked type is not a function",
            ));
        };
        // the callee use is a read of the binder: a closure word's death
        // test compares against this span
        self.rc.use_of(bind, callee_span);
        let place = self
            .binders
            .get(&bind)
            .cloned()
            .ok_or_else(|| self.missing_binder(callee_span))?;
        // the callee word: a Fn-classified slot directly (a scalar word or
        // a callable-defaulted parameter), a Ref-classified value-site
        // word (CALLS reads the FuncRef behind the closure object's
        // header), or the payload word of a union span an extraction
        // handler's parameter aliases
        let word = single(&self.word_addrs(&place, callee_ty)?, callee_span)?;
        let closure = word.slot_ty == SlotType::Ref;
        // the maybe-Ref callee tracking: a proven scalar word is
        // count-free by construction and names exactly one planned
        // function, so a tail call through it can take the CALLT
        // transfer; every other word stays behind the runtime CALLS read
        let proven_fn = self
            .scalar_fn_words
            .contains(&word.slot)
            .then(|| self.fn_words.get(&word.slot).copied())
            .flatten();
        let mut expected_words: Vec<SlotType> = Vec::with_capacity(params.len());
        for ty in params {
            expected_words.extend_from_slice(&self.shapes.words(*ty)?);
        }
        let expected = expected_words.len();
        // the planned body the callee word names, when this frame
        // constructed it (a scalar Fn word or a closure object): its own
        // parameter modes stay readable, so a deinit-carrying argument
        // can be checked against its contract. A word this frame did not
        // construct (a fn-typed parameter's) erases them.
        let callee_index = self.fn_words.get(&word.slot).copied();
        let param_modes = callee_index.and_then(|index| self.planned_param_modes(index));
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call).
        // The borrow window opens before that evaluation and pins the
        // callee word (CALLS reads it during the call) plus every word
        // the staged arguments resolve to
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // per staged word, whether it carries a count to the hand-off: a
        // Ref word always, and the frame's own type-variable word — its
        // single Raw word may hold a counted reference at runtime (the
        // evidence-carrying closure's variable parameter dispatches into
        // Ref-claiming bodies), and the kind-checked inc/dec pair makes
        // the runtime fact safe to stage blind
        let mut counted: Vec<bool> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter: the callee reads the word and never owns its count
        let mut defaulted: Vec<usize> = Vec::new();
        // a deinit-carrying argument pins its hook to this frame's death
        // sites, so a tail through the callable word lowers in place
        // (the callee's borrow would outlive a transferred frame)
        let mut deinit_arg = false;
        self.rc.native_open();
        self.rc.native_reads([word.slot]);
        let mut consts: Vec<(usize, ScalarConst)> = Vec::new();
        for (index, &arg) in arg_exprs.iter().enumerate() {
            let param_ty = params.get(index).copied();
            // A literal scalar argument stages its constant directly on
            // its overlay word. A tail transfer defers the staging past
            // the exit claims, so it keeps the evaluated temp.
            if !tail && let Some((slot, constant)) = self.direct_literal(arg, param_ty)? {
                let at = staged.len();
                staged.push(slot);
                counted.push(false);
                consts.push((at, constant));
                continue;
            }
            // A single-use computed scalar argument writes its overlay
            // word directly: the offer goes to the argument's outermost
            // data op, taken only when every later argument evaluates
            // without touching the overlay.
            let offered = if !tail
                && let Some(slot) = self.direct_compute(arg, param_ty, &arg_exprs[index + 1..])?
            {
                self.scalar_dst = Some(ScalarDst::Arg {
                    index: u8::try_from(staged.len()).expect("overlay word fits u8"),
                    slot,
                });
                true
            } else {
                false
            };
            let arg_val = self.value(arg)?;
            let direct = offered && self.scalar_dst.take().is_none();
            if self.deinit_claim(arg_val.ty, arg.span())?.is_some() {
                // a consuming parameter behind a callable word has no
                // claim contract: the affine pass borrows the argument
                // at an indirect call (the fn type erases the mode), so
                // the callee's own claim would double this frame's —
                // and an untraceable word cannot promise borrow mode
                // either. Per-span claims own the seam; named callees
                // carry the mode.
                match param_modes.as_ref().and_then(|modes| modes.get(index)) {
                    Some(ds_parse::ParamMode::Borrow | ds_parse::ParamMode::Mut(_)) => {},
                    _ => {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument at a callable value",
                            "the callable's fn type erases the parameter mode, so the claim \
                             contract cannot be established; pass the value to a named function",
                        ));
                    },
                }
                if tail {
                    deinit_arg = true;
                }
            }
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter, staged as a borrow window
            if self.is_closure_word(&arg_val.place, arg_val.ty)? {
                Self::guard_callable_staged(Some(&expected_words), staged.len(), arg.span())?;
                defaulted.push(staged.len());
            }
            // a union-typed parameter takes its member's whole region: a
            // bare member, a strict sub-union argument, or an equal-typed
            // argument whose place keeps a wider union's physical layout
            // re-spans into a staging temp of the parameter's region,
            // the named-callee widening family's value-site spelling
            let arg_val = match params.get(index) {
                Some(&param_ty)
                    if self.is_union_ty(param_ty)?
                        && (param_ty != arg_val.ty
                            || !self.place_layout_matches(&arg_val.place, param_ty)?) =>
                {
                    // the same staging-temp backstop as the named
                    // callee path
                    if self.deinit_claim(arg_val.ty, arg.span())?.is_some() {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument into a union parameter",
                            "the re-span's staging temp is anonymous, so it carries no claim \
                             contract of its own; the deinit arc owns the seam",
                        ));
                    }
                    let temp = self.temp_span(param_ty)?;
                    self.widen_into(&temp, param_ty, &arg_val, arg.span())?;
                    TypedPlace {
                        place: temp,
                        ty: param_ty,
                    }
                },
                _ => arg_val,
            };
            // the argument's own variable word stages as the wildcard: a
            // Raw word whose runtime kind the callee claims, so its
            // count rides the hand-off below like a Ref word's
            let var_word = self.is_var_ty(arg_val.ty);
            if direct {
                // the argument's outermost data op wrote its overlay
                // word: the staging records the word's kind; no Mov
                // and no window read ride it (a scalar word carries
                // no count)
                let PlaceShape::Word(slot) = arg_val.place.shape else {
                    return Err(mismatch("a directly staged scalar argument"));
                };
                staged.push(slot);
                counted.push(false);
            } else {
                for addr in self
                    .word_addrs(&arg_val.place, arg_val.ty)
                    .map_err(|err| err.respanned(arg.span()))?
                {
                    self.rc.native_reads([addr.slot]);
                    moves.push((staged.len(), addr.slot));
                    staged.push(addr.slot_ty);
                    counted.push(addr.slot_ty == SlotType::Ref || var_word);
                }
            }
        }
        self.guard_staged_count(staged.len())?;
        if staged.len() != expected {
            let reason = if staged.len() < expected {
                "a defaulted parameter on a callable value has no staging path this round; pass \
                 every argument explicitly"
            } else {
                "the staged words disagree with the callee's parameter span; a union-typed \
                 parameter takes its member's whole region"
            };
            return Err(LowerError::unsupported(
                callee_span,
                "a call argument",
                reason,
            ));
        }
        // the tail transfer: only a proven count-free scalar callee word
        // with no closure-defaulted argument and no deinit-carrying
        // argument takes it — the count-free word needs no settling and
        // the named function's own pool FuncRef drives the CALLT
        let tail_transfer = tail && proven_fn.is_some() && defaulted.is_empty() && !deinit_arg;
        // the literal arguments' constant loads land on their overlay
        // words at the staging position, in staged order
        for &(at, constant) in &consts {
            self.scalar_const_into(
                constant,
                Slot::overlay(u8::try_from(at).expect("overlay word fits u8")),
            );
        }
        for &(index, slot) in &moves {
            if !tail_transfer {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    src: Slot::body(slot),
                });
            }
            // a borrow parameter owns the handed word: forward on the
            // source's death, dup otherwise (C2/C4); a callable-defaulted
            // closure word is a plain borrow — the callee reads it (CALLS
            // never consumes) and never owns its count. A transferring
            // tail's count rides the callee like every staged local word
            // (its own exit drops the handed parameter). The variable
            // word's Raw spelling counts too: the kind-checked pair makes
            // the runtime fact safe to stage blind
            if counted[index] {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
                } else if tail_transfer {
                    self.rc.staged_tail_local(&mut self.code, slot);
                } else {
                    self.rc.staged_local(&mut self.code, slot);
                }
            }
        }
        let mut result_words = self.shapes.words(ret)?;
        // a lambda-value callee is a known function: a factory's fn-typed
        // result is the closure its body constructs, so the result word
        // classifies Ref (a binding carrying an unknown fn value keeps
        // the scalar classification — a closure returned through it traps
        // at the call, the recorded return-flow seam)
        if *result_words == [SlotType::Fn]
            && let Some(&index) = self.fn_words.get(&word.slot)
            && self.tail_builds_a_closure(index)
        {
            result_words = Rc::from([SlotType::Ref]);
        }
        self.overlay_site(&staged, &result_words);
        if tail_transfer {
            // the tracking proved the callee word a count-free scalar
            // naming one planned function: the tail transfers through
            // CALLT with that function's own pool FuncRef [H2], the exit
            // claims staging first and the argument moves after them
            let index = proven_fn.expect("the transfer requires a proven function");
            let kidx = self.pool(Constant::FuncRef {
                key: FuncRefKey::Dep(0), // the enclosing module
                func: index,
            });
            self.rc.native_tail_transfer();
            self.rc.exit(&mut self.code, &[]);
            self.stage_moves(&moves);
            self.emit(Op::Callt {
                callee: kidx,
                nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
            });
            // an unreachable marker place; no code follows a tail call
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        self.emit(Op::Calls {
            callee_slot: word.slot,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        if closure {
            // the call read the closure word: at its death use the count
            // drops here, the object's death releasing the captures
            self.rc.read(&mut self.code, &[word.slot]);
        }
        let place = self.result_place(ty, &result_words, callee_span)?;
        // the same trust as the named-callee path: the factory check ran
        // over this word and left the scalar classification, so the
        // result word is a proven scalar; an unproven callee word keeps
        // no trust (its result may hold a closure's Ref at runtime)
        if *result_words == [SlotType::Fn] && self.fn_words.contains_key(&word.slot) {
            self.scalar_fn_words.insert(place.base);
        }
        if tail {
            // the in-place tail (the unproven maybe-Ref word): the CALLS
            // runs inside this frame, the post-call flush settles the
            // borrow windows, and the return's exit drops flush every
            // other count the frame owes
            self.emit_return(&place, ty, callee_span)?;
            // an unreachable marker place; no code follows the return
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        Ok(place)
    }

    /// A fallible or checked native's result span: neither platform span
    /// carries a source-level return type, so a tail call rejects, and a
    /// value-position call re-spans through its shape's dedicated path.
    fn native_span_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        tail: bool,
        callee_span: Span,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if tail {
            let (what, reason) = if matches!(native.shape, NativeShape::Fallible { .. }) {
                (
                    "a tail call to a fallible native",
                    "CALLT-to-fallible stays unsupported: the two-word platform span re-spans \
                     into the call's union at the call site, and the tail transfer leaves no \
                     position for it; keep the call out of tail position",
                )
            } else {
                (
                    "a tail call to a checked-read native",
                    "the ok/none span has no source-level return type; keep the call out of tail \
                     position",
                )
            };
            return Err(LowerError::unsupported(callee_span, what, reason));
        }
        if let NativeShape::Fallible {
            payload,
            fail_payload,
            err_prefix,
            classes,
        } = native.shape
        {
            return self.fallible_native_result(
                native,
                ty,
                payload,
                fail_payload,
                err_prefix,
                classes,
                staged,
            );
        }
        self.checked_native_result(Callee::Builtin(native), ty, staged, staged.len())
    }

    /// Stages one generic call's evidence words [H3]: the receiver's
    /// builtin instance — one statically-resolved instance reference,
    /// an i64 pool constant — staged on the overlay after the real
    /// arguments, one word per entry of the callee's constraint list.
    /// The checker's recorded instantiation must agree with the
    /// constraint count; a receiver no covered instance names, or one
    /// outside the callee's implemented set, is a lowering error,
    /// never a runtime misdispatch. The minted instance comes back for
    /// the result shape's instance-keyed word kind. `defer` registers
    /// the staged types without emitting the Kimm stores — the tail
    /// transfer path re-stages every word past the exit claims, and a
    /// claim's overlay window would clobber words staged before it.
    // the inputs are the call's identity, the staging vec, and both
    // routing flags: one flat list where each parameter names a fact
    #[allow(clippy::too_many_arguments)]
    fn stage_generic_evidence(
        &mut self,
        generic: &'static GenericNative,
        def: DefId,
        span: Span,
        call: Span,
        staged: &mut Vec<SlotType>,
        receiver_ty: Option<Ty>,
        defer: bool,
        member_routed: bool,
    ) -> Result<BuiltinInstance, LowerError> {
        let sig = self
            .analyzed
            .decls
            .fns
            .get(&def)
            .ok_or_else(|| mismatch("a generic callee signature"))?;
        // a member-routed native (`xs.get(i)`, `m.get(k)`) carries no
        // free-call instantiation record: the member's checked resolution
        // is the instantiation's authority, and only the declared arity
        // matters here — the instance word derives from the receiver's
        // own type below
        if member_routed {
            if sig.generics.len() != usize::from(generic.evidence) {
                return Err(LowerError::unsupported(
                    span,
                    "a generic call",
                    "the builtin's evidence arity disagrees with the declaration's generics",
                ));
            }
        } else {
            let instantiations =
                self.analyzed
                    .checked
                    .generic_calls
                    .get(&call)
                    .ok_or_else(|| {
                        LowerError::unsupported(
                            span,
                            "a generic call",
                            "the checker recorded no full instantiation for it",
                        )
                    })?;
            if instantiations.len() != sig.generics.len()
                || instantiations.len() != usize::from(generic.evidence)
            {
                return Err(LowerError::unsupported(
                    span,
                    "a generic call",
                    "the builtin's evidence arity disagrees with the declaration's generics",
                ));
            }
        }
        let receiver = receiver_ty.ok_or_else(|| mismatch("a generic call's receiver"))?;
        let instance = self.shapes.builtin_instance(receiver).ok_or_else(|| {
            LowerError::unsupported(
                span,
                "a generic call",
                "no builtin instance covers the receiver's type; string, bytes, the list and map \
                 constructors, and the int and float scalars are the covered ones",
            )
        })?;
        if !generic.instances.contains(&instance) {
            return Err(LowerError::unsupported(
                span,
                "a generic call",
                "the builtin's one body implements no instance of the receiver's type",
            ));
        }
        self.guard_staged_count(staged.len() + usize::from(generic.evidence))?;
        for _ in 0..generic.evidence {
            if !defer {
                let kidx = self.pool(Constant::I64(instance as i64));
                self.emit(Op::Kimm {
                    dst: Slot::overlay(u8::try_from(staged.len()).expect("overlay word fits u8")),
                    kidx,
                });
            }
            staged.push(SlotType::I64);
        }
        Ok(instance)
    }

    /// A method-call site's solved dispatch [B6, H3]: the impl body's
    /// function-table index plus the instance id the checker recorded at
    /// the call's own span. An inherent survivor resolves through its
    /// own record below, and a site satisfied only through an in-scope
    /// assumption records no instance — the latter refuses naming the
    /// seam it waits on.
    pub(super) fn resolve_method_dispatch(
        &self,
        call: Span,
        what: Span,
        recv: Option<&Expr>,
        candidates: &[MethodId],
    ) -> Result<TraitDispatch, LowerError> {
        let inherent_only = candidates.iter().all(|&method| {
            matches!(
                self.analyzed.package.method(method).owner,
                MethodOwner::Inherent(_)
            )
        }) && !candidates.is_empty();
        if let Some(&method) = self.analyzed.checked.inherent_calls.get(&call) {
            return self.resolve_inherent_method(method, call, what);
        }
        let Some(tc) = self.analyzed.checked.trait_calls.get(&call).cloned() else {
            // A bound-variable receiver inside a generic impl body: the
            // checker satisfied the call through the impl's own bound
            // (an in-scope assumption), so no instance records here —
            // dispatch resolves at runtime through the evidence slot the
            // call site entering this body staged.
            if let Some(recv) = recv
                && let Ok(val) = self.val(recv.span())
                && let [var] = self.members(val.ty)
                && self.shapes.is_var_atom(*var)
                && let Some(param) =
                    crate::shape::var_param_name(&self.analyzed.decls.universe, *var)
            {
                let matching: Vec<(MethodId, u8)> = self
                    .evidence
                    .iter()
                    .filter(|((pair_var, method), _)| {
                        pair_var == &param && candidates.contains(method)
                    })
                    .map(|((_, method), slot)| (*method, *slot))
                    .collect();
                return match matching.as_slice() {
                    [(method, slot)] => Ok(TraitDispatch::Runtime {
                        slot: *slot,
                        method: *method,
                    }),
                    [] => Err(LowerError::unsupported(
                        what,
                        "a trait-method call on a bound type variable",
                        "this body carries no runtime evidence slot for the member (the body \
                         demands no such pair, or the frame is outside the evidence seam); the \
                         seam covers generic bodies, their lambdas, and their spawned children",
                    )),
                    _ => Err(LowerError::unsupported(
                        what,
                        "a trait-method call on a bound type variable",
                        "the member name is ambiguous across the variable's bounds; qualify the \
                         trait",
                    )),
                };
            }
            return Err(LowerError::unsupported(
                what,
                if inherent_only { "an inherent method call" } else { "a trait-method call" },
                if inherent_only {
                    "the checker recorded no solved inherent dispatch for this site; report the \
                     construct"
                } else {
                    "the checker recorded no solved instance for this site (a bound-type receiver \
                     outside its generic impl body)"
                },
            ));
        };
        let member = self.analyzed.package.method(tc.method).name.clone();
        // a linked impl owns no impl item in this compile: the solved
        // instance's member links address the linked module directly,
        // the same transfer a linked free fn's call takes. An impl
        // spelling no member falls through — the trait's declared
        // default serves it, exactly like a local omission's site
        if let Some(link) = self.analyzed.decls.linked_impls.get(&tc.instance)
            && let Some(func) = link.member(&member)
        {
            if matches!(self.comptime, ComptimeRun::Slice) {
                return Err(Self::comptime_member_refusal(what));
            }
            return Ok(TraitDispatch::Foreign {
                position: link.position,
                func,
                call: tc,
                default_body: false,
            });
        }
        if let Some(impl_site) = tc.impl_site {
            if let Some(index) = self.plan.impl_body(impl_site, &member) {
                return Ok(TraitDispatch::Static { index, call: tc });
            }
            // A defaulted member is omittable exactly because the
            // trait's declaration carries the dispatch target: an impl
            // spelling no member dispatches into the declared default
            // body, the same staged shape (evidence demands trailing
            // the true arguments, the solved instance word trailing
            // them).
            if let MethodOwner::Trait(def) = self.analyzed.package.method(tc.method).owner
                && let Some(index) = self.plan.default_body(def, &member)
            {
                return Ok(TraitDispatch::Static { index, call: tc });
            }
        }
        // The linked trait's declared default body: the linked mirror
        // of the plan's own table above, serving a solving impl that
        // spells no member — a linked impl owns no impl item here, and
        // a local impl of the minted trait plans no default of its
        // own, so this link is the only remaining dispatch target
        if let MethodOwner::Trait(def) = self.analyzed.package.method(tc.method).owner
            && let Some(link) = self.analyzed.decls.linked_trait_defaults.get(&def)
            && let Some(func) = link.member(&member)
        {
            if matches!(self.comptime, ComptimeRun::Slice) {
                return Err(Self::comptime_member_refusal(what));
            }
            return Ok(TraitDispatch::Foreign {
                position: link.position,
                func,
                call: tc,
                default_body: true,
            });
        }
        Err(LowerError::unsupported(
            what,
            "a trait-method call",
            "the solving impl's body did not plan (no body, no solved return, or a shape over its \
             own type variables), and the trait's declared default body is not linked here; \
             report the construct",
        ))
    }

    /// The comptime slice's linked member-call refusal: the slice plans
    /// nothing outside its own module, so a linked body — an impl
    /// member's or a trait's declared default — keeps the local member
    /// call's own door.
    fn comptime_member_refusal(what: Span) -> LowerError {
        LowerError::unsupported(
            what,
            "a call from a comptime function",
            "a comptime function may call the pure builtins and other comptime functions this \
             round; a runtime helper is the slice seam",
        )
    }

    /// A solved inherent-method call site: the impl body the
    /// checker-recorded method id names, reached through the declaration
    /// tables' method-to-impl-item map. The receiver's own type picked
    /// the body at check time — no instance word exists for an inherent
    /// impl, so the call dispatches like a local callee; a generic body's
    /// entering site carries the checker-recorded bounds (the serving
    /// instance per declared bound under the receiver's substitution),
    /// which stage the body's evidence demands.
    /// The native seam a bodiless ambient collection member routes to:
    /// `list`'s `get`/`push`/`concat` and `map`'s `get`/`set` declare no
    /// ds body
    /// (an element-carrying generic member body has no static word kind),
    /// so the call lowers exactly like the native's own retired free
    /// spelling — the receiver feeding the first argument. `window`
    /// marks the in-place mutations whose member answer re-answers the
    /// receiver after the native's unit word. A member with a ds body
    /// (the `keys` shape, or a user impl's own body) lowers that body.
    fn native_member_def(&self, call: Span) -> Option<NativeMember> {
        let &method = self.analyzed.checked.inherent_calls.get(&call)?;
        let MethodOwner::Inherent(target) = &self.analyzed.package.method(method).owner else {
            return None;
        };
        let member = self.analyzed.package.method(method).name.as_str();
        let (native, window) = match (target.as_str(), member) {
            ("list", "get") => ("list_get", false),
            ("list", "set") => ("list_set", true),
            ("list", "push") => ("list_push", true),
            ("list", "insert") => ("list_insert", true),
            ("list", "remove") => ("list_remove", true),
            ("list", "clear") => ("list_clear", true),
            ("list", "concat") => ("list_concat", false),
            ("map", "get") => ("map_get", false),
            ("map", "set") => ("map_set", true),
            ("map", "remove") => ("map_remove", true),
            ("map", "contains") => ("map_contains", false),
            ("map", "entries") => ("map_entries", false),
            ("chan", "send") => ("chan_send", false),
            ("chan", "recv") => ("chan_recv", false),
            ("arena", "insert") => ("arena_insert", false),
            ("arena", "remove") => ("arena_remove", false),
            ("arena", "at") => ("arena_at", false),
            _ => return None,
        };
        let &impl_site = self.analyzed.decls.inherent_impls.get(&method)?;
        if self.plan.impl_body(impl_site, member).is_some() {
            return None;
        }
        let def = self.analyzed.package.lookup_value(native)?;
        Some(NativeMember { def, window })
    }

    fn resolve_inherent_method(
        &self,
        method: MethodId,
        call: Span,
        what: Span,
    ) -> Result<TraitDispatch, LowerError> {
        let Some(&impl_site) = self.analyzed.decls.inherent_impls.get(&method) else {
            return Err(LowerError::unsupported(
                what,
                "an inherent method call",
                "the impl item owning the method is missing from the declaration tables; report \
                 the construct",
            ));
        };
        let member = self.analyzed.package.method(method).name.clone();
        let Some(index) = self.plan.impl_body(impl_site, &member) else {
            return Err(LowerError::unsupported(
                what,
                "an inherent method call",
                "the impl's body did not plan (no body, no solved return, or a shape over its own \
                 type variables); report the construct",
            ));
        };
        let bounds = self
            .analyzed
            .checked
            .inherent_call_bounds
            .get(&call)
            .cloned()
            .unwrap_or_default();
        Ok(TraitDispatch::Inherent {
            index,
            bounds,
            method,
        })
    }

    /// The trait one evidence pair's member belongs to: the walk's key
    /// into the checker's per-site bound records.
    fn pair_trait_id(
        &self,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<TraitId, LowerError> {
        let method = self.analyzed.package.method(pair.method);
        let MethodOwner::Trait(def) = method.owner else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the body's member call did not resolve to a trait member; report the construct",
            ));
        };
        self.analyzed
            .decls
            .traits
            .get(&def)
            .copied()
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    what,
                    "the member's trait is missing from the declaration tables",
                )
            })
    }

    /// The runtime form's evidence word for one pair: this site's
    /// substitution is its own frame's, so the receiver's single
    /// variable peels to the frame's written parameter name and the
    /// pair's slot word — the chain object's index — reads verbatim
    /// from the frame's evidence slots.
    fn slot_evidence(
        &self,
        recv: Ty,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let universe = &self.analyzed.decls.universe;
        let [only] = universe.members(recv) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the receiver unions members across branches; per-branch runtime evidence has no \
                 single serving body",
            ));
        };
        let Some(param) = peeled_param(universe, *only) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the receiver's type carries no type variable of this body, so the demand keys no \
                 frame evidence slot",
            ));
        };
        let Some(&slot) = self.evidence.get(&(param, pair.method)) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "this frame carries no runtime-evidence slot for the member (a demand the walk \
                 does not collect); the evidence seam covers generic bodies, their lambdas, and \
                 their spawned children",
            ));
        };
        Ok(vec![EvidenceSrc::Slot(slot)])
    }

    /// The evidence-chain object one statically-resolved demand resolves
    /// to [H3]: the serving member body's function-table index for
    /// `method` over the substitution `start` names, plus one tail chain
    /// per the serving body's own evidence demands — each demand's own
    /// member and substituted type, walked recursively. The staged word
    /// a site enters this chain through carries the object's table
    /// index; a nested runtime dispatch reads its frame's slot word,
    /// resolves the object, dispatches into `func`, and stages the tail
    /// objects' indices as the body's trailing evidence parameters — so
    /// depth is table-bounded, never frame-bounded, and a body may
    /// demand several or different members (one tail chain each).
    ///
    /// The walk memoizes its states (member, instance, substitution): a
    /// repeated state shares its entry, which is also the termination
    /// story — a cyclic demand graph builds a cyclic table the
    /// dispatches walk lazily, one hop per call.
    fn chain_for(
        &mut self,
        method: MethodId,
        instance: InstanceId,
        start: Option<&[TypeId]>,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        let universe = &self.analyzed.decls.universe;
        let env = &self.analyzed.decls.instances;
        // the one-word instantiation boundary: this level's serving body
        // is entered by a nested dispatch that stages the bound
        // variable's single runtime word, so a substitution spanning more
        // must refuse at this entering site, where the type is concrete
        if let Some(members) = start {
            self.guard_one_word_instantiation(members, what, span)?;
        }
        let key = (method, instance, start.map(<[TypeId]>::to_vec));
        if let Some(&index) = self.plan.chain_memo.get(&key) {
            return Ok(index);
        }
        let impl_site = self
            .analyzed
            .decls
            .instance_impls
            .get(&instance)
            .copied()
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    what,
                    "the serving instance's impl item is missing from the declaration tables",
                )
            })?;
        let func = self.serving_body(impl_site, method, what, span)?;
        // reserve the entry before the recursion so a cyclic demand
        // graph closes over it
        let index = u32::try_from(self.plan.chains.len()).expect("chain table fits u32");
        self.plan.chains.push(ds_mod::EvidenceChain {
            func,
            next: Vec::new(),
        });
        self.plan.chain_memo.insert(key, index);
        let pairs = self.plan.functions()[func as usize].evidence.clone();
        let mut next = Vec::with_capacity(pairs.len());
        if !pairs.is_empty() {
            if self.plan.is_default_body(func) {
                // the serving body is a trait's declared default: every
                // demand keys by the trait's own variables and resolves
                // under the substitution this level entered with — a
                // self-recursive demand closes onto this very entry
                // through the memo, so a cyclic demand graph still
                // builds a cyclic table
                for pair in pairs.iter() {
                    next.push(self.default_pair_chain(method, instance, start, pair, what, span)?);
                }
            } else {
                let Some(members) = start else {
                    return Err(LowerError::unsupported(
                        span,
                        what,
                        "the transitive serving body's own bound is not resolvable at this site; \
                         report the construct",
                    ));
                };
                for pair in pairs.iter() {
                    let pair_trait = self.pair_trait_id(pair, what, span)?;
                    // the serving body's own demand variable names the
                    // next bound: the serving impl declares it, so its
                    // conditions key by that written name
                    let Some((next_members, Some(next_instance))) =
                        ds_check::instance_solve::evidence_step(
                            universe,
                            env,
                            instance,
                            members,
                            Some(pair.var.as_str()),
                            pair_trait,
                        )
                    else {
                        return Err(LowerError::unsupported(
                            span,
                            what,
                            "the element type's transitive bound has no unique serving instance \
                             at this site; runtime evidence resolves one body per bound",
                        ));
                    };
                    // the same boundary one level down: the next link's
                    // serving body receives this substitution as its
                    // receiver
                    self.guard_one_word_instantiation(&next_members, what, span)?;
                    next.push(self.chain_for(
                        pair.method,
                        next_instance,
                        Some(&next_members),
                        what,
                        span,
                    )?);
                }
            }
        }
        self.plan.chains[index as usize].next = next;
        Ok(index)
    }

    /// One demand pair's chain inside a trait's declared default body,
    /// over the member whose default body serves this level, the
    /// instance whose impl enters it, and the receiver member set the
    /// body runs with: a `Self`-keyed pair re-serves the receiver under
    /// the same instance (the entry `chain_for` builds for the called
    /// member — the memo closes a self-recursive demand onto its own
    /// entry), while a pair over the trait's own generic variable
    /// resolves through the argument the impl's trait reference spells
    /// for it: a concrete argument names the substituted type itself,
    /// an impl-variable argument names a variable this receiver binds,
    /// the impl's own declared bound on it carrying the next level
    /// exactly as the generic-impl branch's step.
    fn default_pair_chain(
        &mut self,
        method: MethodId,
        instance: InstanceId,
        start: Option<&[TypeId]>,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        // the receiver stages as the default body's `Self` variable's
        // single Raw word, so the one-word instantiation boundary guards
        // it here (the tail level re-guards what its own chain_for entry
        // already checked)
        if let Some(members) = start {
            self.guard_one_word_instantiation(members, what, span)?;
        }
        if pair.var == "Self" {
            return self.chain_for(pair.method, instance, start, what, span);
        }
        let universe = &self.analyzed.decls.universe;
        let env = &self.analyzed.decls.instances;
        let trait_id = self.pair_trait_id(pair, what, span)?;
        // the default body's trait, and the argument its impl substitutes
        // for the demanded generic (positional over the declaration's
        // own generics)
        let MethodOwner::Trait(def) = self.analyzed.package.method(method).owner else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the declared default body's member did not resolve to a trait; report the \
                 construct",
            ));
        };
        let generics = self.analyzed.decls.trait_generics.get(&def);
        let impl_site = self.analyzed.decls.instance_impls.get(&instance).copied();
        let arg = generics
            .and_then(|names| names.iter().position(|name| name == &pair.var))
            .zip(impl_site.and_then(|site| self.analyzed.decls.trait_impl_args.get(&site)))
            .and_then(|(position, args)| args.get(position).copied());
        let Some(arg) = arg else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the trait variable's substitution is missing from the impl's trait reference at \
                 this site; report the construct",
            ));
        };
        let members = universe.members(arg);
        // the argument is the impl's own variable: this receiver binds
        // it, and the impl's declared bound on it carries the next level
        if let [only] = members
            && let Some(var) = crate::shape::var_param_name(universe, *only)
        {
            let Some(recv) = start else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the trait variable maps to the impl's own variable, whose substitution this \
                     site does not carry; report the construct",
                ));
            };
            let Some((next_members, Some(next_instance))) = ds_check::instance_solve::evidence_step(
                universe,
                env,
                instance,
                recv,
                Some(var.as_str()),
                trait_id,
            ) else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the impl's own bound on the trait variable's substitution has no unique \
                     serving instance at this site; runtime evidence resolves one body per bound",
                ));
            };
            // the same boundary one level down: the next link's serving
            // body receives this substitution as its receiver
            self.guard_one_word_instantiation(&next_members, what, span)?;
            return self.chain_for(pair.method, next_instance, Some(&next_members), what, span);
        }
        // the argument is concrete: it names the substituted type itself
        if !members
            .iter()
            .any(|&atom| carries_a_var(universe, atom, None))
        {
            let Some(next_instance) =
                ds_check::instance_solve::resolve(universe, env, &[], trait_id, arg).solved()
            else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the trait variable's substituted type has no serving instance at this site; \
                     runtime evidence resolves one body per bound",
                ));
            };
            self.guard_one_word_instantiation(members, what, span)?;
            return self.chain_for(pair.method, next_instance, Some(members), what, span);
        }
        Err(LowerError::unsupported(
            span,
            what,
            "the trait variable substitutes to a compound type over the impl's variables, a shape \
             this seam resolves per variable alone; report the construct",
        ))
    }

    /// The dispatch target one solved instance names for one member: the
    /// impl's spelled body, or — when the impl spells no member — the
    /// trait's declared default body (the member is omittable exactly
    /// because the declaration carries the dispatch target).
    fn serving_body(
        &self,
        impl_site: Span,
        method: MethodId,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        let method = self.analyzed.package.method(method);
        if let Some(index) = self.plan.impl_body(impl_site, &method.name) {
            return Ok(index);
        }
        if let MethodOwner::Trait(def) = method.owner
            && let Some(index) = self.plan.default_body(def, &method.name)
        {
            return Ok(index);
        }
        Err(LowerError::unsupported(
            span,
            what,
            "the element type's serving impl body did not plan; report the construct",
        ))
    }

    /// Whether one decomposed branch instance's head mentions a minted
    /// variable of the written name anywhere in its shape: per-branch
    /// solving keeps a branch's condition only for the variables its
    /// own head mentions, so a demand keyed by a name the entering
    /// branch never carries is unreachable in it — the narrowing the
    /// demanding code sits under cannot hold there. A missing head
    /// consults nothing and keeps the honest refusal.
    fn branch_mentions_var(&self, instance: InstanceId, want: &str) -> bool {
        let Ok(head) = self.analyzed.decls.instances.head(instance) else {
            return true;
        };
        let universe = &self.analyzed.decls.universe;
        universe
            .members(head)
            .iter()
            .any(|&atom| carries_a_var(universe, atom, Some(want)))
    }

    /// The staged evidence words a call site entering a generic impl body
    /// stages for one of the body's demands: the checker-recorded bounds
    /// of this call site's solved dispatch answer the pair's bound — a
    /// concrete answer walks the serving chain, a `None` answer (the
    /// site sits inside a generic body whose substitution is that
    /// frame's own) reads the frame's evidence slots.
    fn evidence_words(
        &mut self,
        pair: &RuntimePair,
        bounds: &InherentCallBounds,
        entered: Option<InstanceId>,
        inherent_site: Option<Span>,
        recv: Option<Ty>,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let trait_id = self.pair_trait_id(pair, what, span)?;
        let universe = &self.analyzed.decls.universe;
        let env = &self.analyzed.decls.instances;
        // the recorded bounds key by the declaration's own minted variable;
        // the written parameter name bridges the two mintings
        let entry = bounds.iter().find(|(var, bound, _)| {
            crate::shape::var_param_name(universe, *var).as_deref() == Some(pair.var.as_str())
                && *bound == trait_id
        });
        let Some((_, _, served)) = entry else {
            // a demand whose variable the entering branch's head never
            // mentions: per-branch solving kept no condition for it, so
            // the demanding code sits under a narrowing that branch
            // cannot satisfy — the site stages the dead sentinel, a
            // chain word naming no object that a dispatch reads only if
            // the unreachable narrowing ever ran, trapping loudly there.
            // A union receiver (branches solving the variable
            // differently) and a head that does mention the variable
            // keep the honest refusal below.
            if let Some(instance) = entered
                && !self.branch_mentions_var(instance, &pair.var)
                && recv.is_some_and(|recv| self.members(recv).len() == 1)
            {
                return Ok(vec![EvidenceSrc::Dead]);
            }
            return Err(LowerError::unsupported(
                span,
                what,
                "the element type's bound has no unique serving instance at this site (or the \
                 receiver is a union across branches); runtime evidence resolves one body per \
                 bound",
            ));
        };
        let Some(instance) = served else {
            // the runtime form: this site's substitution is its own frame's
            let Some(recv) = recv else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the site's receiver type is missing; report the construct",
                ));
            };
            return self.slot_evidence(recv, pair, what, span);
        };
        let instance = *instance;
        // the chain start: the entered instance's own bound on this
        // pair's variable, substituted with the receiver this site
        // entered the body with
        let start = match (entered, inherent_site) {
            (Some(entered), _) => recv.and_then(|recv| {
                Self::chain_start(universe, env, entered, recv, &pair.var, trait_id)
            }),
            (None, Some(site)) => recv.and_then(|recv| {
                self.analyzed
                    .decls
                    .inherent_impl_bounds
                    .get(&site)
                    .and_then(|record| {
                        ds_check::instance_solve::inherent_evidence_step(
                            universe,
                            env,
                            record.head,
                            &record.conditions,
                            universe.members(recv),
                            Some(pair.var.as_str()),
                            trait_id,
                        )
                    })
                    .map(|(target, _)| target)
            }),
            (None, None) => None,
        }
        .map(Cow::Owned);
        Ok(vec![EvidenceSrc::Const(self.chain_for(
            pair.method,
            instance,
            start.as_deref(),
            what,
            span,
        )?)])
    }

    /// The staged evidence words a call into a free generic fn's body
    /// stages for one of the body's demands [H3]: the checker's
    /// per-call record answers the pair's bound under this site's own
    /// substitution — a concrete answer stages the serving chain
    /// object, and a `None` answer (the substitution is the calling
    /// frame's own variable) reads the frame's evidence area for the
    /// pair.
    fn free_evidence_words(
        &mut self,
        pair: &RuntimePair,
        record: &GenericCallBounds,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let trait_id = self.pair_trait_id(pair, what, span)?;
        let universe = &self.analyzed.decls.universe;
        let Some((_, _, served, solved)) = record.iter().find(|(var, bound, ..)| {
            crate::shape::var_param_name(universe, *var).as_deref() == Some(pair.var.as_str())
                && *bound == trait_id
        }) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the call site recorded no bound answer for the parameter; report the construct",
            ));
        };
        let Some(instance) = served else {
            return self.slot_evidence(*solved, pair, what, span);
        };
        let instance = *instance;
        // the chain start is the solved type itself: the demand's
        // variable takes it at this site, and the first tail chain
        // serves the body the serving instance's own head re-substitutes
        // it into
        let start = Some(Cow::Borrowed(universe.members(*solved)));
        Ok(vec![EvidenceSrc::Const(self.chain_for(
            pair.method,
            instance,
            start.as_deref(),
            what,
            span,
        )?)])
    }

    /// The one-hop chain-start substitution: the instance's declared
    /// bound on the pair's variable, solved for the receiver this site
    /// hands the body.
    fn chain_start(
        universe: &ds_check::Universe,
        env: &ds_check::instance::InstanceEnv,
        instance: InstanceId,
        recv: Ty,
        var: &str,
        trait_id: TraitId,
    ) -> Option<Vec<TypeId>> {
        ds_check::instance_solve::evidence_step(
            universe,
            env,
            instance,
            universe.members(recv),
            Some(var),
            trait_id,
        )
        .map(|(target, _)| target)
    }

    /// The one-word instantiation boundary of the runtime-evidence seam:
    /// the one generic body lowers a type variable's value to a single
    /// Raw word — the stride-1 element iteration, the binder, and every
    /// nested dispatch's staging carry that one word — while a serving
    /// body claims the substituted type's whole span as its receiver,
    /// so a multi-word substitution must refuse at the entering site
    /// (where the type is concrete), never slide at runtime. The
    /// whole-span vocabulary carries width-static spans alone (concrete
    /// call sites, signature-governed answers); the variable's width
    /// never reaches the frame, so no rider shape can carry it. Reboxing
    /// stays rejected, and per-instantiation frames are the JIT
    /// specialization tier's, never the interpreter's. A shape with no
    /// lowering answer refuses at its own staging, so an error here
    /// stays an error there.
    fn guard_one_word_instantiation(
        &mut self,
        members: &[TypeId],
        what: &'static str,
        span: Span,
    ) -> Result<(), LowerError> {
        let wide = matches!(self.shapes.member_words(members), Ok(ref words) if words.len() > 1);
        if wide {
            return Err(LowerError::unsupported(
                span,
                what,
                "the substituted type spans more than one word, while the one generic body lowers \
                 the variable's value to a single Raw word (the stride-1 element read, the \
                 binder, the nested dispatch's staging each carry that one word); reboxing stays \
                 rejected, and per-instantiation frames are the JIT specialization tier's, never \
                 the interpreter's",
            ));
        }
        Ok(())
    }

    /// Emits one dispatch site's evidence words into the overlay span at
    /// the recorded staged index: a constant word per Kimm store, a frame
    /// word per Mov — neither counted, neither a binder.
    fn stage_member_evidence(&mut self, words: &[EvidenceSrc], at: usize) {
        for (offset, word) in words.iter().enumerate() {
            let dst = Slot::overlay(u8::try_from(at + offset).expect("overlay word fits u8"));
            match word {
                EvidenceSrc::Const(index) => {
                    let kidx = self.pool(Constant::I64(i64::from(*index)));
                    self.emit(Op::Kimm { dst, kidx });
                },
                EvidenceSrc::Slot(slot) => {
                    self.emit(Op::Mov {
                        dst,
                        src: Slot::body(*slot),
                    });
                },
                EvidenceSrc::Dead => {
                    let kidx = self.pool(Constant::I64(DEAD_CHAIN));
                    self.emit(Op::Kimm { dst, kidx });
                },
            }
        }
    }

    fn resolve_callee(&self, def: DefId, span: Span) -> Result<Callee, LowerError> {
        if let Some(index) = self.plan.index_of(def) {
            return Ok(Callee::Local(index));
        }
        // an import-bound def is a linked package's function (G4): the
        // check comes ahead of the native fallback so a linked function
        // never silently answers as a same-named native
        if let Some(link) = self.analyzed.decls.foreign_links.get(&def) {
            return Ok(Callee::Foreign {
                def: Some(def),
                position: link.position,
                func: link.func,
            });
        }
        // a bound extern's declaration names an artifact (H5): the call
        // resolves through the graph's fold — the artifact's exported
        // symbols in enumeration order, the same table the run installs
        // as the artifact's native module — ahead of the compiler-wired
        // native fallback, so a same-named builtin never shadows a
        // declared bind
        if let Some(bind) = self
            .analyzed
            .decls
            .fns
            .get(&def)
            .and_then(|sig| sig.bind.clone())
        {
            return self.bound_callee(def, &bind, span);
        }
        let name = self.analyzed.package.def(def).name.clone();
        if let Some(native) = crate::native::lookup(&name) {
            return Ok(Callee::Builtin(native));
        }
        if let Some(sig) = self.analyzed.decls.fns.get(&def)
            && !sig.generics.is_empty()
        {
            if let Some(generic) = crate::native::lookup_generic(&name) {
                // a fixed-staged descriptor stages exactly one word per
                // declared parameter, so a signature carrying fewer
                // parameters than the contract stages names a seam the
                // free-generic path cannot lower: the ord-routed sort's
                // spelling carries no comparator, its callable staged
                // from the solved ord instance at the ambient's own call
                // sites alone
                if generic.staging == Staging::Fixed && generic.params.len() != sig.params.len() {
                    return Err(LowerError::unsupported(
                        span,
                        "a generic callee",
                        "the builtin's fixed staging covers more words than the spelling's \
                         parameters carry; the comparator-routed sort lowers at the ambient's own \
                         call sites alone, and a module-local declaration of the name has no \
                         lowering",
                    ));
                }
                return Ok(Callee::Generic(generic));
            }
            return Err(LowerError::unsupported(
                span,
                "a generic callee",
                "the generic body did not plan (an extern, `var`-bound, or bodiless generic fn \
                 lowers nothing), so no dispatch reaches it",
            ));
        }
        // the comptime slice carries only comptime functions: a call
        // into the package's runtime half refuses here, naming the seam —
        // a linked package's body included, the slice planning nothing
        // outside its own module
        if matches!(self.comptime, ComptimeRun::Slice) {
            return Err(LowerError::unsupported(
                span,
                "a call from a comptime function",
                "a comptime function may call the pure builtins and other comptime functions this \
                 round; a runtime helper is the slice seam",
            ));
        }
        Err(LowerError::unsupported(
            span,
            "a callee",
            "extern non-builtin and method callees are outside the contract",
        ))
    }

    /// Resolves one bound extern's declaration (H5): the bind target
    /// names an artifact the graph resolved, the extern's name a symbol
    /// its installed library exports — the FuncRef addressing the
    /// artifact's native module at the symbol's enumeration index. The
    /// signature must be the marshaling slice's own shape (the scalar
    /// words, a `string`/`bytes` parameter's call-scoped pair, and the
    /// absent word a unit return rides, at most [`ds_ffi::MAX_PARAMS`]
    /// C words with a pair counting two); anything richer refuses here
    /// naming the seam, before any run starts.
    fn bound_callee(&self, def: DefId, bind: &str, span: Span) -> Result<Callee, LowerError> {
        let sig = self
            .analyzed
            .decls
            .fns
            .get(&def)
            .expect("a bound callee reads the signature that named its target");
        let name = self.analyzed.package.def(def).name.clone();
        let refuse = |reason: String| LowerError::ForeignBind {
            span,
            name: name.clone(),
            reason,
        };
        let Some(symbols) = self.analyzed.artifacts.get(bind) else {
            return Err(refuse(format!(
                "the resolved graph declares no [artifacts] entry `{bind}`; declare the library \
                 in ds.pkg, or run the source as a package — externs bind only package-pinned \
                 libraries"
            )));
        };
        let Some(index) = symbols.iter().position(|symbol| *symbol == name) else {
            return Err(refuse(format!(
                "artifact `{bind}` exports no `{name}`; `ds bind` reports the exported spellings"
            )));
        };
        if !sig.generics.is_empty() {
            return Err(refuse(
                "a C symbol has no generic body; the signature must be monomorphic".to_owned(),
            ));
        }
        // the cap counts C words, not parameters: a string/bytes
        // parameter's (pointer, length) pair occupies two, so two pairs
        // fill the enumerated transmute's envelope exactly
        let mut words = 0usize;
        for param in &sig.params {
            let Some(ty) = &param.ty else {
                return Err(refuse(
                    "an inferred parameter has no written marshaling word; write the int, bool, \
                     float, string, or bytes annotation"
                        .to_owned(),
                ));
            };
            match marshal_word(ty) {
                Some(word) => words += word,
                None => {
                    return Err(refuse(
                        "the C-ABI marshaling slice rules int, bool, float, string, and bytes \
                         parameters; handle and aggregate marshaling is the ownership round's own"
                            .to_owned(),
                    ));
                },
            }
        }
        if words > ds_ffi::MAX_PARAMS {
            return Err(refuse(format!(
                "the C-ABI marshaling slice serves at most {} C words, a string or bytes \
                 parameter's pair occupying two",
                ds_ffi::MAX_PARAMS
            )));
        }
        // a unit return rides the absent word; the returning direction of
        // string/bytes marshaling needs an ownership story the round
        // keeps queued
        if let Some(ret) = &sig.ret
            && scalar_word(ret).is_none()
        {
            return Err(refuse(
                "the C-ABI marshaling slice answers one scalar word or none (a unit return rides \
                 the absent word); a string or bytes return is the ownership round's own"
                    .to_owned(),
            ));
        }
        Ok(Callee::Bound {
            def,
            index: u32::try_from(index).expect("the export enumeration fits u32"),
        })
    }

    /// The literal-only parameter defaults [F7] a local call stages
    /// for its omitted trailing parameters: the checker proved each
    /// default's literal against its parameter type, so the call lowers
    /// them exactly like written arguments. A non-defaulted omission is
    /// the checker's to reject; the call-site span guard names anything
    /// that still slips through.
    fn defaulted_args<'e>(
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

    /// The literal-only parameter defaults [F7] a trait-method dispatch
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
    fn trait_default_exprs(&self, method: MethodId, written: usize) -> Vec<Expr> {
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

    /// The literal-only parameter defaults [F7] an inherent-member call
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
    fn member_default_exprs(
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

    /// A local callee's parameter word count, as its own frame layout
    /// records it (params first, one region per parameter). `None` when
    /// a parameter type has no shape; those callees keep the runtime
    /// arity check.
    /// A declared impl-member body's parameter words, from the checked
    /// binder facts keyed by each parameter's name span — the same table
    /// the body's own binding reads, so call sites and body agree.
    pub(super) fn decl_param_words(&mut self, index: u32) -> Option<Vec<SlotType>> {
        let planned = self.plan.functions().get(index as usize)?;
        let decl = planned.decl.clone()?;
        let mut words = Vec::with_capacity(decl.params.len());
        for param in decl.params.iter() {
            let fact = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .copied()?;
            words.extend_from_slice(&self.shapes.words(fact.ty).ok()?);
        }
        Some(words)
    }

    /// A local callee's per-parameter checked types, when its signature
    /// is static: a declared body's parameters come from the checked
    /// binder facts keyed by each parameter's name span, a def's from
    /// its recorded signature. `None` keeps the runtime arity check.
    fn callee_param_types(&mut self, index: u32) -> Option<Vec<Ty>> {
        let planned = self.plan.functions().get(index as usize)?;
        if let Some(decl) = &planned.decl {
            return decl
                .params
                .iter()
                .map(|param| {
                    self.analyzed
                        .checked
                        .binders
                        .get(&param.name.span)
                        .map(|fact| fact.ty)
                })
                .collect();
        }
        let def = planned.def?;
        let fn_ty = self.analyzed.checked.def_types.get(&def).copied()?;
        let (params, _) = fn_sig_of(&self.analyzed.decls.universe, fn_ty)?;
        Some(params.to_vec())
    }

    /// The declared parameter modes of one planned function (a declared
    /// body's or a lambda's own parameters, a def's recorded signature):
    /// a consuming (`move`) parameter's staging forwards its argument's
    /// count and any deinit claim (C2) — the checker's forced move makes
    /// the death test true at exactly that use. `None` when the body's
    /// parameters are not statically readable here.
    fn planned_param_modes(&self, index: u32) -> Option<Vec<ds_parse::ParamMode>> {
        let planned = self.plan.functions().get(index as usize)?;
        if let Some(decl) = &planned.decl {
            return Some(decl.params.iter().map(|param| param.mode).collect());
        }
        if let Some(def) = planned.def {
            let sig = self.analyzed.decls.fns.get(&def)?;
            return Some(sig.params.iter().map(|param| param.mode).collect());
        }
        if let Expr::Lambda { params, .. } = &planned.lambda {
            return Some(params.iter().map(|param| param.mode).collect());
        }
        None
    }

    /// Whether the parameter at one written-argument position consumes
    /// its argument (`move` mode, C2): the callee is a planned body (a
    /// local callee or a trait dispatch's impl body) or a runtime
    /// dispatch through the trait member's declared signature (impl
    /// bodies agree with the declaration). Natives carry no modes, and a
    /// position with no readable mode borrows.
    fn consuming_args(
        &self,
        target: Callee,
        dispatch: Option<&TraitDispatch>,
        args: usize,
    ) -> Vec<bool> {
        let modes: Option<Vec<ds_parse::ParamMode>> = match target {
            Callee::Local(index) => self.planned_param_modes(index),
            Callee::Foreign { def: Some(def), .. } => self
                .analyzed
                .decls
                .fns
                .get(&def)
                .map(|sig| sig.params.iter().map(|param| param.mode).collect()),
            Callee::Foreign { def: None, .. } | Callee::Evidence(_) => match dispatch {
                Some(
                    TraitDispatch::Runtime { method, .. }
                    | TraitDispatch::Foreign {
                        call: TraitCall { method, .. },
                        ..
                    },
                ) => self
                    .analyzed
                    .decls
                    .members
                    .get(method)
                    .map(|sig| sig.params.iter().map(|param| param.mode).collect()),
                _ => None,
            },
            Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. } => None,
        };
        match modes {
            Some(modes) => (0..args)
                .map(|index| matches!(modes.get(index), Some(ds_parse::ParamMode::Consume(_))))
                .collect(),
            None => vec![false; args],
        }
    }

    fn callee_param_words(&mut self, index: u32) -> Option<usize> {
        let planned = self.plan.functions().get(index as usize)?;
        if planned.decl.is_some() {
            return self.decl_param_words(index).map(|words| words.len());
        }
        let def = planned.def?;
        let fn_ty = self.analyzed.checked.def_types.get(&def).copied()?;
        let (params, _) = fn_sig_of(&self.analyzed.decls.universe, fn_ty)?;
        let mut total = 0;
        for ty in params {
            total += self.shapes.words(*ty).ok()?.len();
        }
        Some(total)
    }

    /// Whether one planned function's tail expression is a capturing
    /// lambda — a factory: its fn-typed result is the closure word the
    /// tail constructs, so a caller of it classifies the result slot Ref.
    fn tail_builds_a_closure(&self, index: u32) -> bool {
        let Expr::Lambda { body, .. } = &self.plan.functions()[index as usize].lambda else {
            return false;
        };
        let mut tail = &**body;
        loop {
            match tail {
                Expr::Paren { inner, .. } => tail = inner,
                // a declared fn's body is a block; only the lone
                // expression tail keeps the provenance pure
                Expr::Block(block) => match block.stmts.split_last() {
                    Some((
                        Stmt {
                            kind: StmtKind::Expr(e),
                            ..
                        },
                        [],
                    )) => tail = e,
                    _ => return false,
                },
                _ => break,
            }
        }
        let Expr::Lambda { .. } = tail else {
            return false;
        };
        let res = &self.analyzed.resolutions[tail.span().file.as_u32() as usize];
        !collect_captures(tail, res, &self.analyzed.checked.binders).is_empty()
    }

    /// The result word types a callee's return lands on the overlay as.
    /// A generic's instance-keyed result reads its word kind from the
    /// minted receiver instance (the gate already proved it is one the
    /// body implements), and a result-type-keyed one reads it from the
    /// call's own checked result type (`ty` — the folded element type
    /// `reduce` answers).
    fn callee_result_words(
        &mut self,
        target: Callee,
        instance: Option<BuiltinInstance>,
        ty: Ty,
    ) -> Result<SpanWords, LowerError> {
        match target {
            Callee::Builtin(native) => match native.shape {
                NativeShape::Total(slot) => Ok(Rc::from([slot])),
                // call_impl routes fallible and checked natives through
                // their own tag-test paths; this generic copy never sees
                // them
                NativeShape::Fallible { .. } => Err(mismatch("a fallible native result")),
                NativeShape::Checked => Err(mismatch("a checked-read native result")),
                NativeShape::TotalByInstance => Err(mismatch("an instance-keyed native result")),
                // the call's own checked result type's words (`chan_recv`
                // answering the payload type's one word)
                NativeShape::TotalByResultType => self.shapes.words(ty),
                NativeShape::TaglessTotal => Err(mismatch("a tag-less-total native result")),
            },
            Callee::Generic(generic) => match generic.shape {
                NativeShape::Total(slot) => Ok(Rc::from([slot])),
                NativeShape::TotalByInstance => match instance {
                    Some(BuiltinInstance::Int) => Ok(Rc::from([SlotType::I64])),
                    Some(BuiltinInstance::Float) => Ok(Rc::from([SlotType::F64])),
                    _ => Err(mismatch("an instance-keyed generic result")),
                },
                // the tag-less total answers the call's own result
                // type's words with no discriminant (partition's
                // two-list tuple: two Ref words the destructuring binds)
                NativeShape::TotalByResultType | NativeShape::TaglessTotal => self.shapes.words(ty),
                NativeShape::Fallible { .. } | NativeShape::Checked => {
                    Err(mismatch("a generic builtin result"))
                },
            },
            Callee::Evidence(_) | Callee::Foreign { .. } | Callee::Bound { .. } => {
                self.shapes.words(ty)
            },
            Callee::Local(index) => {
                let planned = &self.plan.functions()[index as usize];
                if planned.free_generic {
                    // the ONE body returns the type variable's single
                    // Raw word, so the site's solved answer must span
                    // one word too when the declared return is the
                    // variable — the same boundary the arguments guard
                    // (a trait's declared default body carries its solved
                    // return on the plan entry; no def signature names it)
                    let words = self.shapes.words(ty)?;
                    let declared = planned
                        .def
                        .and_then(|def| self.analyzed.checked.def_types.get(&def).copied())
                        .and_then(|fn_ty| fn_sig_of(&self.analyzed.decls.universe, fn_ty))
                        .map(|(_, ret)| ret)
                        .or(planned.ret);
                    if declared.is_some_and(|ret| self.is_var_ty(ret)) {
                        self.guard_one_word_instantiation(
                            self.members(ty),
                            "a generic call's result",
                            no_span(), // re-pointed at the call site
                        )?;
                    }
                    return Ok(words);
                }
                if let Some(ret) = planned.ret {
                    let words = self.shapes.words(ret)?;
                    if *words == [SlotType::Fn] && self.tail_builds_a_closure(index) {
                        return Ok(Rc::from([SlotType::Ref]));
                    }
                    return Ok(words);
                }
                let fn_ty = match planned.def {
                    Some(def) => self.analyzed.checked.def_types.get(&def).copied(),
                    None => self
                        .analyzed
                        .checked
                        .expr_types
                        .get(&planned.lambda.span())
                        .map(|val| val.ty),
                }
                .ok_or_else(|| mismatch("a callee signature"))?;
                let Some((_, ret)) = fn_sig_of(&self.analyzed.decls.universe, fn_ty) else {
                    return Err(mismatch("a callee signature"));
                };
                let words = self.shapes.words(ret)?;
                // A factory's fn-typed result is the closure its tail
                // lambda constructs — the one provenance this frame can
                // prove from the callee alone. Every other fn-typed
                // result keeps the scalar classification (a closure
                // returned through such a callee traps at the call — the
                // recorded return-flow seam).
                if *words == [SlotType::Fn] && self.tail_builds_a_closure(index) {
                    return Ok(Rc::from([SlotType::Ref]));
                }
                Ok(words)
            },
        }
    }

    /// One fallible native call: the two-word platform span (discriminant
    /// at overlay word 0, payload at word 1) lands like any call result.
    /// The success-typed union spelling types the call site as the
    /// failure union itself (`string | io_err`, `int | parse_err`), so
    /// the span re-spans into it per member over the error-class mapping
    /// ([`fn classed_native_union`](Self::classed_native_union)); a total
    /// spelling (the ambient's `fs_list`/`write_file` rows) extracts the
    /// payload into the call's result temp, and failure prints
    /// `<prefix><detail>` through println and halts on the discriminant
    /// — the M3 stand-in for the unhandled-failure handler the `error`
    /// row has no source syntax for.
    fn fallible_native_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        fail_payload: SlotType,
        err_prefix: &'static str,
        classes: &'static [(u8, &'static str)],
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if matches!(&*self.shapes.shape(ty)?, Shape::Union(_)) {
            return self.classed_native_union(native, ty, payload, fail_payload, classes, staged);
        }
        self.fallible_total_result(native, ty, payload, fail_payload, err_prefix, staged)
    }

    /// The total spelling of a fallible native's span: extracts the
    /// payload into the call's result temp, and failure prints
    /// `<prefix><detail>` through println and halts on the discriminant.
    /// The failure detail is the span's word 1: the message `Ref` of an
    /// io/net class, or the parse reader's position `I64`, rendered
    /// through the `str_int` native before the concatenation.
    fn fallible_total_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        fail_payload: SlotType,
        err_prefix: &'static str,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if *self.shapes.words(ty)? != [payload] {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the checked result type's words and the native's payload disagree; list values \
                 have no runtime shape yet",
            ));
        }
        self.overlay_fallible(staged, payload, fail_payload);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        let ok_lbl = self.label();
        let fail_lbl = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        self.bind_label(fail_lbl);
        let discriminant = self.temp_word(SlotType::Tag)?;
        self.emit(Op::Mov {
            dst: Slot::body(discriminant),
            src: Slot::overlay(0),
        });
        let prefix = self.temp_word(SlotType::Ref)?;
        let kidx = self.pool_str(err_prefix);
        self.kstr(prefix, kidx);
        let message = self.temp_word(SlotType::Ref)?;
        if fail_payload == SlotType::Ref {
            self.emit(Op::Mov {
                dst: Slot::body(message),
                src: Slot::overlay(1),
            });
            self.rc.produced(message);
        } else {
            // the parse reader's failure word is the position `I64`:
            // render it through the str_int native before concatenating
            self.emit(Op::Mov {
                dst: Slot::overlay(0),
                src: Slot::overlay(1),
            });
            let render = crate::native::lookup("str_int").expect("the int renderer is covered");
            let kidx = self.pool(self.funcref_const(Callee::Builtin(render)));
            self.rc.native_open();
            self.emit(Op::Call {
                callee: kidx,
                nargs: 1,
            });
            self.rc.native_done(&mut self.code);
            self.emit(Op::Mov {
                dst: Slot::body(message),
                src: Slot::overlay(0),
            });
            self.rc.produced(message);
        }
        let text = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Data {
            op: DataOp::StrCat,
            dst: Slot::body(text),
            lhs: Slot::body(prefix),
            rhs: Slot::body(message),
        });
        self.rc.read(&mut self.code, &[prefix, message]);
        self.rc.produced_strcat(text);
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(text),
        });
        self.rc.native_open();
        self.rc.staged_native(text, false);
        let println = crate::native::lookup("println").expect("the console native is covered");
        let kidx = self.pool(self.funcref_const(Callee::Builtin(println)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 1,
        });
        self.rc.native_done(&mut self.code);
        // program end: HALT keeps its references — the heap dies whole
        self.emit(Op::Halt {
            slot: Slot::body(discriminant),
        });
        self.bind_label(ok_lbl);
        let base = self.alloc(&[payload])?;
        self.emit(Op::Mov {
            dst: Slot::body(base),
            src: Slot::overlay(1),
        });
        if payload == SlotType::Ref {
            self.rc.produced(base);
        }
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    /// The unit-value member: the arity-0 tuple atom [B3] — the `()` of
    /// a classed fallible native's unit-word success member, distinct
    /// from the payload-less class members (the unit tags).
    fn is_unit_member(&self, atom: TypeId) -> bool {
        matches!(
            self.analyzed.decls.universe.atom_decl(atom),
            Ok(decl) if matches!(&decl.kind, AtomKind::Tuple(elements) if elements.is_empty())
        )
    }

    /// One fallible native call over the success-typed union spelling:
    /// the checked call type is the failure union itself (`string |
    /// io_err`, `int | parse_err`), so the two-word platform span
    /// re-spans into it — success constructs the payload member from
    /// overlay word 1, failure the class member the discriminant's
    /// error-class mapping names (each discriminant paired with the
    /// ambient failure-taxonomy member it lands on; a bare member adopts
    /// no word, a one-field member adopts word 1 as its payload — the
    /// `io_other`/`net_other` message `Ref`, the `parse_err` position
    /// `I64`). The union must carry every class member plus exactly one
    /// success member holding the native's one payload word (the unit
    /// value over the native's inert unit word, `net_send`'s `()`) —
    /// the same span-to-union discipline the checked reads use
    /// ([`fn checked_native_result`](Self::checked_native_result)). A
    /// bare class member's native-born message count dies inside its
    /// failure arm; a scalar failure word is inert.
    fn classed_native_union(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        fail_payload: SlotType,
        classes: &'static [(u8, &'static str)],
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a fallible native's union result"));
        };
        // the class members the mapping names, each bare or carrying
        // exactly the failure payload's one word
        let mut class_members: Vec<(u8, &MemberShape)> = Vec::new();
        for (tag, name) in classes {
            let Some(atom) = self.analyzed.decls.universe.atom_by_name(name) else {
                let _ = name;
                return Err(LowerError::unsupported(
                    no_span(),
                    "a fallible native call",
                    "the union must carry the failure taxonomy's members by their ambient names; \
                     a differently named error set has no class mapping",
                ));
            };
            let Some(member) = u.member(atom) else {
                return Err(LowerError::unsupported(
                    no_span(),
                    "a fallible native call",
                    "the union must carry the failure taxonomy's whole member set; every class \
                     member the native answers must be a member",
                ));
            };
            match member.fields.as_slice() {
                [] => {},
                [field] if field.slot() == fail_payload => {},
                _ => {
                    return Err(LowerError::unsupported(
                        no_span(),
                        "a fallible native call",
                        "every class member is a bare tag, or carries exactly the class's one \
                         failure payload word (the *_other message, the parse position)",
                    ));
                },
            }
            class_members.push((*tag, member));
        }
        let success: Vec<_> = u
            .members
            .iter()
            .filter(|m| !class_members.iter().any(|(_, c)| c.atom == m.atom))
            .filter(|m| m.fields.len() == 1 && m.fields[0].slot() == payload)
            .collect();
        let mut success = success;
        if success.is_empty() && payload == SlotType::I64 {
            success = u
                .members
                .iter()
                .filter(|m| !class_members.iter().any(|(_, c)| c.atom == m.atom))
                .filter(|m| self.is_unit_member(m.atom))
                .collect();
        }
        let [ok] = success.as_slice() else {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union's success member must be exactly the native's one payload word, or the \
                 unit value over the native's inert unit word",
            ));
        };
        if success.len() + class_members.len() != u.members.len() {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union must be exactly the success member plus the failure taxonomy's class \
                 members; a wider union has no member mapping for the native's failure classes",
            ));
        }
        self.overlay_site(staged, &[SlotType::Tag, payload]);
        // the failure path lands its class payload in word 1 — a Ref
        // message for the io/net classes, a position I64 for the parse
        // reader — so the word's classification mixes
        self.overlay_len = self.overlay_len.max(2);
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        self.overlay[1].insert(fail_payload);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        let result = self.temp_span(ty)?;
        let ok_lbl = self.label();
        let join = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork dispatch: every class but the last jumps to its own
        // arm, the last class is the fall-through failure
        let total = class_members.len();
        let mut class_lbls = Vec::with_capacity(total.saturating_sub(1));
        for (tag, _) in &class_members[..total.saturating_sub(1)] {
            let cond = self.temp_word(SlotType::Bool)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::overlay(0),
                tag: *tag,
            });
            let lbl = self.label();
            class_lbls.push(lbl);
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: lbl,
            });
        }
        // the failure arms in code order: the fall-through last class
        // first, then every earlier class at its dispatch label
        let base = self.rc.save();
        let mut arm_jmps: Vec<usize> = Vec::with_capacity(total);
        let mut arm_states: Vec<State> = Vec::with_capacity(total);
        for (index, (_, member)) in class_members.iter().enumerate().rev() {
            if index != total - 1 {
                self.bind_label(class_lbls[index]);
            }
            let tag = self.shapes.tag_id(member.atom)?;
            self.emit(Op::TagNew {
                dst: Slot::body(result.base),
                tag,
            });
            self.zero_init_member(result.base, &u.words, &member.fields)?;
            match member.fields.as_slice() {
                // the class's own payload word: the message Ref or the
                // position I64, adopted off the overlay into the field
                [field] => {
                    self.emit(Op::Mov {
                        dst: Slot::body(result.base + field.word),
                        src: Slot::overlay(1),
                    });
                    if field.slot() == SlotType::Ref {
                        self.rc.produced(result.base + field.word);
                    }
                },
                // a bare member: the native-born message count has no
                // member field to live in, so it dies inside the arm
                _ => {
                    if fail_payload == SlotType::Ref {
                        let message = self.temp_word(SlotType::Ref)?;
                        self.emit(Op::Mov {
                            dst: Slot::body(message),
                            src: Slot::overlay(1),
                        });
                        self.rc.produced(message);
                        self.rc.discard(&mut self.code, &[message]);
                    }
                },
            }
            arm_jmps.push(self.code.len());
            arm_states.push(self.rc.take());
            self.emit(Op::Jmp { target: join });
            self.rc.restore(base.clone());
        }
        self.bind_label(ok_lbl);
        let tag = self.shapes.tag_id(ok.atom)?;
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &ok.fields)?;
        if let [field] = ok.fields.as_slice() {
            self.emit(Op::Mov {
                dst: Slot::body(result.base + field.word),
                src: Slot::overlay(1),
            });
            if field.slot() == SlotType::Ref {
                self.rc.produced(result.base + field.word);
            }
        }
        let arm_ok = self.rc.take();
        let mut arms: Vec<(State, bool)> =
            arm_states.into_iter().map(|state| (state, false)).collect();
        arms.push((arm_ok, false));
        let dying = self.rc.reconcile(&arms);
        // splice each failure arm's dying drops before its jump to the
        // join, compensating the shifts the earlier splices introduced
        let mut shift = 0;
        for (jmp, drops) in arm_jmps.iter().zip(&dying) {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, drops);
            let len = epilogue.len();
            self.splice(epilogue, jmp + shift);
            shift += len;
        }
        self.arm_epilogue(dying.last().expect("the ok arm's dying set"));
        self.bind_label(join);
        Ok(result)
    }

    /// Records the words a fallible native's result span lands on: word 0
    /// the discriminant plus the failure handler's println argument, word
    /// 1 the success payload plus the failure class's payload (the
    /// message `Ref`, or the parse reader's position `I64` — the render
    /// staging it back through word 0 when it is the position).
    fn overlay_fallible(&mut self, staged: &[SlotType], payload: SlotType, fail_payload: SlotType) {
        self.overlay_site(staged, &[SlotType::Tag, payload]);
        self.overlay_len = self.overlay_len.max(2);
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        self.overlay[0].insert(SlotType::Ref);
        self.overlay[0].insert(fail_payload);
        self.overlay[1].insert(fail_payload);
    }

    /// One checked-read native call (`list_get`, `map_get`): the ok/none
    /// span lands like a fallible native's, but neither side is a
    /// failure — it re-spans into the call's own `t | none` union: ok
    /// writes the element member with the payload's whole word span (the
    /// native channel: one overlay word per element word, a count per
    /// `Ref` word produced), none the predeclared unit member, so `?`,
    /// `.ok`, and match see an ordinary optional value. A one-atom
    /// element constructs its member (a tag write plus the payload
    /// words); a multi-atom element — an enum inside the optional, every
    /// case atom a member of the call's result union — hands its whole
    /// span back verbatim, the element's own tag word already naming a
    /// result member, admitted when the element's own union layout
    /// equals the result union's (the same atoms claiming the same
    /// region words in the same order). The rejected index the none side
    /// carries has no member field to surface in, so it drops. The
    /// generic spelling routes the same span through CALLG, the evidence
    /// words staged behind the real arguments.
    fn checked_native_result(
        &mut self,
        target: Callee,
        ty: Ty,
        staged: &[SlotType],
        real_args: usize,
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(LowerError::unsupported(
                no_span(),
                "a checked-read native call",
                "the call's result type must be the `t | none` union",
            ));
        };
        let none_member = u
            .member(TypeId::NONE)
            .ok_or_else(|| mismatch("a checked read's none member"))?;
        let element = set_diff(self.members(ty), &[TypeId::NONE]);
        // the ok side's construction: a one-atom element's member entry,
        // or the multi-atom element's verbatim span copy
        let elem_member = match element.as_slice() {
            [] => {
                return Err(LowerError::unsupported(
                    no_span(),
                    "a checked-read native call",
                    "the element type must be one atom; narrow list elements before the read",
                ));
            },
            [elem] => Some(
                u.member(*elem)
                    .ok_or_else(|| mismatch("a checked read's element member"))?,
            ),
            many => {
                let words = shape_words(&self.shapes.members_shape(many, 0)?);
                if words != u.words {
                    return Err(LowerError::unsupported(
                        no_span(),
                        "a checked-read native call",
                        "the element union's own span disagrees with the call's result union; the \
                         verbatim hand-back needs the element's tag-and-region layout to equal \
                         the result's, and a member the element does not carry widened it",
                    ));
                }
                None
            },
        };
        // the ok payload's flat word span — the member's fields flattened
        // in declaration order, or the multi-atom element's whole span
        // (its own tag word first): the native answers the element's
        // whole span, one overlay word per element word
        let elem_words: Vec<SlotType> = match elem_member {
            Some(member) => member
                .fields
                .iter()
                .flat_map(|field| field.slots.iter().copied())
                .collect(),
            None => u.words.clone(),
        };
        let mut result_words = vec![SlotType::Tag];
        result_words.extend_from_slice(&elem_words);
        self.guard_staged_count(staged.len().max(result_words.len()))?;
        self.overlay_site(staged, &result_words);
        let kidx = self.pool(self.funcref_const(target));
        match target {
            // the staged span of a generic call ends in its evidence
            // words; nargs names the real arguments, the constraint list
            // carries the evidence count
            Callee::Generic(_) => {
                self.emit(Op::Callg {
                    callee: kidx,
                    nargs: u8::try_from(real_args).expect("argument count fits u8"),
                });
            },
            Callee::Builtin(_) => {
                self.emit(Op::Call {
                    callee: kidx,
                    nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
                });
            },
            Callee::Local(_)
            | Callee::Evidence(_)
            | Callee::Foreign { .. }
            | Callee::Bound { .. } => {
                return Err(mismatch("a checked-read callee"));
            },
        }
        self.rc.native_done(&mut self.code);
        let result = self.temp_span(ty)?;
        let ok_lbl = self.label();
        let join = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: none and ok each write the span with their own value
        let base = self.rc.save();
        let tag = self.shapes.tag_id(TypeId::NONE)?;
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &none_member.fields)?;
        let arm_none = self.rc.take();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        match elem_member {
            Some(member) => {
                let tag = self.shapes.tag_id(member.atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(result.base),
                    tag,
                });
                self.zero_init_member(result.base, &u.words, &member.fields)?;
                // the payload copies from the flat overlay span into the member's
                // claimed words (fields may claim scattered region offsets; the
                // flat index walks the element's declaration order)
                let mut flat = 0usize;
                for field in &member.fields {
                    for (offset, slot) in field.slots.iter().enumerate() {
                        let word = result.base
                            + field.word
                            + u8::try_from(offset).expect("offset fits u8");
                        self.emit(Op::Mov {
                            dst: Slot::body(word),
                            src: Slot::overlay(
                                u8::try_from(1 + flat + offset).expect("overlay word fits u8"),
                            ),
                        });
                        if *slot == SlotType::Ref {
                            self.rc.produced(word);
                        }
                    }
                    flat += field.slots.len();
                }
            },
            // the multi-atom element: the native's payload IS the
            // element's whole span, its tag word first, and every word
            // copies verbatim into the result's own span — the same
            // atoms claimed the same region, so word `i` of the payload
            // is word `i` of the result
            None => {
                for (index, slot) in u.words.iter().enumerate() {
                    let word = result.base + u8::try_from(index).expect("span width fits u8");
                    self.emit(Op::Mov {
                        dst: Slot::body(word),
                        src: Slot::overlay(u8::try_from(1 + index).expect("overlay word fits u8")),
                    });
                    if *slot == SlotType::Ref {
                        self.rc.produced(word);
                    }
                }
            },
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_none, false), (arm_ok, false)]);
        if let [none, ok] = &dying[..] {
            // a dead Ref payload word keeps its zero-init claim: the miss
            // path decs its interned empty-string reference here (the
            // word is dead by construction — the live member's words are
            // the only ones ever read [BL-2])
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, none);
            self.splice(epilogue, none_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.err` handler: tag tests for the handled members jump to an
    /// inlined handler body; the success value widens into the result.
    fn err_value(&mut self, recv: &Expr, lambda: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let Expr::Lambda { params, body, .. } = lambda else {
            return Err(LowerError::unsupported(
                lambda.span(),
                "a non-lambda handler",
                "`.err` takes a lambda that inlines at the call site",
            ));
        };
        let handled = set_diff(self.members(src_ty), self.members(ty));
        let result = self.temp_span(ty)?;
        let handler_lbl = self.label();
        let join = self.label();
        for member in &handled {
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: handler_lbl,
            });
        }
        // success: the kept members re-span into the result over the
        // operand's real layout — an operand's raw span never lands
        // directly (union regions are per-union-type), and the kept
        // set's region can be narrower than the operand's when a
        // handled member contributed a word shape the kept set does
        // not need
        let base = self.rc.save();
        let kept = set_diff(self.members(src_ty), &handled);
        // every member handled: the fall-through is dead code, so its
        // arm never reaches the join — the handler's answer is the
        // result's only writer and must survive the reconcile instead
        // of dying at the join as one arm's divergent count
        let flow_dead = kept.is_empty();
        if kept.len() == 1 && !self.is_span_ty(ty) {
            // the result is the one kept atom itself: claiming that
            // atom is honest — the word-address path extracts the
            // member's payload words out of the operand's layout
            self.widen_into(
                &result,
                ty,
                &TypedPlace {
                    place: src.place.clone(),
                    ty,
                },
                recv.span(),
            )?;
        } else {
            let labels: Vec<usize> = kept.iter().map(|_| self.label()).collect();
            for (member, label) in kept.iter().zip(&labels) {
                let cond = self.temp_word(SlotType::Bool)?;
                let src_word = self.tag_addr(&src.place)?;
                let tag = self.shapes.tag_id(*member)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src_word),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: *label,
                });
            }
            // dead fallthrough: the handled tests above cover every
            // other member of the operand's type
            self.emit(Op::Jmp { target: join });
            for (member, label) in kept.iter().zip(labels) {
                self.bind_label(label);
                self.respan_into(&result, ty, &src.place, *member)?;
            }
        }
        let arm_ok = self.rc.take();
        let ok_div = self.rc.diverged();
        let ok_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(handler_lbl);
        {
            self.scope_push();
            let [param] = params.as_slice() else {
                return Err(mismatch("a handler parameter"));
            };
            if !matches!(param.mode, ds_parse::ParamMode::Borrow) {
                return Err(LowerError::unsupported(
                    param.span,
                    "a mode on a handler parameter",
                    "the inlined handler binds its parameter as an alias of the payload word; the \
                     extraction seam owns no claim hand-off yet",
                ));
            }
            self.binders.insert(param.name.span, Place {
                base: src.place.base,
                shape: src.place.shape.clone(),
            });
            let param_ty = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .map_or(src_ty, |f| f.ty);
            self.rc_bind_borrowed(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            self.refuse_fn_word_answer(
                ty,
                &body_tp,
                body.span(),
                "a capturing closure in a handler's answer",
                "an unproven callable in a handler's answer",
            )?;
            self.widen_into(&result, ty, &body_tp, body.span())?;
            self.scope_pop();
        }
        let arm_err = self.rc.take();
        let err_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_ok, ok_div || flow_dead), (arm_err, err_div)]);
        if let [ok, err] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, ok);
            self.splice(epilogue, ok_jmp);
            self.arm_epilogue(err);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.ok` handler: the mirror of `.err` — the ok-side members
    /// jump to an inlined handler body, and each remainder member
    /// re-spans into the result, which carries the difference side
    /// composed with the handler's own result.
    pub(super) fn ok_value(
        &mut self,
        recv: &Expr,
        lambda: &Expr,
        ty: Ty,
    ) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let Expr::Lambda { params, body, .. } = lambda else {
            return Err(LowerError::unsupported(
                lambda.span(),
                "a non-lambda handler",
                "`.ok` takes a lambda that inlines at the call site",
            ));
        };
        let [param] = params.as_slice() else {
            return Err(mismatch("a handler parameter"));
        };
        if !matches!(param.mode, ds_parse::ParamMode::Borrow) {
            return Err(LowerError::unsupported(
                param.span,
                "a mode on a handler parameter",
                "the inlined handler binds its parameter as an alias of the payload word; the \
                 extraction seam owns no claim hand-off yet",
            ));
        }
        let handled = self.ok_side(param, body, src_ty)?;
        let result = self.temp_span(ty)?;
        let handler_lbl = self.label();
        let join = self.label();
        for member in &handled {
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: handler_lbl,
            });
        }
        // the difference side flows into the result; the fall-through and
        // the handler arm fork the ownership map
        let base = self.rc.save();
        let remainder = set_diff(self.members(src_ty), &handled);
        let mut flow_jmps = Vec::new();
        // the dead fallthrough arm never reaches the join (the mirror of
        // err_value's kept-empty arm): the handler's answer is the
        // result's only writer and must survive the reconcile
        let flow_dead = remainder.is_empty();
        if remainder.is_empty() {
            // every member jumps to the handler; the fallthrough is dead
            self.emit(Op::Jmp { target: join });
        } else if self.members(src_ty).len() == 1 {
            // a single-member operand: the value itself flows, no tags
            self.widen_into(
                &result,
                ty,
                &TypedPlace {
                    place: src.place.clone(),
                    ty: src_ty,
                },
                recv.span(),
            )?;
            flow_jmps.push(self.code.len());
            self.emit(Op::Jmp { target: join });
        } else {
            let labels: Vec<usize> = remainder.iter().map(|_| self.label()).collect();
            for (member, label) in remainder.iter().zip(&labels) {
                let cond = self.temp_word(SlotType::Bool)?;
                let src_word = self.tag_addr(&src.place)?;
                let tag = self.shapes.tag_id(*member)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src_word),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: *label,
                });
            }
            // dead: the tests cover every member of the operand's type
            self.emit(Op::Jmp { target: join });
            for (member, label) in remainder.iter().zip(labels) {
                self.bind_label(label);
                self.respan_into(&result, ty, &src.place, *member)?;
                flow_jmps.push(self.code.len());
                self.emit(Op::Jmp { target: join });
            }
        }
        self.bind_label(handler_lbl);
        let arm_flow = self.rc.take();
        let flow_div = self.rc.diverged();
        self.rc.restore(base);
        {
            self.scope_push();
            self.binders.insert(param.name.span, Place {
                base: src.place.base,
                shape: src.place.shape.clone(),
            });
            let param_ty = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .map_or(src_ty, |f| f.ty);
            self.rc_bind_borrowed(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            self.refuse_fn_word_answer(
                ty,
                &body_tp,
                body.span(),
                "a capturing closure in a handler's answer",
                "an unproven callable in a handler's answer",
            )?;
            self.widen_into(&result, ty, &body_tp, body.span())?;
            self.scope_pop();
        }
        let arm_ok = self.rc.take();
        let ok_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_flow, flow_div || flow_dead), (arm_ok, ok_div)]);
        if let [flow, ok] = &dying[..] {
            let mut shift = 0;
            for at in &flow_jmps {
                let mut epilogue = Vec::new();
                Placement::emit_drops(&mut epilogue, flow);
                let len = epilogue.len();
                self.splice(epilogue, at + shift);
                shift += len;
            }
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.ok` handler's ok side, the checker's handled-set rule: the
    /// annotated parameter's members, or — unannotated — everything the
    /// handler's own result does not keep of the operand. Foreign
    /// annotated members never test at runtime, so they drop here.
    fn ok_side(
        &mut self,
        param: &Param,
        body: &Expr,
        src_ty: Ty,
    ) -> Result<Vec<TypeId>, LowerError> {
        let src_members = self.members(src_ty);
        if param.ty.is_some() {
            let fact = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .copied()
                .ok_or_else(|| mismatch("a handler parameter"))?;
            let annotated = self.members(fact.ty);
            return Ok(src_members
                .iter()
                .copied()
                .filter(|m| annotated.contains(m))
                .collect());
        }
        let body_members = self.members(self.val(body.span())?.ty);
        Ok(src_members
            .iter()
            .copied()
            .filter(|m| !body_members.contains(m))
            .collect())
    }
}

/// One embedded scalar answer's literal expression, the node a user
/// would have written at `span` (the compound answers lower through
/// their own paths in `embedded_value`, so a compound here is the belt
/// refusal).
fn embedded_const_expr(constant: &crate::ComptimeConst, span: Span) -> Result<Expr, LowerError> {
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

/// The C words a written parameter type marshals (H5): the scalar words
/// one, a `string` or `bytes` parameter its call-scoped (pointer,
/// length) pair — two. `None` for every type outside the marshaling
/// slice (handles, aggregates, type variables), the same slice the
/// callee resolution validated.
pub(super) fn marshal_word(ty: &Type) -> Option<usize> {
    if scalar_word(ty).is_some() {
        return Some(1);
    }
    let Type::Path { path, args, .. } = ty else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let [one] = &path.segments[..] else {
        return None;
    };
    match one.name.as_str() {
        "string" | "bytes" => Some(2),
        _ => None,
    }
}

/// The return's marshaling word a written return type names (H5): the
/// scalar words their own, anything else none — the unit return rides
/// the absent word the caller spells. Shared with the pool constant's
/// return payload, the same slice the callee resolution validated.
pub(super) fn bound_ret_word(ty: &Type) -> Option<BoundRet> {
    match scalar_word(ty)? {
        SlotType::I64 => Some(BoundRet::Int),
        SlotType::Bool => Some(BoundRet::Bool),
        SlotType::F64 => Some(BoundRet::Float),
        _ => None,
    }
}

/// The scalar word a written type names, when it is one of the
/// marshaling slice's atoms (H5): `int` the i64 word, `bool` the bool
/// word, `float` the f64 word. Shared with the pool constant's return
/// payload, the same slice the callee resolution validated.
pub(super) fn scalar_word(ty: &Type) -> Option<SlotType> {
    let Type::Path { path, args, .. } = ty else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let [one] = &path.segments[..] else {
        return None;
    };
    match one.name.as_str() {
        "int" => Some(SlotType::I64),
        "bool" => Some(SlotType::Bool),
        "float" => Some(SlotType::F64),
        _ => None,
    }
}
