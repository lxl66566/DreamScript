//! Parameter and capture binding: the declared parameters into frame
//! slots, the evidence pairs of a generic body, the deinit parameters, a
//! lambda frame's captures, and the name scopes the walk resolves
//! through.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    pub(super) fn bind_params(&mut self, params: &[Param]) -> Result<(), LowerError> {
        for param in params {
            use ds_parse::ParamMode;
            match param.mode {
                // a comptime parameter binds only inside the evaluator's
                // slice: the fenced run stages one reflection-index Tag
                // word per parameter and the body reads it like any
                // borrowed word. The runtime module never plans a comptime
                // function (its calls embed their answers), so this arm
                // refusing at the runtime seam means a comptime parameter
                // reached a surface the round does not lower — an impl or
                // trait member, the checker's refusal class.
                ParamMode::Comptime(_) if matches!(self.comptime, ComptimeRun::Slice) => {},
                ParamMode::Comptime(_) => {
                    return Err(LowerError::unsupported(
                        param.span,
                        "a comptime parameter",
                        "a comptime parameter is a free-fn surface; the evaluator compiles the \
                         free fn's body, so spell the member without one",
                    ));
                },
                // a mut parameter is a re-storable slot over the borrowed
                // staging: the assignment machinery re-stores its words
                // (store_over decs the incoming count the caller handed),
                // so the only seam is the var-typed word whose count the
                // overwrite contract cannot see yet
                ParamMode::Mut(_) if self.param_is_var_ty(param)? => {
                    return Err(LowerError::unsupported(
                        param.span,
                        "a mut parameter over a type variable",
                        "a reassignment would drop the previous word's count by runtime kind, and \
                         the placement has no overwrite contract for it yet",
                    ));
                },
                _ => {},
            }
            let Some(fact) = self.binder_fact(param.name.span)? else {
                return Err(LowerError::unsupported(
                    param.name.span,
                    "a parameter",
                    "it has no checked type",
                ));
            };
            let words = self
                .shapes
                .words(fact.ty)
                .map_err(|err| err.respanned(param.name.span))?;
            let base = self.alloc(&words)?;
            let shape = match &*self.shapes.shape(fact.ty)? {
                Shape::Word(slot) => PlaceShape::Word(*slot),
                _ => PlaceShape::Span(fact.ty),
            };
            let place = Place { base, shape };
            self.binders.insert(param.name.span, place.clone());
            // every parameter owns its incoming reference: the caller
            // handed a count (dup or forward at its staging) and this
            // frame's drops balance it
            for word in self.count_words(base, fact.ty)? {
                self.rc.produced(word);
            }
            match param.mode {
                // a consuming parameter owns the value's death: the
                // deinit claim its type carries registers here, so the
                // hook fires at this frame's own death sites (the scope
                // end, an assignment's replaced content, a re-consuming
                // move) — the caller's staging spent its claim riding
                // the call
                ParamMode::Consume(_) => self.rc_bind(param.name.span, &place, fact.ty)?,
                _ => self.rc_bind_borrowed(param.name.span, base, fact.ty)?,
            }
            self.scopes
                .last_mut()
                .expect("the root scope exists")
                .insert(param.name.name.clone(), param.name.span);
            self.params_len += words.len();
        }
        Ok(())
    }

    /// Whether one parameter's checked type is a generic impl's type
    /// variable (the mut-window seam the var-binding refusal mirrors),
    /// read through the lens: an instance's mut parameter over the
    /// body's variable reads its substituted concrete type.
    pub(super) fn param_is_var_ty(&self, param: &Param) -> Result<bool, LowerError> {
        Ok(self
            .binder_fact(param.name.span)?
            .is_some_and(|fact| self.is_var_ty(fact.ty)))
    }

    /// Binds one generic impl member body's runtime-evidence slots: one
    /// I64 parameter word per recorded pair, appended after the declared
    /// parameters. The call site entering this body stages the words —
    /// one evidence-chain object index per pair, under its own
    /// substitution; the nested sites dispatch through them.
    pub(super) fn bind_evidence(&mut self, pairs: &[RuntimePair]) -> Result<(), LowerError> {
        for pair in pairs {
            let slot = self.temp_word(SlotType::I64)?;
            self.params_len += 1;
            self.evidence.insert((pair.var.clone(), pair.method), slot);
        }
        Ok(())
    }

    /// Binds one lambda's runtime-evidence words as the trailing
    /// capture words: the construction site staged them from the
    /// enclosing frame's evidence slots, and the CALLS copy-in writes
    /// them right after the real captures — one word per pair, read back
    /// by the pair's dispatch. They are plain I64 words — never counted,
    /// never a binder.
    pub(super) fn bind_capture_evidence(
        &mut self,
        pairs: &[RuntimePair],
    ) -> Result<(), LowerError> {
        for pair in pairs {
            let slot = self.alloc(&[SlotType::I64; 1])?;
            self.evidence.insert((pair.var.clone(), pair.method), slot);
        }
        Ok(())
    }

    /// Binds a deinit function's `self` parameter: the dying instance,
    /// borrowed. The caller owns the count and the hook — the drop site
    /// stages the value and decs after the call — so this frame tracks
    /// no ownership on the parameter's words: borrows inside the body
    /// read them and nothing decs at this frame's exit.
    pub(super) fn bind_deinit_param(&mut self, params: &[Param]) -> Result<(), LowerError> {
        let [param] = params else {
            return Err(mismatch("a deinit's parameter"));
        };
        let Some(fact) = self.binder_fact(param.name.span)? else {
            return Err(LowerError::unsupported(
                param.name.span,
                "a deinit's `self`",
                "it has no checked type",
            ));
        };
        let words = self
            .shapes
            .words(fact.ty)
            .map_err(|err| err.respanned(param.name.span))?;
        let base = self.alloc(&words)?;
        let shape = match &*self.shapes.shape(fact.ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(fact.ty),
        };
        self.binders.insert(param.name.span, Place { base, shape });
        self.params_len = words.len();
        self.scopes
            .last_mut()
            .expect("the root scope exists")
            .insert(param.name.name.clone(), param.name.span);
        Ok(())
    }

    /// Binds one synthesized lambda's capture block right after the
    /// params — exactly where the VM's CALLS writes it — one binder per
    /// captured enclosing binding, in the construction's capture order.
    /// Each Ref word is a count the call acquired at its copy-in;
    /// the frame owns it for the call's duration and its exit drops
    /// balance the acquire. A `mutation` frame (a capturing lambda's own
    /// body) additionally records each captured binder's base offset:
    /// the assignment machinery addresses the closure object's capture
    /// slot through it. A spawned child's captures stay copies (the
    /// spawn seam acquires one count per Ref capture and the enclosing
    /// bindings keep their own), so its bindings register borrowed like
    /// a parameter's.
    pub(super) fn bind_captures(
        &mut self,
        captures: &[(Span, Ty)],
        mutation: bool,
    ) -> Result<(), LowerError> {
        let mut offset = 0;
        for &(bind, ty) in captures {
            let words = self.shapes.words(ty).map_err(|err| err.respanned(bind))?;
            let base = self.alloc(&words)?;
            let shape = match &*self.shapes.shape(ty)? {
                Shape::Word(slot) => PlaceShape::Word(*slot),
                _ => PlaceShape::Span(ty),
            };
            self.binders.insert(bind, Place { base, shape });
            for word in self.count_words(base, ty)? {
                self.rc.produced(word);
            }
            // the mutation window needs no borrowed registration: a
            // deinit-carrying capture refused at the construction, so no
            // claim can anchor here; the plain `bound` keeps the
            // checker's last-use rows in force for the frame copy
            if mutation {
                let words = self.count_words(base, ty)?;
                self.rc.bound(bind, &words);
                self.capture_index.insert(bind, offset);
            } else {
                self.rc_bind_borrowed(bind, base, ty)?;
            }
            offset += words.len();
        }
        Ok(())
    }

    pub(super) fn scope_push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Pops one name scope: the leaving binders' still-owned references
    /// drop here, in source order (the checker's deinit discipline).
    pub(super) fn scope_pop(&mut self) {
        let Some(leaving) = self.scopes.pop() else {
            return;
        };
        let mut spans: Vec<Span> = leaving.into_values().collect();
        spans.sort_by(|a, b| {
            (a.file.as_u32(), a.start, a.end).cmp(&(b.file.as_u32(), b.start, b.end))
        });
        for span in spans {
            self.rc.scope_end(&mut self.code, span);
        }
    }

    pub(super) fn scope_insert(&mut self, name: &str, span: Span) {
        self.scopes
            .last_mut()
            .expect("the root scope exists")
            .insert(name.to_owned(), span);
    }

    pub(super) fn scope_lookup(&self, name: &str) -> Option<Span> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    // ---- word addressing ------------------------------------------------------

    /// The atom of a single-atom type.
    pub(super) fn only_atom(&self, ty: Ty, span: Span) -> Result<TypeId, LowerError> {
        match self.members(ty) {
            [only] => Ok(*only),
            _ => Err(LowerError::unsupported(
                span,
                "a value of a multi-atom type",
                "this lowering point needs one member; split the value with a match",
            )),
        }
    }
}
