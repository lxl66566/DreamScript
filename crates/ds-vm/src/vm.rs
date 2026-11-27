//! The tier-1 VM: a dense-match dispatch loop (stable Rust has no computed
//! goto) over encoded uop bodies, proving the locked slot-overlay
//! call ABI.
//!
//! # Decode strategy: decode on dispatch
//!
//! The `Vec<u32>` word stream is the program, and the dispatcher keys one
//! jump table on each word's raw opcode byte, extracting the operand fields
//! inline per arm; a pre-decoded cache is deliberately rejected on the same
//! terms: the encoded stream is the authoritative form (the dsmod loader
//! mmaps it), tier transitions are in-place J-variant rewrites of
//! the encoded words — execution must observe them — and a decode
//! cache keyed per version would be extra state to invalidate across
//! hot-reload swaps. Load decode-checks every word once, and
//! [`ds_abi::uop::Uop::decode`] stays the canonicity authority: the hot
//! arms name the same causes it would, and anything they refuse falls to
//! one slow arm that routes through the decoders, so a dispatch-time
//! [`Trap::BadEncoding`] is a canonicity violation, not a slow path. The
//! bias arithmetic is branch-free.
//!
//! # Fibers and nurseries
//!
//! The dispatcher also owns the fiber switch mechanics over the pool in
//! `crate::fiber`: the control natives park the running fiber (record
//! its resume point), and entering a fiber swaps segment and call-chain
//! ownership between its pool slot and the VM's context — two buffer
//! swaps, O(1), no word copied. A fiber completing at its entry frame
//! marks itself dead and switches back to its resumer with its final
//! value.
//!
//! Nurseries layer on top (`crate::nursery`): fibers spawn into a
//! nursery (the child-of link), the join/teardown driver drives children to
//! death in spawn order, and a first failure raises the nursery's cancel
//! flag — observed at taken backward branches (the `jump!` macro) and at the
//! driver's re-entry of a parked child (the resume-shaped point: a parked
//! child of a cancelled nursery is cut at its next resume instead of
//! resuming), so a linear consume in flight is never interrupted. A fiber
//! joining another open nursery parks at its join call and the joined
//! nursery's drive runs on the interlock stack — frames pushed and consumed
//! LIFO, each answer delivered onto its parked joiner's call site — so join
//! nesting depth costs heap, never the Rust stack. The fork-join
//! combinators park the same way: a CALLG site's spawn-only body
//! leaves the children in a fresh nursery and the drive seam carries
//! the assembly or fold stage, so a nested combinator (a par_map inside
//! a par_map element) stacks drive frames on the interlock's heap
//! stack instead of nesting drives on the Rust stack.
//!
//! # Reentrant native calls
//!
//! The lambda-taking generic builtins (map/filter/reduce) call user code
//! from Rust: `Vm::call_hosted` pushes a synthetic reentry frame (marked
//! by the entry-frame sentinel return pc) on the running fiber's chain
//! and drives it through `execute` with a `boundary` depth, nesting
//! stack-style — map-in-lambda stacks another frame on the same chain.
//! A drive that ends the fiber or parks at the host instead of returning
//! refuses with [`Trap::ReentrancyRefused`]: a Rust-side loop cannot
//! suspend. CALLG stays one instruction from the outer loop's view, so
//! the cancel discipline adds no observation point inside the
//! per-element loop; a lambda's own backward jumps observe through the
//! same `jump!` macro as any code.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    hash::{BuildHasherDefault, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use ds_abi::{
    BIAS_J, BoundRet, CodePc, ConstIdx, FrameLayout, FrameMetadata, RC_MAJOR, RcDecodeError, RcOp,
    SlideDirection, SlideError, SlidePlan, SlotType, Staging, TailChain,
    uop::{DecodeError, MAX_FRAME_SLOTS},
};

use crate::{
    EVIDENCE_MODULE,
    claims::{CLAIM_BAND_CLEAR, CLAIM_BAND_SET, ClaimReading, ClaimTable, HookSite, hook_sites},
    fiber::{ERR_RING_CAP, ErrHop, FIBER_ROOT, Fiber, FiberId, FiberState, ResumePoint, Segment},
    loader::{LoadError, StagedSite},
    module::{ModuleRegistry, ModuleVersion, VersionIdx},
    native::{
        ChannelNative, ControlNative, NURSERY_BOUNDARY_ERR, NURSERY_BOUNDARY_OK, NativeFunction,
        NativeResult, OutputSink, PLATFORM_CANCELLED, PLATFORM_IO_OK, PLATFORM_IO_TIMED_OUT,
        PLATFORM_NONE, TimerNative,
    },
    nursery::{ChildOutcome, JoinOutcome, Nursery, NurseryId, NurseryState, Race},
    object::{ChanPop, ChanSender, Heaps, Objects},
    program::{Const, ConstFuncRef, ConstKind},
    reactor::Reactor,
    sched::{SchedState, WaitEntry, Wake, WorldWait},
    value::{ARENA_LEN_LIMIT, ArenaPackError, ArenaStr, FuncRef, RefId, Value, Word},
};

mod call;
mod chan;
mod debug;
mod execute;
mod fiber;
mod host;
mod install;
mod nursery;
mod proto;
mod race;
mod site;
mod stack;
mod teardown;
mod trap;
mod value;
mod wait;

use call::*;
pub use debug::{
    BacktraceFrame, Breakpoint, BreakpointHit, DebugStop, ErrorTraceFrame, FramePc, ProfileRow,
    SourceLocation,
};
pub use proto::IoDrive;
use proto::*;
pub(crate) use proto::{DriveOutcome, DriveSeam, ExecutorStep, ParkKind};
use site::*;
use stack::*;
pub use trap::Trap;
use value::*;

/// The deadline poll's quanta in taken backward branches: one poll per
/// this many back edges. The poll's idle body is an Option check, the
/// clock read only where a race stands, so the amortized cost is noise
/// against the pin band; one quanta of tight-loop edges is tens of
/// microseconds, far under any real deadline's granularity.
const FUEL_QUANTA: u32 = 16_384;

/// Default value-stack limit: 1 Mi words (8 MiB on 64-bit targets).
const DEFAULT_STACK_LIMIT: usize = 1 << 20;

/// The 2^63 boundary f64: the smallest float magnitude that cannot be an
/// i64, for the CVTF2I range check.
const F64_I64_EDGE: f64 = 9_223_372_036_854_775_808.0;

/// One entry of the call-info stack: the frame base plus the module
/// version's table row and the function the frame runs. Frames live inline
/// in the fiber's value stack; this records which version's code and pool
/// a frame executes, so retiring versions stay runnable until nothing
/// references them. The chain belongs to the fiber whose
/// segment holds the frames.
#[derive(Clone, Debug)]
pub(crate) struct CallInfo {
    pub(crate) fp: usize,
    pub(crate) module: VersionIdx,
    pub(crate) func: u32,
}

/// The function the dispatcher is executing.
pub(crate) struct Active {
    version: VersionIdx,
    func: u32,
}

/// Where a call transfers control next.
struct Transfer {
    version: VersionIdx,
    func: u32,
}

/// Words a native argument list or closure capture block holds before the
/// staging falls back to the heap: every native table entry and every
/// lowered capture shape sits well under this, so the per-call heap
/// allocations disappear from the common cases.
const INLINE_WORDS: usize = 8;

/// Stages a closure's capture block for a CALLS copy-in without a heap
/// allocation per call: real capture blocks are a handful of words, so
/// they copy into the inline array and only an oversized block falls back
/// to the reused heap buffer (cleared per stage, never reallocated).
#[derive(Debug)]
struct CaptureStaging {
    inline: [Value; INLINE_WORDS],
    heap: Vec<Value>,
}

impl CaptureStaging {
    fn new() -> Self {
        Self {
            inline: [Value::I64(0); INLINE_WORDS],
            heap: Vec::new(),
        }
    }

    /// Copies `block` in and answers the staged slice, valid until the
    /// next `stage`.
    fn stage<'a>(&'a mut self, block: &[Value]) -> &'a [Value] {
        if block.len() <= INLINE_WORDS {
            self.inline[..block.len()].copy_from_slice(block);
            &self.inline[..block.len()]
        } else {
            self.heap.clear();
            self.heap.extend_from_slice(block);
            &self.heap
        }
    }
}

/// The outcome of a `RET`.
enum RetOutcome {
    /// The fiber's entry frame returned; `Value` is its final value.
    Root(Value),
    /// The reentry frame of a nested drive returned (the synthetic
    /// entry frame [`Vm::call_hosted`] pushed); `Value` is the hosted
    /// call's answer, and the Rust-side native loop resumes.
    Boundary(Value),
    /// Resume the caller's function at the metadata word's return pc.
    Resume {
        version: VersionIdx,
        func: u32,
        pc: CodePc,
    },
}

/// A callable's whole answer span, the fiber-boundary result channel:
/// the reentry frame of a hosted call (`Vm::call_hosted`) returns it to
/// its Rust-side driver, a spawned child's completion rides it through
/// the nursery ledger (`ChildOutcome::Completed`), and a root drive's
/// death value carries it to its host (`Vm::run`,
/// `Vm::resume_fiber`-style entries) so a multi-word answer crosses
/// whole — up to four words inline (every lowered lambda's answer
/// today; the per-element combinator loop must not pay an allocation),
/// the heap buffer the wide fallback.
#[derive(Clone, Debug, PartialEq)]
pub enum FrameAnswer {
    /// The span's first `n` words, `n` = the span's width.
    Words {
        head: [Value; FRAME_ANSWER_INLINE],
        n: usize,
    },
    /// A span past four words.
    Wide(Vec<Value>),
}

/// The inline width of [`FrameAnswer::Words`]: a span up to this wide
/// stages without the heap, which is what keeps a narrow answer
/// allocation-free (every lowered lambda's answer today).
const FRAME_ANSWER_INLINE: usize = 4;

impl FrameAnswer {
    /// One word (a scalar body's RET).
    #[must_use]
    pub fn one(value: Value) -> Self {
        let mut head = [Value::I64(0); FRAME_ANSWER_INLINE];
        head[0] = value;
        Self::Words { head, n: 1 }
    }

    /// A whole span, inline while it fits.
    #[must_use]
    pub fn many(words: Vec<Value>) -> Self {
        if words.len() <= FRAME_ANSWER_INLINE {
            let mut head = [Value::I64(0); FRAME_ANSWER_INLINE];
            head[..words.len()].copy_from_slice(&words);
            Self::Words {
                head,
                n: words.len(),
            }
        } else {
            Self::Wide(words)
        }
    }

    /// A tag-led span: word 0 the tag, then the payload words. Inline
    /// while it fits, so a span channel's tail answer allocates nothing.
    fn tagged(tag: u8, payload: &[Value]) -> Self {
        let n = 1 + payload.len();
        if n <= FRAME_ANSWER_INLINE {
            let mut head = [Value::I64(0); FRAME_ANSWER_INLINE];
            head[0] = Value::Tag(tag);
            head[1..n].copy_from_slice(payload);
            return Self::Words { head, n };
        }
        let mut words = Vec::with_capacity(n);
        words.push(Value::Tag(tag));
        words.extend_from_slice(payload);
        Self::Wide(words)
    }

    /// A whole span read from `n` word sources, inline while it fits.
    fn staged<E>(n: usize, mut word: impl FnMut(usize) -> Result<Value, E>) -> Result<Self, E> {
        if n <= FRAME_ANSWER_INLINE {
            let mut head = [Value::I64(0); FRAME_ANSWER_INLINE];
            for (i, at) in head.iter_mut().enumerate().take(n) {
                *at = word(i)?;
            }
            return Ok(Self::Words { head, n });
        }
        let mut words = Vec::with_capacity(n);
        for i in 0..n {
            words.push(word(i)?);
        }
        Ok(Self::Wide(words))
    }

    /// The answer's words in order.
    #[must_use]
    pub fn as_slice(&self) -> &[Value] {
        match self {
            Self::Words { head, n } => &head[..*n],
            Self::Wide(words) => words,
        }
    }
}

/// The tier-1 VM. Construct with a [`ModuleRegistry`], then run encoded
/// entry functions. Every VM owns its heap object store, output sink, and
/// the single-threaded fiber pool (slot 0 is the root a `run` entry
/// drives), and installs the builtin core native module
/// ([`crate::native::BUILTIN_MODULE`]) into the registry at construction;
/// the world surface is an installed platform handler's to serve
/// ([`crate::platform`]) — a bare VM carries no world surface.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct Vm {
    pub(crate) registry: ModuleRegistry,
    /// The running fiber's heap object store, swapped in from its pool
    /// slot beside the segment; the slot holds a scratch store while
    /// the fiber runs. Host-facing store access routes by the current
    /// fiber ([`Vm::objects`]).
    pub(crate) objects: Objects,
    out: OutputSink,
    /// The running fiber's segment and call chain, swapped in from its
    /// pool slot beside the store; `ctx_live` tracks whether they are
    /// here (true) or stashed in the slot (false).
    pub(crate) stack: Segment,
    pub(crate) callinfo: Vec<CallInfo>,
    pub(crate) ctx_live: bool,
    pub(crate) pool: Vec<Fiber>,
    pub(crate) current: FiberId,
    /// The nursery pool: one slot per open-or-consumed nursery, Dead
    /// slots reuse like fiber slots.
    pub(crate) nurseries: Vec<Nursery>,
    /// The live root nursery bare host spawns route into; `None` or a
    /// consumed id = the host itself is the supervisor.
    root_nursery: Option<NurseryId>,
    pub(crate) limit: usize,
    pub(crate) trap_pc: Option<CodePc>,
    /// Per-fiber claim ledgers, parallel to the pool: the multiset of
    /// counts each fiber's frames own (crates/ds-vm/src/claims.rs) —
    /// what the teardown drain walk releases. Index-parallel to the
    /// pool, never swapped: the running fiber's ledger is reached
    /// through `current` directly.
    pub(crate) claims: Vec<ClaimTable>,
    /// The CALL-site inline cache: one direct-mapped slot bank keyed on
    /// (caller function identity, pc), entries stamped with the registry
    /// generation they resolved against. Fibers share the bank safely —
    /// the key is per-function code, never per-fiber stack state, and
    /// validity rides the global install generation, which each install
    /// also sweeps the bank against so swapped-out versions' handles do
    /// not linger until eviction.
    call_cache: Vec<Option<CallSite>>,
    /// The word-keyed inline-cache bank (the CALLS arm and the hosted-call
    /// seam): one [`WordCallee`] per slot, keyed by the callable word's
    /// bits, the owner row, and the argument count, validated against the
    /// registry generation. Fibers share the bank safely — the key names
    /// the callee alone, never per-fiber stack state.
    word_cache: Vec<Option<WordCallee>>,
    /// The hosted-call seam's persistent reentry memo, one entry beside
    /// the bank above: the last scalar-callee resolution, validated
    /// against the registry generation at every use, so consecutive
    /// reentries of one callee skip the bank probe. Like the bank, the
    /// key names the callee alone, so fibers share the memo safely.
    reentry: Option<HostedReentry>,
    /// The KSTR pool-const intern cache: per-(pool, fiber) object-id
    /// rows, keyed by the pool's address (stable for its version's
    /// lifetime) and the loading fiber. No claim is held — a hit
    /// re-validates the row against its key and the id against the
    /// constant's content and falls back to the full intern when either
    /// disagrees, so object lifetimes read exactly as without the cache.
    /// Rows are reclaimed at the doors that orphan them: the swap retiring
    /// a version (its pools die with it) and the pool-slot reuse (the
    /// slot's next fiber re-keys its rows on demand).
    kstr_cache: KstrRows,
    /// The derived deinit-hook directories: one per (module version,
    /// function), keyed by the version's address — stable for its lifetime
    /// — and swept wholesale when the registry generation moves, exactly
    /// the kstr cache's discipline. A directory row is decoded from the
    /// function's own words once, so the teardown pass never scans on a
    /// death path more than once per function per install generation.
    hook_dirs: HookDirIndex,
    hook_dirs_stamp: u64,
    /// Inside a teardown hook drive: a cancel flag observed at a backward
    /// branch refuses the drive instead of cutting its fiber (a teardown
    /// hook must not re-enter the nursery — the cut would kill the very
    /// fiber whose death walk is driving the hook).
    teardown_drive: bool,
    /// The reused capture staging buffer for CALLS copy-ins (per-call
    /// `to_vec` copies are gone; a borrow of this buffer lives only
    /// within one dispatch arm or hosted call, never across a reentry).
    calls_staging: CaptureStaging,
    /// The resumed-frame seam's pre-step state (crates/ds-vm/src/repl.rs):
    /// what a trapped submission's rollback restores to. `None` while no
    /// REPL drive is mid-flight.
    pub(crate) repl_pre: Option<crate::repl::ReplPre>,
    /// The REPL park seam's marker (crates/ds-vm/src/repl.rs): `Some` when
    /// the `repl_park` control native parked the root fiber for the host,
    /// carrying the submission's whole staged answer. The seam reaches the
    /// host as [`DriveOutcome::Yielded`]; the REPL drive consumes this to
    /// tell a park from any other resumer-less stop, whose continuation
    /// the drive keeps running.
    pub(crate) repl_park: Option<FrameAnswer>,
    /// The host's pending suspension request (the dev host's capability
    /// face): the named fiber parks at its next taken backward branch under
    /// its next drive, handing the drive's host the fiber's continuation.
    /// Consumed when it parks.
    suspend_request: Option<FiberId>,
    /// The parked branch a resumed suspension re-enters: its own re-take
    /// is the resume, not a fresh preemption point, so the request check
    /// skips it exactly once (a second arrival at the same instruction
    /// parks — the frame has advanced a full iteration). Set by the
    /// switch into a Preempted fiber; every switch resets it.
    suspend_skip: Option<(FiberId, CodePc)>,
    /// The error-trace switch: armed, the ERR_MARK arm writes the
    /// failing frame's position into the running fiber's private ring.
    /// VM-layer state alone — no lowering input reads it, so flipping it
    /// changes no artifact.
    trace_errors: bool,
    /// The debugger's armed breakpoints, resolved to (version, function,
    /// pc) triples through the span tables at arm time; `None` keeps the
    /// dispatch's per-instruction check to one branch test.
    breakpoints: Option<BTreeSet<(u32, u32, u32)>>,
    /// The parked hit a resumed breakpoint re-enters: its own re-take is
    /// the resume, not a fresh stop, so the check skips it exactly once.
    breakpoint_skip: Option<(FiberId, CodePc)>,
    /// The last breakpoint the running fiber stopped at, reset when the
    /// host reads it.
    last_breakpoint: Option<(VersionIdx, u32, u32)>,
    /// Whether the last Yielded park was a breakpoint stop, so the root
    /// drive hands the host the parked fiber instead of resuming it.
    breakpoint_park: bool,
    /// Whether the current root drive was entered through
    /// [`Vm::run_debugged`], so a breakpoint park ends the drive instead
    /// of resuming under the host's nose.
    debug_stops: bool,
    /// The sampling profiler's interval in backward branches (0 off):
    /// armed, every Nth taken back edge records the running frame's
    /// position into the histogram.
    sample_every: u32,
    sample_tick: u32,
    /// The deadline poll's countdown in taken backward branches: at zero
    /// the running fiber's own race compares its deadline against the
    /// clock, a racer that never parks answering the wheel at its edge.
    fuel_tick: u32,
    /// The sampler's histogram: (version, function, pc) hits.
    samples: BTreeMap<(u32, u32, u32), u64>,
    /// The budgeted drive's remaining steps ([`Vm::run_with_budget`]):
    /// charged one per decoded instruction by the fueled instantiation
    /// of the dispatch loop alone, so every unbounded drive ignores it
    /// and its dispatch cost is unchanged.
    fuel: u64,
    /// The fork-join worker pool opt-in: off by default — the sequential
    /// interlock drive is the backend — and when on, a
    /// `par_map` frame whose children hold exclusively scalar entry
    /// words drives them on youpipe workers through whole-fiber
    /// ownership transfer, everything else keeping the sequential drive.
    fork_pool: bool,
    /// How many fork frames this VM drove on the worker pool: the
    /// non-vacuity witness for the differential tests (a pool-enabled
    /// run that answers every fork sequentially through the gate's
    /// fallback reads zero).
    fork_pool_drives: usize,
    /// The installed reactor: `None`
    /// keeps the blocking natives — blocking semantics stay an
    /// embedder's platform choice, never a language fact. The box rides
    /// a Debug-transparent wrapper so `Vm`'s own derive stands.
    reactor: Option<InstalledReactor>,
    /// The IO scheduler's tables: the wait register, the deadline
    /// wheel, the FIFO run queue, and the wake deliveries.
    sched: SchedState,
    /// Whether the host's shutdown interest has woken: the drive raises every open nursery's flag
    /// with it, cuts parked fibers at their resume-shaped points, and
    /// answers [`Trap::Shutdown`] — the host that registered the
    /// interest owns the exit code, never the engine.
    shutdown: bool,
}

/// The caller identity a call-site resolution carries: the running
/// function's address (the inline cache's key half) and its version row
/// (the index space a retiring frame's own-module site reads).
#[derive(Clone, Copy)]
struct CallerAt {
    func: usize,
    row: VersionIdx,
}

/// A slot swap's staged-word plan, validated before the install and
/// consumed after it: the swapped module's registry id plus one row per
/// retiring-table index — the incoming table's index of the same function
/// identity, or `None` where the identity did not survive (a staged word
/// keying a `None` row refused the swap already). Default is the no-op a
/// first install under a fresh name validates to.
#[derive(Debug, Default)]
struct StagedSwap {
    module_id: u32,
    map: Vec<Option<u32>>,
}

/// The opcode byte each canonical uop occupies in the hot dispatch table:
/// one byte per arm, in the uop format's `major << 4 | minor` spelling.
/// The decoder (ds-abi) stays the canonicity authority — module load
/// validates every word — so these constants exist for the interpreter's
/// one dispatch table: adding a uop adds one byte constant, one arm, and
/// never re-lowers its neighbors.
const OP_KIMM: u8 = 0x00;
const OP_KCONST: u8 = 0x01;
const OP_KSTR: u8 = 0x02;
const OP_KSTR_BORROW: u8 = 0x03;
const OP_ADD_I64: u8 = 0x10;
const OP_SUB_I64: u8 = 0x11;
const OP_MUL_I64: u8 = 0x12;
const OP_DIV_I64: u8 = 0x13;
const OP_MOD_I64: u8 = 0x14;
const OP_WRAPADD_I64: u8 = 0x15;
const OP_WRAPSUB_I64: u8 = 0x16;
const OP_WRAPMUL_I64: u8 = 0x17;
const OP_SHL_I64: u8 = 0x18;
const OP_SHR_I64: u8 = 0x19;
const OP_BITAND_I64: u8 = 0x1a;
const OP_BITXOR_I64: u8 = 0x1b;
const OP_BITOR_I64: u8 = 0x1c;
const OP_ADD_F64: u8 = 0x20;
const OP_SUB_F64: u8 = 0x21;
const OP_MUL_F64: u8 = 0x22;
const OP_DIV_F64: u8 = 0x23;
const OP_EQ_I64: u8 = 0x30;
const OP_NEQ_I64: u8 = 0x31;
const OP_LT_I64: u8 = 0x32;
const OP_GE_I64: u8 = 0x33;
const OP_LE_I64: u8 = 0x34;
const OP_GT_I64: u8 = 0x35;
const OP_EQ_F64: u8 = 0x40;
const OP_NEQ_F64: u8 = 0x41;
const OP_LT_F64: u8 = 0x42;
const OP_GE_F64: u8 = 0x43;
const OP_LE_F64: u8 = 0x44;
const OP_GT_F64: u8 = 0x45;
const OP_EQ_BOOL: u8 = 0x50;
const OP_NEQ_BOOL: u8 = 0x51;
const OP_TAG_TEST: u8 = 0x60;
const OP_TAG_TEST_NOT: u8 = 0x61;
const OP_TAG_NEW: u8 = 0x62;
const OP_CLOSURE_NEW: u8 = 0xd3;
const OP_CAPTURE_STORE: u8 = 0xd5;
const OP_CVT_I2F: u8 = 0x70;
const OP_CVT_F2I: u8 = 0x71;
const OP_MOV: u8 = 0x80;
const OP_JMPIF: u8 = 0x90;
const OP_JMPIF_NOT: u8 = 0x91;
const OP_JMP: u8 = 0x92;
const OP_ARENA_MARK: u8 = 0x93;
const OP_ARENA_RESTORE: u8 = 0x94;
const OP_CALL: u8 = 0xa0;
const OP_CALLT: u8 = 0xa1;
const OP_RET: u8 = 0xa2;
const OP_RET_SPAN: u8 = 0xa3;
const OP_CALLG: u8 = 0xa4;
const OP_CALLTG: u8 = 0xa5;
const OP_CALLS: u8 = 0xa6;
const OP_FUNCF: u8 = 0xb0;
const OP_ERR_MARK: u8 = 0xb1;
const OP_HALT: u8 = 0xc0;
const OP_EQ_STR: u8 = 0xd0;
const OP_NEQ_STR: u8 = 0xd1;
const OP_STRCAT: u8 = 0xd2;
const OP_STRCAT_ARENA: u8 = 0xd4;
const OP_RC_INC: u8 = 0xe0;
const OP_RC_DEC: u8 = 0xe1;
impl Vm {
    /// A VM with the default stack limit.
    #[must_use]
    pub fn new(registry: ModuleRegistry) -> Self {
        Self::with_limit(registry, DEFAULT_STACK_LIMIT)
    }

    /// A VM with an explicit value-stack word limit. Installs the builtin
    /// core native module only (a same-named prior native table is
    /// replaced); the platform handler is the host's explicit choice
    /// (a library embedder gets no world surface by default).
    #[must_use]
    pub fn with_limit(mut registry: ModuleRegistry, limit: usize) -> Self {
        registry.install_native(
            crate::native::BUILTIN_MODULE,
            crate::native::builtin_module(),
        );
        Self {
            registry,
            objects: Objects::new(FIBER_ROOT),
            out: OutputSink::default(),
            stack: Segment::new(),
            callinfo: Vec::new(),
            ctx_live: false,
            pool: vec![Fiber::empty(FIBER_ROOT)],
            current: FIBER_ROOT,
            nurseries: Vec::new(),
            root_nursery: None,
            limit,
            trap_pc: None,
            claims: vec![ClaimTable::default()],
            call_cache: vec![None; CALL_CACHE_SLOTS],
            word_cache: vec![None; WORD_CACHE_SLOTS],
            reentry: None,
            kstr_cache: KstrRows::default(),
            hook_dirs: HookDirIndex::default(),
            hook_dirs_stamp: u64::MAX,
            teardown_drive: false,
            calls_staging: CaptureStaging::new(),
            repl_pre: None,
            repl_park: None,
            suspend_request: None,
            suspend_skip: None,
            trace_errors: false,
            breakpoints: None,
            breakpoint_skip: None,
            last_breakpoint: None,
            breakpoint_park: false,
            debug_stops: false,
            sample_every: 0,
            sample_tick: 0,
            fuel_tick: FUEL_QUANTA,
            samples: BTreeMap::new(),
            fuel: 0,
            fork_pool: false,
            fork_pool_drives: 0,
            reactor: None,
            sched: SchedState::default(),
            shutdown: false,
        }
    }

    /// How many fork frames this VM drove on the worker pool: the
    /// non-vacuity witness for the differential tests.
    #[must_use]
    pub fn pooled_fork_drives(&self) -> usize {
        self.fork_pool_drives
    }

    /// Opts this VM's fork-join combinator drives into the worker pool:
    /// a `par_map` frame whose
    /// children hold exclusively scalar entry words transfers each whole
    /// fiber — segment, arena, heap store, call chain, claim ledger — to
    /// a youpipe worker for the element body and receives it at the
    /// join, the answers assembling in spawn order exactly as the
    /// sequential interlock does. Everything the gate refuses (a `Ref`
    /// word in an entry frame, `par_reduce`'s fold chain, every other
    /// seam) keeps the sequential drive, so behavior stays identical;
    /// pooled children run pure code — the world surface never reaches a
    /// worker and a world call refuses with the standard unresolved
    /// trap, the pool's purity contract.
    pub fn enable_fork_pool(&mut self) {
        self.fork_pool = true;
    }

    /// Installs the reactor and flips the net face's data natives to
    /// the parking discipline: `net_recv`/`net_send` answer
    /// park requests on a would-block, the drive runs the next ready
    /// fiber, and readiness or the configured deadline completes the
    /// parked op — single-thread executor; the multi-thread fiber
    /// executor is out of scope. A host that installs nothing keeps the
    /// blocking natives: the same source, the embedder's choice.
    pub fn install_reactor(&mut self, reactor: Box<dyn Reactor>) {
        self.out.enable_net_parking();
        // the process face's blocking rows park the same way on Linux:
        // the child's pipes and its pidfd are
        // fds, the reactor's own readiness interests; any other host
        // keeps the rows' blocking twins under whatever reactor it
        // installs
        self.out.enable_proc_parking();
        self.reactor = Some(InstalledReactor(reactor));
    }

    /// Registers the host's shutdown door as the reactor's own interest:
    /// `handle` is a readable fd the host
    /// owns — Linux's signalfd — and its wake under the reserved token is
    /// the shutdown event, never a wait. The engine decides nothing about
    /// it: the drive raises every open nursery's cancel flag, cuts
    /// parked fibers with the sentinel at their resume-shaped points
    /// (their deinit hooks firing), and answers [`Trap::Shutdown`] — the
    /// host that installed the interest owns the exit code.
    ///
    /// # Errors
    ///
    /// [`crate::reactor::ReactorError`] when no reactor stands (the
    /// interest needs one) or the driver refuses the registration.
    pub fn watch_shutdown(
        &mut self,
        handle: crate::reactor::IoHandle,
    ) -> Result<(), crate::reactor::ReactorError> {
        let Some(installed) = self.reactor.as_mut() else {
            return Err(crate::reactor::ReactorError(
                "no reactor installed".to_owned(),
            ));
        };
        installed.driver().register(
            handle,
            crate::reactor::Direction::Read,
            crate::reactor::SHUTDOWN_TOKEN,
        )
    }

    /// Hands the blocking drive its stop flag: a host without a reactor keeps the
    /// blocking natives, so its own signal helper — a sigwait thread, a
    /// console-control handler — flips this flag and the blocking
    /// twins' poll rows (the bounded accept, the unbounded proc wait)
    /// observe it between rounds, the drive answering [`Trap::Shutdown`]
    /// exactly the reactor path's outcome. The host keeps its clone;
    /// the engine decides nothing.
    pub fn set_shutdown_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.out.set_shutdown_flag(flag);
    }

    /// Whether the drive observed the host's shutdown event (the
    /// registered interest's wake, or the blocking twin's stop flag
    /// answering [`Trap::Shutdown`]): the host reads it after its run
    /// to own the exit code whatever the drive's last outcome was.
    #[must_use]
    pub fn shutdown_observed(&self) -> bool {
        self.shutdown
    }

    /// Interns a string into the root fiber's heap and hands back its
    /// reference word: the host-facing way to stage string arguments
    /// into `run` and bare spawns (and the discipline KSTR follows for
    /// pool constants). The claim is one reference count; a host that
    /// abandons a staged word never decs it, so host-held references
    /// keep their objects alive. Host words always name the root heap —
    /// a child's frame holds them as foreign words, served by routing.
    #[must_use = "the reference is the handle; the count is the host's to drop"]
    pub fn intern_str(&mut self, text: &str) -> Value {
        let id = self.root_store().intern(text);
        Value::Ref(RefId::new(FIBER_ROOT, id))
    }

    /// Stages a closure value for the host (the capturing-lambda object): one
    /// heap object behind one `Ref` word, the header FuncRef word plus the
    /// moved-in capture block, born on the root heap like every host-staged
    /// word. The host-held reference owns the object's birth count; only a host
    /// stages a closure word, module code constructs none.
    #[must_use = "the reference is the handle; the count is the host's to drop"]
    pub fn alloc_closure(&mut self, func: FuncRef, captures: Vec<Value>) -> Value {
        let id = self.root_store().alloc_closure(func, captures);
        Value::Ref(RefId::new(FIBER_ROOT, id))
    }

    /// Switches the output sink to an in-memory buffer: print/println
    /// write there until the next [`Vm::run`] or forever. Callers read the
    /// bytes with [`Vm::captured_output`].
    pub fn capture_output(&mut self) {
        self.out.capture();
    }

    /// The bytes captured so far ([`Vm::capture_output`]).
    #[must_use]
    pub fn captured_output(&self) -> &[u8] {
        self.out.captured_bytes()
    }

    /// Stages the process arguments the `args` world op answers — the
    /// host's own wording of the run's command line (the CLI passes its
    /// `--args` words verbatim).
    pub fn set_argv(&mut self, argv: Vec<String>) {
        self.out.set_argv(argv);
    }

    /// The current fiber's heap object store: hosts and tests inspect run
    /// results (string content, list elements) by the `Ref` word a run
    /// returned. The drive ends on this fiber, so its store is parked in
    /// its slot; a word naming another fiber routes through
    /// [`Vm::objects_at`].
    #[must_use]
    pub fn objects(&self) -> &Objects {
        self.objects_of_fiber(self.current)
    }

    /// The store owning `fiber`'s heap: the running fiber's context store
    /// when it is `fiber`, its pool slot's otherwise.
    #[must_use]
    pub fn objects_at(&self, fiber: FiberId) -> &Objects {
        self.objects_of_fiber(fiber)
    }

    /// The store owning `word`'s heap, by the word's owner axis.
    #[must_use]
    pub fn objects_of(&self, word: RefId) -> &Objects {
        self.objects_of_fiber(word.fiber())
    }

    fn objects_of_fiber(&self, fiber: FiberId) -> &Objects {
        if self.ctx_live && fiber == self.current {
            &self.objects
        } else {
            &self.pool[fiber.index()].objects
        }
    }

    /// Bounds every blocking net native's wait (connect, read, write):
    /// the default is 3 seconds; a hostile host yields `net_err`, never a
    /// hung interpreter.
    pub fn set_net_timeout(&mut self, timeout: Duration) {
        self.out.set_net_timeout(timeout);
    }

    /// Bounds every parked proc wait ([`crate::proc`]): the blocking
    /// twins stay unbounded whatever the bound, and the default is the
    /// far backstop — a wait sees any child out, the deadline wheel
    /// holding only the wake guarantee. A host wanting the net face's
    /// tight bounded discipline sets its own bound, the deadline then
    /// answering the timeout span.
    pub fn set_proc_timeout(&mut self, timeout: Duration) {
        self.out.set_proc_timeout(timeout);
    }

    /// Pins the TLS client of the net face's tls tier: custom trust
    /// roots for an embedding host or a test, the default being the
    /// webpki root set. The tier itself stays the platform's `tls`
    /// capability — a host omitting the capability never serves the
    /// https bodies, whatever roots are pinned.
    pub fn set_tls_config(&mut self, config: Arc<rustls::ClientConfig>) {
        self.out.set_tls_config(config);
    }

    /// Pins the TLS server identity of the net face's tls tier: no default identity exists — a
    /// server certificate is the host's own to pin, the unprovisioned
    /// `net_accept_tls` answering its own refusal.
    pub fn set_tls_server_config(&mut self, config: Arc<rustls::ServerConfig>) {
        self.out.set_tls_server_config(config);
    }

    /// Current value-stack word count: the current fiber's segment length.
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.pool[self.current.index()].segment.len()
    }

    /// High-water mark of the ROOT fiber's segment over the last run.
    /// Constant across unbounded tail recursion is the tail-call
    /// acceptance criterion, per fiber.
    #[must_use]
    pub fn max_stack_len(&self) -> usize {
        self.pool[FIBER_ROOT.index()].max_len
    }

    /// Runs one host-drive step, draining any interlock the step raises
    /// before the interrupted fiber continues (its join answer delivered
    /// onto the parked call site).
    pub(crate) fn execute_host_step<const FUELED: bool>(
        &mut self,
        active: Active,
        pc: CodePc,
        boundary: usize,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        match self.execute::<FUELED>(active, pc, boundary) {
            Ok(DriveOutcome::Interlock(req)) => self.drive_interlock::<FUELED>(req, boundary),
            outcome => outcome,
        }
    }

    /// Ledgers a fiber's death into its nursery, if it has one. A
    /// racer's death resolves its race.
    fn record_child_outcome(&mut self, fiber: FiberId, outcome: ChildOutcome) {
        let Some(n) = self.pool[fiber.index()].nursery else {
            return;
        };
        self.nurseries[n.index()].record(fiber, outcome);
        if self.nurseries[n.index()].race.is_some()
            && let Some(outcome) = self.nurseries[n.index()].outcome(fiber)
        {
            self.resolve_race(n, fiber, &outcome);
        }
    }

    /// A fiber dying trapped under any driver: ledger `Trapped` and raise
    /// its nursery's cancel flag. The fiber's own state
    /// is the caller's business (interrupted under interactive drives,
    /// marked dead by the nursery walk).
    pub(crate) fn trap_child(&mut self, fiber: FiberId, trap: &Trap) {
        if let Some(n) = self.pool[fiber.index()].nursery {
            let nursery = &mut self.nurseries[n.index()];
            nursery.record(fiber, ChildOutcome::Trapped(trap.clone()));
            nursery.cancelled = true;
            if nursery.race.is_some() {
                self.resolve_race(n, fiber, &ChildOutcome::Trapped(trap.clone()));
            }
        }
    }

    /// Whether the running fiber observes a cancel discipline:
    /// its own nursery's raised flag at a taken backward branch, or the
    /// host's shutdown standing — the
    /// cut is the same sentinel death either way. Only nurseries with
    /// live children are ever cancelled, so a stale id cannot fire
    /// here. A timeout racer's own nursery is its race's: the wheel's
    /// deadline door and the back-edge deadline poll raise the same
    /// flag, so the racer observes its deadline exactly a user cancel's
    /// flag.
    fn cancel_requested(&self) -> bool {
        self.shutdown
            || self.pool[self.current.index()]
                .nursery
                .is_some_and(|n| self.nurseries[n.index()].cancelled)
    }

    /// The back-edge deadline poll: the running fiber's own race
    /// compares its deadline against the clock. Past it, the expiry is
    /// recorded — the wheel's deadline door's own marks — and the
    /// nursery's cancel flag rises, so the cut at this same edge answers
    /// the failure span through the racer's ordinary cancel death. No
    /// scheduler state moves here: a parked owner's delivery stays the
    /// racer-death resolution's, and the pool slots hold every other
    /// fiber's true store while this drive runs, so the flag write
    /// never meets a checked-out context.
    fn poll_race_deadline(&mut self) {
        let Some(n) = self.pool[self.current.index()].nursery else {
            return;
        };
        let slot = n.index();
        let nursery = &self.nurseries[slot];
        if nursery.state != NurseryState::Open
            || nursery.cancelled
            || nursery
                .race
                .as_ref()
                .is_none_or(|race| race.deadline > Instant::now())
        {
            return;
        }
        let nursery = &mut self.nurseries[slot];
        nursery.cancelled = true;
        if let Some(race) = nursery.race.as_mut() {
            race.expired = true;
        }
    }

    /// Resolves and checks a run/spawn entry: unknown function, arity,
    /// argument types, entry frame versus the limit.
    fn resolve_entry(
        &self,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<(VersionIdx, u32), Trap> {
        let (version, func_id) =
            self.registry
                .resolve(module, func)
                .ok_or_else(|| Trap::UnknownFunction {
                    module: module.into(),
                    func: func.into(),
                })?;
        self.entry_checks(version, func_id, args)?;
        Ok((version, func_id))
    }

    /// The argument and frame checks every entry shares: arity,
    /// argument word kinds (a Ref word against a Fn parameter is the
    /// callable-defaulted slot), and the entry frame versus the stack
    /// limit.
    pub(crate) fn entry_checks(
        &self,
        version: VersionIdx,
        func_id: u32,
        args: &[Value],
    ) -> Result<(), Trap> {
        let target = &self.registry.pinned(version).functions()[func_id as usize];
        if args.len() != target.layout.params_len() {
            return Err(Trap::ArityMismatch {
                expected: target.layout.params_len(),
                given: args.len(),
            });
        }
        for (i, (&arg, &ty)) in args.iter().zip(target.layout.params()).enumerate() {
            // one classification of the staged word serves the compare, the
            // exemptions, and the report; a Raw parameter is the wildcard
            // and a Ref word against a Fn parameter is the
            // callable-defaulted slot: the host stages a closure word
            // into a fn-typed entry parameter exactly like a module call
            let actual = arg.slot_type();
            if actual != ty
                && ty != SlotType::Raw
                && !(actual == SlotType::Ref && ty == SlotType::Fn)
            {
                return Err(Trap::SlotTypeMismatch {
                    slot: u8::try_from(i).expect("argument index exceeds u8"),
                    expected: ty,
                    actual,
                });
            }
        }
        let frame_size = target.layout.frame_size() as usize;
        if frame_size > self.limit {
            return Err(Trap::StackOverflow {
                needed: frame_size,
                limit: self.limit,
            });
        }
        Ok(())
    }

    /// Builds a fiber's entry frame in its own segment: metadata word,
    /// arguments, zero-filled to the frame span. The caller has already
    /// reset the segment (`clear` for a fresh root run, `truncate(0)` to
    /// reuse a dead fiber's chunks).
    pub(crate) fn push_entry_frame(
        fiber: &mut Fiber,
        version: VersionIdx,
        func_id: u32,
        args: &[Value],
        frame_size: usize,
    ) {
        // Root frame: the return-pc sentinel is never read, because a RET
        // from the entry frame completes the fiber instead of returning.
        fiber.segment.push(Word::Meta(FrameMetadata {
            return_pc: CodePc(u32::MAX),
            frame_size: u32::try_from(frame_size).expect("frame span fits u32"),
            tail: TailChain::default(),
        }));
        for &arg in args {
            fiber.segment.push(Word::Slot(arg));
        }
        fiber.segment.grow_to(frame_size);
        fiber.callinfo.clear();
        fiber.callinfo.push(CallInfo {
            fp: 0,
            module: version,
            func: func_id,
        });
        fiber.max_len = frame_size;
    }

    /// Stashes the running fiber's context into its pool slot, ending a
    /// drive; the pool is then the single home of every fiber's state —
    /// three buffer swaps, no word or object ever copied.
    pub(crate) fn host_release(&mut self) {
        if !self.ctx_live {
            return;
        }
        let fiber = &mut self.pool[self.current.index()];
        std::mem::swap(&mut self.stack, &mut fiber.segment);
        std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
        std::mem::swap(&mut self.objects, &mut fiber.objects);
        self.ctx_live = false;
    }

    /// Parks the running fiber at `resume_pc` (the instruction after the
    /// call that is switching): only the resume point is recorded; segment
    /// and call chain are untouched.
    fn park_current(&mut self, active: &Active, resume_pc: CodePc) {
        let fiber = &mut self.pool[self.current.index()];
        fiber.resume = Some(ResumePoint {
            version: active.version,
            func: active.func,
            pc: resume_pc,
        });
        fiber.state = FiberState::Suspended;
    }

    /// Parks the running fiber on a world op at a plain `CALL` site:
    /// the resume point is the
    /// instruction after the call, the completed op's answer delivered
    /// there when the scheduler wakes the fiber. The wait's
    /// registration belongs to the driver that owns the executor
    /// moment — the host executor loop, a nursery drive, or the
    /// refusal of a driver that cannot schedule.
    // the Result shape mirrors park_world_tail and the drive-step
    // outcomes its call sites fold: the park family keeps one
    // signature
    #[allow(clippy::unnecessary_wraps)]
    fn park_world(
        &mut self,
        active: &Active,
        pc: CodePc,
        kind: ParkKind,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        self.park_current(active, CodePc(pc.0 + 1));
        let fiber = self.current;
        self.pool[fiber.index()].state = FiberState::WorldParked;
        Ok(DriveOutcome::WorldPark(WorldParkRequest { fiber, kind }))
    }

    /// Parks the running fiber on a world op at a `CALLT` site: the
    /// native tail call never reuses a frame, so its parked twin pops
    /// the frame exactly as the ret below would — without delivering a
    /// value — and parks in the caller's context at the pending
    /// return, the wake's answer overlaying the caller's argument area
    /// where the ret would have written it. The entry frame's own tail
    /// park parks with the sentinel resume point (the entry frame's
    /// never-read return pc): the wake completes the fiber with the
    /// delivered answer instead of resuming it. A hosted reentry
    /// boundary frame refuses — its Rust-side loop cannot suspend, the
    /// same refusal every other non-returning hosted outcome takes.
    fn park_world_tail(
        &mut self,
        active: &Active,
        pc: CodePc,
        boundary: usize,
        kind: ParkKind,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        let meta = match self.stack[fp] {
            Word::Meta(m) => m,
            Word::Slot(_) => return Err((Trap::MetaWordCorrupt { fp }, pc)),
        };
        if boundary != usize::MAX && self.callinfo.len() == boundary && meta.return_pc.0 == u32::MAX
        {
            return Err((Trap::ReentrancyRefused, pc));
        }
        self.arena_frame_close(fp);
        if self.callinfo.len() == 1 {
            // the entry frame's return is pending on the world: the
            // sentinel resume point marks the final delivery — the wake
            // completes the fiber with the delivered answer
            self.park_current(active, CodePc(u32::MAX));
            let fiber = self.current;
            self.pool[fiber.index()].state = FiberState::WorldParked;
            return Ok(DriveOutcome::WorldPark(WorldParkRequest { fiber, kind }));
        }
        let caller_at = self.callinfo.len() - 2;
        let caller_row = self.callinfo[caller_at].module;
        let caller_func = self.callinfo[caller_at].func;
        let caller_fp = self.callinfo[caller_at].fp;
        let caller_entry = &self.registry.pinned(caller_row).functions()[caller_func as usize];
        let code_len = code_len_u32(caller_entry.code.len());
        if meta.return_pc.0 >= code_len {
            return Err((
                Trap::InvalidPc {
                    pc: meta.return_pc.0,
                    code_len,
                },
                pc,
            ));
        }
        let restore = (caller_fp + caller_entry.layout.frame_size() as usize).max(fp + 2);
        self.stack.truncate(restore);
        self.callinfo
            .pop()
            .expect("a caller entry stands below the returning frame");
        let caller = Active {
            version: caller_row,
            func: caller_func,
        };
        self.park_current(&caller, meta.return_pc);
        let fiber = self.current;
        self.pool[fiber.index()].state = FiberState::WorldParked;
        Ok(DriveOutcome::WorldPark(WorldParkRequest { fiber, kind }))
    }

    /// Runs one completion step against a parked fiber's OWN store: the
    /// slot's store swaps into the context for the step and back (the
    /// switch-in ownership dance without the rest of a switch), so an
    /// allocation the completion makes — the answer string, the error
    /// message — lands in the store its `Ref` word names. The swap
    /// itself is the ownership discipline: the scheduler's completions
    /// run over released contexts (the pool slots hold every fiber's
    /// true store) and the close-wake door's over a live drive (the
    /// closing fiber's context stands, the parked waiter's slot still
    /// its true store) — a completion allocates, it never drives, so
    /// no re-entrancy crosses the swap either way.
    fn complete_in_store<T>(&mut self, fiber: FiberId, run: impl FnOnce(&mut Heaps<'_>) -> T) -> T {
        std::mem::swap(&mut self.objects, &mut self.pool[fiber.index()].objects);
        let mut heaps = Heaps::new(
            fiber,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            true,
        );
        let answer = run(&mut heaps);
        std::mem::swap(&mut self.objects, &mut self.pool[fiber.index()].objects);
        answer
    }
}

#[cfg(test)]
mod tests {
    use ds_abi::uop::{Inst, Uop};

    use super::*;

    /// The peek-next fusion's opcode bytes are ds-abi encoding facts, never
    /// hex copied into the interpreter: every byte the fused arms match
    /// must equal the uop's own `opcode()` derivation from the encoding
    /// table.
    #[test]
    fn fused_pair_opcode_bytes_match_the_abi_encoding() {
        let byte = |op: Uop| op.opcode();
        assert_eq!(
            OP_KIMM,
            byte(Uop::Kimm {
                dst: 0,
                kidx: ConstIdx(0)
            })
        );
        assert_eq!(
            OP_KCONST,
            byte(Uop::Kconst {
                dst: 0,
                kidx: ConstIdx(0)
            })
        );
        assert_eq!(
            OP_ADD_I64,
            byte(Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(
            OP_SUB_I64,
            byte(Uop::SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(
            OP_MUL_I64,
            byte(Uop::MulI64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(
            OP_MOD_I64,
            byte(Uop::ModI64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(
            OP_ADD_F64,
            byte(Uop::AddF64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(
            OP_MUL_F64,
            byte(Uop::MulF64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        // DIVI64 is deliberately outside the fused set: the census never
        // measured the pair
        assert_eq!(
            OP_DIV_I64,
            byte(Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
    }

    /// The fused compare-branch predicate peeks raw opcode bytes: the fused
    /// compare arms and the JMPIF/JMPIF_NOT dispatch constants must equal
    /// ds-abi's `Uop::opcode()` derivation, so a uop renumbering fails here
    /// instead of silently unfusing (or mis-fusing) at runtime.
    #[test]
    fn fused_branch_opcode_bytes_match_the_abi_encoding() {
        let byte = |op: Uop| op.opcode();
        assert_eq!(
            OP_LT_I64,
            byte(Uop::LtI64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(
            OP_EQ_I64,
            byte(Uop::EqI64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(
            OP_GT_F64,
            byte(Uop::GtF64 {
                dst: 0,
                lhs: 0,
                rhs: 0
            })
        );
        assert_eq!(OP_JMPIF, byte(Uop::Jmpif { cond: 0, target: 0 }));
        assert_eq!(OP_JMPIF_NOT, byte(Uop::JmpifNot { cond: 0, target: 0 }));
    }

    /// The fused tag-branch predicate peeks raw opcode bytes: the fused
    /// TAGTEST arm and the JMPIF dispatch constant must equal ds-abi's
    /// `Uop::opcode()` derivation, so a uop renumbering fails here instead
    /// of silently unfusing (or mis-fusing) at runtime. TAGTEST_NOT stays
    /// outside the fused set: no kernel stream carries the uop, so the
    /// census measured no pair for it.
    #[test]
    fn fused_tag_branch_opcode_bytes_match_the_abi_encoding() {
        let byte = |op: Uop| op.opcode();
        assert_eq!(
            OP_TAG_TEST,
            byte(Uop::TagTest {
                dst: 0,
                src: 0,
                tag: 0
            })
        );
        assert_eq!(
            OP_TAG_TEST_NOT,
            byte(Uop::TagTestNot {
                dst: 0,
                src: 0,
                tag: 0
            })
        );
        assert_eq!(OP_JMPIF, byte(Uop::Jmpif { cond: 0, target: 0 }));
    }

    /// The peek-next predicate fires exactly on the measured census pairs
    /// — a fused arith whose lhs or rhs reads the load's dst — and on
    /// nothing else: DIVI64 stays unfused, a non-consuming arith stays
    /// unfused, an intervening RC word breaks the adjacency, and a load
    /// at the code end peeks nothing.
    #[test]
    fn the_peek_next_predicate_fires_only_on_the_measured_pairs() {
        // the r3 arith operand fields, per ds-abi's own effect tables
        let fields = |op: Uop| {
            let &[dst] = op.writes().as_slice() else {
                unreachable!("an r3 arith writes one dst")
            };
            let &[lhs, rhs] = op.reads().as_slice() else {
                unreachable!("an r3 arith reads lhs and rhs")
            };
            (op.opcode(), dst, lhs, rhs)
        };
        let load = Uop::Kimm {
            dst: 2,
            kidx: ConstIdx(0),
        };
        for arith in [
            Uop::AddI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::SubI64 {
                dst: 1,
                lhs: 1,
                rhs: 2,
            },
            Uop::MulI64 {
                dst: 0,
                lhs: 2,
                rhs: 2,
            },
            Uop::ModI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
        ] {
            let code = crate::program::assemble(&[load, arith]).expect("pair encodes");
            assert_eq!(
                fused_i64_arith(&code, 0, 2),
                Some(fields(arith)),
                "{arith:?} consumes the loaded dst and fuses"
            );
        }
        let fload = Uop::Kconst {
            dst: 2,
            kidx: ConstIdx(0),
        };
        for arith in [
            Uop::AddF64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::MulF64 {
                dst: 1,
                lhs: 1,
                rhs: 2,
            },
        ] {
            let code = crate::program::assemble(&[fload, arith]).expect("pair encodes");
            assert_eq!(
                fused_f64_arith(&code, 0, 2),
                Some(fields(arith)),
                "{arith:?} consumes the loaded dst and fuses"
            );
        }
        // the negatives: DIVI64 (unmeasured), the wrap trio and the
        // shift/bitwise five (new minors, never census-measured), the
        // wrong-kind follower, a non-consuming arith, an RC word
        // between, and the code end
        let negatives = [
            Uop::DivI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::WrapAddI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::WrapSubI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::WrapMulI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::ShlI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::ShrI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::BitAndI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::BitXorI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::BitOrI64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::AddF64 {
                dst: 0,
                lhs: 2,
                rhs: 1,
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 1,
                rhs: 3,
            },
        ];
        for arith in negatives {
            let code = crate::program::assemble(&[load, arith]).expect("pair encodes");
            assert_eq!(
                fused_i64_arith(&code, 0, 2),
                None,
                "{arith:?} is not a fused pair"
            );
        }
        let rc = RcOp::Inc { src: 2 };
        let consuming = Uop::AddI64 {
            dst: 0,
            lhs: 2,
            rhs: 3,
        };
        let body = [Inst::from(load), Inst::from(rc), Inst::from(consuming)];
        let code = crate::program::assemble(&body).expect("body encodes");
        assert_eq!(
            fused_i64_arith(&code, 0, 2),
            None,
            "an intervening RC word breaks the adjacency"
        );
        let code = crate::program::assemble(&[load]).expect("body encodes");
        assert_eq!(
            fused_i64_arith(&code, 0, 2),
            None,
            "a load at the code end peeks nothing"
        );
    }

    /// A span answer up to the inline width stages on the stack — the
    /// per-element combinator loop must not allocate — and a wider span
    /// keeps its heap buffer whole.
    #[test]
    fn a_narrow_span_answer_stages_without_the_heap() {
        let narrow = FrameAnswer::many(vec![Value::I64(1), Value::I64(2)]);
        assert!(matches!(narrow, FrameAnswer::Words { n: 2, .. }));
        assert_eq!(narrow.as_slice(), &[Value::I64(1), Value::I64(2)]);
        let widest = FrameAnswer::many(vec![Value::I64(0); FRAME_ANSWER_INLINE]);
        assert!(matches!(widest, FrameAnswer::Words { .. }));
        let wide = FrameAnswer::many(vec![Value::I64(0); FRAME_ANSWER_INLINE + 1]);
        assert!(matches!(wide, FrameAnswer::Wide(_)));
        assert_eq!(wide.as_slice().len(), FRAME_ANSWER_INLINE + 1);
        let tagged = FrameAnswer::tagged(7, &[Value::I64(9)]);
        assert!(matches!(tagged, FrameAnswer::Words { n: 2, .. }));
        assert_eq!(tagged.as_slice(), &[Value::Tag(7), Value::I64(9)]);
        let staged = FrameAnswer::staged::<()>(3, |i| {
            Ok(Value::I64(i64::try_from(i).expect("a span index fits i64")))
        })
        .expect("the source is infallible");
        assert!(matches!(staged, FrameAnswer::Words { n: 3, .. }));
        assert_eq!(staged.as_slice(), &[
            Value::I64(0),
            Value::I64(1),
            Value::I64(2)
        ]);
    }

    /// A reclaim compacts the row list, so a frame's slot can name another
    /// row afterwards: the key check refuses it and the key index
    /// re-resolves to the survivor, whose ids came along.
    #[test]
    fn a_reclaim_shift_revalidates_row_slots() {
        let mut rows = KstrRows::default();
        let dropped: KstrRowKey = (0x1000, 1);
        let kept: KstrRowKey = (0x2000, 2);
        let (gone, row) = rows.open(dropped, 2);
        row.ids[1] = Some(7);
        let (survives, row) = rows.open(kept, 2);
        row.ids[1] = Some(9);

        rows.reclaim(|row| row.key() != dropped);
        assert_eq!(rows.len(), 1);
        assert!(
            rows.row(survives, kept).is_none(),
            "the surviving row moved down a slot"
        );
        assert!(rows.row(gone, dropped).is_none());
        let (moved, row) = rows.open(kept, 2);
        assert_ne!(moved.0, survives.0);
        assert_eq!(row.ids[1], Some(9), "the moved row kept its ids");
        assert_eq!(rows.row(moved, kept).map(|row| row.ids[1]), Some(Some(9)));
    }

    /// A reused pool address meets its dead predecessor's row, which is
    /// widened before the new pool's const index.
    #[test]
    fn a_reopened_row_widens_to_the_new_pools_width() {
        let mut rows = KstrRows::default();
        let key: KstrRowKey = (0x1000, 0);
        let (_, row) = rows.open(key, 2);
        row.ids[1] = Some(3);
        let (_, row) = rows.open(key, 4);
        assert_eq!(row.ids.len(), 4);
        assert_eq!(row.ids[1], Some(3));
    }

    /// The version table's single home: the row's `Arc` is an installed
    /// version's one owner, so the entry resolution, the call's frame
    /// push, the return's take-back, and the CALL-cache fills between
    /// mint no second count — a drive per iteration over a same-module
    /// CALL, read through the test-only count probe — and the install
    /// sweep frees the version with its row, since no other home exists.
    /// A seam regressing into an Arc hand-off — per call, per cache fill,
    /// or per return — moves the count off 1.
    #[test]
    fn the_tables_row_is_a_versions_single_home_across_drives() {
        // g(n) = n * scale; f(n) = g(n): one same-module CALL plus its
        // return per drive, the retired take-back seam
        fn fixture(scale: i64) -> ModuleVersion {
            let g = crate::program::FunctionDef {
                name: "g".into(),
                layout: FrameLayout::new(vec![SlotType::I64, SlotType::I64], 1, 0)
                    .expect("valid layout"),
                pool: vec![Const::I64(scale)],
                code: crate::program::assemble(&[
                    Uop::Funcf { frame_slots: 2 },
                    Uop::Kimm {
                        dst: 1,
                        kidx: ConstIdx(0),
                    },
                    Uop::MulI64 {
                        dst: 0,
                        lhs: 0,
                        rhs: 1,
                    },
                    Uop::Ret { src: 0 },
                ])
                .expect("fixture body encodes"),
            };
            let f = crate::program::FunctionDef {
                name: "f".into(),
                layout: FrameLayout::new(vec![SlotType::I64, SlotType::I64], 1, 1)
                    .expect("valid layout"),
                pool: vec![Const::FuncRef(ConstFuncRef::Named {
                    module: "m".into(),
                    func: 0,
                })],
                code: crate::program::assemble(&[
                    Uop::Funcf { frame_slots: 2 },
                    Uop::Mov { dst: 1, src: 0 },
                    Uop::Call {
                        callee: ConstIdx(0),
                        nargs: 1,
                    },
                    Uop::Ret { src: 1 },
                ])
                .expect("fixture body encodes"),
            };
            ModuleVersion::new("m", vec![g, f]).expect("valid fixture module")
        }
        let mut vm = Vm::new(ModuleRegistry::new());
        let v1 = vm.install_module(fixture(2)).expect("the swap installs");
        for _ in 0..8 {
            assert_eq!(
                vm.run("m", "f", &[Value::I64(21)]),
                Ok(FrameAnswer::one(Value::I64(42)))
            );
        }
        assert_eq!(
            vm.registry.row_arc_count(v1),
            1,
            "entry, call, return, and cache fills minted no second home"
        );
        // one swap pins v1 through the retiring slot, still its single
        // home; the second swap leaves the slots, but the root drive's
        // kept entry frame still names v1 — the sweep honestly keeps the
        // row (the same pin the fiber sweep tests carry)
        vm.install_module(fixture(3)).expect("the swap installs");
        assert_eq!(vm.registry.row_arc_count(v1), 1);
        assert!(vm.registry.version(v1).is_some());
        vm.install_module(fixture(5)).expect("the swap installs");
        assert_eq!(vm.registry.row_arc_count(v1), 1);
        assert!(vm.registry.version(v1).is_some());
        // the next run resets the root's kept entry frame onto the
        // current row, so the install after it retires v1's row — and the
        // row was the version's single home, so its count reads 0
        assert_eq!(
            vm.run("m", "f", &[Value::I64(21)]),
            Ok(FrameAnswer::one(Value::I64(105)))
        );
        vm.install_module(fixture(7)).expect("the swap installs");
        assert_eq!(vm.registry.row_arc_count(v1), 0);
        assert!(vm.registry.version(v1).is_none());
    }

    /// The bounded-row regression: deep install
    /// churn with the entry frame draining between installs recycles the
    /// swept rows' slots, so the table's width stays a small constant
    /// instead of growing one row per install — and each recycled index
    /// serves its new version exactly, the run answering the fresh
    /// scale.
    #[test]
    fn the_version_table_stays_bounded_under_deep_install_churn() {
        fn fixture(scale: i64) -> ModuleVersion {
            let g = crate::program::FunctionDef {
                name: "g".into(),
                layout: FrameLayout::new(vec![SlotType::I64, SlotType::I64], 1, 0)
                    .expect("valid layout"),
                pool: vec![Const::I64(scale)],
                code: crate::program::assemble(&[
                    Uop::Funcf { frame_slots: 2 },
                    Uop::Kimm {
                        dst: 1,
                        kidx: ConstIdx(0),
                    },
                    Uop::MulI64 {
                        dst: 0,
                        lhs: 0,
                        rhs: 1,
                    },
                    Uop::Ret { src: 0 },
                ])
                .expect("fixture body encodes"),
            };
            let f = crate::program::FunctionDef {
                name: "f".into(),
                layout: FrameLayout::new(vec![SlotType::I64, SlotType::I64], 1, 1)
                    .expect("valid layout"),
                pool: vec![Const::FuncRef(ConstFuncRef::Named {
                    module: "m".into(),
                    func: 0,
                })],
                code: crate::program::assemble(&[
                    Uop::Funcf { frame_slots: 2 },
                    Uop::Mov { dst: 1, src: 0 },
                    Uop::Call {
                        callee: ConstIdx(0),
                        nargs: 1,
                    },
                    Uop::Ret { src: 1 },
                ])
                .expect("fixture body encodes"),
            };
            ModuleVersion::new("m", vec![g, f]).expect("valid fixture module")
        }
        let mut vm = Vm::new(ModuleRegistry::new());
        for round in 1..=500 {
            vm.install_module(fixture(round))
                .expect("the swap installs");
            // the run resets the root's kept entry frame onto the fresh
            // current row, so no frame pins the outgoing one past the next
            // install's sweep
            assert_eq!(
                vm.run("m", "f", &[Value::I64(1)]),
                Ok(FrameAnswer::one(Value::I64(round)))
            );
        }
        assert!(
            vm.registry.version_rows() <= 3,
            "recycled, not accumulated: {}",
            vm.registry.version_rows()
        );
    }

    /// The recycled-slot containment the gone-joiner raise arm leans on:
    /// a dead fiber's pool link belongs to the slot's current occupant
    /// (every spawn overwrites it), and a consumed nursery slot reopens
    /// with a fresh flag and ledger, so a stale trap write can never
    /// cancel a nursery a later spawn runs under.
    #[test]
    fn a_recycled_slot_names_its_own_occupant_and_a_reused_nursery_reopens_fresh() {
        let defs = vec![crate::program::FunctionDef {
            name: "f".into(),
            layout: FrameLayout::new(vec![SlotType::I64], 0, 1).expect("valid layout"),
            pool: vec![Const::I64(7)],
            code: crate::program::assemble(&[Uop::Funcf { frame_slots: 1 }, Uop::Ret { src: 0 }])
                .expect("fixture body encodes"),
        }];
        let mut vm = Vm::new(ModuleRegistry::new());
        vm.install_module(ModuleVersion::new("m", defs).expect("valid fixture module"))
            .expect("install");
        let nursery = vm.open_fresh_nursery();
        let first = vm.spawn_fiber_in(nursery, "m", "f", &[]).expect("spawn");
        assert_eq!(vm.pool[first.index()].nursery, Some(nursery));
        vm.nurseries[nursery.index()].cancelled = true;
        // the child dies and its block consumes; both slots sit reusable
        vm.pool[first.index()].state = FiberState::Dead;
        vm.nurseries[nursery.index()].state = NurseryState::Dead;
        // a bare spawn recycles the dead slot: the link is the new
        // occupant's own, so a stale write against the old id names nothing
        let recycled = vm.spawn_fiber("m", "f", &[]).expect("recycle");
        assert_eq!(recycled, first, "the dead slot is the reuse target");
        assert_eq!(vm.pool[first.index()].nursery, None);
        vm.trap_child(first, &Trap::DivideByZero);
        // the consumed nursery slot reopens fresh: the stale flag cannot
        // survive into the later spawn's block
        let reused = vm.open_fresh_nursery();
        assert_eq!(reused, nursery, "the consumed slot is the reuse target");
        assert!(!vm.nurseries[nursery.index()].cancelled);
        assert!(vm.nurseries[nursery.index()].children.is_empty());
        assert!(vm.nurseries[nursery.index()].outcomes.is_empty());
        // and a trap against the slot's current occupant names its own
        // nursery link alone — no nursery stands cancelled
        vm.trap_child(recycled, &Trap::DivideByZero);
        assert!(vm.nurseries.iter().all(|n| !n.cancelled));
    }
}
