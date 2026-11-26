//! The expression walk: every node's value, the literal arm with its
//! comptime embeddings, and the scalar constant vocabulary (a literal's
//! word, its destination).

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    pub(super) fn value(&mut self, e: &Expr) -> Result<TypedPlace, LowerError> {
        let enclosing = std::mem::replace(&mut self.cur_span, e.span());
        let out = self.value_node(e).map_err(|err| err.respanned(e.span()));
        self.cur_span = enclosing;
        out
    }

    /// The expression walk. Type-keyed refusals (shapes, word
    /// addressing) observe no node, so [`fn value`](Self::value)
    /// re-spans them at the expression whose lowering hit them.
    pub(super) fn value_node(&mut self, e: &Expr) -> Result<TypedPlace, LowerError> {
        let val = self.val(e.span())?;
        let place =
            match e {
                Expr::Literal { lit, .. } => self.literal(lit, e.span())?,
                Expr::Str { parts, .. } => self.string_expr(parts)?,
                Expr::RawStr { text, .. } => {
                    let dst = self.temp_word(SlotType::Ref)?;
                    let kidx = self.pool_str(text);
                    self.kstr(dst, kidx);
                    Place {
                        base: dst,
                        shape: PlaceShape::Word(SlotType::Ref),
                    }
                },
                Expr::List { .. } => self.list_literal(e, val.ty)?,
                Expr::Path { path, .. } => self.path_value(path, e.span(), val.ty)?,
                Expr::Paren { inner, .. } => return self.value(inner),
                Expr::Block(block) => return self.block_value(block),
                Expr::If { .. } => self.if_value(e, val.ty)?,
                Expr::Match { .. } => self.match_value(e, val.ty)?,
                Expr::Struct { .. } => self.struct_literal(e, val.ty)?,
                Expr::Call { .. } => self.call_value(e, val.ty)?,
                Expr::Field { .. } | Expr::FieldKey { .. } => self.field_access(e, val.ty)?,
                Expr::Try { .. } => self.try_value(e, val.ty)?,
                Expr::Unary { op, expr, span } => self.unary(*op, expr, *span, val.ty)?,
                Expr::Binary { .. } => self.binary_value(e, val.ty)?,
                Expr::Is { expr, ty, span } => self.is_value(expr, ty, *span)?,
                Expr::Assign { .. } => self.assign_value(e)?,
                Expr::While { .. } => self.while_value(e)?,
                Expr::Return { .. } => return self.return_value(e),
                Expr::For { comptime: true, .. } => self.for_comptime_value(e)?,
                Expr::For { .. } => self.for_value(e)?,
                Expr::Break(span) => self.break_value(*span)?,
                Expr::Continue(span) => self.continue_value(*span)?,
                // a conversion placeholder never reaches lowering through the
                // pipeline (a tree holding one never leaves `parse`); refuse
                // rather than emit code for a rejected shape
                Expr::Invalid(span) => {
                    return Err(LowerError::unsupported(
                        *span,
                        "an expression",
                        "it was rejected at conversion",
                    ))
                },
                Expr::Nursery { .. } => self.nursery_value(e)?,
                Expr::Spawn { .. } => self.spawn_value(e)?,
                Expr::Defer { .. } => {
                    return Err(LowerError::unsupported(
                        e.span(),
                        "a control statement",
                        "defer statements have no lowering yet",
                    ))
                },
                Expr::Lambda { .. } => self.lambda_value(e)?,
                Expr::Tuple { items, span, .. } => {
                    if items.is_empty() {
                        self.unit_value(*span)?.place
                    } else {
                        self.tuple_literal(items, val.ty, *span)?
                    }
                },
                Expr::MapApply { .. } => {
                    return Err(LowerError::unsupported(
                        e.span(),
                        "a map type application",
                        "the application `map[k, v]` constructs only in call position — `map[k, \
                         v]()`",
                    ))
                },
                Expr::FnType { .. } => {
                    return Err(LowerError::unsupported(
                        e.span(),
                        "a fn type in value position",
                        "a fn type is a type spelling for the constructor brackets — \
                         `list[fn(int) -> int]()`",
                    ))
                },
                Expr::Index { recv, index, .. } => {
                    self.index_read_value(recv, index, val.ty)?.place
                },
                Expr::Range { .. } => {
                    return Err(
                        LowerError::unsupported(
                            e.span(),
                            "a range expression",
                            "a range lowers in the `for` loop's own staging; no range value \
                             exists in                      the record",
                        ),
                    )
                },
            };
        Ok(TypedPlace { place, ty: val.ty })
    }

    pub(super) fn literal(&mut self, lit: &Literal, span: Span) -> Result<Place, LowerError> {
        match lit {
            Literal::Int { suffix, text } => {
                if !suffix.is_empty() {
                    return Err(LowerError::unsupported(
                        span,
                        "a suffixed integer literal",
                        "explicit widths are not in the M3 contract",
                    ));
                }
                let value = parse_int(text).ok_or_else(|| {
                    LowerError::unsupported(span, "an integer literal", "it does not parse as i64")
                })?;
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(value));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                })
            },
            Literal::Float { suffix, text } => {
                if !suffix.is_empty() {
                    return Err(LowerError::unsupported(
                        span,
                        "a suffixed float literal",
                        "explicit widths are not in the M3 contract",
                    ));
                }
                let Ok(value) = text.parse::<f64>() else {
                    return Err(LowerError::unsupported(
                        span,
                        "a float literal",
                        "it does not parse as f64",
                    ));
                };
                let dst = self.temp_word(SlotType::F64)?;
                let kidx = self.pool(Constant::F64(value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::F64),
                })
            },
            Literal::Bool(value) => {
                let dst = self.temp_word(SlotType::Bool)?;
                let kidx = self.pool(Constant::Bool(*value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Bool),
                })
            },
            Literal::Char(c) => {
                // a char stages as its code point: the atom's own scalar
                // word rides the integer slot, the text spelling encoding
                // UTF-8 at the conversion seam
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(i64::from(u32::from(*c))));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                })
            },
            Literal::Byte(b) => {
                // A byte stages as its 0-255 integer value: the atom
                // is its own u8 width class, but the word rides the
                // integer slot encoding — the checker keeps byte outside
                // the int conversion family, so no arithmetic reaches here
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(i64::from(*b)));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                })
            },
        }
    }

    /// One linked const's value fact as a place: the literal embeds at the
    /// use site over the same word shapes and RC treatment the owning
    /// module's own literal lowering gives it — a string or char through
    /// the produced-Kstr channel (an arena temporary), a list through the
    /// `list_new`/`list_push` seam a local list literal lowers through
    /// (the element type read immutably from the use's checked type,
    /// `ty`), everything else a scalar constant word.
    pub(super) fn embed_const_fact(
        &mut self,
        fact: ConstFact,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let text = match fact {
            ConstFact::Int(value) => {
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(value));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                });
            },
            ConstFact::Float(value) => {
                let dst = self.temp_word(SlotType::F64)?;
                let kidx = self.pool(Constant::F64(value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::F64),
                });
            },
            ConstFact::Bool(value) => {
                let dst = self.temp_word(SlotType::Bool)?;
                let kidx = self.pool(Constant::Bool(value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Bool),
                });
            },
            ConstFact::Byte(value) => {
                // a byte stages as its 0-255 integer value, the literal
                // lowering's own convention
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(i64::from(value)));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                });
            },
            ConstFact::Char(c) => {
                // a char stages as its code point, the literal lowering's
                // own convention beside the byte's
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(i64::from(u32::from(c))));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                });
            },
            ConstFact::Str(text) => text,
            ConstFact::List(items) => return self.embed_list_fact(items, ty, span),
        };
        let dst = self.temp_word(SlotType::Ref)?;
        let kidx = self.pool_str(&text);
        self.kstr(dst, kidx);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// One linked list-const's elements onto a fresh list word: the same
    /// construction a local list literal lowers through — `list_new`,
    /// then each element's literal embed staged through `list_push`'s
    /// overlay — so the RC discipline (the frame-owned list borrowing
    /// itself per push, each Ref element staged for the native's own
    /// count) is the local spelling's.
    pub(super) fn embed_list_fact(
        &mut self,
        items: Vec<ConstFact>,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let elem = self.shapes.list_element(ty).ok_or_else(|| {
            LowerError::unsupported(
                span,
                "a linked list const",
                "the use's checked type is not the applied list constructor the fact's elements \
                 stage over",
            )
        })?;
        let list = self.list_new_word()?;
        self.list_push_each(list, items, |this, fact| {
            let place = this.embed_const_fact(fact, elem, span)?;
            Ok((TypedPlace { place, ty: elem }, span))
        })?;
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }
}

pub(super) fn int_radix(text: &str) -> (u32, &str) {
    if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        (16, rest)
    } else if let Some(rest) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        (2, rest)
    } else if let Some(rest) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        (8, rest)
    } else {
        (10, text)
    }
}

pub(super) fn parse_int(text: &str) -> Option<i64> {
    let (radix, digits) = int_radix(text);
    i64::from_str_radix(digits, radix).ok()
}

/// Whether one integer literal's magnitude is exactly i64::MIN's: the
/// sign-folded range check admits the negation as the min value, while
/// the magnitude alone sits outside the positive range, so the direct
/// `-` over the literal lowers as the min constant at the unary seam.
pub(super) fn negated_int_min(text: &str) -> bool {
    let (radix, digits) = int_radix(text);
    u64::from_str_radix(digits, radix).is_ok_and(|m| m == 1u64 << 63)
}

/// The constant one scalar literal writes: the direct-store fast paths
/// (a scalar var's literal init, a literal assignment over a scalar
/// var) load it straight into the destination word.
#[derive(Clone, Copy)]
pub(super) enum ScalarConst {
    I64(i64),
    F64(f64),
}

/// One scalar word an expression evaluation is offered as the
/// destination of its outermost data op: an assignment's var word, or
/// the overlay staging word a call site reserves for one argument.
/// `emit_data` always allocates a fresh temp for its result, so the
/// offered word is born at the consuming op with exactly one consumer
/// — the store that reads it back, or the CALL whose overlay slot
/// holds it — which is the single-use proof the direct write rides
/// on; a shape that does not take the offer keeps the temp spelling.
#[derive(Clone, Copy)]
pub(super) enum ScalarDst {
    /// A scalar var word an assignment's plain store writes.
    Var { word: u8, slot: SlotType },
    /// An overlay staging word a call's argument writes.
    Arg { index: u8, slot: SlotType },
}

impl ScalarDst {
    /// The word index the consuming op's produced place reports.
    pub(super) fn word(self) -> u8 {
        match self {
            Self::Var { word, .. } | Self::Arg { index: word, .. } => word,
        }
    }

    /// The word kind the destination carries.
    pub(super) fn slot(self) -> SlotType {
        match self {
            Self::Var { slot, .. } | Self::Arg { slot, .. } => slot,
        }
    }

    /// The symbolic slot the consuming data op writes.
    pub(super) fn slot_word(self) -> Slot {
        match self {
            Self::Var { word, .. } => Slot::body(word),
            Self::Arg { index, .. } => Slot::overlay(index),
        }
    }
}

/// Probes one literal against a scalar word's slot: suffixed or
/// unparseable spellings and kind disagreements answer None, so the
/// fast paths decline and the temp path (`literal`) owns the refusals.
pub(super) fn scalar_const(lit: &Literal, slot: SlotType) -> Option<ScalarConst> {
    match (lit, slot) {
        (Literal::Int { suffix, text }, SlotType::I64) if suffix.is_empty() => {
            Some(ScalarConst::I64(parse_int(text)?))
        },
        (Literal::Float { suffix, text }, SlotType::F64) if suffix.is_empty() => {
            Some(ScalarConst::F64(text.parse::<f64>().ok()?))
        },
        (Literal::Byte(b), SlotType::I64) => Some(ScalarConst::I64(i64::from(*b))),
        _ => None,
    }
}
