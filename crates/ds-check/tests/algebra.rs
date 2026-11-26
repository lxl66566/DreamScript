//! Exhaustive algebra-law checks over a fixed 7-atom, 2-trait universe [B1].
//!
//! The semantic space of the fixture universe is 2^7 = 128 solved types, so
//! the laws run over all of them: every ordered pair (128x128) and, where the
//! law is ternary, every triple (128^3). An independent bitset model is the
//! oracle for every solver expression family member up to depth two.

use ds_check::{Field, TraitId, Ty, TypeExpr, TypeId, Universe};

struct Fixture {
    universe: Universe,
    atoms: Vec<TypeId>,
    traits: [TraitId; 2],
    /// Ground-truth membership bitsets, built by the fixture itself (never
    /// queried back from the universe).
    membership: [u64; 2],
}

fn fixture() -> Fixture {
    let mut universe = Universe::new();
    let none = TypeId::NONE;
    let io = universe.declare_tag("io_err").unwrap();
    let missing = universe.declare_tag("missing").unwrap();
    let bad_port = universe.declare_struct("bad_port").unwrap();
    universe
        .set_struct_fields(bad_port, vec![Field::named("got", TypeExpr::atom(io))])
        .unwrap();
    let config = universe.declare_struct("config").unwrap();
    universe.set_struct_fields(config, vec![]).unwrap();
    let txt = universe.declare_tag("txt").unwrap();
    let sendable = universe.declare_trait("sendable").unwrap();
    let hashable = universe.declare_trait("hashable").unwrap();
    universe.impl_trait(sendable, io).unwrap();
    universe.impl_trait(sendable, missing).unwrap();
    universe.impl_trait(hashable, missing).unwrap();
    universe.impl_trait(hashable, bad_port).unwrap();
    Fixture {
        universe,
        // [none, type, io, ...]: the builtin `type` atom rides beside none
        // in every universe (G1, G2), a member of no trait.
        atoms: vec![none, TypeId::TYPE, io, missing, bad_port, config, txt],
        traits: [sendable, hashable],
        membership: [(1 << 2) | (1 << 3), (1 << 3) | (1 << 4)],
    }
}

impl Fixture {
    /// Singleton type of a fixture atom.
    fn ty(&mut self, id: TypeId) -> Ty {
        self.universe.atom_ty(id).unwrap()
    }

    fn position(&self, id: TypeId) -> u64 {
        self.atoms
            .iter()
            .position(|a| *a == id)
            .expect("fixture atom") as u64
    }

    fn full(&self) -> u64 {
        (1 << self.atoms.len() as u64) - 1
    }

    /// The bitset of a solved type; also asserts the sorted, deduplicated
    /// interning invariant the universe must maintain.
    fn bits(&self, t: Ty) -> u64 {
        let members = self.universe.members(t);
        let mut sorted = members.to_vec();
        sorted.sort_unstable();
        assert_eq!(members, sorted, "members must stay sorted");
        sorted.dedup();
        assert_eq!(
            members.len(),
            sorted.len(),
            "members must stay deduplicated"
        );
        members
            .iter()
            .map(|id| 1 << self.position(*id))
            .fold(0, |acc, b| acc | b)
    }

    /// Independent evaluator: the model never touches universe state.
    fn eval(&self, expr: &TypeExpr) -> u64 {
        let trait_bits = |id: TraitId| {
            let index = self
                .traits
                .iter()
                .position(|t| *t == id)
                .expect("fixture trait");
            self.membership[index]
        };
        match expr {
            TypeExpr::Atom(id) => 1 << self.position(*id),
            TypeExpr::Trait(id) => trait_bits(*id),
            TypeExpr::Not(id) => self.full() & !trait_bits(*id),
            TypeExpr::Union(a, b) => self.eval(a) | self.eval(b),
            TypeExpr::Inter(a, b) => self.eval(a) & self.eval(b),
            TypeExpr::Diff(a, b) => self.eval(a) & !self.eval(b),
            // This fixture's family expresses no tuples; `tuples.rs` owns the
            // tuple-bearing oracle.
            TypeExpr::Tuple(_) => unreachable!("tuple expressions are covered by tuples.rs"),
            TypeExpr::Empty => 0,
        }
    }

    /// All 128 solved types of the universe, indexed by bitset value.
    fn all_types(&mut self) -> Vec<Ty> {
        let singletons: Vec<Ty> = self.atoms.clone().iter().map(|id| self.ty(*id)).collect();
        let mut all = vec![self.universe.empty_ty()];
        for bits in 1u64..=127 {
            let mut t = all[0];
            for (i, singleton) in singletons.iter().enumerate() {
                if bits & (1 << i) != 0 {
                    t = self.universe.union(t, *singleton);
                }
            }
            all.push(t);
        }
        assert_eq!(all.len(), 128);
        let uniq: std::collections::HashSet<Ty> = all.iter().copied().collect();
        assert_eq!(uniq.len(), 128, "distinct sets must get distinct handles");
        all
    }

    /// Expression family: all literals, all depth-1 trees, and all depth-2
    /// trees over a deterministic subset of depth-1.
    fn expr_family(&self) -> Vec<TypeExpr> {
        let mut literals = vec![];
        for id in &self.atoms {
            literals.push(TypeExpr::atom(*id));
        }
        for t in &self.traits {
            literals.push(TypeExpr::trait_ref(*t));
            literals.push(TypeExpr::not(*t));
        }
        let combine = |a: &TypeExpr, b: &TypeExpr, op: usize| -> TypeExpr {
            match op {
                0 => a.clone().union(b.clone()),
                1 => a.clone().inter(b.clone()),
                _ => a.clone().diff(b.clone()),
            }
        };
        let mut depth1 = literals.clone();
        for a in &literals {
            for b in &literals {
                for op in 0..3 {
                    depth1.push(combine(a, b, op));
                }
            }
        }
        let mut family = depth1.clone();
        let stride: Vec<&TypeExpr> = depth1.iter().step_by(10).collect();
        for a in &stride {
            for b in &stride {
                for op in 0..3 {
                    family.push(combine(a, b, op));
                }
            }
        }
        family
    }
}

#[test]
fn union_commutative_idempotent_empty_identity() {
    let mut f = fixture();
    let all = f.all_types();
    let empty = f.universe.empty_ty();
    for &a in &all {
        assert_eq!(f.universe.union(a, a), a, "idempotent");
        assert_eq!(f.universe.union(a, empty), a, "empty is the identity");
        for &b in &all {
            let ab = f.universe.union(a, b);
            let ba = f.universe.union(b, a);
            assert_eq!(ab, ba, "commutative");
        }
    }
}

#[test]
fn union_associative() {
    let mut f = fixture();
    let all = f.all_types();
    for &a in &all {
        for &b in &all {
            for &c in &all {
                let ab = f.universe.union(a, b);
                let bc = f.universe.union(b, c);
                let left = f.universe.union(ab, c);
                let right = f.universe.union(a, bc);
                assert_eq!(left, right, "associative");
            }
        }
    }
}

#[test]
fn inter_commutative_idempotent_universe_identity() {
    let mut f = fixture();
    let all = f.all_types();
    let full = f.universe.universe_ty();
    for &a in &all {
        assert_eq!(f.universe.inter(a, a), a, "idempotent");
        assert_eq!(f.universe.inter(a, full), a, "universe is the identity");
        for &b in &all {
            let ab = f.universe.inter(a, b);
            let ba = f.universe.inter(b, a);
            assert_eq!(ab, ba, "commutative");
        }
    }
}

#[test]
fn inter_associative_and_distributive() {
    let mut f = fixture();
    let all = f.all_types();
    for &a in &all {
        for &b in &all {
            for &c in &all {
                let ab = f.universe.inter(a, b);
                let bc = f.universe.inter(b, c);
                let left = f.universe.inter(ab, c);
                let right = f.universe.inter(a, bc);
                assert_eq!(left, right, "associative");

                let bc_i = f.universe.inter(b, c);
                let lhs = f.universe.union(a, bc_i);
                let a_join_b = f.universe.union(a, b);
                let a_join_c = f.universe.union(a, c);
                let rhs = f.universe.inter(a_join_b, a_join_c);
                assert_eq!(lhs, rhs, "union distributes over intersection");

                let b_join_c = f.universe.union(b, c);
                let dual_lhs = f.universe.inter(a, b_join_c);
                let a_meet_b = f.universe.inter(a, b);
                let a_meet_c = f.universe.inter(a, c);
                let dual_rhs = f.universe.union(a_meet_b, a_meet_c);
                assert_eq!(dual_lhs, dual_rhs, "intersection distributes over union");
            }
        }
    }
}

#[test]
fn absorption() {
    let mut f = fixture();
    let all = f.all_types();
    for &a in &all {
        for &b in &all {
            let ab = f.universe.inter(a, b);
            let join = f.universe.union(a, ab);
            assert_eq!(join, a, "a | (a & b) == a");
            let ab2 = f.universe.union(a, b);
            let meet = f.universe.inter(a, ab2);
            assert_eq!(meet, a, "a & (a | b) == a");
        }
    }
}

#[test]
fn de_morgan_and_complement_involution() {
    let mut f = fixture();
    let all = f.all_types();
    let empty = f.universe.empty_ty();
    let full = f.universe.universe_ty();
    assert_eq!(f.universe.complement(full), empty);
    assert_eq!(f.universe.complement(empty), full);
    for &a in &all {
        let not_a = f.universe.complement(a);
        let not_not_a = f.universe.complement(not_a);
        assert_eq!(not_not_a, a, "involution");
        let clash = f.universe.inter(a, not_a);
        assert!(f.universe.is_empty(clash), "t & ~t is empty");
        let exhaustive = f.universe.union(a, not_a);
        assert_eq!(exhaustive, full, "t | ~t is the universe");
        for &b in &all {
            let aub = f.universe.union(a, b);
            let aib = f.universe.inter(a, b);
            let lhs = f.universe.complement(aub);
            let not_b = f.universe.complement(b);
            let na_inter_nb = f.universe.inter(not_a, not_b);
            assert_eq!(lhs, na_inter_nb, "~(a | b) == ~a & ~b");
            let lhs2 = f.universe.complement(aib);
            let na_union_nb = f.universe.union(not_a, not_b);
            assert_eq!(lhs2, na_union_nb, "~(a & b) == ~a | ~b");
        }
    }
}

#[test]
fn difference_laws() {
    let mut f = fixture();
    let all = f.all_types();
    let empty = f.universe.empty_ty();
    let full = f.universe.universe_ty();
    for &a in &all {
        assert_eq!(f.universe.diff(a, a), empty, "a - a is empty");
        assert_eq!(f.universe.diff(a, empty), a, "a - empty is a");
        assert_eq!(f.universe.diff(empty, a), empty, "empty - a is empty");
        assert_eq!(f.universe.diff(a, full), empty, "a - universe is empty");
        for &b in &all {
            let d = f.universe.diff(a, b);
            let not_b = f.universe.complement(b);
            let via_complement = f.universe.inter(a, not_b);
            assert_eq!(d, via_complement, "a - b == a & ~b");
            assert!(f.universe.contains(a, d), "a - b stays inside a");
            assert!(f.universe.is_disjoint(d, b), "a - b is disjoint from b");
        }
    }
}

#[test]
fn disjoint_iff_empty_intersection() {
    let mut f = fixture();
    let all = f.all_types();
    for &a in &all {
        for &b in &all {
            let inter = f.universe.inter(a, b);
            assert_eq!(
                f.universe.is_disjoint(a, b),
                f.universe.is_empty(inter),
                "disjoint iff empty intersection"
            );
        }
    }
}

#[test]
fn contains_iff_empty_difference() {
    let mut f = fixture();
    let all = f.all_types();
    for &sup in &all {
        for &sub in &all {
            let residue = f.universe.diff(sub, sup);
            assert_eq!(
                f.universe.contains(sup, sub),
                f.universe.is_empty(residue),
                "contains iff sub - sup is empty"
            );
        }
    }
}

#[test]
fn union_empty_iff_all_members_empty() {
    let mut f = fixture();
    let all = f.all_types();
    for &a in &all {
        for &b in &all {
            let u = f.universe.union(a, b);
            assert_eq!(
                f.universe.is_empty(u),
                f.universe.is_empty(a) && f.universe.is_empty(b),
                "a union is empty only if all members are"
            );
        }
    }
}

#[test]
fn distinct_atoms_are_pairwise_disjoint() {
    let mut f = fixture();
    let ids = f.atoms.clone();
    let singletons: Vec<Ty> = ids.iter().map(|id| f.ty(*id)).collect();
    for (i, &a) in singletons.iter().enumerate() {
        assert!(!f.universe.is_empty(a), "an atom is never empty");
        for (j, &b) in singletons.iter().enumerate() {
            assert_eq!(
                f.universe.is_disjoint(a, b),
                i != j,
                "distinct nominal atoms are disjoint [B1]"
            );
        }
    }
}

#[test]
fn optional_is_union_none_and_idempotent() {
    let mut f = fixture();
    let all = f.all_types();
    let none = f.ty(TypeId::NONE);
    for &a in &all {
        let opt = f.universe.optional(a);
        let written = f.universe.union(a, none);
        assert_eq!(opt, written, "T? is exactly T | none [B4]");
        let opt_opt = f.universe.optional(opt);
        assert_eq!(opt_opt, opt, "(T?)? collapses to T?");
        assert!(f.universe.contains(opt, a), "T? contains T");
        assert!(f.universe.contains(opt, none), "T? contains none");
    }
    let none_opt = f.universe.optional(none);
    assert_eq!(none_opt, none, "none? is none");
}

#[test]
fn solve_matches_bitset_model() {
    let mut f = fixture();
    for expr in f.expr_family() {
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
fn solving_is_hash_consed_and_stable() {
    let mut f = fixture();
    for expr in f.expr_family() {
        let first = f.universe.solve(&expr).unwrap();
        let second = f.universe.solve(&expr).unwrap();
        assert_eq!(first, second, "same expression, same handle");
    }
    // Equal denotations built on different paths share one handle.
    let io = f.atoms[1];
    let missing = f.atoms[2];
    let bad_port = f.atoms[3];
    let io_ty = f.ty(io);
    let missing_ty = f.ty(missing);
    let bad_port_ty = f.ty(bad_port);
    let expr_form = f
        .universe
        .solve(
            &TypeExpr::atom(io)
                .union(missing.into())
                .union(bad_port.into()),
        )
        .unwrap();
    let left = f.universe.union(io_ty, missing_ty);
    let ty_form = f.universe.union(left, bad_port_ty);
    let swapped = f
        .universe
        .solve(
            &TypeExpr::atom(bad_port)
                .union(io.into())
                .union(missing.into()),
        )
        .unwrap();
    assert_eq!(expr_form, ty_form);
    assert_eq!(expr_form, swapped, "union order does not matter");
    // Re-solving after new declarations keeps old handles valid and produces
    // fresh, correct snapshots for universe-relative expressions.
    let before = f.universe.solve(&TypeExpr::not(f.traits[0])).unwrap();
    let txt2 = f.universe.declare_tag("txt2").unwrap();
    let after = f.universe.solve(&TypeExpr::not(f.traits[0])).unwrap();
    assert_ne!(before, after, "a grown universe grows the complement");
    assert!(
        f.universe.contains(after, before),
        "the old snapshot stays inside"
    );
    let txt2_ty = f.ty(txt2);
    assert!(f.universe.contains(after, txt2_ty));
}

#[test]
fn cached_singleton_and_universe_types_stay_correct_across_mints() {
    let mut f = fixture();
    let io = f.atoms[1];
    let singleton = f.ty(io);
    assert_eq!(
        f.ty(io),
        singleton,
        "a repeated singleton answers one handle"
    );
    let full = f.universe.universe_ty();
    assert_eq!(
        f.universe.universe_ty(),
        full,
        "a repeated full type answers one handle"
    );
    // A mint widens the universe: the full type is a fresh snapshot, the
    // singletons minted before it keep their handles.
    let fresh = f.universe.declare_tag("fresh").unwrap();
    let widened = f.universe.universe_ty();
    assert_ne!(widened, full, "a new atom widens the full type");
    assert!(f.universe.contains(widened, full));
    let fresh_ty = f.ty(fresh);
    assert!(f.universe.contains(widened, fresh_ty));
    assert_eq!(f.ty(io), singleton);
    assert!(f.universe.contains(widened, singleton));
    // Structural atoms mint too, so the full type tracks them as well.
    let pair = f
        .universe
        .solve(&TypeExpr::tuple(vec![
            TypeExpr::atom(io),
            TypeExpr::atom(fresh),
        ]))
        .unwrap();
    let regrown = f.universe.universe_ty();
    assert!(f.universe.contains(regrown, pair));
}
