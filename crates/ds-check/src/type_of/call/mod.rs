//! Call checking: the `Call` form's per-kind routing, the package-definition
//! call with its generic instantiation and named/default argument slots, the
//! routed answers a lambda argument settles, the hole predicates, and the
//! argument staging into signature slots.

use super::*;

mod select;

impl Bodies<'_> {
    pub(super) fn call(&mut self, e: &Expr) -> Val {
        let Expr::Call {
            callee,
            args,
            trailing,
            selection,
            span,
        } = e
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let callee = peel_paren(callee);
        let selection = selection.as_ref();
        // Inside a `for comptime` copy the plan-time dispatch records
        // are copy-dependent facts: member and generic dispatch queue
        // with the walker round, while free monomorphic calls (whose
        // records the copy shares) keep lowering.
        if !self.comptime_path.is_empty() {
            let dispatch = match peel_paren(callee) {
                Expr::Field { .. } => true,
                Expr::Path { path, .. } => match path.segments.as_slice() {
                    [only] => match self.res.get(only.span) {
                        Some(Res::Def(def)) => self
                            .decls
                            .fns
                            .get(def)
                            .is_some_and(|sig| !sig.generics.is_empty()),
                        Some(Res::TraitMethod(_) | Res::Dot(_)) => true,
                        _ => false,
                    },
                    _ => true,
                },
                _ => true,
            };
            if dispatch {
                self.error(
                    ErrorCode::InferenceFailed,
                    "a member or generic dispatch inside a `for comptime` copy queues with the \
                     walker round; its per-copy evidence has no plan record yet"
                        .to_owned(),
                    callee.span(),
                    "dispatch inside a comptime copy",
                );
                for arg in args {
                    self.expr(&arg.value, None);
                }
                if let Some(tail) = trailing {
                    self.expr(tail, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
        }
        if let Expr::Field { recv, name, .. } = callee {
            return self.dot_call(recv, name, args, trailing.as_deref(), *span, selection);
        }
        // A generic-application constructor `list[t]()`: the callee
        // head resolves in the type namespace.
        if let Expr::Index { recv, index, .. } = callee
            && let Some(val) = self.ctor_apply_call(recv, index, args, trailing.as_deref(), *span)
        {
            if let Some(selection) = selection {
                self.selection_without_dispatch(
                    selection,
                    *span,
                    "a constructor application dispatches through no trait instance",
                );
            }
            return val;
        }
        // The map constructor `map[k, v]()`: the head resolved
        // in the type namespace.
        if let Expr::MapApply {
            head, key, value, ..
        } = callee
        {
            if let Some(selection) = selection {
                self.selection_without_dispatch(
                    selection,
                    *span,
                    "a map constructor dispatches through no trait instance",
                );
            }
            return self.map_ctor_call(head, key, value, args, trailing.as_deref(), *span);
        }
        if let Expr::Path { path, .. } = callee {
            match path.segments.as_slice() {
                [only] => {
                    if let Some(Res::Def(def)) = self.res.get(only.span) {
                        let def = *def;
                        if let Some(selection) = selection {
                            self.selection_without_dispatch(
                                selection,
                                *span,
                                "the callee is a package definition, not a trait dispatch",
                            );
                        }
                        if self.decls.fns.contains_key(&def) {
                            let val = self.call_def(def, None, args, trailing.as_deref(), *span);
                            return self.spawn_cancel_refines(def, val);
                        }
                        // A minted case constructs by call form; a
                        // bare case falls through to the tag error below.
                        if let Some(case) = self.decls.cases.get(&def)
                            && !matches!(case.payload, EnumPayload::None)
                        {
                            return self.case_call(def, only, args, trailing.as_deref(), *span);
                        }
                        // A payloaded tag constructs by call form too: the
                        // same named-field slots over its own struct shape.
                        if self.package.def(def).kind == DefKind::Tag
                            && self.decls.structs.contains_key(&def)
                        {
                            return self.case_call(def, only, args, trailing.as_deref(), *span);
                        }
                        if self.decls.structs.contains_key(&def)
                            || self.decls.atoms.contains_key(&def)
                        {
                            self.error(
                                ErrorCode::InferenceFailed,
                                format!(
                                    "`{}` is a type; construct structs with `name {{ ... }}` and \
                                     tags by name",
                                    only.name
                                ),
                                only.span,
                                "type called as a function",
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
                        let callee_val = self.def_value(def, only.span, None);
                        return self.indirect_call(callee_val, args, trailing.as_deref(), *span);
                    }
                },
                [_head, method] => {
                    if let Some(Res::TraitMethod(method_id)) = self.res.get(method.span) {
                        return self.qualified_call(
                            *method_id,
                            args,
                            trailing.as_deref(),
                            *span,
                            selection,
                        );
                    }
                    // The qualified label's call form: a payloaded
                    // tag or case constructs through the same case-call
                    // door as the bare spelling; a unit label keeps the
                    // bare-case refusal there.
                    if let Some(Res::Def(def)) = self.res.get(method.span)
                        && matches!(self.package.def(*def).kind, DefKind::Tag | DefKind::Case)
                    {
                        let def = *def;
                        let payloaded = self
                            .decls
                            .cases
                            .get(&def)
                            .is_some_and(|case| !matches!(case.payload, EnumPayload::None))
                            || self.decls.structs.contains_key(&def);
                        if let Some(selection) = selection {
                            self.selection_without_dispatch(
                                selection,
                                *span,
                                "the qualified label is a constructor, not a trait dispatch",
                            );
                        }
                        if payloaded {
                            return self.case_call(def, method, args, trailing.as_deref(), *span);
                        }
                        self.error(
                            ErrorCode::InferenceFailed,
                            format!("`{}` is a bare label; it constructs by name", method.name),
                            method.span,
                            "unit label called as a function",
                        );
                        return Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        };
                    }
                },
                _ => {},
            }
        }
        if let Some(selection) = selection {
            self.selection_without_dispatch(
                selection,
                *span,
                "an indirect callee dispatches through no trait instance",
            );
        }
        let callee_val = self.expr(callee, None);
        self.indirect_call(callee_val, args, trailing.as_deref(), *span)
    }

    /// Constructs one minted enum case by call form: positional
    /// arguments fill tuple-payload slots and named arguments fill
    /// named-payload fields; the result is the case's own atom set.
    pub(super) fn case_call(
        &mut self,
        def: DefId,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let payload = if let Some(case) = self.decls.cases.get(&def) {
            case.payload.clone()
        } else if let Some(shape) = self.decls.structs.get(&def) {
            // a payloaded tag's slots: its own struct shape
            EnumPayload::Struct(shape.fields.clone())
        } else {
            EnumPayload::None
        };
        if trailing.is_some() {
            self.error(
                ErrorCode::ArityMismatch,
                format!("enum case `{}` takes no trailing block", name.name),
                span,
                "trailing block on a case construction",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let case_name = self.package.def(def).name.clone();
        let slots: Vec<(Option<String>, Span, Type)> = match &payload {
            EnumPayload::Tuple(types) => {
                types.iter().map(|t| (None, name.span, t.clone())).collect()
            },
            EnumPayload::Struct(fields) => fields
                .iter()
                .map(|f| (Some(f.name.name.clone()), f.name.span, f.ty.clone()))
                .collect(),
            // A bare case constructs by name like a unit tag; the call
            // site routes here only after the payload check, so this is
            // the belt behind it.
            EnumPayload::None => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` is a bare case; it constructs by name", name.name),
                    name.span,
                    "unit case called as a function",
                );
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            },
        };
        let mut written = vec![false; slots.len()];
        let mut broken = false;
        let mut positional = 0usize;
        for arg in args {
            let slot = if let Some(arg_name) = &arg.name {
                let index = slots
                    .iter()
                    .position(|(field, ..)| field.as_deref().is_some_and(|f| f == arg_name.name));
                let Some(index) = index else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("enum case `{case_name}` has no field `{}`", arg_name.name),
                        arg_name.span,
                        "no such field",
                    );
                    self.expr(&arg.value, None);
                    broken = true;
                    continue;
                };
                index
            } else {
                let index = positional;
                positional += 1;
                let Some(_slot) = slots.get(index) else {
                    self.error(
                        ErrorCode::ArityMismatch,
                        format!("enum case `{case_name}` takes {} argument(s)", slots.len()),
                        arg.span,
                        "extra argument",
                    );
                    self.expr(&arg.value, None);
                    continue;
                };
                index
            };
            if written[slot] {
                self.error(
                    ErrorCode::DuplicateDefinition,
                    format!(
                        "field `{}` of `{case_name}` is written twice",
                        slots[slot].0.clone().unwrap_or_default()
                    ),
                    arg.span,
                    "duplicate field",
                );
            }
            written[slot] = true;
            let Some(field_ty) = self.solve_here(&slots[slot].2) else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("a field of `{case_name}` does not solve"),
                        arg.span,
                        "unsolvable field type",
                    );
                }
                self.expr(&arg.value, None);
                continue;
            };
            let val = self.expr(&arg.value, Some(field_ty));
            self.expect_contains(val, field_ty, arg.value.span(), "case field value");
        }
        if !broken {
            for (index, &is_written) in written.iter().enumerate() {
                if is_written {
                    continue;
                }
                match &slots[index].0 {
                    Some(field) => self.error(
                        ErrorCode::TypeMismatch,
                        format!("enum case `{case_name}` misses field `{field}`"),
                        span,
                        "missing field",
                    ),
                    None => self.error(
                        ErrorCode::ArityMismatch,
                        format!("enum case `{case_name}` takes {} argument(s)", slots.len()),
                        span,
                        "missing argument",
                    ),
                }
            }
        }
        // A minted case constructs into its own atom. `none` is the
        // predeclared unit atom, so a payloaded case named `none`
        // claims no atom of its own and the construction refuses at this
        // seam instead of typing the binding empty.
        let Some(ty) = self
            .decls
            .atoms
            .get(&def)
            .and_then(|&a| self.decls.universe.atom_ty(a).ok())
        else {
            self.error(
                ErrorCode::NonePayloadCase,
                "`none` is the predeclared unit atom; a case named `none` cannot carry a payload"
                    .to_owned(),
                name.span,
                "payload on the unit atom",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    pub(super) fn indirect_call(
        &mut self,
        callee: Val,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some((param_tys, ret)) = self.decompose_fn(callee.ty) else {
            // A poisoned callee already reported; "not callable" on top of
            // it would be a cascade. The arguments still walk.
            if callee.poisoned {
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`{}` is not callable; expected one function type",
                    self.decls.universe.render(callee.ty)
                ),
                span,
                "not a function",
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
        let mut all: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        if let Some(tail) = trailing {
            all.push(tail);
        }
        let params: Vec<Option<Ty>> = param_tys.iter().copied().map(Some).collect();
        self.check_args(&all, &params, span);
        Val {
            ty: ret,
            diverges: false,
            poisoned: false,
        }
    }

    /// Calls one package function by definition: instantiates generics by
    /// structural argument matching (one-body generics); named arguments
    /// fill their parameter's slot and literal-only defaults cover
    /// omissions.
    pub(super) fn call_def(
        &mut self,
        def: DefId,
        fed: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some(sig) = self.decls.fns.get(&def).cloned() else {
            // The callee resolved in the value namespace, so it can
            // be a plain const or var binding (a dot-call's free tier); a
            // call against it is a call-shape error with the receiver and
            // every argument walked, never a silently empty type.
            let name = self.package.def(def).name.clone();
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{name}` is a value binding, not a function"),
                span,
                "not callable",
            );
            if let Some(recv) = fed {
                self.expr(recv, None);
            }
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        // The extraction family types by rule against the checked
        // receiver, never through generic inference: an inferred `t?`
        // parameter would bind a plain `t` receiver, and the fallback's
        // width meeting must be diagnosed, not assumed.
        if let Some(kind) = self.extraction_kind(def, &sig) {
            return self.extraction_call(kind, fed, args, trailing, span);
        }
        // The reflection builtins are comptime-context-only: their
        // answers read the compile-time reflection tables, so a call
        // outside a comptime function's body refuses here — the wiring
        // is by name, the builtin admission criterion.
        let callee_name = self.package.def(def).name.clone();
        if matches!(callee_name.as_str(), "fields" | "type_name") && !self.comptime.ctx {
            self.error(
                ErrorCode::ComptimeValueOutside,
                format!(
                    "`{callee_name}` answers reflection at compile time only; call it inside a \
                     comptime function [G2]"
                ),
                span,
                "reflection outside comptime",
            );
            if let Some(recv) = fed {
                self.expr(recv, None);
            }
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let slots = self.arg_slots(fed, 0, args, trailing, &sig.params, span);
        let Some(slots) = slots else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        self.check_mut_window_args(&slots, &sig.params);
        // A comptime call's arguments name types: the type-value
        // reading opens for exactly these argument positions.
        let outer = self.comptime;
        self.comptime.args = sig
            .params
            .iter()
            .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)));
        if sig.generics.is_empty() {
            let mut param_tys = Vec::with_capacity(sig.params.len());
            for param in &sig.params {
                if let Some(solved) = param.ty.as_ref().and_then(|t| self.solve_here(t)) {
                    param_tys.push(Some(solved));
                } else {
                    if !self.take_solve_reported() {
                        self.error(
                            ErrorCode::InferenceFailed,
                            "a parameter type does not solve; annotate it [B2]".to_owned(),
                            span,
                            "unannotated parameter",
                        );
                    }
                    param_tys.push(None);
                }
            }
            // The site's solved parameter types for the call-site
            // staging's widening family: recorded when every parameter
            // solves, so a union parameter's argument re-spans into the
            // parameter's whole region whatever member the caller wrote.
            if param_tys.iter().all(Option::is_some) {
                self.call_params
                    .insert(span, param_tys.iter().filter_map(|ty| *ty).collect());
            }
            let ret = sig
                .ret
                .as_ref()
                .and_then(|r| self.solve_here(r))
                .unwrap_or_else(|| self.unit());
            self.check_slots(&slots, &sig.params, &param_tys);
            self.comptime = outer;
            return Val {
                ty: ret,
                diverges: false,
                poisoned: false,
            };
        }
        self.comptime = outer;
        // Generic: mint fresh variables, bind non-lambda arguments by
        // structural matching, check lambda arguments against their
        // fn-typed parameters (binding return variables), then solve the
        // return under the substitution. The substitution (not a scope)
        // carries the variables, so the caller's in-scope variables never
        // capture the callee's.
        let var_atoms: Vec<TypeId> = sig
            .generics
            .iter()
            .map(|p| self.mint_var(&p.name))
            .collect();
        let mut subst: FxHashMap<String, Ty> = FxHashMap::default();
        let assumption_mark = self.assumptions.len();
        for (index, param) in sig.generics.iter().enumerate() {
            if let Ok(ty) = self.decls.universe.atom_ty(var_atoms[index]) {
                if param.bound.as_ref().is_some_and(decls::linear_marker) {
                    self.linear_vars.insert(var_atoms[index], false);
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
        // Pass one: non-lambda arguments bind variables structurally,
        // then check against the solved parameter type: a
        // constructor mismatch binds nothing silently otherwise.
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            let (Some(arg), Some(param_ty)) = (slot, param.ty.as_ref()) else {
                continue;
            };
            if matches!(peel_paren(arg), Expr::Lambda { .. }) && matches!(param_ty, Type::Fn { .. })
            {
                continue;
            }
            let arg_val = self.expr(arg, None);
            self.match_param(param_ty, arg_val.ty, &sig.generics, &mut subst);
            if let Some(expected) = self.solve_with(param_ty, &subst, &sig.generics) {
                self.expect_param_contains(arg_val, param_ty, expected, arg.span());
            }
        }
        // Pass two: lambda arguments check against the fn-typed parameter,
        // and their result type binds the parameter's return variables. A
        // written return still carrying an unsolved variable is the
        // instantiation's inference hole: the lambda takes no return
        // expectation, its body infers its own answer, and the written
        // return unifies against that answer by first-order pattern match
        // — an application strips to its arguments and a variable binds
        // directly. General unification stays out: a variable the match
        // cannot reach binds nothing, and the settled check below names
        // the variable no argument fixed. A routed lambda waits for every
        // argument's binding turn before it is judged.
        let mut routed: Vec<(&Type, &Type, Val, Span)> = Vec::new();
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            let Some(
                param_ty @ Type::Fn {
                    params: fn_params,
                    ret: fn_ret,
                    ..
                },
            ) = &param.ty
            else {
                continue;
            };
            let Some(arg) = slot else { continue };
            if !matches!(peel_paren(arg), Expr::Lambda { .. }) {
                continue;
            }
            let mut param_tys = Vec::with_capacity(fn_params.len());
            for p in fn_params {
                let written = p.ty.clone().unwrap_or_else(|| name_type(&p.name.name));
                let solved = self.solve_with(&written, &subst, &sig.generics);
                param_tys.push(match solved {
                    Some(ty) => ty,
                    None => self.fresh_var(&p.name.name),
                });
            }
            let written_ret = fn_ret.as_ref();
            let mut expected_ret = written_ret
                .and_then(|r| self.solve_with(r, &subst, &sig.generics))
                .unwrap_or_else(|| self.empty());
            let inferred_ret = self.has_hole(expected_ret);
            if inferred_ret {
                // A bare variable marks the return as the hole the body
                // solves, never an expectation its answer must meet.
                expected_ret = self.fresh_var("ret");
            }
            let params_boxed: Box<[Ty]> = param_tys.into();
            let expected_ty = self
                .decls
                .universe
                .fn_atom(&params_boxed, expected_ret)
                .ok()
                .and_then(|a| self.decls.universe.atom_ty(a).ok())
                .unwrap_or_else(|| self.empty());
            // The lambda's expectation cites the callee's written fn-typed
            // parameter when the lambda's own arity refuses.
            self.expected_ty_span = param.ty.as_ref().map(Type::span);
            let val = self.expr(arg, Some(expected_ty));
            self.expected_ty_span = None;
            if let Some(fn_ret) = fn_ret {
                // The lambda's own result type binds the parameter's
                // return variables, not the lambda's function type.
                let result = self.decompose_fn(val.ty).map_or(val.ty, |(_, ret)| ret);
                self.match_param(fn_ret, result, &sig.generics, &mut subst);
            }
            if let (true, Some(fn_ret)) = (inferred_ret, written_ret) {
                routed.push((param_ty, fn_ret, val, arg.span()));
            }
        }
        let mut unsolved = false;
        for (param_ty, written_ret, val, arg_span) in routed {
            unsolved |= self.settle_routed_return(
                param_ty,
                written_ret,
                val,
                arg_span,
                &sig.generics,
                &subst,
                span,
            );
        }
        let ret = match sig.ret.as_ref() {
            Some(ret_ty) => self
                .solve_with(ret_ty, &subst, &sig.generics)
                .unwrap_or_else(|| self.empty()),
            None => self.unit(),
        };
        // Record the instantiation when every generic parameter solved:
        // the lowering pass mints the CALLG evidence words from it.
        // A partially-solved call is an inference failure below and the
        // lowerer rejects the missing record, never a silent `any`.
        let mut solved = Vec::with_capacity(sig.generics.len());
        for param in &sig.generics {
            let Some(ty) = subst.get(&param.name) else {
                solved.clear();
                break;
            };
            solved.push(*ty);
        }
        // A routed lambda that fixed nothing leaves holes in the
        // substitution: no record and no bound proof answers for it.
        if unsolved {
            solved.clear();
        }
        // A solved instantiation answers for its declared bounds:
        // every bound must hold for the solved type, through the
        // instance environment or an in-scope assumption (a caller
        // bound propagates that way). An empty solved type rides a
        // cascading argument error and skips the check. Each answered
        // bound records beside the call: the unique instance serving the
        // solved type (`None` when only an in-scope assumption covers
        // it — the runtime-resolved evidence form) plus the solved type
        // itself, the record the lowering pass stages the body's
        // evidence demands from.
        let mut call_bounds: GenericCallBounds = Vec::new();
        for ((index, param), &ty) in sig.generics.iter().enumerate().zip(solved.iter()) {
            let mut arms = Vec::new();
            if let Some(bound) = &param.bound {
                self.bound_traits(bound, &mut arms);
            }
            for trait_id in arms {
                if self.decls.universe.is_empty(ty) {
                    continue;
                }
                let served = if self.trait_satisfied(trait_id, ty, span) {
                    self.instance_resolution(trait_id, ty).solved()
                } else {
                    None
                };
                call_bounds.push((var_atoms[index], trait_id, served, ty));
            }
        }
        if !call_bounds.is_empty() {
            self.record_generic_bounds(span, call_bounds);
        }
        if !solved.is_empty() {
            self.record_generic_call(def, span, solved);
        }
        // Unbound variables leaking into the result are inference
        // failures: no any fallback. The member set is read by index:
        // reporting mutates the checker, so no borrow of it stays open.
        let ret_len = self.decls.universe.members(ret).len();
        for index in 0..ret_len {
            let atom = self.decls.universe.members(ret)[index];
            if let Some(pos) = var_atoms.iter().position(|v| *v == atom) {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "cannot infer `{}` from this call's arguments [B2]",
                        sig.generics[pos].name
                    ),
                    span,
                    "unbound type variable",
                );
            }
        }
        self.assumptions.truncate(assumption_mark);
        Val {
            ty: ret,
            diverges: false,
            poisoned: unsolved,
        }
    }

    /// One lambda argument that took no return expectation, judged once
    /// every argument had its binding turn: the written return unified
    /// against the lambda's answer by first-order pattern match, and the
    /// solved parameter type is the contract. A concrete answer under
    /// a written return still holding an unsolved variable is the match
    /// failing on shape; otherwise a variable the parameter mentions that
    /// stayed unsolved names what no argument fixed. Answers whether this
    /// reported, so the call's result carries the poison and no
    /// instantiation records.
    pub(super) fn settle_routed_return(
        &mut self,
        param: &Type,
        fn_ret: &Type,
        val: Val,
        arg_span: Span,
        generics: &[GenericParam],
        subst: &FxHashMap<String, Ty>,
        span: Span,
    ) -> bool {
        if val.diverges || val.poisoned {
            return false;
        }
        let solved_ret = self.solve_with(fn_ret, subst, generics);
        let answer = self.decompose_fn(val.ty).map(|(_, ret)| ret);
        if let (Some(solved_ret), Some(answer)) = (solved_ret, answer)
            && self.has_hole(solved_ret)
            && !self.has_hole(answer)
            && !self.decls.universe.contains(solved_ret, answer)
        {
            let expected = self.solve_with(param, subst, generics);
            if let Some(expected) = expected
                && !self.decls.universe.is_empty(expected)
                && !self.expect_param_contains(val, param, expected, arg_span)
            {
                return true;
            }
        }
        let unbound: Vec<&str> = generics
            .iter()
            .filter(|generic| {
                type_mentions(param, &generic.name)
                    && subst.get(&generic.name).is_none_or(|ty| self.has_hole(*ty))
            })
            .map(|generic| generic.name.as_str())
            .collect();
        if !unbound.is_empty() {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "cannot infer `{}` from this call's arguments [B2]",
                    unbound.join("`, `")
                ),
                span,
                "unbound type variable",
            );
            return true;
        }
        // Every variable the parameter mentions solved: the solved
        // parameter is the contract, exactly as for any other argument.
        self.solve_with(param, subst, generics)
            .is_some_and(|expected| !self.expect_param_contains(val, param, expected, arg_span))
    }

    /// Whether a solved type still carries an inference hole: a minted
    /// variable no enclosing generic binds (an enclosing body's own type
    /// variables are runtime-evidenced facts, never holes).
    pub(super) fn has_hole(&self, ty: Ty) -> bool {
        self.decls
            .universe
            .members(ty)
            .iter()
            .any(|atom| self.atom_has_hole(*atom))
    }

    pub(super) fn atom_has_hole(&self, atom: TypeId) -> bool {
        match self.decls.universe.atom_decl(atom) {
            Ok(decl) if decl.name.starts_with('$') => !self.type_vars.iter().any(|scope| {
                scope
                    .values()
                    .any(|ty| self.decls.universe.members(*ty) == [atom])
            }),
            Ok(decl) => match &decl.kind {
                AtomKind::Apply(_, args) => args.iter().any(|arg| self.has_hole(*arg)),
                AtomKind::Tuple(elements) => elements.iter().any(|elem| self.atom_has_hole(*elem)),
                AtomKind::FnSig(params, ret) => {
                    params.iter().any(|param| self.has_hole(*param)) || self.has_hole(*ret)
                },
                _ => false,
            },
            Err(_) => false,
        }
    }

    /// Maps call arguments onto signature parameter slots: positional
    /// arguments in order, named arguments by parameter name, the trailing
    /// lambda at the next open position; omissions must carry a literal
    /// default. `None` returns after diagnosing.
    pub(super) fn arg_slots<'x>(
        &mut self,
        fed: Option<&'x Expr>,
        skip: usize,
        args: &'x [ds_parse::CallArg],
        trailing: Option<&'x Expr>,
        params: &[ParamSig],
        span: Span,
    ) -> Option<Vec<Option<&'x Expr>>> {
        let mut slots: Vec<Option<&'x Expr>> = vec![None; params.len()];
        let mut next_positional = skip;
        let mut ok = true;
        // The UFCS-fed receiver occupies the first positional slot.
        if let Some(fed) = fed {
            if slots.is_empty() {
                self.error(
                    ErrorCode::ArityMismatch,
                    "the dot-call receiver has no parameter to feed".to_owned(),
                    span,
                    "extra argument",
                );
                ok = false;
            } else {
                slots[0] = Some(fed);
                next_positional = 1;
            }
        }
        // Leading parameters the caller already handled (a method's
        // receiver) count as filled.
        for arg in args {
            match &arg.name {
                Some(name) => {
                    let value: &'x Expr = &arg.value;
                    match params.iter().position(|p| p.name == name.name) {
                        Some(index) if slots[index].is_none() => slots[index] = Some(value),
                        Some(_) => {
                            self.error(
                                ErrorCode::ArityMismatch,
                                format!("argument `{}` is given twice", name.name),
                                name.span,
                                "duplicate argument",
                            );
                            ok = false;
                        },
                        None => {
                            self.error(
                                ErrorCode::UnresolvedName,
                                format!("no parameter `{}`", name.name),
                                name.span,
                                "no such parameter",
                            );
                            ok = false;
                        },
                    }
                },
                None => {
                    if next_positional < slots.len() {
                        let value: &'x Expr = &arg.value;
                        slots[next_positional] = Some(value);
                        next_positional += 1;
                    } else {
                        self.error(
                            ErrorCode::ArityMismatch,
                            "more arguments than parameters".to_owned(),
                            arg.span,
                            "extra argument",
                        );
                        ok = false;
                    }
                },
            }
        }
        if let Some(tail) = trailing {
            if next_positional < slots.len() && slots[next_positional].is_none() {
                let value: &'x Expr = tail;
                slots[next_positional] = Some(value);
            } else {
                self.error(
                    ErrorCode::ArityMismatch,
                    "the trailing lambda has no parameter to feed".to_owned(),
                    span,
                    "extra argument",
                );
                ok = false;
            }
        }
        let missing: Vec<&str> = slots
            .iter()
            .zip(params.iter())
            .enumerate()
            .filter(|(index, (slot, param))| {
                slot.is_none() && param.default.is_none() && *index >= skip
            })
            .map(|(_, (_, param))| param.name.as_str())
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ArityMismatch,
                    format!("missing argument(s): {names}"),
                )
                .with_primary(span, "missing arguments"),
            );
            ok = false;
        }
        if ok { Some(slots) } else { None }
    }

    /// Checks filled slots against their parameter types and omitted ones
    /// against their written literal defaults.
    pub(super) fn check_slots(
        &mut self,
        slots: &[Option<&Expr>],
        params: &[ParamSig],
        param_tys: &[Option<Ty>],
    ) {
        for ((slot, param), param_ty) in slots.iter().zip(params.iter()).zip(param_tys.iter()) {
            match (slot, param_ty) {
                (Some(arg), Some(param_ty)) => {
                    // A lambda argument's expectation can cite the
                    // callee's written parameter type when the lambda's
                    // arity refuses; any other argument clears it.
                    self.expected_ty_span = match peel_paren(arg) {
                        Expr::Lambda { .. } => param.ty.as_ref().map(Type::span),
                        _ => None,
                    };
                    let val = self.expr(arg, Some(*param_ty));
                    self.expected_ty_span = None;
                    self.expect_contains(val, *param_ty, arg.span(), "argument");
                },
                (Some(arg), None) => {
                    self.expr(arg, None);
                },
                (None, Some(param_ty)) => {
                    if let Some(default) = &param.default {
                        let val = self.expr(default, Some(*param_ty));
                        self.expect_contains(val, *param_ty, default.span(), "default value");
                    }
                },
                (None, None) => {},
            }
        }
    }

    /// The iterated iterable's binding identity when it is a bare name:
    /// only a mutable binding (a local `var` or a top-level `var`)
    /// can open a `mut` window over it later, so a const or computed
    /// iterable records nothing and every body read stays free.
    pub(super) fn iter_window(&self, iter: &Expr) -> Option<IterWindow> {
        let Expr::Path { path, .. } = iter else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        let bind = match self.res.get(only.span) {
            Some(Res::Local { bind }) => self
                .locals
                .get(*bind)
                .is_some_and(|binding| binding.mutable)
                .then_some(IterBind::Local(*bind)),
            Some(Res::Def(def)) => {
                (self.package.def(*def).kind == DefKind::Var).then_some(IterBind::Def(*def))
            },
            _ => None,
        }?;
        Some(IterWindow {
            bind,
            iter_at: only.span,
        })
    }

    /// The innermost enclosing for-in iterating `bind`, by its loop
    /// head's iterable use.
    pub(super) fn iterated_at(&self, bind: IterBind) -> Option<Span> {
        self.for_iters
            .iter()
            .rev()
            .find(|window| window.bind == bind)
            .map(|window| window.iter_at)
    }

    /// The spawn-scoped `cancel` refinement: inside a spawned body
    /// a `cancel` call is the child's deliberate failure — the fiber
    /// dies at the call, so it never answers, and the staging
    /// signature's unit drops out of the body's own unions (a match's
    /// `cancel` arm contributes nothing to the answer the join
    /// collects). Outside spawned bodies the declared signature stands
    /// unchanged. The spelling is recognized like the ambient's other
    /// compiler-required names (`none`): by its name — the ambient's
    /// `cancel` is the only declaration honest code reaches inside a
    /// spawned body.
    pub(super) fn spawn_cancel_refines(&mut self, def: DefId, mut val: Val) -> Val {
        if self.spawn_depth > 0 && self.package.def(def).name == "cancel" {
            val.diverges = true;
        }
        val
    }

    /// A `mut` window parameter over a copyable binding takes only a
    /// `var` receiver: copyable Ref words alias with no
    /// checker-visible trace, so exclusivity is proven by spelling the
    /// binder `var`. `~Copyable` receivers keep their linearity proof and
    /// fresh rvalues are uniquely owned by construction, so only a const
    /// binding of a copyable type is rejected. A hidden copy behind an
    /// alias would keep mutation invisible at the call site, so the
    /// spelling is corrected, never coerced.
    pub(super) fn check_mut_window_args(&mut self, slots: &[Option<&Expr>], params: &[ParamSig]) {
        for (slot, param) in slots.iter().zip(params.iter()) {
            if !matches!(param.mode, ds_parse::ParamMode::Mut(_)) {
                continue;
            }
            let Some(arg) = slot else { continue };
            let Expr::Path { path, .. } = peel_paren(arg) else {
                continue;
            };
            let [only] = path.segments.as_slice() else {
                continue
            };
            let Some(res) = self.res.get(only.span).cloned() else {
                continue;
            };
            // A `mut` window over an enclosing for-in's iterated binding
            // refuses: the loop's element count froze at entry, so
            // the mid-loop mutation never joins this loop's walk — a
            // diagnostic refinement over the frozen-count semantics,
            // which stay the runtime guarantee. Only a mutable binding
            // records a window, so the const-receiver refusal below
            // never doubles with this one.
            let windowed = match &res {
                Res::Local { bind } => Some(IterBind::Local(*bind)),
                Res::Def(def) => Some(IterBind::Def(*def)),
                _ => None,
            };
            if let Some(bind) = windowed
                && let Some(iter_at) = self.iterated_at(bind)
            {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::ForIterMutWindow,
                        format!(
                            "`{}` is this loop's iterated binding; its element count froze at \
                             loop entry, so a `mut` window over it inside the body never joins \
                             this loop's walk [D3]",
                            only.name
                        ),
                    )
                    .with_primary(only.span, "mut window over the iterated binding")
                    .with_secondary(iter_at, "iterated here"),
                );
                continue;
            }
            // A `mut` window over a binding outliving the enclosing
            // nursery block refuses inside a spawned body: once a child
            // can park on a world op the drive interleaves siblings, so a
            // mutation of a binding that survives the join has no sequenced
            // observer — the refusal permanent and drive-independent.
            // Bindings the block itself introduced die with it after
            // the join, so a window over one of those stays legal, and
            // the parent's own statements in the block sequence with
            // its spawns by construction, so only the spawned body is
            // refused. A top-level `var` always outlives the block.
            if self.spawn_depth > 0
                && let Some((outlives, nursery_at)) = self.nursery_scopes.last().map(|scope| {
                    let outlives = match &res {
                        Res::Local { bind } => scope.outlived.contains(bind),
                        Res::Def(def) => self.package.def(*def).kind == DefKind::Var,
                        _ => false,
                    };
                    (outlives, scope.at)
                })
                && outlives
            {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::SpawnOutlivedMutWindow,
                        format!(
                            "`{}` outlives this nursery block; a spawned body may never hold a \
                             `mut` window over it [E2]",
                            only.name
                        ),
                    )
                    .with_primary(only.span, "mut window over an outlived binding")
                    .with_secondary(nursery_at, "the nursery block joins here"),
                );
                continue;
            }
            let (binder, mutable, ty) = match res {
                Res::Local { bind } => {
                    let Some(binding) = self.locals.get(bind) else {
                        continue
                    };
                    (bind, binding.mutable, binding.ty)
                },
                Res::Def(def) => {
                    let def_info = self.package.def(def);
                    if def_info.kind != DefKind::Const {
                        continue;
                    }
                    let Some(ty) = self.def_types.get(&def).copied() else {
                        continue;
                    };
                    (def_info.span, false, ty)
                },
                _ => continue,
            };
            // An empty type is an inference failure the walk already
            // reported; a non-copyable receiver keeps its linearity proof.
            if mutable
                || self.decls.universe.is_empty(ty)
                || !crate::linearity::ty_copyable(self.decls, ty, &self.linear_vars)
            {
                continue;
            }
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::MutWindowConstReceiver,
                    format!(
                        "`{}` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]",
                        only.name
                    ),
                )
                .with_secondary(binder, "bound here")
                .with_primary(only.span, "const receiver"),
            );
        }
    }
}
