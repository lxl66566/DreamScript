//! Call-site instance resolution over declared heads (B6): the modus-ponens
//! step [`InstanceEnv::resolve`] does not cover. The environment's exact
//! lookup asks which declared head *contains* the queried type; a generic
//! impl (`impl [t: shows] shows for list[t]`) declares one instance whose
//! head is the opaque application `list[$t#N]`, so no containment ever
//! holds for a concrete receiver like `list[int]` — the head must be
//! *unified* with the receiver, and the impl's declared bounds must then
//! be proven for the solved substitution.
//!
//! # Matching
//!
//! One instance serves a receiver type when every member atom of the
//! receiver is covered: either contained in the instance's head (the
//! monomorphic case) or unified against the head pattern (a var head
//! binds; an application head matches constructor-for-constructor and
//! unifies arguments; a tuple head matches element-wise). Each receiver
//! member unifies under its own substitution, so a union receiver
//! `list[int] | list[string]` is covered when every branch is — the
//! instance still serves the call with one dispatch target.
//!
//! # Bounds
//!
//! A matching instance's conditions must hold for the substitution: every
//! condition's type is rewritten through the substitution (a bound
//! variable replaced by its solved members) and the bound trait must hold
//! for that type — through an in-scope assumption, an instance whose head
//! contains it, or recursively another instance that serves it by
//! unification. Recursion depth is capped: the declared set's condition
//! cycles were already rejected structurally at declaration, so the cap
//! is a safety net against pathological growth, and reaching it counts
//! as unsatisfied.
//!
//! # Answers
//!
//! With coherent heads the answer is unique for every non-empty query, but
//! a generic head is opaque to the set algebra: `list[$t#1]` and
//! `list[$u#2]` read as disjoint atoms, so coherence itself must look
//! through them. Declaration-time and link-time checks call
//! the instantiation probe: two heads whose provable instantiations
//! intersect (a concrete head unifying with a generic one, or two generic
//! heads grounded at one atom satisfying both bounds) are rejected as an
//! overlap before any call exists; a pair no existing atom can prove
//! declares, and a later conflict surfaces here as true ambiguity and the
//! caller's diagnostic. A head that matches while a bound fails is
//! reported as the nearest miss so the diagnostic can name the unsatisfied
//! bound instead of the receiver alone.

use crate::{
    atom::{AtomKind, TraitId, TypeId},
    instance::{Condition, InstanceEnv, InstanceId},
    universe::{Ty, Universe, is_subset},
};

/// Cap on bound-satisfaction recursion (one level per nested generic
/// instance a proof consumes).
const MAX_DEPTH: u8 = 8;

/// The resolution of one trait for one receiver type.
#[derive(Clone, Debug)]
pub enum Resolution {
    /// No instance serves the receiver.
    None,
    /// The one instance that serves it.
    Unique(InstanceId),
    /// Several instances serve it; true ambiguity, never a priority order.
    Ambiguous(Vec<InstanceId>),
    /// A head matched but a declared bound failed for its substitution.
    BoundUnsat {
        /// The serving-candidate instance whose bound missed.
        instance: InstanceId,
        /// The trait the missed bound demands.
        bound: TraitId,
        /// The substituted type the bound must hold for.
        target: Vec<TypeId>,
    },
}

impl Resolution {
    /// Whether some instance serves the receiver (satisfaction without a
    /// dispatch target: the caller still needs [`Resolution::solved`]).
    #[must_use]
    pub fn satisfied(&self) -> bool {
        !matches!(self, Self::None | Self::BoundUnsat { .. })
    }

    /// The solved instance, when resolution is unique.
    #[must_use]
    pub fn solved(&self) -> Option<InstanceId> {
        match self {
            Self::Unique(id) => Some(*id),
            _ => None,
        }
    }
}

/// Resolves `trait_id` for `target` over the declared instances: every
/// instance whose head contains the target or unifies with it under
/// provable bounds. An empty target is unsatisfied (it matches nothing).
#[must_use]
pub fn resolve(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    trait_id: TraitId,
    target: Ty,
) -> Resolution {
    let members = universe.members(target);
    if members.is_empty() {
        return Resolution::None;
    }
    let mut serving = Vec::new();
    let mut miss = None;
    for index in 0..env.len() {
        let id = InstanceId::from_raw(u32::try_from(index).expect("index fits u32"));
        if env.trait_of(id) != Ok(trait_id) {
            continue;
        }
        if serves(universe, env, assumptions, id, members, MAX_DEPTH) {
            serving.push(id);
        } else if miss.is_none() {
            if let Some(bound) = nearest_miss(universe, env, assumptions, id, members) {
                miss = Some(bound);
            }
        }
    }
    match (serving.as_slice(), miss) {
        ([], None) => Resolution::None,
        ([], Some(miss)) => miss,
        ([only], _) => Resolution::Unique(*only),
        (many, _) => Resolution::Ambiguous(many.to_vec()),
    }
}

/// Whether one instance serves the whole target under provable bounds:
/// the head pattern covers the receiver (containment or unification),
/// and every condition holds for the solved substitution.
fn serves(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    id: InstanceId,
    target: &[TypeId],
    depth: u8,
) -> bool {
    let (Ok(head), Ok(conditions)) = (env.head(id), env.conditions(id)) else {
        return false;
    };
    serves_head(universe, env, assumptions, head, conditions, target, depth)
}

/// Whether one declared head with its conditions serves the target: a
/// stored instance is the [`InstanceEnv`] case; the coherence probe passes
/// a head that is not yet stored.
fn serves_head(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    head: Ty,
    conditions: &[Condition<Ty>],
    target: &[TypeId],
    depth: u8,
) -> bool {
    let head_members = universe.members(head);
    if is_subset(target, head_members) {
        return conditions_hold(universe, env, assumptions, conditions, &[], depth);
    }
    // Structural match: a single-atom head pattern unifies with every
    // target member. Multi-atom heads carry no variables (written unions
    // decompose at declaration, and trait-as-type or negation heads name
    // no generic parameters), so containment already decided them.
    let [pattern] = head_members else {
        return false;
    };
    target.iter().all(|&member| {
        let mut subst = Vec::new();
        unify_atom(universe, &mut subst, *pattern, member)
            && conditions_hold(universe, env, assumptions, conditions, &subst, depth)
    })
}

/// The one substitution a receiver's every member unifies under against
/// an instance's head pattern, shared across members: a later binding must
/// stay inside the earlier one, so a union receiver whose branches solve a
/// variable differently answers `None`. Callers of the runtime-evidence
/// seam use this; plain resolution keeps its per-member substitutions.
fn head_substitution(
    universe: &Universe,
    env: &InstanceEnv,
    id: InstanceId,
    recv: Ty,
) -> Option<Vec<(TypeId, Vec<TypeId>)>> {
    pattern_substitution(universe, env.head(id).ok()?, recv)
}

/// The shared head-pattern form of [`head_substitution`]: one single-atom
/// pattern unified against every receiver member under one substitution.
fn pattern_substitution(
    universe: &Universe,
    head: Ty,
    recv: Ty,
) -> Option<Vec<(TypeId, Vec<TypeId>)>> {
    let [pattern] = universe.members(head) else {
        return None;
    };
    let members = universe.members(recv);
    if members.is_empty() {
        return None;
    }
    let mut subst = Vec::new();
    if !members
        .iter()
        .all(|&member| unify_atom(universe, &mut subst, *pattern, member))
    {
        return None;
    }
    Some(subst)
}

/// The declared bounds of an inherent impl's head pattern against one
/// receiver (F3): the pattern's variables bound by unifying it with the
/// receiver's members under the shared substitution, then every condition's
/// variable rewritten through it — the inherent mirror of
/// [`evidence_bounds`]'s per-condition loop. Each entry names the
/// condition's variable atom, its trait, and the solved type the bound
/// must hold for at this call site; an empty answer means the head did not
/// unify (the lowerer's staging refusal owns that shape).
#[must_use]
pub fn inherent_bound_targets(
    universe: &mut Universe,
    head: Ty,
    conditions: &[Condition<Ty>],
    recv: Ty,
) -> Vec<(TypeId, TraitId, Ty)> {
    let Some(subst) = pattern_substitution(universe, head, recv) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for cond in conditions {
        let var = match universe.members(cond.ty) {
            [only] => *only,
            _ => continue,
        };
        let solved = substitute(&subst, universe.members(cond.ty));
        let target = universe.set_ty(&solved);
        out.push((var, cond.trait_id, target));
    }
    out
}

/// The runtime evidence one call site into a generic impl's body needs
/// (H3): for every declared bound of the solved instance, the unique
/// instance serving that bound's trait for the variable's substituted
/// type at this call site — the checker records it beside the solved
/// instance so the lowering side never re-derives types. A `None` entry
/// marks a bound no unique instance serves (or a receiver whose members
/// solve a variable inconsistently, a union across branches); the
/// lowering side refuses those honestly.
#[must_use]
pub fn evidence_bounds(
    universe: &mut Universe,
    env: &InstanceEnv,
    instance: InstanceId,
    recv: Ty,
) -> Vec<(TypeId, TraitId, Option<InstanceId>)> {
    let Ok(conditions) = env.conditions(instance) else {
        return Vec::new();
    };
    let Some(subst) = head_substitution(universe, env, instance, recv) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for cond in conditions {
        let var = match universe.members(cond.ty) {
            [only] => *only,
            _ => continue,
        };
        let solved = substitute(&subst, universe.members(cond.ty));
        let target = universe.set_ty(&solved);
        let served = resolve(universe, env, &[], cond.trait_id, target).solved();
        out.push((var, cond.trait_id, served));
    }
    out
}

/// The first failing bound of an instance whose head covers the target
/// but whose conditions do not hold, when the head matched by
/// unification: the bound's trait and its substituted type.
fn nearest_miss(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    id: InstanceId,
    target: &[TypeId],
) -> Option<Resolution> {
    let head = env.head(id).ok()?;
    let [pattern] = universe.members(head) else {
        return None;
    };
    let mut subst = Vec::new();
    if !target
        .iter()
        .all(|&m| unify_atom(universe, &mut subst, *pattern, m))
    {
        return None;
    }
    for cond in env.conditions(id).ok()? {
        let solved = substitute(&subst, universe.members(cond.ty));
        if !trait_holds(
            universe,
            env,
            assumptions,
            cond.trait_id,
            &solved,
            MAX_DEPTH,
        ) {
            return Some(Resolution::BoundUnsat {
                instance: id,
                bound: cond.trait_id,
                target: solved,
            });
        }
    }
    None
}

/// One pattern atom against one query atom, binding variables. A variable
/// pattern binds the whole query set at argument positions and a single
/// atom at structural positions; an application matches its constructor
/// and arity with arguments unified pairwise; a tuple matches
/// element-wise; every other atom is nominal and demands equality.
fn unify_atom(
    universe: &Universe,
    subst: &mut Vec<(TypeId, Vec<TypeId>)>,
    pattern: TypeId,
    query: TypeId,
) -> bool {
    if is_var(universe, pattern) {
        return bind(subst, pattern, vec![query]);
    }
    let Ok(decl) = universe.atom_decl(pattern) else {
        return false;
    };
    match &decl.kind {
        AtomKind::Apply(ctor, args) => {
            let Ok(qdecl) = universe.atom_decl(query) else {
                return false;
            };
            let AtomKind::Apply(qctor, qargs) = &qdecl.kind else {
                return false;
            };
            ctor == qctor
                && args.len() == qargs.len()
                && args
                    .iter()
                    .zip(qargs.iter())
                    .all(|(h, q)| unify_set(universe, subst, *h, *q))
        },
        AtomKind::Tuple(elements) => {
            let Ok(qdecl) = universe.atom_decl(query) else {
                return false;
            };
            let AtomKind::Tuple(qelements) = &qdecl.kind else {
                return false;
            };
            elements.len() == qelements.len()
                && elements
                    .iter()
                    .zip(qelements.iter())
                    .all(|(h, q)| unify_atom(universe, subst, *h, *q))
        },
        _ => pattern == query,
    }
}

/// One pattern set against one query set: a lone variable binds the
/// whole query; a lone atom unifies with every query member; anything
/// else is variable-free and falls back to containment.
fn unify_set(
    universe: &Universe,
    subst: &mut Vec<(TypeId, Vec<TypeId>)>,
    pattern: Ty,
    query: Ty,
) -> bool {
    let query_members = universe.members(query);
    if query_members.is_empty() {
        return false;
    }
    match universe.members(pattern) {
        [only] if is_var(universe, *only) => bind(subst, *only, query_members.to_vec()),
        [only] => query_members
            .iter()
            .all(|&q| unify_atom(universe, subst, *only, q)),
        _ => is_subset(query_members, universe.members(pattern)),
    }
}

/// Binds a pattern variable, or checks a later demand fits the earlier
/// binding: the later value must be contained in the bound set.
fn bind(subst: &mut Vec<(TypeId, Vec<TypeId>)>, var: TypeId, mut value: Vec<TypeId>) -> bool {
    if let Some((_, existing)) = subst.iter_mut().find(|(v, _)| *v == var) {
        return is_subset(&value, existing);
    }
    value.sort_unstable();
    value.dedup();
    subst.push((var, value));
    true
}

/// Whether one of the checker's minted type variables (an inference hole,
/// never a data type): names of surface atoms cannot contain `$`.
fn is_var(universe: &Universe, atom: TypeId) -> bool {
    universe
        .atom_decl(atom)
        .is_ok_and(|decl| decl.name.starts_with('$'))
}

/// Every condition of one instance for one substitution: each condition's
/// type is rewritten through it and the bound trait must hold there.
fn conditions_hold(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    conditions: &[Condition<Ty>],
    subst: &[(TypeId, Vec<TypeId>)],
    depth: u8,
) -> bool {
    conditions.iter().all(|cond| {
        let solved = substitute(subst, universe.members(cond.ty));
        trait_holds(universe, env, assumptions, cond.trait_id, &solved, depth)
    })
}

/// Rewrites a member set through a substitution: bound variables take
/// their solved members, everything else stays.
fn substitute(subst: &[(TypeId, Vec<TypeId>)], members: &[TypeId]) -> Vec<TypeId> {
    let mut out = Vec::with_capacity(members.len());
    for &member in members {
        match subst.iter().find(|(var, _)| *var == member) {
            Some((_, value)) => out.extend(value.iter().copied()),
            None => out.push(member),
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether a trait holds for a member set: an in-scope assumption covers
/// it, or some declared instance serves it (containment or unification
/// with its own bounds proven, recursively, under the depth cap).
fn trait_holds(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    trait_id: TraitId,
    target: &[TypeId],
    depth: u8,
) -> bool {
    if depth == 0 || target.is_empty() {
        return false;
    }
    if assumptions
        .iter()
        .any(|(t, assumed)| *t == trait_id && is_subset(target, universe.members(*assumed)))
    {
        return true;
    }
    for index in 0..env.len() {
        let id = InstanceId::from_raw(u32::try_from(index).expect("index fits u32"));
        if env.trait_of(id) == Ok(trait_id)
            && serves(universe, env, assumptions, id, target, depth - 1)
        {
            return true;
        }
    }
    false
}

/// Declaration-time coherence through generic heads (B6): whether two
/// same-trait heads the set algebra reads disjoint (a generic head is one
/// opaque atom, so `list[int]` and `list[$t#1]` never intersect there)
/// still cover one common instantiation, answered by instantiating the
/// call-site machinery: every candidate grounding of either head must be
/// served by both heads under provable conditions.
///
/// Candidates ground each head by replacing every one of its variables
/// with one declared atom (real types only — minted inference variables
/// name no values), so a two-generic overlap is proven exactly when some
/// existing atom satisfies both impls' bounds, and a generic head against
/// a concrete head is decided by unification and the substituted bound.
/// The probe is conservative where grounding cannot reach (both heads
/// generic with no bound-satisfying atom yet, a bare-variable head against
/// an application whose overlap type is not yet minted): those pairs stay
/// disjoint here and any true conflict still surfaces as call-site
/// ambiguity (DS2010). Structural application atoms minted by grounding
/// are ordinary universe atoms — hash-consed, never trait members.
#[must_use]
pub(crate) fn instantiation_overlap(
    universe: &mut Universe,
    env: &InstanceEnv,
    head_a: Ty,
    conds_a: &[Condition<Ty>],
    head_b: Ty,
    conds_b: &[Condition<Ty>],
) -> Option<Ty> {
    if !head_carries_vars(universe, head_a) && !head_carries_vars(universe, head_b) {
        return None;
    }
    let candidates: Vec<TypeId> = universe
        .atom_ids()
        .filter(|&atom| !is_var(universe, atom))
        .collect();
    let mut probed = Vec::new();
    for &x in &candidates {
        for (head, conds_head, other_head, conds_other) in [
            (head_a, conds_a, head_b, conds_b),
            (head_b, conds_b, head_a, conds_a),
        ] {
            let Some(target) = ground(universe, head, x) else {
                continue;
            };
            if probed.contains(&target) {
                continue;
            }
            probed.push(target);
            let members: Vec<TypeId> = universe.members(target).to_vec();
            if serves_head(universe, env, &[], head, conds_head, &members, MAX_DEPTH)
                && serves_head(
                    universe,
                    env,
                    &[],
                    other_head,
                    conds_other,
                    &members,
                    MAX_DEPTH,
                )
            {
                return Some(target);
            }
        }
    }
    None
}

/// Whether a head can carry variables into the probe: only a single-atom
/// head does (written unions decompose at declaration; a trait-as-type or
/// negation head names no generic parameter, and a multi-atom head that
/// merely snapshots minted variables is left to the set algebra —
/// grounding it would rewrite coverage that never depended on them).
fn head_carries_vars(universe: &Universe, head: Ty) -> bool {
    match universe.members(head) {
        [only] => carries_vars(universe, *only),
        _ => false,
    }
}

/// Whether an atom names a variable anywhere in its shape.
fn carries_vars(universe: &Universe, atom: TypeId) -> bool {
    if is_var(universe, atom) {
        return true;
    }
    let Ok(decl) = universe.atom_decl(atom) else {
        return false;
    };
    match &decl.kind {
        AtomKind::Apply(_, args) => args.iter().any(|arg| {
            universe
                .members(*arg)
                .iter()
                .any(|&a| carries_vars(universe, a))
        }),
        _ => false,
    }
}

/// One head grounded at a candidate atom: every variable member becomes
/// the candidate, applications re-mint structurally, every other atom
/// stays. A shape the grounding does not reach (a tuple with variable
/// elements) grounds nothing.
fn ground(universe: &mut Universe, head: Ty, x: TypeId) -> Option<Ty> {
    let head_members = universe.members(head).to_vec();
    let mut members = Vec::with_capacity(head_members.len());
    for atom in head_members {
        members.push(ground_atom(universe, atom, x)?);
    }
    Some(universe.set_ty(&members))
}

fn ground_atom(universe: &mut Universe, atom: TypeId, x: TypeId) -> Option<TypeId> {
    if is_var(universe, atom) {
        return Some(x);
    }
    let kind = universe.atom_decl(atom).ok()?.kind.clone();
    match kind {
        AtomKind::Apply(ctor, args) => {
            let mut grounded = Vec::with_capacity(args.len());
            for arg in args {
                grounded.push(ground(universe, arg, x)?);
            }
            universe.apply_atom(ctor, &grounded).ok()
        },
        _ => Some(atom),
    }
}
