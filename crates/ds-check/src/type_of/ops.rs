//! The F5 operator table, checked over solved operands (B7): trapping
//! defaults need no checker action (the VM traps); wrapping forms are
//! integer-only; mixed int/float arithmetic is an error suggesting the
//! explicit conversion; `==` is B9's structural equality with its
//! carve-outs. The numeric vocabulary is data — a name-to-class table over
//! declared atoms, never builtin identity.

use super::*;

impl Bodies<'_> {
    pub(crate) fn unary(&mut self, op: UnaryOp, operand: &Expr, span: Span) -> Val {
        let val = self.expr(operand, None);
        // A poisoned operand already reported its root cause; the
        // operator's own shape complaint would be a cascade.
        if val.poisoned {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        }
        let class = numeric_class(&self.decls.universe, val.ty);
        match op {
            UnaryOp::Neg => {
                if class.is_some() {
                    Val {
                        ty: val.ty,
                        diverges: false,
                        poisoned: false,
                    }
                } else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`-` needs a numeric operand, `{}` given [B7]",
                            self.decls.universe.render(val.ty)
                        ),
                        span,
                        "not numeric",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                }
            },
            UnaryOp::WrapNeg => match class {
                Some((NumericClass::Int, _)) => Val {
                    ty: val.ty,
                    diverges: false,
                    poisoned: false,
                },
                Some((NumericClass::Float, _)) => {
                    self.error(
                        ErrorCode::TypeMismatch,
                        "`-%` wraps integers; floats negate with `-` [B7]".to_owned(),
                        span,
                        "wrapping negate on a float",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                },
                None => {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`-%` needs an integer operand, `{}` given [B7]",
                            self.decls.universe.render(val.ty)
                        ),
                        span,
                        "not numeric",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                },
            },
            UnaryOp::Not => {
                let bool_ty = self.bool_ty();
                if self.decls.universe.contains(bool_ty, val.ty) {
                    Val {
                        ty: bool_ty,
                        diverges: false,
                        poisoned: false,
                    }
                } else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`!` needs `bool`, `{}` given",
                            self.decls.universe.render(val.ty)
                        ),
                        span,
                        "not a boolean",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                }
            },
        }
    }

    pub(crate) fn binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr, span: Span) -> Val {
        // `x in a..b` / `x in a..=b`: the range fixes the element type
        // instead of a collection's [F7].
        if op == BinaryOp::In
            && let Expr::Range { start, end, .. } = peel_paren(right)
        {
            return self.in_range(left, start, end, span);
        }
        let left_val = self.expr(left, None);
        // `a && b`: b sees a's narrowing [B2] — `x is int && x >= 1` types
        // the comparison against the narrowed x.
        let right_val = if op == BinaryOp::And {
            let narrows = self.harvest(left);
            let saved = self.snapshot();
            for narrow in &narrows {
                self.apply_narrow(narrow);
            }
            let val = self.expr(right, None);
            self.restore(saved);
            val
        } else {
            self.expr(right, None)
        };
        self.binary_val(
            op,
            left_val.ty,
            right_val.ty,
            left.span(),
            right.span(),
            span,
            left_val.poisoned || right_val.poisoned,
        )
    }

    /// The F5 operator table, checked over solved operands: trapping
    /// defaults need no checker action (the VM traps (B7)); wrapping forms
    /// are integer-only; mixed int/float arithmetic is an error suggesting
    /// the explicit conversion; `==` is B9's structural equality with its
    /// carve-outs. A poisoned operand (its subtree already reported)
    /// suppresses the operator's own complaint — the root cause stands
    /// alone.
    pub(crate) fn binary_val(
        &mut self,
        op: BinaryOp,
        left: Ty,
        right: Ty,
        left_span: Span,
        right_span: Span,
        span: Span,
        poisoned: bool,
    ) -> Val {
        use BinaryOp::{
            Add, And, BitAnd, BitOr, BitXor, Div, Eq, Ge, Gt, In, Le, Lt, Mul, Ne, Or, Rem, Shl,
            Shr, Sub, WrapAdd, WrapMul, WrapSub,
        };
        if poisoned {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        }
        let l = numeric_class(&self.decls.universe, left).map(|(c, n)| (c, n.to_owned()));
        let r = numeric_class(&self.decls.universe, right).map(|(c, n)| (c, n.to_owned()));
        let same =
            self.decls.universe.contains(left, right) && self.decls.universe.contains(right, left);
        let bool_ty = self.bool_ty();
        match op {
            Add | Sub | Mul | WrapAdd | WrapSub | WrapMul => {
                let wrapping = matches!(op, WrapAdd | WrapSub | WrapMul);
                if op == Add && same && self.is_string_ty(left) {
                    return Val {
                        ty: left,
                        diverges: false,
                        poisoned: false,
                    }; // string concat
                }
                match (&l, &r) {
                    (Some((NumericClass::Int, ln)), Some((NumericClass::Int, rn))) => {
                        if ln == rn {
                            Val {
                                ty: left,
                                diverges: false,
                                poisoned: false,
                            }
                        } else {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "mixed integer widths `{ln}` and `{rn}`; conversions are \
                                     explicit [B7]: `{ln}(x)`"
                                ),
                                span,
                                "width mismatch",
                            );
                            Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: false,
                            }
                        }
                    },
                    (Some((NumericClass::Float, _)), Some((NumericClass::Float, _)))
                        if !wrapping =>
                    {
                        Val {
                            ty: left,
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    (Some((NumericClass::Float, _)), Some((NumericClass::Float, _))) => {
                        self.error(
                            ErrorCode::TypeMismatch,
                            "wrapping operators are integer-only; floats are IEEE [B7]".to_owned(),
                            span,
                            "wrapping operator on floats",
                        );
                        Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    (Some((lc, ln)), Some((_rc, rn))) => {
                        // Mixed int/float [B7]: suggest converting the
                        // integer side to the float type.
                        let (int_span, int_name) = if matches!(lc, NumericClass::Int) {
                            (left_span, ln.clone())
                        } else {
                            (right_span, rn.clone())
                        };
                        let float_name =
                            if matches!(lc, NumericClass::Float) { ln.clone() } else { rn.clone() };
                        let source = self.span_text(int_span);
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "mixed numeric operands `{}` and `{}`; conversions are \
                                     explicit [B7]",
                                    self.decls.universe.render(left),
                                    self.decls.universe.render(right)
                                ),
                            )
                            .with_primary(span, "mixed int and float")
                            .with_suggestion(ds_diag::Suggestion::Replace {
                                span: int_span,
                                with: format!("{float_name}({source})"),
                            })
                            .with_note(format!(
                                "the `{int_name}` side converts with `{float_name}(...)`"
                            )),
                        );
                        Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    _ => {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "arithmetic needs numeric operands, `{}` and `{}` given [B7]",
                                self.decls.universe.render(left),
                                self.decls.universe.render(right)
                            ),
                            span,
                            "not numeric",
                        );
                        Val {
                            ty: self.empty(),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                }
            },
            Div | Rem | Shl | Shr | BitAnd | BitOr | BitXor => {
                let integer_only = !matches!(op, Div | Rem);
                let class_ok = |c: &Option<(NumericClass, String)>| match c {
                    Some((NumericClass::Int, _)) => true,
                    Some((NumericClass::Float, _)) => !integer_only,
                    None => false,
                };
                if class_ok(&l) && class_ok(&r) && same {
                    Val {
                        ty: left,
                        diverges: false,
                        poisoned: false,
                    }
                } else if !class_ok(&l) || !class_ok(&r) {
                    let what = if integer_only { "integers" } else { "numbers" };
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`{}` needs {}, `{}` and `{}` given [B7]",
                            op_text(op),
                            what,
                            self.decls.universe.render(left),
                            self.decls.universe.render(right)
                        ),
                        span,
                        "not numeric",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                } else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        "one numeric type on both sides; conversions are explicit [B7]".to_owned(),
                        span,
                        "mixed operands",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                }
            },
            Eq | Ne => self.equality_val(left, right, span),
            Lt | Le | Gt | Ge => {
                let orderable = matches!((&l, &r), (Some(_), Some(_))) && same;
                if orderable {
                    Val {
                        ty: bool_ty,
                        diverges: false,
                        poisoned: false,
                    }
                } else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "ordering needs one numeric type on both sides (`{}` vs `{}`); `ord` \
                             derives arrive at M6 [B9, F5]",
                            self.decls.universe.render(left),
                            self.decls.universe.render(right)
                        ),
                        span,
                        "not orderable",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    }
                }
            },
            In => {
                // Only non-range right sides reach here (`binary` routes
                // `x in a..b` first). Membership over one list
                // application is ruled [F7]: the operand carries the
                // element type and the result is bool; every other
                // right side keeps the honest refusal until its ruling.
                if let Some(element) = self.list_element(right) {
                    if !self.decls.universe.contains(element, left) {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "list membership needs the list's element type `{}`, `{}` given",
                                self.decls.universe.render(element),
                                self.decls.universe.render(left)
                            ),
                            span,
                            "operand mismatch",
                        );
                    }
                    return Val {
                        ty: bool_ty,
                        diverges: false,
                        poisoned: false,
                    };
                }
                self.error(
                    ErrorCode::NonRangeIn,
                    "`in` runs over ranges (`x in a..b`) and lists (`x in xs`); membership over \
                     other collections is not ruled yet"
                        .to_owned(),
                    span,
                    "not a range or list",
                );
                Val {
                    ty: bool_ty,
                    diverges: false,
                    poisoned: false,
                }
            },
            And | Or => {
                for (ty, at) in [(left, left_span), (right, right_span)] {
                    if !self.decls.universe.contains(bool_ty, ty) {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "logical operators need `bool`, `{}` given",
                                self.decls.universe.render(ty)
                            ),
                            at,
                            "not a boolean",
                        );
                    }
                }
                Val {
                    ty: bool_ty,
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// Structural equality over all data (B9); carve-outs: (1) function
    /// comparison rejected statically; (2) disjoint operands make the
    /// comparison constant, rejected as dead.
    fn equality_val(&mut self, left: Ty, right: Ty, span: Span) -> Val {
        if self.fn_typed(left) || self.fn_typed(right) {
            self.error(
                ErrorCode::TypeMismatch,
                "function comparison is rejected [B9 carve-out 1]".to_owned(),
                span,
                "function operand",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        if self.decls.universe.is_disjoint(left, right) {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` and `{}` share no member; `==` would be constant [B9]",
                    self.decls.universe.render(left),
                    self.decls.universe.render(right)
                ),
                span,
                "always unequal",
            );
        }
        Val {
            ty: self.bool_ty(),
            diverges: false,
            poisoned: false,
        }
    }

    /// Types one range's endpoints and returns the element type both
    /// agree on: integers of one width. Mixed widths name the
    /// conversion; a float or non-numeric endpoint is rejected (B7). A
    /// poisoned endpoint already reported and stays quiet.
    pub(crate) fn range_element(&mut self, start: &Expr, end: &Expr, span: Span) -> Option<Ty> {
        let start_val = self.expr(start, None);
        let end_val = self.expr(end, None);
        if start_val.poisoned || end_val.poisoned {
            return None;
        }
        match (
            numeric_class(&self.decls.universe, start_val.ty),
            numeric_class(&self.decls.universe, end_val.ty),
        ) {
            (Some((NumericClass::Int, a)), Some((NumericClass::Int, b))) => {
                if a == b {
                    return Some(start_val.ty);
                }
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "mixed integer widths `{a}` and `{b}`; conversions are explicit [B7]: \
                         `{a}(x)`"
                    ),
                    span,
                    "width mismatch",
                );
            },
            _ => {
                let s = self.decls.universe.render(start_val.ty);
                let e = self.decls.universe.render(end_val.ty);
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "range endpoints are integers, `{s}` and `{e}` given; conversions are \
                         explicit [B7]"
                    ),
                    span,
                    "not integer endpoints",
                );
            },
        }
        None
    }

    /// The element type of one whole-list application [F7]: membership
    /// reads it. `None` for any other type — a union of lists, a
    /// non-list application, a non-application.
    fn list_element(&mut self, ty: Ty) -> Option<Ty> {
        let (ctor, args) = self.single_apply(ty)?;
        let [element] = args.as_slice() else {
            return None
        };
        let def = self.package.lookup_type("list")?;
        (self.decls.defs_of_atoms.get(&ctor) == Some(&def)).then_some(*element)
    }

    /// Range membership (F7): the operand must carry the range's integer
    /// type; the result is bool.
    fn in_range(&mut self, left: &Expr, start: &Expr, end: &Expr, span: Span) -> Val {
        let bool_ty = self.bool_ty();
        let Some(element) = self.range_element(start, end, span) else {
            let _ = self.expr(left, None);
            return Val {
                ty: bool_ty,
                diverges: false,
                poisoned: false,
            };
        };
        let left_val = self.expr(left, None);
        let element_name = numeric_class(&self.decls.universe, element)
            .filter(|(class, _)| *class == NumericClass::Int)
            .map(|(_, name)| name.to_owned());
        let ok = numeric_class(&self.decls.universe, left_val.ty)
            .zip(element_name.as_deref())
            .is_some_and(|((lclass, lname), ename)| lclass == NumericClass::Int && lname == ename);
        if !ok {
            let rendered = self.decls.universe.render(left_val.ty);
            let suggestion = element_name
                .map(|name| format!(": `{name}(x)`"))
                .unwrap_or_default();
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "range membership needs the range's integer type, `{rendered}` given; \
                     conversions are explicit [B7]{suggestion}"
                ),
                span,
                "operand mismatch",
            );
        }
        Val {
            ty: bool_ty,
            diverges: false,
            poisoned: false,
        }
    }
}

pub(crate) fn compound_op(op: ds_parse::AssignOp) -> BinaryOp {
    use ds_parse::AssignOp::{Add, Div, Mul, Plain, Rem, Shl, Shr, Sub};
    match op {
        Add | Plain => BinaryOp::Add,
        Sub => BinaryOp::Sub,
        Mul => BinaryOp::Mul,
        Div => BinaryOp::Div,
        Rem => BinaryOp::Rem,
        Shl => BinaryOp::Shl,
        Shr => BinaryOp::Shr,
    }
}

fn op_text(op: BinaryOp) -> &'static str {
    use BinaryOp::{Add, BitAnd, BitOr, BitXor, Div, Mul, Rem, Shl, Shr, Sub};
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Rem => "%",
        Shl => "<<",
        Shr => ">>",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        _ => "?",
    }
}

/// B7 numeric classes, modeled as data: a declared atom name to its class.
/// The checker never hardcodes a type identity — a numeric type is a
/// declared atom whose name sits in this table.
const NUMERIC_TABLE: &[(&str, NumericClass)] = &[
    ("int", NumericClass::Int),
    ("i8", NumericClass::Int),
    ("i16", NumericClass::Int),
    ("i32", NumericClass::Int),
    ("i64", NumericClass::Int),
    ("u8", NumericClass::Int),
    ("u16", NumericClass::Int),
    ("u32", NumericClass::Int),
    ("u64", NumericClass::Int),
    ("float", NumericClass::Float),
    ("f32", NumericClass::Float),
    ("f64", NumericClass::Float),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NumericClass {
    Int,
    Float,
}

fn numeric_class(
    universe: &crate::universe::Universe,
    ty: Ty,
) -> Option<(NumericClass, &'static str)> {
    let members = universe.members(ty);
    let [only] = members else { return None };
    let name = universe.atom_decl(*only).ok()?.name.as_str();
    NUMERIC_TABLE
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(entry, class)| (*class, *entry))
}
