//! Call results: the native-result shapers that build a callee's answer
//! span (fallible overlays, classed unions, keyed answers, the `ok`/`err`
//! arms) and the callee plan queries they read.

use super::*;

impl<'a> FnLower<'a, '_, '_> {
    /// A fallible or checked native's result span: neither platform span
    /// carries a source-level return type, so a tail call rejects, and a
    /// value-position call re-spans through its shape's dedicated path.
    pub(in crate::func) fn native_span_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        tail: bool,
        callee_span: Span,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if tail {
            let (what, reason) = if matches!(
                native.shape,
                NativeShape::Fallible { .. } | NativeShape::FallibleByResultType { .. }
            ) {
                (
                    "a tail call to a fallible native",
                    "CALLT-to-fallible stays unsupported: the two-word platform span re-spans \
                     into the call's union at the call site, and the tail transfer leaves no \
                     position for it; keep the call out of tail position",
                )
            } else {
                (
                    "a tail call to a checked-read native",
                    "the ok/none span has no source-level return type; keep the call out of tail \
                     position",
                )
            };
            return Err(LowerError::unsupported(callee_span, what, reason));
        }
        if let NativeShape::Fallible {
            payload,
            fail_payload,
            family,
        } = native.shape
        {
            let spelling = crate::native::failure_spelling(family);
            return self.fallible_native_result(
                native,
                ty,
                payload,
                fail_payload,
                spelling.err_prefix,
                spelling.classes,
                staged,
            );
        }
        if let NativeShape::FallibleByResultType {
            fail_payload,
            family,
        } = native.shape
        {
            let spelling = crate::native::failure_spelling(family);
            return self.keyed_native_result(native, ty, fail_payload, spelling.classes, staged);
        }
        self.checked_native_result(Callee::Builtin(native), ty, staged, staged.len())
    }

    /// The one-word instantiation boundary of the runtime-evidence seam:
    /// the one generic body lowers a type variable's value to a single
    /// Raw word — the stride-1 element iteration, the binder, and every
    /// nested dispatch's staging carry that one word — while a serving
    /// body claims the substituted type's whole span as its receiver,
    /// so a multi-word substitution must refuse at the entering site
    /// (where the type is concrete), never slide at runtime. The
    /// whole-span vocabulary carries width-static spans alone (concrete
    /// call sites, signature-governed answers); the variable's width
    /// never reaches the frame, so no rider shape can carry it. Reboxing
    /// stays rejected, and per-instantiation frames are the JIT
    /// specialization tier's, never the interpreter's. A shape with no
    /// lowering answer refuses at its own staging, so an error here
    /// stays an error there.
    pub(in crate::func) fn guard_one_word_instantiation(
        &mut self,
        members: &[TypeId],
        what: &'static str,
        span: Span,
    ) -> Result<(), LowerError> {
        let wide = matches!(self.shapes.member_words(members), Ok(ref words) if words.len() > 1);
        if wide {
            return Err(LowerError::unsupported(
                span,
                what,
                "the substituted type spans more than one word, while the one generic body lowers \
                 the variable's value to a single Raw word (the stride-1 element read, the \
                 binder, the nested dispatch's staging each carry that one word); reboxing stays \
                 rejected, and per-instantiation frames are the JIT specialization tier's, never \
                 the interpreter's",
            ));
        }
        Ok(())
    }

    /// A local callee's parameter word count, as its own frame layout
    /// records it (params first, one region per parameter). `None` when
    /// a parameter type has no shape; those callees keep the runtime
    /// arity check.
    /// A declared impl-member body's parameter words, from the checked
    /// binder facts keyed by each parameter's name span — the same table
    /// the body's own binding reads, so call sites and body agree.
    pub(in crate::func) fn decl_param_words(&mut self, index: u32) -> Option<Vec<SlotType>> {
        let planned = self.plan.functions().get(index as usize)?;
        // An instance's parameter words are its solved types' spans —
        // the shapes the entering site stages against, never the body's
        // variable-keyed binder facts.
        if let Some(instance) = &planned.instance {
            let mut words = Vec::with_capacity(instance.params.len());
            for ty in instance.params.iter() {
                words.extend_from_slice(&self.shapes.words(*ty).ok()?);
            }
            return Some(words);
        }
        let decl = planned.decl.clone()?;
        let mut words = Vec::with_capacity(decl.params.len());
        if planned.free_generic {
            // a ONE Raw-word body's parameters keep the caller's runtime
            // fact: a parameter over the body's own variable is the
            // wildcard word, and the kinds read from the written facts —
            // never the caller's lens, which would substitute the
            // variables away and leave the site staging its concrete
            // words against the Raw formals the slide refuses
            for param in decl.params.iter() {
                let fact = self
                    .analyzed
                    .checked
                    .binders
                    .get(&param.name.span)
                    .copied()?;
                let [only] = self.universe().members(fact.ty) else {
                    words.extend_from_slice(&self.shapes.words(fact.ty).ok()?);
                    continue;
                };
                if self.shapes.is_var_atom(*only) {
                    words.push(SlotType::Raw);
                } else {
                    words.extend_from_slice(&self.shapes.words(fact.ty).ok()?);
                }
            }
            return Some(words);
        }
        for param in decl.params.iter() {
            // the belt cannot fire on this path — a declared body other
            // than an instance lowers over no substitution — so the
            // lens failure folds into the `None` this reader's callers
            // already treat as "no static words"
            let fact = self.binder_fact(param.name.span).ok()??;
            words.extend_from_slice(&self.shapes.words(fact.ty).ok()?);
        }
        Some(words)
    }

    /// A local callee's per-parameter checked types, when its signature
    /// is static: a declared body's parameters come from the checked
    /// binder facts keyed by each parameter's name span, a def's from
    /// its recorded signature. `None` keeps the runtime arity check.
    pub(in crate::func) fn callee_param_types(&mut self, index: u32) -> Option<Vec<Ty>> {
        let planned = self.plan.functions().get(index as usize)?;
        // An instance's parameter types are its solved ones — concrete
        // handles the call site re-spans its arguments against.
        if let Some(instance) = &planned.instance {
            return Some(instance.params.to_vec());
        }
        // A ONE Raw-word body's parameter types read as written, never
        // through the caller's lens: the body lowers over the written
        // variables' Raw words, so a variable parameter keeps its
        // variable type (the site's one-word boundary and Raw staging
        // own it) while a wider written parameter — a union head —
        // re-spans the member argument into its whole region
        if planned.free_generic {
            return planned
                .decl
                .as_ref()?
                .params
                .iter()
                .map(|param| {
                    self.analyzed
                        .checked
                        .binders
                        .get(&param.name.span)
                        .copied()
                        .map(|fact| fact.ty)
                })
                .collect();
        }
        if let Some(decl) = &planned.decl {
            return decl
                .params
                .iter()
                .map(|param| {
                    self.binder_fact(param.name.span)
                        .ok()
                        .flatten()
                        .map(|fact| fact.ty)
                })
                .collect();
        }
        let def = planned.def?;
        let fn_ty = self.def_type(def).ok()??;
        let (params, _) = fn_sig_of(self.universe(), fn_ty)?;
        Some(params.to_vec())
    }

    /// The declared parameter modes of one planned function (a declared
    /// body's or a lambda's own parameters, a def's recorded signature):
    /// a consuming (`move`) parameter's staging forwards its argument's
    /// count and any deinit claim — the checker's forced move makes
    /// the death test true at exactly that use. `None` when the body's
    /// parameters are not statically readable here.
    pub(in crate::func) fn planned_param_modes(
        &self,
        index: u32,
    ) -> Option<Vec<ds_parse::ParamMode>> {
        let planned = self.plan.functions().get(index as usize)?;
        if let Some(decl) = &planned.decl {
            return Some(decl.params.iter().map(|param| param.mode).collect());
        }
        if let Some(def) = planned.def {
            let sig = self.analyzed.decls.fns.get(&def)?;
            return Some(sig.params.iter().map(|param| param.mode).collect());
        }
        if let Expr::Lambda { params, .. } = &planned.lambda {
            return Some(params.iter().map(|param| param.mode).collect());
        }
        None
    }

    /// Whether the parameter at one written-argument position consumes
    /// its argument (`move` mode): the callee is a planned body (a
    /// local callee or a trait dispatch's impl body) or a runtime
    /// dispatch through the trait member's declared signature (impl
    /// bodies agree with the declaration). Natives carry no modes, and a
    /// position with no readable mode borrows.
    pub(in crate::func) fn consuming_args(
        &self,
        target: Callee,
        dispatch: Option<&TraitDispatch>,
        args: usize,
    ) -> Vec<bool> {
        let modes: Option<Vec<ds_parse::ParamMode>> = match target {
            Callee::Local(index) => self.planned_param_modes(index),
            Callee::Foreign { def: Some(def), .. } => self
                .analyzed
                .decls
                .fns
                .get(&def)
                .map(|sig| sig.params.iter().map(|param| param.mode).collect()),
            Callee::Foreign { def: None, .. } | Callee::Evidence(_) => match dispatch {
                Some(TraitDispatch::Runtime { method, .. }) => self
                    .analyzed
                    .decls
                    .members
                    .get(method)
                    .map(|sig| sig.params.iter().map(|param| param.mode).collect()),
                Some(TraitDispatch::Foreign {
                    call: TraitCall { method, .. },
                    ..
                }) => self
                    .analyzed
                    .decls
                    .members
                    .get(method)
                    .map(|sig| sig.params.iter().map(|param| param.mode).collect()),
                Some(TraitDispatch::InherentForeign { method, .. }) => self
                    .analyzed
                    .decls
                    .inherent
                    .get(method)
                    .map(|sig| sig.params.iter().map(|param| param.mode).collect()),
                _ => None,
            },
            Callee::Builtin(_) | Callee::Generic(_) | Callee::Bound { .. } => None,
        };
        match modes {
            Some(modes) => (0..args)
                .map(|index| matches!(modes.get(index), Some(ds_parse::ParamMode::Consume(_))))
                .collect(),
            None => vec![false; args],
        }
    }

    pub(in crate::func) fn callee_param_words(&mut self, index: u32) -> Option<usize> {
        let planned = self.plan.functions().get(index as usize)?;
        if planned.decl.is_some() {
            return self.decl_param_words(index).map(|words| words.len());
        }
        let def = planned.def?;
        let fn_ty = self.def_type(def).ok()??;
        let (params, _) = fn_sig_of(self.universe(), fn_ty)?;
        let mut total = 0;
        for ty in params {
            total += self.shapes.words(*ty).ok()?.len();
        }
        Some(total)
    }

    /// Whether one planned function's tail expression is a capturing
    /// lambda — a factory: its fn-typed result is the closure word the
    /// tail constructs, so a caller of it classifies the result slot Ref.
    pub(in crate::func) fn tail_builds_a_closure(&self, index: u32) -> bool {
        let Expr::Lambda { body, .. } = &self.plan.functions()[index as usize].lambda else {
            return false;
        };
        let mut tail = &**body;
        loop {
            match tail {
                Expr::Paren { inner, .. } => tail = inner,
                // a declared fn's body is a block; only the lone
                // expression tail keeps the provenance pure
                Expr::Block(block) => match block.stmts.split_last() {
                    Some((
                        Stmt {
                            kind: StmtKind::Expr(e),
                            ..
                        },
                        [],
                    )) => tail = e,
                    _ => return false,
                },
                _ => break,
            }
        }
        let Expr::Lambda { .. } = tail else {
            return false;
        };
        let res = &self.analyzed.resolutions[tail.span().file.as_u32() as usize];
        !collect_captures(tail, res, &self.analyzed.checked.binders).is_empty()
    }

    /// The result word types a callee's return lands on the overlay as.
    /// A generic's instance-keyed result reads its word kind from the
    /// minted receiver instance (the gate already proved it is one the
    /// body implements), and a result-type-keyed one reads it from the
    /// call's own checked result type (`ty` — the folded element type
    /// `reduce` answers).
    pub(in crate::func) fn callee_result_words(
        &mut self,
        target: Callee,
        instance: Option<BuiltinInstance>,
        ty: Ty,
    ) -> Result<SpanWords, LowerError> {
        match target {
            Callee::Builtin(native) => match native.shape {
                NativeShape::Total(slot) => Ok(Rc::from([slot])),
                // call_impl routes fallible and checked natives through
                // their own tag-test paths; this generic copy never sees
                // them
                NativeShape::Fallible { .. } => Err(mismatch("a fallible native result")),
                NativeShape::Checked => Err(mismatch("a checked-read native result")),
                NativeShape::TotalByInstance => Err(mismatch("an instance-keyed native result")),
                // the call's own checked result type's words (`chan_recv`
                // answering the payload type's one word)
                NativeShape::TotalByResultType => self.shapes.words(ty),
                // call_impl routes the keyed fallible span through its
                // own re-span path; this generic copy never sees it
                NativeShape::FallibleByResultType { .. } => {
                    Err(mismatch("a fallible native result"))
                },
                NativeShape::TaglessTotal => Err(mismatch("a tag-less-total native result")),
            },
            Callee::Generic(generic) => match generic.shape {
                NativeShape::Total(slot) => Ok(Rc::from([slot])),
                NativeShape::TotalByInstance => match instance {
                    Some(BuiltinInstance::Int) => Ok(Rc::from([SlotType::I64])),
                    Some(BuiltinInstance::Float) => Ok(Rc::from([SlotType::F64])),
                    _ => Err(mismatch("an instance-keyed generic result")),
                },
                // the tag-less total answers the call's own result
                // type's words with no discriminant (partition's
                // two-list tuple: two Ref words the destructuring binds)
                NativeShape::TotalByResultType | NativeShape::TaglessTotal => self.shapes.words(ty),
                NativeShape::Fallible { .. }
                | NativeShape::Checked
                | NativeShape::FallibleByResultType { .. } => {
                    Err(mismatch("a generic builtin result"))
                },
            },
            Callee::Evidence(_) | Callee::Foreign { .. } | Callee::Bound { .. } => {
                self.shapes.words(ty)
            },
            Callee::Local(index) => {
                let planned = &self.plan.functions()[index as usize];
                if planned.free_generic {
                    // the ONE body returns the type variable's single
                    // Raw word, so the site's solved answer must span
                    // one word too when the declared return is the
                    // variable — the same boundary the arguments guard
                    // (a trait's declared default body carries its solved
                    // return on the plan entry; no def signature names it)
                    let words = self.shapes.words(ty)?;
                    let declared = match planned.def {
                        Some(def) => self
                            .def_type(def)?
                            .and_then(|fn_ty| fn_sig_of(self.universe(), fn_ty))
                            .map(|(_, ret)| ret),
                        None => None,
                    }
                    .or(planned.ret);
                    if declared.is_some_and(|ret| self.is_var_ty(ret)) {
                        self.guard_one_word_instantiation(
                            self.members(ty),
                            "a generic call's result",
                            no_span(), // re-pointed at the call site
                        )?;
                    }
                    return Ok(words);
                }
                if let Some(ret) = planned.ret {
                    let words = self.shapes.words(ret)?;
                    if *words == [SlotType::Fn] && self.tail_builds_a_closure(index) {
                        return Ok(Rc::from([SlotType::Ref]));
                    }
                    return Ok(words);
                }
                let fn_ty = match planned.def {
                    Some(def) => self.def_type(def)?,
                    None => self.val(planned.lambda.span()).ok().map(|val| val.ty),
                }
                .ok_or_else(|| mismatch("a callee signature"))?;
                let Some((_, ret)) = fn_sig_of(self.universe(), fn_ty) else {
                    return Err(mismatch("a callee signature"));
                };
                let words = self.shapes.words(ret)?;
                // A factory's fn-typed result is the closure its tail
                // lambda constructs — the one provenance this frame can
                // prove from the callee alone. Every other fn-typed
                // result keeps the scalar classification (a closure
                // returned through such a callee traps at the call — the
                // recorded return-flow seam).
                if *words == [SlotType::Fn] && self.tail_builds_a_closure(index) {
                    return Ok(Rc::from([SlotType::Ref]));
                }
                Ok(words)
            },
        }
    }

    /// One fallible native call: the two-word platform span (discriminant
    /// at overlay word 0, payload at word 1) lands like any call result.
    /// The success-typed union spelling types the call site as the
    /// failure union itself (`string | io_err`, `int | parse_err`), so
    /// the span re-spans into it per member over the error-class mapping
    /// ([`fn classed_native_union`](Self::classed_native_union)); a total
    /// spelling (a deinit-carrying handle row such as `open_file`, or a
    /// file-owned extern declaring the success type alone) extracts the
    /// payload into the call's result temp, and failure prints
    /// `<prefix><detail>` through println and halts on the discriminant
    /// — the stand-in for the unhandled-failure handler the `error`
    /// row has no source syntax for.
    pub(in crate::func) fn fallible_native_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        fail_payload: SlotType,
        err_prefix: &'static str,
        classes: &'static [(u8, &'static str)],
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if matches!(&*self.shapes.shape(ty)?, Shape::Union(_)) {
            let shape = self.shapes.shape(ty)?;
            let Shape::Union(u) = &*shape else {
                unreachable!("the union shape matched above")
            };
            let class_members = self.class_members_of(u, fail_payload, classes)?;
            let success = self.static_success_plan(u, &class_members, payload)?;
            return self.classed_native_union(
                native,
                ty,
                &[payload],
                success,
                fail_payload,
                classes,
                staged,
            );
        }
        self.fallible_total_result(native, ty, payload, fail_payload, err_prefix, staged)
    }

    /// A statically-payloaded fallible native's success plan: exactly
    /// one member outside the class set carrying the native's one
    /// payload word, or the unit value over the native's inert unit
    /// word (`net_send`'s `()`).
    pub(in crate::func) fn static_success_plan(
        &self,
        u: &UnionShape,
        class_members: &[(u8, MemberShape)],
        payload: SlotType,
    ) -> Result<SuccessShape, LowerError> {
        let is_class = |atom: TypeId| {
            class_members
                .iter()
                .any(|(_, member): &(u8, MemberShape)| member.atom == atom)
        };
        let mut success: Vec<&MemberShape> = u
            .members
            .iter()
            .filter(|member| !is_class(member.atom))
            .filter(|member| member.fields.len() == 1 && member.fields[0].slot() == payload)
            .collect();
        if success.is_empty() && payload == SlotType::I64 {
            success = u
                .members
                .iter()
                .filter(|member| !is_class(member.atom))
                .filter(|member| self.is_unit_member(member.atom))
                .collect();
        }
        let [ok] = success.as_slice() else {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union's success member must be exactly the native's one payload word, or the \
                 unit value over the native's inert unit word",
            ));
        };
        Ok(match ok.fields.as_slice() {
            [field] => SuccessShape::Payload {
                atom: ok.atom,
                fields: vec![field.clone()],
            },
            _ => SuccessShape::Bare { atom: ok.atom },
        })
    }

    /// The total spelling of a fallible native's span: extracts the
    /// payload into the call's result temp, and failure prints
    /// `<prefix><detail>` through println and halts on the discriminant.
    /// The failure detail is the span's word 1: the message `Ref` of an
    /// io/net class, or the parse reader's position `I64`, rendered
    /// through the `str_int` native before the concatenation.
    pub(in crate::func) fn fallible_total_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        fail_payload: SlotType,
        err_prefix: &'static str,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if *self.shapes.words(ty)? != [payload] {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the checked result type's words and the native's payload disagree; list values \
                 have no runtime shape yet",
            ));
        }
        self.overlay_fallible(staged, payload, fail_payload);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        let ok_lbl = self.label();
        let fail_lbl = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        self.bind_label(fail_lbl);
        let discriminant = self.temp_word(SlotType::Tag)?;
        self.emit(Op::Mov {
            dst: Slot::body(discriminant),
            src: Slot::overlay(0),
        });
        let prefix = self.temp_word(SlotType::Ref)?;
        let kidx = self.pool_str(err_prefix);
        self.kstr(prefix, kidx);
        let message = self.temp_word(SlotType::Ref)?;
        if fail_payload == SlotType::Ref {
            self.emit(Op::Mov {
                dst: Slot::body(message),
                src: Slot::overlay(1),
            });
            self.rc.produced(message);
        } else {
            // the parse reader's failure word is the position `I64`:
            // render it through the str_int native before concatenating
            self.emit(Op::Mov {
                dst: Slot::overlay(0),
                src: Slot::overlay(1),
            });
            let render = crate::native::lookup("str_int").expect("the int renderer is covered");
            let kidx = self.pool(self.funcref_const(Callee::Builtin(render)));
            self.rc.native_open();
            self.emit(Op::Call {
                callee: kidx,
                nargs: 1,
            });
            self.rc.native_done(&mut self.code);
            self.emit(Op::Mov {
                dst: Slot::body(message),
                src: Slot::overlay(0),
            });
            self.rc.produced(message);
        }
        let text = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Data {
            op: DataOp::StrCat,
            dst: Slot::body(text),
            lhs: Slot::body(prefix),
            rhs: Slot::body(message),
        });
        self.rc.read(&mut self.code, &[prefix, message]);
        self.rc.produced_strcat(text);
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(text),
        });
        self.rc.native_open();
        self.rc.staged_native(text, false);
        let println = crate::native::lookup("println").expect("the console native is covered");
        let kidx = self.pool(self.funcref_const(Callee::Builtin(println)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 1,
        });
        self.rc.native_done(&mut self.code);
        // program end: HALT keeps its references — the heap dies whole
        self.emit(Op::Halt {
            slot: Slot::body(discriminant),
        });
        self.bind_label(ok_lbl);
        let base = self.alloc(&[payload])?;
        self.emit(Op::Mov {
            dst: Slot::body(base),
            src: Slot::overlay(1),
        });
        if payload == SlotType::Ref {
            self.rc.produced(base);
        }
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    /// The unit-value member: the arity-0 tuple atom — the `()` of
    /// a classed fallible native's unit-word success member, distinct
    /// from the payload-less class members (the unit tags).
    pub(in crate::func) fn is_unit_member(&self, atom: TypeId) -> bool {
        matches!(
            self.universe().atom_decl(atom),
            Ok(decl) if matches!(&decl.kind, AtomKind::Tuple(elements) if elements.is_empty())
        )
    }

    /// One fallible native call over the success-typed union spelling:
    /// the checked call type is the failure union itself (`string |
    /// io_err`, `int | parse_err`), so the two-word platform span
    /// re-spans into it — success constructs the union's success value
    /// per [`SuccessShape`], failure the class member the discriminant's
    /// error-class mapping names (each discriminant paired with the
    /// ambient failure-taxonomy member it lands on; a bare member adopts
    /// no word, a one-field member adopts word 1 as its payload — the
    /// `io_other`/`net_other` message `Ref`, the `parse_err` position
    /// `I64`). The union must carry every class member plus exactly one
    /// success member holding the native's one payload word (the unit
    /// value over the native's inert unit word, `net_send`'s `()`) —
    /// except a result-type-keyed native, whose success members are the
    /// union's own non-class members (the timer race answering a
    /// tag-only union over its deadline member). The payload rides the
    /// span channel's shape: one word per success word after the
    /// discriminant (`slots 1 ..= n`), a statically-payloaded native's
    /// single word or a wider answer's whole word span. The same
    /// span-to-union discipline the checked reads use
    /// ([`fn checked_native_result`](Self::checked_native_result)). A
    /// bare class member's native-born message count dies inside its
    /// failure arm; a scalar failure word is inert.
    pub(in crate::func) fn classed_native_union(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload_words: &[SlotType],
        success: SuccessShape,
        fail_payload: SlotType,
        classes: &'static [(u8, &'static str)],
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a fallible native's union result"));
        };
        // the class members the mapping names, each bare or carrying
        // exactly the failure payload's one word
        let class_members = self.class_members_of(u, fail_payload, classes)?;
        let success_len = match success {
            SuccessShape::Payload { .. } | SuccessShape::Bare { .. } => 1,
            SuccessShape::Adopt => {
                // a tag-only union answer: every non-class member is a
                // success member by construction
                u.members.len() - class_members.len()
            },
        };
        if success_len + class_members.len() != u.members.len() {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union must be exactly the success member plus the failure taxonomy's class \
                 members; a wider union has no member mapping for the native's failure classes",
            ));
        }
        let mut result_words = Vec::with_capacity(1 + payload_words.len());
        result_words.push(SlotType::Tag);
        result_words.extend_from_slice(payload_words);
        self.guard_staged_count(staged.len().max(result_words.len()))?;
        self.overlay_site(staged, &result_words);
        // the failure path lands its class payload in word 1 — a Ref
        // message for the io/net classes, a position I64 for the parse
        // reader — so the word's classification mixes
        self.overlay_len = self.overlay_len.max(2);
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        self.overlay[1].insert(fail_payload);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        let result = self.temp_span(ty)?;
        let ok_lbl = self.label();
        let join = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork dispatch: every class but the last jumps to its own
        // arm, the last class is the fall-through failure
        let total = class_members.len();
        let mut class_lbls = Vec::with_capacity(total.saturating_sub(1));
        for (tag, _) in &class_members[..total.saturating_sub(1)] {
            let cond = self.temp_word(SlotType::Bool)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::overlay(0),
                tag: *tag,
            });
            let lbl = self.label();
            class_lbls.push(lbl);
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: lbl,
            });
        }
        // the failure arms in code order: the fall-through last class
        // first, then every earlier class at its dispatch label
        let base = self.rc.save();
        let mut arm_jmps: Vec<usize> = Vec::with_capacity(total);
        let mut arm_states: Vec<State> = Vec::with_capacity(total);
        for (index, (_, member)) in class_members.iter().enumerate().rev() {
            if index != total - 1 {
                self.bind_label(class_lbls[index]);
            }
            let tag = self.shapes.tag_id(member.atom)?;
            self.emit(Op::TagNew {
                dst: Slot::body(result.base),
                tag,
            });
            self.zero_init_member(result.base, &u.words, &member.fields)?;
            match member.fields.as_slice() {
                // the class's own payload word: the message Ref or the
                // position I64, adopted off the overlay into the field
                [field] => {
                    self.emit(Op::Mov {
                        dst: Slot::body(result.base + field.word),
                        src: Slot::overlay(1),
                    });
                    if field.slot() == SlotType::Ref {
                        self.rc.produced(result.base + field.word);
                    }
                },
                // a bare member: the native-born message count has no
                // member field to live in, so it dies inside the arm
                _ => {
                    if fail_payload == SlotType::Ref {
                        let message = self.temp_word(SlotType::Ref)?;
                        self.emit(Op::Mov {
                            dst: Slot::body(message),
                            src: Slot::overlay(1),
                        });
                        self.rc.produced(message);
                        self.rc.discard(&mut self.code, &[message]);
                    }
                },
            }
            arm_jmps.push(self.code.len());
            arm_states.push(self.rc.take());
            self.emit(Op::Jmp { target: join });
            self.rc.restore(base.clone());
        }
        self.bind_label(ok_lbl);
        match success {
            SuccessShape::Payload { atom, fields } => {
                let tag = self.shapes.tag_id(atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(result.base),
                    tag,
                });
                self.zero_init_member(result.base, &u.words, &fields)?;
                // the payload copies from the flat overlay span into the
                // member's claimed words (fields may claim scattered
                // region offsets; the flat index walks the member's
                // declaration order)
                let mut flat = 0usize;
                for field in &fields {
                    for (offset, slot) in field.slots.iter().enumerate() {
                        let word = result.base
                            + field.word
                            + u8::try_from(offset).expect("offset fits u8");
                        self.emit(Op::Mov {
                            dst: Slot::body(word),
                            src: Slot::overlay(
                                u8::try_from(1 + flat + offset).expect("overlay word fits u8"),
                            ),
                        });
                        if *slot == SlotType::Ref {
                            self.rc.produced(word);
                        }
                    }
                    flat += field.slots.len();
                }
            },
            SuccessShape::Bare { atom } => {
                // a bare member (a unit value over the native's inert
                // unit word, or a bare tag): the span's payload word
                // stays unread
                let tag = self.shapes.tag_id(atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(result.base),
                    tag,
                });
                self.zero_init_member(result.base, &u.words, &[])?;
            },
            SuccessShape::Adopt => {
                // a multi-atom answer: the racer's answer span is
                // already the union's own value — its tag names the
                // member, its region the member's words — so every word
                // copies verbatim with no re-span
                for (index, slot) in u.words.iter().enumerate() {
                    let word = result.base + u8::try_from(index).expect("span width fits u8");
                    self.emit(Op::Mov {
                        dst: Slot::body(word),
                        src: Slot::overlay(u8::try_from(1 + index).expect("overlay word fits u8")),
                    });
                    if *slot == SlotType::Ref {
                        self.rc.produced(word);
                    }
                }
            },
        }
        let arm_ok = self.rc.take();
        let mut arms: Vec<(State, bool)> =
            arm_states.into_iter().map(|state| (state, false)).collect();
        arms.push((arm_ok, false));
        let dying = self.rc.reconcile(&arms);
        // splice each failure arm's dying drops before its jump to the
        // join, compensating the shifts the earlier splices introduced
        let mut shift = 0;
        for (jmp, drops) in arm_jmps.iter().zip(&dying) {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, drops);
            let len = epilogue.len();
            self.splice(epilogue, jmp + shift);
            shift += len;
        }
        self.arm_epilogue(dying.last().expect("the ok arm's dying set"));
        self.bind_label(join);
        Ok(result)
    }

    /// The class members the error-class mapping names over the call's
    /// own union: each class member present by its ambient name, bare or
    /// carrying exactly the failure payload's one word.
    pub(in crate::func) fn class_members_of(
        &self,
        u: &UnionShape,
        fail_payload: SlotType,
        classes: &'static [(u8, &'static str)],
    ) -> Result<Vec<(u8, MemberShape)>, LowerError> {
        let mut class_members: Vec<(u8, MemberShape)> = Vec::new();
        for (tag, name) in classes {
            let Some(atom) = self.analyzed.decls.universe.atom_by_name(name) else {
                let _ = name;
                return Err(LowerError::unsupported(
                    no_span(),
                    "a fallible native call",
                    "the union must carry the failure taxonomy's members by their ambient names; \
                     a differently named error set has no class mapping",
                ));
            };
            let Some(member) = u.member(atom) else {
                return Err(LowerError::unsupported(
                    no_span(),
                    "a fallible native call",
                    "the union must carry the failure taxonomy's whole member set; every class \
                     member the native answers must be a member",
                ));
            };
            match member.fields.as_slice() {
                [] => {},
                [field] if field.slot() == fail_payload => {},
                _ => {
                    return Err(LowerError::unsupported(
                        no_span(),
                        "a fallible native call",
                        "every class member is a bare tag, or carries exactly the class's one \
                         failure payload word (the *_other message, the parse position)",
                    ));
                },
            }
            class_members.push((*tag, member.clone()));
        }
        Ok(class_members)
    }

    /// A result-type-keyed fallible native's union path (the timer race,
    /// the channel deadline twins): the success members are the union's
    /// own non-class members, the payload's word span resolving from
    /// them — exactly the `reduce` keying's derivation over the call's
    /// result type, over a span instead of a bare word. A deinit hook on
    /// any member refuses at the union shape's own walk (the per-span
    /// deinit-claim seam), so the members that reach here stage their
    /// whole word span: one member's fields the flat payload words, a
    /// multi-atom answer the winner's own union span crossing verbatim
    /// behind the layout-equality check. The shared classed machinery
    /// re-spans the winner span into the union: `?`, `.err`, and match
    /// handle the deadline through the ordinary error machinery, the
    /// `timed_out` class landing on the io taxonomy's own member so
    /// io-handling callers absorb it by set containment.
    pub(in crate::func) fn keyed_native_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        fail_payload: SlotType,
        classes: &'static [(u8, &'static str)],
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(LowerError::unsupported(
                no_span(), // re-pointed at the call site
                "a deadline call's result",
                "the call answers its success type unioned with `timed_out` and the deadline's \
                 member must be handled or propagate; a total spelling has no member for the \
                 deadline",
            ));
        };
        let class_members = self.class_members_of(u, fail_payload, classes)?;
        let is_class = |atom: TypeId| {
            class_members
                .iter()
                .any(|(_, member): &(u8, MemberShape)| member.atom == atom)
        };
        let success_members: Vec<MemberShape> = u
            .members
            .iter()
            .filter(|member| !is_class(member.atom))
            .cloned()
            .collect();
        if success_members.is_empty() {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union must be exactly the success member plus the failure taxonomy's class \
                 members; a wider union has no member mapping for the native's failure classes",
            ));
        }
        let answer: Vec<TypeId> = success_members.iter().map(|member| member.atom).collect();
        let (success, payload_words): (SuccessShape, Vec<SlotType>) =
            match success_members.as_slice() {
                [only] => match only.fields.as_slice() {
                    // a bare member: the unit value or a bare tag over the
                    // span's inert payload word
                    [] => (SuccessShape::Bare { atom: only.atom }, vec![SlotType::I64]),
                    fields => (
                        SuccessShape::Payload {
                            atom: only.atom,
                            fields: fields.to_vec(),
                        },
                        fields
                            .iter()
                            .flat_map(|field| field.slots.iter().copied())
                            .collect(),
                    ),
                },
                // a multi-atom answer: the winner's own tagged span crosses
                // verbatim, admitted when its tag-and-region layout equals
                // the result union's (the same atoms claiming the same
                // region words in the same order)
                _ => {
                    let element = self.shapes.members_shape(&answer, 0)?;
                    if shape_words(&element) != u.words {
                        return Err(LowerError::unsupported(
                            no_span(), // re-pointed at the call site
                            "a deadline answer's span",
                            "the answer union's own span disagrees with the call's result union; \
                             the verbatim hand-back needs the winner's tag-and-region layout to \
                             equal the result's, and a member the winner does not carry widened it",
                        ));
                    }
                    (SuccessShape::Adopt, u.words.clone())
                },
            };
        self.classed_native_union(
            native,
            ty,
            &payload_words,
            success,
            fail_payload,
            classes,
            staged,
        )
    }

    /// Records the words a fallible native's result span lands on: word 0
    /// the discriminant plus the failure handler's println argument, word
    /// 1 the success payload plus the failure class's payload (the
    /// message `Ref`, or the parse reader's position `I64` — the render
    /// staging it back through word 0 when it is the position).
    pub(in crate::func) fn overlay_fallible(
        &mut self,
        staged: &[SlotType],
        payload: SlotType,
        fail_payload: SlotType,
    ) {
        self.overlay_site(staged, &[SlotType::Tag, payload]);
        self.overlay_len = self.overlay_len.max(2);
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        self.overlay[0].insert(SlotType::Ref);
        self.overlay[0].insert(fail_payload);
        self.overlay[1].insert(fail_payload);
    }

    /// One checked-read native call (`list_get`, `map_get`): the ok/none
    /// span lands like a fallible native's, but neither side is a
    /// failure — it re-spans into the call's own `t | none` union: ok
    /// writes the element member with the payload's whole word span (the
    /// native channel: one overlay word per element word, a count per
    /// `Ref` word produced), none the predeclared unit member, so `?`,
    /// `.ok`, and match see an ordinary optional value. A one-atom
    /// element constructs its member (a tag write plus the payload
    /// words); a multi-atom element — an enum inside the optional, every
    /// case atom a member of the call's result union — hands its whole
    /// span back verbatim, the element's own tag word already naming a
    /// result member, admitted when the element's own union layout
    /// equals the result union's (the same atoms claiming the same
    /// region words in the same order). The rejected index the none side
    /// carries has no member field to surface in, so it drops. The
    /// generic spelling routes the same span through CALLG, the evidence
    /// words staged behind the real arguments.
    pub(in crate::func) fn checked_native_result(
        &mut self,
        target: Callee,
        ty: Ty,
        staged: &[SlotType],
        real_args: usize,
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(LowerError::unsupported(
                no_span(),
                "a checked-read native call",
                "the call's result type must be the `t | none` union",
            ));
        };
        let none_member = u
            .member(TypeId::NONE)
            .ok_or_else(|| mismatch("a checked read's none member"))?;
        let element = set_diff(self.members(ty), &[TypeId::NONE]);
        // the ok side's construction: a one-atom element's member entry,
        // or the multi-atom element's verbatim span copy
        let elem_member = match element.as_slice() {
            [] => {
                return Err(LowerError::unsupported(
                    no_span(),
                    "a checked-read native call",
                    "the element type must be one atom; narrow list elements before the read",
                ));
            },
            [elem] => Some(
                u.member(*elem)
                    .ok_or_else(|| mismatch("a checked read's element member"))?,
            ),
            many => {
                let words = shape_words(&self.shapes.members_shape(many, 0)?);
                if words != u.words {
                    return Err(LowerError::unsupported(
                        no_span(),
                        "a checked-read native call",
                        "the element union's own span disagrees with the call's result union; the \
                         verbatim hand-back needs the element's tag-and-region layout to equal \
                         the result's, and a member the element does not carry widened it",
                    ));
                }
                None
            },
        };
        // the ok payload's flat word span — the member's fields flattened
        // in declaration order, or the multi-atom element's whole span
        // (its own tag word first): the native answers the element's
        // whole span, one overlay word per element word
        let elem_words: Vec<SlotType> = match elem_member {
            Some(member) => member
                .fields
                .iter()
                .flat_map(|field| field.slots.iter().copied())
                .collect(),
            None => u.words.clone(),
        };
        let mut result_words = vec![SlotType::Tag];
        result_words.extend_from_slice(&elem_words);
        self.guard_staged_count(staged.len().max(result_words.len()))?;
        self.overlay_site(staged, &result_words);
        let kidx = self.pool(self.funcref_const(target));
        match target {
            // the staged span of a generic call ends in its evidence
            // words; nargs names the real arguments, the constraint list
            // carries the evidence count
            Callee::Generic(_) => {
                self.emit(Op::Callg {
                    callee: kidx,
                    nargs: u8::try_from(real_args).expect("argument count fits u8"),
                });
            },
            Callee::Builtin(_) => {
                self.emit(Op::Call {
                    callee: kidx,
                    nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
                });
            },
            Callee::Local(_)
            | Callee::Evidence(_)
            | Callee::Foreign { .. }
            | Callee::Bound { .. } => {
                return Err(mismatch("a checked-read callee"));
            },
        }
        self.rc.native_done(&mut self.code);
        let result = self.temp_span(ty)?;
        let ok_lbl = self.label();
        let join = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: none and ok each write the span with their own value
        let base = self.rc.save();
        let tag = self.shapes.tag_id(TypeId::NONE)?;
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &none_member.fields)?;
        let arm_none = self.rc.take();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        match elem_member {
            Some(member) => {
                let tag = self.shapes.tag_id(member.atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(result.base),
                    tag,
                });
                self.zero_init_member(result.base, &u.words, &member.fields)?;
                // the payload copies from the flat overlay span into the member's
                // claimed words (fields may claim scattered region offsets; the
                // flat index walks the element's declaration order)
                let mut flat = 0usize;
                for field in &member.fields {
                    for (offset, slot) in field.slots.iter().enumerate() {
                        let word = result.base
                            + field.word
                            + u8::try_from(offset).expect("offset fits u8");
                        self.emit(Op::Mov {
                            dst: Slot::body(word),
                            src: Slot::overlay(
                                u8::try_from(1 + flat + offset).expect("overlay word fits u8"),
                            ),
                        });
                        if *slot == SlotType::Ref {
                            self.rc.produced(word);
                        }
                    }
                    flat += field.slots.len();
                }
            },
            // the multi-atom element: the native's payload IS the
            // element's whole span, its tag word first, and every word
            // copies verbatim into the result's own span — the same
            // atoms claimed the same region, so word `i` of the payload
            // is word `i` of the result
            None => {
                for (index, slot) in u.words.iter().enumerate() {
                    let word = result.base + u8::try_from(index).expect("span width fits u8");
                    self.emit(Op::Mov {
                        dst: Slot::body(word),
                        src: Slot::overlay(u8::try_from(1 + index).expect("overlay word fits u8")),
                    });
                    if *slot == SlotType::Ref {
                        self.rc.produced(word);
                    }
                }
            },
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_none, false), (arm_ok, false)]);
        if let [none, ok] = &dying[..] {
            // a dead Ref payload word keeps its zero-init claim: the miss
            // path decs its interned empty-string reference here (the
            // word is dead by construction — the live member's words are
            // the only ones ever read)
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, none);
            self.splice(epilogue, none_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.err` handler: tag tests for the handled members jump to an
    /// inlined handler body; the success value widens into the result.
    pub(in crate::func) fn err_value(
        &mut self,
        recv: &Expr,
        lambda: &Expr,
        ty: Ty,
        site: Span,
    ) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let Expr::Lambda { params, body, .. } = lambda else {
            return Err(LowerError::unsupported(
                lambda.span(),
                "a non-lambda handler",
                "`.err` takes a lambda that inlines at the call site",
            ));
        };
        let src_members = self.members(src_ty);
        let handled = match self.handler_handled(site)? {
            // the checker's handled-set record, filtered to the members
            // the operand carries: a foreign annotated member never tests
            // at runtime, and the site's result type cannot re-derive the
            // set once the handler's answer re-adds a handled member
            Some(set) => self
                .members(set)
                .iter()
                .copied()
                .filter(|m| src_members.contains(m))
                .collect(),
            None => set_diff(self.members(src_ty), self.members(ty)),
        };
        let result = self.temp_span(ty)?;
        let handler_lbl = self.label();
        let join = self.label();
        for member in &handled {
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: handler_lbl,
            });
        }
        // success: the kept members re-span into the result over the
        // operand's real layout — an operand's raw span never lands
        // directly (union regions are per-union-type), and the kept
        // set's region can be narrower than the operand's when a
        // handled member contributed a word shape the kept set does
        // not need
        let base = self.rc.save();
        let kept = set_diff(self.members(src_ty), &handled);
        // every member handled: the fall-through is dead code, so its
        // arm never reaches the join — the handler's answer is the
        // result's only writer and must survive the reconcile instead
        // of dying at the join as one arm's divergent count
        let flow_dead = kept.is_empty();
        if kept.len() == 1 && !self.is_span_ty(ty) {
            // the result is the one kept atom itself: claiming that
            // atom is honest — the word-address path extracts the
            // member's payload words out of the operand's layout
            self.widen_into(
                &result,
                ty,
                &TypedPlace {
                    place: src.place.clone(),
                    ty,
                },
                recv.span(),
            )?;
        } else {
            let labels: Vec<usize> = kept.iter().map(|_| self.label()).collect();
            for (member, label) in kept.iter().zip(&labels) {
                let cond = self.temp_word(SlotType::Bool)?;
                let src_word = self.tag_addr(&src.place)?;
                let tag = self.shapes.tag_id(*member)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src_word),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: *label,
                });
            }
            // dead fallthrough: the handled tests above cover every
            // other member of the operand's type
            self.emit(Op::Jmp { target: join });
            for (member, label) in kept.iter().zip(labels) {
                self.bind_label(label);
                self.respan_into(&result, ty, &src.place, *member)?;
            }
        }
        let arm_ok = self.rc.take();
        let ok_div = self.rc.diverged();
        let ok_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(handler_lbl);
        {
            self.scope_push();
            let [param] = params.as_slice() else {
                return Err(mismatch("a handler parameter"));
            };
            if !matches!(param.mode, ds_parse::ParamMode::Borrow) {
                return Err(LowerError::unsupported(
                    param.span,
                    "a mode on a handler parameter",
                    "the inlined handler binds its parameter as an alias of the payload word; the \
                     extraction seam owns no claim hand-off yet",
                ));
            }
            self.binders.insert(param.name.span, Place {
                base: src.place.base,
                shape: src.place.shape.clone(),
            });
            let param_ty = match self.binder_fact(param.name.span)? {
                Some(fact) => fact.ty,
                None => src_ty,
            };
            self.rc_bind_borrowed(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            // a diverging handler (`panic` in the body) never hands its
            // answer to the join: the firing arm's own exit stands in
            if !self.diverges_at(body.span()) {
                self.refuse_fn_word_answer(
                    ty,
                    &body_tp,
                    body.span(),
                    "a capturing closure in a handler's answer",
                    "an unproven callable in a handler's answer",
                )?;
                self.widen_into(&result, ty, &body_tp, body.span())?;
            }
            self.scope_pop();
        }
        let arm_err = self.rc.take();
        let err_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_ok, ok_div || flow_dead), (arm_err, err_div)]);
        if let [ok, err] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, ok);
            self.splice(epilogue, ok_jmp);
            self.arm_epilogue(err);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.ok` handler: the mirror of `.err` — the ok-side members
    /// jump to an inlined handler body, and each remainder member
    /// re-spans into the result, which carries the difference side
    /// composed with the handler's own result.
    pub(in crate::func) fn ok_value(
        &mut self,
        recv: &Expr,
        lambda: &Expr,
        ty: Ty,
        site: Span,
    ) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let Expr::Lambda { params, body, .. } = lambda else {
            return Err(LowerError::unsupported(
                lambda.span(),
                "a non-lambda handler",
                "`.ok` takes a lambda that inlines at the call site",
            ));
        };
        let [param] = params.as_slice() else {
            return Err(mismatch("a handler parameter"));
        };
        if !matches!(param.mode, ds_parse::ParamMode::Borrow) {
            return Err(LowerError::unsupported(
                param.span,
                "a mode on a handler parameter",
                "the inlined handler binds its parameter as an alias of the payload word; the \
                 extraction seam owns no claim hand-off yet",
            ));
        }
        let src_members = self.members(src_ty);
        let handled = match self.handler_handled(site)? {
            // the checker's handled-set record (the `.ok` present-value
            // default included), filtered to the members the operand
            // carries; the site-local derivation stands only for a site
            // the checker refused before recording
            Some(set) => self
                .members(set)
                .iter()
                .copied()
                .filter(|m| src_members.contains(m))
                .collect(),
            None => self.ok_side(param, body, src_ty)?,
        };
        let result = self.temp_span(ty)?;
        let handler_lbl = self.label();
        let join = self.label();
        for member in &handled {
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: handler_lbl,
            });
        }
        // the difference side flows into the result; the fall-through and
        // the handler arm fork the ownership map
        let base = self.rc.save();
        let remainder = set_diff(self.members(src_ty), &handled);
        let mut flow_jmps = Vec::new();
        // the dead fallthrough arm never reaches the join (the mirror of
        // err_value's kept-empty arm): the handler's answer is the
        // result's only writer and must survive the reconcile
        let flow_dead = remainder.is_empty();
        if remainder.is_empty() {
            // every member jumps to the handler; the fallthrough is dead
            self.emit(Op::Jmp { target: join });
        } else if remainder.len() == 1 && !self.is_span_ty(ty) {
            // the handled tests already proved the fall-through value is
            // the one remainder member, and the checked difference
            // collapses the result to that atom itself: the word-address
            // path extracts the member's payload words out of the
            // operand's layout, the mirror of err_value's kept-side arm
            self.widen_into(
                &result,
                ty,
                &TypedPlace {
                    place: src.place.clone(),
                    ty,
                },
                recv.span(),
            )?;
            flow_jmps.push(self.code.len());
            self.emit(Op::Jmp { target: join });
        } else if self.members(src_ty).len() == 1 {
            // a single-member operand: the value itself flows, no tags
            self.widen_into(
                &result,
                ty,
                &TypedPlace {
                    place: src.place.clone(),
                    ty: src_ty,
                },
                recv.span(),
            )?;
            flow_jmps.push(self.code.len());
            self.emit(Op::Jmp { target: join });
        } else {
            let labels: Vec<usize> = remainder.iter().map(|_| self.label()).collect();
            for (member, label) in remainder.iter().zip(&labels) {
                let cond = self.temp_word(SlotType::Bool)?;
                let src_word = self.tag_addr(&src.place)?;
                let tag = self.shapes.tag_id(*member)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src_word),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: *label,
                });
            }
            // dead: the tests cover every member of the operand's type
            self.emit(Op::Jmp { target: join });
            for (member, label) in remainder.iter().zip(labels) {
                self.bind_label(label);
                self.respan_into(&result, ty, &src.place, *member)?;
                flow_jmps.push(self.code.len());
                self.emit(Op::Jmp { target: join });
            }
        }
        self.bind_label(handler_lbl);
        let arm_flow = self.rc.take();
        let flow_div = self.rc.diverged();
        self.rc.restore(base);
        {
            self.scope_push();
            self.binders.insert(param.name.span, Place {
                base: src.place.base,
                shape: src.place.shape.clone(),
            });
            let param_ty = match self.binder_fact(param.name.span)? {
                Some(fact) => fact.ty,
                None => src_ty,
            };
            self.rc_bind_borrowed(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            // a diverging handler (`panic` in the body) never hands its
            // answer to the join: the firing arm's own exit stands in
            if !self.diverges_at(body.span()) {
                self.refuse_fn_word_answer(
                    ty,
                    &body_tp,
                    body.span(),
                    "a capturing closure in a handler's answer",
                    "an unproven callable in a handler's answer",
                )?;
                self.widen_into(&result, ty, &body_tp, body.span())?;
            }
            self.scope_pop();
        }
        let arm_ok = self.rc.take();
        let ok_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_flow, flow_div || flow_dead), (arm_ok, ok_div)]);
        if let [flow, ok] = &dying[..] {
            let mut shift = 0;
            for at in &flow_jmps {
                let mut epilogue = Vec::new();
                Placement::emit_drops(&mut epilogue, flow);
                let len = epilogue.len();
                self.splice(epilogue, at + shift);
                shift += len;
            }
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.ok` handler's ok side, the checker's handled-set rule: the
    /// annotated parameter's members, or — unannotated — everything the
    /// handler's own result does not keep of the operand. Foreign
    /// annotated members never test at runtime, so they drop here.
    pub(in crate::func) fn ok_side(
        &mut self,
        param: &Param,
        body: &Expr,
        src_ty: Ty,
    ) -> Result<Vec<TypeId>, LowerError> {
        let src_members = self.members(src_ty);
        if param.ty.is_some() {
            let fact = self
                .binder_fact(param.name.span)?
                .ok_or_else(|| mismatch("a handler parameter"))?;
            let annotated = self.members(fact.ty);
            return Ok(src_members
                .iter()
                .copied()
                .filter(|m| annotated.contains(m))
                .collect());
        }
        let body_members = self.members(self.val(body.span())?.ty);
        Ok(src_members
            .iter()
            .copied()
            .filter(|m| !body_members.contains(m))
            .collect())
    }
}
