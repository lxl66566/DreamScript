//! The match/pattern lowering vocabulary: `if`/`match` arm dispatch,
//! pattern binding over a scrutinee, and member/word test emission.

use super::*;

impl FnLower<'_, '_, '_> {
    // ---- control forms -------------------------------------------------------

    /// An `if` in value position: a result span both arms write into.
    pub(super) fn if_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::If {
            cond, then, else_, ..
        } = e
        else {
            return Err(mismatch("an if expression"));
        };
        let cond_val = self.value(cond)?;
        let cond_word = single(&self.word_addrs(&cond_val.place, cond_val.ty)?, cond.span())?;
        let result = self.join_span(ty)?;
        let else_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_word.slot),
            target: else_lbl,
        });
        // path-fork the ownership map: each arm writes the result span
        // with its own value; the join keeps what both arms kept
        let base = self.rc.save();
        let then_tp = self.block_value(then)?;
        let then_div = self.rc.diverged();
        // an arm that exited the frame never reaches the join: its
        // handed-back place is the exit's dead answer, so the join widen
        // would emit dead code over a mismatched shape (a statement if's
        // early-exit arm) — the arm's own exit stands in for the join
        if !then_div {
            self.refuse_fn_word_answer(
                ty,
                &then_tp,
                then.span,
                "a capturing closure in a fn-typed join arm",
                "an unproven callable in a fn-typed join arm",
            )?;
            self.widen_into(&result, ty, &then_tp, then.span)?;
        }
        let arm_then = self.rc.take();
        let then_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(else_lbl);
        if let Some(else_expr) = else_ {
            let else_tp = self.value(else_expr)?;
            let else_div = self.rc.diverged();
            if !else_div {
                self.refuse_fn_word_answer(
                    ty,
                    &else_tp,
                    else_expr.span(),
                    "a capturing closure in a fn-typed join arm",
                    "an unproven callable in a fn-typed join arm",
                )?;
                self.widen_into(&result, ty, &else_tp, else_expr.span())?;
            }
        } else {
            // a missing else is the unit else [B2]: the checker already
            // types the if as the then-type united with unit
            let unit = self.unit_value(e.span())?;
            self.widen_into(&result, ty, &unit, e.span())?;
        }
        let arm_else = self.rc.take();
        let else_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_then, then_div), (arm_else, else_div)]);
        if let [then, else_] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, then);
            self.splice(epilogue, then_jmp);
            self.arm_epilogue(else_);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// A `match` in value position.
    pub(super) fn match_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::Match {
            scrutinee, arms, ..
        } = e
        else {
            return Err(mismatch("a match expression"));
        };
        let scrut = self.value(scrutinee)?;
        let result = self.join_span(ty)?;
        let join = self.label();
        self.match_arms(&scrut, arms, Some((&result, ty, join)))?;
        self.bind_label(join);
        Ok(result)
    }

    /// A `match` in tail position: every arm returns directly.
    pub(super) fn match_tail(&mut self, e: &Expr) -> Result<(), LowerError> {
        let Expr::Match {
            scrutinee, arms, ..
        } = e
        else {
            return Err(mismatch("a match expression"));
        };
        let scrut = self.value(scrutinee)?;
        self.match_arms(&scrut, arms, None)
    }

    /// Emits one match: the dispatch chain, then every arm's body. Member
    /// arms TAG_TEST and jump in (an or-arm tests each alternative's
    /// tag), the catchall receives the fall-through; a catchall-free
    /// match is exhaustive by the checker's proof, so its dispatch chain
    /// has no live fall-through. A member arm may carry value-level tests
    /// over its own bound payload words (a case constructor pattern's
    /// argument, a struct member pattern's field): the tests run at the
    /// arm section's top and a mismatch slides to the next arm's
    /// dispatch entry, re-running its tag tests over the full scrutinee —
    /// the B2 remainder rule's tag-dispatch form, no reordering — so a
    /// can-fail arm (value tests or a guard) needs a later arm it lands
    /// in.
    fn match_arms(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
    ) -> Result<(), LowerError> {
        if !self.is_span_ty(scrut.ty) {
            return self.match_single(scrut, arms, value);
        }
        let labels: Vec<usize> = arms.iter().map(|_| self.label()).collect();
        // one dispatch-entry label per arm plus the chain tail: a failed
        // value test inside a member arm's section slides to the next
        // entry, continuing the dispatch from that arm onward
        let entries: Vec<usize> = (0..=arms.len()).map(|_| self.label()).collect();
        let arm_atoms: Vec<Vec<TypeId>> = arms
            .iter()
            .map(|arm| {
                self.pattern_atoms(&arm.pat).and_then(|atoms| {
                    atoms
                        .into_iter()
                        .map(|atom| self.pattern_member_atom(scrut, atom, arm.span))
                        .collect()
                })
            })
            .collect::<Result<_, _>>()?;
        let mut catchall = None;
        for (index, arm) in arms.iter().enumerate() {
            let atoms = &arm_atoms[index];
            // every arm owns an entry label: a slide into it finds a
            // bound target, and a catchall's intercepts the chain at its
            // position (arm order kept) unless the tail already jumps
            // there
            self.bind_label(entries[index]);
            if atoms.is_empty() {
                if arm.guard.is_some() {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "a guarded catchall arm over union dispatch",
                        "a failed catchall guard slides to the next entry, and a value no later \
                         arm holds re-enters the catchall through the tail in a loop; bind a name \
                         and test it inside the body",
                    ));
                }
                catchall = Some(index);
                if index + 1 < arms.len() {
                    self.emit(Op::Jmp {
                        target: labels[index],
                    });
                }
                continue;
            }
            // the vocabulary checks over the collected atoms: a tested
            // alternative's payload words are live only under its own
            // tag, so or-alternatives with tests must name one member;
            // and a can-fail arm's slide (value tests or a guard)
            // must land, per atom, in a later settling arm (no tests,
            // no guard) covering it, or the catchall — otherwise the
            // failed test would fall off the checker-proved-exhaustive
            // chain tail into the first section
            let tested = Self::carries_member_tests(&arm.pat);
            if let Pattern::Or { pats, .. } = &arm.pat
                && tested
            {
                let mut distinct: Vec<TypeId> = Vec::new();
                for alt in pats {
                    for atom in self.pattern_atoms(alt)? {
                        let atom = self.pattern_member_atom(scrut, atom, arm.span)?;
                        if !distinct.contains(&atom) {
                            distinct.push(atom);
                        }
                    }
                }
                if distinct.len() > 1 {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "an or-pattern of member tests",
                        "a tested alternative's payload words are live only under its own tag, so \
                         alternatives with payload tests must name one member; split the \
                         alternatives into arms",
                    ));
                }
            }
            if tested || arm.guard.is_some() {
                let mut distinct = atoms.clone();
                distinct.sort_unstable();
                distinct.dedup();
                for atom in &distinct {
                    let lands = arms[index + 1..].iter().enumerate().any(|(ahead, later)| {
                        let at = index + 1 + ahead;
                        let settles =
                            !Self::carries_member_tests(&later.pat) && later.guard.is_none();
                        settles && (arm_atoms[at].is_empty() || arm_atoms[at].contains(atom))
                    });
                    if !lands {
                        return Err(LowerError::unsupported(
                            arm.span,
                            "a member arm's value test or guard",
                            "the failed test would slide past the last arm into no landing \
                             section; add a binding arm for the member or a catchall arm",
                        ));
                    }
                }
            }
            for atom in atoms {
                let cond = self.temp_word(SlotType::Bool)?;
                let src = self.tag_addr(&scrut.place)?;
                let tag = self.shapes.tag_id(*atom)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: labels[index],
                });
            }
        }
        self.bind_label(entries[arms.len()]);
        if let Some(catchall) = catchall {
            self.emit(Op::Jmp {
                target: labels[catchall],
            });
        }
        // path-fork per arm: every arm starts from the dispatch state and
        // the join keeps what every arm kept
        let base = self.rc.save();
        let mut states = Vec::with_capacity(arms.len());
        let mut jumps = Vec::with_capacity(arms.len());
        let mut divs = Vec::with_capacity(arms.len());
        for (index, arm) in arms.iter().enumerate() {
            self.bind_label(labels[index]);
            if arm.ann.is_some() {
                return Err(LowerError::unsupported(
                    arm.span,
                    "an annotated match arm over a union",
                    "annotations narrow through the member machinery; union arms stay unannotated",
                ));
            }
            self.rc.restore(base.clone());
            self.emit_member_tests(&arm.pat, scrut, entries[index + 1])?;
            // a guard runs after the member tests: their failure edge is
            // base-equivalent (borrow reads), while the guard's edge
            // drops the section's transients before the slide
            let mut cleanup = None;
            if let Some(guard) = &arm.guard {
                let copy = self.guard_scrut_copy(arm, scrut)?;
                if let Some(copy) = &copy {
                    self.bind_guarded(&arm.pat, copy, scrut)?;
                } else {
                    self.bind_arm_pattern(&arm.pat, scrut)?;
                }
                let cond = self.value(guard)?;
                let after = self.rc.save();
                // the slide re-enters the dispatch over the base state:
                // a count the guard took would dec again at a later
                // section's exit, so the guard must leave it unchanged
                if base.keys().any(|word| !self.rc.owns(*word)) {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "a guard taking a count over union dispatch",
                        "the failed guard slides to the next arm's entry, which re-enters the \
                         dispatch over the base state; keep a use after the match",
                    ));
                }
                let outstanding: Vec<(u8, Owned)> = after
                    .iter()
                    .filter(|(word, _)| !base.contains_key(word))
                    .map(|(word, owned)| (*word, owned.clone()))
                    .collect();
                let word = single(&self.word_addrs(&cond.place, cond.ty)?, arm.span)?;
                let label = self.label();
                self.emit(Op::JmpifNot {
                    cond: Slot::body(word.slot),
                    target: label,
                });
                cleanup = Some((label, outstanding));
            } else {
                self.bind_arm_pattern(&arm.pat, scrut)?;
            }
            // a value arm decs at its own jump what only it kept; a tail
            // arm exits the frame, its exit path dropping from its own
            // state
            if let Some((result, ty, join)) = value {
                let body_tp = self.value(&arm.body)?;
                let div = self.rc.diverged();
                // an exited arm never reaches the join (if_value's arm
                // comment): its dead answer does not widen
                if !div {
                    self.refuse_fn_word_answer(
                        ty,
                        &body_tp,
                        arm.body.span(),
                        "a capturing closure in a fn-typed join arm",
                        "an unproven callable in a fn-typed join arm",
                    )?;
                    self.widen_into(result, ty, &body_tp, arm.body.span())?;
                }
                states.push(self.rc.take());
                divs.push(div);
                let at = self.code.len();
                self.emit(Op::Jmp { target: join });
                jumps.push(Some(at));
            } else {
                self.tail(&arm.body)?;
                states.push(self.rc.take());
                divs.push(self.rc.diverged());
                jumps.push(None);
            }
            if let Some((label, outstanding)) = cleanup {
                self.bind_label(label);
                Placement::emit_drops(&mut self.code, &outstanding);
                self.emit(Op::Jmp {
                    target: entries[index + 1],
                });
            }
        }
        let forked: Vec<_> = states.into_iter().zip(divs).collect();
        let dying = self.rc.reconcile(&forked);
        let mut shift = 0;
        for (at, dying) in jumps.into_iter().zip(&dying) {
            if let Some(at) = at {
                let mut epilogue = Vec::new();
                Placement::emit_drops(&mut epilogue, dying);
                let len = epilogue.len();
                self.splice(epilogue, at + shift);
                shift += len;
            }
        }
        Ok(())
    }

    /// A match over a non-union scrutinee (a tuple, a scalar, a struct —
    /// one member, no tags to dispatch on): one unguarded catchall arm
    /// lowers as direct destructuring, anything else as a linear test
    /// chain — each arm tests in order, a failed test or guard falls
    /// through to the next arm. A chain that can fail needs one
    /// unguarded catchall arm to close it: literal tests and guards
    /// prove no coverage, so the checker's tuple-arity coverage leaves
    /// the fall-through open. A lone unguarded arm naming the sole
    /// member of a single-member scrutinee is that direct shape too: the
    /// span holds the member's own words, so the arm lowers as its body
    /// with the binders bound there — no tag exists to test.
    fn match_single(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
    ) -> Result<(), LowerError> {
        let sole = match arms {
            [arm] => arm.guard.is_none() && self.names_sole_member(&arm.pat, scrut),
            _ => false,
        };
        let mut catchall = None;
        if !sole {
            let kinds = arms
                .iter()
                .map(|arm| self.value_arm_kind(&arm.pat, scrut))
                .collect::<Result<Vec<ValueArmKind>, _>>()?;
            catchall = arms
                .iter()
                .enumerate()
                .find(|(index, arm)| kinds[*index] == ValueArmKind::Catchall && arm.guard.is_none())
                .map(|(index, _)| index);
            let can_fail =
                kinds.contains(&ValueArmKind::Tested) || arms.iter().any(|arm| arm.guard.is_some());
            if can_fail && catchall.is_none() {
                let span = arms.first().map_or_else(no_span, |arm| arm.span);
                return Err(LowerError::unsupported(
                    span,
                    "a value-level match without a catchall arm",
                    "literal tests and guards prove no coverage, so the checker's exhaustiveness \
                     proof leaves the fall-through open; one unguarded catchall arm closes the \
                     chain",
                ));
            }
        }
        if let [arm] = arms {
            if arm.ann.is_some() {
                return Err(LowerError::unsupported(
                    arm.span,
                    "an annotated match arm",
                    "annotations narrow through the member machinery; value arms stay unannotated \
                     this round",
                ));
            }
            match &arm.pat {
                // a catchall-classified struct pattern (no field tests)
                // destructures directly, its classification already run
                Pattern::Tuple { .. }
                | Pattern::Binding { .. }
                | Pattern::Wildcard(_)
                | Pattern::Struct { .. } => {},
                // the sole member's own spelling: the gate above proved
                // the arm names the scrutinee's one member with untested
                // payload binders, so it destructures directly too
                Pattern::Path { .. } | Pattern::Call { .. } if sole => {},
                _ => {
                    return Err(LowerError::unsupported(
                        pattern_span(&arm.pat),
                        "a member-test pattern on a non-union value",
                        "a single-member scrutinee has no tags to test",
                    ))
                },
            }
            self.bind_arm_pattern(&arm.pat, scrut)?;
            match value {
                Some((result, ty, _join)) => {
                    let body = self.value(&arm.body)?;
                    // an exited arm never reaches the join (if_value's arm
                    // comment): its dead answer does not widen
                    if !self.rc.diverged() {
                        self.refuse_fn_word_answer(
                            ty,
                            &body,
                            arm.body.span(),
                            "a capturing closure in a fn-typed join arm",
                            "an unproven callable in a fn-typed join arm",
                        )?;
                        self.widen_into(result, ty, &body, arm.body.span())?;
                    }
                },
                None => self.tail(&arm.body)?,
            }
            return Ok(());
        }
        self.value_match_chain(
            scrut,
            arms,
            value,
            catchall.expect("the safety check passed"),
        )
    }

    /// The linear arm chain over a non-union value: every arm's section
    /// runs on the fall-through path in arm order (its tests
    /// short-circuit to the next entry on the first mismatch, its guard
    /// jumps there on failure), and each section snapshots its
    /// post-guard state so its body forks from exactly that — the join
    /// reconciles the per-arm states, so a count one guard ate stays
    /// eaten on every later path while a count no reached guard ate
    /// decs at its own arm's join jump. A guarded arm over a
    /// Ref-carrying scrutinee binds copies of the scrutinee's words: a
    /// guard that consumes its binder at the binder's last use must not
    /// eat the scrutinee's count, which later entries still test. The
    /// copy incs at the section's top, before its tests, so every path
    /// reaching the section incs what its decs will eat.
    fn value_match_chain(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
        catchall: usize,
    ) -> Result<(), LowerError> {
        for arm in arms {
            if arm.ann.is_some() {
                return Err(LowerError::unsupported(
                    arm.span,
                    "an annotated match arm",
                    "annotations narrow through the member machinery; value arms stay unannotated \
                     this round",
                ));
            }
        }
        let entries: Vec<usize> = arms.iter().map(|_| self.label()).collect();
        let mut states = Vec::with_capacity(arms.len());
        let mut jumps = Vec::with_capacity(arms.len());
        let mut divs = Vec::with_capacity(arms.len());
        for (index, arm) in arms.iter().enumerate() {
            self.bind_label(entries[index]);
            let fail = entries.get(index + 1).copied().unwrap_or(entries[catchall]);
            let copy = self.guard_scrut_copy(arm, scrut)?;
            self.emit_value_tests(&arm.pat, scrut, fail)?;
            if let Some(guard) = &arm.guard {
                match &copy {
                    Some(copy) => self.bind_guarded(&arm.pat, copy, scrut)?,
                    None => self.bind_arm_pattern(&arm.pat, scrut)?,
                }
                // a guard must not take the scrutinee's own Ref word's
                // count (a direct outer-name reference; the binder copy
                // shields the bound spelling): chain paths bypass the
                // guard while a later entry or body may re-read the word,
                // so the walk refuses the moment it sees the count go — a
                // guard that only reads the word keeps its count when the
                // death sits past the chain (a use after the match)
                let before = self.scrut_owned(scrut)?;
                let cond = self.value(guard)?;
                let eaten = before
                    .iter()
                    .any(|&(word, owned)| owned && !self.rc.owns(word));
                if eaten {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "a guard consuming the scrutinee",
                        "chain paths bypass the guard while a later entry may re-read the word, \
                         so the guard must not take the scrutinee's count; keep a use after the \
                         match, or bind a name for the tested arm instead",
                    ));
                }
                let word = single(&self.word_addrs(&cond.place, cond.ty)?, arm.span)?;
                self.emit(Op::JmpifNot {
                    cond: Slot::body(word.slot),
                    target: fail,
                });
            } else {
                self.bind_arm_pattern(&arm.pat, scrut)?;
            }
            let after_entry = self.rc.save();
            // a value arm decs at its own jump what only it kept; a tail
            // arm exits the frame, its exit path dropping from its own
            // state
            if let Some((result, ty, join)) = value {
                let body_tp = self.value(&arm.body)?;
                let div = self.rc.diverged();
                // an exited arm never reaches the join (if_value's arm
                // comment): its dead answer does not widen
                if !div {
                    self.refuse_fn_word_answer(
                        ty,
                        &body_tp,
                        arm.body.span(),
                        "a capturing closure in a fn-typed join arm",
                        "an unproven callable in a fn-typed join arm",
                    )?;
                    self.widen_into(result, ty, &body_tp, arm.body.span())?;
                }
                states.push(self.rc.take());
                divs.push(div);
                let at = self.code.len();
                self.emit(Op::Jmp { target: join });
                jumps.push(Some(at));
            } else {
                self.tail(&arm.body)?;
                states.push(self.rc.take());
                divs.push(self.rc.diverged());
                jumps.push(None);
            }
            // the next entry's path executed this section's tests and
            // guard but never its body, so the chain walk resumes from
            // the post-guard state (a body that exited the frame also
            // cleared the map; the snapshot is what keeps walking)
            self.rc.restore(after_entry);
        }
        let forked: Vec<_> = states.into_iter().zip(divs).collect();
        let dying = self.rc.reconcile(&forked);
        let mut shift = 0;
        for (at, dying) in jumps.into_iter().zip(&dying) {
            if let Some(at) = at {
                let mut epilogue = Vec::new();
                Placement::emit_drops(&mut epilogue, dying);
                let len = epilogue.len();
                self.splice(epilogue, at + shift);
                shift += len;
            }
        }
        Ok(())
    }

    /// The scrutinee copy a guarded arm binds when the scrutinee carries
    /// Ref words and the pattern binds them: `None` for unguarded arms,
    /// scalar scrutinees (no count rides the words, so the guard cannot
    /// eat one), and binder-free patterns (`_`, an atom naming the sole
    /// member — a guard cannot reach the scrutinee through them). A
    /// deinit-carrying scrutinee refuses: the copy would anchor a second
    /// claim, so the value would deinit twice.
    /// Whether a guarded arm's pattern binds a plain name reading the
    /// scrutinee's words (through the binder, so a guard consuming one
    /// needs the copy): a plain binding, tuple or constructor payload
    /// binders, punned or plain-name struct fields, and an or-arm
    /// through any alternative that binds.
    fn pattern_binds_words(&self, pat: &Pattern) -> bool {
        match pat {
            Pattern::Binding { name, .. } => self.binding_atom(&name.name).is_none(),
            Pattern::Tuple { pats, .. } | Pattern::Call { args: pats, .. } => {
                pats.iter().any(|sub| self.pattern_binds_words(sub))
            },
            Pattern::Struct { fields, .. } => fields.iter().any(|(_, sub)| match sub {
                None => true,
                Some(sub) => self.pattern_binds_words(sub),
            }),
            Pattern::Or { pats, .. } => pats.iter().any(|alt| self.pattern_binds_words(alt)),
            _ => false,
        }
    }

    fn guard_scrut_copy(
        &mut self,
        arm: &MatchArm,
        scrut: &TypedPlace,
    ) -> Result<Option<TypedPlace>, LowerError> {
        let binds = self.pattern_binds_words(&arm.pat);
        if arm.guard.is_none() || !binds {
            return Ok(None);
        }
        let words = self.shapes.words(scrut.ty)?;
        if !words.contains(&SlotType::Ref) {
            return Ok(None);
        }
        if self
            .deinit_claim(scrut.ty, pattern_span(&arm.pat))?
            .is_some()
        {
            return Err(LowerError::unsupported(
                arm.span,
                "a guarded arm over a deinit-carrying value",
                "the guard's binder copy would anchor a second claim, so the value would deinit \
                 twice; the payload-pattern seam covers it",
            ));
        }
        self.scrut_copy(scrut).map(Some)
    }

    /// The scrutinee's Ref words and whether the placement currently
    /// owns each: a guard's consumption check reads the pair before and
    /// after the guard's walk.
    fn scrut_owned(&mut self, scrut: &TypedPlace) -> Result<Vec<(u8, bool)>, LowerError> {
        let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
        Ok(addrs
            .iter()
            .filter(|addr| addr.slot_ty == SlotType::Ref)
            .map(|addr| (addr.slot, self.rc.owns(addr.slot)))
            .collect())
    }

    /// Binds one guarded arm's pattern over its scrutinee copy: the
    /// plain binding aliases the copy's whole span, a tuple pattern its
    /// elements (a literal element tested nothing to bind), a member
    /// arm's fields or payload binders (and an or-arm's last
    /// alternative) read the copy's words, and a wildcard or atom-named
    /// binding binds nothing at all.
    fn bind_guarded(
        &mut self,
        pat: &Pattern,
        copy: &TypedPlace,
        scrut: &TypedPlace,
    ) -> Result<(), LowerError> {
        match pat {
            Pattern::Binding { span, name } if self.binding_atom(&name.name).is_none() => {
                self.binders.insert(*span, copy.place.clone());
                self.rc_bind(*span, &copy.place, scrut.ty)?;
                self.scope_insert(&name.name, *span);
                Ok(())
            },
            Pattern::Tuple { pats, .. } => self.bind_tuple_pattern(pats, copy, true),
            Pattern::Struct { .. } | Pattern::Call { .. } | Pattern::Or { .. } => {
                self.bind_arm_pattern(pat, copy)
            },
            _ => self.bind_arm_pattern(pat, scrut),
        }
    }

    /// A fresh word-for-word copy of the scrutinee: Ref words dup
    /// unconditionally — the source's death position is the checker's,
    /// not this use's, because the fall-through keeps the scrutinee —
    /// and scalar words mov.
    fn scrut_copy(&mut self, scrut: &TypedPlace) -> Result<TypedPlace, LowerError> {
        let words = self.shapes.words(scrut.ty)?;
        let base = self.alloc(&words)?;
        let place = self.place_of_ty(base, scrut.ty)?;
        let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
        for (index, addr) in addrs.iter().enumerate() {
            let word = u8::try_from(index).map_err(|_| mismatch("a scrutinee copy"))?;
            self.emit(Op::Mov {
                dst: Slot::body(base + word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.duped(&mut self.code, base + word, addr.slot);
            }
        }
        Ok(TypedPlace {
            place,
            ty: scrut.ty,
        })
    }

    /// Whether a member arm's pattern carries value-level tests over its
    /// own bound payload words: a case constructor pattern with a tested
    /// argument, a struct member pattern with a tested field, or an
    /// or-arm of such alternatives.
    fn carries_member_tests(pat: &Pattern) -> bool {
        match pat {
            Pattern::Call { args, .. } => args.iter().any(Self::carries_word_tests),
            Pattern::Struct { fields, .. } => fields
                .iter()
                .any(|(_, sub)| sub.as_ref().is_some_and(Self::carries_word_tests)),
            Pattern::Or { pats, .. } => pats.iter().any(Self::carries_member_tests),
            _ => false,
        }
    }

    /// Whether a one-word or nested-struct sub-pattern carries a test:
    /// a literal, an or-ladder of tests, or a nested struct sub-pattern
    /// with a tested inner field at any depth.
    fn carries_word_tests(sub: &Pattern) -> bool {
        match sub {
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => true,
            // a nested struct sub-pattern tests its inner fields at the
            // struct-typed field's own offsets; pure binders and skips
            // test nothing
            Pattern::Struct { fields, .. } => fields
                .iter()
                .any(|(_, sub)| sub.as_ref().is_some_and(Self::carries_word_tests)),
            Pattern::Or { pats, .. } => pats.iter().any(Self::carries_word_tests),
            _ => false,
        }
    }

    /// Emits one member arm's value tests over its own bound payload
    /// words, in payload order, a mismatch sliding to the next dispatch
    /// entry (the value chain's short-circuit discipline over the
    /// member's claimed region offsets; the tested words are borrows
    /// across every arm's tests, the tag word's own discipline).
    /// Binding-only arms and catchalls test nothing.
    fn emit_member_tests(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
        fail: usize,
    ) -> Result<(), LowerError> {
        match pat {
            Pattern::Call { path, args, .. } => {
                let Some(atom) = self.pattern_head_atom(path) else {
                    return Ok(());
                };
                let layout = self.shapes.shape(scrut.ty)?;
                let fields = layout
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member read"))?;
                for (arg, field) in args.iter().zip(fields.iter()) {
                    // a struct-typed payload's nested struct sub-pattern
                    // tests its fields at the payload's own offsets
                    if let Pattern::Struct { .. } = arg {
                        self.emit_struct_sub_tests(arg, scrut.place.base + field.word, fail)?;
                        continue;
                    }
                    if Self::word_sub_kind(arg, "a case payload test")? == ValueArmKind::Tested {
                        let addr = WordAddr {
                            name: field.name.clone(),
                            slot: scrut.place.base + field.word,
                            slot_ty: field.slot(),
                        };
                        self.emit_word_tests(arg, &addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Struct { path, fields, .. } => {
                let Some(atom) = self.pattern_head_atom(path) else {
                    return Ok(());
                };
                let layout = self.shapes.shape(scrut.ty)?;
                let atom = self.pattern_member_atom(scrut, atom, pattern_span(pat))?;
                let member_fields = layout
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member read"))?;
                for (name, sub) in fields {
                    let Some(field) = member_fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    else {
                        return Err(mismatch("a pattern field"));
                    };
                    if let Some(inner @ Pattern::Struct { .. }) = sub {
                        self.emit_struct_sub_tests(inner, scrut.place.base + field.word, fail)?;
                        continue;
                    }
                    if let Some(sub) = sub
                        && Self::word_sub_kind(sub, "a struct field test")? == ValueArmKind::Tested
                    {
                        let addr = WordAddr {
                            name: field.name.clone(),
                            slot: scrut.place.base + field.word,
                            slot_ty: field.slot(),
                        };
                        self.emit_word_tests(sub, &addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Or { pats, .. } => {
                // the alternative ladder: a non-last alternative's tests
                // slide to the next alternative on mismatch and jump past
                // the rest on success; the last one's mismatch slides to
                // the arm's own next entry
                let ok = self.label();
                for (index, sub) in pats.iter().enumerate() {
                    if index + 1 == pats.len() {
                        self.emit_member_tests(sub, scrut, fail)?;
                    } else {
                        let next = self.label();
                        self.emit_member_tests(sub, scrut, next)?;
                        self.emit(Op::Jmp { target: ok });
                        self.bind_label(next);
                    }
                }
                self.bind_label(ok);
                Ok(())
            },
            _ => Ok(()),
        }
    }

    /// Whether one lone arm's pattern names the scrutinee's sole member
    /// with no payload tests: the scrutinee's checked type is that member
    /// alone and the pattern is its bare or constructor spelling. Such a
    /// scrutinee's span holds the member's own words, so the arm lowers
    /// as its body with the binders bound there — no tag exists to test.
    /// Anything else (another member, a tested payload sub-pattern, a
    /// multi-segment path, a name resolving to no atom) answers false and
    /// keeps the member-test refusal below.
    fn names_sole_member(&self, pat: &Pattern, scrut: &TypedPlace) -> bool {
        let (path, args) = match pat {
            Pattern::Path { path, .. } => (path, None),
            Pattern::Call { path, args, .. } => (path, Some(args)),
            _ => return false,
        };
        let Some(atom) = self.pattern_head_atom(path) else {
            return false;
        };
        if self.members(scrut.ty) != [atom] {
            return false;
        }
        // a tested payload sub-pattern can fail and a lone arm has no
        // later landing, so only plain binders and skips qualify
        match args {
            None => true,
            Some(args) => args
                .iter()
                .all(|arg| matches!(arg, Pattern::Binding { .. } | Pattern::Wildcard(_))),
        }
    }

    /// How one arm's pattern tests a non-union scrutinee: `_`, a plain
    /// binding, an atom naming the scrutinee's sole member (always true,
    /// binding nothing), and a destructuring tuple without literal
    /// elements run no test; literal elements and scalar literals test
    /// the value in place, and an or-pattern tests its alternatives'
    /// tests as a disjunction (any catchall alternative makes the whole
    /// arm catch no test).
    fn value_arm_kind(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
    ) -> Result<ValueArmKind, LowerError> {
        if let Pattern::Tuple {
            pats,
            trailing_comma: false,
            ..
        } = pat
            && pats.len() == 1
        {
            return self.value_arm_kind(&pats[0], scrut);
        }
        match pat {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(ValueArmKind::Catchall),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                Ok(ValueArmKind::Tested)
            },
            Pattern::Tuple { pats, .. } => {
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Tuple(s) = &*shape else {
                    return Err(mismatch("a tuple destructuring"));
                };
                if pats.len() != s.fields.len() {
                    return Err(mismatch("a tuple pattern's arity"));
                }
                let mut tested = false;
                for sub in pats {
                    tested |=
                        Self::word_sub_kind(sub, "a tuple element test")? == ValueArmKind::Tested;
                }
                Ok(if tested { ValueArmKind::Tested } else { ValueArmKind::Catchall })
            },
            Pattern::Or { pats, .. } => {
                self.or_binder_agreement(pats, scrut)?;
                for sub in pats {
                    if self.value_arm_kind(sub, scrut)? == ValueArmKind::Catchall {
                        // a catchall alternative always matches, so the
                        // disjunction never fails
                        return Ok(ValueArmKind::Catchall);
                    }
                }
                Ok(ValueArmKind::Tested)
            },
            Pattern::Path { .. } | Pattern::Call { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a member-test pattern on a non-union value",
                "a single-member scrutinee has no tags to test",
            )),
            Pattern::Struct { path, fields, .. } => {
                // the checker proved the pattern's atom is the
                // scrutinee's sole member, so the fields classify
                if self.pattern_head_atom(path).is_none() {
                    return Err(mismatch("a pattern's member atom"));
                }
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Struct(s) = &*shape else {
                    return Err(mismatch("a struct pattern's scrutinee"));
                };
                let mut tested = false;
                for (name, sub) in fields {
                    if s.fields
                        .iter()
                        .all(|f| f.name.as_deref() != Some(name.name.as_str()))
                    {
                        return Err(mismatch("a pattern field"));
                    }
                    if let Some(sub) = sub {
                        tested |= Self::word_sub_kind(sub, "a struct field test")?
                            == ValueArmKind::Tested;
                    }
                }
                Ok(if tested { ValueArmKind::Tested } else { ValueArmKind::Catchall })
            },
        }
    }

    /// The kind of a one-word sub-pattern (a tuple element, a struct
    /// field): `_` and bindings test nothing (bindings bind at arm
    /// entry; an atom-named sub binds nothing per the member-test
    /// predicate), literals test the word, and an or-pattern tests its
    /// alternatives' tests as a disjunction. Wider sub-patterns have no
    /// word to test against.
    fn word_sub_kind(sub: &Pattern, what: &'static str) -> Result<ValueArmKind, LowerError> {
        match sub {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(ValueArmKind::Catchall),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                Ok(ValueArmKind::Tested)
            },
            Pattern::Or { pats, .. } => {
                for alt in pats {
                    if Self::word_sub_kind(alt, what)? == ValueArmKind::Catchall {
                        return Ok(ValueArmKind::Catchall);
                    }
                }
                Ok(ValueArmKind::Tested)
            },
            // a nested struct sub-pattern over a struct-typed field
            // tests its inner fields; pure binders and skips catch
            // nothing
            Pattern::Struct { fields, .. } => {
                let tested = fields
                    .iter()
                    .any(|(_, sub)| sub.as_ref().is_some_and(Self::carries_word_tests));
                Ok(if tested { ValueArmKind::Tested } else { ValueArmKind::Catchall })
            },
            _ => Err(LowerError::unsupported(
                pattern_span(sub),
                what,
                "one word tests as a literal or an or-ladder of literals, binds as a plain name, \
                 or skips as `_`; nested patterns need the word's whole span",
            )),
        }
    }

    /// Every alternative of a value-level or-pattern binds its binders
    /// on the same words: the arm body binds the last alternative's
    /// binders (the scope's name map keeps the last binding), so an
    /// alternative binding a name at a different word would read the
    /// wrong value on its own path. The checker keeps the alternatives'
    /// name set and its order, never the words.
    fn or_binder_agreement(
        &mut self,
        pats: &[Pattern],
        scrut: &TypedPlace,
    ) -> Result<(), LowerError> {
        let words = self.shapes.words(scrut.ty)?;
        let mut agreed: Option<Vec<(String, u8, Vec<SlotType>)>> = None;
        for sub in pats {
            let mut binds = self.alt_binder_words(sub, scrut, 0, &words)?;
            binds.sort_by(|a, b| a.0.cmp(&b.0));
            if let Some(expected) = &agreed
                && expected != &binds
            {
                return Err(LowerError::unsupported(
                    pattern_span(sub),
                    "or-pattern alternatives binding different words",
                    "the arm body binds the last alternative's words, so an alternative binding a \
                     name elsewhere would read the wrong value on its own path",
                ));
            }
            agreed = Some(binds);
        }
        Ok(())
    }

    /// One or-alternative's binder words as (name, word offset, slot
    /// span) triples: a plain binding names the walked span, tuple and
    /// struct alternatives bind through their elements and fields, and
    /// a nested or-ladder contributes every alternative's binders (the
    /// agreement check compares whole alternatives).
    fn alt_binder_words(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
        off: u8,
        slots: &[SlotType],
    ) -> Result<Vec<(String, u8, Vec<SlotType>)>, LowerError> {
        if let Pattern::Tuple {
            pats,
            trailing_comma: false,
            ..
        } = pat
            && pats.len() == 1
        {
            return self.alt_binder_words(&pats[0], scrut, off, slots);
        }
        match pat {
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. } => Ok(Vec::new()),
            Pattern::Binding { name, .. } => {
                if self.binding_atom(&name.name).is_some() {
                    return Err(LowerError::unsupported(
                        pattern_span(pat),
                        "a member-test or-pattern alternative",
                        "an or-pattern over a non-union value tests values; a tag arm needs a \
                         union scrutinee",
                    ));
                }
                Ok(vec![(name.name.clone(), off, slots.to_vec())])
            },
            Pattern::Tuple { pats, .. } => {
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Tuple(s) = &*shape else {
                    return Err(mismatch("a tuple destructuring"));
                };
                if pats.len() != s.fields.len() {
                    return Err(mismatch("a tuple pattern's arity"));
                }
                let mut out = Vec::new();
                for (sub, field) in pats.iter().zip(s.fields.iter()) {
                    out.extend(self.alt_binder_words(
                        sub,
                        scrut,
                        off + field.word,
                        &field.slots,
                    )?);
                }
                Ok(out)
            },
            Pattern::Or { pats, .. } => {
                let mut out = Vec::new();
                for sub in pats {
                    out.extend(self.alt_binder_words(sub, scrut, off, slots)?);
                }
                Ok(out)
            },
            Pattern::Struct { .. } => {
                // the pattern's own atom owns the field offsets (the
                // checker proved it is the walked type), so a nested
                // struct sub-pattern resolves its own fields one level
                // deeper the same way
                let struct_shape = self.struct_sub_shape(pat)?;
                let fields = &struct_shape.fields;
                let Pattern::Struct { fields: pats, .. } = pat else {
                    return Err(mismatch("a struct pattern"));
                };
                let mut out = Vec::new();
                for (name, sub) in pats {
                    let Some(field) = fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    else {
                        return Err(mismatch("a pattern field"));
                    };
                    match sub {
                        // punning binds the field's name at its span
                        None => {
                            out.push((name.name.clone(), off + field.word, field.slots.clone()));
                        },
                        Some(sub) => out.extend(self.alt_binder_words(
                            sub,
                            scrut,
                            off + field.word,
                            &field.slots,
                        )?),
                    }
                }
                Ok(out)
            },
            Pattern::Path { .. } | Pattern::Call { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a member-test pattern on a non-union value",
                "a single-member scrutinee has no tags to test",
            )),
        }
    }

    /// Emits one arm's value tests in place, in element order: each
    /// literal word compares against its constant and jumps to `fail`
    /// on the first mismatch (short-circuit), and an or-pattern's
    /// alternatives test as a disjunction — each alternative's tests
    /// run in order, a mismatch sliding to the next alternative and
    /// the last mismatch jumping to `fail`, so the arm survives on the
    /// first alternative that matches. A catchall arm emits nothing.
    fn emit_value_tests(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
        fail: usize,
    ) -> Result<(), LowerError> {
        if let Pattern::Tuple {
            pats,
            trailing_comma: false,
            ..
        } = pat
            && pats.len() == 1
        {
            return self.emit_value_tests(&pats[0], scrut, fail);
        }
        match pat {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(()),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                let (place, slot) = self.pattern_const(pat)?;
                let word = single(&self.word_addrs(&scrut.place, scrut.ty)?, pattern_span(pat))?;
                self.emit_value_eq(&word, &place, slot, fail)
            },
            Pattern::Tuple { pats, .. } => {
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Tuple(s) = &*shape else {
                    return Err(mismatch("a tuple destructuring"));
                };
                if pats.len() != s.fields.len() {
                    return Err(mismatch("a tuple pattern's arity"));
                }
                let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
                for (sub, addr) in pats.iter().zip(addrs.iter()) {
                    if Self::word_sub_kind(sub, "a tuple element test")? == ValueArmKind::Tested {
                        self.emit_word_tests(sub, addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Struct { path, fields, .. } => {
                if self.pattern_head_atom(path).is_none() {
                    return Err(mismatch("a pattern's member atom"));
                }
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Struct(s) = &*shape else {
                    return Err(mismatch("a struct pattern's scrutinee"));
                };
                // the field words are the scrutinee's own consecutive
                // span words; each tested field compares its word in
                // place, a mismatch jumping to the next arm entry
                let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
                for (name, sub) in fields {
                    let Some(field) = s
                        .fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    else {
                        return Err(mismatch("a pattern field"));
                    };
                    if let Some(inner @ Pattern::Struct { .. }) = sub {
                        self.emit_struct_sub_tests(inner, scrut.place.base + field.word, fail)?;
                        continue;
                    }
                    let addr = addrs
                        .get(usize::from(field.word))
                        .ok_or_else(|| mismatch("a pattern field's word"))?;
                    if let Some(sub) = sub
                        && Self::word_sub_kind(sub, "a struct field test")? == ValueArmKind::Tested
                    {
                        self.emit_word_tests(sub, addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Or { pats, .. } => {
                // the alternative ladder: a non-last alternative's tests
                // slide to the next alternative on mismatch and jump
                // past the remaining ones on success; the last
                // alternative's mismatch is the arm's own failure
                let ok = self.label();
                for (index, sub) in pats.iter().enumerate() {
                    if index + 1 == pats.len() {
                        self.emit_value_tests(sub, scrut, fail)?;
                    } else {
                        let next = self.label();
                        self.emit_value_tests(sub, scrut, next)?;
                        self.emit(Op::Jmp { target: ok });
                        self.bind_label(next);
                    }
                }
                self.bind_label(ok);
                Ok(())
            },
            _ => Err(mismatch("a value test the classification passed")),
        }
    }

    /// Emits a one-word sub-pattern's tests (a tuple element, a struct
    /// field): a literal compares its word and jumps to `fail` on
    /// mismatch, an or-ladder tests its alternatives against the same
    /// word, and bindings and wildcards test nothing.
    fn emit_word_tests(
        &mut self,
        sub: &Pattern,
        addr: &WordAddr,
        fail: usize,
    ) -> Result<(), LowerError> {
        match sub {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(()),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                let (place, slot) = self.pattern_const(sub)?;
                self.emit_value_eq(addr, &place, slot, fail)
            },
            Pattern::Or { pats, .. } => {
                let ok = self.label();
                for (index, alt) in pats.iter().enumerate() {
                    if index + 1 == pats.len() {
                        self.emit_word_tests(alt, addr, fail)?;
                    } else {
                        let next = self.label();
                        self.emit_word_tests(alt, addr, next)?;
                        self.emit(Op::Jmp { target: ok });
                        self.bind_label(next);
                    }
                }
                self.bind_label(ok);
                Ok(())
            },
            _ => Err(mismatch("a value test the classification passed")),
        }
    }

    /// Whether an atom is the constructor of a user-declared generic
    /// struct: the head a nested struct sub-pattern over an applied
    /// field names, whose own atom carries no value shape.
    fn generic_struct_head(&self, atom: TypeId) -> bool {
        let Some(&def) = self.analyzed.decls.defs_of_atoms.get(&atom) else {
            return false;
        };
        self.analyzed
            .decls
            .structs
            .get(&def)
            .is_some_and(|shape| !shape.params.is_empty())
    }

    /// The union member atom one pattern head tests: the head's own
    /// atom when the union carries it, else the member the head's
    /// constructor application matches — a user-declared generic
    /// struct's ctor head names its applied member. A head matching two
    /// applied members refuses (the arm names two tags).
    fn pattern_member_atom(
        &mut self,
        scrut: &TypedPlace,
        head: TypeId,
        span: Span,
    ) -> Result<TypeId, LowerError> {
        let members = self.members(scrut.ty);
        if members.len() <= 1 || members.contains(&head) {
            return Ok(head);
        }
        let applications: Vec<TypeId> = members
            .iter()
            .copied()
            .filter(|&member| {
                matches!(
                    self.universe().atom_decl(member).ok().map(|d| &d.kind),
                    Some(AtomKind::Apply(ctor, _)) if *ctor == head
                )
            })
            .collect();
        match applications.as_slice() {
            [] => Ok(head),
            [one] => Ok(*one),
            _ => Err(LowerError::unsupported(
                span,
                "a member arm over two applications of one constructor",
                "the head names two members; spell the arm's own application or split the members",
            )),
        }
    }

    /// The declared fields of the struct a nested struct sub-pattern's
    /// path names: the checker proved the pattern's atom is the
    /// struct-typed field's own type, so the atom's declaration owns the
    /// nested field offsets. The shape cache shares one allocation per
    /// atom, so the helper copies the nested field list once for its
    /// walk.
    fn struct_sub_shape(&mut self, pat: &Pattern) -> Result<StructShape, LowerError> {
        let Pattern::Struct { path, .. } = pat else {
            return Err(mismatch("a nested struct sub-pattern"));
        };
        let Some(atom) = self.pattern_head_atom(path) else {
            return Err(mismatch("a nested struct sub-pattern's atom"));
        };
        if self.generic_struct_head(atom) {
            return Err(LowerError::unsupported(
                pattern_span(pat),
                "a nested struct sub-pattern over an applied field",
                "the field's type is an application of the head's own struct; bind the field and \
                 destructure the binding in the arm body",
            ));
        }
        let shape = self.shapes.atom_shape(atom)?;
        let Shape::Struct(s) = &*shape else {
            return Err(mismatch("a nested struct sub-pattern's field"));
        };
        Ok(s.clone())
    }

    /// Emits a nested struct sub-pattern's field tests at `base`, the
    /// offset its struct-typed field claims: the inner fields test in
    /// declaration order at the nested span's own consecutive words, a
    /// mismatch sliding to `fail` like any member test, and a
    /// struct-typed inner field's own nested sub-pattern recurses at
    /// the shifted base. Binding-only fields test nothing here.
    fn emit_struct_sub_tests(
        &mut self,
        pat: &Pattern,
        base: u8,
        fail: usize,
    ) -> Result<(), LowerError> {
        let struct_shape = self.struct_sub_shape(pat)?;
        let fields = &struct_shape.fields;
        let Pattern::Struct { fields: pats, .. } = pat else {
            return Err(mismatch("a nested struct sub-pattern"));
        };
        for (name, sub) in pats {
            let Some(field) = fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name.name.as_str()))
            else {
                return Err(mismatch("a pattern field"));
            };
            let Some(sub) = sub else { continue };
            if let Pattern::Struct { .. } = sub {
                self.emit_struct_sub_tests(sub, base + field.word, fail)?;
            } else if Self::word_sub_kind(sub, "a nested struct field test")?
                == ValueArmKind::Tested
            {
                let addr = WordAddr {
                    name: field.name.clone(),
                    slot: base + field.word,
                    slot_ty: field.slot(),
                };
                self.emit_word_tests(sub, &addr, fail)?;
            }
        }
        Ok(())
    }

    /// One equality test against a constant word: the tested word's slot
    /// picks the comparison, a string constant reads its Ref at the use
    /// (the comparison drops a dying constant there), and a mismatch
    /// jumps to `fail`.
    fn emit_value_eq(
        &mut self,
        tested: &WordAddr,
        constant: &Place,
        slot: SlotType,
        fail: usize,
    ) -> Result<(), LowerError> {
        if slot != tested.slot_ty {
            return Err(mismatch("a literal pattern's word class"));
        }
        let op = eq_op(tested.slot_ty)?;
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Data {
            op,
            dst: Slot::body(cond),
            lhs: Slot::body(tested.slot),
            rhs: Slot::body(constant.base),
        });
        if op == DataOp::EqStr {
            // the tested word is a borrow across every arm's test (the
            // union dispatch reads its tag word the same way); the
            // constant dies at its use, its count balanced per pass
            self.rc.read(&mut self.code, &[constant.base]);
        }
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: fail,
        });
        Ok(())
    }

    /// The constant word a literal pattern tests against: the scalar
    /// literal family lowers like the expression literal, a string
    /// pattern its interned constant (interpolation holes refuse).
    fn pattern_const(&mut self, pat: &Pattern) -> Result<(Place, SlotType), LowerError> {
        let intern = |me: &mut Self, text: &str| -> Result<(Place, SlotType), LowerError> {
            let dst = me.temp_word(SlotType::Ref)?;
            let kidx = me.pool_str(text);
            me.kstr(dst, kidx);
            Ok((
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Ref),
                },
                SlotType::Ref,
            ))
        };
        match pat {
            Pattern::Literal { lit, span } => {
                let place = self.literal(lit, *span)?;
                let PlaceShape::Word(slot) = place.shape else {
                    return Err(mismatch("a literal pattern's word"));
                };
                Ok((place, slot))
            },
            Pattern::RawStr { text, .. } => intern(self, text),
            Pattern::Str { parts, span, .. } => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        StrPart::Text { text: run, .. } => text.push_str(run),
                        _ => {
                            return Err(LowerError::unsupported(
                                *span,
                                "a string pattern with interpolation holes",
                                "a pattern tests one constant string; holes have no pattern \
                                 lowering",
                            ))
                        },
                    }
                }
                intern(self, &text)
            },
            _ => Err(mismatch("a literal pattern")),
        }
    }

    /// The member atoms a pattern tests; empty for a catchall arm. A
    /// bare identifier names a member test when the type namespace
    /// carries it [F9] — the same resolution the checker applies, so the
    /// two layers never disagree — and an or-pattern contributes every
    /// alternative's atom.
    fn pattern_atoms(&mut self, pat: &Pattern) -> Result<Vec<TypeId>, LowerError> {
        match pat {
            Pattern::Wildcard(_) => Ok(Vec::new()),
            Pattern::Binding { name, .. } => {
                // The directed probe (F9.5) may resolve an arm's bare label
                // to another declaration's atom than the first namespace
                // holder; the checker's record is the arm's truth.
                if let Some(atom) = self.directed_tag(name.span) {
                    return Ok(vec![atom]);
                }
                Ok(self.binding_atom(&name.name).into_iter().collect())
            },
            Pattern::Or { pats, .. } => {
                let mut atoms = Vec::with_capacity(pats.len());
                for sub in pats {
                    let sub_atoms = self.pattern_atoms(sub)?;
                    let [atom] = sub_atoms.as_slice() else {
                        return Err(LowerError::unsupported(
                            pattern_span(sub),
                            "an or-pattern alternative that is not one member test",
                            "or-arms test tags this round; binding alternatives need their own \
                             seam",
                        ));
                    };
                    atoms.push(*atom);
                }
                Ok(atoms)
            },
            Pattern::Path { path, .. }
            | Pattern::Struct { path, .. }
            | Pattern::Call { path, .. } => self.label_pattern_atoms(path),
            Pattern::Tuple { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a tuple pattern in a member-test position",
                "the dispatch resolves a member arm from the pattern's own shape, and a tuple \
                 pattern names no member; bind the member and destructure it in the arm body",
            )),
            // a case constructor pattern names its case's atom [B3]: the
            // dispatch jumps in on the case's tag and the section tests
            // the pattern's payload sub-patterns over the case's claimed
            // region offsets
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                Err(LowerError::unsupported(
                    pattern_span(pat),
                    "a literal pattern in a member-test position",
                    "a literal tests no member tag; test it inside a constructor pattern's \
                     payload or bind the member and compare in the arm body",
                ))
            },
        }
    }

    /// The member atoms a pattern's head path tests: a bare label reads
    /// the name-namespace atom, a qualified `type::label` (F9.5) the
    /// member the checker picked and recorded at the label's span.
    fn label_pattern_atoms(&self, path: &Path) -> Result<Vec<TypeId>, LowerError> {
        match path.segments.as_slice() {
            [only] => self
                .path_atom(&only.name)
                .map(|atom| vec![atom])
                .ok_or_else(|| mismatch("a pattern's member atom")),
            [_head, tail] => self
                .analyzed
                .checked
                .directed_tags
                .get(&tail.span)
                .map(|&atom| vec![atom])
                .ok_or_else(|| mismatch("a pattern's member atom")),
            _ => Err(LowerError::unsupported(
                path.span,
                "a multi-segment pattern path",
                "qualified patterns past two segments are not in the contract",
            )),
        }
    }

    /// The member atom a pattern's head path names: a bare label reads the
    /// name namespace, a qualified `type::label` (F9.5) the member the
    /// checker recorded at the label's span.
    fn pattern_head_atom(&self, path: &Path) -> Option<TypeId> {
        match path.segments.as_slice() {
            [only] => self.path_atom(&only.name),
            [_head, tail] => self.directed_tag(tail.span),
            _ => None,
        }
    }

    /// The member atom a bare identifier names in a pattern, mirroring
    /// the checker: the predeclared `none`, or a constructor type's
    /// atom (a linked case's def through the name index the linked
    /// install filled); `None` means the identifier is a binding (a
    /// catchall).
    fn binding_atom(&self, name: &str) -> Option<TypeId> {
        if name == "none" && !self.analyzed.package.declares("none") {
            return Some(TypeId::NONE);
        }
        let def = self
            .analyzed
            .package
            .lookup_type(name)
            .or_else(|| self.analyzed.decls.foreign_type(name))?;
        self.analyzed.decls.atoms.get(&def).copied()
    }

    /// Binds one arm's pattern binders to scrutinee words.
    /// Binds a nested struct sub-pattern's fields at `base`, the offset
    /// its struct-typed field claims: the same field vocabulary as an
    /// arm's own struct pattern — punning, plain names, or-ladders
    /// through their last alternative — binding borrowed aliases at the
    /// nested span's own consecutive words; a struct-typed inner
    /// field's own nested sub-pattern recurses at the shifted base, and
    /// tested fields (the section's or the chain's) bind nothing.
    /// Binds a nested struct sub-pattern's fields at `base`, the offset
    /// its struct-typed field claims: the same field vocabulary as an
    /// arm's own struct pattern — punning, plain names, or-ladders
    /// through their last alternative — binding borrowed aliases at the
    /// nested span's own consecutive words; a struct-typed inner
    /// field's own nested sub-pattern recurses at the shifted base, and
    /// tested fields (the section's or the chain's) bind nothing.
    fn bind_struct_sub_pattern(&mut self, pat: &Pattern, base: u8) -> Result<(), LowerError> {
        let struct_shape = self.struct_sub_shape(pat)?;
        let fields = &struct_shape.fields;
        let Pattern::Struct { fields: pats, .. } = pat else {
            return Err(mismatch("a nested struct sub-pattern"));
        };
        for (name, sub) in pats {
            let Some(field) = fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name.name.as_str()))
            else {
                return Err(mismatch("a pattern field"));
            };
            if let Some(inner @ Pattern::Struct { .. }) = sub {
                self.bind_struct_sub_pattern(inner, base + field.word)?;
                continue;
            }
            let bind = match sub {
                None => Some((name.span, name.name.clone())),
                Some(Pattern::Binding { span, name }) => Some((*span, name.name.clone())),
                Some(Pattern::Wildcard(_)) => None,
                Some(sub) => match sub {
                    // the section or the chain tested the literals and
                    // ladders at the field's word; they bind nothing
                    Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => None,
                    Pattern::Or { pats, .. } => match pats.last().map(last_alt) {
                        None
                        | Some(
                            Pattern::Wildcard(_)
                            | Pattern::Literal { .. }
                            | Pattern::Str { .. }
                            | Pattern::RawStr { .. },
                        ) => None,
                        Some(Pattern::Binding { span, name }) => Some((*span, name.name.clone())),
                        _ => {
                            return Err(LowerError::unsupported(
                                pattern_span(sub),
                                "a nested or-ladder field",
                                "a field binds as a plain name or skips as `_`, and tests as a \
                                 literal or an or-ladder of literals",
                            ))
                        },
                    },
                    other => {
                        return Err(LowerError::unsupported(
                            pattern_span(other),
                            "a nested struct field",
                            "fields bind as plain names or skip as `_`, and test as literals at \
                             match time; tuple and member sub-patterns wait on the field's own \
                             span; bind the field and destructure it in the arm body",
                        ))
                    },
                },
            };
            let Some(bind) = bind else { continue };
            let at = base + field.word;
            // a struct-typed inner field binds the span its checked
            // type names, however many words it spans; every other
            // field binds its one word
            let fact = self.binder_fact(bind.0)?;
            let shape = match fact {
                Some(fact) if self.struct_typed(fact.ty) => PlaceShape::Span(fact.ty),
                _ => PlaceShape::Word(field.slot()),
            };
            self.binders.insert(bind.0, Place { base: at, shape });
            if let Some(fact) = fact {
                self.rc_bind_borrowed(bind.0, at, fact.ty)?;
            }
            self.scope_insert(&bind.1, bind.0);
        }
        Ok(())
    }

    fn bind_arm_pattern(&mut self, pat: &Pattern, scrut: &TypedPlace) -> Result<(), LowerError> {
        match pat {
            Pattern::Binding { span, name } => {
                if self.binding_atom(&name.name).is_some() {
                    // a bare tag arm tests membership [F9]; nothing binds
                    return Ok(());
                }
                // a union scrutinee whose place is the union's own tag
                // word alone (an enum payload binder, a struct field
                // bound through a member pattern) binds the catchall
                // over the union's whole span at that word: the field's
                // span is the union's own words, so the use path re-spans
                // onto the member's payload region exactly the
                // wider-scrutinee case — the bare tag word would
                // otherwise stage the tag as the member's payload
                let place = if self.is_span_ty(scrut.ty)
                    && matches!(scrut.place.shape, PlaceShape::Word(SlotType::Tag))
                {
                    Place {
                        base: scrut.place.base,
                        shape: PlaceShape::Span(scrut.ty),
                    }
                } else {
                    scrut.place.clone()
                };
                self.binders.insert(*span, place.clone());
                self.rc_bind(*span, &place, scrut.ty)?;
                self.scope_insert(&name.name, *span);
                Ok(())
            },
            Pattern::Struct { path, fields, .. } => {
                let Some(atom) = self.pattern_head_atom(path) else {
                    return Err(mismatch("a pattern's member atom"));
                };
                let atom = self.pattern_member_atom(scrut, atom, pattern_span(pat))?;
                let PlaceShape::Span(underlying) = scrut.place.shape else {
                    return Err(mismatch("a struct pattern on a scalar"));
                };
                // a union scrutinee places the member's fields at their
                // claimed region offsets and the dispatch slide emits the
                // field tests at the section's top; a bare struct
                // scrutinee places its own consecutive words and the
                // value chain emits them — both test positions skip what
                // the chain already tested
                let layout = self.shapes.shape(underlying)?;
                let (member_fields, value_chain): (&[MemberField], bool) = match &*layout {
                    Shape::Union(_) => (
                        layout
                            .member_fields(atom)
                            .ok_or_else(|| mismatch("a union member read"))?,
                        false,
                    ),
                    Shape::Struct(s) => (s.fields.as_slice(), true),
                    _ => return Err(mismatch("a struct pattern's scrutinee")),
                };
                for (name, sub) in fields {
                    let field = member_fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                        .ok_or_else(|| mismatch("a pattern field"))?;
                    // a nested struct sub-pattern over a struct-typed
                    // field binds its fields at the field's own offset,
                    // the same vocabulary one level deeper (both chains
                    // share it)
                    if let Some(inner @ Pattern::Struct { .. }) = sub {
                        self.bind_struct_sub_pattern(inner, scrut.place.base + field.word)?;
                        continue;
                    }
                    // the binder the field names: punning binds the field's
                    // own name, a plain-name sub its binder; on the value
                    // chain a tested field (a literal, or an or-ladder
                    // binding through its last alternative — the
                    // alternatives agree on the same words) binds nothing,
                    // and `_` skips outright
                    let bind = match sub {
                        None => Some((name.span, name.name.clone())),
                        Some(Pattern::Binding { span, name }) => Some((*span, name.name.clone())),
                        Some(Pattern::Wildcard(_)) => None,
                        Some(sub) if value_chain => match sub {
                            Pattern::Literal { .. }
                            | Pattern::Str { .. }
                            | Pattern::RawStr { .. } => None,
                            Pattern::Or { pats, .. } => match pats.last().map(last_alt) {
                                // the last alternative binds nothing when
                                // it tests or skips; the alternatives
                                // agreed on the same binder words
                                None
                                | Some(
                                    Pattern::Wildcard(_)
                                    | Pattern::Literal { .. }
                                    | Pattern::Str { .. }
                                    | Pattern::RawStr { .. },
                                ) => None,
                                Some(Pattern::Binding { span, name }) => {
                                    Some((*span, name.name.clone()))
                                },
                                _ => {
                                    return Err(LowerError::unsupported(
                                        pattern_span(sub),
                                        "a nested pattern field",
                                        "fields bind as plain names or skip as `_`, and test as \
                                         literals at match time; nested patterns wait on the \
                                         field's whole-span staging",
                                    ))
                                },
                            },
                            _ => {
                                return Err(LowerError::unsupported(
                                    pattern_span(sub),
                                    "a nested pattern field",
                                    "fields bind as plain names or skip as `_`, and test as \
                                     literals at match time; nested patterns wait on the field's \
                                     whole-span staging",
                                ))
                            },
                        },
                        // the union side takes the same resolution: the
                        // dispatch slide tested literals and or-ladders in
                        // the section, so they bind nothing here
                        Some(sub) => match sub {
                            Pattern::Literal { .. }
                            | Pattern::Str { .. }
                            | Pattern::RawStr { .. } => None,
                            Pattern::Or { pats, .. } => match pats.last().map(last_alt) {
                                None
                                | Some(
                                    Pattern::Wildcard(_)
                                    | Pattern::Literal { .. }
                                    | Pattern::Str { .. }
                                    | Pattern::RawStr { .. },
                                ) => None,
                                Some(Pattern::Binding { span, name }) => {
                                    Some((*span, name.name.clone()))
                                },
                                _ => {
                                    return Err(LowerError::unsupported(
                                        pattern_span(sub),
                                        "a nested pattern field",
                                        "nested patterns wait on the field's whole span; bind the \
                                         field and destructure it in the arm body",
                                    ))
                                },
                            },
                            _ => {
                                return Err(LowerError::unsupported(
                                    pattern_span(sub),
                                    "a nested pattern field",
                                    "nested patterns wait on the field's whole span; bind the \
                                     field and destructure it in the arm body",
                                ))
                            },
                        },
                    };
                    let Some(bind) = bind else {
                        continue;
                    };
                    let base = scrut.place.base + field.word;
                    // a struct-typed field binds the span its checked
                    // type names, however many words it spans; every
                    // other field binds its one word
                    let fact = self.binder_fact(bind.0)?;
                    let shape = match fact {
                        Some(fact) if self.struct_typed(fact.ty) => PlaceShape::Span(fact.ty),
                        _ => PlaceShape::Word(field.slot()),
                    };
                    self.binders.insert(bind.0, Place { base, shape });
                    if let Some(fact) = fact {
                        self.rc_bind_borrowed(bind.0, base, fact.ty)?;
                    }
                    self.scope_insert(&bind.1, bind.0);
                }
                Ok(())
            },
            Pattern::Call { path, args, .. } => {
                let Some(atom) = self.pattern_head_atom(path) else {
                    return Err(mismatch("a pattern's member atom"));
                };
                let PlaceShape::Span(underlying) = scrut.place.shape else {
                    return Err(mismatch("a struct pattern on a scalar"));
                };
                let span_layout = self.shapes.shape(underlying)?;
                let scrut_layout = self.shapes.shape(scrut.ty)?;
                let member_fields: &[MemberField] = if self.members(scrut.ty) == [atom] {
                    // the scrutinee's own type is the named member alone,
                    // so its span holds the member's words: the payload at
                    // the atom's own consecutive offsets when the span is
                    // the member's, at the union region's offsets when the
                    // scrutinee refined out of a wider span
                    match &*span_layout {
                        Shape::Union(_) => span_layout
                            .member_fields(atom)
                            .ok_or_else(|| mismatch("a union member read"))?,
                        Shape::Struct(s) if self.members(underlying) == [atom] => {
                            s.fields.as_slice()
                        },
                        _ => return Err(mismatch("a case pattern's scrutinee")),
                    }
                } else {
                    if !matches!(&*span_layout, Shape::Union(_)) {
                        return Err(mismatch("a case pattern's scrutinee"));
                    }
                    scrut_layout
                        .member_fields(atom)
                        .ok_or_else(|| mismatch("a union member read"))?
                };
                if member_fields.len() != args.len() {
                    return Err(mismatch("a case pattern's arity"));
                }
                for (arg, field) in args.iter().zip(member_fields.iter()) {
                    match arg {
                        // a struct-typed payload's nested struct
                        // sub-pattern binds its fields at the payload's
                        // own offsets, the same vocabulary one level
                        // deeper
                        Pattern::Struct { .. } => {
                            self.bind_struct_sub_pattern(arg, scrut.place.base + field.word)?;
                        },
                        // the section tested literals and or-ladders at
                        // the claimed offsets already, and `_` skips;
                        // neither binds anything here
                        Pattern::Wildcard(_)
                        | Pattern::Literal { .. }
                        | Pattern::Str { .. }
                        | Pattern::RawStr { .. } => {},
                        Pattern::Or { pats, .. } => {
                            // binding alternatives bind through the last;
                            // literal ladders tested in the section
                            if let Some(Pattern::Binding { span, name }) = pats.last().map(last_alt)
                            {
                                let base = scrut.place.base + field.word;
                                let fact = self.binder_fact(*span)?;
                                let shape = match fact {
                                    Some(fact) if self.struct_typed(fact.ty) => {
                                        PlaceShape::Span(fact.ty)
                                    },
                                    _ => PlaceShape::Word(field.slot()),
                                };
                                self.binders.insert(*span, Place { base, shape });
                                if let Some(fact) = fact {
                                    self.rc_bind_borrowed(*span, base, fact.ty)?;
                                }
                                self.scope_insert(&name.name, *span);
                            }
                        },
                        Pattern::Binding { span, name } => {
                            // an atom-named payload binding resolves
                            // through the type namespace [F9], so it is a
                            // member test binding nothing — and the
                            // payload has no union of its own to test
                            // (union-typed payload fields refuse at their
                            // shape)
                            if self.binding_atom(&name.name).is_some() {
                                return Err(LowerError::unsupported(
                                    *span,
                                    "an atom-named case payload sub-pattern",
                                    "the name resolves through the type namespace as a type atom, \
                                     so it binds nothing; a payload binder needs a plain name",
                                ));
                            }
                            let base = scrut.place.base + field.word;
                            // a struct-typed payload binds the span its
                            // checked type names, however many words it
                            // spans; every other field binds its one word
                            let fact = self.binder_fact(*span)?;
                            let shape = match fact {
                                Some(fact) if self.struct_typed(fact.ty) => {
                                    PlaceShape::Span(fact.ty)
                                },
                                _ => PlaceShape::Word(field.slot()),
                            };
                            self.binders.insert(*span, Place { base, shape });
                            if let Some(fact) = fact {
                                self.rc_bind_borrowed(*span, base, fact.ty)?;
                            }
                            self.scope_insert(&name.name, *span);
                        },
                        sub => {
                            return Err(LowerError::unsupported(
                                pattern_span(sub),
                                "a nested case payload sub-pattern",
                                "nested patterns wait on the field's whole span; bind the payload \
                                 and destructure it in the arm body",
                            ))
                        },
                    }
                }
                Ok(())
            },
            Pattern::Tuple { pats, .. } => {
                self.bind_tuple_pattern(pats, scrut, true)?;
                Ok(())
            },
            Pattern::Or { pats, .. } => {
                // the alternatives bind the same names on the same words
                // (the classification's agreement check), and the body
                // reads the last alternative's binders — the scope's
                // name map keeps the last binding
                if let Some(last) = pats.last() {
                    self.bind_arm_pattern(last_alt(last), scrut)?;
                }
                Ok(())
            },
            _ => Ok(()),
        }
    }

    /// Binds one tuple pattern's elements to the tuple span's words:
    /// one word per element, bound in place (aliases, the const
    /// convention). A test position skips what the dispatch already
    /// tested: literal elements bind nothing, an atom-named element is
    /// a member test binding nothing [F9], and an or-ladder element
    /// binds its last alternative's binders (the alternatives bind the
    /// same names on the same word) — the same resolution the checker's
    /// pattern walk applies, so the two layers never disagree.
    /// Declaration positions bind every name and refuse literals: a
    /// declaration cannot fail a test.
    pub(super) fn bind_tuple_pattern(
        &mut self,
        pats: &[Pattern],
        scrut: &TypedPlace,
        arm: bool,
    ) -> Result<(), LowerError> {
        let shape = self.shapes.shape(scrut.ty)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(mismatch("a tuple destructuring"));
        };
        if pats.len() != s.fields.len() {
            return Err(mismatch("a tuple pattern's arity"));
        }
        // the elements bind at their whole-word addresses — consecutive
        // words for a bare tuple, the member's claimed offsets for a
        // tuple member read out of a union region
        let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
        for (index, raw) in pats.iter().enumerate() {
            let addr = addrs
                .get(index)
                .ok_or_else(|| mismatch("a tuple pattern's arity"))?;
            let base = addr.slot;
            let sub = match raw {
                Pattern::Or { pats, .. } if arm => pats.last().map_or(raw, last_alt),
                _ => raw,
            };
            match sub {
                Pattern::Wildcard(_) => {},
                Pattern::Binding { span, name } => {
                    if arm && self.binding_atom(&name.name).is_some() {
                        // an atom-named element is a member test [F9];
                        // nothing binds
                        continue;
                    }
                    self.binders.insert(*span, Place {
                        base,
                        shape: PlaceShape::Word(addr.slot_ty),
                    });
                    if let Some(fact) = self.binder_fact(*span)? {
                        self.rc_bind_borrowed(*span, base, fact.ty)?;
                    }
                    self.scope_insert(&name.name, *span);
                },
                Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } if arm => {},
                _ => {
                    return Err(LowerError::unsupported(
                        pattern_span(sub),
                        "a destructuring element pattern",
                        "elements bind as plain names, skip as `_`, or test as literals at match \
                         time; a declaration cannot fail a test, so it never tests",
                    ))
                },
            }
        }
        Ok(())
    }
}
