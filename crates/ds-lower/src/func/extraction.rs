//! The extraction family [E4]: monadic extraction over optional/result
//! unions, the ambient `str`/`cancel` conversions, and `?` in value and
//! tail position.

use super::*;

impl FnLower<'_, '_, '_> {
    // ---- the extraction family [E4] ------------------------------------------

    /// One extraction-family call [E4]: every member inlines at the call
    /// site over the checked receiver's own union shape — a tag test
    /// picks the arm, the chosen side writes the result. No callee
    /// exists, so a tail position lowers the produced place through the
    /// value walk's ordinary return.
    pub(super) fn extraction_value(
        &mut self,
        kind: Extraction,
        args: &[&Expr],
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let [recv, arg] = args else {
            return Err(mismatch("an extraction-family call's arguments"));
        };
        match kind {
            Extraction::UnwrapOr => self.unwrap_or_value(recv, arg, ty, span),
            Extraction::UnwrapOrElse => self.unwrap_or_else_value(recv, arg, ty, span),
            Extraction::OkOr => self.ok_or_value(recv, arg, ty, span),
            // `map_err` is the `.ok` handler over a named spelling: the
            // handled members jump to the inlined lambda, the remainder
            // re-spans, the handler's result unions in.
            Extraction::MapErr => self.ok_value(recv, arg, ty),
        }
    }

    /// The family's receiver decomposition: the checked `t | none`
    /// union's none member and its extracted t side's atom. The side's
    /// words may span any width — a struct member's fields claim their
    /// region spans — and the consumers gather them through
    /// [`fn extracted_words`](Self::extracted_words).
    fn optional_side(&mut self, recv_ty: Ty, span: Span) -> Result<TypeId, LowerError> {
        let shape = self.shapes.shape(recv_ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(LowerError::unsupported(
                span,
                "an extraction-family receiver",
                "the receiver must be the `t | none` union",
            ));
        };
        if u.member(TypeId::NONE).is_none() {
            return Err(mismatch("an optional receiver's none member"));
        }
        let diff = set_diff(self.members(recv_ty), &[TypeId::NONE]);
        let [elem] = diff.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an extraction-family receiver",
                "the extracted side must be one atom; narrow the receiver first",
            ));
        };
        Ok(*elem)
    }

    /// The extracted side's word addresses inside the receiver's union
    /// span: the element type's own field map over the span place, so a
    /// member whose fields claim scattered region offsets gathers
    /// correctly — the same discipline the checked read's ok arm copies
    /// by.
    fn extracted_words(&mut self, src: &Place, elem: Ty) -> Result<Vec<WordAddr>, LowerError> {
        self.word_addrs(src, elem)
    }

    /// `x.unwrap_or(y)` [E4]: the eager fallback — both operands evaluate,
    /// the receiver's tag picks the payload word or the fallback's.
    fn unwrap_or_value(
        &mut self,
        recv: &Expr,
        fallback: &Expr,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        self.optional_side(src_ty, span)?;
        let fb = self.value(fallback)?;
        let result = self.temp_span(ty)?;
        let cond = self.temp_word(SlotType::Bool)?;
        let src_word = self.tag_addr(&src.place)?;
        let none_tag = self.shapes.tag_id(TypeId::NONE)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::body(src_word),
            tag: none_tag,
        });
        let ok_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: the fallback and the payload each own the result
        let base = self.rc.save();
        self.refuse_fn_word_answer(
            ty,
            &fb,
            fallback.span(),
            "a capturing closure in unwrap_or's fallback",
            "an unproven callable in unwrap_or's fallback",
        )?;
        self.widen_into(
            &result,
            ty,
            &TypedPlace {
                place: fb.place.clone(),
                ty: fb.ty,
            },
            span,
        )?;
        let arm_none = self.rc.take();
        let none_div = self.rc.diverged();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        // the payload gathers out of the receiver's union span (fields may
        // claim scattered offsets) into the result's contiguous words
        for (index, addr) in self.extracted_words(&src.place, ty)?.iter().enumerate() {
            let word = result.base + u8::try_from(index).expect("span width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.copied(&mut self.code, word, addr.slot);
            }
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_none, none_div), (arm_ok, false)]);
        if let [none, ok] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, none);
            self.splice(epilogue, none_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// `x.unwrap_or_else(fn)` [E4]: the fallback lambda inlines on the
    /// none side only — lazy by construction; the ok side moves the
    /// payload.
    fn unwrap_or_else_value(
        &mut self,
        recv: &Expr,
        fallback: &Expr,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        self.optional_side(src_ty, span)?;
        let Expr::Lambda { params, body, .. } = fallback else {
            return Err(LowerError::unsupported(
                fallback.span(),
                "a non-lambda fallback",
                "`unwrap_or_else` takes a lambda that inlines at the call site",
            ));
        };
        if !params.is_empty() {
            return Err(mismatch("a fallback lambda's parameters"));
        }
        let result = self.temp_span(ty)?;
        let cond = self.temp_word(SlotType::Bool)?;
        let src_word = self.tag_addr(&src.place)?;
        let none_tag = self.shapes.tag_id(TypeId::NONE)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::body(src_word),
            tag: none_tag,
        });
        let ok_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: the inlined lambda and the payload own the result
        let base = self.rc.save();
        self.scope_push();
        let body_tp = self.value(body)?;
        self.refuse_fn_word_answer(
            ty,
            &body_tp,
            body.span(),
            "a capturing closure in unwrap_or_else's fallback",
            "an unproven callable in unwrap_or_else's fallback",
        )?;
        self.widen_into(&result, ty, &body_tp, body.span())?;
        self.scope_pop();
        let arm_none = self.rc.take();
        let none_div = self.rc.diverged();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        // the payload gathers out of the receiver's union span like
        // `unwrap_or`'s ok arm
        for (index, addr) in self.extracted_words(&src.place, ty)?.iter().enumerate() {
            let word = result.base + u8::try_from(index).expect("span width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.copied(&mut self.code, word, addr.slot);
            }
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_none, none_div), (arm_ok, false)]);
        if let [none, ok] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, none);
            self.splice(epilogue, none_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// `x.ok_or(e)` [E4]: the tag test picks the constructed member of
    /// the call's own `t | e` union — the error value on the none side,
    /// the payload on the ok side — which `?` then propagates.
    fn ok_or_value(
        &mut self,
        recv: &Expr,
        err: &Expr,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        // The degenerate spell: the err value's own type sits inside the
        // receiver's extracted side, so the checked `t | e` collapses to
        // the extracted type and both arms answer one value of it —
        // `unwrap_or`'s exact shape, the err value as the fallback.
        if self
            .shapes
            .shape(ty)
            .is_ok_and(|shape| !matches!(*shape, Shape::Union(_)))
        {
            return self.unwrap_or_value(recv, err, ty, span);
        }
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let elem = self.optional_side(src_ty, span)?;
        let ev = self.value(err)?;
        let err_atom = self.only_atom(ev.ty, err.span())?;
        let err_words = self.word_addrs(&ev.place, ev.ty)?;
        // the err arm stages its words directly into the result member's
        // fields, so the union payload's provenance guard runs here
        // instead of the widening write
        self.refuse_union_fn_payload(&ev, err.span())?;
        let result = self.temp_span(ty)?;
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("an ok_or result union"));
        };
        // the payload's gathered addresses inside the receiver's span: the
        // elem member's fields, scattered region offsets included
        let src_shape = self.shapes.shape(src_ty)?;
        let Shape::Union(src_u) = &*src_shape else {
            return Err(mismatch("an ok_or receiver union"));
        };
        let src_elem = src_u
            .member(elem)
            .ok_or_else(|| mismatch("an ok_or receiver's extracted member"))?;
        let elem_words: Vec<WordAddr> = src_elem
            .fields
            .iter()
            .flat_map(|field| field_addrs(src.place.base, field))
            .collect();
        let none_tag = self.shapes.tag_id(TypeId::NONE)?;
        let err_tag = self.shapes.tag_id(err_atom)?;
        let elem_tag = self.shapes.tag_id(elem)?;
        let err_member = u
            .member(err_atom)
            .ok_or_else(|| mismatch("an ok_or error member"))?;
        let elem_dst = u
            .member(elem)
            .ok_or_else(|| mismatch("an ok_or result's extracted member"))?;
        let cond = self.temp_word(SlotType::Bool)?;
        let src_word = self.tag_addr(&src.place)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::body(src_word),
            tag: none_tag,
        });
        let ok_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: the constructed error member and the payload
        let base = self.rc.save();
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag: err_tag,
        });
        self.zero_init_member(result.base, &u.words, &err_member.fields)?;
        let srcs = ordered_field_srcs(&err_words, &err_member.fields)?;
        for (dst_field, srcs) in err_member.fields.iter().zip(srcs) {
            self.store_field_span(result.base, dst_field, srcs);
        }
        let arm_err = self.rc.take();
        let err_div = self.rc.diverged();
        let err_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag: elem_tag,
        });
        self.zero_init_member(result.base, &u.words, &elem_dst.fields)?;
        // the payload stores per field like the err arm's, gathered out of
        // the receiver's span (scattered offsets name-match by field)
        let srcs = ordered_field_srcs(&elem_words, &elem_dst.fields)?;
        for (dst_field, srcs) in elem_dst.fields.iter().zip(srcs) {
            self.store_field_span(result.base, dst_field, srcs);
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_err, err_div), (arm_ok, false)]);
        if let [err, ok] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, err);
            self.splice(epilogue, err_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// `e?`: every propagated member re-spans into the enclosing return
    /// union via RETSPAN [E4]; the kept set extracts in place. Each exit
    /// forks the ownership map — the exit's own drops fire from its
    /// snapshot, and the continuation keeps the kept set's counts (the
    /// exits diverge, so they constrain nothing after the `?`).
    pub(super) fn try_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::Try { expr, .. } = e else {
            return Err(mismatch("a `?` propagation"));
        };
        let src = self.value(expr)?;
        let src_ty = self.val(expr.span())?.ty;
        let propagated = set_diff(self.members(src_ty), self.members(ty));
        let continue_lbl = self.label();
        let mut exits = Vec::with_capacity(propagated.len());
        for member in &propagated {
            let exit = self.label();
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
                target: exit,
            });
            exits.push(exit);
        }
        self.emit(Op::Jmp {
            target: continue_lbl,
        });
        let base = self.rc.save();
        for (member, exit) in propagated.iter().zip(exits) {
            self.bind_label(exit);
            self.respan_exit(&src.place, *member)?;
            // the exit dropped from its own fork of the post-call state;
            // the next exit forks from the same place
            self.rc.restore(base.clone());
        }
        self.bind_label(continue_lbl);
        // the kept set: one member extracts to its payload word
        self.extract(src.place, ty)
    }

    /// A `?` in tail position: the checker proved the operand union is
    /// contained in the return union, so every member returns through
    /// the return itself — one member is a plain return, several
    /// dispatch by tag and each re-spans into the return union, leaving
    /// no live fall-through.
    pub(super) fn try_tail(&mut self, operand: &Expr) -> Result<(), LowerError> {
        let src = self.value(operand)?;
        let src_ty = self.val(operand.span())?.ty;
        let members = self.members(src_ty);
        match members {
            [] => Err(mismatch("a `?` over an empty operand union")),
            [_] => self.emit_return(&src.place, src_ty, operand.span()),
            _ => {
                let mut exits = Vec::with_capacity(members.len());
                for member in members {
                    let exit = self.label();
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
                        target: exit,
                    });
                    exits.push(exit);
                }
                for (member, exit) in members.iter().zip(exits) {
                    self.bind_label(exit);
                    self.respan_exit(&src.place, *member)?;
                }
                Ok(())
            },
        }
    }

    /// The kept set of a `?` extraction: one member addresses its
    /// payload word; several keep the whole span.
    fn extract(&mut self, place: Place, ty: Ty) -> Result<Place, LowerError> {
        if self.members(ty).len() == 1 {
            let addrs = self.word_addrs(&place, ty)?;
            let [only] = addrs.as_slice() else {
                return Err(mismatch("an extracted member"));
            };
            Ok(Place {
                base: only.slot,
                shape: PlaceShape::Word(only.slot_ty),
            })
        } else {
            Ok(place)
        }
    }

    /// The extraction-family member a callee def addresses, when the
    /// ambient's own shape carries it: a non-local function of a family
    /// name, generic with two parameters — the same name-routing the
    /// native table performs, gated to match the checker's rule so the
    /// two passes never disagree [E4].
    /// Whether `def` is the ambient `str[t]` conversion extern: one
    /// generic over the converted value, one parameter. The lowering
    /// routes the call by the operand's checked type, mirroring the
    /// interpolation hole.
    pub(super) fn ambient_str_def(&self, def: DefId) -> bool {
        self.plan.index_of(def).is_none()
            && self.analyzed.package.def(def).name == "str"
            && self
                .analyzed
                .decls
                .fns
                .get(&def)
                .is_some_and(|sig| sig.generics.len() == 1 && sig.params.len() == 1)
    }

    /// Whether `def` is the ambient `cancel` extern [E1, E2]: the generic
    /// one-parameter declaration the checker rows with the `cancel`
    /// effect — the internal effect the nursery boundary dispatches.
    pub(super) fn ambient_cancel_def(&self, def: DefId) -> bool {
        self.plan.index_of(def).is_none()
            && self.analyzed.package.def(def).name == "cancel"
            && self
                .analyzed
                .decls
                .fns
                .get(&def)
                .is_some_and(|sig| sig.generics.len() == 1 && sig.params.len() == 1)
    }

    /// The `cancel(e)` call [E1, E2]: the operand is one bare enum member
    /// value — the failing fiber's error payload — contained in the
    /// enclosing function's return union, where the nursery boundary
    /// unions it out at the join. The seam traps the running fiber, so
    /// the call never answers; the unit place carries the value walk.
    pub(super) fn cancel_call(&mut self, args: &[&Expr], span: Span) -> Result<Place, LowerError> {
        let native = crate::native::lookup("nursery_cancel")
            .ok_or_else(|| mismatch("the nursery cancel seam"))?;
        let [operand] = args else {
            return Err(LowerError::unsupported(
                span,
                "a `cancel` call",
                "stage exactly the error value; the payload is one member's word span",
            ));
        };
        let tp = self.value(operand)?;
        let payload_members = self.members(tp.ty);
        let bare = match &*self.shapes.shape(tp.ty)? {
            Shape::Union(u) => payload_members
                .iter()
                .all(|m| u.member(*m).is_some_and(|e| e.fields.is_empty())),
            Shape::Word(SlotType::Tag) => true,
            _ => false,
        };
        // the containment proof belongs to the function that owns the
        // nursery the fiber cancels into: the parent's spawn site checks
        // a spawned body's payloads against its own return union, so a
        // child frame skips the check here
        if !self.spawn_child && !set_diff(payload_members, self.members(self.ret_ty)).is_empty() {
            return Err(LowerError::unsupported(
                operand.span(),
                "a `cancel` payload",
                "the error member is not contained in the enclosing function's return union; the \
                 nursery boundary unions child errors out through the return",
            ));
        }
        if bare {
            let addr = single(&self.word_addrs(&tp.place, tp.ty)?, operand.span())?;
            if addr.slot_ty != SlotType::Tag {
                return Err(LowerError::unsupported(
                    operand.span(),
                    "a `cancel` payload",
                    "the error value must be a bare enum member this round",
                ));
            }
            self.seam_call(native, &[(addr.slot, SlotType::Tag)], &[SlotType::I64])?;
            return Ok(self.unit_value(span)?.place);
        }
        // The payloaded staging: the channel is the failing value's flat
        // word span — word 0 the member's tag, the tail the live
        // member's payload words in declaration order (the native
        // channel's staging shape), the words past the live member's
        // width plain-zeroed so a stale overlay word never rides the
        // trap. A union operand dispatches on its own tag word per
        // payloaded member (the operand's region claim layout is this
        // site's fact); any other operand mints its member's tag ahead
        // of its whole span. The staged `Ref` sources keep their counts
        // — a borrow window exactly like any native staging — and the
        // dying fiber's drain hands the delivered words' claims to the
        // joiner.
        self.rc.native_open();
        let layout = self.shapes.shape(tp.ty)?;
        if let Shape::Union(_) = &*layout {
            let tag_word = self.tag_addr(&tp.place)?;
            let mut arms: Vec<(u8, &[MemberField])> = Vec::new();
            for member in payload_members {
                let fields = layout
                    .member_fields(*member)
                    .ok_or_else(|| mismatch("a union member read"))?;
                if fields.is_empty() {
                    continue;
                }
                let tag = self.shapes.tag_id(*member)?;
                arms.push((tag, fields));
            }
            let width = arms
                .iter()
                .map(|(_, fields)| fields.iter().map(MemberField::width).sum::<usize>())
                .max()
                .unwrap_or(0);
            self.guard_staged_count(1 + width)?;
            self.emit(Op::Mov {
                dst: Slot::overlay(0),
                src: Slot::body(tag_word),
            });
            for index in 1..=width {
                let kidx = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    kidx,
                });
            }
            let mut labels = Vec::with_capacity(arms.len());
            for (tag, _) in &arms {
                let cond = self.temp_word(SlotType::Bool)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(tag_word),
                    tag: *tag,
                });
                let label = self.label();
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: label,
                });
                labels.push(label);
            }
            let call = self.label();
            self.emit(Op::Jmp { target: call });
            for ((_, fields), label) in arms.iter().zip(labels) {
                self.bind_label(label);
                let mut flat = 0usize;
                for field in *fields {
                    for offset in 0..field.width() {
                        let at =
                            field.word + u8::try_from(offset).expect("field span offset fits u8");
                        self.emit(Op::Mov {
                            dst: Slot::overlay(
                                u8::try_from(1 + flat + offset).expect("overlay word fits u8"),
                            ),
                            src: Slot::body(tag_word + at),
                        });
                        if field.slots[offset] == SlotType::Ref {
                            self.rc.staged_native(tag_word + at, false);
                        }
                    }
                    flat += field.width();
                }
                self.emit(Op::Jmp { target: call });
            }
            self.bind_label(call);
            // the staged kinds: word 0 the tag, the tail one set per index
            // across the members (mixed indices widen to Raw)
            for (_, fields) in &arms {
                let mut staged = vec![SlotType::Tag];
                staged.extend(fields.iter().flat_map(|f| f.slots.iter().copied()));
                self.overlay_site(&staged, &[]);
            }
            self.overlay_site(&[], &[SlotType::I64]);
            let kidx = self.pool(Constant::FuncRef {
                key: self.native_key(native.module),
                func: native.index,
            });
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(1 + width).expect("argument count fits u8"),
            });
        } else {
            let addrs = self.word_addrs(&tp.place, tp.ty)?;
            let kinds = self.shapes.words(tp.ty)?;
            self.guard_staged_count(1 + addrs.len())?;
            let [member] = payload_members else {
                return Err(mismatch("a payloaded cancel member"));
            };
            let tag = self.shapes.tag_id(*member)?;
            let tag_word = self.temp_word(SlotType::Tag)?;
            self.emit(Op::TagNew {
                dst: Slot::body(tag_word),
                tag,
            });
            self.emit(Op::Mov {
                dst: Slot::overlay(0),
                src: Slot::body(tag_word),
            });
            for (index, (addr, kind)) in addrs.iter().zip(kinds.iter()).enumerate() {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(1 + index).expect("overlay word fits u8")),
                    src: Slot::body(addr.slot),
                });
                if *kind == SlotType::Ref {
                    self.rc.staged_native(addr.slot, false);
                }
            }
            let mut staged = vec![SlotType::Tag];
            staged.extend_from_slice(&kinds);
            self.overlay_site(&staged, &[SlotType::I64]);
            let kidx = self.pool(Constant::FuncRef {
                key: self.native_key(native.module),
                func: native.index,
            });
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(1 + addrs.len()).expect("argument count fits u8"),
            });
        }
        self.rc.native_done(&mut self.code);
        Ok(self.unit_value(span)?.place)
    }

    /// The ambient `str[t]` conversion as an explicit call [F6]: the
    /// operand's checked type routes the same conversion the
    /// interpolation holes use — printable primitives pass their Ref
    /// word through or convert through the str natives, anything else
    /// reports the hole's honest boundary. The one-body CALLG dispatch
    /// waits on the VM-side generic str body, so the call site routes
    /// by its solved type instead. `None` when `def` is not the
    /// conversion extern.
    pub(super) fn ambient_str_place(
        &mut self,
        def: DefId,
        args: &[&Expr],
        span: Span,
    ) -> Result<Option<Place>, LowerError> {
        if !self.ambient_str_def(def) {
            return Ok(None);
        }
        let [arg] = args else {
            return Err(mismatch("a str call's argument"));
        };
        let val = self.value(arg)?;
        let word = self.hole_word(span, &val)?;
        Ok(Some(Place {
            base: word,
            shape: PlaceShape::Word(SlotType::Ref),
        }))
    }

    pub(super) fn extraction_def(&self, def: DefId) -> Option<Extraction> {
        if self.plan.index_of(def).is_some() {
            return None;
        }
        let sig = self.analyzed.decls.fns.get(&def)?;
        if sig.generics.is_empty() || sig.params.len() != 2 {
            return None;
        }
        Extraction::of_name(&self.analyzed.package.def(def).name)
    }
}
