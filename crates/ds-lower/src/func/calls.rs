//! The call-staging vocabulary: the argument-staging guards, list/map
//! construction, embedded comptime answers, the one staging walk over every
//! callee class (`call_impl`), the trait-dispatch evidence chains, the
//! fallible native results, and the `ok`/`err` result arms.

use super::*;

mod ctor;
mod evidence;
mod result;

/// A bodiless ambient collection member's native routing: the extern
/// def the call lowers through, and whether the member re-answers the
/// receiver after the native's in-place mutation (`push`/`set`).
/// One call site's evidence-staging descriptors: the entered
/// instance, the inherent impl's site, the lensed receiver the chain
/// start substitutes, and the written receiver the slot path keys by.
#[derive(Clone, Copy)]
pub(in crate::func) struct EvidenceSite {
    entered: Option<InstanceId>,
    inherent_site: Option<Span>,
    recv: Option<Ty>,
    written_recv: Option<Ty>,
}

#[derive(Clone, Copy)]
pub(in crate::func) struct NativeMember {
    def: DefId,
    window: bool,
    /// The native takes the receiver at its second parameter: the
    /// member route stages the first written argument ahead of the
    /// receiver (cons — the element stages first, the list `Ref`
    /// trailing it).
    element_first: bool,
}

/// How a classed fallible call constructs its success value at the ok
/// arm: the shape the union's own success member(s) dictate.
pub(in crate::func) enum SuccessShape {
    /// One member carrying its payload fields: TagNew the member and
    /// copy each payload word off the span's flat payload words (one
    /// field's single word for the statically-payloaded natives, the
    /// member's whole span for the result-type-keyed ones).
    Payload {
        /// The success member's atom.
        atom: TypeId,
        /// The member's payload fields, in declaration order.
        fields: Vec<MemberField>,
    },
    /// One bare member (a unit value over the native's inert unit word,
    /// or a bare tag): TagNew it, the span's word 1 inert.
    Bare {
        /// The success member's atom.
        atom: TypeId,
    },
    /// A tag-only union answer: the native's answer word is already the
    /// union's own value (its tag names the member), adopted whole off
    /// the span's word 1 with no re-span.
    Adopt,
}

impl FnLower<'_, '_, '_> {
    /// The argument order one native-routed member call stages: the
    /// receiver first, the written arguments behind it — an
    /// `element_first` member's native takes the receiver at its
    /// second parameter (cons: the element ahead of the list `Ref`,
    /// the free spelling's own order, its evidence word naming the
    /// element's instance).
    fn member_native_args<'x>(
        recv: &'x Expr,
        args: &'x [CallArg],
        member: NativeMember,
    ) -> Vec<&'x Expr> {
        if !member.element_first {
            return std::iter::once(recv)
                .chain(args.iter().map(|arg| &arg.value))
                .collect();
        }
        let mut staged: Vec<&Expr> = Vec::with_capacity(args.len() + 1);
        if let Some(first) = args.first() {
            staged.push(&first.value);
        }
        staged.push(recv);
        staged.extend(args.iter().skip(1).map(|arg| &arg.value));
        staged
    }

    pub(super) fn call_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        self.call_impl(e, ty, false)
    }

    // One staging walk covers every callee class; extracting the tail or
    // native arms would re-thread the staged span and evidence state.
    #[allow(clippy::too_many_lines)]
    pub(super) fn call_impl(&mut self, e: &Expr, ty: Ty, tail: bool) -> Result<Place, LowerError> {
        let Expr::Call {
            callee,
            args,
            trailing,
            selection: _,
            span,
        } = e
        else {
            return Err(mismatch("a call"));
        };
        // the call's own span is the trait-dispatch table's key (the
        // checker records the solved instance there); the callee-shape
        // destructure below rebinds `span` to the callee path's own
        let call_span = *span;
        // A minted case constructor takes named arguments; named
        // arguments on functions are outside the lowered surface. A
        // payloaded tag constructs through the same call form.
        // the label span answers for the bare and the qualified
        // `type::label` spellings alike (the resolver records the def at
        // the tail segment)
        let case_ctor = match callee.as_ref() {
            Expr::Path { path, .. } => {
                let label = path.segments.last().map(|segment| segment.span);
                match label.and_then(|span| self.res.get(span)) {
                    Some(Res::Def(def)) => {
                        if let Some(case) = self.analyzed.decls.cases.get(def)
                            && !matches!(case.payload, EnumPayload::None)
                        {
                            Some(*def)
                        } else if self.analyzed.package.def(*def).kind == ds_check::DefKind::Tag
                            && self.analyzed.decls.structs.contains_key(def)
                        {
                            Some(*def)
                        } else {
                            None
                        }
                    },
                    _ => None,
                }
            },
            _ => None,
        };
        for arg in args {
            if arg.name.is_some() && case_ctor.is_none() {
                return Err(LowerError::unsupported(
                    arg.span,
                    "a named argument",
                    "named arguments are not in the M3 contract",
                ));
            }
        }
        // A value-space callee (`fs[0](x)`, a call result): its checked
        // type is the function, so the computed-callee path reads it
        // below; the type-application constructors keep their own route.
        let callee_fn_typed = self
            .val(callee.span())
            .is_ok_and(|val| fn_sig_of(self.universe(), val.ty).is_some());
        // A generic-application constructor `list[t]()`: the
        // bracketed callee head resolved in the type namespace.
        if let Expr::Index { recv, .. } = &**callee
            && !callee_fn_typed
        {
            return self.ctor_call(recv, args, *span);
        }
        // The map constructor `map[k, v]()`: the two-argument
        // application of the map struct.
        if let Expr::MapApply { .. } = &**callee
            && !callee_fn_typed
        {
            return self.map_ctor_call(args);
        }
        // A bodiless ambient collection member (`xs.get(i)`, `xs.push(x)`,
        // `xs.concat(ys)`, `m.get(k)`, `m.set(k, v)`) routes to its native
        // seam through the
        // free-callee path below: the receiver feeds the first argument,
        // exactly like the retired free spelling's own call. `push`/`set`
        // additionally re-answer the receiver after the native's unit word
        // (the member's answer is the collection itself).
        let native_member = self.native_member_def(call_span);
        // the callee and its argument expressions in call order: a plain
        // name call feeds the written arguments; a UFCS dot-call `x.f(a)`
        // resolves to the free function and stages the receiver as its
        // first argument — `f(x, a)`, zero runtime cost. A
        // trait-method site carries no def: the solved instance names
        // the impl body (the dispatch side channel below), and `None`
        // marks the trait path.
        let (def, callee_span, mut arg_exprs, trait_dispatch): (
            Option<DefId>,
            Span,
            Vec<&Expr>,
            Option<TraitDispatch>,
        ) = if let Expr::Field { recv, name, .. } = &**callee {
            // The field tier: the checker's record at the call span owns
            // the routing (the record, not the resolution tier) — the
            // call is the indirect call of the fn-typed field's word, the
            // written arguments alone. The receiver is no argument (a
            // field holds a plain callable, unlike a method member), and
            // the field read's word holds the heap closure's Ref, so
            // the computed-callee path binds it through the temporary
            // before the arguments stage.
            if self.analyzed.checked.field_calls.contains(&call_span) {
                return self.computed_callee_call(callee, ty, args, trailing.as_deref(), tail);
            }
            match self.res.get(name.span) {
                Some(Res::Handler) => {
                    // the `.err`/`.ok` handlers: tag dispatch plus an
                    // inlined handler body
                    let [handler] = args.as_slice() else {
                        return Err(mismatch("a handler argument"));
                    };
                    match name.name.as_str() {
                        "err" => {
                            return self.err_value(recv, &handler.value, ty, call_span);
                        },
                        "ok" => {
                            return self.ok_value(recv, &handler.value, ty, call_span);
                        },
                        _ => {
                            return Err(LowerError::unsupported(
                                name.span,
                                "a handler",
                                "only `.err` and `.ok` are lowerable this round",
                            ))
                        },
                    }
                },
                Some(Res::Dot(DotRes::Free { func, fed_param })) => {
                    if *fed_param != 0 {
                        return Err(LowerError::unsupported(
                            name.span,
                            "a dot-call past a capability parameter",
                            "only the first data parameter feeds the dot position [F3]",
                        ));
                    }
                    // The receiver fallback: a free fn
                    // that cannot take the receiver typed through the
                    // method tier, so the checker's dispatch record at
                    // the call span owns the lowering — the native
                    // member seam first, then the resolved method body.
                    let member_routed = native_member.is_some()
                        || self.has_inherent_record(call_span)
                        || self.has_trait_record(call_span);
                    if member_routed {
                        if let Some(member) = native_member {
                            (
                                Some(member.def),
                                name.span,
                                Self::member_native_args(recv, args, member),
                                None,
                            )
                        } else if self.sort_member_site(call_span) {
                            return self.sort_member_call(recv, ty, call_span, name.span);
                        } else {
                            let candidates: Vec<_> = self
                                .analyzed
                                .package
                                .method_candidates(&name.name)
                                .collect();
                            let dispatch = self.resolve_method_dispatch(
                                call_span,
                                name.span,
                                Some(recv),
                                &candidates,
                            )?;
                            (
                                None,
                                name.span,
                                std::iter::once(&**recv)
                                    .chain(args.iter().map(|arg| &arg.value))
                                    .collect(),
                                Some(dispatch),
                            )
                        }
                    } else {
                        (
                            Some(*func),
                            name.span,
                            std::iter::once(&**recv)
                                .chain(args.iter().map(|arg| &arg.value))
                                .collect(),
                            None,
                        )
                    }
                },
                Some(Res::Dot(DotRes::Methods { candidates })) => {
                    if let Some(member) = native_member {
                        (
                            Some(member.def),
                            name.span,
                            Self::member_native_args(recv, args, member),
                            None,
                        )
                    } else if self.sort_member_site(call_span) {
                        return self.sort_member_call(recv, ty, call_span, name.span);
                    } else {
                        let dispatch = self.resolve_method_dispatch(
                            call_span,
                            name.span,
                            Some(recv),
                            candidates,
                        )?;
                        (
                            None,
                            name.span,
                            std::iter::once(&**recv)
                                .chain(args.iter().map(|arg| &arg.value))
                                .collect(),
                            Some(dispatch),
                        )
                    }
                },
                // The type-keyed member call: no receiver word stages, so
                // the written arguments alone feed the body; the dispatch
                // resolves through the checker's inherent record like any
                // solved inherent site (a plain local callee — no instance
                // word, no evidence words, no dispatch uop).
                Some(Res::Dot(DotRes::Static { candidates })) => {
                    let dispatch =
                        self.resolve_method_dispatch(call_span, name.span, None, candidates)?;
                    (
                        None,
                        name.span,
                        args.iter().map(|arg| &arg.value).collect(),
                        Some(dispatch),
                    )
                },
                // A module alias's qualified call: the receiver is
                // the alias itself — no word stages — and the dot's
                // recorded def is the linked package's function, a plain
                // def call through the import's link facts.
                Some(Res::Dot(DotRes::Module { func })) => (
                    Some(*func),
                    name.span,
                    args.iter().map(|arg| &arg.value).collect(),
                    None,
                ),
                // An implicit dispatch site (an interpolation hole's
                // `show`): no surface name sits behind the dot, so
                // the resolutions carry nothing here — the checker's
                // span-keyed dispatch records ARE the site's resolution
                // (an assumption-answered hole records only the hole
                // table, its dispatch resolving through the frame's
                // runtime evidence below), and the method-name
                // candidates come from the registry by the synthetic
                // name.
                _ if self.has_trait_record(call_span)
                    || self.has_inherent_record(call_span)
                    || self.hole_show_method(call_span).is_some() =>
                {
                    let candidates: Vec<_> = self
                        .analyzed
                        .package
                        .method_candidates(&name.name)
                        .collect();
                    let dispatch = self.resolve_method_dispatch(
                        call_span,
                        name.span,
                        Some(recv),
                        &candidates,
                    )?;
                    (
                        None,
                        name.span,
                        std::iter::once(&**recv)
                            .chain(args.iter().map(|arg| &arg.value))
                            .collect(),
                        Some(dispatch),
                    )
                },
                _ => {
                    return Err(LowerError::unsupported(
                        name.span,
                        "a method call",
                        "the dot name resolves to neither a free function nor a handler",
                    ))
                },
            }
        } else {
            let Expr::Path { path, span, .. } = &**callee else {
                // A computed callee (`fs[0](x)`, a call result, the
                // field tier): the callee word binds through a temporary
                // before the arguments stage.
                return self.computed_callee_call(callee, ty, args, trailing.as_deref(), tail);
            };
            // `Trait::f(x)` qualification: the checker records the
            // solved instance at the call's own span like the dot form,
            // and the receiver is the first written argument
            let qualified = match path.segments.as_slice() {
                [.., last] if matches!(self.res.get(last.span), Some(Res::TraitMethod(_))) => {
                    Some(last.span)
                },
                _ => None,
            };
            if let Some(what) = qualified {
                let recv = args.first().map(|arg| &arg.value);
                let dispatch = self.resolve_method_dispatch(call_span, what, recv, &[])?;
                (
                    None,
                    what,
                    args.iter().map(|arg| &arg.value).collect(),
                    Some(dispatch),
                )
            } else if path.segments.len() == 2
                && let [.., last] = path.segments.as_slice()
                && let Some(Res::Def(def)) = self.res.get(last.span)
                && matches!(
                    self.analyzed.package.def(*def).kind,
                    ds_check::DefKind::Tag | ds_check::DefKind::Case
                )
            {
                // The qualified `type::label` call form: a payloaded
                // label constructs through the same case-value door as
                // the bare spelling, over the call's own checked type
                // (a unit label's call never leaves the checker).
                return self.case_value_for(args, ty, *def, call_span);
            } else if let Some(Res::Def(def)) = self.res.get(*span) {
                (
                    Some(*def),
                    *span,
                    args.iter().map(|arg| &arg.value).collect(),
                    None,
                )
            } else if let Some(Res::Local { bind }) = self.res.get(*span) {
                // a clean check makes this a fn-typed binding: the
                // call dispatches through its Fn word
                let mut arg_exprs: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
                if let Some(block) = trailing {
                    arg_exprs.push(&**block);
                }
                return self.calls_value(ty, *bind, *span, &arg_exprs, tail);
            } else {
                return Err(LowerError::unsupported(
                    *span,
                    "a callee name",
                    "it does not resolve to a package function",
                ));
            }
        };
        // The block-argument sugar: the checker fed the parser's
        // zero-parameter trailing block at the next open positional slot,
        // and with named arguments refused above that slot is exactly the
        // written arguments' end, so the block stages below like a
        // written trailing lambda argument — pure sugar, no calling-
        // convention surface of its own.
        if let Some(block) = trailing {
            arg_exprs.push(&**block);
        }
        if case_ctor.is_some()
            && case_ctor == def
            && let Some(def) = def
        {
            return self.case_value_for(args, ty, def, call_span);
        }
        // A comptime call's answer embeds at the call site: the
        // evaluator ran the callee on the fenced VM and recorded the
        // result on this call's span, so lowering emits it exactly like
        // a written literal and the call never reaches the runtime
        // module. A comptime callee with no recorded answer is the
        // evaluator's contract broken, not a shape the round owes a
        // lowering for — the site refuses naming the seam.
        if let ComptimeRun::Runtime(values) = &self.comptime {
            if let Some(constant) = values.get(call_span) {
                return self.embedded_value(constant, ty, call_span);
            }
            let is_comptime = def.is_some_and(|def| self.comptime_def(def));
            if is_comptime {
                return Err(LowerError::unsupported(
                    call_span,
                    "a comptime call",
                    "the evaluator recorded no answer for this site; run the comptime evaluator \
                     before lowering",
                ));
            }
        }
        // The extraction family inlines at the call site over the
        // checked receiver's own union shape — no callee, no control
        // transfer, word kinds from the call site like every covered
        // generic-safe native.
        if let Some(def) = def {
            if let Some(kind) = self.extraction_def(def) {
                return self.extraction_value(kind, &arg_exprs, ty, callee_span);
            }
            if let Some(place) = self.ambient_str_place(def, &arg_exprs, callee_span)? {
                return Ok(place);
            }
            if self.ambient_extern(def, AmbientExtern::Cancel) {
                return self.cancel_call(&arg_exprs, callee_span);
            }
            if self.ambient_extern(def, AmbientExtern::Die)
                || self.ambient_extern(def, AmbientExtern::Panic)
            {
                return self.trap_call(&arg_exprs, ty, callee_span);
            }
            if self.ambient_extern(def, AmbientExtern::Exit) {
                return self.exit_call(&arg_exprs, ty, callee_span);
            }
            if self.ambient_extern(def, AmbientExtern::Sort) {
                return self.sort_call(&arg_exprs, ty, call_span, callee_span);
            }
            if self.ambient_extern(def, AmbientExtern::Max) {
                return self.ord_pick_call(
                    OrdPick::Max,
                    &arg_exprs,
                    ty,
                    call_span,
                    callee_span,
                    matches!(callee.as_ref(), Expr::Field { .. }),
                );
            }
            if self.ambient_extern(def, AmbientExtern::Min) {
                return self.ord_pick_call(
                    OrdPick::Min,
                    &arg_exprs,
                    ty,
                    call_span,
                    callee_span,
                    matches!(callee.as_ref(), Expr::Field { .. }),
                );
            }
        }
        // a trait-method dispatch names its impl body directly: the call
        // stages and transfers like a local callee, with the solved
        // instance riding as the evidence word after the arguments. An
        // inherent dispatch is a plain local callee — the receiver's own
        // type picked the body, so no instance word exists; a generic
        // body's evidence words stage below like ordinary parameters
        let target = match &trait_dispatch {
            Some(TraitDispatch::Static { index, .. } | TraitDispatch::Inherent { index, .. }) => {
                Callee::Local(*index)
            },
            Some(
                TraitDispatch::Foreign { position, func, .. }
                | TraitDispatch::InherentForeign { position, func, .. },
            ) => Callee::Foreign {
                def: None,
                position: *position,
                func: *func,
            },
            Some(TraitDispatch::Runtime { slot, .. }) => Callee::Evidence(*slot),
            None => self.resolve_callee(
                def.expect("a named callee resolves to a def"),
                callee_span,
                call_span,
            )?,
        };
        // a member call stages its omitted literal defaults at the call
        // site like the free-fn spelling: a trait dispatch stages
        // the trait's declared defaults (the declaration is the single
        // authority — call sites check those literals), while an
        // inherent dispatch stages the impl body's own
        let member_defaults: Vec<Expr>;
        let arg_exprs = match (def, &trait_dispatch) {
            (Some(def), _) => self.defaulted_args(def, target, arg_exprs),
            (_, Some(TraitDispatch::Static { call, .. } | TraitDispatch::Foreign { call, .. })) => {
                member_defaults = self.trait_default_exprs(call.method, arg_exprs.len());
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            (_, Some(TraitDispatch::Runtime { method, .. })) => {
                member_defaults = self.trait_default_exprs(*method, arg_exprs.len());
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            (None, Some(TraitDispatch::Inherent { index, .. })) => {
                member_defaults =
                    self.member_default_exprs(*index, arg_exprs.len(), callee_span)?;
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            _ => arg_exprs,
        };
        // A direct `list_push` stages its (list, value) pair with the
        // value's whole word span — the same element guard the literal
        // path applies (a struct element stages its flattened words at
        // the stride the first push fixes). `cons` mirrors it with the
        // element's whole span in the receiver position (the stride-aware
        // prepend's head). `map_set` stages its value the same way (the
        // map value channel's whole span at the value stride the first
        // store fixes) while its key stays one word — the probe
        // vocabulary hashes whole words. `chan_send` stages its payload
        // the same way (the channel's element stride at the hand-off:
        // the receiver's `chan[t]` names `t`, the payload's whole span
        // crossing at the stride the first deposit fixes, the recv's
        // answer the tag-less total of the same words). `reduce`'s init
        // needs no element
        // guard: the checker proves it the fold's own type, so any
        // element that could inhabit the list already passed this guard,
        // and the fold's interior span carries the init's whole width.
        let element_slot = match target {
            Callee::Builtin(native) if native.name == "list_push" => {
                Some((1usize, "a list_push element"))
            },
            Callee::Builtin(native) if native.name == "list_contains" => {
                Some((1usize, "a list_contains element"))
            },
            Callee::Builtin(native) if native.name == "list_set" => {
                Some((2usize, "a list_set element"))
            },
            Callee::Builtin(native) if native.name == "list_insert" => {
                Some((2usize, "a list_insert element"))
            },
            Callee::Builtin(native) if native.name == "map_set" => {
                Some((2usize, "a map_set value"))
            },
            Callee::Builtin(native) if native.name == "chan_send" => {
                Some((1usize, "a chan_send payload"))
            },
            Callee::Builtin(native) if native.name == "chan_send_deadline" => {
                Some((2usize, "a chan_send_deadline payload"))
            },
            Callee::Builtin(native) if native.name == "arena_insert" => {
                Some((1usize, "an arena_insert element"))
            },
            Callee::Builtin(native) if native.name == "box_set" => {
                Some((1usize, "a box_set value"))
            },
            Callee::Generic(generic) if generic.name == "cons" => Some((0usize, "a cons element")),
            _ => None,
        };
        // whether the call is a `map_set` whose key argument (position 1)
        // must stage exactly one word
        let map_set_key = matches!(target, Callee::Builtin(native) if native.name == "map_set");
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call)
        let mut staged: Vec<SlotType> = Vec::new();
        // which staged words carry a count at runtime: Ref words, plus
        // the one Raw word of a type-variable value (the VM's RC ops
        // check the kind, so a scalar instantiation is a no-op)
        let mut counted: Vec<bool> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // the written argument each staged word belongs to: a consuming
        // (`move`) parameter's words stage as a hand-off
        let mut word_arg: Vec<usize> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter (the closure-capable callable surface): the callee
        // reads the word and never owns its count, so the staging is a
        // borrow window, never a dup or a forward
        let mut defaulted: Vec<usize> = Vec::new();
        let default_dispatch = matches!(
            &trait_dispatch,
            Some(TraitDispatch::Foreign {
                default_body: true,
                ..
            })
        );
        let expected_words = self.expected_param_words(target, default_dispatch);
        // an interior-span generic stages one dynamic span between its
        // leading and trailing named words, so a staged closure word's
        // position is dynamic and the callable-defaulted slot is the
        // trailing named word alone
        let interior_span = matches!(target, Callee::Generic(g) if g.staging == Staging::Interior);
        // a linked member dispatch's solved parameter types, from the
        // checker's per-site record: the registered member signature
        // rides the decls table keyed by its method id, never a def
        // whose fn type the free-fn seam reads, so the staging below
        // reads the call's own solved record instead
        let member_params: Option<Vec<Ty>> = match target {
            Callee::Foreign { def: None, .. } => self.member_call_params(call_span)?,
            _ => None,
        };
        let member_params = member_params.as_deref();
        // a local callee's per-parameter checked types, when its
        // signature is static: a union-typed parameter re-spans a bare
        // member or a strict sub-union argument into its whole region
        // here (the widening family)
        let param_types = match target {
            Callee::Local(index) => self.callee_param_types(index),
            Callee::Foreign { def: Some(def), .. } => self
                .def_type(def)?
                .and_then(|fn_ty| fn_sig_of(self.universe(), fn_ty))
                .map(|(params, _)| params.to_vec()),
            Callee::Foreign { def: None, .. } => member_params.map(<[Ty]>::to_vec),
            // a builtin call reads the checker's per-site solved
            // parameter types: a union parameter (the string-separator
            // pair) re-spans every argument into its whole region, so
            // the staged words match the native's positional span
            // whatever member the caller wrote
            // a native-routed member's site carries no free call's own
            // record: the member's solved parameter types stage the same
            // union re-span (the separator pair's dispatch constant
            // reads them)
            Callee::Builtin(_) => self
                .analyzed
                .checked
                .call_params
                .get(&call_span)
                .cloned()
                .or(self.member_call_params(call_span)?),
            _ => None,
        };
        // whether each written argument position feeds a consuming
        // (`move`) parameter: the staging then forwards the source's
        // count and any deinit claim — the checker's forced move makes
        // the death test true at exactly this use
        let consuming = self.consuming_args(target, trait_dispatch.as_ref(), arg_exprs.len());
        // the receiver's checked type mints a generic call's evidence
        // words (the builtin instance it addresses)
        let mut receiver_ty: Option<Ty> = None;
        // the receiver's written type — the runtime-evidence slot path's
        // key, which peels the entering frame's own variable however the
        // lens substituted the value's type
        let mut written_receiver_ty: Option<Ty> = None;
        // a `push`/`set` member call's receiver place: the native window
        // mutates it in place, and the member re-answers a duplicate of
        // its word after the call
        let window_answer = native_member.is_some_and(|member| member.window);
        let mut window_receiver: Option<TypedPlace> = None;
        // the call's borrow window opens before operand evaluation: a
        // later operand's own use can fire an earlier word's last-use
        // dec there, and the window pins every word this CALL reads
        // (native borrows never dup), deferring the dec to the flush
        // after the CALL
        self.rc.native_open();
        let mut consts: Vec<(usize, ScalarConst)> = Vec::new();
        for (index, arg) in arg_exprs.iter().enumerate() {
            let param_ty = param_types
                .as_ref()
                .and_then(|types| types.get(index).copied());
            // an element-slot argument stages its channel's whole
            // element span, so the direct staging paths read the
            // channel's element type as the position's parameter type:
            // a union element suppresses them exactly a union
            // parameter's own staging does (a bare literal would
            // otherwise stage one word where the element stride needs
            // the member's whole region)
            let element_ty = match element_slot {
                Some((slot, _)) if slot == index => self.element_slot_ty(&target, receiver_ty),
                _ => None,
            };
            // A literal scalar argument stages its constant directly on
            // its overlay word. A tail transfer defers the staging past
            // the exit claims, so it keeps the evaluated temp. A word
            // whose callee-side formal declares Raw (a default body's
            // `Self` receiver) records the wildcard kind, mirroring the
            // moved-word path below — the ABI slide admits the pairing
            // whatever the literal's own static kind.
            if !tail
                && let Some((slot, constant)) = self.direct_literal(arg, element_ty.or(param_ty))?
            {
                if index == 0 {
                    receiver_ty = Some(self.val(arg.span())?.ty);
                    written_receiver_ty = Some(self.written_val(arg.span())?.ty);
                }
                let expected_raw = expected_words
                    .as_deref()
                    .is_some_and(|words| words.get(staged.len()) == Some(&SlotType::Raw));
                let at = staged.len();
                staged.push(if expected_raw { SlotType::Raw } else { slot });
                counted.push(false);
                word_arg.push(index);
                consts.push((at, constant));
                continue;
            }
            // A single-use computed scalar argument writes its overlay
            // word directly: the offer goes to the argument's outermost
            // data op, taken only when every later argument evaluates
            // without touching the overlay.
            let offered = if !tail
                && let Some(slot) =
                    self.direct_compute(arg, element_ty.or(param_ty), &arg_exprs[index + 1..])?
            {
                self.scalar_dst = Some(ScalarDst::Arg {
                    index: u8::try_from(staged.len()).expect("overlay word fits u8"),
                    slot,
                });
                true
            } else {
                false
            };
            let arg_val = self.value(arg)?;
            let direct = offered && self.scalar_dst.take().is_none();
            if index == 0 {
                receiver_ty = Some(arg_val.ty);
                written_receiver_ty = Some(self.written_val(arg.span())?.ty);
                if window_answer {
                    window_receiver = Some(arg_val.clone());
                }
            }
            let deinit = self.deinit_claim(arg_val.ty, arg.span())?;
            if deinit.is_some() {
                // A channel payload names its own refusal seam: the buffer's
                // edge retention owns counts, never claims, so the payload's
                // hook has no door at either wait-ending edge — a buffered
                // element the channel never delivers dies at the send with
                // the sender's claim spent, and a parked sender cut with
                // its span queued fires no hook either way
                if let Some((slot, _)) = element_slot
                    && slot == index
                    && matches!(target, Callee::Builtin(native) if native.name == "chan_send" || native.name == "chan_send_deadline")
                {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying channel payload",
                        "the payload's hook has no claim inside the channel's edge retention; \
                         per-span deinit claims own the seam",
                    ));
                }
                // An arena element names its own refusal seam: the slot's
                // edge retention owns counts exactly a channel buffer's,
                // so the element's hook has no door at the remove or the
                // arena's death
                if let Some((slot, _)) = element_slot
                    && slot == index
                    && matches!(target, Callee::Builtin(native) if native.name == "arena_insert")
                {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying arena element",
                        "the element's hook has no claim inside the arena's edge retention; \
                         per-span deinit claims own the seam",
                    ));
                }
                if matches!(
                    target,
                    Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. }
                ) {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying argument",
                        "a native call borrows staged words with no drop position of their own; \
                         the value's deinit runs only at its binding's death",
                    ));
                }
                if tail
                    && matches!(target, Callee::Local(_) | Callee::Foreign { .. })
                    && !consuming[index]
                {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying argument in tail position",
                        "the tail call reuses this frame, so the callee's borrow would outlive \
                         the frame that owns the value's deinit; keep the call out of tail \
                         position",
                    ));
                }
            }
            let mut arg_val = arg_val;
            if let Some((slot, what)) = element_slot
                && slot == index
            {
                // A bare member value (a case construction, a
                // narrowed binding) re-spans into the receiver's
                // element or value span first — the generic callee
                // has no static parameter type to drive the widening
                // family, and the staged words must be the whole
                // span the channel carries
                let channel_ty = self.element_slot_ty(&target, receiver_ty);
                if let Some(channel_ty) =
                    channel_ty.filter(|ty| *ty != arg_val.ty && !self.diverges_at(arg.span()))
                {
                    let temp = self.temp_span(channel_ty)?;
                    self.widen_into(&temp, channel_ty, &arg_val, arg.span())?;
                    arg_val = TypedPlace {
                        place: temp,
                        ty: channel_ty,
                    };
                }
                arg_val = self.stage_heap_element(arg_val, arg.span(), what)?;
                // an arena element stages exactly one word: the slot array
                // stores one element word per handle, so a wider element (a
                // multi-word record or tuple, a union's region) refuses at the
                // staging with the per-instantiation seam's own refusal
                if matches!(target, Callee::Builtin(native) if native.name == "arena_insert") {
                    let addrs = self.word_addrs(&arg_val.place, arg_val.ty)?;
                    if addrs.len() != 1 {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "an arena_insert element",
                            "arena elements lower as one machine word per slot this round; a \
                             wider element waits on the per-instantiation element-span seam",
                        ));
                    }
                }
            }
            // a map_set's key stages exactly one word: the probe
            // resolution and the row index hash whole words, so a wider
            // key type has no channel yet
            if map_set_key && index == 1 {
                let addrs = self.word_addrs(&arg_val.place, arg_val.ty)?;
                if addrs.len() != 1 {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a map_set key",
                        "map keys lower as one machine word (the probe vocabulary hashes whole \
                         words); wider keys arrive when the native channel carries key spans",
                    ));
                }
            }
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter; its one Ref word marks the borrow-window index.
            // An interior-span generic's callable word rides at the end
            // of the staged span, so the static expected list cannot
            // name its position there. An element channel's position
            // keeps its own discipline: the staged closure word is the
            // payload the collection's count traffic covers
            let element_position = matches!(element_slot, Some((slot, _)) if slot == index);
            if self.is_closure_word(&arg_val.place, arg_val.ty)? && !element_position {
                if interior_span {
                    Self::guard_interior_closure_slot(
                        expected_words.as_deref(),
                        index + 1 == arg_exprs.len(),
                        arg.span(),
                    )?;
                } else {
                    Self::guard_callable_staged(
                        expected_words.as_deref(),
                        staged.len(),
                        arg.span(),
                    )?;
                }
                defaulted.push(staged.len());
            }
            // a union-typed parameter takes its member's whole region: a
            // bare member (a literal or an un-annotated binding), a
            // strict sub-union argument, or an equal-typed argument
            // whose place keeps a wider union's physical layout (an
            // is-narrowed binder over the answer union) re-spans into a
            // staging temp of the parameter's own region here, mirroring
            // the annotated binding's widening write, so the staged
            // words match the callee's parameter span
            let arg_val = match param_types.as_ref().and_then(|types| types.get(index)) {
                // a variable parameter never re-spans — the ONE Raw-word
                // body takes the argument's single word — and the union
                // probe must not consult the shape layer on a written
                // variable under the caller's lens (its belt would fire)
                Some(&param_ty)
                    if !self.is_var_ty(param_ty)
                        && self.is_union_ty(param_ty)?
                        && (param_ty != arg_val.ty
                            || !self.place_layout_matches(&arg_val.place, param_ty)?) =>
                {
                    // a union member cannot carry a deinit yet (the
                    // callee's own parameter shapes refuse first), so
                    // this guard is the staging temp's backstop for the
                    // deinit arc's landing
                    if deinit.is_some() {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument into a union parameter",
                            "the re-span's staging temp is anonymous, so it carries no claim \
                             contract of its own; the deinit arc owns the seam",
                        ));
                    }
                    // a diverging argument never hands its answer to
                    // the staging temp: the call's own exit ends the path
                    if self.diverges_at(arg.span()) {
                        arg_val
                    } else {
                        let temp = self.temp_span(param_ty)?;
                        self.widen_into(&temp, param_ty, &arg_val, arg.span())?;
                        TypedPlace {
                            place: temp,
                            ty: param_ty,
                        }
                    }
                },
                _ => arg_val,
            };
            let var_word = self.is_var_ty(arg_val.ty);
            // a free generic fn's parameter over its own type variable
            // claims the argument as the variable's single Raw word, so
            // an argument spanning more than one word refuses here —
            // the one-word instantiation boundary, at the site where
            // the type is concrete
            let var_param = param_ty.is_some_and(|param| self.is_var_ty(param));
            if var_param {
                self.guard_one_word_instantiation(
                    self.members(arg_val.ty),
                    "a generic call's argument",
                    arg.span(),
                )?;
            }
            if direct {
                // the argument's outermost data op wrote its overlay
                // word: the staging records the word's kind and the
                // argument's ownership; no Mov and no window read ride
                // it (a scalar word carries no count). A Raw-declaring
                // callee formal takes the wildcard kind, exactly like
                // the literal path above and the moved-word path below
                let PlaceShape::Word(slot) = arg_val.place.shape else {
                    return Err(mismatch("a directly staged scalar argument"));
                };
                let expected_raw = expected_words
                    .as_deref()
                    .is_some_and(|words| words.get(staged.len()) == Some(&SlotType::Raw));
                staged.push(if expected_raw { SlotType::Raw } else { slot });
                counted.push(false);
                word_arg.push(index);
            } else {
                let addrs = self
                    .word_addrs(&arg_val.place, arg_val.ty)
                    .map_err(|err| err.respanned(arg.span()))?;
                for addr in addrs {
                    self.rc.native_reads([addr.slot]);
                    moves.push((staged.len(), addr.slot));
                    // the variable's single word stages as the wildcard, and
                    // so does any word whose callee-side formal declares Raw
                    // (a multi-word parameter over type variables, the
                    // argument's own words carrying their concrete kinds):
                    // the overlay word records Raw (the runtime kind is the
                    // callee's fact), so the ABI slide admits the pairing
                    // whatever the source's static kind — a scalar staged at
                    // an overlay index no other type visits would otherwise
                    // fail the slide's static check against the Raw parameter.
                    // A Raw-classified staged word always records the count
                    // hand-off: the RC ops check the runtime kind, so a
                    // scalar instantiation is a no-op (a variable-keyed union
                    // member's region claim holds a counted reference under a
                    // reference substitution)
                    let expected_raw = expected_words
                        .as_deref()
                        .is_some_and(|words| words.get(staged.len()) == Some(&SlotType::Raw));
                    staged.push(if var_param || expected_raw {
                        SlotType::Raw
                    } else {
                        addr.slot_ty
                    });
                    counted.push(matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) || var_word);
                    word_arg.push(index);
                }
            }
        }
        // a closure word in a callable-defaulted parameter has no CALLT
        // path: the transfer leaves no post-call position for the borrow
        // flush, and the pre-transfer exit drops would free the object
        // under the callee's read. Its tail lowers in place instead (CALL
        // plus flush plus return, the native-tail shape) —
        // stack-identical and count-exact; the frame-reuse guarantee
        // keeps every scalar-staging tail site, a linked callee's
        // included
        let closure_tail = !defaulted.is_empty()
            && tail
            && matches!(target, Callee::Local(_) | Callee::Foreign { .. });
        // a trait-method tail transfers through CALLTG now (the dispatch
        // tail form, the guarantee admits no callee-class exception): a
        // Static site's evidence and
        // instance words and a Runtime site's sentinel FuncRef ride the
        // same frame-reuse slide a local tail's arguments take, staged
        // after the exit claims. An inherent dispatch stays excluded — it
        // is a plain local callee taking the CALLT transfer below
        let native_tail_return = tail
            && !self.root
            && matches!(
                target,
                Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. }
            );
        // a closure-staging local tail lowers in place for the root frame
        // too: the callee reads the word during the call, so the count
        // cannot ride the transfer nor die at the pre-transfer exit drops
        // a linked package's tail call takes the same CALLT frame-reuse
        // transfer a local's does: the pool FuncRef carries the
        // module boundary, the slide a pure fact of the two layouts, so
        // the transfer crosses the dsmod seam with the stack flat
        // a collection-mutating member (`push`/`set`) answers the receiver
        // after the native's unit word, so it always lowers in place: the
        // transfer would hand the caller the unit word where the member's
        // answer is the collection
        let in_place_tail = native_tail_return || closure_tail || (tail && window_answer);
        // the frame-ending transfer: the exit drops and every claim hook
        // stage through the overlay as their own argument windows, so the
        // call's argument span stages after them, ahead of the CALLT
        let tail_transfer = tail && !in_place_tail;
        // the separator natives' dispatch constant: the char member's own
        // tag id rides beside the union span, so the body reads the
        // staged region through it — tag ids are per-compile facts no
        // static row can name, and the region's dead words hold their
        // kind zeros (the empty-string Ref beside the zero code), which
        // no word-kind test can see through
        if let Callee::Builtin(native) = target
            && matches!(native.name, "join" | "split")
        {
            let char_tag = self.separator_char_tag(param_types.as_deref(), callee_span)?;
            let dst = self.temp_word(SlotType::I64)?;
            let kidx = self.pool(Constant::I64(i64::from(char_tag)));
            self.emit(Op::Kimm {
                dst: Slot::body(dst),
                kidx,
            });
            moves.push((staged.len(), dst));
            staged.push(SlotType::I64);
            counted.push(false);
            word_arg.push(usize::MAX);
        }
        self.guard_staged_count(staged.len())?;
        // the literal arguments' constant loads land on their overlay
        // words at the staging position, in staged order
        for &(at, constant) in &consts {
            self.scalar_const_into(
                constant,
                Slot::overlay(u8::try_from(at).expect("overlay word fits u8")),
            );
        }
        for &(index, slot) in &moves {
            if !tail_transfer {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    src: Slot::body(slot),
                });
            }
            // a staged word of a consuming (`move`) parameter: the callee
            // owns the value's death, so the count and any deinit claim
            // ride the staging as a hand-off — the checker's forced move
            // spent the source's death at exactly this use. A plain
            // count keeps the dup-or-forward death test outside the tail
            // transfer (a live copyable source dups), while a word a
            // claim covers always rides (a borrowed inc would leave the
            // caller's hook armed beside the callee's own)
            let consumes = word_arg
                .get(index)
                .is_some_and(|&arg| consuming.get(arg).copied().unwrap_or(false));
            // a staged Ref: a local callee's borrow parameter owns the
            // handed word (forward on the source's death, dup otherwise);
            // a native is a transient borrow window that never dups. A
            // tail-position staging abandons only when the frame's tail
            // transfer really ends the program (the root frame's named
            // corner); a non-root frame lowers its native tail call in
            // place below, so its staged borrows stay pending for the
            // post-call flush. A callable-defaulted closure word is a
            // borrow window against every callee kind: the callee reads it
            // (CALLS never consumes) and never owns its count, and a
            // closure-staging tail lowers in place below, so its flush
            // always has a post-call position
            if counted[index] {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
                } else if window_answer && word_arg.get(index) == Some(&0) {
                    // a collection-mutating member's receiver: the native
                    // answers the receiver's post-mutation word, which the
                    // re-store below adopts as the receiver's own claim,
                    // so the window's flush never decs a dying receiver —
                    // the replaced word's count dies at that re-store
                    self.rc.staged_native_keep(slot);
                } else {
                    match target {
                        Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. } => {
                            self.rc.staged_native(slot, tail && self.root);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. }
                            if tail_transfer && consumes =>
                        {
                            self.rc.staged_move(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. }
                            if tail_transfer =>
                        {
                            self.rc.staged_tail_local(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. }
                            if consumes && self.rc.claim_base_of(slot).is_some() =>
                        {
                            self.rc.staged_move(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) | Callee::Foreign { .. } => {
                            self.rc.staged_local(&mut self.code, slot);
                        },
                    }
                }
            } else if consumes && self.rc.claim_base_of(slot).is_some() {
                // a consuming scalar claim anchor (no Ref count of its
                // own): the hook rides the staging silently, or this
                // frame's scope end would fire it beside the callee's
                self.rc.staged_move(&mut self.code, slot);
            }
        }
        // The unbox's take is the move-out only at the receiver's own
        // death: a word that survives this call keeps its holder, so the
        // site specializes to the plain read and every holder keeps the
        // container whole (the value door). The receiver is the call's
        // one staged argument.
        let target = if matches!(target, Callee::Builtin(native) if native.name == "box_take") {
            match moves.first() {
                Some(&(_, slot)) if !self.rc.dies_at(slot) => {
                    let def = self
                        .analyzed
                        .package
                        .lookup_value("box_get")
                        .expect("the box read is covered by the ambient");
                    self.resolve_callee(def, callee_span, *span)?
                },
                _ => target,
            }
        } else {
            target
        };
        self.guard_staged_width(target, &staged, callee_span, member_params)?;
        if let Callee::Builtin(native) = target
            && matches!(
                native.shape,
                NativeShape::Fallible { .. }
                    | NativeShape::FallibleByResultType { .. }
                    | NativeShape::Checked
            )
        {
            return self.native_span_result(native, ty, tail, callee_span, &staged);
        }
        // a generic call site: stage the instantiated types as
        // evidence words after the real arguments and dispatch the
        // callee's ONE body through CALLG — or, from the root frame's
        // tail position, through the CALLTG transfer (a non-root frame
        // lowers the native tail in place below, the shape, because
        // the body runs inside the instruction and no frame is pushed)
        let generic_target = match target {
            Callee::Generic(generic) => Some(generic),
            _ => None,
        };
        let mut generic_instance: Option<BuiltinInstance> = None;
        let generic_tail_at = staged.len();
        if let Some(generic) = generic_target {
            if tail && matches!(generic.shape, NativeShape::Checked) {
                return Err(LowerError::unsupported(
                    callee_span,
                    "a tail call to a checked-read generic",
                    "the ok/none span re-spans into the call's union at the call site, and the \
                     tail transfer leaves no position for it; keep the call out of tail position",
                ));
            }
            generic_instance = Some(self.stage_generic_evidence(
                generic,
                def.expect("a generic callee resolved to a def"),
                callee_span,
                *span,
                &mut staged,
                receiver_ty,
                tail_transfer,
                native_member.is_some(),
            )?);
        }
        // a trait-method dispatch site: a call into a generic impl
        // body stages the runtime-evidence words its substitution
        // resolves (one evidence-chain object index per demand, in body
        // order) and then the solved declared-instance word; a call into
        // a monomorphic body stages the instance word alone. A nested
        // bound-variable site stages none of these — its slot word rides
        // below — and a nested site whose serving body is itself generic
        // stages the callee's words from this frame's own evidence area.
        // The staged types register here (the overlay span sizes from
        // them); on the tail path the stores defer past the exit claims,
        // staged after them like every tail-staged word (a claim's
        // overlay window would clobber words staged before it)
        let mut trait_evidence: Vec<EvidenceSrc> = Vec::new();
        let trait_evidence_at = staged.len();
        if let Some(TraitDispatch::Foreign { call, .. }) = &trait_dispatch {
            // a linked body rides no evidence pairs (the walk refuses
            // the generic and conditional impls that demand them, and a
            // trait's default body crosses only when it stages none),
            // so the solved instance word rides alone — the same
            // trailing word a local monomorphic body's site stages.
            // A default body is the exception on the receiver: its
            // `Self` variable claims the receiver as its single Raw
            // word, so a receiver spanning more than one word keeps
            // the local one-word instantiation boundary's own refusal,
            // mirrored at the linked site
            if default_dispatch {
                self.guard_one_word_instantiation(
                    self.members(call.recv),
                    "a linked default body's receiver",
                    callee_span,
                )?;
            }
            self.guard_staged_count(staged.len() + 1)?;
            self.plan.mark_exercised(call.instance.index());
            trait_evidence.push(EvidenceSrc::Const(call.instance.index()));
            staged.push(SlotType::I64);
            if !tail_transfer {
                self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            }
        }
        if let Some(TraitDispatch::Static { index, call }) = &trait_dispatch {
            let pairs = self.plan.functions()[*index as usize].evidence.clone();
            self.guard_staged_count(staged.len() + pairs.len() + 1)?;
            for pair in pairs.iter() {
                let words =
                    if self.plan.is_default_body(*index) || self.plan.is_default_instance(*index) {
                        // the entered body is the trait's declared default:
                        // every demand keys by the trait's own variables —
                        // `Self` names the receiver (the chain starts at its
                        // member set under the site's own solved instance),
                        // every other names a trait generic the impl behind
                        // that instance substitutes through its trait
                        // reference — the declared bounds record answers
                        // neither shape
                        vec![EvidenceSrc::Const(self.default_pair_chain(
                            call.method,
                            call.instance,
                            Some(self.members(call.recv)),
                            pair,
                            "a trait-method call into a declared default body",
                            callee_span,
                        )?)]
                    } else {
                        self.evidence_words(
                            pair,
                            &call.bounds,
                            EvidenceSite {
                                entered: Some(call.instance),
                                inherent_site: None,
                                recv: Some(call.recv),
                                written_recv: self.written_trait_call(call_span).map(|tc| tc.recv),
                            },
                            "a trait-method call into a generic impl body",
                            callee_span,
                        )?
                    };
                trait_evidence.extend(words);
            }
            // The instance word is a per-branch id: a union-receiver
            // dispatch stages the representative branch of the covering
            // group, whose written head names the one impl item the word
            // stands for (the inline-cache story keys on the written
            // head, never on this branch id).
            self.plan.mark_exercised(call.instance.index());
            trait_evidence.push(EvidenceSrc::Const(call.instance.index()));
            for _ in 0..=pairs.len() {
                staged.push(SlotType::I64);
            }
            if !tail_transfer {
                self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            }
        }
        // a generic inherent member call stages the same per-pair
        // runtime-evidence words the trait path stages, minus the
        // instance word an inherent impl never has: the words trail the
        // staged arguments in the overlay, and the body claims them as
        // its trailing evidence parameters, so the ordinary CALL below —
        // and a tail site's CALLT — names the whole staged count; no
        // dispatch uop exists. The staged types register here (the
        // overlay span sizes from them), while the stores defer past the
        // exit claims on the tail path (a claim's overlay window would
        // clobber words staged before it)
        let mut member_evidence: Vec<EvidenceSrc> = Vec::new();
        let member_evidence_at = staged.len();
        if let Some(TraitDispatch::Inherent {
            index,
            bounds,
            method,
        }) = &trait_dispatch
        {
            let pairs = self.plan.functions()[*index as usize].evidence.clone();
            if !pairs.is_empty() {
                self.guard_staged_count(staged.len() + pairs.len())?;
                let inherent_site = self.analyzed.decls.inherent_impls.get(method).copied();
                for pair in pairs.iter() {
                    let words = self.evidence_words(
                        pair,
                        bounds,
                        EvidenceSite {
                            entered: None,
                            inherent_site,
                            recv: receiver_ty,
                            written_recv: written_receiver_ty,
                        },
                        "an inherent method call into a generic impl body",
                        callee_span,
                    )?;
                    member_evidence.extend(words);
                }
                for _ in 0..pairs.len() {
                    staged.push(SlotType::I64);
                }
                if !tail_transfer {
                    self.stage_member_evidence(&member_evidence, member_evidence_at);
                }
            }
        }
        // a free generic fn's call site: the callee's ONE body
        // claims its runtime-evidence demands as trailing parameters,
        // so the entering site stages them after the true arguments in
        // this outgoing-argument area — one evidence-chain object index
        // per demand, the checker's per-call record answering each bound
        // under this site's own substitution. The ordinary CALL below (a
        // tail site's CALLT) names the whole staged count, like the
        // inherent generic body's entering site. The staged types
        // register here (the overlay span sizes from them), while the
        // stores defer past the exit claims on the tail path (a claim's
        // overlay window would clobber words staged before it)
        let mut free_evidence: Vec<EvidenceSrc> = Vec::new();
        let free_evidence_at = staged.len();
        if let Callee::Local(index) = target {
            let pairs = self.plan.functions()[index as usize].evidence.clone();
            // a dispatch target (a trait's declared default body) stages
            // its evidence words through the dispatch branch above; a
            // generic body — the ONE Raw-word body or an instance alike
            // — serves the free-fn staging here, its entering site the
            // one that carries the checker's bound answers
            if trait_dispatch.is_none()
                && self.plan.functions()[index as usize].is_generic_body()
                && !pairs.is_empty()
            {
                let Some(record) = self.written_generic_call_bounds(call_span) else {
                    return Err(LowerError::unsupported(
                        callee_span,
                        "a call into a free generic fn's body",
                        "the checker recorded no bound answer at this site; report the construct",
                    ));
                };
                self.guard_staged_count(staged.len() + pairs.len())?;
                for pair in pairs.iter() {
                    let words = self.free_evidence_words(
                        pair,
                        &record,
                        "a call into a free generic fn's body",
                        callee_span,
                    )?;
                    free_evidence.extend(words);
                }
                for _ in 0..pairs.len() {
                    staged.push(SlotType::I64);
                }
                if !tail_transfer {
                    self.stage_member_evidence(&free_evidence, free_evidence_at);
                }
            }
        }
        // a nested runtime-resolved site: the target rides the frame's
        // evidence slot, whose word names an evidence-chain object. The
        // DISPATCH stages the chain's tail objects' indices as the
        // serving body's trailing evidence parameters (a static table
        // read per hop), so this site stages no words itself — it only
        // reserves the overlay: the runtime-resolved target's demand
        // count is bounded by the widest planned body's, and the
        // dispatch writes exactly what its chain object carries
        let mut runtime_riders = 0usize;
        if let Some(TraitDispatch::Runtime { .. }) = &trait_dispatch {
            runtime_riders = self.plan.max_evidence_pairs();
            self.guard_staged_count(staged.len() + runtime_riders)?;
            for _ in 0..runtime_riders {
                staged.push(SlotType::I64);
            }
        }
        // a checked generic read (`list_get`, `map_get`): the staged span
        // ends in its evidence words and the ok/none span re-spans
        // through the checked path, which emits the CALLG with the
        // real-argument count
        if let Callee::Generic(generic) = target
            && matches!(generic.shape, NativeShape::Checked)
        {
            let real_args = staged.len() - usize::from(generic.evidence);
            return self.checked_native_result(target, ty, &staged, real_args);
        }
        let result_words = self
            .callee_result_words(target, generic_instance, ty)
            .map_err(|err| err.respanned(callee_span))?;
        self.overlay_site(&staged, &result_words);
        let kidx = self.pool(self.funcref_const(target));
        // A native tail call transfers control only from the root frame:
        // CALLT-to-native (and CALLTG-to-generic) pops the frame inside
        // the instruction, so the staged borrows' decs have no code
        // position there — sound solely because the root frame's transfer
        // ends the program (the heap dies whole). From any other frame the
        // same tail position lowers in place: CALL + RET is
        // stack-identical (the native runs inside the CALL instruction,
        // no frame is pushed), and the post-call flush plus the return's
        // exit drops balance every count.
        // a dispatch tail (a trait site, or the root frame's generic
        // call) transfers through CALLTG: the dispatch resolution is the
        // CALLG emission's own and the transfer is the CALLT slide's, so
        // the guarantee admits no callee-class exception
        let dispatch_tail = generic_target.is_some()
            || matches!(
                trait_dispatch,
                Some(
                    TraitDispatch::Static { .. }
                        | TraitDispatch::Runtime { .. }
                        | TraitDispatch::Foreign { .. }
                )
            );
        if tail_transfer {
            // control transfers to the callee; the stack never grows.
            // A local or linked callee owns its handed words (their counts
            // ride the transfer, its own exit drops them); every other
            // owned reference drops before the transfer, and a root-frame
            // native's staged borrow stays unflushed (the handed word's count
            // dies with the program). The exit drops and every claim hook are
            // staged through the overlay as their own argument windows, so the
            // call's argument span and the dispatch tail's evidence words stage
            // after them — the transfer hands the callee its words intact.
            self.rc.native_tail_transfer();
            self.rc.exit(&mut self.code, &[]);
            self.stage_member_evidence(&member_evidence, member_evidence_at);
            self.stage_member_evidence(&free_evidence, free_evidence_at);
            if let (Some(generic), Some(instance)) = (generic_target, generic_instance) {
                // the deferred evidence stores: one instance word per
                // constraint-list entry, staged after the exit claims
                let word = u32::try_from(instance as i64).expect("instance ids fit u32");
                let words = vec![EvidenceSrc::Const(word); usize::from(generic.evidence)];
                self.stage_member_evidence(&words, generic_tail_at);
            }
            self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            self.stage_moves(&moves);
            if dispatch_tail {
                // the dispatch tail's uop names the real arguments; the
                // evidence/instance words trailing them ride the transfer
                let real_args = if let Some(generic) = generic_target {
                    staged.len() - usize::from(generic.evidence)
                } else if let Some(TraitDispatch::Static { index, .. }) = &trait_dispatch {
                    staged.len() - 1 - self.plan.functions()[*index as usize].evidence.len()
                } else if matches!(trait_dispatch, Some(TraitDispatch::Foreign { .. })) {
                    staged.len() - 1
                } else {
                    staged.len() - runtime_riders
                };
                self.emit(Op::Calltg {
                    callee: kidx,
                    nargs: u8::try_from(real_args).expect("argument count fits u8"),
                });
            } else {
                self.emit(Op::Callt {
                    callee: kidx,
                    nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
                });
            }
            // an unreachable marker place; no code follows a tail call
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        // the staged span of a generic call ends in its evidence words;
        // nargs names the real arguments, and the callee's own constraint
        // list carries the count the dispatch reads. A trait-method call
        // stages the same shape: one instance evidence word trails the
        // arguments, and the impl body the FuncRef names runs to
        // completion inside the instruction
        if generic_target.is_some() {
            let real_args = staged.len() - usize::from(generic_target.map_or(0, |g| g.evidence));
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if let Some(TraitDispatch::Static { index, .. }) = &trait_dispatch {
            // the evidence words trail the real arguments (one per the
            // body's runtime demands, then the solved instance), so the
            // uop names the real-argument count and the callee claims
            // the rest as its trailing evidence parameters
            let real_args =
                staged.len() - 1 - self.plan.functions()[*index as usize].evidence.len();
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if matches!(trait_dispatch, Some(TraitDispatch::Foreign { .. })) {
            // the linked body claims no evidence pairs, so
            // only the solved instance word trails the real arguments
            let real_args = staged.len() - 1;
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if matches!(trait_dispatch, Some(TraitDispatch::Runtime { .. })) {
            // a nested runtime-resolved site: the sentinel FuncRef names
            // the frame's evidence slot and dispatches from it; the
            // chain's tail words trail the real arguments for the
            // serving body the slot may name, staged by the dispatch
            // itself from the chain object
            let real_args = staged.len() - runtime_riders;
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else {
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
            });
        }
        self.rc.native_done(&mut self.code);
        let place = if let Some(recv) = window_receiver {
            // a collection-mutating member (`xs.push(x)`, `m.set(k, v)`,
            // `b.set(x)`) answers the receiver's post-mutation word under
            // one count — the receiver's own object at a unique count, a
            // fresh copy at a shared one. The word re-stores through the
            // receiver's window — the replaced word's count dies, the
            // slot adopting the answer's own claim — and the member's
            // result dups the post-mutation word
            let word = single(
                &self
                    .word_addrs(&recv.place, recv.ty)
                    .map_err(|err| err.respanned(callee_span))?,
                callee_span,
            )?;
            self.rc.store_over(&mut self.code, &[word.slot]);
            self.emit(Op::Mov {
                dst: Slot::body(word.slot),
                src: Slot::overlay(0),
            });
            self.rc.produced(word.slot);
            let base = self.alloc(&[SlotType::Ref])?;
            self.emit(Op::Mov {
                dst: Slot::body(base),
                src: Slot::body(word.slot),
            });
            self.rc.duped(&mut self.code, base, word.slot);
            Place {
                base,
                shape: PlaceShape::Word(SlotType::Ref),
            }
        } else {
            let place = self.result_place(ty, &result_words, callee_span)?;
            // a known-function callee's fn-typed result is the scalar its own
            // provenance proved (a factory's result classifies Ref instead),
            // so the result word joins the provenance oracle and widens into
            // a re-storable slot like a named fn value
            if *result_words == [SlotType::Fn] && matches!(target, Callee::Local(_)) {
                self.scalar_fn_words.insert(place.base);
            }
            place
        };
        if in_place_tail {
            // the in-place tail call (a non-root native or generic tail,
            // or a local tail staging a closure into a callable-defaulted
            // parameter): the tail position returns the call's result,
            // the post-call flush settles the borrow windows, and the
            // return's exit drops flush every other count the frame owes
            self.emit_return(&place, ty, e.span())?;
            // an unreachable marker place; no code follows the return
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        Ok(place)
    }

    /// Stages one call's argument span: the recorded body words move into
    /// the overlay in argument order.
    fn stage_moves(&mut self, moves: &[(usize, u8)]) {
        for &(index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
        }
    }

    /// Copies one call's result span out of the overlay into a fresh temp:
    /// the callee transferred its result references out with the return.
    pub(super) fn result_place(
        &mut self,
        ty: Ty,
        result_words: &[SlotType],
        span: Span,
    ) -> Result<Place, LowerError> {
        let base = self.alloc(result_words)?;
        for (index, slot) in result_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("result width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(index).expect("result word fits u8")),
            });
            // a Raw result word is a type variable's runtime word: the
            // callee transferred its count, and the kind-checked drop
            // balances it when the value is a reference
            if *slot == SlotType::Ref || *slot == SlotType::Raw {
                self.rc.produced(word);
            }
        }
        // a deinit-carrying result owns its hook wherever it lands:
        // the temp holds the value until a binder aliases or discards it
        if let Some(claim) = self.deinit_claim(ty, span)? {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let words = self.ref_words(base, ty)?;
            let band = self.claim_band_word(base)?;
            self.rc
                .claimed(&mut self.code, base, claim.hook, band, &words);
        }
        let shape = match &*self.shapes.shape(ty)? {
            // the copy-out word's own kind leads: a factory's fn-typed
            // result classifies Ref (the closure its tail constructs),
            // so the place names the physical word the call slide
            // validated — a Fn classification would hide the count from
            // every use and the binder
            Shape::Word(slot) => PlaceShape::Word(result_words.first().copied().unwrap_or(*slot)),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    /// The computed callee: the callee expression evaluates first and
    /// its word binds through the temporary, the arguments staging after
    /// it (argument evaluation may itself call), the indirect call
    /// reading the word like any binding's. The heap-closure vocabulary
    /// rides along: a fn-typed field read, a subscript read,
    /// or a call result hands the callee word over — a scalar Fn word or
    /// the closure Ref word CALLS reads through.
    fn computed_callee_call(
        &mut self,
        callee: &Expr,
        ty: Ty,
        args: &[CallArg],
        trailing: Option<&Expr>,
        tail: bool,
    ) -> Result<Place, LowerError> {
        let callee_val = self.value(callee)?;
        let callee_ty = self.val(callee.span())?.ty;
        let Some((params, ret)) = fn_sig_of(self.universe(), callee_ty) else {
            return Err(LowerError::unsupported(
                callee.span(),
                "a computed callee",
                "its checked type is not a function",
            ));
        };
        let params = params.to_vec();
        let word = single(
            &self
                .word_addrs(&callee_val.place, callee_val.ty)
                .map_err(|err| err.respanned(callee.span()))?,
            callee.span(),
        )?;
        let mut arg_exprs: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
        if let Some(block) = trailing {
            arg_exprs.push(block);
        }
        self.calls_word(
            ty,
            &params,
            ret,
            &word,
            None,
            callee.span(),
            &arg_exprs,
            tail,
        )
    }

    /// CALLS (the callable-word call form): the callee is a local
    /// binding whose single word names the function.
    fn calls_value(
        &mut self,
        ty: Ty,
        bind: Span,
        callee_span: Span,
        arg_exprs: &[&Expr],
        tail: bool,
    ) -> Result<Place, LowerError> {
        let callee_ty = self.val(callee_span)?.ty;
        let Some((params, ret)) = fn_sig_of(self.universe(), callee_ty) else {
            return Err(LowerError::unsupported(
                callee_span,
                "a callable callee",
                "its checked type is not a function",
            ));
        };
        let params = params.to_vec();
        // the callee use is a read of the binder: a closure word's death
        // test compares against this span
        self.rc.use_of(bind, callee_span);
        let place = match self.binders.get(&bind).cloned() {
            Some(place) => place,
            // a block-local `fn`'s own name calling itself: no slot binds
            // it, the call stages the fn's own word
            None if self.analyzed.checked.self_fns.contains(&bind) => self.self_fn_word()?,
            None => return Err(self.missing_binder(callee_span)),
        };
        // the callee word: a Fn-classified slot directly (a scalar word or
        // a callable-defaulted parameter), a Ref-classified value-site
        // word (CALLS reads the FuncRef behind the closure object's
        // header), or the payload word of a union span an extraction
        // handler's parameter aliases
        let word = single(&self.word_addrs(&place, callee_ty)?, callee_span)?;
        self.calls_word(
            ty,
            &params,
            ret,
            &word,
            Some(bind),
            callee_span,
            arg_exprs,
            tail,
        )
    }

    /// The indirect call through one callee word — a scalar Fn word, or a
    /// Ref word naming a closure object (the read dispatches on the
    /// word's runtime kind; the emitted op is the same). Arguments stage
    /// on the overlay exactly like a pool call, the callee frame overlays
    /// it, and the result copies out of the overlay. A tail position runs
    /// the maybe-Ref callee tracking: the callee word is maybe-Ref by
    /// classification, and the provenance tables (`scalar_fn_words`
    /// naming the function through `fn_words`) prove some words
    /// count-free scalars naming exactly one planned function — those
    /// tails transfer through CALLT with that function's own pool
    /// FuncRef, and every unprovable word (a closure's Ref, a
    /// callable-defaulted parameter, an unproven alias) keeps the
    /// in-place CALLS plus return.
    #[allow(clippy::too_many_arguments)]
    fn calls_word(
        &mut self,
        ty: Ty,
        params: &[Ty],
        ret: Ty,
        word: &WordAddr,
        bind: Option<Span>,
        callee_span: Span,
        arg_exprs: &[&Expr],
        tail: bool,
    ) -> Result<Place, LowerError> {
        if let Some(bind) = bind {
            // the callee use is a read of the binder: a closure word's
            // death test compares against this span
            self.rc.use_of(bind, callee_span);
        }
        let closure = word.slot_ty == SlotType::Ref;
        // the maybe-Ref callee tracking: a proven scalar word is
        // count-free by construction and names exactly one planned
        // function, so a tail call through it can take the CALLT
        // transfer; every other word stays behind the runtime CALLS read
        let proven_fn = self
            .scalar_fn_words
            .contains(&word.slot)
            .then(|| self.fn_words.get(&word.slot).copied())
            .flatten();
        let mut expected_words: Vec<SlotType> = Vec::with_capacity(params.len());
        for ty in params {
            expected_words.extend_from_slice(&self.shapes.words(*ty)?);
        }
        let expected = expected_words.len();
        // the planned body the callee word names, when this frame
        // constructed it (a scalar Fn word or a closure object): its own
        // parameter modes stay readable, so a deinit-carrying argument
        // can be checked against its contract. A word this frame did not
        // construct (a fn-typed parameter's) erases them.
        let callee_index = self.fn_words.get(&word.slot).copied();
        let param_modes = callee_index.and_then(|index| self.planned_param_modes(index));
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call).
        // The borrow window opens before that evaluation and pins the
        // callee word (CALLS reads it during the call) plus every word
        // the staged arguments resolve to
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // per staged word, whether it carries a count to the hand-off: a
        // Ref word always, and the frame's own type-variable word — its
        // single Raw word may hold a counted reference at runtime (the
        // evidence-carrying closure's variable parameter dispatches into
        // Ref-claiming bodies), and the kind-checked inc/dec pair makes
        // the runtime fact safe to stage blind
        let mut counted: Vec<bool> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter: the callee reads the word and never owns its count
        let mut defaulted: Vec<usize> = Vec::new();
        // a deinit-carrying argument pins its hook to this frame's death
        // sites, so a tail through the callable word lowers in place
        // (the callee's borrow would outlive a transferred frame)
        let mut deinit_arg = false;
        self.rc.native_open();
        self.rc.native_reads([word.slot]);
        let mut consts: Vec<(usize, ScalarConst)> = Vec::new();
        for (index, &arg) in arg_exprs.iter().enumerate() {
            let param_ty = params.get(index).copied();
            // A literal scalar argument stages its constant directly on
            // its overlay word. A tail transfer defers the staging past
            // the exit claims, so it keeps the evaluated temp.
            if !tail && let Some((slot, constant)) = self.direct_literal(arg, param_ty)? {
                let at = staged.len();
                staged.push(slot);
                counted.push(false);
                consts.push((at, constant));
                continue;
            }
            // A single-use computed scalar argument writes its overlay
            // word directly: the offer goes to the argument's outermost
            // data op, taken only when every later argument evaluates
            // without touching the overlay.
            let offered = if !tail
                && let Some(slot) = self.direct_compute(arg, param_ty, &arg_exprs[index + 1..])?
            {
                self.scalar_dst = Some(ScalarDst::Arg {
                    index: u8::try_from(staged.len()).expect("overlay word fits u8"),
                    slot,
                });
                true
            } else {
                false
            };
            let arg_val = self.value(arg)?;
            let direct = offered && self.scalar_dst.take().is_none();
            if self.deinit_claim(arg_val.ty, arg.span())?.is_some() {
                // a consuming parameter behind a callable word has no
                // claim contract: the affine pass borrows the argument
                // at an indirect call (the fn type erases the mode), so
                // the callee's own claim would double this frame's —
                // and an untraceable word cannot promise borrow mode
                // either. Per-span claims own the seam; named callees
                // carry the mode.
                match param_modes.as_ref().and_then(|modes| modes.get(index)) {
                    Some(ds_parse::ParamMode::Borrow | ds_parse::ParamMode::Mut(_)) => {},
                    _ => {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument at a callable value",
                            "the callable's fn type erases the parameter mode, so the claim \
                             contract cannot be established; pass the value to a named function",
                        ));
                    },
                }
                if tail {
                    deinit_arg = true;
                }
            }
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter, staged as a borrow window
            if self.is_closure_word(&arg_val.place, arg_val.ty)? {
                Self::guard_callable_staged(Some(&expected_words), staged.len(), arg.span())?;
                defaulted.push(staged.len());
            }
            // a union-typed parameter takes its member's whole region: a
            // bare member, a strict sub-union argument, or an equal-typed
            // argument whose place keeps a wider union's physical layout
            // re-spans into a staging temp of the parameter's region,
            // the named-callee widening family's value-site spelling
            let arg_val = match params.get(index) {
                Some(&param_ty)
                    if self.is_union_ty(param_ty)?
                        && (param_ty != arg_val.ty
                            || !self.place_layout_matches(&arg_val.place, param_ty)?) =>
                {
                    // the same staging-temp backstop as the named
                    // callee path
                    if self.deinit_claim(arg_val.ty, arg.span())?.is_some() {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument into a union parameter",
                            "the re-span's staging temp is anonymous, so it carries no claim \
                             contract of its own; the deinit arc owns the seam",
                        ));
                    }
                    if self.diverges_at(arg.span()) {
                        arg_val
                    } else {
                        let temp = self.temp_span(param_ty)?;
                        self.widen_into(&temp, param_ty, &arg_val, arg.span())?;
                        TypedPlace {
                            place: temp,
                            ty: param_ty,
                        }
                    }
                },
                _ => arg_val,
            };
            // the argument's own variable word stages as the wildcard: a
            // Raw word whose runtime kind the callee claims, so its
            // count rides the hand-off below like a Ref word's
            let var_word = self.is_var_ty(arg_val.ty);
            if direct {
                // the argument's outermost data op wrote its overlay
                // word: the staging records the word's kind; no Mov
                // and no window read ride it (a scalar word carries
                // no count)
                let PlaceShape::Word(slot) = arg_val.place.shape else {
                    return Err(mismatch("a directly staged scalar argument"));
                };
                staged.push(slot);
                counted.push(false);
            } else {
                for addr in self
                    .word_addrs(&arg_val.place, arg_val.ty)
                    .map_err(|err| err.respanned(arg.span()))?
                {
                    self.rc.native_reads([addr.slot]);
                    moves.push((staged.len(), addr.slot));
                    staged.push(addr.slot_ty);
                    counted.push(addr.slot_ty == SlotType::Ref || var_word);
                }
            }
        }
        self.guard_staged_count(staged.len())?;
        if staged.len() != expected {
            let reason = if staged.len() < expected {
                "a defaulted parameter on a callable value has no staging path this round; pass \
                 every argument explicitly"
            } else {
                "the staged words disagree with the callee's parameter span; a union-typed \
                 parameter takes its member's whole region"
            };
            return Err(LowerError::unsupported(
                callee_span,
                "a call argument",
                reason,
            ));
        }
        // the tail transfer: only a proven count-free scalar callee word
        // with no closure-defaulted argument and no deinit-carrying
        // argument takes it — the count-free word needs no settling and
        // the named function's own pool FuncRef drives the CALLT
        let tail_transfer = tail && proven_fn.is_some() && defaulted.is_empty() && !deinit_arg;
        // the literal arguments' constant loads land on their overlay
        // words at the staging position, in staged order
        for &(at, constant) in &consts {
            self.scalar_const_into(
                constant,
                Slot::overlay(u8::try_from(at).expect("overlay word fits u8")),
            );
        }
        for &(index, slot) in &moves {
            if !tail_transfer {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    src: Slot::body(slot),
                });
            }
            // a borrow parameter owns the handed word: forward on the
            // source's death, dup otherwise; a callable-defaulted
            // closure word is a plain borrow — the callee reads it (CALLS
            // never consumes) and never owns its count. A transferring
            // tail's count rides the callee like every staged local word
            // (its own exit drops the handed parameter). The variable
            // word's Raw spelling counts too: the kind-checked pair makes
            // the runtime fact safe to stage blind
            if counted[index] {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
                } else if tail_transfer {
                    self.rc.staged_tail_local(&mut self.code, slot);
                } else {
                    self.rc.staged_local(&mut self.code, slot);
                }
            }
        }
        let mut result_words = self.shapes.words(ret)?;
        // a lambda-value callee is a known function: a factory's fn-typed
        // result is the closure its body constructs, so the result word
        // classifies Ref (a binding carrying an unknown fn value keeps
        // the scalar classification — a closure returned through it traps
        // at the call, the recorded return-flow seam)
        if *result_words == [SlotType::Fn]
            && let Some(&index) = self.fn_words.get(&word.slot)
            && self.tail_builds_a_closure(index)
        {
            result_words = Rc::from([SlotType::Ref]);
        }
        self.overlay_site(&staged, &result_words);
        if tail_transfer {
            // the tracking proved the callee word a count-free scalar
            // naming one planned function: the tail transfers through
            // CALLT with that function's own pool FuncRef, the exit
            // claims staging first and the argument moves after them
            let index = proven_fn.expect("the transfer requires a proven function");
            let kidx = self.pool(Constant::FuncRef {
                key: FuncRefKey::Dep(0), // the enclosing module
                func: index,
            });
            self.rc.native_tail_transfer();
            self.rc.exit(&mut self.code, &[]);
            self.stage_moves(&moves);
            self.emit(Op::Callt {
                callee: kidx,
                nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
            });
            // an unreachable marker place; no code follows a tail call
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        self.emit(Op::Calls {
            callee_slot: word.slot,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        if closure {
            // the call read the closure word: at its death use the count
            // drops here, the object's death releasing the captures
            self.rc.read(&mut self.code, &[word.slot]);
        }
        let place = self.result_place(ty, &result_words, callee_span)?;
        // the same trust as the named-callee path: the factory check ran
        // over this word and left the scalar classification, so the
        // result word is a proven scalar; an unproven callee word keeps
        // no trust (its result may hold a closure's Ref at runtime)
        if *result_words == [SlotType::Fn] && self.fn_words.contains_key(&word.slot) {
            self.scalar_fn_words.insert(place.base);
        }
        if tail {
            // the in-place tail (the unproven maybe-Ref word): the CALLS
            // runs inside this frame, the post-call flush settles the
            // borrow windows, and the return's exit drops flush every
            // other count the frame owes
            self.emit_return(&place, ty, callee_span)?;
            // an unreachable marker place; no code follows the return
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        Ok(place)
    }
}

/// The C words a written parameter type marshals: the scalar words
/// one, a `string` or `bytes` parameter its call-scoped (pointer,
/// length) pair — two. `None` for every type outside the marshaling
/// slice (handles, aggregates, type variables), the same slice the
/// callee resolution validated.
pub(super) fn marshal_word(ty: &Type) -> Option<usize> {
    if scalar_word(ty).is_some() {
        return Some(1);
    }
    let Type::Path { path, args, .. } = ty else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let [one] = &path.segments[..] else {
        return None;
    };
    match one.name.as_str() {
        "string" | "bytes" => Some(2),
        _ => None,
    }
}

/// The return's marshaling word a written return type names: the
/// scalar words their own, anything else none — the unit return rides
/// the absent word the caller spells. Shared with the pool constant's
/// return payload, the same slice the callee resolution validated.
pub(super) fn bound_ret_word(ty: &Type) -> Option<BoundRet> {
    match scalar_word(ty)? {
        SlotType::I64 => Some(BoundRet::Int),
        SlotType::Bool => Some(BoundRet::Bool),
        SlotType::F64 => Some(BoundRet::Float),
        _ => None,
    }
}

/// The scalar word a written type names, when it is one of the
/// marshaling slice's atoms: `int` the i64 word, `bool` the bool
/// word, `float` the f64 word. Shared with the pool constant's return
/// payload, the same slice the callee resolution validated.
pub(super) fn scalar_word(ty: &Type) -> Option<SlotType> {
    let Type::Path { path, args, .. } = ty else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let [one] = &path.segments[..] else {
        return None;
    };
    match one.name.as_str() {
        "int" => Some(SlotType::I64),
        "bool" => Some(SlotType::Bool),
        "float" => Some(SlotType::F64),
        _ => None,
    }
}
