//! Pattern binding and test emission: the binder walk over a scrutinee
//! (plain, guarded, tuple, struct, and constructor payload binders), the
//! guarded arm's scrutinee copy, and the value/word/member tests an arm
//! runs before its body.

use super::*;

impl FnLower<'_, '_, '_> {
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

    pub(super) fn guard_scrut_copy(
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
    pub(super) fn scrut_owned(
        &mut self,
        scrut: &TypedPlace,
    ) -> Result<Vec<(u8, bool)>, LowerError> {
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
    pub(super) fn bind_guarded(
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
    pub(super) fn carries_member_tests(pat: &Pattern) -> bool {
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
    pub(super) fn emit_member_tests(
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
    pub(super) fn names_sole_member(&self, pat: &Pattern, scrut: &TypedPlace) -> bool {
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
    pub(super) fn value_arm_kind(
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
    pub(super) fn emit_value_tests(
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
    pub(super) fn pattern_member_atom(
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
    /// carries it — the same resolution the checker applies, so the
    /// two layers never disagree — and an or-pattern contributes every
    /// alternative's atom.
    pub(super) fn pattern_atoms(&mut self, pat: &Pattern) -> Result<Vec<TypeId>, LowerError> {
        match pat {
            Pattern::Wildcard(_) => Ok(Vec::new()),
            Pattern::Binding { name, .. } => {
                // The directed probe may resolve an arm's bare label
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
            // a case constructor pattern names its case's atom: the
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
    /// the name-namespace atom, a qualified `type::label` the
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
    /// name namespace, a qualified `type::label` the member the
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

    pub(super) fn bind_arm_pattern(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
    ) -> Result<(), LowerError> {
        match pat {
            Pattern::Binding { span, name } => {
                if self.binding_atom(&name.name).is_some() {
                    // a bare tag arm tests membership; nothing binds
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
                            // through the type namespace, so it is a
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
    /// a member test binding nothing, and an or-ladder element
    /// binds its last alternative's binders (the alternatives bind the
    /// same names on the same word) — the same resolution the checker's
    /// pattern walk applies, so the two layers never disagree.
    /// Declaration positions bind every name and refuse literals: a
    /// declaration cannot fail a test.
    pub(in crate::func) fn bind_tuple_pattern(
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
                        // an atom-named element is a member test;
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
