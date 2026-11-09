//! The affine pass [C1, C5]: one intraprocedural forward walk computing
//! definite-initialization and last-use together.
//!
//! # Model
//!
//! Every binding carries a three-state lattice — [`State::Uninit`],
//! [`State::Live`], [`State::Dead`] — through one forward walk per body. A
//! *consuming* use of a non-copyable owned binding (a `move`-mode call
//! argument, a struct-literal or tuple field it flows into, a lambda
//! capture, a return value) moves it: the binding dies there and its deinit
//! responsibility transfers with it. Every later use of a dead binding is
//! the two-span [`ErrorCode::MovedThenUsed`] ("moved here" at the move,
//! "used again here after move" at the use). Reads of an uninitialized
//! binding are [`ErrorCode::UninitializedUse`]. Joins (`if`/`else`, match
//! arms, `or`) merge conservatively: a binding is live after a join only
//! when live on every non-diverging path, so a move on any path kills it —
//! a later use really does execute on the moved path. `var` re-assignment
//! re-initializes; the replaced value's death is the assignment itself.
//!
//! # Copyability [C1]
//!
//! `~Copyable` subtracts the default copyability constraint, so everything
//! copies by default. A type is non-copyable when any of its member atoms
//! is: a declaration carrying the `~Copyable` marker among its supers, a
//! struct or enum whose (solved) field or payload types are non-copyable,
//! a generic application of a non-copyable argument (a container of linear
//! values is linear), a tuple of a non-copyable element, or a minted type
//! variable whose generic parameter bound is the marker (`[t: ~Copyable]`).
//! The bidirectional walk records each local binder's copyability at
//! binding time ([`BinderFact`], in scope-sensitive positions), so this
//! pass consumes recorded facts instead of re-solving types.
//!
//! # Modes [C2]
//!
//! Borrow and `mut` parameters are windows the caller retains, so their
//! uses never move anything — in the callee's body or at the call site.
//! Only `move` parameters consume their argument. Unresolvable callees
//! (an indirect call through a fn-typed value) default to borrowing; the
//! M4 runtime `rc==1` uniqueness checks close that gap.
//!
//! # deinit well-formedness [C5]
//!
//! The static half: `deinit` is declared on `~Copyable` types only and at
//! most once per struct. Every owned non-copyable binding either moves
//! (ownership, and deinit with it, transfers) or reaches its scope end
//! live, where the walk records the drop site the runtime half (M4) must
//! honor on every path — early return, panic, and cancellation included.
//!
//! # Walk order
//!
//! Top-level bindings are evaluated on demand and order-independent [G3];
//! their initializers walk in a first pass over each file's items so a
//! linear definition consumed in one item and used in another is a move
//! wherever the items sit. Function and impl bodies walk after them.
//!
//! # Known limits (recorded for the later passes that close them)
//!
//! * Per-binder copyability comes from the declared type; refinement inside an `is` branch of a
//!   mixed binding (`file | int` narrowed to `int`) still moves. Per-point precision is M4
//!   borrow-inference work.
//! * A field projection in value position moves the whole receiver binding (partial-move tracking
//!   is the M4 Perceus placement pass).
//! * Field projection out of a borrow/mut window never moves.
//! * `defer` bodies walk at their source position; scope-exit semantics land with defer lowering
//!   [E5, M3].
//! * The loop audit checks one re-execution round (the condition and body of `while`, the body of
//!   `for`) against the merged state; a use the forward walk already reported is never reported
//!   twice.
//! * The uninitialized state is infrastructure: every binding form in the M2 surface carries an
//!   initializer and the resolver binds in flow order, so [`ErrorCode::UninitializedUse`] fires
//!   only once a surface form can defer initialization.
//! * Candidate method calls (`Res::Dot(DotRes::Methods)`) consume their receiver only when every
//!   candidate's first parameter is `move`; their non-receiver arguments walk as borrows until
//!   candidate selection is exposed to this pass.

use std::collections::{HashMap, HashSet};

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    Ast, BinaryOp, BlockExpr, CallArg, Expr, FnDecl, Ident, Item, ItemKind, LocalDecl, MatchArm,
    OrTail, Param, ParamMode, Path, Pattern, Stmt, StructDecl, StructMember, TraitMember,
};

use crate::{
    atom::{AtomKind, Field, TypeId},
    decls::{Decls, FnSig, linear_marker},
    expr::TypeExpr,
    resolve::{DefId, DefKind, DotRes, MethodId, Package, Res, Resolutions},
    type_of::Checked,
    universe::Ty,
};

/// Affine facts of one local binder, recorded by the bidirectional walk at
/// the binding site [C1] and consumed here.
#[derive(Clone, Copy, Debug)]
pub struct BinderFact {
    /// Whether uses of the binding copy freely; the `~Copyable` marker, a
    /// non-copyable field, or a linear generic argument make it false.
    pub copyable: bool,
    /// The solved type at the binding site: the pass reads it to decide
    /// whether a field projection copies (a scalar read) or transfers
    /// (a linear field).
    pub ty: Ty,
}

/// One consuming use of an owned non-copyable binding: the value flows
/// elsewhere and the binding dies here. A lowerer emits the transfer with
/// no dup and owes no drop for this binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    /// The binder's name span.
    pub binder: Span,
    /// The use that consumed it.
    pub at: Span,
}

/// One scope-exit death of a live owned non-copyable binding: where the
/// runtime half (M4) must run deinit on every path [C5].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropSite {
    /// The binder's name span.
    pub binder: Span,
    /// The scope exit: block or lambda body end, fn body end, match arm,
    /// or the assignment replacing the value.
    pub at: Span,
}

/// The final use of one tracked binding, borrowed or consuming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastUse {
    /// The binder's name span.
    pub binder: Span,
    /// The final use.
    pub at: Span,
    /// Whether the use leaves the binding live (a borrow, or any use of a
    /// copyable binding).
    pub borrowed: bool,
}

/// The affine pass over one package: diagnostics plus the placement facts
/// a lowerer relies on.
#[derive(Default, Debug)]
pub struct Linearity {
    /// Diagnostics in walk order.
    pub diags: Vec<Diagnostic>,
    /// Consuming uses of owned non-copyable bindings [C1].
    pub moves: Vec<Move>,
    /// Scope-exit deaths of live owned non-copyable bindings [C5].
    pub drops: Vec<DropSite>,
    /// The final use of every tracked binding.
    pub last_uses: Vec<LastUse>,
}

/// The full static check of one package: bidirectional types [B2] then the
/// affine pass [C1, C5]. Callers wanting every diagnostic read both lists.
#[must_use]
pub fn check(files: &ds_diag::Files, asts: &[Ast]) -> (Checked, Linearity) {
    let (package, resolutions, decls, checked) = crate::type_of::run(files, asts);
    let lin = linearity_check(asts, &package, &resolutions, &decls, &checked);
    (checked, lin)
}

/// Runs the affine pass over a package whose bidirectional check already
/// ran: `checked` supplies binder facts and top-level types, the tables
/// come from the same walk (see `type_of::run`).
#[must_use]
pub fn linearity_check(
    asts: &[Ast],
    package: &Package,
    resolutions: &[Resolutions],
    decls: &Decls,
    checked: &Checked,
) -> Linearity {
    let mut lin = Lin {
        package,
        decls,
        facts: &checked.binders,
        def_types: &checked.def_types,
        res: resolutions.first(),
        out: Linearity::default(),
        state: HashMap::new(),
        info: HashMap::new(),
        scopes: vec![Vec::new()],
        audit: false,
        reported: HashSet::new(),
        last_use: HashMap::new(),
        epoch: 0,
    };
    for (ast, res) in asts.iter().zip(resolutions.iter()) {
        lin.res = Some(res);
        lin.file(ast);
    }
    let mut last_uses: Vec<LastUse> = lin
        .last_use
        .iter()
        .map(|(place, &(at, borrowed))| LastUse {
            binder: place.binder_span(package),
            at,
            borrowed,
        })
        .collect();
    last_uses.sort_by_key(|use_| use_.binder.start);
    lin.out.last_uses = last_uses;
    lin.out
}

// ---- copyability [C1] -------------------------------------------------------

/// Whether a solved type copies freely: every member atom does.
pub(crate) fn ty_copyable(decls: &Decls, ty: Ty, linear_vars: &HashMap<TypeId, bool>) -> bool {
    ty_copyable_seen(decls, ty, linear_vars, &mut Vec::new())
}

fn ty_copyable_seen(
    decls: &Decls,
    ty: Ty,
    linear_vars: &HashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    decls
        .universe
        .members(ty)
        .iter()
        .all(|atom| atom_copyable(decls, *atom, linear_vars, seen))
}

fn atom_copyable(
    decls: &Decls,
    atom: TypeId,
    linear_vars: &HashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    // A cyclic type is illegal in v0 [C4]; stopping as copyable on a
    // revisit keeps the recursion finite without deciding legality here.
    if seen.contains(&atom) {
        return true;
    }
    seen.push(atom);
    let Ok(decl) = decls.universe.atom_decl(atom) else {
        return true;
    };
    // A minted type variable: the generic parameter's bound decides [C1].
    if decl.name.starts_with('$') {
        return linear_vars.get(&atom).copied().unwrap_or(true);
    }
    if decls
        .defs_of_atoms
        .get(&atom)
        .is_some_and(|def| struct_linear(decls, *def))
    {
        return false;
    }
    match &decl.kind {
        AtomKind::Unit | AtomKind::Reserved | AtomKind::FnSig(..) => true,
        AtomKind::Struct(fields) => fields
            .iter()
            .all(|f| field_copyable(decls, f, linear_vars, seen)),
        AtomKind::Enum(cases) => cases.iter().all(|c| {
            c.fields
                .iter()
                .all(|f| field_copyable(decls, f, linear_vars, seen))
        }),
        AtomKind::Tuple(elements) => elements
            .iter()
            .all(|e| atom_copyable(decls, *e, linear_vars, seen)),
        // A container of linear values is linear: the application copies
        // freely only when the constructor and every argument do.
        AtomKind::Apply(ctor, args) => {
            args.iter()
                .all(|a| ty_copyable_seen(decls, *a, linear_vars, seen))
                && !decls
                    .defs_of_atoms
                    .get(ctor)
                    .is_some_and(|def| struct_linear(decls, *def))
        },
    }
}

fn field_copyable(
    decls: &Decls,
    field: &Field,
    linear_vars: &HashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    expr_copyable(decls, &field.ty, linear_vars, seen)
}

fn expr_copyable(
    decls: &Decls,
    expr: &TypeExpr,
    linear_vars: &HashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    match expr {
        TypeExpr::Atom(atom) => atom_copyable(decls, *atom, linear_vars, seen),
        // A trait used as a type copies when every member atom does [B1].
        TypeExpr::Trait(id) => decls.universe.trait_decl(*id).map_or(true, |decl| {
            decl.members
                .iter()
                .all(|atom| atom_copyable(decls, *atom, linear_vars, seen))
        }),
        // A negated constraint in field position subtracts copyability.
        TypeExpr::Not(_) => false,
        TypeExpr::Union(a, b) | TypeExpr::Inter(a, b) | TypeExpr::Diff(a, b) => {
            expr_copyable(decls, a, linear_vars, seen) && expr_copyable(decls, b, linear_vars, seen)
        },
        TypeExpr::Tuple(elements) => elements
            .iter()
            .all(|e| expr_copyable(decls, e, linear_vars, seen)),
    }
}

fn struct_linear(decls: &Decls, def: DefId) -> bool {
    decls.structs.get(&def).is_some_and(|s| s.linear)
        || decls.enums.get(&def).is_some_and(|e| e.linear)
}

// ---- the walk -----------------------------------------------------------------

/// Where a binding's value lives during the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Declared, no value yet. Unreachable in the M2 surface (every
    /// binding form carries an initializer); kept as the lattice's base
    /// so the deferred-initialization forms land on it directly.
    #[allow(dead_code)]
    Uninit,
    /// Holding a value.
    Live,
    /// Moved; `at` is the consuming use.
    Dead {
        /// The move that killed the binding.
        at: Span,
    },
}

/// One tracked binding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Place {
    /// A local binder by its name span.
    Local(Span),
    /// A top-level binding by its package definition.
    Def(DefId),
}

impl Place {
    fn binder_span(self, package: &Package) -> Span {
        match self {
            Self::Local(span) => span,
            Self::Def(def) => package.def(def).span,
        }
    }
}

/// Static facts of one tracked binding.
#[derive(Clone, Copy)]
struct Info<'a> {
    name: &'a str,
    copyable: bool,
    /// Whether this frame owns the value (locals, `move` parameters,
    /// top-level bindings); borrows and `mut` windows never move.
    owned: bool,
    /// The capture depth at binding time: a binding is outer to every
    /// lambda or spawn body entered after it.
    epoch: usize,
}

type States = HashMap<Place, State>;

/// The forward walker. One instance covers one package; top-level
/// bindings' states persist across items so a value consumed in one item
/// and used in another is still a move [G3, C1].
struct Lin<'a> {
    package: &'a Package,
    decls: &'a Decls,
    facts: &'a HashMap<Span, BinderFact>,
    def_types: &'a HashMap<DefId, Ty>,
    res: Option<&'a Resolutions>,
    out: Linearity,
    state: States,
    info: HashMap<Place, Info<'a>>,
    /// Binders introduced per scope, in bind order (deinit order at exit).
    scopes: Vec<Vec<Place>>,
    /// Audit mode: reads are checked against the current state but nothing
    /// mutates — the loop back-edge re-execution check.
    audit: bool,
    /// Use sites already reported, so the loop audit never reports twice.
    reported: HashSet<(Place, Span)>,
    /// The last use per binding: (use span, borrowed).
    last_use: HashMap<Place, (Span, bool)>,
    /// Capture depth: inside a lambda or spawn body, every use of a
    /// binding from an outer epoch is a capture and moves it [C1].
    epoch: usize,
}

impl<'a> Lin<'a> {
    fn file(&mut self, ast: &'a Ast) {
        // First pass: top-level bindings, in source order [G3].
        for block in &ast.blocks {
            for item in &block.items {
                if let ItemKind::Local(local) = &item.kind {
                    self.top_local(local);
                }
            }
        }
        // Second pass: bodies and declaration shapes.
        for block in &ast.blocks {
            for item in &block.items {
                if !matches!(&item.kind, ItemKind::Local(_)) {
                    self.item(item);
                }
            }
        }
    }

    fn item(&mut self, item: &'a Item) {
        self.state
            .retain(|place, _| !matches!(place, Place::Local(_)));
        match &item.kind {
            ItemKind::Fn(f) => {
                if f.body.is_some() {
                    self.fn_decl(f);
                }
            },
            ItemKind::Trait(t) => {
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        if f.body.is_some() {
                            self.fn_decl(f);
                        }
                    }
                }
            },
            ItemKind::Impl(imp) => {
                for f in &imp.fns {
                    if f.body.is_some() {
                        self.fn_decl(f);
                    }
                }
            },
            ItemKind::Struct(s) => self.deinit_shape(s),
            ItemKind::Local(_)
            | ItemKind::ExternFn(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Import(_) => {},
        }
    }

    /// One function body: parameters bind (owned only in `move` mode
    /// [C2]), the body walks, and live owned parameters drop at the end.
    fn fn_decl(&mut self, f: &'a FnDecl) {
        self.scopes.push(Vec::new());
        for param in &f.params {
            self.bind_param(param);
        }
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.drop_scope(f.body.as_ref().map_or(f.name.span, |b| b.span));
    }

    /// One top-level binding: the initializer walks first (the resolver
    /// binds the pattern after it), then the pattern's definitions come
    /// live. Module-lifetime bindings have no scope-exit death [G3].
    fn top_local(&mut self, local: &'a LocalDecl) {
        self.expr(&local.init, true);
        self.bind_pattern_defs(&local.pat);
    }

    /// The declarative deinit checks [C5]: `deinit` lives on `~Copyable`
    /// declarations only, and at most once per struct.
    fn deinit_shape(&mut self, s: &'a StructDecl) {
        let linear = s.supers.iter().any(linear_marker);
        let mut first: Option<Span> = None;
        for member in &s.members {
            if let StructMember::Deinit { span, .. } = member {
                if let Some(first_span) = first {
                    self.out.diags.push(
                        Diagnostic::error(
                            ErrorCode::DeinitIllFormed,
                            "a struct declares at most one `deinit` [C5]".to_owned(),
                        )
                        .with_primary(*span, "second `deinit`")
                        .with_secondary(first_span, "first `deinit`"),
                    );
                } else {
                    first = Some(*span);
                }
                if !linear {
                    self.out.diags.push(
                        Diagnostic::error(
                            ErrorCode::DeinitIllFormed,
                            "deinit is restricted to `~Copyable` types [C5]".to_owned(),
                        )
                        .with_primary(*span, "deinit outside a `~Copyable` declaration"),
                    );
                }
            }
        }
    }

    // ---- scopes and binders ---------------------------------------------------

    fn bind_param(&mut self, param: &'a Param) {
        let copyable = self
            .facts
            .get(&param.name.span)
            .is_none_or(|fact| fact.copyable);
        self.bind_binder(
            param.name.span,
            &param.name.name,
            copyable,
            matches!(param.mode, ParamMode::Consume(_)),
        );
    }

    fn bind_binder(&mut self, span: Span, name: &'a str, copyable: bool, owned: bool) {
        if self.audit {
            return;
        }
        let place = Place::Local(span);
        self.info.insert(place, Info {
            name,
            copyable,
            owned,
            epoch: self.epoch,
        });
        self.state.insert(place, State::Live);
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(place);
        }
    }

    /// Pattern binders of a local or arm: owned, facts from the walk.
    fn bind_pattern(&mut self, pat: &'a Pattern) {
        for (span, name) in Self::pattern_binders(pat) {
            let copyable = self.facts.get(&span).is_none_or(|fact| fact.copyable);
            self.bind_binder(span, name, copyable, true);
        }
    }

    /// Pattern binders as top-level definitions [G3]: owned, copyable from
    /// the memoized top-level types, no scope-exit death.
    fn bind_pattern_defs(&mut self, pat: &'a Pattern) {
        for (_, name) in Self::pattern_binders(pat) {
            let Some(def) = self.package.lookup(name) else {
                continue;
            };
            let place = Place::Def(def);
            let copyable = self
                .def_types
                .get(&def)
                .is_some_and(|ty| ty_copyable(self.decls, *ty, &HashMap::new()));
            self.info.insert(place, Info {
                name,
                copyable,
                owned: true,
                epoch: 0,
            });
            self.state.insert(place, State::Live);
        }
    }

    /// One pattern's binder (name span, name) pairs, in walk order.
    fn pattern_binders(pat: &'a Pattern) -> Vec<(Span, &'a str)> {
        let mut out = Vec::new();
        Self::pattern_binders_into(pat, &mut out);
        out
    }

    fn pattern_binders_into<'x>(pat: &'x Pattern, out: &mut Vec<(Span, &'x str)>) {
        match pat {
            Pattern::Binding { span, name } => out.push((*span, name.name.as_str())),
            Pattern::Struct { fields, .. } => {
                for (name, sub) in fields {
                    match sub {
                        Some(sub) => Self::pattern_binders_into(sub, out),
                        // Punning binds the field name.
                        None => out.push((name.span, name.name.as_str())),
                    }
                }
            },
            Pattern::Tuple {
                pats,
                trailing_comma,
                ..
            } => {
                if pats.len() == 1 && !trailing_comma {
                    Self::pattern_binders_into(&pats[0], out);
                } else {
                    for sub in pats {
                        Self::pattern_binders_into(sub, out);
                    }
                }
            },
            Pattern::Call { args, .. } => {
                for sub in args {
                    Self::pattern_binders_into(sub, out);
                }
            },
            Pattern::Or { pats, .. } => {
                for sub in pats {
                    Self::pattern_binders_into(sub, out);
                }
            },
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. }
            | Pattern::Path { .. } => {},
        }
    }

    /// Records the scope-exit deaths of one scope's live owned
    /// non-copyable binders [C5], in bind order.
    fn drop_scope(&mut self, at: Span) {
        let places = self.scopes.pop().unwrap_or_default();
        if self.audit {
            return;
        }
        for place in places {
            if self.owns_live_noncopyable(place) {
                self.out.drops.push(DropSite {
                    binder: place.binder_span(self.package),
                    at,
                });
            }
        }
    }

    fn owns_live_noncopyable(&self, place: Place) -> bool {
        self.info.get(&place).copied().is_some_and(|info| {
            info.owned && !info.copyable && matches!(self.state.get(&place), Some(State::Live))
        })
    }

    // ---- the read/move core -----------------------------------------------------

    /// One use of a binding: checks initialization and liveness, records
    /// the last use, and moves when the use is consuming [C1].
    fn read(&mut self, place: Place, at: Span, consuming: bool) {
        let Some(info) = self.info.get(&place).copied() else {
            return;
        };
        match self.state.get(&place).copied() {
            Some(State::Live) => {
                // Inside a lambda or spawn body, using an outer binding is
                // a capture: the closure owns the value from here on [C1].
                let captured = self.epoch > info.epoch;
                let moving = info.owned && !info.copyable && (consuming || captured);
                self.last_use.insert(place, (at, !moving));
                if moving && !self.audit {
                    self.out.moves.push(Move {
                        binder: place.binder_span(self.package),
                        at,
                    });
                    self.state.insert(place, State::Dead { at });
                }
            },
            Some(State::Dead { at: moved_at }) => {
                self.report_once(place, at, moved_diag(info.name, at, moved_at));
            },
            Some(State::Uninit) => {
                let diag = Self::uninit_diag(info.name, place, at, self.package);
                self.report_once(place, at, diag);
            },
            None => {},
        }
    }

    /// Pushes `diag` for this use site unless the audit round already
    /// reported it.
    fn report_once(&mut self, place: Place, at: Span, diag: Diagnostic) {
        if self.reported.insert((place, at)) {
            self.out.diags.push(diag);
        }
    }

    // ---- statements ------------------------------------------------------------

    fn block(&mut self, block: &'a BlockExpr) -> bool {
        self.scopes.push(Vec::new());
        let mut diverges = false;
        for stmt in &block.stmts {
            if self.stmt(stmt) {
                diverges = true;
            }
        }
        self.drop_scope(block.span);
        diverges
    }

    fn stmt(&mut self, stmt: &'a Stmt) -> bool {
        match &stmt.kind {
            ds_parse::StmtKind::Local(local) => {
                self.local(local);
                false
            },
            ds_parse::StmtKind::Expr(expr) => self.expr(expr, true),
        }
    }

    fn local(&mut self, local: &'a LocalDecl) {
        self.expr(&local.init, true);
        self.bind_pattern(&local.pat);
    }

    // ---- expressions -------------------------------------------------------------

    /// Walks one expression. `consuming` says whether a binding used at
    /// this position moves (value position) or borrows (window, test,
    /// structural comparison). Returns whether every path exits early.
    fn expr(&mut self, e: &'a Expr, consuming: bool) -> bool {
        match e {
            Expr::Literal { .. } | Expr::RawStr { .. } => false,
            Expr::Str { parts, .. } => {
                for part in parts {
                    if let ds_parse::StrPart::Expr { expr, .. } = part {
                        self.expr(expr, true);
                    }
                }
                false
            },
            Expr::Path { path, .. } => {
                self.path_use(path, consuming);
                false
            },
            Expr::Paren { inner, .. } => self.expr(inner, consuming),
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item, true);
                }
                false
            },
            Expr::Struct { fields, .. } => {
                for (name, value) in fields {
                    match value {
                        Some(expr) => {
                            self.expr(expr, true);
                        },
                        // Punning is a value use of the name [B3].
                        None => self.name_use(name, true),
                    }
                }
                false
            },
            Expr::Lambda { .. } => self.lambda(e),
            Expr::Call { .. } => self.call(e),
            // A field projection: in a borrowed position it reads the
            // receiver; in a consuming position a copyable field reads
            // while a linear field transfers; inside a capture context it
            // always transfers (the closure must own the receiver).
            Expr::Field { recv, name, .. } => {
                let transfers = self.projection_transfers(recv, name, consuming);
                self.expr(recv, transfers)
            },
            Expr::Index { recv, index, .. } => {
                self.expr(recv, consuming);
                self.expr(index, true);
                false
            },
            // The operand's value flows into the split; the failing path
            // exits the function, so no join is needed.
            Expr::Try { expr, .. } | Expr::Unary { expr, .. } => self.expr(expr, true),
            // `is` narrows by borrowing.
            Expr::Is { expr, .. } => self.expr(expr, false),
            Expr::Binary {
                op, left, right, ..
            } => {
                // Structural comparison borrows its operands [B9].
                let borrowed = matches!(op, BinaryOp::Eq | BinaryOp::Ne);
                self.expr(left, !borrowed);
                self.expr(right, !borrowed);
                false
            },
            Expr::Range { start, end, .. } => {
                self.expr(start, true);
                self.expr(end, true);
                false
            },
            Expr::Or { left, right, .. } => self.or_expr(left, right),
            Expr::Assign { .. } => self.assign(e),
            Expr::If {
                cond, then, else_, ..
            } => self.if_expr(cond, then, else_.as_deref()),
            Expr::Match {
                scrutinee, arms, ..
            } => self.match_expr(scrutinee, arms),
            Expr::While { cond, body, .. } => self.while_expr(cond, body),
            Expr::For {
                pat, iter, body, ..
            } => self.for_expr(pat, iter, body),
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr, true);
                }
                true
            },
            Expr::Break(_) | Expr::Continue(_) => true,
            // The nursery body runs now; `defer` blocks are walked at
            // their source position until defer lowering lands [E5].
            Expr::Nursery { body, .. } | Expr::Defer { body, .. } => self.block(body),
            // The child body's uses are captures: ownership moves at the
            // spawn expression, on the current state [E2].
            Expr::Spawn { body, .. } => {
                self.epoch += 1;
                self.block(body);
                self.epoch -= 1;
                false
            },
            Expr::Block(block) => self.block(block),
        }
    }

    fn path_use(&mut self, path: &Path, consuming: bool) {
        let [only] = path.segments.as_slice() else {
            return;
        };
        match self.res.and_then(|res| res.get(only.span)) {
            Some(Res::Local { bind }) => self.read(Place::Local(*bind), only.span, consuming),
            Some(Res::Def(def)) => {
                // Function, type, and tag names used as values are
                // copyable references; only const/var bindings carry
                // linear values.
                if self.is_data_def(*def) {
                    self.read(Place::Def(*def), only.span, consuming);
                }
            },
            None | Some(Res::TraitMethod(_) | Res::Dot(_) | Res::Handler) => {},
        }
    }

    fn name_use(&mut self, name: &'a Ident, consuming: bool) {
        match self.res.and_then(|res| res.get(name.span)) {
            Some(Res::Local { bind }) => self.read(Place::Local(*bind), name.span, consuming),
            Some(Res::Def(def)) => {
                if self.is_data_def(*def) {
                    self.read(Place::Def(*def), name.span, consuming);
                }
            },
            None | Some(Res::TraitMethod(_) | Res::Dot(_) | Res::Handler) => {},
        }
    }

    fn is_data_def(&self, def: DefId) -> bool {
        matches!(self.package.def(def).kind, DefKind::Const | DefKind::Var)
    }

    fn lambda(&mut self, e: &'a Expr) -> bool {
        let Expr::Lambda {
            params, body, span, ..
        } = e
        else {
            return false;
        };
        self.scopes.push(Vec::new());
        for param in params {
            self.bind_param(param);
        }
        self.epoch += 1;
        let diverges = self.expr(body, true);
        self.epoch -= 1;
        self.drop_scope(*span);
        diverges
    }

    fn or_expr(&mut self, left: &'a Expr, right: &'a OrTail) -> bool {
        self.expr(left, true);
        let after_left = self.state.clone();
        let tail_diverges = match right {
            OrTail::Expr(expr) => self.expr(expr, true),
            OrTail::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr, true);
                }
                true
            },
            OrTail::Break(_) | OrTail::Continue(_) => true,
        };
        if tail_diverges {
            self.state = after_left;
        } else {
            self.state = merge_states(&after_left, &self.state);
        }
        false
    }

    fn assign(&mut self, e: &'a Expr) -> bool {
        let Expr::Assign {
            op,
            left,
            right,
            span,
        } = e
        else {
            return false;
        };
        let Some(place) = self.place_of(left) else {
            // Field assignment is undecided surface [D2]; the type walk
            // already reports it.
            self.expr(left, false);
            self.expr(right, true);
            return false;
        };
        if *op == ds_parse::AssignOp::Plain {
            self.expr(right, true);
            if !self.audit && self.owns_live_noncopyable(place) {
                // The replaced value dies at the assignment.
                self.out.drops.push(DropSite {
                    binder: place.binder_span(self.package),
                    at: *span,
                });
            }
            if !self.audit {
                self.state.insert(place, State::Live);
            }
        } else {
            // Compound assignment reads the current value, then mutates it.
            self.read(place, left.span(), false);
            self.expr(right, true);
        }
        false
    }

    /// The uninit-use diagnostic: the use span primary, the binder secondary.
    fn uninit_diag(name: &str, place: Place, at: Span, package: &Package) -> Diagnostic {
        Diagnostic::error(
            ErrorCode::UninitializedUse,
            format!("binding `{name}` may not be initialized on this path"),
        )
        .with_primary(at, "may be uninitialized here")
        .with_secondary(place.binder_span(package), "bound here")
    }

    /// Whether a field projection in this position moves its receiver: it
    /// does inside capture contexts (the receiver must be owned there), in
    /// borrowed positions it never does, and elsewhere only when the
    /// projected field itself is non-copyable. Unknown shapes (unions,
    /// generic constructors) transfer conservatively.
    fn projection_transfers(&self, recv: &'a Expr, name: &Ident, consuming: bool) -> bool {
        if self.epoch
            > self
                .place_of(recv)
                .and_then(|place| self.info.get(&place).map(|info| info.epoch))
                .unwrap_or(0)
        {
            return true;
        }
        if !consuming {
            return false;
        }
        let Some(ty) = self.place_of(recv).and_then(|place| self.place_ty(place)) else {
            return true;
        };
        let mut all_copy = true;
        for atom in self.decls.universe.members(ty) {
            let Ok(decl) = self.decls.universe.atom_decl(*atom) else {
                return true;
            };
            if decl.name.starts_with('$') {
                return true;
            }
            match &decl.kind {
                AtomKind::Struct(fields) => {
                    let Some(field) = fields.iter().find(|f| f.name == Some(name.name.clone()))
                    else {
                        // A member without the field makes the projection
                        // ill-typed; let the type walk report it.
                        continue;
                    };
                    all_copy &=
                        expr_copyable(self.decls, &field.ty, &HashMap::new(), &mut Vec::new());
                },
                // Unions of shapes, generic applications, enums: the
                // projected type is not decidable here.
                _ => return true,
            }
        }
        !all_copy
    }

    fn place_ty(&self, place: Place) -> Option<Ty> {
        match place {
            Place::Local(span) => self.facts.get(&span).map(|fact| fact.ty),
            Place::Def(def) => self.def_types.get(&def).copied(),
        }
    }

    fn place_of(&self, left: &'a Expr) -> Option<Place> {
        let Expr::Path { path, .. } = peel(left) else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        match self.res.and_then(|res| res.get(only.span)) {
            Some(Res::Local { bind }) => Some(Place::Local(*bind)),
            Some(Res::Def(def)) if self.is_data_def(*def) => Some(Place::Def(*def)),
            _ => None,
        }
    }

    fn if_expr(&mut self, cond: &'a Expr, then: &'a BlockExpr, else_: Option<&'a Expr>) -> bool {
        self.expr(cond, true);
        let entry = self.state.clone();
        let then_diverges = self.block(then);
        let then_state = std::mem::take(&mut self.state);
        self.state = entry;
        let (else_diverges, else_state) = match else_ {
            Some(else_expr) => {
                let diverges = self.expr(else_expr, true);
                (diverges, std::mem::take(&mut self.state))
            },
            None => (false, self.state.clone()),
        };
        match (then_diverges, else_diverges) {
            (true, true) => {
                self.state = then_state;
                true
            },
            (true, false) => {
                self.state = else_state;
                false
            },
            (false, true) => {
                self.state = then_state;
                false
            },
            (false, false) => {
                self.state = merge_states(&then_state, &else_state);
                false
            },
        }
    }

    fn match_expr(&mut self, scrutinee: &'a Expr, arms: &'a [MatchArm]) -> bool {
        self.expr(scrutinee, true);
        let entry = std::mem::take(&mut self.state);
        let mut arms_out: Vec<(bool, States)> = Vec::with_capacity(arms.len());
        for arm in arms {
            self.state.clone_from(&entry);
            self.scopes.push(Vec::new());
            self.bind_pattern(&arm.pat);
            let mut diverges = arm
                .guard
                .as_ref()
                .is_some_and(|guard| self.expr(guard, true));
            if !diverges {
                diverges = self.expr(&arm.body, true);
            }
            self.drop_scope(arm.span);
            arms_out.push((diverges, std::mem::take(&mut self.state)));
        }
        let producing: Vec<States> = arms_out
            .iter()
            .filter(|(diverges, _)| !diverges)
            .map(|(_, state)| state.clone())
            .collect();
        self.state = match producing.len() {
            0 => arms_out.last().map_or(entry, |(_, state)| state.clone()),
            1 => producing.into_iter().next().unwrap_or_default(),
            _ => producing
                .into_iter()
                .reduce(|acc, state| merge_states(&acc, &state))
                .unwrap_or_default(),
        };
        arms_out.iter().all(|(diverges, _)| *diverges)
    }

    fn while_expr(&mut self, cond: &'a Expr, body: &'a BlockExpr) -> bool {
        self.expr(cond, true);
        let entry = self.state.clone();
        self.block(body);
        let merged = merge_states(&entry, &self.state);
        // The loop re-executes the condition and body on the back edge:
        // one audit round against the merged state [C1].
        let audited = self.audit;
        self.state = merged.clone();
        self.audit = true;
        self.expr(cond, true);
        self.block(body);
        self.audit = audited;
        self.state = merged;
        false
    }

    fn for_expr(&mut self, pat: &'a Pattern, iter: &'a Expr, body: &'a Expr) -> bool {
        // The iterable feeds a linear cursor: a consuming use [D3].
        self.expr(iter, true);
        let entry = self.state.clone();
        self.scopes.push(Vec::new());
        self.bind_pattern(pat);
        self.expr(body, true);
        self.drop_scope(body.span());
        let merged = merge_states(&entry, &self.state);
        // The body re-executes per iteration; pattern binders are fresh
        // each round, so only outer bindings matter here.
        let audited = self.audit;
        self.state = merged.clone();
        self.audit = true;
        self.expr(body, true);
        self.audit = audited;
        self.state = merged;
        false
    }

    // ---- calls -----------------------------------------------------------------

    fn call(&mut self, e: &'a Expr) -> bool {
        let Expr::Call {
            callee,
            args,
            trailing,
            ..
        } = e
        else {
            return false;
        };
        let callee = peel(callee);
        if let Expr::Field { recv, name, .. } = callee {
            return self.dot_call(recv, name, args, trailing.as_deref());
        }
        if let Expr::Path { path, .. } = callee {
            match path.segments.as_slice() {
                [only] => {
                    if let Some(Res::Def(def)) =
                        self.res.and_then(|res| res.get(only.span)).cloned()
                    {
                        if let Some(sig) = self.decls.fns.get(&def).cloned() {
                            return self.sig_call(&sig, None, args, trailing.as_deref());
                        }
                    }
                },
                [_, method] => {
                    if let Some(Res::TraitMethod(method_id)) =
                        self.res.and_then(|res| res.get(method.span)).cloned()
                    {
                        if let Some(sig) = self.member_sig(method_id) {
                            return self.sig_call(&sig, None, args, trailing.as_deref());
                        }
                    }
                },
                _ => {},
            }
        }
        // An indirect or unknown callee: borrow everything (call-scoped
        // windows close at the call [C2]).
        self.expr(callee, false);
        for arg in args {
            self.expr(&arg.value, false);
        }
        if let Some(tail) = trailing.as_deref() {
            self.expr(tail, false);
        }
        false
    }

    fn dot_call(
        &mut self,
        recv: &'a Expr,
        name: &'a Ident,
        args: &'a [CallArg],
        trailing: Option<&'a Expr>,
    ) -> bool {
        let Some(res) = self.res.and_then(|res| res.get(name.span)).cloned() else {
            self.expr(recv, false);
            self.borrow_args(args, trailing);
            return false;
        };
        match res {
            Res::Handler => {
                // `.err(h)`/`.ok(h)`: the operand's value feeds the split.
                self.expr(recv, true);
                for arg in args {
                    self.expr(&arg.value, true);
                }
                false
            },
            Res::Dot(DotRes::Free { func, fed_param }) => {
                if let Some(sig) = self.decls.fns.get(&func).cloned() {
                    self.sig_call(&sig, (fed_param == 0).then_some(recv), args, trailing)
                } else {
                    self.expr(recv, false);
                    self.borrow_args(args, trailing);
                    false
                }
            },
            Res::Dot(DotRes::Methods { candidates }) => {
                let sigs: Vec<FnSig> = candidates
                    .iter()
                    .filter_map(|id| self.member_sig(*id))
                    .collect();
                let receiver_consumes = sigs.iter().all(|sig| {
                    sig.params
                        .first()
                        .is_some_and(|p| matches!(p.mode, ParamMode::Consume(_)))
                });
                self.expr(recv, receiver_consumes);
                self.borrow_args(args, trailing);
                false
            },
            _ => {
                self.expr(recv, false);
                self.borrow_args(args, trailing);
                false
            },
        }
    }

    fn borrow_args(&mut self, args: &'a [CallArg], trailing: Option<&'a Expr>) {
        for arg in args {
            self.expr(&arg.value, false);
        }
        if let Some(tail) = trailing {
            self.expr(tail, false);
        }
    }

    fn member_sig(&self, method: MethodId) -> Option<FnSig> {
        self.decls
            .members
            .get(&method)
            .or_else(|| self.decls.inherent.get(&method))
            .cloned()
    }

    /// Walks one call's arguments per the callee's parameter modes: an
    /// argument consumes its binding only in `move` mode [C2].
    fn sig_call(
        &mut self,
        sig: &FnSig,
        fed: Option<&'a Expr>,
        args: &'a [CallArg],
        trailing: Option<&'a Expr>,
    ) -> bool {
        let slots = Self::slots(fed, 0, args, trailing, sig);
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            if let Some(arg) = slot {
                self.expr(arg, matches!(param.mode, ParamMode::Consume(_)));
            }
        }
        false
    }

    /// Maps arguments to parameters like the type walk does (fed receiver,
    /// named, positional, trailing), without its diagnostics.
    fn slots<'x>(
        fed: Option<&'x Expr>,
        skip: usize,
        args: &'x [CallArg],
        trailing: Option<&'x Expr>,
        sig: &FnSig,
    ) -> Vec<Option<&'x Expr>> {
        let mut slots: Vec<Option<&'x Expr>> = vec![None; sig.params.len()];
        let mut next = skip;
        if let Some(fed) = fed {
            if let Some(first) = slots.first_mut() {
                *first = Some(fed);
                next = 1;
            }
        }
        for arg in args {
            match &arg.name {
                Some(name) => {
                    if let Some(index) = sig.params.iter().position(|p| p.name == name.name) {
                        if slots[index].is_none() {
                            slots[index] = Some(&arg.value);
                        }
                    }
                },
                None => {
                    if next < slots.len() {
                        slots[next] = Some(&arg.value);
                        next += 1;
                    }
                },
            }
        }
        if let Some(tail) = trailing {
            if next < slots.len() && slots[next].is_none() {
                slots[next] = Some(tail);
            }
        }
        slots
    }
}

// ---- helpers ------------------------------------------------------------------

fn moved_diag(name: &str, at: Span, moved_at: Span) -> Diagnostic {
    if moved_at == at {
        Diagnostic::error(
            ErrorCode::MovedThenUsed,
            format!("value `{name}` is moved again on a re-executing path"),
        )
        .with_primary(at, "moved here")
        .with_note(format!(
            "`{name}` is `~Copyable`; re-initialize it before the path re-executes"
        ))
    } else {
        Diagnostic::error(
            ErrorCode::MovedThenUsed,
            format!("value `{name}` is no longer usable"),
        )
        .with_secondary(moved_at, "moved here")
        .with_primary(at, "used again here after move")
        .with_note(format!("`{name}` is `~Copyable` and was consumed"))
    }
}

/// Live after a join only when live on every path; the first move wins
/// the reported span [C1].
fn merge_states(a: &States, b: &States) -> States {
    let mut out = a.clone();
    for (place, state) in b {
        if matches!(out.get(place), Some(State::Live)) && !matches!(state, State::Live) {
            out.insert(*place, *state);
        }
    }
    out
}

fn peel(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => peel(inner),
        _ => expr,
    }
}
