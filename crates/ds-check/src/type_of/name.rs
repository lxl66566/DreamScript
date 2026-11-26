//! Name and definition resolution at a use site: paths, labels and directed
//! tags, and the value a definition's declared or checked type answers at
//! the use's own span.

use super::*;

impl Bodies<'_> {
    pub(super) fn path_expr(&mut self, path: &Path, expected: Option<Ty>) -> Val {
        if let [only] = path.segments.as_slice() {
            if let Some(res) = self.res.get(only.span) {
                match res {
                    Res::Local { bind } => {
                        // The comptime record binder is no runtime
                        // value; its uses project the `name` member.
                        if self.comptime_record_depth(*bind).is_some() {
                            self.error(
                                ErrorCode::InferenceFailed,
                                "the comptime record is no runtime value; project its `name` \
                                 member (`f.name`) or key a field access with it"
                                    .to_owned(),
                                only.span,
                                "the record binder as a value",
                            );
                            return Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: true,
                            };
                        }
                        Val {
                            ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Res::Def(def) => {
                        // A type name where a `type` value is expected is
                        // the type-value reading: legal only in a
                        // comptime evaluation context — inside a comptime
                        // function's body, or feeding a comptime call's
                        // argument. A struct, enum, tag, or alias names a
                        // declared type; the reflection index it denotes is
                        // the evaluator's, never a runtime word.
                        if expected.is_some_and(|ty| self.is_type_atom(ty))
                            && self.names_declared_type(*def)
                        {
                            if self.comptime.ctx || self.comptime.args {
                                return Val {
                                    ty: self
                                        .decls
                                        .universe
                                        .atom_ty(TypeId::TYPE)
                                        .unwrap_or_else(|_| self.empty()),
                                    diverges: false,
                                    poisoned: false,
                                };
                            }
                            self.error(
                                ErrorCode::ComptimeValueOutside,
                                format!(
                                    "`{}` names a type; type values exist only inside comptime \
                                     evaluation [G2]",
                                    only.name
                                ),
                                only.span,
                                "type value outside comptime",
                            );
                            return Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: false,
                            };
                        }
                        self.def_value(*def, only.span, expected)
                    },
                    Res::TraitMethod(_) | Res::Dot(_) | Res::Handler => Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    },
                    Res::Module(module) => {
                        // A module alias alone is not a value: only the
                        // qualified call behind it lowers.
                        let alias = self.package.module_ref(*module).alias().to_owned();
                        self.error(
                            ErrorCode::InferenceFailed,
                            format!(
                                "`{alias}` is a module alias; call its members through it, as \
                                 `{alias}.name(args)` [G4]",
                            ),
                            only.span,
                            "a module reference is callable only",
                        );
                        Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                }
            } else if only.name == "none" && !self.package.declares("none") {
                Val {
                    ty: self.none_ty(),
                    diverges: false,
                    poisoned: false,
                }
            } else {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` does not resolve to a value", only.name),
                    only.span,
                    "not a value",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            }
        } else if let [head, tail] = path.segments.as_slice() {
            // The qualified `type::label` use: a tag-family label
            // behind a type head resolves against the head's own members,
            // never the expected type — the escape hatch the bare label's
            // refusals point at.
            if let Some(Res::Def(def)) = self.res.get(tail.span)
                && matches!(self.package.def(*def).kind, DefKind::Tag | DefKind::Case)
            {
                return self.qualified_label_val(head, tail);
            }
            if self.res.get(head.span).is_some_and(|res| {
                matches!(res, Res::Def(def) if matches!(
                    self.package.def(*def).kind,
                    DefKind::Tag | DefKind::Case | DefKind::Enum | DefKind::TypeAlias
                ))
            }) {
                // the label did not resolve: resolve's unknown-name
                // refusal is the single report
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
            self.error(
                ErrorCode::InferenceFailed,
                "qualified paths are callable method references, not values [F3]".to_owned(),
                path.span,
                "method reference",
            );
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        } else {
            self.error(
                ErrorCode::InferenceFailed,
                "qualified paths are callable method references, not values [F3]".to_owned(),
                path.span,
                "method reference",
            );
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        }
    }

    /// The value type of a package-level definition used as a value.
    /// One expected union's members carrying a bare label. No
    /// probe runs without an expected type (`None` = undirected).
    pub(super) fn probe_label(
        &mut self,
        label: &str,
        own: Option<TypeId>,
        expected: Ty,
        span: Span,
    ) -> Option<DirectedLabel> {
        let labeled: Vec<TypeId> = self
            .decls
            .universe
            .members(expected)
            .iter()
            .copied()
            .filter(|&atom| {
                self.decls
                    .universe
                    .atom_decl(atom)
                    .is_ok_and(|decl| decl.name == label)
            })
            .collect();
        match labeled.as_slice() {
            [one] => {
                self.record_directed_tag(span, *one);
                Some(DirectedLabel::Directed(*one))
            },
            [] => {
                // the label names a declaration whose atom sits outside
                // the expected union: unknown-member, never an
                // inference
                if own.is_some() {
                    self.error(
                        ErrorCode::UnknownTagMember,
                        format!(
                            "a bare `{label}` names no member of `{}` [F9.5]",
                            self.decls.universe.render(expected)
                        ),
                        span,
                        "unknown member",
                    );
                    Some(DirectedLabel::Refused)
                } else {
                    None
                }
            },
            many => {
                let mut diag = Diagnostic::error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "a bare `{label}` matches several members of `{}` [F9.5]; qualify which \
                         declaration you mean",
                        self.decls.universe.render(expected)
                    ),
                )
                .with_primary(span, "ambiguous member");
                for atom in many {
                    if let Some(decl) = self.decls.defs_of_atoms.get(atom) {
                        diag = diag.with_secondary(
                            self.package.def(*decl).span,
                            "one declaration of the label",
                        );
                    }
                }
                self.diags.push(diag);
                Some(DirectedLabel::Refused)
            },
        }
    }

    /// A resolved tag-family def's value atom, type-directed when the
    /// position carries an expected type: the expected union's
    /// members carrying the label pick the atom — one member wins even
    /// over the use's own declaration, none refuses unknown-member,
    /// several refuse ambiguous naming both candidates. Without an
    /// expected type the declaration's own atom stands.
    /// The solved set of a qualified label's head: the head's
    /// own type, solved under the walking file's namespace so an alias
    /// expands under its declaring file.
    pub(super) fn head_type_of(&mut self, head: &Ident) -> Option<Ty> {
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
        self.solve_here(&head_ty)
    }

    /// The members of one solved set whose declaration carries `label`
    /// — the atoms a bare label's directed probe and a qualified
    /// label's head scan both read.
    pub(super) fn members_labeled(&self, set: Ty, label: &str) -> Vec<TypeId> {
        self.decls
            .universe
            .members(set)
            .iter()
            .copied()
            .filter(|&atom| {
                self.decls
                    .universe
                    .atom_decl(atom)
                    .is_ok_and(|decl| decl.name == label)
            })
            .collect()
    }

    /// The value of one qualified `type::label` use: the head's
    /// own members carrying the label pick the atom — one answers the
    /// member's type, none refuses unknown-member, several refuse
    /// ambiguous naming both declarations. The qualified spelling is
    /// never re-directed by an expected type; it is the escape hatch the
    /// bare label's refusals point at.
    pub(super) fn qualified_label_val(&mut self, head: &Ident, tail: &Ident) -> Val {
        let Some(head_ty) = self.head_type_of(head) else {
            self.error(
                ErrorCode::TypeMismatch,
                format!("the qualified label's head `{}` does not solve", head.name),
                head.span,
                "unsolvable head",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        };
        let labeled = self.members_labeled(head_ty, &tail.name);
        match labeled.as_slice() {
            [one] => {
                self.record_directed_tag(tail.span, *one);
                Val {
                    ty: self
                        .decls
                        .universe
                        .atom_ty(*one)
                        .unwrap_or_else(|_| self.empty()),
                    diverges: false,
                    poisoned: false,
                }
            },
            [] => {
                self.error(
                    ErrorCode::UnknownTagMember,
                    format!(
                        "the qualified `{}::{}` names no member of `{}` [F9.5]",
                        head.name,
                        tail.name,
                        self.decls.universe.render(head_ty)
                    ),
                    tail.span,
                    "unknown member",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                }
            },
            many => {
                let mut diag = Diagnostic::error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "the qualified `{}::{}` matches several members of `{}` [F9.5]; name a \
                         head that carries one",
                        head.name,
                        tail.name,
                        self.decls.universe.render(head_ty)
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
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                }
            },
        }
    }

    pub(super) fn directed_atom_val(
        &mut self,
        def: DefId,
        span: Span,
        expected: Option<Ty>,
    ) -> Val {
        let own = self.decls.atoms.get(&def).copied();
        let label = self.package.def(def).name.clone();
        let atom = match expected.and_then(|expected| self.probe_label(&label, own, expected, span))
        {
            Some(DirectedLabel::Directed(atom)) => Some(atom),
            Some(DirectedLabel::Refused) => None,
            None => own,
        };
        match atom {
            Some(atom) => Val {
                ty: self
                    .decls
                    .universe
                    .atom_ty(atom)
                    .unwrap_or_else(|_| self.empty()),
                diverges: false,
                poisoned: false,
            },
            // a refusal poisoned the span already; the empty type keeps
            // the downstream boundaries silent
            None => Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            },
        }
    }

    pub(super) fn def_value(&mut self, def: DefId, span: Span, expected: Option<Ty>) -> Val {
        match self.package.def(def).kind {
            DefKind::Fn => match self.decls.fns.get(&def).cloned() {
                Some(sig) if sig.generics.is_empty() => self.fn_atom_of(&sig),
                Some(_) => {
                    self.error(
                        ErrorCode::InferenceFailed,
                        "a generic function used as a value needs its type arguments [H3]"
                            .to_owned(),
                        span,
                        "unapplied generic function",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                },
                None => Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                },
            },
            DefKind::Tag => {
                // A payloaded tag constructs by call form only, exactly a
                // minted payload case; the bare name is the cross-spelling.
                if self.decls.structs.contains_key(&def) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "`{}` is a tag with a payload; construct it by call form: `{}(...)`",
                            self.package.def(def).name,
                            self.package.def(def).name
                        ),
                        span,
                        "tag with a payload used bare",
                    );
                }
                self.directed_atom_val(def, span, expected)
            },
            DefKind::Case => {
                // A bare case is the unit form, like a tag; a payload
                // case constructs by call form only, so a bare use
                // is the cross-spelling.
                let payload = self
                    .decls
                    .cases
                    .get(&def)
                    .map_or(EnumPayload::None, |c| c.payload.clone());
                if !matches!(payload, EnumPayload::None) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "`{}` is an enum case with a payload; construct it by call form: \
                             `{}(...)`",
                            self.package.def(def).name,
                            self.package.def(def).name
                        ),
                        span,
                        "payload case used bare",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                }
                self.directed_atom_val(def, span, expected)
            },
            DefKind::Const | DefKind::Var => self.const_ty(def, span),
            DefKind::Struct | DefKind::Enum | DefKind::TypeAlias | DefKind::Trait => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "`{}` is a type, not a value; construct structs with `name {{ ... }}`",
                        self.package.def(def).name
                    ),
                    span,
                    "type used as a value",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// The memoized type of a top-level binding, typed lazily so
    /// order-independent uses work. A linked const has no tree here:
    /// its literal fact is the whole type story — the fact's own mint in
    /// this universe (a scalar's atom, a list's applied constructor),
    /// memoized like a solved initializer.
    pub(super) fn const_ty(&mut self, def: DefId, span: Span) -> Val {
        if let Some(&ty) = self.def_types.get(&def) {
            return Val {
                ty,
                diverges: false,
                poisoned: false,
            };
        }
        if let Some(fact) = self.decls.foreign_consts.get(&def).cloned() {
            let ty = fact
                .ty(&mut self.decls.universe)
                .unwrap_or_else(|| self.empty());
            self.def_types.insert(def, ty);
            return Val {
                ty,
                diverges: false,
                poisoned: false,
            };
        }
        let Some(local) = self.decls.consts.get(&def).cloned() else {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "binding `{}` has no initializer to infer from",
                    self.package.def(def).name
                ),
                span,
                "no initializer",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        if self.visiting.contains(&def) {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "binding `{}` participates in a value cycle [G3]",
                    self.package.def(def).name
                ),
                span,
                "cycle",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        self.top_level_local(&local);
        let ty = self
            .def_types
            .get(&def)
            .copied()
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }
}
