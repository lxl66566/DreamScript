//! The trait instance environment and coherence checking (B6).
//!
//! Traits are implemented by *instances* declared over type heads. The only
//! coherence rule is pairwise disjointness of the heads of one trait: for
//! every pair `H1`, `H2` of same-trait heads, `H1 & H2 = Empty` must hold
//! (B6). It is checked incrementally at declaration (per package (G3)) by
//! [`InstanceEnv::declare`] and again over the final environment — once
//! the single package's declarations settle, and over every link merge —
//! by [`InstanceEnv::check_all`] (a checked-ambient served prefix keeps
//! its stored snapshots there; that method's doc carries the skip's
//! ground). Heads the set algebra reads
//! disjoint because a generic head is opaque get the same rule through
//! instantiation: the probe in `crate::instance_solve` grounds candidate
//! instantiations and asks whether both heads serve one, so a concrete and
//! a generic head (or two generic heads with intersecting bounds) are
//! rejected at the later declaration, or by the closure pass when the
//! bounds only a later impl intersects could not prove the pair at
//! either declaration. There is no orphan rule: any package
//! may declare an instance for any type, foreign included; no priority
//! machinery exists, so dispatch never depends on declaration order.
//!
//! # Head normalization
//!
//! A declaration's head is a [`TypeExpr`], possibly a union. Written unions
//! decompose recursively into per-branch instances before any check (B6):
//! `impl eq for int | string` becomes two instances. Every other expression
//! is one branch whose head is its solved atom set ([`Universe::solve`]), so
//! intersections and differences normalize away: `a & b` of distinct atoms
//! solves to the empty set and is rejected ([`Error::EmptyInstanceHead`])
//! unless spelled as the canonical empty set ([`TypeExpr::Empty`], the
//! conversion layer's form for a name that IS the empty type, a zero-case
//! enum), whose branch declares a vacuous instance — disjoint from every
//! head, never selected for an inhabited query — while `(a | b) - b`
//! normalizes to the singleton `a`. A negated-constraint
//! head (`impl draw for ~Copyable`) or a trait used as a type stays one
//! multi-atom head. Heads are solved snapshots of the universe state at
//! declaration (crate-root snapshot rule); the memoized query layer owns
//! re-solving trait-relative heads after memberships change, and
//! [`InstanceEnv::check_all`] re-solves every stored head and condition
//! against the current universe before its pairwise pass, so membership
//! growth between declaration batches cannot hide an overlap.
//!
//! # Conditional instances and the structural cycle check
//!
//! A conditional instance carries conditions — trait constraints over types
//! (`impl show for json_t if eq[json_t]`). Conditions never filter lookup
//! here ([`InstanceEnv::resolve`] is containment-only); satisfaction is the
//! checker's resolution step. A decomposed union head solves its conditions
//! per branch: each branch instance carries only the conditions whose
//! variables its own head mentions (a branch naming no bound variable is
//! unconditional), and a declaration's requirement set is the union over
//! its branches. A condition edge runs from an instance to every
//! instance that could satisfy the condition: instance `j` satisfies `(T, Q)`
//! when `j` implements `T` and `j`'s head contains `Q`. A conditional
//! instance whose condition edges reach back to itself — directly
//! (`show for json_t if show[json_t]`) or transitively through other
//! conditional instances — is rejected ([`Error::ConditionCycle`]); this is
//! the structural cycle check of design-decisions section 10 item 7.
//!
//! Termination: the condition-dependency graph has the declared instances as
//! its finite vertex set and the edge relation above ranges only over them,
//! so the graph is finite. Cycle detection is a depth-first search with a
//! per-vertex finished flag, visiting each vertex and edge a bounded number
//! of times — O(V + E) per probe over V instances and at most V^2 edges per
//! condition. Declaration-time probes start at the new instances only: any
//! new cycle must pass through a new instance, because every edge introduced
//! by a declaration is incident to one of its branches.
//!
//! Conditional instances contribute no trait members
//! ([`crate::atom::TraitDecl`]): their applicability requires condition
//! satisfaction, so a trait used as a type solves over unconditional members
//! only.
//!
//! # Example
//!
//! The corpus rejection of `conformance/generics_coherent_traits.ds`: after
//! `json` for `int` and `json` for `bool`, the union head decomposes and both
//! branches overlap; the error names both heads and the witness `int` (B6).
//!
//! ```
//! use ds_check::{Error, InstanceDecl, InstanceEnv, TypeExpr, Universe};
//!
//! # fn main() -> Result<(), Error> {
//! let mut u = Universe::new();
//! let json = u.declare_trait("json")?;
//! let int = u.declare_tag("int")?;
//! let boolean = u.declare_tag("bool")?;
//!
//! let mut env = InstanceEnv::new();
//! env.declare(&mut u, &InstanceDecl::new(json, TypeExpr::atom(int)))?;
//! env.declare(&mut u, &InstanceDecl::new(json, TypeExpr::atom(boolean)))?;
//!
//! let union = InstanceDecl::new(json, TypeExpr::atom(int).union(boolean.into()));
//! let Err(Error::InstanceOverlap {
//!     head_a,
//!     head_b,
//!     witness,
//!     ..
//! }) = env.declare(&mut u, &union)
//! else {
//!     panic!("union head must be rejected");
//! };
//! assert_eq!(u.render(head_a), "int");
//! assert_eq!(u.render(head_b), "int"); // per-branch head after decomposition
//! assert_eq!(u.render_witness(&witness), "int");
//! # Ok(())
//! # }
//! ```

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    atom::{AtomKind, TraitId, TypeId},
    error::Error,
    expr::TypeExpr,
    id_table::IdTable,
    universe::{Ty, Universe, narrow},
};

/// Identity of one per-branch instance inside one [`InstanceEnv`]. Ids are
/// allocation order; a merge renumbers the appended environment, so callers
/// re-query after merging.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct InstanceId(u32);

impl InstanceId {
    /// Builds an id from a raw table index (serialization, tests).
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index inside its environment; also its declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A trait constraint over a type: the condition half of a conditional
/// instance (`impl show for json_t if eq[json_t]` requires `eq` for
/// `json_t`). Parameterized over the type representation: [`TypeExpr`] as
/// declared, [`Ty`] solved.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Condition<T> {
    /// The trait that must hold.
    pub trait_id: TraitId,
    /// The type it must hold for.
    pub ty: T,
}

impl<T> Condition<T> {
    /// A constraint of `trait_id` over `ty`.
    #[must_use]
    pub fn new(trait_id: TraitId, ty: T) -> Self {
        Self { trait_id, ty }
    }
}

/// An inherent impl's declared generic bounds (F3): the impl's solved head
/// plus one condition per declared bound over the declaration's minted
/// variable atoms — the trait form's per-instance record mirrored for an
/// impl that declares no instance. Call sites verify and stage their
/// runtime evidence from this record; the lowering pass reads the bounds
/// for its evidence-demand walk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InherentBounds {
    /// The impl's target solved at declaration: the single-atom head
    /// pattern a receiver unifies against, binding the minted variables.
    pub head: Ty,
    /// One condition per declared trait bound; a bare `[t]` parameter
    /// contributes none.
    pub conditions: Vec<Condition<Ty>>,
}

/// The body of an instance declaration. Opaque placeholder: method tables
/// are M2 checker work; the environment carries it without interpreting it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct InstanceBody;

/// One stored instance row in full — the written and solved halves of a
/// per-branch instance — as a checked-ambient artifact both installs it
/// ([`InstanceEnv::serve`]) and reads it back ([`InstanceEnv::row`], the
/// generator's extraction view).
#[derive(Clone, Debug)]
pub struct ServedInstance {
    /// The trait implemented.
    pub trait_id: TraitId,
    /// The branch as written.
    pub head_expr: TypeExpr,
    /// The head solved at declaration.
    pub head: Ty,
    /// The declaration's written head, pre-decomposition.
    pub written: Ty,
    /// The conditions this branch kept, as written.
    pub condition_exprs: Vec<Condition<TypeExpr>>,
    /// The kept conditions solved at declaration.
    pub conditions: Vec<Condition<Ty>>,
}

/// A declared instance as written: the trait implemented, the head (a
/// [`TypeExpr`], possibly a union), optional conditions, and an opaque body.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InstanceDecl {
    /// The trait implemented.
    pub trait_id: TraitId,
    /// The head as written; unions decompose into per-branch instances at
    /// declaration (B6).
    pub head: TypeExpr,
    /// Conditions; empty for an unconditional instance.
    pub conditions: Vec<Condition<TypeExpr>>,
    /// Opaque body payload.
    pub body: InstanceBody,
}

impl InstanceDecl {
    /// An unconditional instance of `trait_id` over `head`.
    #[must_use]
    pub fn new(trait_id: TraitId, head: TypeExpr) -> Self {
        Self {
            trait_id,
            head,
            conditions: Vec::new(),
            body: InstanceBody,
        }
    }

    /// Attaches conditions, making the instance conditional.
    #[must_use]
    pub fn with_conditions(mut self, conditions: Vec<Condition<TypeExpr>>) -> Self {
        self.conditions = conditions;
        self
    }
}

/// One stored per-branch instance: the written branch (kept so
/// [`InstanceEnv::check_all`] can re-solve it) plus the head and conditions
/// solved at declaration.
struct Instance {
    trait_id: TraitId,
    /// The branch as written; memberships may grow after declaration, so the
    /// solved snapshot can go stale until link-time re-solving.
    head_expr: TypeExpr,
    head: Ty,
    /// The declaration's head as written, pre-decomposition: every branch
    /// of one declaration shares it, and the member body behind those
    /// branches was checked over it. A declaration-time snapshot like
    /// `head`.
    written: Ty,
    /// Conditions as written, re-solved alongside the head at link time.
    condition_exprs: Vec<Condition<TypeExpr>>,
    conditions: Vec<Condition<Ty>>,
}

/// The condition-edge view the cycle probe evaluates against: heads and
/// conditions keyed by instance index — stored declaration-time snapshots
/// when probed from [`InstanceEnv::declare`], re-solved forms when probed
/// from [`InstanceEnv::check_all`].
struct CondEdges<'a> {
    heads: Vec<Ty>,
    conditions: Vec<&'a [Condition<Ty>]>,
}

impl CondEdges<'_> {
    /// Whether `start` reaches itself over condition edges, and the cycle
    /// path (start ... start) in edge order when it does.
    fn cycle_from(
        &self,
        instances: &[Instance],
        start: InstanceId,
        universe: &Universe,
    ) -> Option<Vec<InstanceId>> {
        let mut path = vec![start];
        let mut done = vec![false; instances.len()];
        if self.dfs(instances, start, start, universe, &mut path, &mut done) {
            Some(path)
        } else {
            None
        }
    }

    /// Depth-first search over condition edges from `current`, looking for
    /// `start`. Finished vertices are skipped: they already proved they
    /// cannot reach `start`. Extends `path` with the cycle on success.
    fn dfs(
        &self,
        instances: &[Instance],
        current: InstanceId,
        start: InstanceId,
        universe: &Universe,
        path: &mut Vec<InstanceId>,
        done: &mut [bool],
    ) -> bool {
        for cond in self.conditions[current.index() as usize] {
            for (j, cand) in instances.iter().enumerate() {
                if cand.trait_id == cond.trait_id && universe.contains(self.heads[j], cond.ty) {
                    let id = InstanceId::from_raw(narrow(j));
                    if id == start {
                        path.push(id);
                        return true;
                    }
                    if !done[j] {
                        path.push(id);
                        if self.dfs(instances, id, start, universe, path, done) {
                            return true;
                        }
                        path.pop();
                    }
                }
            }
        }
        done[current.index() as usize] = true;
        false
    }
}

/// The trait instance environment: per-branch instances in declaration
/// order, with incremental (declaration) and final (link-time) coherence
/// checking (B6). Heads and conditions are solved snapshots against the
/// universe state at their declaration; the written forms are stored
/// alongside, and [`InstanceEnv::check_all`] re-solves them against the
/// final universe before its passes.
///
/// Per-package checking runs [`InstanceEnv::declare`] while building; the
/// package check then runs [`InstanceEnv::check_all`] once over its own
/// closed set, and link merges run it again over the merged one (G3).
#[derive(Default)]
pub struct InstanceEnv {
    instances: Vec<Instance>,
    /// The count of leading rows a checked-ambient artifact served
    /// pre-declared: zero in a fresh build. [`InstanceEnv::check_all`]
    /// treats the prefix as pre-checked (the artifact's clean generation
    /// proved it coherent) and probes only the pairs and cycles a fresh
    /// row takes part in.
    served: usize,
    /// Same-trait head pairs (raw indices, earlier first) whose
    /// instantiation probe concluded disjoint at declaration: a
    /// structural verdict no later declaration undoes, so
    /// [`InstanceEnv::check_all`] skips their probe. Pairs the probe left
    /// open and pairs no declaration ever put together (link merges) stay
    /// absent and probe there.
    probed_disjoint: FxHashSet<(u32, u32)>,
    /// Per-trait row index: every stored instance of one trait in id
    /// order, maintained at each registration seam ([`InstanceEnv::declare`]
    /// appends, [`InstanceEnv::serve`] appends, [`InstanceEnv::merge`]
    /// rebuilds). The solve-side scans walk only their own trait's rows
    /// through [`InstanceEnv::of_trait`].
    by_trait: IdTable<TraitId, Vec<InstanceId>>,
}

impl InstanceEnv {
    /// An empty environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored per-branch instances.
    #[must_use]
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// Whether no instance is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// The trait an instance implements.
    pub fn trait_of(&self, id: InstanceId) -> Result<TraitId, Error> {
        self.instances
            .get(id.index() as usize)
            .map(|inst| inst.trait_id)
            .ok_or(Error::UnknownInstance(id))
    }

    /// Every stored instance of `trait_id`, in id order: the solve-side
    /// scan's index, so a resolve touches only its own trait's rows. A
    /// trait no instance implements reads as no rows.
    #[must_use]
    pub fn of_trait(&self, trait_id: TraitId) -> &[InstanceId] {
        self.by_trait.get(&trait_id).map_or(&[], Vec::as_slice)
    }

    /// Appends ids to the trait's index list. The ids come from one
    /// registration seam in id order, so the list stays ascending.
    fn index_rows(&mut self, trait_id: TraitId, ids: impl Iterator<Item = InstanceId>) {
        let mut rows = self.by_trait.get(&trait_id).cloned().unwrap_or_default();
        rows.extend(ids);
        self.by_trait.insert(trait_id, rows);
    }

    /// The solved head of an instance.
    pub fn head(&self, id: InstanceId) -> Result<Ty, Error> {
        self.instances
            .get(id.index() as usize)
            .map(|inst| inst.head)
            .ok_or(Error::UnknownInstance(id))
    }

    /// The declaration's written head a branch came from, before union
    /// decomposition: sibling branches of one declaration share it.
    pub fn written(&self, id: InstanceId) -> Result<Ty, Error> {
        self.instances
            .get(id.index() as usize)
            .map(|inst| inst.written)
            .ok_or(Error::UnknownInstance(id))
    }

    /// The solved conditions of an instance.
    pub fn conditions(&self, id: InstanceId) -> Result<&[Condition<Ty>], Error> {
        self.instances
            .get(id.index() as usize)
            .map(|inst| inst.conditions.as_slice())
            .ok_or(Error::UnknownInstance(id))
    }

    /// Declares an instance: solves and validates the head and conditions,
    /// decomposes union heads into per-branch instances (each branch
    /// keeping only the conditions whose variables its own head mentions),
    /// checks the new branches disjoint from every same-trait head already
    /// present (and from each other), runs the structural cycle check for
    /// the new branches, and returns the new per-branch ids.
    ///
    /// A failed declaration leaves the environment unchanged. Unconditional
    /// instances also record their head atoms as trait members
    /// ([`Universe::impl_trait`]); conditional ones record none.
    pub fn declare(
        &mut self,
        universe: &mut Universe,
        decl: &InstanceDecl,
    ) -> Result<Vec<InstanceId>, Error> {
        universe.trait_decl(decl.trait_id)?;
        let mut conditions = Vec::with_capacity(decl.conditions.len());
        for cond in &decl.conditions {
            universe.trait_decl(cond.trait_id)?;
            let ty = universe.solve(&cond.ty)?;
            conditions.push(Condition::new(cond.trait_id, ty));
        }

        let mut branches = Vec::new();
        collect_branches(&decl.head, &mut branches);
        let mut heads = Vec::with_capacity(branches.len());
        for expr in &branches {
            let head = universe.solve(expr)?;
            // The canonical empty spelling declares a vacuous instance (a
            // zero-case enum hosts its derives over a head no value ever
            // inhabits); any other written branch that solves empty names
            // atoms yet denotes none, and stays refused.
            if universe.is_empty(head) && !matches!(expr, TypeExpr::Empty) {
                return Err(Error::EmptyInstanceHead(head));
            }
            heads.push(head);
        }

        // Per-branch condition solving: each branch head solves its own
        // conditions over only the variables the head mentions, so a
        // condition naming a variable its branch head does not carry
        // drops for that branch (a head naming no bound variable solves
        // unconditionally), and the declaration's requirement set is the
        // union over the branches that keep each condition.
        let mut per_branch = Vec::with_capacity(heads.len());
        for &head in &heads {
            let mut head_vars = Vec::new();
            shape_vars(universe, head, &mut head_vars);
            let mut kept = Vec::new();
            let mut kept_exprs = Vec::new();
            for (cond, expr) in conditions.iter().zip(&decl.conditions) {
                if condition_fits(universe, &head_vars, cond.ty) {
                    kept.push(cond.clone());
                    kept_exprs.push(expr.clone());
                }
            }
            per_branch.push((kept, kept_exprs));
        }

        // The declaration's written head, pre-decomposition: the union of
        // the solved branch heads, so sibling branches share one handle
        // and no second solve runs.
        let mut written_members = Vec::new();
        for &head in &heads {
            written_members.extend(universe.members(head));
        }
        let written = universe.set_ty(&written_members);

        // Declaration-time coherence [B6]: every new branch against every
        // existing same-trait head, then new branches against each other
        // (duplicate branches like `int | int` overlap here). Decomposition
        // precedes the check, so errors name per-branch heads, and each
        // branch probes under its own kept conditions. Pairs the probe
        // concludes disjoint are recorded; the records commit only with
        // the declaration (a failed one rolls its branches back, and with
        // them the ids its records name).
        let first = self.instances.len();
        let mut decided: Vec<(u32, u32)> = Vec::new();
        for (i, &head) in heads.iter().enumerate() {
            let new_head = ProbedHead {
                head,
                conditions: &per_branch[i].0,
            };
            for (index, inst) in self.instances.iter().enumerate() {
                if inst.trait_id == decl.trait_id
                    && let Some(outcome) = overlap_error(
                        universe,
                        self,
                        ProbedHead {
                            head: inst.head,
                            conditions: &inst.conditions,
                        },
                        new_head,
                        decl.trait_id,
                        InstanceId::from_raw(narrow(first + i)),
                        true,
                    )?
                    && matches!(outcome, ProbeOutcome::Decided)
                {
                    decided.push((narrow(index), narrow(first + i)));
                }
            }
            for (j, &later) in heads[i + 1..].iter().enumerate() {
                if let Some(outcome) = overlap_error(
                    universe,
                    self,
                    new_head,
                    ProbedHead {
                        head: later,
                        conditions: &per_branch[i + 1 + j].0,
                    },
                    decl.trait_id,
                    InstanceId::from_raw(narrow(first + i + 1 + j)),
                    true,
                )? && matches!(outcome, ProbeOutcome::Decided)
                {
                    decided.push((narrow(first + i), narrow(first + i + 1 + j)));
                }
            }
        }

        for ((expr, head), (kept, kept_exprs)) in branches.into_iter().zip(heads).zip(per_branch) {
            self.instances.push(Instance {
                trait_id: decl.trait_id,
                head_expr: expr,
                head,
                written,
                condition_exprs: kept_exprs,
                conditions: kept,
            });
        }
        let ids: Vec<InstanceId> = (first..self.instances.len())
            .map(|i| InstanceId::from_raw(narrow(i)))
            .collect();

        // Structural cycle check over the new branches; rollback is complete
        // because every instance on a cycle is conditional, and conditional
        // declarations record no trait members. The stored snapshots are
        // current: they were solved moments ago against this universe.
        for &id in &ids {
            if let Some(cycle) = self
                .stored_edges()
                .cycle_from(&self.instances, id, universe)
            {
                self.instances.truncate(first);
                return Err(Error::ConditionCycle { cycle });
            }
        }

        // The index joins only committed rows: a rolled-back declaration
        // truncated `instances` above, so the index stays in lockstep.
        self.index_rows(decl.trait_id, ids.iter().copied());

        if conditions.is_empty() {
            for &id in &ids {
                let head = self.instances[id.index() as usize].head;
                // Recording membership mints no set, so the head's member set
                // is read by index.
                let len = universe.members(head).len();
                for index in 0..len {
                    let atom = universe.members(head)[index];
                    universe.impl_trait(decl.trait_id, atom)?;
                }
            }
        }
        self.probed_disjoint.extend(decided);
        Ok(ids)
    }

    /// Installs rows a checked-ambient artifact serves pre-declared: the
    /// rows append in artifact order (their ids continue the prefix the
    /// mint-determinism pin holds stable), and the environment marks the
    /// served prefix so [`InstanceEnv::check_all`] probes only what a
    /// fresh row takes part in. The artifact's clean generation ran the
    /// rows' own declarations and the closure pass over them — the
    /// per-declaration and closure probes the serve skips — against a
    /// byte-identical universe state (the drift gate pins the bytes), so
    /// a served prefix is a proven-coherent instance set. A fresh row
    /// declaring afterwards probes against the installed rows exactly as
    /// against declared ones ([`InstanceEnv::declare`]'s rule).
    pub fn serve(&mut self, rows: Vec<ServedInstance>) {
        self.served = self.instances.len() + rows.len();
        let base = self.instances.len();
        let mut fresh: FxHashMap<TraitId, Vec<InstanceId>> = FxHashMap::default();
        for (index, row) in rows.into_iter().enumerate() {
            let id = InstanceId::from_raw(narrow(base + index));
            fresh.entry(row.trait_id).or_default().push(id);
            self.instances.push(Instance {
                trait_id: row.trait_id,
                head_expr: row.head_expr,
                head: row.head,
                written: row.written,
                condition_exprs: row.condition_exprs,
                conditions: row.conditions,
            });
        }
        for (trait_id, ids) in fresh {
            self.index_rows(trait_id, ids.into_iter());
        }
    }

    /// One stored row in full, by id (the artifact generator's extraction
    /// view).
    pub fn row(&self, id: InstanceId) -> Result<ServedInstance, Error> {
        let inst = self
            .instances
            .get(id.index() as usize)
            .ok_or(Error::UnknownInstance(id))?;
        Ok(ServedInstance {
            trait_id: inst.trait_id,
            head_expr: inst.head_expr.clone(),
            head: inst.head,
            written: inst.written,
            condition_exprs: inst.condition_exprs.clone(),
            conditions: inst.conditions.clone(),
        })
    }

    /// Number of rows a checked-ambient artifact served.
    #[must_use]
    pub fn served_len(&self) -> usize {
        self.served
    }

    /// Closure check over the final instance environment (B6, section 10
    /// item 7): every head and condition is first re-solved against the
    /// current universe (stored heads are declaration-time snapshots, and a
    /// trait-as-type head widens when its trait gains members), then
    /// pairwise disjointness of same-trait heads runs over all pairs in
    /// declaration order and the structural cycle check over every
    /// instance. The instantiation probe joins a pair only when no
    /// declaration concluded it disjoint (pairs left open, and pairs a
    /// merge first put together); the set-algebra half runs for every
    /// pair. Per-declaration checking is [`InstanceEnv::declare`]'s
    /// incremental rule; this pass catches what only the closed set — or
    /// the grown universe — shows, so it runs after the single package's
    /// declarations settle and over cross-package link merges. It stops
    /// at the first failure; per-declaration reporting stays complete.
    ///
    /// A served prefix ([`InstanceEnv::serve`]) keeps its stored snapshots
    /// and skips its own pairs and cycles: the artifact's generation
    /// proved the prefix coherent against a byte-identical universe
    /// state, and the generator refuses a prefix whose heads or
    /// conditions name a trait-relative node (`TypeExpr::Trait`/`Not` —
    /// the only forms whose solving membership growth widens), so no
    /// later declaration can re-solve a served row wider than its
    /// snapshot. Every pair and cycle a fresh row takes part in is still
    /// probed — a fresh row declared with the served rows installed, so
    /// [`InstanceEnv::declare`]'s own probe covered the served-fresh
    /// pairs, and any cycle this pass could newly find passes through a
    /// fresh row (the new-instance lemma above), so the fresh starts
    /// reach it.
    pub fn check_all(&self, universe: &mut Universe) -> Result<(), Error> {
        let served = self.served;
        let mut solved_conditions = Vec::with_capacity(self.instances.len());
        let mut heads = Vec::with_capacity(self.instances.len());
        for inst in &self.instances[..served] {
            solved_conditions.push(inst.conditions.clone());
            heads.push(inst.head);
        }
        for inst in &self.instances[served..] {
            let mut conds = Vec::with_capacity(inst.condition_exprs.len());
            for cond in &inst.condition_exprs {
                conds.push(Condition::new(cond.trait_id, universe.solve(&cond.ty)?));
            }
            solved_conditions.push(conds);
            heads.push(universe.solve(&inst.head_expr)?);
        }
        let edges = CondEdges {
            heads,
            conditions: solved_conditions.iter().map(Vec::as_slice).collect(),
        };
        for i in 0..self.instances.len() {
            for j in (i + 1)..self.instances.len() {
                if i < served && j < served {
                    continue;
                }
                let (a, b) = (&self.instances[i], &self.instances[j]);
                if a.trait_id == b.trait_id {
                    // The instantiation probe re-runs only for pairs no
                    // declaration concluded disjoint (pairs the probe left
                    // open, and pairs a merge first put together); the
                    // set-algebra half above still runs for every pair,
                    // since memberships grow after declaration.
                    let probe = !self.probed_disjoint.contains(&(narrow(i), narrow(j)));
                    overlap_error(
                        universe,
                        self,
                        ProbedHead {
                            head: edges.heads[i],
                            conditions: edges.conditions[i],
                        },
                        ProbedHead {
                            head: edges.heads[j],
                            conditions: edges.conditions[j],
                        },
                        a.trait_id,
                        InstanceId::from_raw(narrow(j)),
                        probe,
                    )?;
                }
            }
        }
        for i in served..self.instances.len() {
            let id = InstanceId::from_raw(narrow(i));
            if let Some(cycle) = edges.cycle_from(&self.instances, id, universe) {
                return Err(Error::ConditionCycle { cycle });
            }
        }
        Ok(())
    }

    /// Appends another environment's instances (link (G3)). The appended
    /// instances are renumbered, so previously held ids from `other` are
    /// invalid; callers re-query — the appended rows' recorded probe
    /// verdicts renumber with them. Coherence over the merged set is
    /// [`InstanceEnv::check_all`]'s job; the receiver's own served prefix
    /// stays served (the appended rows are fresh to the merged set).
    pub fn merge(&mut self, other: Self) {
        let offset = narrow(self.instances.len());
        self.probed_disjoint
            .extend(other.probed_disjoint.into_iter().map(|(i, j)| {
                (
                    narrow(i as usize + offset as usize),
                    narrow(j as usize + offset as usize),
                )
            }));
        self.instances.extend(other.instances);
        // The merged rows renumber, so the index rebuilds from the merged
        // id space (one list per trait, ascending in id order).
        let mut fresh: FxHashMap<TraitId, Vec<InstanceId>> = FxHashMap::default();
        for (index, inst) in self.instances.iter().enumerate() {
            fresh
                .entry(inst.trait_id)
                .or_default()
                .push(InstanceId::from_raw(narrow(index)));
        }
        let mut by_trait = IdTable::default();
        for (trait_id, ids) in fresh {
            by_trait.insert(trait_id, ids);
        }
        self.by_trait = by_trait;
    }

    /// Lookup: every instance of `trait_id` whose head contains the queried
    /// type ([`Universe::contains`], exact containment). Lookup-only: an
    /// instance's own conditions never filter it here, and there is no
    /// selection policy (B6) — with coherent heads the answer is unique for
    /// every non-empty query.
    #[must_use]
    pub fn resolve(&self, universe: &Universe, trait_id: TraitId, queried: Ty) -> Vec<InstanceId> {
        self.instances
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.trait_id == trait_id && universe.contains(inst.head, queried))
            .map(|(i, _)| InstanceId::from_raw(narrow(i)))
            .collect()
    }

    /// Unique lookup for the checker's dispatch: the single containing head,
    /// `None` when no head contains the query, and an error naming the
    /// competing instances when several do. With coherence enforced,
    /// ambiguity is impossible for non-empty queries (two disjoint heads
    /// never both contain one), so this errors only for the empty type or
    /// over an unchecked environment.
    pub fn resolve_unique(
        &self,
        universe: &Universe,
        trait_id: TraitId,
        queried: Ty,
    ) -> Result<Option<InstanceId>, Error> {
        let candidates = self.resolve(universe, trait_id, queried);
        match candidates.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(*only)),
            many => Err(Error::AmbiguousResolution {
                trait_id,
                queried,
                candidates: many.to_vec(),
            }),
        }
    }

    /// The stored-snapshot edge view; current within `declare` because heads
    /// and conditions are solved immediately before any probe there.
    fn stored_edges(&self) -> CondEdges<'_> {
        CondEdges {
            heads: self.instances.iter().map(|inst| inst.head).collect(),
            conditions: self
                .instances
                .iter()
                .map(|inst| inst.conditions.as_slice())
                .collect(),
        }
    }
}

/// Flattens written unions into branches (B6): only `|` nodes split, so a
/// negated-constraint or trait-ref head stays one branch even when it solves
/// to several atoms.
fn collect_branches(expr: &TypeExpr, out: &mut Vec<TypeExpr>) {
    match expr {
        TypeExpr::Union(a, b) => {
            collect_branches(a, out);
            collect_branches(b, out);
        },
        _ => out.push(expr.clone()),
    }
}

/// The minted variables one solved type's shape mentions, in member
/// order: a variable atom itself, or one nested in an application or
/// tuple argument.
fn shape_vars(universe: &Universe, ty: Ty, out: &mut Vec<TypeId>) {
    for &atom in universe.members(ty) {
        shape_vars_of_atom(universe, atom, out);
    }
}

fn shape_vars_of_atom(universe: &Universe, atom: TypeId, out: &mut Vec<TypeId>) {
    if crate::instance_solve::is_var(universe, atom) {
        out.push(atom);
        return;
    }
    let Ok(decl) = universe.atom_decl(atom) else {
        return;
    };
    match &decl.kind {
        AtomKind::Apply(_, args) => {
            for arg in args {
                shape_vars(universe, *arg, out);
            }
        },
        AtomKind::Tuple(elements) => {
            for element in elements {
                shape_vars_of_atom(universe, *element, out);
            }
        },
        _ => {},
    }
}

/// Whether a condition solves over only variables its branch head
/// mentions: a condition naming a variable the head does not carry has no
/// substitution this branch can ever solve, so it drops for the branch;
/// a condition naming no variable stays for every branch.
fn condition_fits(universe: &Universe, head_vars: &[TypeId], cond_ty: Ty) -> bool {
    let mut vars = Vec::new();
    shape_vars(universe, cond_ty, &mut vars);
    vars.iter().all(|var| head_vars.contains(var))
}

/// The non-overlap outcome of one same-trait head pair: whether the
/// instantiation probe concluded the pair disjoint for good, or left it
/// open for the closure pass over the closed set.
enum ProbeOutcome {
    /// A structural verdict no later declaration undoes.
    Decided,
    /// The shapes can meet but no witness exists yet.
    Open,
}

/// One side of a coherence pair: a solved head with the conditions the
/// probe runs under it.
#[derive(Clone, Copy)]
struct ProbedHead<'a> {
    head: Ty,
    conditions: &'a [Condition<Ty>],
}

/// Builds the overlap error for two same-trait heads that intersect, with a
/// witness value type of the intersection (B6); `Ok(None)` when the heads
/// are disjoint. `a` is the earlier-declared head. Heads the set algebra
/// reads disjoint because a generic head is opaque get the instantiation
/// probe (a pure query — it never writes the universe; the error path
/// alone materializes a witness): an intersection through generic heads is
/// the same error, with the shared instantiation as the witness. `probe`
/// false skips the instantiation half for a pair a declaration already
/// concluded; the set-algebra half always runs.
fn overlap_error(
    universe: &mut Universe,
    env: &InstanceEnv,
    a: ProbedHead<'_>,
    b: ProbedHead<'_>,
    trait_id: TraitId,
    later: InstanceId,
    probe: bool,
) -> Result<Option<ProbeOutcome>, Error> {
    let inter = if universe.is_disjoint(a.head, b.head) {
        if !probe {
            return Ok(None);
        }
        match crate::instance_solve::instantiation_overlap(
            universe,
            env,
            a.head,
            a.conditions,
            b.head,
            b.conditions,
        ) {
            crate::instance_solve::Instantiation::Witness(grounds) => {
                crate::instance_solve::ground_witness(universe, &grounds)?
            },
            crate::instance_solve::Instantiation::Disjoint => {
                return Ok(Some(ProbeOutcome::Decided));
            },
            crate::instance_solve::Instantiation::Undecided => {
                return Ok(Some(ProbeOutcome::Open));
            },
        }
    } else {
        universe.inter(a.head, b.head)
    };
    let witness = universe.witness(inter)?;
    Err(Error::InstanceOverlap {
        trait_id,
        head_a: a.head,
        head_b: b.head,
        witness,
        later,
    })
}
