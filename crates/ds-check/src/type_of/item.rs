//! Item and local collection: the declaration walk over files, items,
//! functions, structs, and impls that enters each body, and the block/stmt
//! walk that answers a block's tail value.

use super::*;

impl Bodies<'_> {
    /// Binds one local and records its affine facts: copyability of
    /// the solved type under the in-scope generic bounds, and the
    /// binding's own mutability — a `var` local's capture by a lambda
    /// moves it, so the affine pass reads the flag at the capture kill.
    pub(super) fn bind_local(&mut self, span: Span, binding: Binding) {
        let copyable = crate::linearity::ty_copyable(self.decls, binding.ty, &self.linear_vars);
        let mutable = binding.mutable;
        self.record_binder(span, crate::linearity::BinderFact {
            copyable,
            ty: binding.ty,
            mutable,
        });
        self.locals.insert(span, binding);
    }

    pub(super) fn file(&mut self, ast: &Ast) {
        for block in &ast.blocks {
            for item in &block.items {
                self.item(item);
            }
        }
    }

    // ---- items ------------------------------------------------------------

    pub(super) fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(f) => {
                if f.body.is_some() {
                    self.fn_decl(f, None, false, false);
                }
            },
            ItemKind::Trait(t) => {
                self.push_generics(&t.generics.iter().map(generic_of).collect::<Vec<_>>());
                let self_ty = self.fresh_var("Self");
                // A trait's members see `Self` as satisfying their own
                // trait: a default body calls sibling members on `self`
                // without an instance — every eventual impl proves the
                // whole member set together. The assumption is scoped to
                // these bodies, never global, and bound recursion stays
                // depth-capped through it.
                let self_trait = self
                    .package
                    .lookup_type(&t.name.name)
                    .and_then(|def| self.decls.traits.get(&def).copied());
                if let Some(trait_id) = self_trait {
                    self.assumptions.push((trait_id, self_ty));
                }
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.fn_decl(f, Some(self_ty), true, false);
                    }
                }
                if self_trait.is_some() {
                    self.assumptions.pop();
                }
                self.pop_generics();
            },
            ItemKind::Impl(imp) => {
                self.impl_item(item, imp);
            },
            ItemKind::Local(local) => self.top_level_local(local),
            ItemKind::Struct(s) => self.struct_item(s),
            ItemKind::ExternFn(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Import(_)
            | ItemKind::Platform(_) => {},
        }
    }

    /// Type-checks every declared `deinit` body: `self` is the
    /// dying instance, borrowed, typed the struct's own atom; the body
    /// types as a unit-returning function body. The recorded facts —
    /// the `self` binder at the member's span, the body's expression
    /// types — are what the deinit's runtime half consumes.
    pub(super) fn struct_item(&mut self, s: &ds_parse::StructDecl) {
        let Some(def) = self.package.lookup_type(&s.name.name) else {
            return;
        };
        let Some(atom) = self.decls.atoms.get(&def).copied() else {
            return;
        };
        let generics: Vec<GenericParam> = s.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        for member in &s.members {
            let ds_parse::StructMember::Deinit { span, body } = member else {
                continue;
            };
            let Ok(self_ty) = self.decls.universe.atom_ty(atom) else {
                continue;
            };
            let saved = self.snapshot();
            self.locals.clear();
            self.refined.clear();
            self.bind_local(*span, Binding {
                ty: self_ty,
                mutable: false,
            });
            let unit = self.unit();
            let old_ret = self.ret;
            self.ret = Some(unit);
            let block = Expr::Block(body.clone());
            self.value_pos = false;
            let val = self.expr(&block, Some(unit));
            if !val.diverges {
                self.expect_contains(val, unit, body.span, "a deinit body");
            }
            self.ret = old_ret;
            self.restore(saved);
        }
        self.pop_generics();
    }

    pub(super) fn impl_item(&mut self, item: &Item, imp: &ds_parse::ImplDecl) {
        let _ = item;
        let generics: Vec<GenericParam> = imp.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        let target = self.solve_here(&imp.target);
        let Some(target) = target else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "impl target `{}` does not solve against the package declarations",
                        decls::render_type(&imp.target)
                    ),
                    imp.target.span(),
                    "unsolvable target",
                );
            }
            self.pop_generics();
            return;
        };
        let trait_def = imp.trait_ref.as_ref().and_then(|tr| {
            let [only] = tr.path.segments.as_slice() else {
                return None
            };
            self.package.lookup_type(&only.name)
        });
        let member_names: Option<Vec<String>> =
            trait_def.and_then(|def| self.decls.trait_members.get(&def).cloned());
        // `Self` resolves to the impl's target while the written member
        // signatures compare.
        self.type_vars
            .push(FxHashMap::from_iter([("Self".to_owned(), target)]));
        let subst = self.trait_arg_subst(imp, trait_def);
        for f in &imp.fns {
            if let Some(members) = &member_names {
                if !members.iter().any(|m| m == &f.name.name) {
                    self.error(
                        ErrorCode::UnresolvedName,
                        format!("the implemented trait has no member `{}`", f.name.name),
                        f.name.span,
                        "not a trait member",
                    );
                    continue;
                }
                if let (Some(def), Some(subst)) = (trait_def, &subst)
                    && let Some((sig, sig_span)) =
                        decls::trait_member_sig(self.package, self.decls, def, &f.name.name)
                {
                    impl_sig::agree_member_signature(self, f, &sig, sig_span, subst);
                }
            }
            let inferred = self.fn_decl(f, Some(target), false, item.synthesized);
            // An inferred return is agreed through the body's checked tail
            // type, exactly as a written one is: the trait member's
            // registered signature stays the call-site contract.
            if f.ret.is_none()
                && let (Some(def), Some(subst)) = (trait_def, &subst)
                && let (Some((sig, sig_span)), Some(inferred)) = (
                    decls::trait_member_sig(self.package, self.decls, def, &f.name.name),
                    inferred,
                )
            {
                impl_sig::agree_inferred_return(self, f, &sig, sig_span, subst, inferred);
            }
        }
        // Every trait member without a default body needs a bodied impl
        // member: the registered signature checks call sites alone, so
        // the missing body would surface only at the dispatch seam.
        if let (Some(def), Some(members)) = (trait_def, &member_names) {
            impl_sig::agree_impl_completeness(self, imp, def, members);
        }
        self.type_vars.pop();
        self.pop_generics();
    }

    /// The trait's generic parameters substituted by the impl's trait
    /// reference, positionally: every name the trait declares maps
    /// to the argument the reference spells. A trait declaring generics
    /// the reference does not spell reports once — its member signatures
    /// could not be spelled in the impl's scope at all.
    pub(super) fn trait_arg_subst(
        &mut self,
        imp: &ds_parse::ImplDecl,
        trait_def: Option<DefId>,
    ) -> Option<FxHashMap<String, Type>> {
        let reference = imp.trait_ref.as_ref()?;
        let def = trait_def?;
        let names = self.decls.trait_generics.get(&def)?.clone();
        if names.len() != reference.args.len() {
            self.error(
                ErrorCode::ImplMemberSignatureMismatch,
                format!(
                    "the trait declares {} generic parameter(s); the impl's trait reference \
                     spells {} argument(s)",
                    names.len(),
                    reference.args.len()
                ),
                reference.span,
                "unsubstituted trait generics",
            );
            return None;
        }
        Some(
            names
                .into_iter()
                .zip(reference.args.iter().cloned())
                .collect(),
        )
    }

    /// A top-level binding: its initializer is typed (memoized for
    /// order-independent uses); a written annotation anchors the check.
    /// A destructuring pattern declares many defs — the memo and the cycle
    /// guard cover them all, so tuple/or/struct binders receive types like
    /// any other top-level name.
    pub(super) fn top_level_local(&mut self, local: &LocalDecl) {
        let defs: Vec<DefId> = crate::pattern::pattern_binders(&local.pat)
            .into_iter()
            .filter_map(|(_, name)| self.package.lookup_value(name))
            .collect();
        if defs.is_empty() {
            return;
        }
        if defs.iter().all(|def| self.def_types.contains_key(def))
            || defs.iter().any(|def| self.visiting.contains(def))
        {
            return;
        }
        let visiting = self.visiting.len();
        self.visiting.extend(defs.iter().copied());
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;
        // The initializer is its own frame: a lazily triggered fill
        // must not inherit the triggering walk's loop and frame depths.
        let (outer_loops, outer_frames) = (self.loop_depth, self.frame_depth);
        let outer_breaks = std::mem::take(&mut self.loop_breaks);
        self.loop_depth = 0;
        self.frame_depth = 0;
        // A use in another file's body can trigger this lazy fill; the
        // initializer's spans resolve under the binding's OWN file table,
        // never the triggering file's.
        let home_res = self.res;
        let home_file = self.file;
        if let Some(index) = self.res_by_file.get(self.package.def(defs[0]).span.file)
            && let Some(res) = self.resolutions.get(index)
        {
            self.res = res;
            self.file = Some(self.package.def(defs[0]).span.file);
        }

        // A top-level fn declaration's initializer is its lambda: mark it
        // so the lambda's own check knows a `comptime` parameter sits on
        // a free fn, never on a nested lambda.
        let was = self.comptime;
        self.comptime.declaring_fn = matches!(local.init, Expr::Lambda { .. });
        self.value_pos = true;
        // the annotation directs the initializer: a bare label
        // in it resolves against the expected members, so the written
        // type solves BEFORE the walk
        let expected = local.ty.as_ref().and_then(|ann| self.solve_here(ann));
        if let Some(expected) = expected
            && self.decls.universe.is_empty(expected)
        {
            self.error(
                ErrorCode::NeverTypedPosition,
                "binding annotated `never`: no value inhabits the type, so the binder holds \
                 nothing"
                    .to_owned(),
                local
                    .ty
                    .as_ref()
                    .map(Type::span)
                    .expect("the annotation solved"),
                "binding typed never",
            );
        }
        let mut val = self.expr(&local.init, expected);
        self.comptime = was;
        if let Some(ann) = &local.ty {
            if let Some(expected) = expected {
                // The annotation's own refusal fired above; checking the
                // initializer against the empty type would cascade a
                // second error onto the root cause.
                if !self.decls.universe.is_empty(expected) {
                    self.expect_contains(val, expected, local.init.span(), "annotated binding");
                    val.ty = expected;
                }
            } else if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "annotation `{}` does not solve against the package declarations",
                        decls::render_type(ann)
                    ),
                    ann.span(),
                    "unsolvable annotation",
                );
            }
        }
        for (name, def, ty) in self.pattern_defs(&local.pat, val.ty) {
            let _ = name;
            if let Some(def) = def {
                self.def_types.insert(def, ty);
            }
        }
        self.restore(saved);
        self.loop_depth = outer_loops;
        self.frame_depth = outer_frames;
        self.loop_breaks = outer_breaks;
        self.res = home_res;
        self.file = home_file;
        self.visiting.truncate(visiting);
    }

    /// One named function declaration: boundary-anchored. `self_ty`
    /// binds `Self` inside trait/impl scopes, where the first parameter
    /// named `self` is the receiver — exempt from annotation and typed
    /// `Self`. A trait member must spell the receiver
    /// (dynamic dispatch is receiver-carried, so DS2021 fires there);
    /// an inherent impl member may omit it — the receiverless member is
    /// the type-keyed static surface. Returns
    /// the member's solved return — the written return, or the body's
    /// checked tail type when inferred — for the impl-member agreement
    /// checks.
    pub(super) fn fn_decl(
        &mut self,
        f: &FnDecl,
        self_ty: Option<Ty>,
        trait_member: bool,
        synthesized: bool,
    ) -> Option<Ty> {
        let generics: Vec<GenericParam> = f.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        if let Some(target) = self_ty {
            self.type_vars
                .push(FxHashMap::from_iter([("Self".to_owned(), target)]));
        }
        // A trait member's signature declares its receiver as its first
        // parameter: both call forms feed the receiver first and
        // the trait dispatch keys on its type, so a trait signature
        // with no `self` has no reachable call form. An inherent impl
        // member without one is the type-keyed static surface instead.
        if trait_member && f.params.first().is_none_or(|p| p.name.name != "self") {
            self.error(
                ErrorCode::MemberWithoutReceiver,
                format!(
                    "member `{}` declares its receiver as its first parameter (`self` or `mut \
                     self`, typed `Self`)",
                    f.name.name
                ),
                f.name.span,
                "member without a receiver",
            );
        }
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;
        for (index, param) in f.params.iter().enumerate() {
            if index == 0 && self_ty.is_some() && param.name.name == "self" {
                if let Some(ann) = &param.ty {
                    self.error(
                        ErrorCode::AnnotatedReceiver,
                        "the receiver is spelled `self` or `mut self` with no annotation; it is \
                         typed `Self`"
                            .to_owned(),
                        ann.span(),
                        "annotated receiver",
                    );
                }
                let recv_ty = self_ty.unwrap_or_else(|| self.empty());
                self.bind_local(param.name.span, Binding {
                    ty: recv_ty,
                    mutable: param_is_mut(param),
                });
                continue;
            }
            if let Some(solved) = param.ty.as_ref().and_then(|t| self.solve_here(t)) {
                // A derive-synthesized member over a zero-case head types
                // its receiver the empty set by construction; the vacuous
                // body never dispatches for an inhabited query, so the
                // no-value refusal stays the user surface's own.
                if !synthesized && self.decls.universe.is_empty(solved) {
                    self.error(
                        ErrorCode::NeverTypedPosition,
                        format!(
                            "parameter `{}` is typed `never`: no value inhabits the type, so no \
                             call ever supplies one",
                            param.name.name
                        ),
                        param
                            .ty
                            .as_ref()
                            .map(Type::span)
                            .expect("the solved parameter carries its annotation"),
                        "parameter typed never",
                    );
                }
                self.bind_local(param.name.span, Binding {
                    ty: solved,
                    mutable: param_is_mut(param),
                });
            } else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "parameter `{}` needs a type annotation at this boundary [B2]",
                            param.name.name
                        ),
                        param.name.span,
                        "unannotated boundary parameter",
                    );
                }
                let empty = self.empty();
                self.bind_local(param.name.span, Binding {
                    ty: empty,
                    mutable: param_is_mut(param),
                });
            }
        }
        let ret = f.ret.as_ref().and_then(|r| self.solve_here(r)).or_else(|| {
            f.ret.as_ref().map(|r| {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "return type `{}` does not solve against the package declarations",
                            decls::render_type(r)
                        ),
                        r.span(),
                        "unsolvable return type",
                    );
                }
                self.empty()
            })
        });
        // The retired wildcard return: a generic whose return type names
        // a variable no parameter mentions never solves from a call's
        // arguments — the escape made every diverging ambient spell a
        // fake generic answer, and `never` is the real spelling.
        if !f.generics.is_empty()
            && let Some(ret_ty) = f.ret.as_ref()
        {
            let generic_names: Vec<String> =
                f.generics.iter().map(|g| g.name.name.clone()).collect();
            let mut unbound = Vec::new();
            collect_type_names(ret_ty, &mut unbound);
            unbound.retain(|name| generic_names.contains(name));
            for param in &f.params {
                if let Some(param_ty) = &param.ty {
                    let mut mentioned = Vec::new();
                    collect_type_names(param_ty, &mut mentioned);
                    unbound.retain(|name| !mentioned.contains(name));
                }
            }
            for name in unbound {
                self.error(
                    ErrorCode::UnconstrainedReturnVariable,
                    format!(
                        "return variable `{name}` names no parameter, so no call's arguments bind \
                         it; a diverging declaration answers `never`"
                    ),
                    ret_ty.span(),
                    "unconstrained return variable",
                );
            }
        }
        let old_ret = self.ret;
        self.ret = ret;
        let mut body_val: Option<Val> = None;
        if let Some(body) = &f.body {
            self.value_pos = false;
            let val = self.block_expected(body, ret);
            if let Some(ret) = ret
                && !val.diverges
            {
                self.expect_contains(val, ret, body.span, "function body");
            }
            body_val = Some(val);
        }
        // An impl-member body's solved return rides the declaration
        // tables for the lowering pass: a member function has no package
        // DefId, so its solved signature lives here — the written return
        // against `Self`, or the body's own checked type when inferred.
        let solved = ret.or_else(|| body_val.as_ref().filter(|v| !v.poisoned).map(|v| v.ty));
        if self_ty.is_some()
            && let Some(ty) = solved
        {
            self.decls.impl_fn_rets.insert(f.span, ty);
        }
        self.ret = old_ret;
        self.restore(saved);
        if self_ty.is_some() {
            self.type_vars.pop();
        }
        self.pop_generics();
        solved
    }

    // ---- statements ---------------------------------------------------------

    pub(super) fn block(&mut self, block: &BlockExpr) -> Val {
        self.block_expected(block, None)
    }

    /// A block whose final expression carries an expected type — the
    /// function-body anchor: a tail `?` splits its operand against the
    /// return union there, so a fully-contained operand yields itself
    /// instead of propagating everything.
    pub(super) fn block_expected(&mut self, block: &BlockExpr, expected: Option<Ty>) -> Val {
        // The tail expression occupies the block's own position (a frame
        // body's tail is a statement slot, a consumed block's tail feeds
        // its consumer); head statements are statements either way.
        let tail_pos = self.value_pos;
        let saved = self.snapshot();
        let mut val = Val {
            ty: self.unit(),
            diverges: false,
            poisoned: false,
        };
        let (head, last) = match block.stmts.split_last() {
            Some((last, head)) => (head, Some(last)),
            None => (&block.stmts[..], None),
        };
        for stmt in head {
            val = self.stmt(stmt);
        }
        match last {
            // Only an expression tail carries the expected type (the
            // function-body anchor); a binding tail still checks, its
            // block value stays unit.
            Some(Stmt {
                kind: ds_parse::StmtKind::Expr(expr),
                ..
            }) => {
                self.value_pos = tail_pos;
                val = self.expr(expr, expected);
            },
            Some(other) => val = self.stmt(other),
            None => {},
        }
        self.restore(saved);
        val
    }

    pub(super) fn stmt(&mut self, stmt: &Stmt) -> Val {
        match &stmt.kind {
            ds_parse::StmtKind::Local(local) => {
                self.local_stmt(local);
                Val {
                    ty: self.unit(),
                    diverges: false,
                    poisoned: false,
                }
            },
            ds_parse::StmtKind::Expr(expr) => {
                self.value_pos = false;
                self.expr(expr, None)
            },
        }
    }

    /// A statement-level binding: locals infer.
    pub(super) fn local_stmt(&mut self, local: &LocalDecl) {
        self.value_pos = true;
        // the annotation directs the initializer, the same
        // solve-before-walk the top-level binding takes
        let expected = local.ty.as_ref().and_then(|ann| self.solve_here(ann));
        if let Some(expected) = expected
            && self.decls.universe.is_empty(expected)
        {
            self.error(
                ErrorCode::NeverTypedPosition,
                "binding annotated `never`: no value inhabits the type, so the binder holds \
                 nothing"
                    .to_owned(),
                local
                    .ty
                    .as_ref()
                    .map(Type::span)
                    .expect("the annotation solved"),
                "binding typed never",
            );
        }
        let mut val = self.expr(&local.init, expected);
        if let Some(ann) = &local.ty {
            if let Some(expected) = expected {
                // The annotation's own refusal fired above; checking the
                // initializer against the empty type would cascade a
                // second error onto the root cause.
                if !self.decls.universe.is_empty(expected) {
                    self.expect_contains(val, expected, local.init.span(), "annotated binding");
                    val.ty = expected;
                }
            } else if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "annotation `{}` does not solve against the package declarations",
                        decls::render_type(ann)
                    ),
                    ann.span(),
                    "unsolvable annotation",
                );
            }
        }
        // A poisoned initializer already reported why its type is empty;
        // only a clean-but-empty initializer is its own root cause. A
        // diverging empty answer (a call to a `never` fn) stays legal:
        // the binder holds a value no path produces, the same dead
        // binding an annotated position refuses.
        if self.decls.universe.is_empty(val.ty)
            && !val.diverges
            && !val.poisoned
            && !self.take_solve_reported()
        {
            self.error(
                ErrorCode::InferenceFailed,
                "the initializer's type is empty; inference found no inhabitant [B1]".to_owned(),
                local.init.span(),
                "empty initializer type",
            );
        }
        self.bind_pattern(&local.pat, val.ty, local.mutable);
    }

    // ---- record writes --------------------------------------------------------
    //
    // Every span-keyed fact the lowering reads funnels through one
    // helper per table: outside a `for comptime` body it lands in the
    // body-wide table; inside one, in the active copy's record (the
    // copies share spans, so the body-wide table cannot hold them) —
    // the written loops' copies in the once-check's table, a
    // per-instance expansion's copies in the instance tables keyed by
    // the walked fn's definition and solved arguments.
}

/// Every single-segment name a written type spells, recursions included:
/// the fn-type parameters and return walk with the union and tuple arms,
/// so a bound mention reads like a parameter mention.
pub(super) fn collect_type_names(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Path { path, args, .. } => {
            if let [only] = path.segments.as_slice() {
                out.push(only.name.clone());
            }
            for arg in args {
                collect_type_names(arg, out);
            }
        },
        Type::Optional { inner, .. }
        | Type::Negation { inner, .. }
        | Type::Paren { inner, .. }
        | Type::Ptr { inner, .. } => collect_type_names(inner, out),
        Type::Union { left, right, .. }
        | Type::Intersect { left, right, .. }
        | Type::Difference { left, right, .. } => {
            collect_type_names(left, out);
            collect_type_names(right, out);
        },
        Type::Tuple { items, .. } => {
            for item in items {
                collect_type_names(item, out);
            }
        },
        Type::Fn { params, ret, .. } => {
            for param in params {
                if let Some(t) = &param.ty {
                    collect_type_names(t, out);
                }
            }
            if let Some(ret) = ret {
                collect_type_names(ret, out);
            }
        },
        Type::Unit(_) | Type::TypeKw(_) | Type::Foreign { .. } => {},
    }
}
