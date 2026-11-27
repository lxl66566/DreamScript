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
/// body: the instance over the receiver's solved shape when the site's
/// written receiver is concrete, else the ONE Raw-word body. The
/// decision reads the written receiver, never the lens: an instance
/// body serves shapes the entering sites' chain words tell apart at
/// runtime, so a body's own sites bake no shape a shared instance
/// could disagree with — a variable-written receiver keeps the ONE
/// body and its slot pass-through whatever the enclosing instantiation
/// solved. A body pushed here walks at once, so its own sites and mints
/// plan inside the entry holding this site.
pub(super) fn enter_member_site(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
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
        let args = match &site {
            Site::Impl(head) => member_args(found, *head, recv, &generic.generics),
            Site::Default { tc, impl_site } => default_args(found, tc, *impl_site, recv),
        };
        match args {
            Some(args) => plan.member_instance(found, &generic, &args, recv)?,
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
    /// receiver type the site's lens reads — the same handle the
    /// discovery substituted and recorded.
    pub(crate) fn member_target(&self, impl_site: Span, member: &str, recv: Ty) -> Option<u32> {
        self.member_instance_args
            .get(&(impl_site, member.to_owned(), recv))
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

    /// The instance of one generic member over the receiver's solved
    /// types: the body lowered over that shape, shared with every site
    /// whose receiver takes the same shape — the body keyed by the
    /// shape, the site lookup keyed by the receiver type itself.
    fn member_instance(
        &mut self,
        found: &mut Discovery<'_, '_>,
        generic: &Rc<GenericMember>,
        args: &[Ty],
        recv: Ty,
    ) -> Result<(u32, bool), LowerError> {
        let key_args = (generic.impl_site, generic.member.clone(), recv);
        if let Some(&index) = self.member_instance_args.get(&key_args) {
            return Ok((index, false));
        }
        let body = self.member_body_types(found.analyzed, generic);
        let subst = found
            .types
            .subst(member_vars(found.analyzed, generic, args));
        for &ty in body.iter() {
            found.types.substitute(&subst, ty)?;
        }
        let key = instance::instance_key(found, &subst, &body)?;
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
            .any(|(site, name, _)| *site == impl_site && name == member)
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

/// The member body's variable atoms to the solved argument types: the
/// impl's declared generic parameters bind to the variables the
/// declaration's checking minted, found by written name anywhere in
/// the head pattern — the checker pushes the impl's generics and mints
/// the head over those same atoms.
fn member_vars(analyzed: &Analyzed, generic: &GenericMember, args: &[Ty]) -> HashMap<TypeId, Ty> {
    let universe = &analyzed.decls.universe;
    let mut vars = HashMap::new();
    for param in generic.params.iter() {
        let Some(fact) = analyzed.checked.binders.get(&param.name.span) else {
            continue;
        };
        for &atom in universe.members(fact.ty) {
            bind_member_var(universe, atom, generic, args, &mut vars);
        }
    }
    if let Some(&ret) = analyzed.decls.impl_fn_rets.get(&generic.decl_span) {
        for &atom in universe.members(ret) {
            bind_member_var(universe, atom, generic, args, &mut vars);
        }
    }
    vars
}

/// Binds one atom (and the nested atoms its declaration carries) when
/// it names a generic parameter of the impl.
fn bind_member_var(
    universe: &ds_check::Universe,
    atom: TypeId,
    generic: &GenericMember,
    args: &[Ty],
    vars: &mut HashMap<TypeId, Ty>,
) {
    // a variable atom binds directly; every other atom's nested
    // declarations may still name the variables, so the walk continues
    // past it whatever it is
    if let Some(name) = crate::shape::var_param_name(universe, atom)
        && let Some(&arg) = generic
            .generics
            .iter()
            .position(|param| *param == name)
            .and_then(|at| args.get(at))
    {
        vars.insert(atom, arg);
    }
    if let Ok(decl) = universe.atom_decl(atom) {
        match decl.kind.clone() {
            AtomKind::Apply(_, bound) => {
                for &arg in &bound {
                    for &nested in universe.members(arg) {
                        bind_member_var(universe, nested, generic, args, vars);
                    }
                }
            },
            AtomKind::Tuple(elements) => {
                for &element in &elements {
                    bind_member_var(universe, element, generic, args, vars);
                }
            },
            AtomKind::FnSig(params, ret) => {
                for &param in &params {
                    for &nested in universe.members(param) {
                        bind_member_var(universe, nested, generic, args, vars);
                    }
                }
                for &nested in universe.members(ret) {
                    bind_member_var(universe, nested, generic, args, vars);
                }
            },
            AtomKind::Unit | AtomKind::Struct(_) | AtomKind::Enum(_) | AtomKind::Reserved => {},
        }
    }
}

impl FnLower<'_, '_, '_> {
    /// The dispatch target of one call site onto a generic impl's
    /// member body: the per-shape instance the site's receiver solved,
    /// or the ONE Raw-word body a site still naming a variable enters —
    /// the same target the plan's discovery planned, so a miss is the
    /// walk and the discovery disagreeing on the site.
    pub(in crate::func) fn member_dispatch_target(
        &self,
        generic: &Rc<GenericMember>,
        recv: Ty,
        what: Span,
    ) -> Result<u32, LowerError> {
        let universe = self.universe();
        let concrete = !crate::types::has_variables(universe, recv);
        let target = if concrete {
            self.plan
                .member_target(generic.impl_site, &generic.member, recv)
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
