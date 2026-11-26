//! # Model
//!
//! Every binding carries a three-state lattice — `State::Uninit`,
//! `State::Live`, `State::Dead` — through one forward walk per body. A
//! *consuming* use of a non-copyable owned binding (a `move`-mode call
//! argument, a struct-literal or tuple field it flows into, a lambda
//! capture, a return value) moves it: the binding dies there and its deinit
//! responsibility transfers with it. Every later use of a dead binding is
//! the two-span [`ErrorCode::MovedThenUsed`] ("moved here" at the move,
//! "used again here after move" at the use). Reads of an uninitialized
//! binding are [`ErrorCode::UninitializedUse`]. Joins (`if`/`else`, match
//! arms, `or`) merge conservatively: a binding is live after a join only
//! when live on every non-diverging path, so a move on any path kills it —
//! a later use really does execute on the moved path. `var` re-assignment
//! re-initializes; the replaced value's death is the assignment itself.

use super::*;

/// Where a binding's value lives during the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    /// Declared, no value yet. Unreachable in the written surface (every
    /// binding form carries an initializer); kept as the lattice's base
    /// so a deferred-initialization form lands on it directly.
    #[allow(dead_code)]
    Uninit,
    /// Holding a value.
    Live,
    /// Moved; `at` is the consuming use.
    Dead {
        /// The move that killed the binding.
        at: Span,
    },
}

/// One tracked binding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Place {
    /// A local binder by its name span.
    Local(Span),
    /// A top-level binding by its package definition.
    Def(DefId),
}

impl Place {
    pub(super) fn binder_span(self, package: &Package) -> Span {
        match self {
            Self::Local(span) => span,
            Self::Def(def) => package.def(def).span,
        }
    }
}

/// Static facts of one tracked binding.
#[derive(Clone, Copy)]
pub(super) struct Info<'a> {
    pub(super) name: &'a str,
    pub(super) copyable: bool,
    /// Whether this frame owns the value (locals, `move` parameters,
    /// top-level bindings); borrows and `mut` windows never move.
    pub(super) owned: bool,
    /// The capture depth at binding time: a binding is outer to every
    /// lambda or spawn body entered after it.
    pub(super) epoch: usize,
}

pub(super) type States = FxHashMap<Place, State>;

/// One branch arm's or loop body's state writes relative to its entry
/// state: every place it wrote, at its final state.
pub(super) type Delta = FxHashMap<Place, State>;

/// One logged state write. Every write a branch, arm, or loop body makes
/// goes through `Lin::set_state`; `Lin::rewind` replays a region's entries
/// newest first, which restores that region's entry state exactly.
#[derive(Clone, Copy)]
pub(super) struct StateUndo {
    pub(super) place: Place,
    /// The state before the write; `None` when the place was untracked.
    pub(super) prev: Option<State>,
}

/// Merges one branch's delta into the accumulator over the join's entry
/// state: a place is live after the join only when live on both sides, and a
/// move on either side wins the recorded span, the accumulator's side first.
///
/// The join reads only places the branches wrote: a place neither wrote keeps
/// its entry state, so it cannot change and never enters the result.
pub(super) fn merge_branch(acc: &mut Delta, entry: &States, branch: &Delta) {
    // Places the accumulator already holds and this branch never wrote: the
    // entry state joins in as the branch side.
    acc.retain(|place, state| match entry.get(place) {
        Some(entry_state)
            if matches!(state, State::Live) && !matches!(entry_state, State::Live) =>
        {
            false
        },
        Some(entry_state) => *entry_state != *state,
        None => true,
    });
    for (place, branch_state) in branch {
        let state = match acc.get(place).or(entry.get(place)) {
            Some(acc_state)
                if matches!(acc_state, State::Live) && !matches!(branch_state, State::Live) =>
            {
                *branch_state
            },
            Some(acc_state) => *acc_state,
            None => continue,
        };
        if entry.get(place) == Some(&state) {
            acc.remove(place);
        } else {
            acc.insert(*place, state);
        }
    }
}
/// Strips parentheses to the expression the walk reads.
pub(super) fn peel(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => peel(inner),
        _ => expr,
    }
}

/// One projection step of an assignment place chain (the assignment
/// sugar), for the affine walk's uses.
pub(super) enum PlaceStep<'a> {
    /// A field projection (no binding use of its own).
    Field,
    /// A subscript projection: the probe expression moves into the
    /// synthesized `set` call.
    Index {
        /// The probe expression.
        index: &'a Expr,
    },
}

/// The root path segment of an assignment place chain, when the left
/// side is one: the binding the chain's mut window opens over.
pub(super) fn place_root(left: &Expr) -> Option<&Ident> {
    let mut recv = peel(left);
    loop {
        match recv {
            Expr::Field { recv: inner, .. } | Expr::Index { recv: inner, .. } => {
                recv = peel(inner);
            },
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                return path.segments.first();
            },
            _ => return None,
        }
    }
}

/// An assignment place chain's projection steps, outermost first.
pub(super) fn place_steps(left: &Expr) -> Vec<PlaceStep<'_>> {
    let mut steps = Vec::new();
    let mut recv = peel(left);
    loop {
        match recv {
            Expr::Field { recv: inner, .. } => {
                steps.push(PlaceStep::Field);
                recv = peel(inner);
            },
            Expr::Index {
                recv: inner, index, ..
            } => {
                steps.push(PlaceStep::Index { index });
                recv = peel(inner);
            },
            _ => return steps,
        }
    }
}
