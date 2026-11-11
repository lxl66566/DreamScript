//! Typed errors for malformed type data and universe misuse.

use ds_diag::ErrorCode;

use crate::{
    atom::{TraitId, TypeId},
    instance::InstanceId,
    universe::Ty,
    witness::Witness,
};

/// Errors raised by the type core. These are library-level invariants (bad
/// ids, duplicate names, unfinished declarations); the checker maps them onto
/// user-facing diagnostics with spans, names, and the B6 head pair.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Error {
    /// A type id outside the universe's atom table.
    #[error("type id {0:?} is not declared in this universe")]
    UnknownAtom(TypeId),
    /// A trait id outside the universe's trait table.
    #[error("trait id {0:?} is not declared in this universe")]
    UnknownTrait(TraitId),
    /// A declaration reusing an existing name. Atoms and traits share one
    /// namespace because both appear in type positions.
    #[error("name `{0}` is already declared")]
    DuplicateName(String),
    /// A shape update on an atom that already carries one.
    #[error("atom {0:?} already has a declared shape")]
    ShapeAlreadySet(TypeId),
    /// A shape update that does not match the atom's declared kind.
    #[error("shape update does not match the declared kind of atom {0:?}")]
    WrongAtomKind(TypeId),
    /// Witnessing an atom whose shape is still pending.
    #[error("atom {0:?} is reserved but not yet shaped")]
    IncompleteAtom(TypeId),
    /// Witnessing a type with no shallowly inhabited member.
    #[error("type {0:?} has no inhabitant to witness")]
    WitnessOfEmpty(Ty),
    /// An instance id outside the environment's table.
    #[error("instance id {0:?} is not declared in this environment")]
    UnknownInstance(InstanceId),
    /// Two instances of one trait have intersecting heads (B6). `head_a` is
    /// the earlier-declared head (at declaration, the existing instance's).
    #[error("instances of trait {trait_id:?} overlap: {head_a:?} & {head_b:?}")]
    InstanceOverlap {
        /// The trait both instances implement.
        trait_id: TraitId,
        /// The earlier-declared overlapping head.
        head_a: Ty,
        /// The later-declared overlapping head.
        head_b: Ty,
        /// A witness value type in the intersection.
        witness: Witness,
    },
    /// A conditional instance's condition resolution reaches back to itself,
    /// directly or transitively (B6, section 10 item 7). The path starts and
    /// ends at the same instance.
    #[error("conditional instance cycle: {cycle:?}")]
    ConditionCycle {
        /// The cycle path in edge order, start repeated at the end.
        cycle: Vec<InstanceId>,
    },
    /// Instance lookup found multiple heads containing one query (B6); with
    /// coherence enforced this is impossible for non-empty queries.
    #[error("ambiguous instances of trait {trait_id:?} for {queried:?}")]
    AmbiguousResolution {
        /// The trait queried.
        trait_id: TraitId,
        /// The queried type.
        queried: Ty,
        /// The competing instances, in declaration order.
        candidates: Vec<InstanceId>,
    },
    /// An instance branch head solved to the empty type.
    #[error("instance head {0:?} is empty")]
    EmptyInstanceHead(Ty),
    /// A tuple distribution whose combination count exceeds the solver's
    /// cap (B3); names the tuple and each element position's member count.
    /// Checked before any atom is minted, so an adversarial product is this
    /// error, never an out-of-memory abort.
    #[error(
        "tuple {tuple} distributes into more than {cap} combinations (element sizes {sizes:?})"
    )]
    TupleDistributionCap {
        /// The tuple type, rendered element-wise.
        tuple: String,
        /// Solved member count per element position.
        sizes: Vec<usize>,
        /// The solver's combination cap.
        cap: usize,
    },
}

impl Error {
    /// Closest user-facing diagnostic code; the checker layers precise spans
    /// and names when rendering.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::UnknownAtom(_) | Self::UnknownTrait(_) | Self::IncompleteAtom(_) => {
                ErrorCode::UnresolvedName
            },
            Self::DuplicateName(_) | Self::ShapeAlreadySet(_) | Self::WrongAtomKind(_) => {
                ErrorCode::DuplicateDefinition
            },
            Self::WitnessOfEmpty(_) | Self::InstanceOverlap { .. } => ErrorCode::InstanceOverlap,
            Self::UnknownInstance(_) => ErrorCode::UnresolvedName,
            Self::ConditionCycle { .. } => ErrorCode::ConditionCycle,
            Self::AmbiguousResolution { .. } => ErrorCode::AmbiguousInstance,
            Self::EmptyInstanceHead(_) => ErrorCode::EmptyInstanceHead,
            Self::TupleDistributionCap { .. } => ErrorCode::TupleDistributionCap,
        }
    }
}
