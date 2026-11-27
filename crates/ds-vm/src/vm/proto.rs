//! The drive protocol the dispatcher speaks to its driver: how a drive ended,
//! what it parked on, the step it took, and the answers the fiber, nursery,
//! channel and timer-race machinery hand back through the drive seams.

use super::*;

/// How a drive of the dispatcher ended for its host: the current fiber
/// completed, it parked with the host as resumer (a control park or a
/// requested suspension at a preemption point), a nested drive's reentry
/// frame returned, or it parked at a cross-nursery join whose drive the
/// host must run first.
pub(crate) enum DriveOutcome {
    /// The current fiber's entry frame returned or `HALT`ed: its whole
    /// answer span — one word for a scalar body, the RETSPAN's width for a
    /// multi-word return — so a host seam reads tuples and widened spans
    /// whole, never a truncated head word.
    Died(FrameAnswer),
    /// The current fiber parked with no in-language resumer: the splice's
    /// park seam or the host's requested suspension carrying its park
    /// word (the bare bounce tag for the suspension).
    Yielded(Value),
    /// A nested reentrant call's synthetic frame returned: the answer's
    /// whole word span (one word for a scalar body, the full span for a
    /// multi-word return — the hosted seam's result channel).
    Frame(FrameAnswer),
    /// The current fiber parked at a join of another open nursery: the
    /// drive hands its host the request, drains the joined nursery, then
    /// delivers the answer onto the parked call site and continues the
    /// joiner. Never the Rust stack — program nesting depth must cost
    /// heap, not stack.
    Interlock(InterlockRequest),
    /// The current fiber parked on a world op: the op would block, the fiber parks with a resume
    /// point, and the driver that owns the executor moment registers
    /// the wait and runs the next ready fiber — the host executor
    /// (`root_drive`'s scheduling loop) or a nursery drive (its next
    /// child, the reactor bridging the gap when nothing is runnable).
    /// A driver that cannot schedule (the hosted native loop) refuses
    /// the park instead, exactly its other non-returning refusals.
    WorldPark(WorldParkRequest),
}

/// The installed reactor behind a Debug-transparent name (the driver's
/// own identity is all a debug render can honestly say).
pub(super) struct InstalledReactor(pub(super) Box<dyn Reactor>);

impl std::fmt::Debug for InstalledReactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstalledReactor").finish_non_exhaustive()
    }
}

impl InstalledReactor {
    /// The driver behind the wrapper.
    pub(super) fn driver(&mut self) -> &mut dyn Reactor {
        self.0.as_mut()
    }
}

/// What one parked fiber waits for, net or channel: the driver routes
/// the two kinds apart — a world wait registers with the scheduler
/// (reactor interest plus a deadline), a channel wait registers in the
/// channel's own waiter queue at park time and needs nothing from the
/// driver (the waking peer is the parked fiber's resumer).
pub(crate) enum ParkKind {
    /// A net op's wait: the scheduler's registration payload.
    World(WorldWait),
    /// A channel op's park: the
    /// waiter already entered the channel's own data, so the driver only
    /// continues the executor — no registration payload exists (the
    /// waking peer is the parked fiber's resumer).
    Chan,
}

/// One world op's park request: the parked fiber and what it waits for.
pub(crate) struct WorldParkRequest {
    /// The fiber that parked (the drive ended on it).
    pub(crate) fiber: FiberId,
    /// What it waits for — the scheduler's registration payload.
    pub(crate) kind: ParkKind,
}

/// A parked joiner's pending nursery drive: the join/boundary seam parked
/// the calling fiber and asks its host drive to drain the joined nursery
/// before the answer is delivered back onto the parked call site.
pub(crate) struct InterlockRequest {
    /// The open nursery being joined.
    pub(super) nursery: NurseryId,
    /// The fiber parked at the join call (the joiner).
    pub(super) joiner: FiberId,
    /// The instruction after the parked join call — where the answer lands.
    pub(super) resume_pc: CodePc,
    /// Which seam parked the joiner, deciding the answer's shape.
    pub(super) seam: DriveSeam,
}

/// One nursery's drive on the interlock stack: the spawn-order children
/// snapshot, the cursor of the child being driven, and who to answer when
/// the nursery consumes. The stack is the join nesting the driver used to
/// carry as nested `execute` frames — one entry per cross-nursery join
/// level, so program nesting depth costs heap, not Rust stack.
pub(super) struct NurseryDrive {
    /// The nursery being driven to consumption.
    pub(super) nursery: NurseryId,
    /// The children in spawn order (the snapshot at push; a child spawned
    /// later never enters this drive).
    pub(super) children: Vec<FiberId>,
    /// The index of the child being driven.
    pub(super) cursor: usize,
    /// The parked joiner and its resume point — the delivery target at
    /// consumption; `None` for the host faces (no call site stands).
    pub(super) joiner: Option<(FiberId, CodePc)>,
    /// How the outcomes become the delivered answer.
    pub(super) seam: DriveSeam,
}

/// The answer a consumed nursery owes its parked joiner.
pub(super) enum Delivery {
    /// `call` overlays onto the parked call site; the boundary span
    /// (`tag`, the payload's whole word list) overlays after the switch
    /// — word 0 the failing member's tag, the rest its payload words;
    /// `gone` is what a moved-on joiner's host drive surfaces instead.
    Answer {
        call: Value,
        span: Option<(u8, Vec<Value>)>,
        gone: Value,
    },
    /// The fork seam's whole-span answer with no discriminant: word 0
    /// delivered by the joiner re-entry's switch payload, the tail
    /// words overlaid past it — the tag-less total's delivery twin.
    Words {
        /// The answer's head word.
        head: Value,
        /// The answer's words past the head.
        tail: Vec<Value>,
        /// What a moved-on joiner's host drive surfaces instead.
        gone: Value,
    },
    /// A boundary failure the joiner's own drive takes: re-enter the
    /// joiner so the drive ends on it, then the trap attributes there.
    Raise(Trap),
}

/// How the initial frame's drain ended for [`Vm::drive_interlock`].
pub(super) enum NurseryDriveEnd {
    /// The initial face frame consumed: the per-child outcome record.
    Face(Vec<ChildOutcome>),
    /// The initial joiner re-entered with its answer: continue its drive
    /// at this point.
    Entered(Active, CodePc),
    /// The initial joiner moved on mid-drive: the answer surfaces to its
    /// host as the drive's death value.
    Gone(Value),
    /// A boundary raise at the initial frame: the trap propagates to the
    /// interrupted host drive at the join call's pc.
    Raised(Trap, CodePc),
}

/// The fork seam's frame end: the fold chain continues with the next
/// step's frame, or the seam consumed with the outcomes and the
/// delivery its answer owes the parked joiner.
pub(super) enum ForkEnd {
    /// `par_reduce`'s chain continues: the next element's child spawned
    /// into its fresh nursery, one drive frame per step.
    Chain(NurseryDrive),
    /// The seam consumed: the outcomes and the parked call site's
    /// delivery (a `Raise` when the stage refused).
    End(Vec<ChildOutcome>, Option<Delivery>),
}

/// One drive segment's outcome for the frame loops: quiet, or a parked
/// cross-nursery join whose frame the stack takes.
pub(super) enum StepFlow {
    /// The segment went quiet (the fiber died, parked at the host, or
    /// trapped — attributed and ledgered).
    Quiet,
    /// The driven fiber parked at a cross-nursery join.
    Interlock(InterlockRequest),
}

/// One woken fiber's entry outcome for the executor loops.
pub(super) enum Woken {
    /// The fiber continues here (its own next instruction, or its
    /// resumer's continuation after it completed into one).
    Step((Active, CodePc)),
    /// The fiber completed at its entry frame with no live resumer:
    /// the answer is the drive's death value.
    Completed(FrameAnswer),
    /// Nothing entered (the fiber was cut or released between its wake
    /// and its entry — its death already handled by whoever cut it).
    Quiet,
}

/// The executor loop's continuation after a quiet drive.
pub(crate) enum ExecutorStep {
    /// The next fiber to drive, and where.
    Continue((Active, CodePc)),
    /// The host fiber's own answer crossed (the program's or the
    /// resumed fiber's) — the drive ends with it.
    HostAnswer(FrameAnswer),
    /// The host's shutdown event cut every parked fiber: nothing live remains, the drive
    /// ends, and the host owns the outcome.
    Shutdown,
    /// The bounded wait spent its budget: nothing is runnable and the
    /// parked waits still stand, so a pumping host keeps its own
    /// waits and pumps again — a blocking drive never answers this
    /// (its wait loops until a wake).
    Idle,
}

/// How one bounded executor-seam drive ended for its host: the host
/// fiber's own answer (it completed, or parked with the host as its
/// resumer — the answer span the drive ends with), or a world park the
/// bounded wait could not resolve inside its budget — the wait stands
/// registered, and the host keeps its own waits until its next pump.
#[derive(Debug)]
pub enum IoDrive {
    /// The drive ended the way [`Vm::resume_fiber`] ends one: the whole
    /// answer span rides here (a suspended fiber's the bounce tag
    /// alone).
    Answer(FrameAnswer),
    /// A world park stands and the budget spent: the host fiber (or a
    /// fiber under it) waits on the world, and the seam needs the
    /// host's next pump to advance — a WorldParked fiber is the
    /// scheduler's alone, never entered by an ordinary resume.
    Parked,
}

/// The eager pass's outcome over one started child.
pub(super) enum EagerPass {
    /// Nothing left to start.
    Idle,
    /// One child drove to a park or a death (its outcome ledgered); the
    /// scan continues before the executor waits.
    Drove,
    /// The drive ended on the host fiber's death: its answer ends the
    /// host drive.
    Host(FrameAnswer),
}

/// A native answer's head word — the word the switch payload delivers
/// onto the parked call site (the full answer overlays after the
/// switch).
/// The timer race's failure span: the io failure taxonomy's `timed_out`
/// class tag over the inert unit word, the deadline's answer to the
/// parked call — the same class discriminant the bounded platform
/// waits answer, so the call site's union re-spans it onto the
/// `timed_out` member the ambient declares in the io taxonomy.
pub(super) fn timeout_failure_span() -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_IO_TIMED_OUT,
        payload: Value::I64(0),
    }
}

/// The deadline twins' winner answer: the unit word under the ok
/// discriminant (the send's union success member).
pub(super) fn ok_tagged(payload: Value) -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload,
    }
}

/// The deadline twins' wider winner answer: the element's whole word
/// span under the ok discriminant (the recv's multi-word success
/// member, the parked call site's span staging).
pub(super) fn ok_spanned(payload: Vec<Value>) -> NativeResult {
    NativeResult::Span {
        tag: PLATFORM_IO_OK,
        payload,
    }
}

pub(super) fn native_head_word(result: &NativeResult) -> Value {
    match result {
        NativeResult::Word(value) => *value,
        NativeResult::Tag { tag, .. } | NativeResult::Span { tag, .. } => Value::Tag(*tag),
        NativeResult::Total { words } => words.first().copied().unwrap_or(Value::I64(0)),
        NativeResult::Fork { .. } | NativeResult::Park(_) => {
            unreachable!("a park request never delivers")
        },
    }
}

/// A native answer's whole-span death value — the completion channel's
/// shape (the parked entry-frame tail park's final delivery).
pub(super) fn frame_answer_of(result: NativeResult) -> FrameAnswer {
    match result {
        NativeResult::Word(value) => FrameAnswer::one(value),
        NativeResult::Tag { tag, payload } => FrameAnswer::tagged(tag, &[payload]),
        NativeResult::Span { tag, payload } => FrameAnswer::tagged(tag, &payload),
        NativeResult::Total { words } => FrameAnswer::many(words),
        NativeResult::Fork { .. } | NativeResult::Park(_) => {
            unreachable!("a park request never delivers")
        },
    }
}

/// Which seam parked a joiner, and how a consumed nursery's outcomes become
/// its delivered answer.
pub(crate) enum DriveSeam {
    /// The debug count join (`nursery_join`): the answer is the trapped
    /// count.
    Count,
    /// The fork-join combinator's parked CALLG site: the drive's
    /// consumption runs the assembly or fold stage the spawn phase
    /// staged, the answers or the final accumulator delivered whole
    /// onto the parked call site — nesting costs the interlock stack's
    /// heap frames, never the Rust stack.
    Fork(crate::native::ForkStage),
    /// The in-language boundary (`nursery_boundary`): the two-word answer
    /// span, `mask` being the caller's return-union containment set.
    Boundary { mask: i64 },
    /// The host join/teardown faces: outcomes returned raw, no call site
    /// stands to answer.
    Face,
}

/// One channel step's outcome for the dispatch arms: the fast path's
/// answer (overlaid like an ordinary native's) or the park request the
/// arm routes through [`Vm::park_world`] or its tail twin.
pub(super) enum ChanStep {
    /// The step answered: overlay like an ordinary native's result.
    Answer(NativeResult),
    /// The step parked the caller: the waiter already entered the
    /// channel's own data; the arm performs the fiber park. `Some`
    /// carries the deadline wait the park registers (the wheel the
    /// arbiter, the loser's cut the timer race's own teardown path),
    /// `None` the plain park no wheel entry arms.
    Park(Option<WorldWait>),
}

/// The timer race's outcome for the `CALL` arm: a deadline already
/// past answers the failure span in place (the drive advancing past
/// the call exactly an ordinary native's answer), a live one parks the
/// caller on the deadline wheel and the drive ends with the event.
pub(super) enum TimerOutcome {
    /// The failure span overlaid; advance past the call.
    Answered,
    /// The caller parked; the drive ends with the event.
    Parked(DriveOutcome),
}

/// The channel step behind a `CALL` site for the dispatch arm: the
/// answer overlaid and the drive advancing past the call, or the park
/// outcome ending the drive.
pub(super) enum ChanOutcome {
    /// The answer overlaid; advance past the call.
    Answered,
    /// The fiber parked; the drive ends with the event.
    Parked(DriveOutcome),
}

/// The result of a control-native switch at a `CALL` site.
pub(super) enum SwitchOutcome {
    /// A fiber was entered: the dispatcher continues here.
    Entered(Active, CodePc),
    /// No switch happened (an unresumable `fiber_resume` target): the
    /// bare `PLATFORM_NONE` bounce tag is on the caller's overlay; the
    /// dispatcher advances past the call.
    Bounced,
    /// The switch reached the host: this drive ends with the event.
    Host(DriveOutcome),
    /// The running fiber parked at a cross-nursery join: the drive unwinds
    /// to its host with the request (the joined nursery's drive runs on the
    /// interlock stack, never nested `execute` frames).
    Interlock(InterlockRequest),
}
