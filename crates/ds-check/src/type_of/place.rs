//! Places: field access, index reads, the assignment sugar's place chains
//! and their projection steps, and the `mut` window gates a place root shares
//! with a `mut` member call's receiver.

use super::*;

/// The quiet answer of a field lookup on a solved receiver type.
pub(super) enum FieldLookup {
    /// The receiver's solved set is not one struct atom.
    NotAStruct,
    /// One struct atom, but it names no such field.
    Missing,
    /// The field's written type solves to nothing.
    Unsolved,
    /// The field's solved type.
    Found(Ty),
}

impl Bodies<'_> {
    /// Bare field access `x.f`: struct-owned, type-directed; on a
    /// generic application through the constructor's shape and the
    /// application's argument substitution.
    pub(super) fn field_access(&mut self, recv: &Expr, name: &Ident) -> Val {
        // A module alias's qualified member in value position: the
        // resolver bound a linked const's def at the member's span —
        // the same res its selection spelling carries at its own use —
        // so the use types as the bound name's value and the receiver
        // is no value of its own. A member that refused at the seam
        // reports nothing here: the resolver named the boundary, and
        // the receiver's module-alias word would only cascade over
        // it.
        if let Some(head) = single_path_ident(recv)
            && let Some(Res::Module(module)) = self.res.get(head.span)
            && self.package.module_ref(*module).linked()
        {
            match self.res.get(name.span) {
                Some(Res::Def(def)) if self.decls.foreign_consts.contains_key(def) => {
                    let def = *def;
                    return self.def_value(def, name.span, None);
                },
                _ => {
                    self.solve_reported = true;
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                },
            }
        }
        // The comptime record binder projects its `name` member (the
        // copy's field name) and its `tag` member (the field type's
        // nominal name) as string constants; the `type` member is a
        // comptime fact — no runtime word carries a type value, and its
        // comptime staging is a per-copy fact the evaluator's span-once
        // answers cannot hold yet. The receiver is no value, so the
        // walk never stages it.
        if self.comptime_record_recv(recv).is_some() {
            if name.name == "name" || name.name == "tag" {
                let string_ty = self
                    .named_atom_ty(name.span, "string", "the record's member")
                    .unwrap_or_else(|| self.empty());
                let poisoned = !self.is_string_ty(string_ty);
                return Val {
                    ty: string_ty,
                    diverges: false,
                    poisoned,
                };
            }
            self.error(
                ErrorCode::InferenceFailed,
                "the comptime record projects its `name` and `tag` members as string constants in \
                 runtime code; its `type` member is a comptime fact no runtime word carries yet"
                    .to_owned(),
                name.span,
                "not a record string member",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        }
        let recv_val = self.expr(recv, None);
        // A poisoned receiver already reported; a field-access complaint
        // over its (empty or wrapper) type would be a cascade.
        if recv_val.poisoned {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        }
        // The field lookup is the one table walk the value access and
        // the dot-call's field tier read.
        match self.lookup_field(recv_val.ty, name) {
            FieldLookup::NotAStruct => {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "field access needs one struct, `{}` given; narrow with `is` first [B2]",
                        self.decls.universe.render(recv_val.ty)
                    ),
                    name.span,
                    "not a single struct",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            FieldLookup::Missing => {
                self.no_field(recv_val.ty, name);
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            FieldLookup::Unsolved => {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("field `{}` does not solve", name.name),
                        name.span,
                        "unsolvable field type",
                    );
                }
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            FieldLookup::Found(ty) => Val {
                ty,
                diverges: false,
                poisoned: false,
            },
        }
    }

    /// The single-struct receiver's field lookup, quiet: the field-access
    /// value walk and the dot-call's field tier read one table walk.
    /// [`FieldLookup::NotAStruct`] when the receiver's solved set is not
    /// one struct atom, [`FieldLookup::Missing`] when that atom names no
    /// such field, [`FieldLookup::Unsolved`] when the written field type
    /// solves to nothing, [`FieldLookup::Found`] with the solved type
    /// otherwise.
    pub(super) fn lookup_field(&mut self, recv_ty: Ty, name: &Ident) -> FieldLookup {
        let members = self.decls.universe.members(recv_ty);
        let Some(single) = members.first().filter(|_| members.len() == 1).copied() else {
            return FieldLookup::NotAStruct;
        };
        let kind = self.decls.universe.atom_decl(single).ok().map(|d| &d.kind);
        let (def, subst): (DefId, FxHashMap<String, Ty>) = match kind {
            Some(AtomKind::Apply(ctor, args)) => {
                let Some(def) = self.decls.defs_of_atoms.get(ctor).copied() else {
                    return FieldLookup::Missing;
                };
                let mut subst = FxHashMap::default();
                if let Some(shape) = self.decls.structs.get(&def) {
                    for (param, arg) in shape.params.iter().zip(args.iter()) {
                        subst.insert(param.name.clone(), *arg);
                    }
                }
                (def, subst)
            },
            _ => match self.decls.defs_of_atoms.get(&single).copied() {
                Some(def) if self.decls.structs.contains_key(&def) => (def, FxHashMap::default()),
                _ => return FieldLookup::Missing,
            },
        };
        let Some(field_ty) = self.decls.structs.get(&def).and_then(|shape| {
            shape
                .fields
                .iter()
                .find(|field| field.name.name == name.name)
                .map(|field| field.ty.clone())
        }) else {
            return FieldLookup::Missing;
        };
        self.type_vars.push(subst);
        let solved = self.solve_here(&field_ty);
        self.type_vars.pop();
        match solved {
            Some(ty) => FieldLookup::Found(ty),
            None => FieldLookup::Unsolved,
        }
    }

    pub(super) fn no_field(&mut self, recv_ty: Ty, name: &Ident) {
        self.error(
            ErrorCode::TypeMismatch,
            format!(
                "`{}` has no field `{}`",
                self.decls.universe.render(recv_ty),
                name.name
            ),
            name.span,
            "no such field",
        );
    }

    /// `e?` propagation: the success part survives, every propagated member
    /// must be contained in the written return union.
    pub(super) fn try_expr(&mut self, operand: &Expr, expected: Option<Ty>, span: Span) -> Val {
        let val = self.expr(operand, None);
        let total = val.ty;
        let ret = self.ret;
        let (ok, propagated) = match (expected, ret) {
            (Some(expected), _) => {
                let ok = self.decls.universe.inter(total, expected);
                let propagated = self.decls.universe.diff(total, expected);
                (ok, propagated)
            },
            (None, Some(ret)) => {
                let ok = self.decls.universe.diff(total, ret);
                let propagated = self.decls.universe.inter(total, ret);
                (ok, propagated)
            },
            (None, None) => {
                // No return union to split against: a top-level
                // initializer (no enclosing function), a fn without a
                // declared return, or a lambda whose return is inferred
                // (no annotation, or a bare type-variable expectation
                // from a generic callee) — the split needs a known
                // destination, never a silent whole-union bind.
                self.error(
                    ErrorCode::PropagationNotContained,
                    "`?` needs an enclosing return union to propagate into [E4]".to_owned(),
                    span,
                    "stray propagation",
                );
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            },
        };
        if let Some(ret) = ret {
            let escape = self.decls.universe.diff(propagated, ret);
            if !self.decls.universe.is_empty(escape) && !val.poisoned {
                self.error(
                    ErrorCode::PropagationNotContained,
                    format!(
                        "`?` propagates `{}`, which the return union `{}` does not contain [E4]",
                        self.decls.universe.render(escape),
                        self.decls.universe.render(ret)
                    ),
                    span,
                    "not contained",
                );
            }
        }
        if self.decls.universe.is_empty(ok) && !self.decls.universe.is_empty(total) && !val.poisoned
        {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`?` leaves nothing: every member of `{}` propagates [E4]",
                    self.decls.universe.render(total)
                ),
                span,
                "empty success part",
            );
        }
        Val {
            ty: ok,
            diverges: false,
            poisoned: val.poisoned,
        }
    }

    pub(super) fn assign(
        &mut self,
        op: ds_parse::AssignOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Val {
        let unit = self.unit();
        let chain = place_chain(left);
        match chain {
            // The whole-binding place.
            PlaceShape::Binding => self.assign_binding(op, left, right, span),
            PlaceShape::Chain(chain) => self.assign_place(&chain, op, right, left.span(), span),
            PlaceShape::Unsupported => {
                self.error(
                    ErrorCode::InferenceFailed,
                    "assignment places are bindings or field/subscript paths rooted at a `var` \
                     binding [D2]"
                        .to_owned(),
                    left.span(),
                    "unsupported place",
                );
                self.expr(right, None);
                Val {
                    ty: unit,
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// One assignment whose place is a field/subscript chain (the
    /// assignment sugar): the root binding must be an
    /// owned `var` — a const receiver refuses as the mut-window
    /// spelling DS3006, the same gate a `mut` member call's receiver
    /// passes — every field step descends one single-member struct, and
    /// every index step resolves the ambient collection's element type
    /// while checking the probe expression against its parameter type.
    /// A plain store's value checks against the leaf type; a compound
    /// form reads the leaf's current value (its declared type: no
    /// refinement tracks a place) through the same operator table the
    /// binding form reads, the result landing back in the leaf. The
    /// walked chain records at the assignment's own span for the
    /// lowering to synthesize the store — exclusivity rides the existing
    /// affine gates, no new mutation channel.
    pub(super) fn assign_place(
        &mut self,
        chain: &PlaceChain<'_>,
        op: ds_parse::AssignOp,
        right: &Expr,
        left_span: Span,
        span: Span,
    ) -> Val {
        let unit = self.unit();
        let root = chain.root;
        let Some(res) = self.res.get(root.span).cloned() else {
            self.error(
                ErrorCode::UnresolvedName,
                format!("the name `{}` does not resolve", root.name),
                root.span,
                "unresolved place root",
            );
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        };
        // The root is a local binding alone: a place chain rooted at a
        // top-level binding keeps the plain assignment's own lowering
        // door.
        let Res::Local { bind } = res else {
            self.error(
                ErrorCode::InferenceFailed,
                "a place path's root stays a local binding; module-level storage arrives with \
                 cross-module state [G4, M7]"
                    .to_owned(),
                root.span,
                "module-level root",
            );
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        };
        if self.gate_mut_window_root(root, &res) {
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        }
        let Some(binding) = self.locals.get(bind) else {
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        };
        let root_ty = self.refined.get(bind).copied().unwrap_or(binding.ty);
        if !crate::linearity::ty_copyable(self.decls, root_ty, &self.linear_vars) {
            self.error(
                ErrorCode::InferenceFailed,
                "a place path needs a copyable root; a `~Copyable` value's field mutation \
                 composes with its deinit claim's own seam [C5]"
                    .to_owned(),
                root.span,
                "linear root",
            );
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        }
        // Walk the steps: every field descends one struct member, every
        // index resolves the collection's element type and checks the
        // probe expression against it.
        let mut ty = root_ty;
        let mut steps: Vec<PlaceStep> = Vec::with_capacity(chain.steps.len());
        for step in &chain.steps {
            match step {
                ChainStep::Field { name } => {
                    let Some(step_ty) = self.field_step_ty(ty, name) else {
                        self.expr(right, None);
                        return Val {
                            ty: unit,
                            diverges: false,
                            poisoned: false,
                        };
                    };
                    steps.push(PlaceStep::Field {
                        name: name.name.clone(),
                        ty: step_ty,
                    });
                    ty = step_ty;
                },
                ChainStep::Keyed { name } => {
                    // The comptime constant resolves to the active
                    // copy's field; the step records as that written
                    // field, so the lowering synthesizes the store
                    // unchanged.
                    let Some((_, field_name)) = self.comptime_name_key(name) else {
                        self.error(
                            ErrorCode::InferenceFailed,
                            "the keyed access names a `for comptime` record's `name` member \
                             (`x.[f.name]`); this key spells no such constant"
                                .to_owned(),
                            name.span(),
                            "not a comptime field name",
                        );
                        self.expr(right, None);
                        return Val {
                            ty: unit,
                            diverges: false,
                            poisoned: true,
                        };
                    };
                    let resolved = Ident {
                        span: name.span(),
                        name: field_name,
                    };
                    let Some(step_ty) = self.field_step_ty(ty, &resolved) else {
                        self.expr(right, None);
                        return Val {
                            ty: unit,
                            diverges: false,
                            poisoned: false,
                        };
                    };
                    steps.push(PlaceStep::Field {
                        name: resolved.name.clone(),
                        ty: step_ty,
                    });
                    ty = step_ty;
                },
                ChainStep::Index { index } => {
                    let Some((native, elem)) = self.index_step_ty(ty) else {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "subscript assignment rides the ambient collections' `set` \
                                 member; `{}` is not one",
                                self.decls.universe.render(ty)
                            ),
                            index.span(),
                            "not a collection",
                        );
                        self.expr(right, None);
                        return Val {
                            ty: unit,
                            diverges: false,
                            poisoned: false,
                        };
                    };
                    // The probe expression checks against the collection's
                    // own parameter type: the list's int index, the map's
                    // key.
                    let probe_ty = match native {
                        StoreIndex::List => {
                            self.named_atom_ty(index.span(), "int", "the list's index type")
                        },
                        StoreIndex::Map => self.index_key_ty(ty),
                    };
                    let probe_val = self.expr(index, probe_ty);
                    if let Some(probe_ty) = probe_ty {
                        self.expect_contains(probe_val, probe_ty, index.span(), "subscript probe");
                    }
                    steps.push(PlaceStep::Index { native, elem });
                    ty = elem;
                },
            }
        }
        let leaf = ty;
        if op == ds_parse::AssignOp::Plain {
            let val = self.expr(right, Some(leaf));
            self.expect_contains(val, leaf, right.span(), "assignment");
        } else {
            let right_val = self.expr(right, Some(leaf));
            let result = self.binary_val(
                compound_op(op),
                leaf,
                right_val.ty,
                left_span,
                right.span(),
                span,
                right_val.poisoned,
            );
            self.expect_contains(result, leaf, span, "compound assignment");
        }
        self.record_place_store(span, PlaceStore {
            binder: bind,
            root_ty,
            steps,
        });
        Val {
            ty: unit,
            diverges: false,
            poisoned: false,
        }
    }

    /// The whole-binding assignment, the pre-sugar surface: the place
    /// is a single-segment path to a binding, the compound forms read
    /// the current value, and a plain store resets the refinement.
    pub(super) fn assign_binding(
        &mut self,
        op: ds_parse::AssignOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Val {
        let unit = self.unit();
        let place = match peel_paren(left) {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                self.res.get(path.segments[0].span).cloned()
            },
            _ => None,
        };
        let Some(res) = place else {
            self.error(
                ErrorCode::InferenceFailed,
                "assignment places are bindings or field/subscript paths rooted at a `var` \
                 binding [D2]"
                    .to_owned(),
                left.span(),
                "unsupported place",
            );
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        };
        let (declared, bind_span, mutable) = match &res {
            Res::Local { bind } => match self.locals.get(*bind) {
                Some(binding) => (binding.ty, Some(*bind), binding.mutable),
                None => (self.empty(), None, false),
            },
            Res::Def(def) => {
                let mutable = self.package.def(*def).kind == DefKind::Var;
                let val = self.const_ty(*def, left.span());
                (val.ty, None, mutable)
            },
            _ => (self.empty(), None, false),
        };
        if !mutable {
            self.error(
                ErrorCode::TypeMismatch,
                "assignment needs a `var` binding; `const` bindings are immutable [C3]".to_owned(),
                left.span(),
                "immutable place",
            );
        }
        if op == ds_parse::AssignOp::Plain {
            let val = self.expr(right, Some(declared));
            self.expect_contains(val, declared, right.span(), "assignment");
        } else {
            let current = bind_span
                .and_then(|span| self.refined.get(span).copied())
                .unwrap_or(declared);
            let right_val = self.expr(right, Some(current));
            let result = self.binary_val(
                compound_op(op),
                current,
                right_val.ty,
                left.span(),
                right.span(),
                span,
                right_val.poisoned,
            );
            self.expect_contains(result, declared, span, "compound assignment");
        }
        // Refinement resets at assignment.
        if let Some(bind_span) = bind_span {
            self.refined.remove(bind_span);
        }
        Val {
            ty: unit,
            diverges: false,
            poisoned: false,
        }
    }

    /// One field step's solved type: the receiver must be one
    /// single-member struct and the field must exist, its declared type
    /// solved under the application's substitution.
    pub(super) fn field_step_ty(&mut self, recv_ty: Ty, name: &Ident) -> Option<Ty> {
        let members = self.decls.universe.members(recv_ty);
        let single = members.first().filter(|_| members.len() == 1).copied();
        let Some(single) = single else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "field access needs one struct, `{}` given; narrow with `is` first [B2]",
                    self.decls.universe.render(recv_ty)
                ),
                name.span,
                "not a single struct",
            );
            return None;
        };
        let kind = self.decls.universe.atom_decl(single).ok().map(|d| &d.kind);
        let (def, subst): (DefId, FxHashMap<String, Ty>) = match kind {
            Some(AtomKind::Apply(ctor, args)) => {
                if let Some(def) = self.decls.defs_of_atoms.get(ctor).copied() {
                    let mut subst = FxHashMap::default();
                    if let Some(shape) = self.decls.structs.get(&def) {
                        for (param, arg) in shape.params.iter().zip(args.iter()) {
                            subst.insert(param.name.clone(), *arg);
                        }
                    }
                    (def, subst)
                } else {
                    self.no_field(recv_ty, name);
                    return None;
                }
            },
            _ => match self.decls.defs_of_atoms.get(&single).copied() {
                Some(def) if self.decls.structs.contains_key(&def) => (def, FxHashMap::default()),
                _ => {
                    self.no_field(recv_ty, name);
                    return None;
                },
            },
        };
        let field_ty = self
            .decls
            .structs
            .get(&def)
            .and_then(|shape| {
                shape
                    .fields
                    .iter()
                    .find(|field| field.name.name == name.name)
            })
            .map(|field| field.ty.clone());
        let Some(field_ty) = field_ty else {
            self.no_field(recv_ty, name);
            return None;
        };
        self.type_vars.push(subst);
        let field_ty = self.solve_here(&field_ty);
        self.type_vars.pop();
        if let Some(ty) = field_ty {
            return Some(ty);
        }
        if !self.take_solve_reported() {
            self.error(
                ErrorCode::TypeMismatch,
                format!("field `{}` does not solve", name.name),
                name.span,
                "unsolvable field type",
            );
        }
        None
    }

    /// A value-position subscript read: the receiver must name one
    /// ambient collection, and the read carries the assignment place's
    /// resolution semantics — the probe checks against the collection's
    /// own parameter type (the list's `int` index, the map's key) and
    /// the answer is the element value itself, so an out-of-range list
    /// index traps and an absent map key halts at the read while the
    /// `get` members keep the checked `t | none` spelling.
    pub(super) fn index_read(&mut self, span: Span, recv: &Expr, index: &Expr) -> Val {
        let recv_val = self.expr(recv, None);
        let members = self.decls.universe.members(recv_val.ty).to_vec();
        let single = members.first().filter(|_| members.len() == 1).copied();
        let route = single.and_then(|single| {
            let AtomKind::Apply(ctor, args) = &self.decls.universe.atom_decl(single).ok()?.kind
            else {
                return None;
            };
            let def = self.decls.defs_of_atoms.get(ctor).copied()?;
            if Some(def) == self.package.lookup_type("list") {
                return Some((*args.first()?, None));
            }
            if Some(def) == self.package.lookup_type("map") {
                return Some((*args.get(1)?, args.first().copied()));
            }
            None
        });
        let Some((elem, key)) = route else {
            // A poisoned receiver already reported; the collection-shape
            // complaint on top of it would be a cascade.
            if !recv_val.poisoned {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "a subscript read rides the ambient collections; `{}` is not one",
                        self.decls.universe.render(recv_val.ty)
                    ),
                    span,
                    "not a collection",
                );
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: recv_val.poisoned,
            };
        };
        // The list's probe is the ambient `int`; the map's is its key.
        let probe_ty = match key {
            None => self.named_atom_ty(span, "int", "the list's index"),
            Some(key) => Some(key),
        };
        let probe_val = self.expr(index, probe_ty);
        if let Some(probe_ty) = probe_ty {
            self.expect_contains(probe_val, probe_ty, index.span(), "subscript probe");
        }
        Val {
            ty: elem,
            diverges: false,
            poisoned: false,
        }
    }

    /// One index step's collection and element type: the receiver must
    /// name the ambient `list[t]` (element `t`) or `map[k, v]` (value
    /// `v`) — the subscript store desugars to their `set` members alone.
    pub(super) fn index_step_ty(&mut self, recv_ty: Ty) -> Option<(StoreIndex, Ty)> {
        let members = self.decls.universe.members(recv_ty);
        let single = members.first().filter(|_| members.len() == 1).copied();
        let single = single?;
        let AtomKind::Apply(ctor, args) = &self.decls.universe.atom_decl(single).ok()?.kind else {
            return None;
        };
        let def = self.decls.defs_of_atoms.get(ctor).copied()?;
        if Some(def) == self.package.lookup_type("list") {
            return Some((StoreIndex::List, *args.first()?));
        }
        if Some(def) == self.package.lookup_type("map") {
            return Some((StoreIndex::Map, *args.get(1)?));
        }
        None
    }

    /// The map receiver's key type, the probe expression's expected type.
    pub(super) fn index_key_ty(&mut self, recv_ty: Ty) -> Option<Ty> {
        let members = self.decls.universe.members(recv_ty);
        let single = members.first().filter(|_| members.len() == 1).copied()?;
        let AtomKind::Apply(ctor, args) = &self.decls.universe.atom_decl(single).ok()?.kind else {
            return None;
        };
        let def = self.decls.defs_of_atoms.get(ctor).copied()?;
        (Some(def) == self.package.lookup_type("map"))
            .then(|| args.first().copied())
            .flatten()
    }

    /// The mut-window receiver gates a place chain's root shares with a
    /// `mut` member call's receiver: the for-in iterated binding (its
    /// element count froze at loop entry, DS3007), the nursery-outlived
    /// binding inside a spawned body, then the const-binding
    /// refusal itself (DS3006) — the place store is a `mut` window
    /// spelled as an assignment. Returns whether the site refused.
    pub(super) fn gate_mut_window_root(&mut self, root: &Ident, res: &Res) -> bool {
        let bind = match res {
            Res::Local { bind } => Some(IterBind::Local(*bind)),
            Res::Def(def) => Some(IterBind::Def(*def)),
            _ => None,
        };
        if let Some(bind) = bind
            && let Some(iter_at) = self.iterated_at(bind)
        {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ForIterMutWindow,
                    format!(
                        "`{}` is this loop's iterated binding; its element count froze at loop \
                         entry, so a `mut` window over it inside the body never joins this loop's \
                         walk [D3]",
                        root.name
                    ),
                )
                .with_primary(root.span, "mut window over the iterated binding")
                .with_secondary(iter_at, "iterated here"),
            );
            return true;
        }
        if self.spawn_depth > 0
            && let Some((outlives, nursery_at)) = self.nursery_scopes.last().map(|scope| {
                let outlives = match res {
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
                        "`{}` outlives this nursery block; a spawned body may never hold a `mut` \
                         window over it [E2]",
                        root.name
                    ),
                )
                .with_primary(root.span, "mut window over an outlived binding")
                .with_secondary(nursery_at, "the nursery block joins here"),
            );
            return true;
        }
        let (binder, mutable, ty) = match res {
            Res::Local { bind } => {
                let Some(binding) = self.locals.get(*bind) else {
                    return false;
                };
                (*bind, binding.mutable, binding.ty)
            },
            Res::Def(def) => {
                let def_info = self.package.def(*def);
                if def_info.kind != DefKind::Const {
                    return false;
                }
                let Some(ty) = self.def_types.get(def).copied() else {
                    return false;
                };
                (def_info.span, false, ty)
            },
            _ => return false,
        };
        if mutable
            || self.decls.universe.is_empty(ty)
            || !crate::linearity::ty_copyable(self.decls, ty, &self.linear_vars)
        {
            return false;
        }
        self.diags.push(
            Diagnostic::error(
                ErrorCode::MutWindowConstReceiver,
                format!(
                    "`{}` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]",
                    root.name
                ),
            )
            .with_secondary(binder, "bound here")
            .with_primary(root.span, "const receiver"),
        );
        true
    }
}

/// One projection step of an assignment place chain (the assignment
/// sugar): a field of the receiver or an index over it.
#[derive(Clone, Copy)]
pub(super) enum ChainStep<'a> {
    /// A field projection.
    Field {
        /// The field's name occurrence.
        name: &'a Ident,
    },
    /// A comptime-keyed projection (`x.[f.name]`): the constant
    /// resolves to the active copy's field at the assignment check.
    Keyed {
        /// The comptime constant name expression.
        name: &'a Expr,
    },
    /// A subscript projection.
    Index {
        /// The probe expression (a list's index, a map's key).
        index: &'a Expr,
    },
}

/// One assignment place chain: the root path segment plus its
/// projection steps in source order.
pub(super) struct PlaceChain<'a> {
    root: &'a Ident,
    steps: Vec<ChainStep<'a>>,
}

/// The place shape of an assignment's left side (the assignment sugar):
/// the shape names no place at all (the caller's unsupported-place
/// refusal), a plain binding path, or a projection chain over a
/// single-segment root.
pub(super) enum PlaceShape<'a> {
    Unsupported,
    Binding,
    Chain(PlaceChain<'a>),
}

fn place_chain(left: &Expr) -> PlaceShape<'_> {
    let mut steps: Vec<ChainStep<'_>> = Vec::new();
    let mut recv = peel_paren(left);
    loop {
        match recv {
            Expr::Field {
                recv: inner, name, ..
            } => {
                steps.push(ChainStep::Field { name });
                recv = peel_paren(inner);
            },
            Expr::FieldKey {
                recv: inner, name, ..
            } => {
                steps.push(ChainStep::Keyed { name });
                recv = peel_paren(inner);
            },
            Expr::Index {
                recv: inner, index, ..
            } => {
                steps.push(ChainStep::Index { index });
                recv = peel_paren(inner);
            },
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                if steps.is_empty() {
                    return PlaceShape::Binding;
                }
                steps.reverse();
                return PlaceShape::Chain(PlaceChain {
                    root: &path.segments[0],
                    steps,
                });
            },
            _ => return PlaceShape::Unsupported,
        }
    }
}
