//! Operators: binary and arithmetic words, the float refusals, compare,
//! the bool chain, `in` over ranges, structural equality (word
//! plans per atom, collection content compares, union tags), and unary.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// Binary operators: typed uops for scalars, short-circuit chains for
    /// `&&`/`||`, memberwise chains for structural `==`, and the
    /// concat call for string `+`.
    pub(super) fn binary_value(&mut self, e: &Expr, static_ty: Ty) -> Result<Place, LowerError> {
        let Expr::Binary {
            op,
            left,
            right,
            span,
        } = e
        else {
            return Err(mismatch("a binary expression"));
        };
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            return self.bool_chain(*op == BinaryOp::And, left, right);
        }
        if *op == BinaryOp::In
            && let Expr::Range {
                inclusive,
                start,
                end,
                ..
            } = peel_parens(right)
        {
            return self.in_range_value(left, start, end, *inclusive, *span);
        }
        if *op == BinaryOp::In {
            // The checker refuses every non-range right side, the list
            // membership spelling the member call `xs.contains(x)`; the
            // belt keeps a shape the check never admits off the runtime
            // path.
            return Err(LowerError::unsupported(
                *span,
                "a membership over a non-range right side",
                "`in` lowers over int ranges; list membership lowers through the `xs.contains(x)` \
                 member call",
            ));
        }
        // string `+` is the concat native's call shape: its borrow
        // window opens before operand evaluation, so a later operand's
        // own last-use dec cannot free a word the CALL still reads
        let str_add = *op == BinaryOp::Add
            && self.is_string_add_ty(self.val(left.span())?.ty, self.val(right.span())?.ty);
        if str_add {
            self.rc.native_open();
        }
        // The scalar destination offer: the assignment store or the
        // call staging that owns this expression may have offered its
        // word to this node. The offer is taken here, before operand
        // evaluation, so no nested node can write the offered word; an
        // arm that declines restores it for the offering site to read
        // back as unconsumed.
        let offered = match op {
            BinaryOp::Add if !str_add => self.scalar_dst.take(),
            BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::WrapAdd
            | BinaryOp::WrapSub
            | BinaryOp::WrapMul
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitXor
            | BinaryOp::BitOr => self.scalar_dst.take(),
            _ => None,
        };
        let lhs = self.value(left)?;
        if str_add {
            let words = self
                .word_addrs(&lhs.place, lhs.ty)
                .map_err(|err| err.respanned(*span))?;
            self.rc.native_reads(words.iter().map(|addr| addr.slot));
        }
        let rhs = self.value(right)?;
        let (place, landed) = match op {
            BinaryOp::Eq | BinaryOp::Ne => (
                self.eq_chain(&lhs, &rhs, e.span())
                    .map_err(|err| err.respanned(e.span()))?,
                false,
            ),
            BinaryOp::Add if str_add => (self.concat_value(&lhs, &rhs, static_ty, *span)?, false),
            BinaryOp::Add => {
                self.arith_dst(DataOp::AddI64, DataOp::AddF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Sub => {
                self.arith_dst(DataOp::SubI64, DataOp::SubF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Mul => {
                self.arith_dst(DataOp::MulI64, DataOp::MulF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Div => {
                self.arith_dst(DataOp::DivI64, DataOp::DivF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Rem => {
                self.reject_float_rem(&lhs, *span)?;
                self.arith_dst(DataOp::ModI64, DataOp::ModI64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Lt => (
                self.compare(DataOp::LtI64, DataOp::LtF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::Le => (
                self.compare(DataOp::LeI64, DataOp::LeF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::Gt => (
                self.compare(DataOp::GtI64, DataOp::GtF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::Ge => (
                self.compare(DataOp::GeI64, DataOp::GeF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::WrapAdd => {
                self.reject_float_wrap(&lhs, *span)?;
                self.arith_dst(
                    DataOp::WrapAddI64,
                    DataOp::WrapAddI64,
                    &lhs,
                    &rhs,
                    offered,
                    *span,
                )?
            },
            BinaryOp::WrapSub => {
                self.reject_float_wrap(&lhs, *span)?;
                self.arith_dst(
                    DataOp::WrapSubI64,
                    DataOp::WrapSubI64,
                    &lhs,
                    &rhs,
                    offered,
                    *span,
                )?
            },
            BinaryOp::WrapMul => {
                self.reject_float_wrap(&lhs, *span)?;
                self.arith_dst(
                    DataOp::WrapMulI64,
                    DataOp::WrapMulI64,
                    &lhs,
                    &rhs,
                    offered,
                    *span,
                )?
            },
            BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor => {
                self.reject_float_shift_bitwise(&lhs, *span)?;
                let op = match op {
                    BinaryOp::Shl => DataOp::ShlI64,
                    BinaryOp::Shr => DataOp::ShrI64,
                    BinaryOp::BitAnd => DataOp::BitAndI64,
                    BinaryOp::BitOr => DataOp::BitOrI64,
                    BinaryOp::BitXor => DataOp::BitXorI64,
                    _ => unreachable!("the outer arm partitions the operators"),
                };
                self.arith_dst(op, op, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::And | BinaryOp::Or | BinaryOp::In => unreachable!("handled above"),
        };
        if !landed {
            self.scalar_dst = offered;
        }
        if *op == BinaryOp::Ne {
            // `!=` over spans: invert the eq chain's bool
            let true_word = self.temp_word(SlotType::Bool)?;
            let kidx = self.pool(Constant::Bool(true));
            self.emit(Op::Kconst {
                dst: Slot::body(true_word),
                kidx,
            });
            let dst = self.temp_word(SlotType::Bool)?;
            self.emit(Op::Data {
                op: DataOp::NeqBool,
                dst: Slot::body(dst),
                lhs: Slot::body(place.base),
                rhs: Slot::body(true_word),
            });
            return Ok(Place {
                base: dst,
                shape: PlaceShape::Word(SlotType::Bool),
            });
        }
        Ok(place)
    }

    pub(super) fn arith(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        self.emit_data(op, lhs, rhs, span)
    }

    /// arith over the offered scalar destination: the data op writes
    /// the offered word directly when its result kind matches the
    /// destination's own, and the fresh temp otherwise. The VM reads
    /// both operands before writing dst, so the offered word may alias
    /// an operand read — the same guarantee the compound store relies
    /// on.
    pub(super) fn arith_dst(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        dst: Option<ScalarDst>,
        span: Span,
    ) -> Result<(Place, bool), LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        let Some(dst) = dst else {
            return Ok((self.emit_data(op, lhs, rhs, span)?, false));
        };
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let slot = op.result_slot(l.slot_ty);
        if slot != dst.slot() {
            return Ok((self.emit_data(op, lhs, rhs, span)?, false));
        }
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        let word = dst.word();
        self.emit(Op::Data {
            op,
            dst: dst.slot_word(),
            lhs: Slot::body(l.slot),
            rhs: Slot::body(r.slot),
        });
        Ok((
            Place {
                base: word,
                shape: PlaceShape::Word(slot),
            },
            true,
        ))
    }

    /// arith writing the pinned word `dst` — the compound assignment's
    /// direct scalar store over the var's own word.
    pub(super) fn arith_at(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        dst: u8,
        span: Span,
    ) -> Result<Place, LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        self.emit_data_at(op, lhs, rhs, dst, span)
    }

    /// Both operands solve to the string atom: `+` is the concat
    /// call's spelling. The checked-type-only probe answers before
    /// operand evaluation, so the call's borrow window opens in time to
    /// pin the words the CALL reads.
    pub(super) fn is_string_add_ty(&self, lhs: Ty, rhs: Ty) -> bool {
        self.shapes.builtin_instance(lhs) == Some(BuiltinInstance::Str)
            && self.shapes.builtin_instance(rhs) == Some(BuiltinInstance::Str)
    }

    /// String `+` lowers to the concat native's own call shape: the two
    /// Ref words stage as borrow windows, the CALL dispatches the
    /// native, and the fresh result lands on a body temp — the same
    /// staging and RC contract an explicit `a.concat(b)` lowers
    /// through, so the operator is a spelling, never a second
    /// operation.
    pub(super) fn concat_value(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        let native = crate::native::lookup("concat").unwrap_or_else(|| panic!("concat is tabled"));
        for (index, word) in [l.slot, r.slot].into_iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("two words fit u8")),
                src: Slot::body(word),
            });
            self.rc.staged_native(word, false);
        }
        self.overlay_site(&[SlotType::Ref, SlotType::Ref], &[SlotType::Ref]);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 2,
        });
        self.rc.native_done(&mut self.code);
        self.result_place(ty, &[SlotType::Ref], span)
    }

    /// Float remainder is a boundary, not a trap: the numeric table admits
    /// float rem, but the locked uop set carries ModI64 only — both
    /// Rem sites (the binary operator and the compound assignment) check
    /// the left operand's word and reject F64 at compile time.
    pub(super) fn reject_float_operand(
        &mut self,
        lhs: &TypedPlace,
        span: Span,
        subject: &'static str,
        reason: &'static str,
    ) -> Result<(), LowerError> {
        if let [only] = self.word_addrs(&lhs.place, lhs.ty)?.as_slice()
            && only.slot_ty == SlotType::F64
        {
            return Err(LowerError::unsupported(span, subject, reason));
        }
        Ok(())
    }

    pub(super) fn reject_float_rem(
        &mut self,
        lhs: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.reject_float_operand(
            lhs,
            span,
            "a float remainder",
            "no ModF64 uop exists in the locked set; the numeric table admits float rem — library \
             rem arrives with @std",
        )
    }

    /// The defensive float refusal behind the checker's own rule: the
    /// wrap uops are i64-only, so a float operand that slips the check
    /// refuses here instead of mis-lowering as an i64 word.
    pub(super) fn reject_float_wrap(
        &mut self,
        lhs: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.reject_float_operand(
            lhs,
            span,
            "a float wrapping operand",
            "wrapping operators are integer-only; floats are IEEE [B7]",
        )
    }

    /// The defensive float refusal behind the checker's own rule for
    /// the shift and bitwise operators: no f64 variant of any of them
    /// exists, so a float operand that slips the check refuses here
    /// instead of mis-lowering as an i64 word.
    pub(super) fn reject_float_shift_bitwise(
        &mut self,
        lhs: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.reject_float_operand(
            lhs,
            span,
            "a float shift or bitwise operand",
            "shift and bitwise operators are integer-only [B7]",
        )
    }

    pub(super) fn compare(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        self.emit_data(op, lhs, rhs, span)
    }

    pub(super) fn pick_int_float(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        span: Span,
    ) -> Result<DataOp, LowerError> {
        let addrs = self.word_addrs(&lhs.place, lhs.ty)?;
        let [only] = addrs.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an arithmetic operand",
                "operands must be single-word scalars this round",
            ));
        };
        match only.slot_ty {
            SlotType::I64 => Ok(int_op),
            SlotType::F64 => Ok(float_op),
            _ => Err(LowerError::unsupported(
                span,
                "an arithmetic operand",
                "int and float operands only in the M3 uop contract",
            )),
        }
    }

    pub(super) fn emit_data(
        &mut self,
        op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let dst = self.temp_word(op.result_slot(l.slot_ty))?;
        self.emit_data_at(op, lhs, rhs, dst, span)
    }

    /// The data op writing the pinned word `dst` — the direct scalar
    /// store. The VM reads both operands before writing dst, so `dst`
    /// may alias an operand word (the compound store reads the var's
    /// current value and overwrites it in one op).
    pub(super) fn emit_data_at(
        &mut self,
        op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        dst: u8,
        span: Span,
    ) -> Result<Place, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        self.emit(Op::Data {
            op,
            dst: Slot::body(dst),
            lhs: Slot::body(l.slot),
            rhs: Slot::body(r.slot),
        });
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(op.result_slot(l.slot_ty)),
        })
    }

    /// `&&`/`||` as short-circuit jump chains; the uop set has no boolean
    /// arithmetic opcode.
    pub(super) fn bool_chain(
        &mut self,
        is_and: bool,
        left: &Expr,
        right: &Expr,
    ) -> Result<Place, LowerError> {
        let short = self.label();
        let lie_lbl = self.label();
        let join = self.label();
        let lhs = self.value(left)?;
        let lcond = single(&self.word_addrs(&lhs.place, lhs.ty)?, left.span())?;
        self.emit(if is_and {
            Op::JmpifNot {
                cond: Slot::body(lcond.slot),
                target: short,
            }
        } else {
            Op::Jmpif {
                cond: Slot::body(lcond.slot),
                target: short,
            }
        });
        // path-fork: the short path skips the right operand entirely
        let base = self.rc.save();
        let rhs = self.value(right)?;
        let rcond = single(&self.word_addrs(&rhs.place, rhs.ty)?, right.span())?;
        self.emit(if is_and {
            Op::JmpifNot {
                cond: Slot::body(rcond.slot),
                target: lie_lbl,
            }
        } else {
            Op::Jmpif {
                cond: Slot::body(rcond.slot),
                target: lie_lbl,
            }
        });
        let truth = self.pool(Constant::Bool(is_and));
        let lie = self.pool(Constant::Bool(!is_and));
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: truth,
        });
        let arm_full = self.rc.take();
        let full_div = self.rc.diverged();
        let truth_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        // The right operand's own shortcut answer: this block runs with
        // the post-right-operand ownership state, so its drops differ
        // from the left shortcut's — a block shared with the first
        // jump would dec the words the right operand's call already
        // handed to its callee (a double drop, the trap the `||`
        // right-arm probes pinned). Each of the full arm's two exits
        // carries its own copy of the full arm's dying drops, and each
        // runtime path decs once.
        self.bind_label(lie_lbl);
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        let lie_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(short);
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        let arm_short = self.rc.take();
        let short_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_full, full_div), (arm_short, short_div)]);
        if let [full, short] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, full);
            // the later site splices first: the earlier site's splice
            // would shift it
            self.splice(epilogue.clone(), lie_jmp);
            self.splice(epilogue, truth_jmp);
            self.arm_epilogue(short);
        }
        self.bind_label(join);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// `x in a..b` / `x in a..=b`: the operand and both bounds
    /// evaluate once; two comparisons join by jumps (the uop set has no
    /// boolean arithmetic).
    pub(super) fn in_range_value(
        &mut self,
        left: &Expr,
        start: &Expr,
        end: &Expr,
        inclusive: bool,
        span: Span,
    ) -> Result<Place, LowerError> {
        let x_tp = self.value(left)?;
        let x_w = single(&self.word_addrs(&x_tp.place, x_tp.ty)?, left.span())?;
        let start_tp = self.value(start)?;
        let start_w = single(
            &self.word_addrs(&start_tp.place, start_tp.ty)?,
            start.span(),
        )?;
        let end_tp = self.value(end)?;
        let end_w = single(&self.word_addrs(&end_tp.place, end_tp.ty)?, end.span())?;
        if x_w.slot_ty != SlotType::I64
            || start_w.slot_ty != SlotType::I64
            || end_w.slot_ty != SlotType::I64
        {
            return Err(LowerError::unsupported(
                span,
                "a range membership over non-`int` widths",
                "only the ambient `int` width lowers this round; conversions are explicit [B7]",
            ));
        }
        let miss = self.label();
        let join = self.label();
        let low = self.emit_data(DataOp::GeI64, &x_tp, &start_tp, span)?;
        let low_w = single(&self.word_addrs(&low, x_tp.ty)?, span)?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(low_w.slot),
            target: miss,
        });
        let bound_op = if inclusive { DataOp::LeI64 } else { DataOp::LtI64 };
        let hi = self.emit_data(bound_op, &x_tp, &end_tp, span)?;
        let hi_w = single(&self.word_addrs(&hi, x_tp.ty)?, span)?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(hi_w.slot),
            target: miss,
        });
        let truth = self.pool(Constant::Bool(true));
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: truth,
        });
        self.emit(Op::Jmp { target: join });
        self.bind_label(miss);
        let lie = self.pool(Constant::Bool(false));
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        self.bind_label(join);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// Structural `==`: one uop per scalar, a fail-fast chain per struct,
    /// a tag dispatch plus payload compare per union.
    pub(super) fn eq_chain(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        // A multi-atom value never compares against one member's payload
        // value: the per-member tag dispatch needs a tag word on both
        // sides, and a narrowed value of one member carries none — the
        // union rides the dispatch side (a unit-tag value against it
        // compares from either side), and every other mixed spelling
        // refuses naming the narrowing seam.
        let l_union = matches!(&*self.shapes.shape(lhs.ty)?, Shape::Union(_));
        let r_union = matches!(&*self.shapes.shape(rhs.ty)?, Shape::Union(_));
        let tag_word = |place: &Place| matches!(place.shape, PlaceShape::Word(SlotType::Tag));
        if l_union != r_union && !tag_word(&lhs.place) && !tag_word(&rhs.place) {
            return Err(LowerError::unsupported(
                span,
                "an equality over a multi-atom value against one member's value",
                "the per-member tag dispatch needs a tag word on both sides and one member's \
                 payload value carries none; narrow the union with `is` or a match arm before \
                 comparing",
            ));
        }
        if r_union && !l_union {
            return self.eq_chain(rhs, lhs, span);
        }
        let lty = lhs.ty;
        let shape = self.shapes.shape(lty)?;
        let dst = match &*shape {
            Shape::Word(slot) => {
                // the heap collections share the string's one Ref word
                // shape but the EqStr uop reads string objects: each
                // collection's content-compare native serves the chain's
                // word
                if *slot == SlotType::Ref {
                    if let Some(word) = self.collection_ty_word(lty)? {
                        self.collection_eq_word(word, lhs, rhs, span)?
                    } else if self.shapes.box_element(lty).is_some() {
                        return Err(box_eq_refusal());
                    } else {
                        self.scalar_eq_word(lhs, rhs, span, *slot)?
                    }
                } else {
                    self.scalar_eq_word(lhs, rhs, span, *slot)?
                }
            },
            Shape::Struct(_) => {
                // the flattened word lists compare pairwise, a field's
                // span word-by-word in declaration order; the compares
                // borrow, so both exits release the operands' dying
                // counts at the join — a fail-fast field mismatch
                // cannot strand a later Ref word's staged count
                let fail = self.label();
                let join = self.label();
                let l = self.word_addrs(&lhs.place, lhs.ty)?;
                let r = self.word_addrs(&rhs.place, rhs.ty)?;
                let plan = self.eq_word_plan(lty)?;
                self.emit_word_compares(&l, &r, &plan, fail)?;
                let truth = self.pool(Constant::Bool(true));
                let dst = self.temp_word(SlotType::Bool)?;
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: truth,
                });
                self.emit(Op::Jmp { target: join });
                self.bind_label(fail);
                let lie = self.pool(Constant::Bool(false));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: lie,
                });
                self.bind_label(join);
                self.release_eq_words(&l, &r);
                dst
            },
            Shape::Tuple(_) => {
                let fail = self.label();
                let join = self.label();
                let l = self.word_addrs(&lhs.place, lhs.ty)?;
                let r = self.word_addrs(&rhs.place, rhs.ty)?;
                let plan = self.eq_word_plan(lty)?;
                self.emit_word_compares(&l, &r, &plan, fail)?;
                let truth = self.pool(Constant::Bool(true));
                let dst = self.temp_word(SlotType::Bool)?;
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: truth,
                });
                self.emit(Op::Jmp { target: join });
                self.bind_label(fail);
                let lie = self.pool(Constant::Bool(false));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: lie,
                });
                self.bind_label(join);
                self.release_eq_words(&l, &r);
                dst
            },
            Shape::Union(_) => self.union_eq(lhs, rhs, lty)?,
        };
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// The scalar `==`/`!=` word: the one uop the word's slot kind
    /// selects (the string Ref's EqStr reads string objects), its reads
    /// owning no word's death.
    pub(super) fn scalar_eq_word(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
        slot: SlotType,
    ) -> Result<u8, LowerError> {
        let op = eq_op(slot)?;
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Data {
            op,
            dst: Slot::body(dst),
            lhs: Slot::body(l.slot),
            rhs: Slot::body(r.slot),
        });
        if op == DataOp::EqStr {
            self.rc.read(&mut self.code, &[l.slot, r.slot]);
        }
        Ok(dst)
    }

    /// The collection `==`/`!=` seam: structural content equality
    /// through the builtin core's content-compare native the word's
    /// collection names — one CALL in the chain's word, the operands
    /// staged as borrows exactly the concat call shape's (a staged word's
    /// count dies at the window's flush, never owning).
    pub(super) fn collection_eq_word(
        &mut self,
        word: WordEq,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<u8, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        self.content_eq_slots(word, l.slot, r.slot, true)
    }

    /// The content compare over two operand words: one CALL, the
    /// operands staged as borrows exactly the concat call shape's, the
    /// bool answer landed on a fresh word — the direct-operand seam and
    /// the per-field chain word share it. `own` marks whether the window's
    /// flush owns a dying operand's dec (a chain word stages keep-mode:
    /// its fail-fast exit skips the flush site, so the chain's merged
    /// exits release instead).
    pub(super) fn content_eq_slots(
        &mut self,
        kind: WordEq,
        l: u8,
        r: u8,
        own: bool,
    ) -> Result<u8, LowerError> {
        let name = match kind {
            WordEq::Bytes => "bytes_eq",
            WordEq::List => "list_eq",
            WordEq::Map => "map_eq",
            WordEq::Scalar(_) => return Err(mismatch("a content compare")),
        };
        let native = crate::native::lookup(name).ok_or_else(|| mismatch("the content compare"))?;
        self.rc.native_open();
        for (index, word) in [l, r].into_iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("two staged words fit u8")),
                src: Slot::body(word),
            });
            if own {
                self.rc.staged_native(word, false);
            } else {
                self.rc.staged_native_keep(word);
            }
        }
        self.overlay_site(&[SlotType::Ref, SlotType::Ref], &[SlotType::Bool]);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 2,
        });
        self.rc.native_done(&mut self.code);
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Mov {
            dst: Slot::body(dst),
            src: Slot::overlay(0),
        });
        Ok(dst)
    }

    /// The per-word compare plan of one type's flattened value span
    /// the chain compares fields by their declared type, never by
    /// their slot kind alone, so the plan pairs with `word_addrs` word
    /// for word. A collection-typed Ref word plans that collection's
    /// content compare (`builtin_instance` names the collection the way
    /// the call sites do); every other word plans the scalar uop its kind
    /// selects — a kind with no uop keeps the refusal the slot-kind
    /// chain gave.
    pub(super) fn eq_word_plan(&mut self, ty: Ty) -> Result<Vec<WordEq>, LowerError> {
        let shape = self.shapes.shape(ty)?;
        match &*shape {
            Shape::Word(slot) => {
                if *slot == SlotType::Ref {
                    if let Some(word) = self.collection_ty_word(ty)? {
                        return Ok(vec![word]);
                    }
                    if self.shapes.box_element(ty).is_some() {
                        return Err(box_eq_refusal());
                    }
                }
                Ok(vec![WordEq::Scalar(eq_op(*slot)?)])
            },
            // a bare struct or tuple flattens its declared fields' plans
            // in span order; a union value never reaches a word chain
            // (its equality rides the tag dispatch), and a union-typed
            // field's span words each plan their own kind — the tag word
            // refusing exactly as the slot-kind chain refused
            Shape::Struct(_) | Shape::Tuple(_) => {
                let [atom] = self.members(ty) else {
                    return Err(mismatch("a span equality plan"));
                };
                self.atom_word_plan(*atom)
            },
            Shape::Union(u) => u
                .words
                .iter()
                .map(|slot| Ok(WordEq::Scalar(eq_op(*slot)?)))
                .collect(),
        }
    }

    /// The per-word compare plan of one atom's own value span: a
    /// collection atom (the bytes word, a list/map heap application)
    /// plans that collection's content compare; a struct or tuple atom
    /// flattens its declared fields' plans; every other atom is its one
    /// word's kind (the uop, or its refusal).
    pub(super) fn atom_word_plan(&mut self, atom: TypeId) -> Result<Vec<WordEq>, LowerError> {
        match &self
            .analyzed
            .decls
            .universe
            .atom_decl(atom)
            .map_err(|_| mismatch("an equality plan"))?
            .kind
        {
            AtomKind::Struct(_) | AtomKind::Tuple(_) => self.payload_word_plan(atom),
            _ => {
                if let Some(word) = self.collection_atom_word(atom)? {
                    return Ok(vec![word]);
                }
                if self.shapes.atom_is_box(atom) {
                    return Err(box_eq_refusal());
                }
                let shape = self.shapes.atom_shape(atom)?;
                let Shape::Word(slot) = &*shape else {
                    return Err(mismatch("an equality plan"));
                };
                Ok(vec![WordEq::Scalar(eq_op(*slot)?)])
            },
        }
    }

    /// The content-compare word one solved collection type plans:
    /// the native its builtin instance names — the by-type entry the
    /// direct operand pair, a membership's element, and the flattened
    /// plan's word arm share.
    ///
    /// `None` for every non-collection type (the string's EqStr serves
    /// its own Ref word).
    pub(super) fn collection_ty_word(&mut self, ty: Ty) -> Result<Option<WordEq>, LowerError> {
        match self.shapes.builtin_instance(ty) {
            Some(BuiltinInstance::Bytes) => Ok(Some(WordEq::Bytes)),
            Some(BuiltinInstance::List) => {
                let element = self
                    .shapes
                    .list_element(ty)
                    .ok_or_else(|| mismatch("a list equality plan"))?;
                self.eq_word_plan(element).map_err(|_| list_refusal())?;
                Ok(Some(WordEq::List))
            },
            Some(BuiltinInstance::Map) => {
                let value = self
                    .shapes
                    .map_value(ty)
                    .ok_or_else(|| mismatch("a map equality plan"))?;
                self.eq_word_plan(value).map_err(|_| map_refusal())?;
                Ok(Some(WordEq::Map))
            },
            _ => Ok(None),
        }
    }

    /// The atom-keyed twin of [`Self::collection_ty_word`]: the content
    /// compare one declared collection atom plans — the entry the field
    /// atoms of the flattened plan ride.
    pub(super) fn collection_atom_word(
        &mut self,
        atom: TypeId,
    ) -> Result<Option<WordEq>, LowerError> {
        let Some(instance) = self.shapes.atom_builtin_instance(atom) else {
            return Ok(None);
        };
        match instance {
            BuiltinInstance::Bytes => Ok(Some(WordEq::Bytes)),
            BuiltinInstance::List => {
                let [element] = self.shapes.apply_args(atom).unwrap_or(&[]) else {
                    return Err(mismatch("a list equality plan"));
                };
                self.eq_word_plan(*element).map_err(|_| list_refusal())?;
                Ok(Some(WordEq::List))
            },
            BuiltinInstance::Map => {
                let [_, value] = self.shapes.apply_args(atom).unwrap_or(&[]) else {
                    return Err(mismatch("a map equality plan"));
                };
                self.eq_word_plan(*value).map_err(|_| map_refusal())?;
                Ok(Some(WordEq::Map))
            },
            _ => Ok(None),
        }
    }

    /// The per-word compare plan of one atom's declared payload fields
    /// (a bare struct's fields, or a union member's), in declaration
    /// order — the typed half of the shape walk `payload_fields` laid
    /// out, so the plan pairs with the laid-out field spans.
    pub(super) fn payload_word_plan(&mut self, atom: TypeId) -> Result<Vec<WordEq>, LowerError> {
        let decl = self
            .analyzed
            .decls
            .universe
            .atom_decl(atom)
            .map_err(|_| mismatch("an equality plan"))?;
        match &decl.kind {
            AtomKind::Struct(fields) => {
                // a part-less struct's payload is its one reserved I64
                // word, the frame's inert zero — no declared field backs
                // it, so the plan mirrors the reserved word itself
                if fields.is_empty() {
                    return Ok(vec![WordEq::Scalar(eq_op(SlotType::I64)?)]);
                }
                let mut plan = Vec::new();
                for field in fields {
                    plan.extend(self.type_word_plan(&field.ty)?);
                }
                Ok(plan)
            },
            AtomKind::Tuple(elements) => {
                let mut plan = Vec::new();
                for element in elements {
                    plan.extend(self.atom_word_plan(*element)?);
                }
                Ok(plan)
            },
            // a bare tag carries no payload; a callable, a primitive,
            // or a heap application is its own one word
            _ => {
                if let Some(word) = self.collection_atom_word(atom)? {
                    return Ok(vec![word]);
                }
                if self.shapes.atom_is_box(atom) {
                    return Err(box_eq_refusal());
                }
                let shape = self.shapes.atom_shape(atom)?;
                let Shape::Word(slot) = &*shape else {
                    return Err(mismatch("an equality plan"));
                };
                Ok(vec![WordEq::Scalar(eq_op(*slot)?)])
            },
        }
    }

    /// The per-word compare plan of one written field type: a bytes
    /// atom plans the content compare; a nested struct or tuple atom
    /// flattens its own fields' plans; a union flattens its whole span's
    /// word kinds; a tuple type recursively flattens its elements' plans.
    pub(super) fn type_word_plan(&mut self, expr: &TypeExpr) -> Result<Vec<WordEq>, LowerError> {
        match expr {
            TypeExpr::Atom(id) => self.atom_word_plan(*id),
            TypeExpr::Union(..) => {
                let members = expr_members(expr).ok_or_else(|| mismatch("an equality plan"))?;
                let shape = self
                    .shapes
                    .members_shape(&members, 0)
                    .map_err(|_| mismatch("an equality plan"))?;
                shape_words(&shape)
                    .iter()
                    .map(|slot| Ok(WordEq::Scalar(eq_op(*slot)?)))
                    .collect()
            },
            TypeExpr::Tuple(elements) => {
                let mut plan = Vec::new();
                for element in elements {
                    plan.extend(self.type_word_plan(element)?);
                }
                Ok(plan)
            },
            // a trait, negation, intersection, or difference never laid
            // out as an inline field span
            _ => Err(mismatch("an equality plan")),
        }
    }

    /// Emits the pairwise word compares of two flattened address lists,
    /// every mismatched word jumping to `fail`; the last word's result
    /// flows on. A struct's whole span compares word-by-word this way —
    /// inline spans are flat words, so per-word equality is the value's,
    /// each word compared by its field's planned kind: a bytes word
    /// through the content-compare native, every other through its
    /// scalar uop. The compares borrow their operands — no read owns its
    /// word's death — because the chain's fail-fast exit skips the later
    /// compares' own release sites; the caller releases every dying
    /// operand word where the chain's exits merge.
    pub(super) fn emit_word_compares(
        &mut self,
        l: &[WordAddr],
        r: &[WordAddr],
        plan: &[WordEq],
        fail: usize,
    ) -> Result<(), LowerError> {
        if l.len() != r.len() || l.len() != plan.len() {
            return Err(mismatch("a span equality's width"));
        }
        for (pair, kind) in l.iter().zip(r.iter()).zip(plan.iter().copied()) {
            let cond = match kind {
                WordEq::Scalar(op) => {
                    if op != eq_op(pair.0.slot_ty)? {
                        return Err(mismatch("a span equality plan"));
                    }
                    let cond = self.temp_word(SlotType::Bool)?;
                    self.emit(Op::Data {
                        op,
                        dst: Slot::body(cond),
                        lhs: Slot::body(pair.0.slot),
                        rhs: Slot::body(pair.1.slot),
                    });
                    cond
                },
                // a collection word dispatches its content compare,
                // staged keep-mode (the chain's merged exits release)
                WordEq::Bytes | WordEq::List | WordEq::Map => {
                    self.content_eq_slots(kind, pair.0.slot, pair.1.slot, false)?
                },
            };
            self.emit(Op::JmpifNot {
                cond: Slot::body(cond),
                target: fail,
            });
        }
        Ok(())
    }

    /// Releases the equality chain's operand counts where its exits merge:
    /// every dying operand word drops here, on the equal path and the
    /// fail-fast path alike (`drop_word` itself defers into an enclosing
    /// window that still reads the word, so a chain nested in a call's
    /// operand staging stays sound).
    pub(super) fn release_eq_words(&mut self, l: &[WordAddr], r: &[WordAddr]) {
        let words: Vec<u8> = l.iter().chain(r.iter()).map(|addr| addr.slot).collect();
        self.rc.read(&mut self.code, &words);
    }

    /// Union equality: per-member tag tests, then the member's payload
    /// compare; distinct members never compare equal.
    pub(super) fn union_eq(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        ty: Ty,
    ) -> Result<u8, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a union equality"));
        };
        let members = &u.members;
        let ltag = self.tag_addr(&lhs.place)?;
        let rtag = self.tag_addr(&rhs.place)?;
        let false_lbl = self.label();
        let done = self.label();
        let dst = self.temp_word(SlotType::Bool)?;
        for member in members {
            let next = self.label();
            self.emit(Op::TagTest {
                dst: Slot::body(dst),
                src: Slot::body(ltag),
                tag: member.tag,
            });
            self.emit(Op::JmpifNot {
                cond: Slot::body(dst),
                target: next,
            });
            self.emit(Op::TagTest {
                dst: Slot::body(dst),
                src: Slot::body(rtag),
                tag: member.tag,
            });
            self.emit(Op::JmpifNot {
                cond: Slot::body(dst),
                target: false_lbl,
            });
            if member.fields.is_empty() {
                let truth = self.pool(Constant::Bool(true));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: truth,
                });
                self.emit(Op::Jmp { target: done });
            } else {
                // the whole payload span compares field by field, the
                // same per-word chain a bare struct's equality rides —
                // the compares borrow, and both of the member's exits
                // release the payload counts from the fork's own state,
                // so a fail-fast field mismatch cannot strand a later
                // field's count
                let l = member
                    .fields
                    .iter()
                    .flat_map(|field| field_addrs(lhs.place.base, field))
                    .collect::<Vec<_>>();
                let r = member
                    .fields
                    .iter()
                    .flat_map(|field| field_addrs(rhs.place.base, field))
                    .collect::<Vec<_>>();
                let plan = self.payload_word_plan(member.atom)?;
                let member_fail = self.label();
                let fork = self.rc.save();
                self.emit_word_compares(&l, &r, &plan, member_fail)?;
                self.release_eq_words(&l, &r);
                self.emit(Op::Jmp { target: done });
                self.bind_label(member_fail);
                self.rc.rewind(fork);
                self.release_eq_words(&l, &r);
                self.emit(Op::Jmp { target: false_lbl });
            }
            self.bind_label(next);
        }
        self.bind_label(false_lbl);
        let lie = self.pool(Constant::Bool(false));
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        self.bind_label(done);
        Ok(dst)
    }

    pub(super) fn unary(
        &mut self,
        op: UnaryOp,
        expr: &Expr,
        span: Span,
        ty: Ty,
    ) -> Result<Place, LowerError> {
        // `-9223372036854775808` writes i64::MIN: the checker folds the
        // sign into the literal's range check, and the exact min
        // magnitude lowers as the min constant (every other negative
        // keeps the `0 - a` lowering below).
        if op == UnaryOp::Neg
            && let Expr::Literal {
                lit: Literal::Int { suffix, text },
                ..
            } = expr
            && suffix.is_empty()
            && negated_int_min(text)
        {
            let dst = self.temp_word(SlotType::I64)?;
            let kidx = self.pool(Constant::I64(i64::MIN));
            self.emit(Op::Kimm {
                dst: Slot::body(dst),
                kidx,
            });
            return Ok(Place {
                base: dst,
                shape: PlaceShape::Word(SlotType::I64),
            });
        }
        let operand = self.value(expr)?;
        let addrs = self.word_addrs(&operand.place, operand.ty)?;
        let [only] = addrs.as_slice() else {
            return Err(mismatch("a unary operand"));
        };
        let place = match op {
            UnaryOp::Neg if only.slot_ty == SlotType::I64 => {
                let zero = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::body(zero),
                    kidx,
                });
                let dst = self.temp_word(SlotType::I64)?;
                self.emit(Op::Data {
                    op: DataOp::SubI64,
                    dst: Slot::body(dst),
                    lhs: Slot::body(zero),
                    rhs: Slot::body(only.slot),
                });
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                }
            },
            UnaryOp::Not if only.slot_ty == SlotType::Bool => {
                let lie = self.temp_word(SlotType::Bool)?;
                let kidx = self.pool(Constant::Bool(false));
                self.emit(Op::Kconst {
                    dst: Slot::body(lie),
                    kidx,
                });
                let dst = self.temp_word(SlotType::Bool)?;
                self.emit(Op::Data {
                    op: DataOp::EqBool,
                    dst: Slot::body(dst),
                    lhs: Slot::body(only.slot),
                    rhs: Slot::body(lie),
                });
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Bool),
                }
            },
            UnaryOp::Neg | UnaryOp::Not => {
                return Err(LowerError::unsupported(
                    span,
                    "a unary operation",
                    "only int negation and boolean not are in the M3 contract",
                ))
            },
            UnaryOp::WrapNeg if only.slot_ty == SlotType::I64 => {
                // `-%a` lowers as `0 -% a`: the KIMM zero mirrors int
                // negation and the wrapping sub never traps, so negating
                // int-min wraps to int-min (never traps)
                let zero = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::body(zero),
                    kidx,
                });
                let dst = self.temp_word(SlotType::I64)?;
                self.emit(Op::Data {
                    op: DataOp::WrapSubI64,
                    dst: Slot::body(dst),
                    lhs: Slot::body(zero),
                    rhs: Slot::body(only.slot),
                });
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                }
            },
            UnaryOp::WrapNeg => {
                return Err(LowerError::unsupported(
                    span,
                    "a wrapping negation",
                    "wrapping operators are integer-only; floats are IEEE [B7]",
                ))
            },
        };
        let _ = ty;
        Ok(place)
    }

    // ---- finalization ---------------------------------------------------------
}

/// The named refusal a list word's element gate answers: the element
/// kind's own plan found a word without content equality, so the chain
/// refuses naming the seam instead of reading the list's Ref word through
/// the EqStr uop (a runtime trap over a non-string object).
pub(super) fn list_refusal() -> LowerError {
    LowerError::unsupported(
        no_span(),
        "a structural equality over list elements",
        "the content compare covers int, float, bool, string, bytes, struct, tuple, and nested \
         collection elements; callable and union elements have no content equality",
    )
}

/// The named refusal a box word answers in an equality plan: the chain
/// has no content compare over the container's word, and two handles
/// may share one held value, so the held values compare through their
/// own `get` reads.
pub(super) fn box_eq_refusal() -> LowerError {
    LowerError::unsupported(
        no_span(),
        "a structural equality over box values",
        "the chain compares held values through their own reads (`b.get()`), never the \
         container's word",
    )
}

/// The named refusal a map word's value gate answers, [`list_refusal`]'s
/// twin over the value channel.
pub(super) fn map_refusal() -> LowerError {
    LowerError::unsupported(
        no_span(),
        "a structural equality over map values",
        "the content compare covers int, float, bool, string, bytes, struct, tuple, and nested \
         collection values; callable and union values have no content equality",
    )
}

/// How one word of a structural `==` chain compares: the word's
/// own scalar uop, or — when the word's field type is a heap collection
/// — that collection's content-compare native, because the EqStr uop
/// reads string objects alone. The chain selects per field type, never
/// per slot kind, so a bytes-, list-, or map-typed struct, tuple, or
/// union field, and a membership element over any of them, dispatch the
/// content compare.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WordEq {
    /// The scalar word's equality uop.
    Scalar(DataOp),
    /// One bytes-typed word: the content compare through the native
    /// call.
    Bytes,
    /// One list-typed word: the content compare through the native call.
    List,
    /// One map-typed word: the content compare through the native call.
    Map,
}

/// The single identifier of a one-segment path expression, if it is one.
/// Whether one expression is an arithmetic binary at its outermost
/// node: the only shape that can take a scalar destination offer, so
/// the offer never survives into branch arms or nested calls, which own
/// other words.
pub(super) fn arith_shape(expr: &Expr) -> bool {
    matches!(peel_parens(expr), Expr::Binary {
        op: BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::WrapAdd
            | BinaryOp::WrapSub
            | BinaryOp::WrapMul
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitXor
            | BinaryOp::BitOr,
        ..
    })
}

/// Whether one argument's evaluation provably touches no overlay word
/// and mutates nothing: plain paths, literals, and field projections
/// over such expressions. A directly staged overlay word may be written
/// only while every later argument stays one of these.
pub(super) fn trivially_staged(expr: &Expr) -> bool {
    match peel_parens(expr) {
        Expr::Literal { .. } | Expr::Path { .. } => true,
        Expr::Field { recv, .. } => trivially_staged(recv),
        _ => false,
    }
}
