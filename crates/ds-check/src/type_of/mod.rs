//! The `type_of` pass: bidirectional checking with monotone flow refinement
//! [B2].
//!
//! # Engine shape
//!
//! Boundary anchoring per [B2]: top-level signatures are REQUIRED and the
//! set algebra checks against them by containment; locals, lambdas, and
//! match arms infer. Inference failure is a compile error — there is no
//! `any` fallback anywhere [B1]. Every type is a solved [`Ty`] from the
//! package universe; nothing unsolved ever enters an environment.
//!
//! The engine is one expression walk `expr(e, expected) -> Val`. The
//! `expected` hint is consumed exactly where inference cannot proceed
//! without it (lambda parameters, `?` splits, generic construction) and
//! ignored everywhere else; containment is checked explicitly at anchors
//! (call arguments, field values, returns, annotated bindings). `Val`
//! carries a `diverges` flag (early exits) so `if`/`match` unify over the
//! arms that actually produce a value.
//!
//! # Flow refinement
//!
//! Refinement is a per-binder overlay `refined: binder span -> Ty` over
//! declared types, narrowed monotonically by intersection: `is` tests,
//! none-comparisons, and match-arm patterns (through the scrutinee binder
//! and the arm's pattern coverage) [B2]. The overlay resets at assignment
//! and at loop entry (sound: refined is always contained in declared, so
//! the declared type is a legal widening). Known limit, from the record:
//! no narrowing through mutable fields of union-typed values.
//!
//! # E4 union errors
//!
//! `?` splits its operand against the success context: at an expected type
//! the members outside it propagate, elsewhere the members INSIDE the
//! enclosing return union propagate — either way every propagated member
//! must be contained in the written return union [DS2008]. `or` extracts
//! the wanted members (the right operand's type for a plain tail, the
//! return union for `or return`); `.err(h)`/`.ok(h)` are difference-typed:
//! the handled set is the handler parameter when annotated, otherwise
//! everything but what the handler's own result keeps.
//!
//! # Out of scope here
//!
//! Effect rows; the affine/move pass (assumption bounds like `[t: json]`
//! feed method selection, `~Copyable` is M4); conditional instance heads
//! seen through substitution (H3); ranges, indexing, and `in`-containment
//! (they need the M3 stdlib's type vocabulary).

use std::collections::HashMap;

use ds_diag::{Diagnostic, ErrorCode, Files, Span};
use ds_parse::{
    Ast, BinaryOp, BlockExpr, EnumPayload, Expr, FnDecl, Ident, Item, ItemKind, Literal, LocalDecl,
    MatchArm, OrTail, Param, Path, Pattern, Stmt, StrPart, TraitMember, Type, TypeParam, UnaryOp,
};

use crate::{
    TypeExpr,
    atom::{AtomKind, TraitId, TypeId},
    decls::{self, Decls, FnSig, GenericParam},
    error::Error,
    resolve::{DefId, DefKind, DotRes, MethodId, MethodOwner, Package, Res, Resolutions},
    universe::Ty,
};

/// A typed expression: its solved type and whether evaluation always exits
/// early (return/break/continue), so arm unification can skip it.
#[derive(Clone, Copy, Debug)]
pub struct Val {
    /// The solved type of the expression.
    pub ty: Ty,
    /// Whether every path through the expression exits early.
    pub diverges: bool,
}

/// One local binding: declared type plus assignability [C3].
#[derive(Clone, Copy, Debug)]
struct Binding {
    ty: Ty,
    mutable: bool,
}

/// One narrowing fact harvested from a condition.
struct Narrow {
    /// The narrowed binder.
    bind: Span,
    /// Narrow into `ty` (true) or away from it (false).
    keep: bool,
    /// The narrowing type.
    ty: Ty,
}

/// The result of one package check: diagnostics in walk order plus the
/// memoized type of every top-level binding [G3].
#[derive(Default)]
pub struct Checked {
    /// All diagnostics of collection, resolution, declaration, and bodies.
    pub diags: Vec<Diagnostic>,
    /// The solved type of every top-level const/var binding.
    pub def_types: HashMap<DefId, Ty>,
    /// Affine facts of every local binder, keyed by binder span: the
    /// copyability its type had at the binding site [C1]. The linearity
    /// pass consumes this; top-level bindings derive theirs from
    /// `def_types` directly.
    pub binders: HashMap<Span, crate::linearity::BinderFact>,
    /// The checked `Val` of every expression, keyed by its span: the solved
    /// type AT THAT USE SITE (flow-refined inside `is` tests and match arms
    /// [B2]) plus the early-exit flag. The lowering pass [M3] consumes this
    /// side table instead of re-deriving types; entries are recorded in
    /// walk order and a re-walk overwrites them.
    pub expr_types: HashMap<Span, Val>,
}

mod ops;
use ops::compound_op;
mod pattern;

/// Checks one package: collects and resolves every file, builds the
/// declaration tables, then body-checks every item. The files registry
/// supplies source text for machine-readable suggestions.
#[must_use]
pub fn type_check(files: &Files, asts: &[Ast]) -> Checked {
    analyze(files, asts).checked
}

/// The typed result of one resolved package: the full bidirectional check
/// plus every top-level binding's solved type, rendered through the
/// package universe. Types render as names because solved handles are
/// meaningful only inside the universe that minted them, and the query
/// layer stores values across computes.
pub struct TypedPackage {
    /// The check: declaration and body diagnostics plus the binder facts
    /// the affine pass reads.
    pub checked: Checked,
    /// The solved type of every top-level const/var binding, keyed by its
    /// package definition id.
    pub type_names: HashMap<DefId, String>,
}

/// The full bidirectional check bundled with the tables later passes read:
/// the package namespace, per-file resolutions, declaration tables, and
/// the typed result. Every `Ty` inside `Checked` was minted by
/// `decls.universe`, so consumers must pass the bundle along unchanged —
/// a second pass over the same sources mints an unrelated universe. The
/// affine pass [C1] and the lowering pass [M3] consume exactly these.
pub fn analyze(files: &Files, asts: &[Ast]) -> Analyzed {
    let (package, resolutions, mut diags) = crate::resolve_package(asts);
    let (decls, checked) = check_bodies(files, asts, &package, &resolutions);
    diags.extend(checked.diags);
    let checked = Checked {
        diags,
        def_types: checked.def_types,
        binders: checked.binders,
        expr_types: checked.expr_types,
    };
    Analyzed {
        package,
        resolutions,
        decls,
        checked,
    }
}

/// Checks a resolved package [B2]: builds the declaration tables and
/// body-checks every item against the package namespace and per-file
/// resolutions from
/// [`resolve_package`](crate::resolve_package). `asts` and `resolutions`
/// align positionally; `asts` may hold shared trees (`Borrow<Ast>`, like
/// [`effects_of`](crate::effects_of)). `checked.diags` carries the
/// declaration and body classes only — collection and resolution
/// diagnostics belong to the resolve half.
#[must_use]
pub fn check_resolved<T>(
    files: &Files,
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
) -> TypedPackage
where
    T: std::borrow::Borrow<Ast>,
{
    let (decls, checked) = check_bodies(files, asts, package, resolutions);
    let type_names = checked
        .def_types
        .iter()
        .map(|(&id, &ty)| (id, decls.universe.render(ty)))
        .collect();
    TypedPackage {
        checked,
        type_names,
    }
}

/// The declaration and body half of the check, exposed as
/// [`check_resolved`] plus the declaration tables the affine pass reads.
/// Iterates `asts` and `resolutions` positionally: the files a caller
/// registers may leave holes in the file-id space (unparseable files
/// contribute no tree), so positions — not file ids — align the two.
fn check_bodies<T>(
    files: &Files,
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
) -> (Decls, Checked)
where
    T: std::borrow::Borrow<Ast>,
{
    let trees: Vec<&Ast> = asts.iter().map(std::borrow::Borrow::borrow).collect();
    let (mut decls, mut diags) = decls::build(package, &trees, resolutions);
    let mut def_types = HashMap::new();
    let mut binders = HashMap::new();
    let mut expr_types = HashMap::new();
    let mut counter = 0u32;
    for (ast, res) in trees.iter().zip(resolutions) {
        let mut bodies = Bodies::new(
            files,
            package,
            res,
            &mut decls,
            &mut def_types,
            &mut binders,
            &mut expr_types,
            &mut counter,
        );
        bodies.file(ast);
        diags.append(&mut bodies.diags);
    }
    (decls, Checked {
        diags,
        def_types,
        binders,
        expr_types,
    })
}

/// The tables of one analyzed package; see [`analyze`].
#[must_use]
pub struct Analyzed {
    /// The package namespace: definitions, name tables, method registry.
    pub package: Package,
    /// Per-file name resolutions, indexed by file id.
    pub resolutions: Vec<Resolutions>,
    /// The declaration tables, owning the universe that minted every `Ty`.
    pub decls: Decls,
    /// The typed result: diagnostics, top-level types, binder facts,
    /// per-expression types.
    pub checked: Checked,
}

/// The body checker over one file.
struct Bodies<'a> {
    files: &'a Files,
    package: &'a Package,
    res: &'a Resolutions,
    decls: &'a mut Decls,
    def_types: &'a mut HashMap<DefId, Ty>,
    binders: &'a mut HashMap<Span, crate::linearity::BinderFact>,
    expr_types: &'a mut HashMap<Span, Val>,
    counter: &'a mut u32,
    diags: Vec<Diagnostic>,
    /// Minted type variables whose generic parameter subtracts the
    /// copyability constraint (`[t: ~Copyable]`) [C1]; the binder facts
    /// consult them while recording.
    linear_vars: HashMap<TypeId, bool>,
    /// Declared types of locals, keyed by binder span.
    locals: HashMap<Span, Binding>,
    /// The refinement overlay, keyed by binder span [B2].
    refined: HashMap<Span, Ty>,
    /// Type-variable scopes, innermost last (generic parameters, `self`).
    type_vars: Vec<HashMap<String, Ty>>,
    /// In-scope trait assumptions from generic bounds (`[t: json]`), with
    /// per-scope truncation marks.
    assumptions: Vec<(TraitId, Ty)>,
    assumption_marks: Vec<usize>,
    /// The enclosing return union; `None` outside function bodies.
    ret: Option<Ty>,
    loop_depth: usize,
    /// Cycle guard for order-independent top-level typing [G3].
    visiting: Vec<DefId>,
    /// Whether the failed solve just below already reported the tuple
    /// distribution cap precisely; the generic fallbacks around
    /// `solve_here` consume it so DS2012 never layers under another code.
    solve_reported: bool,
}

impl<'a> Bodies<'a> {
    fn new(
        files: &'a Files,
        package: &'a Package,
        res: &'a Resolutions,
        decls: &'a mut Decls,
        def_types: &'a mut HashMap<DefId, Ty>,
        binders: &'a mut HashMap<Span, crate::linearity::BinderFact>,
        expr_types: &'a mut HashMap<Span, Val>,
        counter: &'a mut u32,
    ) -> Self {
        Self {
            files,
            package,
            res,
            decls,
            def_types,
            binders,
            expr_types,
            counter,
            diags: Vec::new(),
            linear_vars: HashMap::new(),
            locals: HashMap::new(),
            refined: HashMap::new(),
            type_vars: Vec::new(),
            assumptions: Vec::new(),
            assumption_marks: Vec::new(),
            ret: None,
            loop_depth: 0,
            visiting: Vec::new(),
            solve_reported: false,
        }
    }

    /// Binds one local and records its affine facts [C1]: copyability of
    /// the solved type under the in-scope generic bounds.
    fn bind_local(&mut self, span: Span, binding: Binding) {
        let copyable = crate::linearity::ty_copyable(self.decls, binding.ty, &self.linear_vars);
        self.binders.insert(span, crate::linearity::BinderFact {
            copyable,
            ty: binding.ty,
        });
        self.locals.insert(span, binding);
    }

    fn file(&mut self, ast: &Ast) {
        for block in &ast.blocks {
            for item in &block.items {
                self.item(item);
            }
        }
    }

    // ---- items ------------------------------------------------------------

    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(f) => {
                if f.body.is_some() {
                    self.fn_decl(f, None);
                }
            },
            ItemKind::Trait(t) => {
                self.push_generics(&t.generics.iter().map(generic_of).collect::<Vec<_>>());
                let self_ty = self.fresh_var("self");
                self.type_vars
                    .push(HashMap::from([("self".to_owned(), self_ty)]));
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.fn_decl(f, None);
                    }
                }
                self.type_vars.pop();
                self.pop_generics();
            },
            ItemKind::Impl(imp) => {
                self.impl_item(item, imp);
            },
            ItemKind::Local(local) => self.top_level_local(local),
            ItemKind::ExternFn(_)
            | ItemKind::Struct(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Import(_) => {},
        }
    }

    fn impl_item(&mut self, item: &Item, imp: &ds_parse::ImplDecl) {
        let _ = item;
        let generics: Vec<GenericParam> = imp.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        let target = self.solve_here(&imp.target);
        let Some(target) = target else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "impl target `{}` does not solve against the package declarations",
                        decls::render_type(&imp.target)
                    ),
                    imp.target.span(),
                    "unsolvable target",
                );
            }
            self.pop_generics();
            return;
        };
        self.type_vars
            .push(HashMap::from([("self".to_owned(), target)]));
        let member_names: Option<Vec<String>> = imp.trait_ref.as_ref().and_then(|tr| {
            let [only] = tr.path.segments.as_slice() else {
                return None
            };
            self.package
                .lookup(&only.name)
                .and_then(|def| self.decls.trait_members.get(&def).cloned())
        });
        for f in &imp.fns {
            if let Some(members) = &member_names {
                if !members.iter().any(|m| m == &f.name.name) {
                    self.error(
                        ErrorCode::UnresolvedName,
                        format!("the implemented trait has no member `{}`", f.name.name),
                        f.name.span,
                        "not a trait member",
                    );
                    continue;
                }
            }
            self.fn_decl(f, Some(target));
        }
        self.type_vars.pop();
        self.pop_generics();
    }

    /// A top-level binding: its initializer is typed (memoized for
    /// order-independent uses [G3]); a written annotation anchors the check.
    fn top_level_local(&mut self, local: &LocalDecl) {
        let Some(def) = self.pattern_def(local) else {
            return
        };
        if self.def_types.contains_key(&def) || self.visiting.contains(&def) {
            return;
        }
        self.visiting.push(def);
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;

        let mut val = self.expr(&local.init, None);
        if let Some(ann) = &local.ty {
            if let Some(expected) = self.solve_here(ann) {
                self.expect_contains(val, expected, local.init.span(), "annotated binding");
                val.ty = expected;
            } else if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "annotation `{}` does not solve against the package declarations",
                        decls::render_type(ann)
                    ),
                    ann.span(),
                    "unsolvable annotation",
                );
            }
        }
        for (name, def, ty) in self.pattern_defs(&local.pat, val.ty) {
            let _ = name;
            if let Some(def) = def {
                self.def_types.insert(def, ty);
            }
        }
        self.restore(saved);
        self.visiting.pop();
    }

    /// One named function declaration: boundary-anchored [B2]. `self_ty`
    /// binds the `self` type inside trait/impl scopes [section 6.1].
    fn fn_decl(&mut self, f: &FnDecl, self_ty: Option<Ty>) {
        let generics: Vec<GenericParam> = f.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        if let Some(target) = self_ty {
            self.type_vars
                .push(HashMap::from([("self".to_owned(), target)]));
        }
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;
        for param in &f.params {
            if let Some(solved) = param.ty.as_ref().and_then(|t| self.solve_here(t)) {
                self.bind_local(param.name.span, Binding {
                    ty: solved,
                    mutable: param_is_mut(param),
                });
            } else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "parameter `{}` needs a type annotation at this boundary [B2]",
                            param.name.name
                        ),
                        param.name.span,
                        "unannotated boundary parameter",
                    );
                }
                let empty = self.empty();
                self.bind_local(param.name.span, Binding {
                    ty: empty,
                    mutable: param_is_mut(param),
                });
            }
        }
        let ret = f.ret.as_ref().and_then(|r| self.solve_here(r)).or_else(|| {
            f.ret.as_ref().map(|r| {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "return type `{}` does not solve against the package declarations",
                            decls::render_type(r)
                        ),
                        r.span(),
                        "unsolvable return type",
                    );
                }
                self.empty()
            })
        });
        let old_ret = self.ret;
        self.ret = ret;
        if let Some(body) = &f.body {
            let val = self.block(body);
            if let Some(ret) = ret {
                if !val.diverges {
                    self.expect_contains(val, ret, body.span, "function body");
                }
            }
        }
        self.ret = old_ret;
        self.restore(saved);
        if self_ty.is_some() {
            self.type_vars.pop();
        }
        self.pop_generics();
    }

    // ---- statements ---------------------------------------------------------

    fn block(&mut self, block: &BlockExpr) -> Val {
        let saved = self.snapshot();
        let mut val = Val {
            ty: self.unit(),
            diverges: false,
        };
        for stmt in &block.stmts {
            val = self.stmt(stmt);
        }
        self.restore(saved);
        val
    }

    fn stmt(&mut self, stmt: &Stmt) -> Val {
        match &stmt.kind {
            ds_parse::StmtKind::Local(local) => {
                self.local_stmt(local);
                Val {
                    ty: self.unit(),
                    diverges: false,
                }
            },
            ds_parse::StmtKind::Expr(expr) => self.expr(expr, None),
        }
    }

    /// A statement-level binding: locals infer [B2].
    fn local_stmt(&mut self, local: &LocalDecl) {
        let mut val = self.expr(&local.init, None);
        if let Some(ann) = &local.ty {
            if let Some(expected) = self.solve_here(ann) {
                self.expect_contains(val, expected, local.init.span(), "annotated binding");
                val.ty = expected;
            } else if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "annotation `{}` does not solve against the package declarations",
                        decls::render_type(ann)
                    ),
                    ann.span(),
                    "unsolvable annotation",
                );
            }
        }
        if self.decls.universe.is_empty(val.ty) && !val.diverges && !self.take_solve_reported() {
            self.error(
                ErrorCode::InferenceFailed,
                "the initializer's type is empty; inference found no inhabitant [B1]".to_owned(),
                local.init.span(),
                "empty initializer type",
            );
        }
        self.bind_pattern(&local.pat, val.ty, local.mutable);
    }

    // ---- expressions ----------------------------------------------------------

    fn expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let val = self.expr_inner(e, expected);
        self.expr_types.insert(e.span(), val);
        val
    }

    fn expr_inner(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        match e {
            Expr::Literal { span, lit } => self.literal(*span, lit),
            Expr::Str { span, parts } => self.string_expr(*span, parts),
            Expr::RawStr { span, .. } => self.named_ty(*span, "string", "raw string"),
            Expr::Lambda { .. } => self.lambda(e, expected),
            Expr::Path { path, .. } => self.path_expr(path),
            Expr::Struct { .. } => self.struct_expr(e, expected),
            Expr::Tuple {
                items,
                trailing_comma,
                ..
            } => {
                if items.len() == 1 && !trailing_comma {
                    return self.expr(&items[0], expected);
                }
                let mut elements = Vec::with_capacity(items.len());
                for item in items {
                    elements.push(self.expr(item, None).ty);
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
                }
            },
            Expr::Paren { inner, .. } => self.expr(inner, expected),
            Expr::Call { .. } => self.call(e),
            Expr::Field { recv, name, .. } => self.field_access(recv, name),
            Expr::Index { span, .. } => {
                self.error(
                    ErrorCode::InferenceFailed,
                    "index typing arrives with the stdlib collection types [F5, M3]".to_owned(),
                    *span,
                    "index",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                }
            },
            Expr::Try { expr, span } => self.try_expr(expr, expected, *span),
            Expr::Unary { op, expr, span } => self.unary(*op, expr, *span),
            Expr::Is { expr, ty, .. } => {
                let _ = self.expr(expr, None);
                if self.solve_here(ty).is_none() && !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the `is` test against `{}` does not solve",
                            decls::render_type(ty)
                        ),
                        ty.span(),
                        "unsolvable test type",
                    );
                }
                Val {
                    ty: self.bool_ty(),
                    diverges: false,
                }
            },
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => self.binary(*op, left, right, *span),
            Expr::Range { span, .. } => {
                self.error(
                    ErrorCode::InferenceFailed,
                    "range types arrive with the stdlib [F7, M3]".to_owned(),
                    *span,
                    "range",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                }
            },
            Expr::Or { left, right, span } => self.or_expr(left, right, *span),
            Expr::Assign {
                op,
                left,
                right,
                span,
            } => self.assign(*op, left, right, *span),
            Expr::If {
                cond, then, else_, ..
            } => self.if_expr(cond, then, else_.as_deref()),
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.match_expr(scrutinee, arms, *span),
            Expr::While { cond, body, .. } => {
                let cond_val = self.expr(cond, None);
                self.expect_bool(cond_val, cond.span());
                let saved = self.snapshot();
                self.refined.clear();
                self.loop_depth += 1;
                self.block(body);
                self.loop_depth -= 1;
                self.restore(saved);
                Val {
                    ty: self.unit(),
                    diverges: false,
                }
            },
            Expr::For {
                pat,
                iter,
                body,
                span,
            } => self.for_expr(pat, iter, body, *span),
            Expr::Return { expr, span } => {
                let value = match expr {
                    Some(expr) => self.expr(expr, None),
                    None => Val {
                        ty: self.unit(),
                        diverges: false,
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
                }
            },
            Expr::Break(span) | Expr::Continue(span) => {
                if self.loop_depth == 0 {
                    self.error(
                        ErrorCode::InferenceFailed,
                        "loop exit outside a loop".to_owned(),
                        *span,
                        "stray loop exit",
                    );
                }
                Val {
                    ty: self.empty(),
                    diverges: true,
                }
            },
            Expr::Nursery { body, .. } | Expr::Spawn { body, .. } => {
                let saved = self.snapshot();
                self.refined.clear();
                self.block(body);
                self.restore(saved);
                Val {
                    ty: self.unit(),
                    diverges: false,
                }
            },
            Expr::Defer { body, .. } => {
                self.block(body);
                Val {
                    ty: self.unit(),
                    diverges: false,
                }
            },
            Expr::Block(block) => self.block(block),
        }
    }

    fn literal(&mut self, span: Span, lit: &Literal) -> Val {
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
    /// [B7]: the numeric vocabulary is declared data, never builtin
    /// identity — inference never falls back.
    fn named_ty(&mut self, span: Span, name: &str, what: &str) -> Val {
        if let Some(ty) = self
            .decls
            .universe
            .atom_by_name(name)
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
        {
            Val {
                ty,
                diverges: false,
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
            }
        }
    }

    /// Interpolated strings: every hole is a string operand — no hidden
    /// stringification [F6].
    fn string_expr(&mut self, span: Span, parts: &[StrPart]) -> Val {
        let string_ty = self.named_atom_ty(span, "string", "string literal");
        let Some(string_ty) = string_ty else {
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        for part in parts {
            if let StrPart::Expr { expr, span } = part {
                let val = self.expr(expr, Some(string_ty));
                if !self.decls.universe.contains(string_ty, val.ty) {
                    let hole = self.span_text(*span);
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "interpolation hole has type `{}`; call `str()` at the boundary \
                                 [F6]",
                                self.decls.universe.render(val.ty)
                            ),
                        )
                        .with_primary(*span, "not a string operand")
                        .with_suggestion(ds_diag::Suggestion::Replace {
                            span: *span,
                            with: format!("str({hole})"),
                        }),
                    );
                }
            }
        }
        Val {
            ty: string_ty,
            diverges: false,
        }
    }

    fn path_expr(&mut self, path: &Path) -> Val {
        if let [only] = path.segments.as_slice() {
            if let Some(res) = self.res.get(only.span) {
                match res {
                    Res::Local { bind } => Val {
                        ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                        diverges: false,
                    },
                    Res::Def(def) => self.def_value(*def, only.span),
                    Res::TraitMethod(_) | Res::Dot(_) | Res::Handler => Val {
                        ty: self.empty(),
                        diverges: false,
                    },
                }
            } else if only.name == "none" && self.package.lookup("none").is_none() {
                Val {
                    ty: self.none_ty(),
                    diverges: false,
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
                }
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
            }
        }
    }

    /// The value type of a package-level definition used as a value.
    fn def_value(&mut self, def: DefId, span: Span) -> Val {
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
                    }
                },
                None => Val {
                    ty: self.empty(),
                    diverges: false,
                },
            },
            DefKind::Tag => match self.decls.atoms.get(&def).copied() {
                Some(atom) => Val {
                    ty: self
                        .decls
                        .universe
                        .atom_ty(atom)
                        .unwrap_or_else(|_| self.empty()),
                    diverges: false,
                },
                None => Val {
                    ty: self.empty(),
                    diverges: false,
                },
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
                }
            },
        }
    }

    /// The memoized type of a top-level binding, typed lazily so
    /// order-independent uses work [G3].
    fn const_ty(&mut self, def: DefId, span: Span) -> Val {
        if let Some(&ty) = self.def_types.get(&def) {
            return Val {
                ty,
                diverges: false,
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
        }
    }

    fn lambda(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Lambda {
            generics,
            params,
            ret,
            body,
            ..
        } = e
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let expected_sig = expected.and_then(|ty| self.decompose_fn(ty));
        self.push_generics(&generics.iter().map(generic_of).collect::<Vec<_>>());
        let saved = self.snapshot();
        let mut param_tys = Vec::with_capacity(params.len());
        for (index, param) in params.iter().enumerate() {
            let solved = param
                .ty
                .as_ref()
                .and_then(|t| self.solve_here(t))
                .or_else(|| {
                    expected_sig
                        .as_ref()
                        .and_then(|(ps, _)| ps.get(index).copied())
                });
            let Some(solved) = solved else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "parameter `{}` has no expected type to infer from; annotate it [B2]",
                            param.name.name
                        ),
                        param.name.span,
                        "uninferable parameter",
                    );
                }
                let empty = self.empty();
                param_tys.push(empty);
                self.bind_local(param.name.span, Binding {
                    ty: empty,
                    mutable: false,
                });
                continue;
            };
            param_tys.push(solved);
            self.bind_local(param.name.span, Binding {
                ty: solved,
                mutable: param_is_mut(param),
            });
        }
        let declared_ret = ret.as_ref().and_then(|r| self.solve_here(r));
        let fn_ret_expected = declared_ret.or_else(|| expected_sig.as_ref().map(|(_, r)| *r));
        // A still-unbound variable as the expected return is the
        // instantiation's inference hole (a generic callee's `r`): the
        // lambda's own body type solves it, checked by the caller's
        // match against the fn-typed parameter.
        let expected_is_var = fn_ret_expected.is_some_and(|r| self.is_var_ty(r));
        let old_ret = self.ret;
        self.ret = if expected_is_var { None } else { fn_ret_expected };
        let body_val = self.expr(body, None);
        let fn_ret = match fn_ret_expected {
            Some(r) if !expected_is_var => {
                if !body_val.diverges {
                    self.expect_contains(body_val, r, body.span(), "lambda body");
                }
                r
            },
            _ => body_val.ty,
        };
        self.ret = old_ret;
        self.restore(saved);
        self.pop_generics();
        let params_boxed: Box<[Ty]> = param_tys.into();
        let ty = self
            .decls
            .universe
            .fn_atom(&params_boxed, fn_ret)
            .ok()
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
        }
    }

    fn struct_expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Struct { path, fields, span } = e else {
            return Val {
                ty: self.empty(),
                diverges: false,
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
            };
        };
        let Some(def) = self.package.lookup(&only.name) else {
            return Val {
                ty: self.empty(),
                diverges: false,
            }; // resolver reported
        };
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
            };
        };
        // Substitute the constructor's parameters: from an expected
        // application of the same constructor, else from in-scope variables
        // named like the parameters (generic construction inside its own
        // scope) [B2].
        let mut subst = HashMap::new();
        if !shape.params.is_empty() {
            if let Some(expected) = expected {
                if let Some((ctor, args)) = self.single_apply(expected) {
                    if self.decls.defs_of_atoms.get(&ctor) == Some(&def) {
                        for (param, arg) in shape.params.iter().zip(args.iter()) {
                            subst.insert(param.name.clone(), *arg);
                        }
                    }
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
                            },
                            Res::Def(def) => self.def_value(*def, name.span),
                            _ => Val {
                                ty: self.empty(),
                                diverges: false,
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
        }
    }

    fn call(&mut self, e: &Expr) -> Val {
        let Expr::Call {
            callee,
            args,
            trailing,
            span,
        } = e
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let callee = peel_paren(callee);
        if let Expr::Field { recv, name, .. } = callee {
            return self.dot_call(recv, name, args, trailing.as_deref(), *span);
        }
        if let Expr::Path { path, .. } = callee {
            match path.segments.as_slice() {
                [only] => {
                    if let Some(Res::Def(def)) = self.res.get(only.span) {
                        let def = *def;
                        if self.decls.fns.contains_key(&def) {
                            return self.call_def(def, None, args, trailing.as_deref(), *span);
                        }
                        if self.decls.structs.contains_key(&def)
                            || self.decls.atoms.contains_key(&def)
                        {
                            self.error(
                                ErrorCode::InferenceFailed,
                                format!(
                                    "`{}` is a type; construct structs with `name {{ ... }}` and \
                                     tags by name",
                                    only.name
                                ),
                                only.span,
                                "type called as a function",
                            );
                            for arg in args {
                                self.expr(&arg.value, None);
                            }
                            return Val {
                                ty: self.empty(),
                                diverges: false,
                            };
                        }
                        let callee_val = self.def_value(def, only.span);
                        return self.indirect_call(callee_val, args, trailing.as_deref(), *span);
                    }
                },
                [_head, method] => {
                    if let Some(Res::TraitMethod(method_id)) = self.res.get(method.span) {
                        return self.qualified_call(*method_id, args, trailing.as_deref(), *span);
                    }
                },
                _ => {},
            }
        }
        let callee_val = self.expr(callee, None);
        self.indirect_call(callee_val, args, trailing.as_deref(), *span)
    }

    fn indirect_call(
        &mut self,
        callee: Val,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some((param_tys, ret)) = self.decompose_fn(callee.ty) else {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`{}` is not callable; expected one function type",
                    self.decls.universe.render(callee.ty)
                ),
                span,
                "not a function",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let mut all: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        if let Some(tail) = trailing {
            all.push(tail);
        }
        let params: Vec<Option<Ty>> = param_tys.iter().copied().map(Some).collect();
        self.check_args(&all, &params, span);
        Val {
            ty: ret,
            diverges: false,
        }
    }

    /// Calls one package function by definition: instantiates generics by
    /// structural argument matching (the M2 half of one-body generics
    /// [H3]).
    /// Calls one package function by definition: instantiates generics by
    /// structural argument matching (the M2 half of one-body generics
    /// [H3]); named arguments fill their parameter's slot and literal-only
    /// defaults cover omissions [F7].
    fn call_def(
        &mut self,
        def: DefId,
        fed: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some(sig) = self.decls.fns.get(&def).cloned() else {
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let slots = self.arg_slots(fed, 0, args, trailing, &sig, span);
        let Some(slots) = slots else {
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        if sig.generics.is_empty() {
            let mut param_tys = Vec::with_capacity(sig.params.len());
            for param in &sig.params {
                if let Some(solved) = param.ty.as_ref().and_then(|t| self.solve_here(t)) {
                    param_tys.push(Some(solved));
                } else {
                    if !self.take_solve_reported() {
                        self.error(
                            ErrorCode::InferenceFailed,
                            "a parameter type does not solve; annotate it [B2]".to_owned(),
                            span,
                            "unannotated parameter",
                        );
                    }
                    param_tys.push(None);
                }
            }
            let ret = sig
                .ret
                .as_ref()
                .and_then(|r| self.solve_here(r))
                .unwrap_or_else(|| self.unit());
            self.check_slots(&slots, &sig, &param_tys);
            return Val {
                ty: ret,
                diverges: false,
            };
        }
        // Generic: mint fresh variables, bind non-lambda arguments by
        // structural matching, check lambda arguments against their
        // fn-typed parameters (binding return variables), then solve the
        // return under the substitution. The substitution (not a scope)
        // carries the variables, so the caller's in-scope variables never
        // capture the callee's.
        let var_atoms: Vec<TypeId> = sig
            .generics
            .iter()
            .map(|p| self.mint_var(&p.name))
            .collect();
        let mut subst: HashMap<String, Ty> = HashMap::new();
        let assumption_mark = self.assumptions.len();
        for (index, param) in sig.generics.iter().enumerate() {
            if let Ok(ty) = self.decls.universe.atom_ty(var_atoms[index]) {
                if param.bound.as_ref().is_some_and(decls::linear_marker) {
                    self.linear_vars.insert(var_atoms[index], false);
                }
                if let Some(trait_id) = param.bound.as_ref().and_then(|b| self.trait_of_type(b)) {
                    self.assumptions.push((trait_id, ty));
                }
            }
        }
        // Pass one: non-lambda arguments bind variables structurally.
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            let (Some(arg), Some(param_ty)) = (slot, param.ty.as_ref()) else {
                continue;
            };
            if matches!(peel_paren(arg), Expr::Lambda { .. }) && matches!(param_ty, Type::Fn { .. })
            {
                continue;
            }
            let arg_val = self.expr(arg, None);
            self.match_param(param_ty, arg_val.ty, &sig.generics, &mut subst);
        }
        // Pass two: lambda arguments check against the fn-typed parameter,
        // and their result type binds the parameter's return variables.
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            let Some(Type::Fn {
                params: fn_params,
                ret: fn_ret,
                ..
            }) = &param.ty
            else {
                continue;
            };
            let Some(arg) = slot else { continue };
            if !matches!(peel_paren(arg), Expr::Lambda { .. }) {
                continue;
            }
            let mut param_tys = Vec::with_capacity(fn_params.len());
            for p in fn_params {
                let written = p.ty.clone().unwrap_or_else(|| name_type(&p.name.name));
                let solved = self.solve_with(&written, &subst, &sig.generics);
                param_tys.push(match solved {
                    Some(ty) => ty,
                    None => self.fresh_var(&p.name.name),
                });
            }
            let expected_ret = fn_ret
                .as_ref()
                .and_then(|r| self.solve_with(r, &subst, &sig.generics))
                .unwrap_or_else(|| self.empty());
            let params_boxed: Box<[Ty]> = param_tys.into();
            let expected_ty = self
                .decls
                .universe
                .fn_atom(&params_boxed, expected_ret)
                .ok()
                .and_then(|a| self.decls.universe.atom_ty(a).ok())
                .unwrap_or_else(|| self.empty());
            let val = self.expr(arg, Some(expected_ty));
            if let Some(fn_ret) = fn_ret {
                // The lambda's own result type binds the parameter's
                // return variables, not the lambda's function type.
                let result = self
                    .decompose_fn(val.ty)
                    .map_or_else(|| val.ty, |(_, ret)| ret);
                self.match_param(fn_ret, result, &sig.generics, &mut subst);
            }
        }
        let ret = match sig.ret.as_ref() {
            Some(ret_ty) => self
                .solve_with(ret_ty, &subst, &sig.generics)
                .unwrap_or_else(|| self.empty()),
            None => self.unit(),
        };
        // Unbound variables leaking into the result are inference
        // failures: no any fallback [B1].
        let ret_members = self.decls.universe.members(ret).to_vec();
        for atom in ret_members {
            if let Some(pos) = var_atoms.iter().position(|v| *v == atom) {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "cannot infer `{}` from this call's arguments [B2]",
                        sig.generics[pos].name
                    ),
                    span,
                    "unbound type variable",
                );
            }
        }
        self.assumptions.truncate(assumption_mark);
        Val {
            ty: ret,
            diverges: false,
        }
    }

    /// Maps call arguments onto signature parameter slots [F7]: positional
    /// arguments in order, named arguments by parameter name, the trailing
    /// lambda at the next open position; omissions must carry a literal
    /// default. `None` returns after diagnosing.
    fn arg_slots<'x>(
        &mut self,
        fed: Option<&'x Expr>,
        skip: usize,
        args: &'x [ds_parse::CallArg],
        trailing: Option<&'x Expr>,
        sig: &FnSig,
        span: Span,
    ) -> Option<Vec<Option<&'x Expr>>> {
        let mut slots: Vec<Option<&'x Expr>> = vec![None; sig.params.len()];
        let mut next_positional = skip;
        let mut ok = true;
        // The UFCS-fed receiver occupies the first positional slot [F3].
        if let Some(fed) = fed {
            if slots.is_empty() {
                self.error(
                    ErrorCode::ArityMismatch,
                    "the dot-call receiver has no parameter to feed".to_owned(),
                    span,
                    "extra argument",
                );
                ok = false;
            } else {
                slots[0] = Some(fed);
                next_positional = 1;
            }
        }
        // Leading parameters the caller already handled (a method's
        // receiver) count as filled.
        for arg in args {
            match &arg.name {
                Some(name) => {
                    let value: &'x Expr = &arg.value;
                    match sig.params.iter().position(|p| p.name == name.name) {
                        Some(index) if slots[index].is_none() => slots[index] = Some(value),
                        Some(_) => {
                            self.error(
                                ErrorCode::ArityMismatch,
                                format!("argument `{}` is given twice", name.name),
                                name.span,
                                "duplicate argument",
                            );
                            ok = false;
                        },
                        None => {
                            self.error(
                                ErrorCode::UnresolvedName,
                                format!("no parameter `{}`", name.name),
                                name.span,
                                "no such parameter",
                            );
                            ok = false;
                        },
                    }
                },
                None => {
                    if next_positional < slots.len() {
                        let value: &'x Expr = &arg.value;
                        slots[next_positional] = Some(value);
                        next_positional += 1;
                    } else {
                        self.error(
                            ErrorCode::ArityMismatch,
                            "more arguments than parameters".to_owned(),
                            arg.span,
                            "extra argument",
                        );
                        ok = false;
                    }
                },
            }
        }
        if let Some(tail) = trailing {
            if next_positional < slots.len() && slots[next_positional].is_none() {
                let value: &'x Expr = tail;
                slots[next_positional] = Some(value);
            } else {
                self.error(
                    ErrorCode::ArityMismatch,
                    "the trailing lambda has no parameter to feed".to_owned(),
                    span,
                    "extra argument",
                );
                ok = false;
            }
        }
        let missing: Vec<&str> = slots
            .iter()
            .zip(sig.params.iter())
            .enumerate()
            .filter(|(index, (slot, param))| {
                slot.is_none() && param.default.is_none() && *index >= skip
            })
            .map(|(_, (_, param))| param.name.as_str())
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ArityMismatch,
                    format!("missing argument(s): {names}"),
                )
                .with_primary(span, "missing arguments"),
            );
            ok = false;
        }
        if ok { Some(slots) } else { None }
    }

    /// Checks filled slots against their parameter types and omitted ones
    /// against their written literal defaults [F7].
    fn check_slots(&mut self, slots: &[Option<&Expr>], sig: &FnSig, param_tys: &[Option<Ty>]) {
        for ((slot, param), param_ty) in slots.iter().zip(sig.params.iter()).zip(param_tys.iter()) {
            match (slot, param_ty) {
                (Some(arg), Some(param_ty)) => {
                    let val = self.expr(arg, Some(*param_ty));
                    self.expect_contains(val, *param_ty, arg.span(), "argument");
                },
                (Some(arg), None) => {
                    self.expr(arg, None);
                },
                (None, Some(param_ty)) => {
                    if let Some(default) = &param.default {
                        let val = self.expr(default, Some(*param_ty));
                        self.expect_contains(val, *param_ty, default.span(), "default value");
                    }
                },
                (None, None) => {},
            }
        }
    }

    fn qualified_call(
        &mut self,
        method: MethodId,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some(sig) = self.member_sig(method) else {
            self.error(
                ErrorCode::UnresolvedName,
                "the qualified member has no registered signature".to_owned(),
                span,
                "no signature",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let trait_id = match self.package.method(method).owner {
            MethodOwner::Trait(def) => self.decls.traits.get(&def).copied(),
            MethodOwner::Inherent => None,
        };
        let Some(recv) = args.first() else {
            self.error(
                ErrorCode::ArityMismatch,
                "the qualified member needs its receiver as the first argument".to_owned(),
                span,
                "missing receiver",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let recv_val = self.expr(&recv.value, None);
        if let Some(trait_id) = trait_id {
            if !self.trait_satisfied(trait_id, recv_val.ty, span) {
                for arg in args.iter().skip(1) {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                };
            }
        }
        self.member_call(&sig, recv_val.ty, &args[1..], trailing, span)
    }

    fn dot_call(
        &mut self,
        recv: &Expr,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some(res) = self.res.get(name.span).cloned() else {
            self.expr(recv, None);
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        match res {
            Res::Handler => self.err_ok(recv, &name.name, args, span),
            Res::Dot(DotRes::Free { func, fed_param }) => {
                // The receiver feeds the recorded parameter [F3]; only
                // parameter 0 exists today (capability parameters have no
                // syntax yet).
                let fed = (fed_param == 0).then_some(recv);
                self.call_def(func, fed, args, trailing, span)
            },
            Res::Dot(DotRes::Methods { candidates }) => {
                self.method_select(recv, name, &candidates, args, trailing, span)
            },
            _ => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` does not name a callable", name.name),
                    name.span,
                    "not callable",
                );
                self.expr(recv, None);
                for arg in args {
                    self.expr(&arg.value, None);
                }
                Val {
                    ty: self.empty(),
                    diverges: false,
                }
            },
        }
    }

    /// Method-candidate selection [F3, B6]: candidates narrow by receiver
    /// type through the instance environment (or an in-scope assumption);
    /// survivors beyond one are true ambiguity, never a priority order.
    fn method_select(
        &mut self,
        recv: &Expr,
        name: &Ident,
        candidates: &[MethodId],
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let recv_val = self.expr(recv, None);
        let mut survivors: Vec<FnSig> = Vec::new();
        for &candidate in candidates {
            let Some(sig) = self.member_sig(candidate) else {
                continue
            };
            let trait_id = match self.package.method(candidate).owner {
                MethodOwner::Trait(def) => self.decls.traits.get(&def).copied(),
                MethodOwner::Inherent => None,
            };
            match trait_id {
                Some(trait_id) => {
                    if self.trait_satisfied_quiet(trait_id, recv_val.ty) {
                        survivors.push(sig);
                    }
                },
                None => survivors.push(sig),
            }
        }
        if survivors.is_empty() {
            let trait_names: Vec<String> = candidates
                .iter()
                .filter_map(|c| match self.package.method(*c).owner {
                    MethodOwner::Trait(def) => Some(self.package.def(def).name.clone()),
                    MethodOwner::Inherent => None,
                })
                .collect();
            self.error(
                ErrorCode::UnsatisfiedConstraint,
                if trait_names.is_empty() {
                    format!(
                        "no method `{}` accepts receiver `{}`",
                        name.name,
                        self.decls.universe.render(recv_val.ty)
                    )
                } else {
                    format!(
                        "no instance of {} for `{}`",
                        trait_names
                            .iter()
                            .map(|t| format!("`{t}`"))
                            .collect::<Vec<_>>()
                            .join(" or "),
                        self.decls.universe.render(recv_val.ty)
                    )
                },
                span,
                "unimplemented trait",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        }
        if survivors.len() > 1 {
            let names: Vec<String> = candidates
                .iter()
                .filter(|c| self.member_sig(**c).is_some())
                .map(|c| match self.package.method(*c).owner {
                    MethodOwner::Trait(def) => format!("`{}`", self.package.def(def).name),
                    MethodOwner::Inherent => "`inherent`".to_owned(),
                })
                .collect();
            self.error(
                ErrorCode::AmbiguousCall,
                format!(
                    "dot-call `{}` is truly ambiguous between {}; qualify with `Trait::{}` [F3]",
                    name.name,
                    names.join(" and "),
                    name.name
                ),
                span,
                "ambiguous method",
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        }
        let sig = survivors.remove(0);
        self.member_call(&sig, recv_val.ty, args, trailing, span)
    }

    /// One selected method's call check: the member signature with `self`
    /// bound to the receiver's type [section 6.1].
    fn member_call(
        &mut self,
        sig: &FnSig,
        recv_ty: Ty,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        self.type_vars
            .push(HashMap::from([("self".to_owned(), recv_ty)]));
        let mut param_tys = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            param_tys.push(param.ty.as_ref().and_then(|t| self.solve_here(t)));
        }
        // The receiver already filled the `self` parameter [section 6.1];
        // the remaining arguments feed the rest.
        if let Some(slots) = self.arg_slots(None, 1, args, trailing, sig, span) {
            let rest: Vec<Option<&Expr>> = slots[1..].to_vec();
            let rest_tys: Vec<Option<Ty>> = param_tys[1..].to_vec();
            let rest_sig = FnSig {
                generics: sig.generics.clone(),
                params: sig.params[1..].to_vec(),
                ret: sig.ret.clone(),
            };
            self.check_slots(&rest, &rest_sig, &rest_tys);
        }
        let ret = sig
            .ret
            .as_ref()
            .and_then(|r| self.solve_here(r))
            .unwrap_or_else(|| self.unit());
        self.type_vars.pop();
        Val {
            ty: ret,
            diverges: false,
        }
    }

    /// `.err(h)`/`.ok(h)`: difference-typed handlers [E4]. The handled set
    /// is the handler's annotated parameter, or — unannotated — everything
    /// except what the handler's own result keeps of the operand.
    fn err_ok(&mut self, recv: &Expr, name: &str, args: &[ds_parse::CallArg], span: Span) -> Val {
        let recv_val = self.expr(recv, None);
        let total = recv_val.ty;
        if args.len() != 1 {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ArityMismatch,
                    format!("`.{name}` takes the handler lambda as its one argument"),
                )
                .with_primary(span, "wrong arity"),
            );
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        }
        let handler = args[0].value.clone();
        let Expr::Lambda {
            params: handler_params,
            body,
            ..
        } = peel_paren(&handler)
        else {
            self.error(
                ErrorCode::TypeMismatch,
                format!("`.{name}` handles through a lambda `(e) => ...`"),
                args[0].span,
                "not a handler lambda",
            );
            self.expr(&args[0].value, None);
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let annotated = handler_params
            .first()
            .and_then(|p| p.ty.as_ref())
            .and_then(|t| self.solve_here(t));
        let saved = self.snapshot();
        let param_ty = annotated.unwrap_or(total);
        if let Some(first) = handler_params.first() {
            self.bind_local(first.name.span, Binding {
                ty: param_ty,
                mutable: false,
            });
        }
        let body_val = self.expr(body, None);
        let body_ty = body_val.ty;
        self.restore(saved);
        let handled_set = if let Some(written) = annotated {
            written
        } else {
            let kept = self.decls.universe.inter(total, body_ty);
            self.decls.universe.diff(total, kept)
        };
        let overlap = self.decls.universe.inter(total, handled_set);
        if self.decls.universe.is_empty(overlap) && !self.decls.universe.is_empty(total) {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "the handler's `{}` handles `{}`, which `{}` does not carry",
                    name,
                    self.decls.universe.render(handled_set),
                    self.decls.universe.render(total)
                ),
                span,
                "disjoint handler",
            );
        }
        let remainder = self.decls.universe.diff(total, handled_set);
        let result =
            if name == "ok" { self.decls.universe.union(remainder, body_ty) } else { remainder };
        if self.decls.universe.is_empty(result) && !self.decls.universe.is_empty(total) {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`.{name}` leaves nothing: the handler consumed all of `{}`",
                    self.decls.universe.render(total)
                ),
                span,
                "empty remainder",
            );
        }
        Val {
            ty: result,
            diverges: false,
        }
    }

    /// Bare field access `x.f`: struct-owned, type-directed [F3]; on a
    /// generic application through the constructor's shape and the
    /// application's argument substitution.
    fn field_access(&mut self, recv: &Expr, name: &Ident) -> Val {
        let recv_val = self.expr(recv, None);
        let members = self.decls.universe.members(recv_val.ty).to_vec();
        let Some(single) = members.first().filter(|_| members.len() == 1) else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "field access needs one struct, `{}` given; narrow with `is` first [B2]",
                    self.decls.universe.render(recv_val.ty)
                ),
                name.span,
                "not a single struct",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let kind = self
            .decls
            .universe
            .atom_decl(*single)
            .ok()
            .map(|d| d.kind.clone());
        let (def, subst): (DefId, HashMap<String, Ty>) = match kind {
            Some(AtomKind::Apply(ctor, args)) => {
                if let Some(def) = self.decls.defs_of_atoms.get(&ctor).copied() {
                    let mut subst = HashMap::new();
                    if let Some(shape) = self.decls.structs.get(&def) {
                        for (param, arg) in shape.params.iter().zip(args.iter()) {
                            subst.insert(param.name.clone(), *arg);
                        }
                    }
                    (def, subst)
                } else {
                    self.no_field(recv_val.ty, name);
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                    };
                }
            },
            _ => match self.decls.defs_of_atoms.get(single).copied() {
                Some(def) if self.decls.structs.contains_key(&def) => (def, HashMap::new()),
                _ => {
                    self.no_field(recv_val.ty, name);
                    return Val {
                        ty: self.empty(),
                        diverges: false,
                    };
                },
            },
        };
        let Some(shape) = self.decls.structs.get(&def).cloned() else {
            self.no_field(recv_val.ty, name);
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        let Some(field) = shape.fields.iter().find(|f| f.name.name == name.name) else {
            self.no_field(recv_val.ty, name);
            return Val {
                ty: self.empty(),
                diverges: false,
            };
        };
        self.type_vars.push(subst);
        let field_ty = self.solve_here(&field.ty);
        self.type_vars.pop();
        if let Some(ty) = field_ty {
            Val {
                ty,
                diverges: false,
            }
        } else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!("field `{}` does not solve", name.name),
                    name.span,
                    "unsolvable field type",
                );
            }
            Val {
                ty: self.empty(),
                diverges: false,
            }
        }
    }

    fn no_field(&mut self, recv_ty: Ty, name: &Ident) {
        self.error(
            ErrorCode::TypeMismatch,
            format!(
                "`{}` has no field `{}`",
                self.decls.universe.render(recv_ty),
                name.name
            ),
            name.span,
            "no such field",
        );
    }

    /// `e?` propagation: the success part survives, every propagated member
    /// must be contained in the written return union [B2, E4].
    fn try_expr(&mut self, operand: &Expr, expected: Option<Ty>, span: Span) -> Val {
        let val = self.expr(operand, None);
        let total = val.ty;
        let ret = self.ret;
        let (ok, propagated) = match (expected, ret) {
            (Some(expected), _) => {
                let ok = self.decls.universe.inter(total, expected);
                let propagated = self.decls.universe.diff(total, expected);
                (ok, propagated)
            },
            (None, Some(ret)) => {
                let ok = self.decls.universe.diff(total, ret);
                let propagated = self.decls.universe.inter(total, ret);
                (ok, propagated)
            },
            (None, None) => {
                self.error(
                    ErrorCode::PropagationNotContained,
                    "`?` outside a function has nothing to propagate into [E4]".to_owned(),
                    span,
                    "stray propagation",
                );
                return Val {
                    ty: self.empty(),
                    diverges: false,
                };
            },
        };
        if let Some(ret) = ret {
            let escape = self.decls.universe.diff(propagated, ret);
            if !self.decls.universe.is_empty(escape) {
                self.error(
                    ErrorCode::PropagationNotContained,
                    format!(
                        "`?` propagates `{}`, which the return union `{}` does not contain [E4]",
                        self.decls.universe.render(escape),
                        self.decls.universe.render(ret)
                    ),
                    span,
                    "not contained",
                );
            }
        }
        if self.decls.universe.is_empty(ok) && !self.decls.universe.is_empty(total) {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "`?` leaves nothing: every member of `{}` propagates [E4]",
                    self.decls.universe.render(total)
                ),
                span,
                "empty success part",
            );
        }
        Val {
            ty: ok,
            diverges: false,
        }
    }

    /// `a or tail` [E4]: extracts the wanted members. `none` is always the
    /// extractable member of an optional [B4]: `x or ...` on `t | none`
    /// yields `t`. For non-optional operands the wanted set comes from the
    /// context — the right operand's type for a plain tail, the return
    /// union for `or return`.
    fn or_expr(&mut self, left: &Expr, right: &OrTail, span: Span) -> Val {
        let left_val = self.expr(left, None);
        let total = left_val.ty;
        let none = self.none_ty();
        let optional = self.decls.universe.members(total).contains(&TypeId::NONE);
        let fallback_val = match right {
            OrTail::Expr(fallback) => Some(self.expr(fallback, None)),
            _ => None,
        };
        let wanted = if optional {
            self.decls.universe.diff(total, none)
        } else {
            match (&fallback_val, self.ret) {
                (Some(fallback_val), _) => {
                    if self.decls.universe.is_empty(fallback_val.ty) {
                        // An uninhabited fallback never produces a value:
                        // everything is wanted.
                        total
                    } else {
                        self.decls.universe.inter(total, fallback_val.ty)
                    }
                },
                (None, Some(ret)) => self.decls.universe.inter(total, ret),
                (None, None) => total,
            }
        };
        match right {
            OrTail::Expr(fallback) => {
                let fallback_val = fallback_val.unwrap_or_else(|| Val {
                    ty: self.empty(),
                    diverges: false,
                });
                let _ = fallback;
                let result = if self.decls.universe.is_empty(fallback_val.ty) {
                    total
                } else {
                    self.decls.universe.union(wanted, fallback_val.ty)
                };
                Val {
                    ty: result,
                    diverges: false,
                }
            },
            OrTail::Return {
                expr,
                span: ret_span,
            } => {
                let value = match expr {
                    Some(expr) => self.expr(expr, None),
                    None => Val {
                        ty: self.unit(),
                        diverges: false,
                    },
                };
                if let Some(ret) = self.ret {
                    self.expect_contains(value, ret, *ret_span, "or-return value");
                    if self.decls.universe.is_empty(wanted) && !self.decls.universe.is_empty(total)
                    {
                        self.error(
                            ErrorCode::InferenceFailed,
                            format!(
                                "`or return` extracts nothing from `{}` against the return union \
                                 `{}` [E4]",
                                self.decls.universe.render(total),
                                self.decls.universe.render(ret)
                            ),
                            span,
                            "empty extraction",
                        );
                    }
                    Val {
                        ty: wanted,
                        diverges: false,
                    }
                } else {
                    self.error(
                        ErrorCode::PropagationNotContained,
                        "`or return` outside a function [E4]".to_owned(),
                        *ret_span,
                        "stray early exit",
                    );
                    Val {
                        ty: self.empty(),
                        diverges: false,
                    }
                }
            },
            OrTail::Break(_) | OrTail::Continue(_) => Val {
                ty: wanted,
                diverges: false,
            },
        }
    }

    fn assign(&mut self, op: ds_parse::AssignOp, left: &Expr, right: &Expr, span: Span) -> Val {
        let unit = self.unit();
        let place = match peel_paren(left) {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                self.res.get(path.segments[0].span).cloned()
            },
            _ => None,
        };
        let Some(res) = place else {
            self.error(
                ErrorCode::InferenceFailed,
                "assignment places are bindings; field assignment is undecided surface [D2]"
                    .to_owned(),
                left.span(),
                "unsupported place",
            );
            self.expr(right, None);
            return Val {
                ty: unit,
                diverges: false,
            };
        };
        let (declared, bind_span, mutable) = match &res {
            Res::Local { bind } => match self.locals.get(bind) {
                Some(binding) => (binding.ty, Some(*bind), binding.mutable),
                None => (self.empty(), None, false),
            },
            Res::Def(def) => {
                let mutable = self.package.def(*def).kind == DefKind::Var;
                let val = self.const_ty(*def, left.span());
                (val.ty, None, mutable)
            },
            _ => (self.empty(), None, false),
        };
        if !mutable {
            self.error(
                ErrorCode::TypeMismatch,
                "assignment needs a `var` binding; `const` bindings are immutable [C3]".to_owned(),
                left.span(),
                "immutable place",
            );
        }
        if op == ds_parse::AssignOp::Plain {
            let val = self.expr(right, Some(declared));
            self.expect_contains(val, declared, right.span(), "assignment");
        } else {
            let current = bind_span
                .and_then(|span| self.refined.get(&span).copied())
                .unwrap_or(declared);
            let right_val = self.expr(right, Some(current));
            let result = self.binary_val(
                compound_op(op),
                current,
                right_val.ty,
                left.span(),
                right.span(),
                span,
            );
            self.expect_contains(result, declared, span, "compound assignment");
        }
        // Refinement resets at assignment [B2].
        if let Some(bind_span) = bind_span {
            self.refined.remove(&bind_span);
        }
        Val {
            ty: unit,
            diverges: false,
        }
    }

    fn if_expr(&mut self, cond: &Expr, then: &BlockExpr, else_: Option<&Expr>) -> Val {
        let cond_val = self.expr(cond, None);
        self.expect_bool(cond_val, cond.span());
        let narrows = self.harvest(cond);
        let unit = self.unit();
        let saved = self.snapshot();
        for narrow in &narrows {
            self.apply_narrow(narrow);
        }
        let then_val = self.block(then);
        self.restore(saved);
        let else_val = match else_ {
            Some(else_expr) => {
                let saved = self.snapshot();
                for narrow in &narrows {
                    self.apply_inverse(narrow);
                }
                let val = self.expr(else_expr, None);
                self.restore(saved);
                val
            },
            None => Val {
                ty: unit,
                diverges: false,
            },
        };
        if then_val.diverges {
            return else_val;
        }
        if else_val.diverges {
            return then_val;
        }
        Val {
            ty: self.decls.universe.union(then_val.ty, else_val.ty),
            diverges: false,
        }
    }

    fn match_expr(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) -> Val {
        let scrut_val = self.expr(scrutinee, None);
        let scrut_ty = scrut_val.ty;
        let scrut_bind = self.path_local(scrutinee);
        let mut covered: Vec<TypeId> = Vec::new();
        let mut arm_vals = Vec::with_capacity(arms.len());
        // Arms type and cover sequentially: each arm sees the members
        // earlier arms left, so `s: string, i => ...` binds `i` the
        // remainder [F4 set semantics].
        let mut remaining = scrut_ty;
        for arm in arms {
            let saved = self.snapshot();
            let arm_cover = self.pattern_cover(&arm.pat, remaining, arm.ann.as_ref());
            if let Some(bind) = scrut_bind {
                let refined = self.cover_ty(&arm_cover);
                self.refined.insert(bind, refined);
            }
            let mut ann_binders = Vec::new();
            self.pattern_walk(
                &arm.pat,
                remaining,
                arm.ann.as_ref(),
                &mut Vec::new(),
                &mut ann_binders,
                0,
            );
            for (_, span, ty) in ann_binders {
                self.bind_local(span, Binding { ty, mutable: false });
            }
            if let Some(guard) = &arm.guard {
                let guard_val = self.expr(guard, None);
                self.expect_bool(guard_val, guard.span());
                for narrow in self.harvest(guard) {
                    self.apply_narrow(&narrow);
                }
            }
            let body_val = self.expr(&arm.body, None);
            self.restore(saved);
            arm_vals.push(body_val);
            if arm.guard.is_none() {
                let cover_ty = self.cover_ty(&arm_cover);
                covered.extend(arm_cover);
                remaining = self.decls.universe.diff(remaining, cover_ty);
            }
        }
        // Exhaustiveness by set difference [F4]; guards prove nothing,
        // conservatively, and neither do literal patterns.
        let missing = self.missing_members(scrut_ty, &covered);
        if !missing.is_empty() {
            let scrut_text = self.decls.universe.render(scrut_ty);
            let missing_text = self.render_members(&missing);
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::NonExhaustiveMatch,
                    format!("match on `{scrut_text}` is not exhaustive: missing: {missing_text}"),
                )
                .with_primary(span, "missing members"),
            );
        }
        let producing: Vec<&Val> = arm_vals.iter().filter(|v| !v.diverges).collect();
        if producing.is_empty() {
            return Val {
                ty: self.empty(),
                diverges: true,
            };
        }
        let mut ty = self.empty();
        for val in producing {
            ty = self.decls.universe.union(ty, val.ty);
        }
        Val {
            ty,
            diverges: false,
        }
    }

    fn for_expr(&mut self, pat: &Pattern, iter: &Expr, body: &Expr, span: Span) -> Val {
        let iter_val = self.expr(iter, None);
        let unit = self.unit();
        let members = self.decls.universe.members(iter_val.ty).to_vec();
        let element_ty = (|| {
            let single = members.first().filter(|_| members.len() == 1)?;
            let (_, args) = self.single_apply_of(*single)?;
            args.first().copied()
        })();
        let Some(element_ty) = element_ty else {
            self.error(
                ErrorCode::InferenceFailed,
                format!(
                    "iteration needs one collection type, `{}` given; the stdlib traits arrive at \
                     M3",
                    self.decls.universe.render(iter_val.ty)
                ),
                span,
                "not iterable",
            );
            return Val {
                ty: unit,
                diverges: false,
            };
        };
        let saved = self.snapshot();
        self.refined.clear();
        self.loop_depth += 1;
        self.bind_pattern(pat, element_ty, false);
        self.expr(body, None);
        self.loop_depth -= 1;
        self.restore(saved);
        Val {
            ty: unit,
            diverges: false,
        }
    }

    // ---- patterns ---------------------------------------------------------

    // ---- refinement ---------------------------------------------------------

    /// Narrows harvested from a condition: `is` tests and none-comparisons
    /// over local paths [B2].
    fn harvest(&mut self, cond: &Expr) -> Vec<Narrow> {
        let mut out = Vec::new();
        self.harvest_into(peel_paren(cond), &mut out);
        out
    }

    fn harvest_into(&mut self, cond: &Expr, out: &mut Vec<Narrow>) {
        match cond {
            Expr::Binary {
                op: BinaryOp::And,
                left,
                right,
                ..
            } => {
                self.harvest_into(peel_paren(left), out);
                self.harvest_into(peel_paren(right), out);
            },
            Expr::Binary {
                op, left, right, ..
            } if matches!(op, BinaryOp::Eq | BinaryOp::Ne) => {
                let (path_side, other) = (peel_paren(left), peel_paren(right));
                let bind = self
                    .path_local(path_side)
                    .or_else(|| self.path_local(other));
                let Some(bind) = bind else { return };
                if !is_none_expr(other) && !is_none_expr(path_side) {
                    return;
                }
                let none_ty = self.none_ty();
                out.push(Narrow {
                    bind,
                    keep: *op == BinaryOp::Eq,
                    ty: none_ty,
                });
            },
            Expr::Is { expr, ty, .. } => {
                let Some(bind) = self.path_local(peel_paren(expr)) else {
                    return
                };
                let Some(solved) = self.solve_here(ty) else {
                    return
                };
                out.push(Narrow {
                    bind,
                    keep: true,
                    ty: solved,
                });
            },
            _ => {},
        }
    }

    /// Monotone narrowing: intersect the binder's current type.
    fn apply_narrow(&mut self, narrow: &Narrow) {
        let Some(declared) = self.locals.get(&narrow.bind).map(|b| b.ty) else {
            return
        };
        let current = self.refined.get(&narrow.bind).copied().unwrap_or(declared);
        let next = if narrow.keep {
            self.decls.universe.inter(current, narrow.ty)
        } else {
            self.decls.universe.diff(current, narrow.ty)
        };
        self.refined.insert(narrow.bind, next);
    }

    fn apply_inverse(&mut self, narrow: &Narrow) {
        let inverted = Narrow {
            bind: narrow.bind,
            keep: !narrow.keep,
            ty: narrow.ty,
        };
        self.apply_narrow(&inverted);
    }

    // ---- helpers ---------------------------------------------------------

    fn member_sig(&self, method: MethodId) -> Option<FnSig> {
        self.decls
            .members
            .get(&method)
            .or_else(|| self.decls.inherent.get(&method))
            .cloned()
    }

    fn error(&mut self, code: ErrorCode, message: String, span: Span, label: &str) {
        self.diags
            .push(Diagnostic::error(code, message).with_primary(span, label));
    }

    fn expect_contains(&mut self, val: Val, expected: Ty, span: Span, what: &str) -> bool {
        if val.diverges || self.decls.universe.contains(expected, val.ty) {
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

    fn expect_bool(&mut self, val: Val, span: Span) -> bool {
        let bool_ty = self.bool_ty();
        if val.diverges || self.decls.universe.contains(bool_ty, val.ty) {
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

    fn check_args(&mut self, args: &[&Expr], param_tys: &[Option<Ty>], _span: Span) {
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

    /// Solves a written type in the current scopes: type variables
    /// innermost-first, then package atoms, aliases, and traits.
    fn solve_here(&mut self, ty: &Type) -> Option<Ty> {
        self.solve_reported = false;
        self.solve_with(ty, &HashMap::new(), &[])
    }

    /// Whether the solve that just failed already reported its precise
    /// diagnostic (the tuple distribution cap); a generic fallback then
    /// stays silent so the golden carries one code, not two.
    fn take_solve_reported(&mut self) -> bool {
        std::mem::take(&mut self.solve_reported)
    }

    fn solve_with(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Ty>,
        generics: &[GenericParam],
    ) -> Option<Ty> {
        match ty {
            Type::Path { path, args, .. } => {
                let [only] = path.segments.as_slice() else {
                    return None
                };
                // The callee's substitution binds first: a called function's
                // own variables must not be captured by the caller's
                // in-scope variables of the same name.
                if let Some(ty) = subst.get(&only.name) {
                    return Some(*ty);
                }
                for scope in self.type_vars.iter().rev() {
                    if let Some(ty) = scope.get(&only.name) {
                        return Some(*ty);
                    }
                }
                if generics.iter().any(|g| g.name == only.name) {
                    // An unbound callee variable: mint a fresh atom so the
                    // solve still produces a Ty; leaks are diagnosed at
                    // the call.
                    let var = self.mint_var(&only.name);
                    return self.decls.universe.atom_ty(var).ok();
                }
                if only.name == "none" && self.package.lookup("none").is_none() {
                    return Some(self.none_ty());
                }
                let def = self.package.lookup(&only.name)?;
                if !args.is_empty() {
                    let mut arg_tys = Vec::new();
                    for arg in args {
                        arg_tys.push(self.solve_with(arg, subst, generics)?);
                    }
                    let ctor = self.decls.atoms.get(&def).copied()?;
                    let apply = self.decls.universe.apply_atom(ctor, &arg_tys).ok()?;
                    return self.decls.universe.atom_ty(apply).ok();
                }
                match self.package.def(def).kind {
                    DefKind::Tag | DefKind::Struct | DefKind::Enum => {
                        let atom = self.decls.atoms.get(&def).copied()?;
                        self.decls.universe.atom_ty(atom).ok()
                    },
                    DefKind::TypeAlias => {
                        let (params, body) = self.decls.aliases.get(&def)?.clone();
                        if params.is_empty() {
                            self.solve_with(&body, subst, generics)
                        } else {
                            None // generic aliases resolve through use-site args
                        }
                    },
                    DefKind::Trait => {
                        let trait_id = self.decls.traits.get(&def).copied()?;
                        self.decls
                            .universe
                            .solve(&TypeExpr::trait_ref(trait_id))
                            .ok()
                    },
                    DefKind::Fn | DefKind::Const | DefKind::Var => None,
                }
            },
            Type::Optional { inner, .. } => {
                let inner = self.solve_with(inner, subst, generics)?;
                Some(self.decls.universe.optional(inner))
            },
            Type::Negation { inner, .. } => {
                let Type::Path { path, .. } = inner.as_ref() else {
                    return None
                };
                let [only] = path.segments.as_slice() else {
                    return None
                };
                let def = self.package.lookup(&only.name)?;
                let trait_id = self.decls.traits.get(&def).copied()?;
                self.decls.universe.solve(&TypeExpr::not(trait_id)).ok()
            },
            Type::Paren { inner, .. } => self.solve_with(inner, subst, generics),
            Type::Union { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.union(a, b))
            },
            Type::Intersect { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.inter(a, b))
            },
            Type::Difference { left, right, .. } => {
                let (a, b) = (
                    self.solve_with(left, subst, generics)?,
                    self.solve_with(right, subst, generics)?,
                );
                Some(self.decls.universe.diff(a, b))
            },
            Type::Tuple { items, .. } => {
                let mut elements = Vec::with_capacity(items.len());
                for item in items {
                    elements.push(self.solve_with(item, subst, generics)?);
                }
                match self.decls.universe.product(&elements) {
                    Ok(ty) => Some(ty),
                    Err(err @ Error::TupleDistributionCap { .. }) => {
                        self.error(
                            ErrorCode::TupleDistributionCap,
                            err.to_string(),
                            ty.span(),
                            "tuple distribution cap",
                        );
                        self.solve_reported = true;
                        None
                    },
                    Err(_) => None,
                }
            },
            Type::Unit(_) => Some(self.unit()),
            Type::Fn { params, ret, .. } => {
                let mut param_tys = Vec::with_capacity(params.len());
                for param in params {
                    // A bare fn-type parameter (`fn(t) -> r`) names the
                    // in-scope variable, per the surface spelling.
                    let written = param
                        .ty
                        .clone()
                        .unwrap_or_else(|| name_type(&param.name.name));
                    param_tys.push(self.solve_with(&written, subst, generics)?);
                }
                let ret_ty = match ret {
                    Some(ret) => self.solve_with(ret, subst, generics)?,
                    None => self.unit(),
                };
                let atom = self.decls.universe.fn_atom(&param_tys, ret_ty).ok()?;
                self.decls.universe.atom_ty(atom).ok()
            },
            Type::Ptr { .. } | Type::TypeKw(_) | Type::Foreign { .. } => None,
        }
    }

    /// Structural parameter-to-argument matching for generic
    /// instantiation: binds callee variables by walking parameter shapes
    /// against the argument's solved type.
    fn match_param(
        &mut self,
        param: &Type,
        arg: Ty,
        generics: &[GenericParam],
        subst: &mut HashMap<String, Ty>,
    ) {
        match param {
            Type::Paren { inner, .. } => self.match_param(inner, arg, generics, subst),
            Type::Path { path, args, .. } => {
                let [only] = path.segments.as_slice() else {
                    return
                };
                if args.is_empty() {
                    if generics.iter().any(|g| g.name == only.name) {
                        subst.entry(only.name.clone()).or_insert(arg);
                    }
                    return;
                }
                let members = self.decls.universe.members(arg).to_vec();
                let Some(single) = members.first().filter(|_| members.len() == 1) else {
                    return;
                };
                let Some((ctor, apply_args)) = self.single_apply_of(*single) else {
                    return
                };
                // The parameter's constructor must be the argument's.
                let Some(def) = self.package.lookup(&only.name) else {
                    return
                };
                if self.decls.atoms.get(&def) != Some(&ctor) {
                    return;
                }
                for (param_arg, arg_ty) in args.iter().zip(apply_args.iter()) {
                    self.match_param(param_arg, *arg_ty, generics, subst);
                }
            },
            Type::Optional { inner, .. } => {
                let none = self.none_ty();
                let without_none = self.decls.universe.diff(arg, none);
                self.match_param(inner, without_none, generics, subst);
            },
            Type::Tuple { items, .. } => {
                let members = self.decls.universe.members(arg).to_vec();
                for member in members {
                    if let Some(elements) = self.tuple_elements(member) {
                        if elements.len() != items.len() {
                            continue;
                        }
                        for (item, element) in items.iter().zip(elements.iter()) {
                            if let Ok(ty) = self.decls.universe.atom_ty(*element) {
                                self.match_param(item, ty, generics, subst);
                            }
                        }
                    }
                }
            },
            Type::Fn { params, ret, .. } => {
                let members = self.decls.universe.members(arg).to_vec();
                let Some(single) = members.first().filter(|_| members.len() == 1) else {
                    return
                };
                let Some((param_tys, ret_ty)) = self.decompose_fn_atom(*single) else {
                    return
                };
                for (param, arg_ty) in params.iter().zip(param_tys.iter()) {
                    if let Some(ty) = &param.ty {
                        self.match_param(ty, *arg_ty, generics, subst);
                    }
                }
                if let Some(ret) = ret {
                    self.match_param(ret, ret_ty, generics, subst);
                }
            },
            _ => {},
        }
    }

    fn trait_of_type(&self, ty: &Type) -> Option<TraitId> {
        let Type::Path { path, .. } = ty else {
            return None
        };
        let [only] = path.segments.as_slice() else {
            return None
        };
        let def = self.package.lookup(&only.name)?;
        self.decls.traits.get(&def).copied()
    }

    /// Whether a type is exactly one minted type variable (the checker's
    /// `$name` atoms): an inference hole, not a data type.
    fn is_var_ty(&self, ty: Ty) -> bool {
        match self.decls.universe.members(ty) {
            [only] => self
                .decls
                .universe
                .atom_decl(*only)
                .is_ok_and(|decl| decl.name.starts_with('$')),
            _ => false,
        }
    }

    /// Whether a trait holds for a type: an in-scope assumption, or a
    /// unique containing instance head [B6]. Diagnoses on failure.
    fn trait_satisfied(&mut self, trait_id: TraitId, ty: Ty, span: Span) -> bool {
        if self.trait_satisfied_quiet(trait_id, ty) {
            return true;
        }
        self.error(
            ErrorCode::UnsatisfiedConstraint,
            format!(
                "no instance of `{}` for `{}`",
                self.trait_name(trait_id),
                self.decls.universe.render(ty)
            ),
            span,
            "unimplemented trait",
        );
        false
    }

    fn trait_satisfied_quiet(&self, trait_id: TraitId, ty: Ty) -> bool {
        if self
            .assumptions
            .iter()
            .rev()
            .any(|(t, assumed)| *t == trait_id && self.decls.universe.contains(*assumed, ty))
        {
            return true;
        }
        matches!(
            self.decls
                .instances
                .resolve_unique(&self.decls.universe, trait_id, ty),
            Ok(Some(_))
        )
    }

    fn trait_name(&self, trait_id: TraitId) -> String {
        self.decls
            .universe
            .trait_decl(trait_id)
            .map_or_else(|_| "<unknown>".to_owned(), |d| d.name.clone())
    }

    fn push_generics(&mut self, generics: &[GenericParam]) {
        let mut scope = HashMap::new();
        self.assumption_marks.push(self.assumptions.len());
        for param in generics {
            let var = self.mint_var(&param.name);
            if let Ok(ty) = self.decls.universe.atom_ty(var) {
                scope.insert(param.name.clone(), ty);
                if param.bound.as_ref().is_some_and(decls::linear_marker) {
                    self.linear_vars.insert(var, false);
                }
                if let Some(trait_id) = param.bound.as_ref().and_then(|b| self.trait_of_type(b)) {
                    self.assumptions.push((trait_id, ty));
                }
            }
        }
        self.type_vars.push(scope);
    }

    fn pop_generics(&mut self) {
        self.type_vars.pop();
        if let Some(mark) = self.assumption_marks.pop() {
            self.assumptions.truncate(mark);
        }
    }

    fn mint_var(&mut self, name: &str) -> TypeId {
        *self.counter += 1;
        let unique = format!("${name}#{}", self.counter);
        self.decls
            .universe
            .declare_tag(&unique)
            .unwrap_or(TypeId::NONE)
    }

    fn fresh_var(&mut self, name: &str) -> Ty {
        let var = self.mint_var(name);
        self.decls
            .universe
            .atom_ty(var)
            .unwrap_or_else(|_| self.empty())
    }

    fn snapshot(&self) -> (HashMap<Span, Binding>, HashMap<Span, Ty>, usize) {
        (
            self.locals.clone(),
            self.refined.clone(),
            self.assumptions.len(),
        )
    }

    fn restore(&mut self, saved: (HashMap<Span, Binding>, HashMap<Span, Ty>, usize)) {
        self.locals = saved.0;
        self.refined = saved.1;
        self.assumptions.truncate(saved.2);
    }

    fn local_ty(&self, bind: Span) -> Option<Ty> {
        self.refined
            .get(&bind)
            .copied()
            .or_else(|| self.locals.get(&bind).map(|b| b.ty))
    }

    /// The binder span when an expression is a local path.
    fn path_local(&self, expr: &Expr) -> Option<Span> {
        let Expr::Path { path, .. } = peel_paren(expr) else {
            return None
        };
        let [only] = path.segments.as_slice() else {
            return None
        };
        match self.res.get(only.span) {
            Some(Res::Local { bind }) => Some(*bind),
            _ => None,
        }
    }

    fn cover_ty(&mut self, covered: &[TypeId]) -> Ty {
        let mut ty = self.empty();
        for atom in covered {
            if let Ok(atom_ty) = self.decls.universe.atom_ty(*atom) {
                ty = self.decls.universe.union(ty, atom_ty);
            }
        }
        ty
    }

    fn missing_members(&mut self, scrut: Ty, covered: &[TypeId]) -> Vec<TypeId> {
        let covered_ty = self.cover_ty(covered);
        let missing = self.decls.universe.diff(scrut, covered_ty);
        self.decls.universe.members(missing).to_vec()
    }

    fn render_members(&mut self, members: &[TypeId]) -> String {
        let ty = self.cover_ty(members);
        format!("`{}`", self.decls.universe.render(ty))
    }

    fn struct_shape_of(&self, atom: TypeId) -> Option<&decls::StructShape> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        self.decls.structs.get(&def)
    }

    /// Payload types of a struct (named fields, positionally matched) or an
    /// enum case, for call patterns.
    fn payload_tys(&mut self, path: &Path, atom: TypeId) -> Option<Vec<Ty>> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        if let Some(shape) = self.decls.structs.get(&def).cloned() {
            let mut out = Vec::with_capacity(shape.fields.len());
            for field in &shape.fields {
                out.push(self.solve_here(&field.ty)?);
            }
            return Some(out);
        }
        if let Some(shape) = self.decls.enums.get(&def).cloned() {
            let tail = path.segments.last()?;
            let case = shape.cases.iter().find(|(name, ..)| name == &tail.name)?;
            let payload = match &case.2 {
                EnumPayload::None => Vec::new(),
                EnumPayload::Tuple(types) => {
                    let mut out = Vec::with_capacity(types.len());
                    for ty in types {
                        out.push(self.solve_here(ty)?);
                    }
                    out
                },
                EnumPayload::Struct(fields) => {
                    let mut out = Vec::with_capacity(fields.len());
                    for field in fields {
                        out.push(self.solve_here(&field.ty)?);
                    }
                    out
                },
            };
            return Some(payload);
        }
        None
    }

    fn atom_name(&self, atom: TypeId) -> String {
        self.decls
            .universe
            .atom_decl(atom)
            .map_or_else(|_| "<unknown>".to_owned(), |d| d.name.clone())
    }

    /// The application shape of a singleton type, as constructor plus
    /// argument types.
    fn single_apply_of(&self, atom: TypeId) -> Option<(TypeId, Vec<Ty>)> {
        match self.decls.universe.atom_decl(atom).ok()?.kind {
            AtomKind::Apply(ctor, ref args) => Some((ctor, args.to_vec())),
            _ => None,
        }
    }

    /// The application shape of a whole type, when it is one application.
    fn single_apply(&mut self, ty: Ty) -> Option<(TypeId, Vec<Ty>)> {
        let members = self.decls.universe.members(ty).to_vec();
        let [only] = members.as_slice() else {
            return None
        };
        self.single_apply_of(*only)
    }

    fn decompose_fn(&mut self, ty: Ty) -> Option<(Vec<Ty>, Ty)> {
        let members = self.decls.universe.members(ty).to_vec();
        let [only] = members.as_slice() else {
            return None
        };
        self.decompose_fn_atom(*only)
    }

    fn decompose_fn_atom(&self, atom: TypeId) -> Option<(Vec<Ty>, Ty)> {
        let (params, ret) = self.decls.universe.fn_sig(atom)?;
        Some((params.to_vec(), ret))
    }

    fn fn_typed(&self, ty: Ty) -> bool {
        self.decls
            .universe
            .members(ty)
            .iter()
            .any(|atom| self.decls.universe.fn_sig(*atom).is_some())
    }

    fn tuple_elements(&self, atom: TypeId) -> Option<Vec<TypeId>> {
        match self.decls.universe.atom_decl(atom).ok()?.kind {
            AtomKind::Tuple(ref elements) => Some(elements.clone()),
            _ => None,
        }
    }

    fn named_atom_ty(&mut self, span: Span, name: &str, what: &str) -> Option<Ty> {
        let ty = self
            .decls
            .universe
            .atom_by_name(name)
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok());
        if ty.is_none() {
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{what}` needs a declared `{name}` type [B1]"),
                span,
                "no such type",
            );
        }
        ty
    }

    fn is_string_ty(&self, ty: Ty) -> bool {
        // By name through the atom table: `atom_ty` interns and needs
        // exclusivity this shared-context check does not have.
        match self.decls.universe.members(ty) {
            [only] => self
                .decls
                .universe
                .atom_decl(*only)
                .is_ok_and(|decl| decl.name == "string"),
            _ => false,
        }
    }

    fn none_ty(&mut self) -> Ty {
        self.decls
            .universe
            .atom_ty(TypeId::NONE)
            .unwrap_or_else(|_| self.empty())
    }

    fn bool_ty(&mut self) -> Ty {
        self.decls
            .universe
            .atom_by_name("bool")
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty())
    }

    fn unit(&mut self) -> Ty {
        self.decls.universe.unit_ty()
    }

    fn empty(&mut self) -> Ty {
        self.decls.universe.empty_ty()
    }

    fn fn_atom_of(&mut self, sig: &FnSig) -> Val {
        let mut param_tys = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            param_tys.push(
                param
                    .ty
                    .as_ref()
                    .and_then(|t| self.solve_here(t))
                    .unwrap_or_else(|| self.empty()),
            );
        }
        let ret = sig
            .ret
            .as_ref()
            .and_then(|r| self.solve_here(r))
            .unwrap_or_else(|| self.unit());
        let params_boxed: Box<[Ty]> = param_tys.into();
        let ty = self
            .decls
            .universe
            .fn_atom(&params_boxed, ret)
            .ok()
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
        }
    }

    fn span_text(&self, span: Span) -> String {
        self.files
            .source(span.file)
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
            .to_owned()
    }
}

// ---- free helpers -----------------------------------------------------------

fn generic_of(param: &TypeParam) -> GenericParam {
    GenericParam {
        name: param.name.name.clone(),
        span: param.name.span,
        bound: param.bound.clone(),
    }
}

fn param_is_mut(param: &Param) -> bool {
    !matches!(param.mode, ds_parse::ParamMode::Borrow)
}

/// A synthetic path type naming one identifier — the written form of a
/// bare fn-type parameter (`fn(t) -> r`), resolved through scopes like any
/// written type.
fn name_type(name: &str) -> Type {
    let span = Span::new(ds_diag::FileId::new(0), 0, 0);
    Type::Path {
        span,
        path: Path {
            span,
            segments: vec![Ident {
                span,
                name: name.to_owned(),
            }],
        },
        args: Vec::new(),
    }
}

fn peel_paren(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => peel_paren(inner),
        other => other,
    }
}

fn is_none_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Path { path, .. }
            if path.segments.len() == 1 && path.segments[0].name == "none"
    )
}
