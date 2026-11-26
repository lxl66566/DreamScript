//! Callables: lambda values, closure words and their captures, extern
//! seams, spawn children and nurseries, the boundary answer words, and
//! the guards that refuse a callable the runtime word kind cannot
//! carry.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// A lambda value: the synthesized module function plus its word.
    /// A non-capturing lambda mints the copyable scalar FuncRef into a Fn
    /// slot (KCONST over the pool's same-module FuncRef, resolved through
    /// the registry id space at issue time). A capturing lambda builds a
    /// heap closure object instead: the KCONST-minted FuncRef word heads
    /// it, the capture words stage on the overlay, and CLOSURE_NEW
    /// allocates the object — acquiring one count per `Ref` capture (the
    /// staged sources keep their own counts and die their ordinary
    /// deaths: copyable bindings copy in, an affine capture's outer
    /// binding is dead to the program by the checker's move) — leaving
    /// one `Ref` word in a Ref-classified slot. That word is the value:
    /// every fn-typed slot stays scalar, so the closure flows only
    /// through value-site places (its construction word, const aliases of
    /// it, a provenance-known call result).
    pub(super) fn lambda_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::Lambda { generics, span, .. } = e else {
            return Err(mismatch("a lambda value"));
        };
        if !generics.is_empty() {
            return Err(LowerError::unsupported(
                *span,
                "a generic lambda value",
                "the word carries no instantiation and a call through it has no site to stage the \
                 body's evidence words; free generic fns carry the generic call surface",
            ));
        }
        // The record itself is the plan's: the plan-time discovery
        // minted it for this span, so the site stages the plan's answer
        // (index, captures, evidence) instead of minting mid-walk — one
        // mint truth between discovery and the walk.
        let Some(mint) = self.plan.minted(self.index, *span) else {
            return Err(LowerError::unsupported(
                *span,
                "a lambda value",
                "the plan minted no record for this site: the discovery pass and the walk \
                 disagree on the mint",
            ));
        };
        let (word, slot) =
            self.child_callable(*span, mint.index, &mint.captures, &mint.evidence)?;
        Ok(Place {
            base: word,
            shape: PlaceShape::Word(slot),
        })
    }

    /// The callable word for a synthesized block function (a lambda value
    /// or a spawned child): a scalar `Fn` word when nothing
    /// captures, a closure object's `Ref` staging the captures otherwise.
    /// The capture refusals are the lambda construction's own — a spawned
    /// child is the same shape.
    pub(super) fn child_callable(
        &mut self,
        span: Span,
        index: u32,
        captures: &[(Span, Ty)],
        evidence: &[RuntimePair],
    ) -> Result<(u8, SlotType), LowerError> {
        let kidx = self.pool(Constant::FuncRef {
            key: FuncRefKey::Dep(0), // the enclosing module
            func: index,
        });
        let func_word = self.temp_word(SlotType::Fn)?;
        self.emit(Op::Kconst {
            dst: Slot::body(func_word),
            kidx,
        });
        if captures.is_empty() && evidence.is_empty() {
            self.fn_words.insert(func_word, index);
            self.scalar_fn_words.insert(func_word);
            return Ok((func_word, SlotType::Fn));
        }
        // stage the capture block on the overlay, in the order the
        // synthesized body binds it; a capture whose place is a closure
        // word refuses (the callable-classification seam: the body's
        // capture slot would classify Fn and the Ref word cannot sit there)
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<u8> = Vec::new();
        for &(bind, ty) in captures {
            let place = match self.binders.get(&bind).cloned() {
                Some(place) => place,
                // a block-local `fn`'s own name captured by a nested
                // callable: the staged word is the fn's own, built in
                // this frame exactly as a use inside the body builds it
                None if self.analyzed.checked.self_fns.contains(&bind) => self.self_fn_word()?,
                None => return Err(self.missing_binder(span)),
            };
            if self.is_closure_word(&place, ty)? {
                return Err(LowerError::unsupported(
                    span,
                    "a lambda capturing a closure",
                    "the capture block would carry a Ref word behind a Fn-classified capture \
                     slot; pass the callable as a parameter instead",
                ));
            }
            // a captured deinit value's hook has no death site: the
            // closure object outlives the frame, but its death frees the
            // captured counts through the object cascade — the VM side
            // dispatches no user code there
            if self.deinit_claim(ty, span)?.is_some() {
                return Err(LowerError::unsupported(
                    span,
                    "a lambda capturing a `~Copyable` value with a deinit",
                    "the closure object's death cannot dispatch the hook yet; pass the value as a \
                     parameter instead",
                ));
            }
            // a fn-typed capture must prove scalar provenance: a
            // callable-defaulted parameter (or an alias of one) may hold a
            // closure's Ref word at runtime, and the copy-in would write it
            // into the Fn-classified capture slot
            if matches!(&*self.shapes.shape(ty)?, Shape::Word(SlotType::Fn)) {
                let addrs = self
                    .word_addrs(&place, ty)
                    .map_err(|err| err.respanned(span))?;
                let proven = addrs.len() == 1 && self.scalar_fn_words.contains(&addrs[0].slot);
                if !proven {
                    return Err(LowerError::unsupported(
                        span,
                        "a lambda capturing a callable binding",
                        "the capture cannot prove a non-capturing scalar; a callable-defaulted \
                         parameter may hold a closure at runtime — pass the callable as a \
                         parameter instead",
                    ));
                }
            }
            for addr in self
                .word_addrs(&place, ty)
                .map_err(|err| err.respanned(span))?
            {
                // the capture is a migration point: the construction
                // acquires its own count per Ref capture inside the op,
                // so a produced temporary (an arena StrCat result, a
                // borrow KSTR) transits here and keeps its counted
                // spelling — the count-bearing capture site refuses the
                // arena and borrow servings. A captured `var`'s binding
                // is dead in the source scope by the checker's move
                // row, so the frame word's own count still dies its
                // ordinary death at the binder's scope end.
                self.rc.captured(addr.slot);
                moves.push(addr.slot);
                staged.push(addr.slot_ty);
            }
        }
        // the lambda's evidence words ride the capture block as its
        // trailing words — one per pair: this frame staged them at its
        // own entry, so the moves read the evidence slots and the
        // closure carries the words into every frame the callable enters
        for pair in evidence {
            let slot = self
                .evidence
                .get(&(pair.var.clone(), pair.method))
                .copied()
                .ok_or_else(|| mismatch("a lambda's runtime-evidence demand"))?;
            moves.push(slot);
            staged.push(SlotType::I64);
        }
        self.guard_staged_count(staged.len())?;
        for (word, slot) in moves.iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(word).expect("overlay word fits u8")),
                src: Slot::body(*slot),
            });
            // the construction acquires its own count per Ref capture
            // inside the op: a staged capture is a borrow, never a dup or
            // a forward, and the captured word's own placement migrated
            // at the collection above
        }
        self.overlay_site(&staged, &[]);
        let dst = self.temp_word(SlotType::Ref)?;
        self.emit(Op::ClosureNew {
            dst,
            func: func_word,
            n: u8::try_from(staged.len()).expect("capture count fits u8"),
        });
        self.rc.produced(dst);
        self.fn_words.insert(dst, index);
        Ok((dst, SlotType::Ref))
    }

    /// One internal seam call to a builtin control native: the
    /// staged words are body slots with their word kinds, the result
    /// words land on the overlay for the caller to read. No seam site is
    /// ever in tail position — the emissions below keep them inside their
    /// own blocks.
    pub(super) fn seam_call(
        &mut self,
        native: &Native,
        staged: &[(u8, SlotType)],
        result: &[SlotType],
    ) -> Result<(), LowerError> {
        self.guard_staged_count(staged.len())?;
        self.rc.native_open();
        let kinds: Vec<SlotType> = staged.iter().map(|(_, kind)| *kind).collect();
        for (index, &(slot, _)) in staged.iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
            // a staged Ref is a transient borrow window, the native call
            // convention's own discipline
            if kinds[index] == SlotType::Ref {
                self.rc.staged_native(slot, false);
            }
        }
        self.overlay_site(&kinds, result);
        let kidx = self.pool(Constant::FuncRef {
            key: self.native_key(native.module),
            func: native.index,
        });
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        Ok(())
    }

    /// A `nursery` block: the seam opens a fresh nursery, the body
    /// runs with its `spawn` sites bound to the id word, and scope exit
    /// joins — the boundary answers a clean join or the first failure's
    /// error payload, and the payload re-spans into the enclosing
    /// function's return union exactly like a `?` exit: child errors
    /// union out at the boundary. A child that died by a real trap
    /// re-raises on the joiner (let-it-crash surfaces at the boundary);
    /// a payload outside the boundary's containment mask refuses in the
    /// seam instead of corrupting the union's tag space.
    pub(super) fn nursery_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::Nursery { span, body } = e else {
            return Err(mismatch("a nursery block"));
        };
        if let Some(exit) = stray_exit(body) {
            return Err(LowerError::unsupported(
                exit,
                "an early exit inside a nursery block",
                "the block's scope exit must join its children; route `return` and `?` around it",
            ));
        }
        let open = crate::native::lookup("nursery_open")
            .ok_or_else(|| mismatch("the nursery open seam"))?;
        let join = crate::native::lookup("nursery_boundary")
            .ok_or_else(|| mismatch("the nursery boundary seam"))?;
        self.scope_push();
        let id_word = self.temp_word(SlotType::I64)?;
        self.seam_call(open, &[], &[SlotType::I64])?;
        self.emit(Op::Mov {
            dst: Slot::body(id_word),
            src: Slot::overlay(0),
        });
        self.nurseries.push(id_word);
        let body_val = self.block_inner(body);
        self.nurseries.pop();
        let _ = body_val?;
        // The payloaded members this block's spawned bodies can cancel:
        // the operands' member sets over the spawned children's cancel
        // sites (the containment proof's own walk), kept to the return
        // union's members that carry payload words — exactly the members
        // the mask must admit beyond its bare set. Each member's flat
        // word span is the boundary answer's tail (one overlay word per
        // payload word in declaration order), so the joiner's overlay
        // widens per member too.
        let layout = self.shapes.shape(self.ret_ty)?;
        let mut payloaded: Vec<(TypeId, u8, &[MemberField])> = Vec::new();
        if let Shape::Union(u) = &*layout {
            let mut operand_spans = Vec::new();
            spawned_cancel_operands(body, &mut operand_spans);
            let mut reachable: Vec<TypeId> = Vec::new();
            for operand in &operand_spans {
                let ty = self.val(*operand)?.ty;
                for member in self.members(ty) {
                    if !reachable.contains(member) {
                        reachable.push(*member);
                    }
                }
            }
            for member in &reachable {
                let Some(entry) = u.member(*member) else {
                    continue;
                };
                if entry.fields.is_empty() {
                    continue;
                }
                let tag = self.shapes.tag_id(*member)?;
                payloaded.push((*member, tag, entry.fields.as_slice()));
            }
            for (_, _, fields) in &payloaded {
                let mut result = vec![SlotType::Tag, SlotType::Tag];
                result.extend(fields.iter().flat_map(|f| f.slots.iter().copied()));
                self.overlay_site(&[], &result);
            }
        }
        let mask = self.boundary_mask(*span, &payloaded)?;
        // the mask word always stages (the native's parameter count is
        // fixed); a zero mask answers nothing but clean joins — every
        // member a spawned cancel site could deliver was refused
        // statically
        let mask_word = self.temp_word(SlotType::I64)?;
        let kidx = self.pool(Constant::I64(mask));
        self.emit(Op::Kimm {
            dst: Slot::body(mask_word),
            kidx,
        });
        self.seam_call(
            join,
            &[(id_word, SlotType::I64), (mask_word, SlotType::I64)],
            &[SlotType::Tag, SlotType::Ref],
        )?;
        let answer = if mask != 0 {
            let words = self.shapes.words(self.ret_ty)?.clone();
            let fail = self.label();
            // the bare delivery's landing label: bound at the whole-tag
            // re-span below, targeted by the payloaded dispatch's
            // fall-through skip
            let bare = self.label();
            let disc = self.temp_word(SlotType::Bool)?;
            self.emit(Op::TagTest {
                dst: Slot::body(disc),
                src: Slot::overlay(0),
                tag: NURSERY_BOUNDARY_ERR,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(disc),
                target: fail,
            });
            // the exit consumes the placement state; the continuation
            // forks from before it
            let ok_state = self.rc.save();
            let answers = self.boundary_answers_mov()?;
            let done = self.label();
            self.emit(Op::Jmp { target: done });
            self.bind_label(fail);
            let target = self.temp_span(self.ret_ty)?;
            if !payloaded.is_empty() {
                // The delivered member is dynamic: dispatch on the
                // channel's tag word (overlay word 1) per payloaded
                // member, gathering the flat payload words into the
                // member's claimed, possibly scattered region offsets —
                // the checked read's ok-arm gather — with the delivered
                // `Ref` words' claims registering as the frame's own
                // (the joiner's ledger acquired them at the death
                // handoff). A bare member falls through to the
                // whole-tag path below.
                let mut labels = Vec::with_capacity(payloaded.len());
                for (_, tag, _) in &payloaded {
                    let cond = self.temp_word(SlotType::Bool)?;
                    self.emit(Op::TagTest {
                        dst: Slot::body(cond),
                        src: Slot::overlay(1),
                        tag: *tag,
                    });
                    let label = self.label();
                    self.emit(Op::Jmpif {
                        cond: Slot::body(cond),
                        target: label,
                    });
                    labels.push(label);
                }
                // a delivered member no payloaded arm claims is bare: the
                // dispatch's fall-through must skip the payloaded arms
                // straight to the bare path, never reading the flat
                // payload words a bare delivery never staged
                self.emit(Op::Jmp { target: bare });
                let n = u8::try_from(words.len()).expect("span width fits u8");
                let transferred: Vec<u8> = (0..n).map(|index| target.base + index).collect();
                for ((_, _, fields), label) in payloaded.iter().zip(labels) {
                    self.bind_label(label);
                    self.emit(Op::Mov {
                        dst: Slot::body(target.base),
                        src: Slot::overlay(1),
                    });
                    self.zero_init_member(target.base, &words, fields)?;
                    let mut flat = 0usize;
                    for field in *fields {
                        for offset in 0..field.width() {
                            let word = target.base
                                + field.word
                                + u8::try_from(offset).expect("field span offset fits u8");
                            self.emit(Op::Mov {
                                dst: Slot::body(word),
                                src: Slot::overlay(
                                    u8::try_from(2 + flat + offset).expect("overlay word fits u8"),
                                ),
                            });
                            if field.slots[offset] == SlotType::Ref {
                                self.rc.produced(word);
                            }
                        }
                        flat += field.width();
                    }
                    self.rc.exit(&mut self.code, &transferred);
                    self.emit(Op::RetSpan {
                        src: Slot::body(target.base),
                        n,
                    });
                    self.rc.restore(ok_state.clone());
                }
            }
            // a bare member's own tag word re-spans whole: bare members'
            // tag ids are module-wide, so the union's tag slot takes it
            // and the dead payload words zero-init (the construction
            // convention)
            self.bind_label(bare);
            self.emit(Op::Mov {
                dst: Slot::body(target.base),
                src: Slot::overlay(1),
            });
            self.zero_init_member(target.base, &words, &[])?;
            let n = u8::try_from(words.len()).expect("span width fits u8");
            let transferred: Vec<u8> = (0..n).map(|index| target.base + index).collect();
            self.rc.exit(&mut self.code, &transferred);
            self.emit(Op::RetSpan {
                src: Slot::body(target.base),
                n,
            });
            self.rc.restore(ok_state);
            self.bind_label(done);
            // the fail paths exited on the pre-channel state, so the
            // collected list's ownership registers here alone — the
            // word the clean path moved, owned by the continuation that
            // reads it
            self.rc.produced(answers);
            Place {
                base: answers,
                shape: PlaceShape::Word(SlotType::Ref),
            }
        } else {
            // no contained error member can answer, so the span is
            // always the clean-join word and the answers list; bug traps
            // ride the call's own trap path
            let answers = self.boundary_answers_mov()?;
            self.rc.produced(answers);
            Place {
                base: answers,
                shape: PlaceShape::Word(SlotType::Ref),
            }
        };
        self.scope_pop();
        Ok(answer)
    }

    /// The join channel's clean-join value: the boundary seam
    /// collects the children's answer spans into one list in spawn
    /// order and delivers its `Ref` at overlay word 1, beside the
    /// discriminant. The move alone emits here; the word's ownership
    /// registers at the caller's join point (the fail paths exit on the
    /// pre-channel state), and the delivered reference's claim row is
    /// the joiner's own from the death handoff, so the block's value
    /// dies through the ordinary placement discipline when the consumer
    /// discards it.
    pub(super) fn boundary_answers_mov(&mut self) -> Result<u8, LowerError> {
        let word = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(word),
            src: Slot::overlay(1),
        });
        Ok(word)
    }

    /// The join's containment mask: one bit per bare member of the
    /// function's return union plus one per payloaded member a spawned
    /// body can cancel — exactly the members a `cancel` payload can be
    /// through this boundary. A non-union return keeps the zero mask
    /// (every cancel site refused statically at its own span).
    pub(super) fn boundary_mask(
        &mut self,
        span: Span,
        payloaded: &[(TypeId, u8, &[MemberField])],
    ) -> Result<i64, LowerError> {
        let mut mask = 0i64;
        let shape = self.shapes.shape(self.ret_ty)?;
        let Shape::Union(u) = &*shape else {
            return Ok(mask);
        };
        for member in self.members(self.ret_ty) {
            let Some(entry) = u.member(*member) else {
                continue;
            };
            if !entry.fields.is_empty() {
                continue;
            }
            // the predeclared none is the return's success spelling, never
            // a containable error member: its platform discriminant sits
            // outside the one-word mask, and a child cancelling with it
            // traps uncontained at the boundary (the runtime's own check)
            if *member == TypeId::NONE {
                continue;
            }
            let tag = self.shapes.tag_id(*member)?;
            if tag >= 64 {
                return Err(LowerError::unsupported(
                    span,
                    "a nursery boundary",
                    "the return union's error-member tags pass the one-word containment mask; \
                     keep the module's tag space under 64",
                ));
            }
            mask |= 1i64 << tag;
        }
        for (_, tag, _) in payloaded {
            let tag = *tag;
            if tag >= 64 {
                return Err(LowerError::unsupported(
                    span,
                    "a nursery boundary",
                    "the return union's error-member tags pass the one-word containment mask; \
                     keep the module's tag space under 64",
                ));
            }
            mask |= 1i64 << tag;
        }
        Ok(mask)
    }

    /// A `spawn` expression: the block becomes a synthesized child
    /// function (the lambda construction's own capture list) and the seam
    /// spawns it into the innermost open nursery as a ready fiber — the
    /// block's join drives it. The expression's value is the unit word.
    pub(super) fn spawn_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::Spawn { span, body } = e else {
            return Err(mismatch("a spawn block"));
        };
        let Some(&id_word) = self.nurseries.last() else {
            return Err(LowerError::unsupported(
                *span,
                "a spawn outside a nursery block",
                "spawn exists only inside a nursery block; bare detach requires an explicit root \
                 nursery or supervisor",
            ));
        };
        if let Some(exit) = stray_exit(body) {
            return Err(LowerError::unsupported(
                exit,
                "an early exit inside a spawned block",
                "a spawned body runs as its own fiber and completes with its own value; route \
                 `return` and `?` through the enclosing function",
            ));
        }
        // The child function's return is its body's answer (the
        // join channel): the checker recorded the body's value at its
        // own span, and the frame's exit carries the answer span the
        // boundary collects in spawn order. A body that only cancels
        // keeps the unit return — its exit code is unreachable, and the
        // empty type shapes nothing.
        let ret = {
            let answer = self.val(body.span)?;
            if answer.diverges || self.members(answer.ty).is_empty() {
                self.val(*span)?.ty
            } else {
                answer.ty
            }
        };
        // The join channel's element discipline: the collected list's
        // edge retention owns counts, never claims, so a deinit-carrying
        // answer refuses exactly like a list literal element — the hook
        // has no drop position inside the channel.
        if self.deinit_hook(ret, *span)?.is_some() {
            return Err(LowerError::unsupported(
                *span,
                "a spawn body's answer",
                "a deinit-carrying answer's hook has no claim inside the join channel's list \
                 retention; per-span deinit claims own the seam",
            ));
        }
        // the body's `cancel` payloads union out through THIS function's
        // return at the enclosing nursery's join: prove the containment
        // at the spawn site, where the return union is in scope
        let mut operands = Vec::new();
        cancel_operands(body, &mut operands);
        for operand in operands {
            let ty = self.val(operand)?.ty;
            let members = self.members(ty);
            if !set_diff(members, self.members(self.ret_ty)).is_empty() {
                return Err(LowerError::unsupported(
                    operand,
                    "a `cancel` payload",
                    "the error member is not contained in the enclosing function's return union; \
                     the nursery boundary unions child errors out through the return",
                ));
            }
        }
        // The child record itself is the plan's: the plan-time discovery
        // minted it for this span with the answer type and evidence the
        // site's own checks above read, so the site stages the plan's
        // answer instead of minting mid-walk — one mint truth between
        // discovery and the walk.
        let Some(mint) = self.plan.minted(self.index, *span) else {
            return Err(LowerError::unsupported(
                *span,
                "a spawned child",
                "the plan minted no record for this site: the discovery pass and the walk \
                 disagree on the mint",
            ));
        };
        let native = crate::native::lookup("nursery_spawn")
            .ok_or_else(|| mismatch("the nursery spawn seam"))?;
        let (callable, kind) =
            self.child_callable(*span, mint.index, &mint.captures, &mint.evidence)?;
        self.seam_call(native, &[(id_word, SlotType::I64), (callable, kind)], &[
            SlotType::I64,
        ])?;
        Ok(self.unit_value(*span)?.place)
    }

    /// Whether a place holds a capturing lambda's closure word: a
    /// fn-typed value in a Ref-classified word, where every fn-typed slot
    /// reads the scalar FuncRef directly.
    pub(super) fn is_closure_word(&mut self, place: &Place, ty: Ty) -> Result<bool, LowerError> {
        Ok(matches!(place.shape, PlaceShape::Word(SlotType::Ref))
            && matches!(&*self.shapes.shape(ty)?, Shape::Word(SlotType::Fn)))
    }

    /// The callee's declared parameter word kinds, when statically known:
    /// a local callee's checked signature or a generic builtin's table
    /// entry. A closure word staged against a `Fn` entry lands in the
    /// callable-defaulted parameter as a borrow; a callee whose parameter
    /// words are unknown keeps the closure-argument refusal.
    pub(super) fn expected_param_words(
        &mut self,
        target: Callee,
        default_dispatch: bool,
    ) -> Option<Vec<SlotType>> {
        match target {
            // a linked impl member's parameter words ride its declared
            // signature in the decls table, which this read does not
            // reach; the closure-argument refusal keeps the site honest.
            // A linked trait default body's receiver is the exception:
            // its `Self` variable's formal declares Raw, so the staging
            // records the one word Raw and the ABI slide admits the
            // pairing whatever the receiver's concrete kind
            Callee::Foreign { def: None, .. } if default_dispatch => Some(vec![SlotType::Raw]),
            // the timer race's staged words are its two parameters — the
            // deadline's i64 and the callable-defaulted `Fn` slot, which
            // admits a closure's `Ref` word (the generic table's own
            // spelling of a lambda parameter)
            Callee::Builtin(native) if native.name == "timeout" => {
                Some(vec![SlotType::I64, SlotType::Fn])
            },
            Callee::Builtin(_) | Callee::Evidence(_) | Callee::Foreign { def: None, .. } => None,
            Callee::Generic(generic) => Some(generic.params.to_vec()),
            // a bound extern's staged words are its written signature's
            // the scalar slice validated at resolution, the same
            // derivation the linked arm takes
            Callee::Bound { def, .. } | Callee::Foreign { def: Some(def), .. } => self
                .def_type(def)
                .ok()
                .flatten()
                .and_then(|fn_ty| fn_sig_of(self.universe(), fn_ty))
                .and_then(|(params, _)| {
                    let mut words = Vec::with_capacity(params.len());
                    for ty in params {
                        words.extend_from_slice(&self.shapes.words(*ty).ok()?);
                    }
                    Some(words)
                }),
            Callee::Local(index) => {
                let planned = self.plan.functions().get(index as usize)?;
                if planned.decl.is_some() {
                    return self.decl_param_words(index);
                }
                planned
                    .def
                    .and_then(|def| self.def_type(def).ok().flatten())
                    .and_then(|fn_ty| fn_sig_of(self.universe(), fn_ty))
                    .and_then(|(params, _)| {
                        let mut words = Vec::with_capacity(params.len());
                        for ty in params {
                            words.extend_from_slice(&self.shapes.words(*ty).ok()?);
                        }
                        Some(words)
                    })
            },
        }
    }

    /// A closure word staged at position `at` of a call's staged span: it
    /// lands legally only in a callable-defaulted `Fn` parameter. A callee
    /// whose parameter words are not statically known, or a non-fn word at
    /// the position, keeps the closure-argument refusal.
    pub(super) fn guard_callable_staged(
        expected: Option<&[SlotType]>,
        at: usize,
        span: Span,
    ) -> Result<(), LowerError> {
        if !expected.is_some_and(|words| words.get(at) == Some(&SlotType::Fn)) {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure as a call argument",
                "the parameter is not a callable-defaulted fn slot; a closure is a Ref word and \
                 only a fn-typed parameter admits it",
            ));
        }
        Ok(())
    }

    /// A closure word against an interior-span generic: the dynamic span
    /// (a fold's init) shifts every word behind it, so the static
    /// expected list names the leading and trailing named words alone.
    /// A closure admits the trailing callable slot — the final named
    /// word — never the span's own words, which are the folded value.
    pub(super) fn guard_interior_closure_slot(
        expected: Option<&[SlotType]>,
        trailing: bool,
        span: Span,
    ) -> Result<(), LowerError> {
        if !trailing || !expected.is_some_and(|words| words.last() == Some(&SlotType::Fn)) {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure as a call argument",
                "the parameter is not a callable-defaulted fn slot; a closure is a Ref word and \
                 only a fn-typed parameter admits it",
            ));
        }
        Ok(())
    }

    /// A callable value widened into a var span refuses here: the span
    /// classifies its fn-typed word Fn, so the re-storable slot takes
    /// only the proven scalar words (a named fn, a non-capturing
    /// lambda). A capturing closure is Ref-classified behind the use,
    /// and an unproven callable word (a callable-defaulted binding, an
    /// alias of one) may hold a closure's Ref at runtime — either lands
    /// a Ref word in the re-storable Fn slot and traps at the store.
    pub(super) fn refuse_closure_widen(
        &mut self,
        val: &TypedPlace,
        declared: Ty,
        span: Span,
    ) -> Result<(), LowerError> {
        if self.is_closure_word(&val.place, declared)? {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure in a var binding",
                "a var's fn-typed slot is re-storable and stays the scalar Fn word; construct a \
                 fresh closure for each binding instead",
            ));
        }
        if matches!(&*self.shapes.shape(declared)?, Shape::Word(SlotType::Fn))
            && let [addr] = self.word_addrs(&val.place, val.ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                span,
                "an unproven callable in a var binding",
                "the word may hold a closure's Ref at runtime; only a named fn or a non-capturing \
                 lambda inhabits a re-storable fn-typed var slot",
            ));
        }
        Ok(())
    }

    /// A callable word widened into a union region's fn-typed payload
    /// slot keeps the var slot's provenance rule: the payload word is
    /// Fn-classified and every extraction re-reads it, so a capturing
    /// closure (a Ref-classified word) or an unproven callable word (a
    /// callable-defaulted binding's, an alias of one — the word may hold
    /// a closure's Ref at runtime) lands a Ref word there and traps;
    /// only the proven scalar words (a named fn, a non-capturing
    /// lambda) inhabit the slot.
    pub(super) fn refuse_union_fn_payload(
        &mut self,
        src: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        // a word addressed inside a span place entered through its own
        // provenance guard (a union region's construction here, a
        // collection element's or a tuple element's staging), so a
        // read-back re-widen trusts it
        if matches!(src.place.shape, PlaceShape::Span(_)) {
            return Ok(());
        }
        if self.is_closure_word(&src.place, src.ty)? {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure in a union payload slot",
                "the union's fn-typed payload slot is Fn-classified and the closure word is a Ref \
                 word; store the closure in a plain fn-typed binding instead",
            ));
        }
        if let [addr] = self.word_addrs(&src.place, src.ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                span,
                "an unproven callable in a union payload slot",
                "the word may hold a closure's Ref at runtime; only a named fn or a non-capturing \
                 lambda inhabits a union's fn-typed payload slot",
            ));
        }
        Ok(())
    }

    /// A fn-typed word answer keeps the var slot's provenance rule at
    /// every staging site — the join arms, the extraction family's
    /// fallbacks and handler bodies: the answer word is Fn-classified
    /// and every later re-read (the call, the binding, the re-storable
    /// slot) trusts it scalar, so a capturing closure (a Ref-classified
    /// word) or an unproven callable word (a callable-defaulted
    /// binding's, an alias of one) lands a Ref word there, traps, and
    /// strands the count; only the proven scalar words answer. A word
    /// addressed inside a span place passed its own guard and stays
    /// trusted.
    pub(super) fn refuse_fn_word_answer(
        &mut self,
        dst_ty: Ty,
        src: &TypedPlace,
        span: Span,
        closure_what: &'static str,
        unproven_what: &'static str,
    ) -> Result<(), LowerError> {
        if !matches!(&*self.shapes.shape(dst_ty)?, Shape::Word(SlotType::Fn)) {
            return Ok(());
        }
        if matches!(src.place.shape, PlaceShape::Span(_)) {
            return Ok(());
        }
        if self.is_closure_word(&src.place, src.ty)? {
            return Err(LowerError::unsupported(
                span,
                closure_what,
                "the answer word is Fn-classified and the closure word is a Ref word; keep the \
                 closure behind a callable-defaulted parameter instead",
            ));
        }
        if let [addr] = self.word_addrs(&src.place, src.ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                span,
                unproven_what,
                "the word may hold a closure's Ref at runtime; only a named fn, a non-capturing \
                 lambda, or a proven call result answers here",
            ));
        }
        Ok(())
    }

    /// Whether a solved type's value region is a union span.
    pub(super) fn is_union_ty(&mut self, ty: Ty) -> Result<bool, LowerError> {
        Ok(matches!(&*self.shapes.shape(ty)?, Shape::Union(_)))
    }

    /// The char member's tag id in the separator natives' union
    /// parameter: the dispatch constant a join/split call stages beside
    /// the union span, so the body reads the region's live payload word
    /// through a fact the lowering holds and no static row can name.
    /// Refuses typed when the call site's solved parameter record is
    /// missing or the union spells no char member.
    pub(super) fn separator_char_tag(
        &mut self,
        params: Option<&[Ty]>,
        span: Span,
    ) -> Result<u8, LowerError> {
        let sep = params
            .and_then(|types| types.get(1).copied())
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    "a separator call",
                    "the call site carries no solved parameter record; report the construct",
                )
            })?;
        let Shape::Union(u) = &*self.shapes.shape(sep)? else {
            return Err(mismatch("a separator call's union parameter"));
        };
        for member in &u.members {
            let named_char = self
                .universe()
                .atom_decl(member.atom)
                .is_ok_and(|decl| decl.name == "char");
            if named_char {
                return Ok(member.tag);
            }
        }
        Err(LowerError::unsupported(
            span,
            "a separator call",
            "the separator union spells no char member; report the construct",
        ))
    }

    /// A capturing closure never enters a widened union region: the
    /// member's Fn slot and the closure word's Ref classification
    /// disagree, and no payload read-back contract covers that
    /// pairing yet.
    pub(super) fn refuse_closure_init(
        &mut self,
        val: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        if self.is_closure_word(&val.place, val.ty)? {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure in a union binding",
                "the member's Fn slot and the closure word's Ref classification disagree; store \
                 the closure in a plain fn-typed binding instead",
            ));
        }
        Ok(())
    }

    /// Why a resolved local has no lowered place. Inside a synthesized
    /// lambda body the missing binder is an enclosing one the capture
    /// walk did not collect — a capture shape the collector missed, so
    /// the walk reports it at its use span instead of misbinding it.
    pub(super) fn missing_binder(&self, span: Span) -> LowerError {
        if self.synth {
            LowerError::unsupported(
                span,
                "a captured binding",
                "the capture collector missed this use's shape; report the construct",
            )
        } else {
            LowerError::unsupported(span, "a local", "it has no lowered place")
        }
    }
}
