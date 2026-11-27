//! Blocks, statements, and locals: the block value and its unit
//! canonicalization, the guarded union-member discards, and local
//! initialization (scalar constants, tuple slots).

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// Lowers one block's statements, returning the block's value.
    pub(super) fn block_value(&mut self, block: &BlockExpr) -> Result<TypedPlace, LowerError> {
        let bracket = self.rc.staging_bracket();
        self.scope_push();
        let result = self.block_inner(block);
        // The tail answer's words escape to the consumer outside the
        // block, so they stage past the scope end: the block's tail read
        // of a local binder answers the binder's own words, and the
        // leaving scope would release their counts — and fire a
        // deinit value's claim — before any consumer claims them. The
        // staged-word computation is fallible, so its error waits past
        // the pop: every exit runs scope_pop and retires the staging
        // bracket, matching block_inner's own failure path.
        let staged = match &result {
            Ok(tp) => self.stage_tail_words(tp, block.span),
            Err(_) => Ok(()),
        };
        self.scope_pop();
        self.rc.unstage_tail_answer(bracket);
        staged?;
        result
    }

    /// Computes and stages the words the block's tail answer carries
    /// past its scope end: the tracked words plus, for a deinit-carrying
    /// answer whose base anchors the frame's claim, the claim's own
    /// base word.
    pub(super) fn stage_tail_words(
        &mut self,
        tp: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        let mut words = self.tracked_words(&tp.place, tp.ty)?;
        if self.deinit_claim(tp.ty, span)?.is_some()
            && self.rc.anchors_claim(tp.place.base)
            && !words.contains(&tp.place.base)
        {
            words.push(tp.place.base);
        }
        self.rc.stage_tail_answer(&words);
        Ok(())
    }

    pub(super) fn block_inner(&mut self, block: &BlockExpr) -> Result<TypedPlace, LowerError> {
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
            }) => self.value(expr),
            Some(Stmt {
                kind: StmtKind::Local(local),
                ..
            }) => self.local_stmt(local),
            // a block-local `fn` in tail position answers like a
            // binding tail: it lowers, the block's value stays unit
            Some(Stmt {
                kind: StmtKind::Fn(f),
                span,
                ..
            }) => {
                self.fn_stmt(f, *span)?;
                self.unit_value(block.span)
            },
            None => self.unit_value(block.span),
        }
    }

    /// The unit value: the frame's canonical unit word, one I64 slot
    /// matching the native unit convention. Unit words are idempotent
    /// and carry no counts, so every unit value in the frame shares the
    /// one place instead of materializing a fresh zero per use.
    pub(super) fn unit_value(&mut self, span: Span) -> Result<TypedPlace, LowerError> {
        let ty = self.val(span).map_or(self.ret_ty, |v| v.ty);
        Ok(TypedPlace {
            place: self.canonical_unit()?,
            ty,
        })
    }

    /// The frame's one unit word: allocated at first use, zero-written by
    /// a single `Kimm 0` `finalize` splices at the frame top — the write
    /// dominates every later read (branch arms read the word on paths
    /// that never ran the first use, and frame words carry the previous
    /// frame's garbage), and no site ever writes the word afterwards, so
    /// one entry store serves the whole frame.
    pub(super) fn canonical_unit(&mut self) -> Result<Place, LowerError> {
        if let Some(base) = self.unit_slot {
            return Ok(Place {
                base,
                shape: PlaceShape::Word(SlotType::I64),
            });
        }
        let base = self.temp_word(SlotType::I64)?;
        self.unit_slot = Some(base);
        Ok(Place {
            base,
            shape: PlaceShape::Word(SlotType::I64),
        })
    }

    /// The unit type `()`: the solved member set is exactly the arity-0
    /// tuple atom — distinct from the unit tags (`none`, `io_err`),
    /// which are Tag words, and from `int`, which shares the I64 slot.
    pub(super) fn is_unit_ty(&self, ty: Ty) -> bool {
        let [atom] = self.members(ty) else {
            return false;
        };
        matches!(
            self.universe().atom_decl(*atom),
            Ok(decl)
                if matches!(&decl.kind, AtomKind::Tuple(elements) if elements.is_empty())
        )
    }

    /// The join span a value-position if/match widens its arms into: a
    /// fresh temp span, except a unit join reuses the canonical unit
    /// word — a statement if pays no result span and no per-arm Mov,
    /// because every arm's unit value is that same word, so each
    /// widen degenerates to the in-place no-op and the expression's
    /// place is the unit word itself.
    pub(super) fn join_span(&mut self, ty: Ty) -> Result<Place, LowerError> {
        if self.is_unit_ty(ty) { self.canonical_unit() } else { self.temp_span(ty) }
    }

    pub(super) fn stmt(&mut self, stmt: &Stmt) -> Result<(), LowerError> {
        match &stmt.kind {
            StmtKind::Local(local) => {
                self.local_stmt(local)?;
                self.note_value_end();
                Ok(())
            },
            StmtKind::Fn(f) => {
                self.fn_stmt(f, stmt.span)?;
                self.note_value_end();
                Ok(())
            },
            StmtKind::Expr(expr) => {
                let tp = self.value(expr)?;
                self.note_value_end();
                // a statement-position value is discarded: its references
                // die at this use when nothing else keeps them
                let words = self.discard_words(&tp.place, tp.ty, expr.span())?;
                self.rc.discard(&mut self.code, &words);
                Ok(())
            },
        }
    }

    /// Records one body-level statement's value end (the resumed-frame
    /// seam): the resume target the REPL computes against the current
    /// round's own lowering, so the discarded statement's decs run at the
    /// next resume and its value never re-computes.
    pub(super) fn note_value_end(&mut self) {
        if self.root && self.scopes.len() == 2 {
            self.value_ends
                .push(u32::try_from(self.code.len()).expect("code length fits u32"));
        }
    }

    /// One block-local `fn` statement: the plan's mint for the
    /// declaration's own span stages the callable word — a capturing fn
    /// constructs its closure object here, the capture migration's
    /// counts the statement's own — and the name binds like a const
    /// aliasing its initializer: the sibling statements read the word,
    /// and the block's scope end releases the closure object's count.
    pub(super) fn fn_stmt(&mut self, f: &ds_parse::FnDecl, span: Span) -> Result<(), LowerError> {
        if !f.generics.is_empty() {
            return Err(LowerError::unsupported(
                span,
                "a generic function value",
                "the word carries no instantiation, and a call through it has no site to stage \
                 the body's evidence words; generic calls name their callee directly",
            ));
        }
        let Some(mint) = self.plan.minted(self.index, span) else {
            return Err(LowerError::unsupported(
                span,
                "a block fn",
                "the plan minted no record for this site: the discovery pass and the walk \
                 disagree on the mint",
            ));
        };
        let declared = match self.binder_fact(f.name.span)? {
            Some(fact) => fact.ty,
            None => self.val(span)?.ty,
        };
        let (word, slot) = self.child_callable(span, mint.index, &mint.captures, &mint.evidence)?;
        let place = Place {
            base: word,
            shape: PlaceShape::Word(slot),
        };
        self.binders.insert(f.name.span, place.clone());
        // the place's own word classification leads: a const aliasing a
        // closure word binds the Ref word the declared fn type does not
        // name, and the binder's death drops it
        self.rc_bind(f.name.span, &place, declared)?;
        self.scope_insert(&f.name.name, f.name.span);
        Ok(())
    }

    /// The callable word of the block-local `fn` one use of its own name
    /// reads: the closure object CALLS wrote into this frame when the fn
    /// captures, the fn's own table entry as a scalar FuncRef word when
    /// nothing captures. `bind` names the declaration's own name span,
    /// `span` the use site.
    pub(super) fn self_fn_word(&mut self) -> Result<Place, LowerError> {
        if let Some(word) = self.capture_word {
            self.fn_words.insert(word, self.index);
            return Ok(Place {
                base: word,
                shape: PlaceShape::Word(SlotType::Ref),
            });
        }
        let kidx = self.pool(Constant::FuncRef {
            key: FuncRefKey::Dep(0), // the enclosing module
            func: self.index,
        });
        let dst = self.temp_word(SlotType::Fn)?;
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx,
        });
        self.scalar_fn_words.insert(dst);
        self.fn_words.insert(dst, self.index);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Fn),
        })
    }

    /// One `const`/`var` binding statement.
    ///
    /// A `const` binder aliases its initializer's place (immutable, every
    /// use reads live slots). A `var` binder owns a dedicated span
    /// sized by its declared type; the initializer widens into it and every
    /// later assignment re-stores the same slots — the rebindable local
    /// keeps its slot span for the function's whole body, so the
    /// placement pass will see one stable storage per var.
    /// A deinit value stored in a union payload refuses at its own span:
    /// the union's death decs the member's payload words and frees
    /// nested objects, but dispatches no user deinit — the member
    /// value's hook would never run.
    pub(super) fn guard_deinit_union_member(
        &mut self,
        src_ty: Ty,
        dst_ty: Ty,
        span: Span,
    ) -> Result<(), LowerError> {
        if self.deinit_claim(src_ty, span)?.is_some() && self.members(dst_ty).len() > 1 {
            return Err(LowerError::unsupported(
                span,
                "a deinit-carrying union member",
                "the union's death decs the member's payload words and frees nested objects, but \
                 dispatches no user deinit; a deinit value's death is its own binding's",
            ));
        }
        Ok(())
    }

    pub(super) fn local_stmt(&mut self, local: &LocalDecl) -> Result<TypedPlace, LowerError> {
        // A one-element paren pattern `(a)` is the inner pattern; the
        // parser keeps the tuple node and both layers collapse it.
        let pat = match &local.pat {
            Pattern::Tuple {
                pats,
                trailing_comma: false,
                ..
            } if pats.len() == 1 => &pats[0],
            other => other,
        };
        // A scalar var's int/float literal init writes the var's own
        // word directly: the constant has no other consumer, a scalar
        // word carries no count, and a literal trips neither of the
        // general path's closure/deinit guards, so its temp + re-store
        // reduces to the one constant load.
        if local.mutable
            && let Some(landed) = self.scalar_var_init(local, pat)?
        {
            return Ok(landed);
        }
        let init = self.value(&local.init)?;
        let declared = match pat {
            Pattern::Binding { span, .. } => self.binder_fact(*span)?.map_or(init.ty, |f| f.ty),
            _ => init.ty,
        };
        // A diverging initializer (`exit(1)` behind an annotated binder)
        // never hands its answer to the binding: the call's own exit ends
        // the path, so the binder stages its declared span unwritten
        // instead of widening dead words over a mismatched shape
        let diverging = self.diverges_at(local.init.span());
        let place = if local.mutable {
            if self.is_var_ty(declared) {
                return Err(LowerError::unsupported(
                    local.span,
                    "a var binding over a type variable",
                    "a reassignment would drop the previous word's count by runtime kind, and the \
                     placement has no overwrite contract for it yet",
                ));
            }
            self.refuse_closure_widen(&init, declared, local.span)?;
            self.guard_deinit_union_member(init.ty, declared, local.span)?;
            // A destructuring var owns one re-storable slot per
            // element: each element's span copies into its own var
            // slot at the binding, so an assignment through an
            // element re-stores its slot alone — the const
            // convention aliases the tuple's words instead.
            if diverging {
                let target = self.temp_span(declared)?;
                self.place_of_ty(target.base, declared)?
            } else if let Pattern::Tuple { pats, .. } = pat {
                self.var_tuple_slots(pats, &init, declared)?;
                init.place.clone()
            } else {
                let target = self.temp_span(declared)?;
                self.widen_into(&target, declared, &init, local.span)?;
                self.place_of_ty(target.base, declared)?
            }
        } else if diverging && declared != init.ty {
            let target = self.temp_span(declared)?;
            self.place_of_ty(target.base, declared)?
        } else if declared != init.ty && self.is_union_ty(declared)? {
            // An annotated const over a narrower source claims the
            // destination union's region under its source member: the
            // binding must physically carry the union's tag, so a bare
            // member literal re-spans through the same construction the
            // if/match join uses instead of aliasing one word.
            let target = self.temp_span(declared)?;
            self.refuse_closure_init(&init, local.span)?;
            self.widen_into(&target, declared, &init, local.span)?;
            self.place_of_ty(target.base, declared)?
        } else {
            init.place.clone()
        };
        match pat {
            Pattern::Binding { span, name } => {
                self.guard_deinit_union_member(init.ty, declared, local.span)?;
                self.binders.insert(*span, place.clone());
                // the place's own word classification leads: a const
                // aliasing a closure word binds the Ref word the declared
                // fn type does not name, and the binder's death drops it
                self.rc_bind(*span, &place, declared)?;
                self.scope_insert(&name.name, *span);
            },
            Pattern::Tuple { pats, .. } => {
                // a mutable tuple already bound its per-element var
                // slots; the const convention aliases in place
                if !local.mutable {
                    self.bind_tuple_pattern(
                        pats.as_slice(),
                        &TypedPlace {
                            place: place.clone(),
                            ty: declared,
                        },
                        false,
                    )?;
                }
            },
            Pattern::Wildcard(_) => {
                // `const _ = e` discards: no binder names the value, so
                // its references die here when nothing else keeps them
                let words = self.discard_words(&place, declared, local.span)?;
                self.rc.discard(&mut self.code, &words);
            },
            _ => {
                return Err(LowerError::unsupported(
                    local.span,
                    "a destructuring binding pattern",
                    "plain names, tuple patterns, and `_` lower this round",
                ))
            },
        }
        Ok(TypedPlace {
            place,
            ty: declared,
        })
    }

    /// `var x = <int/float literal>` over a scalar word: allocates the
    /// var's span and writes the constant straight into it, so the
    /// binding answers its own word with no temp and no re-store.
    /// Declines for every other shape (an annotated non-scalar, a
    /// type-variable word, a suffixed or unparseable spelling), where
    /// the general path keeps its refusals and staging — the probe
    /// precedes the allocation, so a declined literal leaves no dead
    /// frame word behind.
    pub(super) fn scalar_var_init(
        &mut self,
        local: &LocalDecl,
        pat: &Pattern,
    ) -> Result<Option<TypedPlace>, LowerError> {
        let Pattern::Binding { span, name } = pat else {
            return Ok(None);
        };
        let Expr::Literal { lit, .. } = peel_parens(&local.init) else {
            return Ok(None);
        };
        let Some(fact) = self.binder_fact(*span)? else {
            return Ok(None);
        };
        if self.is_var_ty(fact.ty) {
            return Ok(None);
        }
        let Ok(shape) = self.shapes.shape(fact.ty) else {
            return Ok(None);
        };
        let Shape::Word(slot @ (SlotType::I64 | SlotType::F64)) = &*shape else {
            return Ok(None);
        };
        let Some(constant) = scalar_const(lit, *slot) else {
            return Ok(None);
        };
        let target = self.temp_span(fact.ty)?;
        self.scalar_const_into(constant, Slot::body(target.base));
        let place = self.place_of_ty(target.base, fact.ty)?;
        self.guard_deinit_union_member(fact.ty, fact.ty, local.span)?;
        self.binders.insert(*span, place.clone());
        self.rc_bind(*span, &place, fact.ty)?;
        self.scope_insert(&name.name, *span);
        Ok(Some(TypedPlace { place, ty: fact.ty }))
    }

    /// Loads one probed scalar constant into the slot `dst` with the
    /// literal arm's own op spelling (Kimm for I64, Kconst for F64).
    pub(super) fn scalar_const_into(&mut self, constant: ScalarConst, dst: Slot) {
        match constant {
            ScalarConst::I64(value) => {
                let kidx = self.pool(Constant::I64(value));
                self.emit(Op::Kimm { dst, kidx });
            },
            ScalarConst::F64(value) => {
                let kidx = self.pool(Constant::F64(value));
                self.emit(Op::Kconst { dst, kidx });
            },
        }
    }

    /// Binds one destructuring `var`'s elements to fresh per-element
    /// slots: each element's whole word span copies into its own var
    /// slot span (a Ref word inc's at the copy), so an assignment
    /// through an element re-stores its slot alone and the source tuple
    /// keeps its words.
    pub(super) fn var_tuple_slots(
        &mut self,
        pats: &[Pattern],
        init: &TypedPlace,
        declared: Ty,
    ) -> Result<(), LowerError> {
        let shape = self.shapes.shape(declared)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(mismatch("a destructuring var binding"));
        };
        if pats.len() != s.fields.len() {
            return Err(mismatch("a tuple pattern's arity"));
        }
        let addrs = self.word_addrs(&init.place, declared)?;
        let srcs = ordered_field_srcs(&addrs, &s.fields)?;
        for ((sub, field), src) in pats.iter().zip(s.fields.iter()).zip(srcs) {
            let Pattern::Binding { span, name } = sub else {
                match sub {
                    Pattern::Wildcard(_) => continue,
                    _ => {
                        return Err(LowerError::unsupported(
                            pattern_span(sub),
                            "a var destructuring element pattern",
                            "elements bind as plain names or skip as `_`; literals test at match \
                             time and never bind",
                        ))
                    },
                }
            };
            let Some(fact) = self.binder_fact(*span)? else {
                return Err(mismatch("a var destructuring binder"));
            };
            if self.is_var_ty(fact.ty) {
                return Err(LowerError::unsupported(
                    *span,
                    "a var element over a type variable",
                    "a reassignment would drop the previous word's count by runtime kind, and the \
                     placement has no overwrite contract for it yet",
                ));
            }
            let base = src
                .first()
                .ok_or_else(|| mismatch("a tuple element's span"))?
                .slot;
            let place = match &*self.shapes.shape(fact.ty)? {
                Shape::Word(slot) => {
                    if *slot != field.slot() {
                        return Err(mismatch("a var destructuring element word"));
                    }
                    let target = self.temp_word(*slot)?;
                    self.emit(Op::Mov {
                        dst: Slot::body(target),
                        src: Slot::body(base),
                    });
                    if src[0].slot_ty == SlotType::Ref {
                        self.rc.copied(&mut self.code, target, base);
                    }
                    Place {
                        base: target,
                        shape: PlaceShape::Word(*slot),
                    }
                },
                _ => {
                    // a wide element owns a fresh slot span: the element's
                    // words copy in word for word, so an assignment
                    // re-stores the span alone while the source tuple
                    // keeps its words
                    let words = self.shapes.words(fact.ty)?;
                    if words.len() != src.len()
                        || words.iter().zip(src.iter()).any(|(w, a)| *w != a.slot_ty)
                    {
                        return Err(mismatch("a var destructuring element word"));
                    }
                    let target = self.temp_span(fact.ty)?;
                    for (offset, addr) in src.iter().enumerate() {
                        let word =
                            target.base + u8::try_from(offset).expect("element span fits u8");
                        self.emit(Op::Mov {
                            dst: Slot::body(word),
                            src: Slot::body(addr.slot),
                        });
                        if addr.slot_ty == SlotType::Ref {
                            self.rc.copied(&mut self.code, word, addr.slot);
                        }
                    }
                    Place {
                        base: target.base,
                        shape: PlaceShape::Span(fact.ty),
                    }
                },
            };
            self.binders.insert(*span, place.clone());
            self.rc_bind(*span, &place, fact.ty)?;
            self.scope_insert(&name.name, *span);
        }
        Ok(())
    }
}
