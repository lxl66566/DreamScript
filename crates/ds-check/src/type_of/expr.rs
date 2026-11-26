//! The expression walk: one dispatcher over the expression forms, threading
//! the `expected` hint to the forms that consume it, plus the literal,
//! string, and hole checks it reaches.

use super::*;

impl Bodies<'_> {
    pub(super) fn expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        // Expression-error epoch: a diagnostic pushed anywhere inside this
        // expression poisons its value — every enclosing position consumes
        // it without re-reporting (the containment helpers and the
        // operand-shape checks), so a root cause stands alone instead of
        // cascading into "empty"/wrapper-type mismatches above it.
        let before = self.diags.len();
        let mut val = self.expr_inner(e, expected);
        val.poisoned |= self.diags.len() > before;
        self.record_val(e.span(), val);
        val
    }

    /// Refusal for a conversion placeholder: a tree holding one never
    /// leaves `parse`, so this arm only fires on a synthesized tree.
    pub(super) fn invalid_expr(&mut self, span: Span) -> Val {
        self.error(
            ErrorCode::InferenceFailed,
            "the expression was rejected at conversion".to_owned(),
            span,
            "rejected expression",
        );
        Val {
            ty: self.empty(),
            diverges: false,
            poisoned: true,
        }
    }

    pub(super) fn expr_inner(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        // The position this expression occupies (statement or
        // value-consumed) was set by the parent walk; every sub-expression
        // defaults back to value-consumed, and the frame-like bodies (a
        // lambda's, a loop's, a spawned fiber's) and the arm inheritance
        // below override it.
        let pos = self.value_pos;
        self.value_pos = true;
        match e {
            Expr::Literal { span, lit } => self.literal(*span, lit),
            Expr::Str { span, parts } => self.string_expr(*span, parts),
            Expr::RawStr { span, .. } => self.named_ty(*span, "string", "raw string"),
            Expr::Lambda { span, .. } => {
                // A copy's lambda mints per copy, a fact the mint walk's
                // span-once records do not carry yet.
                if !self.comptime_path.is_empty() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        "a lambda inside a `for comptime` copy queues with the walker round; its \
                         mint record is a per-copy fact"
                            .to_owned(),
                        *span,
                        "lambda inside a comptime copy",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: true,
                    };
                }
                self.lambda(e, expected, None)
            },
            Expr::Path { path, .. } => self.path_expr(path, expected),
            Expr::Struct { .. } => self.struct_expr(e, expected),
            Expr::Tuple {
                items,
                trailing_comma,
                ..
            } => self.tuple_expr(e, items, *trailing_comma, expected),
            Expr::List { .. } => self.list_expr(e, expected),
            Expr::Paren { inner, .. } => {
                self.value_pos = pos;
                self.expr(inner, expected)
            },
            Expr::Call { .. } => self.call(e),
            Expr::Field { recv, name, .. } => self.field_access(recv, name),
            Expr::FieldKey { span, recv, name } => self.keyed_field_access(*span, recv, name),
            Expr::Index { span, recv, index } => {
                if let Some(head) = single_path_ident(recv)
                    && self.package.lookup_type(&head.name).is_some()
                {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`{}` is a type; the application `{}[...]` constructs only in call \
                             position — `{name}[t]()` [F9, B3]",
                            head.name,
                            head.name,
                            name = head.name
                        ),
                        *span,
                        "type application outside a call",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    };
                }
                self.index_read(*span, recv, index)
            },
            Expr::MapApply { span, .. } => {
                self.error(
                    ErrorCode::TypeMismatch,
                    "`map` is a type; the application `map[...]` constructs only in call position \
                     — `map[k, v]()` [F9, B3]"
                        .to_owned(),
                    *span,
                    "type application outside a call",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            // A fn type is a type spelling, never a value; the constructor
            // brackets consume it before value typing, so this arm holds
            // the boundary for any other position.
            Expr::FnType { span, .. } => {
                self.error(
                    ErrorCode::TypeMismatch,
                    "a fn type is a type spelling, not a value; it belongs in a constructor's \
                     brackets — `list[fn(int) -> int]()` [F2, F9]"
                        .to_owned(),
                    *span,
                    "fn type outside a type application",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                }
            },
            Expr::Try { expr, span } => self.try_expr(expr, expected, *span),
            Expr::Unary { op, expr, span } => self.unary(*op, expr, *span),
            Expr::Is { expr, ty, span, .. } => self.is_expr(expr, ty, *span),
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => self.binary(*op, left, right, *span),
            Expr::Range { span, .. } => {
                self.error(
                    ErrorCode::InferenceFailed,
                    "a range is not a storable value; ranges are `for`-in and `in` sugar [F7]"
                        .to_owned(),
                    *span,
                    "range value",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
            },
            Expr::Assign {
                op,
                left,
                right,
                span,
            } => self.assign(*op, left, right, *span),
            Expr::If {
                cond, then, else_, ..
            } => self.if_expr(cond, then, else_.as_deref(), expected, pos),
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.match_expr(scrutinee, arms, *span, expected, pos),
            Expr::While { cond, body, .. } => self.while_expr(cond, body),
            Expr::For {
                pat,
                iter,
                body,
                span,
                comptime,
            } => self.for_expr(pat, iter, body, *span, *comptime),
            Expr::Return { expr, span } => {
                // The early exit is a statement: a block whose value
                // feeds a binding, an argument, or a join arm has no
                // lowering for it — the exit's answer is dead at the point
                // the consuming slot reads — so the position refuses
                // instead of trapping past a clean check.
                if pos {
                    self.error(
                        ErrorCode::ReturnInValuePosition,
                        "`return` is a statement; this position consumes the block's value, so \
                         the exit has no answer to feed it"
                            .to_owned(),
                        *span,
                        "return in a value position",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: true,
                        poisoned: true,
                    };
                }
                // The returned expression carries the declared return as
                // its expected type, matching the final-expression anchor:
                // a `?` operand splits against the return union here too.
                let value = match expr {
                    Some(expr) => self.expr(expr, self.ret),
                    None => Val {
                        ty: self.unit(),
                        diverges: false,
                        poisoned: false,
                    },
                };
                match self.ret {
                    Some(ret) => {
                        self.expect_contains(value, ret, *span, "return");
                    },
                    None => self.error(
                        ErrorCode::PropagationNotContained,
                        "`return` outside a function body".to_owned(),
                        *span,
                        "stray return",
                    ),
                }
                Val {
                    ty: self.empty(),
                    diverges: true,
                    poisoned: false,
                }
            },
            Expr::Break(span) | Expr::Continue(span) => {
                if self.loop_depth == 0 {
                    // The loop depth is frame-scoped: inside a lambda or
                    // spawned body it counts only that frame's own loops.
                    let (message, label) = if self.frame_depth > 0 {
                        (
                            "loop exits never cross a lambda or spawned-body frame boundary",
                            "frame-crossing loop exit",
                        )
                    } else {
                        ("loop exit outside a loop", "stray loop exit")
                    };
                    self.error(ErrorCode::InferenceFailed, message.to_owned(), *span, label);
                } else if matches!(e, Expr::Break(..)) {
                    // A break leaves its loop's value reachable, which a
                    // `while true` diverge decision below must see.
                    if let Some(count) = self.loop_breaks.last_mut() {
                        *count += 1;
                    }
                }
                Val {
                    ty: self.empty(),
                    diverges: true,
                    poisoned: false,
                }
            },
            // A conversion placeholder only ever exists beside recorded
            // conversion diagnostics, so `parse` never returns a tree
            // holding one; refusing keeps a synthesized tree from reading
            // as a loop exit.
            Expr::Invalid(span) => self.invalid_expr(*span),
            Expr::Nursery { body, span } => {
                if !self.comptime_path.is_empty() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        "a nursery inside a `for comptime` copy queues with the walker round"
                            .to_owned(),
                        *span,
                        "nursery inside a comptime copy",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: true,
                    };
                }
                self.nursery_block(body, *span)
            },
            // A spawned body runs as its own fiber: its loop exits stay
            // inside its frame, and its answer rides the join's
            // collected list.
            Expr::Spawn { body, span, .. } => {
                if !self.comptime_path.is_empty() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        "a spawn inside a `for comptime` copy queues with the walker round; its \
                         record is a per-copy fact"
                            .to_owned(),
                        *span,
                        "spawn inside a comptime copy",
                    );
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: true,
                    };
                }
                self.spawned_block(body)
            },
            Expr::Defer { body, .. } => {
                self.value_pos = false;
                self.block(body);
                Val {
                    ty: self.unit(),
                    diverges: false,
                    poisoned: false,
                }
            },
            // A bare block inherits its consumer's position: statement
            // blocks keep statement exits, consumed blocks refuse them in
            // their tail.
            Expr::Block(block) => {
                self.value_pos = pos;
                self.block_expected(block, expected)
            },
        }
    }

    /// A while loop: the condition types in condition position (an
    /// always-false `is` test refines legitimately there), the body in
    /// statement position inside its own loop frame.
    pub(super) fn while_expr(&mut self, cond: &Expr, body: &BlockExpr) -> Val {
        self.cond_depth += 1;
        let cond_val = self.expr(cond, None);
        self.cond_depth -= 1;
        self.expect_bool(cond_val, cond.span());
        let saved = self.snapshot();
        self.refined.clear();
        self.loop_depth += 1;
        self.loop_breaks.push(0);
        self.value_pos = false;
        let body_val = self.block(body);
        let breaks = self.loop_breaks.pop().unwrap_or(0);
        self.loop_depth -= 1;
        self.restore(saved);
        // A literal-`true` condition with no break in the body
        // never completes the loop normally, so the while keeps
        // the body's own diverge flag (`while true { return 1 }`
        // leaves through the return). Any other condition — or a
        // break — leaves the loop's unit value reachable, and a
        // `for` body's early return never diverges the `for`
        // itself: zero iterations complete it.
        Val {
            ty: self.unit(),
            diverges: is_true_expr(cond) && breaks == 0 && body_val.diverges,
            poisoned: false,
        }
    }

    /// A tuple literal: one element without a trailing comma is a
    /// parenthesized expression; the product type otherwise.
    pub(super) fn tuple_expr(
        &mut self,
        e: &Expr,
        items: &[Expr],
        trailing_comma: bool,
        expected: Option<Ty>,
    ) -> Val {
        if items.len() == 1 && !trailing_comma {
            return self.expr(&items[0], expected);
        }
        // Bidirectional against an expected tuple type: a
        // single-member expectation of the pattern's arity feeds
        // each element its expected type, so the
        // expected-consuming forms (the empty list literal, a
        // struct literal, a nested tuple, `?`) infer inside a
        // tuple the way they do in a return position.
        let expected_elems: Option<Vec<Ty>> =
            expected.and_then(|exp| self.expected_tuple_elems(exp, items.len()));
        let mut elements = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let item_expected = expected_elems
                .as_ref()
                .and_then(|elems| elems.get(index).copied());
            elements.push(self.expr(item, item_expected).ty);
        }
        let ty = match self.decls.universe.product(&elements) {
            Ok(ty) => ty,
            Err(err) => {
                // The distribution cap is a real rejection, not an
                // inference fallback: render DS2012 instead of
                // degrading the whole expression to the empty type.
                self.error(
                    ErrorCode::TupleDistributionCap,
                    err.to_string(),
                    e.span(),
                    "tuple distribution cap",
                );
                // Downstream empty-type fallbacks must not re-report
                // this rejection as an inference hole.
                self.solve_reported = true;
                self.empty()
            },
        };
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    pub(super) fn literal(&mut self, span: Span, lit: &Literal) -> Val {
        if let Literal::Int { suffix, text } = lit
            && suffix.is_empty()
            && int_literal_value(text, false).is_none()
        {
            self.error(
                ErrorCode::InvalidLiteral,
                format!("integer literal `{text}` does not fit the i64 range"),
                span,
                "out of range",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let (name, what) = match lit {
            Literal::Int { suffix, .. } => {
                if suffix.is_empty() {
                    ("int", "integer literal")
                } else {
                    (suffix.as_str(), "integer literal suffix")
                }
            },
            Literal::Float { suffix, .. } => {
                if suffix.is_empty() {
                    ("float", "float literal")
                } else {
                    (suffix.as_str(), "float literal suffix")
                }
            },
            Literal::Bool(_) => ("bool", "boolean literal"),
            Literal::Char(_) => ("char", "char literal"),
            Literal::Byte(_) => ("byte", "byte literal"),
        };
        self.named_ty(span, name, what)
    }

    /// The atom a literal names, by name, through the package universe
    /// The atom a literal names, by name, through the package universe:
    /// the numeric vocabulary is declared data, never builtin
    /// identity — inference never falls back.
    pub(super) fn named_ty(&mut self, span: Span, name: &str, what: &str) -> Val {
        if let Some(ty) = self
            .decls
            .universe
            .atom_by_name(name)
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
        {
            Val {
                ty,
                diverges: false,
                poisoned: false,
            }
        } else {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`{what}` needs a declared `{name}` type; inference never falls back [B1, B7]"
                ),
                span,
                "no such type",
            );
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        }
    }

    /// Interpolated strings: every hole is an explicit string context —
    /// a printable primitive (bool, char, float, int, string)
    /// converts through `str(e)` implicitly; every other operand is
    /// legal exactly when its type carries `show`, the same
    /// solved-instance proof a written `.show()` runs, the dispatch
    /// recorded at the hole's own span for the lowering's staging.
    pub(super) fn string_expr(&mut self, span: Span, parts: &[StrPart]) -> Val {
        let string_ty = self.named_atom_ty(span, "string", "string literal");
        let Some(string_ty) = string_ty else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        for part in parts {
            let (hole_span, hole_val) = match part {
                StrPart::Text { .. } => continue,
                // The hole is a conversion context, never a directed one:
                // no expected type threads in, so a bare label
                // keeps its declaring atom and the `show` proof below
                // owns the operand.
                StrPart::Expr { span, expr } => (*span, self.expr(expr, None)),
                StrPart::Ident { span, name } => {
                    let val = self.ident_hole_value(name);
                    self.record_val(name.span, val);
                    // the hole's own span (`$x`, dollar included) is the
                    // dispatch record's key — the lowerer's synthetic
                    // member call carries it, and the name's own span
                    // keeps recording the operand's type
                    (*span, val)
                },
            };
            if self.printable_hole(hole_val.ty) {
                continue;
            }
            // An empty or poisoned hole is an already-reported operand,
            // never a new mismatch: the cascade stays with its root cause.
            if self.decls.universe.is_empty(hole_val.ty) || hole_val.poisoned {
                continue;
            }
            match self.hole_show_dispatch(hole_span, hole_val.ty, string_ty) {
                HoleShow::Dispatched | HoleShow::Reported => continue,
                HoleShow::Refused => {},
            }
            let hole_ty = hole_val.ty;
            let note = if self.decls.universe.members(hole_ty).len() == 1 {
                "derive `show` (`#[derive(show)]`) or implement `show` for this type"
            } else {
                "extract a member first [B4, E4], or implement `show` over the whole union"
            };
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "interpolation hole has type `{}`; the printable primitives (bool, char, \
                         float, int, string) convert implicitly, every other type carries `show` \
                         [F6]",
                        self.decls.universe.render(hole_ty)
                    ),
                )
                .with_primary(hole_span, "not a printable operand")
                .with_note(note),
            );
        }
        Val {
            ty: string_ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// One hole's `show` proof: the candidate selection a written
    /// `.show()` runs — trait owners through the solved-instance
    /// environment (a union receiver through its covering sibling group),
    /// inherent owners by coverage, an in-scope assumption through the
    /// frame's runtime-evidence form — with the survivor's dispatch
    /// recorded at the hole's own span, exactly where a written call's
    /// span records it, so the lowering stages the ordinary member call.
    /// The survivor's registered signature must be the family's shape:
    /// the receiver alone, `string` back. Every dispatched hole records
    /// its member in `hole_show_dispatches` (the affine and effects
    /// passes' seam); a solved instance or an inherent owner records
    /// beside it as a written call does, while the assumption form
    /// records alone — its dispatch resolves at runtime through the
    /// evidence slot the entering site stages.
    pub(super) fn hole_show_dispatch(&mut self, span: Span, ty: Ty, string_ty: Ty) -> HoleShow {
        let mut survivors: Vec<MethodId> = Vec::new();
        let mut runtime: Vec<MethodId> = Vec::new();
        let mut ambiguous = None;
        for candidate in self.package.method_candidates("show") {
            let Some(sig) = self.member_sig(candidate) else {
                continue;
            };
            // the family's call shape: the receiver alone, `string` back —
            // the hole stages no argument and concatenates one string word
            let receiver_only = sig.params.len() == 1 && sig.params[0].name == "self";
            let ret_string =
                sig.ret.as_ref().and_then(|ret| self.solve_here(ret)) == Some(string_ty);
            if !receiver_only || !ret_string {
                continue;
            }
            match &self.package.method(candidate).owner {
                MethodOwner::Trait(def) => {
                    let Some(trait_id) = self.decls.traits.get(def).copied() else {
                        continue;
                    };
                    if self.assumption_covers(trait_id, ty) {
                        runtime.push(candidate);
                        continue;
                    }
                    match self.instance_resolution(trait_id, ty) {
                        Resolution::Unique(_) | Resolution::Covered(_) => {
                            survivors.push(candidate);
                        },
                        Resolution::Ambiguous(candidates) => {
                            ambiguous = Some((trait_id, candidates));
                        },
                        Resolution::None | Resolution::BoundUnsat { .. } => {},
                    }
                },
                MethodOwner::Inherent(target) => {
                    if self.inherent_accepts(target, ty) {
                        survivors.push(candidate);
                    }
                },
            }
        }
        if let Some((trait_id, candidates)) = ambiguous {
            self.instance_ambiguity(trait_id, ty, &candidates, span);
            return HoleShow::Reported;
        }
        match (survivors.as_slice(), runtime.as_slice()) {
            ([only], []) => {
                self.record_solved_dispatch(*only, ty, None, span);
                self.record_hole_show(span, *only);
                self.record_val(span, Val {
                    ty: string_ty,
                    diverges: false,
                    poisoned: false,
                });
                HoleShow::Dispatched
            },
            ([], [only]) => {
                self.record_hole_show(span, *only);
                self.record_val(span, Val {
                    ty: string_ty,
                    diverges: false,
                    poisoned: false,
                });
                HoleShow::Dispatched
            },
            ([], []) => HoleShow::Refused,
            (survivors, runtime) => {
                self.error(
                    ErrorCode::AmbiguousCall,
                    format!(
                        "the interpolation hole's `show` is ambiguous between {} owners; write \
                         the dispatch inside the hole and qualify it [F3, F6]",
                        survivors.len() + runtime.len()
                    ),
                    span,
                    "ambiguous implicit dispatch",
                );
                HoleShow::Reported
            },
        }
    }

    /// One hole operand under the printable rule: the string
    /// context itself, or exactly one printable primitive atom.
    pub(super) fn printable_hole(&self, ty: Ty) -> bool {
        let [atom] = self.decls.universe.members(ty) else {
            return false;
        };
        let name = self
            .decls
            .universe
            .atom_decl(*atom)
            .map_or("", |decl| decl.name.as_str());
        matches!(name, "bool" | "char" | "float" | "int" | "string")
    }

    /// The value type of a bare `$name` hole: the single-segment path
    /// resolution, recorded at the name's own span so the lowerer reads
    /// it there.
    pub(super) fn ident_hole_value(&mut self, name: &Ident) -> Val {
        if let Some(res) = self.res.get(name.span) {
            match res {
                Res::Local { bind } => Val {
                    ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                    diverges: false,
                    poisoned: false,
                },
                Res::Def(def) => self.def_value(*def, name.span, None),
                Res::TraitMethod(_) | Res::Dot(_) | Res::Handler | Res::Module(_) => Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                },
            }
        } else if name.name == "none" && !self.package.declares("none") {
            Val {
                ty: self.none_ty(),
                diverges: false,
                poisoned: false,
            }
        } else {
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{}` does not resolve to a value", name.name),
                name.span,
                "not a value",
            );
            Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }
        }
    }
}
