//! The control forms: `if` in value position, `match` in value and tail
//! position, the union dispatch chain, and the value-level arm chain.

use super::*;

impl FnLower<'_, '_, '_> {
    /// An `if` in value position: a result span both arms write into.
    pub(in crate::func) fn if_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
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
        let then_div = self.rc.diverged() || self.diverges_at(then.span);
        // an arm that exited the frame never reaches the join: its
        // handed-back place is the exit's dead answer, so the join widen
        // would emit dead code over a mismatched shape (a statement if's
        // early-exit arm, or a diverging call's — the call answers
        // `never` and the arm's own exit stands in for the join
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
            let else_div = self.rc.diverged() || self.diverges_at(else_expr.span());
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
            // a missing else is the unit else: the checker already
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
    pub(in crate::func) fn match_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
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
    pub(in crate::func) fn match_tail(&mut self, e: &Expr) -> Result<(), LowerError> {
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
    /// the remainder rule's tag-dispatch form, no reordering — so a
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
                let div = self.rc.diverged() || self.diverges_at(arm.body.span());
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
                    if !(self.rc.diverged() || self.diverges_at(arm.body.span())) {
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
                let div = self.rc.diverged() || self.diverges_at(arm.body.span());
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
}
