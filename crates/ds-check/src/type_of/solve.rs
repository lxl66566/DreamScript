//! Type solving: written types solved against the current type-variable scopes,
//! the generic stacks and fresh variables that scope them, and trait
//! satisfaction with its bound evidence and instance resolutions.

use super::*;

impl Bodies<'_> {
    /// Solves a written type in the current scopes: type variables
    /// innermost-first, then package atoms, aliases, and traits.
    pub(super) fn solve_here(&mut self, ty: &Type) -> Option<Ty> {
        self.solve_reported = false;
        self.solve_with(ty, &FxHashMap::default(), &[])
    }

    /// The qualified `type::label` solved as a type: the head's
    /// own members carrying the label name the member's type. None
    /// reports unknown-member naming the head; several report the
    /// ambiguity naming both declarations; a head that does not solve
    /// keeps the quiet solve refusal.
    pub(super) fn solve_qualified(
        &mut self,
        head: &Ident,
        tail: &Ident,
        subst: &FxHashMap<String, Ty>,
        generics: &[GenericParam],
    ) -> Option<Ty> {
        let head_path = Path {
            span: head.span,
            segments: vec![head.clone()],
        };
        let head_ty = Type::Path {
            span: head.span,
            path: head_path,
            args: Vec::new(),
            delim: Delim::Bracket,
        };
        let solved = self.solve_with(&head_ty, subst, generics)?;
        let labeled = self.members_labeled(solved, &tail.name);
        match labeled.as_slice() {
            [one] => self.decls.universe.atom_ty(*one).ok(),
            [] => {
                self.error(
                    ErrorCode::UnknownTagMember,
                    format!(
                        "the qualified `{}::{}` names no member of `{}` [F9.5]",
                        head.name,
                        tail.name,
                        self.decls.universe.render(solved)
                    ),
                    tail.span,
                    "unknown member",
                );
                self.solve_reported = true;
                None
            },
            many => {
                let mut diag = Diagnostic::error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "the qualified `{}::{}` matches several members of `{}` [F9.5]; name a \
                         head that carries one",
                        head.name,
                        tail.name,
                        self.decls.universe.render(solved)
                    ),
                )
                .with_primary(tail.span, "ambiguous member");
                for atom in many {
                    if let Some(decl) = self.decls.defs_of_atoms.get(atom) {
                        diag = diag.with_secondary(
                            self.package.def(*decl).span,
                            "one declaration of the label",
                        );
                    }
                }
                self.diags.push(diag);
                self.solve_reported = true;
                None
            },
        }
    }

    pub(super) fn solve_with(
        &mut self,
        ty: &Type,
        subst: &FxHashMap<String, Ty>,
        generics: &[GenericParam],
    ) -> Option<Ty> {
        match ty {
            Type::Path {
                span, path, args, ..
            } => {
                let [only] = path.segments.as_slice() else {
                    // The qualified `type::label`: the head's own
                    // members carrying the label name the member's type;
                    // none reports unknown-member and several the
                    // ambiguity, both keyed to the qualified spelling.
                    if let [head, tail] = path.segments.as_slice() {
                        return self.solve_qualified(head, tail, subst, generics);
                    }
                    return None;
                };
                // The callee's substitution binds first: a called function's
                // own variables must not be captured by the caller's
                // in-scope variables of the same name.
                if let Some(ty) = subst.get(&only.name) {
                    return Some(*ty);
                }
                for scope in self.type_vars.iter().rev() {
                    if let Some(ty) = scope.get(&only.name) {
                        return Some(*ty);
                    }
                }
                if generics.iter().any(|g| g.name == only.name) {
                    // An unbound callee variable: mint a fresh atom so the
                    // solve still produces a Ty; leaks are diagnosed at
                    // the call.
                    let var = self.mint_var(&only.name);
                    return self.decls.universe.atom_ty(var).ok();
                }
                if only.name == "none" && !self.package.declares("none") {
                    return Some(self.none_ty());
                }
                if only.name == "self" && !self.package.declares("self") {
                    // `self` is no type name: resolve reports DS2015, so
                    // suppress the generic fallbacks.
                    self.solve_reported = true;
                    return None;
                }
                // a case label keeps its type-side reading (the directed
                // annotations, unions, and `is` tests ride it); only the
                // shared type NAMESPACE exited, so a struct of the same
                // label still wins its own entry here
                let def = self
                    .type_def_of(&only.name)
                    .or_else(|| self.case_def_of(&only.name))
                    .or_else(|| self.decls.foreign_type(&only.name))?;
                if !args.is_empty() {
                    if let Err(mismatch) =
                        decls::check_application(self.package, def, args.len(), self.decls)
                    {
                        self.error(
                            ErrorCode::TypeApplicationMismatch,
                            mismatch.message(&only.name),
                            *span,
                            "type application",
                        );
                        self.solve_reported = true;
                        return None;
                    }
                    let mut arg_tys = Vec::with_capacity(args.len());
                    for arg in args {
                        arg_tys.push(self.solve_with(arg, subst, generics)?);
                    }
                    let ctor = self.decls.atoms.get(&def).copied()?;
                    let apply = self.decls.universe.apply_atom(ctor, &arg_tys).ok()?;
                    return self.decls.universe.atom_ty(apply).ok();
                }
                match self.package.def(def).kind {
                    DefKind::Tag | DefKind::Struct | DefKind::Case => {
                        let atom = self.decls.atoms.get(&def).copied()?;
                        self.decls.universe.atom_ty(atom).ok()
                    },
                    DefKind::Enum => {
                        // A generic enum keeps its opaque atom; a
                        // non-generic one names the union of its minted
                        // case atoms — a linked enum's case defs
                        // answer through the name index the linked
                        // install filled. A zero-case enum names the
                        // empty set: uninhabited, still its own type, so
                        // impl heads and annotations over it check.
                        if let Some(&atom) = self.decls.atoms.get(&def) {
                            return self.decls.universe.atom_ty(atom).ok();
                        }
                        let shape = self.decls.enums.get(&def)?.clone();
                        if !shape.params.is_empty() {
                            return None;
                        }
                        // the case labels are the enum's own written
                        // members: they resolve under the enum's
                        // declaring file, never the use site's shadow
                        // map — the file restores after the whole loop,
                        // so every case reads the declaring file's map
                        let decl_file = self.file;
                        self.file = Some(self.package.def(def).span.file);
                        let mut union: Option<Ty> = None;
                        for (name, ..) in &shape.cases {
                            let case_def = self
                                .case_def_of(name)
                                .or_else(|| self.decls.foreign_type(name));
                            let case_def = case_def?;
                            let atom = self.decls.atoms.get(&case_def).copied()?;
                            let ty = self.decls.universe.atom_ty(atom).ok()?;
                            union = Some(match union {
                                Some(acc) => self.decls.universe.union(acc, ty),
                                None => ty,
                            });
                        }
                        self.file = decl_file;
                        // A zero-case enum names the empty set.
                        Some(union.unwrap_or_else(|| self.empty()))
                    },
                    DefKind::TypeAlias => {
                        let (params, body) = self.decls.aliases.get(&def)?.clone();
                        if params.is_empty() {
                            // the body is the alias declaration's own
                            // text: its tag labels resolve under the
                            // alias's declaring file, never the
                            // use site's shadow map
                            let decl_file = self.file;
                            self.file = Some(self.package.def(def).span.file);
                            let solved = self.solve_with(&body, subst, generics);
                            self.file = decl_file;
                            solved
                        } else {
                            None // generic aliases resolve through use-site args
                        }
                    },
                    DefKind::Trait => {
                        let trait_id = self.decls.traits.get(&def).copied()?;
                        self.decls
                            .universe
                            .solve(&TypeExpr::trait_ref(trait_id))
                            .ok()
                    },
                    DefKind::Fn | DefKind::Const | DefKind::Var => None,
                }
            },
            Type::Optional { inner, .. } => {
                let inner = self.solve_with(inner, subst, generics)?;
                Some(self.decls.universe.optional(inner))
            },
            Type::Negation { inner, .. } => {
                let Type::Path { path, .. } = inner.as_ref() else {
                    return None
                };
                let [only] = path.segments.as_slice() else {
                    return None
                };
                let def = self.package.lookup_type(&only.name)?;
                let trait_id = self.decls.traits.get(&def).copied()?;
                self.decls.universe.solve(&TypeExpr::not(trait_id)).ok()
            },
            Type::Paren { inner, .. } => self.solve_with(inner, subst, generics),
            Type::Union { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.union(a, b))
            },
            Type::Intersect { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.inter(a, b))
            },
            Type::Difference { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.diff(a, b))
            },
            Type::Tuple { items, .. } => {
                let mut elements = Vec::with_capacity(items.len());
                for item in items {
                    elements.push(self.solve_with(item, subst, generics)?);
                }
                match self.decls.universe.product(&elements) {
                    Ok(ty) => Some(ty),
                    Err(err @ Error::TupleDistributionCap { .. }) => {
                        self.error(
                            ErrorCode::TupleDistributionCap,
                            err.to_string(),
                            ty.span(),
                            "tuple distribution cap",
                        );
                        self.solve_reported = true;
                        None
                    },
                    Err(_) => None,
                }
            },
            Type::Unit(_) => Some(self.unit()),
            Type::Fn { params, ret, .. } => {
                let mut param_tys = Vec::with_capacity(params.len());
                for param in params {
                    // A bare fn-type parameter (`fn(t) -> r`) names the
                    // in-scope variable, per the surface spelling.
                    let written = param
                        .ty
                        .clone()
                        .unwrap_or_else(|| name_type(&param.name.name));
                    param_tys.push(self.solve_with(&written, subst, generics)?);
                }
                let ret_ty = match ret {
                    Some(ret) => self.solve_with(ret, subst, generics)?,
                    None => self.unit(),
                };
                let atom = self.decls.universe.fn_atom(&param_tys, ret_ty).ok()?;
                self.decls.universe.atom_ty(atom).ok()
            },
            // The `type` keyword solves to the builtin comptime type-value
            // atom: the comptime evaluator's staged reflection
            // indices carry it, and nothing constructs its values at
            // runtime.
            Type::TypeKw(_) => self.decls.universe.atom_ty(TypeId::TYPE).ok(),
            Type::Ptr { .. } | Type::Foreign { .. } => None,
        }
    }

    /// Structural parameter-to-argument matching for generic
    /// instantiation: binds callee variables by walking parameter shapes
    /// against the argument's solved type.
    pub(super) fn match_param(
        &mut self,
        param: &Type,
        arg: Ty,
        generics: &[GenericParam],
        subst: &mut FxHashMap<String, Ty>,
    ) {
        match param {
            Type::Paren { inner, .. } => self.match_param(inner, arg, generics, subst),
            Type::Path { path, args, .. } => {
                let [only] = path.segments.as_slice() else {
                    return
                };
                if args.is_empty() {
                    if generics.iter().any(|g| g.name == only.name) {
                        subst.entry(only.name.clone()).or_insert(arg);
                    }
                    return;
                }
                let members = self.decls.universe.members(arg);
                let Some(single) = members.first().filter(|_| members.len() == 1) else {
                    return;
                };
                let Some((ctor, apply_args)) = self.single_apply_of(*single) else {
                    return
                };
                // The parameter's constructor must be the argument's.
                let Some(def) = self.package.lookup_type(&only.name) else {
                    return
                };
                if self.decls.atoms.get(&def) != Some(&ctor) {
                    return;
                }
                for (param_arg, arg_ty) in args.iter().zip(apply_args.iter()) {
                    self.match_param(param_arg, *arg_ty, generics, subst);
                }
            },
            Type::Optional { inner, .. } => {
                let none = self.none_ty();
                let without_none = self.decls.universe.diff(arg, none);
                self.match_param(inner, without_none, generics, subst);
            },
            Type::Tuple { items, .. } => {
                // Tuple elements solve and mint through the universe, so the
                // member set is read by index across the walk.
                let len = self.decls.universe.members(arg).len();
                for index in 0..len {
                    let member = self.decls.universe.members(arg)[index];
                    if let Some(elements) = self.tuple_elements(member) {
                        if elements.len() != items.len() {
                            continue;
                        }
                        for (item, element) in items.iter().zip(elements.iter()) {
                            if let Ok(ty) = self.decls.universe.atom_ty(*element) {
                                self.match_param(item, ty, generics, subst);
                            }
                        }
                    }
                }
            },
            Type::Fn { params, ret, .. } => {
                let members = self.decls.universe.members(arg);
                let Some(single) = members.first().filter(|_| members.len() == 1) else {
                    return
                };
                let Some((param_tys, ret_ty)) = self.decompose_fn_atom(*single) else {
                    return
                };
                for (param, arg_ty) in params.iter().zip(param_tys.iter()) {
                    if let Some(ty) = &param.ty {
                        self.match_param(ty, *arg_ty, generics, subst);
                    }
                }
                if let Some(ret) = ret {
                    self.match_param(ret, ret_ty, generics, subst);
                }
            },
            _ => {},
        }
    }

    pub(super) fn trait_of_type(&self, ty: &Type) -> Option<TraitId> {
        let Type::Path { path, .. } = ty else {
            return None
        };
        let [only] = path.segments.as_slice() else {
            return None
        };
        let def = self.package.lookup_type(&only.name)?;
        self.decls.traits.get(&def).copied()
    }

    /// The trait arms one written bound carries: a bare trait path, or
    /// every arm of an intersection — a derive's synthesized bound
    /// intersects the family's trait with whatever the declaration
    /// wrote, and a hand-written intersection spells the same shape.
    pub(super) fn bound_traits(&self, ty: &Type, out: &mut Vec<TraitId>) {
        match ty {
            Type::Intersect { left, right, .. } => {
                self.bound_traits(left, out);
                self.bound_traits(right, out);
            },
            other => {
                if let Some(trait_id) = self.trait_of_type(other) {
                    out.push(trait_id);
                }
            },
        }
    }

    /// Whether a type is exactly one minted type variable (the checker's
    /// `$name` atoms): an inference hole, not a data type.
    pub(super) fn is_var_ty(&self, ty: Ty) -> bool {
        match self.decls.universe.members(ty) {
            [only] => self
                .decls
                .universe
                .atom_decl(*only)
                .is_ok_and(|decl| decl.name.starts_with('$')),
            _ => false,
        }
    }

    /// The runtime evidence one inherent call site must stage for the
    /// impl's declared bounds: the head pattern substitutes against
    /// the receiver, and every bound must hold for the substituted type
    /// exactly as the trait form's instance bounds do — an in-scope
    /// assumption satisfies the checker while the record keeps `None`
    /// (the lowerer refuses that site naming the staging seam). A head
    /// that does not unify (a union receiver solving the variables
    /// differently) records nothing.
    pub(super) fn inherent_evidence(
        &mut self,
        head: Ty,
        conditions: &[Condition<Ty>],
        recv: Ty,
        span: Span,
    ) -> InherentCallBounds {
        let targets = crate::instance_solve::inherent_bound_targets(
            &mut self.decls.universe,
            head,
            conditions,
            recv,
        );
        let mut out = Vec::new();
        for (var, trait_id, target) in targets {
            if self.decls.universe.is_empty(target) {
                continue; // a cascading argument error's empty type
            }
            let served = if self.trait_satisfied(trait_id, target, span) {
                self.instance_resolution(trait_id, target).solved()
            } else {
                None
            };
            out.push((var, trait_id, served));
        }
        out
    }

    /// Whether a trait holds for a type: an in-scope assumption covers
    /// it, or the environment resolves an instance — a containing head
    /// for concrete receivers, or a generic head that unifies with the
    /// receiver under provable bounds. Diagnoses on failure.
    pub(super) fn trait_satisfied(&mut self, trait_id: TraitId, ty: Ty, span: Span) -> bool {
        if self.assumption_covers(trait_id, ty) {
            return true;
        }
        match self.instance_resolution(trait_id, ty) {
            Resolution::Unique(_) | Resolution::Covered(_) => true,
            Resolution::Ambiguous(candidates) => {
                self.instance_ambiguity(trait_id, ty, &candidates, span);
                false
            },
            Resolution::None => {
                self.error(
                    ErrorCode::UnsatisfiedConstraint,
                    format!(
                        "no instance of `{}` for `{}`",
                        self.trait_name(trait_id),
                        self.decls.universe.render(ty)
                    ),
                    span,
                    "unimplemented trait",
                );
                false
            },
            Resolution::BoundUnsat { bound, target, .. } => {
                self.report_bound_miss(trait_id, bound, &target, span);
                false
            },
        }
    }

    /// The bound-miss diagnostic both call spellings share: the head
    /// matched and the substituted bound is the missing proof, so the
    /// message names the bound's trait over the substituted type.
    pub(super) fn report_bound_miss(
        &mut self,
        trait_id: TraitId,
        bound: TraitId,
        target: &[TypeId],
        span: Span,
    ) {
        let miss = self.render_members(target);
        self.error(
            ErrorCode::UnsatisfiedConstraint,
            format!(
                "no instance of `{}` for {}: the `{}` impl's bound is unsatisfied there",
                self.trait_name(bound),
                miss,
                self.trait_name(trait_id)
            ),
            span,
            "unsatisfied bound",
        );
    }

    /// Whether an in-scope generic bound (`[t: json]`) assumes the trait
    /// for the type.
    pub(super) fn assumption_covers(&self, trait_id: TraitId, ty: Ty) -> bool {
        self.assumptions
            .iter()
            .rev()
            .any(|(t, assumed)| *t == trait_id && self.decls.universe.contains(*assumed, ty))
    }

    /// The environment's call-site resolution: the instance whose
    /// head contains the receiver, or — for a generic impl's head — the
    /// one whose pattern unifies with it under provable bounds. The
    /// solved instance is the declared instance; a per-substitution
    /// instance is never minted, because the call's own receiver type
    /// (recorded beside it) carries the substitution the dispatch seam
    /// needs.
    pub(super) fn instance_resolution(&self, trait_id: TraitId, ty: Ty) -> Resolution {
        self.instance_in_slot(trait_id, ty, None)
    }

    /// The resolution over one vocabulary slot: `None` scans the unnamed
    /// default rows (every plain call spelling), `Some(name)` the impl
    /// naming itself (a selected call). A named instance never answers an
    /// unselected call, and an unnamed one never answers a selected one.
    pub(super) fn instance_in_slot(
        &self,
        trait_id: TraitId,
        ty: Ty,
        slot: Option<&str>,
    ) -> Resolution {
        crate::instance_solve::resolve(
            &self.decls.universe,
            &self.decls.instances,
            &self.assumptions,
            trait_id,
            ty,
            slot,
        )
    }

    /// Several impls serve one call site: declared heads are
    /// pairwise disjoint, but a generic head is opaque to the set
    /// algebra, so two generic impls of one trait can both serve one
    /// receiver. A compile error, never a priority order.
    pub(super) fn instance_ambiguity(
        &mut self,
        trait_id: TraitId,
        recv: Ty,
        candidates: &[InstanceId],
        span: Span,
    ) {
        let mut diag = Diagnostic::error(
            ErrorCode::AmbiguousInstance,
            format!(
                "{} impls of `{}` accept receiver `{}`; dispatch must pick one [B6]",
                candidates.len(),
                self.trait_name(trait_id),
                self.decls.universe.render(recv)
            ),
        )
        .with_primary(span, "ambiguous dispatch");
        for &id in candidates {
            if let (Ok(head), Some(site)) = (
                self.decls.instances.head(id),
                self.decls.instance_impls.get(&id),
            ) {
                diag = diag.with_secondary(
                    *site,
                    format!("instance head `{}`", self.decls.universe.render(head)),
                );
            }
        }
        self.diags.push(diag);
    }

    pub(super) fn trait_name(&self, trait_id: TraitId) -> String {
        self.decls
            .universe
            .trait_decl(trait_id)
            .map_or_else(|_| "<unknown>".to_owned(), |d| d.name.clone())
    }

    pub(super) fn push_generics(&mut self, generics: &[GenericParam]) {
        let mut scope = FxHashMap::default();
        self.assumption_marks.push(self.assumptions.len());
        for param in generics {
            let var = self.mint_var(&param.name);
            if let Ok(ty) = self.decls.universe.atom_ty(var) {
                scope.insert(param.name.clone(), ty);
                if param.bound.as_ref().is_some_and(decls::linear_marker) {
                    self.linear_vars.insert(var, false);
                }
                if let Some(bound) = &param.bound {
                    let mut arms = Vec::new();
                    self.bound_traits(bound, &mut arms);
                    for trait_id in arms {
                        self.assumptions.push((trait_id, ty));
                    }
                }
            }
        }
        self.type_vars.push(scope);
    }

    pub(super) fn pop_generics(&mut self) {
        self.type_vars.pop();
        if let Some(mark) = self.assumption_marks.pop() {
            self.assumptions.truncate(mark);
        }
    }

    pub(super) fn mint_var(&mut self, name: &str) -> TypeId {
        self.decls.var_counter += 1;
        let unique = format!("${name}#{}", self.decls.var_counter);
        // Impossible by construction: surface names cannot contain `$` or
        // `#`, and both minting sites (instance declaration and body
        // checking) draw from the shared var_counter, so the minted name
        // is always fresh — a failure would be a silently wrong type, so
        // it panics instead of falling back to NONE.
        self.decls
            .universe
            .declare_tag(&unique)
            .expect("minted variable names are unique by the shared counter")
    }

    pub(super) fn fresh_var(&mut self, name: &str) -> Ty {
        let var = self.mint_var(name);
        self.decls
            .universe
            .atom_ty(var)
            .unwrap_or_else(|_| self.empty())
    }
}
