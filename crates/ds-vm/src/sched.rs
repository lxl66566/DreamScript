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
    /// `net_accept(lid)`: the readable wake (a pending connection is
    /// readable) performs the accept and answers the accepted
    /// connection's handle, the stream registering into the connection
    /// table like a connecting one.
    Accept { listener: i64, deadline: Instant },
    /// `net_connect(host, port)`: the staged machine's rows — the
    /// resolve stage first (item 79, ruled 2026-11-24: a name's
    /// resolution runs on the resolver helper thread, its posted
    /// eventfd the readable interest) and the in-progress raw connect
    /// after (the 2026-11-23 ruling, the writability the completion
    /// reports — the wake probing `SO_ERROR`, connected registering
    /// the stream under the pending row's id, a spurious wake
    /// leaving the wait standing). The row's fd releases with the
    /// wait whichever door ends it; `target`
    /// carries the "host:port" the error spans name (no table key
    /// renders it).
    Connect {
        conn: i64,
        target: String,
        deadline: Instant,
    },
    /// `http_get(url)`'s plain exchange: the staged connect, the
    /// request drain, and the response reads to the peer's close — one
    /// machine advancing a step per wake, each step's readiness
    /// re-aiming the interest and re-arming the bound; `target`
    /// carries the URL the error spans name.
    Fetch {
        fetch: i64,
        target: String,
        deadline: Instant,
    },
    /// `http_get`/`https_get`'s TLS exchange: the staged connect, the
    /// rustls handshake, the request's records, and the response reads
    /// to the session's end — the session's own wants-read/wants-write
    /// steering mapped onto readiness interests, one step of the
    /// machine per wake (each step re-aiming the interest and re-arming
    /// the bound); `op` names the op the error spans carry (the
    /// routing body or the direct op), `target` the URL.
    TlsFetch {
        fetch: i64,
        op: &'static str,
        target: String,
        deadline: Instant,
    },
    /// `net_accept_tls(lid)`'s machine: the accept on the listener's
    /// readability, then the staged rustls handshake over the accepted
    /// stream — the tls tier's server machine, one step per wake
    /// (each step re-aiming the interest and re-arming the bound),
    /// the completion registering the stream with its session under
    /// the pre-minted id; `accept` carries the machine row's id (that
    /// same id), `target` the listener id the error spans name.
    TlsAccept {
        accept: i64,
        target: String,
        deadline: Instant,
    },
    /// `proc_read`/`proc_read_err(pid, max)` (the 2026-11-23 ruling):
    /// the child's pipe IS an fd, so the readable wake performs one
    /// read (at most `max` bytes) and answers its string span; the
    /// empty string is the pipe's orderly end. Linux parked twin only
    /// — the pipes own no portable non-blocking shape elsewhere.
    ProcRead {
        pid: i64,
        pipe: ProcPipe,
        max: usize,
        deadline: Instant,
    },
    /// `proc_wait(pid)`: the exit interest rides a pidfd (the
    /// 2026-11-23 ruling) — the readable wake (the child exited)
    /// reaps the status non-blockingly and answers it idempotently
    /// over the reaped word; a spurious wake (the child still runs)
    /// leaves the wait standing.
    ProcWait { pid: i64, deadline: Instant },
    /// `sleep(nanos)`: the timer-only wait — no IO interest, no
    /// deregistration target, the wheel's own firing IS the
    /// completion, answering the unit word (the time face's op, the
    /// 2026-11-21 timer ruling).
    Sleep { deadline: Instant },
}

/// Which of a spawned child's pipes a proc read waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcPipe {
    /// The child's stdout.
    Out,
    /// The child's stderr.
    Err,
}

impl ProcPipe {
    /// The read row's own name, the error span's prefix.
    #[must_use]
    pub fn op_name(self) -> &'static str {
        match self {
            Self::Out => "proc_read",
            Self::Err => "proc_read_err",
        }
    }
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

    /// An accept wait's constructor (the listener's own handle rides
    /// in; the deadline the configured net timeout restated).
    #[must_use]
    pub fn accept(listener: i64, deadline: Instant) -> Self {
        Self::Accept { listener, deadline }
    }

    /// A connect wait's constructor: the pending connect's row id, the
    /// "host:port" the error spans name, and the configured net
    /// timeout restated as the deadline.
    #[must_use]
    pub fn connect(conn: i64, target: String, deadline: Instant) -> Self {
        Self::Connect {
            conn,
            target,
            deadline,
        }
    }

    /// A fetch wait's constructor: the exchange machine's row id, the
    /// URL the error spans name, and the first stage's bound (every
    /// later step re-arming it).
    #[must_use]
    pub fn fetch(fetch: i64, target: String, deadline: Instant) -> Self {
        Self::Fetch {
            fetch,
            target,
            deadline,
        }
    }

    /// A TLS fetch wait's constructor: the machine's row id, the op
    /// name its error spans carry, the URL the spans name, and the
    /// first stage's bound (every later step re-arming it).
    #[must_use]
    pub fn tls_fetch(fetch: i64, op: &'static str, target: String, deadline: Instant) -> Self {
        Self::TlsFetch {
            fetch,
            op,
            target,
            deadline,
        }
    }

    /// An accept-tls wait's constructor: the machine's row id, the
    /// listener id the error spans name, and the first stage's bound
    /// (every later step re-arming it).
    #[must_use]
    pub fn tls_accept(accept: i64, target: String, deadline: Instant) -> Self {
        Self::TlsAccept {
            accept,
            target,
            deadline,
        }
    }

    /// A proc read wait's constructor: the child's handle, the pipe
    /// the read addresses, the clamped max, and the process face's
    /// configured bound as the deadline.
    #[must_use]
    pub fn proc_read(pid: i64, pipe: ProcPipe, max: usize, deadline: Instant) -> Self {
        Self::ProcRead {
            pid,
            pipe,
            max,
            deadline,
        }
    }

    /// A proc wait wait's constructor: the child's handle and the
    /// process face's configured bound restated as the deadline.
    #[must_use]
    pub fn proc_wait(pid: i64, deadline: Instant) -> Self {
        Self::ProcWait { pid, deadline }
    }

    /// The wait's deadline.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        match self {
            Self::Recv { deadline, .. }
            | Self::Send { deadline, .. }
            | Self::Accept { deadline, .. }
            | Self::Connect { deadline, .. }
            | Self::Fetch { deadline, .. }
            | Self::TlsFetch { deadline, .. }
            | Self::TlsAccept { deadline, .. }
            | Self::ProcRead { deadline, .. }
            | Self::ProcWait { deadline, .. }
            | Self::Sleep { deadline } => *deadline,
        }
    }

    /// The handle id the wait names, `None` when no table key names
    /// the wait. A recv or send names a connection; an accept names
    /// the listener; a connect names its pending row; a fetch's
    /// transport stays inside its machine (the registration site
    /// resolves it there) and the sleep names nothing. The
    /// error-span's target rendering is [`WorldWait::target`]'s own.
    #[must_use]
    pub fn conn(&self) -> Option<i64> {
        match self {
            Self::Recv { conn, .. } | Self::Send { conn, .. } | Self::Connect { conn, .. } => {
                Some(*conn)
            },
            Self::Accept { listener, .. } => Some(*listener),
            Self::ProcRead { pid, .. } | Self::ProcWait { pid, .. } => Some(*pid),
            Self::Fetch { .. }
            | Self::TlsFetch { .. }
            | Self::TlsAccept { .. }
            | Self::Sleep { .. } => None,
        }
    }

    /// The wait's error-span target: the "host:port" or URL the
    /// machine waits name (no table key renders it), the handle id
    /// every table-keyed wait answers instead.
    #[must_use]
    pub fn target(&self) -> String {
        match self {
            Self::Connect { target, .. }
            | Self::Fetch { target, .. }
            | Self::TlsFetch { target, .. }
            | Self::TlsAccept { target, .. } => target.clone(),
            _ => self.conn().map(|conn| conn.to_string()).unwrap_or_default(),
        }
    }

    /// The direction the wait watches, `None` for the timer-only wait
    /// (no IO interest stands). The staged machines' resolve stage
    /// (a connect or fetch row whose hostname is still resolving)
    /// answers the readable eventfd instead — the dispatcher resolves
    /// the stage's own direction through the net tables at
    /// registration (a direction the wait's static shape cannot name).
    #[must_use]
    pub fn direction(&self) -> Option<Direction> {
        match self {
            // the readable wakes: a received connection, a bound
            // listener's pending accept (the tls accept machine's own
            // first stage the same door), a pipe holding the child's
            // bytes, a pidfd holding its exit
            Self::Recv { .. }
            | Self::Accept { .. }
            | Self::TlsAccept { .. }
            | Self::ProcRead { .. }
            | Self::ProcWait { .. } => Some(Direction::Read),
            // a connect parks on the writability its completion
            // reports; a fetch starts at its connect-or-write stage,
            // the machine re-aiming the interest at its later steps
            Self::Send { .. }
            | Self::Connect { .. }
            | Self::Fetch { .. }
            | Self::TlsFetch { .. } => Some(Direction::Write),
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
            Self::Accept { .. } => "net_accept",
            Self::Connect { .. } => "net_connect",
            Self::Fetch { .. } => "http_get",
            Self::TlsFetch { op, .. } => op,
            Self::TlsAccept { .. } => "net_accept_tls",
            Self::ProcRead { pipe, .. } => pipe.op_name(),
            Self::ProcWait { .. } => "proc_wait",
            Self::Sleep { .. } => "time_sleep",
        }
    }

    /// Whether the wait belongs to the process face (the io error
    /// span) rather than the net face (the net error span): the
    /// deadline, refusal, and reactor-failure paths render each
    /// face's own member shape.
    #[must_use]
    pub fn is_proc(&self) -> bool {
        matches!(self, Self::ProcRead { .. } | Self::ProcWait { .. })
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

    /// The proc waits' shape: the readable direction both interests
    /// watch, the pid the target and key, the op names the row, and
    /// the face discriminator tells the deadline path which member
    /// shape to render.
    #[test]
    fn the_proc_waits_name_their_pipe_and_exit_interests() {
        let at = Instant::now() + Duration::from_millis(5);
        let read = WorldWait::proc_read(3, ProcPipe::Out, 4096, at);
        assert_eq!(read.deadline(), at);
        assert_eq!(read.conn(), Some(3));
        assert_eq!(read.direction(), Some(Direction::Read));
        assert_eq!(read.op_name(), "proc_read");
        assert_eq!(read.target(), "3");
        assert!(read.is_proc());
        let err = WorldWait::proc_read(3, ProcPipe::Err, 4096, at);
        assert_eq!(err.op_name(), "proc_read_err");
        let wait = WorldWait::proc_wait(3, at);
        assert_eq!(wait.deadline(), at);
        assert_eq!(wait.conn(), Some(3));
        assert_eq!(wait.direction(), Some(Direction::Read));
        assert_eq!(wait.op_name(), "proc_wait");
        assert!(wait.is_proc());
        assert!(!WorldWait::recv(1, 8, at).is_proc());
        assert!(!WorldWait::sleep(at).is_proc());
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
