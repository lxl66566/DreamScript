//! The atom registry, the hash-consed solved types, and the decision
//! procedures (B1). The normal-form decision and the termination argument are
//! documented at the crate root.

use std::{cmp::Ordering, collections::BTreeSet, rc::Rc};

use rustc_hash::FxHashMap;

use crate::{
    atom::{AtomDecl, AtomKind, Case, Field, TraitDecl, TraitId, TypeId},
    error::Error,
    expr::TypeExpr,
};

/// A solved type: a handle into the minting universe's hash-consed sets.
///
/// Handles are minted only by their [`Universe`]; two handles are equal
/// exactly when their solved member sets are equal, so structural type
/// equality is handle equality. A handle from one universe must never be
/// passed to another.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ty(u32);

impl Ty {
    /// The handle's raw index into its universe's interned sets
    /// (serialization, the checked-ambient artifact).
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Cap on the combinations one tuple distribution may mint. Each combination
/// mints a named structural atom plus a hash-cons index entry (hundreds of
/// bytes), so the cap bounds an adversarial distribution (e.g. eight
/// 1000-member element sets, a 10^24-atom product) to a few hundred KiB
/// while admitting any honest one — the corpus's largest is four
/// combinations. Over the cap, [`Universe::solve`] fails with a typed error
/// before any atom exists.
pub(crate) const MAX_TUPLE_COMBINATIONS: usize = 4096;

/// An atom and trait registry plus the hash-consed solved types over it.
///
/// Solved forms are snapshots of the universe state they were solved against:
/// declaring atoms can change what a negated constraint or a trait used as a
/// type solves to, so consumers re-solve after declarations change (the
/// memoized query layer owns that invalidation). Minted handles stay valid
/// and immutable forever.
///
/// Nominal atoms come from declarations; anonymous tuple atoms are minted
/// structurally on demand ([`Universe::solve`] on [`TypeExpr::Tuple`]) and
/// hash-consed on their element-atom lists (B3).
pub struct Universe {
    atoms: Vec<AtomDecl>,
    traits: Vec<TraitDecl>,
    atom_names: FxHashMap<String, TypeId>,
    trait_names: FxHashMap<String, TraitId>,
    sets: Vec<Rc<[TypeId]>>,
    index: FxHashMap<Rc<[TypeId]>, u32>,
    tuple_index: FxHashMap<Box<[TypeId]>, TypeId>,
    apply_index: FxHashMap<(TypeId, Box<[Ty]>), TypeId>,
    fn_index: FxHashMap<(Box<[Ty]>, Ty), TypeId>,
    /// Singleton solved type of every atom, filled on demand; a mint clears
    /// the entry it pushes.
    atom_tys: Vec<Option<Ty>>,
    /// The full-universe solved type, cleared by every mint.
    full_ty: Option<Ty>,
}

impl Universe {
    /// An empty universe with only the `none` atom declared (B4) and the
    /// builtin `type` atom beside it (G1, G2) — the comptime type-value
    /// type, declared in every universe so the keyword's solved handle is
    /// stable everywhere.
    #[must_use]
    pub fn new() -> Self {
        let none = AtomDecl {
            name: "none".to_owned(),
            kind: AtomKind::Unit,
        };
        let type_atom = AtomDecl {
            name: "type".to_owned(),
            kind: AtomKind::Unit,
        };
        let mut atom_names = FxHashMap::default();
        atom_names.insert(none.name.clone(), TypeId::NONE);
        atom_names.insert(type_atom.name.clone(), TypeId::TYPE);
        Self {
            atoms: vec![none, type_atom],
            traits: Vec::new(),
            atom_names,
            trait_names: FxHashMap::default(),
            sets: Vec::new(),
            index: FxHashMap::default(),
            tuple_index: FxHashMap::default(),
            apply_index: FxHashMap::default(),
            fn_index: FxHashMap::default(),
            atom_tys: vec![None, None],
            full_ty: None,
        }
    }

    /// Appends one atom declaration and clears the solved-type caches a new
    /// atom invalidates. Every atom — structural ones included — lands
    /// in the name index: a structural atom's name is derived from its
    /// content (characters no identifier spells), so name resolution is
    /// total over declared atoms, the address a serve's name-keyed rows
    /// resolve through.
    fn push_atom(&mut self, decl: AtomDecl) -> TypeId {
        let id = TypeId::from_raw(narrow(self.atoms.len()));
        self.atom_names.insert(decl.name.clone(), id);
        self.atoms.push(decl);
        self.atom_tys.push(None);
        self.full_ty = None;
        id
    }

    // ---- declarations ----------------------------------------------------

    /// Declares a unit tag: `tag io_err` (B1). Inhabited by the tag itself.
    pub fn declare_tag(&mut self, name: &str) -> Result<TypeId, Error> {
        self.declare_atom(name, AtomKind::Unit)
    }

    /// Declares a struct by reserving its identity (B1, B3); the field
    /// signature follows via [`Universe::set_struct_fields`], so recursive
    /// shapes can reference the returned id.
    pub fn declare_struct(&mut self, name: &str) -> Result<TypeId, Error> {
        self.declare_atom(name, AtomKind::Reserved)
    }

    /// Declares an enum by reserving its identity (B3); the case signature
    /// follows via [`Universe::set_enum_cases`].
    pub fn declare_enum(&mut self, name: &str) -> Result<TypeId, Error> {
        self.declare_atom(name, AtomKind::Reserved)
    }

    fn declare_atom(&mut self, name: &str, kind: AtomKind) -> Result<TypeId, Error> {
        if self.atom_names.contains_key(name) || self.trait_names.contains_key(name) {
            return Err(Error::DuplicateName(name.to_owned()));
        }
        Ok(self.push_atom(AtomDecl {
            name: name.to_owned(),
            kind,
        }))
    }

    /// Fills a reserved struct's field signature.
    pub fn set_struct_fields(&mut self, id: TypeId, fields: Vec<Field>) -> Result<(), Error> {
        match self.atom_decl(id)?.kind {
            AtomKind::Reserved => {
                self.atoms[id.index() as usize].kind = AtomKind::Struct(fields);
                Ok(())
            },
            AtomKind::Struct(_) => Err(Error::ShapeAlreadySet(id)),
            AtomKind::Unit | AtomKind::Enum(_) | AtomKind::Tuple(_) => {
                Err(Error::WrongAtomKind(id))
            },
            AtomKind::Apply(..) | AtomKind::FnSig(..) => Err(Error::WrongAtomKind(id)),
        }
    }

    /// Fills a reserved enum's case signature.
    pub fn set_enum_cases(&mut self, id: TypeId, cases: Vec<Case>) -> Result<(), Error> {
        match self.atom_decl(id)?.kind {
            AtomKind::Reserved => {
                self.atoms[id.index() as usize].kind = AtomKind::Enum(cases);
                Ok(())
            },
            AtomKind::Enum(_) => Err(Error::ShapeAlreadySet(id)),
            AtomKind::Unit | AtomKind::Struct(_) | AtomKind::Tuple(_) => {
                Err(Error::WrongAtomKind(id))
            },
            AtomKind::Apply(..) | AtomKind::FnSig(..) => Err(Error::WrongAtomKind(id)),
        }
    }

    /// Declares a trait (B6); member atoms follow via
    /// [`Universe::impl_trait`].
    pub fn declare_trait(&mut self, name: &str) -> Result<TraitId, Error> {
        if self.atom_names.contains_key(name) || self.trait_names.contains_key(name) {
            return Err(Error::DuplicateName(name.to_owned()));
        }
        let id = TraitId::from_raw(narrow(self.traits.len()));
        self.traits.push(TraitDecl {
            name: name.to_owned(),
            members: BTreeSet::new(),
        });
        self.trait_names.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Records that an atom implements a trait (`struct dog: animal`, or an
    /// instance head). Membership is a set: re-inserting an existing member
    /// is a no-op; duplicate instances at head level are
    /// [`crate::instance`]'s coherence error.
    pub fn impl_trait(&mut self, trait_id: TraitId, atom_id: TypeId) -> Result<(), Error> {
        self.atom_decl(atom_id)?;
        self.trait_decl_mut(trait_id)?.members.insert(atom_id);
        Ok(())
    }

    // ---- registry queries -------------------------------------------------

    /// The declared atom with the given name, if any.
    #[must_use]
    pub fn atom_by_name(&self, name: &str) -> Option<TypeId> {
        self.atom_names.get(name).copied()
    }

    /// The declared trait with the given name, if any.
    #[must_use]
    pub fn trait_by_name(&self, name: &str) -> Option<TraitId> {
        self.trait_names.get(name).copied()
    }

    /// The declaration of an atom.
    pub fn atom_decl(&self, id: TypeId) -> Result<&AtomDecl, Error> {
        self.atoms
            .get(id.index() as usize)
            .ok_or(Error::UnknownAtom(id))
    }

    /// The declaration of a trait.
    pub fn trait_decl(&self, id: TraitId) -> Result<&TraitDecl, Error> {
        self.traits
            .get(id.index() as usize)
            .ok_or(Error::UnknownTrait(id))
    }

    fn trait_decl_mut(&mut self, id: TraitId) -> Result<&mut TraitDecl, Error> {
        self.traits
            .get_mut(id.index() as usize)
            .ok_or(Error::UnknownTrait(id))
    }

    // ---- solving ------------------------------------------------------------

    /// Solves an expression into its canonical form over the current universe
    /// state: the sorted, deduplicated set of atoms it denotes. Difference
    /// and negation nodes erase here (B1), and tuple products distribute over
    /// unions element-wise into structural atoms (B3).
    ///
    /// Solving is one structural fold over the finite expression tree; every
    /// step is a finite set operation over the universe's atoms, so the
    /// procedure always terminates (crate-root termination argument). A tuple
    /// mints one atom per element combination — at most the product of the
    /// element set sizes, each an existing atom, and never more than
    /// `MAX_TUPLE_COMBINATIONS` (past that, a typed cap error).
    pub fn solve(&mut self, expr: &TypeExpr) -> Result<Ty, Error> {
        match expr {
            TypeExpr::Atom(id) => self.atom_ty(*id),
            TypeExpr::Trait(id) => {
                let members: Vec<TypeId> = self.trait_decl(*id)?.members.iter().copied().collect();
                Ok(self.intern_sorted(members))
            },
            TypeExpr::Not(id) => {
                let members = &self.trait_decl(*id)?.members;
                let atoms = narrow(self.atoms.len());
                let complement: Vec<TypeId> = (0..atoms)
                    .map(TypeId::from_raw)
                    .filter(|atom| !members.contains(atom))
                    .collect();
                Ok(self.intern_sorted(complement))
            },
            TypeExpr::Union(a, b) => {
                let (x, y) = (self.solve(a)?, self.solve(b)?);
                Ok(self.union(x, y))
            },
            TypeExpr::Inter(a, b) => {
                let (x, y) = (self.solve(a)?, self.solve(b)?);
                Ok(self.inter(x, y))
            },
            TypeExpr::Diff(a, b) => {
                let (x, y) = (self.solve(a)?, self.solve(b)?);
                Ok(self.diff(x, y))
            },
            TypeExpr::Tuple(elements) => self.tuple_solve(elements),
            TypeExpr::Empty => Ok(self.empty_ty()),
        }
    }

    /// Solves a tuple by distributing the product over the elements' solved
    /// sets (B3); the combinations themselves are [`Universe::product`],
    /// which enforces the distribution cap.
    fn tuple_solve(&mut self, elements: &[TypeExpr]) -> Result<Ty, Error> {
        let mut solved = Vec::with_capacity(elements.len());
        for expr in elements {
            solved.push(self.solve(expr)?);
        }
        self.product(&solved)
    }

    /// The structural atom of one element-atom list, minting it if absent
    /// (B3). Hash-consed on the list, so tuple atom identity is structural
    /// equality: same arity and same atoms position-wise. Elements must
    /// already be declared atoms (each a single atom by construction, since
    /// only [`Universe::tuple_solve`] mints tuples).
    /// The structural atom of one element-atom list (the serve's tuple
    /// replay mints through this beside [`Universe::apply_atom`]).
    pub fn tuple_atom(&mut self, elements: &[TypeId]) -> Result<TypeId, Error> {
        if let Some(&id) = self.tuple_index.get(elements) {
            return Ok(id);
        }
        for &element in elements {
            self.atom_decl(element)?;
        }
        let name = self.tuple_name(elements);
        let id = self.push_atom(AtomDecl {
            name,
            kind: AtomKind::Tuple(elements.to_vec()),
        });
        self.tuple_index.insert(elements.into(), id);
        Ok(id)
    }

    /// A tuple atom's structural name: `(t0, t1, ...)`, `()` at arity 0.
    fn tuple_name(&self, elements: &[TypeId]) -> String {
        let parts: Vec<&str> = elements
            .iter()
            .map(|id| {
                self.atom_decl(*id)
                    .map_or("<unknown>", |decl| decl.name.as_str())
            })
            .collect();
        format!("({})", parts.join(", "))
    }

    /// The structural atom of one generic application: a declared
    /// constructor plus its argument types as solved sets (B3). Hash-consed
    /// on `(constructor, arguments)`, so application identity is structural
    /// equality. Unlike tuples, applications stay opaque to the set algebra
    /// — a container does not distribute over a union argument — which is
    /// what keeps distinct applications disjoint by construction.
    pub fn apply_atom(&mut self, ctor: TypeId, args: &[Ty]) -> Result<TypeId, Error> {
        if let Some(&id) = self.apply_index.get(&(ctor, args.into())) {
            return Ok(id);
        }
        self.atom_decl(ctor)?;
        for arg in args {
            let _ = self.members(*arg);
        }
        let name = self.apply_name(ctor, args);
        let id = self.push_atom(AtomDecl {
            name,
            kind: AtomKind::Apply(ctor, args.into()),
        });
        self.apply_index.insert((ctor, args.into()), id);
        Ok(id)
    }

    /// An application atom's structural name: `list[int | bool]`.
    fn apply_name(&self, ctor: TypeId, args: &[Ty]) -> String {
        let ctor_name = self
            .atom_decl(ctor)
            .map_or("<unknown>", |decl| decl.name.as_str());
        let parts: Vec<String> = args.iter().map(|ty| self.render(*ty)).collect();
        format!("{}[{}]", ctor_name, parts.join(", "))
    }

    /// The structural atom of one function type: solved parameter types plus
    /// the solved return type (F2). Hash-consed on the signature, so two
    /// written forms of one signature mint one atom — the identity the
    /// checker decomposes at call sites and lambda checks.
    pub fn fn_atom(&mut self, params: &[Ty], ret: Ty) -> Result<TypeId, Error> {
        let key = (params.to_vec().into(), ret);
        if let Some(&id) = self.fn_index.get(&key) {
            return Ok(id);
        }
        let name = self.fn_name(&key.0, key.1);
        let id = self.push_atom(AtomDecl {
            name,
            kind: AtomKind::FnSig(key.0.clone(), key.1),
        });
        self.fn_index.insert(key, id);
        Ok(id)
    }

    /// A function signature atom's structural name: `fn(int, int) -> string`.
    fn fn_name(&self, params: &[Ty], ret: Ty) -> String {
        let parts: Vec<String> = params.iter().map(|ty| self.render(*ty)).collect();
        format!("fn({}) -> {}", parts.join(", "), self.render(ret))
    }

    /// The signature of a function-type atom, if it is one; the checker's
    /// decomposition point for indirect calls and lambda expectations.
    #[must_use]
    pub fn fn_sig(&self, id: TypeId) -> Option<(&[Ty], Ty)> {
        match self.atom_decl(id).ok()?.kind {
            AtomKind::FnSig(ref params, ret) => Some((params.as_ref(), ret)),
            _ => None,
        }
    }

    /// Every declared atom in declaration order, minted type variables and
    /// structural applications included: the candidate groundings a
    /// coherence probe over generic heads draws from.
    pub fn atom_ids(&self) -> impl Iterator<Item = TypeId> + '_ {
        (0..narrow(self.atoms.len())).map(TypeId::from_raw)
    }

    /// The solved type of an explicit member set: the intern inverse of
    /// [`Universe::members`], for consumers that re-derive a
    /// substitution's solved type at one site (the runtime-evidence
    /// seam over generic impls).
    pub fn set_ty(&mut self, members: &[TypeId]) -> Ty {
        self.intern(members.to_vec())
    }

    // ---- checked-ambient replay ------------------------------------------

    /// The interned member sets in intern order (the checked-ambient
    /// artifact's extraction view): handle `i` names entry `i`, so a
    /// replay of the whole log in order reproduces every handle the
    /// declaring pass minted.
    pub fn set_log(&self) -> impl ExactSizeIterator<Item = &[TypeId]> + '_ {
        self.sets.iter().map(AsRef::as_ref)
    }

    /// The atom count so far: the merged walk's mint base the serve's
    /// landing checks read.
    #[must_use]
    pub fn atom_count(&self) -> u32 {
        narrow(self.atoms.len())
    }

    /// The singleton type of one atom.
    pub fn atom_ty(&mut self, id: TypeId) -> Result<Ty, Error> {
        self.atom_decl(id)?;
        let index = id.index() as usize;
        if let Some(ty) = self.atom_tys[index] {
            return Ok(ty);
        }
        let ty = self.intern_sorted(vec![id]);
        self.atom_tys[index] = Some(ty);
        Ok(ty)
    }

    /// The empty type: the union's identity.
    pub fn empty_ty(&mut self) -> Ty {
        self.intern_sorted(Vec::new())
    }

    /// The full universe as a type: every declared atom. The intersection's
    /// identity for the current universe state.
    pub fn universe_ty(&mut self) -> Ty {
        if let Some(ty) = self.full_ty {
            return ty;
        }
        let all: Vec<TypeId> = (0..narrow(self.atoms.len()))
            .map(TypeId::from_raw)
            .collect();
        let ty = self.intern_sorted(all);
        self.full_ty = Some(ty);
        ty
    }

    // ---- the algebra over solved types -------------------------------------

    /// Union of two solved types. Idempotent, commutative, associative by
    /// set semantics; the empty type is the identity.
    pub fn union(&mut self, a: Ty, b: Ty) -> Ty {
        let merged = union_slices(self.members(a), self.members(b));
        self.intern_sorted(merged)
    }

    /// Intersection of two solved types. Idempotent, commutative,
    /// associative by set semantics; the full universe is the identity.
    pub fn inter(&mut self, a: Ty, b: Ty) -> Ty {
        let common = inter_slices(self.members(a), self.members(b));
        self.intern_sorted(common)
    }

    /// Difference lowers to the solved shape of `a` minus `b`'s members,
    /// exactly `a & ~b`; no difference node survives (B1).
    pub fn diff(&mut self, a: Ty, b: Ty) -> Ty {
        let rest = diff_slices(self.members(a), self.members(b));
        self.intern_sorted(rest)
    }

    /// Complement over the universe's current atoms: `~t`. Member slices
    /// are sorted (see `Universe::intern`), so membership tests binary
    /// search instead of scanning.
    pub fn complement(&mut self, t: Ty) -> Ty {
        let members = self.members(t);
        let rest: Vec<TypeId> = (0..narrow(self.atoms.len()))
            .map(TypeId::from_raw)
            .filter(|atom| members.binary_search(atom).is_err())
            .collect();
        self.intern_sorted(rest)
    }

    /// `T?` is exactly `T | none` (B4); optionals, error unions, and enums
    /// share one mechanism.
    pub fn optional(&mut self, t: Ty) -> Ty {
        let with_none = union_slices(self.members(t), &[TypeId::NONE]);
        self.intern_sorted(with_none)
    }

    /// The tuple product of solved element types: one minted structural
    /// atom per element-atom combination, unioned (B3) — `(a | b, c)` is
    /// `(a, c) | (b, c)`. The empty list of elements is the unit atom
    /// `()`, the arity-0 product with exactly one combination.
    pub fn product(&mut self, elements: &[Ty]) -> Result<Ty, Error> {
        let sizes: Vec<usize> = elements.iter().map(|&t| self.members(t).len()).collect();
        let product = sizes.iter().try_fold(1_usize, |acc, &n| acc.checked_mul(n));
        if product.is_none_or(|p| p > MAX_TUPLE_COMBINATIONS) {
            let rendered: Vec<String> = elements.iter().map(|&t| self.render(t)).collect();
            return Err(Error::TupleDistributionCap {
                tuple: format!("({})", rendered.join(", ")),
                sizes,
                cap: MAX_TUPLE_COMBINATIONS,
            });
        }
        // The combinations are walked by a mixed-radix counter over the
        // element sets, so one combination mints from one buffer.
        let combinations = product.expect("the distribution cap is checked above");
        let mut digits = vec![0_usize; elements.len()];
        let mut combo: Vec<TypeId> = Vec::with_capacity(elements.len());
        let mut atoms = Vec::with_capacity(combinations);
        'combinations: for _ in 0..combinations {
            combo.clear();
            for (position, digit) in digits.iter().enumerate() {
                combo.push(self.members(elements[position])[*digit]);
            }
            atoms.push(self.tuple_atom(&combo)?);
            for position in (0..elements.len()).rev() {
                digits[position] += 1;
                if digits[position] < self.members(elements[position]).len() {
                    continue 'combinations;
                }
                digits[position] = 0;
            }
        }
        // The structural atoms are minted in combination order, so their ids
        // need sorting into the canonical member order.
        atoms.sort_unstable();
        atoms.dedup();
        Ok(self.intern_sorted(atoms))
    }

    /// The unit type `()`: the arity-0 tuple atom, a sibling of `none`
    /// with exactly one inhabitant (B3).
    pub fn unit_ty(&mut self) -> Ty {
        self.product(&[])
            .unwrap_or_else(|_| self.intern_sorted(Vec::new()))
    }

    // ---- decision procedures ------------------------------------------------

    /// A type is empty iff its solved member set is empty (B1). An
    /// intersection of distinct nominal atoms, an atom minus itself, `t &
    /// ~t`, and a union of empty members all reduce to the empty set.
    #[must_use]
    pub fn is_empty(&self, t: Ty) -> bool {
        self.members(t).is_empty()
    }

    /// Disjointness: `a` and `b` are disjoint iff `a & b` is empty (B1).
    #[must_use]
    pub fn is_disjoint(&self, a: Ty, b: Ty) -> bool {
        is_disjoint_slices(self.members(a), self.members(b))
    }

    /// Containment: `contains(sup, sub)` holds iff `sub <= sup`. `?`
    /// propagation checks this against the written return union (B2).
    #[must_use]
    pub fn contains(&self, sup: Ty, sub: Ty) -> bool {
        is_subset(self.members(sub), self.members(sup))
    }

    /// The solved members of a type, sorted and deduplicated.
    #[must_use]
    pub fn members(&self, t: Ty) -> &[TypeId] {
        self.sets[t.0 as usize].as_ref()
    }

    // ---- rendering -----------------------------------------------------------

    /// Renders a solved type as union syntax for diagnostics. The empty set
    /// renders as `empty` (an internal marker; the decided surface has no
    /// empty-type keyword). Members appear in declaration order.
    #[must_use]
    pub fn render(&self, t: Ty) -> String {
        let names: Vec<&str> = self
            .members(t)
            .iter()
            .map(|id| {
                self.atom_decl(*id)
                    .map_or("<unknown>", |decl| decl.name.as_str())
            })
            .collect();
        if names.is_empty() { "empty".to_owned() } else { names.join(" | ") }
    }

    fn intern(&mut self, mut set: Vec<TypeId>) -> Ty {
        set.sort_unstable();
        set.dedup();
        self.intern_sorted(set)
    }

    /// [`Universe::intern`] for a member set the caller already produced
    /// sorted and deduplicated (the sorted-slice algebra's output).
    fn intern_sorted(&mut self, set: Vec<TypeId>) -> Ty {
        debug_assert!(
            set.windows(2).all(|pair| pair[0] < pair[1]),
            "intern_sorted input must be sorted and deduplicated"
        );
        if let Some(&handle) = self.index.get(set.as_slice()) {
            return Ty(handle);
        }
        let handle = narrow(self.sets.len());
        let set: Rc<[TypeId]> = set.into();
        self.sets.push(Rc::clone(&set));
        self.index.insert(set, handle);
        Ty(handle)
    }
}

impl Default for Universe {
    fn default() -> Self {
        Self::new()
    }
}

/// Table index narrowing: every table entry is an allocation, so tables hit
/// memory limits long before u32 overflows.
pub(crate) fn narrow(len: usize) -> u32 {
    u32::try_from(len).expect("table index exceeds u32")
}

/// Sorted-slice union.
fn union_slices(a: &[TypeId], b: &[TypeId]) -> Vec<TypeId> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                out.push(a[i]);
                i += 1;
            },
            Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            },
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            },
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// Sorted-slice intersection.
fn inter_slices(a: &[TypeId], b: &[TypeId]) -> Vec<TypeId> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            },
        }
    }
    out
}

/// Sorted-slice difference `a \ b`.
fn diff_slices(a: &[TypeId], b: &[TypeId]) -> Vec<TypeId> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                out.push(a[i]);
                i += 1;
            },
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                i += 1;
                j += 1;
            },
        }
    }
    out.extend_from_slice(&a[i..]);
    out
}

/// Whether two sorted slices share no element.
fn is_disjoint_slices(a: &[TypeId], b: &[TypeId]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => return false,
        }
    }
    true
}

/// Whether every element of `sub` occurs in `sup` (both sorted).
pub(crate) fn is_subset(sub: &[TypeId], sup: &[TypeId]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < sub.len() && j < sup.len() {
        match sub[i].cmp(&sup[j]) {
            Ordering::Less => return false,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                i += 1;
                j += 1;
            },
        }
    }
    i == sub.len()
}
