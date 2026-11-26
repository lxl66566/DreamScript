//! Text: the `str` interpolation walk, its hole words, and the show
//! dispatch each hole routes through.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// One interpolating string: a STRCAT chain over its parts; a hole
    /// is `str(e)` — printable primitives route through their str
    /// natives, a string passes its Ref word through, and an
    /// operand whose `show` the checker solved at the hole's own span
    /// dispatches through the ordinary member-call machinery.
    pub(super) fn string_expr(&mut self, parts: &[StrPart]) -> Result<Place, LowerError> {
        let mut acc: Option<u8> = None;
        for part in parts {
            let word = match part {
                StrPart::Text { text, .. } => {
                    let dst = self.temp_word(SlotType::Ref)?;
                    let kidx = self.pool_str(text);
                    self.kstr(dst, kidx);
                    dst
                },
                StrPart::Expr { span, expr } => {
                    // A derive-synthesized body's non-printable part hole
                    // mints one span shared by the hole and the written
                    // `.show()` call it wraps: that record belongs to the
                    // call, so the expression lowers as written, not as an
                    // implicit dispatch over its own result
                    let written_show = matches!(
                        &**expr,
                        Expr::Call {
                            span: call_span,
                            callee,
                            ..
                        } if call_span == span
                            && matches!(
                                &**callee,
                                Expr::Field { name, .. } if name.name == "show"
                            )
                    );
                    if !written_show && self.hole_show_dispatch(*span) {
                        self.show_dispatch_word(*span, (**expr).clone())?
                    } else {
                        let val = self.value(expr)?;
                        self.hole_word(expr.span(), &val)?
                    }
                },
                StrPart::Ident { span, name } => {
                    if self.hole_show_dispatch(*span) {
                        let recv = Expr::Path {
                            span: name.span,
                            path: Path {
                                span: name.span,
                                segments: vec![name.clone()],
                            },
                        };
                        self.show_dispatch_word(*span, recv)?
                    } else {
                        let ty = self.val(name.span)?.ty;
                        let path = Path {
                            span: name.span,
                            segments: vec![name.clone()],
                        };
                        let place = self.path_value(&path, name.span, ty)?;
                        self.hole_word(name.span, &TypedPlace { place, ty })?
                    }
                },
            };
            acc = Some(match acc {
                None => word,
                Some(left) => {
                    let dst = self.temp_word(SlotType::Ref)?;
                    self.emit(Op::Data {
                        op: DataOp::StrCat,
                        dst: Slot::body(dst),
                        lhs: Slot::body(left),
                        rhs: Slot::body(word),
                    });
                    // the operands are reads; the result is a fresh owner
                    self.rc.read(&mut self.code, &[left, word]);
                    self.rc.produced_strcat(dst);
                    dst
                },
            });
        }
        let base = if let Some(base) = acc {
            base
        } else {
            // the empty string literal: a plain constant, not a chain
            let dst = self.temp_word(SlotType::Ref)?;
            let kidx = self.pool_str("");
            self.kstr(dst, kidx);
            dst
        };
        Ok(Place {
            base,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// `str(e)` of one hole: the operand's checked type routes the
    /// conversion. Anything the checker did not prove printable lands as
    /// a lowering error (a `show`-carrying operand never reaches here —
    /// the checker's hole record routed it through the dispatch above).
    pub(super) fn hole_word(&mut self, span: Span, val: &TypedPlace) -> Result<u8, LowerError> {
        let members = self.members(val.ty);
        let [atom] = members else {
            return Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the operand's type is not a printable primitive and carries no solved `show` \
                 dispatch; report the construct",
            ));
        };
        let name = self
            .analyzed
            .decls
            .universe
            .atom_decl(*atom)
            .map_err(|_| mismatch("a printable atom"))?
            .name
            .clone();
        match name.as_str() {
            "string" => {
                let addrs = self.word_addrs(&val.place, val.ty)?;
                match addrs.as_slice() {
                    [only] if only.slot_ty == SlotType::Ref => Ok(only.slot),
                    _ => Err(LowerError::unsupported(
                        span,
                        "an interpolation hole",
                        "the string value does not occupy one reference word",
                    )),
                }
            },
            // a char encodes UTF-8 at the hole: the code point word rides
            // the integer slot, so the char conversion native renders the
            // text the interpolation spells
            "char" => self.str_native_word("str_char", SlotType::I64, val, span),
            // a byte prints its 0-255 value: the word IS the integer
            // slot staging, so the int native renders it; interpolation
            // holes stay checker-gated to the five printable primitives,
            // so only an explicit `str(b)` reaches this arm
            "int" | "byte" => self.str_native_word("str_int", SlotType::I64, val, span),
            "float" => self.str_native_word("str_float", SlotType::F64, val, span),
            "bool" => self.str_native_word("str_bool", SlotType::Bool, val, span),
            _ => Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the operand's type is not a printable primitive and carries no solved `show` \
                 dispatch; report the construct",
            )),
        }
    }

    /// Whether the checker solved one hole's `show` at its own span:
    /// the hole's dispatch record (the trait-dispatch or inherent table
    /// beside the solved instance, the runtime-evidence table alone for
    /// an assumption-answered hole) the synthetic member call below
    /// resolves through.
    pub(super) fn hole_show_dispatch(&self, span: Span) -> bool {
        self.hole_show_method(span).is_some()
            || self.analyzed.checked.trait_calls.contains_key(&span)
            || self.analyzed.checked.inherent_calls.contains_key(&span)
    }

    /// One hole's `show` dispatch: the receiver wrapped as the
    /// written `recv.show()` call — no surface name exists, so the
    /// checker's span-keyed record is the resolution — staging through
    /// the ordinary member-call machinery (receiver argument, evidence
    /// words, the CALLG dispatch) and answering the hole's one string
    /// word. The receiver expression is the hole's own operand, so it
    /// evaluates exactly once.
    pub(super) fn show_dispatch_word(&mut self, span: Span, recv: Expr) -> Result<u8, LowerError> {
        let call = Expr::Call {
            span,
            callee: Box::new(Expr::Field {
                span,
                recv: Box::new(recv),
                name: Ident {
                    span,
                    name: "show".to_owned(),
                },
            }),
            args: Vec::new(),
            trailing: None,
            selection: None,
        };
        let val = self.value(&call)?;
        let addrs = self.word_addrs(&val.place, val.ty)?;
        match addrs.as_slice() {
            [only] if only.slot_ty == SlotType::Ref => Ok(only.slot),
            _ => Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the `show` dispatch did not answer one reference word",
            )),
        }
    }

    /// One total str native call over a scalar hole word: stage the word
    /// onto the overlay, call, and land the fresh string on a body temp.
    pub(super) fn str_native_word(
        &mut self,
        native_name: &str,
        arg_slot: SlotType,
        val: &TypedPlace,
        span: Span,
    ) -> Result<u8, LowerError> {
        let addrs = self.word_addrs(&val.place, val.ty)?;
        let [only] = addrs.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the scalar value does not occupy one word",
            ));
        };
        if only.slot_ty != arg_slot {
            return Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the scalar value's word and the str native's parameter disagree",
            ));
        }
        let native = crate::native::lookup(native_name)
            .unwrap_or_else(|| panic!("the str native `{native_name}` is covered"));
        self.overlay_site(&[arg_slot], &[SlotType::Ref]);
        self.rc.native_open();
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(only.slot),
        });
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 1,
        });
        self.rc.native_done(&mut self.code);
        let dst = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(dst),
            src: Slot::overlay(0),
        });
        self.rc.produced(dst);
        Ok(dst)
    }
}
