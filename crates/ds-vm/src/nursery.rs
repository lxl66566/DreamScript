//! Nurseries [E2, E5]: the join, cancellation, and boundary-error machinery
//! over the fiber pool. A nursery is interpreter data like a fiber — a pool
//! slot holding its children (in spawn order), a per-child outcome ledger,
//! and a cancel flag. Nothing here schedules: the pool is single-threaded,
//! and the driving discipline is fixed below so every observable follows
//! from spawn order alone.
//!
//! # Determinism rules
//!
//! - Spawn order is the total order. Children are recorded in spawn order, the join/teardown driver
//!   enters them in spawn order (sequential: each child is driven to death before the next is
//!   entered), and outcomes are reported in spawn order.
//! - First failure is spawn-drive order: the first child that dies trapped raises the cancel flag;
//!   children driven later observe it at their next taken backward branch and stop there [E5].
//! - A child that parks at a host-visible yield during a join/teardown drive is re-entered with the
//!   bare `PLATFORM_NONE` bounce tag — the same convention a `run` entry follows.
//! - A child's own resumer links stay live under the driver: in-language parent/child switches
//!   inside the nursery keep their answers.
//!
//! # Cancellation [E5]
//!
//! Cooperative and nursery-scoped: the flag is observed only at taken
//! backward branches (the dispatch loop's preemption points), so a linear
//! consume in flight is never interrupted — a child with no backward branch
//! runs its straight-line work to completion and lands normally under the
//! flag. A child stopped at a preemption point dies with the
//! [`crate::native::PLATFORM_CANCELLED`] sentinel; a child that completed
//! normally after the flag keeps its value — the boundary reports which
//! children failed, which were cancelled, and which landed.

use crate::{fiber::FiberId, value::Value, vm::Trap};

/// A nursery's identity in the VM pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NurseryId(pub u32);

impl NurseryId {
    /// The pool index this id names.
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

/// Where a nursery stands: open and accepting spawns, or consumed (its slot
/// is reuse-ready). A consumed nursery has no live children — the driver
/// drove them all to death before closing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NurseryState {
    /// Accepting spawns; joinable.
    Open,
    /// Consumed by a join or teardown; the slot is spawn-reusable.
    Dead,
}

/// One child's fate at a nursery boundary, in spawn order.
#[derive(Clone, Debug, PartialEq)]
pub enum ChildOutcome {
    /// Ran to its entry RET/HALT with this final value — including a child
    /// that landed normally under the cancel flag (cooperative).
    Completed(Value),
    /// Stopped at a taken backward branch under the cancel flag; its death
    /// value to any resumer is the `PLATFORM_CANCELLED` sentinel.
    Cancelled,
    /// Died trapped; the trap that killed it, unioned out at the boundary.
    Trapped(Trap),
}

/// The join result at a nursery boundary: every child joined, or the
/// first-failure cancellation record [E2].
#[derive(Clone, Debug, PartialEq)]
pub enum JoinOutcome {
    /// Every child completed; final values in spawn order.
    Joined(Vec<Value>),
    /// At least one child died trapped: per-child outcomes in spawn order
    /// (`Trapped` for the failures, `Cancelled` for siblings stopped at a
    /// preemption point, `Completed` for those that landed).
    Failed(Vec<ChildOutcome>),
}

/// One pooled nursery: its children in spawn order, the parallel outcome
/// ledger (one slot per child; `None` = still live), and the cancel flag.
/// Outcomes are written at each child's death — whoever drives it — so the
/// boundary collects a complete record.
#[derive(Debug)]
pub(crate) struct Nursery {
    pub(crate) children: Vec<FiberId>,
    pub(crate) outcomes: Vec<Option<ChildOutcome>>,
    pub(crate) cancelled: bool,
    pub(crate) state: NurseryState,
}

impl Nursery {
    /// An empty open nursery.
    pub(crate) fn open() -> Self {
        Self {
            children: Vec::new(),
            outcomes: Vec::new(),
            cancelled: false,
            state: NurseryState::Open,
        }
    }

    /// Records a spawn: the child list is the spawn-order ledger.
    pub(crate) fn push_child(&mut self, child: FiberId) {
        self.children.push(child);
        self.outcomes.push(None);
    }

    /// Ledgers a child's death at its spawn position. Called from every
    /// death path (completion, cancellation, trap), whichever driver ran it.
    pub(crate) fn record(&mut self, child: FiberId, outcome: ChildOutcome) {
        let at = self
            .children
            .iter()
            .position(|&c| c == child)
            .expect("a ledgered fiber is this nursery's child");
        self.outcomes[at] = Some(outcome);
    }
}
