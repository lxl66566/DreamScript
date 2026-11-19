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
//!   bare `PLATFORM_NONE` bounce tag — the same convention a `run` entry follows — unless the
//!   nursery's cancel flag is raised: the re-entry then delivers the cut (the resume-shaped
//!   preemption point), so a parked child of a cancelled nursery never runs another instruction.
//! - A child's own resumer links stay live under the driver: in-language parent/child switches
//!   inside the nursery keep their answers.
//! - A trap escaping a child's drive is attributed to the fiber the drive ended on: a child parked
//!   at its own `fiber_resume` call while its resumee traps is innocent — the resumee ledgers
//!   `Trapped`, the parked child is bounced back in and lands (or reports `Cancelled` if the
//!   boundary cut it down mid-park).
//!
//! # Cancellation [E5]
//!
//! Cooperative and nursery-scoped: the flag is observed at taken
//! backward branches (the dispatch loop's preemption points) and at the
//! join/teardown driver's re-entry of a parked child (the resume-shaped
//! point), so a linear consume in flight is never interrupted — a child
//! with no backward branch runs its straight-line work to completion and
//! lands normally under the flag, while a child parked at a switch point
//! is cut at its next resume, bounding a yield loop with no backward jump
//! left between parks. A child stopped at a preemption point dies with the
//! [`crate::native::PLATFORM_CANCELLED`] sentinel; a child that completed
//! normally after the flag keeps its value — the boundary reports which
//! children failed, which were cancelled, and which landed.

use crate::{
    fiber::FiberId,
    vm::{FrameAnswer, Trap},
};

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
    /// Ran to its entry RET/HALT with this final answer span — one word
    /// for a scalar body, the whole span for a multi-word RETSPAN or
    /// tail-native answer (the fiber completion channel) — including a
    /// child that landed normally under the cancel flag (cooperative).
    Completed(FrameAnswer),
    /// Stopped under the cancel discipline: at a taken backward branch
    /// under the flag or at the driver's re-entry of its park (the
    /// resume-shaped preemption point; its death value to any resumer is
    /// the `PLATFORM_CANCELLED` sentinel), or cut down by the boundary
    /// walk before any ledgered death.
    Cancelled,
    /// Died trapped; the trap that killed it, unioned out at the boundary.
    Trapped(Trap),
}

/// The join result at a nursery boundary: every child joined, or the
/// first-failure cancellation record [E2].
#[derive(Clone, Debug, PartialEq)]
pub enum JoinOutcome {
    /// Every child completed; the answer spans in spawn order.
    Joined(Vec<FrameAnswer>),
    /// At least one child did not complete — a trap, or a boundary
    /// cancellation: per-child outcomes in spawn order (`Trapped` for the
    /// failures, `Cancelled` for siblings stopped under the flag or cut
    /// down by the boundary, `Completed` for those that landed).
    Failed(Vec<ChildOutcome>),
}

/// A pool slot that is no nursery child.
const NOT_A_CHILD: u32 = u32::MAX;

/// One pooled nursery: its children in spawn order, the parallel outcome
/// ledger (one slot per child; `None` = still live), the cancel flag, and
/// the in-flight mark of the join/teardown drive standing over it. Outcomes
/// are written at each child's death — whoever drives it — so the boundary
/// collects a complete record.
#[derive(Debug)]
pub(crate) struct Nursery {
    pub(crate) children: Vec<FiberId>,
    pub(crate) outcomes: Vec<Option<ChildOutcome>>,
    /// The spawn position of each child, indexed by the child's pool slot:
    /// the death paths look a child up by its id on every write and read,
    /// so the position is one index rather than a scan of the spawn order.
    /// Pool slots are dense above the reserved root, and a nursery's
    /// children are pool slots, so the vector is bounded by the pool.
    positions: Vec<u32>,
    pub(crate) cancelled: bool,
    pub(crate) state: NurseryState,
    /// Whether a drive of this nursery stands: set when its frame is pushed
    /// on the interlock stack, cleared at consumption. A second joiner of a
    /// driven nursery would interlock the two drives on each other (the
    /// cycle the own-nursery rule refuses for one level).
    pub(crate) driving: bool,
    /// The fiber whose code opened this nursery (the owning seam a trapped
    /// owner's release follows), `None` for the host-supervised root: a
    /// fiber's block opens its nursery, spawns into it, and joins at scope
    /// exit, so an open nursery whose owner is dead is abandoned — no drive
    /// will ever consume it and the release door tears it down.
    pub(crate) owner: Option<FiberId>,
}

impl Nursery {
    /// An empty open nursery owned by `owner`.
    pub(crate) fn open(owner: Option<FiberId>) -> Self {
        Self {
            children: Vec::new(),
            outcomes: Vec::new(),
            positions: Vec::new(),
            cancelled: false,
            state: NurseryState::Open,
            driving: false,
            owner,
        }
    }

    /// Records a spawn: the child list is the spawn-order ledger.
    pub(crate) fn push_child(&mut self, child: FiberId) {
        let at = u32::try_from(self.children.len()).expect("nursery child count fits u32");
        let slot = child.index();
        if slot >= self.positions.len() {
            self.positions.resize(slot + 1, NOT_A_CHILD);
        }
        self.positions[slot] = at;
        self.children.push(child);
        self.outcomes.push(None);
    }

    /// The spawn position of one child, `None` for a fiber this nursery
    /// did not spawn.
    fn position(&self, child: FiberId) -> Option<usize> {
        match self.positions.get(child.index()) {
            Some(&at) if at != NOT_A_CHILD => Some(at as usize),
            _ => None,
        }
    }

    /// The outcome recorded for one child, `None` until its death
    /// ledgered (or the fiber is not this nursery's child).
    pub(crate) fn outcome(&self, child: FiberId) -> Option<ChildOutcome> {
        let at = self.position(child)?;
        self.outcomes.get(at).cloned().flatten()
    }

    /// Ledgers a child's death at its spawn position. Called from every
    /// death path (completion, cancellation, trap), whichever driver ran it.
    /// A death can arrive after the boundary consumed the nursery (the
    /// gone-joiner shape: a child resumed the parked joiner mid-drive, the
    /// joiner moved on, and the boundary's re-raised trap attributes to
    /// the dead child the drive ended on) — the drained ledger has no
    /// slot, and the death was already recorded once at its origin, so
    /// the duplicate drops. A recycled nursery slot (a stale id naming
    /// another nursery's children) drops the same way.
    pub(crate) fn record(&mut self, child: FiberId, outcome: ChildOutcome) {
        let Some(at) = self.position(child) else {
            return;
        };
        if let Some(slot) = self.outcomes.get_mut(at) {
            *slot = Some(outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChildOutcome, Nursery};
    use crate::fiber::FiberId;

    /// Deaths land at their own spawn slot whatever order they arrive in
    /// and whatever pool slots the children hold.
    #[test]
    fn a_death_lands_at_the_childs_own_spawn_slot() {
        let mut nursery = Nursery::open(None);
        let spawned = [FiberId(4), FiberId(1), FiberId(7), FiberId(2)];
        for &child in &spawned {
            nursery.push_child(child);
        }
        for &child in spawned.iter().rev() {
            nursery.record(child, ChildOutcome::Cancelled);
        }
        for &child in &spawned {
            assert_eq!(nursery.outcome(child), Some(ChildOutcome::Cancelled));
        }
        assert_eq!(nursery.outcome(FiberId(3)), None, "an unspawned slot");
    }

    /// A death arriving for a fiber this nursery never spawned drops: a
    /// recycled slot naming another nursery's child, or the root.
    #[test]
    fn a_record_for_a_foreign_fiber_drops() {
        let mut nursery = Nursery::open(None);
        nursery.push_child(FiberId(5));
        nursery.push_child(FiberId(2));
        nursery.record(FiberId(3), ChildOutcome::Cancelled);
        nursery.record(FiberId(0), ChildOutcome::Cancelled);
        assert_eq!(nursery.outcome(FiberId(5)), None, "still live");
        assert_eq!(nursery.outcome(FiberId(2)), None, "still live");
        nursery.record(FiberId(2), ChildOutcome::Cancelled);
        assert_eq!(nursery.outcome(FiberId(2)), Some(ChildOutcome::Cancelled));
        assert_eq!(nursery.children, vec![FiberId(5), FiberId(2)]);
    }

    /// A death arriving after the boundary consumed the nursery (the
    /// drained ledger) drops instead of overwriting a newer spawn.
    #[test]
    fn a_record_after_the_ledger_is_drained_drops() {
        let mut nursery = Nursery::open(None);
        nursery.push_child(FiberId(1));
        nursery.outcomes.clear();
        nursery.record(FiberId(1), ChildOutcome::Cancelled);
        assert_eq!(nursery.outcomes.len(), 0, "the drained ledger has no slot");
    }
}
