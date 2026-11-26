//! The structured stop conditions: every refusal the dispatcher and the frame
//! operations raise, as an enum and never a panic.

use super::*;

/// A structured stop condition raised as an enum error, never a panic (the
/// per-entry stack-limit compare is the only stack exit).
#[derive(Clone, Debug, PartialEq)]
pub enum Trap {
    /// A frame entry needs more stack words than the limit allows.
    StackOverflow {
        /// Words the entering frame needed.
        needed: usize,
        /// The configured stack limit.
        limit: usize,
    },
    /// An i64 add/sub/mul/div/mod left the i64 range.
    IntegerOverflow,
    /// DIVI64/MODI64 with a zero divisor.
    DivideByZero,
    /// SHLI64/SHRI64 with a shift amount outside `0..64` (shifts trap past the
    /// bit width; wrapping stays the explicit operators).
    OversizedShift,
    /// CVTF2I on a NaN or a value outside the i64 range.
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
    /// over a staged word outside the scalar slice, or a signature
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
    /// CAPTURE_STORE addressed a word past a closure object's capture
    /// block. Lowered code cannot trip this — the lowerer derives the
    /// index from the capture list it lowered the block with — so it
    /// names hand-assembled code.
    CaptureIndex {
        /// The closure object's table id.
        obj: u32,
        /// The addressed capture index.
        idx: usize,
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
    /// An indexed mutation (an element store, a removal, an insertion)
    /// named an index outside the list's live element range: the
    /// checked reads answer the `none` member because the caller can
    /// extract a miss, but a mutating member and an assignment place
    /// keep no extractable miss channel — the place must resolve or
    /// halt, so the out-of-range index traps loud naming it.
    ListIndex {
        /// The list object's table id.
        obj: u32,
        /// The rejected index.
        index: i64,
        /// The list's live element count.
        len: usize,
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
    /// The bare trap primitive's deliberate trap: a `die` spelling's message-bearing abort. The
    /// engine reports nothing itself — what the trap prints and which exit code
    /// follows is the host's own policy.
    Die {
        /// The message the spelling staged.
        message: String,
    },
    /// The `exit(code)` deliberate process status: like `Die`, the engine
    /// decides nothing — the code rides the trap out and the host's
    /// exit-code policy applies above it. A negative status traps at
    /// the seam and a wider one wraps to its low
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
    /// The IO scheduler found nothing runnable and nothing waiting: every fiber is parked
    /// on a wait that no reactor interest and no deadline can answer.
    /// A net wait always registers one or the other, so for those this
    /// names a scheduler bug; a channel wait registers neither (the
    /// waking peer is a fiber, and none can run), so a program whose
    /// every fiber sits parked on a channel no live fiber can serve
    /// deadlocks here by design — the honest degenerate: no sender, no
    /// answer.
    SchedulerDeadlock,
    /// `chan_new(cap)` with a negative bound: zero is the rendezvous bound — the buffer
    /// holds nothing and every hand-off pairs a parked sender's staged
    /// span with a receiving fiber — so only a bound below zero names
    /// no buffer and no rendezvous.
    ChanCapacity {
        /// The refused bound.
        cap: i64,
    },
    /// An arena handle named no live element (the read-side aliasing
    /// container): the copy carried a retired slot's generation, an
    /// index the arena never held, or a reused slot's old one — the
    /// deterministic trap the removal makes of every handle copy taken
    /// before it. Lowered code reaches it plainly (a handle is a copyable
    /// int-backed scalar), so unlike most traps this is source-reachable
    /// by honest programs.
    DanglingArenaHandle {
        /// The handle's slot index half.
        index: u32,
        /// The handle's generation half, the one the check refused.
        generation: u32,
    },
    /// `cancel(payload)`: the fiber's own deliberate failure. The
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
    /// The host's shutdown event cut the drive: the host's own registered interest — the CLI's
    /// signalfd — woke the reactor, every parked fiber answered the
    /// cancel sentinel at its resume-shaped point with its deinit
    /// hooks fired, and the drive hands control back. The engine
    /// decides nothing here: no message, no exit code — the host that
    /// installed the interest owns the status it answers, per the die
    /// split.
    Shutdown,
    /// A REPL entry met the root fiber in the wrong session state:
    /// [`Vm::repl_start`](crate::repl::Vm::repl_start) needs an empty
    /// root, `repl_resume` and `repl_drop_binding` a parked one. The
    /// refusal is the state itself, never a callable, so the trap names
    /// the entry the host chose.
    ReplState {
        /// The entry the session state refused.
        entry: crate::repl::ReplEntry,
    },
}
