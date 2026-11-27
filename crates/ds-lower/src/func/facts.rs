//! The check-side fact readers: one method per span-keyed table the
//! checker records (solved values, binder facts, trait calls, generic
//! arguments, handler answers, place stores), plus the written-arm
//! variants that read an assignment's target.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// The registry as this lowering reads it: the analyzed package's
    /// own outside every instantiation, the discovery's copy while one
    /// lowered (the substituted types minted there).
    pub(super) fn universe(&self) -> &'a ds_check::Universe {
        self.shapes.universe()
    }

    /// The checked `Val` recorded at one span, read through the lens:
    /// under an instantiation every type the checker recorded for the
    /// body is the one the plan's discovery substituted it to, so the
    /// walk lowers an instance exactly like a monomorphic body. A type
    /// naming a variable the discovery never resolved refuses here,
    /// re-spanned at the node, instead of lowering a variable's word.
    /// The copy record at one span: inside a `for comptime` copy the
    /// checker's facts for the body live in the per-copy table keyed by
    /// the copy path; outside one, none exists.
    pub(super) fn copy_val(&self, span: Span) -> Option<Val> {
        self.copy_record(span)?.val
    }

    /// The copy record at one span: inside a `for comptime` copy the
    /// checker's facts for the body live in the per-copy tables. A
    /// per-instance expansion's facts sit under the walked fn's
    /// definition and solved arguments; a written loop's under the copy
    /// path alone. A body-wide table cannot hold them; the lowering
    /// reads the active expansion's record first, the once-check's
    /// second.
    pub(super) fn copy_record(&self, span: Span) -> Option<&ds_check::type_of::CopyRecords> {
        if self.comptime_path.is_empty() {
            return None;
        }
        if let Some(scope) = &self.comptime_instance {
            if let Some(rec) = self.analyzed.checked.comptime_instance_records.get(&(
                scope.0,
                Rc::clone(&scope.1),
                self.comptime_path.clone(),
                span,
            )) {
                return Some(rec);
            }
        }
        self.analyzed
            .checked
            .comptime_records
            .get(&(self.comptime_path.clone(), span))
    }

    pub(super) fn copy_binder(&self, span: Span) -> Option<BinderFact> {
        self.copy_record(span)?.binder
    }

    pub(super) fn copy_trait_call(&self, span: Span) -> Option<TraitCall> {
        self.copy_record(span)?.trait_call.clone()
    }

    pub(super) fn copy_is_test(&self, span: Span) -> Option<Ty> {
        self.copy_record(span)?.is_test
    }

    pub(super) fn copy_generic_call(&self, span: Span) -> Option<Vec<Ty>> {
        self.copy_record(span)?.generic_call.clone()
    }

    pub(super) fn copy_member_params(&self, span: Span) -> Option<Vec<Ty>> {
        self.copy_record(span)?.member_params.clone()
    }

    pub(super) fn copy_handler_handled(&self, span: Span) -> Option<Ty> {
        self.copy_record(span)?.handler_handled
    }

    pub(super) fn copy_place_store(&self, span: Span) -> Option<PlaceStore> {
        self.copy_record(span)?.place_store.clone()
    }

    /// The directed label's atom at one span, the copy-active read: a
    /// generic enum's case record maps to the active instance's own
    /// minted atom under an instantiation.
    pub(super) fn directed_tag(&self, span: Span) -> Option<TypeId> {
        if let Some(atom) = self.copy_record(span).and_then(|rec| rec.directed_tag) {
            return self.shapes.substituted_case_atom(atom).or(Some(atom));
        }
        self.analyzed
            .checked
            .directed_tags
            .get(&span)
            .copied()
            .and_then(|atom| self.shapes.substituted_case_atom(atom).or(Some(atom)))
    }

    /// The hole's solved show dispatch at one span, the copy-active read.
    pub(super) fn hole_show_method(&self, span: Span) -> Option<MethodId> {
        if let Some(method) = self.copy_record(span).and_then(|rec| rec.hole_show) {
            return Some(method);
        }
        self.analyzed
            .checked
            .hole_show_dispatches
            .get(&span)
            .copied()
    }

    /// Whether the checked walk marked the expression at this span
    /// diverging: a call to a `never` fn exits at the call, so the arm
    /// or operand never hands its answer to the join, and the widen
    /// over the answer span would stage dead words over a mismatched
    /// shape.
    pub(super) fn diverges_at(&self, span: Span) -> bool {
        if !self.comptime_path.is_empty() {
            return self.copy_val(span).is_some_and(|val| val.diverges);
        }
        self.analyzed
            .checked
            .expr_types
            .get(&span)
            .is_some_and(|val| val.diverges)
    }

    pub(super) fn val(&self, span: Span) -> Result<Val, LowerError> {
        let val = if !self.comptime_path.is_empty() {
            self.copy_val(span).ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    "an expression",
                    "it has no checked type in the active copy",
                )
            })?
        } else {
            self.analyzed
                .checked
                .expr_types
                .get(&span)
                .copied()
                .ok_or_else(|| {
                    LowerError::unsupported(span, "an expression", "it has no checked type")
                })?
        };
        let ty = self
            .shapes
            .subst(val.ty)
            .map_err(|err| err.respanned(span))?;
        Ok(Val { ty, ..val })
    }

    /// The checked `Val` the body's own checking recorded, un-substituted:
    /// the runtime-evidence dispatch's decision read. A generic body is
    /// written over its variables, and its nested bound calls dispatch
    /// through the frame's evidence slots whatever the instantiation
    /// solved, so the decision reads the written type and the lens
    /// stays out of it.
    pub(super) fn written_val(&self, span: Span) -> Result<Val, LowerError> {
        if !self.comptime_path.is_empty() {
            return self.copy_val(span).ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    "an expression",
                    "it has no checked type in the active copy",
                )
            });
        }
        self.analyzed
            .checked
            .expr_types
            .get(&span)
            .copied()
            .ok_or_else(|| LowerError::unsupported(span, "an expression", "it has no checked type"))
    }

    /// The checked binder fact at one span, its type read through the
    /// lens. `Ok(None)` keeps the site's own no-record handling; a belt
    /// refusal (`Err`) re-spans at the binder and never lowers a
    /// variable's word for an instance.
    pub(super) fn binder_fact(&self, span: Span) -> Result<Option<BinderFact>, LowerError> {
        let Some(fact) = self
            .copy_binder(span)
            .or_else(|| self.analyzed.checked.binders.get(&span).copied())
        else {
            return Ok(None);
        };
        let ty = self
            .shapes
            .subst(fact.ty)
            .map_err(|err| err.respanned(span))?;
        Ok(Some(BinderFact { ty, ..fact }))
    }

    /// A def's checked type, read through the lens: a monomorphic
    /// callee's signature names no variable of the body being lowered,
    /// so the lens is an identity there and a belt refusal names a
    /// construct to report — re-spanned by the call site's own error
    /// mapping.
    pub(super) fn def_type(&self, def: DefId) -> Result<Option<Ty>, LowerError> {
        let Some(ty) = self.analyzed.checked.def_types.get(&def).copied() else {
            return Ok(None);
        };
        Ok(Some(self.shapes.subst(ty)?))
    }

    /// The checked trait-call record at one span, its receiver type read
    /// through the lens.
    pub(super) fn trait_call(&self, span: Span) -> Result<Option<TraitCall>, LowerError> {
        let Some(call) = self
            .copy_trait_call(span)
            .or_else(|| self.analyzed.checked.trait_calls.get(&span).cloned())
        else {
            return Ok(None);
        };
        let recv = self
            .shapes
            .subst(call.recv)
            .map_err(|err| err.respanned(span))?;
        Ok(Some(TraitCall { recv, ..call }))
    }

    /// The checked trait-call record at one span, un-substituted: the
    /// runtime-evidence slot path's read, which peels the entering
    /// frame's own written variable.
    pub(super) fn written_trait_call(&self, span: Span) -> Option<TraitCall> {
        self.copy_trait_call(span)
            .or_else(|| self.analyzed.checked.trait_calls.get(&span).cloned())
    }

    /// The checked `is`-test type at one span, read through the lens.
    pub(super) fn is_test(&self, span: Span) -> Result<Option<Ty>, LowerError> {
        let Some(ty) = self
            .copy_is_test(span)
            .or_else(|| self.analyzed.checked.is_tests.get(&span).copied())
        else {
            return Ok(None);
        };
        Ok(Some(
            self.shapes.subst(ty).map_err(|err| err.respanned(span))?,
        ))
    }

    /// The solved generic-argument types one call site's checker record
    /// carries, read through the lens: under an instantiation the
    /// record names this body's variables, and the substitution is what
    /// the plan's discovery keyed the instance by.
    pub(super) fn generic_call(&self, span: Span) -> Result<Option<Vec<Ty>>, LowerError> {
        let Some(solved) = self
            .copy_generic_call(span)
            .or_else(|| self.analyzed.checked.generic_calls.get(&span).cloned())
        else {
            return Ok(None);
        };
        let solved = solved
            .iter()
            .map(|ty| self.shapes.subst(*ty).map_err(|err| err.respanned(span)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(solved))
    }

    /// The checked bound answers one call entering a generic body
    /// carries, un-substituted: the runtime-evidence staging's read. A
    /// `None` serving answer names the entering body's own written
    /// variable — the frame's evidence slot the entering site staged —
    /// while a solved answer's type reaches the chain start through the
    /// lens at the staging itself.
    pub(super) fn written_generic_call_bounds(&self, span: Span) -> Option<GenericCallBounds> {
        if let Some(bounds) = self
            .copy_record(span)
            .and_then(|rec| rec.generic_bounds.clone())
        {
            return Some(bounds);
        }
        self.analyzed
            .checked
            .generic_call_bounds
            .get(&span)
            .cloned()
    }

    /// The member call's solved parameter types, read through the lens.
    pub(super) fn member_call_params(&self, span: Span) -> Result<Option<Vec<Ty>>, LowerError> {
        let Some(params) = self
            .copy_member_params(span)
            .or_else(|| self.analyzed.checked.member_call_params.get(&span).cloned())
        else {
            return Ok(None);
        };
        let params = params
            .iter()
            .map(|ty| self.shapes.subst(*ty).map_err(|err| err.respanned(span)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(params))
    }

    /// The `.err`/`.ok` handler site's handled member set, read through
    /// the lens: the checker's difference-rule record is what the tag
    /// dispatch emits, the site's result type being unable to re-derive
    /// it once the handler's answer re-adds a handled member.
    pub(super) fn handler_handled(&self, span: Span) -> Result<Option<Ty>, LowerError> {
        let Some(set) = self
            .copy_handler_handled(span)
            .or_else(|| self.analyzed.checked.handler_handled.get(&span).copied())
        else {
            return Ok(None);
        };
        Ok(Some(
            self.shapes.subst(set).map_err(|err| err.respanned(span))?,
        ))
    }

    /// The checked assignment place chain, read through the lens.
    pub(super) fn place_store(&self, span: Span) -> Result<Option<PlaceStore>, LowerError> {
        let Some(store) = self
            .copy_place_store(span)
            .or_else(|| self.analyzed.checked.place_stores.get(&span).cloned())
        else {
            return Ok(None);
        };
        let root_ty = self
            .shapes
            .subst(store.root_ty)
            .map_err(|err| err.respanned(span))?;
        let steps = store
            .steps
            .into_iter()
            .map(|step| {
                Ok(match step {
                    PlaceStep::Field { name, ty } => PlaceStep::Field {
                        name,
                        ty: self.shapes.subst(ty).map_err(|err| err.respanned(span))?,
                    },
                    PlaceStep::Index { native, elem } => PlaceStep::Index {
                        native,
                        elem: self.shapes.subst(elem).map_err(|err| err.respanned(span))?,
                    },
                })
            })
            .collect::<Result<Vec<_>, LowerError>>()?;
        Ok(Some(PlaceStore {
            binder: store.binder,
            root_ty,
            steps,
        }))
    }

    pub(super) fn members(&self, ty: Ty) -> &'a [TypeId] {
        self.universe().members(ty)
    }
}

/// Set difference over member lists.
/// The solved signature of a function-typed value: the fn atom's TypeId
/// comes from the handle's member set (one member, by construction).
pub(super) fn fn_sig_of(universe: &ds_check::Universe, fn_ty: Ty) -> Option<(&[Ty], Ty)> {
    universe
        .members(fn_ty)
        .first()
        .and_then(|atom| universe.fn_sig(*atom))
}

pub(super) fn set_diff(a: &[TypeId], b: &[TypeId]) -> Vec<TypeId> {
    a.iter().copied().filter(|m| !b.contains(m)).collect()
}
