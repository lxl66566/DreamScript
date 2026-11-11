//! Nominal atoms and declaration data (B1, B3), plus structural atoms —
//! anonymous tuples and generic applications (B3).
//!
//! Declarations are modeled as data; resolution from AST is later work. Every
//! tag, struct, and enum gets one [`TypeId`] when declared. Structs carry
//! field signatures, enums carry case signatures, unit tags carry nothing.
//! Anonymous tuples and generic applications are not declared: the universe
//! mints one structural atom per distinct shape on demand.

use std::collections::BTreeSet;

use crate::{expr::TypeExpr, universe::Ty};

/// Identity of an atom (a nominal tag, struct, or enum, or a structural
/// tuple minted by the universe) inside one [`crate::Universe`]. Distinct
/// atoms denote disjoint value sets (B1) — for tuples because solving
/// distributes products over unions element-wise (crate root), so distinct
/// tuple atoms differ in some element atom; [`TypeId::NONE`] is the
/// distinguished `none` unit atom (B4).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TypeId(u32);

impl TypeId {
    /// The distinguished `none` unit atom (B4); every universe declares it
    /// first, so it is also the earliest-declared atom of any universe.
    pub const NONE: Self = Self(0);

    /// Builds an id from a raw table index (serialization, tests). Ids are
    /// only meaningful inside the universe that allocated them.
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index of the atom inside its universe; also its declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Identity of a trait inside one [`crate::Universe`]. A trait denotes the
/// set of its member atoms (B6); marker traits are used as negated
/// constraints (`~Copyable`) (C1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TraitId(u32);

impl TraitId {
    /// Builds an id from a raw table index (serialization, tests). Ids are
    /// only meaningful inside the universe that allocated them.
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index of the trait inside its universe; also its declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// One field of a struct or an enum case.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    /// Field name; `None` marks a positional (tuple-case) payload slot (B3).
    pub name: Option<String>,
    /// Declared field type, as written.
    pub ty: TypeExpr,
}

impl Field {
    /// A named field.
    #[must_use]
    pub fn named(name: &str, ty: TypeExpr) -> Self {
        Self {
            name: Some(name.to_owned()),
            ty,
        }
    }

    /// A positional payload slot (tuple enums) (B3).
    #[must_use]
    pub fn positional(ty: TypeExpr) -> Self {
        Self { name: None, ty }
    }
}

/// One case of an enum, with its payload signature (B3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Case {
    /// Case name; uniqueness inside the enum is the checker's validation.
    pub name: String,
    /// Payload fields; empty for a bare case.
    pub fields: Vec<Field>,
}

impl Case {
    /// A case with the given payload fields.
    #[must_use]
    pub fn new(name: &str, fields: Vec<Field>) -> Self {
        Self {
            name: name.to_owned(),
            fields,
        }
    }
}

/// The declared shape of an atom.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AtomKind {
    /// A unit tag: `tag io_err` — inhabited by the tag itself; `none` is the
    /// distinguished one (B4).
    Unit,
    /// Identity allocated, shape still pending; whole-package fixpoint
    /// resolution fills it (G3). Witnessing a reserved atom is an error.
    Reserved,
    /// A nominal struct carrying field signatures (B1, B3).
    Struct(Vec<Field>),
    /// A nominal enum carrying case signatures; tuple-enums allowed (B3).
    Enum(Vec<Case>),
    /// An anonymous tuple's structural shape (B3): the element atoms, one per
    /// position. Each element is a single atom because solving distributes
    /// tuple products over unions element-wise (crate root), so equality of
    /// tuple atoms is structural: the universe hash-conses on the element
    /// list, and two tuple atoms are equal exactly when their elements match
    /// position-wise. The empty tuple `()` is the arity-0 atom with exactly
    /// one inhabitant — a sibling of `none`, not an alias: the two are
    /// distinct atoms and never equal values.
    Tuple(Vec<TypeId>),
    /// A generic application's structural shape: a declared constructor atom
    /// plus its argument types as solved sets (`list[int | bool]`). Unlike
    /// tuple atoms, applications are OPAQUE to the set algebra — a container
    /// does not distribute over a union argument — so equality is structural
    /// on `(constructor, arguments)` and distinct application atoms denote
    /// distinct value sets by construction. Inhabited, but with no value
    /// shape the type core can show; witnesses render the structural name.
    Apply(TypeId, Box<[Ty]>),
    /// A function type's structural shape: parameter types plus the return
    /// type (`fn(int) -> string`) (F2 "functions are values"). Opaque to the
    /// set algebra exactly like applications: equality is structural, one
    /// atom per distinct signature, and a function value is never a member
    /// of any data atom. The checker decomposes the signature at call sites
    /// and lambda checks; the core only carries the identity.
    FnSig(Box<[Ty]>, Ty),
}

/// A declared atom: name plus shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AtomDecl {
    /// Declared name, unique across atoms and traits in one universe (both
    /// appear in type positions, so they share one namespace).
    pub name: String,
    /// Declared shape.
    pub kind: AtomKind,
}

/// A declared trait: a name plus its member atoms (B6).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TraitDecl {
    /// Declared name, unique across atoms and traits in one universe.
    pub name: String,
    /// Atoms implementing the trait. Membership is a set, so re-inserting an
    /// existing member is a no-op; head-level coherence is
    /// [`crate::instance`]'s pairwise disjointness check.
    pub members: BTreeSet<TypeId>,
}
