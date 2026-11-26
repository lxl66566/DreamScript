//! Trait-dispatch evidence: which serving body answers a call, the chain
//! the frame stages for it, the member resolvers, and the staged evidence
//! words.

use super::*;

impl<'a> FnLower<'a, '_, '_> {
    /// Stages one generic call's evidence words: the receiver's
    /// builtin instance — one statically-resolved instance reference,
    /// an i64 pool constant — staged on the overlay after the real
    /// arguments, one word per entry of the callee's constraint list.
    /// The checker's recorded instantiation must agree with the
    /// constraint count; a receiver no covered instance names, or one
    /// outside the callee's implemented set, is a lowering error,
    /// never a runtime misdispatch. The minted instance comes back for
    /// the result shape's instance-keyed word kind. `defer` registers
    /// the staged types without emitting the Kimm stores — the tail
    /// transfer path re-stages every word past the exit claims, and a
    /// claim's overlay window would clobber words staged before it.
    // the inputs are the call's identity, the staging vec, and both
    // routing flags: one flat list where each parameter names a fact
    #[allow(clippy::too_many_arguments)]
    pub(in crate::func) fn stage_generic_evidence(
        &mut self,
        generic: &'static GenericNative,
        def: DefId,
        span: Span,
        call: Span,
        staged: &mut Vec<SlotType>,
        receiver_ty: Option<Ty>,
        defer: bool,
        member_routed: bool,
    ) -> Result<BuiltinInstance, LowerError> {
        let sig = self
            .analyzed
            .decls
            .fns
            .get(&def)
            .ok_or_else(|| mismatch("a generic callee signature"))?;
        // a member-routed native (`xs.get(i)`, `m.get(k)`) carries no
        // free-call instantiation record: the member's checked resolution
        // is the instantiation's authority, and only the declared arity
        // matters here — the instance word derives from the receiver's
        // own type below
        if member_routed {
            if sig.generics.len() != usize::from(generic.evidence) {
                return Err(LowerError::unsupported(
                    span,
                    "a generic call",
                    "the builtin's evidence arity disagrees with the declaration's generics",
                ));
            }
        } else {
            let instantiations = self.generic_call(call)?.ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    "a generic call",
                    "the checker recorded no full instantiation for it",
                )
            })?;
            if instantiations.len() != sig.generics.len()
                || instantiations.len() != usize::from(generic.evidence)
            {
                return Err(LowerError::unsupported(
                    span,
                    "a generic call",
                    "the builtin's evidence arity disagrees with the declaration's generics",
                ));
            }
        }
        let receiver = receiver_ty.ok_or_else(|| mismatch("a generic call's receiver"))?;
        let instance = self.shapes.builtin_instance(receiver).ok_or_else(|| {
            LowerError::unsupported(
                span,
                "a generic call",
                "no builtin instance covers the receiver's type; string, bytes, the list and map \
                 constructors, and the int and float scalars are the covered ones",
            )
        })?;
        if !generic.instances.contains(&instance) {
            return Err(LowerError::unsupported(
                span,
                "a generic call",
                "the builtin's one body implements no instance of the receiver's type",
            ));
        }
        self.guard_staged_count(staged.len() + usize::from(generic.evidence))?;
        for _ in 0..generic.evidence {
            if !defer {
                let kidx = self.pool(Constant::I64(instance as i64));
                self.emit(Op::Kimm {
                    dst: Slot::overlay(u8::try_from(staged.len()).expect("overlay word fits u8")),
                    kidx,
                });
            }
            staged.push(SlotType::I64);
        }
        Ok(instance)
    }

    /// A method-call site's solved dispatch: the impl body's
    /// function-table index plus the instance id the checker recorded at
    /// the call's own span. An inherent survivor resolves through its
    /// own record below, and a site satisfied only through an in-scope
    /// assumption records no instance — the latter refuses naming the
    /// seam it waits on.
    pub(in crate::func) fn resolve_method_dispatch(
        &self,
        call: Span,
        what: Span,
        recv: Option<&Expr>,
        candidates: &[MethodId],
    ) -> Result<TraitDispatch, LowerError> {
        let inherent_only = candidates.iter().all(|&method| {
            matches!(
                self.analyzed.package.method(method).owner,
                MethodOwner::Inherent(_)
            )
        }) && !candidates.is_empty();
        if let Some(&method) = self.analyzed.checked.inherent_calls.get(&call) {
            return self.resolve_inherent_method(method, call, what);
        }
        let Some(tc) = self.trait_call(call)? else {
            // A bound-variable receiver inside a generic body: the
            // checker satisfied the call through the body's own bound
            // (an in-scope assumption), so no instance records here —
            // dispatch resolves at runtime through the evidence slot the
            // call site entering this body staged. The decision reads the
            // body's written type, not the lensed one: an instance serves
            // the same runtime evidence word whatever the site solved,
            // and a statically-resolved dispatch would need an instance
            // record the body's own checking never minted.
            if let Some(recv) = recv
                && let Ok(val) = self.written_val(recv.span())
                && let [var] = self.members(val.ty)
                && self.shapes.is_var_atom(*var)
                && let Some(param) = crate::shape::var_param_name(self.universe(), *var)
            {
                let matching: Vec<(MethodId, u8)> = self
                    .evidence
                    .iter()
                    .filter(|((pair_var, method), _)| {
                        pair_var == &param && candidates.contains(method)
                    })
                    .map(|((_, method), slot)| (*method, *slot))
                    .collect();
                return match matching.as_slice() {
                    [(method, slot)] => Ok(TraitDispatch::Runtime {
                        slot: *slot,
                        method: *method,
                    }),
                    [] => Err(LowerError::unsupported(
                        what,
                        "a trait-method call on a bound type variable",
                        "this body carries no runtime evidence slot for the member (the body \
                         demands no such pair, or the frame is outside the evidence seam); the \
                         seam covers generic bodies, their lambdas, and their spawned children",
                    )),
                    _ => Err(LowerError::unsupported(
                        what,
                        "a trait-method call on a bound type variable",
                        "the member name is ambiguous across the variable's bounds; qualify the \
                         trait",
                    )),
                };
            }
            return Err(LowerError::unsupported(
                what,
                if inherent_only { "an inherent method call" } else { "a trait-method call" },
                if inherent_only {
                    "the checker recorded no solved inherent dispatch for this site; report the \
                     construct"
                } else {
                    "the checker recorded no solved instance for this site (a bound-type receiver \
                     outside its generic impl body)"
                },
            ));
        };
        let member = self.analyzed.package.method(tc.method).name.clone();
        // a linked impl owns no impl item in this compile: the solved
        // instance's member links address the linked module directly,
        // the same transfer a linked free fn's call takes. An impl
        // spelling no member falls through — the trait's declared
        // default serves it, exactly like a local omission's site
        if let Some(link) = self.analyzed.decls.linked_impls.get(&tc.instance)
            && let Some(func) = link.member(&member)
        {
            if matches!(self.comptime, ComptimeRun::Slice) {
                return Err(Self::comptime_member_refusal(what));
            }
            return Ok(TraitDispatch::Foreign {
                position: link.position,
                func,
                call: tc,
                default_body: false,
            });
        }
        if let Some(impl_site) = tc.impl_site {
            if let Some(index) = self.plan.impl_body(impl_site, &member) {
                return Ok(TraitDispatch::Static { index, call: tc });
            }
            // A defaulted member is omittable exactly because the
            // trait's declaration carries the dispatch target: an impl
            // spelling no member dispatches into the declared default
            // body, the same staged shape (evidence demands trailing
            // the true arguments, the solved instance word trailing
            // them).
            if let MethodOwner::Trait(def) = self.analyzed.package.method(tc.method).owner
                && let Some(index) = self.plan.default_body(def, &member)
            {
                return Ok(TraitDispatch::Static { index, call: tc });
            }
        }
        // The linked trait's declared default body: the linked mirror
        // of the plan's own table above, serving a solving impl that
        // spells no member — a linked impl owns no impl item here, and
        // a local impl of the minted trait plans no default of its
        // own, so this link is the only remaining dispatch target
        if let MethodOwner::Trait(def) = self.analyzed.package.method(tc.method).owner
            && let Some(link) = self.analyzed.decls.linked_trait_defaults.get(&def)
            && let Some(func) = link.member(&member)
        {
            if matches!(self.comptime, ComptimeRun::Slice) {
                return Err(Self::comptime_member_refusal(what));
            }
            return Ok(TraitDispatch::Foreign {
                position: link.position,
                func,
                call: tc,
                default_body: true,
            });
        }
        Err(LowerError::unsupported(
            what,
            "a trait-method call",
            "the solving impl's body did not plan (no body, no solved return, or a shape over its \
             own type variables), and the trait's declared default body is not linked here; \
             report the construct",
        ))
    }

    /// The comptime slice's linked member-call refusal: the slice plans
    /// nothing outside its own module, so a linked body — an impl
    /// member's or a trait's declared default — keeps the local member
    /// call's own door.
    pub(in crate::func) fn comptime_member_refusal(what: Span) -> LowerError {
        LowerError::unsupported(
            what,
            "a call from a comptime function",
            "a comptime function may call the pure builtins and other comptime functions this \
             round; a runtime helper is the slice seam",
        )
    }

    /// A solved inherent-method call site: the impl body the
    /// checker-recorded method id names, reached through the declaration
    /// tables' method-to-impl-item map. The receiver's own type picked
    /// the body at check time — no instance word exists for an inherent
    /// impl, so the call dispatches like a local callee; a generic body's
    /// entering site carries the checker-recorded bounds (the serving
    /// instance per declared bound under the receiver's substitution),
    /// which stage the body's evidence demands.
    /// The native seam a bodiless ambient collection member routes to:
    /// `list`'s `get`/`push`/`concat` and `map`'s `get`/`set` declare no
    /// ds body
    /// (an element-carrying generic member body has no static word kind),
    /// so the call lowers exactly like the native's own retired free
    /// spelling — the receiver feeding the first argument. `window`
    /// marks the in-place mutations whose member answer re-answers the
    /// receiver after the native's unit word. A member with a ds body
    /// (the `keys` shape, or a user impl's own body) lowers that body.
    pub(in crate::func) fn native_member_def(&self, call: Span) -> Option<NativeMember> {
        let &method = self.analyzed.checked.inherent_calls.get(&call)?;
        let MethodOwner::Inherent(target) = &self.analyzed.package.method(method).owner else {
            return None;
        };
        let member = self.analyzed.package.method(method).name.as_str();
        let (native, window) = match (target.as_str(), member) {
            ("list", "get") => ("list_get", false),
            ("list", "contains") => ("list_contains", false),
            ("list", "set") => ("list_set", true),
            ("list", "push") => ("list_push", true),
            ("list", "insert") => ("list_insert", true),
            ("list", "remove") => ("list_remove", true),
            ("list", "clear") => ("list_clear", true),
            ("list", "concat") => ("list_concat", false),
            ("map", "get") => ("map_get", false),
            ("map", "set") => ("map_set", true),
            ("map", "remove") => ("map_remove", true),
            ("map", "contains") => ("map_contains", false),
            ("map", "entries") => ("map_entries", false),
            ("chan", "send") => ("chan_send", false),
            ("chan", "recv") => ("chan_recv", false),
            ("chan", "send_deadline") => ("chan_send_deadline", false),
            ("chan", "recv_deadline") => ("chan_recv_deadline", false),
            ("arena", "insert") => ("arena_insert", false),
            ("arena", "remove") => ("arena_remove", false),
            ("arena", "at") => ("arena_at", false),
            _ => return None,
        };
        let &impl_site = self.analyzed.decls.inherent_impls.get(&method)?;
        if self.plan.impl_body(impl_site, member).is_some() {
            return None;
        }
        let def = self.analyzed.package.lookup_value(native)?;
        Some(NativeMember { def, window })
    }

    pub(in crate::func) fn resolve_inherent_method(
        &self,
        method: MethodId,
        call: Span,
        what: Span,
    ) -> Result<TraitDispatch, LowerError> {
        // A linked member body addresses the linked module directly
        // (the type-keyed receiver seam): the method id's member link
        // names the dep position and the table index, the same transfer
        // a linked free fn's call takes
        if let Some(link) = self.analyzed.decls.linked_inherent.get(&method) {
            if matches!(self.comptime, ComptimeRun::Slice) {
                return Err(Self::comptime_member_refusal(what));
            }
            return Ok(TraitDispatch::InherentForeign {
                position: link.position,
                func: link.func,
                method,
            });
        }
        let Some(&impl_site) = self.analyzed.decls.inherent_impls.get(&method) else {
            return Err(LowerError::unsupported(
                what,
                "an inherent method call",
                "the impl item owning the method is missing from the declaration tables; report \
                 the construct",
            ));
        };
        let member = self.analyzed.package.method(method).name.clone();
        let Some(index) = self.plan.impl_body(impl_site, &member) else {
            return Err(LowerError::unsupported(
                what,
                "an inherent method call",
                "the impl's body did not plan (no body, no solved return, or a shape over its own \
                 type variables); report the construct",
            ));
        };
        let bounds = self
            .analyzed
            .checked
            .inherent_call_bounds
            .get(&call)
            .cloned()
            .unwrap_or_default();
        Ok(TraitDispatch::Inherent {
            index,
            bounds,
            method,
        })
    }

    /// The trait one evidence pair's member belongs to: the walk's key
    /// into the checker's per-site bound records.
    pub(in crate::func) fn pair_trait_id(
        &self,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<TraitId, LowerError> {
        let method = self.analyzed.package.method(pair.method);
        let MethodOwner::Trait(def) = method.owner else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the body's member call did not resolve to a trait member; report the construct",
            ));
        };
        self.analyzed
            .decls
            .traits
            .get(&def)
            .copied()
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    what,
                    "the member's trait is missing from the declaration tables",
                )
            })
    }

    /// The runtime form's evidence word for one pair: this site's
    /// substitution is its own frame's, so the receiver's single
    /// variable peels to the frame's written parameter name and the
    /// pair's slot word — the chain object's index — reads verbatim
    /// from the frame's evidence slots.
    pub(in crate::func) fn slot_evidence(
        &self,
        recv: Ty,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let universe = self.universe();
        let [only] = universe.members(recv) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the receiver unions members across branches; per-branch runtime evidence has no \
                 single serving body",
            ));
        };
        let Some(param) = peeled_param(universe, *only) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the receiver's type carries no type variable of this body, so the demand keys no \
                 frame evidence slot",
            ));
        };
        let Some(&slot) = self.evidence.get(&(param, pair.method)) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "this frame carries no runtime-evidence slot for the member (a demand the walk \
                 does not collect); the evidence seam covers generic bodies, their lambdas, and \
                 their spawned children",
            ));
        };
        Ok(vec![EvidenceSrc::Slot(slot)])
    }

    /// The evidence-chain object one statically-resolved demand resolves
    /// to: the serving member body's function-table index for
    /// `method` over the substitution `start` names, plus one tail chain
    /// per the serving body's own evidence demands — each demand's own
    /// member and substituted type, walked recursively. The staged word
    /// a site enters this chain through carries the object's table
    /// index; a nested runtime dispatch reads its frame's slot word,
    /// resolves the object, dispatches into `func`, and stages the tail
    /// objects' indices as the body's trailing evidence parameters — so
    /// depth is table-bounded, never frame-bounded, and a body may
    /// demand several or different members (one tail chain each).
    ///
    /// The walk memoizes its states (member, instance, substitution): a
    /// repeated state shares its entry, which is also the termination
    /// story — a cyclic demand graph builds a cyclic table the
    /// dispatches walk lazily, one hop per call.
    pub(in crate::func) fn chain_for(
        &mut self,
        method: MethodId,
        instance: InstanceId,
        start: Option<&[TypeId]>,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        let universe = self.universe();
        let env = &self.analyzed.decls.instances;
        // the one-word instantiation boundary: this level's serving body
        // is entered by a nested dispatch that stages the bound
        // variable's single runtime word, so a substitution spanning more
        // must refuse at this entering site, where the type is concrete
        if let Some(members) = start {
            self.guard_one_word_instantiation(members, what, span)?;
        }
        let key = (method, instance, start.map(<[TypeId]>::to_vec));
        if let Some(&index) = self.plan.chain_memo.get(&key) {
            return Ok(index);
        }
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
        let func = self.serving_body(impl_site, method, what, span)?;
        // the chain is a baked route to the serving body under this
        // instance: the internal-exercise fact the link layering reads
        self.plan.mark_exercised(instance.index());
        // reserve the entry before the recursion so a cyclic demand
        // graph closes over it
        let index = u32::try_from(self.plan.chains.len()).expect("chain table fits u32");
        self.plan.chains.push(ds_mod::EvidenceChain {
            func,
            next: Vec::new(),
        });
        self.plan.chain_memo.insert(key, index);
        let pairs = self.plan.functions()[func as usize].evidence.clone();
        let mut next = Vec::with_capacity(pairs.len());
        if !pairs.is_empty() {
            if self.plan.is_default_body(func) {
                // the serving body is a trait's declared default: every
                // demand keys by the trait's own variables and resolves
                // under the substitution this level entered with — a
                // self-recursive demand closes onto this very entry
                // through the memo, so a cyclic demand graph still
                // builds a cyclic table
                for pair in pairs.iter() {
                    next.push(self.default_pair_chain(method, instance, start, pair, what, span)?);
                }
            } else {
                let Some(members) = start else {
                    return Err(LowerError::unsupported(
                        span,
                        what,
                        "the transitive serving body's own bound is not resolvable at this site; \
                         report the construct",
                    ));
                };
                for pair in pairs.iter() {
                    let pair_trait = self.pair_trait_id(pair, what, span)?;
                    // the serving body's own demand variable names the
                    // next bound: the serving impl declares it, so its
                    // conditions key by that written name
                    let Some((next_members, Some(next_instance))) =
                        ds_check::instance_solve::evidence_step(
                            universe,
                            env,
                            instance,
                            members,
                            Some(pair.var.as_str()),
                            pair_trait,
                        )
                    else {
                        return Err(LowerError::unsupported(
                            span,
                            what,
                            "the element type's transitive bound has no unique serving instance \
                             at this site; runtime evidence resolves one body per bound",
                        ));
                    };
                    // the same boundary one level down: the next link's
                    // serving body receives this substitution as its
                    // receiver
                    self.guard_one_word_instantiation(&next_members, what, span)?;
                    next.push(self.chain_for(
                        pair.method,
                        next_instance,
                        Some(&next_members),
                        what,
                        span,
                    )?);
                }
            }
        }
        self.plan.chains[index as usize].next = next;
        Ok(index)
    }

    /// One demand pair's chain inside a trait's declared default body,
    /// over the member whose default body serves this level, the
    /// instance whose impl enters it, and the receiver member set the
    /// body runs with: a `Self`-keyed pair re-serves the receiver under
    /// the same instance (the entry `chain_for` builds for the called
    /// member — the memo closes a self-recursive demand onto its own
    /// entry), while a pair over the trait's own generic variable
    /// resolves through the argument the impl's trait reference spells
    /// for it: a concrete argument names the substituted type itself,
    /// an impl-variable argument names a variable this receiver binds,
    /// the impl's own declared bound on it carrying the next level
    /// exactly as the generic-impl branch's step.
    pub(in crate::func) fn default_pair_chain(
        &mut self,
        method: MethodId,
        instance: InstanceId,
        start: Option<&[TypeId]>,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        // the receiver stages as the default body's `Self` variable's
        // single Raw word, so the one-word instantiation boundary guards
        // it here (the tail level re-guards what its own chain_for entry
        // already checked)
        if let Some(members) = start {
            self.guard_one_word_instantiation(members, what, span)?;
        }
        if pair.var == "Self" {
            return self.chain_for(pair.method, instance, start, what, span);
        }
        let universe = self.universe();
        let env = &self.analyzed.decls.instances;
        let trait_id = self.pair_trait_id(pair, what, span)?;
        // the default body's trait, and the argument its impl substitutes
        // for the demanded generic (positional over the declaration's
        // own generics)
        let MethodOwner::Trait(def) = self.analyzed.package.method(method).owner else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the declared default body's member did not resolve to a trait; report the \
                 construct",
            ));
        };
        let generics = self.analyzed.decls.trait_generics.get(&def);
        let impl_site = self.analyzed.decls.instance_impls.get(&instance).copied();
        let arg = generics
            .and_then(|names| names.iter().position(|name| name == &pair.var))
            .zip(impl_site.and_then(|site| self.analyzed.decls.trait_impl_args.get(&site)))
            .and_then(|(position, args)| args.get(position).copied());
        let Some(arg) = arg else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the trait variable's substitution is missing from the impl's trait reference at \
                 this site; report the construct",
            ));
        };
        let members = universe.members(arg);
        // the argument is the impl's own variable: this receiver binds
        // it, and the impl's declared bound on it carries the next level
        if let [only] = members
            && let Some(var) = crate::shape::var_param_name(universe, *only)
        {
            let Some(recv) = start else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the trait variable maps to the impl's own variable, whose substitution this \
                     site does not carry; report the construct",
                ));
            };
            let Some((next_members, Some(next_instance))) = ds_check::instance_solve::evidence_step(
                universe,
                env,
                instance,
                recv,
                Some(var.as_str()),
                trait_id,
            ) else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the impl's own bound on the trait variable's substitution has no unique \
                     serving instance at this site; runtime evidence resolves one body per bound",
                ));
            };
            // the same boundary one level down: the next link's serving
            // body receives this substitution as its receiver
            self.guard_one_word_instantiation(&next_members, what, span)?;
            return self.chain_for(pair.method, next_instance, Some(&next_members), what, span);
        }
        // the argument is concrete: it names the substituted type itself
        if !members
            .iter()
            .any(|&atom| carries_a_var(universe, atom, None))
        {
            let Some(next_instance) =
                ds_check::instance_solve::resolve(universe, env, &[], trait_id, arg, None).solved()
            else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the trait variable's substituted type has no serving instance at this site; \
                     runtime evidence resolves one body per bound",
                ));
            };
            self.guard_one_word_instantiation(members, what, span)?;
            return self.chain_for(pair.method, next_instance, Some(members), what, span);
        }
        Err(LowerError::unsupported(
            span,
            what,
            "the trait variable substitutes to a compound type over the impl's variables, a shape \
             this seam resolves per variable alone; report the construct",
        ))
    }

    /// The dispatch target one solved instance names for one member: the
    /// impl's spelled body, or — when the impl spells no member — the
    /// trait's declared default body (the member is omittable exactly
    /// because the declaration carries the dispatch target).
    pub(in crate::func) fn serving_body(
        &self,
        impl_site: Span,
        method: MethodId,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        let method = self.analyzed.package.method(method);
        if let Some(index) = self.plan.impl_body(impl_site, &method.name) {
            return Ok(index);
        }
        if let MethodOwner::Trait(def) = method.owner
            && let Some(index) = self.plan.default_body(def, &method.name)
        {
            return Ok(index);
        }
        Err(LowerError::unsupported(
            span,
            what,
            "the element type's serving impl body did not plan; report the construct",
        ))
    }

    /// Whether one decomposed branch instance's head mentions a minted
    /// variable of the written name anywhere in its shape: per-branch
    /// solving keeps a branch's condition only for the variables its
    /// own head mentions, so a demand keyed by a name the entering
    /// branch never carries is unreachable in it — the narrowing the
    /// demanding code sits under cannot hold there. A missing head
    /// consults nothing and keeps the honest refusal.
    pub(in crate::func) fn branch_mentions_var(&self, instance: InstanceId, want: &str) -> bool {
        let Ok(head) = self.analyzed.decls.instances.head(instance) else {
            return true;
        };
        let universe = self.universe();
        universe
            .members(head)
            .iter()
            .any(|&atom| carries_a_var(universe, atom, Some(want)))
    }

    /// The staged evidence words a call site entering a generic impl body
    /// stages for one of the body's demands: the checker-recorded bounds
    /// of this call site's solved dispatch answer the pair's bound — a
    /// concrete answer walks the serving chain (the chain start
    /// substituting the entered instance's variable with the receiver
    /// this site hands the body, read through the lens), and a `None`
    /// answer (the site sits inside a generic body whose substitution is
    /// that frame's own) reads the frame's evidence slots, keyed by the
    /// written receiver the record names — the slot the entering site
    /// staged into this frame, the same word under an instantiation.
    pub(in crate::func) fn evidence_words(
        &mut self,
        pair: &RuntimePair,
        bounds: &InherentCallBounds,
        site: EvidenceSite,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let EvidenceSite {
            entered,
            inherent_site,
            recv,
            written_recv,
        } = site;
        let trait_id = self.pair_trait_id(pair, what, span)?;
        let universe = self.universe();
        let env = &self.analyzed.decls.instances;
        // the recorded bounds key by the declaration's own minted variable;
        // the written parameter name bridges the two mintings
        let entry = bounds.iter().find(|(var, bound, _)| {
            crate::shape::var_param_name(universe, *var).as_deref() == Some(pair.var.as_str())
                && *bound == trait_id
        });
        let Some((_, _, served)) = entry else {
            // a demand whose variable the entering branch's head never
            // mentions: per-branch solving kept no condition for it, so
            // the demanding code sits under a narrowing that branch
            // cannot satisfy — the site stages the dead sentinel, a
            // chain word naming no object that a dispatch reads only if
            // the unreachable narrowing ever ran, trapping loudly there.
            // A union receiver (branches solving the variable
            // differently) and a head that does mention the variable
            // keep the honest refusal below.
            if let Some(instance) = entered
                && !self.branch_mentions_var(instance, &pair.var)
                && recv.is_some_and(|recv| self.members(recv).len() == 1)
            {
                return Ok(vec![EvidenceSrc::Dead]);
            }
            return Err(LowerError::unsupported(
                span,
                what,
                "the element type's bound has no unique serving instance at this site (or the \
                 receiver is a union across branches); runtime evidence resolves one body per \
                 bound",
            ));
        };
        let Some(instance) = served else {
            // the runtime form: this site's substitution is its own frame's
            let Some(recv) = written_recv else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the site's receiver type is missing; report the construct",
                ));
            };
            return self.slot_evidence(recv, pair, what, span);
        };
        let instance = *instance;
        // the chain start: the entered instance's own bound on this
        // pair's variable, substituted with the receiver this site
        // entered the body with
        let start = match (entered, inherent_site) {
            (Some(entered), _) => recv.and_then(|recv| {
                Self::chain_start(universe, env, entered, recv, &pair.var, trait_id)
            }),
            (None, Some(site)) => recv.and_then(|recv| {
                self.analyzed
                    .decls
                    .inherent_impl_bounds
                    .get(&site)
                    .and_then(|record| {
                        ds_check::instance_solve::inherent_evidence_step(
                            universe,
                            env,
                            record.head,
                            &record.conditions,
                            universe.members(recv),
                            Some(pair.var.as_str()),
                            trait_id,
                        )
                    })
                    .map(|(target, _)| target)
            }),
            (None, None) => None,
        }
        .map(Cow::Owned);
        Ok(vec![EvidenceSrc::Const(self.chain_for(
            pair.method,
            instance,
            start.as_deref(),
            what,
            span,
        )?)])
    }

    /// The staged evidence words a call into a free generic fn's body
    /// stages for one of the body's demands: the checker's
    /// per-call record answers the pair's bound under this site's own
    /// substitution — a concrete answer stages the serving chain
    /// object, and a `None` answer (the substitution is the calling
    /// frame's own variable) reads the frame's evidence area for the
    /// pair, keyed by the written variable the record names (an
    /// instantiation stages the same slot the entering site staged into
    /// the frame's trailing evidence parameters, so the written form —
    /// never the lensed one — keys it).
    pub(in crate::func) fn free_evidence_words(
        &mut self,
        pair: &RuntimePair,
        record: &GenericCallBounds,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let trait_id = self.pair_trait_id(pair, what, span)?;
        let universe = self.universe();
        let Some((_, _, served, solved)) = record.iter().find(|(var, bound, ..)| {
            crate::shape::var_param_name(universe, *var).as_deref() == Some(pair.var.as_str())
                && *bound == trait_id
        }) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the call site recorded no bound answer for the parameter; report the construct",
            ));
        };
        let Some(instance) = served else {
            return self.slot_evidence(*solved, pair, what, span);
        };
        let instance = *instance;
        // the chain start is the solved type itself under this frame's
        // substitution (a concrete site's solved type names nothing to
        // substitute): the demand's variable takes it at this site, and
        // the first tail chain serves the body the serving instance's
        // own head re-substitutes it into
        let solved = self
            .shapes
            .subst(*solved)
            .map_err(|err| err.respanned(span))?;
        let start = Some(Cow::Borrowed(self.members(solved)));
        Ok(vec![EvidenceSrc::Const(self.chain_for(
            pair.method,
            instance,
            start.as_deref(),
            what,
            span,
        )?)])
    }

    /// The one-hop chain-start substitution: the instance's declared
    /// bound on the pair's variable, solved for the receiver this site
    /// hands the body.
    pub(in crate::func) fn chain_start(
        universe: &ds_check::Universe,
        env: &ds_check::instance::InstanceEnv,
        instance: InstanceId,
        recv: Ty,
        var: &str,
        trait_id: TraitId,
    ) -> Option<Vec<TypeId>> {
        ds_check::instance_solve::evidence_step(
            universe,
            env,
            instance,
            universe.members(recv),
            Some(var),
            trait_id,
        )
        .map(|(target, _)| target)
    }

    /// Emits one dispatch site's evidence words into the overlay span at
    /// the recorded staged index: a constant word per Kimm store, a frame
    /// word per Mov — neither counted, neither a binder.
    pub(in crate::func) fn stage_member_evidence(&mut self, words: &[EvidenceSrc], at: usize) {
        for (offset, word) in words.iter().enumerate() {
            let dst = Slot::overlay(u8::try_from(at + offset).expect("overlay word fits u8"));
            match word {
                EvidenceSrc::Const(index) => {
                    let kidx = self.pool(Constant::I64(i64::from(*index)));
                    self.emit(Op::Kimm { dst, kidx });
                },
                EvidenceSrc::Slot(slot) => {
                    self.emit(Op::Mov {
                        dst,
                        src: Slot::body(*slot),
                    });
                },
                EvidenceSrc::Dead => {
                    let kidx = self.pool(Constant::I64(DEAD_CHAIN));
                    self.emit(Op::Kimm { dst, kidx });
                },
            }
        }
    }

    pub(in crate::func) fn resolve_callee(
        &self,
        def: DefId,
        span: Span,
        call_span: Span,
    ) -> Result<Callee, LowerError> {
        // A declared free generic fn answers its entering site first: an
        // instance over the site's solved argument types (the same key
        // the plan's discovery planned by), the ONE Raw-word body where
        // the site's vector still names a variable (a call from inside a
        // body that itself lowers over its variables). Neither found is
        // the walk and the discovery disagreeing on the site — refused
        // here, because a walk-time plan would break the served splice.
        if self.plan.generic_fn(def).is_some() {
            let solved = self.generic_call(call_span)?.unwrap_or_default();
            let concrete = !solved.is_empty()
                && solved
                    .iter()
                    .all(|ty| !crate::types::has_variables(self.universe(), *ty));
            let target = if concrete {
                self.plan.instance_target(def, &solved)
            } else {
                self.plan.index_of(def)
            };
            return match target {
                Some(index) => Ok(Callee::Local(index)),
                None => Err(LowerError::unsupported(
                    span,
                    "a call into a free generic fn",
                    "the plan's discovery planned no body for this site; the walk and the \
                     discovery disagree — report the construct",
                )),
            };
        }
        if let Some(index) = self.plan.index_of(def) {
            return Ok(Callee::Local(index));
        }
        // an import-bound def is a linked package's function: the
        // check comes ahead of the native fallback so a linked function
        // never silently answers as a same-named native
        if let Some(link) = self.analyzed.decls.foreign_links.get(&def) {
            return Ok(Callee::Foreign {
                def: Some(def),
                position: link.position,
                func: link.func,
            });
        }
        // a bound extern's declaration names an artifact: the call
        // resolves through the graph's fold — the artifact's exported
        // symbols in enumeration order, the same table the run installs
        // as the artifact's native module — ahead of the compiler-wired
        // native fallback, so a same-named builtin never shadows a
        // declared bind
        if let Some(bind) = self
            .analyzed
            .decls
            .fns
            .get(&def)
            .and_then(|sig| sig.bind.clone())
        {
            return self.bound_callee(def, &bind, span);
        }
        let name = self.analyzed.package.def(def).name.clone();
        if let Some(native) = crate::native::lookup(&name) {
            return Ok(Callee::Builtin(native));
        }
        if let Some(sig) = self.analyzed.decls.fns.get(&def)
            && !sig.generics.is_empty()
        {
            if let Some(generic) = crate::native::lookup_generic(&name) {
                // a fixed-staged descriptor stages exactly one word per
                // declared parameter, so a signature carrying fewer
                // parameters than the contract stages names a seam the
                // free-generic path cannot lower: the ord-routed sort's
                // spelling carries no comparator, its callable staged
                // from the solved ord instance at the ambient's own call
                // sites alone
                if generic.staging == Staging::Fixed && generic.params.len() != sig.params.len() {
                    return Err(LowerError::unsupported(
                        span,
                        "a generic callee",
                        "the builtin's fixed staging covers more words than the spelling's \
                         parameters carry; the comparator-routed sort lowers at the ambient's own \
                         call sites alone, and a module-local declaration of the name has no \
                         lowering",
                    ));
                }
                return Ok(Callee::Generic(generic));
            }
            return Err(LowerError::unsupported(
                span,
                "a generic callee",
                "the generic body did not plan (an extern, `var`-bound, or bodiless generic fn \
                 lowers nothing), so no dispatch reaches it",
            ));
        }
        // the comptime slice carries only comptime functions: a call
        // into the package's runtime half refuses here, naming the seam —
        // a linked package's body included, the slice planning nothing
        // outside its own module
        if matches!(self.comptime, ComptimeRun::Slice) {
            return Err(LowerError::unsupported(
                span,
                "a call from a comptime function",
                "a comptime function may call the pure builtins and other comptime functions this \
                 round; a runtime helper is the slice seam",
            ));
        }
        Err(LowerError::unsupported(
            span,
            "a callee",
            "extern non-builtin and method callees are outside the contract",
        ))
    }

    /// Resolves one bound extern's declaration: the bind target
    /// names an artifact the graph resolved, the extern's name a symbol
    /// its installed library exports — the FuncRef addressing the
    /// artifact's native module at the symbol's enumeration index. The
    /// signature must be the marshaling slice's own shape (the scalar
    /// words, a `string`/`bytes` parameter's call-scoped pair, and the
    /// absent word a unit return rides, at most [`ds_ffi::MAX_PARAMS`]
    /// C words with a pair counting two); anything richer refuses here
    /// naming the seam, before any run starts.
    pub(in crate::func) fn bound_callee(
        &self,
        def: DefId,
        bind: &str,
        span: Span,
    ) -> Result<Callee, LowerError> {
        let sig = self
            .analyzed
            .decls
            .fns
            .get(&def)
            .expect("a bound callee reads the signature that named its target");
        let name = self.analyzed.package.def(def).name.clone();
        let refuse = |reason: String| LowerError::ForeignBind {
            span,
            name: name.clone(),
            reason,
        };
        let Some(symbols) = self.analyzed.artifacts.get(bind) else {
            return Err(refuse(format!(
                "the resolved graph declares no [artifacts] entry `{bind}`; declare the library \
                 in ds.pkg, or run the source as a package — externs bind only package-pinned \
                 libraries"
            )));
        };
        let Some(index) = symbols.iter().position(|symbol| *symbol == name) else {
            return Err(refuse(format!(
                "artifact `{bind}` exports no `{name}`; `ds bind` reports the exported spellings"
            )));
        };
        if !sig.generics.is_empty() {
            return Err(refuse(
                "a C symbol has no generic body; the signature must be monomorphic".to_owned(),
            ));
        }
        // the cap counts C words, not parameters: a string/bytes
        // parameter's (pointer, length) pair occupies two, so two pairs
        // fill the enumerated transmute's envelope exactly
        let mut words = 0usize;
        for param in &sig.params {
            let Some(ty) = &param.ty else {
                return Err(refuse(
                    "an inferred parameter has no written marshaling word; write the int, bool, \
                     float, string, or bytes annotation"
                        .to_owned(),
                ));
            };
            match marshal_word(ty) {
                Some(word) => words += word,
                None => {
                    return Err(refuse(
                        "the C-ABI marshaling slice rules int, bool, float, string, and bytes \
                         parameters; handle and aggregate marshaling is the ownership round's own"
                            .to_owned(),
                    ));
                },
            }
        }
        if words > ds_ffi::MAX_PARAMS {
            return Err(refuse(format!(
                "the C-ABI marshaling slice serves at most {} C words, a string or bytes \
                 parameter's pair occupying two",
                ds_ffi::MAX_PARAMS
            )));
        }
        // a unit return rides the absent word; the returning direction of
        // string/bytes marshaling needs an ownership story the round
        // keeps queued
        if let Some(ret) = &sig.ret
            && scalar_word(ret).is_none()
        {
            return Err(refuse(
                "the C-ABI marshaling slice answers one scalar word or none (a unit return rides \
                 the absent word); a string or bytes return is the ownership round's own"
                    .to_owned(),
            ));
        }
        Ok(Callee::Bound {
            def,
            index: u32::try_from(index).expect("the export enumeration fits u32"),
        })
    }
}
