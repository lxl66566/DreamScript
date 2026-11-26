//! Control flow: `if`, `match` and `for` typed against the expected hint, the
//! `for comptime` copy walk with its loop projection, the `is`-test predicates,
//! and the refinement harvest the branch arms apply.

use super::*;

impl Bodies<'_> {
    /// `?` in a branch-final position splits against the expected type
    /// when one flows in: a branch-final expression is the enclosing
    /// function's final expression when the if/match itself is last.
    pub(super) fn if_expr(
        &mut self,
        cond: &Expr,
        then: &BlockExpr,
        else_: Option<&Expr>,
        expected: Option<Ty>,
        pos: bool,
    ) -> Val {
        self.cond_depth += 1;
        let cond_val = self.expr(cond, None);
        self.cond_depth -= 1;
        self.expect_bool(cond_val, cond.span());
        let narrows = self.harvest(cond);
        // The else path runs on ¬(a∧b∧…) = ¬a∨¬b∨…: when the condition is
        // an `&&` chain, no single conjunct is guaranteed false, so none
        // of its narrows inverts soundly there. A single-conjunct
        // condition still inverts (¬a is exactly the else).
        let else_narrows: &[Narrow] = if matches!(peel_paren(cond), Expr::Binary {
            op: BinaryOp::And,
            ..
        }) {
            &[]
        } else {
            &narrows
        };
        let unit = self.unit();
        let saved = self.snapshot();
        for narrow in &narrows {
            self.apply_narrow(narrow);
        }
        let then_val = {
            self.value_pos = pos;
            let val = self.block_expected(then, expected);
            self.value_pos = true;
            // the arm's own early-exit flag lands on the block's span:
            // the lowering reads it to skip the join widen for an arm a
            // diverging call exits (the checked Val the block walk
            // returns is not recorded by the expression wrapper)
            self.record_val(then.span, val);
            val
        };
        self.restore(saved);
        let else_val = if let Some(else_expr) = else_ {
            let saved = self.snapshot();
            for narrow in else_narrows {
                self.apply_inverse(narrow);
            }
            self.value_pos = pos;
            let val = self.expr(else_expr, expected);
            self.restore(saved);
            val
        } else {
            if then_val.diverges {
                // The then arm never falls through, so the continuation
                // is exactly the implicit else path: its inverse
                // narrowing stays live after the if.
                for narrow in else_narrows {
                    self.apply_inverse(narrow);
                }
            }
            Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            }
        };
        if then_val.diverges {
            return else_val;
        }
        if else_val.diverges {
            return then_val;
        }
        Val {
            ty: self.decls.universe.union(then_val.ty, else_val.ty),
            diverges: false,
            poisoned: false,
        }
    }

    pub(super) fn match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        expected: Option<Ty>,
        pos: bool,
    ) -> Val {
        let scrut_val = self.expr(scrutinee, None);
        let scrut_ty = scrut_val.ty;
        let scrut_bind = self.path_local(scrutinee);
        let mut covered: Vec<TypeId> = Vec::new();
        let mut arm_vals = Vec::with_capacity(arms.len());
        // Arms type and cover sequentially: each arm sees the members
        // earlier arms left, so `s: string, i => ...` binds `i` the
        // remainder.
        let mut remaining = scrut_ty;
        for arm in arms {
            let saved = self.snapshot();
            let (arm_cover, arm_binders) =
                self.pattern_cover(&arm.pat, remaining, arm.ann.as_ref());
            if let Some(bind) = scrut_bind {
                // An arm covering no member atom (a literal arm) refines
                // nothing: narrowing the scrutinee to the empty type
                // would poison the arm body.
                if !arm_cover.is_empty() {
                    let refined = self.cover_ty(&arm_cover);
                    self.refined.insert(bind, refined);
                }
            }
            for (_, span, ty) in arm_binders {
                self.bind_local(span, Binding { ty, mutable: false });
            }
            if let Some(guard) = &arm.guard {
                self.cond_depth += 1;
                self.value_pos = true;
                let guard_val = self.expr(guard, None);
                self.cond_depth -= 1;
                self.expect_bool(guard_val, guard.span());
                for narrow in self.harvest(guard) {
                    self.apply_narrow(&narrow);
                }
            }
            self.value_pos = pos;
            let body_val = self.expr(&arm.body, expected);
            self.restore(saved);
            arm_vals.push(body_val);
            if arm.guard.is_none() {
                covered.extend(arm_cover.iter().copied());
                // a value-level arm (any literal test at any depth)
                // subtracts nothing from the remainder: the test may
                // fail, so later arms check over the full scrutinee
                // type — a branch checked on a superset may be dead,
                // never wrong. The exhaustiveness accounting above
                // keeps the arm's own cover regardless.
                if !Self::carries_value_tests(&arm.pat) {
                    let cover_ty = self.cover_ty(&arm_cover);
                    remaining = self.decls.universe.diff(remaining, cover_ty);
                }
            }
        }
        // Exhaustiveness by set difference; guards prove nothing,
        // conservatively, and neither do literal patterns. An applied
        // enum head stays opaque to the algebra, so its case atoms
        // exhaust it case-wise: a catchall that covered the application
        // itself or every minted case atom empties the remainder, while
        // a case minting no atom (a payloaded case over a generic head)
        // keeps the match honestly non-exhaustive.
        let missing = match self.applied_enum_head(scrut_ty) {
            Some((head_atom, case_atoms, unminted)) => {
                if covered.contains(&head_atom)
                    || (!unminted && case_atoms.iter().all(|atom| covered.contains(atom)))
                {
                    self.empty()
                } else {
                    scrut_ty
                }
            },
            None => self.missing_ty(scrut_ty, &covered),
        };
        if !self.decls.universe.members(missing).is_empty() {
            let scrut_text = self.decls.universe.render(scrut_ty);
            let missing_text = format!("`{}`", self.decls.universe.render(missing));
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::NonExhaustiveMatch,
                    format!("match on `{scrut_text}` is not exhaustive: missing: {missing_text}"),
                )
                .with_primary(span, "missing members"),
            );
        }
        let producing: Vec<&Val> = arm_vals.iter().filter(|v| !v.diverges).collect();
        if producing.is_empty() {
            return Val {
                ty: self.empty(),
                diverges: true,
                poisoned: false,
            };
        }
        let mut ty = self.empty();
        for val in producing {
            ty = self.decls.universe.union(ty, val.ty);
        }
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    pub(super) fn for_expr(
        &mut self,
        pat: &Pattern,
        iter: &Expr,
        body: &Expr,
        span: Span,
        comptime: bool,
    ) -> Val {
        if comptime {
            return self.for_comptime(pat, iter, body, span);
        }
        let unit = self.unit();
        // A range is the surface's one pinned iterable: integer
        // endpoints, the pattern binds the element; a range in value
        // position refuses instead.
        if let Expr::Range { start, end, .. } = peel_paren(iter) {
            if let Some(element_ty) = self.range_element(start, end, span) {
                let saved = self.snapshot();
                self.refined.clear();
                self.loop_depth += 1;
                self.loop_breaks.push(0);
                self.bind_pattern(pat, element_ty, false);
                self.value_pos = false;
                self.expr(body, None);
                self.loop_breaks.pop();
                self.loop_depth -= 1;
                self.restore(saved);
            }
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        }
        let iter_val = self.expr(iter, None);
        let single = {
            let members = self.decls.universe.members(iter_val.ty);
            members.first().filter(|_| members.len() == 1).copied()
        };
        let element_ty = single.and_then(|single| {
            let (_, args) = self.single_apply_of(single)?;
            args.first().copied()
        });
        let Some(element_ty) = element_ty else {
            // A poisoned iterable already reported; the collection-shape
            // complaint on top of it would be a cascade.
            if !iter_val.poisoned {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "iteration needs one collection type, `{}` given; the stdlib traits \
                         arrive at M3",
                        self.decls.universe.render(iter_val.ty)
                    ),
                    span,
                    "not iterable",
                );
            }
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        };
        // A bare-name iterable of a mutable binding records its window:
        // the body's own `mut` window over it refuses below, since the
        // entry count froze. A range head or a computed iterable
        // records nothing.
        let iter_window = self.iter_window(peel_paren(iter));
        if let Some(window) = iter_window {
            self.for_iters.push(window);
        }
        let saved = self.snapshot();
        self.refined.clear();
        self.loop_depth += 1;
        self.loop_breaks.push(0);
        self.bind_pattern(pat, element_ty, false);
        self.value_pos = false;
        self.expr(body, None);
        self.loop_breaks.pop();
        self.loop_depth -= 1;
        self.restore(saved);
        if iter_window.is_some() {
            self.for_iters.pop();
        }
        Val {
            ty: unit,
            diverges: false,
            poisoned: false,
        }
    }

    /// The `for comptime` expansion loop (the comptime field seam): the
    /// iterable projects a written struct's fields, and the body walks
    /// once per field with the record binder a comptime constant each
    /// copy. The copies run in sequence at runtime, so the linear state
    /// threads across them; the loop itself is no runtime loop, so
    /// `break`/`continue` answer to the enclosing one.
    pub(super) fn for_comptime(
        &mut self,
        pat: &Pattern,
        iter: &Expr,
        body: &Expr,
        span: Span,
    ) -> Val {
        let unit = self.unit();
        if self.comptime.ctx {
            self.error(
                ErrorCode::ComptimeValueOutside,
                "a comptime function already evaluates at compile time; walk its loops with the \
                 ordinary `for`"
                    .to_owned(),
                span,
                "expansion loop inside a comptime function",
            );
            return Val {
                ty: unit,
                diverges: false,
                poisoned: true,
            };
        }
        let Some(projection) = self.project_loop_fields(iter, span) else {
            return Val {
                ty: unit,
                diverges: false,
                poisoned: true,
            };
        };
        let Pattern::Binding { name, .. } = pat else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop's pattern binds one name; the record walks field by field"
                    .to_owned(),
                span,
                "not a name binder",
            );
            return Val {
                ty: unit,
                diverges: false,
                poisoned: true,
            };
        };
        // A loop over the fn's own type variable cannot unroll at the
        // once-check: its fields are a per-instance fact. The loop
        // records its marker and defers — each entering site's
        // expansion walk unrolls it over the site's own substitution.
        // The once-check's table holds the written loops and the
        // per-instance markers; an expansion walk's concrete projections
        // land in the instance tables under the walked fn's key, so two
        // instances of one loop never share a projection.
        let (copies, per_instance) = match &self.instance {
            Some(run) => {
                let (fields, tags) = match projection {
                    Projection::Written(fields, tags) => (fields, tags),
                    Projection::PerInstance(_) => {
                        return Val {
                            ty: unit,
                            diverges: false,
                            poisoned: true,
                        };
                    },
                };
                let copies = fields.len();
                self.comptime_instance_loops.insert(
                    (run.def, Rc::clone(&run.args), span),
                    ComptimeLoop {
                        fields,
                        tags,
                        per_instance: None,
                        binder: name.span,
                    },
                );
                (copies, None)
            },
            None => {
                let (fields, tags, marker) = match projection {
                    Projection::Written(fields, tags) => (fields, tags, None),
                    Projection::PerInstance(variable) => (Vec::new(), Vec::new(), Some(variable)),
                };
                let copies = fields.len();
                self.comptime_loops.insert(span, ComptimeLoop {
                    fields,
                    tags,
                    per_instance: marker,
                    binder: name.span,
                });
                (copies, marker)
            },
        };
        if per_instance.is_some() {
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        }
        let record_ty = self.comptime_record_ty(name.span);
        self.comptime_stack.push(span);
        let diag_start = self.diags.len();
        for index in 0..copies {
            self.comptime_path
                .push(u32::try_from(index).expect("copy index fits u32"));
            let saved = self.snapshot();
            self.refined.clear();
            self.bind_local(name.span, Binding {
                ty: record_ty,
                mutable: false,
            });
            self.value_pos = false;
            self.expr(body, None);
            self.restore(saved);
            self.comptime_path.pop();
        }
        self.comptime_stack.pop();
        // The copies share their source spans, so the walks re-derive
        // the same diagnostic per copy; the loop's own diagnostics
        // dedupe by (code, primary span), first wins.
        let mut seen = HashSet::new();
        let mut kept = Vec::with_capacity(self.diags.len() - diag_start);
        for diag in self.diags.drain(diag_start..) {
            let key = (diag.code, diag.labels.first().map(|label| label.span));
            if seen.insert(key) {
                kept.push(diag);
            }
        }
        self.diags.extend(kept);
        Val {
            ty: unit,
            diverges: false,
            poisoned: false,
        }
    }

    /// One per-instance expansion's body entry: the walked fn's lambda
    /// under the entering site's solved arguments pushed as its generic
    /// scope (no variables mint — the concrete types solve their own
    /// instances), the parameters solved and bound over them, the body
    /// walked once. Mirrors the lambda walk's frame discipline; the
    /// walk's body-wide records run to scratch tables, so only the
    /// per-instance copy facts it lands survive.
    pub(super) fn instance_fn_body(&mut self, lambda: &Expr, args: &[Ty]) {
        let Expr::Lambda {
            generics,
            params,
            ret,
            body,
            ..
        } = lambda
        else {
            return;
        };
        let mut scope = FxHashMap::default();
        for (param, ty) in generics.iter().zip(args.iter()) {
            scope.insert(param.name.name.clone(), *ty);
        }
        self.type_vars.push(scope);
        let saved = self.snapshot();
        for param in params {
            let Some(solved) = param.ty.as_ref().and_then(|t| self.solve_here(t)) else {
                continue;
            };
            self.bind_local(param.name.span, Binding {
                ty: solved,
                mutable: param_is_mut(param),
            });
        }
        let declared_ret = ret.as_ref().and_then(|r| self.solve_here(r));
        let old_ret = self.ret;
        self.ret = declared_ret;
        let frame = self.enter_frame();
        let outer_answers = std::mem::take(&mut self.spawn_answers);
        self.value_pos = false;
        let body_val = self.expr(body, self.ret);
        self.spawn_answers = outer_answers;
        self.exit_frame(frame);
        if let Some(r) = declared_ret
            && !body_val.diverges
        {
            self.expect_contains(body_val, r, body.span(), "instance body");
        }
        self.ret = old_ret;
        self.restore(saved);
        self.type_vars.pop();
    }

    /// The projected field names of a `for comptime` loop's iterable:
    /// exactly `fields(T)` over one written parameter-less struct type.
    /// The iterable never walks as an expression — its argument is a
    /// type spelling, read through the resolution directly.
    pub(super) fn project_loop_fields(&mut self, iter: &Expr, span: Span) -> Option<Projection> {
        let Expr::Call {
            callee,
            args,
            trailing,
            ..
        } = peel_paren(iter)
        else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop iterates `fields(t)` over one written struct type".to_owned(),
                span,
                "not a `fields` projection",
            );
            return None;
        };
        let named = single_path_ident(peel_paren(callee));
        let builtin = named
            .and_then(|head| self.res.get(head.span))
            .map(|res| matches!(res, Res::Def(def) if self.package.def(*def).name == "fields"))
            .unwrap_or(false);
        if !builtin || args.len() != 1 || trailing.is_some() {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop iterates `fields(t)` over one written struct type".to_owned(),
                span,
                "not a `fields` projection",
            );
            return None;
        }
        self.project_loop_type(&args[0].value)
    }

    /// The loop's iterable argument: a written struct type name projects
    /// its fields here; a name resolving to the fn's own type variable
    /// marks the loop per-instance (the once-check cannot project it —
    /// each entering site's expansion walk unrolls it under the site's
    /// own substitution, and inside one such walk the variable scope
    /// already holds the instance's concrete type, projecting directly).
    pub(super) fn project_loop_type(&mut self, arg: &Expr) -> Option<Projection> {
        let Expr::Path { path, .. } = peel_paren(arg) else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop's type is one written type name (`fields(point)`)".to_owned(),
                arg.span(),
                "not a type name",
            );
            return None;
        };
        let [only] = path.segments.as_slice() else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop's type is one written type name (`fields(point)`)".to_owned(),
                arg.span(),
                "not a type name",
            );
            return None;
        };
        // The fn's own generic parameter, in expression position: the
        // resolver answers no row for it (the type-variable namespace is
        // the checker's), so the scopes decide.
        if self.res.get(only.span).is_none()
            && let Some(ty) = self.variable_scope_ty(&only.name)
        {
            if self.is_var_ty(ty) {
                return Some(Projection::PerInstance(ty));
            }
            return self.project_loop_atom(ty, only.span);
        }
        let def = match self.res.get(only.span) {
            Some(Res::Def(def)) => *def,
            // An unresolved non-variable name (the resolver kept quiet
            // for the seam's arguments) refuses here.
            _ => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "`{}` names no declared type for the comptime loop to walk",
                        only.name
                    ),
                    only.span,
                    "not a declared type",
                );
                return None;
            },
        };
        let Some(shape) = self.decls.structs.get(&def) else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop walks a struct's fields; an enum's cases carry no field \
                 offsets, so its head refuses here"
                    .to_owned(),
                only.span,
                "not a struct",
            );
            return None;
        };
        if !shape.params.is_empty() {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop walks a written concrete struct; a generic head's fields are \
                 per-application facts, the expansion queues with the walker round"
                    .to_owned(),
                only.span,
                "a generic struct head",
            );
            return None;
        }
        let fields = shape.fields.iter().map(|f| f.name.name.clone()).collect();
        Some(Projection::Written(fields, self.struct_def_tags(def)))
    }

    /// The type one name resolves to through the variable scopes,
    /// innermost first; `None` when no scope names it.
    pub(super) fn variable_scope_ty(&self, name: &str) -> Option<Ty> {
        self.type_vars
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    /// Each field's nominal tag name (the type's own name when the
    /// field's solved type is one bare atom, empty when composite),
    /// aligned with the declaration order.
    pub(super) fn struct_def_tags(&mut self, def: DefId) -> Vec<String> {
        let Some(shape) = self.decls.structs.get(&def) else {
            return Vec::new();
        };
        let written: Vec<Type> = shape.fields.iter().map(|f| f.ty.clone()).collect();
        written
            .iter()
            .map(|t| match self.solve_here(t) {
                Some(ty) => self.bare_atom_name(ty),
                None => String::new(),
            })
            .collect()
    }

    /// The declared name of the one bare atom `ty` names, empty when the
    /// type is composite (a tuple, an fn type, an application) or names
    /// no single atom — the same nominal-tag rule the comptime
    /// evaluator's reflection table keeps.
    pub(super) fn bare_atom_name(&self, ty: Ty) -> String {
        let [only] = self.decls.universe.members(ty) else {
            return String::new();
        };
        let Ok(decl) = self.decls.universe.atom_decl(*only) else {
            return String::new();
        };
        match decl.kind {
            AtomKind::Apply(..) | AtomKind::Tuple(_) | AtomKind::FnSig(..) | AtomKind::Reserved => {
                String::new()
            },
            AtomKind::Unit | AtomKind::Struct(_) | AtomKind::Enum(_) => decl.name.clone(),
        }
    }

    /// The concrete type's field projection for a loop whose variable
    /// scope holds it: the walked struct's fields, in declaration
    /// order. An application, an enum, or any non-struct type refuses
    /// exactly as a written one does.
    pub(super) fn project_loop_atom(&mut self, ty: Ty, span: Span) -> Option<Projection> {
        let [only] = self.decls.universe.members(ty) else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop walks a written concrete struct; this type names no single \
                 struct"
                    .to_owned(),
                span,
                "not a struct",
            );
            return None;
        };
        let Ok(decl) = self.decls.universe.atom_decl(*only) else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop walks a struct's fields; this type names no declared struct"
                    .to_owned(),
                span,
                "not a struct",
            );
            return None;
        };
        if !matches!(decl.kind, AtomKind::Struct(_)) {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop walks a written struct's fields; an enum's cases, a builtin, \
                 and a generic application's per-instance fields name none here"
                    .to_owned(),
                span,
                "not a written struct",
            );
            return None;
        }
        let Some(def) = self.decls.defs_of_atoms.get(&only).copied() else {
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime loop walks a struct's fields; this type names no declared struct"
                    .to_owned(),
                span,
                "not a declared struct",
            );
            return None;
        };
        let shape = self.decls.structs.get(&def)?;
        let fields = shape.fields.iter().map(|f| f.name.name.clone()).collect();
        Some(Projection::Written(fields, self.struct_def_tags(def)))
    }

    /// The comptime field record's type (the ambient's `field` struct).
    pub(super) fn comptime_record_ty(&mut self, span: Span) -> Ty {
        self.named_atom_ty(span, "field", "the comptime loop's record")
            .unwrap_or_else(|| self.empty())
    }

    /// The enclosing comptime loop whose record binder a path names,
    /// innermost match first. An expansion walk reads the loop info
    /// from the instance tables first — a written loop nested inside a
    /// per-instance copy projects only there.
    pub(super) fn comptime_record_depth(&self, bind: Span) -> Option<usize> {
        self.comptime_stack.iter().rposition(|&loop_span| {
            let info = if let Some(run) = &self.instance {
                self.comptime_instance_loops
                    .get(&(run.def, Rc::clone(&run.args), loop_span))
                    .or_else(|| self.comptime_loops.get(&loop_span))
            } else {
                self.comptime_loops.get(&loop_span)
            };
            info.is_some_and(|loop_info| loop_info.binder == bind)
        })
    }

    /// Whether a receiver expression names the active comptime record
    /// binder (`f` inside `for comptime f in ...`).
    pub(super) fn comptime_record_recv(&self, recv: &Expr) -> Option<usize> {
        let Expr::Path { path, .. } = peel_paren(recv) else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        match self.res.get(only.span) {
            Some(Res::Local { bind }) => self.comptime_record_depth(*bind),
            _ => None,
        }
    }

    /// The comptime constant a keyed access's name expression spells:
    /// exactly `<record binder>.name`, answering the enclosing loop and
    /// the copy's field name.
    pub(super) fn comptime_name_key(&self, name: &Expr) -> Option<(usize, String)> {
        let Expr::Field {
            recv, name: member, ..
        } = peel_paren(name)
        else {
            return None;
        };
        if member.name != "name" {
            return None;
        }
        let Expr::Path { path, .. } = peel_paren(recv) else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        let bind = match self.res.get(only.span) {
            Some(Res::Local { bind }) => *bind,
            _ => return None,
        };
        let depth = self.comptime_record_depth(bind)?;
        let (field, _) = self.active_loop_field(depth)?;
        Some((depth, field))
    }

    /// The active copy's own field facts at a record binder's depth:
    /// the field name and its nominal tag string. A per-instance
    /// expansion reads its projection from the instance tables; a
    /// written loop's from the once-check's table (an expansion walk
    /// re-walks written loops too, whose projections never depended on
    /// the walked variable).
    pub(super) fn active_loop_field(&self, depth: usize) -> Option<(String, String)> {
        let loop_span = *self.comptime_stack.get(depth)?;
        let index = *self.comptime_path.get(depth)? as usize;
        let info = if let Some(run) = &self.instance {
            self.comptime_instance_loops
                .get(&(run.def, Rc::clone(&run.args), loop_span))
                .or_else(|| self.comptime_loops.get(&loop_span))?
        } else {
            self.comptime_loops.get(&loop_span)?
        };
        let name = info.fields.get(index)?.clone();
        let tag = info.tags.get(index)?.clone();
        Some((name, tag))
    }

    /// The keyed field access `x.[f.name]`: the comptime constant
    /// resolves to the copy's own field, and the access checks as that
    /// written field read.
    pub(super) fn keyed_field_access(&mut self, _span: Span, recv: &Expr, name: &Expr) -> Val {
        let Some((_, field_name)) = self.comptime_name_key(name) else {
            self.error(
                ErrorCode::InferenceFailed,
                "the keyed access names a `for comptime` record's `name` member (`x.[f.name]`); \
                 this key spells no such constant"
                    .to_owned(),
                name.span(),
                "not a comptime field name",
            );
            let _ = self.expr(recv, None);
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        };
        let ident = Ident {
            span: name.span(),
            name: field_name,
        };
        self.field_access(recv, &ident)
    }

    // ---- refinement ---------------------------------------------------------

    /// Narrows harvested from a condition: `is` tests and none-comparisons
    /// over local paths.
    pub(super) fn harvest(&mut self, cond: &Expr) -> Vec<Narrow> {
        let mut out = Vec::new();
        self.harvest_into(peel_paren(cond), &mut out);
        out
    }

    pub(super) fn harvest_into(&mut self, cond: &Expr, out: &mut Vec<Narrow>) {
        match cond {
            Expr::Binary {
                op: BinaryOp::And,
                left,
                right,
                ..
            } => {
                self.harvest_into(peel_paren(left), out);
                self.harvest_into(peel_paren(right), out);
            },
            Expr::Binary {
                op, left, right, ..
            } if matches!(op, BinaryOp::Eq | BinaryOp::Ne) => {
                let (path_side, other) = (peel_paren(left), peel_paren(right));
                let bind = self
                    .path_local(path_side)
                    .or_else(|| self.path_local(other));
                let Some(bind) = bind else { return };
                if !is_none_expr(other) && !is_none_expr(path_side) {
                    return;
                }
                let none_ty = self.none_ty();
                out.push(Narrow {
                    bind,
                    keep: *op == BinaryOp::Eq,
                    ty: none_ty,
                });
            },
            Expr::Is { expr, ty, .. } => {
                let Some(bind) = self.path_local(peel_paren(expr)) else {
                    return
                };
                // The test type solved once at its own site; a re-solve
                // here only feeds the narrowing, so a repeated refusal
                // is dropped instead of reported twice.
                let before = self.diags.len();
                let solved = self.solve_here(ty);
                self.diags.truncate(before);
                let Some(solved) = solved else { return };
                out.push(Narrow {
                    bind,
                    keep: true,
                    ty: solved,
                });
            },
            _ => {},
        }
    }

    /// Monotone narrowing: intersect the binder's current type.
    pub(super) fn apply_narrow(&mut self, narrow: &Narrow) {
        let Some(declared) = self.locals.get(narrow.bind).map(|b| b.ty) else {
            return;
        };
        let current = self.refined.get(narrow.bind).copied().unwrap_or(declared);
        let next = if narrow.keep {
            self.decls.universe.inter(current, narrow.ty)
        } else {
            self.decls.universe.diff(current, narrow.ty)
        };
        self.refined.insert(narrow.bind, next);
    }

    pub(super) fn apply_inverse(&mut self, narrow: &Narrow) {
        let inverted = Narrow {
            bind: narrow.bind,
            keep: !narrow.keep,
            ty: narrow.ty,
        };
        self.apply_narrow(&inverted);
    }

    // ---- helpers ---------------------------------------------------------

    /// The directed test label: a single-segment written test
    /// type naming no declaration probes the operand's own members by
    /// label. One member carrying it answers the test's set; none keeps
    /// the solve refusal; several report the ambiguity.
    /// The `is` test against a written type: the operand's value plus
    /// the tested set the solve (or the directed label probe)
    /// answers. A declared tag label disjoint from the operand's union
    /// in value position is the unknown-member refusal, not the
    /// constant-false story; every other provably-disjoint test keeps
    /// the constant-`==` rule (a value/assert-position compile error,
    /// a condition-position refinement).
    pub(super) fn is_expr(&mut self, expr: &Expr, ty: &Type, span: Span) -> Val {
        let operand = self.expr(expr, None);
        let Some(solved) = self.solve_here(ty).or_else(|| {
            // the directed test label: an undeclared
            // single-segment label probes the operand's own
            // members before the solve refusal
            self.is_test_label_probe(ty, operand.ty)
        }) else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "the `is` test against `{}` does not solve",
                        decls::render_type(ty)
                    ),
                    ty.span(),
                    "unsolvable test type",
                );
            }
            return Val {
                ty: self.bool_ty(),
                diverges: false,
                poisoned: false,
            };
        };
        if self.cond_depth == 0
            && !operand.poisoned
            && self.is_tagged_test_label(ty)
            && !self.decls.universe.is_empty(operand.ty)
            && !self.decls.universe.is_empty(solved)
            && self.decls.universe.is_disjoint(operand.ty, solved)
        {
            self.error(
                ErrorCode::UnknownTagMember,
                format!(
                    "the tested label `{}` names no member of `{}` [F9.5]",
                    decls::render_type(ty),
                    self.decls.universe.render(operand.ty)
                ),
                ty.span(),
                "unknown member",
            );
            return Val {
                ty: self.bool_ty(),
                diverges: false,
                poisoned: true,
            };
        }
        self.record_is_test(span, solved);
        // A provably-disjoint test computes a constant false, so in
        // value/assert position it is a compile error — the same
        // treatment as a constant `==`. In
        // condition position (if/match guard/while, any depth) the
        // test refines legitimately: the kept branch narrows to
        // empty, which is sound dead code, and a match arm covering
        // nothing is the exhaustiveness story.
        if self.cond_depth == 0
            && !operand.poisoned
            && !self.decls.universe.is_empty(operand.ty)
            && !self.decls.universe.is_empty(solved)
            && self.decls.universe.is_disjoint(operand.ty, solved)
        {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "the `is` test against `{}` is always false over `{}` [B2]",
                    self.decls.universe.render(solved),
                    self.decls.universe.render(operand.ty)
                ),
                ty.span(),
                "always false",
            );
        }
        Val {
            ty: self.bool_ty(),
            diverges: false,
            poisoned: false,
        }
    }

    pub(super) fn is_test_label_probe(&mut self, ty: &Type, operand: Ty) -> Option<Ty> {
        let Type::Path { path, args, .. } = ty else {
            return None;
        };
        if !args.is_empty() {
            return None;
        }
        let [only] = path.segments.as_slice() else {
            return None;
        };
        if only.name == "none" && !self.package.declares("none") {
            return None;
        }
        if self.type_def_of(&only.name).is_some() {
            return None;
        }
        let labeled: Vec<TypeId> = self
            .decls
            .universe
            .members(operand)
            .iter()
            .copied()
            .filter(|&atom| {
                self.decls
                    .universe
                    .atom_decl(atom)
                    .is_ok_and(|decl| decl.name == only.name)
            })
            .collect();
        match labeled.as_slice() {
            [one] => self.decls.universe.atom_ty(*one).ok(),
            [] => None,
            many => {
                let mut diag = Diagnostic::error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "the tested label `{}` matches several members of `{}` [F9.5]; qualify \
                         which declaration you mean",
                        only.name,
                        self.decls.universe.render(operand)
                    ),
                )
                .with_primary(only.span, "ambiguous member");
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

    /// Whether the written test type is a bare tag-family label:
    /// its disjointness against the operand reports unknown-member, not
    /// the constant-false story a set-level test keeps.
    pub(super) fn is_tagged_test_label(&self, ty: &Type) -> bool {
        let Type::Path { path, args, .. } = ty else {
            return false;
        };
        if !args.is_empty() {
            return false;
        }
        let [only] = path.segments.as_slice() else {
            return false;
        };
        self.type_def_of(&only.name)
            .is_some_and(|def| matches!(self.package.def(def).kind, DefKind::Tag | DefKind::Case))
    }
}
