//! Per-shape instantiation of free generic fns: the plan's site
//! discovery, the shape key, and the instance records.
//!
//! A free generic fn holds no table entry of its own. Every call site
//! entering it plans the body it needs before anything lowers: an
//! instance over the types the checker solved for the site's generic
//! arguments, or the ONE body where the site still names a variable (a
//! call from inside a body that lowers over its variables' Raw words).
//! Instances key by shape, so two argument types that lower identically
//! share one body, and every instance, its nested instances, and its
//! minted records plan inside the item holding the first site that
//! entered it — the plan stays a pure function of the trees, which is
//! what the served splice pins.

use ds_mod::Hash128;

use super::*;

/// One free generic fn declared in the package: the body the entering
/// sites instantiate.
pub(crate) struct GenericFn {
    /// The fn's name, the ONE body's table name.
    pub(crate) name: String,
    /// The fn's package definition.
    pub(crate) def: DefId,
    /// The fn's lambda shape: its parameters and block.
    pub(crate) lambda: Expr,
    /// The body's runtime-evidence demands after the package-wide
    /// propagation, the same list for the ONE body and every instance.
    pub(crate) pairs: Rc<Vec<RuntimePair>>,
    /// The fn's declared bounds, by parameter name.
    pub(crate) bounds: Rc<HashMap<String, HashSet<TraitId>>>,
}

/// One instantiation of a free generic fn: the body lowered over the
/// types one entering site solved. The plan keys the body by the
/// generic fn's definition and the solved argument types, and the entry
/// carries the substitution the body lowers under and the substituted
/// return on its own `ret` row, so the instance record itself holds the
/// one fact the plan entry cannot: the concrete parameter types the
/// entering site stages and re-spans its arguments against.
pub(crate) struct Instance {
    /// The body's parameter types under the substitution.
    pub(crate) params: Rc<[Ty]>,
}

/// The key instances share a body by: the shape every type the checker
/// recorded for the body takes under the substitution, in the body's
/// fixed type order. Two substitutions agreeing on it lower the same
/// code, so they share one body.
pub(crate) type InstanceKey = [ShapeKey];

/// The most instances one generic fn plans. A body entering itself over
/// an argument shape that keeps growing (a tuple over its own variable)
/// never reaches a fixed point; the cap refuses it typed, where the
/// frame width alone would never.
const MAX_INSTANCES_OF_ONE_BODY: usize = 256;

/// Walks one planned entry's body: its lambda and spawn sites mint their
/// records and its calls entering a free generic fn plan their targets.
/// Each entry walks once, when the plan pushes it.
pub(super) fn walk_entry(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
    at: u32,
) -> Result<(), LowerError> {
    let analyzed = found.analyzed;
    let (name, bounds, subst, file, body) = {
        let entry = &plan.functions[at as usize];
        (
            entry.name.clone(),
            entry.bounds.clone(),
            entry.subst.clone(),
            entry.lambda.span().file,
            entry_body(entry).clone(),
        )
    };
    let ctx = MintCtx {
        enclosing: at,
        enclosing_name: name,
        bounds,
        subst,
        res: &analyzed.resolutions[file.as_u32() as usize],
    };
    discover_expr(plan, found, &ctx, &body)
}

/// One call site entering a free generic fn: the instance over the
/// solved argument types when the site's substitution makes them
/// concrete, else the ONE body. A body pushed here walks at once, so
/// its own sites and mints plan inside the entry holding this site.
pub(super) fn enter_site(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
    ctx: &MintCtx,
    callee: &Expr,
    call_span: Span,
) -> Result<(), LowerError> {
    let Some(def) = free_callee_def(found.analyzed, ctx.res, callee, call_span) else {
        return Ok(());
    };
    let Some(generic) = plan.generic_fn(def) else {
        return Ok(());
    };
    let solved = found
        .analyzed
        .checked
        .generic_calls
        .get(&call_span)
        .cloned()
        .unwrap_or_default();
    let args = match &ctx.subst {
        Some(subst) => solved
            .iter()
            .map(|&ty| found.types.substitute(subst, ty))
            .collect::<Result<Vec<_>, _>>()?,
        None => solved,
    };
    let concrete = !args.is_empty()
        && args
            .iter()
            .all(|&ty| !crate::types::has_variables(found.types.universe(), ty));
    let (index, planned) =
        if concrete { plan.instance(found, &generic, &args)? } else { plan.one_body(&generic) };
    if planned {
        walk_entry(plan, found, index)?;
    }
    Ok(())
}

/// The free function one call's callee names, by the resolution rules the
/// call walk applies: a plain name, a UFCS dot-call the method tier did
/// not claim, or a module alias's qualified call.
fn free_callee_def(
    analyzed: &Analyzed,
    res: &Resolutions,
    callee: &Expr,
    call_span: Span,
) -> Option<DefId> {
    match callee {
        Expr::Path { span, .. } => match res.get(*span) {
            Some(Res::Def(def)) => Some(*def),
            _ => None,
        },
        Expr::Field { name, .. } => match res.get(name.span) {
            Some(Res::Dot(DotRes::Free { func, fed_param: 0 }))
                if !analyzed.checked.inherent_calls.contains_key(&call_span)
                    && !analyzed.checked.trait_calls.contains_key(&call_span) =>
            {
                Some(*func)
            },
            Some(Res::Dot(DotRes::Module { func })) => Some(*func),
            _ => None,
        },
        _ => None,
    }
}

/// Every type the checker recorded for one generic fn's body, in a fixed
/// order: the signature, then every expression type, binder type, and
/// call-site record inside the body's span. The discovery substitutes
/// exactly these, and the instance key reads exactly these — a type the
/// walk reads and this list misses refuses at the walk instead of
/// lowering a variable's word.
fn body_types(analyzed: &Analyzed, generic: &GenericFn) -> Vec<Ty> {
    let span = generic.lambda.span();
    let inside = |at: Span| at.file == span.file && at.start >= span.start && at.end <= span.end;
    let mut out = Vec::new();
    if let Some(&fn_ty) = analyzed.checked.def_types.get(&generic.def)
        && let Some((params, ret)) = fn_sig_of(&analyzed.decls.universe, fn_ty)
    {
        out.extend_from_slice(params);
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
    out.sort_unstable_by_key(|ty: &Ty| ty.index());
    out.dedup();
    out
}

/// The body's own variable atoms to the solved argument types: the fn's
/// generic parameters, in declaration order, bind to the variables the
/// body's checking minted, found by name anywhere in the fn's recorded
/// signature — the checker pushes the generics and solves the signature
/// and the body in one scope, so a variable nested inside a tuple
/// parameter, an application's arguments, or a nested fn type is one of
/// these atoms and the instance declaration's minting is bridged by the
/// written parameter name.
fn body_vars(analyzed: &Analyzed, generic: &GenericFn, args: &[Ty]) -> HashMap<TypeId, Ty> {
    /// Binds every atom one type mentions that names a generic
    /// parameter of the signature.
    fn visit_ty(
        universe: &ds_check::Universe,
        ty: Ty,
        vars: &mut HashMap<TypeId, Ty>,
        bind: &impl Fn(TypeId, &mut HashMap<TypeId, Ty>),
    ) {
        for &atom in universe.members(ty) {
            visit_atom(universe, atom, vars, bind);
        }
    }
    /// Binds one atom and walks the nested atoms its declaration
    /// carries.
    fn visit_atom(
        universe: &ds_check::Universe,
        atom: TypeId,
        vars: &mut HashMap<TypeId, Ty>,
        bind: &impl Fn(TypeId, &mut HashMap<TypeId, Ty>),
    ) {
        bind(atom, vars);
        let Ok(decl) = universe.atom_decl(atom) else {
            return;
        };
        match decl.kind.clone() {
            AtomKind::Apply(_, args) => {
                for &arg in &args {
                    visit_ty(universe, arg, vars, bind);
                }
            },
            AtomKind::Tuple(elements) => {
                for &element in &elements {
                    visit_atom(universe, element, vars, bind);
                }
            },
            AtomKind::FnSig(params, ret) => {
                for &param in &params {
                    visit_ty(universe, param, vars, bind);
                }
                visit_ty(universe, ret, vars, bind);
            },
            AtomKind::Unit | AtomKind::Struct(_) | AtomKind::Enum(_) | AtomKind::Reserved => {},
        }
    }
    let universe = &analyzed.decls.universe;
    let Some(sig) = analyzed.decls.fns.get(&generic.def) else {
        return HashMap::new();
    };
    let Some(&fn_ty) = analyzed.checked.def_types.get(&generic.def) else {
        return HashMap::new();
    };
    let Some((params, ret)) = fn_sig_of(universe, fn_ty) else {
        return HashMap::new();
    };
    let mut vars = HashMap::new();
    let bind = |atom: TypeId, vars: &mut HashMap<TypeId, Ty>| {
        let Some(name) = crate::shape::var_param_name(universe, atom) else {
            return;
        };
        if let Some(&arg) = sig
            .generics
            .iter()
            .position(|param| param.name == name)
            .and_then(|at| args.get(at))
        {
            vars.insert(atom, arg);
        }
    };
    for &ty in params {
        visit_ty(universe, ty, &mut vars, &bind);
    }
    visit_ty(universe, ret, &mut vars, &bind);
    vars
}

impl Plan {
    /// The free generic fn's ONE body, pushed when no site entered it
    /// yet: the body a call site still naming a variable targets.
    pub(crate) fn one_body(&mut self, generic: &GenericFn) -> (u32, bool) {
        match self.index.get(&generic.def) {
            Some(&index) => (index, false),
            None => (self.push_free_generic(generic), true),
        }
    }

    /// The instance of one generic fn over the solved argument types:
    /// the body lowered over that shape, shared with every site whose
    /// argument types take the same shape. The body's recorded types are
    /// substituted here in full, so the walk's reads are cache hits.
    fn instance(
        &mut self,
        found: &mut Discovery<'_, '_>,
        generic: &Rc<GenericFn>,
        args: &[Ty],
    ) -> Result<(u32, bool), LowerError> {
        if let Some(&index) = self.instance_args.get(&(generic.def, args.to_vec())) {
            return Ok((index, false));
        }
        let body = self.generic_body_types(found.analyzed, generic);
        let subst = found.types.subst(body_vars(found.analyzed, generic, args));
        for &ty in body.iter() {
            found.types.substitute(&subst, ty)?;
        }
        let key = instance_key(found, &subst, &body)?;
        let mut planned = false;
        let index = if let Some(&index) = self.instances.get(&(generic.def, key.clone())) {
            index
        } else {
            let count = self
                .instances
                .keys()
                .filter(|(def, _)| *def == generic.def)
                .count();
            if count >= MAX_INSTANCES_OF_ONE_BODY {
                return Err(LowerError::unsupported(
                    generic.lambda.span(),
                    "a generic fn instantiating itself without end",
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
                // naming a variable the discovery never resolved is
                // the belt this loud refusal owns
                if subst.mentions(found.types.universe(), ty) {
                    return Err(LowerError::unsupported(
                        generic.lambda.span(),
                        "a generic fn's signature",
                        "the discovery substituted every recorded type but this one; report the \
                         construct",
                    ));
                }
                Ok(ty)
            };
            let fn_ty = found
                .analyzed
                .checked
                .def_types
                .get(&generic.def)
                .copied()
                .expect("a planned generic fn carries a checked type");
            let (params, ret) = fn_sig_of(found.types.universe(), fn_ty)
                .expect("a generic fn's checked type is a function type");
            let params = params
                .iter()
                .map(|&ty| substituted(ty))
                .collect::<Result<Vec<_>, _>>()?;
            let ret = substituted(ret)?;
            let name = self.instance_name(found, &generic.name, &key);
            let index = u32::try_from(self.functions.len()).expect("function index fits u32");
            let Expr::Lambda {
                params: declared,
                body: block,
                ..
            } = &generic.lambda
            else {
                unreachable!("a free generic fn plans a lambda-armed local");
            };
            self.functions.push(PlannedFn {
                name,
                lambda: generic.lambda.clone(),
                def: Some(generic.def),
                deinit: None,
                captures: Rc::new(Vec::new()),
                decl: Some(Rc::new(DeclFn {
                    params: Rc::new(declared.clone()),
                    body: Rc::new((**block).clone()),
                })),
                ret: Some(ret),
                evidence: generic.pairs.clone(),
                bounds: generic.bounds.clone(),
                spawn_child: false,
                free_generic: false,
                instance: Some(Rc::new(Instance {
                    params: params.into(),
                })),
                subst: Some(subst),
            });
            self.instances.insert((generic.def, key), index);
            planned = true;
            index
        };
        self.instance_args
            .insert((generic.def, args.to_vec()), index);
        Ok((index, planned))
    }

    /// One generic fn's recorded types, collected once.
    fn generic_body_types(&mut self, analyzed: &Analyzed, generic: &GenericFn) -> Rc<[Ty]> {
        if let Some(types) = self.body_types.get(&generic.def) {
            return Rc::clone(types);
        }
        let types: Rc<[Ty]> = body_types(analyzed, generic).into();
        self.body_types.insert(generic.def, Rc::clone(&types));
        types
    }

    /// One instance's table name: the generic fn's name at the shape
    /// key's digest, so a body keeps its name across compiles while its
    /// shape holds. A digest two keys share gets an ordinal.
    fn instance_name(&self, found: &Discovery<'_, '_>, name: &str, key: &InstanceKey) -> String {
        let probe = Shapes::new(found.analyzed, found.types, HashSet::new());
        let rendered = probe.render_key(key);
        let digest = u64::from_le_bytes(
            Hash128::compute(rendered.as_bytes()).as_bytes()[..8]
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
}

/// The shape key of one generic body under one substitution: every
/// recorded type's shape, over the substituted type the discovery
/// resolved it to.
fn instance_key(
    found: &mut Discovery<'_, '_>,
    subst: &Subst,
    body: &[Ty],
) -> Result<Rc<InstanceKey>, LowerError> {
    let deinits = found.deinits.clone();
    let mut probe = Shapes::new(found.analyzed, found.types, deinits);
    let mut keys = Vec::with_capacity(body.len());
    for &ty in body {
        let ty = found.types.substituted(subst, ty).unwrap_or(ty);
        keys.push(probe.type_key(ty)?);
    }
    Ok(Rc::from(keys))
}
