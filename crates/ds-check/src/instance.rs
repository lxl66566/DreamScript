//! The trait instance environment and coherence checking (B6).
//!
//! Traits are implemented by *instances* declared over type heads. The only
//! coherence rule is pairwise disjointness of the heads of one trait: for
//! every pair `H1`, `H2` of same-trait heads, `H1 & H2 = Empty` must hold
//! (B6). It is checked incrementally at declaration (per package (G3)) by
//! [`InstanceEnv::declare`] and again over the final environment — once
//! the single package's declarations settle, and over every link merge —
//! by [`InstanceEnv::check_all`]. Heads the set algebra reads
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
//! solves to the empty set and is rejected ([`Error::EmptyInstanceHead`]),
//! while `(a | b) - b` normalizes to the singleton `a`. A negated-constraint
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
//! checker's resolution step. A condition edge runs from an instance to every
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

use crate::{
    atom::TraitId,
    error::Error,
    expr::TypeExpr,
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
    /// decomposes union heads into per-branch instances, checks the new
    /// branches disjoint from every same-trait head already present (and
    /// from each other), runs the structural cycle check for the new
    /// branches, and returns the new per-branch ids.
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
            if universe.is_empty(head) {
                return Err(Error::EmptyInstanceHead(head));
            }
            heads.push(head);
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
        // precedes the check, so errors name per-branch heads.
        let first = self.instances.len();
        for (i, &head) in heads.iter().enumerate() {
            for inst in &self.instances {
                if inst.trait_id == decl.trait_id {
                    if let Some(err) = overlap_error(
                        universe,
                        self,
                        inst.head,
                        &inst.conditions,
                        head,
                        &conditions,
                        decl.trait_id,
                        InstanceId::from_raw(narrow(first + i)),
                    )? {
                        return Err(err);
                    }
                }
            }
            for (j, &later) in heads[i + 1..].iter().enumerate() {
                if let Some(err) = overlap_error(
                    universe,
                    self,
                    head,
                    &conditions,
                    later,
                    &conditions,
                    decl.trait_id,
                    InstanceId::from_raw(narrow(first + i + 1 + j)),
                )? {
                    return Err(err);
                }
            }
        }

        for (expr, head) in branches.into_iter().zip(heads) {
            self.instances.push(Instance {
                trait_id: decl.trait_id,
                head_expr: expr,
                head,
                written,
                condition_exprs: decl.conditions.clone(),
                conditions: conditions.clone(),
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
        Ok(ids)
    }

    /// Closure check over the final instance environment (B6, section 10
    /// item 7): every head and condition is first re-solved against the
    /// current universe (stored heads are declaration-time snapshots, and a
    /// trait-as-type head widens when its trait gains members), then
    /// pairwise disjointness of same-trait heads runs over all pairs in
    /// declaration order and the structural cycle check over every
    /// instance. Per-declaration checking is [`InstanceEnv::declare`]'s
    /// incremental rule; this pass catches what only the closed set — or
    /// the grown universe — shows, so it runs after the single package's
    /// declarations settle and over cross-package link merges. It stops
    /// at the first failure; per-declaration reporting stays complete.
    pub fn check_all(&self, universe: &mut Universe) -> Result<(), Error> {
        let mut solved_conditions = Vec::with_capacity(self.instances.len());
        let mut heads = Vec::with_capacity(self.instances.len());
        for inst in &self.instances {
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
                let (a, b) = (&self.instances[i], &self.instances[j]);
                if a.trait_id == b.trait_id {
                    if let Some(err) = overlap_error(
                        universe,
                        self,
                        edges.heads[i],
                        edges.conditions[i],
                        edges.heads[j],
                        edges.conditions[j],
                        a.trait_id,
                        InstanceId::from_raw(narrow(j)),
                    )? {
                        return Err(err);
                    }
                }
            }
        }
        for i in 0..self.instances.len() {
            let id = InstanceId::from_raw(narrow(i));
            if let Some(cycle) = edges.cycle_from(&self.instances, id, universe) {
                return Err(Error::ConditionCycle { cycle });
            }
        }
        Ok(())
    }

    /// Appends another environment's instances (link (G3)). The appended
    /// instances are renumbered, so previously held ids from `other` are
    /// invalid; callers re-query. Coherence over the merged set is
    /// [`InstanceEnv::check_all`]'s job.
    pub fn merge(&mut self, other: Self) {
        self.instances.extend(other.instances);
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

/// Builds the overlap error for two same-trait heads that intersect, with a
/// witness value type of the intersection (B6); `None` when the heads are
/// disjoint. `head_a` is the earlier-declared head. Heads the set algebra
/// reads disjoint because a generic head is opaque get the instantiation
/// probe: an intersection through generic heads is the same error, with
/// the shared instantiation as the witness.
fn overlap_error(
    universe: &mut Universe,
    env: &InstanceEnv,
    head_a: Ty,
    conds_a: &[Condition<Ty>],
    head_b: Ty,
    conds_b: &[Condition<Ty>],
    trait_id: TraitId,
    later: InstanceId,
) -> Result<Option<Error>, Error> {
    let inter = if universe.is_disjoint(head_a, head_b) {
        let Some(shared) = crate::instance_solve::instantiation_overlap(
            universe, env, head_a, conds_a, head_b, conds_b,
        ) else {
            return Ok(None);
        };
        shared
    } else {
        universe.inter(head_a, head_b)
    };
    let witness = universe.witness(inter)?;
    Ok(Some(Error::InstanceOverlap {
        trait_id,
        head_a,
        head_b,
        witness,
        later,
    }))
}
