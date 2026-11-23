//! Surface type expressions over atoms and trait constraints (B1).
//!
//! Expressions are the durable written form (field signatures, annotations).
//! [`crate::Universe::solve`] evaluates them into canonical solved types;
//! difference and negation nodes never survive solving, and tuple products
//! distribute over unions element-wise (B3).

use crate::atom::{TraitId, TypeId};

/// A type expression: unions, intersections, differences of nominal atoms,
/// plus trait constraints (B1). [`TypeExpr::Not`] is the complement of a
/// marker trait such as `~Copyable` (C1); [`TypeExpr::Diff`] is compile-time
/// only and erases entirely (B1).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypeExpr {
    /// A nominal atom by identity.
    Atom(TypeId),
    /// A trait used as a type: the set of its member atoms (B1).
    Trait(TraitId),
    /// A negated constraint: the complement of a trait's members (C1).
    Not(TraitId),
    /// Union.
    Union(Box<Self>, Box<Self>),
    /// Intersection.
    Inter(Box<Self>, Box<Self>),
    /// Difference `a & ~b`; erases at solving (B1).
    Diff(Box<Self>, Box<Self>),
    /// An anonymous tuple `(t0, t1, ...)` (B3). Solving distributes the
    /// product over union elements into one structural atom per combination,
    /// so `(a | b, c)` solves to `(a, c) | (b, c)`.
    Tuple(Vec<Self>),
}

impl TypeExpr {
    /// An atom reference.
    #[must_use]
    pub fn atom(id: TypeId) -> Self {
        Self::Atom(id)
    }

    /// A trait used as a type.
    #[must_use]
    pub fn trait_ref(id: TraitId) -> Self {
        Self::Trait(id)
    }

    /// A negated trait constraint.
    #[must_use]
    pub fn not(id: TraitId) -> Self {
        Self::Not(id)
    }

    /// `self | other`.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self::Union(Box::new(self), Box::new(other))
    }

    /// `self & other`.
    #[must_use]
    pub fn inter(self, other: Self) -> Self {
        Self::Inter(Box::new(self), Box::new(other))
    }

    /// `self - other`.
    #[must_use]
    pub fn diff(self, other: Self) -> Self {
        Self::Diff(Box::new(self), Box::new(other))
    }

    /// An anonymous tuple of the given elements (B3).
    #[must_use]
    pub fn tuple(elements: Vec<Self>) -> Self {
        Self::Tuple(elements)
    }
}

impl From<TypeId> for TypeExpr {
    fn from(id: TypeId) -> Self {
        Self::Atom(id)
    }
}
