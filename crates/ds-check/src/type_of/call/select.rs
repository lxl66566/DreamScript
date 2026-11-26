//! Selection: the `as` vocabulary's static and member resolution — qualified
//! paths, inherent and trait method selection, the dispatch facts they record,
//! and the `.err`/`.ok` handler arms and extraction calls result types answer
//! through.

use super::super::*;

impl Bodies<'_> {
    /// DS2050: a call site's `as` selection where no trait dispatch
    /// carries an instance vocabulary to select from. The call still
    /// types through its ordinary reading.
    pub(in crate::type_of) fn selection_without_dispatch(
        &mut self,
        selection: &Ident,
        span: Span,
        what: &str,
    ) {
        self.error(
            ErrorCode::InstanceSelectionWithoutDispatch,
            format!(
                "the `as {}` selection names no trait dispatch: {what}",
                selection.name
            ),
            span,
            "selection on a non-dispatch call",
        );
    }

    /// DS2051: the `as` selection names no serving impl of the trait for
    /// the receiver. The note names the named impls serving the receiver
    /// when any do, else every named impl the trait carries.
    pub(in crate::type_of) fn unknown_selection(
        &mut self,
        trait_id: TraitId,
        recv: Ty,
        selection: &Ident,
        span: Span,
    ) {
        let serving = crate::instance_solve::named_servers(
            &self.decls.universe,
            &self.decls.instances,
            &self.assumptions,
            trait_id,
            recv,
        );
        let mut names: Vec<&str> = serving
            .iter()
            .filter_map(|id| self.decls.instances.name(*id))
            .collect();
        if names.is_empty() {
            names = self
                .decls
                .instances
                .of_trait(trait_id)
                .iter()
                .filter_map(|id| self.decls.instances.name(*id))
                .collect();
        }
        names.sort_unstable();
        names.dedup();
        let mut diag = Diagnostic::error(
            ErrorCode::UnknownInstanceSelection,
            format!(
                "the `as {}` selection names no impl of `{}` for `{}`",
                selection.name,
                self.trait_name(trait_id),
                self.decls.universe.render(recv)
            ),
        )
        .with_primary(span, "no impl carries this name for this receiver");
        if !names.is_empty() {
            diag = diag.with_note(format!(
                "the named impls of the trait: {}",
                names
                    .iter()
                    .map(|name| format!("`as {name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        self.diags.push(diag);
    }

    /// The unselected-call note when a receiver's only serving impls are
    /// named: the refusal names the selection the vocabulary offers
    /// instead of a bare missing instance.
    pub(in crate::type_of) fn named_selection_note(
        &self,
        trait_id: TraitId,
        recv: Ty,
    ) -> Option<String> {
        let names: Vec<&str> = crate::instance_solve::named_servers(
            &self.decls.universe,
            &self.decls.instances,
            &self.assumptions,
            trait_id,
            recv,
        )
        .iter()
        .filter_map(|id| self.decls.instances.name(*id))
        .collect();
        if names.is_empty() {
            return None;
        }
        Some(format!(
            "this receiver's impls are named; a call site selects one with `as {}`",
            names.first().unwrap_or(&"")
        ))
    }

    pub(in crate::type_of) fn qualified_call(
        &mut self,
        method: MethodId,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
        selection: Option<&Ident>,
    ) -> Val {
        let Some(sig) = self.member_sig(method) else {
            self.error(
                ErrorCode::UnresolvedName,
                "the qualified member has no registered signature".to_owned(),
                span,
                "no signature",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let trait_id = match &self.package.method(method).owner {
            MethodOwner::Trait(def) => self.decls.traits.get(def).copied(),
            MethodOwner::Inherent(_) => None,
        };
        let Some(recv) = args.first() else {
            self.error(
                ErrorCode::ArityMismatch,
                "the qualified member needs its receiver as the first argument".to_owned(),
                span,
                "missing receiver",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let recv_val = self.expr(&recv.value, None);
        // A selection narrows the gate and the dispatch to one vocabulary
        // slot, the resolution solved once and handed to the record and
        // the member call; the unselected spelling keeps the default
        // vocabulary's gate and resolution.
        let mut selected = None;
        if let Some(trait_id) = trait_id {
            if let Some(selection) = selection {
                let resolution =
                    self.instance_in_slot(trait_id, recv_val.ty, Some(selection.name.as_str()));
                match resolution {
                    Resolution::Unique(_) | Resolution::Covered(_) => {},
                    Resolution::Ambiguous(candidates) => {
                        self.instance_ambiguity(trait_id, recv_val.ty, &candidates, span);
                        for arg in args.iter().skip(1) {
                            self.expr(&arg.value, None);
                        }
                        return Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        };
                    },
                    Resolution::None => {
                        self.unknown_selection(trait_id, recv_val.ty, selection, span);
                        for arg in args.iter().skip(1) {
                            self.expr(&arg.value, None);
                        }
                        return Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        };
                    },
                    Resolution::BoundUnsat { bound, target, .. } => {
                        self.report_bound_miss(trait_id, bound, &target, span);
                        for arg in args.iter().skip(1) {
                            self.expr(&arg.value, None);
                        }
                        return Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        };
                    },
                }
                selected = Some(resolution);
            } else if !self.trait_satisfied(trait_id, recv_val.ty, span) {
                for arg in args.iter().skip(1) {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            }
        } else if let Some(selection) = selection {
            self.selection_without_dispatch(
                selection,
                span,
                "the qualified member is inherent, not a trait dispatch",
            );
        }
        let selected = selected.as_ref();
        if trait_id.is_some() {
            self.record_solved_dispatch(method, recv_val.ty, selected, span);
        }
        self.member_call(
            method,
            &sig,
            recv_val.ty,
            Some(&recv.value),
            &args[1..],
            trailing,
            span,
            selected,
        )
    }

    pub(in crate::type_of) fn dot_call(
        &mut self,
        recv: &Expr,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
        selection: Option<&Ident>,
    ) -> Val {
        // A bare type name as the receiver takes the type-keyed tier: its
        // own impl members claim the spelling, and no value form feeds the
        // receiver. A tag or case name with no recorded member keeps its
        // constructor-value reading, so the ordinary tiers below type it;
        // every other type has no value reading even when the resolver
        // already reported the missing member, so the static path owns the
        // receiver and the poison silences the cascade.
        if let Some((def, recv_span, written)) = self.type_keyed_receiver(recv) {
            let recorded_static = matches!(
                self.res.get(name.span),
                Some(Res::Dot(DotRes::Static { .. }))
            );
            let value_reading = matches!(self.package.def(def).kind, DefKind::Tag | DefKind::Case);
            if recorded_static || !value_reading {
                if let Some(selection) = selection {
                    self.selection_without_dispatch(
                        selection,
                        span,
                        "the type-keyed call dispatches an inherent member, not a trait instance",
                    );
                }
                return self.static_member_call(
                    def,
                    recv_span,
                    written.as_ref(),
                    name,
                    args,
                    trailing,
                    span,
                );
            }
        }
        let Some(res) = self.res.get(name.span).cloned() else {
            // The resolution pass already reported the root cause (no
            // name behind the dot); the empty result must not cascade
            // into downstream empty-intersection errors. The flag sets
            // last — a nested solve_here would wipe it (solve_here
            // clears the flag it reads).
            self.expr(recv, None);
            for arg in args {
                self.expr(&arg.value, None);
            }
            self.solve_reported = true;
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        match res {
            Res::Handler => {
                if let Some(selection) = selection {
                    self.selection_without_dispatch(
                        selection,
                        span,
                        "the handler form dispatches through no trait instance",
                    );
                }
                self.err_ok(recv, &name.name, args, span)
            },
            Res::Dot(DotRes::Free { func, fed_param }) => {
                // The receiver feeds the recorded parameter; only
                // parameter 0 exists (capability parameters have no
                // syntax yet). The free tier keeps the preference, but a
                // free fn whose first parameter cannot contain the
                // receiver's type never claims the call: the registered
                // method candidates serve it
                // instead, so a member and a same-named free fn coexist
                // over disjoint receivers.
                if fed_param == 0 {
                    let candidates: Vec<MethodId> =
                        self.package.method_candidates(&name.name).collect();
                    if !candidates.is_empty() {
                        let recv_val = self.expr(recv, None);
                        if !self.free_fn_takes_receiver(func, recv_val.ty) {
                            return self.method_select_typed(
                                recv,
                                recv_val,
                                name,
                                &candidates,
                                args,
                                trailing,
                                span,
                                selection,
                            );
                        }
                        if let Some(selection) = selection {
                            self.selection_without_dispatch(
                                selection,
                                span,
                                "the free function serves this call, not a trait instance",
                            );
                        }
                        return self.call_def(func, Some(recv), args, trailing, span);
                    }
                }
                if let Some(selection) = selection {
                    self.selection_without_dispatch(
                        selection,
                        span,
                        "the free function serves this call, not a trait instance",
                    );
                }
                let fed = (fed_param == 0).then_some(recv);
                self.call_def(func, fed, args, trailing, span)
            },
            Res::Dot(DotRes::Module { func }) => {
                // A module-qualified call: the linked function takes
                // the written arguments alone — no receiver word stages —
                // and types exactly like the bare call of the bound name.
                if let Some(selection) = selection {
                    self.selection_without_dispatch(
                        selection,
                        span,
                        "the module-qualified function dispatches through no trait instance",
                    );
                }
                self.call_def(func, None, args, trailing, span)
            },
            Res::Dot(DotRes::Methods { candidates }) => {
                self.method_select(recv, name, &candidates, args, trailing, span, selection)
            },
            _ => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` does not name a callable", name.name),
                    name.span,
                    "not callable",
                );
                self.expr(recv, None);
                for arg in args {
                    self.expr(&arg.value, None);
                }
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// The receiver of a type-keyed dot-call: a bare single-segment path
    /// (parentheses break the spelling, mirroring the resolver's side)
    /// whose recorded use names a type-namespace def, or an instantiated
    /// application — a one-argument bracket, a two-argument bracket, or
    /// a written application type carried by the fn-type node — whose
    /// head use names the type. The resolver made the namespace decision
    /// (locals and value defs shadow the type), so this only reads it
    /// back. The written application rides along when the receiver
    /// spells one, so the static tier solves the applied type the call
    /// dispatches.
    pub(in crate::type_of) fn type_keyed_receiver(
        &self,
        recv: &Expr,
    ) -> Option<(DefId, Span, Option<Type>)> {
        if let Expr::Path { path, .. } = recv {
            let [only] = path.segments.as_slice() else {
                return None;
            };
            return match self.res.get(only.span) {
                Some(Res::Def(def)) if self.package.def(*def).kind.in_type_namespace() => {
                    Some((*def, only.span, None))
                },
                _ => None,
            };
        }
        self.applied_receiver(recv)
    }

    /// The instantiated form of a type-keyed receiver: the head's
    /// recorded def plus the written application it dispatches. The
    /// bracket forms carry expression-position type spellings the
    /// checker rereads as types (the constructor-bracket convention);
    /// the fn-type node carries the written application itself — the
    /// synthesized spelling for shapes the brackets cannot express.
    pub(in crate::type_of) fn applied_receiver(
        &self,
        recv: &Expr,
    ) -> Option<(DefId, Span, Option<Type>)> {
        let (head_span, def, written): (Span, DefId, Option<Type>) = match recv {
            Expr::Index { recv, index, .. } => {
                let Expr::Path { span, path, .. } = recv.as_ref() else {
                    return None;
                };
                let [only] = path.segments.as_slice() else {
                    return None;
                };
                let def = self.recorded_type_def(only.span)?;
                let arg = Self::ctor_type_arg(index)?;
                (
                    only.span,
                    def,
                    Some(Type::Path {
                        span: *span,
                        path: path.clone(),
                        args: vec![arg],
                        delim: Delim::Bracket,
                    }),
                )
            },
            Expr::MapApply {
                span,
                head,
                key,
                value,
                ..
            } => {
                let def = self.recorded_type_def(head.span)?;
                let key = Self::ctor_type_arg(key)?;
                let value = Self::ctor_type_arg(value)?;
                (
                    head.span,
                    def,
                    Some(Type::Path {
                        span: *span,
                        path: Path {
                            span: head.span,
                            segments: vec![head.clone()],
                        },
                        args: vec![key, value],
                        delim: Delim::Bracket,
                    }),
                )
            },
            Expr::FnType { ty, .. } => {
                let Type::Path { path, args, .. } = ty.as_ref() else {
                    return None;
                };
                let [only] = path.segments.as_slice() else {
                    return None;
                };
                if args.is_empty() {
                    return None;
                }
                (
                    only.span,
                    self.recorded_type_def(only.span)?,
                    Some((**ty).clone()),
                )
            },
            _ => return None,
        };
        Some((def, head_span, written))
    }

    /// The type-namespace def a resolver-recorded use names, `None` when
    /// the span carries no type-namespace def.
    pub(in crate::type_of) fn recorded_type_def(&self, span: Span) -> Option<DefId> {
        match self.res.get(span) {
            Some(Res::Def(def)) if self.package.def(*def).kind.in_type_namespace() => Some(*def),
            _ => None,
        }
    }

    /// Refusal epilogue shared by `static_member_call`'s dead ends: the
    /// argument words are still walked so their own checks run, then the
    /// empty-typed value returns. `poisoned` is that value's cascade flag;
    /// `reported` raises `solve_reported` before the walk, as the drained
    /// words may observe it.
    pub(in crate::type_of) fn refuse_static_member_call(
        &mut self,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        poisoned: bool,
        reported: bool,
    ) -> Val {
        if reported {
            self.solve_reported = true;
        }
        for arg in args {
            self.expr(&arg.value, None);
        }
        if let Some(tail) = trailing {
            self.expr(tail, None);
        }
        Val {
            ty: self.empty(),
            diverges: false,
            poisoned,
        }
    }

    /// The type-keyed static member call: the
    /// receiver names a type, the member lives in that type's own impl,
    /// and the call carries no instance — resolution is a check-time
    /// fact keyed by the type alone. The resolver registered the
    /// type's members with that name as the candidates, so at most one
    /// exists (registration dedups by name and owner); the member must
    /// be receiverless, its owner's `Self` binds the call, and a
    /// generic impl's member refuses behind the bare type name alone —
    /// an instantiated receiver carries the type arguments, so it
    /// proceeds and stages the body's evidence exactly like a solved
    /// inherent dot-call.
    pub(in crate::type_of) fn static_member_call(
        &mut self,
        def: DefId,
        recv_span: Span,
        written: Option<&Type>,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let type_name = self.package.def(def).name.clone();
        // A missing record means the resolver reported the missing member at
        // the method position; the empty result must not cascade.
        let Some(Res::Dot(DotRes::Static { candidates })) = self.res.get(name.span) else {
            return self.refuse_static_member_call(args, trailing, true, true);
        };
        let Some(&method) = candidates.first() else {
            return self.refuse_static_member_call(args, trailing, true, true);
        };
        let Some(sig) = self.member_sig(method) else {
            self.error(
                ErrorCode::UnresolvedName,
                format!("member `{}` has no registered signature", name.name),
                span,
                "no signature",
            );
            return self.refuse_static_member_call(args, trailing, false, false);
        };
        // A receiver-taking member is not reachable through the type name
        // — the type-keyed surface serves receiverless members only.
        if sig.params.first().is_some_and(|p| p.name == "self") {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "member `{}` of `{}` takes a receiver; call it on a value of `{}`",
                    name.name, type_name, type_name
                ),
                span,
                "receiverless call of a receiver member",
            );
            return self.refuse_static_member_call(args, trailing, false, false);
        }
        // A generic impl's member cannot be spelled through the bare type
        // name: the call would need the impl's type arguments, and the
        // bare receiver position carries none. An instantiated receiver
        // carries them, so the refusal names the applied spelling.
        if written.is_none()
            && let Some(&impl_site) = self.decls.inherent_impls.get(&method)
            && self.decls.inherent_impl_bounds.contains_key(&impl_site)
        {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "member `{}` of `{}` lives in a generic impl; the type-keyed call needs the \
                     applied receiver `{}[...]`, which the bare type name cannot spell [H3]",
                    name.name, type_name, type_name
                ),
                span,
                "generic impl member",
            );
            return self.refuse_static_member_call(args, trailing, false, false);
        }
        // The owner's own type binds `Self` at the call: solved through
        // the normal name solver over the receiver's own spelling, so an
        // enum is the union of its cases and an alias its body — and an
        // instantiated receiver solves the applied type, whose
        // substitution the member's signature reads below.
        let owner_written = written.cloned().unwrap_or_else(|| Type::Path {
            span: recv_span,
            path: Path {
                span: recv_span,
                segments: vec![Ident {
                    span: recv_span,
                    name: type_name.clone(),
                }],
            },
            args: Vec::new(),
            delim: Delim::Bracket,
        });
        let Some(owner_ty) = self.solve_here(&owner_written) else {
            if let Some(applied) = &written {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the type-keyed receiver `{}` does not solve against the package \
                             declarations",
                            decls::render_type(applied)
                        ),
                        span,
                        "unsolvable application",
                    );
                }
                self.solve_reported = true;
            } else {
                self.error(
                    ErrorCode::UnresolvedName,
                    format!("`{type_name}` names no value-bearing type"),
                    span,
                    "no owner type",
                );
            }
            return self.refuse_static_member_call(args, trailing, false, false);
        };
        // The static dispatch record: the method id at the call's own
        // span, the same table a solved inherent dot-call records into —
        // the lowerer resolves the member's body through it and stages no
        // receiver word. A generic impl's body dispatches on runtime
        // evidence: the instantiated receiver records the serving
        // instance per declared bound under the application's
        // substitution, the value-receiver record's own mirror.
        self.record_inherent_call(span, method);
        self.method_selections.insert(name.span, method);
        if let Some(&impl_site) = self.decls.inherent_impls.get(&method)
            && let Some(record) = self.decls.inherent_impl_bounds.get(&impl_site).cloned()
        {
            let bounds = self.inherent_evidence(record.head, &record.conditions, owner_ty, span);
            if !bounds.is_empty() {
                self.record_inherent_bounds(span, bounds);
            }
        }
        self.member_call(method, &sig, owner_ty, None, args, trailing, span, None)
    }

    /// Method-candidate selection: candidates narrow by receiver
    /// type — trait members through the instance environment (or an
    /// in-scope assumption), inherent members by owner type; survivors
    /// beyond one are true ambiguity, never a priority order.
    pub(in crate::type_of) fn method_select(
        &mut self,
        recv: &Expr,
        name: &Ident,
        candidates: &[MethodId],
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
        selection: Option<&Ident>,
    ) -> Val {
        let recv_val = self.expr(recv, None);
        self.method_select_typed(
            recv, recv_val, name, candidates, args, trailing, span, selection,
        )
    }

    /// The candidate selection over a receiver already typed once (the
    /// free-tier fallback's entry): identical to [`Self::method_select`]
    /// apart from the pre-typed receiver, whose second typing would
    /// double every diagnostic an erroneous receiver already reported.
    /// A selection narrows every trait candidate's resolution to one
    /// vocabulary slot and bypasses the assumption shortcut: a named
    /// instance answers a selected call alone, and an in-scope bound
    /// covers the default vocabulary only.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::type_of) fn method_select_typed(
        &mut self,
        recv: &Expr,
        recv_val: Val,
        name: &Ident,
        candidates: &[MethodId],
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
        selection: Option<&Ident>,
    ) -> Val {
        let slot = selection.map(|s| s.name.as_str());
        // Each survivor carries the resolution its trait branch solved:
        // the selected survivor hands its resolution to the dispatch
        // record and the member call, so the pair solves once.
        let mut survivors: Vec<(MethodId, Arc<FnSig>, Option<Resolution>)> = Vec::new();
        let mut miss = None;
        let mut selected_miss = None;
        for &candidate in candidates {
            let Some(sig) = self.member_sig(candidate) else {
                continue
            };
            match &self.package.method(candidate).owner {
                MethodOwner::Trait(def) => {
                    let Some(trait_id) = self.decls.traits.get(def).copied() else {
                        continue;
                    };
                    if slot.is_none() && self.assumption_covers(trait_id, recv_val.ty) {
                        survivors.push((candidate, sig, None));
                        continue;
                    }
                    match self.instance_in_slot(trait_id, recv_val.ty, slot) {
                        resolution @ (Resolution::Unique(_)
                        | Resolution::Covered(_)
                        | Resolution::Ambiguous(_)) => {
                            survivors.push((candidate, sig, Some(resolution)));
                        },
                        // A head that matched while its bound failed is
                        // the nearest miss: the diagnostic below names the
                        // unsatisfied bound instead of the receiver alone.
                        Resolution::BoundUnsat { bound, target, .. } => {
                            if miss.is_none() {
                                miss = Some((trait_id, bound, target));
                            }
                        },
                        Resolution::None => {
                            if slot.is_some() && selected_miss.is_none() {
                                selected_miss = Some(trait_id);
                            }
                        },
                    }
                },
                MethodOwner::Inherent(target) => {
                    // A receiverless member answers through the type-keyed
                    // surface alone; a value receiver never selects it.
                    if sig.params.first().is_none_or(|p| p.name != "self") {
                        continue;
                    }
                    if self.inherent_accepts(target, recv_val.ty) {
                        survivors.push((candidate, sig, None));
                    }
                },
            }
        }
        if survivors.is_empty() {
            // A poisoned receiver already reported; "no method accepts
            // `<empty>`" on top of it would be a cascade. The arguments
            // still walk so their own errors surface.
            if recv_val.poisoned {
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
            if let Some((trait_id, bound, target)) = miss {
                self.report_bound_miss(trait_id, bound, &target, span);
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            }
            if let (Some(selection), Some(trait_id)) = (selection, selected_miss) {
                self.unknown_selection(trait_id, recv_val.ty, selection, span);
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            }
            let trait_names: Vec<String> = candidates
                .iter()
                .filter_map(|c| match &self.package.method(*c).owner {
                    MethodOwner::Trait(def) => Some(self.package.def(*def).name.clone()),
                    MethodOwner::Inherent(_) => None,
                })
                .collect();
            let mut diag = Diagnostic::error(
                ErrorCode::UnsatisfiedConstraint,
                if trait_names.is_empty() {
                    format!(
                        "no method `{}` accepts receiver `{}`",
                        name.name,
                        self.decls.universe.render(recv_val.ty)
                    )
                } else {
                    format!(
                        "no instance of {} for `{}`",
                        trait_names
                            .iter()
                            .map(|t| format!("`{t}`"))
                            .collect::<Vec<_>>()
                            .join(" or "),
                        self.decls.universe.render(recv_val.ty)
                    )
                },
            )
            .with_primary(
                span,
                if trait_names.is_empty() { "no accepting owner" } else { "unimplemented trait" },
            );
            if !trait_names.is_empty()
                && let Some(trait_id) = self
                    .package
                    .lookup_type(&trait_names[0])
                    .and_then(|def| self.decls.traits.get(&def).copied())
                && let Some(note) = self.named_selection_note(trait_id, recv_val.ty)
            {
                diag = diag.with_note(note);
            }
            self.diags.push(diag);
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        if survivors.len() > 1 {
            let names: Vec<String> = candidates
                .iter()
                .filter(|c| self.member_sig(**c).is_some())
                .map(|c| match &self.package.method(*c).owner {
                    MethodOwner::Trait(def) => format!("`{}`", self.package.def(*def).name),
                    MethodOwner::Inherent(target) => format!("inherent `{target}`"),
                })
                .collect();
            self.error(
                ErrorCode::AmbiguousCall,
                format!(
                    "dot-call `{}` is truly ambiguous between {}; qualify with `Trait::{}` [F3]",
                    name.name,
                    names.join(" and "),
                    name.name
                ),
                span,
                "ambiguous method",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let (method, sig, resolution) = survivors.remove(0);
        if selection.is_some()
            && matches!(self.package.method(method).owner, MethodOwner::Inherent(_))
        {
            self.selection_without_dispatch(
                selection.unwrap_or(name),
                span,
                "the member is inherent, not a trait dispatch",
            );
        }
        self.method_selections.insert(name.span, method);
        self.record_solved_dispatch(method, recv_val.ty, resolution.as_ref(), span);
        self.member_call(
            method,
            &sig,
            recv_val.ty,
            Some(recv),
            args,
            trailing,
            span,
            resolution.as_ref(),
        )
    }

    /// Whether a dot-call's free tier can take the receiver: the free fn
    /// keeps the preference when its
    /// first parameter's solved type contains the receiver's type. A
    /// missing signature or parameter, an unsolvable parameter type, or
    /// an already-failed receiver keeps the free reading — the fallback
    /// never widens an existing refusal.
    pub(in crate::type_of) fn free_fn_takes_receiver(&mut self, func: DefId, recv: Ty) -> bool {
        let Some(sig) = self.decls.fns.get(&func).cloned() else {
            return true;
        };
        let Some(param) = sig.params.first() else {
            return true;
        };
        let Some(written) = param.ty.as_ref() else {
            return true;
        };
        let Some(expected) = self.solve_here(written) else {
            return true;
        };
        if self.decls.universe.is_empty(recv) {
            return true;
        }
        self.decls.universe.contains(expected, recv)
    }

    /// Whether an inherent impl's owner accepts the receiver: every
    /// member of the receiver's type must be the owner's atom or an
    /// application of it (a generic impl's constructor). The receiver is
    /// contained in what the owner covers — the instance check's inherent
    /// mirror, never the converse — so a union receiver narrows first and
    /// a receiver the owner does not cover reports at the dot. An owner
    /// naming no declared atom (an alias, a union target) accepts nothing.
    pub(in crate::type_of) fn inherent_accepts(&self, target: &str, recv: Ty) -> bool {
        let Some(def) = self
            .package
            .lookup_type(target)
            .or_else(|| self.decls.foreign_type(target))
        else {
            return false;
        };
        let Some(&owner) = self.decls.atoms.get(&def) else {
            return false;
        };
        let universe = &self.decls.universe;
        universe.members(recv).iter().all(|&member| {
            member == owner
                || matches!(
                    universe.atom_decl(member),
                    Ok(decl) if matches!(decl.kind, AtomKind::Apply(ctor, _) if ctor == owner)
                )
        })
    }

    /// Records one selected method's solved dispatch at the call's own
    /// span. A trait survivor records the instance the environment
    /// resolved for the receiver's type — a containing head, a generic
    /// head that unifies under provable bounds, or a covering
    /// group of sibling branches whose representative names the shared
    /// impl item — where the solved instance is the declared one and
    /// the recorded receiver type carries the substitution. An inherent survivor records its
    /// method id (the lowerer resolves the impl item through it, and no
    /// instance exists). A site satisfied only through an in-scope
    /// assumption with no resolving instance records nothing — its
    /// dispatch is the runtime-resolved evidence form.
    pub(in crate::type_of) fn record_solved_dispatch(
        &mut self,
        method: MethodId,
        recv: Ty,
        resolution: Option<&Resolution>,
        span: Span,
    ) {
        if matches!(&self.package.method(method).owner, MethodOwner::Inherent(_)) {
            self.record_inherent_call(span, method);
            // A generic inherent impl's body dispatches on runtime
            // evidence: the site records the serving instance per
            // declared bound under the receiver's substitution — the
            // trait arm's per-bound record minus the instance word an
            // inherent impl never has.
            if let Some(&impl_site) = self.decls.inherent_impls.get(&method)
                && let Some(record) = self.decls.inherent_impl_bounds.get(&impl_site).cloned()
            {
                let bounds = self.inherent_evidence(record.head, &record.conditions, recv, span);
                if !bounds.is_empty() {
                    self.record_inherent_bounds(span, bounds);
                }
            }
            return;
        }
        let MethodOwner::Trait(def) = &self.package.method(method).owner else {
            return;
        };
        let Some(&trait_id) = self.decls.traits.get(def) else {
            return;
        };
        // The dot-call's selected resolution arrives from selection; the
        // other call spellings solve the pair here.
        let resolved;
        let resolution = if let Some(resolution) = resolution {
            resolution
        } else {
            resolved = self.instance_resolution(trait_id, recv);
            &resolved
        };
        match resolution {
            Resolution::Unique(instance) | Resolution::Covered(instance) => {
                let instance = *instance;
                // a generic impl's body dispatches on runtime evidence:
                // record the per-bound instances the substitution serves,
                // so the lowering side stages them without re-deriving types
                let bounds = crate::instance_solve::evidence_bounds(
                    &mut self.decls.universe,
                    &self.decls.instances,
                    instance,
                    recv,
                );
                self.record_trait_call(span, TraitCall {
                    method,
                    trait_id,
                    instance,
                    impl_site: self.decls.instance_impls.get(&instance).copied(),
                    recv,
                    bounds,
                });
            },
            Resolution::Ambiguous(candidates) => {
                self.instance_ambiguity(trait_id, recv, candidates, span);
            },
            Resolution::None | Resolution::BoundUnsat { .. } => {},
        }
    }

    /// The `Self` a member call binds: the receiver's type, widened to the
    /// solving instance's written head when that head is closed and
    /// contains the receiver — the dispatched member body was checked
    /// over the written head, so its `Self`-typed parameters accept the
    /// head's whole member set and a case-narrowed receiver does not
    /// pins them to its own case. A generic written head keeps the
    /// receiver, which carries the call's substitution. The dot-call's
    /// selected resolution arrives from selection; the other call
    /// spellings solve the pair here.
    pub(in crate::type_of) fn self_binding(
        &self,
        method: MethodId,
        recv: Ty,
        resolution: Option<&Resolution>,
    ) -> Ty {
        let MethodOwner::Trait(def) = &self.package.method(method).owner else {
            return recv;
        };
        let Some(&trait_id) = self.decls.traits.get(def) else {
            return recv;
        };
        let resolved;
        let resolution = if let Some(resolution) = resolution {
            resolution
        } else {
            resolved = self.instance_resolution(trait_id, recv);
            &resolved
        };
        let (&Resolution::Unique(id) | &Resolution::Covered(id)) = resolution else {
            return recv;
        };
        let Ok(written) = self.decls.instances.written(id) else {
            return recv;
        };
        if self.decls.universe.contains(written, recv)
            && !crate::instance_solve::head_carries_vars(&self.decls.universe, written)
        {
            written
        } else {
            recv
        }
    }

    /// One selected method's call check: the member signature with `Self`
    /// bound through [`Self::self_binding`] and the trait's own generics
    /// bound through the dispatch's solved instance (the impl's trait
    /// reference rewritten against the receiver), so arguments check
    /// against the site's substitution — a generic parameter left free
    /// bound itself to any argument, accepting types the substitution
    /// does not cover. A value receiver passes through the
    /// same mut-window exclusivity gate as a free call's fed argument.
    /// The dot-call's selected resolution arrives from
    /// selection (the pair solved once there); the other call spellings
    /// pass `None` and solve here.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::type_of) fn member_call(
        &mut self,
        method: MethodId,
        sig: &FnSig,
        recv_ty: Ty,
        recv_expr: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
        resolution: Option<&Resolution>,
    ) -> Val {
        let self_ty = self.self_binding(method, recv_ty, resolution);
        let mut vars = FxHashMap::from_iter([("Self".to_owned(), self_ty)]);
        if let MethodOwner::Trait(def) = &self.package.method(method).owner
            && let Some(&trait_id) = self.decls.traits.get(def)
            && let Some(instance) = match resolution {
                Some(resolution) => Resolution::solved(resolution),
                None => self.instance_resolution(trait_id, recv_ty).solved(),
            }
            && let Some(&site) = self.decls.instance_impls.get(&instance)
            && let Some(decl_args) = self.decls.trait_impl_args.get(&site)
            && let Some(names) = self.decls.trait_generics.get(def)
            && let Some(substituted) = crate::instance_solve::substituted_trait_args(
                &mut self.decls.universe,
                &self.decls.instances,
                instance,
                decl_args,
                recv_ty,
            )
        {
            for (name, ty) in names.iter().zip(substituted) {
                if let Some(ty) = ty {
                    vars.insert(name.clone(), ty);
                }
            }
        }
        // A generic inherent impl's head pattern binds its variables at
        // the call: the receiver unifies with the head, and each
        // variable's solved type substitutes into the member's written
        // signature (the trait branch's instance substitution mirrored
        // on the inherent side — a variable the head does not bind, or a
        // receiver it cannot unify with, leaves the written name to the
        // ordinary solver).
        if let Some(&impl_site) = self.decls.inherent_impls.get(&method)
            && let Some(record) = self.decls.inherent_impl_bounds.get(&impl_site).cloned()
            && let Some(pairs) = crate::instance_solve::pattern_substitution(
                &self.decls.universe,
                record.head,
                self_ty,
            )
        {
            for (var, members) in pairs {
                let Some(name) = crate::instance_solve::var_param_name(&self.decls.universe, var)
                else {
                    continue;
                };
                let ty = self.decls.universe.set_ty(&members);
                vars.insert(name, ty);
            }
        }
        self.type_vars.push(vars);
        // The receiver already filled the `self` parameter;
        // the remaining arguments feed the rest. A receiverless member
        // (the type-keyed surface) has no filled slot, so the written
        // arguments feed the whole signature. The receiver's own slot
        // types as the call's Self binding: the declaration leaves
        // `self` un-annotated, and every other un-annotated parameter
        // keeps its written spelling to the ordinary solver below.
        let skip = usize::from(sig.params.first().is_some_and(|p| p.name == "self"));
        let mut param_tys = Vec::with_capacity(sig.params.len());
        for (index, param) in sig.params.iter().enumerate() {
            param_tys.push(match param.ty.as_ref() {
                Some(t) => self.solve_here(t),
                None if index < skip => Some(self_ty),
                None => None,
            });
        }
        // The site's solved parameter types, for the call-site staging
        // a dispatch through a body no planned def names reads (a linked
        // member's): recorded only when every parameter solves, so a
        // written type this site could not spell keeps the consuming
        // side's own fallback.
        if param_tys.iter().all(Option::is_some) {
            self.record_member_params(span, param_tys.iter().filter_map(|ty| *ty).collect());
        }
        // The receiver expression fills the `self` slot, so a `mut self`
        // member checks its exclusivity exactly like a free call's fed
        // `mut` argument: a dot-call on a `const` binding of a
        // copyable type rejects identically to the free-function
        // spelling. The static tier passes no receiver, so it skips.
        if skip == 1
            && let Some(recv) = recv_expr
        {
            self.check_mut_window_args(&[Some(recv)], &sig.params[..1]);
        }
        if let Some(slots) = self.arg_slots(None, skip, args, trailing, &sig.params, span) {
            self.check_slots(&slots[skip..], &sig.params[skip..], &param_tys[skip..]);
        }
        let ret = sig
            .ret
            .as_ref()
            .and_then(|r| self.solve_here(r))
            .unwrap_or_else(|| self.unit());
        self.type_vars.pop();
        Val {
            ty: ret,
            diverges: self.decls.universe.is_empty(ret),
            poisoned: false,
        }
    }

    /// `.err(h)`/`.ok(h)`: difference-typed handlers. The handled set
    /// is the handler's annotated parameter, or — unannotated — everything
    /// except what the handler's own result keeps of the operand; that set
    /// is the parameter's expected type.
    pub(in crate::type_of) fn err_ok(
        &mut self,
        recv: &Expr,
        name: &str,
        args: &[ds_parse::CallArg],
        span: Span,
    ) -> Val {
        let recv_val = self.expr(recv, None);
        let total = recv_val.ty;
        if args.len() != 1 {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ArityMismatch,
                    format!("`.{name}` takes the handler lambda as its one argument"),
                )
                .with_primary(span, "wrong arity"),
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let Expr::Lambda {
            params: handler_params,
            body,
            ..
        } = peel_paren(&args[0].value)
        else {
            self.error(
                ErrorCode::TypeMismatch,
                format!("`.{name}` handles through a lambda `(e) => ...`"),
                args[0].span,
                "not a handler lambda",
            );
            self.expr(&args[0].value, None);
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let annotated = handler_params
            .first()
            .and_then(|p| p.ty.as_ref())
            .and_then(|t| self.solve_here(t));
        let diags_mark = self.diags.len();
        let value_pos = self.value_pos;
        let mut body_val = self.handler_body(handler_params, body, annotated.unwrap_or(total));
        let handled_set = if let Some(written) = annotated {
            written
        } else {
            let kept = self.decls.universe.inter(total, body_val.ty);
            let mut handled = self.decls.universe.diff(total, kept);
            // `.ok` on a `t | none` receiver whose body walk derives no
            // set defaults the handled side to the receiver minus
            // `none`: the handler reads as the present-value arm, the
            // `none` member flowing the remainder. `.err` keeps the
            // difference rule's own answer.
            if name == "ok"
                && self.decls.universe.is_empty(body_val.ty)
                && !self.decls.universe.is_empty(total)
                && !recv_val.poisoned
                && self.decls.universe.members(total).contains(&TypeId::NONE)
            {
                let present = self.none_ty();
                handled = self.decls.universe.diff(total, present);
            }
            // The handled members thread back as the parameter's expected
            // type: a handler covering a proper part of the operand checks
            // again with its parameter at that set, so a use of the
            // parameter at the handled type stands. The first walk's
            // diagnostics all sit inside the body it typed and the second
            // walk's replace them; one reported from elsewhere (a lazily
            // typed top-level binding's initializer) keeps the first walk.
            let remainder = self.decls.universe.diff(total, handled);
            if !self.decls.universe.is_empty(handled)
                && !self.decls.universe.is_empty(remainder)
                && self.diags[diags_mark..]
                    .iter()
                    .all(|diag| reported_in(diag, body.span()))
            {
                self.diags.truncate(diags_mark);
                self.value_pos = value_pos;
                body_val = self.handler_body(handler_params, body, handled);
            }
            handled
        };
        self.record_handler_handled(span, handled_set);
        let body_ty = body_val.ty;
        // An annotated handler disjoint from the operand refuses; an
        // unannotated one whose result keeps every member covers nothing
        // and leaves the operand whole.
        let overlap = self.decls.universe.inter(total, handled_set);
        if annotated.is_some()
            && self.decls.universe.is_empty(overlap)
            && !self.decls.universe.is_empty(total)
            && !recv_val.poisoned
        {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "the handler's `{}` handles `{}`, which `{}` does not carry",
                    name,
                    self.decls.universe.render(handled_set),
                    self.decls.universe.render(total)
                ),
                span,
                "disjoint handler",
            );
        }
        let remainder = self.decls.universe.diff(total, handled_set);
        // the result carries the handler's own answer beside the kept
        // side (the MapErr contract `fn(e) -> e2` answers `t | e2`): a
        // handler mapping the error onto a member the receiver never
        // carried must widen the answer in, or the recorded type omits
        // the only member the err arm ever writes. An answer that never
        // solves at the site (the `die` idiom: a generic extern whose
        // return variable no expectation feeds) names no knowable
        // member — a minted variable's atom name is the only `$` a
        // solved set can carry, surface names cannot spell it — and
        // contributes nothing.
        let answered = self.decls.universe.members(body_ty).iter().any(|m| {
            self.decls
                .universe
                .atom_decl(*m)
                .is_ok_and(|d| d.name.contains('$'))
        });
        let result =
            if answered { remainder } else { self.decls.universe.union(remainder, body_ty) };
        if self.decls.universe.is_empty(result)
            && !self.decls.universe.is_empty(total)
            && !recv_val.poisoned
        {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`.{name}` leaves nothing: the handler consumed all of `{}`",
                    self.decls.universe.render(total)
                ),
                span,
                "empty remainder",
            );
        }
        Val {
            ty: result,
            diverges: false,
            poisoned: false,
        }
    }

    /// One handler lambda's body under its parameter's type: the first
    /// parameter binds immutably at `param_ty`, the body carries no
    /// expectation (its own answer is the result's own member), and the
    /// walk leaves no binding behind.
    pub(in crate::type_of) fn handler_body(
        &mut self,
        params: &[Param],
        body: &Expr,
        param_ty: Ty,
    ) -> Val {
        let saved = self.snapshot();
        if let Some(first) = params.first() {
            self.bind_local(first.name.span, Binding {
                ty: param_ty,
                mutable: false,
            });
        }
        let val = self.expr(body, None);
        self.restore(saved);
        val
    }

    /// The family member a call addresses, when the callee's shape could
    /// be the ambient's: a generic function of the family name and
    /// two-parameter arity. Without the ambient, a same-named generic
    /// function rides the family rules (the natives' name-routing
    /// hazard, unchanged).
    pub(in crate::type_of) fn extraction_kind(
        &self,
        def: DefId,
        sig: &FnSig,
    ) -> Option<Extraction> {
        let kind = Extraction::of_name(&self.package.def(def).name)?;
        (!sig.generics.is_empty() && sig.params.len() == 2).then_some(kind)
    }

    /// One extraction-family call: the receiver and one argument,
    /// typed locally against the checked receiver's members. The eager
    /// `unwrap_or` binds its fallback to the extracted type, the lazy
    /// `unwrap_or_else` checks a zero-parameter lambda against it, `ok_or`
    /// unions the new member in, and `map_err` mirrors the `.err`/`.ok`
    /// handler rules — the annotated parameter handles, or everything
    /// but what the lambda's own result keeps.
    pub(in crate::type_of) fn extraction_call(
        &mut self,
        kind: Extraction,
        fed: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        if trailing.is_some() {
            self.error(
                ErrorCode::ArityMismatch,
                format!(
                    "`{}` takes no trailing block argument; write the lambda inline",
                    kind.name()
                ),
                span,
                "extra argument",
            );
            return self.empty_val();
        }
        let (recv, arg): (&Expr, &Expr) = match (fed, args) {
            (Some(recv), [only]) => (recv, &only.value),
            (None, [recv, arg]) => (&recv.value, &arg.value),
            _ => {
                self.error(
                    ErrorCode::ArityMismatch,
                    format!("`{}` takes the receiver and one argument", kind.name()),
                    span,
                    "wrong arity",
                );
                return self.empty_val();
            },
        };
        if args.iter().any(|arg| arg.name.is_some()) {
            self.error(
                ErrorCode::ArityMismatch,
                format!("`{}` takes positional arguments only", kind.name()),
                span,
                "named argument",
            );
            return self.empty_val();
        }
        match kind {
            Extraction::UnwrapOr | Extraction::UnwrapOrElse | Extraction::OkOr => {
                let recv_val = self.expr(recv, None);
                let total = recv_val.ty;
                let name = kind.name();
                let none = self.none_ty();
                if !self.decls.universe.contains(total, none) {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`{name}` extracts a `t | none` receiver; `{}` carries no `none` \
                             member [E4]",
                            self.decls.universe.render(total)
                        ),
                        span,
                        "receiver is not optional",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                }
                let extracted = self.decls.universe.diff(total, none);
                if self.decls.universe.is_empty(extracted) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!("`{name}` over `none` leaves nothing to extract [E4]"),
                        span,
                        "empty extraction",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                }
                match kind {
                    Extraction::UnwrapOr => {
                        let arg_val = self.expr(arg, None);
                        if !self.decls.universe.contains(extracted, arg_val.ty) {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "the fallback `{}` does not meet the extracted type `{}` [E4]",
                                    self.decls.universe.render(arg_val.ty),
                                    self.decls.universe.render(extracted)
                                ),
                                span,
                                "fallback mismatch",
                            );
                            return self.empty_val();
                        }
                        Val {
                            ty: extracted,
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::UnwrapOrElse => {
                        let Expr::Lambda { params, body, .. } = peel_paren(arg) else {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "`{name}` takes a `fn() -> t` lambda that inlines at the call \
                                     site"
                                ),
                                arg.span(),
                                "not a fallback lambda",
                            );
                            self.expr(arg, None);
                            return self.empty_val();
                        };
                        if !params.is_empty() {
                            self.error(
                                ErrorCode::ArityMismatch,
                                "the fallback lambda takes no parameters".to_owned(),
                                arg.span(),
                                "wrong lambda arity",
                            );
                            return self.empty_val();
                        }
                        let saved = self.snapshot();
                        let body_val = self.expr(body, Some(extracted));
                        self.restore(saved);
                        if !self.decls.universe.contains(extracted, body_val.ty) {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "the fallback lambda's `{}` does not meet the extracted type \
                                     `{}` [E4]",
                                    self.decls.universe.render(body_val.ty),
                                    self.decls.universe.render(extracted)
                                ),
                                span,
                                "fallback mismatch",
                            );
                            return self.empty_val();
                        }
                        Val {
                            ty: extracted,
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::OkOr => {
                        let arg_val = self.expr(arg, None);
                        Val {
                            ty: self.decls.universe.union(extracted, arg_val.ty),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::MapErr => unreachable!("the optional arm owns three of four"),
                }
            },
            Extraction::MapErr => {
                let Expr::Lambda {
                    params: handler_params,
                    body,
                    ..
                } = peel_paren(arg)
                else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        "`map_err` takes a one-parameter lambda `(e) => ...`".to_owned(),
                        arg.span(),
                        "not a handler lambda",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                };
                let [param] = handler_params.as_slice() else {
                    self.error(
                        ErrorCode::ArityMismatch,
                        "`map_err` takes a one-parameter lambda".to_owned(),
                        arg.span(),
                        "wrong lambda arity",
                    );
                    return self.empty_val();
                };
                let recv_val = self.expr(recv, None);
                let total = recv_val.ty;
                let annotated = param.ty.as_ref().and_then(|t| self.solve_here(t));
                let saved = self.snapshot();
                let param_ty = annotated.unwrap_or(total);
                self.bind_local(param.name.span, Binding {
                    ty: param_ty,
                    mutable: false,
                });
                let body_val = self.expr(body, None);
                self.restore(saved);
                let handled_set = if let Some(written) = annotated {
                    written
                } else {
                    let kept = self.decls.universe.inter(total, body_val.ty);
                    self.decls.universe.diff(total, kept)
                };
                let overlap = self.decls.universe.inter(total, handled_set);
                if self.decls.universe.is_empty(overlap) && !self.decls.universe.is_empty(total) {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`map_err` handles `{}`, which `{}` does not carry",
                            self.decls.universe.render(handled_set),
                            self.decls.universe.render(total)
                        ),
                        span,
                        "disjoint handler",
                    );
                    return self.empty_val();
                }
                let remainder = self.decls.universe.diff(total, handled_set);
                let result = self.decls.universe.union(remainder, body_val.ty);
                if self.decls.universe.is_empty(result) && !self.decls.universe.is_empty(total) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "`map_err` leaves nothing: the handler consumed all of `{}`",
                            self.decls.universe.render(total)
                        ),
                        span,
                        "empty remainder",
                    );
                    return self.empty_val();
                }
                Val {
                    ty: result,
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }
}
