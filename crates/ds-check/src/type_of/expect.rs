//! Expectation checking: containment and argument checks at anchored
//! positions, and the diagnostics they report.

use super::*;

impl Bodies<'_> {
    pub(super) fn error(&mut self, code: ErrorCode, message: String, span: Span, label: &str) {
        self.diags
            .push(Diagnostic::error(code, message).with_primary(span, label));
    }

    pub(super) fn expect_contains(
        &mut self,
        val: Val,
        expected: Ty,
        span: Span,
        what: &str,
    ) -> bool {
        // A poisoned operand already reported its root cause; a boundary
        // mismatch on top of it would be a cascade.
        if val.diverges || val.poisoned || self.decls.universe.contains(expected, val.ty) {
            true
        } else {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "`{}` has type `{}`, not contained in `{}` [B2]",
                        what,
                        self.decls.universe.render(val.ty),
                        self.decls.universe.render(expected)
                    ),
                )
                .with_primary(span, "not contained"),
            );
            false
        }
    }

    /// One generic call's argument against its solved parameter type:
    /// the structural binder stays silent on a constructor mismatch (it
    /// binds nothing for `list_len("words")`), so the solved parameter has
    /// to contain the argument or the mismatch reports. A parameter that
    /// still carries an unbound variable renders its written spelling,
    /// never the internal `$t` name.
    pub(super) fn expect_param_contains(
        &mut self,
        val: Val,
        written: &Type,
        expected: Ty,
        span: Span,
    ) -> bool {
        // An empty side is an already-reported operand, and a poisoned one
        // reported inside its own subtree — never a new mismatch: the
        // cascade stays with its root cause.
        if val.diverges
            || val.poisoned
            || self.decls.universe.is_empty(val.ty)
            || self.decls.universe.is_empty(expected)
            || self.decls.universe.contains(expected, val.ty)
        {
            return true;
        }
        let expected_text = if self.has_free_vars(expected) {
            decls::render_type(written)
        } else {
            self.decls.universe.render(expected)
        };
        self.diags.push(
            Diagnostic::error(
                ErrorCode::TypeMismatch,
                format!(
                    "`argument` has type `{}`, not contained in `{}` [B2]",
                    self.decls.universe.render(val.ty),
                    expected_text
                ),
            )
            .with_primary(span, "not contained"),
        );
        false
    }

    /// Whether a solved type still carries an inference variable: a bare
    /// minted variable, or a structural shape applied over one.
    pub(super) fn has_free_vars(&self, ty: Ty) -> bool {
        self.decls
            .universe
            .members(ty)
            .iter()
            .any(|atom| self.atom_has_free_vars(*atom))
    }

    pub(super) fn atom_has_free_vars(&self, atom: TypeId) -> bool {
        match self.decls.universe.atom_decl(atom) {
            Ok(decl) => {
                decl.name.starts_with('$')
                    || match &decl.kind {
                        AtomKind::Apply(_, args) => args.iter().any(|arg| self.has_free_vars(*arg)),
                        AtomKind::Tuple(elements) => {
                            elements.iter().any(|elem| self.atom_has_free_vars(*elem))
                        },
                        AtomKind::FnSig(params, ret) => {
                            params.iter().any(|param| self.has_free_vars(*param))
                                || self.has_free_vars(*ret)
                        },
                        _ => false,
                    }
            },
            Err(_) => false,
        }
    }

    pub(super) fn expect_bool(&mut self, val: Val, span: Span) -> bool {
        let bool_ty = self.bool_ty();
        if val.diverges || val.poisoned || self.decls.universe.contains(bool_ty, val.ty) {
            true
        } else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "condition has type `{}`, expected `bool`",
                    self.decls.universe.render(val.ty)
                ),
                span,
                "not a boolean",
            );
            false
        }
    }

    pub(super) fn check_args(&mut self, args: &[&Expr], param_tys: &[Option<Ty>], span: Span) {
        if args.len() > param_tys.len() {
            // The direct path reports extra arguments where they appear;
            // the fn-typed-value path matches it, and the dropped
            // expressions still walk so inner errors surface.
            self.error(
                ErrorCode::ArityMismatch,
                "more arguments than the function type takes".to_owned(),
                args[param_tys.len()].span(),
                "extra argument",
            );
            for arg in &args[param_tys.len()..] {
                self.expr(arg, None);
            }
        } else if args.len() < param_tys.len() {
            self.error(
                ErrorCode::ArityMismatch,
                format!(
                    "missing argument(s): the function type takes {}",
                    param_tys.len()
                ),
                span,
                "missing arguments",
            );
        }
        for (arg, param) in args.iter().zip(param_tys.iter()) {
            match param {
                Some(param) => {
                    let val = self.expr(arg, Some(*param));
                    self.expect_contains(val, *param, arg.span(), "argument");
                },
                None => {
                    self.expr(arg, None);
                },
            }
        }
    }
}
