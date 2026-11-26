//! Anonymous-tuple modeling: structural tuple atoms, product
//! distribution at solving, tuple witnesses, coherence over tuple heads, and
//! an exhaustive bitset-oracle pass over a tuple-bearing universe mirroring
//! `algebra.rs`. Containers of the corpus (`list[json]`,
//! `map[string, json]` in `conformance/types_hybrid.ds`) are not tuples and
//! stay unmodeled.

use ds_check::{
    AtomKind, Error, Field, InstanceDecl, InstanceEnv, TraitId, Ty, TypeExpr, TypeId, Universe,
    Witness,
};

/// `(a, b)` as a written expression.
fn pair(a: TypeId, b: TypeId) -> TypeExpr {
    TypeExpr::tuple(vec![TypeExpr::atom(a), TypeExpr::atom(b)])
}

/// `(a, b)` as a written expression with expression elements.
fn pair_expr(a: TypeExpr, b: TypeExpr) -> TypeExpr {
    TypeExpr::tuple(vec![a, b])
}

/// `a | b` over atom ids, for element expressions.
fn union_of(a: TypeId, b: TypeId) -> TypeExpr {
    TypeExpr::atom(a).union(b.into())
}

#[test]
fn tuples_are_structural_and_hash_consed() {
    let mut u = Universe::new();
    let int = u.declare_tag("int").unwrap();
    let boolean = u.declare_tag("bool").unwrap();
    let string = u.declare_tag("string").unwrap();

    let ty = u.solve(&pair(int, boolean)).unwrap();
    assert_eq!(u.members(ty).len(), 1, "one structural atom per tuple");
    assert_eq!(u.render(ty), "(int, bool)");
    // Same elements, same atom, same handle — hash-consed structurally.
    assert_eq!(u.solve(&pair(int, boolean)).unwrap(), ty);
    // Element order is position: a different tuple.
    let swapped = u.solve(&pair(boolean, int)).unwrap();
    assert_ne!(swapped, ty);
    assert_eq!(u.render(swapped), "(bool, int)");
    assert!(u.is_disjoint(ty, swapped));

    // The corpus kv shape ("host", 8080) renders structurally.
    let kv = u.solve(&pair(string, int)).unwrap();
    assert_eq!(u.render(kv), "(string, int)");
    assert!(u.is_disjoint(kv, ty));

    // set_struct_fields/set_enum_cases reject structural atoms.
    let atom = u.members(ty)[0];
    assert!(matches!(
        u.set_struct_fields(atom, vec![]),
        Err(Error::WrongAtomKind(_))
    ));
}

#[test]
fn tuple_products_distribute_over_unions() {
    let mut u = Universe::new();
    let a = u.declare_tag("a").unwrap();
    let b = u.declare_tag("b").unwrap();
    let c = u.declare_tag("c").unwrap();

    // (a | b, c) == (a, c) | (b, c).
    let distributed = u.solve(&pair_expr(union_of(a, b), c.into())).unwrap();
    let manual = u.solve(&pair(a, c).union(pair(b, c))).unwrap();
    assert_eq!(distributed, manual);
    assert_eq!(u.render(distributed), "(a, c) | (b, c)");

    // Containment and overlap are exact element-wise after distribution.
    let ac = u.solve(&pair(a, c)).unwrap();
    assert!(u.contains(distributed, ac));
    assert!(!u.contains(ac, distributed));
    assert!(!u.is_disjoint(distributed, ac));
    let bc = u.solve(&pair(b, c)).unwrap();
    assert!(u.is_disjoint(ac, bc), "same arity, disjoint elements");
    // Different arity is trivially disjoint; arity-1 is its own atom.
    let a_solo = u.solve(&TypeExpr::tuple(vec![a.into()])).unwrap();
    let a_unit = u.atom_ty(a).unwrap();
    assert_ne!(a_solo, a_unit, "an arity-1 tuple is not its element");
    assert!(u.is_disjoint(a_solo, ac));
    let abc = u
        .solve(&TypeExpr::tuple(vec![a.into(), b.into(), c.into()]))
        .unwrap();
    assert!(u.is_disjoint(abc, ac));

    // An empty element empties the product; no atom is minted for it.
    let empty_el = u
        .solve(&pair_expr(a.into(), TypeExpr::atom(b).inter(c.into())))
        .unwrap();
    assert!(u.is_empty(empty_el));
    assert_eq!(u.render(empty_el), "never");
    assert!(matches!(u.witness(empty_el), Err(Error::WitnessOfEmpty(_))));
}

/// An element expression: the union of `n` freshly declared tags.
fn members_union(u: &mut Universe, name: &str, n: usize) -> TypeExpr {
    let mut acc: Option<TypeExpr> = None;
    for i in 0..n {
        let tag = u.declare_tag(&format!("{name}{i}")).unwrap();
        acc = Some(match acc {
            None => TypeExpr::atom(tag),
            Some(acc) => acc.union(tag.into()),
        });
    }
    acc.expect("element union")
}

#[test]
fn tuple_distribution_is_capped_before_minting() {
    let mut u = Universe::new();
    // Three 100-member element sets: a 10^6-atom product, refused before
    // any combination atom exists.
    let elements = (0..3)
        .map(|e| members_union(&mut u, &format!("e{e}_"), 100))
        .collect();
    let err = u.solve(&TypeExpr::tuple(elements)).unwrap_err();
    let Error::TupleDistributionCap { tuple, sizes, cap } = &err else {
        panic!("wrong error: {err:?}");
    };
    assert_eq!(sizes, &vec![100, 100, 100]);
    assert_eq!(*cap, 4096);
    assert!(tuple.starts_with("(e0_0"), "names the tuple: {tuple}");
    // The rendered error names the tuple, the element sizes, and the cap.
    assert_eq!(
        err.to_string(),
        format!(
            "tuple {tuple} distributes into more than {cap} combinations (element sizes {sizes:?})"
        )
    );
    // Nothing was minted: a later, small distribution still solves.
    assert!(u.solve(&TypeExpr::tuple(vec![])).is_ok());
}

#[test]
fn tuple_distribution_within_the_cap_mints() {
    let mut u = Universe::new();
    // 40 x 50 = 2000 combinations, inside the cap of 4096.
    let a = members_union(&mut u, "a", 40);
    let b = members_union(&mut u, "b", 50);
    let ty = u.solve(&TypeExpr::tuple(vec![a, b])).unwrap();
    assert_eq!(u.members(ty).len(), 2000);
}

#[test]
fn empty_tuple_is_a_unit_atom_distinct_from_none() {
    let mut u = Universe::new();
    let none_ty = u.atom_ty(TypeId::NONE).unwrap();
    let unit = u.solve(&TypeExpr::tuple(vec![])).unwrap();
    assert_eq!(u.render(unit), "()");
    assert_ne!(unit, none_ty);
    assert!(u.is_disjoint(unit, none_ty));
    // Both are singletons with exactly one witness.
    let unit_witness = u.witness(unit).unwrap();
    let none_witness = u.witness(none_ty).unwrap();
    assert_eq!(u.render_witness(&unit_witness), "()");
    assert_eq!(u.render_witness(&none_witness), "none");
    // T? over () keeps both atoms distinct; members render in declaration
    // order, and `none` is every universe's first atom.
    let opt = u.optional(unit);
    assert!(u.contains(opt, unit));
    assert!(u.contains(opt, none_ty));
    assert_eq!(u.render(opt), "none | ()");
}

#[test]
fn nested_tuples_nest_one_level_structurally() {
    let mut u = Universe::new();
    let a = u.declare_tag("a").unwrap();
    let b = u.declare_tag("b").unwrap();
    let c = u.declare_tag("c").unwrap();

    // ((a, b), c) is one atom whose first element is the (a, b) atom.
    let nested = u.solve(&pair_expr(pair(a, b), c.into())).unwrap();
    assert_eq!(u.members(nested).len(), 1);
    assert_eq!(u.render(nested), "((a, b), c)");

    // Distribution reaches through nesting: (a | b, (a, b)) splits the first
    // element into two atoms.
    let split = u.solve(&pair_expr(union_of(a, b), pair(a, b))).unwrap();
    assert_eq!(u.render(split), "(a, (a, b)) | (b, (a, b))");

    // A nested tuple is disjoint from its own element: different shape.
    let ab = u.solve(&pair(a, b)).unwrap();
    let a_ty = u.atom_ty(a).unwrap();
    assert!(u.is_disjoint(nested, ab));
    assert!(u.is_disjoint(nested, a_ty));
    // Containment through nesting.
    let inner = u.solve(&pair_expr(b.into(), pair(a, b))).unwrap();
    assert!(u.contains(split, inner));
}

#[test]
fn tuple_witnesses_are_structural() {
    let mut u = Universe::new();
    let int = u.declare_tag("int").unwrap();
    let boolean = u.declare_tag("bool").unwrap();
    let string = u.declare_tag("string").unwrap();
    let bad_port = u.declare_struct("bad_port").unwrap();
    u.set_struct_fields(bad_port, vec![Field::named("got", TypeExpr::atom(int))])
        .unwrap();

    let ty = u.solve(&pair(int, bad_port)).unwrap();
    let witness = u.witness(ty).unwrap();
    assert!(matches!(witness, Witness::Tuple { .. }));
    assert_eq!(u.render_witness(&witness), "(int, bad_port{got: int})");

    // Nested elements recurse; unions pick the earliest-declared member, so a
    // tuple minted before a later tag is witnessed first.
    let nested = u
        .solve(&pair_expr(pair(int, boolean), string.into()))
        .unwrap();
    let nested_witness = u.witness(nested).unwrap();
    assert_eq!(u.render_witness(&nested_witness), "((int, bool), string)");
    let extra = u.declare_tag("extra").unwrap();
    let extra_ty = u.atom_ty(extra).unwrap();
    let union = u.union(nested, extra_ty);
    let union_witness = u.witness(union).unwrap();
    assert_eq!(
        u.render_witness(&union_witness),
        "((int, bool), string)",
        "the earlier-minted tuple atom is witnessed first"
    );

    // Struct fields holding tuples render through the field type.
    let record = u.declare_struct("record").unwrap();
    u.set_struct_fields(record, vec![Field::named("kv", pair(string, int))])
        .unwrap();
    let record_ty = u.atom_ty(record).unwrap();
    let record_witness = u.witness(record_ty).unwrap();
    assert_eq!(
        u.render_witness(&record_witness),
        "record{kv: (string, int)}"
    );
}

#[test]
fn coherence_and_membership_over_tuple_heads() {
    // Tuple atoms are atoms: instance heads, overlap witnesses, and trait
    // membership all run through the same set algebra.
    let mut u = Universe::new();
    let int = u.declare_tag("int").unwrap();
    let boolean = u.declare_tag("bool").unwrap();
    let string = u.declare_tag("string").unwrap();
    let json = u.declare_trait("json").unwrap();

    let mut env = InstanceEnv::new();
    env.declare(&mut u, &InstanceDecl::new(json, pair(int, boolean)))
        .unwrap();
    // (int, bool) and (int, string) share element 0 but differ at 1: disjoint.
    env.declare(&mut u, &InstanceDecl::new(json, pair(int, string)))
        .unwrap();

    // (int, bool | string) distributes; its (int, bool) combination overlaps
    // the first instance, and the witness names the tuple value type.
    let err = env
        .declare(
            &mut u,
            &InstanceDecl::new(
                json,
                pair_expr(int.into(), TypeExpr::atom(boolean).union(string.into())),
            ),
        )
        .unwrap_err();
    let Error::InstanceOverlap { witness, .. } = &err else {
        panic!("expected InstanceOverlap, got {err:?}");
    };
    assert_eq!(u.render_witness(witness), "(int, bool)");

    // Unconditional instances add their tuple atom to the trait's members.
    let json_ty = u.solve(&TypeExpr::trait_ref(json)).unwrap();
    let ib = u.solve(&pair(int, boolean)).unwrap();
    assert!(u.contains(json_ty, ib));
}

// ---- exhaustive oracle over a tuple-bearing universe ----------------------

/// Fixture: nominal atoms `none, a, b` (3), one trait `s` over `{a, b}` and
/// one trait `r` over `{b}`, and the pre-minted tuple atoms `()` plus every
/// arity-1 and arity-2 tuple over `{a, b}` (5 more) — 8 atoms total, so the
/// semantic space is 2^8 = 256 solved types and the pairwise laws run over
/// all of them. The expression family restricts tuple elements to
/// expressions denoting subsets of `{a, b}` (`a`, `b`, `a | b`, `s`), so no
/// evaluation mints a tuple outside the pre-minted set and the bitset model
/// stays a complete oracle for the universe.
struct TupleFixture {
    universe: Universe,
    atoms: Vec<TypeId>,
    s: TraitId,
    r: TraitId,
    membership: [u64; 2],
}

fn tuple_fixture() -> TupleFixture {
    let mut universe = Universe::new();
    let a = universe.declare_tag("a").unwrap();
    let b = universe.declare_tag("b").unwrap();
    let s = universe.declare_trait("s").unwrap();
    let r = universe.declare_trait("r").unwrap();
    universe.impl_trait(s, a).unwrap();
    universe.impl_trait(s, b).unwrap();
    universe.impl_trait(r, b).unwrap();

    // Pre-mint every tuple the family can express, capturing the ids.
    // The builtin `type` atom rides beside none, in no trait.
    let mut atoms = vec![TypeId::NONE, TypeId::TYPE, a, b];
    let tuples: Vec<TypeExpr> = vec![
        TypeExpr::tuple(vec![]),
        TypeExpr::tuple(vec![a.into()]),
        TypeExpr::tuple(vec![b.into()]),
        pair(a, a),
        pair(a, b),
        pair(b, a),
        pair(b, b),
    ];
    for expr in &tuples {
        let ty = universe.solve(expr).unwrap();
        atoms.push(universe.members(ty)[0]);
    }
    TupleFixture {
        universe,
        atoms,
        s,
        r,
        membership: [(1 << 2) | (1 << 3), 1 << 3],
    }
}

impl TupleFixture {
    fn position(&self, id: TypeId) -> u64 {
        self.atoms
            .iter()
            .position(|x| *x == id)
            .expect("fixture atom") as u64
    }

    fn full(&self) -> u64 {
        (1 << self.atoms.len() as u64) - 1
    }

    /// The bit of the pre-minted tuple atom for one element combination;
    /// panics if the family minted outside the pre-minted set (closed-universe
    /// invariant).
    fn tuple_bit(&self, combo: &[TypeId]) -> u64 {
        for id in &self.atoms {
            if let AtomKind::Tuple(elements) = &self.universe.atom_decl(*id).unwrap().kind
                && elements.as_slice() == combo
            {
                return 1 << self.position(*id);
            }
        }
        panic!("combination {combo:?} was not pre-minted");
    }

    /// The bitset of a solved type; also asserts the interning invariant.
    fn bits(&self, t: Ty) -> u64 {
        let members = self.universe.members(t);
        let mut sorted = members.to_vec();
        sorted.sort_unstable();
        assert_eq!(members, sorted, "members must stay sorted");
        sorted.dedup();
        assert_eq!(members.len(), sorted.len(), "members stay deduplicated");
        members
            .iter()
            .map(|id| 1 << self.position(*id))
            .fold(0, |acc, bit| acc | bit)
    }

    /// Independent evaluator; never queries universe state.
    fn eval(&self, expr: &TypeExpr) -> u64 {
        let trait_bits = |id: TraitId| {
            // Fixture order: membership[0] is `s`, membership[1] is `r`.
            let index = usize::from(id != self.s);
            self.membership[index]
        };
        match expr {
            TypeExpr::Atom(id) => 1 << self.position(*id),
            TypeExpr::Trait(id) => trait_bits(*id),
            TypeExpr::Not(id) => self.full() & !trait_bits(*id),
            TypeExpr::Union(x, y) => self.eval(x) | self.eval(y),
            TypeExpr::Inter(x, y) => self.eval(x) & self.eval(y),
            TypeExpr::Diff(x, y) => self.eval(x) & !self.eval(y),
            // The product of the element bitsets, mapped through the
            // pre-minted combination atoms.
            TypeExpr::Tuple(elements) => {
                let mut combinations: Vec<Vec<TypeId>> = vec![Vec::new()];
                for element in elements {
                    let bits = self.eval(element);
                    let members: Vec<TypeId> = self
                        .atoms
                        .iter()
                        .copied()
                        .filter(|id| bits & (1 << self.position(*id)) != 0)
                        .collect();
                    let mut next = Vec::new();
                    for prefix in &combinations {
                        for atom in &members {
                            let mut combo = prefix.clone();
                            combo.push(*atom);
                            next.push(combo);
                        }
                    }
                    combinations = next;
                }
                combinations
                    .iter()
                    .map(|combo| self.tuple_bit(combo))
                    .fold(0, |acc, bit| acc | bit)
            },
            TypeExpr::Empty => 0,
        }
    }
}

#[test]
fn exhaustive_solve_matches_bitset_model_with_tuples() {
    let mut f = tuple_fixture();
    // The fixture universe is closed: 8 atoms, no strays minted by the family.
    for expr in expr_family(&f) {
        let solved = f.universe.solve(&expr).unwrap();
        let model = f.eval(&expr);
        assert_eq!(
            f.bits(solved),
            model,
            "solver disagrees with the model on {expr:?}"
        );
        assert_eq!(
            f.universe.is_empty(solved),
            model == 0,
            "emptiness disagrees with the model"
        );
    }
}

#[test]
fn exhaustive_pairwise_laws_hold_over_tuple_universe() {
    let mut f = tuple_fixture();
    let all = all_types(&mut f);
    let empty = f.universe.empty_ty();
    let full = f.universe.universe_ty();
    let none = f.universe.atom_ty(TypeId::NONE).unwrap();

    // Distinct atoms — nominal and structural alike — stay pairwise disjoint;
    // this is what keeps every set-based procedure exact over tuples.
    let singletons: Vec<Ty> = f
        .atoms
        .clone()
        .iter()
        .map(|id| f.universe.atom_ty(*id).unwrap())
        .collect();
    for (i, &x) in singletons.iter().enumerate() {
        assert!(!f.universe.is_empty(x));
        for (j, &y) in singletons.iter().enumerate() {
            assert_eq!(
                f.universe.is_disjoint(x, y),
                i != j,
                "distinct atoms must be disjoint"
            );
        }
    }

    for &a in &all {
        assert_eq!(f.universe.union(a, a), a, "idempotent");
        assert_eq!(f.universe.union(a, empty), a, "empty identity");
        assert_eq!(f.universe.inter(a, a), a, "idempotent");
        assert_eq!(f.universe.inter(a, full), a, "universe identity");
        assert_eq!(f.universe.diff(a, a), empty, "a - a is empty");
        assert_eq!(f.universe.diff(a, empty), a, "a - empty is a");
        let not_a = f.universe.complement(a);
        let clash = f.universe.inter(a, not_a);
        assert!(f.universe.is_empty(clash));
        assert_eq!(f.universe.union(a, not_a), full, "excluded middle");
        let opt = f.universe.optional(a);
        assert_eq!(opt, f.universe.union(a, none), "T? is T | none [B4]");
        for &b in &all {
            let ab = f.universe.union(a, b);
            assert_eq!(ab, f.universe.union(b, a), "commutative");
            let inter = f.universe.inter(a, b);
            assert_eq!(
                f.universe.is_disjoint(a, b),
                f.universe.is_empty(inter),
                "disjoint iff empty intersection"
            );
            let residue = f.universe.diff(b, a);
            assert_eq!(
                f.universe.contains(a, b),
                f.universe.is_empty(residue),
                "contains iff sub - sup is empty"
            );
            assert_eq!(
                f.universe.is_empty(ab),
                f.universe.is_empty(a) && f.universe.is_empty(b),
                "a union is empty only if all members are"
            );
            let not_b = f.universe.complement(b);
            let via_complement = f.universe.inter(a, not_b);
            assert_eq!(f.universe.diff(a, b), via_complement, "a - b == a & ~b");
        }
    }
}

#[test]
fn sampled_triple_laws_hold_over_tuple_universe() {
    // Associativity and distributivity over a deterministic stride of the
    // 512-type space (full 512^3 is out of budget; `algebra.rs` covers the
    // tuple-free universe exhaustively).
    let mut f = tuple_fixture();
    let all = all_types(&mut f);
    let stride: Vec<&Ty> = all.iter().step_by(32).collect();
    assert_eq!(stride.len(), 16);
    for &a in &stride {
        for &b in &stride {
            for &c in &stride {
                let ab = f.universe.union(*a, *b);
                let bc = f.universe.union(*b, *c);
                assert_eq!(
                    f.universe.union(ab, *c),
                    f.universe.union(*a, bc),
                    "associative"
                );
                let bc_inter = f.universe.inter(*b, *c);
                let lhs = f.universe.union(*a, bc_inter);
                let left = f.universe.union(*a, *b);
                let right = f.universe.union(*a, *c);
                let rhs = f.universe.inter(left, right);
                assert_eq!(lhs, rhs, "union distributes over intersection");
            }
        }
    }
}

/// All 256 solved types of the fixture universe, indexed by bitset value.
fn all_types(f: &mut TupleFixture) -> Vec<Ty> {
    let singletons: Vec<Ty> = f
        .atoms
        .clone()
        .iter()
        .map(|id| f.universe.atom_ty(*id).unwrap())
        .collect();
    let mut all = vec![f.universe.empty_ty()];
    for bits in 1u64..=511 {
        let mut t = all[0];
        for (i, singleton) in singletons.iter().enumerate() {
            if bits & (1 << i) != 0 {
                t = f.universe.union(t, *singleton);
            }
        }
        all.push(t);
    }
    assert_eq!(all.len(), 512);
    let uniq: std::collections::HashSet<Ty> = all.iter().copied().collect();
    assert_eq!(uniq.len(), 512, "distinct sets must get distinct handles");
    all
}

/// Expression family: literals (atoms, trait refs, complements, tuple
/// literals with elements denoting subsets of {a, b}), all depth-1 trees,
/// and depth-2 trees over a deterministic stride — mirroring `algebra.rs`.
fn expr_family(f: &TupleFixture) -> Vec<TypeExpr> {
    // [none, type, a, b, ...]: the declared pair sits past the builtin
    // atoms every universe carries.
    let (a, b) = (f.atoms[2], f.atoms[3]);
    let mut literals = vec![
        TypeExpr::atom(TypeId::NONE),
        TypeExpr::atom(a),
        TypeExpr::atom(b),
        TypeExpr::trait_ref(f.s),
        TypeExpr::trait_ref(f.r),
        TypeExpr::not(f.s),
        TypeExpr::not(f.r),
        TypeExpr::tuple(vec![]),
        TypeExpr::tuple(vec![a.into()]),
        TypeExpr::tuple(vec![b.into()]),
        pair(a, a),
        pair(a, b),
        pair(b, a),
        pair(b, b),
        // Tuple elements restricted to {a, b}: distribution stays inside the
        // pre-minted combinations, keeping the model a closed oracle.
        pair_expr(union_of(a, b), a.into()),
        pair_expr(union_of(a, b), b.into()),
        pair_expr(TypeExpr::trait_ref(f.s), a.into()),
        pair_expr(b.into(), TypeExpr::trait_ref(f.s)),
    ];
    // A tuple literal over the trait and a union: same restriction.
    literals.push(pair_expr(a.into(), union_of(a, b)));
    let combine = |x: &TypeExpr, y: &TypeExpr, op: usize| -> TypeExpr {
        match op {
            0 => x.clone().union(y.clone()),
            1 => x.clone().inter(y.clone()),
            _ => x.clone().diff(y.clone()),
        }
    };
    let mut depth1 = literals.clone();
    for x in &literals {
        for y in &literals {
            for op in 0..3 {
                depth1.push(combine(x, y, op));
            }
        }
    }
    let mut family = depth1.clone();
    let stride: Vec<&TypeExpr> = depth1.iter().step_by(10).collect();
    for x in &stride {
        for y in &stride {
            for op in 0..3 {
                family.push(combine(x, y, op));
            }
        }
    }
    family
}
