//! Per-shape instantiation of generic impl members: the plan
//! registration, the ONE Raw-word body, the shape-keyed instances, and
//! the discovery hooks that plan them.
//!
//! A generic impl's member body holds no table entry of its own. Every
//! call site entering it plans the body it needs before anything
//! lowers: an instance over the types the site's receiver solved for
//! the impl's head pattern, or the ONE Raw-word body where the site
//! still names a variable (a call from inside a body that lowers over
//! its variables' Raw words). A member whose runtime-evidence demands
//! some other body serves also keeps its ONE body planned at its own
//! declaration — the runtime form dispatches through evidence chains
//! whose objects name table entries, and the chain's entering site may
//! sit far from the member's own item, so the body plans where the
//! declaration guarantees it: the plan stays a pure function of the
//! trees, which is what the served splice pins.

use super::*;

/// The site key a member instance's entering sites look their target up
/// by: the impl item, the member, the receiver type, and the solved
/// clause's tail segment — the same handles the discovery planned the
/// instance over.
pub(crate) type MemberSiteKey = (Span, String, Ty, Rc<Vec<Ty>>);

/// One member body of a generic impl (a trait impl's or an inherent
/// one's): the body the entering sites instantiate.
pub(crate) struct GenericMember {
    /// The mangled ONE-body table name `<trait>#<target>#<member>`,
    /// plus the vocabulary `#name` of a named impl.
    pub(crate) name: String,
    /// The impl item's span: the identity the dispatch records key.
    pub(crate) impl_site: Span,
    /// The member's name.
    pub(crate) member: String,
    /// The impl's declared generic parameter names, in declaration
    /// order — the argument order instance keys and site lookups share.
    pub(crate) generics: Vec<String>,
    /// The member fn's own generic clause names, in declaration order:
    /// the variables the entering sites solve from the arguments behind
    /// the receiver, appended behind the head arguments at the instance
    /// key. A trait's declared default body declares none.
    pub(crate) clause: Vec<String>,
    /// The member's declared parameters, the `self` receiver among
    /// them: the body's declared shape.
    pub(crate) params: Rc<Vec<Param>>,
    /// The body block wrapped as the block expression the tail walk
    /// lowers.
    pub(crate) body: Rc<Expr>,
    /// The member declaration's own span: the key of its solved return.
    pub(crate) decl_span: Span,
    /// The body's runtime-evidence demands after the package-wide
    /// propagation, the same list for the ONE body and every instance.
    pub(crate) pairs: Rc<Vec<RuntimePair>>,
    /// The impl's declared bounds, by parameter name.
    pub(crate) bounds: Rc<HashMap<String, HashSet<TraitId>>>,
    /// The trait's definition when this member is a trait's declared
    /// default body (the per-shape story runs over the trait's `Self`
    /// variable and the trait's own generics): `None` for an impl's
    /// member.
    pub(crate) default_of: Option<DefId>,
}

/// Every type the checker recorded for one member body, in a fixed
/// order: the declared parameters' binder facts, the solved return,
/// then every expression type, binder type, and call-site record inside
/// the body's block span. The discovery substitutes exactly these, and
/// the instance key reads exactly these — a type the walk reads and
/// this list misses refuses at the walk instead of lowering a
/// variable's word.
fn member_body_types(analyzed: &Analyzed, generic: &GenericMember) -> Vec<Ty> {
    let span = generic.body.span();
    let inside = |at: Span| at.file == span.file && at.start >= span.start && at.end <= span.end;
    let mut out = Vec::new();
    for param in generic.params.iter() {
        if let Some(fact) = analyzed.checked.binders.get(&param.name.span) {
            out.push(fact.ty);
        }
    }
    if let Some(&ret) = analyzed.decls.impl_fn_rets.get(&generic.decl_span) {
        out.push(ret);
    }
    let checked = &analyzed.checked;
    for (at, val) in &checked.expr_types {
        if inside(*at) {
            out.push(val.ty);
        }
    }
    for (at, fact) in &checked.binders {
        if inside(*at) {
            out.push(fact.ty);
        }
    }
    for (at, solved) in &checked.generic_calls {
        if inside(*at) {
            out.extend(solved);
        }
    }
    for (at, bounds) in &checked.generic_call_bounds {
        if inside(*at) {
            out.extend(bounds.iter().map(|(_, _, _, ty)| *ty));
        }
    }
    for (at, call) in &checked.trait_calls {
        if inside(*at) {
            out.push(call.recv);
        }
    }
    for (at, params) in &checked.member_call_params {
        if inside(*at) {
            out.extend(params);
        }
    }
    for (at, solved) in &checked.member_call_clauses {
        if inside(*at) {
            out.extend(solved);
        }
    }
    for (at, ty) in &checked.is_tests {
        if inside(*at) {
            out.push(*ty);
        }
    }
    for (at, store) in &checked.place_stores {
        if inside(*at) {
            out.push(store.root_ty);
            for step in &store.steps {
                match step {
                    PlaceStep::Field { ty, .. } | PlaceStep::Index { elem: ty, .. } => {
                        out.push(*ty);
                    },
                }
            }
        }
    }
    for ((_, at), rec) in &checked.comptime_records {
        if !inside(*at) {
            continue;
        }
        if let Some(val) = &rec.val {
            out.push(val.ty);
        }
        if let Some(fact) = &rec.binder {
            out.push(fact.ty);
        }
        if let Some(solved) = &rec.generic_call {
            out.extend(solved.iter().copied());
        }
        if let Some(bounds) = &rec.generic_bounds {
            out.extend(bounds.iter().map(|(_, _, _, ty)| *ty));
        }
        if let Some(call) = &rec.trait_call {
            out.push(call.recv);
        }
        if let Some(params) = &rec.member_params {
            out.extend(params.iter().copied());
        }
        if let Some(solved) = &rec.member_clause {
            out.extend(solved.iter().copied());
        }
        if let Some(ty) = rec.is_test {
            out.push(ty);
        }
        if let Some(store) = &rec.place_store {
            out.push(store.root_ty);
            for step in &store.steps {
                match step {
                    PlaceStep::Field { ty, .. } | PlaceStep::Index { elem: ty, .. } => {
                        out.push(*ty);
                    },
                }
            }
        }
        if let Some(ty) = rec.handler_handled {
            out.push(ty);
        }
    }
    out.sort_unstable_by_key(|ty: &Ty| ty.index());
    out.dedup();
    out
}

/// The impl's generic variables bound by one concrete receiver: the
/// head pattern unified against the receiver, each declared generic
/// parameter answered by the members it bound, in declaration order —
/// minted through the lens's own registry. `None` when the head does
/// not unify or a parameter stays unbound — the site keeps the ONE
/// body there.
fn member_args(
    found: &mut Discovery<'_, '_>,
    head: Ty,
    recv: Ty,
    generics: &[String],
) -> Option<Vec<Ty>> {
    let subst = ds_check::instance_solve::pattern_substitution(found.types.universe(), head, recv)?;
    let mut bound: Vec<Vec<TypeId>> = Vec::with_capacity(generics.len());
    for name in generics {
        let (_, members) = subst.iter().find(|(var, _)| {
            crate::shape::var_param_name(found.types.universe(), *var).as_deref()
                == Some(name.as_str())
        })?;
        bound.push(members.clone());
    }
    let mut args = Vec::with_capacity(bound.len());
    for members in &bound {
        args.push(found.types.set_of(members));
    }
    Some(args)
}

/// A trait default dispatch site's solved arguments: the receiver for
/// `Self`, then the impl's written trait arguments substituted through
/// the head-versus-receiver solving — a concrete argument names itself,
/// an impl's variable names what the receiver bound it to, and an
/// argument no receiver solving frees leaves the instance un-planned
/// (the ONE Raw-word body serves the site instead).
fn default_args(
    found: &mut Discovery<'_, '_>,
    tc: &TraitCall,
    impl_site: Span,
    recv: Ty,
) -> Option<Vec<Ty>> {
    let mut args = vec![recv];
    let Some(written) = found.analyzed.decls.trait_impl_args.get(&impl_site) else {
        return Some(args);
    };
    let env = &found.analyzed.decls.instances;
    let solved = ds_check::instance_solve::substituted_trait_args(
        found.types.universe_mut(),
        env,
        tc.instance,
        written,
        recv,
    )?;
    for arg in solved {
        args.push(arg?);
    }
    Some(args)
}

// how the site's solved arguments derive: an impl's member solves
// its generics from the head-versus-receiver pattern, a trait's
// declared default from the receiver for `Self` plus the impl's
// written trait arguments substituted through that same head
// solving
enum Site {
    Impl(Ty),
    Default { tc: TraitCall, impl_site: Span },
}

/// One dispatch site the checker solved onto a generic impl's member
/// body: the instance over the site's solved shape — the receiver's
/// head solving plus the member's own clause when the call's checker
/// row solved every clause parameter — or the ONE Raw-word body where
/// the solved list still names a variable. The decision reads the
/// written receiver and the solved clause types, never the lens
/// alone: an instance body serves shapes the entering sites' chain
/// words tell apart at runtime, so a body's own sites bake no shape a
/// shared instance could disagree with — a variable-written receiver
/// keeps the ONE body and its slot pass-through whatever the enclosing
/// instantiation solved. A body pushed here walks at once, so its own
/// sites and mints plan inside the entry holding this site.
pub(super) fn enter_member_site(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
    ctx: &MintCtx<'_>,
    call_span: Span,
    recv_span: Option<Span>,
) -> Result<(), LowerError> {
    let analyzed = found.analyzed;
    // The dispatch records, the copy-routed read: inside a `for
    // comptime` copy the checker's facts live in the per-copy tables,
    // the active expansion's instance records first.
    let copy_record = copy_record_at(
        &analyzed.checked,
        found.comptime_instance.as_deref(),
        &found.comptime_path,
        call_span,
    );
    let (generic, recv, site) = if let Some(tc) = copy_record
        .and_then(|rec| rec.trait_call.clone())
        .or_else(|| analyzed.checked.trait_calls.get(&call_span).cloned())
        && let Some(impl_site) = tc.impl_site
    {
        let member = analyzed.package.method(tc.method).name.clone();
        if let Some(generic) = plan.generic_member(impl_site, &member) {
            let head = analyzed
                .decls
                .instances
                .head(tc.instance)
                .map_err(|_| mismatch("a trait instance head"))?;
            (generic, tc.recv, Site::Impl(head))
        } else if let MethodOwner::Trait(def) = analyzed.package.method(tc.method).owner
            && let Some(generic) = plan.generic_default(def, &member)
        {
            // the solving impl spells no member: the trait's declared
            // default body serves the site, its `Self` variable the
            // receiver instantiates
            (generic, tc.recv, Site::Default {
                tc: tc.clone(),
                impl_site,
            })
        } else {
            return Ok(());
        }
    } else if let Some(method) = copy_record
        .and_then(|rec| rec.inherent_call)
        .or_else(|| analyzed.checked.inherent_calls.get(&call_span).copied())
        && let Some(&impl_site) = analyzed.decls.inherent_impls.get(&method)
    {
        let member = analyzed.package.method(method).name.clone();
        let Some(generic) = plan.generic_member(impl_site, &member) else {
            return Ok(());
        };
        let Some(record) = analyzed.decls.inherent_impl_bounds.get(&impl_site) else {
            return Ok(());
        };
        let Some(recv) = recv_span
            .and_then(|span| {
                copy_record
                    .and_then(|rec| rec.val)
                    .or_else(|| analyzed.checked.expr_types.get(&span).copied())
            })
            .map(|val| val.ty)
        else {
            // no receiver value (a type-keyed static call): the site
            // carries no shape to instantiate over — the ONE body the
            // declaration-side planning may hold, else the lowering's
            // honest refusal
            let (index, planned) = plan.member_one_body(analyzed, &generic);
            if planned {
                instance::walk_entry(plan, found, index)?;
            }
            return Ok(());
        };
        (generic, recv, Site::Impl(record.head))
    } else {
        return Ok(());
    };
    // the receiver reads as the body wrote it: a variable-written
    // receiver keeps the ONE body whatever the walking entry's own
    // instantiation solved for it
    let universe = found.types.universe();
    let concrete = !crate::types::has_variables(universe, recv);
    let (index, planned) = if concrete {
        // The site's solved clause, the copy-routed read (the free
        // call's solved-argument row mirrored): the types may name the
        // walking body's own variables, so the entering entry's
        // substitution resolves them first. A clause segment still
        // naming a variable keeps the ONE body; a site no row covers
        // stays the head-only instance.
        let clause = copy_record
            .and_then(|rec| rec.member_clause.clone())
            .or_else(|| {
                analyzed
                    .checked
                    .member_call_clauses
                    .get(&call_span)
                    .cloned()
            });
        let clause = match (&ctx.subst, clause) {
            (Some(subst), Some(solved)) => Some(
                solved
                    .iter()
                    .map(|&ty| found.types.substitute(subst, ty))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            (None, Some(solved)) => Some(solved),
            (_, None) => None,
        };
        let head_solved = match &site {
            Site::Impl(head) => member_args(found, *head, recv, &generic.generics),
            Site::Default { tc, impl_site } => default_args(found, tc, *impl_site, recv),
        };
        match head_solved {
            Some(mut args) => {
                let clause_ready = clause.as_ref().is_some_and(|solved| {
                    solved
                        .iter()
                        .all(|&ty| !crate::types::has_variables(found.types.universe(), ty))
                });
                if clause.is_some() && !clause_ready {
                    plan.member_one_body(analyzed, &generic)
                } else {
                    if let Some(solved) = clause {
                        args.extend(solved);
                    }
                    plan.member_instance(found, &generic, &args, recv)?
                }
            },
            None => plan.member_one_body(analyzed, &generic),
        }
    } else {
        plan.member_one_body(analyzed, &generic)
    };
    if planned {
        instance::walk_entry(plan, found, index)?;
    }
    Ok(())
}

impl Plan {
    /// Records one generic impl's member body: it holds no table entry
    /// of its own until a site enters it or a runtime-evidence demand
    /// names its member.
    pub(crate) fn register_generic_member(&mut self, generic: GenericMember) {
        self.generic_members.insert(
            (generic.impl_site, generic.member.clone()),
            Rc::new(generic),
        );
    }

    /// Records one trait's declared default body: the per-shape story
    /// runs over the trait's `Self` variable, keyed by the trait's own
    /// span the same way an impl's member keys by its item span, so the
    /// shared instance tables address both stories alike.
    pub(crate) fn register_generic_default(
        &mut self,
        trait_def: DefId,
        trait_span: Span,
        generic: GenericMember,
        one_body: u32,
    ) {
        let member = generic.member.clone();
        self.generic_defaults
            .insert((trait_def, member.clone()), Rc::new(generic));
        self.impl_index.insert((trait_span, member), one_body);
    }

    /// The registered generic default one trait spells for a member.
    pub(crate) fn generic_default(
        &self,
        trait_def: DefId,
        member: &str,
    ) -> Option<Rc<GenericMember>> {
        self.generic_defaults
            .get(&(trait_def, member.to_owned()))
            .cloned()
    }

    /// The registered generic member one impl item spells, `None` for a
    /// monomorphic impl's member.
    pub(crate) fn generic_member(
        &self,
        impl_site: Span,
        member: &str,
    ) -> Option<Rc<GenericMember>> {
        self.generic_members
            .get(&(impl_site, member.to_owned()))
            .cloned()
    }

    /// The instance one entering site solved for, when the plan's
    /// discovery planned it: the lowering's target lookup, keyed by the
    /// receiver type the site's lens reads and the solved clause's tail
    /// segment — the same handle and row the discovery substituted and
    /// recorded.
    pub(crate) fn member_target(
        &self,
        impl_site: Span,
        member: &str,
        recv: Ty,
        clause: Rc<Vec<Ty>>,
    ) -> Option<u32> {
        self.member_instance_args
            .get(&(impl_site, member.to_owned(), recv, clause))
            .copied()
    }

    /// The generic member's ONE Raw-word body, pushed when no site
    /// entered it yet: the body a call site still naming a variable
    /// targets, and the body every runtime-evidence chain names.
    pub(crate) fn member_one_body(
        &mut self,
        analyzed: &Analyzed,
        generic: &GenericMember,
    ) -> (u32, bool) {
        if let Some(&index) = self
            .impl_index
            .get(&(generic.impl_site, generic.member.clone()))
        {
            return (index, false);
        }
        let ret = analyzed
            .decls
            .impl_fn_rets
            .get(&generic.decl_span)
            .copied()
            .expect("a registered member body carries a solved return");
        (self.push_member_one_body(generic, ret), true)
    }

    /// Pushes one generic member's ONE body: its declared parameters
    /// may sit over the impl's own type variables (one Raw word each —
    /// the runtime kind is the caller's fact), and the entering call
    /// sites stage its runtime-evidence demands as trailing words,
    /// exactly a free generic fn's ONE body.
    fn push_member_one_body(&mut self, generic: &GenericMember, ret: Ty) -> u32 {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.impl_index
            .insert((generic.impl_site, generic.member.clone()), index);
        self.functions.push(PlannedFn {
            name: generic.name.clone(),
            lambda: (*generic.body).clone(),
            def: None,
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn {
                params: Rc::clone(&generic.params),
                body: Rc::clone(&generic.body),
            })),
            ret: Some(ret),
            evidence: Rc::clone(&generic.pairs),
            bounds: Rc::clone(&generic.bounds),
            spawn_child: false,
            free_generic: true,
            instance: None,
            subst: None,
            comptime_instance: None,
        });
        index
    }

    /// The instance of one generic member over the solved argument
    /// types: the body lowered over that shape — the impl head's
    /// arguments plus the member's own clause segment — shared with
    /// every site whose solved list takes the same shape, the site
    /// lookup keyed by the receiver type and the clause's tail segment.
    fn member_instance(
        &mut self,
        found: &mut Discovery<'_, '_>,
        generic: &Rc<GenericMember>,
        args: &[Ty],
        recv: Ty,
    ) -> Result<(u32, bool), LowerError> {
        let key_args: MemberSiteKey = (
            generic.impl_site,
            generic.member.clone(),
            recv,
            Rc::new(args[generic.generics.len()..].to_vec()),
        );
        if let Some(&index) = self.member_instance_args.get(&key_args) {
            return Ok((index, false));
        }
        let body = self.member_body_types(found.analyzed, generic);
        let mut_vars = member_vars(found.analyzed, generic, args);
        let subst = found.types.subst(mut_vars);
        for &ty in body.iter() {
            found.types.substitute(&subst, ty)?;
        }
        let key = instance::instance_key(found, &subst, &body, generic.body.span())?;
        let mut planned = false;
        let index = if let Some(&index) =
            self.member_instances
                .get(&(generic.impl_site, generic.member.clone(), Rc::clone(&key)))
        {
            index
        } else {
            let count = self
                .member_instances
                .keys()
                .filter(|(site, member, _)| *site == generic.impl_site && *member == generic.member)
                .count();
            if count >= instance::MAX_INSTANCES_OF_ONE_BODY {
                return Err(LowerError::unsupported(
                    generic.body.span(),
                    "a generic impl member instantiating itself without end",
                    "its body enters itself over an argument shape that keeps growing; name a \
                     fixed shape at some depth of the recursion",
                ));
            }
            let substituted = |ty: Ty| {
                if let Some(hit) = found.types.substituted(&subst, ty) {
                    return Ok(hit);
                }
                // a type naming no substituted variable is itself —
                // the substitution mints nothing for it — while one
                // naming a variable the head pattern never bound is
                // the belt this loud refusal owns
                if subst.mentions(found.types.universe(), ty) {
                    return Err(LowerError::unsupported(
                        generic.body.span(),
                        "a generic impl member's signature",
                        "the discovery substituted every recorded type but this one; report the \
                         construct",
                    ));
                }
                Ok(ty)
            };
            let mut params = Vec::with_capacity(generic.params.len());
            for param in generic.params.iter() {
                let fact = found
                    .analyzed
                    .checked
                    .binders
                    .get(&param.name.span)
                    .copied()
                    .expect("a planned member body carries checked parameter facts");
                params.push(substituted(fact.ty)?);
            }
            let ret = substituted(
                found
                    .analyzed
                    .decls
                    .impl_fn_rets
                    .get(&generic.decl_span)
                    .copied()
                    .expect("a planned member body carries a solved return"),
            )?;
            let name = self.member_instance_name(found, &generic.name, &key);
            let index = u32::try_from(self.functions.len()).expect("function index fits u32");
            self.functions.push(PlannedFn {
                name,
                lambda: (*generic.body).clone(),
                def: None,
                deinit: None,
                captures: Rc::new(Vec::new()),
                decl: Some(Rc::new(DeclFn {
                    params: Rc::clone(&generic.params),
                    body: Rc::clone(&generic.body),
                })),
                ret: Some(ret),
                evidence: Rc::clone(&generic.pairs),
                bounds: Rc::clone(&generic.bounds),
                spawn_child: false,
                free_generic: false,
                instance: Some(Rc::new(Instance {
                    params: params.into(),
                })),
                subst: Some(subst),
                comptime_instance: None,
            });
            self.member_instances
                .insert((generic.impl_site, generic.member.clone(), key), index);
            if generic.default_of.is_some() {
                self.default_instances.insert(index);
            }
            planned = true;
            index
        };
        self.member_instance_args.insert(key_args, index);
        Ok((index, planned))
    }

    /// One generic member's recorded types, collected once per member.
    fn member_body_types(&mut self, analyzed: &Analyzed, generic: &GenericMember) -> Rc<[Ty]> {
        if let Some(types) = self
            .member_types
            .get(&(generic.impl_site, generic.member.clone()))
        {
            return Rc::clone(types);
        }
        let types: Rc<[Ty]> = member_body_types(analyzed, generic).into();
        self.member_types.insert(
            (generic.impl_site, generic.member.clone()),
            Rc::clone(&types),
        );
        types
    }

    /// One instance's table name: the member's mangled name at the
    /// shape key's digest, so a body keeps its name across compiles
    /// while its shape holds.
    fn member_instance_name(
        &self,
        found: &Discovery<'_, '_>,
        name: &str,
        key: &Rc<InstanceKey>,
    ) -> String {
        let probe = Shapes::new(found.analyzed, found.types, HashSet::new());
        let rendered = probe.render_key(key);
        let digest = u64::from_le_bytes(
            ds_mod::Hash128::compute(rendered.as_bytes()).as_bytes()[..8]
                .try_into()
                .expect("a digest holds eight bytes"),
        );
        let base = format!("{name}@{digest:016x}");
        let mut candidate = base.clone();
        let mut ordinal = 1;
        while self.functions.iter().any(|entry| entry.name == candidate) {
            candidate = format!("{base}~{ordinal}");
            ordinal += 1;
        }
        candidate
    }

    /// Whether any body's runtime-evidence demands name this member of
    /// this trait: a demand's chain names the serving body under some
    /// instance of the trait, and the site baking the chain may sit in
    /// any item, so the member's ONE body plans at its own declaration.
    /// An inherent member is never demanded (a demand names a trait
    /// member alone), so its ONE body plans at its entering sites only.
    pub(crate) fn member_is_demanded(
        &self,
        trait_name: &str,
        member: &str,
        analyzed: &Analyzed,
    ) -> bool {
        let Some(def) = analyzed.package.lookup_type(trait_name) else {
            return false;
        };
        let Some(&trait_id) = analyzed.decls.traits.get(&def) else {
            return false;
        };
        for pairs in self.demands.pairs.values() {
            for pair in pairs.iter() {
                let method = analyzed.package.method(pair.method);
                if method.name != member {
                    continue;
                }
                if let MethodOwner::Trait(def) = &method.owner
                    && analyzed.decls.traits.get(def) == Some(&trait_id)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Whether any body of one impl member planned — the ONE body or a
    /// per-shape instance: the native-routing readers' check, so a
    /// generic impl's bodied member never falls through to the native
    /// its impl shadowed on a site whose discovery planned an instance
    /// alone.
    pub(crate) fn member_planned(&self, impl_site: Span, member: &str) -> bool {
        if self
            .impl_index
            .contains_key(&(impl_site, member.to_owned()))
        {
            return true;
        }
        self.member_instance_args
            .keys()
            .any(|(site, name, ..)| *site == impl_site && name == member)
    }

    /// Plans one named generic member's ONE body at its own declaration,
    /// when a runtime-evidence demand names the member.
    pub(crate) fn member_one_body_named(
        &mut self,
        analyzed: &Analyzed,
        impl_site: Span,
        member: &str,
    ) {
        if let Some(generic) = self.generic_member(impl_site, member) {
            self.member_one_body(analyzed, &generic);
        }
    }
}

/// The member body's variable atoms to the solved argument types. An
/// impl's member binds two segments by written name — the instance
/// head's registration minting and the body's own checking minting
/// are distinct atom sets, so the name alone bridges them: the
/// receiver's fact carries the impl's own variable atoms as the
/// body's checking minted them, and the member fn's clause mints in
/// the body's scope on top of them, shadowing a same-named impl
/// generic inside the written signature — so an atom outside the
/// receiver's set belongs to the clause, and the two segments share
/// atoms by name alone. The clause's variables bind positionally
/// behind the head segment; a trait's declared default body runs the
/// same story over the trait's own `Self` variable and declares no
/// clause, binding every argument by name against the declared list.
fn member_vars(analyzed: &Analyzed, generic: &GenericMember, args: &[Ty]) -> HashMap<TypeId, Ty> {
    let universe = &analyzed.decls.universe;
    let mut vars = HashMap::new();
    if generic.default_of.is_some() {
        for param in generic.params.iter() {
            let Some(fact) = analyzed.checked.binders.get(&param.name.span) else {
                continue;
            };
            for &atom in universe.members(fact.ty) {
                bind_member_var(universe, atom, generic, args, &[], &mut vars);
            }
        }
        if let Some(&ret) = analyzed.decls.impl_fn_rets.get(&generic.decl_span) {
            for &atom in universe.members(ret) {
                bind_member_var(universe, atom, generic, args, &[], &mut vars);
            }
        }
        return vars;
    }
    // the impl's own variable atoms, as the body's checking minted
    // them: the receiver's fact holds the head pattern's atoms, the
    // clause's mintings never occur in it
    let mut head_atoms: Vec<TypeId> = Vec::new();
    if let Some(param) = generic
        .params
        .iter()
        .find(|param| param.name.name == "self")
        && let Some(fact) = analyzed.checked.binders.get(&param.name.span)
    {
        collect_head_atoms(universe, fact.ty, &mut head_atoms);
    }
    for param in generic.params.iter() {
        let Some(fact) = analyzed.checked.binders.get(&param.name.span) else {
            continue;
        };
        for &atom in universe.members(fact.ty) {
            bind_member_var(universe, atom, generic, args, &head_atoms, &mut vars);
        }
    }
    if let Some(&ret) = analyzed.decls.impl_fn_rets.get(&generic.decl_span) {
        for &atom in universe.members(ret) {
            bind_member_var(universe, atom, generic, args, &head_atoms, &mut vars);
        }
    }
    vars
}

/// Every atom one type mentions, the nested declarations included.
fn collect_head_atoms(universe: &ds_check::Universe, ty: Ty, out: &mut Vec<TypeId>) {
    for &atom in universe.members(ty) {
        collect_head_atom(universe, atom, out);
    }
}

/// One atom and the nested atoms its declaration carries.
fn collect_head_atom(universe: &ds_check::Universe, atom: TypeId, out: &mut Vec<TypeId>) {
    if out.contains(&atom) {
        return;
    }
    out.push(atom);
    let Ok(decl) = universe.atom_decl(atom) else {
        return;
    };
    match decl.kind.clone() {
        AtomKind::Apply(_, bound) => {
            for &arg in &bound {
                collect_head_atoms(universe, arg, out);
            }
        },
        AtomKind::Tuple(elements) => {
            for &element in &elements {
                collect_head_atom(universe, element, out);
            }
        },
        AtomKind::FnSig(params, ret) => {
            for &param in &params {
                collect_head_atoms(universe, param, out);
            }
            collect_head_atoms(universe, ret, out);
        },
        AtomKind::Unit | AtomKind::Struct(_) | AtomKind::Enum(_) | AtomKind::Reserved => {},
    }
}

/// Binds one atom (and the nested atoms its declaration carries)
/// against the two segments: a declared default's atom names a trait
/// variable and binds at its declared position, an impl member's atom
/// the receiver's set holds names an impl generic and binds at its
/// declared position, and every other named atom belongs to the
/// member's own clause, binding positionally behind the head segment.
fn bind_member_var(
    universe: &ds_check::Universe,
    atom: TypeId,
    generic: &GenericMember,
    args: &[Ty],
    head_atoms: &[TypeId],
    vars: &mut HashMap<TypeId, Ty>,
) {
    if let Some(name) = crate::shape::var_param_name(universe, atom) {
        let in_head = generic.default_of.is_some() || head_atoms.contains(&atom);
        let segment = if in_head {
            generic
                .generics
                .iter()
                .position(|param| *param == name)
                .and_then(|at| args.get(at))
        } else {
            generic
                .clause
                .iter()
                .position(|param| *param == name)
                .and_then(|at| args.get(generic.generics.len() + at))
        };
        if let Some(&arg) = segment {
            vars.insert(atom, arg);
        }
    }
    if let Ok(decl) = universe.atom_decl(atom) {
        match decl.kind.clone() {
            AtomKind::Apply(_, bound) => {
                for &arg in &bound {
                    for &nested in universe.members(arg) {
                        bind_member_var(universe, nested, generic, args, head_atoms, vars);
                    }
                }
            },
            AtomKind::Tuple(elements) => {
                for &element in &elements {
                    bind_member_var(universe, element, generic, args, head_atoms, vars);
                }
            },
            AtomKind::FnSig(params, ret) => {
                for &param in &params {
                    for &nested in universe.members(param) {
                        bind_member_var(universe, nested, generic, args, head_atoms, vars);
                    }
                }
                for &nested in universe.members(ret) {
                    bind_member_var(universe, nested, generic, args, head_atoms, vars);
                }
            },
            AtomKind::Unit | AtomKind::Struct(_) | AtomKind::Enum(_) | AtomKind::Reserved => {},
        }
    }
}

impl FnLower<'_, '_, '_> {
    /// The dispatch target of one call site onto a generic impl's member
    /// body: the per-shape instance the site's receiver and solved
    /// clause picked, or the ONE Raw-word body a site still naming a
    /// variable enters — the same target the plan's discovery planned,
    /// so a miss is the walk and the discovery disagreeing on the site.
    pub(in crate::func) fn member_dispatch_target(
        &self,
        generic: &Rc<GenericMember>,
        recv: Ty,
        call: Span,
        what: Span,
    ) -> Result<u32, LowerError> {
        let universe = self.universe();
        let concrete = !crate::types::has_variables(universe, recv);
        let target = if concrete {
            // the site's solved clause through the lens: the same tail
            // segment the discovery appended behind the head arguments,
            // an unrecorded clause reading the empty segment a
            // head-only instance keys
            let clause = self.member_call_clause(call)?.unwrap_or_default();
            self.plan
                .member_target(generic.impl_site, &generic.member, recv, Rc::new(clause))
        } else {
            None
        };
        target
            .or_else(|| self.plan.impl_body(generic.impl_site, &generic.member))
            .ok_or_else(|| {
                LowerError::unsupported(
                    what,
                    "a member dispatch into a generic impl body",
                    "the plan's discovery planned no body for this site; the walk and the \
                     discovery disagree — report the construct",
                )
            })
    }
}
