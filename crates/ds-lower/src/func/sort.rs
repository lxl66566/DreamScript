//! The ord-routed list sort's call-site lowering (todo item 101): the
//! ambient `sort[t: ord]` extern spells no comparator parameter, so the
//! site routes one out of the solved element type's serving ord
//! instance — the `compare` member's planned body mints as the callable
//! word (the named-fn-value channel) and stages beside the list `Ref`,
//! the receiver's list-instance evidence word trailing the real
//! arguments exactly like filter's, and the native's stable merge sort
//! reenters the dispatcher through that word per comparison (the reduce
//! seam's CALLG discipline).

use super::*;

impl FnLower<'_, '_, '_> {
    /// One `sort(xs)` site (the free and UFCS spellings share it): the
    /// checker's per-call bound record answers the ord instance, the
    /// instance's compare body supplies the callable, and the lowering's
    /// generic-native mirror names the CALLG addressing. A bound answer
    /// carrying no instance (an in-scope assumption, the element type
    /// this frame's own type variable) and a compare body carrying
    /// runtime-evidence demands (a generic impl's one body) both refuse
    /// naming the per-instantiation seam those shapes wait on.
    pub(super) fn sort_call(
        &mut self,
        args: &[&Expr],
        ty: Ty,
        call: Span,
        span: Span,
    ) -> Result<Place, LowerError> {
        let generic = crate::native::lookup_generic("sort")
            .ok_or_else(|| mismatch("the sort seam's native mirror"))?;
        let [xs] = args else {
            return Err(mismatch("a sort call's arguments"));
        };
        let Some(record) = self
            .analyzed
            .checked
            .generic_call_bounds
            .get(&call)
            .cloned()
        else {
            return Err(LowerError::unsupported(
                span,
                "a sort call",
                "the checker recorded no ord answer for the element type; report the construct",
            ));
        };
        let &[(_, bound, served, _)] = record.as_slice() else {
            return Err(mismatch("a sort call's bound record"));
        };
        let Some(ord) = self.analyzed.decls.universe.trait_by_name("ord") else {
            return Err(mismatch("the ord trait's declaration"));
        };
        if bound != ord {
            return Err(mismatch("a sort call's ord bound"));
        }
        let Some(instance) = served else {
            return Err(LowerError::unsupported(
                span,
                "a sort call",
                "the element type's ord instance solved only through an in-scope assumption (the \
                 element type is this frame's own type variable); the comparator word needs the \
                 instance's own compare body, and per-instantiation frames are the JIT \
                 specialization tier's, never the interpreter's",
            ));
        };
        let impl_site = self
            .analyzed
            .decls
            .instance_impls
            .get(&instance)
            .copied()
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    "a sort call",
                    "the serving instance's impl item is missing from the declaration tables",
                )
            })?;
        let Some(compare) = self.plan.impl_body(impl_site, "compare") else {
            return Err(LowerError::unsupported(
                span,
                "a sort call",
                "the solving impl's compare body did not plan (it spells no compare member, or a \
                 linked impl's member body is outside this round's seam); report the construct",
            ));
        };
        if !self.plan.functions()[compare as usize].evidence.is_empty() {
            return Err(LowerError::unsupported(
                span,
                "a sort call",
                "the solving impl's compare body carries runtime-evidence demands (a generic \
                 impl's one body); the comparator word needs the monomorphic body, and \
                 per-instantiation frames are the JIT specialization tier's, never the \
                 interpreter's",
            ));
        }
        let list = self.value(xs)?;
        let addr = single(&self.word_addrs(&list.place, list.ty)?, xs.span())?;
        if addr.slot_ty != SlotType::Ref {
            return Err(LowerError::unsupported(
                xs.span(),
                "a sort receiver",
                "the receiver must be the list's one reference word the declaration names",
            ));
        }
        let receiver = self
            .shapes
            .builtin_instance(list.ty)
            .ok_or_else(|| mismatch("a sort receiver's builtin instance"))?;
        if !generic.instances.contains(&receiver) {
            return Err(LowerError::unsupported(
                span,
                "a sort call",
                "the builtin's one body implements no instance of the receiver's type",
            ));
        }
        // the compare body's scalar FuncRef: the named-fn-value channel
        // (a planned module function, no instantiation and no captures)
        let kidx = self.pool(Constant::FuncRef {
            key: FuncRefKey::Dep(0),
            func: compare,
        });
        let callable = self.temp_word(SlotType::Fn)?;
        self.emit(Op::Kconst {
            dst: Slot::body(callable),
            kidx,
        });
        self.scalar_fn_words.insert(callable);
        self.fn_words.insert(callable, compare);
        // the staging: the list `Ref` opens the native's transient
        // borrow window, the callable word follows it, and the receiver's
        // list-instance evidence word trails the real arguments
        self.guard_staged_count(3)?;
        self.rc.native_open();
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(addr.slot),
        });
        self.rc.staged_native(addr.slot, false);
        self.emit(Op::Mov {
            dst: Slot::overlay(1),
            src: Slot::body(callable),
        });
        let evidence = self.pool(Constant::I64(receiver as i64));
        self.emit(Op::Kimm {
            dst: Slot::overlay(2),
            kidx: evidence,
        });
        self.overlay_site(&[SlotType::Ref, SlotType::Fn, SlotType::I64], &[
            SlotType::Ref,
        ]);
        let callee = self.pool(Constant::FuncRef {
            key: self.native_key(crate::native::NativeModule::Builtin),
            func: generic.index,
        });
        self.emit(Op::Callg { callee, nargs: 2 });
        self.rc.native_done(&mut self.code);
        self.result_place(ty, &[SlotType::Ref], span)
    }
}
