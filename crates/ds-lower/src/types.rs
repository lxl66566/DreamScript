//! The type lens: the universe as one lowering reads it, and the
//! substitution an instantiation applies.
//!
//! A generic body checks once over its own type variables, so every type
//! the checker recorded for it names those variables. An instantiation
//! reads each of those types substituted — a variable becomes the type its
//! entering site solved, whole span included — and a substituted type is a
//! real solved handle again, so the shape layer and the word addressing
//! lower an instance exactly like a monomorphic body.
//!
//! Minting a substituted type needs a mutable registry and the analyzed
//! package lends only a shared one, so the lens keeps its own copy of the
//! universe, taken at the first mint: every analyzed handle keeps its set
//! there and the substituted types append after them. The plan's discovery
//! substitutes under the mutable half ([`Types`]); the lowering then reads
//! the frozen results through the shape layer, where a type the discovery
//! never substituted refuses instead of lowering a variable's word.

use std::{collections::HashMap, rc::Rc};

use ds_check::{AtomKind, Ty, TypeExpr, TypeId, Universe};

use crate::{error::LowerError, shape::is_var_atom};

/// One instantiation's substitution: the generic body's own variable atoms
/// (the atoms the body's checking minted) to the types the entering site
/// solved, one per generic parameter. Keyed by the atoms themselves, never
/// by the written parameter names: a nested callee's variables may share
/// the names and belong to another body.
pub(crate) struct Subst {
    /// The substitution's identity: the mint cache keys by it.
    pub(crate) id: u32,
    /// Variable atom to its substituted type.
    vars: HashMap<TypeId, Ty>,
}

impl Subst {
    /// The type one variable atom substitutes to.
    pub(crate) fn get(&self, var: TypeId) -> Option<Ty> {
        self.vars.get(&var).copied()
    }

    /// Whether `ty` names one of this substitution's variables, at any
    /// depth.
    pub(crate) fn mentions(&self, universe: &Universe, ty: Ty) -> bool {
        universe
            .members(ty)
            .iter()
            .any(|&atom| self.mentions_atom(universe, atom))
    }

    fn mentions_atom(&self, universe: &Universe, atom: TypeId) -> bool {
        if self.vars.contains_key(&atom) {
            return true;
        }
        match universe.atom_decl(atom).map(|decl| &decl.kind) {
            Ok(AtomKind::Apply(_, args)) => args.iter().any(|&arg| self.mentions(universe, arg)),
            Ok(AtomKind::Tuple(elements)) => elements
                .iter()
                .any(|&element| self.mentions_atom(universe, element)),
            Ok(AtomKind::FnSig(params, ret)) => {
                params.iter().any(|&param| self.mentions(universe, param))
                    || self.mentions(universe, *ret)
            },
            _ => false,
        }
    }
}

/// The universe as one lowering reads it.
enum Uni<'a> {
    /// The analyzed package's own registry, while nothing minted.
    Analyzed(&'a Universe),
    /// The lowering's copy, once a substitution minted a type — boxed
    /// because the registry dwarfs the borrowed arm.
    Owned(Box<Universe>),
}

/// The mutable half of the lens: the plan's discovery substitutes under
/// it, and the lowering reads what it minted.
pub(crate) struct Types<'a> {
    uni: Uni<'a>,
    next_id: u32,
    /// Types a substitution resolved, keyed by (substitution, source
    /// type).
    minted: HashMap<(u32, Ty), Ty>,
    /// The solved field signature of every application atom a
    /// substitution minted, derived from its source's own entry — the
    /// checker's recorded fields when the source atom is one it minted,
    /// else an outer substitution's derivation. Keyed by the minted
    /// atom; the analyzed package's own entries live on its tables.
    apply_structs: HashMap<TypeId, Rc<[ds_check::decls::SolvedField]>>,
    /// The analyzed package's own solved signatures, the derivation's
    /// source table for the atoms the checker minted.
    analyzed_apply_structs: &'a HashMap<TypeId, Rc<[ds_check::decls::SolvedField]>>,
    /// The per-case facts of every enum application atom a
    /// substitution minted, derived from its source's own record —
    /// the minted case atoms substituted per the same rule.
    apply_enums: HashMap<TypeId, Rc<[ds_check::decls::AppliedCase]>>,
    /// The payloaded case atoms each substitution minted, keyed by
    /// (substitution, the source record's own case atom): the arm
    /// and construction reads map the checked record's atom to the
    /// instance's own.
    case_mints: HashMap<(u32, TypeId), TypeId>,
    /// The analyzed package's own per-case facts, the derivation's
    /// source table for the applications the checker minted.
    analyzed_apply_enums: &'a HashMap<TypeId, Rc<[ds_check::decls::AppliedCase]>>,
}

impl<'a> Types<'a> {
    /// The lens over the analyzed package's registry and its own solved
    /// application signatures.
    pub(crate) fn new(
        universe: &'a Universe,
        analyzed_apply_structs: &'a HashMap<TypeId, Rc<[ds_check::decls::SolvedField]>>,
        analyzed_apply_enums: &'a HashMap<TypeId, Rc<[ds_check::decls::AppliedCase]>>,
    ) -> Self {
        Self {
            uni: Uni::Analyzed(universe),
            next_id: 1,
            minted: HashMap::new(),
            apply_structs: HashMap::new(),
            analyzed_apply_structs,
            apply_enums: HashMap::new(),
            case_mints: HashMap::new(),
            analyzed_apply_enums,
        }
    }

    /// The registry as read so far.
    pub(crate) fn universe(&self) -> &Universe {
        match &self.uni {
            Uni::Analyzed(universe) => universe,
            Uni::Owned(universe) => universe,
        }
    }

    /// Every substituted type the discovery resolved, for the frozen read.
    pub(crate) fn minted(&self) -> &HashMap<(u32, Ty), Ty> {
        &self.minted
    }

    /// A fresh substitution over the given variable bindings.
    pub(crate) fn subst(&mut self, vars: HashMap<TypeId, Ty>) -> Rc<Subst> {
        let id = self.next_id;
        self.next_id += 1;
        Rc::new(Subst { id, vars })
    }

    /// The frozen read: the type `ty` becomes under `subst` when the
    /// discovery already substituted it, `None` when it did not.
    pub(crate) fn substituted(&self, subst: &Subst, ty: Ty) -> Option<Ty> {
        self.minted.get(&(subst.id, ty)).copied()
    }

    /// The substitution's own derived signatures, the frozen read's
    /// first table.
    pub(crate) fn apply_map(&self) -> &HashMap<TypeId, Rc<[ds_check::decls::SolvedField]>> {
        &self.apply_structs
    }

    /// The substitution's own derived case records, the frozen read's
    /// first table over the analyzed package's own.
    pub(crate) fn apply_enum_map(&self) -> &HashMap<TypeId, Rc<[ds_check::decls::AppliedCase]>> {
        &self.apply_enums
    }

    /// The case atoms each substitution minted, keyed by
    /// (substitution, the source record's own atom).
    pub(crate) fn case_mint_map(&self) -> &HashMap<(u32, TypeId), TypeId> {
        &self.case_mints
    }

    /// The solved field signature one application atom carries: the
    /// substitution's own derivations first, the analyzed package's
    /// entries under them.
    pub(crate) fn apply_fields(&self, atom: TypeId) -> Option<Rc<[ds_check::decls::SolvedField]>> {
        self.apply_structs
            .get(&atom)
            .cloned()
            .or_else(|| self.analyzed_apply_structs.get(&atom).cloned())
    }

    /// The per-case facts one enum application atom carries: the
    /// substitution's own derivations first, the analyzed package's
    /// records under them.
    pub(crate) fn apply_cases(&self, atom: TypeId) -> Option<Rc<[ds_check::decls::AppliedCase]>> {
        self.apply_enums
            .get(&atom)
            .cloned()
            .or_else(|| self.analyzed_apply_enums.get(&atom).cloned())
    }

    /// Records the substituted signature of one minted application: the
    /// source application's fields under the same substitution. A
    /// source naming no declared struct (a heap container) records
    /// nothing, its shape argument-independent.
    fn derive_apply_fields(
        &mut self,
        applied: TypeId,
        ctor: TypeId,
        subst: &Subst,
    ) -> Result<(), LowerError> {
        if self.apply_structs.contains_key(&applied) {
            return Ok(());
        }
        let Some(fields) = self.apply_fields(ctor) else {
            return Ok(());
        };
        let mut derived = Vec::with_capacity(fields.len());
        for field in fields.iter() {
            derived.push(ds_check::decls::SolvedField {
                name: field.name.clone(),
                ty: self.substitute(subst, field.ty)?,
            });
        }
        self.apply_structs.insert(applied, derived.into());
        Ok(())
    }

    /// Records the per-case facts of one minted enum application and the
    /// case-atom mapping its arm records read: the source application's
    /// own cases under the same substitution — a bare case keeps its
    /// shared unit atom, a payloaded case mints its own atom over the
    /// substituted slots. The substituted application's record answers
    /// from either table first (the checker minted the concrete
    /// application's own case atoms when a site's written types solved
    /// it), so the mapping points at whichever record the shape layer
    /// reads.
    fn derive_apply_enum_cases(
        &mut self,
        applied: TypeId,
        source_app: TypeId,
        subst: &Subst,
    ) -> Result<(), LowerError> {
        let Some(source) = self.apply_cases(source_app) else {
            return Ok(());
        };
        let target = if let Some(cases) = self.apply_enums.get(&applied).cloned() {
            cases
        } else if let Some(cases) = self.analyzed_apply_enums.get(&applied).cloned() {
            cases
        } else {
            let cases = self.derive_enum_cases(&source, subst)?;
            self.apply_enums.insert(applied, cases.clone());
            cases
        };
        for (from, to) in source.iter().zip(target.iter()) {
            self.case_mints.insert((subst.id, from.atom), to.atom);
        }
        Ok(())
    }

    /// One substituted application's case list: each source case's slots
    /// substituted, its payloaded cases minting fresh atoms.
    fn derive_enum_cases(
        &mut self,
        source: &Rc<[ds_check::decls::AppliedCase]>,
        subst: &Subst,
    ) -> Result<Rc<[ds_check::decls::AppliedCase]>, LowerError> {
        let mut cases = Vec::with_capacity(source.len());
        for case in source.iter() {
            let Some(slots) = &case.slots else {
                // a bare case keeps its shared unit atom: a unit word
                // carries no substitution
                cases.push(ds_check::decls::AppliedCase {
                    def: case.def,
                    atom: case.atom,
                    slots: None,
                });
                continue;
            };
            let mut solved = Vec::with_capacity(slots.len());
            for slot in slots.iter() {
                solved.push(ds_check::decls::AppliedSlot {
                    name: slot.name.clone(),
                    ty: self.substitute(subst, slot.ty)?,
                });
            }
            let label = self
                .universe()
                .atom_decl(case.atom)
                .map_or_else(|_| "case".to_owned(), |decl| decl.name.clone());
            let atom = self.universe_mut().mint_payload_tag(&label);
            let fields = solved
                .iter()
                .map(|slot| ds_check::Field {
                    name: slot.name.clone(),
                    ty: Self::expr_of_members(self.universe(), slot.ty),
                })
                .collect();
            let _ = self.universe_mut().set_struct_fields(atom, fields);
            cases.push(ds_check::decls::AppliedCase {
                def: case.def,
                atom,
                slots: Some(solved.into()),
            });
        }
        Ok(cases.into())
    }

    /// The solved type re-expressed as the union-of-atoms [`TypeExpr`] a
    /// minted case atom's field signature carries, over the lens's own
    /// registry.
    fn expr_of_members(universe: &Universe, ty: Ty) -> TypeExpr {
        let mut expr: Option<TypeExpr> = None;
        for atom in universe.members(ty) {
            expr = Some(match expr {
                None => TypeExpr::atom(*atom),
                Some(e) => e.union((*atom).into()),
            });
        }
        expr.unwrap_or(TypeExpr::Empty)
    }

    fn universe_mut(&mut self) -> &mut Universe {
        if let Uni::Analyzed(universe) = &self.uni {
            self.uni = Uni::Owned(Box::new((*universe).clone()));
        }
        match &mut self.uni {
            Uni::Owned(universe) => universe,
            Uni::Analyzed(..) => unreachable!("the copy is taken above"),
        }
    }

    /// Whether an atom is a variable or carries one inside an application,
    /// a tuple, or a function type.
    fn atom_has_variables(universe: &Universe, atom: TypeId) -> bool {
        if is_var_atom(universe, atom) {
            return true;
        }
        match universe.atom_decl(atom).map(|decl| &decl.kind) {
            Ok(AtomKind::Apply(_, args)) => args.iter().any(|&arg| has_variables(universe, arg)),
            Ok(AtomKind::Tuple(elements)) => elements
                .iter()
                .any(|&element| Self::atom_has_variables(universe, element)),
            Ok(AtomKind::FnSig(params, ret)) => {
                params.iter().any(|&param| has_variables(universe, param))
                    || has_variables(universe, *ret)
            },
            _ => false,
        }
    }

    /// One tuple element under `subst`: a variable element becomes its
    /// solved type's single member — a tuple element over a variable whose
    /// solved type is a union has no one atom to take.
    fn element_under(&mut self, subst: &Subst, element: TypeId) -> Result<TypeId, LowerError> {
        if let Some(bound) = subst.get(element) {
            let [only] = self.universe().members(bound) else {
                return Err(unsubstituted());
            };
            return Ok(*only);
        }
        if subst.mentions_atom(self.universe(), element) {
            return Err(unsubstituted());
        }
        Ok(element)
    }

    fn args_under(&mut self, subst: &Subst, args: &[Ty]) -> Result<Vec<Ty>, LowerError> {
        args.iter()
            .map(|&arg| self.substitute(subst, arg))
            .collect()
    }

    /// The type `ty` becomes under `subst`: `ty` itself when it names none
    /// of the substitution's variables, else the solved type over the
    /// substituted members — a variable expands to its type's whole member
    /// set, and an application, a tuple, or a function type carrying a
    /// variable is replaced by the structural atom over the substituted
    /// arguments (minted when the package never solved it).
    pub(crate) fn substitute(&mut self, subst: &Subst, ty: Ty) -> Result<Ty, LowerError> {
        if let Some(&hit) = self.minted.get(&(subst.id, ty)) {
            return Ok(hit);
        }
        if !subst.mentions(self.universe(), ty) {
            return Ok(ty);
        }
        let source = self.universe().members(ty).to_vec();
        let mut members = Vec::with_capacity(source.len());
        for atom in source {
            if let Some(bound) = subst.get(atom) {
                members.extend_from_slice(self.universe().members(bound));
                continue;
            }
            let kind = match self.universe().atom_decl(atom) {
                Ok(decl) => decl.kind.clone(),
                Err(_) => return Err(unsubstituted()),
            };
            let structural = match &kind {
                AtomKind::Apply(ctor, args)
                    if args.iter().any(|&arg| subst.mentions(self.universe(), arg)) =>
                {
                    let args = self.args_under(subst, args)?;
                    let applied = self
                        .universe_mut()
                        .apply_atom(*ctor, &args)
                        .map_err(|_| unsubstituted())?;
                    self.derive_apply_fields(applied, *ctor, subst)?;
                    self.derive_apply_enum_cases(applied, atom, subst)?;
                    Some(applied)
                },
                AtomKind::Tuple(elements)
                    if elements
                        .iter()
                        .any(|&element| subst.mentions_atom(self.universe(), element)) =>
                {
                    let elements = elements
                        .iter()
                        .map(|&element| self.element_under(subst, element))
                        .collect::<Result<Vec<_>, _>>()?;
                    Some(
                        self.universe_mut()
                            .tuple_atom(&elements)
                            .map_err(|_| unsubstituted())?,
                    )
                },
                AtomKind::FnSig(params, ret)
                    if subst.mentions(self.universe(), *ret)
                        || params
                            .iter()
                            .any(|&param| subst.mentions(self.universe(), param)) =>
                {
                    let params = self.args_under(subst, params)?;
                    let ret = self.substitute(subst, *ret)?;
                    Some(
                        self.universe_mut()
                            .fn_atom(&params, ret)
                            .map_err(|_| unsubstituted())?,
                    )
                },
                _ => None,
            };
            members.push(structural.unwrap_or(atom));
        }
        let substituted = self.universe_mut().set_ty(&members);
        self.minted.insert((subst.id, ty), substituted);
        Ok(substituted)
    }
}

/// Whether a type names a variable atom anywhere inside it: no concrete
/// substitution is recorded for it yet, and an argument still naming one
/// cannot shape a body by itself.
pub(crate) fn has_variables(universe: &Universe, ty: Ty) -> bool {
    universe
        .members(ty)
        .iter()
        .any(|&atom| Types::atom_has_variables(universe, atom))
}

/// The one refusal of a substitution that cannot complete: a member no
/// declaration backs, or a tuple element over a variable whose solved type
/// is a union.
fn unsubstituted() -> LowerError {
    LowerError::unsupported(
        ds_diag::Span::new(ds_diag::FileId::new(0), 0, 0),
        "a substituted type",
        "one of its members has no declaration, or a tuple element's solved type is a union, \
         which names no single atom",
    )
}

#[cfg(test)]
mod tests {
    use ds_check::{Field, TypeExpr};

    use super::*;

    /// The registry reads as the analyzed one until a substitution mints;
    /// afterwards every analyzed handle keeps its set, a variable becomes
    /// its solved type, and a union over the variable solves over the
    /// substituted members.
    #[test]
    fn a_substituted_type_mints_into_the_lowering_s_own_universe() {
        let mut universe = Universe::new();
        let io = universe.declare_tag("io_err").expect("io_err declares");
        let point = universe.declare_struct("point").expect("point declares");
        universe
            .set_struct_fields(point, vec![Field::named("x", TypeExpr::atom(TypeId::NONE))])
            .expect("point's fields set");
        let var = universe.mint_tag("$t#1");
        let io_ty = universe.atom_ty(io).expect("io_err solves");
        let point_ty = universe.atom_ty(point).expect("point solves");
        let var_ty = universe.atom_ty(var).expect("the variable solves");
        let optional = universe.union(var_ty, io_ty);
        let expected = universe.union(point_ty, io_ty);

        let empty_apply = HashMap::new();
        let empty_enum_apply = HashMap::new();
        let mut types = Types::new(&universe, &empty_apply, &empty_enum_apply);
        let subst = types.subst(HashMap::from([(var, point_ty)]));
        // a type naming no substituted variable is itself, and nothing
        // minted yet
        assert_eq!(types.substitute(&subst, io_ty), Ok(io_ty));
        assert!(types.minted().is_empty());
        // the variable becomes its solved type
        assert_eq!(types.substitute(&subst, var_ty), Ok(point_ty));
        // the union over it solves over the substituted members
        let solved = types
            .substitute(&subst, optional)
            .expect("the union substitutes");
        assert_eq!(types.universe().members(solved), universe.members(expected));
        // the analyzed registry is untouched, and the frozen read answers
        // exactly what minted
        assert_eq!(universe.members(optional), [io, var]);
        assert_eq!(types.substituted(&subst, optional), Some(solved));
        assert_eq!(types.substituted(&subst, io_ty), None);
    }
}
