//! Constructors: struct, list, and map literals, the applied constructor
//! calls, and the applied head types the builtin collections answer through.

use super::*;

impl Bodies<'_> {
    pub(super) fn struct_expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Struct { path, fields, span } = e else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let [only] = path.segments.as_slice() else {
            self.error(
                ErrorCode::UnresolvedName,
                "qualified struct literals resolve through imports [G4]".to_owned(),
                path.span,
                "qualified path",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let Some(def) = self
            .package
            .lookup_type(&only.name)
            .or_else(|| self.case_def_of(&only.name))
            .or_else(|| self.decls.foreign_type(&only.name))
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }; // resolver reported
        };
        // An enum case constructs by call form only; the brace form
        // is the struct construction, so this is the cross-spelling. A
        // payloaded tag keeps the same discipline.
        if self.decls.cases.contains_key(&def)
            || (self.package.def(def).kind == DefKind::Tag && self.decls.structs.contains_key(&def))
        {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` is an enum case; construct it by call form: `{}(...)`",
                    only.name, only.name
                ),
                only.span,
                "brace form on an enum case",
            );
            for (_, value) in fields {
                if let Some(expr) = value {
                    self.expr(expr, None);
                }
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let Some(shape) = self.decls.structs.get(&def).cloned() else {
            self.error(
                ErrorCode::TypeMismatch,
                format!("`{}` does not construct a struct", only.name),
                only.span,
                "not a struct",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        // Substitute the constructor's parameters: from an expected
        // application of the same constructor, else from in-scope variables
        // named like the parameters (generic construction inside its own
        // scope).
        let mut subst = FxHashMap::default();
        if !shape.params.is_empty() {
            if let Some(expected) = expected
                && let Some((ctor, args)) = self.single_apply(expected)
                && self.decls.defs_of_atoms.get(&ctor) == Some(&def)
            {
                for (param, arg) in shape.params.iter().zip(args.iter()) {
                    subst.insert(param.name.clone(), *arg);
                }
            }
            let scoped: Vec<(String, Ty)> = shape
                .params
                .iter()
                .filter_map(|p| {
                    self.type_vars
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(&p.name).copied())
                        .map(|ty| (p.name.clone(), ty))
                })
                .collect();
            for (name, ty) in scoped {
                subst.entry(name).or_insert(ty);
            }
        }
        self.type_vars.push(subst);
        let mut written = vec![false; shape.fields.len()];
        for (name, value) in fields {
            let Some(index) = shape.fields.iter().position(|f| f.name.name == name.name) else {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!("`{}` has no field `{}`", only.name, name.name),
                    name.span,
                    "no such field",
                );
                continue;
            };
            written[index] = true;
            let field_ty = self.solve_here(&shape.fields[index].ty);
            let Some(field_ty) = field_ty else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("field `{}` of `{}` does not solve", name.name, only.name),
                        name.span,
                        "unsolvable field type",
                    );
                }
                continue;
            };
            match value {
                Some(expr) => {
                    let val = self.expr(expr, Some(field_ty));
                    self.expect_contains(val, field_ty, expr.span(), "field value");
                },
                None => {
                    // Punning: the field name is itself a value use.
                    if let Some(res) = self.res.get(name.span) {
                        let val = match res {
                            Res::Local { bind } => Val {
                                ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                                diverges: false,
                                poisoned: false,
                            },
                            Res::Def(def) => self.def_value(*def, name.span, Some(field_ty)),
                            _ => Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: false,
                            },
                        };
                        self.expect_contains(val, field_ty, name.span, "field value");
                    }
                },
            }
        }
        for (index, &is_written) in written.iter().enumerate() {
            if !is_written {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "`{}` literal misses field `{}`",
                        only.name, shape.fields[index].name.name
                    ),
                    *span,
                    "missing field",
                );
            }
        }
        let ty = if shape.params.is_empty() {
            self.decls
                .atoms
                .get(&def)
                .and_then(|&a| self.decls.universe.atom_ty(a).ok())
                .unwrap_or_else(|| self.empty())
        } else {
            let bound: Option<Vec<Ty>> = shape
                .params
                .iter()
                .map(|p| self.type_vars.last().and_then(|s| s.get(&p.name).copied()))
                .collect();
            if let Some(args) = bound {
                let ctor = self.decls.atoms.get(&def).copied().unwrap_or(TypeId::NONE);
                self.decls
                    .universe
                    .apply_atom(ctor, &args)
                    .ok()
                    .and_then(|a| self.decls.universe.atom_ty(a).ok())
                    .unwrap_or_else(|| self.empty())
            } else {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "constructing generic `{}` needs an expected type or in-scope parameters \
                         [B2]",
                        only.name
                    ),
                    *span,
                    "unbound generic construction",
                );
                self.empty()
            }
        };
        self.type_vars.pop();
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// One list literal `[a, b, c]`. Bidirectional: an expected
    /// `list[t]` fixes the element type and every item checks against it;
    /// without one, the first item fixes the type and a later item
    /// outside it is the mixed-element error. The empty literal has no
    /// element evidence — it needs the expected type, and `list[t]()`
    /// is the always-spellable empty form.
    pub(super) fn list_expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::List { span, items, .. } = e else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let Some(list_def) = self.package.lookup_type("list") else {
            for item in items {
                self.expr(item, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }; // the resolver reported the unknown type
        };
        let expected_elem = expected.and_then(|exp| {
            let (ctor, args) = self.single_apply(exp)?;
            let [elem] = args.as_slice() else {
                return None;
            };
            (self.decls.defs_of_atoms.get(&ctor) == Some(&list_def)).then_some(*elem)
        });
        let elem = if items.is_empty() {
            let Some(elem) = expected_elem else {
                self.error(
                    ErrorCode::InferenceFailed,
                    "the empty list literal needs an expected `list[t]`; write `list[t]()` for \
                     the explicit empty list [B3]"
                        .to_owned(),
                    *span,
                    "empty list literal",
                );
                // downstream empty-type fallbacks must not re-report
                self.solve_reported = true;
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            };
            elem
        } else if let Some(elem) = expected_elem {
            elem
        } else {
            self.expr(&items[0], None).ty
        };
        for (index, item) in items.iter().enumerate() {
            if index == 0 && expected_elem.is_none() {
                continue; // walked above for the element evidence
            }
            let val = self.expr(item, Some(elem));
            if val.diverges || self.decls.universe.contains(elem, val.ty) {
                continue;
            }
            match expected_elem {
                Some(_) => {
                    self.expect_contains(val, elem, item.span(), "list element");
                },
                None => {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "list literal mixes element types: item {} has type `{}`, the first \
                             item `{}` [B2]",
                            index + 1,
                            self.decls.universe.render(val.ty),
                            self.decls.universe.render(elem)
                        ),
                        item.span(),
                        "mixed element types",
                    );
                },
            }
        }
        let ty = self.applied_list_ty(elem).unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// One generic-application constructor call `list[page]()`:
    /// the head resolved in the type namespace, so the bracketed form is
    /// a type application, never an index. Returns `None` when the head
    /// names no type — the ordinary index error reports instead.
    pub(super) fn ctor_apply_call(
        &mut self,
        recv: &Expr,
        index: &Expr,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Option<Val> {
        let head = single_path_ident(recv)?;
        let def = self.package.lookup_type(&head.name)?;
        let walk_args = |this: &mut Self| {
            for arg in args {
                this.expr(&arg.value, None);
            }
            if let Some(tail) = trailing {
                this.expr(tail, None);
            }
        };
        let is_list = |this: &Self| {
            this.decls
                .structs
                .get(&def)
                .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty())
        };
        // the channel's construction: the one type application that
        // carries a value
        // argument — the bound, `chan[int](4)` — because the payload
        // type never reaches the code (every channel one Ref word) while
        // the bound does
        let is_chan = |this: &Self| {
            head.name.as_str() == "chan"
                && this
                    .decls
                    .structs
                    .get(&def)
                    .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty())
        };
        // the owning container's construction: the one other type
        // application carrying a value argument — the held value,
        // `box[t](x)`
        let is_box = |this: &Self| {
            head.name.as_str() == "box"
                && this
                    .decls
                    .structs
                    .get(&def)
                    .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty())
        };
        if !is_list(self) && !is_chan(self) && !is_box(self) {
            // Construction is fixed per declaration; every type
            // application besides the collection surface is the
            // cross-spelling error.
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` constructs per its declaration, not by type application [B3]",
                    head.name
                ),
                span,
                "cross-spelling construction",
            );
            walk_args(self);
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        let chan_bound = if is_chan(self) {
            if trailing.is_some() {
                self.error(
                    ErrorCode::TypeMismatch,
                    "the channel constructor takes no trailing block".to_owned(),
                    span,
                    "trailing block",
                );
                self.solve_reported = true;
                return Some(self.empty_val());
            }
            let [cap] = args else {
                self.error(
                    ErrorCode::TypeMismatch,
                    "the channel constructor takes the bound alone: `chan[t](n)`".to_owned(),
                    span,
                    "constructor arguments",
                );
                walk_args(self);
                self.solve_reported = true;
                return Some(self.empty_val());
            };
            let cap_val = self.expr(&cap.value, None);
            let bound = self.named_ty(cap.value.span(), "int", "the channel bound");
            self.expect_contains(cap_val, bound.ty, cap.value.span(), "the channel bound");
            true
        } else {
            false
        };
        if trailing.is_some() {
            self.error(
                ErrorCode::TypeMismatch,
                "a type application takes no trailing block".to_owned(),
                span,
                "trailing block",
            );
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        if !args.is_empty() && !chan_bound && !is_box(self) {
            self.error(
                ErrorCode::TypeMismatch,
                "the empty-list constructor takes no arguments; build with a list literal `[...]` \
                 [B3]"
                    .to_owned(),
                span,
                "constructor arguments",
            );
            walk_args(self);
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        let Some(arg_ty) = Self::ctor_type_arg(index) else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "the bracketed argument of `{}[...]` is a type, not a value [F9]",
                    head.name
                ),
                index.span(),
                "not a type",
            );
            self.solve_reported = true;
            return Some(self.empty_val());
        };
        let Some(elem) = self.solve_here(&arg_ty) else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "the type argument `{}` does not solve against the package declarations",
                        decls::render_type(&arg_ty)
                    ),
                    arg_ty.span(),
                    "unsolvable type argument",
                );
            }
            self.solve_reported = true;
            return Some(self.empty_val());
        };
        // the owning container's construction: the held value stages
        // against the written element type and moves in; a `~Copyable`
        // value refuses — the box object owns one count per `Ref` word
        // and its death has no position to fire a user deinit, so a
        // linear value's hook would die with the container
        if is_box(self) {
            if trailing.is_some() {
                self.error(
                    ErrorCode::TypeMismatch,
                    "the box constructor takes no trailing block".to_owned(),
                    span,
                    "trailing block",
                );
                self.solve_reported = true;
                return Some(self.empty_val());
            }
            let [value] = args else {
                self.error(
                    ErrorCode::TypeMismatch,
                    "the box constructor takes the value alone: `box[t](x)`".to_owned(),
                    span,
                    "constructor arguments",
                );
                self.solve_reported = true;
                return Some(self.empty_val());
            };
            let val = self.expr(&value.value, None);
            self.expect_contains(val, elem, value.value.span(), "the box value");
            if !crate::linearity::ty_copyable(self.decls, elem, &self.linear_vars) {
                self.error(
                    ErrorCode::TypeMismatch,
                    "a `~Copyable` value refuses the box: the container's death owns counts, \
                     never claims, so the value's deinit has no firing position"
                        .to_owned(),
                    value.value.span(),
                    "non-copyable box value",
                );
            }
            let ty = self.applied_box_ty(elem)?;
            return Some(Val {
                ty,
                diverges: false,
                poisoned: false,
            });
        }
        // the arena's construction (the read-side aliasing container):
        // the empty-arena spelling `arena[t]()` — one more type
        // application constructing by call form, the element type the
        // only argument
        let is_arena = |this: &Self| {
            head.name.as_str() == "arena"
                && this
                    .decls
                    .structs
                    .get(&def)
                    .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty())
        };
        let ty = if chan_bound {
            self.applied_chan_ty(elem)?
        } else if is_arena(self) {
            self.applied_arena_ty(elem)?
        } else {
            self.applied_list_ty(elem)?
        };
        Some(Val {
            ty,
            diverges: false,
            poisoned: false,
        })
    }

    /// The map constructor call `map[k, v]()`: the mirror of the
    /// list constructor over the two-argument application of the map
    /// struct. Construction takes no value arguments — the builder call
    /// (`map_set`) fills the map; the map literal spelling rides the
    /// anonymous-record-literal reservation.
    pub(super) fn map_ctor_call(
        &mut self,
        head: &Ident,
        key: &Expr,
        value: &Expr,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let walk_args = |this: &mut Self| {
            for arg in args {
                this.expr(&arg.value, None);
            }
            if let Some(tail) = trailing {
                this.expr(tail, None);
            }
        };
        let Some(def) = self.package.lookup_type(&head.name) else {
            // the resolver already reported the unknown type name
            self.solve_reported = true;
            return self.empty_val();
        };
        let is_map = self
            .decls
            .structs
            .get(&def)
            .is_some_and(|shape| shape.params.len() == 2 && shape.fields.is_empty());
        if !is_map {
            // Construction is fixed per declaration; the map
            // constructs by type application, any other head is the
            // cross-spelling error.
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` constructs per its declaration, not by type application [B3]",
                    head.name
                ),
                span,
                "cross-spelling construction",
            );
            walk_args(self);
            self.solve_reported = true;
            return self.empty_val();
        }
        if trailing.is_some() {
            self.error(
                ErrorCode::TypeMismatch,
                "a type application takes no trailing block".to_owned(),
                span,
                "trailing block",
            );
            self.solve_reported = true;
            return self.empty_val();
        }
        if !args.is_empty() {
            self.error(
                ErrorCode::TypeMismatch,
                "the empty-map constructor takes no arguments; build by `map_set` calls [B3]"
                    .to_owned(),
                span,
                "constructor arguments",
            );
            walk_args(self);
            self.solve_reported = true;
            return self.empty_val();
        }
        let arg_ty = |this: &mut Self, arg: &Expr| -> Option<Ty> {
            let Some(ty) = Self::ctor_type_arg(arg) else {
                this.error(
                    ErrorCode::TypeMismatch,
                    "the bracketed arguments of `map[...]` are types, not values [F9]".to_owned(),
                    arg.span(),
                    "not a type",
                );
                this.solve_reported = true;
                return None;
            };
            let Some(solved) = this.solve_here(&ty) else {
                if !this.take_solve_reported() {
                    this.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the type argument `{}` does not solve against the package \
                             declarations",
                            decls::render_type(&ty)
                        ),
                        ty.span(),
                        "unsolvable type argument",
                    );
                }
                this.solve_reported = true;
                return None;
            };
            Some(solved)
        };
        let (Some(key_ty), Some(value_ty)) = (arg_ty(self, key), arg_ty(self, value)) else {
            return self.empty_val();
        };
        let Some(ty) = self.applied_map_ty(key_ty, value_ty) else {
            self.solve_reported = true;
            return self.empty_val();
        };
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// The bracketed argument of a type application: a type spelled in
    /// expression position — a path, a parenthesized form, a tuple of
    /// the same, a fn type (`fn(int) -> int`), or a nested generic
    /// application (`list[string]`, `map[string, int]`) whose own bracketed
    /// arguments recurse.
    pub(super) fn ctor_type_arg(arg: &Expr) -> Option<Type> {
        match arg {
            Expr::Path { span, path, .. } if path.segments.len() == 1 => Some(Type::Path {
                span: *span,
                path: path.clone(),
                args: Vec::new(),
                delim: Delim::Bracket,
            }),
            Expr::FnType { ty, .. } => Some((**ty).clone()),
            Expr::Paren { inner, .. } => Self::ctor_type_arg(inner),
            Expr::Tuple { items, span, .. } => Some(Type::Tuple {
                span: *span,
                items: items.iter().filter_map(Self::ctor_type_arg).collect(),
            }),
            Expr::Index { recv, index, .. } => {
                let head = single_path_ident(recv)?;
                let arg = Self::ctor_type_arg(index)?;
                Some(Type::Path {
                    span: recv.span(),
                    path: Path {
                        span: head.span,
                        segments: vec![head.clone()],
                    },
                    args: vec![arg],
                    delim: Delim::Bracket,
                })
            },
            Expr::MapApply {
                head, key, value, ..
            } => {
                let key = Self::ctor_type_arg(key)?;
                let value = Self::ctor_type_arg(value)?;
                Some(Type::Path {
                    span: head.span,
                    path: Path {
                        span: head.span,
                        segments: vec![head.clone()],
                    },
                    args: vec![key, value],
                    delim: Delim::Bracket,
                })
            },
            _ => None,
        }
    }

    /// The applied `list[t]` type over one element type.
    pub(super) fn applied_list_ty(&mut self, elem: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("list")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[elem]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    /// The applied `box[t]` type over the element type.
    pub(super) fn applied_box_ty(&mut self, elem: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("box")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[elem]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    /// The applied `chan[t]` type over the payload type.
    pub(super) fn applied_chan_ty(&mut self, elem: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("chan")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[elem]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    /// The applied `arena[t]` type over the element type.
    pub(super) fn applied_arena_ty(&mut self, elem: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("arena")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[elem]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    /// The applied `map[k, v]` type over the key and value types.
    pub(super) fn applied_map_ty(&mut self, key: Ty, value: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("map")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[key, value]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }
}
