//! Call-site instance resolution over declared heads: the modus-ponens
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
//! instance still serves the call with one dispatch target. A union
//! receiver no single instance serves is covered branch-wise when the
//! covering instances are sibling branches of one written head: their
//! one impl item (the member body checked over that head) serves the
//! whole union, and resolution names a representative branch.
//!
//! # Bounds
//!
//! A matching instance's conditions must hold for the substitution: every
//! condition's type is rewritten through the substitution (a bound
//! variable replaced by its solved members) and the bound trait must hold
//! for that type — through an in-scope assumption, an instance whose head
//! contains it, or recursively another instance that serves it by
//! unification. Every condition a branch carries names variables its own
//! head mentions (per-branch solving), so the substitution binds each
//! one; a branch naming no bound variable is unconditional. Recursion depth is capped: the declared
//! set's condition cycles were already rejected structurally at declaration, so the cap
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
//! caller's diagnostic. The probe is a pure query over declared atoms —
//! it grounds heads as structural descriptions and never writes the
//! universe, so its products cannot become later probes' candidates (and
//! a clean check mints nothing). A head that matches while a bound fails
//! is reported as the nearest miss so the diagnostic can name the
//! unsatisfied bound instead of the receiver alone.

use crate::{
    atom::{AtomKind, TraitId, TypeId},
    error::Error,
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
    /// A union receiver no single instance serves, covered member-wise by
    /// sibling branches of one written head: the representative names the
    /// group, whose one impl item serves the whole union.
    Covered(InstanceId),
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

    /// The solved instance, when resolution names one: the unique server,
    /// or a covering group's representative branch.
    #[must_use]
    pub fn solved(&self) -> Option<InstanceId> {
        match self {
            Self::Unique(id) | Self::Covered(id) => Some(*id),
            _ => None,
        }
    }
}

/// Resolves `trait_id` for `target` over the declared instances: every
/// instance whose head contains the target or unifies with it under
/// provable bounds. An empty target is unsatisfied (it matches nothing).
/// `selection` narrows the scan to one vocabulary slot: `None` scans the
/// unnamed default rows alone, `Some(name)` the impl naming itself — a
/// named instance never answers an unselected call, and an unnamed one
/// never answers a selected one.
#[must_use]
pub fn resolve(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    trait_id: TraitId,
    target: Ty,
    selection: Option<&str>,
) -> Resolution {
    resolve_members(
        universe,
        env,
        assumptions,
        trait_id,
        universe.members(target),
        selection,
    )
}

/// The rows of `trait_id` in one vocabulary slot: the unnamed default rows
/// for a `None` selection, the rows of the named impl for `Some(name)`.
fn slot_rows(env: &InstanceEnv, trait_id: TraitId, selection: Option<&str>) -> Vec<InstanceId> {
    env.of_trait(trait_id)
        .into_iter()
        .filter(|id| env.name(*id) == selection)
        .collect()
}

/// The named rows of `trait_id` that serve `target` under provable
/// bounds: the unselected-call refusal's evidence — a head whose only
/// impls are named diagnoses the selections instead of a bare missing
/// instance.
#[must_use]
pub fn named_servers(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    trait_id: TraitId,
    target: Ty,
) -> Vec<InstanceId> {
    env.of_trait(trait_id)
        .into_iter()
        .filter(|id| env.name(*id).is_some())
        .filter(|id| {
            serves(
                universe,
                env,
                assumptions,
                *id,
                universe.members(target),
                MAX_DEPTH,
            )
        })
        .collect()
}

/// The member-set form of [`resolve`]: the immutable core the lowering
/// side's transitive chain walks also reads (a solved member set names no
/// interned `Ty` handle there).
fn resolve_members(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    trait_id: TraitId,
    members: &[TypeId],
    selection: Option<&str>,
) -> Resolution {
    if members.is_empty() {
        return Resolution::None;
    }
    let mut serving = Vec::new();
    let mut miss = None;
    for id in slot_rows(env, trait_id, selection) {
        if serves(universe, env, assumptions, id, members, MAX_DEPTH) {
            serving.push(id);
        } else if miss.is_none()
            && let Some(bound) = nearest_miss(universe, env, assumptions, id, members)
        {
            miss = Some(bound);
        }
    }
    // A union receiver no single instance serves may still be covered
    // member-wise by sibling branches of one written head; the check
    // runs only on the empty answer, so a serving instance always wins
    // and the nearest-miss diagnostics survive a partial cover.
    if serving.is_empty()
        && members.len() > 1
        && let Some(covered) =
            covered_branches(universe, env, assumptions, trait_id, members, selection)
    {
        return covered;
    }
    match (serving.as_slice(), miss) {
        ([], None) => Resolution::None,
        ([], Some(miss)) => miss,
        ([only], _) => Resolution::Unique(*only),
        (many, _) => Resolution::Ambiguous(many.to_vec()),
    }
}

/// The branch-wise aggregation over a union receiver no single instance
/// serves: per member atom, the instances that cover it; a group of
/// sibling instances sharing one written head (the per-branch
/// decomposition of one union-head declaration, one impl item behind
/// them) serves the union when every member is covered by some instance
/// of the group. Distinct written heads covering the union together do
/// not — dispatch cannot pick between impl items — so members answered
/// by separate declarations still refuse. Exactly one covering group
/// answers [`Resolution::Covered`] with its first branch as the
/// representative; several covering groups stay true ambiguity; none
/// leaves the caller's nearest-miss answer in place.
fn covered_branches(
    universe: &Universe,
    env: &InstanceEnv,
    assumptions: &[(TraitId, Ty)],
    trait_id: TraitId,
    members: &[TypeId],
    selection: Option<&str>,
) -> Option<Resolution> {
    // Instances grouped by written head, in declaration order; each entry
    // carries its per-member coverage.
    let mut heads: Vec<Ty> = Vec::new();
    let mut groups: Vec<Vec<(InstanceId, Vec<bool>)>> = Vec::new();
    for id in slot_rows(env, trait_id, selection) {
        let Ok(written) = env.written(id) else {
            continue;
        };
        let coverage: Vec<bool> = members
            .iter()
            .map(|&member| serves(universe, env, assumptions, id, &[member], MAX_DEPTH))
            .collect();
        if let Some(pos) = heads.iter().position(|&head| head == written) {
            groups[pos].push((id, coverage));
        } else {
            heads.push(written);
            groups.push(vec![(id, coverage)]);
        }
    }
    let mut representatives = Vec::new();
    for group in &groups {
        let covers = members
            .iter()
            .enumerate()
            .all(|(index, _)| group.iter().any(|(_, coverage)| coverage[index]));
        if covers {
            representatives.push(group[0].0);
        }
    }
    match representatives.as_slice() {
        [] => None,
        [only] => Some(Resolution::Covered(*only)),
        many => Some(Resolution::Ambiguous(many.to_vec())),
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
/// Also serves the inherent side directly: a call site into a generic
/// inherent impl substitutes the head pattern's variables into the
/// member's written signature from these pairs.
pub(crate) fn pattern_substitution(
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

/// The written parameter name of one minted type-variable atom
/// (`$t#12` -> `t`), `None` for any other atom: the name a member's
/// written signature spells the impl's variable as, so a call site's
/// substitution keys by it.
#[must_use]
pub(crate) fn var_param_name(universe: &Universe, atom: TypeId) -> Option<String> {
    let decl = universe
        .atom_decl(atom)
        .ok()
        .filter(|decl| decl.name.starts_with('$'))?;
    let base = decl.name.strip_prefix('$')?;
    base.rsplit_once('#').map(|(name, _)| name.to_owned())
}

/// The declared bounds of an inherent impl's head pattern against one
/// receiver: the pattern's variables bound by unifying it with the
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

/// The runtime evidence one call site into a generic impl's body needs:
/// for every declared bound of the solved instance, the unique
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
        // Nested demands carry no selection of their own: a named
        // instance never serves one, so the default slot alone answers.
        let served = resolve(universe, env, &[], cond.trait_id, target, None).solved();
        out.push((var, cond.trait_id, served));
    }
    out
}

/// The trait's declared generics substituted at one receiver: the
/// impl behind the solved instance spells the substitution in its trait
/// reference, and unifying the instance's head pattern with the
/// receiver's members binds the impl's own variables — each argument
/// rewritten through that binding names the type its trait generic holds
/// at this site, so a member call's arguments check against the site's
/// own substitution rather than binding the generics freely. An argument
/// still naming an unbound variable answers `None` in its position (the
/// caller leaves that generic free, exactly as an impl spelling no
/// arguments does); `None` overall when the head does not unify with the
/// receiver (a covered union receiver).
#[must_use]
pub fn substituted_trait_args(
    universe: &mut Universe,
    env: &InstanceEnv,
    instance: InstanceId,
    args: &[Ty],
    recv: Ty,
) -> Option<Vec<Option<Ty>>> {
    let head = env.head(instance).ok()?;
    let subst = pattern_substitution(universe, head, recv)?;
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        match rewrite_set(universe, &subst, *arg) {
            Some(members) => out.push(Some(universe.set_ty(&members))),
            None => out.push(None),
        }
    }
    Some(out)
}

/// Rewrites one member set through a substitution, structurally: a
/// minted variable expands to its bound members, an application re-mints
/// with rewritten arguments, every other atom stays. `None` when any
/// variable the shape names stays unbound — no atom spells it, so the
/// caller leaves the generic it serves free.
fn rewrite_set(
    universe: &mut Universe,
    subst: &[(TypeId, Vec<TypeId>)],
    ty: Ty,
) -> Option<Vec<TypeId>> {
    // Rewriting mints through the universe, so the member set is read by
    // index; minting appends and never moves an existing handle's set.
    let len = universe.members(ty).len();
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        let atom = universe.members(ty)[index];
        if is_var(universe, atom) {
            let (_, bound) = subst.iter().find(|(var, _)| *var == atom)?;
            out.extend(bound.iter().copied());
            continue;
        }
        let Ok(decl) = universe.atom_decl(atom) else {
            out.push(atom);
            continue;
        };
        match decl.kind {
            AtomKind::Apply(ctor, ref args) => {
                // the owned copy ends the declaration borrow before the
                // rewriting mints through the universe
                let owned: Vec<Ty> = args.to_vec();
                let mut rewritten = Vec::with_capacity(owned.len());
                for arg in owned {
                    let members = rewrite_set(universe, subst, arg)?;
                    rewritten.push(universe.set_ty(&members));
                }
                out.push(universe.apply_atom(ctor, &rewritten).ok()?);
            },
            _ => out.push(atom),
        }
    }
    Some(out)
}

/// One step of a call site's transitive evidence chain, over a
/// declared trait instance: for the instance serving one of a generic
/// body's demands, the substituted member set its matching declared bound
/// demands at this receiver, plus the unique instance serving that bound
/// there. The entering site iterates this to stage the follow words a
/// runtime dispatch into a generic serving body slides; a `None` answer
/// (a head that does not unify, no bound of the trait on that variable)
/// and a `None` server are the lowering side's honest refusals.
#[must_use]
pub fn evidence_step(
    universe: &Universe,
    env: &InstanceEnv,
    instance: InstanceId,
    recv: &[TypeId],
    var: Option<&str>,
    trait_id: TraitId,
) -> Option<(Vec<TypeId>, Option<InstanceId>)> {
    let (Ok(head), Ok(conditions)) = (env.head(instance), env.conditions(instance)) else {
        return None;
    };
    bound_evidence_step(universe, env, head, conditions, recv, var, trait_id)
}

/// The same step over an inherent impl's declared head and bounds:
/// the inherent mirror never names an instance of its own, so the chain
/// starts from the impl's record directly.
#[must_use]
pub fn inherent_evidence_step(
    universe: &Universe,
    env: &InstanceEnv,
    head: Ty,
    conditions: &[Condition<Ty>],
    recv: &[TypeId],
    var: Option<&str>,
    trait_id: TraitId,
) -> Option<(Vec<TypeId>, Option<InstanceId>)> {
    bound_evidence_step(universe, env, head, conditions, recv, var, trait_id)
}

/// The shared step: unify the head pattern with the receiver members,
/// then answer the bound the named variable carries for the trait.
fn bound_evidence_step(
    universe: &Universe,
    env: &InstanceEnv,
    head: Ty,
    conditions: &[Condition<Ty>],
    recv: &[TypeId],
    var: Option<&str>,
    trait_id: TraitId,
) -> Option<(Vec<TypeId>, Option<InstanceId>)> {
    let [pattern] = universe.members(head) else {
        return None;
    };
    let mut subst = Vec::new();
    if !recv
        .iter()
        .all(|&member| unify_atom(universe, &mut subst, *pattern, member))
    {
        return None;
    }
    for cond in conditions {
        if cond.trait_id != trait_id {
            continue;
        }
        if let Some(want) = var
            && !carries_var(universe, cond.ty, want)
        {
            continue;
        }
        let solved = substitute(&subst, universe.members(cond.ty));
        // The evidence chain resolves nested demands in the default
        // vocabulary: its calls spell no selection.
        let served = resolve_members(universe, env, &[], cond.trait_id, &solved, None).solved();
        return Some((solved, served));
    }
    None
}

/// Whether a condition's type is one minted variable with the written
/// base name: instance declaration and body checking mint their own
/// atoms, so demands key by the name they share.
fn carries_var(universe: &Universe, ty: Ty, want: &str) -> bool {
    let [only] = universe.members(ty) else {
        return false;
    };
    universe
        .atom_decl(*only)
        .ok()
        .and_then(|decl| decl.name.strip_prefix('$'))
        .and_then(|base| base.rsplit_once('#'))
        .is_some_and(|(name, _)| name == want)
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
pub(crate) fn is_var(universe: &Universe, atom: TypeId) -> bool {
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
/// with its own bounds proven, recursively, under the depth cap). Bound
/// proofs scan the default vocabulary alone: a named instance answers a
/// call site's selection, never a nested proof demand.
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
    for id in env.of_trait(trait_id) {
        if env.name(id).is_some() {
            continue;
        }
        if serves(universe, env, assumptions, id, target, depth - 1) {
            return true;
        }
    }
    false
}

/// The instantiation probe's verdict on one same-trait head pair:
/// whether two heads the set algebra reads disjoint (a generic head is one
/// opaque atom, so `list[int]` and `list[$t#1]` never intersect there)
/// still cover one common instantiation.
///
/// The probe is a pure query over declared atoms: a structural gate
/// rejects pairs whose shapes cannot meet (distinct constructors,
/// arities, or nominal atoms) without any candidate, and the pairs that
/// pass ground each head at every declared non-variable atom — as
/// [`Ground`] descriptions, never minting — and ask whether both heads
/// serve one grounding under provable conditions. A pair whose shapes
/// meet but that no declared atom witnesses is
/// [`Instantiation::Undecided`]: it declares, and the closure pass over
/// the closed set re-probes it (later atoms and instances can witness
/// what declaration order could not); a true conflict nothing ever
/// witnesses still surfaces at the call site as true ambiguity (DS2010).
#[must_use]
pub(crate) enum Instantiation {
    /// The heads cannot share an instantiation: a structural verdict no
    /// later declaration undoes, so the pair never needs re-probing.
    Disjoint,
    /// The shapes can meet but no declared atom witnesses both bounds
    /// yet: re-probe over the closed set.
    Undecided,
    /// Both heads serve one grounding; the grounds are the overlap's
    /// witness, materialized once on the error path ([`ground_witness`]).
    Witness(Vec<Ground>),
}

/// One member of a probe-side grounding: an atom that exists, or an
/// application rebuilt structurally without minting — the probe never
/// writes the universe, so its products never join the candidate set of a
/// later probe and a clean check leaves the atom space untouched.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Ground {
    /// An atom as declared.
    Atom(TypeId),
    /// An application of the constructor with grounded argument member
    /// sets.
    App(TypeId, Vec<Vec<Ground>>),
}

/// Whether two heads with variables cover one common instantiation (see
/// [`Instantiation`]). The candidate set is the declared atoms alone, so
/// the scan is linear in the program's own types no matter how many pairs
/// probe.
pub(crate) fn instantiation_overlap(
    universe: &Universe,
    env: &InstanceEnv,
    head_a: Ty,
    conds_a: &[Condition<Ty>],
    head_b: Ty,
    conds_b: &[Condition<Ty>],
) -> Instantiation {
    let carries_a = head_carries_vars(universe, head_a);
    let carries_b = head_carries_vars(universe, head_b);
    if !carries_a && !carries_b {
        return Instantiation::Disjoint;
    }
    // A head serves a grounding of itself only as one pattern atom
    // (containment fails for rebuilt members, unification needs a single
    // pattern), so a pair is decidable only through single-atom heads:
    // with both heads generic each must be one, and with one generic the
    // generic side must be.
    if carries_a && carries_b {
        let ([pattern_a], [pattern_b]) = (universe.members(head_a), universe.members(head_b))
        else {
            return Instantiation::Disjoint;
        };
        if !shapes_meet(universe, *pattern_a, *pattern_b) {
            return Instantiation::Disjoint;
        }
    } else {
        let (pattern, concrete) = if carries_a {
            (universe.members(head_a), head_b)
        } else {
            (universe.members(head_b), head_a)
        };
        let [pattern] = pattern else {
            return Instantiation::Disjoint;
        };
        // A grounding of the generic head is served by the concrete one
        // only through a member it equals or unifies with, so a pattern
        // meeting no member decides the pair.
        if !universe.members(concrete).iter().any(|&member| {
            let mut shape = Vec::new();
            unify_atom(universe, &mut shape, *pattern, member)
        }) {
            return Instantiation::Disjoint;
        }
    }
    for x in universe.atom_ids().filter(|&atom| !is_var(universe, atom)) {
        for (head, conds_head, other_head, conds_other) in [
            (head_a, conds_a, head_b, conds_b),
            (head_b, conds_b, head_a, conds_a),
        ] {
            let Some(target) = ground_head(universe, head, x) else {
                continue;
            };
            if serves_ground_head(universe, env, head, conds_head, &target, MAX_DEPTH)
                && serves_ground_head(universe, env, other_head, conds_other, &target, MAX_DEPTH)
            {
                return Instantiation::Witness(target);
            }
        }
    }
    Instantiation::Undecided
}

/// Whether two pattern atoms can share one instantiation at all: a
/// variable absorbs anywhere, applications meet on constructor and arity
/// with argument sets meeting, tuples meet element-wise, and every other
/// atom meets only itself. The verdict is structural — independent of
/// which atoms and instances exist — so a pair it rejects is disjoint for
/// good.
fn shapes_meet(universe: &Universe, a: TypeId, b: TypeId) -> bool {
    if is_var(universe, a) || is_var(universe, b) {
        return true;
    }
    let (Ok(adecl), Ok(bdecl)) = (universe.atom_decl(a), universe.atom_decl(b)) else {
        return false;
    };
    match (&adecl.kind, &bdecl.kind) {
        (AtomKind::Apply(ca, args_a), AtomKind::Apply(cb, args_b)) => {
            ca == cb
                && args_a.len() == args_b.len()
                && args_a
                    .iter()
                    .zip(args_b.iter())
                    .all(|(x, y)| sets_meet(universe, *x, *y))
        },
        (AtomKind::Tuple(xs), AtomKind::Tuple(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|(x, y)| shapes_meet(universe, *x, *y))
        },
        _ => a == b,
    }
}

/// Whether two argument sets can meet: same-size sets meet member-wise,
/// and different-size sets meet only when either carries a variable — a
/// lone variable binds a whole set (`unify_set`), while member-wise
/// pairing of different sizes never completes.
fn sets_meet(universe: &Universe, x: Ty, y: Ty) -> bool {
    let (xs, ys) = (universe.members(x), universe.members(y));
    if xs.is_empty() || ys.is_empty() {
        return false;
    }
    if xs.len() == ys.len() {
        return xs
            .iter()
            .zip(ys.iter())
            .all(|(&a, &b)| shapes_meet(universe, a, b));
    }
    xs.iter().any(|&a| carries_vars(universe, a)) || ys.iter().any(|&b| carries_vars(universe, b))
}

/// Whether a head can carry variables into the probe: every member
/// decides (written unions decompose at declaration; a trait-as-type or
/// negation head names no generic parameter), so a multi-atom head
/// whose any member names a variable carries like a single-atom one —
/// leaving such a head to the set algebra would let a written head
/// with variables pass the carries-none gate.
pub(crate) fn head_carries_vars(universe: &Universe, head: Ty) -> bool {
    universe
        .members(head)
        .iter()
        .any(|&atom| carries_vars(universe, atom))
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

/// One head grounded at a candidate atom, as descriptions: every variable
/// member becomes the candidate, applications re-build structurally,
/// every other atom stays. A shape the grounding does not reach (a tuple
/// with variable elements) grounds nothing.
fn ground_head(universe: &Universe, head: Ty, x: TypeId) -> Option<Vec<Ground>> {
    universe
        .members(head)
        .iter()
        .map(|&atom| ground_atom_desc(universe, atom, x))
        .collect()
}

fn ground_atom_desc(universe: &Universe, atom: TypeId, x: TypeId) -> Option<Ground> {
    if is_var(universe, atom) {
        return Some(Ground::Atom(x));
    }
    let (ctor, args) = match &universe.atom_decl(atom).ok()?.kind {
        AtomKind::Apply(ctor, args) => (*ctor, args),
        _ => return Some(Ground::Atom(atom)),
    };
    let mut grounded = Vec::with_capacity(args.len());
    for arg in args {
        grounded.push(ground_head(universe, *arg, x)?);
    }
    Some(Ground::App(ctor, grounded))
}

/// [`serves_head`] over grounding descriptions: containment reads only
/// whole atoms, the structural match unifies the pattern with every
/// grounding under its own substitution, and the conditions must hold
/// there.
fn serves_ground_head(
    universe: &Universe,
    env: &InstanceEnv,
    head: Ty,
    conditions: &[Condition<Ty>],
    target: &[Ground],
    depth: u8,
) -> bool {
    let head_members = universe.members(head);
    if target
        .iter()
        .all(|ground| matches!(ground, Ground::Atom(atom) if head_members.contains(atom)))
    {
        return ground_conditions_hold(universe, env, conditions, &[], depth);
    }
    let [pattern] = head_members else {
        return false;
    };
    target.iter().all(|ground| {
        let mut subst = Vec::new();
        unify_ground_atom(universe, &mut subst, *pattern, ground)
            && ground_conditions_hold(universe, env, conditions, &subst, depth)
    })
}

/// [`serves`] over grounding descriptions: the probe's recursion into the
/// declared bounds a condition demands.
fn serves_ground(
    universe: &Universe,
    env: &InstanceEnv,
    id: InstanceId,
    target: &[Ground],
    depth: u8,
) -> bool {
    let (Ok(head), Ok(conditions)) = (env.head(id), env.conditions(id)) else {
        return false;
    };
    serves_ground_head(universe, env, head, conditions, target, depth)
}

/// [`trait_holds`] over grounding descriptions: some declared instance
/// serves the target, the default vocabulary alone (a named instance
/// answers no proof demand). The probe assumes nothing in scope.
fn trait_holds_ground(
    universe: &Universe,
    env: &InstanceEnv,
    trait_id: TraitId,
    target: &[Ground],
    depth: u8,
) -> bool {
    if depth == 0 || target.is_empty() {
        return false;
    }
    for id in env.of_trait(trait_id) {
        if env.name(id).is_some() {
            continue;
        }
        if serves_ground(universe, env, id, target, depth - 1) {
            return true;
        }
    }
    false
}

/// [`conditions_hold`] over grounding descriptions: each condition's type
/// is rewritten through the substitution and the bound trait must hold
/// there.
fn ground_conditions_hold(
    universe: &Universe,
    env: &InstanceEnv,
    conditions: &[Condition<Ty>],
    subst: &[(TypeId, Vec<Ground>)],
    depth: u8,
) -> bool {
    conditions.iter().all(|cond| {
        let mut solved = Vec::new();
        for &member in universe.members(cond.ty) {
            match subst.iter().find(|(var, _)| *var == member) {
                Some((_, grounds)) => solved.extend(grounds.iter().cloned()),
                None => solved.push(Ground::Atom(member)),
            }
        }
        trait_holds_ground(universe, env, cond.trait_id, &solved, depth)
    })
}

/// [`unify_atom`] against one grounding description, binding variables.
/// An existing application atom matches through its declaration, so a
/// grounding that rebuilds a shape some declaration already solved meets
/// the atom it would have minted.
fn unify_ground_atom(
    universe: &Universe,
    subst: &mut Vec<(TypeId, Vec<Ground>)>,
    pattern: TypeId,
    query: &Ground,
) -> bool {
    if is_var(universe, pattern) {
        return bind_ground(subst, pattern, vec![query.clone()]);
    }
    let Ok(decl) = universe.atom_decl(pattern) else {
        return false;
    };
    match (&decl.kind, query) {
        (AtomKind::Apply(ctor, args), Ground::App(qctor, qargs)) => {
            ctor == qctor
                && args.len() == qargs.len()
                && args
                    .iter()
                    .zip(qargs.iter())
                    .all(|(h, q)| unify_ground_set(universe, subst, *h, q))
        },
        (AtomKind::Apply(ctor, args), Ground::Atom(qatom)) => {
            let Ok(qdecl) = universe.atom_decl(*qatom) else {
                return false;
            };
            let AtomKind::Apply(qctor, qargs) = &qdecl.kind else {
                return false;
            };
            ctor == qctor
                && args.len() == qargs.len()
                && args.iter().zip(qargs.iter()).all(|(h, q)| {
                    let members: Vec<Ground> = universe
                        .members(*q)
                        .iter()
                        .map(|a| Ground::Atom(*a))
                        .collect();
                    unify_ground_set(universe, subst, *h, &members)
                })
        },
        (AtomKind::Tuple(elements), Ground::Atom(qatom)) => {
            let Ok(qdecl) = universe.atom_decl(*qatom) else {
                return false;
            };
            let AtomKind::Tuple(qelements) = &qdecl.kind else {
                return false;
            };
            elements.len() == qelements.len()
                && elements
                    .iter()
                    .zip(qelements.iter())
                    .all(|(h, q)| unify_ground_atom(universe, subst, *h, &Ground::Atom(*q)))
        },
        (_, Ground::Atom(qatom)) => pattern == *qatom,
        _ => false,
    }
}

/// [`unify_set`] against a grounding's member descriptions.
fn unify_ground_set(
    universe: &Universe,
    subst: &mut Vec<(TypeId, Vec<Ground>)>,
    pattern: Ty,
    query: &[Ground],
) -> bool {
    if query.is_empty() {
        return false;
    }
    match universe.members(pattern) {
        [only] if is_var(universe, *only) => bind_ground(subst, *only, query.to_vec()),
        [only] => query
            .iter()
            .all(|q| unify_ground_atom(universe, subst, *only, q)),
        _ => query
            .iter()
            .all(|q| matches!(q, Ground::Atom(atom) if universe.members(pattern).contains(atom))),
    }
}

/// [`bind`] over grounding descriptions: a later demand must stay inside
/// the earlier binding.
fn bind_ground(subst: &mut Vec<(TypeId, Vec<Ground>)>, var: TypeId, value: Vec<Ground>) -> bool {
    if let Some((_, existing)) = subst.iter_mut().find(|(v, _)| *v == var) {
        return value.iter().all(|g| existing.contains(g));
    }
    subst.push((var, value));
    true
}

/// Materializes a probe grounding as a type: the error path alone
/// mints — once an overlap is certain — so the probe itself never writes
/// the universe. Applications intern structurally, so the witness lands
/// on the atom a declaration solving the same shape would mint.
pub(crate) fn ground_witness(universe: &mut Universe, grounds: &[Ground]) -> Result<Ty, Error> {
    let mut members = Vec::with_capacity(grounds.len());
    for ground in grounds {
        members.push(ground_witness_atom(universe, ground)?);
    }
    Ok(universe.set_ty(&members))
}

fn ground_witness_atom(universe: &mut Universe, ground: &Ground) -> Result<TypeId, Error> {
    match ground {
        Ground::Atom(atom) => Ok(*atom),
        Ground::App(ctor, args) => {
            let mut arg_tys = Vec::with_capacity(args.len());
            for arg in args {
                arg_tys.push(ground_witness(universe, arg)?);
            }
            universe.apply_atom(*ctor, &arg_tys)
        },
    }
}
