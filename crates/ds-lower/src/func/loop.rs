//! Control forms and loops: assignment, `while`, the three `for` forms
//! with their comptime copies, `break`/`continue`, the loop exit
//! discards, and the element fetch and gather words.

use super::*;

impl FnLower<'_, '_, '_> {
    /// One assignment `x = e` / `x op= e`: the place is a single-segment
    /// path to a `var` local (the checker rejects every other place), the
    /// right side evaluates first, then re-stores the binder's slots. A
    /// reassignment kills the old value: the replacement drop threads right
    /// here, before the store Movs overwrite the old reference.
    pub(super) fn assign_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        // A checked place chain (the assignment sugar) synthesizes its
        // store from the walk the checker recorded.
        if let Some(store) = self.place_store(e.span())? {
            return self.assign_place_store(e, &store);
        }
        let Expr::Assign {
            op, left, right, ..
        } = e
        else {
            return Err(mismatch("an assignment"));
        };
        let Expr::Path { path, .. } = &**left else {
            return Err(LowerError::unsupported(
                left.span(),
                "an assignment place",
                "places are plain name bindings; field assignment is undecided surface [D2]",
            ));
        };
        let [only] = path.segments.as_slice() else {
            return Err(LowerError::unsupported(
                left.span(),
                "an assignment place",
                "qualified places resolve through modules [G4, M7]",
            ));
        };
        let binder = match self.res.get(only.span) {
            Some(Res::Local { bind: binder }) => *binder,
            Some(Res::Def(_)) => {
                return Err(LowerError::unsupported(
                    only.span,
                    "an assignment to a top-level binding",
                    "module-level storage arrives with cross-module state [G4, M7]",
                ))
            },
            _ => {
                return Err(LowerError::unsupported(
                    only.span,
                    "an assignment place",
                    "the name does not address a local binding",
                ))
            },
        };
        let place = self
            .binders
            .get(&binder)
            .cloned()
            .ok_or_else(|| self.missing_binder(only.span))?;
        let declared = self
            .binder_fact(binder)?
            .ok_or_else(|| mismatch("an assignment place"))?
            .ty;
        let assigned = match op {
            ds_parse::AssignOp::Plain => {
                // An int/float literal stores straight into a scalar
                // var word: the constant has no other consumer and a
                // scalar word carries no count, so the temp + re-store
                // reduces to the one constant load (a declined spelling
                // keeps the temp path and its refusals)
                if let PlaceShape::Word(slot @ (SlotType::I64 | SlotType::F64)) = place.shape
                    && let Expr::Literal { lit, .. } = peel_parens(right)
                    && let Some(constant) = scalar_const(lit, slot)
                {
                    self.scalar_const_into(constant, Slot::body(place.base));
                    TypedPlace {
                        place: place.clone(),
                        ty: declared,
                    }
                } else if let PlaceShape::Word(slot @ (SlotType::I64 | SlotType::F64)) = place.shape
                    && arith_shape(right)
                {
                    // A computed scalar stores straight into the var
                    // word: the assignment offers the word to the
                    // right-hand side's outermost data op (the scalar
                    // destination offer), whose produced temp is
                    // single-use by construction — this store is its
                    // only consumer. A shape that declines the offer
                    // (a call result, a path read) keeps the temp +
                    // re-store.
                    self.scalar_dst = Some(ScalarDst::Var {
                        word: place.base,
                        slot,
                    });
                    let assigned_val = self.value(right)?;
                    let _ = self.scalar_dst.take();
                    assigned_val
                } else {
                    self.value(right)?
                }
            },
            ds_parse::AssignOp::Add
            | ds_parse::AssignOp::Sub
            | ds_parse::AssignOp::Mul
            | ds_parse::AssignOp::Div
            | ds_parse::AssignOp::Rem
            | ds_parse::AssignOp::Shl
            | ds_parse::AssignOp::Shr => {
                // `x op= e` reads the current slots, then re-stores the
                // result: the same word moves the binary path emits.
                self.compound_value(
                    *op,
                    &TypedPlace {
                        place: place.clone(),
                        ty: declared,
                    },
                    right,
                    left.span(),
                )?
            },
        };
        // the replaced reference dies before the store overwrites its word
        // (compound stores re-store arithmetic results or the string
        // concat's fresh Ref word, never a borrowed operand). A deinit
        // value's claim fires first: the hook runs while the old
        // content's counts are still live, and the span claim's drop decs
        // every `Ref` word of the value — the folded words carry no entry
        // of their own
        if self.deinit_claim(declared, e.span())?.is_some() {
            self.rc.store_over(&mut self.code, &[place.base]);
        } else {
            for word in self.count_words(place.base, declared)? {
                self.rc.store_over(&mut self.code, &[word]);
            }
        }
        // A diverging right-hand side (`exit(1)` as the assigned value)
        // never hands its answer to the store: the call's own exit ends
        // the path, so the var keeps its content and no widen stages
        // dead words over a mismatched shape
        if !self.diverges_at(right.span()) {
            self.refuse_closure_widen(&assigned, declared, e.span())?;
            self.guard_deinit_union_member(assigned.ty, declared, e.span())?;
            self.widen_into(&place, declared, &assigned, e.span())?;
        }
        // An assignment inside a lambda body to the frame's own captured
        // binding also publishes the new words into the running closure
        // object's capture block (the mut window over the uniquely-held
        // slot): the frame's copy stays this frame's, the object acquires
        // its own count per Ref word and releases the replaced word's.
        // The store happens after `widen_into` wrote the frame words, so
        // the op reads the new content; the frame's replaced copy died
        // at `store_over` above.
        if let Some(&offset) = self.capture_index.get(&binder) {
            let obj = self
                .capture_word
                .expect("a mutation frame carries its closure word");
            for (word, addr) in self.word_addrs(&place, declared)?.iter().enumerate() {
                self.emit(Op::CaptureStore {
                    src: addr.slot,
                    obj,
                    idx: u8::try_from(offset + word)
                        .expect("the capture block fits the u8 idx field"),
                });
            }
        }
        // The replaced value's claim re-registers at its slot: the store
        // fired the old content's drop (spending the site's band) and the
        // incoming claim's transfer spent its own, so without this the
        // teardown doors read the slot dead and the value the frame still
        // holds never fires its hook — the same re-registration a binder
        // performs over a moved-in claim.
        if let Some(claim) = self.deinit_claim(declared, e.span())?
            && self.rc.has_claim(place.base)
            && self.rc.anchors_claim(place.base)
        {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let words = self.tracked_words(&place, declared)?;
            let band = self.claim_band_word(place.base)?;
            self.rc
                .claimed(&mut self.code, place.base, claim.hook, band, &words);
        }
        Ok(self.unit_value(e.span())?.place)
    }

    /// `current op= e`: reads `current`'s value, then folds the right
    /// side in through the operator table the binary spelling uses. A
    /// scalar leaf computes in place over its own word (the VM reads both
    /// operands before writing dst), a wider leaf answers a fresh temp, and
    /// the string `+=` answers the concat's fresh Ref word. The result's
    /// type is the leaf's own; storing it back is the caller's.
    pub(super) fn compound_value(
        &mut self,
        op: ds_parse::AssignOp,
        current: &TypedPlace,
        right: &Expr,
        span: Span,
    ) -> Result<TypedPlace, LowerError> {
        let declared = current.ty;
        match op {
            ds_parse::AssignOp::Rem => self.reject_float_rem(current, span)?,
            ds_parse::AssignOp::Shl | ds_parse::AssignOp::Shr => {
                self.reject_float_shift_bitwise(current, span)?;
            },
            _ => {},
        }
        let (int_op, float_op) = match op {
            ds_parse::AssignOp::Add => (DataOp::AddI64, DataOp::AddF64),
            ds_parse::AssignOp::Sub => (DataOp::SubI64, DataOp::SubF64),
            ds_parse::AssignOp::Mul => (DataOp::MulI64, DataOp::MulF64),
            ds_parse::AssignOp::Div => (DataOp::DivI64, DataOp::DivF64),
            ds_parse::AssignOp::Shl => (DataOp::ShlI64, DataOp::ShlI64),
            ds_parse::AssignOp::Shr => (DataOp::ShrI64, DataOp::ShrI64),
            _ => (DataOp::ModI64, DataOp::ModI64),
        };
        // string `+=` opens the concat window before the rhs evaluation
        // and pins the replaced word: a last use inside the rhs defers the
        // replaced content's dec to the flush (after the CALL), and the
        // store below then finds the word unowned and skips its own dec
        let str_add = matches!(op, ds_parse::AssignOp::Add)
            && self.is_string_add_ty(declared, self.val(right.span())?.ty);
        if str_add {
            self.rc.native_open();
            let words = self
                .word_addrs(&current.place, declared)
                .map_err(|err| err.respanned(right.span()))?;
            self.rc.native_reads(words.iter().map(|addr| addr.slot));
        }
        let rhs = self.value(right)?;
        let result = if str_add {
            // string `+=` desugars like the binary arm: the compound
            // store re-stores the concat result, a fresh Ref word
            self.concat_value(current, &rhs, declared, span)?
        } else if let PlaceShape::Word(SlotType::I64 | SlotType::F64) = current.place.shape {
            // a scalar compound store writes the leaf word itself: the VM
            // reads both operands before writing dst, so the word safely
            // aliases its own read, and the re-store below degenerates to
            // the in-place no-op (a scalar word carries no count)
            self.arith_at(int_op, float_op, current, &rhs, current.place.base, span)?
        } else {
            self.arith(int_op, float_op, current, &rhs, span)?
        };
        Ok(TypedPlace {
            place: result,
            ty: declared,
        })
    }

    /// One `while cond { body }`: a backward jump over the re-tested
    /// condition. The body lowers as statements (its value is unit);
    /// every body slot is frame-static, so an iteration re-writes the same
    /// words. Refinements harvested inside an iteration never escape it
    /// (the checker restores its snapshot at loop exit; the lowerer reads
    /// only per-expression checked types, which already respect that).
    /// The loop forks the ownership map before the head: the cond replays
    /// per iteration, so a Ref operand born outside the loop is
    /// loop-carried (its count never decs at the in-head use; it dies at
    /// the owning scope's end, past the loop), while words the cond
    /// itself produces die per iteration at their uses.
    pub(super) fn while_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::While { cond, body, span } = e else {
            return Err(mismatch("a while loop"));
        };
        let head = self.label();
        let exit = self.label();
        // continue targets the head: the cond re-evaluates per iteration
        // (body_depth is the scope count before the body scope pushes,
        // so a jump sweeps every scope from the body scope inwards)
        self.loops.push(LoopCtx {
            exit,
            cont: head,
            body_depth: self.scopes.len(),
        });
        // loop-fork the ownership map: words owned on entry AND at the
        // back edge survive the loop; body-only words died inside it, and
        // uses one depth deeper never kill outer productions
        let entry = self.rc.save();
        let carried: Vec<u8> = entry.keys().copied().collect();
        self.rc.loop_enter();
        self.loop_spans.push(LoopSpan {
            head: self.code.len(),
            back: usize::MAX,
        });
        self.bind_label(head);
        let cond_val = self.value(cond)?;
        let cond_word = single(&self.word_addrs(&cond_val.place, cond_val.ty)?, cond.span())?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_word.slot),
            target: exit,
        });
        self.scope_push();
        for stmt in &body.stmts {
            self.stmt(stmt)?;
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.loop_back_edge();
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        // an anonymous cond operand consumed in-loop has no scope of its
        // own: its carried count dies here, once per evaluation
        self.rc.drop_unnamed(&mut self.code, &carried);
        self.loops.pop();
        Ok(self.unit_value(*span)?.place)
    }

    /// One `for x in it { body }`: the iterable is an int range or one
    /// list; every other iterable refuses.
    /// The `for comptime` expansion loop: one straight-line copy per
    /// projected field, the record binder recognized by span and every
    /// checked record read keyed by the copy path. No loop machinery
    /// exists at runtime — the copies run in sequence, each in its own
    /// scope so its locals die at the copy's end.
    pub(super) fn for_comptime_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::For {
            pat, body, span, ..
        } = e
        else {
            return Err(mismatch("the comptime loop"));
        };
        let loop_info = self.comptime_loop_info(*span)?;
        let Pattern::Binding {
            span: binder_span,
            name: _,
        } = pat
        else {
            return Err(LowerError::unsupported(
                *span,
                "a destructuring comptime pattern",
                "the record binds one name",
            ));
        };
        for index in 0..loop_info.fields.len() {
            self.comptime_path
                .push(u32::try_from(index).expect("copy index fits u32"));
            self.comptime_stack.push((*span, *binder_span));
            self.scope_push();
            match &**body {
                Expr::Block(block) => {
                    for stmt in &block.stmts {
                        self.stmt(stmt)?;
                    }
                },
                other => {
                    let tp = self.value(other)?;
                    let words = self.discard_words(&tp.place, tp.ty, other.span())?;
                    self.rc.discard(&mut self.code, &words);
                },
            }
            self.scope_pop();
            self.comptime_stack.pop();
            self.comptime_path.pop();
        }
        Ok(self.unit_value(*span)?.place)
    }

    /// The projected fields of the loop at `span`: an active expansion's
    /// from the instance tables under the walked fn's definition and
    /// solved arguments — a written loop nested inside a per-instance
    /// copy projects only there, the once-check never walking it — then
    /// the once-check's table. A per-instance loop with no active
    /// expansion — the ONE body lowering over its variables — refuses:
    /// its fields are a fact of each concrete entering shape, and a
    /// variable's shape names none.
    pub(super) fn comptime_loop_info(
        &self,
        span: Span,
    ) -> Result<ds_check::type_of::ComptimeLoop, LowerError> {
        if let Some(scope) = &self.comptime_instance
            && let Some(info) = self.analyzed.checked.comptime_instance_loops.get(&(
                scope.0,
                Rc::clone(&scope.1),
                span,
            ))
        {
            return Ok(info.clone());
        }
        let marked = self.analyzed.checked.comptime_loops.get(&span);
        if marked.is_some_and(|info| info.per_instance.is_some()) {
            if self.comptime_instance.is_none() {
                return Err(LowerError::unsupported(
                    span,
                    "a `for comptime` loop over the fn's own type variable",
                    "this body lowers over its variables; only a concrete entering shape projects \
                     the loop's fields",
                ));
            }
            return Err(LowerError::unsupported(
                span,
                "a `for comptime` loop",
                "the checker expanded no per-instance fields for this instance",
            ));
        }
        marked.cloned().ok_or_else(|| {
            LowerError::unsupported(
                span,
                "a `for comptime` loop",
                "the checker projected no fields for it",
            )
        })
    }

    /// The comptime constant a keyed access's name expression spells:
    /// exactly `<record binder>.name`, answering the field name of the
    /// active copy at the binder's depth.
    pub(super) fn comptime_key_name(&self, name: &Expr) -> Option<String> {
        let Expr::Field {
            recv, name: member, ..
        } = peel_parens(name)
        else {
            return None;
        };
        if member.name != "name" {
            return None;
        }
        let Expr::Path { path, .. } = peel_parens(recv) else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        let Res::Local { bind } = self.res.get(only.span)? else {
            return None;
        };
        let depth = self
            .comptime_stack
            .iter()
            .rposition(|(_, binder)| binder == bind)?;
        let (loop_span, _) = self.comptime_stack.get(depth).copied()?;
        let fields = &self.comptime_loop_info(loop_span).ok()?.fields;
        let index = *self.comptime_path.get(depth)?;
        fields.get(index as usize).cloned()
    }

    /// The copy's own field fact when a receiver expression names an
    /// active comptime record binder (`f` inside `for comptime f in
    /// ...`): the field name, and its type's nominal tag string. The
    /// binder's stack depth indexes the copy path.
    pub(super) fn comptime_record_field(&self, recv: &Expr) -> Option<(String, String)> {
        let Expr::Path { path, .. } = peel_parens(recv) else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        let Res::Local { bind } = self.res.get(only.span)? else {
            return None;
        };
        let depth = self
            .comptime_stack
            .iter()
            .rposition(|(_, binder)| binder == bind)?;
        let (loop_span, _) = self.comptime_stack.get(depth).copied()?;
        let info = self.comptime_loop_info(loop_span).ok()?;
        let index = *self.comptime_path.get(depth)?;
        let name = info.fields.get(index as usize)?.clone();
        let tag = info.tags.get(index as usize)?.clone();
        Some((name, tag))
    }

    pub(super) fn for_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::For {
            pat,
            iter,
            body,
            span,
            ..
        } = e
        else {
            return Err(mismatch("a for loop"));
        };
        match peel_parens(iter) {
            Expr::Range {
                inclusive,
                start,
                end,
                ..
            } => self.for_range_value(pat, start, end, *inclusive, body, *span),
            _ => self.for_list_value(pat, iter, body, *span),
        }
    }

    /// A `for x in a..b` over an int range: a counted loop with one
    /// induction slot. The endpoints evaluate once; the step test
    /// re-runs per iteration against the frozen end. An inclusive back
    /// edge parked at i64::MAX exits instead of stepping (the add has
    /// no successor word). The pattern binder
    /// names the induction word, so every body read sees the current
    /// step.
    pub(super) fn for_range_value(
        &mut self,
        pat: &Pattern,
        start: &Expr,
        end: &Expr,
        inclusive: bool,
        body: &Expr,
        span: Span,
    ) -> Result<Place, LowerError> {
        let start_tp = self.value(start)?;
        let start_w = single(
            &self.word_addrs(&start_tp.place, start_tp.ty)?,
            start.span(),
        )?;
        let end_tp = self.value(end)?;
        let end_w = single(&self.word_addrs(&end_tp.place, end_tp.ty)?, end.span())?;
        if start_w.slot_ty != SlotType::I64 || end_w.slot_ty != SlotType::I64 {
            return Err(LowerError::unsupported(
                span,
                "a range over non-`int` widths",
                "only the ambient `int` width lowers this round; conversions are explicit [B7]",
            ));
        }
        let ind = self.temp_word(SlotType::I64)?;
        self.emit(Op::Mov {
            dst: Slot::body(ind),
            src: Slot::body(start_w.slot),
        });
        let step = self.temp_word(SlotType::I64)?;
        let one = self.pool(Constant::I64(1));
        self.emit(Op::Kimm {
            dst: Slot::body(step),
            kidx: one,
        });
        let ind_place = Place {
            base: ind,
            shape: PlaceShape::Word(SlotType::I64),
        };
        let ind_tp = TypedPlace {
            place: ind_place.clone(),
            ty: start_tp.ty,
        };
        let head = self.label();
        let exit = self.label();
        let back = self.label();
        // continue lands at the backedge start so the induction step
        // still runs before the head re-test (body_depth is the scope
        // count before the body scope pushes, so a jump sweeps every
        // scope from the body scope inwards)
        self.loops.push(LoopCtx {
            exit,
            cont: back,
            body_depth: self.scopes.len(),
        });
        self.bind_label(head);
        let bound_op = if inclusive { DataOp::LeI64 } else { DataOp::LtI64 };
        let cond = self.emit_data(bound_op, &ind_tp, &end_tp, span)?;
        let cond_w = single(&self.word_addrs(&cond, start_tp.ty)?, span)?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_w.slot),
            target: exit,
        });
        // loop-fork the ownership map exactly like the while shape
        let entry = self.rc.save();
        self.rc.loop_enter();
        self.loop_spans.push(LoopSpan {
            head: self.code.len(),
            back: usize::MAX,
        });
        self.scope_push();
        match pat {
            Pattern::Binding {
                span: bind_span,
                name,
            } => {
                self.binders.insert(*bind_span, ind_place.clone());
                self.rc_bind(*bind_span, &ind_place, start_tp.ty)?;
                self.scope_insert(&name.name, *bind_span);
            },
            Pattern::Wildcard(_) => {},
            _ => {
                return Err(LowerError::unsupported(
                    span,
                    "a destructuring range pattern",
                    "an int step binds as a plain name or `_`; there is nothing to destructure",
                ))
            },
        }
        match body {
            Expr::Block(block) => {
                for stmt in &block.stmts {
                    self.stmt(stmt)?;
                }
            },
            other => {
                let tp = self.value(other)?;
                let words = self.discard_words(&tp.place, tp.ty, other.span())?;
                self.rc.discard(&mut self.code, &words);
            },
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.bind_label(back);
        if inclusive {
            // an inclusive count parked at i64::MAX has no successor
            // step (the add would trap), so the back edge exits there;
            // continue lands here too, so the guard covers every exit
            // from the final iteration
            let ceiling = self.temp_word(SlotType::I64)?;
            let max = self.pool(Constant::I64(i64::MAX));
            self.emit(Op::Kimm {
                dst: Slot::body(ceiling),
                kidx: max,
            });
            let at_max = self.temp_word(SlotType::Bool)?;
            self.emit(Op::Data {
                op: DataOp::EqI64,
                dst: Slot::body(at_max),
                lhs: Slot::body(ind),
                rhs: Slot::body(ceiling),
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(at_max),
                target: exit,
            });
        }
        self.emit(Op::Data {
            op: DataOp::AddI64,
            dst: Slot::body(ind),
            lhs: Slot::body(ind),
            rhs: Slot::body(step),
        });
        self.loop_back_edge();
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        self.loops.pop();
        Ok(self.unit_value(span)?.place)
    }

    /// A `for x in xs` over one list: the iterable evaluates once
    /// and its element count freezes at loop entry (one `list_len` call
    /// into a hidden count word) — elements the body appends are not
    /// visited this loop — then an index-counted fetch loop: one hidden
    /// induction word, `list_get` per iteration over its ok/none span
    /// (one payload word per element word at the list's stride), the
    /// frozen count bounds the loop while the none span stays the
    /// fetch's fail-closed guard. The pattern binder names the
    /// gathered element — one word or a whole
    /// struct span: the native's handed-out counts (one per `Ref` word)
    /// die at the binding's scope end each rollover, never carried
    /// across the back edge, while the iterable's count survives the
    /// whole loop (a loop-carried borrow the fetch re-stages per
    /// iteration) and dies past it.
    pub(super) fn for_list_value(
        &mut self,
        pat: &Pattern,
        iter: &Expr,
        body: &Expr,
        span: Span,
    ) -> Result<Place, LowerError> {
        let iter_tp = self.value(iter)?;
        let Some(element) = self.shapes.list_element(iter_tp.ty) else {
            return Err(LowerError::unsupported(
                span,
                "a for over a non-list collection",
                "iteration lowers over int ranges and `list[t]`; map iteration waits on an \
                 ordered-entries native",
            ));
        };
        let elem_words = self.element_words(element, span)?;
        let list_w = single(&self.word_addrs(&iter_tp.place, iter_tp.ty)?, iter.span())?;
        let ind = self.temp_word(SlotType::I64)?;
        let zero = self.pool(Constant::I64(0));
        self.emit(Op::Kimm {
            dst: Slot::body(ind),
            kidx: zero,
        });
        let step = self.temp_word(SlotType::I64)?;
        let one = self.pool(Constant::I64(1));
        self.emit(Op::Kimm {
            dst: Slot::body(step),
            kidx: one,
        });
        // loop-fork the ownership map before the head: the iterable is
        // a loop-carried word, so the per-iteration fetch staging never
        // decs it in-body
        let entry = self.rc.save();
        self.rc.loop_enter();
        // the frozen element count: one length read here bounds the
        // whole loop, so a body appending to the iterable never extends
        // it. The read sits after loop_enter (these pre-head ops run
        // once) so the iterable is already loop-carried: an anonymous
        // iterable dies at first consumption, and the length read would
        // otherwise consume its count before the fetch runs.
        let len = self.temp_word(SlotType::I64)?;
        self.list_len_word(list_w.slot, len)?;
        self.loop_spans.push(LoopSpan {
            head: self.code.len(),
            back: usize::MAX,
        });
        let head = self.label();
        let exit = self.label();
        let back = self.label();
        // continue lands at the backedge start so the index step still
        // runs before the head re-fetch (body_depth is the scope count
        // before the body scope pushes, so a jump sweeps every scope
        // from the body scope inwards)
        self.loops.push(LoopCtx {
            exit,
            cont: back,
            body_depth: self.scopes.len(),
        });
        self.bind_label(head);
        // the frozen count is the loop's exit test: the index check runs
        // before the fetch, so indices past the entry count never stage
        // a borrow or hand out counts
        let bound = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Data {
            op: DataOp::LtI64,
            dst: Slot::body(bound),
            lhs: Slot::body(ind),
            rhs: Slot::body(len),
        });
        self.emit(Op::JmpifNot {
            cond: Slot::body(bound),
            target: exit,
        });
        self.fetch_element(list_w.slot, ind, &elem_words)?;
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: exit,
        });
        self.scope_push();
        let elem_place = self.gather_element(&elem_words, element)?;
        match pat {
            Pattern::Binding {
                span: bind_span,
                name,
            } => {
                self.binders.insert(*bind_span, elem_place.clone());
                self.rc_bind(*bind_span, &elem_place, element)?;
                self.scope_insert(&name.name, *bind_span);
            },
            Pattern::Tuple { pats, .. } => {
                // the destructuring elements bind at their addresses in
                // the fetched element's span, borrowed the const
                // convention — the binding sites' own machinery
                self.bind_tuple_pattern(
                    pats.as_slice(),
                    &TypedPlace {
                        place: elem_place.clone(),
                        ty: element,
                    },
                    false,
                )?;
            },
            Pattern::Wildcard(_) => {
                let words = self.tracked_words(&elem_place, element)?;
                self.rc.discard(&mut self.code, &words);
            },
            _ => {
                return Err(LowerError::unsupported(
                    span,
                    "a destructuring for pattern",
                    "plain names, tuple patterns, and `_` lower this round",
                ))
            },
        }
        match body {
            Expr::Block(block) => {
                for stmt in &block.stmts {
                    self.stmt(stmt)?;
                }
            },
            other => {
                let tp = self.value(other)?;
                let words = self.discard_words(&tp.place, tp.ty, other.span())?;
                self.rc.discard(&mut self.code, &words);
            },
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.bind_label(back);
        self.emit(Op::Data {
            op: DataOp::AddI64,
            dst: Slot::body(ind),
            lhs: Slot::body(ind),
            rhs: Slot::body(step),
        });
        self.loop_back_edge();
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        // an anonymous iterable (a literal, a call result) has no scope
        // of its own: its consumed count dies here, once per evaluation
        self.rc.drop_unnamed(&mut self.code, &[list_w.slot]);
        self.loops.pop();
        Ok(self.unit_value(span)?.place)
    }

    /// One `break`: the binders of the loop body's scopes inwards die at
    /// the jump — their words drop here, once per path, while the
    /// fall-through path drops the same words at the scope pops the jump
    /// skips. Carried and outer words stay untouched: the exit label's
    /// carried cleanup and the outer scope pops run past the loop on
    /// every exit path.
    pub(super) fn break_value(&mut self, span: Span) -> Result<Place, LowerError> {
        let Some(ctx) = self.loops.last().copied() else {
            return Err(LowerError::unsupported(
                span,
                "a break outside a loop",
                "loop exits never cross a function or lambda frame boundary",
            ));
        };
        self.loop_exit_drops(ctx.body_depth);
        self.emit(Op::Jmp { target: ctx.exit });
        Ok(self.unit_value(span)?.place)
    }

    /// One `continue`: the same scope accounting as `break`, jumping to
    /// the backedge start so the induction step and the head re-test
    /// still run.
    pub(super) fn continue_value(&mut self, span: Span) -> Result<Place, LowerError> {
        let Some(ctx) = self.loops.last().copied() else {
            return Err(LowerError::unsupported(
                span,
                "a continue outside a loop",
                "loop exits never cross a function or lambda frame boundary",
            ));
        };
        self.loop_exit_drops(ctx.body_depth);
        self.emit(Op::Jmp { target: ctx.cont });
        Ok(self.unit_value(span)?.place)
    }

    /// The words dying at a loop-exit jump: every binder of the scopes
    /// from the loop's body scope inwards, innermost scope first and
    /// binders in source order inside one scope, matching the scope-pop
    /// drop order the fall-through path takes.
    pub(super) fn loop_exit_drops(&mut self, body_depth: usize) {
        let mut doomed: Vec<Span> = Vec::new();
        for scope in self.scopes[body_depth..].iter().rev() {
            let mut spans: Vec<Span> = scope.values().copied().collect();
            spans.sort_by(|a, b| {
                (a.file.as_u32(), a.start, a.end).cmp(&(b.file.as_u32(), b.start, b.end))
            });
            doomed.extend(spans);
        }
        let dying = self.rc.loop_exit_drops(&doomed);
        Placement::emit_drops(&mut self.code, &dying);
    }

    /// Stages one `list_len` call through CALLG: the list word as
    /// the single real argument, the list instance as the one evidence
    /// word behind it, the total i64 result moved into the caller's
    /// word. The list word stages as the native's transient borrow
    /// window.
    pub(super) fn list_len_word(&mut self, list: u8, dst: u8) -> Result<(), LowerError> {
        let generic = GENERIC_NATIVES
            .iter()
            .find(|native| native.name == "list_len")
            .expect("the list_len body is tabled");
        self.guard_staged_count(2)?;
        self.rc.native_open();
        self.rc.native_reads([list]);
        self.overlay_site(&[SlotType::Ref, SlotType::I64], &[SlotType::I64]);
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(list),
        });
        self.rc.staged_native(list, false);
        let evidence = self.pool(Constant::I64(BuiltinInstance::List as i64));
        self.emit(Op::Kimm {
            dst: Slot::overlay(1),
            kidx: evidence,
        });
        let callee = self.pool(self.funcref_const(Callee::Generic(generic)));
        self.emit(Op::Callg { callee, nargs: 1 });
        self.rc.native_done(&mut self.code);
        self.emit(Op::Mov {
            dst: Slot::body(dst),
            src: Slot::overlay(0),
        });
        Ok(())
    }

    /// Stages one `list_get` fetch through CALLG: the list word and
    /// the index as the real arguments, the list instance as the one
    /// evidence word behind them, and the ok/none span answering on the
    /// overlay — word 0 the discriminant, then one payload word per
    /// element word (`elem_words`, the native channel's span). The list
    /// word stages as the native's transient borrow window.
    pub(super) fn fetch_element(
        &mut self,
        list: u8,
        index: u8,
        elem_words: &[SlotType],
    ) -> Result<(), LowerError> {
        let generic = GENERIC_NATIVES
            .iter()
            .find(|native| native.name == "list_get")
            .expect("the list_get body is tabled");
        let mut result_words = vec![SlotType::Tag];
        result_words.extend_from_slice(elem_words);
        self.guard_staged_count(3.max(result_words.len()))?;
        self.rc.native_open();
        self.rc.native_reads([list]);
        self.overlay_site(
            &[SlotType::Ref, SlotType::I64, SlotType::I64],
            &result_words,
        );
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(list),
        });
        self.rc.staged_native(list, false);
        self.emit(Op::Mov {
            dst: Slot::overlay(1),
            src: Slot::body(index),
        });
        let evidence = self.pool(Constant::I64(BuiltinInstance::List as i64));
        self.emit(Op::Kimm {
            dst: Slot::overlay(2),
            kidx: evidence,
        });
        let callee = self.pool(self.funcref_const(Callee::Generic(generic)));
        self.emit(Op::Callg { callee, nargs: 2 });
        self.rc.native_done(&mut self.code);
        Ok(())
    }

    /// Gathers one fetched element out of the overlay span into a fresh
    /// element place: one word per element word moved in declaration
    /// order (a struct's flattened words are contiguous), each counted
    /// word produced for the frame — Ref, or Raw (a type variable's
    /// runtime word under a counted instantiation) — because the fetch
    /// handed one count per counted word, which this place owns until
    /// its death. The one-word shape stays a bare word, owned and
    /// kind-checked at its drop.
    pub(super) fn gather_element(
        &mut self,
        elem_words: &[SlotType],
        element: Ty,
    ) -> Result<Place, LowerError> {
        if let [only] = elem_words {
            let elem = self.temp_word(*only)?;
            self.emit(Op::Mov {
                dst: Slot::body(elem),
                src: Slot::overlay(1),
            });
            if *only == SlotType::Ref || *only == SlotType::Raw {
                self.rc.produced(elem);
            }
            return Ok(Place {
                base: elem,
                shape: PlaceShape::Word(*only),
            });
        }
        let base = self.alloc(elem_words)?;
        for (index, slot) in elem_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("span width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(1 + index).expect("overlay word fits u8")),
            });
            // a Raw element word is a type variable's runtime word under a
            // counted instantiation, so the production (the fetch's handed
            // count) registers like a Ref — the RC ops check the runtime
            // kind, matching the one-word shape above
            if matches!(*slot, SlotType::Ref | SlotType::Raw) {
                self.rc.produced(word);
            }
        }
        Ok(Place {
            base,
            shape: PlaceShape::Span(element),
        })
    }

    /// The word kinds of one list element type for the iteration and
    /// membership fetches: one scalar word stays the one-word shape,
    /// a struct's flattened words ride the native channel's span, and
    /// every other shape (tuple, union) keeps the checked-read refusal
    /// family's name — the element cannot form its optional or compare
    /// atomically.
    pub(super) fn element_words(
        &mut self,
        element: Ty,
        span: Span,
    ) -> Result<SpanWords, LowerError> {
        let shape = self.shapes.shape(element)?;
        match &*shape {
            // a fn element's span is the one Ref word the heap position
            // carries — the fetch hands the closure word out under its own
            // count and the binder's death releases it
            Shape::Word(SlotType::Fn) => Ok(vec![SlotType::Ref].into()),
            Shape::Word(
                SlotType::I64
                | SlotType::F64
                | SlotType::Bool
                | SlotType::Tag
                | SlotType::Ref
                | SlotType::Raw,
            ) => self.shapes.words(element),
            Shape::Struct(_) => {
                // the list's edge retention owns counts, never claims
                // (the element guard's own refusal)
                if self.deinit_hook(element, span)?.is_some() {
                    return Err(LowerError::unsupported(
                        span,
                        "a deinit-carrying list element",
                        "the iteration binder would own a claim the list's edge retention cannot \
                         honor; per-span deinit claims own the seam",
                    ));
                }
                self.shapes
                    .words(element)
                    .map_err(|err| err.respanned(span))
            },
            Shape::Tuple(_) => {
                // one positional word per element — the same flat span a
                // struct's flattened words ride, gathered positionally
                // (a tuple element can carry no deinit: a multi-word
                // element refuses its own shape)
                self.shapes
                    .words(element)
                    .map_err(|err| err.respanned(span))
            },
            Shape::Union(_) => {
                // a union element's whole tag-plus-region span, gathered
                // verbatim (the checked read's own multi-atom hand-back
                // shape — the element's own tag word already names the
                // member the binder narrows by); a union member cannot
                // carry a deinit, so the struct arm's guard is vacuous
                self.shapes
                    .words(element)
                    .map_err(|err| err.respanned(span))
            },
            Shape::Word(_) => Err(LowerError::unsupported(
                span,
                "a list element of this shape",
                "iteration and membership bind one scalar word, one struct's flattened span, a \
                 tuple's positional words, or a union's whole span",
            )),
        }
    }

    // ---- the expression walk ---------------------------------------------------
}
