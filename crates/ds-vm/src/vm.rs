//! The tier-1 VM: a dense-match dispatch loop [BL-3 note: stable Rust has no
//! computed goto] over encoded uop bodies, proving the locked slot-overlay
//! call ABI [BL-6, H2].
//!
//! # Decode strategy: decode on dispatch
//!
//! The `Vec<u32>` word stream is the program, and the dispatcher keys one
//! jump table on each word's raw opcode byte, extracting the operand fields
//! inline per arm; a pre-decoded cache is deliberately rejected on the same
//! terms: the encoded stream is the authoritative form (the dsmod loader
//! mmaps it [BL-7]), tier transitions are in-place J-variant rewrites of
//! the encoded words [H1] — execution must observe them — and a decode
//! cache keyed per version would be extra state to invalidate across
//! hot-reload swaps [H6 item 5]. Load decode-checks every word once, and
//! [`ds_abi::uop::Uop::decode`] stays the canonicity authority: the hot
//! arms name the same causes it would, and anything they refuse falls to
//! one slow arm that routes through the decoders, so a dispatch-time
//! [`Trap::BadEncoding`] is a canonicity violation, not a slow path. The
//! bias arithmetic is branch-free [BL-1].
//!
//! # Fibers and nurseries
//!
//! The dispatcher also owns the fiber switch mechanics over the pool in
//! `crate::fiber` [E2]: the control natives park the running fiber (record
//! its resume point), and entering a fiber swaps segment and call-chain
//! ownership between its pool slot and the VM's context — two buffer
//! swaps, O(1), no word copied. A fiber completing at its entry frame
//! marks itself dead and switches back to its resumer with its final
//! value.
//!
//! Nurseries [E2, E5] layer on top (`crate::nursery`): fibers spawn into a
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
//! combinators [E3] park the same way: a CALLG site's spawn-only body
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
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
    sync::Arc,
    time::Instant,
};

use ds_abi::{
    BIAS_J, BoundRet, CodePc, ConstIdx, FrameLayout, FrameMetadata, RC_MAJOR, RcDecodeError, RcOp,
    SlideDirection, SlideError, SlidePlan, SlotType, Staging, TailChain,
    uop::{DecodeError, MAX_FRAME_SLOTS},
};

use crate::{
    EVIDENCE_MODULE,
    claims::{CLAIM_BAND_CLEAR, CLAIM_BAND_SET, ClaimReading, ClaimTable, HookSite, hook_sites},
    fiber::{FIBER_ROOT, Fiber, FiberId, FiberState, ResumePoint, Segment},
    loader::{LoadError, StagedSite},
    module::{ModuleRegistry, ModuleVersion, VersionIdx},
    native::{
        ChannelNative, ControlNative, NURSERY_BOUNDARY_ERR, NURSERY_BOUNDARY_OK, NativeFunction,
        NativeResult, OutputSink, PLATFORM_CANCELLED, PLATFORM_NONE,
    },
    nursery::{ChildOutcome, JoinOutcome, Nursery, NurseryId, NurseryState},
    object::{Heaps, Objects},
    program::{Const, ConstFuncRef, ConstKind},
    reactor::Reactor,
    sched::{SchedState, WaitEntry, WorldWait},
    value::{ARENA_LEN_LIMIT, ArenaPackError, ArenaStr, FuncRef, RefId, Value, Word},
};

/// Default value-stack limit: 1 Mi words (8 MiB on 64-bit targets).
const DEFAULT_STACK_LIMIT: usize = 1 << 20;

/// The 2^63 boundary f64: the smallest float magnitude that cannot be an
/// i64, for the CVTF2I range check [B7].
const F64_I64_EDGE: f64 = 9_223_372_036_854_775_808.0;

/// A structured stop condition raised as an enum error, never a panic [H1:
/// the per-entry stack-limit compare is the only stack exit].
#[derive(Clone, Debug, PartialEq)]
pub enum Trap {
    /// A frame entry needs more stack words than the limit allows.
    StackOverflow {
        /// Words the entering frame needed.
        needed: usize,
        /// The configured stack limit.
        limit: usize,
    },
    /// An i64 add/sub/mul/div/mod left the i64 range [B7].
    IntegerOverflow,
    /// DIVI64/MODI64 with a zero divisor [B7].
    DivideByZero,
    /// SHLI64/SHRI64 with a shift amount outside `0..64` [B7: shifts
    /// trap past the bit width; wrapping stays the explicit operators].
    OversizedShift,
    /// CVTF2I on a NaN or a value outside the i64 range [B7].
    CvtOutOfRange,
    /// A run entry resolved through no current module slot.
    UnknownFunction {
        /// The entry module's name.
        module: String,
        /// The entry function's name.
        func: String,
    },
    /// A pool FuncRef resolved to no function in the target module's
    /// current slot. Also the host-side fiber-routing failure
    /// ([`Vm::resume_fiber`]), with the virtual module `fiber` naming the
    /// rejected pool index.
    UnknownCallee {
        /// The target module's name.
        module: String,
        /// The target function-table index.
        func: u32,
    },
    /// A scalar FuncRef word named a module id the registry never
    /// assigned: hand-assembled code or a host argument carried a bogus
    /// callable word.
    UnknownModule {
        /// The word's module id.
        module: u32,
    },
    /// A world call reached an op the installed platform does not serve:
    /// install-time validation refuses such call sites, so this fires
    /// only under bypassed validation.
    UnservedWorldOp {
        /// The unserved op's index in the platform's fixed table.
        op: u32,
    },
    /// A bound extern's C-ABI call refused at the marshaling backstop
    /// (H5): a staged word outside the scalar slice, or a signature
    /// wider than the enumerated transmute serves — shapes the lowering
    /// refuses for source programs ahead of the run, so this names
    /// hand-assembled code or a corrupted pool, never a source program.
    BoundMarshaling {
        /// The refused export's name.
        symbol: String,
        /// Why the marshaling refused.
        reason: &'static str,
    },
    /// A call or run passed the wrong argument count.
    ArityMismatch {
        /// Parameter count the callee's layout declares.
        expected: usize,
        /// Argument count the call staged.
        given: usize,
    },
    /// A slot holds (or a uop produces) a value of the wrong static type.
    SlotTypeMismatch {
        /// The offending slot index.
        slot: u8,
        /// The slot's static type.
        expected: SlotType,
        /// The value's type.
        actual: SlotType,
    },
    /// A runtime-evidence dispatch read the dead sentinel: the entering
    /// site staged no chain for a demand whose variable its branch never
    /// mentions, so the code reading it ran under a narrowing that
    /// branch cannot satisfy — reachable only through a checker gap,
    /// never a silent wrong call.
    DeadEvidenceChain {
        /// The frame slot holding the sentinel word.
        slot: u8,
    },
    /// A uop addressed a slot outside its function's frame.
    SlotOutOfRange {
        /// The addressed slot index.
        slot: u8,
        /// The function's slot count.
        frame_slots: usize,
    },
    /// The ABI's own call-site layout check rejected the call pair.
    CallLayout(SlideError),
    /// Control left the function's code stream.
    InvalidPc {
        /// The offending instruction index.
        pc: u32,
        /// The function's instruction count.
        code_len: u32,
    },
    /// `FUNCF`'s slot count disagrees with the function's layout.
    FrameSlotsMismatch {
        /// The `FUNCF` operand.
        funcef: u16,
        /// The layout's slot count.
        layout: usize,
    },
    /// The entry descriptor's word 0 disagrees with the function table index.
    /// Debug-only integrity tripwire: release builds drop the compare.
    EntryWordMismatch {
        /// The descriptor's word 0.
        resolved: usize,
        /// The function's table index.
        expected: usize,
    },
    /// A pool index is out of range for the current module version's pool.
    /// Install proves every pool operand's index (module.rs's body
    /// validation refuses the module there), so no dispatch path raises
    /// this; the variant stays the trap a future seam would name.
    PoolIndex {
        /// The offending pool index.
        kidx: u16,
    },
    /// A pool entry was loaded by an opcode of the wrong kind. Install
    /// proves every pool operand's kind, so the dispatch arms raise this
    /// only as the corrupted-pool tripwire.
    PoolKind {
        /// The offending pool index.
        kidx: u16,
        /// The kind the opcode requires.
        expected: ConstKind,
        /// The kind the entry holds.
        found: ConstKind,
    },
    /// A CALLG evidence word names no builtin instance. Lowering only
    /// mints known instance ids, so this names a corrupted stream.
    UnknownInstance {
        /// The staged evidence word.
        evidence: i64,
    },
    /// A body word does not decode: the stream is not canonical. Load
    /// rejects these, so this is defensive.
    BadEncoding {
        /// The instruction index of the word.
        pc: u32,
        /// Why the word does not decode.
        cause: DecodeError,
    },
    /// A frame base or slot holds a value word where the metadata word must
    /// be (or vice versa): hand-assembled code corrupted the layout.
    MetaWordCorrupt {
        /// The frame base whose metadata word is wrong.
        fp: usize,
    },
    /// A Ref word named no object (out of range, or the object is not the
    /// required kind — a non-string read as a string, a non-list read as
    /// a list): hand-assembled code or a host argument carried a bogus
    /// reference.
    UnknownObject {
        /// The bogus object-table index.
        obj: u32,
    },
    /// A result span (RETSPAN's, or a fallible native's tag+payload
    /// span) is wider than the caller's outgoing-argument area, so the
    /// caller could never receive it whole.
    RetSpanTooWide {
        /// The span's word count.
        span: u16,
        /// The caller's outgoing-argument slot count.
        caller_area: u16,
    },
    /// A list's per-element stride contract broke: an append staged an
    /// element span whose width disagrees with the non-empty list's
    /// stride (or a one-word-element native met a wider list). Lowered
    /// code cannot trip this — the checker holds one element type per
    /// list value — so it names hand-assembled code or a host argument.
    ElementStride {
        /// The list object's table id.
        obj: u32,
        /// The list's own stride.
        stride: usize,
        /// The width the call staged.
        given: usize,
    },
    /// A map store staged a fresh row under a reference word foreign to
    /// the map's heap: the content probe reads a stored key's payload on
    /// the map's own store and ids are per-store, so the row would be
    /// storable yet unfindable by any content probe — the store refuses
    /// before any count moves. A content-equal foreign probe still
    /// overwrites an existing row (the stored key never changes), and
    /// foreign value words stay storable.
    ForeignMapKey {
        /// The map object's table id.
        obj: u32,
        /// The refused key word.
        key: RefId,
    },
    /// An arena concatenation or its packed handle hit a field limit: a
    /// slice at or past the 24-bit length field, an offset at it, or a
    /// chunk index at the 16-bit field. Lowered code traps at the
    /// allocation boundary; hand-assembled code at the packing.
    ArenaExhausted {
        /// The field that filled.
        field: ArenaPackError,
    },
    /// An arena word reached a count-bearing site - a reference-count
    /// op's operand, a counted argument read, or a native's reference
    /// argument. The placement proof keeps every designated temporary
    /// away from each site, so lowered code never trips it; hand-assembled
    /// code does, and the trap is the misclassification tripwire.
    ArenaHandoff {
        /// The slot the word came from.
        slot: u8,
    },
    /// A hand-packed arena handle or ARENA_RESTORE cursor named memory its
    /// arena cannot serve or rewind to: a chunk index past the arena's
    /// growth, or a range or rewind target below an already-restored
    /// position. The arena names the misuse class itself.
    ArenaHandle {
        /// The arena's own misuse class.
        class: ds_rt::ArenaMisuse,
    },
    /// String content reached a text operation as invalid UTF-8.
    /// Unreachable from lowered code (every string source is UTF-8 by
    /// construction); the name keeps a corrupted range a structured
    /// refusal instead of an unchecked conversion.
    Utf8Content,
    /// The bare trap primitive's deliberate trap (the 2026-11-17 die
    /// split): a `die` spelling's message-bearing abort. The engine
    /// reports nothing itself — what the trap prints and which exit code
    /// follows is the host's own policy.
    Die {
        /// The message the spelling staged.
        message: String,
    },
    /// The `exit(code)` host member's deliberate process status (the
    /// CLI process surface, ruled 2026-11-21): like `Die`, the engine
    /// decides nothing — the code rides the trap out and the host's
    /// exit-code policy applies above it. A negative status traps at
    /// the seam (ruled 2026-11-22) and a wider one wraps to its low
    /// byte, so the carried word is always the portable 0..=255
    /// status.
    Exit {
        /// The requested process status.
        code: i64,
    },
    /// A print/println write failed at the output sink.
    OutputWrite {
        /// The class of the underlying io failure.
        kind: std::io::ErrorKind,
    },
    /// A reentrant native drive (a generic container body invoking its
    /// user lambda) could not return: the lambda's execution ended the
    /// fiber — HALT under nesting, a fiber switch whose resumer chain
    /// never came back, a nursery cancel — or parked at the host (a
    /// yield with no in-language resumer). A Rust-side loop cannot
    /// suspend, so the drive refuses instead of corrupting the frame
    /// chain; the frames stay for [`Vm::backtrace`].
    ReentrancyRefused,
    /// The IO scheduler found nothing runnable and nothing waiting (the
    /// 2026-11-21 scheduler ruling's stall guard): every fiber is parked
    /// on a wait that no reactor interest and no deadline can answer.
    /// A net wait always registers one or the other, so for those this
    /// names a scheduler bug; a channel wait registers neither (the
    /// waking peer is a fiber, and none can run), so a program whose
    /// every fiber sits parked on a channel no live fiber can serve
    /// deadlocks here by design — the honest degenerate the
    /// shared-state ruling leaves: no sender, no answer.
    SchedulerDeadlock,
    /// `chan_new(cap)` with a bound below one (the 2026-11-21
    /// shared-state ruling): the bounded channel's buffer holds at
    /// least one payload word — a zero-capacity rendezvous channel is a
    /// combinator round over the timer capability, not the buffer's
    /// degenerate.
    ChanCapacity {
        /// The refused bound.
        cap: i64,
    },
    /// `cancel(payload)` [E1, E2]: the fiber's own deliberate failure. The
    /// payload is the error value's whole word span — word 0 the
    /// failing member's tag, the rest its payload words in declaration
    /// order (the native channel's flat span shape); the trap never
    /// resumes — the trap path ledgers the outcome into the fiber's
    /// nursery and raises the cancel flag, and the boundary seam unions
    /// the payload out at the nursery's join.
    NurseryCancelled {
        /// The failing value's word span: the member tag plus its
        /// payload words.
        payload: Vec<Value>,
    },
    /// A nursery boundary's first-failure payload sits outside the
    /// caller's return-union containment set, so the join refuses to
    /// union it out; the payload rides for the trap report.
    NurseryUncontained {
        /// The payload span the boundary refused.
        payload: Vec<Value>,
    },
    /// A fork-join combinator (par_map, par_reduce) at a tail dispatch
    /// site (CALLTG): the tail call returns the reused frame at the
    /// call, but the join's answer arrives through the drive's delivery
    /// onto a parked site — the fork combinators need the CALLG site,
    /// and hand-assembled code traps here instead.
    TailForkJoin,
    /// A budgeted drive ([`Vm::run_with_budget`]) consumed its whole
    /// step budget: a compile-time evaluation whose body never
    /// terminates while its stack stays flat (a loop forgetting its
    /// decrement). The unbounded drives never raise it.
    OutOfFuel,
}

/// One entry of the call-info stack: the frame base plus the module
/// version's table row and the function the frame runs. Frames live inline
/// in the fiber's value stack; this records which version's code and pool
/// a frame executes, so retiring versions stay runnable until nothing
/// references them [H6 item 5]. The chain belongs to the fiber whose
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

/// One `Raw`-staged word a call site reads per call: the parameter
/// position (the mismatch trap's index), the staged slot's u8 address, and
/// the callee parameter's static type the word's runtime kind must match.
#[derive(Clone, Copy, Debug)]
struct RawStagedWord {
    index: u8,
    slot: u8,
    expected: SlotType,
}

/// A call site's layout-static facts, derived once per (caller, callee)
/// layout pair at cache fill and cached beside the callee: the validated
/// slide plan plus the `Raw`-staged words the site reads per call, or the
/// plan's layout-static refusal, which the call seam re-raises exactly
/// where the per-call derivation raised it (never answered as a hit-side
/// trap: the refusal rides the entry under the same key discipline, so a
/// refill after any install re-derives it).
#[derive(Clone, Debug)]
enum SiteFacts {
    /// The plan validated; `raw` is `None` when the caller's area stages
    /// no `Raw` word (then no word is read at all).
    Ready {
        plan: SlidePlan,
        raw: Option<Arc<[RawStagedWord]>>,
    },
    /// The layout pair refuses the slide.
    Refused(SlideError),
}

impl SiteFacts {
    /// Derives the facts from a caller/callee layout pair — a pure
    /// function of the pair, [`ds_abi::slide_plan`] the authority.
    // caller/callee is this contract's domain vocabulary, not a naming slip.
    #[allow(clippy::similar_names)]
    fn derive(caller: &FrameLayout, callee: &FrameLayout) -> Self {
        match ds_abi::slide_plan(caller, callee) {
            Ok(plan) => Self::Ready {
                plan,
                raw: raw_staged_words(caller, callee),
            },
            Err(error) => Self::Refused(error),
        }
    }

    /// The refusal, when the pair refused the slide.
    fn refused(&self) -> SlideError {
        match self {
            Self::Refused(error) => *error,
            Self::Ready { .. } => unreachable!("a ready site carries no refusal"),
        }
    }
}

/// The `Raw`-staged words of a proven plan the call site must read per
/// call: exactly the staged-`Raw` positions whose callee parameter is not
/// itself `Raw` (a `Raw` parameter is the wildcard — the word can never
/// fail, so the site never reads it). `None` when there is nothing to
/// read.
// caller/callee is this contract's domain vocabulary, not a naming slip.
#[allow(clippy::similar_names)]
fn raw_staged_words(caller: &FrameLayout, callee: &FrameLayout) -> Option<Arc<[RawStagedWord]>> {
    if !caller.arg_area_has_raw() {
        return None;
    }
    let start = caller.arg_area().start;
    let words: Vec<RawStagedWord> = caller.arg_types()[..callee.params_len()]
        .iter()
        .zip(callee.params())
        .enumerate()
        .filter(|&(_, (&staged, &expected))| staged == SlotType::Raw && expected != SlotType::Raw)
        .map(|(index, (_, &expected))| RawStagedWord {
            index: u8::try_from(index).expect("parameter index fits u8"),
            slot: u8::try_from(start + index).expect("slot index fits u8"),
            expected,
        })
        .collect();
    (!words.is_empty()).then(|| words.into())
}

/// An owned CALL-site resolution handed to a dispatch arm: the uop shape
/// carries the callee's version-table row plus the site's cached layout
/// facts, the native shape the shared table handle plus index — borrows
/// of the arm's own locals, so no loan on the VM outlives the arm's
/// whole-self calls (the control-native switch).
#[derive(Debug)]
enum Resolved {
    /// Enter the callee's encoded body.
    Uop {
        /// The callee's module version's table row.
        version: VersionIdx,
        /// The callee's function-table index.
        func: u32,
        /// The site's cached layout facts (the plan to transfer under).
        facts: SiteFacts,
    },
    /// Invoke the native in the caller's frame (no frame push).
    Native {
        /// The native module's shared function table.
        table: Arc<Vec<NativeFunction>>,
        /// The callee's index in the table.
        index: u32,
    },
    /// Invoke the bound artifact's C symbol in the caller's frame (H5,
    /// no frame push): the loaded module's shared symbol table, the
    /// export's enumeration index, and the return's marshaling word —
    /// the absent word or a scalar word.
    Bound {
        /// The bound module's shared symbol table.
        module: Arc<crate::bound::BoundModule>,
        /// The symbol's index in the export enumeration.
        index: u32,
        /// The return's marshaling word: the absent word or a scalar
        /// word.
        ret: BoundRet,
    },
}

/// The ownable form of a call-site resolution the inline cache stores: the
/// uop shape holds the resolved version's table row plus the site's layout
/// facts, the native shape the shared table plus index (natives are not
/// values — the table is the stable handle, the index the callee).
#[derive(Clone, Debug)]
enum CachedCallee {
    Uop {
        version: VersionIdx,
        func: u32,
        facts: SiteFacts,
    },
    Native {
        table: Arc<Vec<NativeFunction>>,
        index: u32,
    },
    /// The bound-artifact shape (H5): the loaded module's shared symbol
    /// table, the export's index, and the return's marshaling word (a
    /// bound symbol is not a value — the table is the stable handle, the
    /// index the callee).
    Bound {
        module: Arc<crate::bound::BoundModule>,
        index: u32,
        ret: BoundRet,
    },
}

/// The resolution of a dispatch site's (CALLG/CALLTG) pool FuncRef against
/// the uop module tables: the impl body a trait site's FuncRef names, or
/// the not-a-uop-callee signal that hands the site to the generic builtin
/// tables (the sentinel evidence FuncRef never reaches the resolver).
#[derive(Debug)]
enum DispatchResolved {
    /// Enter (or tail-transfer into) the named uop function's body.
    Uop {
        version: VersionIdx,
        func: u32,
        /// The site's cached layout facts (the plan to transfer under).
        facts: SiteFacts,
    },
    /// The FuncRef names no uop function: the generic builtin tables
    /// answer next.
    NotUop,
}

/// One CALL-site inline-cache slot: the resolution of (caller function,
/// pc) as of `stamp`. A hit requires all three to agree — the caller
/// identity is the executing function's address (stable for the version's
/// lifetime; a freed-and-reused address requires an install, which bumps
/// the stamp), the pc discriminates call sites within one body, and the
/// stamp is the registry generation the resolution ran against, so any
/// install (a two-slot swap, a native-table replacement) invalidates every
/// entry at once. A miss falls back to the full lookup, so dispatch
/// correctness never depends on the cache. A uop entry also carries the
/// site's layout facts (the slide plan and its `Raw`-staged reads), a pure
/// function of the caller/callee layout pair the key already pins — the
/// caller address names the caller's layout, the cached callee the
/// callee's — so the plan is re-derived exactly when the callee is, never
/// on a hit.
#[derive(Clone, Debug)]
struct CallSite {
    caller: usize,
    pc: u32,
    stamp: u64,
    callee: CachedCallee,
}

/// CALL-site cache slot count (direct-mapped, one uop per slot).
const CALL_CACHE_SLOTS: usize = 512;

/// A KSTR intern-row key: the pool's address (stable for its version's
/// lifetime) and the loading fiber — each heap interns its own copy of a
/// constant, so a cached id is only ever served against the fiber whose
/// store minted it.
type KstrRowKey = (usize, u32);

/// The VM's own table key hasher: the multiply-rotate round the claim
/// ledger and the intern map use, one per key word, so a per-frame table
/// probe never pays the default SipHash. The seed is fixed, never
/// per-process random, so a fuzz run replays exactly.
#[derive(Default)]
struct WordKeyHasher(u64);

impl WordKeyHasher {
    fn mix(&mut self, word: u64) {
        self.0 = self.0.rotate_left(13) ^ word;
        self.0 = self.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

impl Hasher for WordKeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.mix(u64::from(byte));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.mix(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }

    fn write_u32(&mut self, n: u32) {
        self.mix(u64::from(n));
    }
}

type KstrRowIndex = HashMap<KstrRowKey, u32, BuildHasherDefault<WordKeyHasher>>;

/// A derived deinit-hook directory's index: (module version address,
/// function) keys, hashed by the VM's own round.
type HookDirIndex = HashMap<(usize, u32), Arc<Vec<HookSite>>, BuildHasherDefault<WordKeyHasher>>;

/// A frame's resolved KSTR intern row: its index in the row list. Reclaim
/// drops and shifts rows, so the slot is a hint revalidated against the
/// row's own key on every load — a mismatch costs the frame one key
/// lookup, never a wrong serve.
#[derive(Clone, Copy, Debug)]
struct KstrRowSlot(u32);

/// One (pool, fiber) intern row: the object ids the pool's string
/// constants interned on that fiber's heap, by const index.
#[derive(Debug)]
struct KstrRow {
    pool: usize,
    fiber: u32,
    ids: Vec<Option<u32>>,
}

impl KstrRow {
    fn key(&self) -> KstrRowKey {
        (self.pool, self.fiber)
    }
}

/// The KSTR intern rows: a dense row list plus the key index into it. A
/// frame resolves its row once and its later loads in that frame read the
/// slot directly instead of probing the key index per load, which is the
/// literal-load cost that matters.
#[derive(Debug, Default)]
struct KstrRows {
    rows: Vec<KstrRow>,
    index: KstrRowIndex,
}

impl KstrRows {
    /// The row `slot` names, when it is still `key`'s row.
    fn row(&self, slot: KstrRowSlot, key: KstrRowKey) -> Option<&KstrRow> {
        self.rows
            .get(slot.0 as usize)
            .filter(|row| row.key() == key)
    }

    /// The key's row, opened when it has none. A reused pool address can
    /// meet a row of a dead pool's width, so the ids widen before they are
    /// indexed.
    fn open(&mut self, key: KstrRowKey, width: usize) -> (KstrRowSlot, &mut KstrRow) {
        let slot = if let Some(&slot) = self.index.get(&key) {
            slot
        } else {
            let slot = u32::try_from(self.rows.len()).expect("row slots fit u32");
            self.rows.push(KstrRow {
                pool: key.0,
                fiber: key.1,
                ids: Vec::new(),
            });
            self.index.insert(key, slot);
            slot
        };
        let row = &mut self.rows[slot as usize];
        if row.ids.len() < width {
            row.ids.resize(width, None);
        }
        (KstrRowSlot(slot), row)
    }

    /// Drops the rows `keep` refuses and rebuilds the index over the
    /// survivors: the row list stays dense, so nothing outlives its
    /// reclaim as a tombstone, and a surviving row keeps its ids.
    fn reclaim(&mut self, keep: impl Fn(&KstrRow) -> bool) {
        if self.rows.is_empty() {
            return;
        }
        self.rows.retain(keep);
        self.index.clear();
        for (slot, row) in self.rows.iter().enumerate() {
            let slot = u32::try_from(slot).expect("row slots fit u32");
            self.index.insert(row.key(), slot);
        }
    }

    /// How many (pool, fiber) rows the table holds — the bounded-memory
    /// reclamation witness for test assertions.
    fn len(&self) -> usize {
        self.rows.len()
    }
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
    /// The current fiber parked on a world op (the 2026-11-21 scheduler
    /// ruling): the op would block, the fiber parks with a resume
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
struct InstalledReactor(Box<dyn Reactor>);

impl std::fmt::Debug for InstalledReactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstalledReactor").finish_non_exhaustive()
    }
}

impl InstalledReactor {
    /// The driver behind the wrapper.
    fn driver(&mut self) -> &mut dyn Reactor {
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
    /// A channel op's park (the 2026-11-21 shared-state ruling): the
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
    nursery: NurseryId,
    /// The fiber parked at the join call (the joiner).
    joiner: FiberId,
    /// The instruction after the parked join call — where the answer lands.
    resume_pc: CodePc,
    /// Which seam parked the joiner, deciding the answer's shape.
    seam: DriveSeam,
}

/// One nursery's drive on the interlock stack: the spawn-order children
/// snapshot, the cursor of the child being driven, and who to answer when
/// the nursery consumes. The stack is the join nesting the driver used to
/// carry as nested `execute` frames — one entry per cross-nursery join
/// level, so program nesting depth costs heap, not Rust stack.
struct NurseryDrive {
    /// The nursery being driven to consumption.
    nursery: NurseryId,
    /// The children in spawn order (the snapshot at push; a child spawned
    /// later never enters this drive).
    children: Vec<FiberId>,
    /// The index of the child being driven.
    cursor: usize,
    /// The parked joiner and its resume point — the delivery target at
    /// consumption; `None` for the host faces (no call site stands).
    joiner: Option<(FiberId, CodePc)>,
    /// How the outcomes become the delivered answer.
    seam: DriveSeam,
}

/// The answer a consumed nursery owes its parked joiner.
enum Delivery {
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
enum NurseryDriveEnd {
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
enum ForkEnd {
    /// `par_reduce`'s chain continues: the next element's child spawned
    /// into its fresh nursery, one drive frame per step.
    Chain(NurseryDrive),
    /// The seam consumed: the outcomes and the parked call site's
    /// delivery (a `Raise` when the stage refused).
    End(Vec<ChildOutcome>, Option<Delivery>),
}

/// One drive segment's outcome for the frame loops: quiet, or a parked
/// cross-nursery join whose frame the stack takes.
enum StepFlow {
    /// The segment went quiet (the fiber died, parked at the host, or
    /// trapped — attributed and ledgered).
    Quiet,
    /// The driven fiber parked at a cross-nursery join.
    Interlock(InterlockRequest),
}

/// One woken fiber's entry outcome for the executor loops.
enum Woken {
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
}

/// The eager pass's outcome over one started child.
enum EagerPass {
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
fn native_head_word(result: &NativeResult) -> Value {
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
fn frame_answer_of(result: NativeResult) -> FrameAnswer {
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
enum DriveSeam {
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
enum ChanStep {
    /// The step answered: overlay like an ordinary native's result.
    Answer(NativeResult),
    /// The step parked the caller: the waiter already entered the
    /// channel's own data; the arm performs the fiber park.
    Park,
}

/// The channel step behind a `CALL` site for the dispatch arm: the
/// answer overlaid and the drive advancing past the call, or the park
/// outcome ending the drive.
enum ChanOutcome {
    /// The answer overlaid; advance past the call.
    Answered,
    /// The fiber parked; the drive ends with the event.
    Parked(DriveOutcome),
}

/// The result of a control-native switch at a `CALL` site.
enum SwitchOutcome {
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

/// The mutable execution state the frame operations share, borrowed disjoint
/// from the dispatcher's current-function snapshot. The stack is the
/// current fiber's segment; `max_len` is that fiber's high-water mark, so
/// the TCO criterion holds per fiber [H2].
struct StackState<'a> {
    stack: &'a mut Segment,
    callinfo: &'a mut Vec<CallInfo>,
    versions: &'a ModuleRegistry,
    pub(crate) limit: usize,
    max_len: &'a mut usize,
}

impl StackState<'_> {
    /// Grows the segment to `new_len` after the per-entry limit compare
    /// [H1]. Never shrinks; growth appends chunks, so live frames' words
    /// keep their addresses [H2].
    fn grow(&mut self, new_len: usize) -> Result<(), Trap> {
        if new_len <= self.stack.len() {
            return Ok(());
        }
        if new_len > self.limit {
            return Err(Trap::StackOverflow {
                needed: new_len,
                limit: self.limit,
            });
        }
        self.stack.grow_to(new_len);
        *self.max_len = (*self.max_len).max(new_len);
        Ok(())
    }

    /// Resolves a scalar FuncRef word (module id + function index, the
    /// `Fn` slot classification, F2) for the CALLS dispatch and the hosted
    /// call seam [H6 item 5]. A staged fn word is a VALUE: it carries the
    /// current table's index space (the swap-time re-key keeps it there),
    /// so `None` resolves through the target module's current slot — the
    /// hot-reload redirect a staged word picks up. `Some(row)` is the
    /// frame-own rule — the teardown pass alone uses it, driving a hook
    /// the dying frame's own code declared. Callable words address the
    /// uop module tables only — natives are not values.
    fn resolve_func_word(
        registry: &ModuleRegistry,
        funcref: FuncRef,
        owner: Option<VersionIdx>,
        nargs: u8,
    ) -> Result<(VersionIdx, u32), Trap> {
        let owner_name = owner.map(|row| registry.pinned(row).name());
        let target =
            if owner_name.is_some_and(|own| registry.module_name(funcref.module()) == Some(own)) {
                registry.resolve_indexed_in(owner.expect("checked above"), funcref.func())
            } else {
                let name = registry
                    .module_name(funcref.module())
                    .ok_or(Trap::UnknownModule {
                        module: funcref.module(),
                    })?;
                registry.resolve_indexed(name, funcref.func())
            };
        let (version, func_id) = target.ok_or_else(|| Trap::UnknownCallee {
            module: registry
                .module_name(funcref.module())
                .unwrap_or("")
                .to_owned(),
            func: funcref.func(),
        })?;
        let target = &registry.pinned(version).functions()[func_id as usize];
        debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
        if usize::from(nargs) != target.layout.params_len() {
            return Err(Trap::ArityMismatch {
                expected: target.layout.params_len(),
                given: usize::from(nargs),
            });
        }
        Ok((version, func_id))
    }

    /// Mints a scalar fn word from a pool FuncRef (the KCONST arm, F2):
    /// a retiring frame's own-module ref carries its row's index space, so
    /// when the minting frame's row no longer holds the module's current
    /// slot the mint translates the index through the identity the row
    /// records — name plus frame layout, the staged re-check's judge,
    /// extended by the content identity a moved index-embedded name
    /// judges by (body plus capture set hashing equal against exactly
    /// one current candidate) — into the current table. The minted value
    /// enters current-index-valid exactly like a re-keyed staged word,
    /// and a dropped or ambiguous identity refuses the mint instead of
    /// minting a word the current slot would silently misroute. A frame
    /// on the current row (the ordinary case) and every cross-module or
    /// native ref keep the verbatim spelling: a dependency's version the
    /// minting pool linked against is not recorded, the cross-module
    /// redirect staying the current-slot rule.
    fn minted_fn_word(
        registry: &ModuleRegistry,
        frame_row: VersionIdx,
        fref: FuncRef,
    ) -> Result<Value, Trap> {
        let own_name = registry.pinned(frame_row).name();
        if registry.module_name(fref.module()) != Some(own_name) {
            return Ok(Value::Fn(fref));
        }
        let current = registry
            .slots(own_name)
            .expect("a frame's module stays installed")
            .current_idx();
        if current == frame_row {
            return Ok(Value::Fn(fref));
        }
        let refused = || Trap::UnknownCallee {
            module: own_name.to_owned(),
            func: fref.func(),
        };
        let Some(at) = usize::try_from(fref.func()).ok() else {
            return Err(refused());
        };
        let row = registry.pinned(frame_row);
        let Some(old) = row.functions().get(at) else {
            return Err(refused());
        };
        let current_version = registry.pinned(current);
        let moved = current_version
            .functions()
            .iter()
            .position(|f| f.name == old.name && f.layout == old.layout)
            .and_then(|at| u32::try_from(at).ok())
            .or_else(|| {
                old.embeds_table_index()
                    .then(|| current_version.content_unique(old.content_hash()))
                    .flatten()
            });
        moved.map_or_else(
            || Err(refused()),
            |at| Ok(Value::Fn(FuncRef::new(fref.module(), at))),
        )
    }

    /// The per-call-site check for `Raw`-staged arguments: they carry no
    /// static type, so the staged value words are checked against the
    /// callee's parameter types here (concrete staging was already proved
    /// by the site's plan). The site's cached word list names exactly the
    /// `Raw`-staged slots whose parameter is not itself the `Raw`
    /// wildcard, so a caller staging no `Raw` word reads nothing.
    fn check_raw_staged(
        stack: &Segment,
        raw: &[RawStagedWord],
        caller: &FrameLayout,
        fp: usize,
    ) -> Result<(), Trap> {
        let body_len = caller.body_len();
        for word in raw {
            let value = read_value(stack, fp, word.slot, caller.slots(), body_len)?;
            let staged = value.slot_type();
            // a Ref word staged against a Fn parameter is the
            // callable-defaulted slot (F2): a closure object's word behind
            // a fn-typed parameter, admitted exactly like the slide admits
            // the static pairing
            if staged != word.expected
                && !(staged == SlotType::Ref && word.expected == SlotType::Fn)
            {
                return Err(Trap::CallLayout(SlideError::TypeMismatch {
                    index: usize::from(word.index),
                    staged,
                    expected: word.expected,
                }));
            }
        }
        Ok(())
    }

    /// Non-tail `CALL`: the callee's metadata word lands in the caller's
    /// call word, the staged arguments overlay the caller's outgoing-argument
    /// area with no copying, and the stack grows for the callee's frame
    /// [BL-6, H2]. The plan (the ABI's own overlay fact) arrives proven by
    /// the site's cached facts; a refused pair re-raises here, exactly
    /// where the per-call derivation raised it.
    fn call(
        &mut self,
        version: VersionIdx,
        func_id: u32,
        facts: &SiteFacts,
        caller: &FrameLayout,
        fp: usize,
        return_pc: u32,
    ) -> Result<Transfer, Trap> {
        let target = &self.versions.pinned(version).functions()[func_id as usize];
        let SiteFacts::Ready { raw, .. } = facts else {
            return Err(Trap::CallLayout(facts.refused()));
        };
        Self::check_raw_staged(self.stack, raw.as_deref().unwrap_or(&[]), caller, fp)?;
        // the callee's metadata word lands in the caller's call word:
        // fp + 1 + body_len (the pinned physical mapping)
        let callee_fp = fp + 1 + caller.body_len();
        self.grow(callee_fp + target.layout.frame_size() as usize)?;
        self.stack[callee_fp] = Word::Meta(FrameMetadata {
            return_pc: CodePc(return_pc),
            frame_size: target.layout.frame_size(),
            tail: TailChain::default(),
        });
        self.callinfo.push(CallInfo {
            fp: callee_fp,
            module: version,
            func: func_id,
        });
        Ok(Transfer {
            version,
            func: func_id,
        })
    }

    /// `CALLT`, the locked contract [H2]: keep the frame pointer, slide the
    /// staged arguments into slots `0..nargs` per the ABI slide plan (source
    /// derived from the caller's arg area), rewrite the metadata word in
    /// place (return pc preserved, callee frame size, tail chain grows by
    /// one elided frame), and enter the callee. The stack never grows at any
    /// depth; the only growth here is the one-time span extension when the
    /// callee's frame is physically larger, which is bounded by the largest
    /// callee frame, not by call depth. The plan arrives proven by the
    /// site's cached facts; a refused pair re-raises here, exactly where
    /// the per-call derivation raised it.
    fn tail_call(
        &mut self,
        version: VersionIdx,
        func_id: u32,
        facts: &SiteFacts,
        caller: &FrameLayout,
        fp: usize,
    ) -> Result<Transfer, Trap> {
        let target = &self.versions.pinned(version).functions()[func_id as usize];
        let SiteFacts::Ready { plan, raw } = facts else {
            return Err(Trap::CallLayout(facts.refused()));
        };
        Self::check_raw_staged(self.stack, raw.as_deref().unwrap_or(&[]), caller, fp)?;
        // The O(delta) slide, per the plan's direction. Two different slot
        // mappings meet here: the source slots are the CALLER's staged
        // arguments, which sit caller-relative (above the caller's call
        // word); the destination is the callee reusing the frame at the same
        // fp, whose param slot i sits at fp+1+i unconditionally, below the
        // CALLEE's own call word - never mapped through the caller's body,
        // or wide callees (params_len > caller body) would land one word off.
        let n = plan.len();
        let (dst0, src0) = (plan.dst().start, plan.src().start);
        let caller_body_len = caller.body_len();
        match plan.direction() {
            SlideDirection::None => {},
            SlideDirection::Ascending => {
                for i in 0..n {
                    let (d, s) = (fp + 1 + dst0 + i, word_of(fp, src0 + i, caller_body_len));
                    self.stack[d] = self.stack[s];
                }
            },
            SlideDirection::Descending => {
                for i in (0..n).rev() {
                    let (d, s) = (fp + 1 + dst0 + i, word_of(fp, src0 + i, caller_body_len));
                    self.stack[d] = self.stack[s];
                }
            },
        }
        let Word::Meta(meta) = &mut self.stack[fp] else {
            return Err(Trap::MetaWordCorrupt { fp });
        };
        meta.enter_tail_call(target.layout.frame_size());
        self.grow(fp + target.layout.frame_size() as usize)?;
        let frame = self.callinfo.last_mut().expect("a frame is always live");
        frame.module = version;
        frame.func = func_id;
        Ok(Transfer {
            version,
            func: func_id,
        })
    }

    /// `RET`: the result word overlays slot 0 (the caller reads it from its
    /// outgoing-argument slot 0 [H2]); the stack restores the caller's frame
    /// span; the caller resumes at the metadata word's return pc. The
    /// restore answers the caller's own chain-entry row as the new
    /// [`Active`]: the row is a copyable table name, so the return moves no
    /// Arc count in any shape — a same-module return, a cross-module
    /// return, or a swap between the two entries' rows alike — and the
    /// entry always names the version the frame was entered under [H6
    /// item 5]. A nested drive's synthetic reentry frame answers
    /// [`RetOutcome::Boundary`] instead: its caller is Rust-side code
    /// (`Vm::call_hosted`), not a frame. The sentinel return pc is the
    /// frame's identity marker — every CALL-pushed frame carries a real
    /// return pc, and a fiber switched in mid-drive never carries a
    /// reentry frame at all (a resumable fiber parks only at switch
    /// points, never mid-native), so the chain-position test cannot
    /// false-positive across a switch.
    fn ret(&mut self, value: Value, fp: usize, boundary: usize) -> Result<RetOutcome, Trap> {
        let meta = match self.stack[fp] {
            Word::Meta(m) => m,
            Word::Slot(_) => return Err(Trap::MetaWordCorrupt { fp }),
        };
        self.stack[fp + 1] = Word::Slot(value);
        if self.callinfo.len() == 1 {
            self.stack.truncate(fp + 2);
            return Ok(RetOutcome::Root(value));
        }
        if boundary != usize::MAX && self.callinfo.len() == boundary && meta.return_pc.0 == u32::MAX
        {
            // the reentry frame and everything above it leave the chain;
            // the value rides the outcome
            self.stack.truncate(fp);
            self.callinfo.pop();
            return Ok(RetOutcome::Boundary(value));
        }
        // the caller's entry answers the bounds checks (its version's code
        // length, its layout's frame span); the row is Copy, so no borrow
        // outlives the chain move
        let caller_at = self.callinfo.len() - 2;
        let caller_row = self.callinfo[caller_at].module;
        let caller_fn =
            &self.versions.pinned(caller_row).functions()[self.callinfo[caller_at].func as usize];
        let code_len = code_len_u32(caller_fn.code.len());
        if meta.return_pc.0 >= code_len {
            return Err(Trap::InvalidPc {
                pc: meta.return_pc.0,
                code_len,
            });
        }
        let restore =
            (self.callinfo[caller_at].fp + caller_fn.layout.frame_size() as usize).max(fp + 2);
        self.stack.truncate(restore);
        self.callinfo
            .pop()
            .expect("a caller entry stands below the returning frame");
        let caller = &self.callinfo[caller_at];
        Ok(RetOutcome::Resume {
            version: caller.module,
            func: caller.func,
            pc: meta.return_pc,
        })
    }
}

fn code_len_u32(len: usize) -> u32 {
    u32::try_from(len).expect("code length exceeds u32")
}

/// Maps a slot index to its stack-word index: parameters and locals sit at
/// `fp + 1 + i`; the call word sits between them and the outgoing-argument
/// area, whose slots live at `fp + 2 + i` (the frame diagram in
/// `ds-abi/src/layout.rs`). `body_len` is the parameter-plus-local count of
/// the frame's OWNING layout.
#[inline]
fn word_of(fp: usize, slot: usize, body_len: usize) -> usize {
    fp + 1 + slot + usize::from(slot >= body_len)
}

fn read_value(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<Value, Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    match stack[at] {
        Word::Slot(v) => Ok(v),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

/// Resolves one runtime-evidence sentinel dispatch (H3) over the
/// module's evidence-chain table: the frame's slot word names a chain
/// object whose `func` is the serving body and whose `next` tail list
/// answers that body's own evidence demands — one chain word per demand,
/// staged onto the caller's overlay past the real arguments exactly
/// where the static trait path's constant stores land. One static table
/// read per hop, so chain depth is table-bounded, never frame-bounded.
/// Answers the target's function-table index.
#[allow(clippy::too_many_arguments)]
fn evidence_chain_dispatch(
    vm: &mut Vm,
    version: VersionIdx,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    fp: usize,
    caller: &FrameLayout,
    nargs: u8,
    pc: CodePc,
) -> Result<u32, (Trap, CodePc)> {
    let word = read_value(&vm.stack, fp, slot, slots, body_len).map_err(|e| (e, pc))?;
    let Value::I64(chain_idx) = word else {
        return Err((
            Trap::SlotTypeMismatch {
                slot,
                expected: SlotType::I64,
                actual: word.slot_type(),
            },
            pc,
        ));
    };
    // the dead sentinel: the entering site staged no chain for a demand
    // whose variable its branch never mentions, so the code reading it
    // ran under a narrowing that branch cannot satisfy — a checker gap,
    // never a silent wrong call
    if chain_idx < 0 {
        return Err((Trap::DeadEvidenceChain { slot }, pc));
    }
    let module = vm.registry.pinned(version);
    let chains = module.chains();
    let index = usize::try_from(chain_idx)
        .ok()
        .filter(|index| *index < chains.len());
    let Some(index) = index else {
        return Err((
            Trap::UnknownCallee {
                module: EVIDENCE_MODULE.to_owned(),
                func: chain_idx.try_into().unwrap_or(u32::MAX),
            },
            pc,
        ));
    };
    let chain = &chains[index];
    let func_id = chain.func;
    let target = &module.functions()[func_id as usize];
    let params_len = target.layout.params_len();
    let riders = chain.next.len();
    // the exact sentinel-arity contract: the chain's tail list was built
    // one entry per the serving body's own evidence demands, so the
    // parameter count must be the staged real-argument count plus it — a
    // gap names a corrupted table (a one-word receiver staged against a
    // multi-word `self`), never a silent slide of a
    // rider into the parameter span
    if usize::from(nargs) + riders != params_len {
        return Err((
            Trap::ArityMismatch {
                expected: params_len,
                given: usize::from(nargs),
            },
            pc,
        ));
    }
    // the riders share the overlay with the staged arguments; the
    // lowering reserves the widest serving body's demand count, so an
    // escape names hand-assembled code
    if usize::from(nargs) + riders > caller.arg_area_len() {
        return Err((
            Trap::CallLayout(SlideError::StagingTooSmall {
                available: caller.arg_area_len(),
                needed: usize::from(nargs) + riders,
            }),
            pc,
        ));
    }
    let caller_body_len = caller.body_len();
    for (i, tail) in chain.next.iter().enumerate() {
        let slot = u8::try_from(caller.arg_area().start + usize::from(nargs) + i)
            .expect("slot index fits u8");
        let at = word_of(fp, usize::from(slot), caller_body_len);
        vm.stack[at] = Word::Slot(Value::I64(i64::from(*tail)));
    }
    Ok(func_id)
}

/// Reads a slot typed `want`; the value-type check doubles as the check of
/// the static layout (writes enforce layout and value agreement).
fn read_typed(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    want: SlotType,
) -> Result<Value, Trap> {
    let v = read_value(stack, fp, slot, slots, body_len)?;
    if v.slot_type() != want {
        return Err(Trap::SlotTypeMismatch {
            slot,
            expected: want,
            actual: v.slot_type(),
        });
    }
    Ok(v)
}

/// The frame word one slot read addresses, with its static bounds: the
/// shared front of every specialized slot reader (the typed check is the
/// caller's match on the word itself).
#[inline]
fn slot_word<'a>(
    stack: &'a Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<&'a Word, Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    Ok(&stack[at])
}

fn read_i64(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<i64, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::I64(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::I64,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

fn read_f64(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<f64, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::F64(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::F64,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

fn read_bool(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<bool, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::Bool(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::Bool,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

fn read_tag(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<u8, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::Tag(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::Tag,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

fn read_ref(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<RefId, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::Ref(word)) => Ok(*word),
        // an arena word in a counted read is a broken proof or
        // hand-assembled code: the design keeps designated temporaries
        // away from every count funnel, and this trap is the tripwire
        Word::Slot(Value::Arena(_)) => Err(Trap::ArenaHandoff { slot }),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::Ref,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

/// The reference one RC op moves, when the word holds one: a `Raw` slot's
/// kind is a runtime fact (a type-variable value), so its inc/dec apply
/// only when the word is a reference at runtime and no-op on scalars;
/// every other slot classification keeps the strict typed read.
fn read_counted(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<Option<RefId>, Trap> {
    if slots[usize::from(slot)] == SlotType::Raw {
        return match read_value(stack, fp, slot, slots, body_len)? {
            Value::Ref(word) => Ok(Some(word)),
            Value::Arena(_) => Err(Trap::ArenaHandoff { slot }),
            _ => Ok(None),
        };
    }
    read_ref(stack, fp, slot, slots, body_len).map(Some)
}

/// The string content behind one operand of the string data ops: a heap
/// string word routed to its own store, or an arena handle read from the
/// running fiber's arena. The operands arrive already slot-checked
/// (`read_typed` against `SlotType::Ref`), so the remaining work is
/// content routing. The direct field borrows (never a `&self` method)
/// keep the returned slices alive beside the arena's interior-mutable
/// allocation in the dispatcher's arms.
fn str_content<'a>(
    pool: &'a [Fiber],
    objects: &'a Objects,
    ctx_live: bool,
    current: FiberId,
    word: Value,
) -> Result<&'a [u8], Trap> {
    match word {
        Value::Ref(id) => {
            let store = if ctx_live && id.fiber() == current {
                objects
            } else {
                &pool[id.fiber().index()].objects
            };
            Ok(store.str_at(id.id())?.as_bytes())
        },
        Value::Arena(handle) => {
            let (chunk, offset, len) = handle.unpack();
            let bytes = pool[current.index()]
                .arena
                .bytes_at(ds_rt::Mark::at(chunk, offset), len)
                .map_err(|class| Trap::ArenaHandle { class })?;
            // SAFETY: the claim ledger is the aliasing discipline for arena
            // words: the running frame granted these bytes and no restore
            // reclaims them before the instruction reading them completes.
            Ok(unsafe { &*bytes })
        },
        _ => unreachable!("slot kind agrees with the variant"),
    }
}

/// Reads and compares both operands' string contents of an EQSTR/NEQSTR:
/// structural byte equality across both string domains.
fn str_eq_content(
    vm: &mut Vm,
    fp: usize,
    lhs: u8,
    rhs: u8,
    slots: &[SlotType],
    body_len: usize,
    pc: CodePc,
) -> Result<bool, (Trap, CodePc)> {
    let (x, y) = (
        read_typed(&vm.stack, fp, lhs, slots, body_len, SlotType::Ref).map_err(|e| (e, pc))?,
        read_typed(&vm.stack, fp, rhs, slots, body_len, SlotType::Ref).map_err(|e| (e, pc))?,
    );
    // interning makes equal string constants one object, and an arena word
    // names one slice: an equal word pair holds equal content
    if x == y {
        return Ok(true);
    }
    let xb = str_content(&vm.pool, &vm.objects, vm.ctx_live, vm.current, x).map_err(|e| (e, pc))?;
    let yb = str_content(&vm.pool, &vm.objects, vm.ctx_live, vm.current, y).map_err(|e| (e, pc))?;
    Ok(xb == yb)
}

/// Joins two operand contents into one owned UTF-8 string. Every byte
/// range reaches here from a heap or pool `str` or from an arena slice the
/// concat op wrote, so the join is one validation total: a range a
/// hand-assembled or corrupted operand names stays a named refusal, and
/// since the join copies every byte of both operands one pass over the
/// result answers for both.
fn concat_text(xb: &[u8], yb: &[u8]) -> Result<String, Trap> {
    let mut joined = Vec::with_capacity(xb.len() + yb.len());
    joined.extend_from_slice(xb);
    joined.extend_from_slice(yb);
    String::from_utf8(joined).map_err(|_| Trap::Utf8Content)
}

/// Writes a value into a slot, checking it against the slot's static type:
/// hand-assembled code that mis-stages a value traps here.
fn write_slot(
    stack: &mut Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    value: Value,
) -> Result<(), Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    // a Fn slot is callable-defaulted (F2): a closure object's Ref word
    // may inhabit it at every read (the entry check, the generic
    // dispatch, the callable-word call), so the write admits the same
    // pairing — a bodied wrapper's pass-through stages its callable
    // parameter through a Fn-typed temp, and a capturing argument
    // arrives as a Ref. The pairing stays one-directional: a Ref slot
    // (a value site) never takes a scalar Fn word.
    let actual = value.slot_type();
    if slots[i] != actual
        && slots[i] != SlotType::Raw
        && !(slots[i] == SlotType::Fn && actual == SlotType::Ref)
    {
        return Err(Trap::SlotTypeMismatch {
            slot,
            expected: slots[i],
            actual,
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    stack[at] = Word::Slot(value);
    Ok(())
}

/// DIVI64 [B7]: zero divide and `MIN / -1` are distinct traps.
fn int_div(x: i64, y: i64) -> Result<i64, Trap> {
    if y == 0 {
        return Err(Trap::DivideByZero);
    }
    x.checked_div(y).ok_or(Trap::IntegerOverflow)
}

/// MODI64 [B7]: same trap split as [`int_div`].
fn int_mod(x: i64, y: i64) -> Result<i64, Trap> {
    if y == 0 {
        return Err(Trap::DivideByZero);
    }
    x.checked_rem(y).ok_or(Trap::IntegerOverflow)
}

/// SHLI64/SHRI64/BITAND/BITXOR/BITORI64 [B7]: the shift amounts trap
/// outside the 0..64 window (no wrap form exists, so both carry a trap
/// edge like the trapping arithmetic), `>>` is the arithmetic
/// (sign-propagating) shift, and the bitwise trio is total. The dispatch
/// reaches this through its one range arm over the five opcodes.
fn shift_bitwise_word(
    stack: &mut Segment,
    op: u8,
    dst: u8,
    lhs: u8,
    rhs: u8,
    fp: usize,
    slots: &[SlotType],
    body_len: usize,
) -> Result<(), Trap> {
    let (x, y) = (
        read_i64(stack, fp, lhs, slots, body_len)?,
        read_i64(stack, fp, rhs, slots, body_len)?,
    );
    let value = match op {
        OP_SHL_I64 => {
            let Some(shift) = u32::try_from(y).ok().filter(|&y| y < 64) else {
                return Err(Trap::OversizedShift);
            };
            Value::I64(x << shift)
        },
        OP_SHR_I64 => {
            let Some(shift) = u32::try_from(y).ok().filter(|&y| y < 64) else {
                return Err(Trap::OversizedShift);
            };
            // Rust's i64 `>>` is arithmetic: the sign bit propagates,
            // the shape B7 freezes
            Value::I64(x >> shift)
        },
        OP_BITAND_I64 => Value::I64(x & y),
        OP_BITXOR_I64 => Value::I64(x ^ y),
        OP_BITOR_I64 => Value::I64(x | y),
        _ => unreachable!("the range arm partitions the quintet"),
    };
    write_slot(stack, fp, dst, slots, body_len, value)
}

// The peek-next load-op fusion [BL-4, the opcode census' ranked leader]:
// a constant load whose immediately following word is one of the measured
// census arith pairs consuming the loaded slot runs both uops inside the
// load's dispatch arm — one dispatch for two uops, pc += 2. The uop
// stream and the module format stay byte-identical (no new uops; the peek
// only reads a word the loader already validated). Three invariants hold
// by construction: the fused arith's traps and slot errors attribute to
// its own pc (pc+1), so a trap is indistinguishable from the unfused run;
// an intervening RC word fails the opcode-byte match (the census adjacency
// rule), so a counted word between the pair never fuses; and a load at
// the code end peeks nothing (`get` on the out-of-bounds index). DIVI64
// stays unfused: the census never measured the pair.

/// The fused pair shape for a KIMM at `idx`: the next word's opcode byte
/// and register fields when it is an i64 arith (ADD/SUB/MUL/MOD — the
/// measured census pairs) whose lhs or rhs operand reads the load's dst.
/// Returns `(opcode, arith dst, lhs, rhs)`.
fn fused_i64_arith(code: &[u32], idx: usize, dst: u8) -> Option<(u8, u8, u8, u8)> {
    let [op, adst, lhs, rhs] = code.get(idx + 1)?.to_be_bytes();
    if lhs != dst && rhs != dst {
        return None;
    }
    matches!(op, OP_ADD_I64 | OP_SUB_I64 | OP_MUL_I64 | OP_MOD_I64).then_some((op, adst, lhs, rhs))
}

/// The fused pair shape for a KCONST at `idx`: the float counterpart of
/// [`fused_i64_arith`] over the measured census pairs (ADD/MUL F64).
fn fused_f64_arith(code: &[u32], idx: usize, dst: u8) -> Option<(u8, u8, u8, u8)> {
    let [op, adst, lhs, rhs] = code.get(idx + 1)?.to_be_bytes();
    if lhs != dst && rhs != dst {
        return None;
    }
    matches!(op, OP_ADD_F64 | OP_MUL_F64).then_some((op, adst, lhs, rhs))
}

// The peek-next compare-branch fusion [BL-4, the opcode census' ranked
// second family]: a comparison whose immediately following word is the
// measured census branch spelling — JMPIF_NOT, the universal loop-head
// exit — testing the compare's own dst takes its branch decision inside
// the compare's dispatch: one dispatch for two uops, pc += 2, the uop
// stream and the module format staying byte-identical. The bool write
// stays (the dst may be read on either path), and the branch runs through
// the exact JMPIF_NOT arm body, so the `jump!` macro's bounds check, trap
// attribution, and taken-backward cancel observation read the branch's
// own pc exactly as the unfused spelling does — a fused back-edge
// preempts a parked fiber where the two-dispatch run does, jump-to-self
// included. An intervening word fails the opcode-byte match, and a
// compare at the code end peeks nothing (`code.get` on the
// out-of-bounds index). JMPIF consuming the compare's dst never fuses
// (the census measured no such pair), and neither do the unmeasured
// compares.

/// The fused pair shape for a comparison at `idx`: the next word's biased
/// jump field when it is a JMPIF_NOT whose tested cond is the compare's
/// own dst.
fn fused_jmpif_not(code: &[u32], idx: usize, dst: u8) -> Option<u16> {
    let [op, cond, hi, lo] = code.get(idx + 1)?.to_be_bytes();
    if cond != dst {
        return None;
    }
    (op == OP_JMPIF_NOT).then_some(u16::from_be_bytes([hi, lo]))
}

// The peek-next tag-branch fusion [BL-4, the census' ranked third family]:
// a TAGTEST whose immediately following word is the measured census branch
// spelling — JMPIF, the match dispatch's arm-guard exit — testing the tag
// test's own dst takes its branch decision inside the tag test's dispatch,
// the compare-branch slice's discipline exactly: one dispatch for two
// uops, the uop stream and the module format staying byte-identical. The
// bool write stays (the dst may be read on either path), and the branch
// runs through the exact JMPIF arm body, so the `jump!` macro's bounds
// check, trap attribution, and taken-backward cancel observation read the
// branch's own pc exactly as the unfused spelling does — a fused back-edge
// preempts a parked fiber where the two-dispatch run does, jump-to-self
// included. An intervening word fails the opcode-byte match, and a tag
// test at the code end peeks nothing (`code.get` on the out-of-bounds
// index). JMPIF_NOT after a TAGTEST never fuses (the census measured no
// such pair — the guard loops the lowering emits around tag tests spell
// JMPIF), and TAGTEST_NOT stays outside the family entirely: no kernel
// stream carries the uop at all, so it has no measured pair.

/// The fused pair shape for a tag test at `idx`: the next word's biased
/// jump field when it is a JMPIF whose tested cond is the tag test's own
/// dst.
fn fused_jmpif(code: &[u32], idx: usize, dst: u8) -> Option<u16> {
    let [op, cond, hi, lo] = code.get(idx + 1)?.to_be_bytes();
    if cond != dst {
        return None;
    }
    (op == OP_JMPIF).then_some(u16::from_be_bytes([hi, lo]))
}

/// Reads a closure word's header and stages its capture block through the
/// shared buffer, routing to the word's own heap. The disjoint-field
/// parameters keep the staged slice's borrow on the staging buffer alone,
/// so the caller may push frames and route count mutations while it lives.
fn stage_closure<'s>(
    objects: &mut Objects,
    pool: &mut [Fiber],
    owner: FiberId,
    staging: &'s mut CaptureStaging,
    word: RefId,
) -> Result<(FuncRef, &'s [Value]), Trap> {
    let store = if word.fiber() == owner {
        objects
    } else {
        pool.get_mut(word.fiber().index())
            .map(|fiber| &mut fiber.objects)
            .ok_or(Trap::UnknownObject { obj: word.id() })?
    };
    let (header, block) = store.closure_at(word.id())?;
    Ok((header, staging.stage(block)))
}

/// Runs a native in the caller's frame: reads the staged overlay words
/// as arguments (type-checked against the native's parameter list, the
/// same discipline a uop callee's layout gets) and invokes the body. The
/// CALL site then places the result on the caller's overlay words
/// ([`overlay_native`]); the CALLT site returns it from the tail
/// position. No frame is pushed and no metadata word moves. A free
/// function over the disjoint state the dispatcher hands it (stack, heap,
/// sink), so the registry borrow the callee came from stays alive.
fn invoke_native(
    nf: &NativeFunction,
    staged: usize,
    stack: &mut Segment,
    heaps: &mut Heaps,
    out: &mut OutputSink,
    caller: &FrameLayout,
    fp: usize,
) -> Result<NativeResult, Trap> {
    let body_len = caller.body_len();
    if nf.params.len() > caller.arg_area_len() || staged < nf.params.len() {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: caller.arg_area_len(),
            needed: nf.params.len(),
        }));
    }
    let start = caller.arg_area().start;
    // the argument list stages inline — every native table entry sits well
    // under the inline width — with the heap buffer the oversized fallback;
    // a variadic native reads the whole staged span (its `params` cover the
    // named prefix), every other native exactly its parameter count
    let words = if nf.variadic { staged } else { nf.params.len() };
    let mut inline = [Value::I64(0); INLINE_WORDS];
    let mut heap = Vec::new();
    let args: &[Value] = if words <= INLINE_WORDS {
        for (i, word) in inline.iter_mut().enumerate().take(words) {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            *word = read_value(stack, fp, slot, caller.slots(), body_len)?;
        }
        &inline[..words]
    } else {
        heap.reserve(words);
        for i in 0..words {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            heap.push(read_value(stack, fp, slot, caller.slots(), body_len)?);
        }
        &heap
    };
    for (index, (&value, &want)) in args.iter().zip(nf.params).enumerate() {
        // a `Raw` parameter is the wildcard: the native's argument kind
        // varies by call site (list_push's element), so the body receives
        // whatever word staged
        let kind = value.slot_type();
        if kind != want && want != SlotType::Raw {
            return Err(Trap::CallLayout(SlideError::TypeMismatch {
                index,
                staged: kind,
                expected: want,
            }));
        }
    }
    (nf.func)(heaps, out, args)
}

/// The generic dispatch's staged-span contract shared by the CALLG and
/// CALLTG paths: the two span forms take at least their named words plus
/// one span word (`cons`'s head span riding the front, a fold's
/// accumulator span between the leading and trailing named words), a
/// fixed entry stages exactly its named count, and the evidence words
/// need the overlay past the whole staged span either way. Answers the
/// needed width.
fn generic_staged_check(g: &crate::native::GenericFunction, nargs: u8) -> Result<usize, Trap> {
    let given = usize::from(nargs);
    let arity_ok = match g.staging {
        Staging::Fixed => given == g.params.len(),
        Staging::Leading => given >= g.params.len(),
        Staging::Interior => given > g.params.len(),
    };
    if !arity_ok {
        return Err(Trap::ArityMismatch {
            expected: g.params.len(),
            given,
        });
    }
    Ok(given + usize::from(g.evidence))
}

/// The generic dispatch's kind check over the named words: a `Raw`
/// parameter is the wildcard (the staged value words carry the call
/// site's concrete kinds), and a `Ref` word against a `Fn` parameter is
/// the callable-defaulted slot (F2) — the combinators' staged lambda
/// may be a closure object. The span forms' dynamic words are the call
/// site's own fact and stay unchecked: a `Leading` entry checks its
/// trailing named words, an `Interior` one its leading named words plus
/// the final named word at the staged span's end. The mismatch's index
/// names the offending word's position among the named words.
fn generic_kind_check(g: &crate::native::GenericFunction, args: &[Value]) -> Result<(), Trap> {
    let check = |index: usize, value: &Value, want: SlotType| -> Result<(), Trap> {
        let staged = value.slot_type();
        if staged != want
            && want != SlotType::Raw
            && !(staged == SlotType::Ref && want == SlotType::Fn)
        {
            return Err(Trap::CallLayout(SlideError::TypeMismatch {
                index,
                staged,
                expected: want,
            }));
        }
        Ok(())
    };
    match g.staging {
        Staging::Fixed => args
            .iter()
            .zip(g.params)
            .enumerate()
            .try_for_each(|(index, (&value, &want))| check(index, &value, want)),
        Staging::Leading => {
            let named = g.params.len();
            args[args.len() - named..]
                .iter()
                .zip(g.params)
                .enumerate()
                .try_for_each(|(index, (&value, &want))| check(index, &value, want))
        },
        Staging::Interior => {
            let lead = g.params.len() - 1;
            (0..lead).try_for_each(|index| check(index, &args[index], g.params[index]))?;
            check(
                lead,
                args.last().expect("the arity check ran"),
                g.params[lead],
            )
        },
    }
}

/// Places a native's result on the caller's outgoing-argument overlay:
/// one word on argument slot 0, a fallible native's two-word
/// tag+payload span [BL-2] on argument slots 0 and 1, or a span
/// channel's discriminant plus multi-word payload on slots
/// `0 ..= payload.len()`. Like a RET's write, this bypasses slot typing:
/// the overlay words belong to the call ABI, and reading them as typed
/// slots is the lowering's discipline (mixed shapes declare the slots
/// `Raw`). The fixed widths are pre-checked at the call site; a span
/// channel answer's width is runtime data, so its write bounds-checks
/// against the overlay area here (the lowering sizes the area, RETSPAN's
/// discipline — a violation names hand-assembled code).
fn overlay_native(
    result: NativeResult,
    stack: &mut Segment,
    caller: &FrameLayout,
    fp: usize,
) -> Result<(), Trap> {
    let body_len = caller.body_len();
    let start = caller.arg_area().start;
    match result {
        NativeResult::Word(value) => {
            stack[word_of(fp, start, body_len)] = Word::Slot(value);
        },
        // a park request never overlays: the site parked, the wake
        // delivering the completed answer there instead
        NativeResult::Park(_) => unreachable!("a world park never overlays"),
        NativeResult::Tag { tag, payload } => {
            debug_assert!(caller.arg_area_len() >= 2, "width pre-checked");
            stack[word_of(fp, start, body_len)] = Word::Slot(Value::Tag(tag));
            stack[word_of(fp, start + 1, body_len)] = Word::Slot(payload);
        },
        NativeResult::Span { tag, payload } => {
            let span = 1 + payload.len();
            if span > caller.arg_area_len() {
                return Err(Trap::RetSpanTooWide {
                    span: u16::try_from(span).expect("span widths fit u16"),
                    caller_area: u16::try_from(caller.arg_area_len())
                        .expect("argument areas fit u16"),
                });
            }
            stack[word_of(fp, start, body_len)] = Word::Slot(Value::Tag(tag));
            for (index, value) in payload.into_iter().enumerate() {
                let slot = start + 1 + index;
                stack[word_of(fp, slot, body_len)] = Word::Slot(value);
            }
        },
        // the tag-less total rides the same bounds-checked write with no
        // discriminant: the words are the answer, slots `0 .. n`
        NativeResult::Total { words } => {
            if words.len() > caller.arg_area_len() {
                return Err(Trap::RetSpanTooWide {
                    span: u16::try_from(words.len()).expect("span widths fit u16"),
                    caller_area: u16::try_from(caller.arg_area_len())
                        .expect("argument areas fit u16"),
                });
            }
            for (index, value) in words.into_iter().enumerate() {
                let slot = start + index;
                stack[word_of(fp, slot, body_len)] = Word::Slot(value);
            }
        },
        // a park request never reaches an overlay: the CALLG path parks
        // (or refuses and releases) before it, and the CALLTG site
        // refuses the fork before its spawn phase runs
        NativeResult::Fork { .. } => {
            unreachable!("the dispatcher parks or refuses a fork request first")
        },
    }
    Ok(())
}

/// Reads the i64 the caller staged on overlay word `index` for a control
/// native — the same staged-type discipline `invoke_native` applies to
/// ordinary natives.
fn control_i64(
    stack: &Segment,
    fp: usize,
    caller: &FrameLayout,
    index: usize,
) -> Result<i64, Trap> {
    let body_len = caller.body_len();
    let slot = u8::try_from(caller.arg_area().start + index).expect("slot index fits u8");
    match read_value(stack, fp, slot, caller.slots(), body_len)? {
        Value::I64(v) => Ok(v),
        v => Err(Trap::CallLayout(SlideError::TypeMismatch {
            index,
            staged: v.slot_type(),
            expected: SlotType::I64,
        })),
    }
}

/// Reads one staged word of any kind on overlay word `index` for a
/// control native (the callable and cancel seams carry words whose kinds
/// the call site proves, not the table).
fn control_word(
    stack: &Segment,
    fp: usize,
    caller: &FrameLayout,
    index: usize,
) -> Result<Value, Trap> {
    let body_len = caller.body_len();
    let slot = u8::try_from(caller.arg_area().start + index).expect("slot index fits u8");
    read_value(stack, fp, slot, caller.slots(), body_len)
}

/// The control paths' infallible one-word overlay: a `Word` write has no
/// width check to fail (only the span channel's dynamic width can), so
/// the switch and delivery sites keep the unchecked shape.
fn overlay_word(value: Value, stack: &mut Segment, caller: &FrameLayout, fp: usize) {
    let body_len = caller.body_len();
    let start = caller.arg_area().start;
    stack[word_of(fp, start, body_len)] = Word::Slot(value);
}

/// The boundary seam's infallible tag-plus-flat-span overlay: the area
/// is checked against `1 + payload.len()` by the caller
/// ([`overlay_boundary`]), so like [`overlay_word`] the write cannot
/// fail.
fn overlay_tag(tag: u8, payload: &[Value], stack: &mut Segment, caller: &FrameLayout, fp: usize) {
    debug_assert!(
        caller.arg_area_len() >= 2,
        "the static floor is pre-checked"
    );
    let body_len = caller.body_len();
    let start = caller.arg_area().start;
    stack[word_of(fp, start, body_len)] = Word::Slot(Value::Tag(tag));
    for (index, value) in payload.iter().enumerate() {
        let slot = start + 1 + index;
        debug_assert!(
            slot < caller.arg_area().start + caller.arg_area_len(),
            "width pre-checked"
        );
        stack[word_of(fp, slot, body_len)] = Word::Slot(*value);
    }
}

/// The pre-invocation width check for a native call: the callee's result
/// must fit the overlay area that receives it. For a CALL that is the
/// caller's staging area; for a CALLT the span lands on the ORIGINAL
/// caller's overlay words (the tail frame's slots alias them), so the
/// frame below supplies the area. Checked before the native runs — a
/// world-effect side effect must not happen only for the call to trap
/// on the result width. The checked width is the static `result_words`
/// floor; a span channel answer's extra words bounds-check at the
/// overlay write.
fn check_native_overlay(area: usize, nf: &NativeFunction) -> Result<(), Trap> {
    if usize::from(nf.result_words) > area {
        return Err(Trap::RetSpanTooWide {
            span: u16::from(nf.result_words),
            caller_area: u16::try_from(area).expect("argument areas fit u16"),
        });
    }
    Ok(())
}

/// The pre-invocation checks for a native CALL, in trap order: the staged
/// arguments must fit the overlay area (the same StagingTooSmall a uop
/// callee's layout yields), then the result width must fit it too.
fn check_native_call_site(caller: &FrameLayout, nf: &NativeFunction) -> Result<(), Trap> {
    let area = caller.arg_area_len();
    if nf.params.len() > area {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: area,
            needed: nf.params.len(),
        }));
    }
    check_native_overlay(area, nf)
}

/// The pre-invocation width check for a bound extern's CALL (H5): the
/// staged words and the one overlay word the scalar answer takes must
/// fit the caller's staging area — the same discipline
/// [`check_native_call_site`] applies, checked before the foreign call
/// runs so a world-effect side effect never happens only for the trap.
fn check_bound_call_site(caller: &FrameLayout, staged: usize) -> Result<(), Trap> {
    let area = caller.arg_area_len();
    if staged.max(1) > area {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: area,
            needed: staged.max(1),
        }));
    }
    Ok(())
}

/// The pre-invocation width check for a bound extern's CALLT: the one
/// overlay word lands on the ORIGINAL caller's area (the tail frame's
/// slot 0 aliases it), so the frame below supplies the area — the same
/// rule [`check_native_overlay`] applies to a native's `result_words`.
fn check_bound_overlay(area: usize) -> Result<(), Trap> {
    if area < 1 {
        return Err(Trap::RetSpanTooWide {
            span: 1,
            caller_area: u16::try_from(area).expect("argument areas fit u16"),
        });
    }
    Ok(())
}

/// Runs one bound extern's C-ABI call (H5) in the caller's frame: the
/// staged words read exactly like a native's, each marshaling per its
/// own kind — a scalar word singly, a string or bytes word as its
/// call-scoped (pointer, length) pair over the borrowed content, a heap
/// word routed to its owning store and an arena handle read from the
/// running fiber's arena (the lowering proved the written signature
/// inside the marshaling slice ahead of the run) — and the foreign
/// answer boxes by the pool constant's return word, a unit return
/// answering the canonical unit word. A blocking call pins the calling
/// fiber for its whole duration — no preemption point exists inside the
/// C call — and a foreign abort kills the process (panic=abort, the VM
/// never unwinding through a foreign frame).
#[allow(clippy::too_many_arguments)]
fn invoke_bound(
    pool: &[Fiber],
    objects: &Objects,
    ctx_live: bool,
    current: FiberId,
    module: &crate::bound::BoundModule,
    index: u32,
    ret: BoundRet,
    staged: usize,
    stack: &mut Segment,
    caller: &FrameLayout,
    fp: usize,
) -> Result<Value, Trap> {
    let body_len = caller.body_len();
    check_bound_call_site(caller, staged)?;
    let start = caller.arg_area().start;
    let mut inline = [Value::I64(0); INLINE_WORDS];
    let mut heap = Vec::new();
    let args: &[Value] = if staged <= INLINE_WORDS {
        for (i, word) in inline.iter_mut().enumerate().take(staged) {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            *word = read_value(stack, fp, slot, caller.slots(), body_len)?;
        }
        &inline[..staged]
    } else {
        heap.reserve(staged);
        for i in 0..staged {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            heap.push(read_value(stack, fp, slot, caller.slots(), body_len)?);
        }
        &heap
    };
    let outside = |word: &'static str| Trap::BoundMarshaling {
        symbol: module.name(index).to_owned(),
        reason: word,
    };
    let mut params = Vec::with_capacity(args.len());
    let mut marshaled = Vec::with_capacity(args.len());
    for &value in args {
        match value {
            Value::I64(word) => {
                params.push(ds_ffi::Param::Int);
                marshaled.push(ds_ffi::Arg::Int(word));
            },
            Value::Bool(flag) => {
                params.push(ds_ffi::Param::Bool);
                marshaled.push(ds_ffi::Arg::Bool(flag));
            },
            Value::F64(number) => {
                params.push(ds_ffi::Param::Float);
                marshaled.push(ds_ffi::Arg::Float(number));
            },
            // a string or bytes word marshals as its call-scoped
            // (pointer, length) pair: a heap word's own payload decides
            // the encoding (the store holds exactly one of the two
            // kinds), an arena handle is a concat temporary over the
            // running fiber's arena
            Value::Ref(id) => {
                let store = if ctx_live && id.fiber() == current {
                    objects
                } else {
                    &pool[id.fiber().index()].objects
                };
                if let Ok(text) = store.str_at(id.id()) {
                    params.push(ds_ffi::Param::Str);
                    marshaled.push(ds_ffi::Arg::Str(text));
                } else if let Ok(data) = store.bytes_at(id.id()) {
                    params.push(ds_ffi::Param::Bytes);
                    marshaled.push(ds_ffi::Arg::Bytes(data));
                } else {
                    return Err(outside(
                        "a staged word outside the int, bool, float, string, and bytes slice",
                    ));
                }
            },
            Value::Arena(_) => {
                let bytes = str_content(pool, objects, ctx_live, current, value)
                    .map_err(|_| outside("a staged arena word outside the running fiber"))?;
                let text = core::str::from_utf8(bytes)
                    .map_err(|_| outside("a staged arena word outside UTF-8 text"))?;
                params.push(ds_ffi::Param::Str);
                marshaled.push(ds_ffi::Arg::Str(text));
            },
            _ => {
                return Err(outside(
                    "a staged word outside the int, bool, float, string, and bytes slice",
                ));
            },
        }
    }
    let symbol = module.symbol(index).ok_or_else(|| Trap::BoundMarshaling {
        symbol: module.name(index).to_owned(),
        reason: "an export index past the artifact's enumeration",
    })?;
    let sig = ds_ffi::Sig {
        params,
        ret: crate::bound::marshal_ret(ret),
    };
    let answer = symbol
        .call(&sig, &marshaled)
        .map_err(|_| Trap::BoundMarshaling {
            symbol: module.name(index).to_owned(),
            reason: "the enumerated transmute serves at most the marshaling arity cap",
        })?;
    Ok(crate::bound::answer_value(answer, ret))
}

/// Where a backtrace frame stands: a bytecode position, or parked inside
/// a hosted native's reentrant call — the synthetic reentry frame
/// `[Vm::call_hosted]` pushed carries the entry-frame sentinel return pc,
/// which names no real position, so the frame's caller renders the seam
/// instead of a bogus integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePc {
    /// A real position in the frame's function: the trapping uop for the
    /// innermost frame, the callee's saved return pc for callers.
    At(CodePc),
    /// The frame called a native that reenters the VM per element (the
    /// container combinators' hosted calls): the frame parks inside the
    /// native, so no bytecode position exists.
    Hosted,
}

/// One backtrace entry: a real frame plus its tail chain's elided count [H2].
#[derive(Clone, Debug, PartialEq)]
pub struct BacktraceFrame {
    /// The frame's module.
    pub module: String,
    /// The frame's function.
    pub func: String,
    /// Where the frame is.
    pub pc: FramePc,
    /// How many logical frames this physical frame elided via tail calls
    /// since the normal call that created it.
    pub elided: u32,
}

/// The tier-1 VM. Construct with a [`ModuleRegistry`], then run encoded
/// entry functions. Every VM owns its heap object store, output sink, and
/// the single-threaded fiber pool [E2] (slot 0 is the root a `run` entry
/// drives), and installs the builtin core native module
/// ([`crate::native::BUILTIN_MODULE`]) into the registry at construction;
/// the world surface is an installed platform handler's to serve
/// ([`crate::platform`]) — a bare VM carries no world surface.
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
    /// The nursery pool [E2]: one slot per open-or-consumed nursery, Dead
    /// slots reuse like fiber slots.
    pub(crate) nurseries: Vec<Nursery>,
    /// The live root nursery bare host spawns route into [E2]; `None` or a
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
    /// The derived deinit-hook directories (E5): one per (module version,
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
    /// The budgeted drive's remaining steps ([`Vm::run_with_budget`]):
    /// charged one per decoded instruction by the fueled instantiation
    /// of the dispatch loop alone, so every unbounded drive ignores it
    /// and its dispatch cost is unchanged.
    fuel: u64,
    /// The fork-join worker pool opt-in (E3's 2026-11-15 sendability
    /// ruling, crates/ds-vm/src/forkpool.rs): off by default — the
    /// sequential interlock drive is M5's backend — and when on, a
    /// `par_map` frame whose children hold exclusively scalar entry
    /// words drives them on youpipe workers through whole-fiber
    /// ownership transfer, everything else keeping the sequential drive.
    fork_pool: bool,
    /// How many fork frames this VM drove on the worker pool: the
    /// non-vacuity witness for the differential tests (a pool-enabled
    /// run that answers every fork sequentially through the gate's
    /// fallback reads zero).
    fork_pool_drives: usize,
    /// The installed reactor (the 2026-11-21 scheduler ruling): `None`
    /// keeps the blocking natives — blocking semantics stay an
    /// embedder's platform choice, never a language fact. The box rides
    /// a Debug-transparent wrapper so `Vm`'s own derive stands.
    reactor: Option<InstalledReactor>,
    /// The IO scheduler's tables: the wait register, the deadline
    /// wheel, the FIFO run queue, and the wake deliveries.
    sched: SchedState,
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
            kstr_cache: KstrRows::default(),
            hook_dirs: HookDirIndex::default(),
            hook_dirs_stamp: u64::MAX,
            teardown_drive: false,
            calls_staging: CaptureStaging::new(),
            repl_pre: None,
            repl_park: None,
            suspend_request: None,
            suspend_skip: None,
            fuel: 0,
            fork_pool: false,
            fork_pool_drives: 0,
            reactor: None,
            sched: SchedState::default(),
        }
    }

    /// How many fork frames this VM drove on the worker pool: the
    /// non-vacuity witness for the differential tests.
    #[must_use]
    pub fn pooled_fork_drives(&self) -> usize {
        self.fork_pool_drives
    }

    /// Opts this VM's fork-join combinator drives into the worker pool
    /// (E3's 2026-11-15 sendability ruling): a `par_map` frame whose
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
    /// the parking discipline (the 2026-11-21 scheduler ruling, the E1
    /// capability seam's runtime half): `net_recv`/`net_send` answer
    /// park requests on a would-block, the drive runs the next ready
    /// fiber, and readiness or the configured deadline completes the
    /// parked op — single-thread executor, the multi-thread fiber
    /// executor ruled post-1.0. A host that installs nothing keeps the
    /// blocking natives: the same source, the embedder's choice.
    pub fn install_reactor(&mut self, reactor: Box<dyn Reactor>) {
        self.out.enable_net_parking();
        self.reactor = Some(InstalledReactor(reactor));
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

    /// Stages a closure value for the host (the F2 capturing-lambda
    /// object): one heap object behind one `Ref` word, the header FuncRef
    /// word plus the moved-in capture block, born on the root heap like
    /// every host-staged word. The host-held reference owns the object's
    /// birth count; the module-code construction path (and with it the
    /// value's own RC traffic) waits on lambda-value lowering.
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
    pub fn set_net_timeout(&mut self, timeout: std::time::Duration) {
        self.out.set_net_timeout(timeout);
    }

    /// Pins the TLS client of the net face's tls tier: custom trust
    /// roots for an embedding host or a test, the default being the
    /// webpki root set. The tier itself stays the platform's `tls`
    /// capability — a host omitting the capability never serves the
    /// https bodies, whatever roots are pinned.
    pub fn set_tls_config(&mut self, config: Arc<rustls::ClientConfig>) {
        self.out.set_tls_config(config);
    }

    /// Installs a module version into its two-slot table (hot reload swap):
    /// the swap re-checks run first (a reordering table re-keys the staged
    /// words whose identity survived while refusing a dropped one, and a
    /// live code row's direct-call site naming a moved or dropped index
    /// refuses the swap), then the swap moves the retiring slot's old row
    /// out of the registry's own reachability, and the install-time sweep
    /// retires every row no live frame or parked fiber names — the retired
    /// pools' KSTR rows reclaimed at the same moment. Answers the installed
    /// version's table row, the version's single home.
    ///
    /// # Errors
    ///
    /// [`crate::LoadError::StagedFnMisroute`] or
    /// [`crate::LoadError::PoolRefMisroute`] when a re-check refuses (the
    /// installed version keeps running).
    pub fn install_module(&mut self, version: ModuleVersion) -> Result<VersionIdx, LoadError> {
        let name = version.name().to_owned();
        let swap = self.validate_staged_swap(&name, &version)?;
        let mut dead_pools = self.retiring_pool_addrs(&name);
        let row = self.registry.install(version);
        self.finish_swap(&swap);
        self.sweep_call_cache();
        dead_pools.extend(self.sweep_versions());
        self.reclaim_retired_kstr_rows(&dead_pools);
        Ok(row)
    }

    /// Installs a dsmod module into its two-slot table: the swap re-checks
    /// run first (below), the loader maps the payload, fills the
    /// entry-descriptor placeholders, and the slots swap [BL-7, H6 item 5].
    /// The module is consumed, so every body's code words reach the
    /// installed table without a copy. Answers the installed version's
    /// table row, the version's single home.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LoadError`] for unqualified or unresolved callee
    /// strings, for bodies failing the load checks, and for the swap
    /// re-checks' refusals (the installed version keeps running).
    pub fn install_dsmod(&mut self, module: ds_mod::Module) -> Result<VersionIdx, LoadError> {
        let name = module.name().to_owned();
        // build first, so the re-checks read the incoming table in its
        // installed spelling (a failed build touches no slot, exactly like
        // a refused swap)
        let version = crate::loader::build(&self.registry, module)?;
        let swap = self.validate_staged_swap(&name, &version)?;
        let mut dead_pools = self.retiring_pool_addrs(&name);
        let row = self.registry.install(version);
        self.finish_swap(&swap);
        self.sweep_call_cache();
        dead_pools.extend(self.sweep_versions());
        self.reclaim_retired_kstr_rows(&dead_pools);
        Ok(row)
    }

    /// Loads a dsmod module with its whole dependency closure: the
    /// deps-first walk installs every uop dependency the root's dep list
    /// names (fetching each uninstalled one through `provider`) before
    /// the root itself, so first-install ids assign in the deterministic
    /// dep-list depth-first order, and every link's content hash is
    /// validated — a mismatched link, a dep cycle, or a provider miss
    /// refuses the load. Resolution keeps reading each name's current
    /// slot, so a later swap of a loaded dep redirects the dependent's
    /// calls [H6 item 5]. The swap re-checks cover the root's swap (the
    /// walk's own door: already-installed deps validate and skip, so only
    /// the root can move a slot). Answers the root's table row, the
    /// version's single home.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LoadError`] for every refusal the walk names and
    /// for the swap re-checks' refusals; dependencies that finished
    /// installing stay installed.
    pub fn load(
        &mut self,
        root: ds_mod::Module,
        provider: impl FnMut(&str, ds_mod::Hash128) -> Option<ds_mod::Module>,
    ) -> Result<VersionIdx, LoadError> {
        let name = root.name().to_owned();
        // the walk installs the dependencies and answers the root built;
        // only the root can swap a slot (already-installed deps validate
        // and skip), so the re-checks read the built table before the
        // install lands and the pre-walk retiring pools are its own
        let version = crate::load::load_built(&mut self.registry, root, provider)?;
        let swap = self.validate_staged_swap(&name, &version)?;
        let mut dead_pools = self.retiring_pool_addrs(&name);
        let row = self.registry.install(version);
        self.finish_swap(&swap);
        self.sweep_call_cache();
        dead_pools.extend(self.sweep_versions());
        self.reclaim_retired_kstr_rows(&dead_pools);
        Ok(row)
    }

    /// The content identity the name's current installed version records
    /// (`None`: no current version under the name): the host-side seam a
    /// dev session reads to decide whether a reload round moves a dep
    /// before the walk validates its link — the identity stays pinned at
    /// link time for the walk itself.
    #[must_use]
    pub fn installed_content_hash(&self, name: &str) -> Option<ds_mod::Hash128> {
        self.registry
            .current_version(name)
            .and_then(ModuleVersion::content_hash)
    }

    /// The slot swap's staged-word re-check [H6 item 5's re-check landing]:
    /// a scalar callable word keys on the function-table index and
    /// resolution reads the name's current slot, so a swap whose incoming
    /// table moves or drops the indexed function would silently misroute
    /// every staged word into whatever function now holds the index. The
    /// identity judge is what the table already records — the function's
    /// name plus its frame layout, extended by the content identity a
    /// moved index-embedded name judges by (body plus capture set hashing
    /// equal against exactly one incoming candidate, never a guess).
    /// Phase one (here, read-only, before any install): build the
    /// retiring table's re-key map against the incoming rows and refuse on
    /// the first staged word whose identity did not survive, then the
    /// direct-call half ([`Vm::validate_pool_call_refs`]) refuses on the
    /// first live code row's call site whose named identity the incoming
    /// table moves or drops. Phase two ([`Vm::finish_swap`], after the
    /// install landed): re-key every staged word through the validated
    /// map, transparent where the identity kept, moved, or content-
    /// survived its index. A first install under a fresh name needs no
    /// check — no staged word can name an id the registry has not
    /// assigned.
    ///
    /// # Errors
    ///
    /// [`LoadError::StagedFnMisroute`] naming the staged value and the
    /// dropped function, or [`LoadError::PoolRefMisroute`] naming the
    /// refused call site; the registry is untouched.
    fn validate_staged_swap(
        &mut self,
        name: &str,
        incoming: &ModuleVersion,
    ) -> Result<StagedSwap, LoadError> {
        let Some(old) = self.registry.current_version(name) else {
            return Ok(StagedSwap::default());
        };
        let old_names: Vec<String> = old.functions().iter().map(|f| f.name.clone()).collect();
        let map: Vec<Option<u32>> = (0..u32::try_from(old.functions().len())
            .expect("function tables fit u32"))
            .map(|at| old.surviving_index(at, incoming))
            .collect();
        let module_id = self
            .registry
            .module_id(name)
            .expect("a module with a current version has its id");
        let module = name.to_owned();
        self.walk_staged_fns(&mut |site, func| {
            if func.module() != module_id {
                return Ok(());
            }
            let at = usize::try_from(func.func()).ok();
            let survived = at.and_then(|at| map.get(at).copied().flatten()).is_some();
            if survived {
                return Ok(());
            }
            Err(LoadError::StagedFnMisroute {
                module: module.clone(),
                index: func.func(),
                func: at.and_then(|at| old_names.get(at).cloned()),
                site,
            })
        })?;
        self.validate_pool_call_refs(name, incoming)?;
        Ok(StagedSwap { module_id, map })
    }

    /// The re-check's direct-call half: a live code row's CALL/CALLT/
    /// CALLG/CALLTG site names the swapped module through its constant
    /// pool by function-table index, the site's code is immutable, and
    /// resolution follows the module's current slot — so a swap that
    /// re-keys the index onto a different function NAME would silently
    /// route every live frame's next direct call into whatever function
    /// now holds the index. The staged-word half covers the callable
    /// VALUES (re-key or refuse at the swap); this half covers the
    /// executable CODE. The site's identity is the name alone: a kept
    /// name with a changed frame layout is the ordinary body edit (the
    /// inline cache re-derives the site's layout facts at its stamp
    /// miss). A moved name refuses — with one carve-out: the swapped
    /// module's OWN retiring rows carry sites whose index reads that
    /// row's index space, and the dispatch translates those through the
    /// identity the row records, so a moved index-embedded name (a
    /// synthesized lambda, renamed by the reorder itself) passes exactly
    /// when its content identity survives the move — body plus capture
    /// set hashing equal against exactly one incoming candidate. A
    /// dependent module's sites keep the strict name judge: their rows
    /// record no linked version of the swapped module, so the
    /// cross-module redirect stays the verbatim current-slot rule. The
    /// walk reads every version-table row a live frame or parked fiber
    /// pins (the registry's own slots excluded — a row no runnable
    /// frame names never executes again, so its pool refs take the
    /// ordinary redirect the inline-cache tests pin), so a dependent
    /// module's direct calls into the swapped module are covered exactly
    /// like the swapped module's own.
    ///
    /// # Errors
    ///
    /// [`LoadError::PoolRefMisroute`] naming the pool that carries the
    /// site, the index, and the identity the installed table records;
    /// the registry is untouched.
    fn validate_pool_call_refs(
        &self,
        name: &str,
        incoming: &ModuleVersion,
    ) -> Result<(), LoadError> {
        let current = self
            .registry
            .current_version(name)
            .expect("the staged phase proved a current version exists");
        let module_id = self
            .registry
            .module_id(name)
            .expect("a module with a current version has its id");
        let identity = |index: u32| -> Option<&str> {
            usize::try_from(index)
                .ok()
                .and_then(|at| current.functions().get(at))
                .map(|f| f.name.as_str())
        };
        // the rows live frames name — the sweep's marking walk minus the
        // registry's own slots: a row no frame or parked fiber names will
        // never execute again (a fresh run resolves through the swapped-in
        // current table), so its pool refs take the ordinary redirect, the
        // completed-frame semantics the inline-cache tests pin
        let mut live = vec![false; self.registry.version_rows()];
        for frame in &self.callinfo {
            live[frame.module.0 as usize] = true;
        }
        for fiber in &self.pool {
            // a dead fiber's kept frames never execute again (the recycle
            // door walks them for deinit claims alone, resolving hooks
            // through the row the sweep pins), so they take no part in
            // the valve: only a fiber that can run its code names a row
            if fiber.state == FiberState::Dead {
                continue;
            }
            for frame in &fiber.callinfo {
                live[frame.module.0 as usize] = true;
            }
            if let Some(resume) = &fiber.resume {
                live[resume.version.0 as usize] = true;
            }
        }
        for (row, &named) in live.iter().enumerate() {
            if !named {
                continue;
            }
            let Some(version) = self.registry.version(VersionIdx(
                u32::try_from(row).expect("version rows fit u32"),
            )) else {
                continue;
            };
            for function in version.functions() {
                for (pc, &word) in function.code.iter().enumerate() {
                    let pc = u32::try_from(pc).expect("pc fits u32");
                    let Ok(ds_abi::Inst::Uop(op)) = ds_abi::Inst::decode(pc, word) else {
                        continue;
                    };
                    let (ds_abi::uop::Uop::Call { callee, .. }
                    | ds_abi::uop::Uop::Callt { callee, .. }
                    | ds_abi::uop::Uop::Callg { callee, .. }
                    | ds_abi::uop::Uop::Calltg { callee, .. }) = op
                    else {
                        continue;
                    };
                    let Some(Const::FuncRef(entry)) = function.pool.get(usize::from(callee.0))
                    else {
                        continue;
                    };
                    let index = match entry {
                        ConstFuncRef::Uop { module, func } if *module == module_id => *func,
                        ConstFuncRef::Named { module, func } if module == name => *func,
                        _ => continue,
                    };
                    let Some(func_name) = identity(index) else {
                        return Err(LoadError::PoolRefMisroute {
                            module: name.to_owned(),
                            pool: version.name().to_owned(),
                            index,
                            func: None,
                        });
                    };
                    // a call site's identity is the NAME the index holds —
                    // the index re-keying a different name is the misroute.
                    // A kept name with a changed frame layout is the
                    // ordinary body edit: the inline cache re-derives the
                    // site's layout facts at its stamp miss, and an edited
                    // signature traps at the call exactly like a fresh
                    // run's would, so layout never gates the swap here
                    // (unlike a staged callable WORD, whose re-check judges
                    // name plus layout). The one moved name that passes is
                    // the swapped module's own retiring row's index-
                    // embedded callee whose content identity survived —
                    // the dispatch translates the site through the row's
                    // recorded identity, so the install keeps its
                    // byte-identical redirect (never a guess: zero or
                    // several content matches refuse)
                    let kept = usize::try_from(index)
                        .ok()
                        .and_then(|at| incoming.functions().get(at))
                        .is_some_and(|f| f.name == func_name);
                    let translated = !kept
                        && version.name() == name
                        && usize::try_from(index)
                            .ok()
                            .and_then(|at| current.functions().get(at))
                            .filter(|f| f.embeds_table_index())
                            .and_then(|f| incoming.content_unique(f.content_hash()))
                            .is_some();
                    if !kept && !translated {
                        return Err(LoadError::PoolRefMisroute {
                            module: name.to_owned(),
                            pool: version.name().to_owned(),
                            index,
                            func: Some(func_name.to_owned()),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// The re-check's phase two: rewrites every staged word keyed to the
    /// swapped module's id through the validated map. Runs only after the
    /// install landed, so a refused or failed install never leaves a
    /// re-keyed word behind (a re-keyed index is wrong against the table
    /// still installed).
    fn finish_swap(&mut self, swap: &StagedSwap) {
        let mut rekey =
            |_: StagedSite, func: &mut FuncRef| -> Result<(), std::convert::Infallible> {
                if let Some(to) = usize::try_from(func.func())
                    .ok()
                    .and_then(|at| swap.map.get(at).copied().flatten())
                {
                    *func = FuncRef::new(swap.module_id, to);
                }
                Ok(())
            };
        let () = self
            .walk_staged_fns(&mut rekey)
            .unwrap_or_else(|never| match never {});
    }

    /// Walks every staged callable word the VM owns, applying `visit`,
    /// which may rewrite the word in place; the first `Err` short-circuits.
    /// The sites: every fiber's heap objects (closure headers and captures,
    /// list elements, map keys and values) and every fiber's live segment
    /// words — the live drive context's store and segment walk through the
    /// context fields while its pool slot (scratch while the context is
    /// live) is skipped, every other fiber's through its slot. Dead
    /// fibers' stores and segments walk too: their objects survive
    /// foreign-held words and their frames fire deinit claims at slot
    /// reuse, the sweep's own reachability discipline.
    fn walk_staged_fns<E>(
        &mut self,
        visit: &mut impl FnMut(StagedSite, &mut FuncRef) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.ctx_live {
            self.objects.for_each_staged_fn(visit)?;
            let fiber = self.current.0;
            for at in 0..self.stack.len() {
                if let Word::Slot(Value::Fn(func)) = &mut self.stack[at] {
                    visit(StagedSite::FrameSlot { fiber }, func)?;
                }
            }
        }
        for (index, slot) in self.pool.iter_mut().enumerate() {
            if self.ctx_live && index == self.current.index() {
                continue;
            }
            slot.objects.for_each_staged_fn(visit)?;
            let fiber = u32::try_from(index).expect("fiber ids fit u32");
            for at in 0..slot.segment.len() {
                if let Word::Slot(Value::Fn(func)) = &mut slot.segment[at] {
                    visit(StagedSite::FrameSlot { fiber }, func)?;
                }
            }
        }
        Ok(())
    }

    // -- Bounded-memory reclamation over the two hint caches -------------
    //
    // Both caches sit beside an authoritative path (the full call-site
    // lookup, the full intern), so every reclamation below drops hints
    // only: a dropped row or entry costs its next use one full path and
    // nothing else. No count and no object moves at any door.

    /// The pool addresses of `module`'s retiring version's functions: the
    /// version the NEXT install under this name drops out of the registry
    /// (in-flight frames may hold it longer; its pools' rows are hints, so
    /// reclaiming them early only costs those frames a re-intern).
    fn retiring_pool_addrs(&self, module: &str) -> Vec<usize> {
        self.registry
            .retiring_version(module)
            .map(|version| {
                version
                    .functions()
                    .iter()
                    .map(|f| f.pool.as_ptr() as usize)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drops the KSTR intern rows keyed by pools a swap retired. The
    /// reclaim is unconditional even when kept frames still hold the
    /// retiring version — the version leaving the two-slot table never
    /// serves another host entry, but those frames keep loading through
    /// its pools, and each dropped row costs their next load one full
    /// intern (a reused address re-keys its row at the new pool's first
    /// load). Rows of the version moving into the retiring slot stay —
    /// its in-flight frames still load through them.
    fn reclaim_retired_kstr_rows(&mut self, pools: &[usize]) {
        self.kstr_cache.reclaim(|row| !pools.contains(&row.pool));
    }

    /// Drops the KSTR intern rows a dead pool slot leaves behind at its
    /// reuse door: the slot's store persists (a foreign-held object
    /// outlives its dead owner), so the rows stay valid hints for the
    /// next spawn — but the store's own intern map already holds the
    /// content, so a dropped row costs its next load one intern hit, and
    /// the VM-global table tracks live loads, not every fiber that ever
    /// loaded.
    fn reclaim_fiber_kstr_rows(&mut self, fiber: FiberId) {
        self.kstr_cache.reclaim(|row| row.fiber != fiber.0);
    }

    /// Empties the CALL inline-cache entries a swap unstamped: a stale
    /// entry can never hit again (the stamp check fails), so by the time
    /// the install's sweep counts reachability no cache entry is left to
    /// name a row. The full lookup stays the fallback and the refill, so
    /// dispatch correctness never depends on the cache [H3].
    fn sweep_call_cache(&mut self) {
        let stamp = self.registry.generation();
        for entry in &mut self.call_cache {
            if entry.as_ref().is_some_and(|e| e.stamp != stamp) {
                *entry = None;
            }
        }
    }

    /// The install-time reachability sweep over the version table [H6
    /// item 5]: a row stays pinned while the registry itself keeps it
    /// (module slots), while any fiber's live call chain names it (the
    /// context's while a drive runs, every pool slot's otherwise —
    /// interrupted fibers keep their frames), or while any parked or
    /// ready fiber's resume point names it. The run-entry hint and the
    /// CALL cache never reach a sweep as namers: the install takes the
    /// hint and the stamp check empties every cache entry before the
    /// sweep runs, and no dispatch happens between them. Every unmarked
    /// row is dropped — the table's one reference — and the sweep
    /// answers the retired versions' pool addresses so their KSTR rows
    /// die with them; the row is a version's single home, so a retired
    /// version's memory frees right there.
    fn sweep_versions(&mut self) -> Vec<usize> {
        let mut live = vec![false; self.registry.version_rows()];
        self.registry.mark_own_rows(&mut live);
        for frame in &self.callinfo {
            live[frame.module.0 as usize] = true;
        }
        for fiber in &self.pool {
            // every fiber's kept frames pin, a Dead one's included: the
            // recycle door walks a dead slot's leftover frames firing
            // whatever banded deinit claims survived the death, so their
            // rows must outlive the slot's reuse (the reuse itself clears
            // the chain and drops the pin)
            for frame in &fiber.callinfo {
                live[frame.module.0 as usize] = true;
            }
            if let Some(resume) = &fiber.resume {
                live[resume.version.0 as usize] = true;
            }
        }
        self.registry.retire_unmarked_rows(&live)
    }

    /// How many (pool, fiber) intern-row keys the KSTR cache holds — the
    /// bounded-memory reclamation witness for test assertions.
    #[must_use]
    pub fn kstr_cache_rows(&self) -> usize {
        self.kstr_cache.len()
    }

    /// The running fiber's arena snapshot - the bounded-memory reclamation
    /// witness for the loop-back-edge restore in test assertions.
    #[must_use]
    pub fn arena_stats(&self) -> ds_rt::ArenaStats {
        self.pool[self.current.index()].arena.stats()
    }

    /// The registry, for slot inspection.
    #[must_use]
    pub fn registry(&self) -> &ModuleRegistry {
        &self.registry
    }

    /// Installs a bound-artifact module under its manifest name (H5):
    /// the same replacement discipline a native table takes, so a host
    /// or a dev session installs ahead of the module installs whose
    /// pool validation resolves bound constants against the table.
    pub fn install_bound(&mut self, name: &str, module: crate::bound::BoundModule) {
        self.registry.install_bound(name, module);
    }

    /// Current value-stack word count: the current fiber's segment length.
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.pool[self.current.index()].segment.len()
    }

    /// High-water mark of the ROOT fiber's segment over the last run.
    /// Constant across unbounded tail recursion is the tail-call
    /// acceptance criterion [H2, per fiber].
    #[must_use]
    pub fn max_stack_len(&self) -> usize {
        self.pool[FIBER_ROOT.index()].max_len
    }

    // -- The fiber substrate [E2] --------------------------------------
    //
    // Pool ownership invariant: the running fiber's segment, call chain,
    // and heap store live in the VM's context fields while `ctx_live`
    // holds; every other time (and for every other fiber, always) they
    // live in the pool slot. A switch is three ownership swaps — no word
    // or object is ever copied.

    /// Spawns a fiber bare [E2]: with a live root nursery
    /// ([`Vm::open_root_nursery`]) the fiber is born its child; without one
    /// the host itself is the supervisor and the fiber detaches (no
    /// child-of link — the substrate regime the fiber fixtures pin). The
    /// in-language surface rejects bare spawn outside any nursery block;
    /// that check lands with the checker/lowerer rounds.
    ///
    /// # Errors
    ///
    /// The entry checks of [`Vm::run`] verbatim: unknown function, arity,
    /// argument types, entry frame versus the stack limit.
    pub fn spawn_fiber(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let nursery = self.live_root_nursery();
        self.spawn_into(nursery, module, func, args)
    }

    /// Spawns a fiber into an open nursery [E2]: the child-of link is set
    /// and the nursery records the child in spawn order.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery; the entry checks of [`Vm::run`]
    /// verbatim otherwise.
    pub fn spawn_fiber_in(
        &mut self,
        nursery: NurseryId,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        self.nursery_open(nursery)?;
        self.spawn_into(Some(nursery), module, func, args)
    }

    /// The spawn core: one pool entry with a fresh entry frame, born ready
    /// and never started, plus the child-of bookkeeping. The first switch
    /// into it (a `fiber_resume` call or [`Vm::resume_fiber`]) runs its
    /// entry function; the pool slot of a dead fiber is reused.
    fn spawn_into(
        &mut self,
        nursery: Option<NurseryId>,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let (version, func_id) = self.resolve_entry(module, func, args)?;
        Ok(self.push_spawned(nursery, version, func_id, args))
    }

    /// Spawns a fiber from a callable word's own function [E2]: the
    /// in-language `spawn` seam, and the fork-join combinator's per-element
    /// fork (the element's whole word span as the entry arguments). The
    /// FuncRef resolves through the target module's current slot; the
    /// callable's captures become the entry arguments, so the entry checks
    /// of [`Vm::run`] apply verbatim.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownModule`] when the FuncRef's module id names no
    /// installed module; the entry checks of [`Vm::run`] otherwise.
    pub(crate) fn spawn_ref_into(
        &mut self,
        nursery: Option<NurseryId>,
        fref: FuncRef,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let name = self
            .registry
            .module_name(fref.module())
            .ok_or(Trap::UnknownModule {
                module: fref.module(),
            })?;
        let (version, func_id) =
            self.registry
                .resolve_indexed(name, fref.func())
                .ok_or(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: fref.func(),
                })?;
        self.entry_checks(version, func_id, args)?;
        self.spawn_checked(nursery, version, func_id, args)
    }

    /// Spawns a fiber from a closure word's own body [E3]: the fork-join
    /// combinator's closure callable. The child is entered with `args`
    /// (the element's whole word span) against the closure's own body
    /// function, and the closure's capture block is staged into the
    /// entry frame's leading local slots past the parameters — exactly
    /// the CALLS copy-in discipline, so a forked lambda is one shape
    /// with a called one. Each `Ref` capture acquires one count at the
    /// copy-in, claimed for the child (the count lands on the word's
    /// own heap); the body's own exit drops balance it, and a child
    /// that never runs them releases at its drain door. The closure
    /// object keeps its own counts untouched — the callable word is the
    /// caller's borrow.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when `word` names no closure object (the
    /// callable-defaulted `Ref` spelling must hold a closure); the
    /// resolve and entry checks of [`Vm::spawn_ref_into`] verbatim;
    /// [`Trap::SlotOutOfRange`] when the capture block does not fit the
    /// frame's local slots past the parameters.
    pub(crate) fn spawn_closure_into(
        &mut self,
        nursery: Option<NurseryId>,
        word: RefId,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let (header, captures) = self.closure_parts(word)?;
        let name = self
            .registry
            .module_name(header.module())
            .ok_or(Trap::UnknownModule {
                module: header.module(),
            })?;
        let (version, func_id) =
            self.registry
                .resolve_indexed(name, header.func())
                .ok_or(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: header.func(),
                })?;
        self.entry_checks(version, func_id, args)?;
        // the capture block lands in the frame's local slots past the
        // parameters — the CALLS range check, before the child exists
        let layout = &self.registry.pinned(version).functions()[func_id as usize].layout;
        let capture_body_len = layout.body_len();
        let end = layout.params_len() + captures.len();
        if end > capture_body_len {
            return Err(Trap::SlotOutOfRange {
                slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                frame_slots: layout.slots().len(),
            });
        }
        let child = self.spawn_checked(nursery, version, func_id, args)?;
        let handoff_owner = self.current;
        for (index, &value) in captures.iter().enumerate() {
            if let Value::Ref(word) = value {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    self.ctx_live,
                    child,
                    word,
                )?;
            }
            let slot = u8::try_from(layout.params_len() + index)
                .expect("capture slot fits u8 after the range check");
            // the entry frame's fp is 0 and the capture slots sit below
            // the body boundary, so each write re-checks the word against
            // the slot's declared type exactly like the CALLS copy-in
            let fiber = &mut self.pool[child.index()];
            write_slot(
                &mut fiber.segment,
                0,
                slot,
                layout.slots(),
                capture_body_len,
                value,
            )?;
        }
        Ok(child)
    }

    /// The shared spawn tail: the entry frame pushed with `args`, each
    /// `Ref` argument acquiring one count for the child's frame at the
    /// copy-in — the child's own exit drops balance it, precisely a
    /// closure call's capture copy-in. The count lands on the word's own
    /// heap (which the spawner may not be running on) and the claim on
    /// the child's ledger.
    fn spawn_checked(
        &mut self,
        nursery: Option<NurseryId>,
        version: VersionIdx,
        func_id: u32,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let child = self.push_spawned(nursery, version, func_id, args);
        let handoff_owner = self.current;
        for &arg in args {
            if let Value::Ref(word) = arg {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    self.ctx_live,
                    child,
                    word,
                )?;
            }
        }
        Ok(child)
    }

    /// The pool-side half of every spawn: one pool entry with a fresh
    /// entry frame, born ready and never started, plus the child-of
    /// bookkeeping. The first switch into it runs its entry function; the
    /// pool slot of a dead fiber is reused.
    fn push_spawned(
        &mut self,
        nursery: Option<NurseryId>,
        version: VersionIdx,
        func_id: u32,
        args: &[Value],
    ) -> FiberId {
        let frame_size = self.registry.pinned(version).functions()[func_id as usize]
            .layout
            .frame_size() as usize;
        // born into the pool: reuse a dead slot above the reserved root,
        // or grow the pool by one
        let slot = match self
            .pool
            .iter()
            .skip(1)
            .position(|f| f.state == FiberState::Dead)
        {
            Some(at) => at + 1,
            None => self.pool.len(),
        };
        if slot == self.pool.len() {
            let id = FiberId(u32::try_from(slot).expect("pool size fits u32 ids"));
            self.pool.push(Fiber::empty(id));
            self.claims.push(ClaimTable::new(id));
        } else {
            // the reuse door of the death walk: every death path drains
            // its fiber, so this releases what an un-walked marking or an
            // exotic death left behind — and nothing on the honest paths.
            // The recycled fiber's kept frames (a resume-interrupted
            // detached child's) fire their deinits here, at their last
            // door [E5]
            let recycled = FiberId(u32::try_from(slot).expect("slot fits u32"));
            self.fire_teardown_hooks(recycled, &[]);
            self.drain_claims(recycled);
            self.reclaim_fiber_kstr_rows(recycled);
        }
        let fiber = &mut self.pool[slot];
        fiber.segment.truncate(0); // reuse keeps the chunks: spawn stays cheap
        Self::push_entry_frame(fiber, version, func_id, args, frame_size);
        fiber.resume = Some(ResumePoint {
            version,
            func: func_id,
            pc: CodePc(0),
        });
        fiber.state = FiberState::Ready;
        fiber.resumer = None;
        fiber.nursery = nursery;
        let id = FiberId(u32::try_from(slot).expect("pool size fits u32 ids"));
        if let Some(n) = nursery {
            self.nurseries[n.index()].push_child(id);
        }
        id
    }

    /// Requests that `fiber` park at its next taken backward branch under
    /// its next host drive ([`Vm::resume_fiber`]), the dev host's
    /// suspension-injection face: the drive ends handing the host the
    /// fiber's continuation as the suspension outcome, exactly the drive
    /// a parked-at-a-switch entry answers, and the fiber parks `Preempted`
    /// — no call site stands, so the later resume drops its payload. The
    /// request never bites inside a hosted reentry frame (a Rust-side
    /// native loop cannot suspend) or a teardown drive, and it waits
    /// past those instead. One request stands at a time; a later request
    /// overwrites it, and the drive consumes it when it parks.
    pub fn request_suspend(&mut self, fiber: FiberId) {
        self.suspend_request = Some(fiber);
    }

    /// Drives one fiber slice from the host: enters `fiber` (Ready or
    /// parked), delivering `payload` as the answer of the call it parked at
    /// (a Ready fiber drops the payload; a fiber parked by the host's
    /// suspension request drops it too — no call site stands), and runs
    /// until that fiber next parks with the host as resumer (a control
    /// park, or the requested suspension at a taken backward branch) or
    /// completes (its final value). A trapped drive leaves the fiber
    /// interrupted (not resumable) with its frames kept for
    /// [`Vm::backtrace`].
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with module `fiber` when `fiber` is absent
    /// or not parked at a switch point; the execution traps otherwise.
    pub fn resume_fiber(&mut self, fiber: FiberId, payload: Value) -> Result<FrameAnswer, Trap> {
        if !self.pool.get(fiber.index()).is_some_and(Fiber::resumable) {
            return Err(Trap::UnknownCallee {
                module: "fiber".into(),
                func: fiber.0,
            });
        }
        self.pool[fiber.index()].resumer = None; // the host resumes
        let (mut active, mut pc) = self.switch_in(fiber, payload);
        let outcome = loop {
            match self.execute_host_step::<false>(active, pc, usize::MAX) {
                // a world park under a host resume: the wait registers
                // and the executor's continuation answers it — the host
                // called a synchronous face, so the drive blocks on the
                // reactor like any blocking native would, the fiber
                // never blocking the world out
                Ok(DriveOutcome::WorldPark(req)) => {
                    if let ParkKind::World(wait) = req.kind {
                        self.register_wait(req.fiber, wait);
                    }
                    match self.executor_continue(fiber) {
                        Ok(ExecutorStep::Continue((a, p))) => {
                            active = a;
                            pc = p;
                        },
                        Ok(ExecutorStep::HostAnswer(answer)) => {
                            self.host_release();
                            for value in answer.as_slice() {
                                self.host_delivery(value);
                            }
                            break Ok(DriveOutcome::Died(answer));
                        },
                        Err(trap) => break Err((trap, pc)),
                    }
                },
                other => break other,
            }
        };
        self.host_release();
        match outcome {
            Ok(DriveOutcome::Died(answer)) => {
                // the drive-ended fiber's whole answer span crosses to the
                // host; the fiber itself keeps its remaining claims
                // (interrupted drives keep their frames, and their ledgers,
                // whole)
                for value in answer.as_slice() {
                    self.host_delivery(value);
                }
                Ok(answer)
            },
            Ok(DriveOutcome::Yielded(v)) => {
                // a park with the host as resumer answers one word (the
                // suspension's bare bounce tag); the fiber itself keeps its
                // remaining claims (interrupted drives keep their frames,
                // and their ledgers, whole)
                self.host_delivery(&v);
                Ok(FrameAnswer::one(v))
            },
            // a host-level drive passes no boundary, so no reentry frame
            // exists to return through
            Ok(DriveOutcome::Frame(_)) => unreachable!("a host drive answers no reentry frame"),
            Ok(DriveOutcome::Interlock(_)) => {
                unreachable!("the step drains every interlock before returning")
            },
            // the drive loop above drained every world park into the
            // executor's continuation before breaking its outcome out
            Ok(DriveOutcome::WorldPark(_)) => {
                unreachable!("the drive loop drained every world park")
            },
            Err((trap, at)) => {
                self.trap_pc = Some(at);
                // a nursery child dying trapped under a host drive: ledger
                // it and raise the cancel flag [E2 first failure]; the
                // fiber itself stays interrupted (frames kept). The blame
                // follows the fiber the drive ended on — the drive may have
                // switched away from `fiber` (it parked at its own resume
                // call) before the trap struck
                self.trap_child(self.current, &trap);
                // the trap's payload word crosses to the host
                self.trap_delivery(&trap);
                Err(trap)
            },
        }
    }

    /// Releases a fiber a trapped host drive left interrupted: the kept
    /// frames' deinit hooks fire exactly once (the teardown pass every
    /// drain door shares, each drive resolving through its owning frame's
    /// version row), the claims drain, the slot's intern rows die, and
    /// the frames clear — the pool slot returns to the spawn-reuse set
    /// and the executing version's row unpins, so the next install's
    /// sweep retires it (a dead drive pins nothing). The owner's open
    /// nurseries die with it: every never-consumed nursery the fiber (or
    /// an already-dead child under it) left open tears down, its live
    /// children releasing their pins under this same discipline. The
    /// caller reports first: the trap's backtrace reads the kept frames,
    /// so the release belongs after it. Answers whether this call
    /// released the fiber; a resumable one (a parked entry keeps its
    /// drive), a dead slot, or the live running context answers false
    /// untouched.
    pub fn release_interrupted(&mut self, fiber: FiberId) -> bool {
        // the interrupted shape: a drive ended in a Trap on this fiber,
        // so it keeps Running with no resume point and its context sits
        // parked in its slot — never the context a drive still holds
        let interrupted = self
            .pool
            .get(fiber.index())
            .is_some_and(|f| f.state == FiberState::Running && f.resume.is_none());
        if !interrupted || (self.ctx_live && self.current == fiber) {
            return false;
        }
        self.release_pooled_fiber(fiber);
        self.release_owned_nurseries(fiber);
        true
    }

    /// The per-fiber release pass the interrupted door and the nursery
    /// teardown share: the kept frames' deinit hooks fire (through their
    /// owning frames' version rows, a trapping hook keeping its effects
    /// and ending the pass), the claims drain, the slot's intern rows
    /// die, and the cleared chain plus the truncated segment drop every
    /// pin the frames held — version rows through the call chain and the
    /// resume point, staged callable words through the swap re-check —
    /// before the Dead marking returns the slot to the spawn-reuse set.
    fn release_pooled_fiber(&mut self, fiber: FiberId) {
        // a fiber parked on the world releases its reactor interest
        // first (the parked-IO cancellation discipline): the reactor
        // never reports into a released slot
        self.cancel_wait(fiber);
        self.fire_teardown_hooks(fiber, &[]);
        self.drain_claims(fiber);
        self.reclaim_fiber_kstr_rows(fiber);
        let slot = &mut self.pool[fiber.index()];
        slot.callinfo.clear();
        slot.segment.truncate(0);
        slot.resume = None;
        slot.state = FiberState::Dead;
    }

    /// Tears down every open nursery whose owner's death released it:
    /// `released` (this door's fiber — already Dead by its own pass) and
    /// every already-dead owner (a child a drive walked dead inside its
    /// own nursery block left it open with no drive left to consume it).
    /// Each torn nursery's live children release in spawn order under the
    /// per-fiber release pass — no child is entered or re-driven at this
    /// door: a never-entered child never ran (nothing of it stands but
    /// its staged words' counts and pins, released cold), a parked child
    /// keeps the effects that ran and fires its kept frames' hooks
    /// without the cut's sentinel (its resumer is dead or releases in the
    /// same pass), and a Dead child was already walked — skipped. A
    /// released child's own open nurseries fold into the same fixpoint,
    /// so an abandoned nest dies whole; nursery slots close (Dead,
    /// spawn-reusable) lowest-slot-first, deterministic.
    fn release_owned_nurseries(&mut self, released: FiberId) {
        while let Some(slot) = self.nurseries.iter().position(|n| {
            n.state == NurseryState::Open
                && n.owner.is_some_and(|owner| {
                    owner == released || self.pool[owner.index()].state == FiberState::Dead
                })
        }) {
            self.tear_down_nursery(slot);
        }
    }

    /// Closes one abandoned nursery: its live children release in spawn
    /// order under the per-fiber release pass — no child is entered (a
    /// reused slot belongs to another nursery: the child died and was
    /// ledgered before its slot recycled; a child a drive already walked
    /// is Dead, its hooks fired at its walk, never double-released here)
    /// — then the slot closes (Dead, spawn-reusable). The shared body of
    /// every release door.
    fn tear_down_nursery(&mut self, slot: usize) {
        let id = NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"));
        let children = self.nurseries[slot].children.clone();
        for child in children {
            if self.pool[child.index()].nursery != Some(id) {
                continue;
            }
            if self.pool[child.index()].state == FiberState::Dead {
                continue;
            }
            self.release_pooled_fiber(child);
        }
        let nursery = &mut self.nurseries[slot];
        nursery.driving = false;
        nursery.state = NurseryState::Dead;
        nursery.outcomes.clear();
    }

    /// The REPL rollback's line-scoped nursery door: the trapping
    /// line's root survives its own rollback, so no dead-owner door
    /// ever catches the line's open nursery. The pre-step nursery
    /// reading — the pool's states as the step entered, beside the
    /// ledger reading the rollback's claim gate already matches —
    /// floors the teardown: every root-owned open nursery sitting at
    /// a slot the reading held Dead (recycled into by the line) or
    /// past the reading's extent is the line's own and tears down
    /// with its never-entered children releasing cold, while the
    /// host-supervised root nursery and anything the reading held
    /// Open stay untouched. A child the line's join drive walked dead
    /// inside its own block left its nursery open with no drive left
    /// to consume it — the dead-owner pass below folds such a nest
    /// in, exactly the drive release's fixpoint.
    pub(crate) fn release_line_nurseries(&mut self, floor: &[NurseryState]) {
        loop {
            let slot = (0..self.nurseries.len()).find(|&slot| {
                self.nurseries[slot].state == NurseryState::Open
                    && self.nurseries[slot].owner == Some(FIBER_ROOT)
                    && !matches!(floor.get(slot), Some(NurseryState::Open))
            });
            let Some(slot) = slot else { break };
            self.tear_down_nursery(slot);
        }
        while let Some(slot) = self.nurseries.iter().position(|n| {
            n.state == NurseryState::Open
                && n.owner
                    .is_some_and(|owner| self.pool[owner.index()].state == FiberState::Dead)
        }) {
            self.tear_down_nursery(slot);
        }
    }

    /// A fiber's lifecycle state, for debug and test assertions.
    #[must_use]
    pub fn fiber_state(&self, fiber: FiberId) -> Option<FiberState> {
        self.pool.get(fiber.index()).map(|f| f.state)
    }

    /// High-water mark of a fiber's segment: the per-fiber TCO criterion —
    /// a tail-looping fiber's mark never rises past its frame size [H2].
    #[must_use]
    pub fn fiber_max_len(&self, fiber: FiberId) -> Option<usize> {
        self.pool.get(fiber.index()).map(|f| f.max_len)
    }

    /// How many chunks a fiber's segment owns — the chunk-append growth
    /// witness.
    #[must_use]
    pub fn fiber_chunk_count(&self, fiber: FiberId) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .map(|f| f.segment.chunk_count())
    }

    /// The address of word `index` in a fiber's segment — any allocated
    /// word, live or truncated-over: stable across later growth (chunks
    /// never move) and across switch round-trips (a switch copies no
    /// words) [H2, E2].
    #[must_use]
    pub fn fiber_word_addr(&self, fiber: FiberId, index: usize) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .filter(|f| index < f.segment.capacity())
            .map(|f| f.segment.word_addr(index))
    }

    /// The frame base of a fiber's innermost live frame.
    #[must_use]
    pub fn fiber_top_fp(&self, fiber: FiberId) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .and_then(|f| f.callinfo.last().map(|ci| ci.fp))
    }

    /// The nursery a fiber was spawned into (the child-of link [E2]);
    /// `None` = the root fiber or a detached spawn.
    #[must_use]
    pub fn fiber_nursery(&self, fiber: FiberId) -> Option<NurseryId> {
        self.pool.get(fiber.index()).and_then(|f| f.nursery)
    }

    // -- Nurseries [E2, E5] ---------------------------------------------
    //
    // Determinism: spawn order drives everything observable — the join and
    // teardown drivers enter children in spawn order, one to death at a
    // time, and outcomes report in spawn order. A host-visible yield under
    // a drive is answered with the bare PLATFORM_NONE bounce tag (the
    // run-entry convention); a child's own resumer links stay live.

    /// Opens (or returns the live) root nursery [E2]: the explicit root a
    /// bare host spawn routes into. With none live, the host itself is the
    /// supervisor and bare spawns detach.
    pub fn open_root_nursery(&mut self) -> NurseryId {
        if let Some(id) = self.live_root_nursery() {
            return id;
        }
        let id = self.open_fresh_nursery();
        // the host supervises the root: no fiber owns it, so no release
        // door ever tears it down
        self.nurseries[id.index()].owner = None;
        self.root_nursery = Some(id);
        id
    }

    /// Opens a fresh nursery [E2]: the in-language `nursery` block's own
    /// (never the root — bare host spawns route there alone), and the
    /// fork-join combinator's own over the same reuse discipline. Dead
    /// slots reuse like fiber slots. The opening fiber owns it — the link
    /// a trapped owner's release follows to the nursery its abandoned
    /// block left open.
    pub(crate) fn open_fresh_nursery(&mut self) -> NurseryId {
        let owner = Some(self.current);
        let slot = match self
            .nurseries
            .iter()
            .position(|n| n.state == NurseryState::Dead)
        {
            Some(at) => at,
            None => self.nurseries.len(),
        };
        if slot == self.nurseries.len() {
            self.nurseries.push(Nursery::open(owner));
        } else {
            self.nurseries[slot] = Nursery::open(owner);
        }
        NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"))
    }

    /// The root nursery while it is open.
    fn live_root_nursery(&self) -> Option<NurseryId> {
        self.root_nursery.filter(|&n| {
            self.nurseries
                .get(n.index())
                .is_some_and(|nr| nr.state == NurseryState::Open)
        })
    }

    /// Whether `nursery` names an open nursery.
    fn nursery_open(&self, nursery: NurseryId) -> Result<(), Trap> {
        if self
            .nurseries
            .get(nursery.index())
            .is_some_and(|n| n.state == NurseryState::Open)
        {
            Ok(())
        } else {
            Err(Trap::UnknownCallee {
                module: "nursery".into(),
                func: nursery.0,
            })
        }
    }

    /// Joins a nursery [E2]: drives every child to death in spawn order —
    /// a child dying trapped raises the cancel flag for the rest [E5] —
    /// then reports the boundary result and consumes the nursery.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery (absent, or already consumed).
    pub fn join_nursery(&mut self, nursery: NurseryId) -> Result<JoinOutcome, Trap> {
        self.nursery_open(nursery)?;
        let outcomes = self.face_drive(nursery, false);
        Ok(
            // only an all-Completed record joins; a synthesized Cancelled
            // with no Trapped sibling (an interrupted drive) is a boundary
            // failure the same way a trap is
            if outcomes
                .iter()
                .all(|o| matches!(o, ChildOutcome::Completed(_)))
            {
                JoinOutcome::Joined(
                    outcomes
                        .into_iter()
                        .map(|o| match o {
                            ChildOutcome::Completed(span) => span,
                            // the all-Completed test took every other shape
                            // to the Failed branch
                            ChildOutcome::Cancelled | ChildOutcome::Trapped(_) => {
                                unreachable!("a joined child completed")
                            },
                        })
                        .collect(),
                )
            } else {
                JoinOutcome::Failed(outcomes)
            },
        )
    }

    /// Tears a nursery down [E5]: raises the cancel flag, drives every
    /// live child to death (straight-line work lands, looping work stops
    /// at its next backward branch), then consumes the nursery and reports
    /// the per-child outcomes. The owner-death panic path will call this;
    /// the per-child death walk is where the future deinit pass hooks in.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery.
    pub fn teardown_nursery(&mut self, nursery: NurseryId) -> Result<Vec<ChildOutcome>, Trap> {
        self.nursery_open(nursery)?;
        Ok(self.face_drive(nursery, true))
    }

    /// A nursery's lifecycle state, for debug and test assertions.
    #[must_use]
    pub fn nursery_state(&self, nursery: NurseryId) -> Option<NurseryState> {
        self.nurseries.get(nursery.index()).map(|n| n.state)
    }

    /// Whether a nursery's cancel flag is raised, for debug and test
    /// assertions.
    #[must_use]
    pub fn nursery_cancelled(&self, nursery: NurseryId) -> Option<bool> {
        self.nurseries.get(nursery.index()).map(|n| n.cancelled)
    }

    /// The host face's drive (`join_nursery`/`teardown_nursery`): the
    /// nursery consumed with no in-language call site to answer. A child's
    /// own cross-nursery join pushes frames the same way, so the face
    /// drives through the identical interlock stack.
    fn face_drive(&mut self, nursery: NurseryId, cancel_first: bool) -> Vec<ChildOutcome> {
        if cancel_first {
            self.nurseries[nursery.index()].cancelled = true;
        }
        let frame = self.push_drive_frame(nursery, None, DriveSeam::Face);
        match self.nursery_drive_run(frame) {
            NurseryDriveEnd::Face(outcomes) => outcomes,
            _ => unreachable!("a face frame consumes without a delivery"),
        }
    }

    /// Tears a fork combinator's nursery down for a refused park or
    /// spawn ([E5] the cancel discipline): the face drive runs the
    /// children under the raised flag — a straight-line child's first
    /// entry still lands — and the calling fiber's context, which the
    /// first child switch stashed into its pool slot, restores (the
    /// combinator body continues on its own fiber with the refusal
    /// about to surface). A completed answer the caller abandons (a
    /// landed-under-the-flag word) keeps its claim row until the
    /// receiver's own drain door.
    pub(crate) fn teardown_fork_nursery(
        &mut self,
        nursery: NurseryId,
    ) -> Result<Vec<ChildOutcome>, Trap> {
        let host = self.current;
        let outcomes = self.teardown_nursery(nursery)?;
        for outcome in &outcomes {
            if let ChildOutcome::Completed(span) = outcome {
                for word in span.as_slice() {
                    if let Value::Ref(word) = word {
                        self.claims[host.index()].acquire(*word);
                    }
                }
            }
        }
        self.restore_context(host);
        Ok(outcomes)
    }

    /// Swaps `target`'s context back in after a face drive left no
    /// context live: the stash half of [`Vm::switch_in`] without the
    /// resume-point hand-shake (the fiber never parked — it hosts the
    /// native body that drove).
    fn restore_context(&mut self, target: FiberId) {
        self.host_release();
        let fiber = &mut self.pool[target.index()];
        std::mem::swap(&mut self.stack, &mut fiber.segment);
        std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
        std::mem::swap(&mut self.objects, &mut fiber.objects);
        self.current = target;
        self.ctx_live = true;
    }

    /// Snapshots a nursery into a drive frame and marks it in flight: the
    /// spawn-order children, the cursor of the child being driven, and who
    /// to answer at consumption. The mark refuses a second joiner while the
    /// drive stands.
    fn push_drive_frame(
        &mut self,
        nursery: NurseryId,
        joiner: Option<(FiberId, CodePc)>,
        seam: DriveSeam,
    ) -> NurseryDrive {
        debug_assert!(
            !self.nurseries[nursery.index()].driving,
            "one drive of a nursery stands at a time"
        );
        self.nurseries[nursery.index()].driving = true;
        NurseryDrive {
            children: self.nurseries[nursery.index()].children.clone(),
            nursery,
            cursor: 0,
            joiner,
            seam,
        }
    }

    /// Runs one drive segment of the join/teardown driver until it goes
    /// quiet (the fiber dies, parks at the host, or traps) or parks at a
    /// cross-nursery join — the request bubbles to the interlock stack. A
    /// trap escaping the drive is attributed to the fiber the drive ended
    /// on (a child that resumed another fiber parks at its resume call
    /// while the resumee traps, and that parked child is innocent); the
    /// caller's loop then re-validates the driven child: itself trapped →
    /// interrupted (already ledgered, the guard breaks); parked → bounced
    /// back in, or cut at that resume when the flag is up.
    fn drive_step(&mut self, active: Active, pc: CodePc) -> StepFlow {
        match self.execute::<false>(active, pc, usize::MAX) {
            Ok(DriveOutcome::Died(_) | DriveOutcome::Yielded(_)) => StepFlow::Quiet,
            Ok(DriveOutcome::Frame(_)) => unreachable!("a host drive answers no reentry frame"),
            Ok(DriveOutcome::Interlock(req)) => StepFlow::Interlock(req),
            // a child parked on a world op: the drive registers the
            // wait and moves on — the next child, or the reactor's
            // wait when every child stands parked (the pass loop in
            // `drive_frame_children`)
            Ok(DriveOutcome::WorldPark(req)) => {
                if let ParkKind::World(wait) = req.kind {
                    self.register_wait(req.fiber, wait);
                }
                StepFlow::Quiet
            },
            Err((trap, _at)) => {
                self.host_release();
                self.trap_child(self.current, &trap);
                StepFlow::Quiet
            },
        }
    }

    /// The worker-pool half of the fork drive [E3, the 2026-11-15
    /// sendability ruling, crates/ds-vm/src/forkpool.rs]: eligible frames
    /// are exactly a `par_map` seam (`par_reduce`'s fold chains one child
    /// per step — the left data dependency — and every other seam keeps
    /// the sequential drive) whose children all still belong to the
    /// nursery and all pass the scalar sendability gate — a `Ref` word in
    /// an entry frame names a heap a worker cannot route, so one such
    /// child sends the whole frame back to the sequential interlock,
    /// byte-identical by construction. The eligible children's fibers and
    /// ledgers leave the pool (the slots vacated) for a youpipe scoped
    /// drive over the code snapshot, and the homecoming runs the
    /// sequential drive's own tails in spawn order: the death walk for a
    /// fiber the worker left non-Dead (a trapped child keeps its frames),
    /// the nursery's ledger write, and the first-failure flag. The seam's
    /// consumption after the frame pops is untouched — the same ledger
    /// feeds it, so the assembly, deliveries, and count crossings are
    /// the sequential drive's own.
    fn try_fork_pool(&mut self, frame: &mut NurseryDrive) -> bool {
        if !matches!(
            frame.seam,
            DriveSeam::Fork(crate::native::ForkStage::Map { .. })
        ) || frame.children.len() < 2
            || !frame.children.iter().all(|&child| {
                self.pool[child.index()].nursery == Some(frame.nursery)
                    && crate::forkpool::poolable(&self.pool[child.index()])
            })
        {
            return false;
        }
        let nursery = frame.nursery;
        self.fork_pool_drives += 1;
        let mut cargos = Vec::with_capacity(frame.children.len());
        for &child in &frame.children {
            let fiber = std::mem::replace(&mut self.pool[child.index()], Fiber::empty(child));
            let claims = std::mem::replace(&mut self.claims[child.index()], ClaimTable::new(child));
            cargos.push(crate::forkpool::ForkCargo {
                fiber,
                claims,
                child,
            });
        }
        let code = self.registry.code_snapshot();
        let drive = crate::forkpool::next_drive_id();
        let limit = self.limit;
        let returns = youpipe::scope(|s| {
            s.pipe(cargos)
                .map(|cargo| crate::forkpool::exec_cargo(cargo, drive, &code, limit))
                .collect()
        });
        // the homecoming's first half, in spawn order (the scoped collect
        // is index-addressed): every fiber back in its own slot with its
        // ledger and its nursery link restored, before any count routes
        // through the pool
        let mut ledger = Vec::with_capacity(returns.len());
        for ret in returns {
            let child = ret.child;
            let mut fiber = ret.fiber;
            fiber.nursery = Some(nursery);
            self.pool[child.index()] = fiber;
            self.claims[child.index()] = ret.claims;
            ledger.push((child, ret.outcome));
        }
        // the sequential drive's own tails, in spawn order: the death
        // walk for a fiber the worker left non-Dead (a trapped child
        // keeps its frames — hooks first, then the claim drain around
        // the held payload, its claims crossing to the joiner), the
        // ledger write, and the first-failure flag
        let joiner = frame.joiner.map(|(fiber, _)| fiber);
        for (child, outcome) in ledger {
            if self.pool[child.index()].state != FiberState::Dead {
                let held = outcome
                    .as_ref()
                    .map(Self::payload_words)
                    .unwrap_or_default();
                self.child_death_walk(child, &held, joiner);
            }
            let outcome = outcome.unwrap_or(ChildOutcome::Cancelled);
            if matches!(outcome, ChildOutcome::Trapped(_)) {
                self.nurseries[nursery.index()].cancelled = true;
            }
            self.nurseries[nursery.index()].record(child, outcome);
        }
        frame.cursor = frame.children.len();
        true
    }

    /// The `Ref` words a trapped outcome's cancel payload carries — the
    /// words that ride the trap's delivery past a death walk, their counts
    /// held back from the drain and their claims routed to the joiner.
    fn payload_words(outcome: &ChildOutcome) -> Vec<RefId> {
        match outcome {
            ChildOutcome::Trapped(Trap::NurseryCancelled { payload }) => payload
                .iter()
                .filter_map(|value| match value {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The trapped-child death walk every drive's tail shares [E5]: the
    /// kept frames' deinit pass with the payload words held back, the
    /// claim drain around them, each held word's claim crossing to the
    /// joiner (the trap's delivery lands its counts there), and the Dead
    /// marking.
    fn child_death_walk(&mut self, child: FiberId, held: &[RefId], joiner: Option<FiberId>) {
        self.fire_teardown_hooks(child, held);
        self.drain_claims_except(child, held);
        for &word in held {
            self.claims[child.index()].release(word);
            if let Some(target) = joiner {
                self.claims[target.index()].acquire(word);
            }
        }
        let fiber = &mut self.pool[child.index()];
        fiber.state = FiberState::Dead;
        fiber.resume = None;
    }

    /// Drives one frame's remaining children in spawn order, each to death
    /// before the next (sequential — the deterministic choice). Every
    /// death path ledgers its outcome into the nursery whoever ran it, so
    /// this loop only drives and collects. A parked child of a cancelled
    /// (or discontinuing) nursery is cut at its next resume instead of
    /// being bounced back in [E2, E5]; a trapped child ledgers `Trapped`
    /// and raises the flag, and the walk that marks such an interrupted
    /// child dead below is the future deinit pass's hook point [E5:
    /// teardown walks frames]. A child parking at a cross-nursery join
    /// parks and returns the request — the frame pauses on that child
    /// until the pushed frame's answer re-enters it.
    fn drive_frame_children(&mut self, frame: &mut NurseryDrive) -> Option<InterlockRequest> {
        if self.fork_pool && self.try_fork_pool(frame) {
            return None;
        }
        // The pass loop: one sweep drives every child it can (in spawn
        // order, each to death or a park), and a sweep that ends with
        // live children left — all standing world-parked (the 2026-11-21
        // scheduler ruling: the drive ran the next ready fiber, and
        // nothing is ready) — waits on the reactor before the next
        // sweep. Once a sibling parks on a world op the relative order
        // of sibling observables is unspecified (ruled), so re-sweeping
        // in spawn order per wake is one fair interleaving of many.
        loop {
            while frame.cursor < frame.children.len() {
                let child = frame.children[frame.cursor];
                // a reused slot belongs to another nursery: the child died and
                // was ledgered before its slot was recycled
                if self.pool[child.index()].nursery != Some(frame.nursery) {
                    frame.cursor += 1;
                    continue;
                }
                // a dead child's sweep slot (its death walk already
                // ran on the sweep that drove it)
                if self.pool[child.index()].state == FiberState::Dead {
                    frame.cursor += 1;
                    continue;
                }
                while self.pool[child.index()].state != FiberState::Dead {
                    let state = self.pool[child.index()].state;
                    let cancelled = self.nurseries[frame.nursery.index()].cancelled;
                    // A woken child's entry delivers the completed op's
                    // staged answer at its parked site (the scheduler's
                    // wake), exactly the join answer delivery's shape.
                    //
                    // Otherwise the resume is preemption-shaped [E2, E5]:
                    // a child parked at a suspension it chose (a control
                    // call) or one the host's request injected sits at a
                    // park, so the bounce under the nursery's raised
                    // cancel flag delivers the cut instead of resuming —
                    // the driver-side twin of the backward-branch
                    // preemption point, and the only bound on a child that
                    // parks with no taken backward jump left between parks
                    // (a park inside a tail loop would otherwise bounce
                    // forever; the CALLT pc-reset is deliberately not a
                    // preemption point). A world-parked child — waiting or
                    // woken — under the flag is cut the same way, its
                    // reactor interest deregistered first (the ruling's
                    // parked-IO cancellation: the sentinel at the
                    // resume-shaped point, the reactor never reporting
                    // into a cut fiber). A Ready child is a first entry,
                    // not a resume: its straight-line work still lands
                    // [E5].
                    let step = if cancelled
                        && matches!(
                            state,
                            FiberState::Suspended | FiberState::Preempted | FiberState::WorldParked
                        ) {
                        // enter, then die at the resume point: cancel_current
                        // fires the kept-frame deinits (the cancel-cut door)
                        // and delivers the sentinel to the child's resumer,
                        // whose continuation the drive then runs
                        self.cancel_wait(child);
                        self.switch_in(child, Value::Tag(PLATFORM_NONE));
                        self.cancel_current()
                    } else if state == FiberState::WorldParked {
                        break; // waiting on the world, a later sweep owns it
                    } else if !self.pool[child.index()].resumable() {
                        break; // interrupted mid-trap (already ledgered)
                    } else if self.sched.deliveries.contains_key(&child) {
                        // the sweep enters the woken child itself: its
                        // run-queue entry goes with it (a stale entry
                        // would answer the executor's wait forever)
                        self.sched.take_ready(child);
                        match self.enter_woken(child) {
                            Ok(Woken::Step(step)) => Some(step),
                            // a completed-or-cut woken child: the ledger
                            // or the cut already wrote its outcome, the
                            // sweep's death check below finishes it
                            Ok(Woken::Completed(_) | Woken::Quiet) => None,
                            Err(trap) => {
                                self.host_release();
                                self.trap_child(child, &trap);
                                None
                            },
                        }
                    } else {
                        Some(self.switch_in(child, Value::Tag(PLATFORM_NONE)))
                    };
                    let Some((active, pc)) = step else {
                        continue;
                    };
                    if let StepFlow::Interlock(req) = self.drive_step(active, pc) {
                        return Some(req);
                    }
                }
                // the child's death walk completes here: a trapped child keeps
                // its frames until now, then its slot is released. The deinit
                // pass fires the kept frames' hooks first (E5: teardown walks
                // frames running deinits), holding the delivery's payload
                // back; then every other claim the kept frames own is
                // released, routed to each word's own heap (the one
                // cross-heap count channel of a death [E5]). A
                // world-parked child skips the walk — its frames still
                // stand for the wake.
                match self.pool[child.index()].state {
                    FiberState::Dead | FiberState::WorldParked => {},
                    _ => {
                        let held = self.nurseries[frame.nursery.index()]
                            .outcome(child)
                            .as_ref()
                            .map(Self::payload_words)
                            .unwrap_or_default();
                        self.child_death_walk(child, &held, frame.joiner.map(|(fiber, _)| fiber));
                    },
                }
                frame.cursor += 1;
            }
            // the sweep is done: waiting children keep the frame open.
            // Under the raised cancel flag the next sweep cuts them —
            // no reactor wait stands between a first failure and its
            // siblings' cut (the 2026-11-12 amendment's parked-IO
            // extension); otherwise the reactor owns the gap (a stall
            // guard failure raises the flag, the honest degenerate). A
            // woken sibling counts too: a channel send mid-pass wakes a
            // child the cursor already passed, so a staged delivery
            // keeps the frame open for the next sweep's entry instead
            // of consuming the nursery out from under a live child
            let waiting = frame.children.iter().any(|&child| {
                self.pool[child.index()].nursery == Some(frame.nursery)
                    && (self.pool[child.index()].state == FiberState::WorldParked
                        || self.sched.deliveries.contains_key(&child))
            });
            if !waiting {
                return None;
            }
            if !self.nurseries[frame.nursery.index()].cancelled && self.wait_world().is_err() {
                self.nurseries[frame.nursery.index()].cancelled = true;
            }
            frame.cursor = 0;
        }
    }

    /// Consumes a driven nursery — the in-flight mark down, the slot Dead,
    /// the ledger drained — and computes the delivery its seam owes the
    /// parked joiner. A child cut down before any ledgered death (an
    /// interrupted drive, an exotic switch tangle) reports as a boundary
    /// cancellation — a structured outcome, never a panic.
    /// The consumption prologue every seam shares: the drive's live
    /// context stashed, the in-flight mark down, the slot Dead, the
    /// ledger drained.
    fn consume_nursery(&mut self, nursery: NurseryId) -> Vec<ChildOutcome> {
        self.host_release();
        let slot = &mut self.nurseries[nursery.index()];
        slot.driving = false;
        slot.state = NurseryState::Dead;
        slot.outcomes
            .drain(..)
            .map(|o| o.unwrap_or(ChildOutcome::Cancelled))
            .collect()
    }

    fn consume_frame(&mut self, frame: &NurseryDrive) -> (Vec<ChildOutcome>, Option<Delivery>) {
        let outcomes = self.consume_nursery(frame.nursery);
        // A boundary seam about to deliver its collected answers keeps
        // every completed span for the channel: the join answers list
        // below adopts the words instead of the walk. Every other
        // no-answer case — the count seam, a failed boundary (first
        // failure, interrupted drive), or a joiner that moved on before
        // consumption — decs each discarded span's `Ref` words once
        // through the death walk's no-ledger door (the child's claim row
        // released at its death, the count left in flight for a receiver
        // that never comes), so the word's unique death still runs — its
        // free cascade, deinit claims included. The face seam's outcomes
        // cross to the host driver and the fork seam's stage adopts its
        // own words, so neither walks here.
        let boundary_collects = match (
            &frame.seam,
            outcomes
                .iter()
                .find(|o| matches!(o, ChildOutcome::Trapped(_) | ChildOutcome::Cancelled)),
        ) {
            // a clean join with a parked joiner answers the collected
            // list; a cancelled outcome with no trapped one cannot stand
            // (the flag only rises at a trapped death), but the scan
            // stays honest about it
            (DriveSeam::Boundary { .. }, None) => frame
                .joiner
                .is_some_and(|(joiner, resume_pc)| self.joiner_parked(joiner, resume_pc)),
            _ => false,
        };
        if matches!(frame.seam, DriveSeam::Count | DriveSeam::Boundary { .. }) && !boundary_collects
        {
            for outcome in &outcomes {
                if let ChildOutcome::Completed(span) = outcome {
                    for word in span.as_slice() {
                        if let Value::Ref(word) = word {
                            let _ = self.heaps_walk().dec_walk(*word);
                        }
                    }
                }
            }
        }
        // the first failure in spawn-drive order decides: a cancelled child
        // with no trapped one before it cannot happen (the flag only rises
        // at a trapped death)
        let delivery = match &frame.seam {
            // the fork seam ends through [`Vm::fork_frame_end`] in the
            // drive loop, which moves the stage out of the frame
            DriveSeam::Fork(_) => unreachable!("the fork seam ends through fork_frame_end"),
            DriveSeam::Face => None,
            DriveSeam::Count => {
                let failed = outcomes
                    .iter()
                    .filter(|o| matches!(o, ChildOutcome::Trapped(_)))
                    .count();
                let count = Value::I64(i64::try_from(failed).expect("child count fits i64"));
                Some(Delivery::Answer {
                    call: count,
                    span: None,
                    gone: count,
                })
            },
            DriveSeam::Boundary { mask } => {
                match outcomes
                    .iter()
                    .find(|o| matches!(o, ChildOutcome::Trapped(_)))
                {
                    None => {
                        if boundary_collects {
                            // The join channel [E2, the 2026-11-21
                            // scheduler ruling]: the children's answer
                            // spans collect into one list in spawn order
                            // — the joiner's own heap owns the list, the
                            // delivered words' claim rows acquire on the
                            // joiner (the fork stage's crossing), each
                            // push adopts the span as one element, and
                            // the in-flight count dies behind it. The
                            // whole-span element discipline is the fork
                            // seam's: the first push fixes the stride, a
                            // mismatching width traps before any count
                            // moves.
                            let (joiner_id, _) =
                                frame.joiner.expect("a collecting boundary parks a joiner");
                            // The joiner's store owns the collected list:
                            // swap it live for the allocation (the
                            // consumption prologue stashed every live
                            // context back into the pool), the fresh
                            // word's claim row acquiring on the joiner's
                            // ledger exactly where the fork stage's
                            // pre-park allocation did on its caller's.
                            let result = {
                                std::mem::swap(
                                    &mut self.objects,
                                    &mut self.pool[joiner_id.index()].objects,
                                );
                                let mut heaps = Heaps::new(
                                    joiner_id,
                                    &mut self.objects,
                                    &mut self.pool,
                                    &mut self.claims,
                                    true,
                                );
                                let word = heaps.new_list_id(Vec::new());
                                std::mem::swap(
                                    &mut self.objects,
                                    &mut self.pool[joiner_id.index()].objects,
                                );
                                word
                            };
                            for outcome in &outcomes {
                                let ChildOutcome::Completed(answer) = outcome else {
                                    continue;
                                };
                                for word in answer.as_slice() {
                                    if let Value::Ref(word) = word {
                                        self.claims[joiner_id.index()].acquire(*word);
                                    }
                                }
                                let mut heaps = self.fork_heaps(joiner_id);
                                if let Err(trap) = heaps.push_list(result, answer.as_slice()) {
                                    let _ = heaps.dec(result);
                                    return (Vec::new(), Some(Delivery::Raise(trap)));
                                }
                                for word in answer.as_slice() {
                                    if let Value::Ref(word) = word
                                        && let Err(trap) = heaps.dec(*word)
                                    {
                                        let _ = heaps.dec(result);
                                        return (Vec::new(), Some(Delivery::Raise(trap)));
                                    }
                                }
                            }
                            Some(Delivery::Answer {
                                call: Value::I64(0),
                                span: Some((NURSERY_BOUNDARY_OK, vec![Value::Ref(result)])),
                                gone: Value::Tag(NURSERY_BOUNDARY_OK),
                            })
                        } else {
                            Some(Delivery::Answer {
                                call: Value::I64(0),
                                span: Some((NURSERY_BOUNDARY_OK, vec![Value::I64(0)])),
                                gone: Value::Tag(NURSERY_BOUNDARY_OK),
                            })
                        }
                    },
                    Some(ChildOutcome::Trapped(Trap::NurseryCancelled { payload })) => {
                        // the staged mask is the caller's return-union
                        // member containment set: an error the boundary
                        // cannot union out refuses instead of corrupting
                        // the union's tag space. The member's identity is
                        // the span's word 0 — the tag the cancel site
                        // staged first
                        let contained = matches!(
                            payload.first(),
                            Some(Value::Tag(tag)) if *tag < 64 && (mask & (1i64 << *tag)) != 0
                        );
                        if contained {
                            // the gone surfacing is the member's tag word:
                            // a moved-on joiner's host drive sees the
                            // identity; the payload span's `Ref` words
                            // ride the trap delivery's host handoff
                            let gone = payload.first().copied().unwrap_or(Value::I64(0));
                            Some(Delivery::Answer {
                                call: Value::I64(0),
                                span: Some((NURSERY_BOUNDARY_ERR, payload.clone())),
                                gone,
                            })
                        } else {
                            Some(Delivery::Raise(Trap::NurseryUncontained {
                                payload: payload.clone(),
                            }))
                        }
                    },
                    Some(ChildOutcome::Trapped(trap)) => {
                        // a real trap surfaces at the boundary
                        Some(Delivery::Raise(trap.clone()))
                    },
                    Some(_) => unreachable!("the scan yields only trapped failures"),
                }
            },
        };
        (outcomes, delivery)
    }

    /// The interlock stack: drives the initial frame's children (and every
    /// frame a driven child pushes — its own cross-nursery join parks it
    /// and pushes the joined nursery, LIFO, the nesting order the
    /// recursive driver used to carry on the Rust stack), consumes each
    /// exhausted frame, and delivers its answer onto the parked joiner,
    /// whose continuation runs inside the same drain. The initial frame's
    /// delivery returns to its host instead.
    fn nursery_drive_run(&mut self, initial: NurseryDrive) -> NurseryDriveEnd {
        let mut stack = vec![initial];
        loop {
            while let Some(req) = self.drive_frame_children(
                stack
                    .last_mut()
                    .expect("the initial frame stands until its nursery consumes"),
            ) {
                stack.push(self.push_drive_frame(
                    req.nursery,
                    Some((req.joiner, req.resume_pc)),
                    req.seam,
                ));
            }
            let frame = stack
                .pop()
                .expect("the initial frame stands until its nursery consumes");
            let (outcomes, delivery) = match frame.seam {
                DriveSeam::Fork(stage) => {
                    let joiner = frame.joiner.expect("the fork seam parks a call site");
                    match self.fork_frame_end(frame.nursery, joiner, stage) {
                        ForkEnd::Chain(next) => {
                            stack.push(next);
                            continue;
                        },
                        ForkEnd::End(outcomes, delivery) => (outcomes, delivery),
                    }
                },
                _ => self.consume_frame(&frame),
            };
            let Some((joiner, resume_pc)) = frame.joiner else {
                return NurseryDriveEnd::Face(outcomes);
            };
            let Some(delivery) = delivery else {
                unreachable!("a join seam frames a delivery");
            };
            if stack.is_empty() {
                // the initial frame: the delivery belongs to the
                // interrupted host drive's fiber
                return match delivery {
                    Delivery::Answer { call, span, gone } => {
                        match self.reenter_joiner(joiner, resume_pc, call) {
                            Some((a, at)) => {
                                if let Some((tag, payload)) = span {
                                    self.overlay_boundary(&a, tag, &payload);
                                }
                                NurseryDriveEnd::Entered(a, at)
                            },
                            // no call site stands: the answer surfaces to
                            // the host
                            None => NurseryDriveEnd::Gone(gone),
                        }
                    },
                    Delivery::Words { head, tail, gone } => {
                        match self.reenter_joiner(joiner, resume_pc, head) {
                            Some((a, at)) => {
                                self.overlay_span_tail(&a, &tail);
                                NurseryDriveEnd::Entered(a, at)
                            },
                            None => NurseryDriveEnd::Gone(gone),
                        }
                    },
                    Delivery::Raise(trap) => {
                        // re-enter the joiner so the drive ends on it and
                        // the trap attributes at the boundary; the call
                        // site never reads the answer
                        let _ = self.reenter_joiner(joiner, resume_pc, Value::I64(0));
                        NurseryDriveEnd::Raised(trap, CodePc(resume_pc.0 - 1))
                    },
                };
            }
            // a nested frame: deliver onto the parked joiner and run its
            // continuation here (the recursive driver ran it inside the
            // same drive step); a further join pushes the next frame, a
            // gone joiner's answer surfaces to no one (its drive already
            // ended elsewhere)
            match delivery {
                Delivery::Answer { call, span, .. } => {
                    if let Some((a, at)) = self.reenter_joiner(joiner, resume_pc, call) {
                        if let Some((tag, payload)) = span {
                            self.overlay_boundary(&a, tag, &payload);
                        }
                        if let StepFlow::Interlock(req) = self.drive_step(a, at) {
                            stack.push(self.push_drive_frame(
                                req.nursery,
                                Some((req.joiner, req.resume_pc)),
                                req.seam,
                            ));
                        }
                    }
                },
                Delivery::Words { head, tail, .. } => {
                    if let Some((a, at)) = self.reenter_joiner(joiner, resume_pc, head) {
                        self.overlay_span_tail(&a, &tail);
                        if let StepFlow::Interlock(req) = self.drive_step(a, at) {
                            stack.push(self.push_drive_frame(
                                req.nursery,
                                Some((req.joiner, req.resume_pc)),
                                req.seam,
                            ));
                        }
                    }
                },
                Delivery::Raise(trap) => {
                    // re-enter the joiner so the drive ends on it, then
                    // the trap attributes there — the joiner's own nursery
                    // ledgers it and raises its flag
                    let _ = self.reenter_joiner(joiner, resume_pc, Value::I64(0));
                    self.host_release();
                    self.trap_child(self.current, &trap);
                },
            }
        }
    }

    /// The fork seam's router at consumption: the joiner's store and
    /// ledger reached through the pool — the drive's live context
    /// belongs to a dead child (the consumption prologue stashed it),
    /// while the parked joiner's real store sits in its own pool slot.
    fn fork_heaps(&mut self, joiner: FiberId) -> Heaps<'_> {
        Heaps::new(
            joiner,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            false,
        )
    }

    /// The fork seam's frame end: the consumed nursery's outcomes run
    /// through the assembly or fold stage exactly the synchronous
    /// body's post-join work did — the claims crossing first (the
    /// joiner's rows, one per `Ref` word of each completed answer: the
    /// release-side exact-ownership gate's row), the heap operations
    /// routed through the joiner's own store and ledger.
    /// `par_reduce`'s chain continues while elements remain — the next
    /// step's child spawns into its fresh nursery and the frame rides
    /// [`ForkEnd::Chain`] — the fold's left data dependency, one drive
    /// frame per step, nesting depth still heap. Any trap the stage
    /// raises (a stride refusal, a bogus word, a delivered width the
    /// checker never proved) releases the staged state and surfaces at
    /// the parked call site like a child failure.
    fn fork_frame_end(
        &mut self,
        nursery: NurseryId,
        joiner: (FiberId, CodePc),
        stage: crate::native::ForkStage,
    ) -> ForkEnd {
        let outcomes = self.consume_nursery(nursery);
        let (joiner_id, resume_pc) = joiner;
        match stage {
            crate::native::ForkStage::Map { result } => {
                for outcome in &outcomes {
                    if let ChildOutcome::Completed(answer) = outcome {
                        for word in answer.as_slice() {
                            if let Value::Ref(word) = word {
                                self.claims[joiner_id.index()].acquire(*word);
                            }
                        }
                    }
                }
                for outcome in outcomes {
                    match outcome {
                        ChildOutcome::Completed(answer) => {
                            let mut heaps = self.fork_heaps(joiner_id);
                            // the answer's whole span lands as one
                            // element: the first push fixes the result
                            // list's stride (a width mismatch traps at
                            // the stride check before any count moves,
                            // so the releases balance)
                            if let Err(trap) = heaps.push_list(result, answer.as_slice()) {
                                let _ = heaps.dec(result);
                                return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                            }
                            for word in answer.as_slice() {
                                if let Value::Ref(word) = word
                                    && let Err(trap) = heaps.dec(*word)
                                {
                                    let _ = heaps.dec(result);
                                    return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                                }
                            }
                        },
                        ChildOutcome::Trapped(trap) => {
                            let mut heaps = self.fork_heaps(joiner_id);
                            let _ = heaps.dec(result);
                            return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                        },
                        // the cancel flag only rises at a trapped death,
                        // so a cancelled outcome with no trapped one is
                        // an interrupted drive — the cut sentinel is its
                        // honest cause
                        ChildOutcome::Cancelled => {
                            let mut heaps = self.fork_heaps(joiner_id);
                            let _ = heaps.dec(result);
                            return ForkEnd::End(
                                Vec::new(),
                                Some(Delivery::Raise(Trap::NurseryCancelled {
                                    payload: vec![Value::Tag(PLATFORM_CANCELLED)],
                                })),
                            );
                        },
                    }
                }
                ForkEnd::End(
                    Vec::new(),
                    Some(Delivery::Answer {
                        call: Value::Ref(result),
                        span: None,
                        gone: Value::Ref(result),
                    }),
                )
            },
            crate::native::ForkStage::Reduce {
                elements,
                stride,
                callable,
                mut acc,
                mut owned,
                mut at,
            } => match outcomes.as_slice() {
                [ChildOutcome::Completed(answer)] => {
                    // the crossing's claim rows: the joiner owns the
                    // delivered words
                    for word in answer.as_slice() {
                        if let Value::Ref(word) = word {
                            self.claims[joiner_id.index()].acquire(*word);
                        }
                    }
                    let words = answer.as_slice();
                    if words.len() != acc.len() {
                        // the delivered span disagrees with the staged
                        // init's width — the callable the checker never
                        // proved: release the owned accumulator and the
                        // delivered words' own counts, then refuse
                        let mut heaps = self.fork_heaps(joiner_id);
                        if let Err(trap) =
                            crate::native::release_owned_span(&mut heaps, &acc, owned)
                        {
                            return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                        }
                        for word in words {
                            if let Value::Ref(id) = *word
                                && let Err(trap) = heaps.dec(id)
                            {
                                return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                            }
                        }
                        return ForkEnd::End(
                            Vec::new(),
                            Some(Delivery::Raise(Trap::ArityMismatch {
                                expected: acc.len(),
                                given: words.len(),
                            })),
                        );
                    }
                    let mut heaps = self.fork_heaps(joiner_id);
                    if let Err(trap) = crate::native::release_owned_span(&mut heaps, &acc, owned) {
                        return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                    }
                    acc.clear();
                    acc.extend_from_slice(words);
                    owned = true;
                    at += stride;
                    if at >= elements.len() {
                        // the chain's last answer: the whole span
                        // delivers onto the parked call site, the head
                        // through the switch payload, the tail past it
                        let head = acc[0];
                        let tail = acc[1..].to_vec();
                        return ForkEnd::End(
                            Vec::new(),
                            Some(Delivery::Words {
                                head,
                                tail,
                                gone: head,
                            }),
                        );
                    }
                    // the fold chain continues: the next element's child
                    // spawns with the running accumulator and rides the
                    // next frame — the left data dependency, one drive
                    // frame per step
                    let mut child_args = Vec::with_capacity(acc.len() + stride);
                    child_args.extend_from_slice(&acc);
                    child_args.extend_from_slice(&elements[at..at + stride]);
                    let nursery = self.open_fresh_nursery();
                    if let Err(trap) =
                        crate::native::fork_spawn(self, nursery, callable, &child_args)
                    {
                        // a refused spawn's child may be half-built: tear
                        // the step's nursery down under the cancel
                        // discipline, then release the owned accumulator
                        // before the trap surfaces
                        let _ = self.teardown_fork_nursery(nursery);
                        let mut heaps = self.fork_heaps(joiner_id);
                        if let Err(release) =
                            crate::native::release_owned_span(&mut heaps, &acc, true)
                        {
                            return ForkEnd::End(Vec::new(), Some(Delivery::Raise(release)));
                        }
                        return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                    }
                    ForkEnd::Chain(self.push_drive_frame(
                        nursery,
                        Some((joiner_id, resume_pc)),
                        DriveSeam::Fork(crate::native::ForkStage::Reduce {
                            elements,
                            stride,
                            callable,
                            acc,
                            owned,
                            at,
                        }),
                    ))
                },
                [ChildOutcome::Trapped(trap)] => {
                    // the chain holds no live sibling at the failure —
                    // the join consumed the step's nursery — so the
                    // trap re-raises with the owned accumulator released
                    let mut heaps = self.fork_heaps(joiner_id);
                    let _ = crate::native::release_owned_span(&mut heaps, &acc, owned);
                    ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap.clone())))
                },
                [ChildOutcome::Cancelled] => {
                    // the cancel flag only rises at a trapped death, so
                    // a cancelled outcome with no trap is an interrupted
                    // drive — the cut sentinel is its honest cause
                    let mut heaps = self.fork_heaps(joiner_id);
                    let _ = crate::native::release_owned_span(&mut heaps, &acc, owned);
                    ForkEnd::End(
                        Vec::new(),
                        Some(Delivery::Raise(Trap::NurseryCancelled {
                            payload: vec![Value::Tag(PLATFORM_CANCELLED)],
                        })),
                    )
                },
                _ => unreachable!("the chain joins exactly the one spawned child"),
            },
        }
    }

    /// Drives a parked joiner's request to its answer and continues the
    /// joiner's drive under `boundary` (the interrupted host drive's
    /// reentry boundary): the nursery's frame plus every frame a driven
    /// child pushes, then the answer delivered onto the parked call site
    /// and the joiner re-entered — program nesting depth costs heap, not
    /// Rust stack. A gone joiner's answer surfaces to the host drive as
    /// its death value; a boundary raise propagates at the join call's pc.
    fn drive_interlock<const FUELED: bool>(
        &mut self,
        req: InterlockRequest,
        boundary: usize,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        let frame = self.push_drive_frame(req.nursery, Some((req.joiner, req.resume_pc)), req.seam);
        let (mut active, mut pc) = match self.nursery_drive_run(frame) {
            NurseryDriveEnd::Face(_) => unreachable!("a join seam frames a delivery"),
            NurseryDriveEnd::Gone(value) => return Ok(DriveOutcome::Died(FrameAnswer::one(value))),
            NurseryDriveEnd::Raised(trap, at) => return Err((trap, at)),
            NurseryDriveEnd::Entered(a, at) => (a, at),
        };
        loop {
            match self.execute::<FUELED>(active, pc, boundary) {
                Ok(DriveOutcome::Interlock(req)) => {
                    let frame = self.push_drive_frame(
                        req.nursery,
                        Some((req.joiner, req.resume_pc)),
                        req.seam,
                    );
                    match self.nursery_drive_run(frame) {
                        NurseryDriveEnd::Face(_) => unreachable!("a join seam frames a delivery"),
                        NurseryDriveEnd::Gone(value) => {
                            return Ok(DriveOutcome::Died(FrameAnswer::one(value)))
                        },
                        NurseryDriveEnd::Raised(trap, at) => return Err((trap, at)),
                        NurseryDriveEnd::Entered(a, at) => {
                            active = a;
                            pc = at;
                        },
                    }
                },
                outcome => return outcome,
            }
        }
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

    /// Ledgers a fiber's death into its nursery, if it has one.
    fn record_child_outcome(&mut self, fiber: FiberId, outcome: ChildOutcome) {
        if let Some(n) = self.pool[fiber.index()].nursery {
            self.nurseries[n.index()].record(fiber, outcome);
        }
    }

    /// A fiber dying trapped under any driver: ledger `Trapped` and raise
    /// its nursery's cancel flag [E2 first failure]. The fiber's own state
    /// is the caller's business (interrupted under interactive drives,
    /// marked dead by the nursery walk).
    pub(crate) fn trap_child(&mut self, fiber: FiberId, trap: &Trap) {
        if let Some(n) = self.pool[fiber.index()].nursery {
            let nursery = &mut self.nurseries[n.index()];
            nursery.record(fiber, ChildOutcome::Trapped(trap.clone()));
            nursery.cancelled = true;
        }
    }

    /// Whether the running fiber's nursery has its cancel flag raised
    /// [E5]: the preemption check. Only nurseries with live children are
    /// ever cancelled, so a stale id cannot fire here.
    fn cancel_requested(&self) -> bool {
        self.pool[self.current.index()]
            .nursery
            .is_some_and(|n| self.nurseries[n.index()].cancelled)
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

    /// The argument and frame checks every entry shares [E2]: arity,
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
            // callable-defaulted slot (F2): the host stages a closure word
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

    /// Parks the running fiber on a world op at a plain `CALL` site
    /// (the 2026-11-21 scheduler ruling): the resume point is the
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
    /// message — lands in the store its `Ref` word names. The context
    /// is released whenever the scheduler runs (`wait_world` releases
    /// it on entry), so the pool slots hold every fiber's true store.
    fn complete_in_store<T>(&mut self, fiber: FiberId, run: impl FnOnce(&mut Heaps<'_>) -> T) -> T {
        debug_assert!(
            !self.ctx_live || self.current == fiber,
            "the scheduler runs over released contexts"
        );
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

    /// Registers one world park with the scheduler: the token minted,
    /// the interest registered with the reactor (a registration refusal
    /// wakes the fiber at once with the refusal as its own error
    /// class), the deadline armed on the wheel. The fiber keeps its
    /// `WorldParked` state until a wake moves it to the run queue.
    pub(crate) fn register_wait(&mut self, fiber: FiberId, wait: WorldWait) {
        let op = wait.op_name();
        let target = wait.target();
        let deadline = wait.deadline();
        // the wait's IO interest resolves through the table its handle
        // names: the connection table for the data ops, the listener
        // table for the accept, the pending-connect row for a parked
        // connect, the exchange machine's own transport for a fetch
        let handle = match &wait {
            WorldWait::Accept { listener, .. } => self.out.listeners().raw_handle(*listener),
            WorldWait::Connect { conn, .. } => self.out.net().raw_connect_handle(*conn),
            WorldWait::Fetch { fetch, .. } => self.out.net().raw_fetch_handle(*fetch),
            WorldWait::TlsFetch { fetch, .. } => self.out.net().raw_tls_fetch_handle(*fetch),
            _ => wait.conn().and_then(|conn| self.out.net().raw_handle(conn)),
        };
        let token = self.sched.mint();
        if let (Some(handle), Some(direction)) = (handle, wait.direction()) {
            let registered = match self.reactor.as_mut() {
                Some(installed) => installed.driver().register(handle, direction, token),
                None => Err(crate::reactor::ReactorError(
                    "no reactor installed".to_owned(),
                )),
            };
            if let Err(err) = registered {
                let result = self.complete_in_store(fiber, |heaps| {
                    crate::net::net_err(heaps, crate::net::NetClass::Refused, op, &target, &err.0)
                });
                self.sched.waits.insert(token, WaitEntry {
                    fiber,
                    wait,
                    handle: None,
                });
                self.finish_wait(token, result);
                return;
            }
        }
        self.sched.arm(deadline, token);
        self.sched.waits.insert(token, WaitEntry {
            fiber,
            wait,
            handle,
        });
    }

    /// Drops every wait one fiber owns (a nursery's cancel cut, a
    /// release door): the reactor interest deregisters first — the
    /// ruling's parked-IO cancellation — then the table row and any
    /// staged delivery go; stale timer entries drop lazily on pop.
    fn cancel_wait(&mut self, fiber: FiberId) {
        self.sched.deliveries.remove(&fiber);
        let tokens: Vec<u64> = self
            .sched
            .waits
            .iter()
            .filter(|(_, entry)| entry.fiber == fiber)
            .map(|(token, _)| *token)
            .collect();
        for token in tokens {
            if let Some(entry) = self.sched.waits.remove(&token) {
                self.release_wait_resources(&entry.wait);
                if let Some(handle) = entry.handle
                    && let Some(installed) = self.reactor.as_mut()
                {
                    let _ = installed.driver().deregister(handle);
                }
            }
        }
    }

    /// Releases the OS resource one ended wait's stage owned: a parked
    /// connect's raw socket and a parked exchange's transport — every
    /// other shape parks on a row its handle's own table already owns.
    fn release_wait_resources(&mut self, wait: &WorldWait) {
        match wait {
            WorldWait::Connect { conn, .. } => self.out.net().cancel_connect(*conn),
            WorldWait::Fetch { fetch, .. } => self.out.net().cancel_fetch(*fetch),
            WorldWait::TlsFetch { fetch, .. } => self.out.net().cancel_tls_fetch(*fetch),
            _ => {},
        }
    }

    /// Resolves one wait with `result`: the table row and its reactor
    /// interest drop, the answer stages as the fiber's wake delivery,
    /// and the fiber — still `WorldParked` — moves to `Suspended` on
    /// the run queue in wake order.
    fn finish_wait(&mut self, token: u64, result: NativeResult) {
        let Some(entry) = self.sched.waits.remove(&token) else {
            return;
        };
        if let Some(handle) = entry.handle
            && let Some(installed) = self.reactor.as_mut()
        {
            let _ = installed.driver().deregister(handle);
        }
        self.release_wait_resources(&entry.wait);
        let fiber = entry.fiber;
        if self.pool[fiber.index()].state == FiberState::WorldParked {
            self.pool[fiber.index()].state = FiberState::Suspended;
            self.sched.deliveries.insert(fiber, result);
            self.sched.push_ready(fiber);
        }
    }

    /// Re-arms one staged machine's pending step (a fetch exchange,
    /// plain or tls): the bound resets to the step-grained deadline the
    /// blocking twin's socket timeouts keep, and the standing interest
    /// re-aims at the direction the machine named.
    fn rearm_step(
        &mut self,
        token: u64,
        handle: Option<crate::reactor::IoHandle>,
        watch: crate::reactor::Direction,
    ) {
        let deadline = Instant::now() + self.out.net().timeout();
        if let Some(live) = self.sched.waits.get_mut(&token)
            && let WorldWait::Fetch {
                deadline: armed, ..
            } = &mut live.wait
        {
            *armed = deadline;
        }
        if let Some(live) = self.sched.waits.get_mut(&token)
            && let WorldWait::TlsFetch {
                deadline: armed, ..
            } = &mut live.wait
        {
            *armed = deadline;
        }
        self.sched.arm(deadline, token);
        if let Some(installed) = self.reactor.as_mut()
            && let Some(handle) = handle
        {
            let _ = installed.driver().register(handle, watch, token);
        }
    }

    /// Attempts one wait's completion: the deadline answers the timeout
    /// span, an fd readiness runs the op's non-blocking completion
    /// against the parked fiber's own store, a spurious wake (wrong
    /// direction, or the readiness already consumed) leaves the wait
    /// standing, and a partial send keeps its remainder waiting. No
    /// trap path: every failure is the op's own error span.
    fn complete_wait(&mut self, token: u64, ready: Option<crate::reactor::TokenEvent>) {
        let Some(entry) = self.sched.waits.get(&token).cloned() else {
            return;
        };
        let fiber = entry.fiber;
        // the timer-only wait: the wheel's firing IS the op's completion
        // — the unit word delivered at the parked site, no readiness to
        // consume and no error span the op could answer
        if matches!(entry.wait, WorldWait::Sleep { .. }) {
            self.finish_wait(token, NativeResult::Word(Value::I64(0)));
            return;
        }
        let target = entry.wait.target();
        let op = entry.wait.op_name();
        if entry.wait.deadline() <= Instant::now() {
            let bound = self.out.net().timeout();
            let result = self.complete_in_store(fiber, |heaps| {
                crate::net::net_err(
                    heaps,
                    crate::net::NetClass::Timeout,
                    op,
                    &target,
                    &format!("timed out after {bound:?}"),
                )
            });
            self.finish_wait(token, result);
            return;
        }
        let result =
            match entry.wait {
                // the timer-only wait completed at its own arm above; the
                // arm keeps this match total
                WorldWait::Sleep { .. } => unreachable!("the sleep completed at its own arm"),
                WorldWait::Recv { conn: id, max, .. } => {
                    let Some(event) = ready.filter(|event| event.read) else {
                        return;
                    };
                    let _ = event;
                    match self.out.net().try_recv(id, max) {
                        crate::net::NetAttempt::Done(Ok(bytes)) => self
                            .complete_in_store(fiber, |heaps| {
                                crate::net::recv_answer(heaps, id, bytes)
                            }),
                        crate::net::NetAttempt::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        // spurious: the readiness was consumed elsewhere,
                        // the wait stands for the next poll
                        crate::net::NetAttempt::Park => return,
                    }
                },
                WorldWait::Send {
                    conn: id, mut data, ..
                } => {
                    if !ready.is_some_and(|event| event.write) {
                        return;
                    }
                    match self.out.net().try_send(id, &mut data) {
                        crate::net::NetAttempt::Done(Ok(())) => crate::net::ok(Value::I64(0)),
                        crate::net::NetAttempt::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        // partial drain: the remainder keeps the wait, its
                        // level-triggered readiness reporting again once
                        // the buffer takes more
                        crate::net::NetAttempt::Park => {
                            if let Some(live) = self.sched.waits.get_mut(&token)
                                && let WorldWait::Send { data: pending, .. } = &mut live.wait
                            {
                                *pending = data;
                            }
                            return;
                        },
                    }
                },
                WorldWait::Accept { listener, .. } => {
                    let Some(event) = ready.filter(|event| event.read) else {
                        return;
                    };
                    let _ = event;
                    match self.out.listeners().try_accept(listener) {
                        // the accepted stream registers like a connecting
                        // one, so the parked wake answers the connection
                        // handle the caller's net_handle pairs with
                        crate::net::NetAttempt::Done(Ok(stream)) => {
                            let conn = self.out.net().register(stream);
                            crate::net::ok(Value::I64(conn))
                        },
                        crate::net::NetAttempt::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        // spurious: the pending connection was consumed
                        // elsewhere, the wait stands for the next poll
                        crate::net::NetAttempt::Park => return,
                    }
                },
                // the raw socket's completion: the writable (or errored)
                // wake probes SO_ERROR — connected registering the stream
                // under the pending row's id, the errno it holds the
                // failure — still-in-progress a spurious wake the wait
                // stands over
                WorldWait::Connect { conn: id, .. } => {
                    let Some(event) = ready.filter(|event| event.write || event.read) else {
                        return;
                    };
                    let _ = event;
                    match self.out.net().connect_wake(id) {
                        crate::net::NetAttempt::Done(Ok(conn)) => crate::net::ok(Value::I64(conn)),
                        crate::net::NetAttempt::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        crate::net::NetAttempt::Park => return,
                    }
                },
                // the plain exchange's machine: the OS probes gate the
                // stages (the event advisory), a pending step re-aiming
                // the standing interest at its next direction and re-arming
                // the bound — the per-op bound the blocking twin's socket
                // timeouts keep, step-grained
                WorldWait::Fetch { fetch, .. } => {
                    let _ = ready;
                    match self.out.net().fetch_wake(fetch) {
                        crate::net::FetchWake::Done(Ok(body)) => self
                            .complete_in_store(fiber, |heaps| crate::net::ok(heaps.new_str(body))),
                        crate::net::FetchWake::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        crate::net::FetchWake::Pending { watch } => {
                            self.rearm_step(token, entry.handle, watch);
                            return;
                        },
                    }
                },
                // the TLS exchange's machine: the same staged discipline,
                // the session's own wants naming the direction each step
                // watches next (the wake runs the session against the
                // non-blocking socket until it would block, so the event's
                // own directions carry no extra truth the machine lacks)
                WorldWait::TlsFetch { fetch, .. } => {
                    let _ = ready;
                    match self.out.net().tls_fetch_wake(fetch) {
                        crate::net::FetchWake::Done(Ok(body)) => self
                            .complete_in_store(fiber, |heaps| crate::net::ok(heaps.new_str(body))),
                        crate::net::FetchWake::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        crate::net::FetchWake::Pending { watch } => {
                            self.rearm_step(token, entry.handle, watch);
                            return;
                        },
                    }
                },
            };
        self.finish_wait(token, result);
    }

    /// The single-thread executor's wait: fire every expired deadline,
    /// then — while nothing is runnable — poll the reactor with the
    /// distance to the nearest deadline as the timeout, completing
    /// whatever the events answer; waits with no registered IO
    /// interest (the sleep's timer-only shape) wait the nearest
    /// deadline out on the thread instead, the wheel their whole wake.
    /// The context releases first (the pool slots hold every fiber's
    /// true store while the scheduler runs). A reactor refusal fails
    /// every standing IO wait with its own refusal span, so the program
    /// surfaces the failure through its error channel; the stall guard answers
    /// [`Trap::SchedulerDeadlock`] when nothing is runnable and
    /// nothing can wake.
    fn wait_world(&mut self) -> Result<(), Trap> {
        let mut events = Vec::new();
        loop {
            self.host_release();
            let now = Instant::now();
            for token in self.sched.pop_expired(now) {
                if self.sched.waits.contains_key(&token) {
                    self.complete_wait(token, None);
                }
            }
            if self.sched.has_ready() {
                return Ok(());
            }
            if !self.sched.can_wake() {
                return Err(Trap::SchedulerDeadlock);
            }
            // no registered IO interest stands: the wheel alone wakes
            // the waits that are left (a sleep parks with none, and a
            // wait the reactor refused never gained one), so the
            // thread waits the nearest deadline out — a broken or
            // absent reactor cannot strand a timer
            if !self
                .sched
                .waits
                .values()
                .any(|entry| entry.handle.is_some())
            {
                match self.sched.next_deadline() {
                    Some(at) => {
                        std::thread::sleep(at.saturating_duration_since(Instant::now()));
                    },
                    None => return Err(Trap::SchedulerDeadlock),
                }
                continue;
            }
            let Some(mut installed) = self.reactor.take() else {
                return Err(Trap::SchedulerDeadlock);
            };
            let timeout = self
                .sched
                .next_deadline()
                .map(|at| at.saturating_duration_since(now));
            let polled = installed.driver().poll(timeout, &mut events);
            self.reactor = Some(installed);
            match polled {
                Ok(()) => {
                    let drained = std::mem::take(&mut events);
                    for event in drained {
                        self.complete_wait(event.token, Some(event));
                    }
                },
                Err(err) => {
                    let tokens: Vec<u64> = self.sched.waits.keys().copied().collect();
                    for token in tokens {
                        let Some(entry) = self.sched.waits.get(&token).cloned() else {
                            continue;
                        };
                        // a sleep never touched the reactor: the wheel
                        // still owes it, and the branch above answers a
                        // reactor-less wait without one
                        if matches!(entry.wait, WorldWait::Sleep { .. }) {
                            continue;
                        }
                        let detail = format!("reactor: {}", err.0);
                        let target = entry.wait.target();
                        let op = entry.wait.op_name();
                        let result = self.complete_in_store(entry.fiber, |heaps| {
                            crate::net::net_err(
                                heaps,
                                crate::net::NetClass::Refused,
                                op,
                                &target,
                                &detail,
                            )
                        });
                        self.finish_wait(token, result);
                    }
                },
            }
        }
    }

    /// Enters one woken fiber: the staged answer's head word rides the
    /// switch payload, the whole answer overlays the parked call site,
    /// and the fiber continues. The sentinel resume point (an entry
    /// frame's pending tail park) instead completes the fiber with the
    /// delivered answer — its nursery ledgered, its resumer (or the
    /// host) taking the delivery. `Quiet` answers a fiber that cannot
    /// be entered (already cut, or its delivery gone with the cut).
    fn enter_woken(&mut self, fiber: FiberId) -> Result<Woken, Trap> {
        let Some(result) = self.sched.deliveries.remove(&fiber) else {
            return Ok(Woken::Quiet);
        };
        if !self.pool[fiber.index()].resumable() {
            return Ok(Woken::Quiet);
        }
        let final_delivery = self.pool[fiber.index()]
            .resume
            .as_ref()
            .is_some_and(|resume| resume.pc.0 == u32::MAX);
        let head = native_head_word(&result);
        let (active, pc) = self.switch_in(fiber, head);
        if final_delivery {
            let answer = frame_answer_of(result);
            return Ok(match self.complete_current_span(&answer) {
                Some((a, at)) => Woken::Step((a, at)),
                None => Woken::Completed(answer),
            });
        }
        let f = &self.registry.pinned(active.version).functions()[active.func as usize];
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        overlay_native(result, &mut self.stack, &f.layout, fp)?;
        Ok(Woken::Step((active, pc)))
    }

    /// The single-thread executor's continuation after a drive went
    /// quiet on a world park (the 2026-11-21 ruling: the drive runs the
    /// next ready fiber, FIFO): enter the next woken fiber, and when
    /// nothing is ready, eagerly start the parked joiner's own Ready
    /// nursery children (a spawned-but-unentered child otherwise runs
    /// only under its join drive), and when nothing is runnable, wait on
    /// the reactor. `HostAnswer` surfaces `host_fiber`'s own completion
    /// (the root drive's program answer, a resumed fiber's answer); any
    /// other completed orphan crosses to the host here and the executor
    /// keeps going.
    pub(crate) fn executor_continue(&mut self, host_fiber: FiberId) -> Result<ExecutorStep, Trap> {
        loop {
            while let Some(fiber) = self.sched.pop_ready() {
                // the executor's own children drive contained: a child's
                // trap is its nursery's first failure, reported at the
                // boundary like a sweep's drive reports it
                if fiber != host_fiber && self.owned_parked_child(fiber) {
                    match self.enter_woken(fiber) {
                        Ok(Woken::Step(step)) => {
                            if let Some(answer) = self.drive_owned_child(step, host_fiber)? {
                                return Ok(ExecutorStep::HostAnswer(answer));
                            }
                        },
                        // the completed span rides the nursery ledger,
                        // and a quiet entry was cut elsewhere — the
                        // sweep's own woken-child discard
                        Ok(Woken::Completed(_) | Woken::Quiet) => {},
                        Err(trap) => {
                            self.host_release();
                            self.trap_child(fiber, &trap);
                        },
                    }
                    continue;
                }
                match self.enter_woken(fiber)? {
                    Woken::Step(step) => return Ok(ExecutorStep::Continue(step)),
                    Woken::Completed(answer) => {
                        if fiber == host_fiber {
                            return Ok(ExecutorStep::HostAnswer(answer));
                        }
                        // an orphan's completion: the answer crosses to
                        // the host, the executor keeps its fiber
                        self.host_release();
                        for value in answer.as_slice() {
                            self.host_delivery(value);
                        }
                    },
                    Woken::Quiet => {},
                }
            }
            match self.eager_start(host_fiber)? {
                EagerPass::Idle => {},
                // a child drove to a park or a death: rescan before the
                // wait — its wake may have queued a fiber, and later
                // Ready siblings still stand
                EagerPass::Drove => continue,
                EagerPass::Host(answer) => {
                    return Ok(ExecutorStep::HostAnswer(answer));
                },
            }
            self.wait_world()?;
        }
    }

    /// Whether the open nursery `nursery`'s owner is a live parked
    /// fiber: the in-language block's joiner itself waits (a world op, a
    /// channel op, an in-language resume, a host suspension), so its
    /// children are the executor's to start. A dead owner's children
    /// release cold at the release doors without ever running.
    fn owner_parked(&self, nursery: &Nursery) -> bool {
        nursery.owner.is_some_and(|owner| {
            self.pool
                .get(owner.index())
                .is_some_and(|slot| !matches!(slot.state, FiberState::Running | FiberState::Dead))
        })
    }

    /// Whether `fiber` is a nursery child the executor owns while its
    /// joiner parks: nobody resumed it in-language, and the open nursery
    /// it belongs to has a live parked owner. The host-supervised root
    /// nursery (no owner) keeps its children for the host's explicit
    /// join drive.
    fn owned_parked_child(&self, fiber: FiberId) -> bool {
        self.pool.get(fiber.index()).is_some_and(|slot| {
            slot.resumer.is_none()
                && slot.nursery.is_some_and(|id| {
                    self.nurseries.get(id.index()).is_some_and(|nursery| {
                        nursery.state == NurseryState::Open && self.owner_parked(nursery)
                    })
                })
        })
    }

    /// The next Ready child of an open parked-owner nursery, in nursery
    /// slot order then spawn order — the eager start's scan.
    fn eager_ready_child(&self) -> Option<FiberId> {
        for (slot, nursery) in self.nurseries.iter().enumerate() {
            if nursery.state != NurseryState::Open || !self.owner_parked(nursery) {
                continue;
            }
            let id = NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"));
            for &child in &nursery.children {
                if self.pool[child.index()].nursery == Some(id)
                    && self.pool[child.index()].state == FiberState::Ready
                {
                    return Some(child);
                }
            }
        }
        None
    }

    /// Starts one Ready child — the first entry runs the spawn arguments,
    /// the dropped payload the switch convention — and drives it under
    /// the contained pass; the pass answers whether anything started.
    fn eager_start(&mut self, host_fiber: FiberId) -> Result<EagerPass, Trap> {
        let Some(child) = self.eager_ready_child() else {
            return Ok(EagerPass::Idle);
        };
        let step = self.switch_in(child, Value::Tag(PLATFORM_NONE));
        match self.drive_owned_child(step, host_fiber)? {
            Some(answer) => Ok(EagerPass::Host(answer)),
            None => Ok(EagerPass::Drove),
        }
    }

    /// One owned child's drive, sweep-shaped: the child's world parks
    /// register their waits, its traps ledger into its nursery and raise
    /// the flag exactly like a sweep's drive (the boundary reports them,
    /// the host drive never sees one), a cross-nursery join drains
    /// through the interlock machinery, and a death discards the drive's
    /// answer — the nursery ledger holds the span the boundary collects.
    /// The host drive answers its own fiber alone, so a child's step
    /// never leaves this pass for the host loop; `Some` answers the host
    /// fiber's death value, `Err` its own trap.
    fn drive_owned_child(
        &mut self,
        step: (Active, CodePc),
        host_fiber: FiberId,
    ) -> Result<Option<FrameAnswer>, Trap> {
        let (active, pc) = step;
        match self.drive_step(active, pc) {
            StepFlow::Quiet => Ok(None),
            StepFlow::Interlock(req) => match self.drive_interlock::<false>(req, usize::MAX) {
                Ok(DriveOutcome::WorldPark(park)) => {
                    if let ParkKind::World(wait) = park.kind {
                        self.register_wait(park.fiber, wait);
                    }
                    Ok(None)
                },
                Ok(DriveOutcome::Died(answer)) => {
                    if self.current == host_fiber {
                        return Ok(Some(answer));
                    }
                    Ok(None)
                },
                Ok(DriveOutcome::Yielded(_)) => Ok(None),
                Ok(DriveOutcome::Interlock(_) | DriveOutcome::Frame(_)) => {
                    unreachable!("the interlock loop drains its own frames")
                },
                Err((trap, _at)) => {
                    if self.current == host_fiber {
                        return Err(trap);
                    }
                    self.host_release();
                    self.trap_child(self.current, &trap);
                    Ok(None)
                },
            },
        }
    }

    /// Parks the running fiber at a fork-join CALLG site: the
    /// combinator's spawn phase ran (the children stand in `nursery`),
    /// and the join runs on the interlock stack — the request's frame
    /// drives the children, runs the assembly/fold stage, and delivers
    /// the answer onto this call site, nesting depth costing the
    /// interlock's heap frames, never the Rust stack. The whole-span
    /// answer's width check runs here (the total overlay's write
    /// check): the parked overlay must hold the answer (the lowerer
    /// pre-widens; hand-assembled code refuses instead of losing the
    /// tail words at the delivery).
    fn fork_park(
        &mut self,
        active: &Active,
        pc: CodePc,
        area: usize,
        nursery: NurseryId,
        stage: crate::native::ForkStage,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        let resume_pc = CodePc(pc.0 + 1);
        let answer_words = stage.answer_words();
        if answer_words > area {
            // the staged children and state must not outlive the
            // refused park: tear the nursery down, release the staged
            // words, then refuse
            crate::native::fork_abandon(self, nursery, stage).map_err(|e| (e, pc))?;
            return Err((
                Trap::RetSpanTooWide {
                    span: u16::try_from(answer_words).expect("answer widths fit u16"),
                    caller_area: u16::try_from(area).expect("argument areas fit u16"),
                },
                pc,
            ));
        }
        self.park_current(active, resume_pc);
        Ok(DriveOutcome::Interlock(InterlockRequest {
            nursery,
            joiner: self.current,
            resume_pc,
            seam: DriveSeam::Fork(stage),
        }))
    }

    /// Enters `target` with `payload` [E2: switch is O(1)]: stash the
    /// outgoing fiber's context, swap the target's in (buffer ownership
    /// moves, no word is copied), and deliver `payload` onto the call the
    /// target parked at (a Ready fiber drops it — first entry runs the
    /// spawn arguments). The caller has validated the target. Entering
    /// the fiber the last drive ended on is the host's O(1) round-trip:
    /// its context was stashed at host release and swaps back in.
    pub(crate) fn switch_in(&mut self, target: FiberId, payload: Value) -> (Active, CodePc) {
        if target != self.current {
            self.host_release(); // stash the outgoing fiber's context if live
        }
        if target != self.current || !self.ctx_live {
            let fiber = &mut self.pool[target.index()];
            std::mem::swap(&mut self.stack, &mut fiber.segment);
            std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
            std::mem::swap(&mut self.objects, &mut fiber.objects);
            self.current = target;
            self.ctx_live = true;
        }
        let fiber = &mut self.pool[target.index()];
        let resume = fiber
            .resume
            .take()
            .expect("a validated switch target carries its continuation");
        let parked = fiber.state == FiberState::Suspended;
        let preempted = fiber.state == FiberState::Preempted;
        fiber.state = FiberState::Running;
        if parked {
            let f = &self.registry.pinned(resume.version).functions()[resume.func as usize];
            let fp = self.callinfo[self.callinfo.len() - 1].fp;
            overlay_word(payload, &mut self.stack, &f.layout, fp);
        }
        // a suspension-resumed fiber re-enters at its parked branch: that
        // re-take is the resume itself, so the request check skips it once
        self.suspend_skip = preempted.then_some((target, resume.pc));
        (
            Active {
                version: resume.version,
                func: resume.func,
            },
            resume.pc,
        )
    }

    /// Ends the running fiber [E2]: ledgers `outcome` into its nursery,
    /// marks it dead, and switches to its resumer delivering `value` to the
    /// call the resumer parked at. `None` = the host takes the delivery —
    /// it was the resumer, or the resumer died while this fiber was parked
    /// (the leftover pattern; the join/teardown drivers must survive it).
    /// Closes the frame at `fp` for the arena: a frame that allocated
    /// arena temporaries pops its entry mark (the frame-pop restore
    /// reclaims its bytes), and the flag clears for the frame a tail
    /// transfer reuses.
    fn arena_frame_close(&mut self, fp: usize) {
        if let Word::Meta(meta) = &mut self.stack[fp]
            && meta.arena_active()
        {
            self.pool[self.current.index()].arena.pop_frame_mark();
            meta.clear_arena_active();
        }
    }

    fn die_current(&mut self, value: Value, outcome: ChildOutcome) -> Option<(Active, CodePc)> {
        // the delivered words' claims cross with them — one release per
        // `Ref` word of the completed span (the channel is whole-span), a
        // single word for every other death. The receiver of the head word
        // is resolved below; each delivered count stays exact through the
        // joiner's own single dec. A completed fiber's exit decs have
        // released its other claims already, so no walk runs here: a
        // raised leftover is observable state (the rc_diag witness),
        // released only by the cancel-cut and interrupt doors
        match &outcome {
            ChildOutcome::Completed(span) => {
                for word in span.as_slice().iter().filter_map(|v| match v {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                }) {
                    self.claims[self.current.index()].release(word);
                }
            },
            _ => {
                if let Value::Ref(word) = value {
                    self.claims[self.current.index()].release(word);
                }
            },
        }
        self.record_child_outcome(self.current, outcome);
        // the dying fiber's frames never run again, so its store's intern
        // pins release with the death: every completed, cancelled, and
        // halted exit funnels here (a cancelled fiber's claims already
        // drained below, and the release is idempotent). Interrupted and
        // marking deaths release at their own drain doors.
        self.objects.release_pins();
        self.pool[self.current.index()].arena.release_all();
        let resumer = self.pool[self.current.index()].resumer;
        let fiber = &mut self.pool[self.current.index()];
        fiber.state = FiberState::Dead;
        fiber.resume = None;
        let live = resumer.filter(|&r| self.pool[r.index()].resumable());
        if let (Some(&target), Value::Ref(word)) = (live.as_ref(), value) {
            self.claims[target.index()].acquire(word);
        }
        live.map(|target| self.switch_in(target, value))
    }

    /// The running fiber completed at its entry frame with `value`.
    fn complete_current(&mut self, value: Value) -> Option<(Active, CodePc)> {
        self.die_current(value, ChildOutcome::Completed(FrameAnswer::one(value)))
    }

    /// The running fiber completed at its entry frame answering the
    /// whole span `answer` (a RETSPAN or a tail native's span/total):
    /// the span rides the nursery ledger whole, the head word alone
    /// reaching a live resumer (the resume channel stays one word). The
    /// caller keeps the span for the drive's death value — the host
    /// seam's whole answer.
    fn complete_current_span(&mut self, answer: &FrameAnswer) -> Option<(Active, CodePc)> {
        let head = answer.as_slice().first().copied().unwrap_or(Value::I64(0));
        // the record clones the inline span so the caller keeps the words
        // for the drive's death value; the clone runs once per entry-frame
        // completion, never per call, so the hot seams stay untouched
        self.die_current(head, ChildOutcome::Completed(answer.clone()))
    }

    /// The running fiber observed its nursery's cancel flag at a taken
    /// backward branch [E5]: it stops here — no instruction of the loop
    /// body it was re-entering runs — and its resumer receives the
    /// `PLATFORM_CANCELLED` sentinel.
    fn cancel_current(&mut self) -> Option<(Active, CodePc)> {
        // the cut fiber's exit decs never run: the deinit pass fires its
        // kept frames' hooks first (E5: teardown walks frames running
        // deinits), then the death walk releases the claims — the
        // cancel-cut door
        self.fire_teardown_hooks(self.current, &[]);
        self.drain_claims(self.current);
        self.die_current(Value::Tag(PLATFORM_CANCELLED), ChildOutcome::Cancelled)
    }

    /// The switch behind a control native's `CALL` site (the fiber pair,
    /// and the nursery join's park-drive-redeliver). The staged words are
    /// already width- and type-checked by the caller of this helper.
    fn control_switch(
        &mut self,
        control: ControlNative,
        active: &Active,
        pc: u32,
        fp: usize,
        caller: &FrameLayout,
        nargs: u8,
    ) -> Result<SwitchOutcome, Trap> {
        // Teardown never re-enters the nursery: under the teardown-drive
        // flag every nursery control seam refuses like the drive rule's own
        // refusals — a join or boundary would drive another nursery's
        // children to death inside the pass (their death-walk doors meeting
        // the pass's own walk from below), and a spawn would strand its
        // child outside every standing drive's child snapshot. The refusal
        // precedes every read and park, so the dying fiber's takeover is
        // never parked mid-pass; the own-nursery and standing-drive refusals
        // below keep their scope outside drives.
        if self.teardown_drive
            && matches!(
                control,
                ControlNative::NurseryJoin
                    | ControlNative::NurseryOpen
                    | ControlNative::NurserySpawn
                    | ControlNative::NurseryBoundary
                    | ControlNative::NurseryCancel
            )
        {
            return Err(Trap::ReentrancyRefused);
        }
        match control {
            ControlNative::Resume => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let payload = control_i64(&self.stack, fp, caller, 1)?;
                let target = u32::try_from(id).ok().map(FiberId).filter(|t| {
                    *t != self.current && self.pool.get(t.index()).is_some_and(Fiber::resumable)
                });
                let Some(target) = target else {
                    // no switch: the checked-read bounce answer [E2 debug surface]
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                self.park_current(active, CodePc(pc + 1));
                self.pool[target.index()].resumer = Some(self.current);
                let (a, at) = self.switch_in(target, Value::I64(payload));
                Ok(SwitchOutcome::Entered(a, at))
            },
            ControlNative::NurseryJoin => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    // absent or consumed: the checked-read bounce answer
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                if self.pool[self.current.index()].nursery == Some(nursery) {
                    // a child joining its own nursery would deadlock the
                    // single-threaded driver on itself [E2]
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                if self.nurseries[nursery.index()].driving {
                    // a drive of this nursery already stands: the second
                    // joiner would interlock the two drives on each other
                    // (its answer needs the standing drive's completion,
                    // the standing drive needs the joiner's death) — the
                    // cycle the own-nursery rule refuses for one level [E2]
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                self.park_current(active, CodePc(pc + 1));
                Ok(SwitchOutcome::Interlock(InterlockRequest {
                    nursery,
                    joiner: self.current,
                    resume_pc: CodePc(pc + 1),
                    seam: DriveSeam::Count,
                }))
            },
            // the in-language seams [E2]: open, spawn, the boundary join,
            // and cancel. The lowerer synthesizes every call site, so a
            // staged id always names this frame's own open nursery; the
            // guards answer the checked-read bounce tag (the native-table
            // convention) for hand-assembled corruption.
            ControlNative::NurseryOpen => {
                let id = self.open_fresh_nursery();
                overlay_word(Value::I64(i64::from(id.0)), &mut self.stack, caller, fp);
                Ok(SwitchOutcome::Bounced)
            },
            ControlNative::NurserySpawn => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let word = control_word(&self.stack, fp, caller, 1)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                // the callable word: a scalar `Fn` names the child with no
                // captures; a closure object's `Ref` carries them as the
                // entry arguments
                let (fref, args) = match word {
                    Value::Fn(fref) => (fref, Vec::new()),
                    Value::Ref(obj) => match self.closure_parts(obj) {
                        Ok((fref, captures)) => (fref, captures),
                        Err(e) => return Err(e),
                    },
                    staged => {
                        return Err(Trap::CallLayout(SlideError::TypeMismatch {
                            index: 1,
                            staged: staged.slot_type(),
                            expected: SlotType::Fn,
                        }));
                    },
                };
                self.spawn_ref_into(Some(nursery), fref, &args)?;
                overlay_word(Value::I64(0), &mut self.stack, caller, fp);
                Ok(SwitchOutcome::Bounced)
            },
            ControlNative::NurseryBoundary => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let mask = control_i64(&self.stack, fp, caller, 1)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                if self.pool[self.current.index()].nursery == Some(nursery) {
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                if self.nurseries[nursery.index()].driving {
                    // the interlock cycle of the join seam, one level up
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                self.park_current(active, CodePc(pc + 1));
                Ok(SwitchOutcome::Interlock(InterlockRequest {
                    nursery,
                    joiner: self.current,
                    resume_pc: CodePc(pc + 1),
                    seam: DriveSeam::Boundary { mask },
                }))
            },
            ControlNative::NurseryCancel => {
                // the fiber's deliberate failure: the trap path ledgers
                // the outcome and raises the cancel flag; the call never
                // answers. The staged words are the failing value's flat
                // span — word 0 the member's tag, the rest its payload
                // words (the native channel's staging shape); the staged
                // count is the call site's fact, bounded by the frame's
                // own area
                if usize::from(nargs) > caller.arg_area_len() {
                    return Err(Trap::CallLayout(SlideError::StagingTooSmall {
                        available: caller.arg_area_len(),
                        needed: usize::from(nargs),
                    }));
                }
                let mut payload = Vec::with_capacity(usize::from(nargs));
                for index in 0..usize::from(nargs) {
                    payload.push(control_word(&self.stack, fp, caller, index)?);
                }
                Err(Trap::NurseryCancelled { payload })
            },
            ControlNative::ReplPark => {
                // the REPL's resumed frame parks here: the staged span is
                // the submission's whole answer (the splice mirrored the
                // exit's RET staging), word 0 riding the event to the host
                // while the whole span rides the seam's marker for the
                // REPL drive to read. Only a host drive ever meets the
                // seam — the call sites live in the entry function the
                // splice owns — so the resumer switch of the yield arm
                // cannot occur; a stray in-language resumer still gets the
                // honest switch.
                if usize::from(nargs) > caller.arg_area_len() {
                    return Err(Trap::CallLayout(SlideError::StagingTooSmall {
                        available: caller.arg_area_len(),
                        needed: usize::from(nargs),
                    }));
                }
                let answer = FrameAnswer::staged(usize::from(nargs), |index| {
                    control_word(&self.stack, fp, caller, index)
                })?;
                let head = answer.as_slice().first().copied().unwrap_or(Value::I64(0));
                self.park_current(active, CodePc(pc + 1));
                let resumer = self.pool[self.current.index()].resumer;
                match resumer.filter(|&r| self.pool[r.index()].resumable()) {
                    None => {
                        self.repl_park = Some(answer);
                        Ok(SwitchOutcome::Host(DriveOutcome::Yielded(head)))
                    },
                    Some(target) => {
                        let (a, at) = self.switch_in(target, head);
                        Ok(SwitchOutcome::Entered(a, at))
                    },
                }
            },
        }
    }

    /// Writes the boundary seam's answer span onto the joiner's call
    /// site: word 0 the discriminant, the tail the failing member's flat
    /// word span. Runs after the joiner's re-entry, so the current
    /// context is its frame.
    fn overlay_boundary(&mut self, active: &Active, tag: u8, payload: &[Value]) {
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        let f = &self.registry.pinned(active.version).functions()[active.func as usize];
        let area = f.layout.arg_area_len();
        // the span channel's bounds check: the boundary's static floor is
        // the two-word answer, and the payload words check at the write
        // exactly like a span answer's (a too-narrow caller is a
        // hand-assembled module — the lowerer widens the area to the
        // widest contained member)
        if 1 + payload.len() > area {
            // a hand-assembled module only: the lowerer widens the area to
            // the widest contained member. The write cannot happen, and
            // the payload's rows are already this fiber's from the death
            // handoff — its own drain settles them, so no ledger action
            return;
        }
        overlay_tag(tag, payload, &mut self.stack, &f.layout, fp);
    }

    /// Writes the fork seam's whole-span answer tail onto the re-entered
    /// joiner's call site: the head word already landed through the
    /// switch payload, the tail words following on the overlay slots the
    /// CALLG site's answer occupies — the tag-less total's delivery
    /// twin. The park-time width check refused an overlay too narrow
    /// for the whole span, so a missed write is hand-assembled-only
    /// (the boundary seam's same defensive skip; the delivered words'
    /// rows settle at the joiner's drain door).
    fn overlay_span_tail(&mut self, active: &Active, tail: &[Value]) {
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        let f = &self.registry.pinned(active.version).functions()[active.func as usize];
        let area = f.layout.arg_area_len();
        if 1 + tail.len() > area {
            return;
        }
        let body_len = f.layout.body_len();
        let start = f.layout.arg_area().start;
        for (index, value) in tail.iter().enumerate() {
            self.stack[word_of(fp, start + 1 + index, body_len)] = Word::Slot(*value);
        }
    }

    /// Whether the joiner a drive parked at `resume_pc` still stands
    /// parked there — the same predicate [`Self::reenter_joiner`]
    /// applies, read at consumption time so a channel that hands the
    /// joiner words (the boundary's collected answers list) builds them
    /// only for a delivery that can land.
    fn joiner_parked(&self, joiner: FiberId, resume_pc: CodePc) -> bool {
        self.pool.get(joiner.index()).is_some_and(|f| {
            f.state == FiberState::Suspended && f.resume.as_ref().is_some_and(|r| r.pc == resume_pc)
        })
    }

    /// Re-enters the joiner a join/boundary drive parked at `resume_pc`,
    /// unless a child resumed it mid-drive and it moved on — died, was
    /// cancel-cut, trapped, or re-parked at another call — in which case
    /// the boundary's answer has no call site, and the drive surfaces
    /// `value` to its own host instead of switching into a fiber with no
    /// continuation (the shrunk `nursery_joiner_reentry` repro).
    fn reenter_joiner(
        &mut self,
        joiner: FiberId,
        resume_pc: CodePc,
        value: Value,
    ) -> Option<(Active, CodePc)> {
        let parked_here = self.pool.get(joiner.index()).is_some_and(|f| {
            f.state == FiberState::Suspended && f.resume.as_ref().is_some_and(|r| r.pc == resume_pc)
        });
        if parked_here { Some(self.switch_in(joiner, value)) } else { None }
    }

    /// Runs an entry function to its `HALT` or entry-frame `RET`/`RETSPAN`
    /// and reports the resulting answer span: the root fiber's drive to
    /// completion [E2]. A multi-word answer (a tuple, a widened span)
    /// crosses whole — one word per span word, never a truncated head.
    /// The root's execution state resets per run (other pool fibers keep
    /// their state); on a `Trap` the state is kept for [`Vm::backtrace`].
    /// A root that parks with the host as resumer is re-entered with the
    /// bare `PLATFORM_NONE` tag as the park's answer — a `run` entry
    /// drives to completion; interactive stepping is
    /// [`Vm::resume_fiber`].
    pub fn run(&mut self, module: &str, func: &str, args: &[Value]) -> Result<FrameAnswer, Trap> {
        self.run_budgeted(module, func, args, None)
    }

    /// [`Vm::run`] under a total step budget (the comptime fence's
    /// runaway bound): the fueled instantiation of the dispatch loop
    /// charges one step per decoded instruction, and a drive past the
    /// budget answers [`Trap::OutOfFuel`] instead of hanging — a pure
    /// fn whose loop never terminates while its stack stays flat has
    /// no other termination witness. The budget is the calling host's
    /// policy, never a property of the code; every unbounded drive
    /// keeps [`Vm::run`].
    pub fn run_with_budget(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
        fuel: u64,
    ) -> Result<FrameAnswer, Trap> {
        self.run_budgeted(module, func, args, Some(fuel))
    }

    /// The two run entries' shared setup and drive: `fuel` selects the
    /// dispatch loop's instantiation, so the unbounded run's dispatch
    /// cost stays untouched (the pin table's contract).
    fn run_budgeted(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
        fuel: Option<u64>,
    ) -> Result<FrameAnswer, Trap> {
        self.trap_pc = None;
        let (version, func_id) = self.resolve_entry(module, func, args)?;
        let frame_size = self.registry.pinned(version).functions()[func_id as usize]
            .layout
            .frame_size() as usize;
        self.host_release(); // stash any interrupted drive's context
        // the reset door of the death walk: a halted or trapped root
        // drive's kept frames fire their deinits here (E5), then release
        // their claims before the reset discards them (the tail-native
        // abandon's count dies here now, at the next run, instead of
        // leaking to the VM's drop) — and a trapped root's abandoned
        // nurseries die with it here, their never-entered children
        // releasing their rows and slots the same way
        self.fire_teardown_hooks(FIBER_ROOT, &[]);
        self.drain_claims(FIBER_ROOT);
        self.release_owned_nurseries(FIBER_ROOT);
        let root = &mut self.pool[FIBER_ROOT.index()];
        root.segment.clear();
        Self::push_entry_frame(root, version, func_id, args, frame_size);
        root.resume = None;
        root.resumer = None;
        root.state = FiberState::Running;
        let root_slot = &mut self.pool[FIBER_ROOT.index()];
        std::mem::swap(&mut self.stack, &mut root_slot.segment);
        std::mem::swap(&mut self.callinfo, &mut root_slot.callinfo);
        std::mem::swap(&mut self.objects, &mut root_slot.objects);
        self.current = FIBER_ROOT;
        self.ctx_live = true;
        let active = Active {
            version,
            func: func_id,
        };
        let answer = match fuel {
            Some(fuel) => {
                self.fuel = fuel;
                self.root_drive::<true>(active)
            },
            None => self.root_drive::<false>(active),
        };
        self.fuel = 0;
        answer
    }

    /// A root drive's completion loop, shared by the two run entries:
    /// the constant parameter selects the dispatch loop's instantiation,
    /// so the unbounded drive compiles exactly the dispatch it always
    /// had and the budgeted drive adds its per-instruction step charge
    /// inside the fueled instantiation alone.
    fn root_drive<const FUELED: bool>(&mut self, active: Active) -> Result<FrameAnswer, Trap> {
        let mut active = active;
        let mut pc = CodePc(0);
        loop {
            match self.execute_host_step::<FUELED>(active, pc, usize::MAX) {
                Ok(DriveOutcome::Died(answer)) => {
                    self.host_release();
                    // the completed root's whole answer span crosses to
                    // the host, one delivered word per span word
                    for value in answer.as_slice() {
                        self.host_delivery(value);
                    }
                    return Ok(answer);
                },
                Ok(DriveOutcome::Yielded(_)) => {
                    let (a, p) = self.switch_in(FIBER_ROOT, Value::Tag(PLATFORM_NONE));
                    active = a;
                    pc = p;
                },
                // the drive went quiet on a world park: register the
                // wait, then the single-thread executor's continuation —
                // the next ready fiber (FIFO), the reactor's wait
                // bridging the gap when nothing is runnable
                Ok(DriveOutcome::WorldPark(req)) => {
                    if let ParkKind::World(wait) = req.kind {
                        self.register_wait(req.fiber, wait);
                    }
                    match self.executor_continue(FIBER_ROOT) {
                        Ok(ExecutorStep::Continue((a, p))) => {
                            active = a;
                            pc = p;
                        },
                        Ok(ExecutorStep::HostAnswer(answer)) => {
                            self.host_release();
                            for value in answer.as_slice() {
                                self.host_delivery(value);
                            }
                            return Ok(answer);
                        },
                        Err(trap) => {
                            self.trap_pc = Some(pc);
                            self.host_release();
                            self.trap_child(self.current, &trap);
                            self.trap_delivery(&trap);
                            return Err(trap);
                        },
                    }
                },
                Ok(DriveOutcome::Frame(_)) => {
                    unreachable!("a host drive answers no reentry frame")
                },
                Ok(DriveOutcome::Interlock(_)) => {
                    unreachable!("the step drains every interlock before returning")
                },
                Err((trap, at)) => {
                    self.trap_pc = Some(at);
                    self.host_release();
                    // the drive may have ended on a nursery child entered
                    // in-language: ledger it and raise its nursery's flag
                    self.trap_child(self.current, &trap);
                    // the trap's payload word crosses to the host
                    self.trap_delivery(&trap);
                    return Err(trap);
                },
            }
        }
    }

    /// Whether the live call chain still carries this drive's reentry
    /// frame (the sentinel-metadata frame [`Vm::call_hosted`] pushed at
    /// `boundary - 1`): the honesty condition for effects that cannot
    /// suspend the Rust-side native loop. A fiber switched in mid-drive
    /// never carries one — a resumable fiber parks only at switch
    /// points, never mid-native — so the sentinel test cannot
    /// false-positive across a switch.
    fn reentry_live(&self, boundary: usize) -> bool {
        if boundary == usize::MAX || self.callinfo.len() < boundary {
            return false;
        }
        matches!(
            self.stack[self.callinfo[boundary - 1].fp],
            Word::Meta(m) if m.return_pc.0 == u32::MAX
        )
    }

    /// The outgoing-argument area of the frame below the executing one,
    /// when the chain has one: a tail site's answer overlays that caller's
    /// area, never the reusing frame's own.
    fn caller_arg_area(&self) -> Option<usize> {
        let at = self.callinfo.len().checked_sub(2)?;
        let caller = &self.callinfo[at];
        Some(
            self.registry.pinned(caller.module).functions()[caller.func as usize]
                .layout
                .arg_area_len(),
        )
    }

    /// Drives one reentrant call of a planned function from native code
    /// — the lambda-taking container combinators' per-element
    /// invocation: a synthetic reentry frame (the entry-frame sentinel
    /// return pc marks it) is pushed on the CURRENT fiber's chain at
    /// `boundary` and driven to its return, which answers
    /// [`DriveOutcome::Frame`]. The callee word is callable-defaulted
    /// (F2): a scalar FuncRef names the function directly, a `Ref` word
    /// names a closure object whose capture block lands in the callee's
    /// leading local slots exactly like a CALLS copy-in (one acquired
    /// count per `Ref` capture, balanced by the callee body's own
    /// placement). Each Ref argument carries one count into
    /// the frame (the local-call hand-off: the callee's placement owns
    /// its params), and the returned words transfer their counts to the
    /// caller — one word for a scalar body, the whole span for a
    /// multi-word return (the element span each way: the arguments are
    /// the callee's flattened parameter words, the answer its whole
    /// return span). A drive that ends any other way —
    /// the lambda HALTs under nesting, a fiber switch's resumer chain
    /// never returns, a nursery cancel kills the fiber, a yield parks
    /// at the host — refuses with [`Trap::ReentrancyRefused`]: a
    /// Rust-side loop cannot suspend, and the chain is already lost or
    /// parked. On every non-returning outcome the fiber that hosted the
    /// reentry frame is interrupted (never resumable again — a parked
    /// resume would run its lambda into the sentinel return pc). A
    /// trapped lambda propagates its trap with the frames kept for
    /// [`Vm::backtrace`].
    pub(crate) fn call_hosted(
        &mut self,
        callee: Value,
        args: &[Value],
    ) -> Result<FrameAnswer, Trap> {
        self.call_hosted_in(callee, args, None)
    }

    /// The hosted call seam's owner-explicit half: the teardown pass
    /// names the dying frame's version row itself (the hook's owning
    /// frame may sit deep in the dying chain, not at its innermost entry),
    /// every other caller resolving staged values through the current
    /// slot.
    pub(crate) fn call_hosted_in(
        &mut self,
        callee: Value,
        args: &[Value],
        owner: Option<VersionIdx>,
    ) -> Result<FrameAnswer, Trap> {
        let nargs = u8::try_from(args.len()).expect("hosted-call arities are tiny");
        let (funcref, captures) = match callee {
            Value::Fn(funcref) => (funcref, None),
            Value::Ref(obj) => {
                let (header, block) = stage_closure(
                    &mut self.objects,
                    &mut self.pool,
                    self.current,
                    &mut self.calls_staging,
                    obj,
                )?;
                (header, Some(block))
            },
            other => {
                return Err(Trap::SlotTypeMismatch {
                    slot: 0,
                    expected: SlotType::Fn,
                    actual: other.slot_type(),
                })
            },
        };
        let (version, func_id) =
            StackState::resolve_func_word(&self.registry, funcref, owner, nargs)?;
        let target = &self.registry.pinned(version).functions()[func_id as usize];
        let frame_size = target.layout.frame_size() as usize;
        let base = self.stack.len();
        let needed = base + frame_size;
        if needed > self.limit {
            return Err(Trap::StackOverflow {
                needed,
                limit: self.limit,
            });
        }
        self.stack.push(Word::Meta(FrameMetadata {
            return_pc: CodePc(u32::MAX),
            frame_size: u32::try_from(frame_size).expect("frame span fits u32"),
            tail: TailChain::default(),
        }));
        // the local-call hand-off: each Ref argument carries one count
        // into the frame (the callee's placement treats its params as
        // owned — returning one transfers that count with the answer,
        // and a dropped one balances at the body's exit drops). The
        // word may name a foreign heap, so the inc routes.
        let handoff_owner = self.current;
        for &arg in args {
            if let Value::Ref(word) = arg {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    self.ctx_live,
                    handoff_owner,
                    word,
                )?;
            }
        }
        for &arg in args {
            self.stack.push(Word::Slot(arg));
        }
        self.stack.grow_to(needed);
        // the closure's capture block lands in the callee's leading local
        // slots — the CALLS copy-in discipline: one acquired count per
        // Ref capture, the body's own placement balancing it, while the
        // closure object keeps its own count untouched
        let capture_body_len = target.layout.body_len();
        if let Some(block) = &captures {
            let end = target.layout.params_len() + block.len();
            if end > capture_body_len {
                return Err(Trap::SlotOutOfRange {
                    slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                    frame_slots: target.layout.slots().len(),
                });
            }
            let owner = self.current;
            for (index, &value) in block.iter().enumerate() {
                if let Value::Ref(word) = value {
                    Self::inc_ref_split(
                        &mut self.objects,
                        &mut self.pool,
                        &mut self.claims,
                        owner,
                        self.ctx_live,
                        owner,
                        word,
                    )?;
                }
                let slot = u8::try_from(target.layout.params_len() + index)
                    .expect("capture slot fits u8 after the range check");
                write_slot(
                    &mut self.stack,
                    base,
                    slot,
                    target.layout.slots(),
                    capture_body_len,
                    value,
                )?;
            }
        }
        self.callinfo.push(CallInfo {
            fp: base,
            module: version,
            func: func_id,
        });
        let fiber = &mut self.pool[self.current.index()];
        fiber.max_len = fiber.max_len.max(needed);
        let boundary = self.callinfo.len();
        let host = self.current;
        // the entry and the reentry drive's Active name one table row:
        // a hosted call mints no version handle
        let active = Active {
            version,
            func: func_id,
        };
        let driven = match self.execute_host_step::<false>(active, CodePc(0), boundary) {
            Ok(DriveOutcome::Frame(value)) => Ok(value),
            Ok(DriveOutcome::Died(_) | DriveOutcome::Yielded(_) | DriveOutcome::WorldPark(_)) => {
                Err(Trap::ReentrancyRefused)
            },
            Ok(DriveOutcome::Interlock(_)) => {
                unreachable!("the step drains every interlock before returning")
            },
            Err((trap, _at)) => Err(trap),
        };
        if driven.is_err() {
            // the Rust-side loop is gone, so the sentinel reentry frame on
            // the hosting fiber's chain can never be answered — the drive
            // answered anything but its Frame. A parked host left resumable
            // would re-enter on a later resume and run its lambda straight
            // into the sentinel return pc, so interrupt it like a trapped
            // drive: frames kept (the staged-argument counts stay with
            // them, the same accounting as every trap path), re-entry
            // refused with the structured fiber refusal
            let fiber = &mut self.pool[host.index()];
            if fiber.state != FiberState::Dead {
                fiber.resume = None;
                fiber.state = FiberState::Running;
            }
        }
        driven
    }

    /// The routing face over the running fiber's store and the pool, for
    /// the dispatcher's own routed count mutations and reads.
    fn heaps(&mut self) -> Heaps<'_> {
        Heaps::new(
            self.current,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            self.ctx_live,
        )
    }

    /// The death walk's router: a parked drive's context store is the
    /// scratch, so every store is reached through the pool.
    fn heaps_walk(&mut self) -> Heaps<'_> {
        Heaps::new(
            self.current,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            false,
        )
    }

    /// The root fiber's store, for the host faces: the running context's
    /// when the root runs, its pool slot's otherwise.
    fn root_store(&mut self) -> &mut Objects {
        if self.ctx_live && self.current == FIBER_ROOT {
            &mut self.objects
        } else {
            &mut self.pool[FIBER_ROOT.index()].objects
        }
    }

    /// Routed `rc_inc` by the word's owner axis, queues drained.
    fn inc_ref(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().inc(word)
    }

    /// Routed `rc_dec` by the word's owner axis, queues drained.
    fn dec_ref(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().dec(word)
    }

    /// [`Vm::inc_ref`] over disjoint fields, for sites holding a live
    /// staging-buffer borrow (the capture copy-ins). The count is claimed
    /// for `claimant`'s frames — the running fiber's own copy-ins, or the
    /// spawned child's entry frame at the spawn seam.
    fn inc_ref_split(
        objects: &mut Objects,
        pool: &mut [Fiber],
        claims: &mut [ClaimTable],
        running: FiberId,
        live: bool,
        claimant: FiberId,
        word: RefId,
    ) -> Result<(), Trap> {
        Heaps::new(running, objects, pool, claims, live).inc_for(claimant, word)
    }

    /// An object-graph edge's count (a closure's capture adoption): routed
    /// by the word's owner, owned by the object — its free cascade
    /// releases it, no frame claim.
    fn inc_edge(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().inc_edge(word)
    }

    /// The teardown drain walk: every claim the fiber's frames own,
    /// released, routed to each word's own heap — the one channel for
    /// cross-heap count mutation on a death. A bogus word (a
    /// hand-assembled release without an acquire) errors and is ignored:
    /// the walk releases what the store knows.
    pub(crate) fn drain_claims(&mut self, fiber: FiberId) {
        self.drain_claims_except(fiber, &[]);
    }

    /// [`Self::drain_claims`] holding `held` back — a word about to ride
    /// its recorded outcome to a delivery keeps its claim. The claims are
    /// taken before any dec runs, so both doors ride the walk's no-ledger
    /// dec: a fiber running under the live context through its context
    /// store, every other case (and every fiber while the drive is parked)
    /// through the pool, the scratch never receiving a walk's dec.
    fn drain_claims_except(&mut self, fiber: FiberId, held: &[RefId]) {
        let live_here = self.ctx_live && fiber == self.current;
        for (word, n) in self.claims[fiber.index()].take_all_except(held) {
            for _ in 0..n {
                let _ = if live_here {
                    self.heaps().dec_walk(word)
                } else {
                    self.heaps_walk().dec_walk(word)
                };
            }
        }
        // the door's frames never run again, so the fiber's intern pins
        // release with the walk (interrupted, marking, reuse, and reset
        // deaths; a live-here death releases again in `die_current`,
        // idempotently)
        if live_here {
            self.objects.release_pins();
        } else {
            self.pool[fiber.index()].objects.release_pins();
        }
        // the door's frames never run again, so the fiber's arena
        // temporaries die with them: a wholesale rewind to the base, the
        // frame marks dropping with it (a recycled slot starts empty)
        self.pool[fiber.index()].arena.release_all();
    }

    /// The teardown deinit pass (E5: teardown walks frames running
    /// deinits): every deinit claim of the dying fiber's kept frames
    /// whose liveness the claim ledger certifies fires its hook, exactly
    /// once, deterministically — frames innermost-first (an unwind's
    /// order), each frame's claims in directory (drop-site) order. The
    /// drive rides [`Vm::call_hosted`] on a live context: the dying
    /// fiber's own when it is current (the cancel-cut door), a takeover
    /// of its parked context otherwise (interrupted and dead fibers carry
    /// no resume point, so the three ownership swaps run directly). A
    /// takeover over a live third fiber parks that fiber's context first
    /// and re-adopts it after the pass: the door's caller continues on
    /// its own frames.
    ///
    /// The drive rule: a teardown hook must return to its driver. A hook
    /// that parks (a yield, a resume switch), dies (a `HALT`), traps,
    /// observes a raised cancel flag at a backward branch, or calls a
    /// nursery control seam (the natives refuse at the dispatch under the
    /// drive flag) refuses with [`Trap::ReentrancyRefused`]; its
    /// already-run effects stand, the pass ends at that hook, and the
    /// remaining hooks' claims drain unfired — teardown never re-enters
    /// the nursery and never surfaces a user-hook failure, exactly the
    /// death walk's bogus-word honesty.
    pub(crate) fn fire_teardown_hooks(&mut self, fiber: FiberId, held: &[RefId]) {
        self.fire_teardown_hooks_past(fiber, held, None, None);
    }

    /// [`Self::fire_teardown_hooks`] with the REPL rollback's partial
    /// gate: `pre` — the pre-step ledger reading — and `parked` — the
    /// parked frame's words as the step entered — together skip every
    /// claim that was already live before the partial run started (a
    /// band set in the parked words is the parked frame's own binder, a
    /// span word already counted in the reading is a value moved into a
    /// dying frame through a parked binding), so only the claims the run
    /// itself established fire and every surviving value keeps its hook
    /// for its real death.
    pub(crate) fn fire_teardown_hooks_past(
        &mut self,
        fiber: FiberId,
        held: &[RefId],
        pre: Option<&ClaimReading>,
        parked: Option<&[Word]>,
    ) {
        if self.teardown_drive {
            // a door inside a teardown drive never starts a second pass.
            // The nursery seams refuse under the drive flag, so no path
            // reaches a door in-drive; this stays as the safety net for an
            // exotic switch tangle — the outer pass remains the only walker
            return;
        }
        let drives = self.collect_teardown_drives(fiber, held, pre, parked);
        if drives.is_empty() {
            return;
        }
        let takeover = !(self.ctx_live && self.current == fiber);
        let interrupted = takeover.then_some(self.current).filter(|_| self.ctx_live);
        if takeover {
            self.host_release();
            let slot = &mut self.pool[fiber.index()];
            std::mem::swap(&mut self.stack, &mut slot.segment);
            std::mem::swap(&mut self.callinfo, &mut slot.callinfo);
            std::mem::swap(&mut self.objects, &mut slot.objects);
            self.current = fiber;
            self.ctx_live = true;
        }
        self.teardown_drive = true;
        for (row, callee, words) in drives {
            if self
                .call_hosted_in(Value::Fn(callee), &words, Some(row))
                .is_err()
            {
                break;
            }
        }
        self.teardown_drive = false;
        if takeover {
            self.host_release();
        }
        if let Some(resume) = interrupted {
            // the door fired mid-flight of a live fiber (a spawner's
            // recycle door over a dead slot): the drive's caller
            // continues on its own frames, so the parked context swaps
            // back in — otherwise it stays stranded in its slot while
            // the caller runs on the dying fiber's stale scratch buffer
            let slot = &mut self.pool[resume.index()];
            std::mem::swap(&mut self.stack, &mut slot.segment);
            std::mem::swap(&mut self.callinfo, &mut slot.callinfo);
            std::mem::swap(&mut self.objects, &mut slot.objects);
            self.current = resume;
            self.ctx_live = true;
        }
    }

    /// The pass's drive list: for each kept frame (innermost-first) and
    /// each directory site (drop-site order), the hook target plus the
    /// span words read from the frame, gated on the site's establishment
    /// band when it carries one — the frame word the claim's
    /// registration set and every death path cleared, so the doors fire
    /// exactly the claims that live at their slots: Ref-free spans
    /// included, dead claims whose counts survive on outstanding borrows
    /// excluded. A bandless site (a hand-assembled shape) falls back to
    /// the claim ledger: every `Ref` word of the span must still hold
    /// its kind (the frame word is a live reference, not zero init or a
    /// clobbered slot) and a count (the claim's own drop releases its
    /// counts, so a dead claim answers zero), so a span with no `Ref`
    /// word stays unfired there. The held-back cancel payload skips, a
    /// word consumed by an earlier drive never fires twice, and each
    /// fired band clears in its frame word so a later door on the same
    /// kept frames cannot fire the claim again. Each drive names its
    /// owning frame's version row too: the hook declares in that row's
    /// module, so the hosted drive resolves it through the row (the
    /// frame-own rule), never the module's current slot.
    fn collect_teardown_drives(
        &mut self,
        fiber: FiberId,
        held: &[RefId],
        pre: Option<&ClaimReading>,
        parked: Option<&[Word]>,
    ) -> Vec<(VersionIdx, FuncRef, Vec<Value>)> {
        let live = self.ctx_live && self.current == fiber;
        // the frames are read by index, innermost first: each frame's
        // version row is Copy, so the walk holds no handle at all
        let depth =
            if live { self.callinfo.len() } else { self.pool[fiber.index()].callinfo.len() };
        let mut drives = Vec::new();
        let mut driven_words: Vec<RefId> = Vec::new();
        let mut driven_bases: Vec<u8> = Vec::new();
        for index in (0..depth).rev() {
            let (fp, version, func) = if live {
                let frame = &self.callinfo[index];
                (frame.fp, frame.module, frame.func)
            } else {
                let frame = &self.pool[fiber.index()].callinfo[index];
                (frame.fp, frame.module, frame.func)
            };
            let Some(module_id) = self
                .registry
                .module_id(self.registry.pinned(version).name())
            else {
                continue;
            };
            let sites = self.hook_sites_for(version, func, module_id);
            if sites.is_empty() {
                continue;
            }
            driven_bases.clear();
            for site in sites.iter().copied() {
                // the partial gate's band witness: a site of the frame
                // the parked words cover whose establishment band was
                // already set before the partial run started belongs to
                // the parked frame's own binder — its claim survives the
                // rollback for the value's real death, so only a band
                // the run itself established fires
                if let (Some(words), Some(band)) = (parked, site.band) {
                    let at = fp + 1 + usize::from(band);
                    if matches!(words.get(at), Some(Word::Slot(Value::Tag(CLAIM_BAND_SET)))) {
                        continue;
                    }
                }
                if driven_bases.contains(&site.base) {
                    continue; // one value lives in one slot per frame
                }
                let Some(drive) = self.gate_teardown_site(
                    fiber,
                    fp,
                    version,
                    site,
                    module_id,
                    held,
                    &driven_words,
                    pre,
                ) else {
                    continue;
                };
                driven_bases.push(site.base);
                driven_words.extend(drive.1.iter().filter_map(|w| match w {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                }));
                if let Some(band) = site.band {
                    // the fired claim's band clears in the frame word
                    // itself: the drive runs the hook, not the body's own
                    // clear op, and a later door over the same kept frames
                    // (the recycle safety net) must never fire it twice
                    let at = fp + 1 + usize::from(band);
                    let cleared = Word::Slot(Value::Tag(CLAIM_BAND_CLEAR));
                    if live {
                        self.stack[at] = cleared;
                    } else {
                        self.pool[fiber.index()].segment[at] = cleared;
                    }
                }
                // the hook addresses this frame's own module, so its row
                // (not the chain's innermost entry) drives the resolution
                drives.push((version, drive.0, drive.1));
            }
        }
        drives
    }

    /// One site's gate: reads the span words out of the frame (the live
    /// stack under the dying fiber's context, its pool segment otherwise)
    /// and checks every span word against the hook's parameter kinds. A
    /// site carrying an establishment band (the marker the body's own
    /// drop clears right after the hook call) reads the band word: set
    /// means the claim lives at this slot — the exact once-per-value
    /// witness, firing Ref-free spans and refusing dead claims whose
    /// counts survive on outstanding borrows. A bandless site (a
    /// hand-assembled shape) falls back to the claim ledger: every `Ref`
    /// word of the span must still hold a count. `pre` — the pre-step
    /// ledger reading of a partial run's rollback — skips the site when
    /// any `Ref` word already carried a count before the run started:
    /// that value survives the rollback through its parked claim.
    /// `None` = skip.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gate_teardown_site(
        &self,
        fiber: FiberId,
        fp: usize,
        version: VersionIdx,
        site: HookSite,
        module_id: u32,
        held: &[RefId],
        driven_words: &[RefId],
        pre: Option<&ClaimReading>,
    ) -> Option<(FuncRef, Vec<Value>)> {
        let hook = &self.registry.pinned(version).functions()[site.hook as usize];
        let params = hook.layout.params();
        let width = usize::from(site.width);
        if params.len() != width {
            return None; // not this body's drop shape (a stale directory)
        }
        let read = |at: usize| -> Option<Value> {
            let word = if self.ctx_live && self.current == fiber {
                self.stack.get(fp + 1 + at)
            } else {
                self.pool[fiber.index()].segment.get(fp + 1 + at)
            }?;
            match word {
                Word::Slot(v) => Some(*v),
                Word::Meta(_) => None,
            }
        };
        let mut words = Vec::with_capacity(width);
        for at in usize::from(site.base)..usize::from(site.base) + width {
            words.push(read(at)?);
        }
        if !held.is_empty() {
            // the outcome's payload rides to a delivery: its values are
            // alive
            if words
                .iter()
                .any(|w| matches!(w, Value::Ref(word) if held.contains(word)))
            {
                return None;
            }
        }
        // the establishment band is the witness; span kinds still check
        // (a clobbered slot refuses) and a word an earlier drive consumed
        // never fires twice, but the ledger's counts no longer gate
        if site.band.is_some_and(|band| {
            !matches!(read(usize::from(band)), Some(Value::Tag(CLAIM_BAND_SET)))
        }) {
            return None;
        }
        let ledger_witness = site.band.is_none();
        let mut witness = false;
        // one match classifies each span word against the hook's parameter
        // kind: the layout's own Ref arm names its object (an arena word
        // refuses there — it classifies Ref but names no object), a Raw
        // parameter stages any word, every other kind checks once
        for (word, ty) in words.iter().zip(params.iter()) {
            match (word, ty) {
                (Value::Ref(id), SlotType::Ref) => {
                    if driven_words.contains(id)
                        || (ledger_witness && self.claims[fiber.index()].count_of(*id) == 0)
                        || pre.is_some_and(|reading| reading.count_of(*id) > 0)
                    {
                        return None;
                    }
                    witness = true;
                },
                (_, SlotType::Raw) => {},
                (_, SlotType::Ref) => return None,
                (word, ty) if word.slot_type() == *ty => {},
                _ => return None,
            }
        }
        // a band carries its own witness; the ledger path needs a Ref word
        (witness || site.band.is_some()).then(|| (FuncRef::new(module_id, site.hook), words))
    }

    /// The derived deinit-claim directory of one function (crates/ds-vm/
    /// src/claims.rs), cached per (version, function) and swept wholesale
    /// when the registry generation moves — the kstr cache's discipline.
    pub(crate) fn hook_sites_for(
        &mut self,
        version: VersionIdx,
        func: u32,
        own_id: u32,
    ) -> Arc<Vec<HookSite>> {
        let generation = self.registry.generation();
        if self.hook_dirs_stamp != generation {
            self.hook_dirs.clear();
            self.hook_dirs_stamp = generation;
        }
        // the key is the version's pinned address: stable for the row's
        // whole life, and the directory clears at the same install that
        // sweeps the row, so a reused address never meets a stale row
        let pinned = self.registry.pinned(version);
        let key = (std::ptr::from_ref(pinned) as usize, func);
        if let Some(sites) = self.hook_dirs.get(&key) {
            return Arc::clone(sites);
        }
        let sites = hook_sites(pinned, func, own_id);
        self.hook_dirs.insert(key, Arc::clone(&sites));
        sites
    }

    /// A value crossing to the host from the drive-ended fiber: its
    /// claim leaves the ledger. The host never releases, so the count
    /// lives until the stores die — exactly the pre-split behavior.
    pub(crate) fn host_delivery(&mut self, value: &Value) {
        if let Value::Ref(word) = value {
            self.claims[self.current.index()].release(*word);
        }
    }

    /// The payload span a nursery trap carries: delivered to the host
    /// like any drive-ending value, one `Ref` word at a time.
    pub(crate) fn trap_delivery(&mut self, trap: &Trap) {
        match trap {
            Trap::NurseryCancelled { payload } | Trap::NurseryUncontained { payload } => {
                for value in payload {
                    self.host_delivery(value);
                }
            },
            _ => {},
        }
    }

    /// A closure word's header and capture block copied out of its own
    /// heap ([`Heaps::closure_parts`]).
    fn closure_parts(&mut self, word: RefId) -> Result<(FuncRef, Vec<Value>), Trap> {
        self.heaps().closure_parts(word)
    }

    /// Splits the VM for a generic builtin body's pure-object work: the
    /// store-side routing face and the output sink, disjoint from every
    /// other field. The running fiber's store is the allocation default;
    /// every id-taking service routes. The reentrant bodies (the
    /// lambda-taking combinators) keep `vm` whole and call back through
    /// [`Vm::call_hosted`].
    pub(crate) fn split(&mut self) -> (Heaps<'_>, &mut OutputSink) {
        (
            Heaps::new(
                self.current,
                &mut self.objects,
                &mut self.pool,
                &mut self.claims,
                self.ctx_live,
            ),
            &mut self.out,
        )
    }

    /// The current-table index a pool FuncRef's uop target resolves to for
    /// a caller running `caller_row`: the verbatim index unless the caller
    /// runs one of the target module's retiring rows and names its own
    /// module — then the site's index reads the caller row's own index
    /// space, so a kept name keeps the index (the ordinary redirect the
    /// re-check's name judge pins) and a moved index-embedded name (a
    /// synthesized lambda, renamed by the reorder itself) translates
    /// through the content judge: body plus capture set hashing equal
    /// against exactly one current candidate. `None` names an identity
    /// that did not survive the swap — the call refuses rather than
    /// routing the verbatim index into whatever function now holds it.
    /// Sites crossing modules keep the verbatim rule outright: the
    /// caller's row records no linked version of the target, so the
    /// cross-module redirect stays the current-slot read.
    fn uop_site_index(
        registry: &ModuleRegistry,
        caller_row: VersionIdx,
        module: &str,
        index: u32,
    ) -> Option<u32> {
        let row = registry.version(caller_row)?;
        if row.name() != module {
            return Some(index);
        }
        let current = registry.slots(module)?.current_idx();
        if current == caller_row {
            return Some(index);
        }
        let at = usize::try_from(index).ok()?;
        let old = row.functions().get(at)?;
        let current = registry.version(current)?;
        // the kept-name fast path: the current table's function at the
        // same index carries the site's name — verbatim
        if current
            .functions()
            .get(at)
            .is_some_and(|f| f.name == old.name)
        {
            return Some(index);
        }
        // the moved index-embedded name: the content judge, exactly one
        // match — zero or several answer None (the never-guessing refusal)
        if !old.embeds_table_index() {
            return None;
        }
        current.content_unique(old.content_hash())
    }

    /// Resolves a pool FuncRef for the CALL/CALLT dispatch: the per-site
    /// inline cache first — the entry keyed on the caller function's
    /// identity and pc, validated against the registry's install
    /// generation — then the full lookup as the fallback and the refill.
    /// The full path is the same resolution discipline as ever: uop
    /// modules first (through the target's current slot, a retiring
    /// caller's own-module index translating through the identity its row
    /// records), then the native tables (a uop module installed under a
    /// native table's name shadows the natives); a stamp mismatch after a
    /// swap lands here, so callee resolution always follows the current
    /// slot [H6 item 5]. Traps are
    /// never cached — every failure comes from the
    /// full path (a refused layout pair rides the entry as its cached
    /// refusal, re-raised at the call seam, never answered as a resolution
    /// failure). A uop fill also derives the site's layout facts against
    /// `caller_layout`, so the plan is re-derived exactly when the callee
    /// is.
    fn resolve_site(
        &mut self,
        pool: &[Const],
        caller: CallerAt,
        caller_layout: &FrameLayout,
        pc: u32,
        kidx: ConstIdx,
        nargs: u8,
    ) -> Result<Resolved, Trap> {
        let at = pc as usize & (CALL_CACHE_SLOTS - 1);
        let stamp = self.registry.generation();
        let hit = self.call_cache[at]
            .as_ref()
            .is_some_and(|e| e.caller == caller.func && e.pc == pc && e.stamp == stamp);
        if !hit {
            let entry =
                self.resolve_site_full(pool, caller, caller_layout, pc, kidx, nargs, stamp)?;
            self.call_cache[at] = Some(entry);
        }
        Ok(
            match &self.call_cache[at]
                .as_ref()
                .expect("the slot was just filled")
                .callee
            {
                CachedCallee::Uop {
                    version,
                    func,
                    facts,
                } => Resolved::Uop {
                    version: *version,
                    func: *func,
                    facts: facts.clone(),
                },
                CachedCallee::Native { table, index } => Resolved::Native {
                    table: Arc::clone(table),
                    index: *index,
                },
                CachedCallee::Bound { module, index, ret } => Resolved::Bound {
                    module: Arc::clone(module),
                    index: *index,
                    ret: *ret,
                },
            },
        )
    }

    /// The full, uncached call-site resolution the cache falls back to:
    /// pool kind check, the target module's current slot, the entry-word
    /// and arity checks. The pool belongs to the CALLER's module version:
    /// instruction stream and pool are version-stable, callee resolution
    /// follows the target's current slot [H6 item 5] — the redirect a
    /// parked old frame's next direct call picks up, with a retiring
    /// caller's own-module index translated through the identity its row
    /// records ([`Vm::uop_site_index`]). The pool operand's
    /// index and FuncRef kind were proved at install (module.rs's body
    /// validation), so the fetch indexes directly and the kind arm is the
    /// corrupted-pool tripwire.
    fn resolve_site_full(
        &self,
        pool: &[Const],
        caller: CallerAt,
        caller_layout: &FrameLayout,
        pc: u32,
        kidx: ConstIdx,
        nargs: u8,
        stamp: u64,
    ) -> Result<CallSite, Trap> {
        let found = &pool[usize::from(kidx.0)];
        let Const::FuncRef(entry) = found else {
            return Err(Trap::PoolKind {
                kidx: kidx.0,
                expected: ConstKind::FuncRef,
                found: found.kind(),
            });
        };
        // a bound callee resolves through the registry's bound tier alone
        // (H5): never a uop module, never the native tables — the
        // artifact's symbol table under its manifest name
        if let ConstFuncRef::Bound { module, index, ret } = entry {
            let bound = self
                .registry
                .bound_table(module)
                .filter(|table| usize::try_from(*index).is_ok_and(|at| at < table.len()))
                .ok_or(Trap::UnknownCallee {
                    module: module.clone(),
                    func: *index,
                })?;
            return Ok(CallSite {
                caller: caller.func,
                pc,
                stamp,
                callee: CachedCallee::Bound {
                    module: Arc::clone(bound),
                    index: *index,
                    ret: *ret,
                },
            });
        }
        // uop first: an install-resolved id addresses the dep's module
        // directly (its name sits in the registry's id vector), a name
        // through the module table — so a uop module installed under a
        // native table's name still shadows the natives. A retiring
        // caller's own-module index translates first; a translation that
        // cannot be proven traps rather than routing the verbatim index.
        let uop_target =
            match entry {
                ConstFuncRef::Uop { module, func } => {
                    let name = self
                        .registry
                        .module_name(*module)
                        .ok_or(Trap::UnknownModule { module: *module })?;
                    let func = Self::uop_site_index(&self.registry, caller.row, name, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: name.to_owned(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(name, func)
                },
                ConstFuncRef::Named { module, func } => {
                    let func = Self::uop_site_index(&self.registry, caller.row, module, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: module.clone(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(module, func)
                },
                // the bound tier answered ahead of this match (H5)
                ConstFuncRef::Bound { .. } => None,
            };
        if let Some((version, func_id)) = uop_target {
            let target = &self.registry.pinned(version).functions()[func_id as usize];
            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
            if usize::from(nargs) != target.layout.params_len() {
                return Err(Trap::ArityMismatch {
                    expected: target.layout.params_len(),
                    given: usize::from(nargs),
                });
            }
            let facts = SiteFacts::derive(caller_layout, &target.layout);
            return Ok(CallSite {
                caller: caller.func,
                pc,
                stamp,
                callee: CachedCallee::Uop {
                    version,
                    func: func_id,
                    facts,
                },
            });
        }
        // the native fallback belongs to name-keyed callees only: a
        // dep-position callee addressed a uop module at install, so its
        // miss here is a corrupted pool, never a native
        let (module, func) = match entry {
            ConstFuncRef::Uop { module, func } => {
                let name = self
                    .registry
                    .module_name(*module)
                    .ok_or(Trap::UnknownModule { module: *module })?;
                return Err(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: *func,
                });
            },
            ConstFuncRef::Named { module, func } => (module.as_str(), *func),
            // the bound tier answered ahead of this match (H5)
            ConstFuncRef::Bound { .. } => {
                unreachable!("the bound tier resolved ahead of the native fallback")
            },
        };
        let table = self
            .registry
            .native_table(module)
            .filter(|t| usize::try_from(func).is_ok_and(|i| i < t.len()))
            .ok_or(Trap::UnknownCallee {
                module: module.to_owned(),
                func,
            })?;
        let native = &table[func as usize];
        // a variadic native takes at least its named parameters (the
        // staged tail is one element's whole word span, its width the
        // call site's static fact); every other native takes exactly them
        let arity_ok = if native.variadic {
            usize::from(nargs) >= native.params.len()
        } else {
            usize::from(nargs) == native.params.len()
        };
        if !arity_ok {
            return Err(Trap::ArityMismatch {
                expected: native.params.len(),
                given: usize::from(nargs),
            });
        }
        Ok(CallSite {
            caller: caller.func,
            pc,
            stamp,
            callee: CachedCallee::Native {
                table: Arc::clone(table),
                index: func,
            },
        })
    }

    /// Resolves a dispatch site's (CALLG/CALLTG) pool FuncRef — fetched and
    /// matched by the calling arm — against the
    /// uop module tables: the same CALL-site inline cache bank CALL/CALLT
    /// key — the caller function's identity and pc, validated against the
    /// registry's install generation — so a dispatch tail site keys its
    /// callee exactly like every other call site, with the full lookup as
    /// the fallback and the refill. The dispatch arity rule differs from
    /// a plain call: the callee's parameters include the trailing
    /// evidence parameters the body claims, so `nargs` names only the
    /// real arguments and the full path proves `nargs <= params_len`
    /// (the staged evidence words' kinds are the arm's own check, on
    /// both paths). The sentinel evidence FuncRef never reaches here
    /// (its target is runtime data, never cacheable), and a generic
    /// builtin's FuncRef answers [`DispatchResolved::NotUop`] uncached —
    /// the static generic table needs no cache. Traps are never cached:
    /// every failure comes from the full path. The caller hands in the
    /// pool entry it already fetched and matched (the arm's own refusal
    /// path raised the pool traps), so the resolver never re-classifies
    /// the word, and a uop fill derives the site's layout facts against
    /// `caller_layout` — the plan is re-derived exactly when the callee
    /// is.
    fn resolve_dispatch_site(
        &mut self,
        entry: &ConstFuncRef,
        caller: usize,
        caller_row: VersionIdx,
        caller_layout: &FrameLayout,
        pc: u32,
        nargs: u8,
    ) -> Result<DispatchResolved, Trap> {
        let at = pc as usize & (CALL_CACHE_SLOTS - 1);
        let stamp = self.registry.generation();
        let hit = self.call_cache[at].as_ref().is_some_and(|e| {
            e.caller == caller
                && e.pc == pc
                && e.stamp == stamp
                && matches!(e.callee, CachedCallee::Uop { .. })
        });
        if !hit {
            // an install-resolved id addresses its dep's module directly
            // (the name sits in the registry's id vector), a name through
            // the module table; a retiring caller's own-module index
            // translates through the identity its row records
            let target = match entry {
                ConstFuncRef::Uop { module, func } => {
                    let name = self
                        .registry
                        .module_name(*module)
                        .ok_or(Trap::UnknownModule { module: *module })?;
                    let func = Self::uop_site_index(&self.registry, caller_row, name, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: name.to_owned(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(name, func)
                },
                ConstFuncRef::Named { module, func } => {
                    let func = Self::uop_site_index(&self.registry, caller_row, module, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: module.clone(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(module, func)
                },
                // a bound C symbol never rides the generic dispatch (the
                // lowering refuses generic bound externs), so the honest
                // refusal names the callee rather than routing a foreign
                // address into the uop tables (H5)
                ConstFuncRef::Bound { module, index, .. } => {
                    return Err(Trap::UnknownCallee {
                        module: module.clone(),
                        func: *index,
                    });
                },
            };
            let Some((version, func_id)) = target else {
                return Ok(DispatchResolved::NotUop);
            };
            let target = &self.registry.pinned(version).functions()[func_id as usize];
            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
            if usize::from(nargs) > target.layout.params_len() {
                return Err(Trap::ArityMismatch {
                    expected: target.layout.params_len(),
                    given: usize::from(nargs),
                });
            }
            let facts = SiteFacts::derive(caller_layout, &target.layout);
            self.call_cache[at] = Some(CallSite {
                caller,
                pc,
                stamp,
                callee: CachedCallee::Uop {
                    version,
                    func: func_id,
                    facts: facts.clone(),
                },
            });
            return Ok(DispatchResolved::Uop {
                version,
                func: func_id,
                facts,
            });
        }
        match &self.call_cache[at]
            .as_ref()
            .expect("the slot was just validated")
            .callee
        {
            CachedCallee::Uop {
                version,
                func,
                facts,
            } => Ok(DispatchResolved::Uop {
                version: *version,
                func: *func,
                facts: facts.clone(),
            }),
            // unreachable by the hit predicate; answered defensively so a
            // corrupted bank degrades to the generic path's honest trap
            CachedCallee::Native { .. } | CachedCallee::Bound { .. } => {
                Ok(DispatchResolved::NotUop)
            },
        }
    }

    /// KSTR over the pool-const table: the cache remembers the (pool,
    /// fiber, index) → object-id mapping from the first load — each heap
    /// interns its own copy of a constant, so a row is only ever served
    /// against the fiber whose store minted it — and a hit serves the id
    /// only after validating that it still names a live string
    /// with the pool constant's exact content — no claim is held, so a
    /// program dropping its last reference still frees the object at the
    /// same moment as without the cache (a dead or recycled id simply
    /// falls back to the full intern). Every claiming load, first or
    /// cached, acquires one count for the destination slot, exactly the
    /// discipline a fresh intern follows; a borrow load (KSTR_BORROW,
    /// the placement pass's proof that the word's count never moves)
    /// claims nothing and the intern map pins the canonical copy. The
    /// row key is a hint, never an authority: a dead pool's row can meet
    /// a DIFFERENT pool at the reused address (wrong length, wrong kinds),
    /// so the hit path validates kind as well as content and any mismatch
    /// degrades to the miss path — the kind trap stays exactly where an
    /// uncached load raises it. `slot` is the executing frame's
    /// resolved row: the first load of a frame resolves it, every later
    /// load in that frame reads it, and a row reclaimed or shifted under
    /// the frame re-resolves through the key index once.
    fn kstr_load(
        &mut self,
        pool: &[Const],
        kidx: ConstIdx,
        claim: bool,
        slot: &mut Option<KstrRowSlot>,
    ) -> Result<u32, Trap> {
        let idx = usize::from(kidx.0);
        // install proved the operand's pool index in range and its kind Str
        // (module.rs's body validation); the fetch indexes directly and the
        // kind arm is the corrupted-pool tripwire
        let text = match &pool[idx] {
            Const::Str(text) => text.as_str(),
            found => {
                return Err(Trap::PoolKind {
                    kidx: kidx.0,
                    expected: ConstKind::Str,
                    found: found.kind(),
                });
            },
        };
        let key: KstrRowKey = (pool.as_ptr() as usize, self.current.0);
        let cached = (*slot)
            .and_then(|at| self.kstr_cache.row(at, key))
            .and_then(|row| row.ids.get(idx))
            .copied()
            .flatten();
        if let Some(id) = cached
            && self.objects.is_live_str_with(id, text)
        {
            if claim {
                self.objects.inc(id)?;
            }
            return Ok(id);
        }
        let id = if claim { self.objects.intern(text) } else { self.objects.intern_borrow(text) };
        let (at, row) = self.kstr_cache.open(key, pool.len());
        *slot = Some(at);
        row.ids[idx] = Some(id);
        Ok(id)
    }

    /// The dispatcher: decode-on-dispatch (see the module docs), then a
    /// dense `match` over the uop [BL-3 note: computed goto is not available
    /// on stable Rust]. A drive ends when the current fiber completes
    /// ([`DriveOutcome::Died`]) or parks with the host as resumer
    /// ([`DriveOutcome::Yielded`]); switches between fibers happen inside.
    /// `boundary` is the call-chain depth of a nested reentrant drive's
    /// synthetic reentry frame ([`Vm::call_hosted`]): a host-level drive
    /// passes `usize::MAX` and never sees [`DriveOutcome::Frame`], and a
    /// nested drive answers it when its reentry frame returns.
    /// `FUELED` selects the budgeted instantiation ([`Vm::run_with_budget`]
    /// alone drives it): `true` charges `fuel` one step per decoded
    /// instruction and answers [`Trap::OutOfFuel`] at zero; `false` compiles
    /// the charge away entirely, keeping the unbounded drives' dispatch
    /// cost (the pin table's contract) untouched.
    // One flat match over the whole uop set is the point of a dispatch loop
    // [BL-3]; splitting arms into helpers would fragment it and add a call
    // per op in debug builds. The bias word re-reads u16 as i16 (the
    // format's documented flip-is-subtract), and the call arms bind the
    // caller/callee pair the site vocabulary names.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::similar_names)]
    fn execute<const FUELED: bool>(
        &mut self,
        mut active: Active,
        mut pc: CodePc,
        boundary: usize,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        // The frame context (body, code, slots, pool, fp) is derived once
        // per frame transfer, not per instruction: the executing frame and
        // its fp change only at the transfer arms (CALL/CALLT/CALLS, the
        // RET/RETSPAN/HALT resume paths, a control-native switch, the
        // cancel switch), and every other instruction runs against the
        // same immutable body — the frame-context cache. A transfer arm
        // assigns `active`/`pc` and `continue 'transfer`; the pc bounds
        // check stays per instruction.
        'transfer: loop {
            // the frame context reads the version table through the
            // address-stable pointer its row pins, so the borrows below
            // hold nothing on the VM while the arms call into it whole
            let f = &self.registry.pinned(active.version).functions()[active.func as usize];
            let code = &f.code;
            let slots = f.layout.slots();
            let frame_slots = slots.len();
            let body_len = f.layout.body_len();
            let pool = &f.pool;
            let fp = self.callinfo[self.callinfo.len() - 1].fp;
            // The frame's KSTR intern row: resolved by the frame's first
            // literal load, read directly by every later one. A row that
            // reclaim drops or shifts revalidates against its own key.
            let mut kstr_slot: Option<KstrRowSlot> = None;
            loop {
                // the budgeted drive's step charge: the constant folds
                // the whole block away on the unbounded instantiation,
                // so only `run_with_budget`'s drive pays it
                if FUELED {
                    match self.fuel {
                        0 => return Err((Trap::OutOfFuel, pc)),
                        _ => self.fuel -= 1,
                    }
                }
                let idx = pc.0 as usize;
                if idx >= code.len() {
                    let e = Trap::InvalidPc {
                        pc: pc.0,
                        code_len: code_len_u32(code.len()),
                    };
                    return Err((e, pc));
                }
                macro_rules! jump {
                    ($target:expr) => {{
                        let t = $target;
                        if t as usize >= code.len() {
                            let e = Trap::InvalidPc {
                                pc: t,
                                code_len: code_len_u32(code.len()),
                            };
                            return Err((e, pc));
                        }
                        // the preemption point [E2, E5]: a taken backward branch
                        // (a loop back-edge, self or below) is where the running
                        // fiber observes its nursery's cancel flag. Never
                        // mid-instruction: a linear consume in flight is done.
                        // Inside a teardown hook drive the observation refuses
                        // instead of cutting: the cut would kill the fiber whose
                        // death walk is driving the hook (a teardown hook must
                        // not re-enter the nursery)
                        if t <= pc.0 && self.cancel_requested() {
                            if self.teardown_drive {
                                return Err((Trap::ReentrancyRefused, pc));
                            }
                            match self.cancel_current() {
                                Some((a, at)) => {
                                    active = a;
                                    pc = at;
                                    continue 'transfer;
                                },
                                None => {
                                    return Ok(DriveOutcome::Died(FrameAnswer::one(Value::Tag(
                                        PLATFORM_CANCELLED,
                                    ))))
                                },
                            }
                        }
                        // the host's suspension request [the dev host's
                        // capability face]: the requested fiber parks at the
                        // same observation point a cancel uses, and the drive
                        // ends handing the host its continuation. The resume
                        // point is the branch itself: the frame is frozen in
                        // between, so the resumed drive re-takes the branch
                        // and decides the same. The parked branch's own
                        // re-take is the resume — the skip marker — so a
                        // resumed drive parks at its NEXT backward branch,
                        // one loop iteration per drive. Never inside a
                        // hosted reentry frame — a Rust-side native loop
                        // cannot suspend, so the request waits for the
                        // hosting drive to return — and never under a
                        // teardown drive, which must finish its walk.
                        if t <= pc.0
                            && self.suspend_request == Some(self.current)
                            && !self.teardown_drive
                            && !self.reentry_live(boundary)
                        {
                            if self.suspend_skip.take() == Some((self.current, pc)) {
                                // the resumed branch's own re-take
                            } else {
                                self.suspend_request = None;
                                let fiber = &mut self.pool[self.current.index()];
                                fiber.resume = Some(ResumePoint {
                                    version: active.version,
                                    func: active.func,
                                    pc: CodePc(pc.0),
                                });
                                fiber.state = FiberState::Preempted;
                                return Ok(DriveOutcome::Yielded(Value::Tag(PLATFORM_NONE)));
                            }
                        }
                        pc = CodePc(t);
                    }};
                }
                macro_rules! stack_state {
                    () => {
                        StackState {
                            stack: &mut self.stack,
                            callinfo: &mut self.callinfo,
                            versions: &self.registry,
                            limit: self.limit,
                            max_len: &mut self.pool[self.current.index()].max_len,
                        }
                    };
                }
                // the inline canonicity doors every fast arm shares: each
                // names the same error the decoder would, without the
                // cross-crate call per instruction
                macro_rules! bad_encoding {
                    ($cause:expr) => {
                        return Err((
                            Trap::BadEncoding {
                                pc: pc.0,
                                cause: $cause,
                            },
                            pc,
                        ))
                    };
                }
                macro_rules! reserved {
                    () => {
                        bad_encoding!(DecodeError::ReservedField)
                    };
                }
                // the biased jump offset of the uop format, branch-free
                let word = code[idx];
                let [byte, fa, fb, fc] = word.to_be_bytes();
                let fd = u16::from_be_bytes([fb, fc]);
                // the raw jump target: a fast arm's bias math; a target
                // outside the u32 index range is the same JumpOutOfRange
                // the decoder reports
                macro_rules! jump_target {
                    () => {
                        match u32::try_from(i64::from(pc.0) + 1 + i64::from((fd ^ BIAS_J) as i16)) {
                            Ok(target) => target,
                            Err(_) => {
                                bad_encoding!(DecodeError::JumpOutOfRange {
                                    pc: pc.0,
                                    field: fd
                                })
                            },
                        }
                    };
                }
                // the fused compare-branch tail: pc first bumped onto the
                // branch's own word, then the JMPIF_NOT arm's exact body —
                // the biased target from the peeked word's field (the same
                // math and JumpOutOfRange attribution `jump_target!` gives
                // the decoded word at that pc), the decision on the
                // computed bool (the branch reads the dst the compare just
                // wrote, so the read cannot fail and the value is in
                // hand), the taken branch through the `jump!` macro body
                // verbatim, and the not-taken fallthrough at the branch's
                // next word — the pair consumed two words
                macro_rules! fused_jmpif_not {
                    ($field:expr, $cond:expr) => {{
                        pc.0 += 1;
                        let Ok(target) = u32::try_from(
                            i64::from(pc.0) + 1 + i64::from(($field ^ BIAS_J) as i16),
                        ) else {
                            bad_encoding!(DecodeError::JumpOutOfRange {
                                pc: pc.0,
                                field: $field
                            })
                        };
                        if $cond {
                            pc.0 += 1;
                        } else {
                            jump!(target);
                        }
                    }};
                }
                // the fused tag-branch tail: the JMPIF polarity of
                // `fused_jmpif_not!` — taken on the held bool — over the
                // same pc-first bump onto the branch's own word
                macro_rules! fused_jmpif {
                    ($field:expr, $cond:expr) => {{
                        pc.0 += 1;
                        let Ok(target) = u32::try_from(
                            i64::from(pc.0) + 1 + i64::from(($field ^ BIAS_J) as i16),
                        ) else {
                            bad_encoding!(DecodeError::JumpOutOfRange {
                                pc: pc.0,
                                field: $field
                            })
                        };
                        if $cond {
                            jump!(target);
                        } else {
                            pc.0 += 1;
                        }
                    }};
                }
                // one dispatch table over the raw opcode byte: the loader
                // validated every word, and decode stays the canonicity
                // authority for whatever the fast arms refuse, so the hot
                // loop extracts each arm's operand fields inline and jumps
                // through the byte table — adding a uop adds one arm and
                // never re-lowers its neighbors. Field names follow the uop
                // format: fa/fb/fc the register bytes, fd the wide field.
                match byte {
                    OP_FUNCF => {
                        if fa != 0 {
                            reserved!();
                        }
                        if fd > MAX_FRAME_SLOTS {
                            bad_encoding!(DecodeError::FrameSlotsExceedCap { frame_slots: fd });
                        }
                        let n = fd;
                        if usize::from(n) != frame_slots {
                            let e = Trap::FrameSlotsMismatch {
                                funcef: n,
                                layout: frame_slots,
                            };
                            return Err((e, pc));
                        }
                        pc.0 += 1;
                    },
                    OP_KIMM => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        // install proved the operand's pool index in range and
                        // its kind I64 (module.rs's body validation); the fetch
                        // indexes directly and the kind arm is the
                        // corrupted-pool tripwire
                        let v = match &pool[usize::from(kidx.0)] {
                            Const::I64(v) => *v,
                            found => {
                                let e = Trap::PoolKind {
                                    kidx: kidx.0,
                                    expected: ConstKind::I64,
                                    found: found.kind(),
                                };
                                return Err((e, pc));
                            },
                        };
                        if let Some((op, adst, lhs, rhs)) = fused_i64_arith(code, idx, dst) {
                            // the load's own errors are spent; the fused
                            // arith's errors attribute to its own pc
                            write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                                .map_err(|e| (e, pc))?;
                            let at = CodePc(pc.0 + 1);
                            let (x, y) = (
                                read_i64(&self.stack, fp, lhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                                read_i64(&self.stack, fp, rhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                            );
                            let result = match op {
                                OP_ADD_I64 => x.checked_add(y).ok_or(Trap::IntegerOverflow),
                                OP_SUB_I64 => x.checked_sub(y).ok_or(Trap::IntegerOverflow),
                                OP_MUL_I64 => x.checked_mul(y).ok_or(Trap::IntegerOverflow),
                                // the fourth fused arith, MODI64: the same
                                // zero-divide / overflow split as DIVI64,
                                // spelled inline so `int_mod` keeps its one
                                // call site (a second site leaves the helper
                                // outlined, and the per-mod call and result
                                // move show up on the reentry kernels)
                                _ => {
                                    if y == 0 {
                                        Err(Trap::DivideByZero)
                                    } else {
                                        x.checked_rem(y).ok_or(Trap::IntegerOverflow)
                                    }
                                },
                            }
                            .map_err(|e| (e, at))?;
                            write_slot(
                                &mut self.stack,
                                fp,
                                adst,
                                slots,
                                body_len,
                                Value::I64(result),
                            )
                            .map_err(|e| (e, at))?;
                            pc.0 += 2;
                        } else {
                            write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                                .map_err(|e| (e, pc))?;
                            pc.0 += 1;
                        }
                    },
                    OP_KCONST => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        // install proved the operand's pool index in range and
                        // its kind among F64/Bool/FuncRef (module.rs's body
                        // validation); the fetch indexes directly and the kind
                        // arm is the corrupted-pool tripwire
                        let v = match &pool[usize::from(kidx.0)] {
                            Const::F64(v) => Value::F64(*v),
                            Const::Bool(v) => Value::Bool(*v),
                            // A fn word (F2): an install-resolved id mints
                            // the word directly — the dsmod install already
                            // walked position → dep name → module id, and the
                            // mint carries exactly that one vector index.
                            // A retiring frame's own-module ref carries its
                            // row's index space, so the mint translates it
                            // into the current table (below): the minted
                            // value enters current-index-valid exactly like
                            // a re-keyed staged word. A name-keyed callee
                            // resolves at issue time: a directly installed
                            // module through the registry's id space, a
                            // native surface refusing as no callable word
                            // (natives are not values). The loader validated
                            // the reference at install, so an unresolved
                            // name is a corrupted pool, never a program
                            // fact.
                            Const::FuncRef(callee) => match callee {
                                ConstFuncRef::Uop { module, func } => StackState::minted_fn_word(
                                    &self.registry,
                                    active.version,
                                    FuncRef::new(*module, *func),
                                )
                                .map_err(|e| (e, pc))?,
                                ConstFuncRef::Named { module, func } => {
                                    let module_id = self.registry.module_id(module).ok_or((
                                        Trap::UnknownCallee {
                                            module: module.clone(),
                                            func: *func,
                                        },
                                        pc,
                                    ))?;
                                    StackState::minted_fn_word(
                                        &self.registry,
                                        active.version,
                                        FuncRef::new(module_id, *func),
                                    )
                                    .map_err(|e| (e, pc))?
                                },
                                // a bound C symbol is not a callable word
                                // (H5): the lowering routes every bound
                                // extern through its call sites, so a
                                // value-site use never lowers to this pool
                                // kind — the honest refusal, the same
                                // discipline a native surface takes
                                ConstFuncRef::Bound { module, index, .. } => {
                                    return Err((
                                        Trap::UnknownCallee {
                                            module: module.clone(),
                                            func: *index,
                                        },
                                        pc,
                                    ));
                                },
                            },
                            found => {
                                let e = Trap::PoolKind {
                                    kidx: kidx.0,
                                    expected: ConstKind::F64,
                                    found: found.kind(),
                                };
                                return Err((e, pc));
                            },
                        };
                        if let Some((op, adst, lhs, rhs)) = fused_f64_arith(code, idx, dst) {
                            // the load's own errors are spent; the fused
                            // arith's errors attribute to its own pc
                            write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                                .map_err(|e| (e, pc))?;
                            let at = CodePc(pc.0 + 1);
                            let (x, y) = (
                                read_f64(&self.stack, fp, lhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                                read_f64(&self.stack, fp, rhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                            );
                            let result = if op == OP_ADD_F64 { x + y } else { x * y };
                            write_slot(
                                &mut self.stack,
                                fp,
                                adst,
                                slots,
                                body_len,
                                Value::F64(result),
                            )
                            .map_err(|e| (e, at))?;
                            pc.0 += 2;
                        } else {
                            write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                                .map_err(|e| (e, pc))?;
                            pc.0 += 1;
                        }
                    },
                    OP_KSTR => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        let obj = self
                            .kstr_load(pool, kidx, true, &mut kstr_slot)
                            .map_err(|e| (e, pc))?;
                        let word = RefId::new(self.current, obj);
                        // the load's acquire is the loading frame's claim
                        self.claims[self.current.index()].acquire(word);
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(word))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_KSTR_BORROW => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        // the placement pass proved this word's count never
                        // moves inside its frame, so the load borrows the
                        // intern map's pinned canonical copy: no store
                        // count, no claim ledger row, no terminal dec
                        let obj = self
                            .kstr_load(pool, kidx, false, &mut kstr_slot)
                            .map_err(|e| (e, pc))?;
                        let word = RefId::new(self.current, obj);
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(word))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_ADD_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let Some(sum) = x.checked_add(y) else {
                            return Err((Trap::IntegerOverflow, pc));
                        };
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(sum))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_SUB_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let Some(diff) = x.checked_sub(y) else {
                            return Err((Trap::IntegerOverflow, pc));
                        };
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(diff))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MUL_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let Some(prod) = x.checked_mul(y) else {
                            return Err((Trap::IntegerOverflow, pc));
                        };
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(prod))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_DIV_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let q = int_div(x, y).map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(q))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MOD_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let r = int_mod(x, y).map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(r))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the wrapping trio never traps (B7's wrap set), so
                    // these are the only IntArith arms without a cold
                    // overflow edge
                    OP_WRAPADD_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(x.wrapping_add(y)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_WRAPSUB_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(x.wrapping_sub(y)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_WRAPMUL_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(x.wrapping_mul(y)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the shift and bitwise quintet stays out of the
                    // hot loop's match shape: one range arm hands the
                    // word to [`shift_bitwise_word`], so the five extra
                    // opcode arms never re-lower the dispatch's
                    // neighbors (the pin bake caught them costing ~1% Ir
                    // on every kernel)
                    OP_SHL_I64..=OP_BITOR_I64 => {
                        shift_bitwise_word(&mut self.stack, byte, fa, fb, fc, fp, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_ADD_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x + y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_SUB_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x - y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MUL_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x * y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_DIV_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        // IEEE division: a zero divisor produces inf, not a trap
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x / y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_EQ_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x == y;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif_not(code, idx, dst) {
                            fused_jmpif_not!(field, v);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_NEQ_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x != y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LT_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x < y;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif_not(code, idx, dst) {
                            fused_jmpif_not!(field, v);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_GE_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x >= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LE_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x <= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_GT_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x > y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_EQ_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        // bit-structural equality: reflexive, total, dict-safe
                        // [B9]
                        let v = x.to_bits() == y.to_bits();
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_NEQ_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x.to_bits() != y.to_bits();
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LT_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        // partial order: every NaN comparison answers false
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x < y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_GE_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x >= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LE_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x <= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_GT_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x > y;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif_not(code, idx, dst) {
                            fused_jmpif_not!(field, v);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_EQ_BOOL => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_bool(&self.stack, fp, lhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                            read_bool(&self.stack, fp, rhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x == y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_NEQ_BOOL => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_bool(&self.stack, fp, lhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                            read_bool(&self.stack, fp, rhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x != y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the string data ops take operands of both string
                    // domains: a heap string word or an arena temporary
                    OP_EQ_STR => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let eq = str_eq_content(self, fp, lhs, rhs, slots, body_len, pc)?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(eq))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_NEQ_STR => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let eq = str_eq_content(self, fp, lhs, rhs, slots, body_len, pc)?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(!eq))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_STRCAT => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_typed(&self.stack, fp, lhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                            read_typed(&self.stack, fp, rhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                        );
                        // a heap-to-heap pair keeps the store's own
                        // fast path; any arena operand is a mixed-domain
                        // concat whose contents build the counted result
                        // directly on the live context store (the content
                        // borrows read pool and objects immutably, the
                        // alloc and the claim touch objects and claims
                        // alone, and the fields are disjoint)
                        let obj = if let (Value::Ref(xw), Value::Ref(yw)) = (x, y) {
                            self.heaps().concat(xw, yw).map_err(|e| (e, pc))?
                        } else {
                            let xb = str_content(
                                &self.pool,
                                &self.objects,
                                self.ctx_live,
                                self.current,
                                x,
                            )
                            .map_err(|e| (e, pc))?;
                            let yb = str_content(
                                &self.pool,
                                &self.objects,
                                self.ctx_live,
                                self.current,
                                y,
                            )
                            .map_err(|e| (e, pc))?;
                            let text = concat_text(xb, yb).map_err(|e| (e, pc))?;
                            let id = self.objects.alloc_str(text);
                            self.claims[self.current.index()].acquire(RefId::new(self.current, id));
                            id
                        };
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Ref(RefId::new(self.current, obj)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the placement pass proved this result a designated
                    // temporary: its bytes serve from the running fiber's
                    // frame arena and its word carries no count, so no
                    // acquire and no claim row exist; the first
                    // allocation in a frame also serves that frame's
                    // entry mark (the return-side close restores it)
                    OP_STRCAT_ARENA => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_typed(&self.stack, fp, lhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                            read_typed(&self.stack, fp, rhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                        );
                        let xb =
                            str_content(&self.pool, &self.objects, self.ctx_live, self.current, x)
                                .map_err(|e| (e, pc))?;
                        let yb =
                            str_content(&self.pool, &self.objects, self.ctx_live, self.current, y)
                                .map_err(|e| (e, pc))?;
                        let total = xb.len().checked_add(yb.len()).ok_or((
                            Trap::ArenaExhausted {
                                field: ArenaPackError::Length,
                            },
                            pc,
                        ))?;
                        if total >= ARENA_LEN_LIMIT {
                            return Err((
                                Trap::ArenaExhausted {
                                    field: ArenaPackError::Length,
                                },
                                pc,
                            ));
                        }
                        let Word::Meta(meta) = &mut self.stack[fp] else {
                            return Err((Trap::MetaWordCorrupt { fp }, pc));
                        };
                        if !meta.arena_active() {
                            meta.mark_arena_active();
                            // the mark precedes this frame's first
                            // allocation, so it is the frame's exact
                            // entry position
                            self.pool[self.current.index()].arena.push_frame_mark();
                        }
                        let at = self.pool[self.current.index()].arena.alloc_concat(xb, yb);
                        let handle = ArenaStr::pack(at.chunk(), at.offset(), total)
                            .map_err(|field| (Trap::ArenaExhausted { field }, pc))?;
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Arena(handle),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CLOSURE_NEW => {
                        let dst = fa;
                        let func = fb;
                        let n = fc;
                        // CLOSURE_NEW (F2): the header is the scalar FuncRef
                        // word at `func` (KCONST-minted), the capture block the
                        // n words staged at overlay positions 0..n. The object
                        // acquires one count per Ref capture as it adopts the
                        // block — the staged sources keep their own counts — and
                        // the fresh Ref lands in dst with birth count one.
                        let funcref = match read_value(&self.stack, fp, func, slots, body_len)
                            .map_err(|e| (e, pc))?
                        {
                            Value::Fn(funcref) => funcref,
                            v => {
                                return Err((
                                    Trap::SlotTypeMismatch {
                                        slot: func,
                                        expected: SlotType::Fn,
                                        actual: v.slot_type(),
                                    },
                                    pc,
                                ))
                            },
                        };
                        let overlay_base = slots.len() - f.layout.arg_area_len();
                        let mut captures = Vec::with_capacity(usize::from(n));
                        for index in 0..usize::from(n) {
                            let slot = u8::try_from(overlay_base + index)
                                .expect("overlay words fit u8; frames cap at 256 slots");
                            captures.push(
                                read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?,
                            );
                        }
                        for word in &captures {
                            if let Value::Ref(obj) = *word {
                                // the object adopts the block: one edge
                                // count per Ref capture, no frame claim
                                self.inc_edge(obj).map_err(|e| (e, pc))?;
                            }
                        }
                        let obj = self.objects.alloc_closure(funcref, captures);
                        // the fresh closure's birth count is the caller's
                        // frame claim
                        let closure_word = RefId::new(self.current, obj);
                        self.claims[self.current.index()].acquire(closure_word);
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Ref(closure_word),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_TAG_TEST => {
                        let dst = fa;
                        let src = fb;
                        let tag = fc;
                        let v =
                            read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        let held = v == tag;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(held))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif(code, idx, dst) {
                            fused_jmpif!(field, held);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_TAG_TEST_NOT => {
                        let dst = fa;
                        let src = fb;
                        let tag = fc;
                        let v =
                            read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(v != tag),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_TAG_NEW => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let tag = fb;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Tag(tag))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CVT_I2F => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let src = fb;
                        let x =
                            read_i64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        // precision loss beyond 2^53 is the semantics of the
                        // i64-to-f64 conversion itself
                        #[allow(clippy::cast_precision_loss)]
                        let v = x as f64;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CVT_F2I => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let src = fb;
                        let x =
                            read_f64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        // trap on NaN and on magnitudes at or past 2^63; -2^63
                        // itself is exactly i64::MIN and converts [B7]
                        if x.is_nan() || x >= F64_I64_EDGE || x < -F64_I64_EDGE {
                            return Err((Trap::CvtOutOfRange, pc));
                        }
                        // in range by the check above, so the cast is exact
                        #[allow(clippy::cast_possible_truncation)]
                        let v = x as i64;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MOV => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let src = fb;
                        let v = read_value(&self.stack, fp, src, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_JMPIF => {
                        let cond = fa;
                        let target = jump_target!();
                        if read_bool(&self.stack, fp, cond, slots, body_len).map_err(|e| (e, pc))? {
                            jump!(target);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_JMPIF_NOT => {
                        let cond = fa;
                        let target = jump_target!();
                        if read_bool(&self.stack, fp, cond, slots, body_len).map_err(|e| (e, pc))? {
                            pc.0 += 1;
                        } else {
                            jump!(target);
                        }
                    },
                    OP_JMP => {
                        if fa != 0 {
                            reserved!();
                        }
                        let target = jump_target!();
                        jump!(target);
                    },
                    // the loop-head mark: the paired restore at the back
                    // edge rewinds here, so each iteration re-marks before
                    // the cond and body allocate
                    OP_ARENA_MARK => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let at = self.pool[self.current.index()].arena.mark();
                        let packed = ArenaStr::pack(at.chunk(), at.offset(), 0)
                            .map_err(|field| (Trap::ArenaExhausted { field }, pc))?;
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(packed.raw() as i64),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_ARENA_RESTORE => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        let src = fa;
                        let cursor =
                            read_i64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        let raw = u64::try_from(cursor).map_err(|_| {
                            (
                                Trap::ArenaHandle {
                                    class: ds_rt::ArenaMisuse::Range,
                                },
                                pc,
                            )
                        })?;
                        let (chunk, offset, _) = ArenaStr::unpack_raw(raw);
                        self.pool[self.current.index()]
                            .arena
                            .try_restore(ds_rt::Mark::at(chunk, offset))
                            .map_err(|class| (Trap::ArenaHandle { class }, pc))?;
                        pc.0 += 1;
                    },
                    OP_CALL => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        let caller = CallerAt {
                            func: std::ptr::from_ref(f) as usize,
                            row: active.version,
                        };
                        let resolved = self
                            .resolve_site(pool, caller, &f.layout, pc.0, callee, nargs)
                            .map_err(|e| (e, pc))?;
                        match resolved {
                            Resolved::Uop {
                                version,
                                func,
                                facts,
                            } => {
                                let mut st = stack_state!();
                                let t = st
                                    .call(version, func, &facts, &f.layout, fp, pc.0 + 1)
                                    .map_err(|e| (e, pc))?;
                                active = Active {
                                    version: t.version,
                                    func: t.func,
                                };
                                pc = CodePc(0);
                                continue 'transfer;
                            },
                            // A native call stays in the caller's frame: the
                            // staged words are read as arguments and the result
                            // overlays argument slot 0 exactly where a RET
                            // would put it (a fallible native's span overlays
                            // slots 0 and 1). The fiber-control natives are the
                            // exception: the dispatcher itself is the switch
                            // [E2] — their bodies never run from a CALL site.
                            Resolved::Native { table, index } => {
                                let nf = &table[index as usize];
                                check_native_call_site(&f.layout, nf).map_err(|e| (e, pc))?;
                                if let Some(control) = nf.control {
                                    match self
                                        .control_switch(
                                            control, &active, pc.0, fp, &f.layout, nargs,
                                        )
                                        .map_err(|e| (e, pc))?
                                    {
                                        SwitchOutcome::Entered(a, at) => {
                                            active = a;
                                            pc = at;
                                            continue 'transfer;
                                        },
                                        SwitchOutcome::Bounced => {
                                            pc.0 += 1;
                                        },
                                        SwitchOutcome::Host(event) => return Ok(event),
                                        SwitchOutcome::Interlock(req) => {
                                            return Ok(DriveOutcome::Interlock(req))
                                        },
                                    }
                                } else if let Some(channel) = nf.channel {
                                    match self
                                        .chan_switch(channel, &active, pc.0, fp, &f.layout, nargs)
                                        .map_err(|e| (e, pc))?
                                    {
                                        ChanOutcome::Answered => {
                                            pc.0 += 1;
                                        },
                                        ChanOutcome::Parked(event) => return Ok(event),
                                    }
                                } else {
                                    let mut heaps = Heaps::new(
                                        self.current,
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        self.ctx_live,
                                    );
                                    let result = invoke_native(
                                        nf,
                                        usize::from(nargs),
                                        &mut self.stack,
                                        &mut heaps,
                                        &mut self.out,
                                        &f.layout,
                                        fp,
                                    )
                                    .map_err(|e| (e, pc))?;
                                    // a world op's would-block parks the
                                    // calling fiber instead of blocking
                                    // the drive — the resume point is
                                    // the instruction after the call
                                    let result = match result {
                                        NativeResult::Park(wait) => {
                                            return self.park_world(
                                                &active,
                                                pc,
                                                ParkKind::World(wait),
                                            )
                                        },
                                        other => other,
                                    };
                                    overlay_native(result, &mut self.stack, &f.layout, fp)
                                        .map_err(|e| (e, pc))?;
                                    pc.0 += 1;
                                }
                            },
                            // A bound artifact's C symbol runs in the
                            // caller's frame like a native (H5): the staged
                            // words marshal per their own kinds and the
                            // scalar answer overlays argument slot 0. The
                            // call is blocking and synchronous — it pins the
                            // fiber for its whole duration, and a foreign
                            // abort kills the process (panic=abort, the VM
                            // never unwinding through a foreign frame).
                            Resolved::Bound { module, index, ret } => {
                                let value = invoke_bound(
                                    &self.pool,
                                    &self.objects,
                                    self.ctx_live,
                                    self.current,
                                    &module,
                                    index,
                                    ret,
                                    usize::from(nargs),
                                    &mut self.stack,
                                    &f.layout,
                                    fp,
                                )
                                .map_err(|e| (e, pc))?;
                                overlay_native(
                                    NativeResult::Word(value),
                                    &mut self.stack,
                                    &f.layout,
                                    fp,
                                )
                                .map_err(|e| (e, pc))?;
                                pc.0 += 1;
                            },
                        }
                    },
                    OP_CALLT => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        let caller = CallerAt {
                            func: std::ptr::from_ref(f) as usize,
                            row: active.version,
                        };
                        let resolved = self
                            .resolve_site(pool, caller, &f.layout, pc.0, callee, nargs)
                            .map_err(|e| (e, pc))?;
                        let result = match resolved {
                            Resolved::Uop {
                                version,
                                func,
                                facts,
                            } => {
                                self.arena_frame_close(fp);
                                let mut st = stack_state!();
                                let t = st
                                    .tail_call(version, func, &facts, &f.layout, fp)
                                    .map_err(|e| (e, pc))?;
                                active = Active {
                                    version: t.version,
                                    func: t.func,
                                };
                                pc = CodePc(0);
                                continue 'transfer;
                            },
                            // Tail-calling a native runs it, then returns its
                            // result from the current frame: the value the
                            // native produced is the tail position's value.
                            // A span result rides the reused frame back to
                            // the ORIGINAL caller: this frame's slot 0
                            // already aliases the caller's overlay word 0
                            // (the RET below writes the tag there), and
                            // overlay word 1 is the physical word fp+2. At
                            // the root frame only the head word survives,
                            // the same root rule RETSPAN returns by.
                            Resolved::Native { table, index } => {
                                let nf = &table[index as usize];
                                if let Some(caller_area) = self.caller_arg_area() {
                                    check_native_overlay(caller_area, nf).map_err(|e| (e, pc))?;
                                }
                                if let Some(channel) = nf.channel {
                                    // the channel step runs in place of
                                    // the body; a park pops the frame
                                    // exactly the net tail park would
                                    match self
                                        .chan_step(channel, fp, &f.layout, nargs)
                                        .map_err(|e| (e, pc))?
                                    {
                                        ChanStep::Answer(result) => result,
                                        ChanStep::Park => {
                                            return self.park_world_tail(
                                                &active,
                                                pc,
                                                boundary,
                                                ParkKind::Chan,
                                            )
                                        },
                                    }
                                } else {
                                    let mut heaps = Heaps::new(
                                        self.current,
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        self.ctx_live,
                                    );
                                    invoke_native(
                                        nf,
                                        usize::from(nargs),
                                        &mut self.stack,
                                        &mut heaps,
                                        &mut self.out,
                                        &f.layout,
                                        fp,
                                    )
                                    .map_err(|e| (e, pc))?
                                }
                            },
                            // A bound artifact's C symbol tail-calls in
                            // place (H5): the blocking call pins the fiber
                            // for its whole duration, and the one scalar
                            // word answers exactly a native word result's
                            // tail ride
                            Resolved::Bound { module, index, ret } => {
                                if let Some(caller_area) = self.caller_arg_area() {
                                    check_bound_overlay(caller_area).map_err(|e| (e, pc))?;
                                }
                                NativeResult::Word(
                                    invoke_bound(
                                        &self.pool,
                                        &self.objects,
                                        self.ctx_live,
                                        self.current,
                                        &module,
                                        index,
                                        ret,
                                        usize::from(nargs),
                                        &mut self.stack,
                                        &f.layout,
                                        fp,
                                    )
                                    .map_err(|e| (e, pc))?,
                                )
                            },
                        };
                        // a world op's would-block parks the calling
                        // fiber instead of blocking the drive — the
                        // frame pops as the ret below would, the wake's
                        // answer delivering at the pending return
                        let result = match result {
                            NativeResult::Park(wait) => {
                                return self.park_world_tail(
                                    &active,
                                    pc,
                                    boundary,
                                    ParkKind::World(wait),
                                )
                            },
                            other => other,
                        };
                        // the answer's whole word span rides a
                        // boundary return (the hosted seam's result
                        // channel); every other outcome keeps its
                        // prior words
                        let (value, answer) = match result {
                            NativeResult::Word(value) => (value, FrameAnswer::one(value)),
                            // a plain table native never parks (the
                            // generic fork bodies live behind the
                            // CALLG dispatch, and this arm never
                            // reaches them)
                            NativeResult::Fork { .. } => {
                                unreachable!("a plain native never parks")
                            },
                            // intercepted above: the park returned
                            // before this match ever ran
                            NativeResult::Park(_) => {
                                unreachable!("a world park returned before the match")
                            },
                            NativeResult::Tag { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    self.stack[fp + 2] = Word::Slot(payload);
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &[payload]))
                            },
                            // a span channel's tail ride: the payload
                            // words land on the original caller's
                            // overlay past the head (physical words
                            // fp+2..), bounds-checked against that
                            // caller's area; at the root only the
                            // head word survives, the root rule
                            // RETSPAN returns by
                            NativeResult::Span { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if 1 + payload.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(1 + payload.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in payload.iter().enumerate() {
                                        self.stack[fp + 2 + index] = Word::Slot(*word);
                                    }
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &payload))
                            },
                            // the tag-less total's tail ride: no
                            // discriminant, so word 0 rides the RET
                            // and the rest land on the original
                            // caller's overlay words 1.. (physical
                            // words fp+2..), bounds-checked against
                            // that caller's area; at the root only
                            // word 0 survives
                            NativeResult::Total { words } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if words.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(words.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in words.iter().enumerate().skip(1) {
                                        self.stack[fp + 1 + index] = Word::Slot(*word);
                                    }
                                }
                                let lead = words
                                    .first()
                                    .copied()
                                    .expect("a tag-less total carries a word");
                                (lead, FrameAnswer::many(words))
                            },
                        };
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(_) => {
                                // the native's whole span answer rides the
                                // completion channel to a live resumer and
                                // the drive's death value whole
                                match self.complete_current_span(&answer) {
                                    Some((a, at)) => {
                                        active = a;
                                        pc = at;
                                        continue 'transfer;
                                    },
                                    None => return Ok(DriveOutcome::Died(answer)),
                                }
                            },
                            RetOutcome::Boundary(_) => return Ok(DriveOutcome::Frame(answer)),
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_CALLS => {
                        if fc != 0 {
                            reserved!();
                        }
                        let callee_slot = fb;
                        let nargs = fa;
                        // CALLS, the callable-word call form (F2): the callee
                        // operand is a frame slot, the ISA's one dynamic-dispatch
                        // point — the callee word's identity is runtime data by
                        // construction, so the read dispatches on the word's
                        // kind: a scalar FuncRef names the function directly, a
                        // Ref word names a closure object whose header is the
                        // FuncRef and whose capture block rides along. The
                        // slot's static classification stays the boundary: a
                        // Fn slot is callable-defaulted (either word may
                        // arrive), a Ref slot holds a value-site closure.
                        // read_value bounds-checks the slot operand first.
                        let word = read_value(&self.stack, fp, callee_slot, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        if !matches!(
                            slots[usize::from(callee_slot)],
                            SlotType::Fn | SlotType::Ref
                        ) {
                            return Err((
                                Trap::SlotTypeMismatch {
                                    slot: callee_slot,
                                    expected: SlotType::Fn,
                                    actual: slots[usize::from(callee_slot)],
                                },
                                pc,
                            ));
                        }
                        let (funcref, captures) = match word {
                            Value::Fn(funcref) => (funcref, None),
                            Value::Ref(obj) => {
                                let (header, block) = stage_closure(
                                    &mut self.objects,
                                    &mut self.pool,
                                    self.current,
                                    &mut self.calls_staging,
                                    obj,
                                )
                                .map_err(|e| (e, pc))?;
                                (header, Some(block))
                            },
                            v => {
                                return Err((
                                    Trap::SlotTypeMismatch {
                                        slot: callee_slot,
                                        expected: SlotType::Fn,
                                        actual: v.slot_type(),
                                    },
                                    pc,
                                ))
                            },
                        };
                        let (version, func_id) =
                            StackState::resolve_func_word(&self.registry, funcref, None, nargs)
                                .map_err(|e| (e, pc))?;
                        let target = &self.registry.pinned(version).functions()[func_id as usize];
                        // a closure's capture block lands in the callee's frame
                        // starting at the first local slot (after the params),
                        // so the body reads a capture where a lowered lambda
                        // declares it. Each Ref capture acquires one count for
                        // the frame at the copy-in — the callee owns its copy
                        // like a borrow parameter and balances it with its own
                        // exit drops, while the closure object keeps its own
                        // count untouched. The range check runs before the
                        // frame is pushed; each write re-checks the word
                        // against the slot's declared type.
                        let capture_body_len = target.layout.body_len();
                        if let Some(block) = captures {
                            let end = target.layout.params_len() + block.len();
                            if end > capture_body_len {
                                return Err((
                                    Trap::SlotOutOfRange {
                                        slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                                        frame_slots: target.layout.slots().len(),
                                    },
                                    pc,
                                ));
                            }
                        }
                        // the frame setup mirrors CALL: slide-plan validation,
                        // the raw-staged check, the callee's metadata word in
                        // the caller's call word, the args overlaid. No tail
                        // variant exists, so this is an ordinary push. The
                        // callee is runtime data (the staged word named it), so
                        // the facts derive here, once per call. The resolved
                        // handle moves through the call into the transfer
                        // (one mint per call, CALL's own discipline), and the
                        // capture copy-in re-derives its layout borrow from
                        // the transfer's handle.
                        let facts = SiteFacts::derive(&f.layout, &target.layout);
                        let t = {
                            let mut st = stack_state!();
                            st.call(version, func_id, &facts, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?
                        };
                        if let Some(block) = captures {
                            // the callee's frame base: fp + 1 + the caller's
                            // body span (the pinned physical mapping)
                            let callee_fp = fp + 1 + f.layout.body_len();
                            let target =
                                &self.registry.pinned(t.version).functions()[t.func as usize];
                            let capture_owner = self.current;
                            for (index, &value) in block.iter().enumerate() {
                                if let Value::Ref(word) = value {
                                    Self::inc_ref_split(
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        capture_owner,
                                        self.ctx_live,
                                        capture_owner,
                                        word,
                                    )
                                    .map_err(|e| (e, pc))?;
                                }
                                let slot = u8::try_from(target.layout.params_len() + index)
                                    .expect("capture slot fits u8 after the range check");
                                write_slot(
                                    &mut self.stack,
                                    callee_fp,
                                    slot,
                                    target.layout.slots(),
                                    capture_body_len,
                                    value,
                                )
                                .map_err(|e| (e, pc))?;
                            }
                        }
                        active = Active {
                            version: t.version,
                            func: t.func,
                        };
                        pc = CodePc(0);
                        continue 'transfer;
                    },
                    OP_CALLG => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        // A generic call site [H3]: the callee is a generic
                        // builtin's ONE body, and the call's evidence words sit
                        // in the overlay after the staged arguments — their
                        // count travels with the callee, so the uop carries
                        // CALL's operand layout. The dispatch reads the
                        // evidence words and selects the body's behavior on the
                        // resolved types; monomorphic instances do not exist.
                        // Install proved the operand's pool index in range and
                        // its kind FuncRef (module.rs's body validation); the
                        // fetch indexes directly and the kind arm is the
                        // corrupted-pool tripwire.
                        let found = &pool[usize::from(callee.0)];
                        let Const::FuncRef(callee) = found else {
                            return Err((
                                Trap::PoolKind {
                                    kidx: callee.0,
                                    expected: ConstKind::FuncRef,
                                    found: found.kind(),
                                },
                                pc,
                            ));
                        };
                        // A runtime-resolved trait dispatch inside a
                        // generic impl body [H3]: the sentinel FuncRef
                        // names the calling frame's evidence slot, and
                        // the word staged there by the site that entered
                        // the body names an evidence-chain object — the
                        // dispatch resolves its serving body and stages
                        // the tail objects' indices as the body's trailing
                        // evidence parameters, then transfers exactly like
                        // a CALL's uop callee.
                        if let ConstFuncRef::Named { module, func } = callee
                            && module == EVIDENCE_MODULE
                        {
                            let slot = u8::try_from(*func).map_err(|_| {
                                (
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *func,
                                    },
                                    pc,
                                )
                            })?;
                            let version = active.version;
                            let func_id = evidence_chain_dispatch(
                                self, version, slot, slots, body_len, fp, &f.layout, nargs, pc,
                            )?;
                            let target =
                                &self.registry.pinned(version).functions()[func_id as usize];
                            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
                            // the serving body is runtime data (the frame's
                            // evidence word named it), so the facts derive
                            // here, once per call
                            let facts = SiteFacts::derive(&f.layout, &target.layout);
                            let mut st = stack_state!();
                            let t = st
                                .call(version, func_id, &facts, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // A user trait-method dispatch site [H3]: the
                        // FuncRef names the impl body the lowering resolved
                        // from the call's statically solved instance. The
                        // resolution runs through the per-site inline cache
                        // (the same bank and key CALL/CALLT use, full
                        // lookup as the fallback), so a hot dispatch site
                        // skips the module lookup. The
                        // staged words past `nargs` carry the runtime
                        // evidence a generic impl body claims as its
                        // trailing parameters (one per the body's nested
                        // demands, validated as I64 words), then the
                        // solved declared-instance id riding past them —
                        // validated here, carried rather than
                        // re-dispatched (the instance-keyed cache story
                        // keys off this word later). The body transfers
                        // exactly like a CALL's uop callee: the carried
                        // word sits past the callee's parameters, so the
                        // callee's frame never claims it.
                        let caller_site = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_dispatch_site(
                                callee,
                                caller_site,
                                active.version,
                                &f.layout,
                                pc.0,
                                nargs,
                            )
                            .map_err(|e| (e, pc))?;
                        if let DispatchResolved::Uop {
                            version,
                            func: func_id,
                            facts,
                        } = resolved
                        {
                            let target =
                                &self.registry.pinned(version).functions()[func_id as usize];
                            let params_len = target.layout.params_len();
                            if usize::from(nargs) > params_len {
                                return Err((
                                    Trap::ArityMismatch {
                                        expected: params_len,
                                        given: usize::from(nargs),
                                    },
                                    pc,
                                ));
                            }
                            // the staged evidence parameters the body
                            // claims, then the solved instance word
                            let ev_count = params_len - usize::from(nargs);
                            let ev_body_len = f.layout.body_len();
                            for i in 0..=ev_count {
                                let ev_slot = u8::try_from(
                                    f.layout.arg_area().start + usize::from(nargs) + i,
                                )
                                .expect("slot index fits u8");
                                let word = read_value(&self.stack, fp, ev_slot, slots, ev_body_len)
                                    .map_err(|e| (e, pc))?;
                                if word.slot_type() != SlotType::I64 {
                                    return Err((
                                        Trap::SlotTypeMismatch {
                                            slot: ev_slot,
                                            expected: SlotType::I64,
                                            actual: word.slot_type(),
                                        },
                                        pc,
                                    ));
                                }
                            }
                            let mut st = stack_state!();
                            let t = st
                                .call(version, func_id, &facts, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // the generic builtins are native-by-name by
                        // construction: an id-spelled callee reaches
                        // NotUop only through a corrupted pool
                        let (module, func) = match callee {
                            ConstFuncRef::Named { module, func } => (module.as_str(), *func),
                            ConstFuncRef::Uop { module, func } => {
                                let name = self
                                    .registry
                                    .module_name(*module)
                                    .unwrap_or("an uninstalled dependency");
                                return Err((
                                    Trap::UnknownCallee {
                                        module: name.to_owned(),
                                        func: *func,
                                    },
                                    pc,
                                ));
                            },
                            // a bound C symbol never rides the generic
                            // dispatch (H5)
                            ConstFuncRef::Bound { module, index, .. } => {
                                return Err((
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *index,
                                    },
                                    pc,
                                ));
                            },
                        };
                        let g = crate::native::resolve_generic(module, func).ok_or((
                            Trap::UnknownCallee {
                                module: module.to_owned(),
                                func,
                            },
                            pc,
                        ))?;
                        // a fork-join combinator parks its call site; a
                        // teardown-hook drive must return to its driver,
                        // so the fork refuses before its spawn phase runs
                        // (a parked hook would strand the pass)
                        if self.teardown_drive && crate::native::is_fork_join(module, func) {
                            return Err((Trap::ReentrancyRefused, pc));
                        }
                        let needed = generic_staged_check(g, nargs).map_err(|e| (e, pc))?;
                        // the evidence words share the overlay with the staged
                        // arguments, so the area must hold both plus the result
                        let area = f.layout.arg_area_len();
                        if needed > area {
                            return Err((
                                Trap::CallLayout(SlideError::StagingTooSmall {
                                    available: area,
                                    needed,
                                }),
                                pc,
                            ));
                        }
                        if usize::from(g.result_words) > area {
                            return Err((
                                Trap::RetSpanTooWide {
                                    span: u16::from(g.result_words),
                                    caller_area: u16::try_from(area)
                                        .expect("argument areas fit u16"),
                                },
                                pc,
                            ));
                        }
                        let body_len = f.layout.body_len();
                        let start = f.layout.arg_area().start;
                        // the staged arguments and evidence words copy inline
                        // (generic call sites sit well under the inline width),
                        // with the heap buffer the oversized fallback
                        let mut inline = [Value::I64(0); INLINE_WORDS];
                        let mut heap = Vec::new();
                        let args: &[Value] = if usize::from(nargs) <= INLINE_WORDS {
                            for (i, word) in inline.iter_mut().enumerate().take(usize::from(nargs))
                            {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &inline[..usize::from(nargs)]
                        } else {
                            heap.reserve(usize::from(nargs));
                            for i in 0..usize::from(nargs) {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &heap
                        };
                        generic_kind_check(g, args).map_err(|e| (e, pc))?;
                        let mut ev_inline = [Value::I64(0); INLINE_WORDS];
                        let mut ev_heap = Vec::new();
                        let evidence: &[Value] = if usize::from(g.evidence) <= INLINE_WORDS {
                            for (i, word) in ev_inline
                                .iter_mut()
                                .enumerate()
                                .take(usize::from(g.evidence))
                            {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &ev_inline[..usize::from(g.evidence)]
                        } else {
                            ev_heap.reserve(usize::from(g.evidence));
                            for i in 0..usize::from(g.evidence) {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                ev_heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &ev_heap
                        };
                        // the body takes the whole VM: the pure bodies
                        // split off the heap and sink themselves, the
                        // lambda-taking combinators reenter the
                        // dispatcher through `call_hosted`, and the
                        // fork-join combinators park — the spawn phase
                        // ran inside the body, and the join runs on
                        // the interlock stack, the drive delivering the
                        // answer onto this call site
                        let result = (g.func)(self, evidence, args).map_err(|e| (e, pc))?;
                        if let NativeResult::Fork { nursery, stage } = result {
                            return self.fork_park(&active, pc, area, nursery, stage);
                        }
                        overlay_native(result, &mut self.stack, &f.layout, fp)
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CALLTG => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        // CALLTG, the dispatch tail call [H2's guarantee
                        // closure]: the dispatch resolution is CALLG's own
                        // — the sentinel evidence FuncRef, a trait site's
                        // impl body, or a generic builtin's ONE body — and
                        // the transfer is CALLT's: the staged words slide
                        // into the reused frame's parameters per the ABI
                        // slide plan, the metadata word rewrites in place,
                        // and the stack never grows at any depth. The
                        // evidence/instance words past the callee's
                        // parameters ride the transfer as count-free dead
                        // words the callee never claims, so the slide moves
                        // exactly the parameter count. Install proved the
                        // operand's pool index in range and its kind FuncRef
                        // (module.rs's body validation); the fetch indexes
                        // directly and the kind arm is the corrupted-pool
                        // tripwire.
                        let found = &pool[usize::from(callee.0)];
                        let Const::FuncRef(callee) = found else {
                            return Err((
                                Trap::PoolKind {
                                    kidx: callee.0,
                                    expected: ConstKind::FuncRef,
                                    found: found.kind(),
                                },
                                pc,
                            ));
                        };
                        // the sentinel names the frame's evidence slot: the
                        // target is runtime data the entering site staged,
                        // so this path reads the slot every time and never
                        // touches the cache — the chain object stages the
                        // riders exactly like the CALLG sentinel path above,
                        // before the frame-reuse slide moves them
                        if let ConstFuncRef::Named { module, func } = callee
                            && module == EVIDENCE_MODULE
                        {
                            let slot = u8::try_from(*func).map_err(|_| {
                                (
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *func,
                                    },
                                    pc,
                                )
                            })?;
                            let version = active.version;
                            let func_id = evidence_chain_dispatch(
                                self, version, slot, slots, body_len, fp, &f.layout, nargs, pc,
                            )?;
                            let target =
                                &self.registry.pinned(version).functions()[func_id as usize];
                            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
                            // the serving body is runtime data (the frame's
                            // evidence word named it), so the facts derive
                            // here, once per call
                            let facts = SiteFacts::derive(&f.layout, &target.layout);
                            self.arena_frame_close(fp);
                            let mut st = stack_state!();
                            let t = st
                                .tail_call(version, func_id, &facts, &f.layout, fp)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // a trait site's impl body: the per-site inline
                        // cache keys the callee (full lookup as the
                        // fallback), the staged evidence words validate as
                        // I64 before the slide reads them, and the transfer
                        // reuses this frame
                        let caller_site = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_dispatch_site(
                                callee,
                                caller_site,
                                active.version,
                                &f.layout,
                                pc.0,
                                nargs,
                            )
                            .map_err(|e| (e, pc))?;
                        if let DispatchResolved::Uop {
                            version,
                            func: func_id,
                            facts,
                        } = resolved
                        {
                            let params_len = self.registry.pinned(version).functions()
                                [func_id as usize]
                                .layout
                                .params_len();
                            // the staged evidence parameters the body
                            // claims, then the solved instance word — both
                            // count-free I64 constants riding past the
                            // slide's width
                            let ev_count = params_len - usize::from(nargs);
                            let ev_body_len = f.layout.body_len();
                            for i in 0..=ev_count {
                                let ev_slot = u8::try_from(
                                    f.layout.arg_area().start + usize::from(nargs) + i,
                                )
                                .expect("slot index fits u8");
                                let word = read_value(&self.stack, fp, ev_slot, slots, ev_body_len)
                                    .map_err(|e| (e, pc))?;
                                if word.slot_type() != SlotType::I64 {
                                    return Err((
                                        Trap::SlotTypeMismatch {
                                            slot: ev_slot,
                                            expected: SlotType::I64,
                                            actual: word.slot_type(),
                                        },
                                        pc,
                                    ));
                                }
                            }
                            self.arena_frame_close(fp);
                            let mut st = stack_state!();
                            let t = st
                                .tail_call(version, func_id, &facts, &f.layout, fp)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // a generic builtin's ONE body runs inside the
                        // instruction and the frame returns its result —
                        // the CALLT-to-native discipline: the span result
                        // lands on the ORIGINAL caller's overlay words, and
                        // at the root frame only the head word survives.
                        // A fork-join combinator cannot run here: the
                        // join parks its site through the drive, and the
                        // tail call would return this frame at the call —
                        // the fork needs the CALLG site
                        // the generic builtins are native-by-name by
                        // construction: an id-spelled callee reaches
                        // NotUop only through a corrupted pool
                        let (module, func) = match callee {
                            ConstFuncRef::Named { module, func } => (module.as_str(), *func),
                            ConstFuncRef::Uop { module, func } => {
                                let name = self
                                    .registry
                                    .module_name(*module)
                                    .unwrap_or("an uninstalled dependency");
                                return Err((
                                    Trap::UnknownCallee {
                                        module: name.to_owned(),
                                        func: *func,
                                    },
                                    pc,
                                ));
                            },
                            // a bound C symbol never rides the generic
                            // dispatch (H5)
                            ConstFuncRef::Bound { module, index, .. } => {
                                return Err((
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *index,
                                    },
                                    pc,
                                ));
                            },
                        };
                        let g = crate::native::resolve_generic(module, func).ok_or((
                            Trap::UnknownCallee {
                                module: module.to_owned(),
                                func,
                            },
                            pc,
                        ))?;
                        if crate::native::is_fork_join(module, func) {
                            return Err((Trap::TailForkJoin, pc));
                        }
                        let needed = generic_staged_check(g, nargs).map_err(|e| (e, pc))?;
                        let area = f.layout.arg_area_len();
                        if needed > area {
                            return Err((
                                Trap::CallLayout(SlideError::StagingTooSmall {
                                    available: area,
                                    needed,
                                }),
                                pc,
                            ));
                        }
                        if let Some(caller_area) = self.caller_arg_area()
                            && usize::from(g.result_words) > caller_area
                        {
                            return Err((
                                Trap::RetSpanTooWide {
                                    span: u16::from(g.result_words),
                                    caller_area: u16::try_from(caller_area)
                                        .expect("argument areas fit u16"),
                                },
                                pc,
                            ));
                        }
                        let start = f.layout.arg_area().start;
                        let mut inline = [Value::I64(0); INLINE_WORDS];
                        let mut heap = Vec::new();
                        let args: &[Value] = if usize::from(nargs) <= INLINE_WORDS {
                            for (i, word) in inline.iter_mut().enumerate().take(usize::from(nargs))
                            {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &inline[..usize::from(nargs)]
                        } else {
                            heap.reserve(usize::from(nargs));
                            for i in 0..usize::from(nargs) {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &heap
                        };
                        generic_kind_check(g, args).map_err(|e| (e, pc))?;
                        let mut ev_inline = [Value::I64(0); INLINE_WORDS];
                        let mut ev_heap = Vec::new();
                        let evidence: &[Value] = if usize::from(g.evidence) <= INLINE_WORDS {
                            for (i, word) in ev_inline
                                .iter_mut()
                                .enumerate()
                                .take(usize::from(g.evidence))
                            {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &ev_inline[..usize::from(g.evidence)]
                        } else {
                            ev_heap.reserve(usize::from(g.evidence));
                            for i in 0..usize::from(g.evidence) {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                ev_heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &ev_heap
                        };
                        let result = (g.func)(self, evidence, args).map_err(|e| (e, pc))?;
                        // the answer's whole word span rides a boundary
                        // return (the hosted seam's result channel); every
                        // other outcome keeps its prior words
                        let (value, answer) = match result {
                            NativeResult::Word(value) => (value, FrameAnswer::one(value)),
                            // the tail site refuses the fork before its
                            // body runs (the is_fork_join check above)
                            NativeResult::Fork { .. } => {
                                unreachable!("the tail site refuses a fork request first")
                            },
                            // the generic builtins are pure: a world op
                            // lives behind the platform table's plain
                            // call sites, never the generic dispatch
                            NativeResult::Park(_) => {
                                unreachable!("a generic builtin never parks")
                            },
                            NativeResult::Tag { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    self.stack[fp + 2] = Word::Slot(payload);
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &[payload]))
                            },
                            // the span channel's tail ride, the CALLT
                            // native shape's: the payload words land on the
                            // original caller's overlay past the head,
                            // bounds-checked against that caller's area
                            // (the root keeps only the head word)
                            NativeResult::Span { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if 1 + payload.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(1 + payload.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in payload.iter().enumerate() {
                                        self.stack[fp + 2 + index] = Word::Slot(*word);
                                    }
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &payload))
                            },
                            // the tag-less total's tail ride, the CALLT
                            // native shape's with no discriminant: word 0
                            // rides the RET, the rest land on the original
                            // caller's overlay words 1.., bounds-checked
                            // against that caller's area
                            NativeResult::Total { words } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if words.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(words.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in words.iter().enumerate().skip(1) {
                                        self.stack[fp + 1 + index] = Word::Slot(*word);
                                    }
                                }
                                let lead = words
                                    .first()
                                    .copied()
                                    .expect("a tag-less total carries a word");
                                (lead, FrameAnswer::many(words))
                            },
                        };
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(_) => {
                                // the entry frame answers with its whole span:
                                // it rides the completion channel (a RETSPAN
                                // at the root, or a tail native's span/total)
                                // to a live resumer, and the drive's death
                                // value carries it whole
                                match self.complete_current_span(&answer) {
                                    Some((a, at)) => {
                                        active = a;
                                        pc = at;
                                        continue 'transfer;
                                    },
                                    None => return Ok(DriveOutcome::Died(answer)),
                                }
                            },
                            RetOutcome::Boundary(_) => return Ok(DriveOutcome::Frame(answer)),
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_RET => {
                        if fd != 0 {
                            reserved!();
                        }
                        let src = fa;
                        let value = read_value(&self.stack, fp, src, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(v) => match self.complete_current(v) {
                                Some((a, at)) => {
                                    active = a;
                                    pc = at;
                                    continue 'transfer;
                                },
                                None => return Ok(DriveOutcome::Died(FrameAnswer::one(v))),
                            },
                            RetOutcome::Boundary(v) => {
                                return Ok(DriveOutcome::Frame(FrameAnswer::one(v)))
                            },
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_RET_SPAN => {
                        if fb != 0 || fc == 0 {
                            reserved!();
                        }
                        let src = fa;
                        let n = fc;
                        // the span must fit inside this frame's own slots (a
                        // load-time fact, re-checked defensively)
                        if usize::from(src) + usize::from(n) > frame_slots {
                            return Err((
                                Trap::SlotOutOfRange {
                                    slot: src,
                                    frame_slots,
                                },
                                pc,
                            ));
                        }
                        // the caller's overlay area must be wide enough to
                        // receive the whole span (cross-function, so runtime)
                        // — except at the hosted seam's reentry frame, where
                        // the span rides the drive's outcome instead of any
                        // caller's overlay (the sentinel return pc marks it)
                        let at_reentry = boundary != usize::MAX
                            && self.callinfo.len() == boundary
                            && matches!(self.stack[fp], Word::Meta(m) if m.return_pc.0 == u32::MAX);
                        if self.callinfo.len() > 1 && !at_reentry {
                            let caller = &self.callinfo[self.callinfo.len() - 2];
                            let caller_fn = &self.registry.pinned(caller.module).functions()
                                [caller.func as usize];
                            let area = caller_fn.layout.arg_area_len();
                            if usize::from(n) > area {
                                return Err((
                                    Trap::RetSpanTooWide {
                                        span: u16::from(n),
                                        caller_area: u16::try_from(area)
                                            .expect("argument areas fit u16"),
                                    },
                                    pc,
                                ));
                            }
                        }
                        // the span's answer rides the boundary return whole
                        // and lands at overlay words 0..n — the caller's
                        // outgoing-argument words, since the callee's own slots
                        // alias them. The words are read whole before any is
                        // placed, so the callee's slots and the caller's
                        // overlay words never overlap, and a narrow span
                        // (every lowered lambda's answer) allocates nothing.
                        let answer = FrameAnswer::staged(usize::from(n), |i| {
                            let slot = u8::try_from(usize::from(src) + i)
                                .expect("span stays inside the u8 slot range");
                            read_value(&self.stack, fp, slot, slots, body_len)
                        })
                        .map_err(|e| (e, pc))?;
                        for i in 0..usize::from(n) {
                            self.stack[fp + 1 + i] = Word::Slot(answer.as_slice()[i]);
                        }
                        let value = match self.stack[fp + 1] {
                            Word::Slot(v) => v,
                            Word::Meta(_) => {
                                return Err((Trap::MetaWordCorrupt { fp }, pc));
                            },
                        };
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(_) => {
                                // the entry frame answers with its whole span:
                                // it rides the completion channel (a RETSPAN
                                // at the root, or a tail native's span/total)
                                // to a live resumer, and the drive's death
                                // value carries it whole
                                match self.complete_current_span(&answer) {
                                    Some((a, at)) => {
                                        active = a;
                                        pc = at;
                                        continue 'transfer;
                                    },
                                    None => return Ok(DriveOutcome::Died(answer)),
                                }
                            },
                            RetOutcome::Boundary(_) => return Ok(DriveOutcome::Frame(answer)),
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_HALT => {
                        if fd != 0 {
                            reserved!();
                        }
                        let slot = fa;
                        let value = read_value(&self.stack, fp, slot, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        // HALT ends the fiber; under a nested reentrant drive
                        // that fiber carries the Rust-side native loop's
                        // frames — refuse instead of dying mid-native (the
                        // frames stay for backtrace)
                        if self.reentry_live(boundary) {
                            return Err((Trap::ReentrancyRefused, pc));
                        }
                        match self.complete_current(value) {
                            Some((a, at)) => {
                                active = a;
                                pc = at;
                                continue 'transfer;
                            },
                            None => return Ok(DriveOutcome::Died(FrameAnswer::one(value))),
                        }
                    },
                    // Major 0xE: the placed RC pair (C4). Load decode-checks
                    // every word once, so a canonicity violation here is like
                    // any other; each arm moves one counted heap block's
                    // count per word.
                    OP_RC_INC => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        if let Some(word) = read_counted(&self.stack, fp, fa, slots, body_len)
                            .map_err(|e| (e, pc))?
                        {
                            self.inc_ref(word).map_err(|e| (e, pc))?;
                        }
                        pc.0 += 1;
                    },
                    OP_RC_DEC => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        if let Some(word) = read_counted(&self.stack, fp, fa, slots, body_len)
                            .map_err(|e| (e, pc))?
                        {
                            self.dec_ref(word).map_err(|e| (e, pc))?;
                        }
                        pc.0 += 1;
                    },
                    _ => {
                        // every canonical byte is served by an arm above,
                        // so anything here is a canonicity violation and the
                        // decoders remain the error authority for its exact
                        // cause
                        let cause = if byte >> 4 == RC_MAJOR {
                            match RcOp::decode(word) {
                                Err(RcDecodeError::UnknownOpcode { major, minor }) => {
                                    DecodeError::UnknownOpcode { major, minor }
                                },
                                Err(RcDecodeError::ReservedField) => DecodeError::ReservedField,
                                Ok(_) => unreachable!("every canonical RC byte is a fast arm"),
                            }
                        } else {
                            DecodeError::UnknownOpcode {
                                major: byte >> 4,
                                minor: byte & 0x0f,
                            }
                        };
                        return Err((Trap::BadEncoding { pc: pc.0, cause }, pc));
                    },
                }
            }
        }
    }

    /// Renders the live frames of the fiber the last drive ended on
    /// (innermost first): each real frame with its position plus the
    /// tail-chain elided count [H2]. The counter is always available;
    /// per-elision `ElisionRecord`s for lazy materialization come with
    /// per-fiber debug recording, so at M3 the count reports how many
    /// logical frames a debugger would materialize later.
    #[must_use]
    pub fn backtrace(&self) -> Vec<BacktraceFrame> {
        let fiber = &self.pool[self.current.index()];
        let mut frames = Vec::with_capacity(fiber.callinfo.len());
        for depth in (0..fiber.callinfo.len()).rev() {
            let ci = &fiber.callinfo[depth];
            let f = &self.registry.pinned(ci.module).functions()[ci.func as usize];
            let pc = if depth + 1 == fiber.callinfo.len() {
                FramePc::At(self.trap_pc.unwrap_or(CodePc(0)))
            } else {
                match fiber.segment[fiber.callinfo[depth + 1].fp] {
                    Word::Meta(m) if m.return_pc.0 == u32::MAX => FramePc::Hosted,
                    Word::Meta(m) => FramePc::At(m.return_pc),
                    Word::Slot(_) => FramePc::At(CodePc(0)),
                }
            };
            let elided = match fiber.segment[ci.fp] {
                Word::Meta(m) => m.tail.elided,
                Word::Slot(_) => 0,
            };
            frames.push(BacktraceFrame {
                module: self.registry.pinned(ci.module).name().to_owned(),
                func: f.name.clone(),
                pc,
                elided,
            });
        }
        frames
    }

    /// One channel step (the 2026-11-21 shared-state ruling): the fast
    /// paths run against the channel's own store through the router
    /// (deposit, pop, the waiter queues), a full send or empty receive
    /// parks the caller in the channel's waiter queue — the payload's
    /// whole word span staged with no count move, the parked frame
    /// keeping its own counts exactly the net send wait's discipline —
    /// and a completed hand-off wakes the parked peer through the
    /// scheduler's delivery and run queue, the peer fiber itself the
    /// resumer (no reactor interest, no deadline). The payload element
    /// spans words at the buffer's element stride (the collection span
    /// seam: a record or tuple payload's whole span crosses, the
    /// recv's answer the tag-less total of its words), the staged span
    /// read past the channel word exactly a `list_push` element's.
    fn chan_step(
        &mut self,
        channel: ChannelNative,
        fp: usize,
        caller: &FrameLayout,
        nargs: u8,
    ) -> Result<ChanStep, Trap> {
        match channel {
            ChannelNative::New => {
                let cap = control_i64(&self.stack, fp, caller, 0)?;
                if cap < 1 {
                    return Err(Trap::ChanCapacity { cap });
                }
                let cap = usize::try_from(cap).expect("a positive capacity fits usize");
                let word = self.heaps().alloc_chan(cap);
                Ok(ChanStep::Answer(NativeResult::Word(word)))
            },
            ChannelNative::Send => {
                let Value::Ref(chan) = control_word(&self.stack, fp, caller, 0)? else {
                    return Err(Trap::UnknownObject { obj: 0 });
                };
                // the one-word payload keeps the allocation-free read; a
                // wider element's whole staged span rides the vec (the
                // park path owns its copy either way)
                let payload = if usize::from(nargs) == 2 {
                    vec![control_word(&self.stack, fp, caller, 1)?]
                } else {
                    (1..usize::from(nargs))
                        .map(|index| control_word(&self.stack, fp, caller, index))
                        .collect::<Result<Vec<_>, Trap>>()?
                };
                let here = self.current;
                if !self.heaps().chan_room(chan)? {
                    self.heaps().chan_park_sender(chan, here, payload)?;
                    return Ok(ChanStep::Park);
                }
                self.heaps().chan_deposit(chan, &payload)?;
                self.chan_release(chan)?;
                Ok(ChanStep::Answer(NativeResult::Word(Value::I64(0))))
            },
            ChannelNative::Recv => {
                let Value::Ref(chan) = control_word(&self.stack, fp, caller, 0)? else {
                    return Err(Trap::UnknownObject { obj: 0 });
                };
                let here = self.current;
                let Some(elem) = self.heaps().chan_pop(chan)? else {
                    self.heaps().chan_park_receiver(chan, here)?;
                    return Ok(ChanStep::Park);
                };
                let result = match elem {
                    crate::object::ChanPop::One(word) => {
                        if let Value::Ref(payload) = word {
                            self.heaps().adopt_claim(here, payload);
                        }
                        NativeResult::Word(word)
                    },
                    crate::object::ChanPop::Span(words) => {
                        for word in &words {
                            if let Value::Ref(payload) = word {
                                self.heaps().adopt_claim(here, *payload);
                            }
                        }
                        NativeResult::Total { words }
                    },
                };
                self.chan_refill(chan)?;
                Ok(ChanStep::Answer(result))
            },
        }
    }

    /// The channel switch behind a `CALL` site: the step runs, the
    /// answer overlays argument slot 0, and a park ends the drive with
    /// the world-park event (the resume point the instruction after the
    /// call).
    fn chan_switch(
        &mut self,
        channel: ChannelNative,
        active: &Active,
        pc: u32,
        fp: usize,
        caller: &FrameLayout,
        nargs: u8,
    ) -> Result<ChanOutcome, Trap> {
        match self.chan_step(channel, fp, caller, nargs)? {
            ChanStep::Answer(result) => {
                overlay_native(result, &mut self.stack, caller, fp)?;
                Ok(ChanOutcome::Answered)
            },
            ChanStep::Park => match self
                .park_world(active, CodePc(pc), ParkKind::Chan)
                .map_err(|(trap, _)| trap)?
            {
                event @ DriveOutcome::WorldPark(_) => Ok(ChanOutcome::Parked(event)),
                _ => unreachable!("a park answers the park event"),
            },
        }
    }

    /// Hands one buffered payload element to the head live receiver
    /// after a deposit: each `Ref` word's buffer edge count becomes the
    /// receiver's claim (the ledger transfer, no count mutation), the
    /// delivery staged — the one-word element as a word, a wider one as
    /// the tag-less total of its span — and the receiver queued ready.
    /// Stale waiters — a nursery cancel cut them, a release door took
    /// them — drop; their entry rode no count.
    fn chan_release(&mut self, chan: RefId) -> Result<(), Trap> {
        loop {
            let Some(receiver) = self.heaps().chan_take_receiver(chan)? else {
                return Ok(());
            };
            if self
                .pool
                .get(receiver.index())
                .is_none_or(|f| f.state != FiberState::WorldParked)
            {
                continue;
            }
            let elem = self.heaps().chan_pop(chan)?;
            let Some(elem) = elem else {
                // a woken receiver parks only over an empty buffer and
                // the deposit preceded, so this names hand-assembled
                // waiter tangle; the receiver goes back empty-handed
                self.heaps().chan_park_receiver(chan, receiver)?;
                return Ok(());
            };
            let result = match elem {
                crate::object::ChanPop::One(word) => {
                    if let Value::Ref(payload) = word {
                        self.heaps().adopt_claim(receiver, payload);
                    }
                    NativeResult::Word(word)
                },
                crate::object::ChanPop::Span(words) => {
                    for word in &words {
                        if let Value::Ref(payload) = word {
                            self.heaps().adopt_claim(receiver, *payload);
                        }
                    }
                    NativeResult::Total { words }
                },
            };
            self.wake_chan_waiter(receiver, result);
            return Ok(());
        }
    }

    /// Moves one parked sender's staged payload span into the slot a
    /// receive freed: the deposit takes the buffer's edge count per
    /// `Ref` word on the payload's own heap (the C4 funnel), the sender
    /// woken with the unit word. Stale senders drop — the payload's
    /// counts were the parked frame's own, released by whoever cut it.
    fn chan_refill(&mut self, chan: RefId) -> Result<(), Trap> {
        loop {
            let Some(sender) = self.heaps().chan_take_sender(chan)? else {
                return Ok(());
            };
            if self
                .pool
                .get(sender.fiber.index())
                .is_none_or(|f| f.state != FiberState::WorldParked)
            {
                continue;
            }
            self.heaps().chan_deposit(chan, &sender.payload)?;
            self.wake_chan_waiter(sender.fiber, NativeResult::Word(Value::I64(0)));
            return Ok(());
        }
    }

    /// Wakes one channel-parked fiber with `result`: the delivery and
    /// run-queue push the net wake performs, minus the wait table (a
    /// channel waiter registers in the channel's own data, never the
    /// scheduler's — nothing to deregister, no deadline to cancel).
    fn wake_chan_waiter(&mut self, fiber: FiberId, result: NativeResult) {
        if self
            .pool
            .get(fiber.index())
            .is_some_and(|f| f.state == FiberState::WorldParked)
        {
            self.pool[fiber.index()].state = FiberState::Suspended;
            self.sched.deliveries.insert(fiber, result);
            self.sched.push_ready(fiber);
        }
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
}
