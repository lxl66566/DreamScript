//! Paths: a path's value (a binder, a function value, a constant, or a
//! type atom), the `is` type test, and the solved member sets.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    pub(super) fn path_value(
        &mut self,
        path: &Path,
        span: Span,
        ty: Ty,
    ) -> Result<Place, LowerError> {
        if let [_head, tail] = path.segments.as_slice()
            && let Some(atom) = self.directed_tag(tail.span)
            && matches!(
                self.res.get(tail.span),
                Some(Res::Def(def))
                    if matches!(
                        self.analyzed.package.def(*def).kind,
                        ds_check::DefKind::Tag | ds_check::DefKind::Case
                    )
            )
        {
            // The qualified `type::label` use: the head's member
            // the checker picked is the value's atom, recorded at the
            // label's span.
            let dst = self.temp_word(SlotType::Tag)?;
            let tag = self.shapes.tag_id(atom)?;
            self.emit(Op::TagNew {
                dst: Slot::body(dst),
                tag,
            });
            return Ok(Place {
                base: dst,
                shape: PlaceShape::Word(SlotType::Tag),
            });
        }
        let [only] = path.segments.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "a multi-segment path expression",
                "qualified paths are not in the M3 contract",
            ));
        };
        match self.res.get(span) {
            Some(Res::Local { bind }) => {
                // the checker's use site: the RC death test compares
                // against the binder's last-use facts at exactly this span
                self.rc.use_of(*bind, only.span);
                match self.binders.get(bind).cloned() {
                    Some(place) => Ok(place),
                    // a block-local `fn`'s own name inside its own frame:
                    // no slot binds it, the use reads the fn's own word
                    None if self.analyzed.checked.self_fns.contains(bind) => self.self_fn_word(),
                    None => Err(self.missing_binder(span)),
                }
            },
            Some(Res::Def(def)) => {
                let kind = self.analyzed.package.def(*def).kind;
                match kind {
                    ds_check::DefKind::Tag | ds_check::DefKind::Case => {
                        // The directed probe may resolve a bare
                        // label to another declaration's atom than the one
                        // name resolution bound; the checker's record is
                        // the value's truth.
                        let atom = self
                            .analyzed
                            .checked
                            .directed_tags
                            .get(&only.span)
                            .copied()
                            .or_else(|| self.analyzed.decls.atoms.get(def).copied())
                            .ok_or_else(|| mismatch("a tag construction"))?;
                        let dst = self.temp_word(SlotType::Tag)?;
                        let tag = self.shapes.tag_id(atom)?;
                        self.emit(Op::TagNew {
                            dst: Slot::body(dst),
                            tag,
                        });
                        Ok(Place {
                            base: dst,
                            shape: PlaceShape::Word(SlotType::Tag),
                        })
                    },
                    _ => {
                        // A linked const's value fact: the literal is
                        // the whole fact, so the use embeds it exactly as
                        // the literal a user would have written — the same
                        // word shapes and RC treatment the owning module's
                        // own literal lowering gives it.
                        if let Some(fact) = self.analyzed.decls.foreign_consts.get(def).cloned() {
                            return self.embed_const_fact(fact, ty, span);
                        }
                        // A free generic fn holds no callable word: a call
                        // through it has no site to stage the body's
                        // evidence words, and the per-shape bodies answer
                        // only direct calls, so the value use refuses ahead
                        // of the table lookup — whether the plan holds a
                        // ONE body for it or not.
                        if self.plan.generic_fn(*def).is_some() {
                            return Err(LowerError::unsupported(
                                span,
                                "a generic function value",
                                "the word carries no instantiation, and a call through it has no \
                                 site to stage the body's evidence words; generic calls name \
                                 their callee directly",
                            ));
                        }
                        // A named fn used as a value (functions are
                        // values): the same pool FuncRef const channel a
                        // non-capturing lambda mints — one copyable scalar
                        // word, resolved through the two-slot module table
                        // at call time. A bodied `fn` declaration plans as
                        // a Local binding (DefKind::Const), so planned
                        // membership — not the def kind — identifies the
                        // function value. Natives run inside their CALL
                        // (never through a frame push) and generics stage
                        // evidence words per call site, so neither has a
                        // callable word.
                        if let Some(index) = self.plan.index_of(*def) {
                            // an evaluation site's planned body is the
                            // fenced run's, never a callable word: a
                            // value use keeps the top-level refusal the
                            // unplanned spelling always answered
                            if self.plan.const_defs.contains(def) {
                                return Err(LowerError::unsupported(
                                    span,
                                    "a top-level value",
                                    "only functions and unit tags are lowerable at top level",
                                ));
                            }
                            if self.plan.functions()[index as usize].free_generic {
                                return Err(LowerError::unsupported(
                                    span,
                                    "a generic function value",
                                    "the word carries no instantiation, and a call through it has \
                                     no site to stage the body's evidence words; generic calls \
                                     name their callee directly",
                                ));
                            }
                            let kidx = self.pool(Constant::FuncRef {
                                key: FuncRefKey::Dep(0), // the enclosing module
                                func: index,
                            });
                            let dst = self.temp_word(SlotType::Fn)?;
                            self.emit(Op::Kconst {
                                dst: Slot::body(dst),
                                kidx,
                            });
                            self.scalar_fn_words.insert(dst);
                            self.fn_words.insert(dst, index);
                            Ok(Place {
                                base: dst,
                                shape: PlaceShape::Word(SlotType::Fn),
                            })
                        } else if kind == ds_check::DefKind::Fn
                            || self
                                .def_type(*def)?
                                .is_some_and(|ty| fn_sig_of(self.universe(), ty).is_some())
                        {
                            // a bodied fn that did not plan (a `var`-bound
                            // generic fn) still carries a checked fn type,
                            // so the fn type — not the def kind — routes it
                            // here
                            Err(LowerError::unsupported(
                                span,
                                "a function value",
                                "only planned module functions mint callable words; native and \
                                 unplanned generic callees have no word",
                            ))
                        } else {
                            Err(LowerError::unsupported(
                                span,
                                "a top-level value",
                                "only functions and unit tags are lowerable at top level",
                            ))
                        }
                    },
                }
            },
            None if only.name == "none" => {
                // the predeclared unit atom: no package DefId exists
                let dst = self.temp_word(SlotType::Tag)?;
                let tag = self.shapes.tag_id(TypeId::NONE)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(dst),
                    tag,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Tag),
                })
            },
            _ => Err(LowerError::unsupported(
                span,
                "a name use",
                "it resolved to a method or handler name the lowerer cannot address",
            )),
        }
        .inspect(|_place| {
            let _ = ty;
        })
    }

    /// `x is T`: TAG_TESTs against the tested atoms, or a constant when
    /// the checked types already decide it (legal specialization). A
    /// multi-atom test (a written union spelling) joins one TAG_TEST
    /// per atom by jumps — the first hit answers true, the fall-through
    /// answers false.
    pub(super) fn is_value(
        &mut self,
        expr: &Expr,
        ty: &Type,
        span: Span,
    ) -> Result<Place, LowerError> {
        let operand = self.value(expr)?;
        let operand_ty = self.val(expr.span())?.ty;
        // The checker's solved test is the site's truth: a
        // type-position label resolves per file, so the first namespace
        // holder the nominal spelling finds may be another declaration's
        // atom); the nominal path spelling covers only the shapes the
        // checker recorded no solve for.
        let nominal;
        let tested: &[TypeId] = if let Some(members) = self.solved_is_members(span)? {
            members
        } else {
            let atom = self.type_atom(ty)?;
            nominal = vec![atom];
            &nominal
        };
        let members = self.members(operand_ty);
        // the operand's member set decides the test: its whole coverage
        // folds to true, a disjoint test to false, and the rest tests the
        // members the two sets share
        let live: Vec<TypeId> = members
            .iter()
            .copied()
            .filter(|m| tested.contains(m))
            .collect();
        let dst = self.temp_word(SlotType::Bool)?;
        // the covered fold is instantiation-stable even over type
        // variables (the variable's atom covers whatever it becomes);
        // every other shape reads a tag the variable's raw word cannot
        // answer — or folds against a member set an instantiation may
        // re-solve — so a type variable on either side of the test
        // refuses here
        if !members.is_empty() && live.len() == members.len() {
            let kidx = self.pool(Constant::Bool(true));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx,
            });
        } else if tested
            .iter()
            .chain(members.iter())
            .any(|a| self.shapes.is_var_atom(*a))
        {
            return Err(LowerError::unsupported(
                span,
                "an `is` test over a type variable",
                "a type-variable value rides one raw word, so no tag discriminates it and no \
                 member set survives instantiation; testing it needs runtime reflection",
            ));
        } else if live.is_empty() {
            let kidx = self.pool(Constant::Bool(false));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx,
            });
        } else if let [atom] = live.as_slice() {
            let src = self.tag_addr(&operand.place)?;
            let tag = self.shapes.tag_id(*atom)?;
            self.emit(Op::TagTest {
                dst: Slot::body(dst),
                src: Slot::body(src),
                tag,
            });
        } else {
            let src = self.tag_addr(&operand.place)?;
            let hit = self.label();
            let join = self.label();
            for atom in &live {
                let probe = self.temp_word(SlotType::Bool)?;
                let tag = self.shapes.tag_id(*atom)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(probe),
                    src: Slot::body(src),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(probe),
                    target: hit,
                });
            }
            let k_false = self.pool(Constant::Bool(false));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx: k_false,
            });
            self.emit(Op::Jmp { target: join });
            self.bind_label(hit);
            let k_true = self.pool(Constant::Bool(true));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx: k_true,
            });
            self.bind_label(join);
        }
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// The tested members of one `is` test, from the checker's solved
    /// record keyed by the expression's span, read through the lens: the
    /// solution's members are what the TAG_TESTs discriminate by, the
    /// authority the nominal path spelling only backs up (a written type
    /// the checker recorded no solve for).
    pub(super) fn solved_is_members(&self, span: Span) -> Result<Option<&'a [TypeId]>, LowerError> {
        let Some(solved) = self.is_test(span)? else {
            return Ok(None);
        };
        Ok(Some(self.members(solved)))
    }

    /// The atom a written type names; nominal single-segment paths only.
    pub(super) fn type_atom(&self, ty: &Type) -> Result<TypeId, LowerError> {
        let Type::Path { path, args, .. } = ty else {
            return Err(LowerError::unsupported(
                ty.span(),
                "a written type",
                "only nominal single-segment type names resolve here",
            ));
        };
        if !args.is_empty() || path.segments.len() != 1 {
            return Err(LowerError::unsupported(
                ty.span(),
                "a written type",
                "only nominal single-segment type names resolve here",
            ));
        }
        self.path_atom(&path.segments[0].name)
            .ok_or_else(|| mismatch("a type atom"))
    }

    pub(super) fn path_atom(&self, name: &str) -> Option<TypeId> {
        // `none` is the universe's predeclared unit atom, never a
        // package declaration
        if name == "none" {
            return Some(TypeId::NONE);
        }
        let def = self
            .analyzed
            .package
            .lookup(name)
            .or_else(|| self.analyzed.decls.foreign_type(name))?;
        self.analyzed.decls.atoms.get(&def).copied()
    }
}

pub(super) fn single_path_ident(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Path { path, .. } if path.segments.len() == 1 => path.segments.first(),
        _ => None,
    }
}
