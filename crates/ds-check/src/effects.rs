//! The `effects_of` pass: per-definition effect rows over the parsed and
//! resolved surface [B5, B7, E1, E2, E4].
//!
//! v1 has no effect syntax [E1] — rows are computed, never written. The
//! computation is a pure walk over the package's trees and the resolution
//! side table [`crate::resolve`]: it emits no diagnostics, and unresolved
//! names contribute nothing (the ambient row table [`DeclaredRows`] is the
//! surface for world-touching functions until M7's platform declaration
//! supplies it). Rows carry the record's five-effect vocabulary
//! ([`Effect`]).
//!
//! # Composition
//!
//! A row is the union of the definition's declared row, its body's
//! intrinsic contributions, and one edge per resolved call or value use,
//! unioning the callee's row. `?` and `or return` contribute `error`
//! [E4]; trapping integer defaults contribute `trap` [B7]; a `nursery`
//! block contributes `cancel | discontinue` [E1, E2] — the internal
//! effects its boundary dispatches evidence for. The E4 consuming forms
//! mask their operand's `error`: a plain `or` extracts the wanted member
//! and `.err(h)`/`.ok(h)` handle the failure set, so their operand's call
//! edges carry `error` subtracted; `or return` additionally contributes
//! `error` because the early exit hands the failure out. Lambda bodies
//! contribute in place — combinators and spawn blocks run them inside the
//! enclosing definition's dynamic extent [D3, E2]. Rows grow monotonically
//! over the call graph to a least fixpoint, so recursion and
//! intra-package cycles terminate by construction [G3].
//!
//! # Over-approximation, deliberate
//!
//! Rows never miss an effect but may carry more: trapping operators
//! contribute `trap` without solved operand types (floats never trap
//! [B7] — the checker's per-expression types are not exposed to this
//! pass); a method call unions every registered candidate and a trait
//! member's row unions every impl body [F3]; a match scrutinee keeps its
//! operand's `error` since arms may hand it on; `deinit` blocks [C5] are
//! walked by no definition — their effects ride with the value's owning
//! function once the affine pass places deaths (M4). Supersets fail
//! closed: M7's platform subset check may over-reject, never silently
//! accept.
//!
//! # Example
//!
//! ```
//! use ds_check::{DeclaredRows, Effect, EffectRow, effects_of};
//! use ds_diag::Files;
//! use ds_parse::parse;
//!
//! let mut files = Files::new();
//! let file = files.add_source(
//!     "net.ds",
//!     concat!(
//!         "extern fn fetch (u: int) -> int | io_err\n",
//!         "const body = (u: int) -> int { fetch(u)? }\n",
//!     ),
//! );
//! let ast = parse(&files, file).expect("parses");
//! let declared = DeclaredRows::new().with(
//!     "fetch",
//!     EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
//! );
//! let effects = effects_of(&[ast], &declared);
//!
//! // `body` unions the declared ambient row with its own `?` [E4].
//! let body = effects.of_name("body").expect("row-bearing definition");
//! assert!(body.contains(Effect::World));
//! assert_eq!(body.render(), "error | world");
//! ```

use std::collections::HashMap;

use ds_diag::Span;
use ds_parse::{
    AssignOp, Ast, BinaryOp, BlockExpr, CallArg, Expr, FnDecl, Ident, ImplDecl, Item, ItemKind,
    LocalDecl, MatchArm, Path, Pattern, StmtKind, StrPart, TraitDecl, TraitMember, UnaryOp,
};

use crate::resolve::{DefId, DefKind, DotRes, MethodId, MethodOwner, Package, Res, Resolutions};

/// One named effect. The vocabulary is the record's set: `error` [B5, E4]
/// — union-member failure a definition can hand to its caller; `trap`
/// [B7, E4] — the unhandleable bug boundary of panics and trapping
/// integer defaults; `world` [E1] — the world-touching capability surface,
/// entering through [`DeclaredRows`] until M7's platform declaration
/// supplies it; `cancel` and `discontinue` [E1] — the two internal effects
/// a `nursery` boundary dispatches evidence for.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Effect {
    /// Union-member failure handed to a caller [B5, E4].
    Error,
    /// The unhandleable bug boundary [B7, E4].
    Trap,
    /// The world-touching capability surface [E1].
    World,
    /// The nursery cancellation internal effect [E1, E2].
    Cancel,
    /// The nursery discontinue internal effect [E1].
    Discontinue,
}

impl Effect {
    /// The effect's canonical row spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Trap => "trap",
            Self::World => "world",
            Self::Cancel => "cancel",
            Self::Discontinue => "discontinue",
        }
    }
}

/// A set of named effects: one row. Structural equality is set equality;
/// the row is a bitset over [`Effect`]. The empty row is
/// [`EffectRow::PURE`] — a pure definition.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct EffectRow(u8);

impl EffectRow {
    /// The empty row.
    pub const PURE: Self = Self(0);

    /// The singleton row of `effect`.
    #[must_use]
    pub const fn of(effect: Effect) -> Self {
        Self(1 << (effect as u8))
    }

    /// Set union.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Set difference: `self` minus every effect of `other`.
    #[must_use]
    pub const fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether `effect` is in the row.
    #[must_use]
    pub const fn contains(self, effect: Effect) -> bool {
        self.0 & Self::of(effect).0 != 0
    }

    /// Whether the row is empty — a pure definition.
    #[must_use]
    pub const fn is_pure(self) -> bool {
        self.0 == 0
    }

    /// The row's effects, in vocabulary order.
    pub fn effects(self) -> impl Iterator<Item = Effect> {
        const VOCAB: [Effect; 5] = [
            Effect::Error,
            Effect::Trap,
            Effect::World,
            Effect::Cancel,
            Effect::Discontinue,
        ];
        VOCAB
            .into_iter()
            .filter(move |effect| self.contains(*effect))
    }

    /// Renders the row: `error | world` in vocabulary order, or `pure`
    /// when empty.
    #[must_use]
    pub fn render(self) -> String {
        let joined = self
            .effects()
            .map(Effect::name)
            .collect::<Vec<_>>()
            .join(" | ");
        if joined.is_empty() { "pure".to_owned() } else { joined }
    }
}

impl std::ops::BitOr for EffectRow {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for EffectRow {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// Rows the embedding surface declares for ambient functions — externs,
/// the stdlib, platform capabilities [E1, H5]. v1 has no effect syntax, so
/// this table is the only row input the surface cannot express; M7's
/// `platform "..." provides <...>` declaration is its source. Applied by
/// name; a declared row is a lower bound on the computed row. Absent names
/// carry [`EffectRow::PURE`].
#[derive(Clone, Debug, Default)]
pub struct DeclaredRows {
    rows: HashMap<String, EffectRow>,
}

impl DeclaredRows {
    /// No declared rows: every ambient name is row-pure.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
        }
    }

    /// Builder: declares `name`'s row.
    #[must_use]
    pub fn with(mut self, name: &str, row: EffectRow) -> Self {
        self.rows.insert(name.to_owned(), row);
        self
    }

    /// The declared row of `name`, [`EffectRow::PURE`] when absent.
    #[must_use]
    pub fn row(&self, name: &str) -> EffectRow {
        self.rows.get(name).copied().unwrap_or(EffectRow::PURE)
    }
}

/// One value-producing definition's computed row, with the identity
/// consumers key on (the package itself is not re-exposed; ids are stable
/// within one computation).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DefEffects {
    /// The definition's package id.
    pub def: DefId,
    /// The declared name.
    pub name: String,
    /// What the definition is.
    pub kind: DefKind,
    /// The computed row.
    pub row: EffectRow,
}

/// The computed rows of one package: every value-producing definition
/// (named and extern functions, bindings). Type declarations carry no row
/// and report `None` from [`Effects::of`].
#[derive(Clone, Default)]
pub struct Effects {
    /// Row-bearing definitions, in declaration order.
    defs: Vec<DefEffects>,
    /// DefId index into `defs`; `None` for type-level definitions.
    index: Vec<Option<u32>>,
    /// First-wins name lookup, mirroring the package table.
    names: HashMap<String, DefId>,
}

impl Effects {
    /// The row of `def`: `None` when out of range or when `def` is a
    /// type-level declaration.
    #[must_use]
    pub fn of(&self, def: DefId) -> Option<EffectRow> {
        let position = self.index.get(def.index() as usize).copied().flatten()?;
        Some(self.defs[position as usize].row)
    }

    /// The row of the named definition: `None` when unknown or
    /// type-level.
    #[must_use]
    pub fn of_name(&self, name: &str) -> Option<EffectRow> {
        self.of(*self.names.get(name)?)
    }

    /// Every row-bearing definition, in declaration order.
    #[must_use]
    pub fn defs(&self) -> &[DefEffects] {
        &self.defs
    }

    /// The number of definitions the rows were computed over (DefId
    /// validity bound).
    #[must_use]
    pub fn def_count(&self) -> usize {
        self.index.len()
    }
}

/// One fixpoint node: a package definition, a registered method, or a
/// span-keyed anonymous node (trait-impl functions, which register as
/// neither, and shared top-level initializers).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Node {
    /// A package definition.
    Def(DefId),
    /// A registered method (trait member or inherent impl function).
    Method(MethodId),
    /// A span-keyed anonymous node.
    Anon(Span),
}

/// One call edge: union the callee's row minus `mask`.
#[derive(Clone, Copy, Debug)]
struct Edge {
    /// The callee.
    callee: Node,
    /// Effects consumed before the value flows on (the E4 forms mask
    /// `error`).
    mask: EffectRow,
}

/// One node's fixpoint inputs: its own contributions plus its call edges.
#[derive(Clone, Default, Debug)]
struct Info {
    /// Own contributions: declared row and intrinsics.
    base: EffectRow,
    /// Call edges.
    edges: Vec<Edge>,
}

/// B7's trapping defaults: integer add/sub/mul, div/rem, and shifts trap
/// (the wrapping forms never do; unary `-` traps on int-min).
const fn traps_binary(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::Shl
            | BinaryOp::Shr
    )
}

/// B7's trapping unary: `-` traps on int-min, `-%` wraps, `!` is pure.
const fn traps_unary(op: UnaryOp) -> bool {
    matches!(op, UnaryOp::Neg)
}

/// B7's trapping compound assignments: every arithmetic form; `=` is
/// pure.
const fn traps_assign(op: AssignOp) -> bool {
    !matches!(op, AssignOp::Plain)
}

/// Computes the package's effect rows: collects and resolves the files
/// (the same collection and resolution passes
/// [`type_check`](crate::type_check) runs), walks every definition's
/// body, and solves the call graph.
///
/// Pure by contract: no diagnostics are emitted or consumed — a
/// resolution failure leaves the failing name contributing nothing, and
/// rows are computed whatever the file's items check like. The package is
/// every tree passed here, in order [G3]. `T: Borrow<Ast>` lets callers
/// pass owned trees or shared arcs (Borrow is the reflexive bound).
#[must_use]
pub fn effects_of<T>(asts: &[T], declared: &DeclaredRows) -> Effects
where
    T: std::borrow::Borrow<Ast>,
{
    let mut package = Package::new();
    for ast in asts {
        // duplicate-name diagnostics belong to the resolve query
        let _ = package.add_file(ast.borrow());
    }
    let count = u32::try_from(package.len()).expect("definition count fits u32");
    let mut graph = HashMap::new();
    // nodes up front, so edges are order-independent across files [G3]
    for raw in 0..count {
        let def = DefId::from_raw(raw);
        if matches!(
            package.def(def).kind,
            DefKind::Fn | DefKind::Const | DefKind::Var
        ) {
            graph.insert(Node::Def(def), Info::default());
        }
    }
    for ast in asts {
        let ast = ast.borrow();
        let (res, _) = package.resolve(ast);
        let mut walker = Walker {
            package: &package,
            declared,
            res: &res,
            graph: &mut graph,
            node: Node::Anon(Span::new(ds_diag::FileId::new(0), 0, 0)),
        };
        for block in &ast.blocks {
            for item in &block.items {
                walker.item(item);
            }
        }
    }
    let rows = solve(&graph);
    let mut defs = Vec::new();
    let mut index = vec![None; package.len()];
    let mut names = HashMap::new();
    for raw in 0..count {
        let def_id = DefId::from_raw(raw);
        let def = package.def(def_id);
        if matches!(def.kind, DefKind::Fn | DefKind::Const | DefKind::Var) {
            index[raw as usize] = Some(u32::try_from(defs.len()).expect("def count fits u32"));
            defs.push(DefEffects {
                def: def_id,
                name: def.name.clone(),
                kind: def.kind,
                row: rows
                    .get(&Node::Def(def_id))
                    .copied()
                    .unwrap_or(EffectRow::PURE),
            });
        }
        names.entry(def.name.clone()).or_insert(def_id);
    }
    Effects { defs, index, names }
}

/// Rows to the least fixpoint over the call graph: each node's row unions
/// its base with its edges' masked callee rows until nothing grows. Rows
/// only grow and the lattice is finite (two to the vocabulary size), so
/// this terminates.
fn solve(graph: &HashMap<Node, Info>) -> HashMap<Node, EffectRow> {
    let mut rows: HashMap<Node, EffectRow> = graph
        .iter()
        .map(|(node, info)| (*node, info.base))
        .collect();
    loop {
        let mut changed = false;
        for (node, info) in graph {
            let mut row = info.base;
            for edge in &info.edges {
                let callee = rows.get(&edge.callee).copied().unwrap_or(EffectRow::PURE);
                row = row.union(callee.minus(edge.mask));
            }
            let solved = rows.get_mut(node).expect("every node is seeded");
            if *solved != row {
                *solved = row;
                changed = true;
            }
        }
        if !changed {
            return rows;
        }
    }
}

/// One file's walk: fills the graph under the node each item enters.
struct Walker<'a, 'g> {
    package: &'a Package,
    declared: &'a DeclaredRows,
    res: &'a Resolutions,
    graph: &'g mut HashMap<Node, Info>,
    /// The node being contributed to; every walk path enters its node
    /// first.
    node: Node,
}

impl Walker<'_, '_> {
    /// Starts contributing to `node`.
    fn enter(&mut self, node: Node) {
        self.graph.entry(node).or_default();
        self.node = node;
    }

    /// Adds `row` to the current node's own contributions.
    fn contribute(&mut self, row: EffectRow) {
        let info = self
            .graph
            .get_mut(&self.node)
            .expect("walks enter their node first");
        info.base = info.base.union(row);
    }

    /// Adds one effect to the current node's own contributions.
    fn intrinsic(&mut self, effect: Effect) {
        self.contribute(EffectRow::of(effect));
    }

    /// Records one call edge. The callee's row is read at the fixpoint, so
    /// node creation order never matters; a callee outside the graph (a
    /// type used as a value) carries the pure row.
    fn edge(&mut self, callee: Node, mask: EffectRow) {
        let info = self
            .graph
            .get_mut(&self.node)
            .expect("walks enter their node first");
        info.edges.push(Edge { callee, mask });
    }

    // ---- items ------------------------------------------------------------

    /// Walks one item into the graph.
    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => self.fn_decl(f),
            ItemKind::Local(local) => self.top_local(local),
            ItemKind::Trait(t) => self.trait_decl(t),
            ItemKind::Impl(imp) => self.impl_decl(imp),
            // type declarations carry no row; `deinit` blocks are walked
            // by no definition — see the module docs
            ItemKind::Struct(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Import(_) => {},
        }
    }

    /// One named or extern function: its declared row plus its body
    /// (externs and trait signatures have none, so they carry exactly
    /// their declared row).
    fn fn_decl(&mut self, f: &FnDecl) {
        let Some(def) = self.package.lookup_value(&f.name.name) else {
            return;
        };
        self.enter(Node::Def(def));
        self.contribute(self.declared.row(&f.name.name));
        if let Some(body) = &f.body {
            self.block(body, EffectRow::PURE);
        }
    }

    /// One top-level binding: every name the pattern declares carries the
    /// initializer's row.
    fn top_local(&mut self, local: &LocalDecl) {
        self.bind_pattern(&local.pat, &local.init);
    }

    /// Binds the initializer's row to the pattern's definitions:
    /// element-wise when a tuple pattern destructures a tuple initializer
    /// (each binding's row is its own element's), shared whole for every
    /// other shape.
    fn bind_pattern(&mut self, pat: &Pattern, init: &Expr) {
        if let (Pattern::Tuple { pats, .. }, Expr::Tuple { items, .. }) = (pat, init) {
            if pats.len() == items.len() {
                for (pat, item) in pats.iter().zip(items.iter()) {
                    self.bind_pattern(pat, item);
                }
                return;
            }
        }
        let mut defs = Vec::new();
        self.pattern_defs(pat, &mut defs);
        if defs.is_empty() {
            return;
        }
        let scratch = Node::Anon(init.span());
        self.enter(scratch);
        self.expr(init, EffectRow::PURE);
        let (base, edges) = {
            let info = &self.graph[&scratch];
            (info.base, info.edges.clone())
        };
        for def in defs {
            let declared = self.declared.row(&self.package.def(def).name);
            let info = self.graph.entry(Node::Def(def)).or_default();
            info.base = info.base.union(declared).union(base);
            info.edges.extend(edges.iter().copied());
        }
    }

    /// One trait: each member's node carries its declared row and its
    /// default body when present; impl bodies flow in through
    /// [`Walker::impl_decl`].
    fn trait_decl(&mut self, t: &TraitDecl) {
        let Some(trait_def) = self.package.lookup_type(&t.name.name) else {
            return;
        };
        for member in &t.members {
            let TraitMember::Fn(f) = member else {
                continue;
            };
            let Some(m) = self.trait_member(trait_def, f) else {
                continue;
            };
            self.enter(Node::Method(m));
            self.contribute(self.declared.row(&f.name.name));
            if let Some(body) = &f.body {
                self.block(body, EffectRow::PURE);
            }
        }
    }

    /// One impl block: inherent functions register as methods; a
    /// trait-impl function is an anonymous node flowing into the trait
    /// member it implements, so a call through the trait unions every
    /// impl body [F3].
    fn impl_decl(&mut self, imp: &ImplDecl) {
        let trait_def = imp
            .trait_ref
            .as_ref()
            .and_then(|tr| tr.path.segments.first())
            .and_then(|head| match self.res.get(head.span) {
                Some(Res::Def(def)) if matches!(self.package.def(*def).kind, DefKind::Trait) => {
                    Some(*def)
                },
                _ => None,
            });
        for f in &imp.fns {
            let Some(body) = &f.body else {
                continue;
            };
            let declared = self.declared.row(&f.name.name);
            if let Some(trait_def) = trait_def {
                let anon = Node::Anon(f.name.span);
                self.enter(anon);
                self.contribute(declared);
                self.block(body, EffectRow::PURE);
                if let Some(m) = self.trait_member(trait_def, f) {
                    self.enter(Node::Method(m));
                    self.edge(anon, EffectRow::PURE);
                }
            } else {
                let Some(m) = self
                    .package
                    .method_candidates(&f.name.name)
                    .iter()
                    .copied()
                    .find(|&m| self.package.method(m).span == f.name.span)
                else {
                    continue;
                };
                self.enter(Node::Method(m));
                self.contribute(declared);
                self.block(body, EffectRow::PURE);
            }
        }
    }

    /// The member's registered method id, by owner: candidates are
    /// already name-filtered, and the owning trait disambiguates same-name
    /// members of different traits.
    fn trait_member(&self, trait_def: DefId, f: &FnDecl) -> Option<MethodId> {
        self.package
            .method_candidates(&f.name.name)
            .iter()
            .copied()
            .find(|&m| self.package.method(m).owner == MethodOwner::Trait(trait_def))
    }

    /// The package definitions a top-level binding pattern declares,
    /// through the shared binder walker.
    fn pattern_defs(&self, pat: &Pattern, out: &mut Vec<DefId>) {
        for (_, name) in crate::pattern::pattern_binders(pat) {
            if let Some(def) = self.package.lookup_value(name) {
                if !out.contains(&def) {
                    out.push(def);
                }
            }
        }
    }

    // ---- expressions ------------------------------------------------------

    /// Walks one expression's contributions into the current node.
    ///
    /// `mask` is the set of effects consumed before this expression's
    /// value flows on (the E4 consuming forms mask `error`): call edges
    /// subtract it from their callee's row, while intrinsic `error`
    /// contributions — the `?` and `or return` early exits — are control
    /// flow and never masked.
    fn expr(&mut self, e: &Expr, mask: EffectRow) {
        match e {
            Expr::Literal { .. } | Expr::RawStr { .. } | Expr::Break(_) | Expr::Continue(_) => {},
            Expr::Str { parts, .. } => self.string(parts, mask),
            Expr::Lambda { body, .. } => self.expr(body, mask),
            Expr::Path { path, .. } => self.path(path, mask),
            Expr::Struct { path, fields, .. } => self.struct_lit(path, fields, mask),
            Expr::List { items, .. } | Expr::Tuple { items, .. } => self.elements(items, mask),
            Expr::Paren { inner, .. } => self.expr(inner, mask),
            Expr::Range { start, end, .. } => {
                self.expr(start, mask);
                self.expr(end, mask);
            },
            Expr::Call {
                callee,
                args,
                trailing,
                ..
            } => self.call(callee, args, trailing.as_deref(), mask),
            Expr::Field { recv, name, .. } => self.field(recv, name, mask),
            Expr::Index { recv, index, .. } => {
                self.expr(recv, mask);
                self.expr(index, mask);
            },
            // The map application's bracketed arguments are type spellings
            // [F9]; they carry no value effects, and the head is a type
            // name — the list application's walk treats its own head the
            // same way.
            Expr::MapApply { key, value, .. } => {
                self.expr(key, mask);
                self.expr(value, mask);
            },
            Expr::Try { expr, .. } => {
                self.intrinsic(Effect::Error);
                self.expr(expr, mask);
            },
            Expr::Unary { op, expr, .. } => self.unary(*op, expr, mask),
            Expr::Is { expr, .. } => self.expr(expr, mask),
            Expr::Binary {
                op, left, right, ..
            } => self.binary(*op, left, right, mask),
            Expr::Assign {
                op, left, right, ..
            } => self.assign(*op, left, right, mask),
            Expr::If {
                cond, then, else_, ..
            } => self.if_expr(cond, then, else_.as_deref(), mask),
            Expr::Match {
                scrutinee, arms, ..
            } => self.match_expr(scrutinee, arms, mask),
            Expr::While { cond, body, .. } => {
                self.expr(cond, mask);
                self.block(body, mask);
            },
            Expr::For { iter, body, .. } => {
                self.expr(iter, mask);
                self.expr(body, mask);
            },
            Expr::Return { expr, .. } => {
                if let Some(e) = expr {
                    self.expr(e, mask);
                }
            },
            Expr::Nursery { body, .. } => {
                self.intrinsic(Effect::Cancel);
                self.intrinsic(Effect::Discontinue);
                self.block(body, mask);
            },
            Expr::Spawn { body, .. } | Expr::Defer { body, .. } => self.block(body, mask),
            Expr::Block(b) => self.block(b, mask),
        }
    }

    /// Interpolation: expression holes walk; `$x` holes are value uses.
    fn string(&mut self, parts: &[StrPart], mask: EffectRow) {
        for part in parts {
            match part {
                StrPart::Text { .. } => {},
                StrPart::Expr { expr, .. } => self.expr(expr, mask),
                StrPart::Ident { name, .. } => self.use_res(name.span, mask),
            }
        }
    }

    /// A path use: each segment's resolution names a callee (`Trait::f`
    /// resolves both the trait and the member; the trait itself carries no
    /// row).
    fn path(&mut self, path: &Path, mask: EffectRow) {
        for seg in &path.segments {
            self.use_res(seg.span, mask);
        }
    }

    /// A tuple: each element contributes.
    fn elements(&mut self, items: &[Expr], mask: EffectRow) {
        for item in items {
            self.expr(item, mask);
        }
    }

    /// A struct literal: construction is pure data; the path use and the
    /// field values contribute.
    fn struct_lit(&mut self, path: &Path, fields: &[(Ident, Option<Expr>)], mask: EffectRow) {
        self.path(path, mask);
        for (_, value) in fields {
            if let Some(v) = value {
                self.expr(v, mask);
            }
        }
    }

    /// A call: the callee expression, the arguments, and the trailing
    /// block argument contribute; the callee's resolution is a use inside
    /// the callee expression itself.
    fn call(&mut self, callee: &Expr, args: &[CallArg], trailing: Option<&Expr>, mask: EffectRow) {
        self.expr(callee, mask);
        for arg in args {
            self.expr(&arg.value, mask);
        }
        if let Some(t) = trailing {
            self.expr(t, mask);
        }
    }

    /// A dot use: the `.err`/`.ok` handler forms consume the receiver's
    /// failure members [E4]; every other dot names a callee (a free
    /// function the receiver feeds, or the method candidates).
    fn field(&mut self, recv: &Expr, name: &Ident, mask: EffectRow) {
        let handled = matches!(self.res.get(name.span), Some(Res::Handler));
        self.expr(
            recv,
            if handled { mask.union(EffectRow::of(Effect::Error)) } else { mask },
        );
        self.use_res(name.span, mask);
    }

    fn assign(&mut self, op: AssignOp, left: &Expr, right: &Expr, mask: EffectRow) {
        if traps_assign(op) {
            self.intrinsic(Effect::Trap);
        }
        self.expr(left, mask);
        self.expr(right, mask);
    }

    fn unary(&mut self, op: UnaryOp, expr: &Expr, mask: EffectRow) {
        if traps_unary(op) {
            self.intrinsic(Effect::Trap);
        }
        self.expr(expr, mask);
    }

    fn binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr, mask: EffectRow) {
        if traps_binary(op) {
            self.intrinsic(Effect::Trap);
        }
        self.expr(left, mask);
        self.expr(right, mask);
    }

    /// Branches union: every arm that may run contributes.
    fn if_expr(&mut self, cond: &Expr, then: &BlockExpr, else_: Option<&Expr>, mask: EffectRow) {
        self.expr(cond, mask);
        self.block(then, mask);
        if let Some(e) = else_ {
            self.expr(e, mask);
        }
    }

    /// The scrutinee keeps its row unmasked: arms may hand failures on.
    fn match_expr(&mut self, scrutinee: &Expr, arms: &[MatchArm], mask: EffectRow) {
        self.expr(scrutinee, mask);
        for arm in arms {
            if let Some(guard) = &arm.guard {
                self.expr(guard, mask);
            }
            self.expr(&arm.body, mask);
        }
    }

    /// A block's statements in order; binding initializers contribute.
    fn block(&mut self, block: &BlockExpr, mask: EffectRow) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Local(local) => self.expr(&local.init, mask),
                StmtKind::Expr(e) => self.expr(e, mask),
            }
        }
    }

    // ---- resolution -------------------------------------------------------

    /// One recorded name use: the resolved callee's row (or every
    /// registered candidate's) flows through a call edge. Locals
    /// contribute their lambda bodies where they bind; the handler
    /// surface form contributes here as nothing.
    fn use_res(&mut self, span: Span, mask: EffectRow) {
        match self.res.get(span) {
            Some(Res::Def(def)) => self.edge(Node::Def(*def), mask),
            Some(Res::TraitMethod(m)) => self.edge(Node::Method(*m), mask),
            Some(Res::Dot(DotRes::Free { func, .. })) => self.edge(Node::Def(*func), mask),
            Some(Res::Dot(DotRes::Methods { candidates })) => {
                for m in candidates {
                    self.edge(Node::Method(*m), mask);
                }
            },
            Some(Res::Local { .. } | Res::Handler) | None => {},
        }
    }
}
