//! The IO scheduler's data (the 2026-11-21 scheduler ruling): the wait
//! table, the deadline wheel, and the FIFO run queue. A fiber parking
//! on a world op records what it waits for (an IO readiness interest
//! plus a bounded deadline — the net face's own wait bound — or the
//! timer-only wait, the time face's sleep op riding the same wheel);
//! the wake completes the parked op against the fiber's own store and
//! stages the answer for delivery at the resume point, the fiber
//! entering the run queue in wake order (FIFO fairness — the pure
//! fork-join drive's spawn-order determinism is already ruled
//! unspecified once a sibling can park on a world op). Cancellation
//! deregisters the interest first: the reactor never reports into a
//! fiber the cancel discipline already cut.
//!
//! The switching mechanics stay in the dispatcher (`crate::vm`); this
//! module is the representation and the pure queries over it.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, VecDeque},
    time::Instant,
};

use crate::{
    fiber::FiberId,
    native::NativeResult,
    reactor::{Direction, IoHandle},
};

/// What one parked fiber waits for: the pending world op itself, in the
/// shape its completion needs. Every wait carries its own deadline —
/// the bounded-wait discipline the blocking natives always had
/// (`crate::net`'s configured timeout), restated as the parked twin —
/// so the deadline wheel, not a blocked thread, draws the line.
#[derive(Clone, Debug, PartialEq)]
pub enum WorldWait {
    /// `net_recv(conn, max)`: the readable wake performs the read (one
    /// read, at most `max` bytes) and answers its string span; the
    /// empty string is the orderly close.
    Recv {
        conn: i64,
        max: usize,
        deadline: Instant,
    },
    /// `net_send(conn, data)`: the writable wake writes as much of
    /// `data` as the buffer takes; a partial write re-waits on the
    /// remainder (level-triggered readiness, no re-arm needed), the
    /// full drain answering the unit span.
    Send {
        conn: i64,
        data: Vec<u8>,
        deadline: Instant,
    },
    /// `sleep(nanos)`: the timer-only wait — no IO interest, no
    /// deregistration target, the wheel's own firing IS the
    /// completion, answering the unit word (the time face's op, the
    /// 2026-11-21 timer ruling).
    Sleep { deadline: Instant },
}

impl WorldWait {
    /// A recv wait's constructor (the native's clamped max rides in).
    #[must_use]
    pub fn recv(conn: i64, max: usize, deadline: Instant) -> Self {
        Self::Recv {
            conn,
            max,
            deadline,
        }
    }

    /// A send wait's constructor (the staged bytes copied once at park
    /// time; the frame's own word keeps its count where it stood).
    #[must_use]
    pub fn send(conn: i64, data: Vec<u8>, deadline: Instant) -> Self {
        Self::Send {
            conn,
            data,
            deadline,
        }
    }

    /// A sleep wait's constructor: the requested span already folded
    /// into its deadline, the wheel the only wake there is.
    #[must_use]
    pub fn sleep(deadline: Instant) -> Self {
        Self::Sleep { deadline }
    }

    /// The wait's deadline.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        match self {
            Self::Recv { deadline, .. }
            | Self::Send { deadline, .. }
            | Self::Sleep { deadline } => *deadline,
        }
    }

    /// The connection the wait names (its deregistration target),
    /// `None` for the timer-only wait.
    #[must_use]
    pub fn conn(&self) -> Option<i64> {
        match self {
            Self::Recv { conn, .. } | Self::Send { conn, .. } => Some(*conn),
            Self::Sleep { .. } => None,
        }
    }

    /// The direction the wait watches, `None` for the timer-only wait
    /// (no IO interest stands).
    #[must_use]
    pub fn direction(&self) -> Option<Direction> {
        match self {
            Self::Recv { .. } => Some(Direction::Read),
            Self::Send { .. } => Some(Direction::Write),
            Self::Sleep { .. } => None,
        }
    }

    /// The op's own name, the error span's prefix (the native's own
    /// message shape, shared by the parked wake's completion).
    #[must_use]
    pub fn op_name(&self) -> &'static str {
        match self {
            Self::Recv { .. } => "net_recv",
            Self::Send { .. } => "net_send",
            Self::Sleep { .. } => "time_sleep",
        }
    }
}

/// One live wait: the parked fiber, the pending op, and the IO handle
/// its interest registered (a timer-only wait carries none).
#[derive(Clone, Debug)]
pub(crate) struct WaitEntry {
    pub(crate) fiber: FiberId,
    pub(crate) wait: WorldWait,
    pub(crate) handle: Option<IoHandle>,
}

/// The scheduler's tables: VM state the dispatcher owns, nothing more.
#[derive(Debug, Default)]
pub(crate) struct SchedState {
    /// The live waits by token; a token is minted per registration and
    /// never reused, so a stale reactor event (an interest dropped
    /// between the poll and the delivery) finds no row and drops.
    pub(crate) waits: HashMap<u64, WaitEntry>,
    /// The next wait's token.
    next_token: u64,
    /// The deadline wheel: (deadline, token) pairs; stale tokens (the
    /// wait woke or cancelled by another door first) drop on pop.
    pub(crate) timers: BinaryHeap<Reverse<(Instant, u64)>>,
    /// The ready fibers in wake order (FIFO).
    pub(crate) run_queue: VecDeque<FiberId>,
    /// The completed answers awaiting delivery at each woken fiber's
    /// resume point — the parked call site's overlay words.
    pub(crate) deliveries: HashMap<FiberId, NativeResult>,
}

impl SchedState {
    /// Mints the next registration token.
    pub(crate) fn mint(&mut self) -> u64 {
        let token = self.next_token;
        self.next_token += 1;
        token
    }

    /// Whether any fiber is ready to run.
    pub(crate) fn has_ready(&self) -> bool {
        !self.run_queue.is_empty()
    }

    /// Whether anything can ever wake again: a wait standing or a
    /// timer armed. The executor's stall guard reads this.
    pub(crate) fn can_wake(&self) -> bool {
        !self.waits.is_empty() || !self.timers.is_empty()
    }

    /// Pops the next ready fiber (FIFO).
    pub(crate) fn pop_ready(&mut self) -> Option<FiberId> {
        self.run_queue.pop_front()
    }

    /// Queues a fiber ready (wake order).
    pub(crate) fn push_ready(&mut self, fiber: FiberId) {
        self.run_queue.push_back(fiber);
    }

    /// Takes one fiber's queued entry: a drive that enters a woken
    /// child directly (its own sweep) consumes the entry itself, so
    /// the queue never reports a fiber a drive already entered as
    /// still ready — the executor's wait would otherwise return
    /// instantly forever on a stale entry.
    pub(crate) fn take_ready(&mut self, fiber: FiberId) {
        self.run_queue.retain(|&queued| queued != fiber);
    }

    /// The nearest armed deadline, `None` when no timer stands.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.timers.peek().map(|Reverse((at, _))| *at)
    }

    /// Pops every deadline at or before `now`, answering their tokens
    /// in deadline order; a token whose wait already left the table
    /// (woken or cancelled by another door first) drops here.
    pub(crate) fn pop_expired(&mut self, now: Instant) -> Vec<u64> {
        let mut fired = Vec::new();
        while let Some(Reverse((at, token))) = self.timers.peek().copied() {
            if at > now {
                break;
            }
            self.timers.pop();
            fired.push(token);
        }
        fired
    }

    /// Arms a wait's deadline on the wheel.
    pub(crate) fn arm(&mut self, deadline: Instant, token: u64) {
        self.timers.push(Reverse((deadline, token)));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The deadline wheel fires in deadline order and drops stale
    /// tokens; the run queue is FIFO; the stall guard reads both
    /// tables.
    #[test]
    fn the_wheel_and_queue_discipline() {
        let mut sched = SchedState::default();
        assert!(!sched.can_wake(), "nothing armed, nothing waiting");
        let a = FiberId(1);
        let b = FiberId(2);
        sched.push_ready(a);
        sched.push_ready(b);
        assert!(sched.has_ready());
        assert_eq!(sched.pop_ready(), Some(a));
        assert_eq!(sched.pop_ready(), Some(b));
        assert_eq!(sched.pop_ready(), None);
        let t0 = sched.mint();
        let t1 = sched.mint();
        assert_ne!(t0, t1, "tokens never repeat");
        let now = Instant::now();
        sched.arm(now + Duration::from_millis(20), t1);
        sched.arm(now + Duration::from_millis(5), t0);
        assert_eq!(sched.next_deadline(), Some(now + Duration::from_millis(5)));
        assert!(sched.pop_expired(now).is_empty(), "neither due");
        assert_eq!(
            sched.pop_expired(now + Duration::from_millis(6)),
            vec![t0],
            "the earlier deadline first"
        );
        assert_eq!(
            sched.pop_expired(now + Duration::from_millis(30)),
            vec![t1],
            "the later deadline once due, stale-free"
        );
        assert_eq!(sched.next_deadline(), None);
    }

    /// The timer-only wait's shape: no connection to deregister, no
    /// direction to watch, the deadline the whole of the wait.
    #[test]
    fn the_sleep_wait_names_no_io_interest() {
        let at = Instant::now() + Duration::from_millis(5);
        let wait = WorldWait::sleep(at);
        assert_eq!(wait.deadline(), at);
        assert_eq!(wait.conn(), None, "no deregistration target");
        assert_eq!(wait.direction(), None, "no readiness direction");
        assert_eq!(wait.op_name(), "time_sleep");
        assert_eq!(wait, WorldWait::Sleep { deadline: at });
    }
}
