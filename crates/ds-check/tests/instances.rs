//! Instance-environment behavior over the shapes of
//! `conformance/generics_coherent_traits.ds`: union-head decomposition,
//! disjoint-head rejection with witness, head normalization, conditional
//! instances with the structural cycle check, per-package versus link-time
//! checking, and lookup — plus one exhaustive small-universe pass mirroring
//! `algebra.rs`.

use ds_check::{Condition, Error, InstanceDecl, InstanceEnv, TraitId, TypeExpr, TypeId, Universe};
use ds_diag::ErrorCode;

struct Fixture {
    universe: Universe,
    env: InstanceEnv,
    json: TraitId,
    show: TraitId,
    eq: TraitId,
    copyable: TraitId,
    int: TypeId,
    boolean: TypeId,
    string: TypeId,
}

fn fixture() -> Fixture {
    let mut universe = Universe::new();
    let int = universe.declare_tag("int").unwrap();
    let boolean = universe.declare_tag("bool").unwrap();
    let string = universe.declare_tag("string").unwrap();
    let json = universe.declare_trait("json").unwrap();
    let show = universe.declare_trait("show").unwrap();
    let eq = universe.declare_trait("eq").unwrap();
    let copyable = universe.declare_trait("copyable").unwrap();
    Fixture {
        universe,
        env: InstanceEnv::new(),
        json,
        show,
        eq,
        copyable,
        int,
        boolean,
        string,
    }
}

impl Fixture {
    fn ty(&mut self, id: TypeId) -> ds_check::Ty {
        self.universe.atom_ty(id).unwrap()
    }

    fn declare(
        &mut self,
        trait_id: TraitId,
        head: TypeExpr,
    ) -> Result<Vec<ds_check::InstanceId>, Error> {
        self.env
            .declare(&mut self.universe, &InstanceDecl::new(trait_id, head))
    }

    fn declare_conditional(
        &mut self,
        trait_id: TraitId,
        head: TypeExpr,
        conditions: Vec<Condition<TypeExpr>>,
    ) -> Result<Vec<ds_check::InstanceId>, Error> {
        self.env.declare(
            &mut self.universe,
            &InstanceDecl::new(trait_id, head).with_conditions(conditions),
        )
    }

    fn declare_named(
        &mut self,
        trait_id: TraitId,
        head: TypeExpr,
        name: Option<&str>,
    ) -> Result<Vec<ds_check::InstanceId>, Error> {
        let name = name.map(std::sync::Arc::from);
        self.env.declare(
            &mut self.universe,
            &InstanceDecl::new(trait_id, head).with_name(name),
        )
    }
}

#[test]
fn union_heads_decompose_into_per_branch_instances() {
    let mut f = fixture();
    let union = TypeExpr::atom(f.int).union(f.boolean.into());
    let ids = f.declare(f.json, union).unwrap();
    assert_eq!(ids.len(), 2, "one instance per union branch");
    assert_eq!(f.env.len(), 2);
    assert_eq!(f.universe.render(f.env.head(ids[0]).unwrap()), "int");
    assert_eq!(f.universe.render(f.env.head(ids[1]).unwrap()), "bool");
    assert_eq!(f.env.trait_of(ids[0]).unwrap(), f.json);

    // Every branch keeps the declaration's written head, pre-decomposition.
    for &id in &ids {
        assert_eq!(f.universe.render(f.env.written(id).unwrap()), "int | bool");
    }

    // Nested unions flatten fully, still one branch per member atom.
    let nested = TypeExpr::atom(f.int)
        .union(f.boolean.into())
        .union(f.string.into());
    let ids = f.declare(f.show, nested).unwrap();
    assert_eq!(ids.len(), 3);
    assert_eq!(f.env.len(), 5);

    // Duplicate branches of one declaration overlap each other.
    let dup = TypeExpr::atom(f.int).union(f.int.into());
    let err = f.declare(f.eq, dup).unwrap_err();
    assert!(matches!(err, Error::InstanceOverlap { .. }));
    assert_eq!(f.env.len(), 5, "failed declarations insert nothing");
}

/// The named-instance vocabulary (the two-impls escape): a named impl
/// coexists with an unnamed one over the same trait and head, each
/// answering its own slot alone — the default slot keeps the unnamed
/// resolution, the named row answers only its selected call, and a
/// missing name answers nothing.
#[test]
fn named_impls_answer_only_their_slot_selections() {
    use ds_check::instance_solve::{Resolution, resolve};

    let mut f = fixture();
    let int = f.ty(f.int);
    f.declare_named(f.json, TypeExpr::atom(f.int), None)
        .unwrap();
    let named = f
        .declare_named(f.json, TypeExpr::atom(f.int), Some("fast"))
        .unwrap();

    // The default vocabulary resolves exactly the unnamed row.
    let Resolution::Unique(plain) = resolve(&f.universe, &f.env, &[], f.json, int, None) else {
        panic!("the unnamed row keeps the default slot");
    };
    assert_ne!(plain, named[0]);
    // The selection resolves the named row; a name nothing carries does
    // not.
    let Resolution::Unique(fast) = resolve(&f.universe, &f.env, &[], f.json, int, Some("fast"))
    else {
        panic!("the selection resolves the named row");
    };
    assert_eq!(fast, named[0]);
    assert!(matches!(
        resolve(&f.universe, &f.env, &[], f.json, int, Some("slow")),
        Resolution::None
    ));
    // The unselected-call evidence names the named row: the refusal's
    // note points at the selection the vocabulary offers.
    assert_eq!(
        ds_check::instance_solve::named_servers(&f.universe, &f.env, &[], f.json, int),
        vec![named[0]]
    );
}

/// Coherence runs per slot: two named impls of one trait over one head
/// conflict only when they share the name — distinct names coexist as
/// the escape for two legitimate impls, and the unnamed row conflicts
/// with neither named one.
#[test]
fn coherence_and_link_elections_run_per_vocabulary_slot() {
    let mut f = fixture();
    let head = TypeExpr::atom(f.int);
    f.declare_named(f.show, head.clone(), Some("fast")).unwrap();
    f.declare_named(f.show, head.clone(), Some("slow")).unwrap();
    // An unnamed impl coexists beside both named ones.
    f.declare_named(f.show, head.clone(), None).unwrap();
    assert_eq!(f.env.len(), 3);

    // The same slot twice over the same head overlaps, with the witness.
    let err = f.declare_named(f.show, head, Some("fast")).unwrap_err();
    assert!(matches!(err, Error::InstanceOverlap { .. }));
    assert_eq!(f.env.len(), 3, "the failed declaration inserts nothing");
}

#[test]
fn corpus_disjoint_head_rejection_names_witness() {
    // conformance/generics_coherent_traits.ds impl-json-int-bool: the union
    // head must be rejected against impl-json-int and impl-json-bool with
    // witness `int`.
    let mut f = fixture();
    f.declare(f.json, TypeExpr::atom(f.int)).unwrap();
    f.declare(f.json, TypeExpr::atom(f.boolean)).unwrap();
    let union = TypeExpr::atom(f.int).union(f.boolean.into());
    let err = f.declare(f.json, union).unwrap_err();
    let Error::InstanceOverlap {
        head_a,
        head_b,
        witness,
        ..
    } = &err
    else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    // Decomposition precedes the check, so the named heads are per-branch.
    assert_eq!(f.universe.render(*head_a), "int");
    assert_eq!(f.universe.render(*head_b), "int");
    assert_eq!(f.universe.render_witness(witness), "int");
    assert_eq!(err.code(), ErrorCode::InstanceOverlap);
    assert_eq!(f.env.len(), 2, "failed declarations insert nothing");
}

#[test]
fn disjointness_is_per_trait_and_duplicates_are_overlaps() {
    let mut f = fixture();
    // Different traits may share a head: no orphan rule, no cross-trait rule.
    f.declare(f.json, TypeExpr::atom(f.int)).unwrap();
    f.declare(f.show, TypeExpr::atom(f.int)).unwrap();
    assert_eq!(f.env.len(), 2);

    // Re-declaring the same head for the same trait is an overlap.
    let err = f.declare(f.json, TypeExpr::atom(f.int)).unwrap_err();
    let Error::InstanceOverlap { witness, .. } = &err else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(f.universe.render_witness(witness), "int");
    // Atomic rollback across branches: the bool branch passes, int fails,
    // nothing is inserted.
    let union = TypeExpr::atom(f.boolean).union(f.int.into());
    assert!(matches!(
        f.declare(f.json, union),
        Err(Error::InstanceOverlap { .. })
    ));
    assert_eq!(f.env.len(), 2);
}

#[test]
fn unconditional_instances_record_trait_members_conditionals_do_not() {
    let mut f = fixture();
    f.declare(f.json, TypeExpr::atom(f.int)).unwrap();
    f.declare(f.json, TypeExpr::atom(f.boolean)).unwrap();
    // A trait used as a type solves over unconditional members only.
    let (int_ty, bool_ty, string_ty) = (f.ty(f.int), f.ty(f.boolean), f.ty(f.string));
    let json_ty = f.universe.solve(&TypeExpr::trait_ref(f.json)).unwrap();
    assert!(f.universe.contains(json_ty, int_ty));
    assert!(f.universe.contains(json_ty, bool_ty));
    assert!(!f.universe.contains(json_ty, string_ty));

    let cond = Condition::new(f.eq, TypeExpr::atom(f.int));
    f.declare_conditional(f.show, TypeExpr::atom(f.string), vec![cond])
        .unwrap();
    let show_ty = f.universe.solve(&TypeExpr::trait_ref(f.show)).unwrap();
    assert!(
        !f.universe.contains(show_ty, string_ty),
        "conditional instances add no members"
    );
    let conds = f.env.conditions(ds_check::InstanceId::from_raw(2)).unwrap();
    assert_eq!(conds.len(), 1);
    assert_eq!(conds[0].trait_id, f.eq);
    assert_eq!(f.universe.render(conds[0].ty), "int");
}

#[test]
fn per_branch_condition_solving_keeps_each_branchs_own_conditions() {
    // `show for int | list[t] if show[t], eq[int]` decomposes with the
    // `int` branch naming no bound variable — it solves the variable
    // condition unconditionally — while the `list[t]` branch keeps it;
    // a concrete condition names no variable and stays for every branch,
    // and the declaration's requirement set is the union over branches.
    let mut f = fixture();
    let list = f.universe.declare_struct("list").unwrap();
    let t = f.universe.declare_tag("$t#1").unwrap();
    let t_ty = f.universe.atom_ty(t).unwrap();
    let list_t = f.universe.apply_atom(list, &[t_ty]).unwrap();
    let head = TypeExpr::atom(f.int).union(list_t.into());
    let ids = f
        .declare_conditional(f.show, head, vec![
            Condition::new(f.show, TypeExpr::atom(t)),
            Condition::new(f.eq, TypeExpr::atom(f.int)),
        ])
        .unwrap();
    assert_eq!(ids.len(), 2);
    let conds = f.env.conditions(ids[0]).unwrap();
    assert_eq!(
        conds.len(),
        1,
        "the int branch keeps the concrete condition alone"
    );
    assert_eq!(conds[0].trait_id, f.eq);
    assert_eq!(f.universe.render(conds[0].ty), "int");
    let conds = f.env.conditions(ids[1]).unwrap();
    assert_eq!(conds.len(), 2, "the list[t] branch keeps both conditions");
    assert_eq!(conds[0].trait_id, f.show);
    assert_eq!(f.universe.render(conds[0].ty), "$t#1");
    assert_eq!(conds[1].trait_id, f.eq);
    // The declaration's full condition set is non-empty, so no branch
    // records trait members: a conditional declaration's unconditional
    // branch adds none either (the trait-as-type set stays conservative).
    let show_ty = f.universe.solve(&TypeExpr::trait_ref(f.show)).unwrap();
    let int_ty = f.ty(f.int);
    assert!(!f.universe.contains(show_ty, int_ty));
}

#[test]
fn per_branch_conditions_solve_at_call_sites() {
    // Over `show for int | list[t] if show[t]` plus `show for string`: the
    // int branch serves an int receiver with no bound to prove, the list
    // branch serves `list[string]` once its own condition holds, and a
    // `list[bool]` receiver misses the branch's own condition with the
    // element named — a branch naming no bound variable never mints a
    // raw-variable miss.
    let mut f = fixture();
    let list = f.universe.declare_struct("list").unwrap();
    let t = f.universe.declare_tag("$t#1").unwrap();
    let t_ty = f.universe.atom_ty(t).unwrap();
    let list_t = f.universe.apply_atom(list, &[t_ty]).unwrap();
    let head = TypeExpr::atom(f.int).union(list_t.into());
    let ids = f
        .declare_conditional(f.show, head, vec![Condition::new(
            f.show,
            TypeExpr::atom(t),
        )])
        .unwrap();
    f.declare(f.show, TypeExpr::atom(f.string)).unwrap();
    let int_ty = f.ty(f.int);
    let string_ty = f.ty(f.string);
    let bool_ty = f.ty(f.boolean);
    let list_string = f.universe.apply_atom(list, &[string_ty]).unwrap();
    let list_bool = f.universe.apply_atom(list, &[bool_ty]).unwrap();
    let list_string_ty = f.universe.atom_ty(list_string).unwrap();
    let list_bool_ty = f.universe.atom_ty(list_bool).unwrap();
    let list_int = f.universe.apply_atom(list, &[int_ty]).unwrap();
    let list_int_ty = f.universe.atom_ty(list_int).unwrap();
    assert!(matches!(
        ds_check::instance_solve::resolve(&f.universe, &f.env, &[], f.show, int_ty, None),
        ds_check::instance_solve::Resolution::Unique(id) if id == ids[0]
    ));
    // The unconditioned branch's own instance satisfies the list branch's
    // element condition, so `list[int]` resolves through the same impl
    // without any instance beyond the declaration's own branches.
    assert!(matches!(
        ds_check::instance_solve::resolve(&f.universe, &f.env, &[], f.show, list_int_ty, None),
        ds_check::instance_solve::Resolution::Unique(id) if id == ids[1]
    ));
    assert!(matches!(
        ds_check::instance_solve::resolve(
            &f.universe,
            &f.env,
            &[],
            f.show,
            list_string_ty,
            None
        ),
        ds_check::instance_solve::Resolution::Unique(id) if id == ids[1]
    ));
    match ds_check::instance_solve::resolve(&f.universe, &f.env, &[], f.show, list_bool_ty, None) {
        ds_check::instance_solve::Resolution::BoundUnsat { bound, target, .. } => {
            assert_eq!(bound, f.show);
            let miss_ty = f.universe.set_ty(&target);
            assert_eq!(
                f.universe.render(miss_ty),
                "bool",
                "the miss names the substituted element, never a raw variable"
            );
        },
        other => panic!("the list branch's own condition misses: {other:?}"),
    }
}

#[test]
fn non_atom_heads_normalize_or_reject() {
    let mut f = fixture();
    // (int | bool) - bool is not a written union: one branch, normalized to
    // the singleton int by solving.
    let diff = TypeExpr::atom(f.int)
        .union(f.boolean.into())
        .diff(f.boolean.into());
    let ids = f.declare(f.json, diff).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(f.universe.render(f.env.head(ids[0]).unwrap()), "int");

    // The normalized head feeds coherence: json for int now overlaps it.
    let err = f.declare(f.json, TypeExpr::atom(f.int)).unwrap_err();
    let Error::InstanceOverlap { witness, .. } = &err else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(f.universe.render_witness(witness), "int");

    // An intersection of distinct atoms solves to empty and is rejected.
    let inter = TypeExpr::atom(f.int).inter(f.boolean.into());
    let err = f.declare(f.show, inter).unwrap_err();
    assert!(matches!(err, Error::EmptyInstanceHead(_)));
    assert_eq!(err.code(), ErrorCode::EmptyInstanceHead);

    // A negated-constraint head stays one multi-atom head and participates
    // in coherence as a set.
    let draw = f.universe.declare_trait("draw").unwrap();
    f.declare(f.copyable, TypeExpr::atom(f.int)).unwrap();
    let not_copyable = TypeExpr::not(f.copyable);
    let ids = f.declare(draw, not_copyable).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        f.universe.render(f.env.head(ids[0]).unwrap()),
        "none | type | bool | string"
    );
    let err = f.declare(draw, TypeExpr::atom(f.boolean)).unwrap_err();
    let Error::InstanceOverlap { witness, .. } = &err else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(f.universe.render_witness(witness), "bool");
}

/// The canonical empty spelling declares a vacuous instance: a head no
/// value inhabits (a zero-case enum's name) hosts its trait, stays
/// disjoint from every other head — including another empty head —
/// records no trait members, and is never selected for an inhabited
/// query. The written intersection that solves empty keeps its refusal
/// (the pin above): only the canonical spelling hosts.
#[test]
fn canonical_empty_head_hosts_a_vacuous_instance() {
    let mut f = fixture();
    let ids = f.declare(f.show, TypeExpr::Empty).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(f.universe.render(f.env.head(ids[0]).unwrap()), "never");
    assert_eq!(f.universe.render(f.env.written(ids[0]).unwrap()), "never");

    // Disjoint from every inhabited head, and from a second vacuous
    // instance of the same trait: both coexist and the closure pass
    // proves the whole set coherent.
    let int_ids = f.declare(f.show, TypeExpr::atom(f.int)).unwrap();
    f.declare(f.show, TypeExpr::Empty).unwrap();
    f.env.check_all(&mut f.universe).unwrap();

    // An inhabited query never selects the vacuous heads.
    let int = f.ty(f.int);
    assert_eq!(
        f.env.resolve(&f.universe, f.show, int),
        int_ids,
        "the empty heads never contain an inhabited query"
    );
    let empty = f.universe.empty_ty();
    let solved = ds_check::instance_solve::resolve(&f.universe, &f.env, &[], f.show, empty, None);
    assert!(
        !solved.satisfied(),
        "an empty target matches nothing at a call site"
    );

    // The vacuous instance records no trait members: `show` as a type
    // keeps exactly the inhabited heads' atoms.
    let members = f.universe.solve(&TypeExpr::trait_ref(f.show)).unwrap();
    assert_eq!(f.universe.render(members), "int");
}

#[test]
fn direct_self_condition_rejected_with_rollback() {
    let mut f = fixture();
    let json_t = f.universe.declare_tag("json_t").unwrap();
    // show for json_t if show[json_t]: the condition resolves to the
    // instance itself.
    let cond = Condition::new(f.show, TypeExpr::atom(json_t));
    let err = f
        .declare_conditional(f.show, TypeExpr::atom(json_t), vec![cond])
        .unwrap_err();
    let Error::ConditionCycle { cycle } = &err else {
        panic!("expected ConditionCycle, got {err:?}");
    };
    assert_eq!(cycle, &[
        ds_check::InstanceId::from_raw(0),
        ds_check::InstanceId::from_raw(0)
    ]);
    assert_eq!(err.code(), ErrorCode::ConditionCycle);
    assert_eq!(f.env.len(), 0, "rollback removes the instance");
    // The environment stays usable after rollback.
    f.declare(f.show, TypeExpr::atom(json_t)).unwrap();
    assert_eq!(f.env.len(), 1);
}

#[test]
fn mutual_two_instance_cycle_rejected() {
    let mut f = fixture();
    let a = f.universe.declare_tag("a").unwrap();
    let b = f.universe.declare_tag("b").unwrap();
    f.declare_conditional(f.show, TypeExpr::atom(a), vec![Condition::new(
        f.eq,
        TypeExpr::atom(b),
    )])
    .unwrap();
    let err = f
        .declare_conditional(f.eq, TypeExpr::atom(b), vec![Condition::new(
            f.show,
            TypeExpr::atom(a),
        )])
        .unwrap_err();
    let Error::ConditionCycle { cycle } = &err else {
        panic!("expected ConditionCycle, got {err:?}");
    };
    assert_eq!(cycle, &[
        ds_check::InstanceId::from_raw(1),
        ds_check::InstanceId::from_raw(0),
        ds_check::InstanceId::from_raw(1)
    ]);
    assert_eq!(f.env.len(), 1, "only the first instance stays");
}

#[test]
fn transitive_three_instance_cycle_rejected() {
    let mut f = fixture();
    let a = f.universe.declare_tag("a").unwrap();
    let b = f.universe.declare_tag("b").unwrap();
    let c = f.universe.declare_tag("c").unwrap();
    let draw = f.universe.declare_trait("draw").unwrap();
    f.declare_conditional(f.show, TypeExpr::atom(a), vec![Condition::new(
        draw,
        TypeExpr::atom(b),
    )])
    .unwrap();
    f.declare_conditional(draw, TypeExpr::atom(b), vec![Condition::new(
        f.eq,
        TypeExpr::atom(c),
    )])
    .unwrap();
    let err = f
        .declare_conditional(f.eq, TypeExpr::atom(c), vec![Condition::new(
            f.show,
            TypeExpr::atom(a),
        )])
        .unwrap_err();
    let Error::ConditionCycle { cycle } = &err else {
        panic!("expected ConditionCycle, got {err:?}");
    };
    assert_eq!(cycle, &[
        ds_check::InstanceId::from_raw(2),
        ds_check::InstanceId::from_raw(0),
        ds_check::InstanceId::from_raw(1),
        ds_check::InstanceId::from_raw(2)
    ]);
}

#[test]
fn benign_chains_are_not_rejected() {
    let mut f = fixture();
    let a = f.universe.declare_tag("a").unwrap();
    let b = f.universe.declare_tag("b").unwrap();
    let c = f.universe.declare_tag("c").unwrap();
    let draw = f.universe.declare_trait("draw").unwrap();

    // A condition satisfied by an unconditional instance is a chain, not a
    // cycle: json[int] resolves to the json-for-int instance, which has no
    // conditions.
    f.declare(f.json, TypeExpr::atom(f.int)).unwrap();
    f.declare_conditional(f.show, TypeExpr::atom(a), vec![Condition::new(
        f.json,
        TypeExpr::atom(f.int),
    )])
    .unwrap();

    // A longer chain of conditionals ending at an unconditional instance.
    f.declare(draw, TypeExpr::atom(c)).unwrap();
    f.declare_conditional(f.eq, TypeExpr::atom(b), vec![Condition::new(
        draw,
        TypeExpr::atom(c),
    )])
    .unwrap();
    f.declare_conditional(f.show, TypeExpr::atom(b), vec![Condition::new(
        f.eq,
        TypeExpr::atom(b),
    )])
    .unwrap();

    f.env.check_all(&mut f.universe).unwrap();
    assert_eq!(f.env.len(), 5);
}

#[test]
fn link_time_check_catches_cross_batch_overlap() {
    // Two packages, each coherent alone; the overlap is only visible in the
    // merged environment.
    let mut f = fixture();
    let mut package_a = InstanceEnv::new();
    package_a
        .declare(
            &mut f.universe,
            &InstanceDecl::new(f.json, TypeExpr::atom(f.int)),
        )
        .unwrap();
    package_a.check_all(&mut f.universe).unwrap();

    let mut package_b = InstanceEnv::new();
    let union = TypeExpr::atom(f.int).union(f.boolean.into());
    package_b
        .declare(&mut f.universe, &InstanceDecl::new(f.json, union))
        .unwrap();
    package_b.check_all(&mut f.universe).unwrap();

    package_a.merge(package_b);
    assert_eq!(package_a.len(), 3);
    let err = package_a.check_all(&mut f.universe).unwrap_err();
    let Error::InstanceOverlap {
        head_a,
        head_b,
        witness,
        ..
    } = &err
    else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(f.universe.render(*head_a), "int");
    assert_eq!(f.universe.render(*head_b), "int");
    assert_eq!(f.universe.render_witness(witness), "int");
}

#[test]
fn link_time_check_re_solves_heads_after_membership_growth() {
    // Stored heads are declaration-time snapshots: `json for copyable`
    // (trait as type) solves over copyable's members at declaration ({int}),
    // and the later `copyable for bool` widens the real head to {int, bool}.
    // The second batch's `json for bool` passes the incremental check (the
    // snapshot still looks disjoint); the link pass must re-solve against
    // the grown universe to catch the overlap.
    let mut f = fixture();
    f.declare(f.copyable, TypeExpr::atom(f.int)).unwrap();
    f.declare(f.json, TypeExpr::trait_ref(f.copyable)).unwrap();
    // Membership growth between the two declaration batches.
    f.declare(f.copyable, TypeExpr::atom(f.boolean)).unwrap();
    // Accepted at declaration: the stored json head snapshot is still {int}.
    f.declare(f.json, TypeExpr::atom(f.boolean)).unwrap();
    assert_eq!(f.env.len(), 4, "both batches declare cleanly");
    let err = f.env.check_all(&mut f.universe).unwrap_err();
    let Error::InstanceOverlap {
        head_a,
        head_b,
        witness,
        ..
    } = &err
    else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(f.universe.render(*head_a), "int | bool");
    assert_eq!(f.universe.render(*head_b), "bool");
    assert_eq!(f.universe.render_witness(witness), "bool");
}

#[test]
fn link_time_check_catches_cross_batch_cycle() {
    // The self-satisfying pair only closes across the merge.
    let mut f = fixture();
    let a = f.universe.declare_tag("a").unwrap();
    let b = f.universe.declare_tag("b").unwrap();
    let draw = f.universe.declare_trait("draw").unwrap();

    let mut package_a = InstanceEnv::new();
    package_a
        .declare(
            &mut f.universe,
            &InstanceDecl::new(f.show, TypeExpr::atom(a))
                .with_conditions(vec![Condition::new(draw, TypeExpr::atom(b))]),
        )
        .unwrap();
    package_a.check_all(&mut f.universe).unwrap();

    let mut package_b = InstanceEnv::new();
    package_b
        .declare(
            &mut f.universe,
            &InstanceDecl::new(draw, TypeExpr::atom(b))
                .with_conditions(vec![Condition::new(f.show, TypeExpr::atom(a))]),
        )
        .unwrap();
    package_b.check_all(&mut f.universe).unwrap();

    package_a.merge(package_b);
    let err = package_a.check_all(&mut f.universe).unwrap_err();
    let Error::ConditionCycle { cycle } = &err else {
        panic!("expected ConditionCycle, got {err:?}");
    };
    assert_eq!(cycle, &[
        ds_check::InstanceId::from_raw(0),
        ds_check::InstanceId::from_raw(1),
        ds_check::InstanceId::from_raw(0)
    ]);
}

#[test]
fn generic_head_overlaps_concrete_head_at_declaration() {
    // `json for list[int]` plus `json for list[t] if json[t]`: the heads
    // read disjoint to the set algebra (a generic head is one opaque
    // atom), but the probe grounds the generic head at `int` and both
    // serve `list[int]` once `json for int` exists — a declaration-time
    // overlap with the shared instantiation as the witness.
    let mut f = fixture();
    f.declare(f.json, TypeExpr::atom(f.int)).unwrap();
    let list = f.universe.declare_struct("list").unwrap();
    let int_ty = f.universe.atom_ty(f.int).unwrap();
    let list_int = f.universe.apply_atom(list, &[int_ty]).unwrap();
    f.declare(f.json, TypeExpr::atom(list_int)).unwrap();
    let t = f.universe.declare_tag("$t#1").unwrap();
    let t_ty = f.universe.atom_ty(t).unwrap();
    let list_t = f.universe.apply_atom(list, &[t_ty]).unwrap();
    let err = f
        .declare_conditional(f.json, TypeExpr::atom(list_t), vec![Condition::new(
            f.json,
            TypeExpr::atom(t),
        )])
        .unwrap_err();
    let Error::InstanceOverlap { witness, .. } = &err else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(f.universe.render_witness(witness), "list[int]");
}

#[test]
fn generic_heads_of_distinct_constructors_stay_disjoint() {
    // `list[t]` and `pair[t]` never share an instantiation, bounds or
    // not: the probe grounds both and no candidate type is served twice.
    let mut f = fixture();
    let list = f.universe.declare_struct("list").unwrap();
    let pair = f.universe.declare_struct("pair").unwrap();
    let t = f.universe.declare_tag("$t#1").unwrap();
    let t_ty = f.universe.atom_ty(t).unwrap();
    let list_t = f.universe.apply_atom(list, &[t_ty]).unwrap();
    let pair_t = f.universe.apply_atom(pair, &[t_ty]).unwrap();
    f.declare(f.json, TypeExpr::atom(list_t)).unwrap();
    f.declare(f.json, TypeExpr::atom(pair_t)).unwrap();
    assert_eq!(f.env.len(), 2, "distinct constructors both declare");
}

#[test]
fn generic_pair_unprovable_at_declaration_closes_at_link_time() {
    // Both generic impls declare while no atom proves their bounds; the
    // later `json for int` makes both serve `list[int]`, and the link
    // pass over the closed environment catches the pair the declaration
    // order could not.
    let mut f = fixture();
    let list = f.universe.declare_struct("list").unwrap();
    let t = f.universe.declare_tag("$t#1").unwrap();
    let t_ty = f.universe.atom_ty(t).unwrap();
    let list_t = f.universe.apply_atom(list, &[t_ty]).unwrap();
    let u = f.universe.declare_tag("$u#2").unwrap();
    let u_ty = f.universe.atom_ty(u).unwrap();
    let list_u = f.universe.apply_atom(list, &[u_ty]).unwrap();
    f.declare_conditional(f.json, TypeExpr::atom(list_t), vec![Condition::new(
        f.json,
        TypeExpr::atom(t),
    )])
    .unwrap();
    f.declare_conditional(f.json, TypeExpr::atom(list_u), vec![Condition::new(
        f.json,
        TypeExpr::atom(u),
    )])
    .unwrap();
    f.declare(f.json, TypeExpr::atom(f.int)).unwrap();
    let err = f.env.check_all(&mut f.universe).unwrap_err();
    let Error::InstanceOverlap { witness, .. } = &err else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(f.universe.render_witness(witness), "list[int]");
}

#[test]
fn many_distinct_generic_heads_declare_and_close() {
    // Any number of same-trait generic impls over distinct constructors:
    // every pair is structurally disjoint, so declaration decides each
    // pair once and the closure pass re-probes none. The probe once
    // minted its groundings into the shared universe, and its own
    // products became later probes' candidates — five impls never
    // returned.
    for count in [5_usize, 50] {
        let mut f = fixture();
        for i in 0..count {
            let ctor = f.universe.declare_struct(&format!("c{i}")).unwrap();
            let var = f.universe.declare_tag(&format!("$t#{i}")).unwrap();
            let var_ty = f.universe.atom_ty(var).unwrap();
            let head = f.universe.apply_atom(ctor, &[var_ty]).unwrap();
            f.declare(f.json, TypeExpr::atom(head)).unwrap();
        }
        assert_eq!(f.env.len(), count);
        f.env.check_all(&mut f.universe).unwrap();
    }
}

#[test]
fn probes_write_no_atoms_into_the_universe() {
    // The coherence probe is a pure query: an open pair whose scan finds
    // no witness still runs the full candidate scan at closure, and the
    // atom space is untouched after it — a clean check mints nothing, so
    // the atom order stays independent of probe order.
    let mut f = fixture();
    let list = f.universe.declare_struct("list").unwrap();
    let t = f.universe.declare_tag("$t#1").unwrap();
    let t_ty = f.universe.atom_ty(t).unwrap();
    let list_t = f.universe.apply_atom(list, &[t_ty]).unwrap();
    let u = f.universe.declare_tag("$u#2").unwrap();
    let u_ty = f.universe.atom_ty(u).unwrap();
    let list_u = f.universe.apply_atom(list, &[u_ty]).unwrap();
    f.declare_conditional(f.json, TypeExpr::atom(list_t), vec![Condition::new(
        f.eq,
        TypeExpr::atom(t),
    )])
    .unwrap();
    f.declare_conditional(f.json, TypeExpr::atom(list_u), vec![Condition::new(
        f.eq,
        TypeExpr::atom(u),
    )])
    .unwrap();
    let before = f.universe.atom_ids().count();
    f.env.check_all(&mut f.universe).unwrap();
    assert_eq!(
        f.universe.atom_ids().count(),
        before,
        "the probe mints nothing into the shared universe"
    );
}

#[test]
fn resolve_is_exact_containment_and_lookup_only() {
    let mut f = fixture();
    let int_ty = f.ty(f.int);
    let bool_ty = f.ty(f.boolean);

    let unconditional = f.declare(f.eq, TypeExpr::atom(f.int)).unwrap()[0];
    // The conditional's own condition never filters lookup.
    let conditional = f
        .declare_conditional(f.show, TypeExpr::atom(f.int), vec![Condition::new(
            f.eq,
            TypeExpr::atom(f.int),
        )])
        .unwrap()[0];

    assert_eq!(f.env.resolve(&f.universe, f.eq, int_ty), vec![
        unconditional
    ]);
    assert_eq!(f.env.resolve(&f.universe, f.show, int_ty), vec![
        conditional
    ]);
    assert_eq!(f.env.resolve(&f.universe, f.show, bool_ty), Vec::new());
    // A query wider than any head matches nothing: containment is exact.
    let both = f.universe.union(int_ty, bool_ty);
    assert_eq!(f.env.resolve(&f.universe, f.show, both), Vec::new());
    // Unknown traits simply have no instances.
    assert_eq!(
        f.env
            .resolve(&f.universe, TraitId::from_raw(99), int_ty)
            .len(),
        0
    );

    assert_eq!(
        f.env.resolve_unique(&f.universe, f.show, int_ty).unwrap(),
        Some(conditional)
    );
    assert_eq!(
        f.env.resolve_unique(&f.universe, f.show, bool_ty).unwrap(),
        None
    );
}

#[test]
fn declare_and_accessors_reject_bad_ids() {
    let mut f = fixture();
    assert!(matches!(
        f.declare(TraitId::from_raw(99), TypeExpr::atom(f.int)),
        Err(Error::UnknownTrait(_))
    ));
    assert!(matches!(
        f.declare(f.json, TypeExpr::atom(TypeId::from_raw(99))),
        Err(Error::UnknownAtom(_))
    ));
    let cond = Condition::new(TraitId::from_raw(99), TypeExpr::atom(f.int));
    assert!(matches!(
        f.declare_conditional(f.json, TypeExpr::atom(f.int), vec![cond]),
        Err(Error::UnknownTrait(_))
    ));
    let bad = ds_check::InstanceId::from_raw(0);
    assert!(matches!(
        f.env.head(bad),
        Err(Error::UnknownInstance(bad_id)) if bad_id == bad
    ));
    assert_eq!(
        f.env.head(bad).unwrap_err().code(),
        ErrorCode::UnresolvedName
    );
}

/// Exhaustive pass over a 5-atom universe (32 solved types), mirroring the
/// `algebra.rs` oracle style: with coherent heads, lookup is unique for
/// every non-empty query, and same-trait heads stay pairwise disjoint.
#[test]
fn exhaustive_lookup_unique_for_non_empty_queries() {
    let mut u = Universe::new();
    // [none, type, a, b, c, d]: every universe carries the builtin `type`
    // atom beside none, so the model accounts for it.
    let atoms: Vec<TypeId> = [TypeId::NONE, TypeId::TYPE]
        .into_iter()
        .chain(
            ["a", "b", "c", "d"]
                .iter()
                .map(|name| u.declare_tag(name).unwrap()),
        )
        .collect();
    assert_eq!(atoms.len(), 6);
    let marker = u.declare_trait("marker").unwrap();
    let ord = u.declare_trait("ord").unwrap();

    let mut env = InstanceEnv::new();
    let marker_ids = env
        .declare(&mut u, &InstanceDecl::new(marker, TypeExpr::atom(atoms[2])))
        .unwrap();
    assert_eq!(marker_ids.len(), 1);
    let marker_ids_b = env
        .declare(&mut u, &InstanceDecl::new(marker, TypeExpr::atom(atoms[3])))
        .unwrap();
    assert_eq!(marker_ids_b.len(), 1);

    // ord heads: the singleton a, and the multi-atom complement of marker.
    env.declare(&mut u, &InstanceDecl::new(ord, TypeExpr::atom(atoms[2])))
        .unwrap();
    env.declare(&mut u, &InstanceDecl::new(ord, TypeExpr::not(marker)))
        .unwrap();

    let singletons: Vec<_> = atoms.iter().map(|id| u.atom_ty(*id).unwrap()).collect();
    let mut all = vec![u.empty_ty()];
    for bits in 1u64..=63 {
        let mut t = all[0];
        for (i, singleton) in singletons.iter().enumerate() {
            if bits & (1 << i) != 0 {
                t = u.union(t, *singleton);
            }
        }
        all.push(t);
    }
    assert_eq!(all.len(), 64);

    let id_of = |i: usize| ds_check::InstanceId::from_raw(u32::try_from(i).unwrap());
    let bits_of = |t: ds_check::Ty| -> u64 {
        u.members(t)
            .iter()
            .map(|id| 1 << atoms.iter().position(|a| a == id).unwrap())
            .fold(0, |acc, b| acc | b)
    };
    let head_bits: Vec<(TraitId, u64)> = (0..env.len())
        .map(|i| {
            let id = id_of(i);
            (env.trait_of(id).unwrap(), bits_of(env.head(id).unwrap()))
        })
        .collect();

    // Same-trait heads stay pairwise disjoint (declared in this order:
    // marker a, marker b, ord a, ord ~marker over {none, type, a, b, c, d}).
    let full = (1u64 << atoms.len()) - 1;
    let expected_heads = vec![
        (marker, 1 << 2),
        (marker, 1 << 3),
        (ord, 1 << 2),
        (ord, full & !((1 << 2) | (1 << 3))),
    ];
    assert_eq!(head_bits, expected_heads);
    for (i, &(ta, ba)) in head_bits.iter().enumerate() {
        for &(tb, bb) in head_bits.iter().skip(i + 1) {
            if ta == tb {
                assert_eq!(ba & bb, 0, "same-trait heads must be disjoint");
            }
        }
    }

    // Lookup over every solved type: candidates are exactly the same-trait
    // heads containing the query; non-empty queries are unambiguous.
    for (index, &query) in all.iter().enumerate() {
        for trait_id in [marker, ord] {
            let expected: Vec<_> = (0..env.len())
                .filter(|&i| {
                    let id = id_of(i);
                    env.trait_of(id).unwrap() == trait_id
                        && bits_of(env.head(id).unwrap()) & index as u64 == index as u64
                })
                .map(id_of)
                .collect();
            let got = env.resolve(&u, trait_id, query);
            assert_eq!(got, expected, "query bits {index} for trait {trait_id:?}");
            let unique = env.resolve_unique(&u, trait_id, query);
            match got.len() {
                0 => assert_eq!(unique.unwrap(), None),
                1 => assert_eq!(unique.unwrap(), got.first().copied()),
                _ => {
                    assert_eq!(index, 0, "ambiguity is only possible for the empty query");
                    let Err(Error::AmbiguousResolution {
                        trait_id: t,
                        candidates,
                        ..
                    }) = unique
                    else {
                        panic!("expected AmbiguousResolution");
                    };
                    assert_eq!(t, trait_id);
                    assert_eq!(candidates, got);
                },
            }
        }
    }
}

/// The per-trait row index keeps the solve's answers and the scan's row
/// order: rows a checked-ambient artifact served join before the package's
/// own declarations, every list stays ascending in id order across a link
/// merge, and a solve over the indexed table answers exactly what the
/// full-table scan did (two same-trait instances disambiguated by the
/// receiver's shape).
#[test]
fn indexed_solve_answers_and_row_order_match_the_scan() {
    use ds_check::{
        InstanceId, ServedInstance,
        instance_solve::{Resolution, resolve},
    };

    let mut f = fixture();
    let int_head = f.universe.atom_ty(f.int).unwrap();
    let string_head = f.universe.atom_ty(f.string).unwrap();
    f.env.serve(vec![
        ServedInstance {
            trait_id: f.json,
            name: None,
            head_expr: TypeExpr::atom(f.int),
            head: int_head,
            written: int_head,
            condition_exprs: Vec::new(),
            conditions: Vec::new(),
        },
        ServedInstance {
            trait_id: f.json,
            name: None,
            head_expr: TypeExpr::atom(f.string),
            head: string_head,
            written: string_head,
            condition_exprs: Vec::new(),
            conditions: Vec::new(),
        },
    ]);
    let fresh = f.declare(f.json, TypeExpr::atom(f.boolean)).unwrap();
    assert_eq!(fresh.len(), 1);

    // The index names the served rows first, then the package's own.
    assert_eq!(f.env.of_trait(f.json), &[
        InstanceId::from_raw(0),
        InstanceId::from_raw(1),
        fresh[0]
    ]);
    assert_eq!(f.env.of_trait(f.show), &[] as &[InstanceId]);

    // A solve over the indexed table picks by receiver shape, exactly as
    // the full-table scan did.
    for (atom, want) in [
        (f.int, InstanceId::from_raw(0)),
        (f.string, InstanceId::from_raw(1)),
        (f.boolean, fresh[0]),
    ] {
        let ty = f.universe.atom_ty(atom).unwrap();
        let Resolution::Unique(id) = resolve(&f.universe, &f.env, &[], f.json, ty, None) else {
            panic!("the receiver shape must solve uniquely");
        };
        assert_eq!(id, want);
    }
    assert!(matches!(
        resolve(&f.universe, &f.env, &[], f.show, int_head, None),
        Resolution::None
    ));

    // A link merge renumbers the appended rows; the index rebuilds with
    // them and both traits still answer.
    let mut other = InstanceEnv::new();
    let show_int = other
        .declare(
            &mut f.universe,
            &InstanceDecl::new(f.show, TypeExpr::atom(f.int)),
        )
        .unwrap();
    assert_eq!(other.of_trait(f.show), show_int.as_slice());
    f.env.merge(other);
    assert_eq!(f.env.of_trait(f.json), &[
        InstanceId::from_raw(0),
        InstanceId::from_raw(1),
        fresh[0]
    ]);
    // The appended rows renumber at the merge, so the show row lands at 3.
    let merged_show = InstanceId::from_raw(3);
    assert_eq!(f.env.of_trait(f.show), &[merged_show]);
    let Resolution::Unique(id) = resolve(&f.universe, &f.env, &[], f.show, int_head, None) else {
        panic!("the merged row must serve the receiver");
    };
    assert_eq!(id, merged_show);
}
