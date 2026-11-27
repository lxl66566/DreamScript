//! The ord-routed list sort's call-site lowering: the
//! ambient `sort[t: ord]` extern spells no comparator parameter, so the
//! site routes one out of the solved element type's serving ord
//! instance — the `compare` member's planned body mints as the callable
//! word (the named-fn-value channel) and stages beside the list `Ref`,
//! the receiver's list-instance evidence word trailing the real
//! arguments exactly like filter's, and the native's stable merge sort
//! reenters the dispatcher through that word per comparison (the reduce
//! seam's CALLG discipline).

use super::*;

/// The selection an ord-routed two-operand pick performs.
#[derive(Clone, Copy)]
pub(super) enum OrdPick {
    Max,
    Min,
}

impl OrdPick {
    const fn name(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Min => "min",
        }
    }
}

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
        let Some(ord) = self.universe().trait_by_name("ord") else {
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
        self.sort_stage(xs, instance, ty, span)
    }

    /// One `xs.sort()` member site (the bodiless `sort` member of the
    /// list impl, its element bound `ord`): the checker's inherent
    /// bound record answers the ord instance the element type solves,
    /// and the staging is the free spelling's own — the callable word
    /// the instance's compare body supplies, the receiver's
    /// list-instance evidence word trailing it.
    pub(in crate::func) fn sort_member_call(
        &mut self,
        recv: &Expr,
        ty: Ty,
        call: Span,
        span: Span,
    ) -> Result<Place, LowerError> {
        let Some(ord) = self.universe().trait_by_name("ord") else {
            return Err(mismatch("the ord trait's declaration"));
        };
        let Some(record) = self
            .analyzed
            .checked
            .inherent_call_bounds
            .get(&call)
            .cloned()
        else {
            return Err(LowerError::unsupported(
                span,
                "a sort call",
                "the checker recorded no ord answer for the element type; report the construct",
            ));
        };
        let Some((_, _, served)) = record.iter().find(|(_, bound, _)| *bound == ord) else {
            return Err(mismatch("a sort call's bound record"));
        };
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
        self.sort_stage(recv, *instance, ty, span)
    }

    /// One generic `max(a, b)` or `min(a, b)` site. Concrete int calls
    /// enter the existing inherent body through a plain CALL. A concrete
    /// float member keeps its partial-order body; the free generic pair
    /// uses float's total ord instance like every other operand type.
    pub(super) fn ord_pick_call(
        &mut self,
        kind: OrdPick,
        args: &[&Expr],
        ty: Ty,
        call: Span,
        span: Span,
        member: bool,
    ) -> Result<Place, LowerError> {
        let [a, b] = args else {
            return Err(mismatch("an ord-routed pick's arguments"));
        };
        let primitive = self.shapes.builtin_instance(ty);
        if primitive == Some(BuiltinInstance::Int)
            || (member && primitive == Some(BuiltinInstance::Float))
        {
            return self.ord_pick_primitive(kind, a, b, ty, call, span);
        }
        let Some(record) = self.written_generic_call_bounds(call) else {
            return Err(LowerError::unsupported(
                span,
                "an ord-routed pick",
                "the checker recorded no ord answer for the operand type; report the construct",
            ));
        };
        let &[(_, bound, served, _)] = record.as_slice() else {
            return Err(mismatch("an ord-routed pick's bound record"));
        };
        let Some(ord) = self.universe().trait_by_name("ord") else {
            return Err(mismatch("the ord trait's declaration"));
        };
        if bound != ord {
            return Err(mismatch("an ord-routed pick's ord bound"));
        }
        let Some(instance) = served else {
            return Err(LowerError::unsupported(
                span,
                "an ord-routed pick",
                "the operand type's ord instance solved only through an in-scope assumption; the \
                 comparator word needs the instance's own compare body",
            ));
        };
        self.ord_pick_stage(kind, a, b, instance, ty, span)
    }

    /// The primitive member dispatch: two scalar words stage for an
    /// ordinary local or linked inherent call, with no comparator word
    /// or generic-dispatch reentry.
    fn ord_pick_primitive(
        &mut self,
        kind: OrdPick,
        a: &Expr,
        b: &Expr,
        ty: Ty,
        call: Span,
        span: Span,
    ) -> Result<Place, LowerError> {
        let owner = match self.shapes.builtin_instance(ty) {
            Some(BuiltinInstance::Int) => "int",
            Some(BuiltinInstance::Float) => "float",
            _ => return Err(mismatch("an ord pick's primitive result")),
        };
        let method = self
            .analyzed
            .package
            .method_candidates(kind.name())
            .find(|&method| {
                matches!(
                    &self.analyzed.package.method(method).owner,
                    MethodOwner::Inherent(target) if target == owner
                )
            })
            .ok_or_else(|| mismatch("the primitive ord pick's member"))?;
        let dispatch = self.resolve_inherent_method(method, call, span, Some(a))?;
        let key_func = match dispatch {
            TraitDispatch::Inherent { index, .. } => (FuncRefKey::Dep(0), index),
            TraitDispatch::InherentForeign { position, func, .. } => {
                (FuncRefKey::Dep(position), func)
            },
            _ => return Err(mismatch("the primitive ord pick's inherent dispatch")),
        };
        let left = self.value(a)?;
        let left = single(&self.word_addrs(&left.place, left.ty)?, a.span())?;
        let right = self.value(b)?;
        let right = single(&self.word_addrs(&right.place, right.ty)?, b.span())?;
        let slot = match self.shapes.builtin_instance(ty) {
            Some(BuiltinInstance::Int) => SlotType::I64,
            Some(BuiltinInstance::Float) => SlotType::F64,
            _ => unreachable!(),
        };
        if left.slot_ty != slot || right.slot_ty != slot {
            return Err(mismatch("the primitive ord pick's operand words"));
        }
        self.guard_staged_count(2)?;
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(left.slot),
        });
        self.emit(Op::Mov {
            dst: Slot::overlay(1),
            src: Slot::body(right.slot),
        });
        self.overlay_site(&[slot, slot], &[slot]);
        let callee = self.pool(Constant::FuncRef {
            key: key_func.0,
            func: key_func.1,
        });
        self.emit(Op::Call { callee, nargs: 2 });
        self.result_place(ty, &[slot], span)
    }

    /// Stages both operands' whole spans beside the solved compare body.
    /// The native answers one operand span and the caller owns its counts.
    fn ord_pick_stage(
        &mut self,
        kind: OrdPick,
        a: &Expr,
        b: &Expr,
        instance: InstanceId,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let generic = crate::native::lookup_generic(kind.name())
            .ok_or_else(|| mismatch("the ord pick's native mirror"))?;
        let left = self.value(a)?;
        let left = self.word_addrs(&left.place, left.ty)?;
        let right = self.value(b)?;
        let right = self.word_addrs(&right.place, right.ty)?;
        self.guard_staged_count(left.len() + right.len() + 1)?;
        let callable = self.compare_callable(instance, span, "an ord-routed pick")?;
        self.rc.native_open();
        let mut staged = Vec::with_capacity(left.len() + right.len() + 1);
        for addr in left.iter().chain(&right) {
            let at = u8::try_from(staged.len()).expect("the staged-width guard ran");
            self.emit(Op::Mov {
                dst: Slot::overlay(at),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.staged_native(addr.slot, false);
            }
            staged.push(addr.slot_ty);
        }
        let cmp = u8::try_from(staged.len()).expect("the staged-width guard ran");
        self.emit(Op::Mov {
            dst: Slot::overlay(cmp),
            src: Slot::body(callable),
        });
        staged.push(SlotType::Fn);
        let result = self.shapes.words(ty)?;
        self.overlay_site(&staged, &result);
        let callee = self.pool(Constant::FuncRef {
            key: self.native_key(crate::native::NativeModule::Builtin),
            func: generic.index,
        });
        self.emit(Op::Callg {
            callee,
            nargs: u8::try_from(staged.len()).expect("the staged-width guard ran"),
        });
        self.rc.native_done(&mut self.code);
        self.result_place(ty, &result, span)
    }

    /// The solved ord instance's monomorphic compare body as a callable word.
    fn compare_callable(
        &mut self,
        instance: InstanceId,
        span: Span,
        what: &'static str,
    ) -> Result<u8, LowerError> {
        let impl_site = self
            .analyzed
            .decls
            .instance_impls
            .get(&instance)
            .copied()
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    what,
                    "the serving instance's impl item is missing from the declaration tables",
                )
            })?;
        let Some(compare) = self.plan.impl_body(impl_site, "compare") else {
            let reason = if self.plan.generic_member(impl_site, "compare").is_some() {
                "the solving impl's compare body is a generic impl member: its callable word would \
                 have to carry the body's runtime-evidence demands, and the comparator word needs \
                 a monomorphic body"
            } else {
                "the solving impl's compare body did not plan (it spells no compare member, or a \
                 linked impl's member body is outside this round's seam); report the construct"
            };
            return Err(LowerError::unsupported(span, what, reason));
        };
        if !self.plan.functions()[compare as usize].evidence.is_empty() {
            return Err(LowerError::unsupported(
                span,
                what,
                "the solving impl's compare body carries runtime-evidence demands; the comparator \
                 word needs a monomorphic body",
            ));
        }
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
        Ok(callable)
    }

    /// The shared staging of both sort spellings: the solved ord
    /// instance's `compare` body mints as the callable word beside the
    /// list `Ref`, the receiver's list-instance evidence word trailing
    /// them, and the native's stable merge sort reenters the dispatcher
    /// through that word per comparison (the reduce seam's CALLG
    /// discipline).
    fn sort_stage(
        &mut self,
        xs: &Expr,
        instance: InstanceId,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let generic = crate::native::lookup_generic("sort")
            .ok_or_else(|| mismatch("the sort seam's native mirror"))?;
        let callable = self.compare_callable(instance, span, "a sort call")?;
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
