//! The `type_of` pass: bidirectional checking with monotone flow refinement
//! (B2).
//!
//! # Engine shape
//!
//! Boundary anchoring per (B2): top-level signatures are REQUIRED and the
//! set algebra checks against them by containment; locals, lambdas, and
//! match arms infer. Inference failure is a compile error — there is no
//! `any` fallback anywhere (B1). Every type is a solved [`Ty`] from the
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
//! and the arm's pattern coverage) (B2). The overlay resets at assignment
//! and at loop entry (sound: refined is always contained in declared, so
//! the declared type is a legal widening). Known limit, from the record:
//! no narrowing through mutable fields of union-typed values.
//!
//! # E4 union errors
//!
//! `?` splits its operand against the success context: at an expected type
//! the members outside it propagate, elsewhere the members INSIDE the
//! enclosing return union propagate — either way every propagated member
//! must be contained in the written return union (DS2008). `or` extracts
//! the wanted members (the right operand's type for a plain tail, the
//! return union for `or return`); `.err(h)`/`.ok(h)` are difference-typed:
//! the handled set is the handler parameter when annotated, otherwise
//! everything but what the handler's own result keeps.
//!
//! # Out of scope here
//!
//! Effect rows; the affine/move pass (assumption bounds like `[t: json]`
//! feed method selection, `~Copyable` is M4); ranges, indexing, and
//! `in`-containment (they need the M3 stdlib's type vocabulary).

use std::{collections::HashMap, sync::Arc};

use ds_diag::{Diagnostic, ErrorCode, Files, Span};
use ds_parse::{
    Ast, BinaryOp, BlockExpr, EnumPayload, Expr, FnDecl, Ident, Item, ItemKind, Literal, LocalDecl,
    MatchArm, Param, Path, Pattern, Stmt, StrPart, TraitMember, Type, TypeParam, UnaryOp,
};

use crate::{
    TypeExpr,
    atom::{AtomKind, TraitId, TypeId},
    decls::{self, Decls, FnSig, GenericParam, ParamSig},
    error::Error,
    extraction::Extraction,
    instance::InstanceId,
    instance_solve::Resolution,
    resolve::{DefId, DefKind, DotRes, MethodId, MethodOwner, Package, Res, Resolutions},
    universe::Ty,
};

/// A typed expression: its solved type, whether evaluation always exits
/// early (return/break/continue), so arm unification can skip it, and
/// whether its own subtree already reported a diagnostic.
#[derive(Clone, Copy, Debug)]
pub struct Val {
    /// The solved type of the expression.
    pub ty: Ty,
    /// Whether every path through the expression exits early.
    pub diverges: bool,
    /// Whether any diagnostic was reported inside this expression: the
    /// expression-error epoch. Enclosing positions consume a poisoned
    /// value without re-reporting, so the root cause stands alone;
    /// sibling positions check their own subtree and stay honest.
    pub poisoned: bool,
}

/// One local binding: declared type plus assignability (C3).
#[derive(Clone, Copy, Debug)]
struct Binding {
    ty: Ty,
    mutable: bool,
}

/// One scope boundary's rollback point: both binder tables' undo-log
/// marks, the assumption stack's length, and the enclosing return union.
/// Cheap by construction — restoring replays only what the scope changed.
#[derive(Clone, Copy)]
struct ScopeMark {
    locals: usize,
    refined: usize,
    assumptions: usize,
    ret: Option<Ty>,
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
/// memoized type of every top-level binding (G3).
#[derive(Default)]
pub struct Checked {
    /// All diagnostics of collection, resolution, declaration, and bodies.
    pub diags: Vec<Diagnostic>,
    /// The solved type of every top-level const/var binding.
    pub def_types: HashMap<DefId, Ty>,
    /// Affine facts of every local binder, keyed by binder span: the
    /// copyability its type had at the binding site (C1). The linearity
    /// pass consumes this; top-level bindings derive theirs from
    /// `def_types` directly.
    pub binders: HashMap<Span, crate::linearity::BinderFact>,
    /// The checked `Val` of every expression, keyed by its span: the solved
    /// type AT THAT USE SITE (flow-refined inside `is` tests and match arms
    /// (B2)) plus the early-exit flag. The lowering pass (M3) consumes this
    /// side table instead of re-deriving types; entries are recorded in
    /// walk order and a re-walk overwrites them.
    pub expr_types: HashMap<Span, Val>,
    /// The instantiation of every generic call, keyed by the call's span:
    /// the solved type of each generic parameter, in declaration order
    /// (H3). Recorded only when every parameter solves (no `any` fallback
    /// (B1)); the lowering pass (M3) mints the CALLG evidence words from
    /// it.
    pub generic_calls: HashMap<Span, Vec<Ty>>,
    /// The solved dispatch of every trait-method call whose instance the
    /// environment statically resolved, keyed by the call's span (B6).
    /// The lowering pass mints the CALLG evidence word from it.
    pub trait_calls: HashMap<Span, TraitCall>,
}

/// One trait-method call's solved dispatch (B6): the selected trait member
/// plus the unique instance whose head contains the receiver's type,
/// recorded at the call's own span. A site satisfied only through an
/// in-scope assumption (the receiver's type is a bound type variable)
/// records nothing — that site is the runtime-resolved evidence form, and
/// its dispatch waits for the runtime-tag shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TraitCall {
    /// The selected trait member.
    pub method: MethodId,
    /// The trait the member belongs to.
    pub trait_id: TraitId,
    /// The unique containing instance for the receiver's type.
    pub instance: InstanceId,
    /// The declaring impl item's span, when the package recorded one; the
    /// impl's bodies live in that item.
    pub impl_site: Option<Span>,
    /// The receiver's solved type at the call site.
    pub recv: Ty,
    /// The runtime evidence a call into a generic impl's body needs (H3):
    /// one entry per declared bound of the solved instance — the bound's
    /// trait, the type variable it constrains, and the unique instance
    /// serving the substituted type at this call site (`None` when none
    /// serves it uniquely). Empty for monomorphic instances, whose
    /// bodies need no evidence.
    pub bounds: Vec<(TypeId, TraitId, Option<InstanceId>)>,
}

mod impl_sig;
mod ops;
use ops::compound_op;
mod pattern;
mod scope;
use scope::ScopedMap;

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
/// affine pass (C1) and the lowering pass (M3) consume exactly these.
pub fn analyze(files: &Files, asts: &[Ast]) -> Analyzed {
    let (package, resolutions, mut diags) = crate::resolve_package(asts);
    let (decls, checked) = check_bodies(files, asts, &package, &resolutions);
    diags.extend(checked.diags);
    let checked = Checked {
        diags,
        def_types: checked.def_types,
        binders: checked.binders,
        expr_types: checked.expr_types,
        generic_calls: checked.generic_calls,
        trait_calls: checked.trait_calls,
    };
    Analyzed {
        package,
        resolutions,
        decls,
        checked,
    }
}

/// Checks a resolved package (B2): builds the declaration tables and
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
    let res_by_file: HashMap<ds_diag::FileId, usize> = trees
        .iter()
        .zip(resolutions.iter().enumerate())
        .map(|(ast, (index, _))| (ast.file, index))
        .collect();
    let mut def_types = HashMap::new();
    let mut binders = HashMap::new();
    let mut expr_types = HashMap::new();
    let mut generic_calls = HashMap::new();
    let mut trait_calls = HashMap::new();
    for (ast, res) in trees.iter().zip(resolutions) {
        let mut bodies = Bodies {
            files,
            package,
            res,
            resolutions,
            res_by_file: &res_by_file,
            decls: &mut decls,
            def_types: &mut def_types,
            binders: &mut binders,
            expr_types: &mut expr_types,
            generic_calls: &mut generic_calls,
            trait_calls: &mut trait_calls,
            diags: Vec::new(),
            linear_vars: HashMap::new(),
            locals: ScopedMap::default(),
            refined: ScopedMap::default(),
            type_vars: Vec::new(),
            assumptions: Vec::new(),
            assumption_marks: Vec::new(),
            ret: None,
            loop_depth: 0,
            cond_depth: 0,
            visiting: Vec::new(),
            solve_reported: false,
        };
        bodies.file(ast);
        diags.append(&mut bodies.diags);
    }
    (decls, Checked {
        diags,
        def_types,
        binders,
        expr_types,
        generic_calls,
        trait_calls,
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
    /// Every file's resolutions, aligned with the trees by position; a
    /// lazily typed top-level binding walks under its OWN file's table
    /// (G3), no matter which file's body triggered the memo fill.
    resolutions: &'a [Resolutions],
    res_by_file: &'a HashMap<ds_diag::FileId, usize>,
    decls: &'a mut Decls,
    def_types: &'a mut HashMap<DefId, Ty>,
    binders: &'a mut HashMap<Span, crate::linearity::BinderFact>,
    expr_types: &'a mut HashMap<Span, Val>,
    generic_calls: &'a mut HashMap<Span, Vec<Ty>>,
    trait_calls: &'a mut HashMap<Span, TraitCall>,
    diags: Vec<Diagnostic>,
    /// Minted type variables whose generic parameter subtracts the
    /// copyability constraint (`[t: ~Copyable]`) (C1); the binder facts
    /// consult them while recording.
    linear_vars: HashMap<TypeId, bool>,
    /// Declared types of locals, keyed by binder span; snapshot/restore
    /// at scope boundaries runs through its undo log, never a clone.
    locals: ScopedMap<Binding>,
    /// The refinement overlay, keyed by binder span (B2).
    refined: ScopedMap<Ty>,
    /// Type-variable scopes, innermost last (generic parameters, `Self`).
    type_vars: Vec<HashMap<String, Ty>>,
    /// In-scope trait assumptions from generic bounds (`[t: json]`), with
    /// per-scope truncation marks.
    assumptions: Vec<(TraitId, Ty)>,
    assumption_marks: Vec<usize>,
    /// The enclosing return union; `None` outside function bodies.
    ret: Option<Ty>,
    loop_depth: usize,
    /// Depth of condition positions being walked (if/match-guard/while
    /// conditions). An `is` test there refines legitimately — a disjoint
    /// test narrows a branch to empty, which is sound dead code — so the
    /// always-false diagnostic fires only at value/assert depth.
    cond_depth: usize,
    /// Cycle guard for order-independent top-level typing (G3).
    visiting: Vec<DefId>,
    /// Whether the failed solve just below already reported the tuple
    /// distribution cap precisely; the generic fallbacks around
    /// `solve_here` consume it so DS2012 never layers under another code.
    solve_reported: bool,
}

impl Bodies<'_> {
    /// Binds one local and records its affine facts (C1): copyability of
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
                let self_ty = self.fresh_var("Self");
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.fn_decl(f, Some(self_ty));
                    }
                }
                self.pop_generics();
            },
            ItemKind::Impl(imp) => {
                self.impl_item(item, imp);
            },
            ItemKind::Local(local) => self.top_level_local(local),
            ItemKind::Struct(s) => self.struct_item(s),
            ItemKind::ExternFn(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Import(_) => {},
        }
    }

    /// Type-checks every declared `deinit` body (C5): `self` is the
    /// dying instance, borrowed, typed the struct's own atom; the body
    /// types as a unit-returning function body. The recorded facts —
    /// the `self` binder at the member's span, the body's expression
    /// types — are what the deinit's runtime half consumes.
    fn struct_item(&mut self, s: &ds_parse::StructDecl) {
        let Some(def) = self.package.lookup_type(&s.name.name) else {
            return;
        };
        let Some(atom) = self.decls.atoms.get(&def).copied() else {
            return;
        };
        let generics: Vec<GenericParam> = s.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        for member in &s.members {
            let ds_parse::StructMember::Deinit { span, body } = member else {
                continue;
            };
            let Ok(self_ty) = self.decls.universe.atom_ty(atom) else {
                continue;
            };
            let saved = self.snapshot();
            self.locals.clear();
            self.refined.clear();
            self.bind_local(*span, Binding {
                ty: self_ty,
                mutable: false,
            });
            let unit = self.unit();
            let old_ret = self.ret;
            self.ret = Some(unit);
            let block = Expr::Block(body.clone());
            let val = self.expr(&block, Some(unit));
            if !val.diverges {
                self.expect_contains(val, unit, body.span, "a deinit body");
            }
            self.ret = old_ret;
            self.restore(saved);
        }
        self.pop_generics();
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
        let trait_def = imp.trait_ref.as_ref().and_then(|tr| {
            let [only] = tr.path.segments.as_slice() else {
                return None
            };
            self.package.lookup_type(&only.name)
        });
        let member_names: Option<Vec<String>> =
            trait_def.and_then(|def| self.decls.trait_members.get(&def).cloned());
        // `Self` resolves to the impl's target while the written member
        // signatures compare (B6).
        self.type_vars
            .push(HashMap::from([("Self".to_owned(), target)]));
        let subst = self.trait_arg_subst(imp, trait_def);
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
                if let (Some(def), Some(subst)) = (trait_def, &subst) {
                    if let Some((sig, sig_span)) =
                        decls::trait_member_sig(self.package, self.decls, def, &f.name.name)
                    {
                        impl_sig::agree_member_signature(self, f, &sig, sig_span, subst);
                    }
                }
            }
            self.fn_decl(f, Some(target));
        }
        self.type_vars.pop();
        self.pop_generics();
    }

    /// The trait's generic parameters substituted by the impl's trait
    /// reference, positionally (B6): every name the trait declares maps
    /// to the argument the reference spells. A trait declaring generics
    /// the reference does not spell reports once — its member signatures
    /// could not be spelled in the impl's scope at all.
    fn trait_arg_subst(
        &mut self,
        imp: &ds_parse::ImplDecl,
        trait_def: Option<DefId>,
    ) -> Option<HashMap<String, Type>> {
        let reference = imp.trait_ref.as_ref()?;
        let def = trait_def?;
        let names = self.decls.trait_generics.get(&def)?.clone();
        if names.len() != reference.args.len() {
            self.error(
                ErrorCode::ImplMemberSignatureMismatch,
                format!(
                    "the trait declares {} generic parameter(s); the impl's trait reference \
                     spells {} argument(s)",
                    names.len(),
                    reference.args.len()
                ),
                reference.span,
                "unsubstituted trait generics",
            );
            return None;
        }
        Some(
            names
                .into_iter()
                .zip(reference.args.iter().cloned())
                .collect(),
        )
    }

    /// A top-level binding: its initializer is typed (memoized for
    /// order-independent uses (G3)); a written annotation anchors the check.
    /// A destructuring pattern declares many defs — the memo and the cycle
    /// guard cover them all, so tuple/or/struct binders receive types like
    /// any other top-level name.
    fn top_level_local(&mut self, local: &LocalDecl) {
        let defs: Vec<DefId> = crate::pattern::pattern_binders(&local.pat)
            .into_iter()
            .filter_map(|(_, name)| self.package.lookup_value(name))
            .collect();
        if defs.is_empty() {
            return;
        }
        if defs.iter().all(|def| self.def_types.contains_key(def))
            || defs.iter().any(|def| self.visiting.contains(def))
        {
            return;
        }
        let visiting = self.visiting.len();
        self.visiting.extend(defs.iter().copied());
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;
        // A use in another file's body can trigger this lazy fill; the
        // initializer's spans resolve under the binding's OWN file table,
        // never the triggering file's [G3].
        let home_res = self.res;
        if let Some(&index) = self.res_by_file.get(&self.package.def(defs[0]).span.file) {
            if let Some(res) = self.resolutions.get(index) {
                self.res = res;
            }
        }

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
        self.res = home_res;
        self.visiting.truncate(visiting);
    }

    /// One named function declaration: boundary-anchored (B2). `self_ty`
    /// binds `Self` inside trait/impl scopes, where the first parameter
    /// named `self` is the receiver — exempt from annotation and typed
    /// `Self` [record 6.1].
    fn fn_decl(&mut self, f: &FnDecl, self_ty: Option<Ty>) {
        let generics: Vec<GenericParam> = f.generics.iter().map(generic_of).collect();
        self.push_generics(&generics);
        if let Some(target) = self_ty {
            self.type_vars
                .push(HashMap::from([("Self".to_owned(), target)]));
        }
        let saved = self.snapshot();
        self.locals.clear();
        self.refined.clear();
        self.ret = None;
        for (index, param) in f.params.iter().enumerate() {
            if index == 0 && self_ty.is_some() && param.name.name == "self" {
                if let Some(ann) = &param.ty {
                    self.error(
                        ErrorCode::AnnotatedReceiver,
                        "the receiver is spelled `self` or `mut self` with no annotation; it is \
                         typed `Self`"
                            .to_owned(),
                        ann.span(),
                        "annotated receiver",
                    );
                }
                let recv_ty = self_ty.unwrap_or_else(|| self.empty());
                self.bind_local(param.name.span, Binding {
                    ty: recv_ty,
                    mutable: param_is_mut(param),
                });
                continue;
            }
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
        let mut body_val: Option<Val> = None;
        if let Some(body) = &f.body {
            let val = self.block_expected(body, ret);
            if let Some(ret) = ret {
                if !val.diverges {
                    self.expect_contains(val, ret, body.span, "function body");
                }
            }
            body_val = Some(val);
        }
        // An impl-member body's solved return rides the declaration
        // tables for the lowering pass: a member function has no package
        // DefId, so its solved signature lives here — the written return
        // against `Self`, or the body's own checked type when inferred.
        if self_ty.is_some() {
            let solved = ret.or_else(|| body_val.as_ref().filter(|v| !v.poisoned).map(|v| v.ty));
            if let Some(ty) = solved {
                self.decls.impl_fn_rets.insert(f.span, ty);
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
        self.block_expected(block, None)
    }

    /// A block whose final expression carries an expected type — the
    /// function-body anchor: a tail `?` splits its operand against the
    /// return union there, so a fully-contained operand yields itself
    /// instead of propagating everything (E4).
    fn block_expected(&mut self, block: &BlockExpr, expected: Option<Ty>) -> Val {
        let saved = self.snapshot();
        let mut val = Val {
            ty: self.unit(),
            diverges: false,
            poisoned: false,
        };
        let (head, last) = match block.stmts.split_last() {
            Some((last, head)) => (head, Some(last)),
            None => (&block.stmts[..], None),
        };
        for stmt in head {
            val = self.stmt(stmt);
        }
        match last {
            // Only an expression tail carries the expected type (the
            // function-body anchor); a binding tail still checks, its
            // block value stays unit [F2].
            Some(Stmt {
                kind: ds_parse::StmtKind::Expr(expr),
                ..
            }) => val = self.expr(expr, expected),
            Some(other) => val = self.stmt(other),
            None => {},
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
                    poisoned: false,
                }
            },
            ds_parse::StmtKind::Expr(expr) => self.expr(expr, None),
        }
    }

    /// A statement-level binding: locals infer (B2).
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
        // A poisoned initializer already reported why its type is empty;
        // only a clean-but-empty initializer is its own root cause.
        if self.decls.universe.is_empty(val.ty)
            && !val.diverges
            && !val.poisoned
            && !self.take_solve_reported()
        {
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
        // Expression-error epoch: a diagnostic pushed anywhere inside this
        // expression poisons its value — every enclosing position consumes
        // it without re-reporting (the containment helpers and the
        // operand-shape checks), so a root cause stands alone instead of
        // cascading into "empty"/wrapper-type mismatches above it.
        let before = self.diags.len();
        let mut val = self.expr_inner(e, expected);
        val.poisoned |= self.diags.len() > before;
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
            },
            Expr::List { .. } => self.list_expr(e, expected),
            Expr::Paren { inner, .. } => self.expr(inner, expected),
            Expr::Call { .. } => self.call(e),
            Expr::Field { recv, name, .. } => self.field_access(recv, name),
            Expr::Index { span, recv, .. } => {
                if let Some(head) = single_path_ident(recv) {
                    if self.package.lookup_type(&head.name).is_some() {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "`{}` is a type; the application `{}[...]` constructs only in \
                                 call position — `{name}[t]()` [F9, B3]",
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
                }
                self.error(
                    ErrorCode::InferenceFailed,
                    "index typing arrives with the stdlib collection types [F5, M3]".to_owned(),
                    *span,
                    "index",
                );
                Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                }
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
            Expr::Is { expr, ty, .. } => {
                let operand = self.expr(expr, None);
                let Some(solved) = self.solve_here(ty) else {
                    if !self.take_solve_reported() {
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
                    return Val {
                        ty: self.bool_ty(),
                        diverges: false,
                        poisoned: false,
                    };
                };
                // A provably-disjoint test computes a constant false, so in
                // value/assert position it is a compile error — the same
                // ruling as the constant `==` [B9 carve-out 2]. In
                // condition position (if/match guard/while, any depth) the
                // test refines legitimately: the kept branch narrows to
                // empty, which is sound dead code, and a match arm covering
                // nothing is the exhaustiveness story.
                if self.cond_depth == 0
                    && !operand.poisoned
                    && !self.decls.universe.is_empty(operand.ty)
                    && !self.decls.universe.is_empty(solved)
                    && self.decls.universe.is_disjoint(operand.ty, solved)
                {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the `is` test against `{}` is always false over `{}` [B2]",
                            self.decls.universe.render(solved),
                            self.decls.universe.render(operand.ty)
                        ),
                        ty.span(),
                        "always false",
                    );
                }
                Val {
                    ty: self.bool_ty(),
                    diverges: false,
                    poisoned: false,
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
            } => self.if_expr(cond, then, else_.as_deref(), expected),
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.match_expr(scrutinee, arms, *span, expected),
            Expr::While { cond, body, .. } => {
                self.cond_depth += 1;
                let cond_val = self.expr(cond, None);
                self.cond_depth -= 1;
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
                    poisoned: false,
                }
            },
            Expr::For {
                pat,
                iter,
                body,
                span,
            } => self.for_expr(pat, iter, body, *span),
            Expr::Return { expr, span } => {
                // The returned expression carries the declared return as
                // its expected type, matching the final-expression anchor:
                // a `?` operand splits against the return union here too
                // [E4].
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
                    poisoned: false,
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
                    poisoned: false,
                }
            },
            Expr::Defer { body, .. } => {
                self.block(body);
                Val {
                    ty: self.unit(),
                    diverges: false,
                    poisoned: false,
                }
            },
            Expr::Block(block) => self.block_expected(block, expected),
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
    /// (B7): the numeric vocabulary is declared data, never builtin
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

    /// Interpolated strings: every hole is an explicit string context
    /// (F6) — a printable primitive (bool, char, float, int, string)
    /// converts through `str(e)` implicitly; every other operand needs
    /// extraction or the `show` derive (the M6 seam).
    fn string_expr(&mut self, span: Span, parts: &[StrPart]) -> Val {
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
                StrPart::Expr { span, expr } => (*span, self.expr(expr, Some(string_ty))),
                StrPart::Ident { name, .. } => {
                    let val = self.ident_hole_value(name);
                    self.expr_types.insert(name.span, val);
                    (name.span, val)
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
            let hole_ty = hole_val.ty;
            let note = if self.decls.universe.members(hole_ty).len() == 1 {
                "the `show` derive arrives with M6"
            } else {
                "extract a member first [B4, E4]"
            };
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "interpolation hole has type `{}`; the printable primitives (bool, char, \
                         float, int, string) convert implicitly [F6]",
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

    /// One hole operand under the printable rule (F6): the string
    /// context itself, or exactly one printable primitive atom.
    fn printable_hole(&self, ty: Ty) -> bool {
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
    /// it there (F6).
    fn ident_hole_value(&mut self, name: &Ident) -> Val {
        if let Some(res) = self.res.get(name.span) {
            match res {
                Res::Local { bind } => Val {
                    ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                    diverges: false,
                    poisoned: false,
                },
                Res::Def(def) => self.def_value(*def, name.span),
                Res::TraitMethod(_) | Res::Dot(_) | Res::Handler => Val {
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

    fn path_expr(&mut self, path: &Path) -> Val {
        if let [only] = path.segments.as_slice() {
            if let Some(res) = self.res.get(only.span) {
                match res {
                    Res::Local { bind } => Val {
                        ty: self.local_ty(*bind).unwrap_or_else(|| self.empty()),
                        diverges: false,
                        poisoned: false,
                    },
                    Res::Def(def) => self.def_value(*def, only.span),
                    Res::TraitMethod(_) | Res::Dot(_) | Res::Handler => Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
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
                        poisoned: false,
                    }
                },
                None => Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
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
                    poisoned: false,
                },
                None => Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                },
            },
            DefKind::Case => {
                // A bare case is the unit form, like a tag; a payload
                // case constructs by call form only [B3], so a bare use
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
                match self.decls.atoms.get(&def).copied() {
                    Some(atom) => Val {
                        ty: self
                            .decls
                            .universe
                            .atom_ty(atom)
                            .unwrap_or_else(|_| self.empty()),
                        diverges: false,
                        poisoned: false,
                    },
                    None => Val {
                        ty: self.empty(),
                        diverges: false,
                        poisoned: false,
                    },
                }
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
    /// order-independent uses work (G3).
    fn const_ty(&mut self, def: DefId, span: Span) -> Val {
        if let Some(&ty) = self.def_types.get(&def) {
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

    fn lambda(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Lambda {
            span,
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
                poisoned: false,
            };
        };
        // The script-entry fold's synthesized `main` reaches here as a
        // lambda the user never wrote; its own span is empty (no real node
        // owns an empty span — the synthesis's alias guard), so the
        // boundary label names the script entry, not a lambda.
        let body_label = if span.start == span.end { "script main body" } else { "lambda body" };
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
        let body_val = self.expr(body, self.ret);
        let fn_ret = match fn_ret_expected {
            Some(r) if !expected_is_var => {
                if !body_val.diverges {
                    self.expect_contains(body_val, r, body.span(), body_label);
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
            poisoned: false,
        }
    }

    fn struct_expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Struct { path, fields, span } = e else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
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
                poisoned: false,
            };
        };
        let Some(def) = self.package.lookup_type(&only.name) else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }; // resolver reported
        };
        // An enum case constructs by call form only [B3]; the brace form
        // is the struct construction, so this is the cross-spelling.
        if self.decls.cases.contains_key(&def) {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` is an enum case; construct it by call form: `{}(...)`",
                    only.name, only.name
                ),
                only.span,
                "brace form on an enum case",
            );
            for (_, value) in fields {
                if let Some(expr) = value {
                    self.expr(expr, None);
                }
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
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
                poisoned: false,
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
                                poisoned: false,
                            },
                            Res::Def(def) => self.def_value(*def, name.span),
                            _ => Val {
                                ty: self.empty(),
                                diverges: false,
                                poisoned: false,
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
            poisoned: false,
        }
    }

    /// One list literal `[a, b, c]` (F7, B3). Bidirectional: an expected
    /// `list[t]` fixes the element type and every item checks against it;
    /// without one, the first item fixes the type and a later item
    /// outside it is the mixed-element error. The empty literal has no
    /// element evidence — it needs the expected type, and `list[t]()`
    /// is the always-spellable empty form.
    fn list_expr(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::List { span, items, .. } = e else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let Some(list_def) = self.package.lookup_type("list") else {
            for item in items {
                self.expr(item, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            }; // the resolver reported the unknown type
        };
        let expected_elem = expected.and_then(|exp| {
            let (ctor, args) = self.single_apply(exp)?;
            let [elem] = args.as_slice() else {
                return None;
            };
            (self.decls.defs_of_atoms.get(&ctor) == Some(&list_def)).then_some(*elem)
        });
        let elem = if items.is_empty() {
            let Some(elem) = expected_elem else {
                self.error(
                    ErrorCode::InferenceFailed,
                    "the empty list literal needs an expected `list[t]`; write `list[t]()` for \
                     the explicit empty list [B3]"
                        .to_owned(),
                    *span,
                    "empty list literal",
                );
                // downstream empty-type fallbacks must not re-report
                self.solve_reported = true;
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            };
            elem
        } else if let Some(elem) = expected_elem {
            elem
        } else {
            self.expr(&items[0], None).ty
        };
        for (index, item) in items.iter().enumerate() {
            if index == 0 && expected_elem.is_none() {
                continue; // walked above for the element evidence
            }
            let val = self.expr(item, Some(elem));
            if val.diverges || self.decls.universe.contains(elem, val.ty) {
                continue;
            }
            match expected_elem {
                Some(_) => {
                    self.expect_contains(val, elem, item.span(), "list element");
                },
                None => {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "list literal mixes element types: item {} has type `{}`, the first \
                             item `{}` [B2]",
                            index + 1,
                            self.decls.universe.render(val.ty),
                            self.decls.universe.render(elem)
                        ),
                        item.span(),
                        "mixed element types",
                    );
                },
            }
        }
        let ty = self.applied_list_ty(elem).unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// One generic-application constructor call `list[page]()` (F9, B3):
    /// the head resolved in the type namespace, so the bracketed form is
    /// a type application, never an index. Returns `None` when the head
    /// names no type — the ordinary index error reports instead.
    fn ctor_apply_call(
        &mut self,
        recv: &Expr,
        index: &Expr,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Option<Val> {
        let head = single_path_ident(recv)?;
        let def = self.package.lookup_type(&head.name)?;
        let walk_args = |this: &mut Self| {
            for arg in args {
                this.expr(&arg.value, None);
            }
            if let Some(tail) = trailing {
                this.expr(tail, None);
            }
        };
        let is_list = |this: &Self| {
            this.decls
                .structs
                .get(&def)
                .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty())
        };
        if !is_list(self) {
            // Construction is fixed per declaration [B3]; every type
            // application besides the list surface is the cross-spelling
            // error.
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` constructs per its declaration, not by type application [B3]",
                    head.name
                ),
                span,
                "cross-spelling construction",
            );
            walk_args(self);
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        if trailing.is_some() {
            self.error(
                ErrorCode::TypeMismatch,
                "a type application takes no trailing block".to_owned(),
                span,
                "trailing block",
            );
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        if !args.is_empty() {
            self.error(
                ErrorCode::TypeMismatch,
                "the empty-list constructor takes no arguments; build with a list literal `[...]` \
                 [B3]"
                    .to_owned(),
                span,
                "constructor arguments",
            );
            walk_args(self);
            self.solve_reported = true;
            return Some(self.empty_val());
        }
        let Some(arg_ty) = Self::ctor_type_arg(index) else {
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "the bracketed argument of `{}[...]` is a type, not a value [F9]",
                    head.name
                ),
                index.span(),
                "not a type",
            );
            self.solve_reported = true;
            return Some(self.empty_val());
        };
        let Some(elem) = self.solve_here(&arg_ty) else {
            if !self.take_solve_reported() {
                self.error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "the type argument `{}` does not solve against the package declarations",
                        decls::render_type(&arg_ty)
                    ),
                    arg_ty.span(),
                    "unsolvable type argument",
                );
            }
            self.solve_reported = true;
            return Some(self.empty_val());
        };
        let ty = self.applied_list_ty(elem)?;
        Some(Val {
            ty,
            diverges: false,
            poisoned: false,
        })
    }

    /// The map constructor call `map[k, v]()` (F9, B3): the mirror of the
    /// list constructor over the two-argument application of the map
    /// struct. Construction takes no value arguments — the builder call
    /// (`map_set`) fills the map, the interim the record pins while the
    /// map literal rides the anonymous-record-literal reservation.
    fn map_ctor_call(
        &mut self,
        head: &Ident,
        key: &Expr,
        value: &Expr,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let walk_args = |this: &mut Self| {
            for arg in args {
                this.expr(&arg.value, None);
            }
            if let Some(tail) = trailing {
                this.expr(tail, None);
            }
        };
        let Some(def) = self.package.lookup_type(&head.name) else {
            // the resolver already reported the unknown type name
            self.solve_reported = true;
            return self.empty_val();
        };
        let is_map = self
            .decls
            .structs
            .get(&def)
            .is_some_and(|shape| shape.params.len() == 2 && shape.fields.is_empty());
        if !is_map {
            // Construction is fixed per declaration [B3]; the map
            // constructs by type application, any other head is the
            // cross-spelling error.
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "`{}` constructs per its declaration, not by type application [B3]",
                    head.name
                ),
                span,
                "cross-spelling construction",
            );
            walk_args(self);
            self.solve_reported = true;
            return self.empty_val();
        }
        if trailing.is_some() {
            self.error(
                ErrorCode::TypeMismatch,
                "a type application takes no trailing block".to_owned(),
                span,
                "trailing block",
            );
            self.solve_reported = true;
            return self.empty_val();
        }
        if !args.is_empty() {
            self.error(
                ErrorCode::TypeMismatch,
                "the empty-map constructor takes no arguments; build by `map_set` calls [B3]"
                    .to_owned(),
                span,
                "constructor arguments",
            );
            walk_args(self);
            self.solve_reported = true;
            return self.empty_val();
        }
        let arg_ty = |this: &mut Self, arg: &Expr| -> Option<Ty> {
            let Some(ty) = Self::ctor_type_arg(arg) else {
                this.error(
                    ErrorCode::TypeMismatch,
                    "the bracketed arguments of `map[...]` are types, not values [F9]".to_owned(),
                    arg.span(),
                    "not a type",
                );
                this.solve_reported = true;
                return None;
            };
            let Some(solved) = this.solve_here(&ty) else {
                if !this.take_solve_reported() {
                    this.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "the type argument `{}` does not solve against the package \
                             declarations",
                            decls::render_type(&ty)
                        ),
                        ty.span(),
                        "unsolvable type argument",
                    );
                }
                this.solve_reported = true;
                return None;
            };
            Some(solved)
        };
        let (Some(key_ty), Some(value_ty)) = (arg_ty(self, key), arg_ty(self, value)) else {
            return self.empty_val();
        };
        let Some(ty) = self.applied_map_ty(key_ty, value_ty) else {
            self.solve_reported = true;
            return self.empty_val();
        };
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// The bracketed argument of a type application: a type spelled in
    /// expression position (F9) — a path, a parenthesized form, a tuple of
    /// the same, a fn type (`fn(int) -> int`), or a nested generic
    /// application (`list[string]`, `map[string, int]`) whose own bracketed
    /// arguments recurse.
    fn ctor_type_arg(arg: &Expr) -> Option<Type> {
        match arg {
            Expr::Path { span, path, .. } if path.segments.len() == 1 => Some(Type::Path {
                span: *span,
                path: path.clone(),
                args: Vec::new(),
            }),
            Expr::FnType { ty, .. } => Some((**ty).clone()),
            Expr::Paren { inner, .. } => Self::ctor_type_arg(inner),
            Expr::Tuple { items, span, .. } => Some(Type::Tuple {
                span: *span,
                items: items.iter().filter_map(Self::ctor_type_arg).collect(),
            }),
            Expr::Index { recv, index, .. } => {
                let head = single_path_ident(recv)?;
                let arg = Self::ctor_type_arg(index)?;
                Some(Type::Path {
                    span: recv.span(),
                    path: Path {
                        span: head.span,
                        segments: vec![head.clone()],
                    },
                    args: vec![arg],
                })
            },
            Expr::MapApply {
                head, key, value, ..
            } => {
                let key = Self::ctor_type_arg(key)?;
                let value = Self::ctor_type_arg(value)?;
                Some(Type::Path {
                    span: head.span,
                    path: Path {
                        span: head.span,
                        segments: vec![head.clone()],
                    },
                    args: vec![key, value],
                })
            },
            _ => None,
        }
    }

    /// The applied `list[t]` type over one element type.
    fn applied_list_ty(&mut self, elem: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("list")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[elem]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
    }

    /// The applied `map[k, v]` type over the key and value types.
    fn applied_map_ty(&mut self, key: Ty, value: Ty) -> Option<Ty> {
        let def = self.package.lookup_type("map")?;
        let ctor = self.decls.atoms.get(&def).copied()?;
        let atom = self.decls.universe.apply_atom(ctor, &[key, value]).ok()?;
        self.decls.universe.atom_ty(atom).ok()
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
                poisoned: false,
            };
        };
        let callee = peel_paren(callee);
        if let Expr::Field { recv, name, .. } = callee {
            return self.dot_call(recv, name, args, trailing.as_deref(), *span);
        }
        // A generic-application constructor `list[t]()` [F9]: the callee
        // head resolves in the type namespace.
        if let Expr::Index { recv, index, .. } = callee {
            if let Some(val) = self.ctor_apply_call(recv, index, args, trailing.as_deref(), *span) {
                return val;
            }
        }
        // The map constructor `map[k, v]()` [F9, B3]: the head resolved
        // in the type namespace.
        if let Expr::MapApply {
            head, key, value, ..
        } = callee
        {
            return self.map_ctor_call(head, key, value, args, trailing.as_deref(), *span);
        }
        if let Expr::Path { path, .. } = callee {
            match path.segments.as_slice() {
                [only] => {
                    if let Some(Res::Def(def)) = self.res.get(only.span) {
                        let def = *def;
                        if self.decls.fns.contains_key(&def) {
                            return self.call_def(def, None, args, trailing.as_deref(), *span);
                        }
                        // A minted case constructs by call form [B3]; a
                        // bare case falls through to the tag error below.
                        if let Some(case) = self.decls.cases.get(&def).cloned() {
                            if !matches!(case.payload, EnumPayload::None) {
                                return self.case_call(def, only, args, trailing.as_deref(), *span);
                            }
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
                                poisoned: false,
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

    /// Constructs one minted enum case by call form (B3): positional
    /// arguments fill tuple-payload slots and named arguments fill
    /// named-payload fields; the result is the case's own atom set.
    fn case_call(
        &mut self,
        def: DefId,
        name: &Ident,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let payload = self
            .decls
            .cases
            .get(&def)
            .map_or(EnumPayload::None, |c| c.payload.clone());
        if trailing.is_some() {
            self.error(
                ErrorCode::ArityMismatch,
                format!("enum case `{}` takes no trailing block", name.name),
                span,
                "trailing block on a case construction",
            );
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        let case_name = self.package.def(def).name.clone();
        let slots: Vec<(Option<String>, Span, Type)> = match &payload {
            EnumPayload::Tuple(types) => {
                types.iter().map(|t| (None, name.span, t.clone())).collect()
            },
            EnumPayload::Struct(fields) => fields
                .iter()
                .map(|f| (Some(f.name.name.clone()), f.name.span, f.ty.clone()))
                .collect(),
            // A bare case constructs by name like a unit tag; the call
            // site routes here only after the payload check, so this is
            // the belt behind it.
            EnumPayload::None => {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!("`{}` is a bare case; it constructs by name", name.name),
                    name.span,
                    "unit case called as a function",
                );
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            },
        };
        let mut written = vec![false; slots.len()];
        let mut broken = false;
        let mut positional = 0usize;
        for arg in args {
            let slot = if let Some(arg_name) = &arg.name {
                let index = slots
                    .iter()
                    .position(|(field, ..)| field.as_deref().is_some_and(|f| f == arg_name.name));
                let Some(index) = index else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("enum case `{case_name}` has no field `{}`", arg_name.name),
                        arg_name.span,
                        "no such field",
                    );
                    self.expr(&arg.value, None);
                    broken = true;
                    continue;
                };
                index
            } else {
                let index = positional;
                positional += 1;
                let Some(_slot) = slots.get(index) else {
                    self.error(
                        ErrorCode::ArityMismatch,
                        format!("enum case `{case_name}` takes {} argument(s)", slots.len()),
                        arg.span,
                        "extra argument",
                    );
                    self.expr(&arg.value, None);
                    continue;
                };
                index
            };
            if written[slot] {
                self.error(
                    ErrorCode::DuplicateDefinition,
                    format!(
                        "field `{}` of `{case_name}` is written twice",
                        slots[slot].0.clone().unwrap_or_default()
                    ),
                    arg.span,
                    "duplicate field",
                );
            }
            written[slot] = true;
            let Some(field_ty) = self.solve_here(&slots[slot].2) else {
                if !self.take_solve_reported() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("a field of `{case_name}` does not solve"),
                        arg.span,
                        "unsolvable field type",
                    );
                }
                self.expr(&arg.value, None);
                continue;
            };
            let val = self.expr(&arg.value, Some(field_ty));
            self.expect_contains(val, field_ty, arg.value.span(), "case field value");
        }
        if !broken {
            for (index, &is_written) in written.iter().enumerate() {
                if is_written {
                    continue;
                }
                match &slots[index].0 {
                    Some(field) => self.error(
                        ErrorCode::TypeMismatch,
                        format!("enum case `{case_name}` misses field `{field}`"),
                        span,
                        "missing field",
                    ),
                    None => self.error(
                        ErrorCode::ArityMismatch,
                        format!("enum case `{case_name}` takes {} argument(s)", slots.len()),
                        span,
                        "missing argument",
                    ),
                }
            }
        }
        let ty = self
            .decls
            .atoms
            .get(&def)
            .and_then(|&a| self.decls.universe.atom_ty(a).ok())
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    fn indirect_call(
        &mut self,
        callee: Val,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some((param_tys, ret)) = self.decompose_fn(callee.ty) else {
            // A poisoned callee already reported; "not callable" on top of
            // it would be a cascade. The arguments still walk.
            if callee.poisoned {
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
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
                poisoned: false,
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
            poisoned: false,
        }
    }

    /// Calls one package function by definition: instantiates generics by
    /// structural argument matching (the M2 half of one-body generics
    /// (H3)).
    /// Calls one package function by definition: instantiates generics by
    /// structural argument matching (the M2 half of one-body generics
    /// (H3)); named arguments fill their parameter's slot and literal-only
    /// defaults cover omissions (F7).
    fn call_def(
        &mut self,
        def: DefId,
        fed: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        let Some(sig) = self.decls.fns.get(&def).cloned() else {
            // The callee resolved in the value namespace [F9], so it can
            // be a plain const or var binding (a dot-call's free tier); a
            // call against it is a call-shape error with the receiver and
            // every argument walked, never a silently empty type.
            let name = self.package.def(def).name.clone();
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{name}` is a value binding, not a function"),
                span,
                "not callable",
            );
            if let Some(recv) = fed {
                self.expr(recv, None);
            }
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        // The E4 extraction family types by rule against the checked
        // receiver, never through generic inference: an inferred `t?`
        // parameter would bind a plain `t` receiver, and the fallback's
        // width meeting must be diagnosed, not assumed.
        if let Some(kind) = self.extraction_kind(def, &sig) {
            return self.extraction_call(kind, fed, args, trailing, span);
        }
        let slots = self.arg_slots(fed, 0, args, trailing, &sig.params, span);
        let Some(slots) = slots else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        self.check_mut_window_args(&slots, &sig.params);
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
            self.check_slots(&slots, &sig.params, &param_tys);
            return Val {
                ty: ret,
                diverges: false,
                poisoned: false,
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
        // Pass one: non-lambda arguments bind variables structurally,
        // then check against the solved parameter type [B2]: a
        // constructor mismatch binds nothing silently otherwise.
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
            if let Some(expected) = self.solve_with(param_ty, &subst, &sig.generics) {
                self.expect_param_contains(arg_val, param_ty, expected, arg.span());
            }
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
        // Record the instantiation when every generic parameter solved
        // [H3]: the lowering pass mints the CALLG evidence words from it.
        // A partially-solved call is an inference failure below and the
        // lowerer rejects the missing record, never a silent `any`.
        let mut solved = Vec::with_capacity(sig.generics.len());
        for param in &sig.generics {
            let Some(ty) = subst.get(&param.name) else {
                solved.clear();
                break;
            };
            solved.push(*ty);
        }
        // A solved instantiation answers for its declared bounds [B6]:
        // every bound must hold for the solved type, through the
        // instance environment or an in-scope assumption (a caller
        // bound propagates that way). An empty solved type rides a
        // cascading argument error and skips the check.
        for (param, ty) in sig.generics.iter().zip(solved.iter()) {
            let Some(trait_id) = param.bound.as_ref().and_then(|b| self.trait_of_type(b)) else {
                continue;
            };
            if self.decls.universe.is_empty(*ty) {
                continue;
            }
            self.trait_satisfied(trait_id, *ty, span);
        }
        if !solved.is_empty() {
            self.generic_calls.insert(span, solved);
        }
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
            poisoned: false,
        }
    }

    /// Maps call arguments onto signature parameter slots (F7): positional
    /// arguments in order, named arguments by parameter name, the trailing
    /// lambda at the next open position; omissions must carry a literal
    /// default. `None` returns after diagnosing.
    fn arg_slots<'x>(
        &mut self,
        fed: Option<&'x Expr>,
        skip: usize,
        args: &'x [ds_parse::CallArg],
        trailing: Option<&'x Expr>,
        params: &[ParamSig],
        span: Span,
    ) -> Option<Vec<Option<&'x Expr>>> {
        let mut slots: Vec<Option<&'x Expr>> = vec![None; params.len()];
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
                    match params.iter().position(|p| p.name == name.name) {
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
            .zip(params.iter())
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
    /// against their written literal defaults (F7).
    fn check_slots(
        &mut self,
        slots: &[Option<&Expr>],
        params: &[ParamSig],
        param_tys: &[Option<Ty>],
    ) {
        for ((slot, param), param_ty) in slots.iter().zip(params.iter()).zip(param_tys.iter()) {
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

    /// A `mut` window parameter over a copyable binding takes only a
    /// `var` receiver (D2, C3): copyable Ref words alias with no
    /// checker-visible trace, so exclusivity is proven by spelling the
    /// binder `var`. `~Copyable` receivers keep their linearity proof and
    /// fresh rvalues are uniquely owned by construction, so only a const
    /// binding of a copyable type is rejected. A hidden copy behind an
    /// alias would keep mutation invisible at the call site, so the
    /// spelling is corrected, never coerced.
    fn check_mut_window_args(&mut self, slots: &[Option<&Expr>], params: &[ParamSig]) {
        for (slot, param) in slots.iter().zip(params.iter()) {
            if !matches!(param.mode, ds_parse::ParamMode::Mut(_)) {
                continue;
            }
            let Some(arg) = slot else { continue };
            let Expr::Path { path, .. } = peel_paren(arg) else {
                continue;
            };
            let [only] = path.segments.as_slice() else {
                continue
            };
            let Some(res) = self.res.get(only.span).cloned() else {
                continue;
            };
            let (binder, mutable, ty) = match res {
                Res::Local { bind } => {
                    let Some(binding) = self.locals.get(bind) else {
                        continue
                    };
                    (bind, binding.mutable, binding.ty)
                },
                Res::Def(def) => {
                    let def_info = self.package.def(def);
                    if def_info.kind != DefKind::Const {
                        continue;
                    }
                    let Some(ty) = self.def_types.get(&def).copied() else {
                        continue;
                    };
                    (def_info.span, false, ty)
                },
                _ => continue,
            };
            // An empty type is an inference failure the walk already
            // reported; the ruling is about copyable shared-heap words.
            if mutable
                || self.decls.universe.is_empty(ty)
                || !crate::linearity::ty_copyable(self.decls, ty, &self.linear_vars)
            {
                continue;
            }
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::MutWindowConstReceiver,
                    format!(
                        "`{}` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]",
                        only.name
                    ),
                )
                .with_secondary(binder, "bound here")
                .with_primary(only.span, "const receiver"),
            );
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
                poisoned: false,
            };
        };
        let trait_id = match &self.package.method(method).owner {
            MethodOwner::Trait(def) => self.decls.traits.get(def).copied(),
            MethodOwner::Inherent(_) => None,
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
                poisoned: false,
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
                    poisoned: false,
                };
            }
            self.record_solved_dispatch(method, recv_val.ty, span);
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
            // The resolution pass already reported the root cause (no
            // name behind the dot); the empty result must not cascade
            // into downstream empty-intersection errors. The flag sets
            // last — a nested solve_here would wipe it (solve_here
            // clears the flag it reads).
            self.expr(recv, None);
            for arg in args {
                self.expr(&arg.value, None);
            }
            self.solve_reported = true;
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
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
                    poisoned: false,
                }
            },
        }
    }

    /// Method-candidate selection (F3, B6): candidates narrow by receiver
    /// type — trait members through the instance environment (or an
    /// in-scope assumption), inherent members by owner type; survivors
    /// beyond one are true ambiguity, never a priority order.
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
        let mut survivors: Vec<(MethodId, Arc<FnSig>)> = Vec::new();
        for &candidate in candidates {
            let Some(sig) = self.member_sig(candidate) else {
                continue
            };
            match &self.package.method(candidate).owner {
                MethodOwner::Trait(def) => {
                    let Some(trait_id) = self.decls.traits.get(def).copied() else {
                        continue;
                    };
                    if self.trait_satisfied_quiet(trait_id, recv_val.ty) {
                        survivors.push((candidate, sig));
                    }
                },
                MethodOwner::Inherent(target) => {
                    if self.inherent_accepts(target, recv_val.ty) {
                        survivors.push((candidate, sig));
                    }
                },
            }
        }
        if survivors.is_empty() {
            // A poisoned receiver already reported; "no method accepts
            // `<empty>`" on top of it would be a cascade. The arguments
            // still walk so their own errors surface.
            if recv_val.poisoned {
                for arg in args {
                    self.expr(&arg.value, None);
                }
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: true,
                };
            }
            let trait_names: Vec<String> = candidates
                .iter()
                .filter_map(|c| match &self.package.method(*c).owner {
                    MethodOwner::Trait(def) => Some(self.package.def(*def).name.clone()),
                    MethodOwner::Inherent(_) => None,
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
                if trait_names.is_empty() { "no accepting owner" } else { "unimplemented trait" },
            );
            for arg in args {
                self.expr(&arg.value, None);
            }
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        }
        if survivors.len() > 1 {
            let names: Vec<String> = candidates
                .iter()
                .filter(|c| self.member_sig(**c).is_some())
                .map(|c| match &self.package.method(*c).owner {
                    MethodOwner::Trait(def) => format!("`{}`", self.package.def(*def).name),
                    MethodOwner::Inherent(target) => format!("inherent `{target}`"),
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
                poisoned: false,
            };
        }
        let (method, sig) = survivors.remove(0);
        self.record_solved_dispatch(method, recv_val.ty, span);
        self.member_call(&sig, recv_val.ty, args, trailing, span)
    }

    /// Whether an inherent impl's owner accepts the receiver (F3): every
    /// member of the receiver's type must be the owner's atom or an
    /// application of it (a generic impl's constructor). The receiver is
    /// contained in what the owner covers — the instance check's inherent
    /// mirror, never the converse — so a union receiver narrows first and
    /// a receiver the owner does not cover reports at the dot. An owner
    /// naming no declared atom (an alias, a union target) accepts nothing.
    fn inherent_accepts(&self, target: &str, recv: Ty) -> bool {
        let Some(def) = self.package.lookup_type(target) else {
            return false;
        };
        let Some(&owner) = self.decls.atoms.get(&def) else {
            return false;
        };
        let universe = &self.decls.universe;
        universe.members(recv).iter().all(|&member| {
            member == owner
                || matches!(
                    universe.atom_decl(member),
                    Ok(decl) if matches!(decl.kind, AtomKind::Apply(ctor, _) if ctor == owner)
                )
        })
    }

    /// Records one selected trait method's solved dispatch at the call's
    /// own span: the instance the environment resolved for the
    /// receiver's type — a containing head, or a generic head that
    /// unifies under provable bounds (B6). The solved instance is the
    /// declared one; the recorded receiver type carries the
    /// substitution. An inherent-impl survivor records nothing, and a
    /// site satisfied only through an in-scope assumption with no
    /// resolving instance records nothing — its dispatch is the
    /// runtime-resolved evidence form.
    fn record_solved_dispatch(&mut self, method: MethodId, recv: Ty, span: Span) {
        let MethodOwner::Trait(def) = &self.package.method(method).owner else {
            return;
        };
        let Some(&trait_id) = self.decls.traits.get(def) else {
            return;
        };
        match self.instance_resolution(trait_id, recv) {
            Resolution::Unique(instance) => {
                // a generic impl's body dispatches on runtime evidence:
                // record the per-bound instances the substitution serves,
                // so the lowering side stages them without re-deriving types
                let bounds = crate::instance_solve::evidence_bounds(
                    &mut self.decls.universe,
                    &self.decls.instances,
                    instance,
                    recv,
                );
                self.trait_calls.insert(span, TraitCall {
                    method,
                    trait_id,
                    instance,
                    impl_site: self.decls.instance_impls.get(&instance).copied(),
                    recv,
                    bounds,
                });
            },
            Resolution::Ambiguous(candidates) => {
                self.instance_ambiguity(trait_id, recv, &candidates, span);
            },
            Resolution::None | Resolution::BoundUnsat { .. } => {},
        }
    }

    /// One selected method's call check: the member signature with `Self`
    /// bound to the receiver's type [record 6.1].
    fn member_call(
        &mut self,
        sig: &FnSig,
        recv_ty: Ty,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        self.type_vars
            .push(HashMap::from([("Self".to_owned(), recv_ty)]));
        let mut param_tys = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            param_tys.push(param.ty.as_ref().and_then(|t| self.solve_here(t)));
        }
        // The receiver already filled the `self` parameter [record 6.1];
        // the remaining arguments feed the rest.
        if let Some(slots) = self.arg_slots(None, 1, args, trailing, &sig.params, span) {
            self.check_slots(&slots[1..], &sig.params[1..], &param_tys[1..]);
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
            poisoned: false,
        }
    }

    /// `.err(h)`/`.ok(h)`: difference-typed handlers (E4). The handled set
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
                poisoned: false,
            };
        }
        let Expr::Lambda {
            params: handler_params,
            body,
            ..
        } = peel_paren(&args[0].value)
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
                poisoned: false,
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
        if self.decls.universe.is_empty(overlap)
            && !self.decls.universe.is_empty(total)
            && !recv_val.poisoned
        {
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
        if self.decls.universe.is_empty(result)
            && !self.decls.universe.is_empty(total)
            && !recv_val.poisoned
        {
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
            poisoned: false,
        }
    }

    /// The family member a call addresses, when the callee's shape could
    /// be the ambient's: a generic function of the family name and
    /// two-parameter arity. Without the ambient, a same-named generic
    /// function rides the family rules (the natives' name-routing
    /// hazard, unchanged).
    fn extraction_kind(&self, def: DefId, sig: &FnSig) -> Option<Extraction> {
        let kind = Extraction::of_name(&self.package.def(def).name)?;
        (!sig.generics.is_empty() && sig.params.len() == 2).then_some(kind)
    }

    /// One extraction-family call (E4): the receiver and one argument,
    /// typed locally against the checked receiver's members. The eager
    /// `unwrap_or` binds its fallback to the extracted type, the lazy
    /// `unwrap_or_else` checks a zero-parameter lambda against it, `ok_or`
    /// unions the new member in, and `map_err` mirrors the `.err`/`.ok`
    /// handler rules — the annotated parameter handles, or everything
    /// but what the lambda's own result keeps.
    fn extraction_call(
        &mut self,
        kind: Extraction,
        fed: Option<&Expr>,
        args: &[ds_parse::CallArg],
        trailing: Option<&Expr>,
        span: Span,
    ) -> Val {
        if trailing.is_some() {
            self.error(
                ErrorCode::ArityMismatch,
                format!(
                    "`{}` takes no trailing block argument; write the lambda inline",
                    kind.name()
                ),
                span,
                "extra argument",
            );
            return self.empty_val();
        }
        let (recv, arg): (&Expr, &Expr) = match (fed, args) {
            (Some(recv), [only]) => (recv, &only.value),
            (None, [recv, arg]) => (&recv.value, &arg.value),
            _ => {
                self.error(
                    ErrorCode::ArityMismatch,
                    format!("`{}` takes the receiver and one argument", kind.name()),
                    span,
                    "wrong arity",
                );
                return self.empty_val();
            },
        };
        if args.iter().any(|arg| arg.name.is_some()) {
            self.error(
                ErrorCode::ArityMismatch,
                format!("`{}` takes positional arguments only", kind.name()),
                span,
                "named argument",
            );
            return self.empty_val();
        }
        match kind {
            Extraction::UnwrapOr | Extraction::UnwrapOrElse | Extraction::OkOr => {
                let recv_val = self.expr(recv, None);
                let total = recv_val.ty;
                let name = kind.name();
                let none = self.none_ty();
                if !self.decls.universe.contains(total, none) {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`{name}` extracts a `t | none` receiver; `{}` carries no `none` \
                             member [E4]",
                            self.decls.universe.render(total)
                        ),
                        span,
                        "receiver is not optional",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                }
                let extracted = self.decls.universe.diff(total, none);
                if self.decls.universe.is_empty(extracted) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!("`{name}` over `none` leaves nothing to extract [E4]"),
                        span,
                        "empty extraction",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                }
                match kind {
                    Extraction::UnwrapOr => {
                        let arg_val = self.expr(arg, None);
                        if !self.decls.universe.contains(extracted, arg_val.ty) {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "the fallback `{}` does not meet the extracted type `{}` [E4]",
                                    self.decls.universe.render(arg_val.ty),
                                    self.decls.universe.render(extracted)
                                ),
                                span,
                                "fallback mismatch",
                            );
                            return self.empty_val();
                        }
                        Val {
                            ty: extracted,
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::UnwrapOrElse => {
                        let Expr::Lambda { params, body, .. } = peel_paren(arg) else {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "`{name}` takes a `fn() -> t` lambda that inlines at the call \
                                     site"
                                ),
                                arg.span(),
                                "not a fallback lambda",
                            );
                            self.expr(arg, None);
                            return self.empty_val();
                        };
                        if !params.is_empty() {
                            self.error(
                                ErrorCode::ArityMismatch,
                                "the fallback lambda takes no parameters".to_owned(),
                                arg.span(),
                                "wrong lambda arity",
                            );
                            return self.empty_val();
                        }
                        let saved = self.snapshot();
                        let body_val = self.expr(body, Some(extracted));
                        self.restore(saved);
                        if !self.decls.universe.contains(extracted, body_val.ty) {
                            self.error(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "the fallback lambda's `{}` does not meet the extracted type \
                                     `{}` [E4]",
                                    self.decls.universe.render(body_val.ty),
                                    self.decls.universe.render(extracted)
                                ),
                                span,
                                "fallback mismatch",
                            );
                            return self.empty_val();
                        }
                        Val {
                            ty: extracted,
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::OkOr => {
                        let arg_val = self.expr(arg, None);
                        Val {
                            ty: self.decls.universe.union(extracted, arg_val.ty),
                            diverges: false,
                            poisoned: false,
                        }
                    },
                    Extraction::MapErr => unreachable!("the optional arm owns three of four"),
                }
            },
            Extraction::MapErr => {
                let Expr::Lambda {
                    params: handler_params,
                    body,
                    ..
                } = peel_paren(arg)
                else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        "`map_err` takes a one-parameter lambda `(e) => ...`".to_owned(),
                        arg.span(),
                        "not a handler lambda",
                    );
                    self.expr(arg, None);
                    return self.empty_val();
                };
                let [param] = handler_params.as_slice() else {
                    self.error(
                        ErrorCode::ArityMismatch,
                        "`map_err` takes a one-parameter lambda".to_owned(),
                        arg.span(),
                        "wrong lambda arity",
                    );
                    return self.empty_val();
                };
                let recv_val = self.expr(recv, None);
                let total = recv_val.ty;
                let annotated = param.ty.as_ref().and_then(|t| self.solve_here(t));
                let saved = self.snapshot();
                let param_ty = annotated.unwrap_or(total);
                self.bind_local(param.name.span, Binding {
                    ty: param_ty,
                    mutable: false,
                });
                let body_val = self.expr(body, None);
                self.restore(saved);
                let handled_set = match annotated {
                    Some(written) => written,
                    None => {
                        let kept = self.decls.universe.inter(total, body_val.ty);
                        self.decls.universe.diff(total, kept)
                    },
                };
                let overlap = self.decls.universe.inter(total, handled_set);
                if self.decls.universe.is_empty(overlap) && !self.decls.universe.is_empty(total) {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "`map_err` handles `{}`, which `{}` does not carry",
                            self.decls.universe.render(handled_set),
                            self.decls.universe.render(total)
                        ),
                        span,
                        "disjoint handler",
                    );
                    return self.empty_val();
                }
                let remainder = self.decls.universe.diff(total, handled_set);
                let result = self.decls.universe.union(remainder, body_val.ty);
                if self.decls.universe.is_empty(result) && !self.decls.universe.is_empty(total) {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "`map_err` leaves nothing: the handler consumed all of `{}`",
                            self.decls.universe.render(total)
                        ),
                        span,
                        "empty remainder",
                    );
                    return self.empty_val();
                }
                Val {
                    ty: result,
                    diverges: false,
                    poisoned: false,
                }
            },
        }
    }

    /// Bare field access `x.f`: struct-owned, type-directed (F3); on a
    /// generic application through the constructor's shape and the
    /// application's argument substitution.
    fn field_access(&mut self, recv: &Expr, name: &Ident) -> Val {
        let recv_val = self.expr(recv, None);
        // A poisoned receiver already reported; a field-access complaint
        // over its (empty or wrapper) type would be a cascade.
        if recv_val.poisoned {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: true,
            };
        }
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
                poisoned: false,
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
                        poisoned: false,
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
                        poisoned: false,
                    };
                },
            },
        };
        let Some(shape) = self.decls.structs.get(&def).cloned() else {
            self.no_field(recv_val.ty, name);
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        let Some(field) = shape.fields.iter().find(|f| f.name.name == name.name) else {
            self.no_field(recv_val.ty, name);
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        self.type_vars.push(subst);
        let field_ty = self.solve_here(&field.ty);
        self.type_vars.pop();
        if let Some(ty) = field_ty {
            Val {
                ty,
                diverges: false,
                poisoned: false,
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
                poisoned: false,
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
    /// must be contained in the written return union (B2, E4).
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
                // No return union to split against: a top-level
                // initializer (no enclosing function), a fn without a
                // declared return, or a lambda whose return is inferred
                // (no annotation, or a bare type-variable expectation
                // from a generic callee) — the split needs a known
                // destination, never a silent whole-union bind.
                self.error(
                    ErrorCode::PropagationNotContained,
                    "`?` needs an enclosing return union to propagate into [E4]".to_owned(),
                    span,
                    "stray propagation",
                );
                return Val {
                    ty: self.empty(),
                    diverges: false,
                    poisoned: false,
                };
            },
        };
        if let Some(ret) = ret {
            let escape = self.decls.universe.diff(propagated, ret);
            if !self.decls.universe.is_empty(escape) && !val.poisoned {
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
        if self.decls.universe.is_empty(ok) && !self.decls.universe.is_empty(total) && !val.poisoned
        {
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
            poisoned: val.poisoned,
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
                poisoned: false,
            };
        };
        let (declared, bind_span, mutable) = match &res {
            Res::Local { bind } => match self.locals.get(*bind) {
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
                .and_then(|span| self.refined.get(span).copied())
                .unwrap_or(declared);
            let right_val = self.expr(right, Some(current));
            let result = self.binary_val(
                compound_op(op),
                current,
                right_val.ty,
                left.span(),
                right.span(),
                span,
                right_val.poisoned,
            );
            self.expect_contains(result, declared, span, "compound assignment");
        }
        // Refinement resets at assignment [B2].
        if let Some(bind_span) = bind_span {
            self.refined.remove(bind_span);
        }
        Val {
            ty: unit,
            diverges: false,
            poisoned: false,
        }
    }

    /// `?` in a branch-final position splits against the expected type
    /// when one flows in (E4): a branch-final expression is the enclosing
    /// function's final expression when the if/match itself is last.
    fn if_expr(
        &mut self,
        cond: &Expr,
        then: &BlockExpr,
        else_: Option<&Expr>,
        expected: Option<Ty>,
    ) -> Val {
        self.cond_depth += 1;
        let cond_val = self.expr(cond, None);
        self.cond_depth -= 1;
        self.expect_bool(cond_val, cond.span());
        let narrows = self.harvest(cond);
        // The else path runs on ¬(a∧b∧…) = ¬a∨¬b∨…: when the condition is
        // an `&&` chain, no single conjunct is guaranteed false, so none
        // of its narrows inverts soundly there. A single-conjunct
        // condition still inverts (¬a is exactly the else).
        let else_narrows: &[Narrow] = if matches!(peel_paren(cond), Expr::Binary {
            op: BinaryOp::And,
            ..
        }) {
            &[]
        } else {
            &narrows
        };
        let unit = self.unit();
        let saved = self.snapshot();
        for narrow in &narrows {
            self.apply_narrow(narrow);
        }
        let then_val = self.block_expected(then, expected);
        self.restore(saved);
        let else_val = if let Some(else_expr) = else_ {
            let saved = self.snapshot();
            for narrow in else_narrows {
                self.apply_inverse(narrow);
            }
            let val = self.expr(else_expr, expected);
            self.restore(saved);
            val
        } else {
            if then_val.diverges {
                // The then arm never falls through, so the continuation
                // is exactly the implicit else path: its inverse
                // narrowing stays live after the if.
                for narrow in else_narrows {
                    self.apply_inverse(narrow);
                }
            }
            Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            }
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
            poisoned: false,
        }
    }

    fn match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        expected: Option<Ty>,
    ) -> Val {
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
            let (arm_cover, arm_binders) =
                self.pattern_cover(&arm.pat, remaining, arm.ann.as_ref());
            if let Some(bind) = scrut_bind {
                // An arm covering no member atom (a literal arm) refines
                // nothing: narrowing the scrutinee to the empty type
                // would poison the arm body.
                if !arm_cover.is_empty() {
                    let refined = self.cover_ty(&arm_cover);
                    self.refined.insert(bind, refined);
                }
            }
            for (_, span, ty) in arm_binders {
                self.bind_local(span, Binding { ty, mutable: false });
            }
            if let Some(guard) = &arm.guard {
                self.cond_depth += 1;
                let guard_val = self.expr(guard, None);
                self.cond_depth -= 1;
                self.expect_bool(guard_val, guard.span());
                for narrow in self.harvest(guard) {
                    self.apply_narrow(&narrow);
                }
            }
            let body_val = self.expr(&arm.body, expected);
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
                poisoned: false,
            };
        }
        let mut ty = self.empty();
        for val in producing {
            ty = self.decls.universe.union(ty, val.ty);
        }
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    fn for_expr(&mut self, pat: &Pattern, iter: &Expr, body: &Expr, span: Span) -> Val {
        let unit = self.unit();
        // A range is the surface's one pinned iterable: integer
        // endpoints, the pattern binds the element [F7]. A storable
        // range value is not in the record; the value-position stub
        // below stands.
        if let Expr::Range { start, end, .. } = peel_paren(iter) {
            if let Some(element_ty) = self.range_element(start, end, span) {
                let saved = self.snapshot();
                self.refined.clear();
                self.loop_depth += 1;
                self.bind_pattern(pat, element_ty, false);
                self.expr(body, None);
                self.loop_depth -= 1;
                self.restore(saved);
            }
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
            };
        }
        let iter_val = self.expr(iter, None);
        let members = self.decls.universe.members(iter_val.ty).to_vec();
        let element_ty = (|| {
            let single = members.first().filter(|_| members.len() == 1)?;
            let (_, args) = self.single_apply_of(*single)?;
            args.first().copied()
        })();
        let Some(element_ty) = element_ty else {
            // A poisoned iterable already reported; the collection-shape
            // complaint on top of it would be a cascade.
            if !iter_val.poisoned {
                self.error(
                    ErrorCode::InferenceFailed,
                    format!(
                        "iteration needs one collection type, `{}` given; the stdlib traits \
                         arrive at M3",
                        self.decls.universe.render(iter_val.ty)
                    ),
                    span,
                    "not iterable",
                );
            }
            return Val {
                ty: unit,
                diverges: false,
                poisoned: false,
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
            poisoned: false,
        }
    }

    // ---- patterns ---------------------------------------------------------

    // ---- refinement ---------------------------------------------------------

    /// Narrows harvested from a condition: `is` tests and none-comparisons
    /// over local paths (B2).
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
        let Some(declared) = self.locals.get(narrow.bind).map(|b| b.ty) else {
            return;
        };
        let current = self.refined.get(narrow.bind).copied().unwrap_or(declared);
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

    /// The method's registered signature, shared (the handle clones; the
    /// AST-shaped signature never deep-copies at call sites).
    fn member_sig(&self, method: MethodId) -> Option<Arc<FnSig>> {
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

    /// One generic call's argument against its solved parameter type (B2):
    /// the structural binder stays silent on a constructor mismatch (it
    /// binds nothing for `list_len("words")`), so the solved parameter has
    /// to contain the argument or the mismatch reports. A parameter that
    /// still carries an unbound variable renders its written spelling,
    /// never the internal `$t` name.
    fn expect_param_contains(
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
    fn has_free_vars(&self, ty: Ty) -> bool {
        self.decls
            .universe
            .members(ty)
            .iter()
            .any(|atom| self.atom_has_free_vars(*atom))
    }

    fn atom_has_free_vars(&self, atom: TypeId) -> bool {
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

    fn expect_bool(&mut self, val: Val, span: Span) -> bool {
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

    fn check_args(&mut self, args: &[&Expr], param_tys: &[Option<Ty>], span: Span) {
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
                if only.name == "none" && !self.package.declares("none") {
                    return Some(self.none_ty());
                }
                if only.name == "self" && !self.package.declares("self") {
                    // `self` as a type name retired [record 6.1]; resolve
                    // reported DS2015, so suppress the generic fallbacks.
                    self.solve_reported = true;
                    return None;
                }
                let def = self.package.lookup_type(&only.name)?;
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
                    DefKind::Tag | DefKind::Struct | DefKind::Case => {
                        let atom = self.decls.atoms.get(&def).copied()?;
                        self.decls.universe.atom_ty(atom).ok()
                    },
                    DefKind::Enum => {
                        // A generic enum keeps its opaque atom; a
                        // non-generic one names the union of its minted
                        // case atoms [B3].
                        if let Some(&atom) = self.decls.atoms.get(&def) {
                            return self.decls.universe.atom_ty(atom).ok();
                        }
                        let shape = self.decls.enums.get(&def)?.clone();
                        if !shape.params.is_empty() {
                            return None;
                        }
                        let mut union: Option<Ty> = None;
                        for (name, ..) in &shape.cases {
                            let case_def = self.package.lookup_type(name)?;
                            let atom = self.decls.atoms.get(&case_def).copied()?;
                            let ty = self.decls.universe.atom_ty(atom).ok()?;
                            union = Some(match union {
                                Some(acc) => self.decls.universe.union(acc, ty),
                                None => ty,
                            });
                        }
                        union
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
                let def = self.package.lookup_type(&only.name)?;
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
                let Some(def) = self.package.lookup_type(&only.name) else {
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
        let def = self.package.lookup_type(&only.name)?;
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

    /// Whether a trait holds for a type: an in-scope assumption covers
    /// it, or the environment resolves an instance — a containing head
    /// for concrete receivers, or a generic head that unifies with the
    /// receiver under provable bounds (B6). Diagnoses on failure.
    fn trait_satisfied(&mut self, trait_id: TraitId, ty: Ty, span: Span) -> bool {
        if self.assumption_covers(trait_id, ty) {
            return true;
        }
        match self.instance_resolution(trait_id, ty) {
            Resolution::Unique(_) => true,
            Resolution::Ambiguous(candidates) => {
                self.instance_ambiguity(trait_id, ty, &candidates, span);
                false
            },
            Resolution::None => {
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
            },
            Resolution::BoundUnsat { bound, target, .. } => {
                let miss = self.render_members(&target);
                self.error(
                    ErrorCode::UnsatisfiedConstraint,
                    format!(
                        "no instance of `{}` for {}: the `{}` impl's bound is unsatisfied there",
                        self.trait_name(bound),
                        miss,
                        self.trait_name(trait_id)
                    ),
                    span,
                    "unsatisfied bound",
                );
                false
            },
        }
    }

    fn trait_satisfied_quiet(&self, trait_id: TraitId, ty: Ty) -> bool {
        self.assumption_covers(trait_id, ty) || self.instance_resolution(trait_id, ty).satisfied()
    }

    /// Whether an in-scope generic bound (`[t: json]`) assumes the trait
    /// for the type.
    fn assumption_covers(&self, trait_id: TraitId, ty: Ty) -> bool {
        self.assumptions
            .iter()
            .rev()
            .any(|(t, assumed)| *t == trait_id && self.decls.universe.contains(*assumed, ty))
    }

    /// The environment's call-site resolution (B6): the instance whose
    /// head contains the receiver, or — for a generic impl's head — the
    /// one whose pattern unifies with it under provable bounds. The
    /// solved instance is the declared instance; a per-substitution
    /// instance is never minted, because the call's own receiver type
    /// (recorded beside it) carries the substitution the dispatch seam
    /// needs.
    fn instance_resolution(&self, trait_id: TraitId, ty: Ty) -> Resolution {
        crate::instance_solve::resolve(
            &self.decls.universe,
            &self.decls.instances,
            &self.assumptions,
            trait_id,
            ty,
        )
    }

    /// Several impls serve one call site (B6): declared heads are
    /// pairwise disjoint, but a generic head is opaque to the set
    /// algebra, so two generic impls of one trait can both serve one
    /// receiver. A compile error, never a priority order.
    fn instance_ambiguity(
        &mut self,
        trait_id: TraitId,
        recv: Ty,
        candidates: &[InstanceId],
        span: Span,
    ) {
        let mut diag = Diagnostic::error(
            ErrorCode::AmbiguousInstance,
            format!(
                "{} impls of `{}` accept receiver `{}`; dispatch must pick one [B6]",
                candidates.len(),
                self.trait_name(trait_id),
                self.decls.universe.render(recv)
            ),
        )
        .with_primary(span, "ambiguous dispatch");
        for &id in candidates {
            if let (Ok(head), Some(site)) = (
                self.decls.instances.head(id),
                self.decls.instance_impls.get(&id),
            ) {
                diag = diag.with_secondary(
                    *site,
                    format!("instance head `{}`", self.decls.universe.render(head)),
                );
            }
        }
        self.diags.push(diag);
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
        self.decls.var_counter += 1;
        let unique = format!("${name}#{}", self.decls.var_counter);
        // Impossible by construction: surface names cannot contain `$` or
        // `#`, and both minting sites (instance declaration and body
        // checking) draw from the shared var_counter, so the minted name
        // is always fresh — a failure would be a silently wrong type, so
        // it panics instead of falling back to NONE.
        self.decls
            .universe
            .declare_tag(&unique)
            .expect("minted variable names are unique by the shared counter")
    }

    fn fresh_var(&mut self, name: &str) -> Ty {
        let var = self.mint_var(name);
        self.decls
            .universe
            .atom_ty(var)
            .unwrap_or_else(|_| self.empty())
    }

    fn snapshot(&self) -> ScopeMark {
        ScopeMark {
            locals: self.locals.mark(),
            refined: self.refined.mark(),
            assumptions: self.assumptions.len(),
            ret: self.ret,
        }
    }

    fn restore(&mut self, saved: ScopeMark) {
        self.locals.restore_to(saved.locals);
        self.refined.restore_to(saved.refined);
        self.assumptions.truncate(saved.assumptions);
        self.ret = saved.ret;
    }

    fn local_ty(&self, bind: Span) -> Option<Ty> {
        self.refined
            .get(bind)
            .copied()
            .or_else(|| self.locals.get(bind).map(|b| b.ty))
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
    fn payload_tys(&mut self, _path: &Path, atom: TypeId) -> Option<Vec<Ty>> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        if let Some(shape) = self.decls.structs.get(&def).cloned() {
            let mut out = Vec::with_capacity(shape.fields.len());
            for field in &shape.fields {
                out.push(self.solve_here(&field.ty)?);
            }
            return Some(out);
        }
        // A minted tuple-payload case carries no struct shape; its
        // payload types come from the case table in declaration order.
        let tuple_payload = match self.decls.cases.get(&def).map(|c| &c.payload) {
            Some(EnumPayload::Tuple(types)) => Some(types.clone()),
            _ => None,
        };
        if let Some(types) = tuple_payload {
            let mut out = Vec::with_capacity(types.len());
            for ty in &types {
                out.push(self.solve_here(ty)?);
            }
            return Some(out);
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

    /// The per-element expected types of one expected tuple type: the
    /// expectation must be a single tuple atom of the construction's
    /// arity (a union of tuples or a wider member set carries no
    /// element-wise expectation).
    fn expected_tuple_elems(&mut self, expected: Ty, arity: usize) -> Option<Vec<Ty>> {
        let members = self.decls.universe.members(expected).to_vec();
        let [only] = members.as_slice() else {
            return None;
        };
        let elements = self.tuple_elements(*only)?;
        if elements.len() != arity {
            return None;
        }
        elements
            .iter()
            .map(|e| self.decls.universe.atom_ty(*e).ok())
            .collect()
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

    /// The empty-typed value of an errored expression.
    fn empty_val(&mut self) -> Val {
        Val {
            ty: self.empty(),
            diverges: false,
            poisoned: false,
        }
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
            poisoned: false,
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

/// The single identifier of a one-segment path expression, if it is one.
fn single_path_ident(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Path { path, .. } if path.segments.len() == 1 => path.segments.first(),
        _ => None,
    }
}

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
