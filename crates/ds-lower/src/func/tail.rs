//! The tail-position vocabulary: the tail-callee test, the tail walk
//! (blocks lower their last expression in place, every other shape emits
//! its return), the `return` value walk, the return emission (ret, ret
//! span, respan into the declared return type), and the place-widening
//! every caller site shares.

use super::*;

impl FnLower<'_, '_, '_> {
    // ---- tail position and returns ---------------------------------------------

    /// Whether a def declares a comptime parameter: the runtime module
    /// embeds or refuses every call to it in place, while the comptime
    /// slice lowers such functions as ordinary locals, so only the
    /// runtime run never transfers control to one.
    pub(super) fn comptime_def(&self, def: DefId) -> bool {
        matches!(self.comptime, ComptimeRun::Runtime(_))
            && self.analyzed.decls.fns.get(&def).is_some_and(|sig| {
                sig.params
                    .iter()
                    .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)))
            })
    }

    /// Whether a call can transfer control in tail position: a
    /// name call, a UFCS dot-call onto a resolved free function,
    /// or a member dot-call the checker solved to a monomorphic
    /// inherent body — its mangled `<target>#<member>` identity plans
    /// as a plain local callee, so the CALLT transfer applies. A
    /// trait-method site dispatches through CALLG (CALLTG stays
    /// reserved) and a runtime-evidence site resolves at run, so
    /// neither tails; their tail position lowers in place. Handlers
    /// inline, so `.err` stays a value-position call.
    fn is_tail_callee(&self, call: &Expr) -> bool {
        let Expr::Call {
            callee,
            span: call_span,
            ..
        } = call
        else {
            return false;
        };
        match &**callee {
            // A minted case constructor produces a value in place; it
            // never transfers control, so tail position lowers it as a
            // plain value. The extraction family inlines too, so a
            // tail-position family call lowers through the value walk's
            // return, not a control transfer. A comptime call in
            // the runtime module embeds or refuses in place, so its tail
            // position takes the same value walk: treating it as a
            // transfer stages the embedded answer and emits no return,
            // leaving the frame's code stream without an exit.
            Expr::Path { span, .. } => {
                // A call through a local binding reaches the maybe-Ref
                // callee tracking: the tail transfers through CALLT when
                // the callee word is provably count-free (a scalar naming
                // one function), and lowers in place (CALLS plus return)
                // where tracking cannot prove it
                !matches!(
                    self.res.get(*span),
                    Some(Res::Def(def))
                        if self.extraction_def(*def).is_some()
                            || self.ambient_extern(*def, AmbientExtern::Str)
                            || self.ambient_extern(*def, AmbientExtern::Sort)
                            || self.ambient_extern(*def, AmbientExtern::Max)
                            || self.ambient_extern(*def, AmbientExtern::Min)
                            || self.comptime_def(*def)
                            || self
                                .analyzed
                                .decls
                                .cases
                                .get(def)
                                .is_some_and(|c| !matches!(c.payload, EnumPayload::None))
                            || (self.analyzed.package.def(*def).kind == ds_check::DefKind::Tag
                                && self.analyzed.decls.structs.contains_key(def))
                )
            },
            Expr::Field { recv, name, .. } => {
                // The record, not the resolution tier, owns the routing:
                // a same-named free fn leaves the tier at Free where the
                // field tier answers. A field call is the computed-callee
                // path, whose tail contract decides CALLT against the
                // in-place CALLS plus return by the callee word's own
                // proof, so it reports a transfer like a local binding's.
                self.analyzed.checked.field_calls.contains(call_span)
                    || match self.res.get(name.span) {
                        Some(Res::Dot(DotRes::Free { func, .. })) => {
                            self.extraction_def(*func).is_none() && !self.comptime_def(*func)
                        },
                        Some(Res::Dot(DotRes::Methods { candidates })) => matches!(
                            self.resolve_method_dispatch(
                                *call_span,
                                name.span,
                                Some(recv),
                                candidates
                            ),
                            Ok(TraitDispatch::Inherent { .. }
                                | TraitDispatch::Static { .. }
                                | TraitDispatch::Runtime { .. }
                                | TraitDispatch::Foreign { .. }
                                | TraitDispatch::InherentForeign { .. })
                        ),
                        Some(Res::Dot(DotRes::Static { candidates })) => matches!(
                            self.resolve_method_dispatch(*call_span, name.span, None, candidates),
                            Ok(TraitDispatch::Inherent { .. }
                                | TraitDispatch::InherentForeign { .. })
                        ),
                        _ => false,
                    }
            },
            _ => false,
        }
    }

    /// Lowers a tail-position expression: calls become CALLT, everything
    /// else returns directly.
    pub(super) fn tail(&mut self, e: &Expr) -> Result<(), LowerError> {
        match e {
            Expr::Paren { inner, .. } => self.tail(inner),
            Expr::Block(block) => self.block_tail(block),
            Expr::Return { .. } => self.return_value(e).map(|_| ()),
            Expr::If {
                cond, then, else_, ..
            } => {
                let cond_val = self.value(cond)?;
                let cond_word =
                    single(&self.word_addrs(&cond_val.place, cond_val.ty)?, cond.span())?;
                let else_lbl = self.label();
                self.emit(Op::JmpifNot {
                    cond: Slot::body(cond_word.slot),
                    target: else_lbl,
                });
                // path-fork: both arms exit, each dropping from its own state
                let base = self.rc.save();
                self.block_tail(then)?;
                let arm_then = self.rc.take();
                self.rc.restore(base);
                self.bind_label(else_lbl);
                let result = if let Some(else_expr) = else_ {
                    self.tail(else_expr)
                } else {
                    // a missing else falls through to the unit return
                    let unit = self.unit_value(e.span())?;
                    self.emit_return(&unit.place, unit.ty, e.span())
                };
                let arm_else = self.rc.take();
                // both arms exit the frame: each exit path dropped from
                // its own state, so nothing survives the join
                self.rc.reconcile(&[(arm_then, true), (arm_else, true)]);
                result
            },
            Expr::Match { .. } => self.match_tail(e),
            Expr::Try { expr, .. } => self.try_tail(expr),
            // The frame-reuse transfer slides the callee's own answer
            // words onto this frame's caller, so it is sound only while
            // the call's answer is the declared return itself. A member
            // or strict sub-union answer would need the return's re-span
            // after the transfer, and the transfer leaves no position
            // for it — the same boundary a fallible-native tail keeps —
            // so it lowers in place through the value walk.
            Expr::Call { .. } => {
                let ty = self.val(e.span())?.ty;
                if self.is_tail_callee(e) && ty == self.ret_ty {
                    self.call_impl(e, ty, true)?;
                } else if self.members(ty).is_empty() {
                    // a diverging tail (`exit`/`panic` in the body's
                    // final position) never hands its answer to the
                    // return: the call's own exit ends the frame, so no
                    // return emission stages dead words over the unit
                    // answer
                    self.value(e)?;
                } else {
                    let tp = self.value(e)?;
                    self.emit_return(&tp.place, tp.ty, e.span())?;
                }
                Ok(())
            },
            _ => {
                let tp = self.value(e)?;
                self.emit_return(&tp.place, tp.ty, e.span())
            },
        }
    }

    fn block_tail(&mut self, block: &BlockExpr) -> Result<(), LowerError> {
        self.scope_push();
        let result = self.block_tail_inner(block);
        self.scope_pop();
        result
    }

    fn block_tail_inner(&mut self, block: &BlockExpr) -> Result<(), LowerError> {
        let (head, last) = match block.stmts.split_last() {
            Some((last, head)) => (head, Some(last)),
            None => (&block.stmts[..], None),
        };
        for stmt in head {
            self.stmt(stmt)?;
        }
        match last {
            Some(Stmt {
                kind: StmtKind::Expr(expr),
                ..
            }) => {
                // A spawned child's tail answer is the join channel's
                // element: the frame returns it, the completion
                // ledger carries the whole span, and the boundary
                // collects the answers in spawn order — the plan entry's
                // return is the body's answer type, so the exit shapes
                // like any function's tail.
                self.tail(expr)
            },
            Some(Stmt {
                kind: StmtKind::Local(local),
                ..
            }) => {
                // A binding tail is a statement, not the block's value:
                // it lowers, and the frame returns unit.
                self.local_stmt(local)?;
                let unit = self.unit_value(block.span)?;
                self.emit_return(&unit.place, unit.ty, block.span)
            },
            Some(Stmt {
                kind: StmtKind::Fn(f),
                span,
                ..
            }) => {
                // A block-local `fn` tail is a statement the same way: it
                // lowers, and the frame returns unit.
                self.fn_stmt(f, *span)?;
                let unit = self.unit_value(block.span)?;
                self.emit_return(&unit.place, unit.ty, block.span)
            },
            None => {
                let unit = self.unit_value(block.span)?;
                self.emit_return(&unit.place, unit.ty, block.span)
            },
        }
    }

    /// An early `return e`: the value leaves through the frame's return
    /// shape. The place handed back is dead code — the RET ends the frame,
    /// so the enclosing value slots never read it.
    pub(super) fn return_value(&mut self, e: &Expr) -> Result<TypedPlace, LowerError> {
        let Expr::Return { expr, span } = e else {
            return Err(mismatch("a return"));
        };
        let tp = match expr {
            Some(expr) => self.value(expr)?,
            None => self.unit_value(*span)?,
        };
        self.emit_return(&tp.place, tp.ty, *span)?;
        self.unit_value(*span)
    }

    /// Returns one value in the enclosing function's return shape. The
    /// returned words transfer their references out with the frame; every
    /// other still-owned reference drops right before the exit op. A
    /// shape refusal over the return type re-points at `span` — the
    /// expression whose value needs the return span — instead of the
    /// type-keyed sentinel span.
    pub(super) fn emit_return(
        &mut self,
        place: &Place,
        src_ty: Ty,
        span: Span,
    ) -> Result<(), LowerError> {
        self.emit_return_inner(place, src_ty)
            .map_err(|err| err.respanned(span))
    }

    fn emit_return_inner(&mut self, place: &Place, src_ty: Ty) -> Result<(), LowerError> {
        // the tail's value end is the exit boundary: the last entry of
        // the resumed-frame seam's per-statement record
        self.note_value_end();
        // A fn-typed return slot keeps the provenance rule over the
        // unproven callable words: the caller classifies the result
        // word Fn unless the tail is the factory lambda, so a
        // callable-defaulted word returned here hands the caller a Ref
        // behind the Fn trust and traps at the call. A closure word's
        // own return keeps the factory boundary (the recorded
        // return-flow seam) outside this guard, and a word addressed
        // inside a span place passed its own guard.
        if matches!(&*self.shapes.shape(self.ret_ty)?, Shape::Word(SlotType::Fn))
            && !matches!(place.shape, PlaceShape::Span(_))
            && let [addr] = self.word_addrs(place, src_ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                no_span(),
                "an unproven callable in a fn-typed return",
                "the caller classifies the fn-typed result word Fn, so a closure riding the \
                 callable word would trap at the call; only a named fn, a non-capturing lambda, \
                 or a proven call result returns here",
            ));
        }
        // A returned value that is a part of a deinit-carrying value (a
        // field read) leaves with the dying value's own words: the span
        // claim covering the read fires here, the struct's death staged
        // while its other words are still live, the span's non-escaping
        // `Ref` words dec-ing behind the hook. The value itself — the
        // returned type carries the deinit — keeps its claim with the
        // transfer.
        if self.deinit_claim(src_ty, no_span())?.is_none() {
            let escaping = self.word_addrs(place, src_ty)?;
            let words: Vec<u8> = escaping.iter().map(|addr| addr.slot).collect();
            let mut fired = Vec::new();
            for &word in &words {
                if let Some(base) = self.rc.claim_base_of(word)
                    && !fired.contains(&base)
                {
                    fired.push(base);
                    self.rc.fire_claim(&mut self.code, base, &words);
                }
            }
        }
        if src_ty == self.ret_ty {
            let addrs = self.word_addrs(place, src_ty)?;
            if addrs.len() == 1 {
                let word = addrs[0].slot;
                self.rc.exit(&mut self.code, &[word]);
                self.emit(Op::Ret {
                    src: Slot::body(word),
                });
                return Ok(());
            }
            let contiguous = addrs.iter().enumerate().all(|(index, addr)| {
                addr.slot == place.base + u8::try_from(index).expect("span width fits u8")
            });
            if contiguous {
                let transferred: Vec<u8> = addrs.iter().map(|addr| addr.slot).collect();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(place.base),
                    n: u8::try_from(addrs.len()).expect("span width fits u8"),
                });
                return Ok(());
            }
        }
        let ret_shape = self.shapes.shape(self.ret_ty)?;
        match &*ret_shape {
            Shape::Word(_) => {
                let addrs = self.word_addrs(place, src_ty)?;
                let [only] = addrs.as_slice() else {
                    return Err(mismatch("a scalar return"));
                };
                let word = only.slot;
                self.rc.exit(&mut self.code, &[word]);
                self.emit(Op::Ret {
                    src: Slot::body(word),
                });
                Ok(())
            },
            Shape::Struct(s) => {
                let target = self.temp_span(self.ret_ty)?;
                let addrs = self.word_addrs(place, src_ty)?;
                self.transfer_claim(target.base, place, src_ty)?;
                let srcs = ordered_field_srcs(&addrs, &s.fields)?;
                for (field, srcs) in s.fields.iter().zip(srcs) {
                    self.store_field_span(target.base, field, srcs);
                }
                let transferred: Vec<u8> = addrs
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        target.base + u8::try_from(index).expect("span width fits u8")
                    })
                    .collect();
                let width = self.shapes.words(self.ret_ty)?.len();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(width).expect("span width fits u8"),
                });
                Ok(())
            },
            Shape::Tuple(s) => {
                let target = self.temp_span(self.ret_ty)?;
                let addrs = self.word_addrs(place, src_ty)?;
                self.transfer_claim(target.base, place, src_ty)?;
                let srcs = ordered_field_srcs(&addrs, &s.fields)?;
                for (field, srcs) in s.fields.iter().zip(srcs) {
                    self.store_field_span(target.base, field, srcs);
                }
                let width = self.shapes.words(self.ret_ty)?.len();
                let transferred: Vec<u8> = (0..width)
                    .map(|offset| target.base + u8::try_from(offset).expect("span width fits u8"))
                    .collect();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(width).expect("span width fits u8"),
                });
                Ok(())
            },
            Shape::Union(u) => {
                // A multi-member operand (a narrower union than the
                // declared return — the checker proved containment) is
                // tag-dispatched per member into the return union's
                // region, mirroring the tail-`?` shape — an operand's raw
                // span never returns directly, union regions are
                // per-union-type — and each member's exit drops from its
                // own fork of the ownership state, so every exit releases
                // the frame's owned words, not just the first.
                let src_members = self.members(src_ty);
                if src_members.len() > 1 {
                    let src_word = self.tag_addr(place)?;
                    let mut exits = Vec::with_capacity(src_members.len());
                    for member in src_members {
                        let exit = self.label();
                        let cond = self.temp_word(SlotType::Bool)?;
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
                    let base = self.rc.save();
                    for (index, (member, exit)) in src_members.iter().zip(exits).enumerate() {
                        self.bind_label(exit);
                        self.respan_exit(place, *member)?;
                        // every member's exit drops from the same fork of
                        // the ownership map; the divergence marker stays
                        // set, so the frame reads exited on every path
                        if index + 1 < src_members.len() {
                            self.rc.rewind(base.clone());
                        }
                    }
                    return Ok(());
                }
                let atom = self.only_atom(src_ty, no_span())?;
                let member = u
                    .member(atom)
                    .ok_or_else(|| mismatch("a union member return"))?;
                // the return union's fn-typed payload slot keeps the
                // same provenance rule; the caller-side respan lifts the
                // refusal onto the returned expression's own span
                self.refuse_union_fn_payload(
                    &TypedPlace {
                        place: place.clone(),
                        ty: src_ty,
                    },
                    no_span(),
                )?;
                let target = self.temp_span(self.ret_ty)?;
                let tag = self.shapes.tag_id(atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(target.base),
                    tag,
                });
                self.zero_init_member(target.base, &u.words, &member.fields)?;
                let addrs = self.word_addrs(place, src_ty)?;
                let srcs = ordered_field_srcs(&addrs, &member.fields)?;
                for (field, srcs) in member.fields.iter().zip(srcs) {
                    self.store_field_span(target.base, field, srcs);
                }
                let transferred: Vec<u8> = (0..u8::try_from(u.words.len())
                    .expect("span width fits u8"))
                    .map(|index| target.base + index)
                    .collect();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(u.words.len()).expect("span width fits u8"),
                });
                Ok(())
            },
        }
    }

    /// One propagated member's early exit: re-span it into the enclosing
    /// return union, then return the span.
    pub(super) fn respan_exit(
        &mut self,
        src_place: &Place,
        member: TypeId,
    ) -> Result<(), LowerError> {
        if !matches!(&*self.shapes.shape(self.ret_ty)?, Shape::Union(_)) {
            return Err(mismatch("a `?` exit in a non-union return"));
        }
        let target = self.temp_span(self.ret_ty)?;
        self.respan_into(&target, self.ret_ty, src_place, member)?;
        let n = self.shapes.words(self.ret_ty)?.len();
        // the span's references transfer out with the return
        let transferred: Vec<u8> = (0..u8::try_from(n).expect("span width fits u8"))
            .map(|index| target.base + index)
            .collect();
        self.rc.exit(&mut self.code, &transferred);
        self.emit(Op::RetSpan {
            src: Slot::body(target.base),
            n: u8::try_from(n).expect("span width fits u8"),
        });
        Ok(())
    }

    /// Writes one member of a union place into a destination union span:
    /// the tag word moves (tag ids are module-wide), the dead payload
    /// words zero-init, the member's fields follow from the source
    /// layout.
    pub(super) fn respan_into(
        &mut self,
        dst: &Place,
        dst_ty: Ty,
        src: &Place,
        member: TypeId,
    ) -> Result<(), LowerError> {
        let shape = self.shapes.shape(dst_ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a re-span destination"));
        };
        // the entry resolves through the substitution-aware lookup: a
        // strict sub-union of the site's substituted head carries the
        // substituted member atoms, which the destination's
        // variable-keyed entries match structurally
        let entry = self
            .shapes
            .member_for(u, member)
            .ok_or_else(|| mismatch("a re-span member"))?;
        let src_tag = self.tag_addr(src)?;
        if entry.atom == member {
            // tag ids are module-wide, so the source's own tag moves
            self.emit(Op::Mov {
                dst: Slot::body(dst.base),
                src: Slot::body(src_tag),
            });
        } else {
            // a substituted source atom names no entry of this head;
            // writing its tag would dispatch a member the body's tag
            // tests never test, so the tag mints the entry's own id
            self.emit(Op::TagNew {
                dst: Slot::body(dst.base),
                tag: entry.tag,
            });
        }
        self.zero_init_member(dst.base, &u.words, &entry.fields)?;
        let PlaceShape::Span(underlying) = src.shape else {
            return Err(mismatch("a re-span source"));
        };
        let src_layout = self.shapes.shape(underlying)?;
        let fields = src_layout
            .member_fields(member)
            .ok_or_else(|| mismatch("a re-span member"))?;
        for (index, field) in entry.fields.iter().enumerate() {
            // a positional field (a tuple member's) matches by position —
            // the None names all compare equal, so the name-find would
            // return the first field for every element
            let src_field = if field.name.is_some() {
                fields
                    .iter()
                    .find(|f| f.name == field.name)
                    .ok_or_else(|| mismatch("a re-span field"))?
            } else {
                fields
                    .get(index)
                    .ok_or_else(|| mismatch("a re-span field"))?
            };
            let srcs = field_addrs(src.base, src_field);
            self.store_field_span(dst.base, field, &srcs);
        }
        Ok(())
    }

    /// Writes one value into a destination span: same-type word moves, or
    /// a member construction with TAG_NEW into a union.
    /// Moves a deinit value's claim from its source base word to a
    /// re-span destination's base: the movs write a fresh copy of
    /// the words, and a pinned copy never dups, so the claim transfers
    /// silently. In-place spans and claim-less sources do nothing.
    fn transfer_claim(&mut self, dst_base: u8, src: &Place, src_ty: Ty) -> Result<(), LowerError> {
        let addrs = self.word_addrs(src, src_ty)?;
        let [first] = addrs.as_slice() else {
            return Ok(());
        };
        if first.slot == dst_base || !self.rc.has_claim(first.slot) {
            return Ok(());
        }
        self.rc.copied(&mut self.code, dst_base, first.slot);
        Ok(())
    }

    pub(super) fn widen_into(
        &mut self,
        dst: &Place,
        dst_ty: Ty,
        src: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.transfer_claim(dst.base, &src.place, src.ty)?;
        // Two applications of distinct generic enums with equal word
        // layouts move whole when an instance the shapes share lowered
        // the value: the shared body keyed the first site's nominal
        // union, the tag word's content is the value's own case, and no
        // body over the variable names a case to test — so the spans
        // pair word for word and the in-place move is exact.
        let src_members = self.members(src.ty).to_vec();
        let dst_members = self.members(dst_ty).to_vec();
        let shared_enum_layouts = src.ty != dst_ty
            && matches!((src_members.as_slice(), dst_members.as_slice()),
                ([src_app], [dst_app])
                    if self.shapes.is_enum_application(*src_app)
                        && self.shapes.is_enum_application(*dst_app)
                        && self.shapes.words(src.ty)? == self.shapes.words(dst_ty)?);
        // A place keyed to a wider union can keep a different layout,
        // so an equal checked type is not always an in-place move: the
        // flat path would stage the operand's words over the
        // destination's own region (a narrowed binder over an answer
        // union whose payload interleaves differently). Only a place
        // whose layout is the type's own takes the fast path; the rest
        // re-span member-wise below.
        if (src.ty == dst_ty || shared_enum_layouts)
            && self.place_layout_matches(&src.place, src.ty)?
        {
            let addrs = self.word_addrs(&src.place, src.ty)?;
            if addrs.is_empty() {
                return Ok(());
            }
            // Defense on the same-type word moves: the claimed type's
            // word list must fit the destination place. A caller
            // claiming a subset of the place's physical union would
            // otherwise emit full-operand-width writes (and RC claims)
            // past the destination's allocation — every honest caller
            // has the two widths equal, so this fires only on a
            // lowerer bug, as a typed refusal instead of memory
            // corruption.
            let dst_width = match dst.shape {
                PlaceShape::Word(_) => 1,
                PlaceShape::Span(underlying) => self.shapes.words(underlying)?.len(),
            };
            if addrs.len() > dst_width {
                return Err(mismatch("a widened destination span"));
            }
            if addrs.iter().enumerate().all(|(index, addr)| {
                addr.slot == dst.base + u8::try_from(index).expect("span width fits u8")
            }) {
                return Ok(()); // already in place
            }
            for (index, addr) in addrs.iter().enumerate() {
                let word = dst.base + u8::try_from(index).expect("span width fits u8");
                self.emit(Op::Mov {
                    dst: Slot::body(word),
                    src: Slot::body(addr.slot),
                });
                if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
                    self.rc.copied(&mut self.code, word, addr.slot);
                }
            }
            return Ok(());
        }
        let dst_shape = self.shapes.shape(dst_ty)?;
        match &*dst_shape {
            Shape::Word(_) => {
                let addr = single(&self.word_addrs(&src.place, src.ty)?, no_span())?;
                self.emit(Op::Mov {
                    dst: Slot::body(dst.base),
                    src: Slot::body(addr.slot),
                });
                if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
                    self.rc.copied(&mut self.code, dst.base, addr.slot);
                }
                Ok(())
            },
            Shape::Struct(s) | Shape::Tuple(s) => {
                let addrs = self.word_addrs(&src.place, src.ty)?;
                let srcs = ordered_field_srcs(&addrs, &s.fields)?;
                for (field, srcs) in s.fields.iter().zip(srcs) {
                    self.store_field_span(dst.base, field, srcs);
                }
                Ok(())
            },
            Shape::Union(u) => {
                // A multi-member source (a narrower union than the
                // destination — the checker proved containment at the
                // annotated binding, the assignment, or the branch
                // join) is tag-dispatched per member into the
                // destination region, mirroring the union return path:
                // an operand's raw span never lands directly, union
                // regions are per-union-type.
                let src_members = self.members(src.ty);
                if src_members.len() > 1 {
                    let src_word = self.tag_addr(&src.place)?;
                    let continue_lbl = self.label();
                    let mut exits = Vec::with_capacity(src_members.len());
                    for member in src_members {
                        let exit = self.label();
                        let cond = self.temp_word(SlotType::Bool)?;
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
                    for (member, exit) in src_members.iter().zip(exits) {
                        self.bind_label(exit);
                        self.respan_into(dst, dst_ty, &src.place, *member)?;
                        self.emit(Op::Jmp {
                            target: continue_lbl,
                        });
                    }
                    self.bind_label(continue_lbl);
                    return Ok(());
                }
                let atom = self.only_atom(src.ty, span)?;
                // a written generic head keys its members by the
                // variable's atoms while the entering site carries the
                // substituted concrete value, so the lookup resolves
                // through the substitution's structural match — a heap
                // application by its constructor (one Ref word whatever
                // the argument), a tuple member by its elements (one
                // Raw-claimed word each, filled from the substituted
                // value's own words) — and the tag writes the member's
                // own minted id: re-minting the source's atom would tag
                // a member the body's tests never dispatch on
                let member = self.shapes.member_for(u, atom).ok_or_else(|| {
                    LowerError::unsupported(
                        span,
                        "a union member write",
                        "the destination union carries no member whose region holds the value's \
                         shape; a written generic head takes the member the site's substitution \
                         names — a heap application by its constructor, or a tuple member whose \
                         one-word-per-element claim the substituted value pairs exactly",
                    )
                })?;
                // a callable word entering the union's fn-typed
                // payload slot keeps the var slot's provenance rule:
                // the payload word is Fn-classified and every extraction
                // re-reads it, so a closure's Ref word traps there the
                // same way it traps a re-storable var slot
                self.refuse_union_fn_payload(src, span)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(dst.base),
                    tag: member.tag,
                });
                self.zero_init_member(dst.base, &u.words, &member.fields)?;
                let addrs = self.word_addrs(&src.place, src.ty)?;
                let srcs = ordered_field_srcs(&addrs, &member.fields)?;
                for (field, srcs) in member.fields.iter().zip(srcs) {
                    self.store_field_span(dst.base, field, srcs);
                }
                Ok(())
            },
        }
    }
}

impl FnLower<'_, '_, '_> {
    /// Whether a place's physical words are the type's own union
    /// layout: a span place keyed to that very type, a single word, or
    /// a wider union whose region the type's members claim identically
    /// (an extra payload-free member adds no words). A wider union that
    /// interleaves its payload differently keeps a different layout,
    /// so re-spanning into the type's own region needs the
    /// member-dispatch path.
    pub(super) fn place_layout_matches(
        &mut self,
        place: &Place,
        ty: Ty,
    ) -> Result<bool, LowerError> {
        match place.shape {
            PlaceShape::Span(underlying) if underlying != ty => {
                Ok(self.shapes.words(underlying)? == self.shapes.words(ty)?)
            },
            _ => Ok(true),
        }
    }
}
