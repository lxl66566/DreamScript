//! The `effects_of` pass: per-definition effect rows over the parsed and
//! resolved surface (B5, B7, E1, E2, E4), plus the per-expression table
//! beside them — every walked expression's own row, keyed by its span
//! ([`Effects::expr_row`], the grain the REPL's answer print and the
//! LSP's hover read), collected in the same walk the fixpoint graph is
//! built by and finalized against its solution.
//!
//! v1 has no effect syntax (E1) — rows are computed, never written. The
//! computation is a pure walk over the package's trees and the resolution
//! side table [`crate::resolve`]: it emits no diagnostics, and unresolved
//! names contribute nothing. World-touching ambient functions enter
//! through the row table [`DeclaredRows`] with their platform capability
//! attribution beside it ([`crate::WorldCaps`], the platform declaration's
//! provides list its licenser); the package's platform declaration is
//! checked against the computed rows by [`crate::check_platform`]. Rows
//! carry the record's five-effect vocabulary ([`Effect`]).
//!
//! # Composition
//!
//! A row is the union of the definition's declared row, its body's
//! intrinsic contributions, and one edge per resolved call or value use,
//! unioning the callee's row. `?` and `or return` contribute `error`
//! (E4); trapping integer defaults contribute `trap` (B7); a `nursery`
//! block contributes `cancel | discontinue` (E1, E2) — the internal
//! effects its boundary dispatches evidence for. The E4 consuming forms
//! mask their operand's `error`: a plain `or` extracts the wanted member
//! and `.err(h)`/`.ok(h)` handle the failure set, so their operand's call
//! edges carry `error` subtracted; `or return` additionally contributes
//! `error` because the early exit hands the failure out. Lambda bodies
//! contribute in place — combinators and spawn blocks run them inside the
//! enclosing definition's dynamic extent (D3, E2). Rows grow monotonically
//! over the call graph to a least fixpoint, so recursion and
//! intra-package cycles terminate by construction (G3).
//!
//! # Over-approximation, deliberate
//!
//! Rows never miss an effect but may carry more: trapping operators
//! contribute `trap` without solved operand types (floats never trap
//! (B7) — the checker's per-expression types are not exposed to this
//! pass); a method call unions every registered candidate and a trait
//! member's row unions every impl body (F3); a match scrutinee keeps its
//! operand's `error` since arms may hand it on; `deinit` blocks (C5) are
//! walked by no definition — their effects ride with the value's owning
//! function once the affine pass places deaths (M4). Supersets fail
//! closed: M7's platform subset check may over-reject, never silently
//! accept.
//!
//! # Example
//!
//! ```
//! use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps, effects_of};
//! use ds_diag::Files;
//! use ds_parse::parse;
//!
//! let mut files = Files::new();
//! let file = files.add_source(
//!     "net.ds",
//!     concat!(
//!         "extern fn fetch (u: int) -> int | io_err\n",
//!         "fn body(u: int) -> int { fetch(u)? }\n",
//!     ),
//! );
//! let ast = parse(&files, file).expect("parses");
//! let declared = DeclaredRows::new().with(
//!     "fetch",
//!     EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
//! );
//! let effects = effects_of(&[ast], &declared, &WorldCaps::new());
//!
//! // `body` unions the declared ambient row with its own `?` [E4].
//! let body = effects.of_name("body").expect("row-bearing definition");
//! assert!(body.contains(Effect::World));
//! assert_eq!(body.render(), "error | world");
//! ```

use std::{collections::HashMap, sync::Arc};

use ds_diag::{FileId, Span};
use ds_parse::{
    AssignOp, Ast, BinaryOp, BlockExpr, CallArg, Expr, FnDecl, Ident, ImplDecl, Item, ItemKind,
    LocalDecl, MatchArm, Path, Pattern, StmtKind, StrPart, TraitDecl, TraitMember, UnaryOp,
};
use rustc_hash::FxHashMap;

use crate::{
    platform::WorldCaps,
    resolve::{DefId, DefKind, DotRes, MethodId, MethodOwner, Package, Res, Resolutions},
    type_of::HoleShowDispatches,
};

/// One named effect. The vocabulary is the record's set: `error` (B5, E4)
/// — union-member failure a definition can hand to its caller; `trap`
/// (B7, E4) — the unhandleable bug boundary of panics and trapping
/// integer defaults; `world` (E1) — the world-touching capability surface,
/// entering through [`DeclaredRows`] until M7's platform declaration
/// supplies it; `cancel` and `discontinue` (E1) — the two internal effects
/// a `nursery` boundary dispatches evidence for.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Effect {
    /// Union-member failure handed to a caller (B5, E4).
    Error,
    /// The unhandleable bug boundary (B7, E4).
    Trap,
    /// The world-touching capability surface (E1).
    World,
    /// The nursery cancellation internal effect (E1, E2).
    Cancel,
    /// The nursery discontinue internal effect (E1).
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

    /// The row's bit set — the wire form the checked-ambient artifact
    /// carries.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The row of a raw bit set, the decode side of
    /// [`EffectRow::bits`].
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
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
/// the stdlib, platform capabilities (E1, H5). v1 has no effect syntax, so
/// this table is the only row input the surface cannot express. Applied by
/// name; a declared row is a lower bound on the computed row. Absent names
/// carry [`EffectRow::PURE`]. World rows pair with their capability
/// attribution in [`crate::WorldCaps`]; the platform declaration's
/// provides list (checked by [`crate::check_platform`]) is the licenser.
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
    /// The defining name occurrence (carries the file): the platform
    /// check's diagnostic span.
    pub span: Span,
    /// The computed row.
    pub row: EffectRow,
    /// The world capabilities the definition's world usage needs (the
    /// platform subset check's finer grain, H5): a bitmask over the
    /// capability vocabulary, zero when the row is world-free.
    pub caps: u8,
}

/// The served prefix's parked name rows: the [`Effects`] name map's
/// base-tier entries alone (the assembled rows and their ids never
/// park), built once per process by the first served compile's
/// assemble and installed by every later one. The entries name the
/// serve's own definition ids — identities every serve mints the same
/// way, never minted atoms — so the park holds G3.5's rule the way
/// every served table's park does: process memory, never an artifact
/// row, rebuilt by whatever process runs the compile.
pub type ServedEffectNames = Arc<HashMap<String, DefId>>;

/// The computed rows of one package: every value-producing definition
/// (named and extern functions, bindings) plus the per-expression table
/// (every walked expression's own row, keyed by its span). Type
/// declarations carry no row and report `None` from [`Effects::of`].
#[derive(Clone, Default)]
pub struct Effects {
    /// Row-bearing definitions, in declaration order.
    defs: Vec<DefEffects>,
    /// DefId index into `defs`; `None` for type-level definitions.
    index: Vec<Option<u32>>,
    /// First-wins name lookup over the growth tier alone: a served
    /// prefix's entries park per process behind [`ServedEffectNames`]
    /// and install as `base_names`, so a served compile's own map
    /// holds only its files' names.
    names: HashMap<String, DefId>,
    /// The served prefix's parked name rows, installed by every serve
    /// past the first ([`of_name`]'s base tier below the growth map).
    base_names: Option<ServedEffectNames>,
    /// Every walked expression's own row (its subtree's intrinsics plus
    /// its uses' masked callee rows), keyed by the expression's span.
    expr_rows: FxHashMap<Span, EffectRow>,
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
        let def = self
            .names
            .get(name)
            .or_else(|| self.base_names.as_deref().and_then(|base| base.get(name)))?;
        self.of(*def)
    }

    /// The row of one expression, keyed by its exact span: `None` when
    /// the walk recorded no entry there.
    #[must_use]
    pub fn expr_row(&self, span: Span) -> Option<EffectRow> {
        self.expr_rows.get(&span).copied()
    }

    /// The tightest recorded expression row covering `span` — an
    /// expression's own row when it is recorded, else the tightest
    /// enclosing expression's. `None` when no recorded span covers it
    /// (the caller falls back to the definition grain).
    #[must_use]
    pub fn covering_row(&self, span: Span) -> Option<EffectRow> {
        self.expr_rows
            .iter()
            .filter(|(s, _)| s.file == span.file && s.start <= span.start && span.end <= s.end)
            .min_by_key(|(s, _)| s.end - s.start)
            .map(|(_, row)| *row)
    }

    /// The tightest recorded expression row covering the position — a
    /// non-empty span covers strictly inside it, an empty span only the
    /// position it is (the hover predicate, mirroring how the type table
    /// is scanned). `None` when no recorded span covers it.
    #[must_use]
    pub fn row_at(&self, file: FileId, offset: u32) -> Option<EffectRow> {
        self.expr_rows
            .iter()
            .filter(|(s, _)| {
                s.file == file
                    && ((s.start < s.end && s.start <= offset && offset < s.end)
                        || (s.start == s.end && s.start == offset))
            })
            .min_by_key(|(s, _)| s.end - s.start)
            .map(|(_, row)| *row)
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
    /// Own world-capability contributions: the bitmask of the platform
    /// capabilities the node's declared world row names (the platform
    /// subset check's finer grain, solved beside the row).
    base_caps: u8,
    /// Call edges.
    edges: Vec<Edge>,
}

/// One expression's pending row: the intrinsics and call edges collected
/// inside its subtree so far. Callee rows do not exist until the fixpoint
/// runs, so the walk accumulates and the table is finalized against the
/// solved nodes afterwards — one walk, no re-visit.
#[derive(Default, Debug)]
struct ExprAcc {
    /// The subtree's intrinsic contributions.
    base: EffectRow,
    /// The subtree's call edges, with each use's own mask.
    edges: Vec<Edge>,
}

impl ExprAcc {
    /// The pending row as one list of fixpoint inputs, ready to merge
    /// into an enclosing accumulator or a node's [`Info`].
    fn parts(&self) -> (EffectRow, &[Edge]) {
        (self.base, &self.edges)
    }
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
/// every tree passed here, in order (G3). `T: Borrow<Ast>` lets callers
/// pass owned trees or shared arcs (Borrow is the reflexive bound). No
/// check ran, so no implicit hole dispatch record exists: a hole over a
/// `show`-carrying operand contributes only its operand's own row here;
/// callers that checked pass their records through
/// [`effects_of_resolved`].
#[must_use]
pub fn effects_of<T>(asts: &[T], declared: &DeclaredRows, world: &WorldCaps) -> Effects
where
    T: std::borrow::Borrow<Ast>,
{
    let (package, resolutions, _) = crate::resolve::resolve_package(asts);
    effects_of_resolved(
        asts,
        &package,
        &resolutions,
        &FxHashMap::default(),
        declared,
        world,
    )
}

/// Computes the package's effect rows over an already collected and
/// resolved package: `asts` and `resolutions` align positionally with the
/// package's collected files, like
/// [`analyze_resolved`](crate::type_of::analyze_resolved). Same contract as
/// [`effects_of`], which collects and resolves the trees first; callers
/// holding a resolve result pass it in instead of re-running the pass per
/// file. `holes` carries the checker's implicit hole dispatch records
/// ([`Checked::hole_show_dispatches`](crate::type_of::Checked::hole_show_dispatches)):
/// a hole that staged a `show` dispatch unions the dispatched member's
/// row, the edge a written `.show()` call contributes — the walk is
/// resolution-driven and no surface name sits behind the implicit call,
/// so the record is the only source of the edge. Callers that have not
/// type-checked pass an empty map, accepting the miss.
#[must_use]
pub fn effects_of_resolved<T>(
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
    holes: &HoleShowDispatches,
    declared: &DeclaredRows,
    world: &WorldCaps,
) -> Effects
where
    T: std::borrow::Borrow<Ast>,
{
    let mut graph = HashMap::new();
    let exprs = graph_of(
        asts,
        package,
        resolutions,
        holes,
        declared,
        world,
        &mut graph,
    );
    let solved = solve(&graph);
    let expr_rows = finalize_expr_rows(exprs, &solved);
    assemble(package, &solved, expr_rows, None)
}

/// One registered method's solved row, the checked-ambient artifact's
/// method-half serve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ServedMethodRow {
    /// The registered method.
    pub method: MethodId,
    /// The solved row.
    pub row: EffectRow,
    /// The solved capability bits.
    pub caps: u8,
}

/// The checked-ambient artifact's extraction half: the ambient's own
/// registered methods' solved rows, in registration order, over its own
/// closed package — the rows a served run seeds so user edges into
/// ambient method nodes read without walking the ambient's trees. Runs
/// the same walk and fixpoint [`effects_of_resolved`]
/// runs.
#[must_use]
pub fn solved_method_rows<T>(
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
    declared: &DeclaredRows,
    world: &WorldCaps,
) -> Vec<ServedMethodRow>
where
    T: std::borrow::Borrow<Ast>,
{
    let mut graph = HashMap::new();
    // The method-half serve reads the solved method nodes only; the
    // walk's per-span table is dropped unused.
    let _ = graph_of(
        asts,
        package,
        resolutions,
        &FxHashMap::default(),
        declared,
        world,
        &mut graph,
    );
    let solved = solve(&graph);
    package
        .methods()
        .enumerate()
        .map(|(index, _)| {
            let method = MethodId::from_raw(u32::try_from(index).expect("method count fits u32"));
            let (row, caps) = solved
                .get(&Node::Method(method))
                .copied()
                .unwrap_or((EffectRow::PURE, 0));
            ServedMethodRow { method, row, caps }
        })
        .collect()
}

/// [`effects_of_resolved`] behind a
/// checked-ambient artifact: only the user trees walk; the ambient's
/// solved rows — its definitions' (from the artifact's
/// [`Effects::defs`]) and its registered methods' — seed the graph as
/// fixed inputs. The ambient's own subgraph cannot reference a user
/// definition (its bodies name only ambient names), so its fixpoint
/// solution is independent of the user half and seeding it collapses
/// that subgraph without walking it; the solve and the assembly then run
/// exactly as in the fresh pass over the merged package's full
/// definition range, so the answer equals the fresh one whenever the
/// served rows are the ambient's true fixpoint rows (the artifact's
/// drift gate pins that). The prefix's name rows park once per process
/// in `park` (the first compile filling, every later one installing —
/// see [`ServedEffectNames`]), the cell owned by the serve whose
/// prefix it holds, so two serves never share one park.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn effects_of_resolved_served<T>(
    user_asts: &[T],
    package: &Package,
    user_resolutions: &[Resolutions],
    holes: &HoleShowDispatches,
    served_defs: &[DefEffects],
    served_methods: &[ServedMethodRow],
    declared: &DeclaredRows,
    world: &WorldCaps,
    park: &crate::Park<ServedEffectNames>,
) -> Effects
where
    T: std::borrow::Borrow<Ast>,
{
    let mut seeds = HashMap::new();
    for row in served_defs {
        seeds.insert(Node::Def(row.def), Info {
            base: row.row,
            base_caps: row.caps,
            edges: Vec::new(),
        });
    }
    for row in served_methods {
        seeds.insert(Node::Method(row.method), Info {
            base: row.row,
            base_caps: row.caps,
            edges: Vec::new(),
        });
    }
    let exprs = graph_of(
        user_asts,
        package,
        user_resolutions,
        holes,
        declared,
        world,
        &mut seeds,
    );
    let solved = solve(&seeds);
    // The per-span table covers the walked user trees (the seeded ambient
    // nodes answer its edges like any solved callee; the ambient's own
    // spans carry no entries, and no REPL or hover query reaches them).
    let expr_rows = finalize_expr_rows(exprs, &solved);
    assemble(package, &solved, expr_rows, Some(park))
}

/// Builds the row graph over the walked trees, seeding into `graph` (the
/// served run's fixed inputs) instead of an empty map when the caller
/// hands one: nodes up front for every value definition of the merged
/// package — `entry` keeps a served seed — then one Walker per tree,
/// which also collects the walk's per-span pending rows (the expression
/// table's pre-fixpoint form, every walked tree's spans).
fn graph_of<T>(
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
    holes: &HoleShowDispatches,
    declared: &DeclaredRows,
    world: &WorldCaps,
    graph: &mut HashMap<Node, Info>,
) -> FxHashMap<Span, ExprAcc>
where
    T: std::borrow::Borrow<Ast>,
{
    let count = u32::try_from(package.len()).expect("definition count fits u32");
    // nodes up front, so edges are order-independent across files [G3]
    for raw in 0..count {
        let def = DefId::from_raw(raw);
        if matches!(
            package.def(def).kind,
            DefKind::Fn | DefKind::Const | DefKind::Var
        ) {
            graph.entry(Node::Def(def)).or_default();
        }
    }
    // An import-bound function's own row rides its link surface (G4): its
    // body lives in the linked module, never in this package's trees, so
    // the surface's carried row and capabilities are its base — the same
    // seeding shape the checked-ambient serve gives the ambient's rows,
    // and a caller's edge into the node unions exactly what the linked
    // module computed.
    for (def, entry) in package.foreign() {
        graph.insert(Node::Def(*def), Info {
            base: entry.row,
            base_caps: entry.caps,
            edges: Vec::new(),
        });
    }
    let mut exprs = FxHashMap::default();
    for (ast, res) in asts.iter().zip(resolutions) {
        let ast = ast.borrow();
        let mut walker = Walker {
            package,
            declared,
            world,
            res,
            holes,
            graph: &mut *graph,
            exprs: &mut exprs,
            pending: Vec::new(),
            node: Node::Anon(Span::new(FileId::new(0), 0, 0)),
        };
        for block in &ast.blocks {
            for item in &block.items {
                walker.item(item);
            }
        }
    }
    exprs
}

/// Finalizes the walk's per-span accumulators against the solved nodes:
/// each pending row unions its edges' masked callee rows (the seeded
/// ambient nodes answering like any solved callee on the served path).
/// Same inputs as the node fixpoint, read once — no second walk.
fn finalize_expr_rows(
    exprs: FxHashMap<Span, ExprAcc>,
    solved: &HashMap<Node, (EffectRow, u8)>,
) -> FxHashMap<Span, EffectRow> {
    exprs
        .into_iter()
        .map(|(span, acc)| {
            let mut row = acc.base;
            for edge in &acc.edges {
                let (callee_row, _) = solved
                    .get(&edge.callee)
                    .copied()
                    .unwrap_or((EffectRow::PURE, 0));
                row |= callee_row.minus(edge.mask);
            }
            (span, row)
        })
        .collect()
}

/// Assembles the solved rows into the package's [`Effects`]: one entry
/// per value definition in declaration order, the DefId index, the
/// first-wins name table, and the finalized per-span expression rows.
/// A caller handing a park splits the name table at the served
/// prefix's def boundary: the prefix's entries build the park's rows
/// (filling it when no earlier compile did), and every serve past the
/// first installs the parked rows instead of re-cloning the prefix's
/// names per compile.
fn assemble(
    package: &Package,
    solved: &HashMap<Node, (EffectRow, u8)>,
    expr_rows: FxHashMap<Span, EffectRow>,
    park: Option<&crate::Park<ServedEffectNames>>,
) -> Effects {
    let count = u32::try_from(package.len()).expect("definition count fits u32");
    let boundary = package.base_def_count();
    let parked = park.and_then(|park| park.get());
    // the tiered build needs a served prefix to split at and a park to
    // hold the prefix's rows; a base-tier package without a park (the
    // fresh pass, a caller that hands none) builds the one map as ever
    let tiered = parked.is_some() || (boundary > 0 && park.is_some());
    let installing = parked.is_some();
    let mut defs = Vec::new();
    let mut index = vec![None; package.len()];
    let mut names = HashMap::new();
    let mut prefix_names = HashMap::new();
    for raw in 0..count {
        let def_id = DefId::from_raw(raw);
        let def = package.def(def_id);
        if matches!(def.kind, DefKind::Fn | DefKind::Const | DefKind::Var) {
            index[raw as usize] = Some(u32::try_from(defs.len()).expect("def count fits u32"));
            let (row, caps) = solved
                .get(&Node::Def(def_id))
                .copied()
                .unwrap_or((EffectRow::PURE, 0));
            defs.push(DefEffects {
                def: def_id,
                name: def.name.clone(),
                kind: def.kind,
                span: def.span,
                row,
                caps,
            });
        }
        let in_prefix = tiered && (raw as usize) < boundary;
        if in_prefix {
            prefix_names.entry(def.name.clone()).or_insert(def_id);
            // the filling compile keeps the one first-wins map too, so
            // its own lookups answer before the park exists
            if !installing {
                names.entry(def.name.clone()).or_insert(def_id);
            }
        } else {
            names.entry(def.name.clone()).or_insert(def_id);
        }
    }
    let base_names = if installing {
        parked.map(Arc::clone)
    } else {
        if let Some(park) = park {
            // the rows drop when a concurrent fill won the race, the
            // loser's set discarded exactly like the fill's own
            let _ = park.fill(Arc::new(prefix_names));
        }
        None
    };
    Effects {
        defs,
        index,
        names,
        base_names,
        expr_rows,
    }
}

/// Rows and world-capability sets to the least fixpoint over the call
/// graph: each node's row unions its base with its edges' masked callee
/// rows, and its capability set unions its base with its edges' callee
/// sets (unmasked — the error masks never touch world), until nothing
/// grows. Both lattices only grow and are finite, so this terminates.
fn solve(graph: &HashMap<Node, Info>) -> HashMap<Node, (EffectRow, u8)> {
    let mut solved: HashMap<Node, (EffectRow, u8)> = graph
        .iter()
        .map(|(node, info)| (*node, (info.base, info.base_caps)))
        .collect();
    loop {
        let mut changed = false;
        for (node, info) in graph {
            let mut row = info.base;
            let mut caps = info.base_caps;
            for edge in &info.edges {
                let (callee_row, callee_caps) = solved
                    .get(&edge.callee)
                    .copied()
                    .unwrap_or((EffectRow::PURE, 0));
                row = row.union(callee_row.minus(edge.mask));
                caps |= callee_caps;
            }
            let entry = solved.get_mut(node).expect("every node is seeded");
            if *entry != (row, caps) {
                *entry = (row, caps);
                changed = true;
            }
        }
        if !changed {
            return solved;
        }
    }
}

/// One file's walk: fills the graph under the node each item enters,
/// collecting one pending row per enclosing expression on the side.
struct Walker<'a, 'g> {
    package: &'a Package,
    declared: &'a DeclaredRows,
    world: &'a WorldCaps,
    res: &'a Resolutions,
    /// The checker's implicit hole dispatch records (F6): one entry per
    /// interpolation hole that staged a `show` dispatch, naming the
    /// dispatched member whose row the hole unions — no resolved name
    /// sits behind the implicit call, so this record is the edge's only
    /// source.
    holes: &'a HoleShowDispatches,
    graph: &'g mut HashMap<Node, Info>,
    /// Finished per-expression accumulators, keyed by span: the table's
    /// pre-fixpoint form, shared across the package's files (spans carry
    /// their file, so entries never collide).
    exprs: &'g mut FxHashMap<Span, ExprAcc>,
    /// The enclosing expressions' pending rows, innermost last: while
    /// non-empty, contributions land on the top and merge upward as each
    /// expression exits, so the node's own inputs are exactly what its
    /// statement roots collected.
    pending: Vec<(Span, ExprAcc)>,
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

    /// Adds `row` to the current contributions: the innermost enclosing
    /// expression's pending row, or the node's own when no expression is
    /// open.
    fn contribute(&mut self, row: EffectRow) {
        if let Some((_, acc)) = self.pending.last_mut() {
            acc.base |= row;
            return;
        }
        let info = self
            .graph
            .get_mut(&self.node)
            .expect("walks enter their node first");
        info.base = info.base.union(row);
    }

    /// The world-capability bits of `name`'s declared row: zero when the
    /// declared row carries no world effect or the capability table does
    /// not name the function (an unattributed world touch, which the
    /// platform check licenses on any world-bearing platform).
    fn declared_caps(&self, name: &str) -> u8 {
        if self.declared.row(name).contains(Effect::World) { self.world.bit(name) } else { 0 }
    }

    /// Adds one effect to the current node's own contributions.
    fn intrinsic(&mut self, effect: Effect) {
        self.contribute(EffectRow::of(effect));
    }

    /// Adds world-capability bits to the current node's own
    /// contributions.
    fn caps(&mut self, caps: u8) {
        let info = self
            .graph
            .get_mut(&self.node)
            .expect("walks enter their node first");
        info.base_caps |= caps;
    }

    /// Records one call edge. The callee's row is read at the fixpoint, so
    /// node creation order never matters; a callee outside the graph (a
    /// type used as a value) carries the pure row.
    fn edge(&mut self, callee: Node, mask: EffectRow) {
        let edge = Edge { callee, mask };
        if let Some((_, acc)) = self.pending.last_mut() {
            acc.edges.push(edge);
            return;
        }
        let info = self
            .graph
            .get_mut(&self.node)
            .expect("walks enter their node first");
        info.edges.push(edge);
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
            | ItemKind::Import(_)
            | ItemKind::Platform(_) => {},
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
        self.caps(self.declared_caps(&f.name.name));
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
        if let (Pattern::Tuple { pats, .. }, Expr::Tuple { items, .. }) = (pat, init)
            && pats.len() == items.len()
        {
            for (pat, item) in pats.iter().zip(items.iter()) {
                self.bind_pattern(pat, item);
            }
            return;
        }
        let mut defs = Vec::new();
        self.pattern_defs(pat, &mut defs);
        if defs.is_empty() {
            return;
        }
        let scratch = Node::Anon(init.span());
        self.enter(scratch);
        self.expr(init, EffectRow::PURE);
        let (base, base_caps, edges) = {
            let info = &self.graph[&scratch];
            (info.base, info.base_caps, info.edges.clone())
        };
        for def in defs {
            let name = &self.package.def(def).name;
            let declared = self.declared.row(name);
            let declared_caps = self.declared_caps(name);
            let info = self.graph.entry(Node::Def(def)).or_default();
            info.base = info.base.union(declared).union(base);
            info.base_caps |= declared_caps | base_caps;
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
            self.caps(self.declared_caps(&f.name.name));
            if let Some(body) = &f.body {
                self.block(body, EffectRow::PURE);
            }
        }
    }

    /// One impl block: inherent functions register as methods; a
    /// trait-impl function is an anonymous node flowing into the trait
    /// member it implements, so a call through the trait unions every
    /// impl body (F3).
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
                self.caps(self.declared_caps(&f.name.name));
                self.block(body, EffectRow::PURE);
                if let Some(m) = self.trait_member(trait_def, f) {
                    self.enter(Node::Method(m));
                    self.edge(anon, EffectRow::PURE);
                }
            } else {
                let Some(m) = self
                    .package
                    .method_candidates(&f.name.name)
                    .find(|&m| self.package.method(m).span == f.name.span)
                else {
                    continue;
                };
                self.enter(Node::Method(m));
                self.contribute(declared);
                self.caps(self.declared_caps(&f.name.name));
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
            .find(|&m| self.package.method(m).owner == MethodOwner::Trait(trait_def))
    }

    /// The package definitions a top-level binding pattern declares,
    /// through the shared binder walker.
    fn pattern_defs(&self, pat: &Pattern, out: &mut Vec<DefId>) {
        for (_, name) in crate::pattern::pattern_binders(pat) {
            if let Some(def) = self.package.lookup_value(name)
                && !out.contains(&def)
            {
                out.push(def);
            }
        }
    }

    // ---- expressions ------------------------------------------------------

    /// Walks one expression's contributions into the current node, under
    /// a pending row of its own: every expression leaves its subtree's
    /// intrinsics and edges in the per-span table, so consumers can read
    /// any expression's row (the definition grain stays the fixpoint's).
    ///
    /// `mask` is the set of effects consumed before this expression's
    /// value flows on (the E4 consuming forms mask `error`): call edges
    /// subtract it from their callee's row, while intrinsic `error`
    /// contributions — the `?` and `or return` early exits — are control
    /// flow and never masked.
    fn expr(&mut self, e: &Expr, mask: EffectRow) {
        self.pending.push((e.span(), ExprAcc::default()));
        self.expr_intrinsic(e, mask);
        self.exit_expr();
    }

    /// The expression walk's match half, under the pushed pending row.
    fn expr_intrinsic(&mut self, e: &Expr, mask: EffectRow) {
        match e {
            // A fn type in the constructor brackets is a type spelling; it
            // carries no value effects.
            Expr::Literal { .. }
            | Expr::RawStr { .. }
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::FnType { .. } => {},
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

    /// Pops the innermost pending row: the expression's own inputs are
    /// complete (children merged in as they exited), so the span's entry
    /// is finished, and the inputs flow on — into the enclosing
    /// expression's pending row, or into the current node when this
    /// expression is a statement's root. The node's fixpoint inputs are
    /// exactly what its statement roots collected, so the solved
    /// definition rows are unchanged by the side table.
    fn exit_expr(&mut self) {
        let (span, acc) = self.pending.pop().expect("every expression push exits");
        let (base, edges) = acc.parts();
        if let Some((_, parent)) = self.pending.last_mut() {
            parent.base |= base;
            parent.edges.extend_from_slice(edges);
        } else {
            let info = self
                .graph
                .get_mut(&self.node)
                .expect("walks enter their node first");
            info.base |= base;
            info.edges.extend_from_slice(edges);
        }
        self.exprs.insert(span, acc);
    }

    /// Interpolation: expression holes walk; `$x` holes are value uses.
    /// A hole that staged an implicit `show` dispatch unions the
    /// dispatched member's row — the edge the written call's resolved
    /// name contributes.
    fn string(&mut self, parts: &[StrPart], mask: EffectRow) {
        for part in parts {
            match part {
                StrPart::Text { .. } => {},
                StrPart::Expr { span, expr } => {
                    self.expr(expr, mask);
                    self.hole_edge(*span, mask);
                },
                StrPart::Ident { span, name } => {
                    self.use_res(name.span, mask);
                    self.hole_edge(*span, mask);
                },
            }
        }
    }

    /// One hole's implicit dispatch edge: the member the checker's
    /// record names (a solved instance, an inherent owner, or a trait
    /// member whose row unions every impl body — the written bound
    /// call's own over-approximation); no record, no edge.
    fn hole_edge(&mut self, span: Span, mask: EffectRow) {
        if let Some(&method) = self.holes.get(&span) {
            self.edge(Node::Method(method), mask);
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
    /// failure members (E4); every other dot names a callee (a free
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
            Some(Res::Dot(DotRes::Free { func, .. } | DotRes::Module { func })) => {
                self.edge(Node::Def(*func), mask);
            },
            Some(Res::Dot(DotRes::Methods { candidates } | DotRes::Static { candidates })) => {
                for m in candidates {
                    self.edge(Node::Method(*m), mask);
                }
            },
            Some(Res::Local { .. } | Res::Handler | Res::Module(_)) | None => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use ds_diag::{Files, Span};
    use ds_parse::parse;
    use rustc_hash::FxHashMap;

    use super::{
        DeclaredRows, DefEffects, DefId, DefKind, Effect, EffectRow, ServedEffectNames, WorldCaps,
        effects_of, effects_of_resolved_served, solved_method_rows,
    };
    use crate::{
        Park,
        resolve::{Def, Package, PackageBase, Vis, resolve_package},
    };

    /// The served prefix's name rows park once per process: two served
    /// compiles over one base share the prefix's name map — the first
    /// fills the park with its own map still answering the whole
    /// first-wins table, the second installs the parked handle
    /// (identity-pinned, its own map carrying the growth alone) and
    /// answers every name the one-map fresh pass does, the base's
    /// extern and the growth's own fn alike.
    #[test]
    fn the_served_prefix_name_rows_park_per_process() {
        let prefix_source = "extern fn speak (t: string) -> ()\n";
        let user_source = concat!(
            "fn twice() -> int {\n",
            "  speak(\"hi\")\n",
            "  1 + 2\n",
            "}\n",
        );
        let mut files = Files::new();
        let prefix_file = files.add_source("prefix.ds", prefix_source);
        let user_file = files.add_source("user.ds", user_source);
        let prefix_ast = parse(&files, prefix_file).expect("the prefix parses");

        // the served rows: the fresh prefix pass's own rows, at the ids
        // the base tier assigns (declaration order)
        let declared = DeclaredRows::new().with("speak", EffectRow::of(Effect::World));
        let caps = WorldCaps::new();
        let (prefix_package, prefix_resolutions, _) =
            resolve_package(std::slice::from_ref(&prefix_ast));
        let served_defs: Vec<DefEffects> =
            effects_of(std::slice::from_ref(&prefix_ast), &declared, &caps)
                .defs()
                .to_vec();
        let served_methods = solved_method_rows(
            std::slice::from_ref(&prefix_ast),
            &prefix_package,
            &prefix_resolutions,
            &declared,
            &caps,
        );

        // the base tier: the prefix's one extern under the same id the
        // fresh pass assigns
        let base = PackageBase::from_parts(
            vec![prefix_file],
            vec![Def {
                name: "speak".to_owned(),
                kind: DefKind::Fn,
                vis: Vis::Pub,
                span: Span::new(prefix_file, 10, 15),
            }],
            Vec::new(),
            vec![("speak".to_owned(), DefId::from_raw(0))],
            Vec::new(),
            Vec::new(),
        );

        let park = Park::<ServedEffectNames>::new();
        let run_served = |park: &Park<ServedEffectNames>| {
            let user_ast = parse(&files, user_file).expect("the user parses");
            let mut package = Package::with_base(std::sync::Arc::new(base.clone()));
            let collection = package.add_file(&user_ast);
            assert!(
                collection.is_empty(),
                "the user file collects clean: {collection:?}"
            );
            let (resolutions, _) = package.resolve(&user_ast);
            effects_of_resolved_served(
                &[user_ast],
                &package,
                &[resolutions],
                &FxHashMap::default(),
                &served_defs,
                &served_methods,
                &declared,
                &caps,
                park,
            )
        };

        // the fresh one-map pass over prefix and user alike
        let fresh_user_ast = parse(&files, user_file).expect("the user parses");
        let fresh = effects_of(&[prefix_ast, fresh_user_ast], &declared, &caps);

        // the first served compile fills the park, its own map answering
        // the whole table
        let first = run_served(&park);
        let parked = park.get().expect("the first serve fills the park");
        assert!(
            first.base_names.is_none(),
            "the filling compile reads its own one map"
        );

        // the second installs the parked handle and keeps only its own
        // files' names
        let second = run_served(&park);
        assert!(
            second
                .base_names
                .as_ref()
                .is_some_and(|base| std::sync::Arc::ptr_eq(base, parked)),
            "the second serve installs the parked handle"
        );
        assert!(
            !second.names.contains_key("speak"),
            "the installed compile's growth map carries only its own names"
        );

        // the tiered read answers the one-map pass on every name
        assert_eq!(first.of_name("speak"), second.of_name("speak"));
        assert_eq!(first.of_name("twice"), second.of_name("twice"));
        for (effects, label) in [(&first, "first"), (&second, "second")] {
            assert_eq!(
                effects.of_name("speak"),
                fresh.of_name("speak"),
                "the {label} serve answers the base extern's row"
            );
            assert_eq!(
                effects.of_name("twice"),
                fresh.of_name("twice"),
                "the {label} serve answers the growth fn's row"
            );
            assert_eq!(
                effects.of_name("nowhere"),
                None,
                "an unknown name stays unknown"
            );
        }
    }

    /// The per-span table reads each expression's own row: the call under
    /// the cursor, not the sibling statements; the covering and position
    /// lookups answer the tightest recorded span; the definition rows stay
    /// the fixpoint's.
    #[test]
    fn expression_rows_read_per_span() {
        let source = concat!(
            "extern fn speak (t: string) -> ()\n",
            "fn mixed() -> int {\n",
            "  speak(\"hi\")\n",
            "  1 + 2\n",
            "}\n",
        );
        let mut files = Files::new();
        let file = files.add_source("rows.ds", source);
        let ast = parse(&files, file).expect("parses");
        let declared = DeclaredRows::new().with("speak", EffectRow::of(Effect::World));
        let effects = effects_of(&[ast], &declared, &WorldCaps::new());

        // The definition unions both statements; the tail's own span
        // carries only its trap, the call's only its world edge.
        assert_eq!(
            effects.of_name("mixed").expect("row-bearing").render(),
            "trap | world"
        );
        let at = |needle: &str| {
            u32::try_from(source.find(needle).expect("the needle occurs"))
                .expect("the source fits u32 offsets")
        };
        let span_of = |text: &str| {
            let len = u32::try_from(text.len()).expect("the text fits u32 offsets");
            Span::new(file, at(text), at(text) + len)
        };
        let call = span_of("speak(\"hi\")");
        let literal = span_of("\"hi\"");
        let sum = span_of("1 + 2");
        assert_eq!(
            effects.expr_row(call).expect("the call is walked"),
            EffectRow::of(Effect::World)
        );
        assert_eq!(
            effects.expr_row(literal).expect("literals are walked"),
            EffectRow::PURE
        );
        assert_eq!(
            effects.expr_row(sum).expect("the tail is walked"),
            EffectRow::of(Effect::Trap)
        );

        // The tightest covering span of an inner position is the operator
        // gap's enclosing sum (trap), not the pure operands; between the
        // statements the body block covers (the fn's body is itself an
        // expression); a position on the extern's signature covers
        // nothing — the honest fallback boundary.
        let plus = at("1 + 2") + 2;
        assert_eq!(
            effects.row_at(file, plus),
            Some(EffectRow::of(Effect::Trap))
        );
        assert_eq!(effects.row_at(file, at("hi")), Some(EffectRow::PURE));
        assert_eq!(
            effects.row_at(file, call.end),
            Some(EffectRow::of(Effect::Trap) | EffectRow::of(Effect::World))
        );
        assert_eq!(effects.row_at(file, at("string")), None);
        assert_eq!(effects.covering_row(literal), Some(EffectRow::PURE));
    }
}
