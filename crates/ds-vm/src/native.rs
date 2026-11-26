//! Native (Rust-implemented) functions: the H5.5 layering after the M7
//! platform seam — the VM core's `builtin` module (this file) and the
//! world surface served by the installed platform handler
//! ([`crate::platform`]).
//!
//! Builtins are functions in the source (`str(x)`), so the tier-1
//! mechanism routes them as functions, not uops: a VM installs reserved
//! native modules into its registry, and a pool
//! `FuncRef { module, func: i }` resolves through the same path as any
//! uop callee. A `CALL` to a native never pushes a frame: the native reads
//! the caller's staged overlay words and its result overlays argument
//! slot 0 exactly where a `RET` would put it. This was chosen over a
//! dedicated uop per builtin because `CALL` is already the ISA's
//! function-call mechanism — a uop per builtin would grow the locked set
//! for every stdlib addition, while the module route adds builtins with no
//! opcode growth, keeps arity/typing checks in one place, and lets
//! higher-milestone builtins (list/map/reduce) land as table entries.
//!
//! # The core module and the platform handler
//!
//! The H5.5 layering after its M7 migration: the pure, irreducible
//! primitives every VM carries ([`BUILTIN_MODULE`], installed by
//! [`crate::Vm`] itself), and the world surface — console, fsys, net —
//! which is an installed platform handler's to serve
//! ([`crate::platform`]): a host installs a [`crate::platform::Platform`]
//! under the source's platform declaration name, and the world calls'
//! pool FuncRefs resolve against its capability-scoped table. A library
//! embedder gets no world effects by default (no platform installed
//! means no world call site validates). The `@std` ds-source package
//! layer above both is future work.
//!
//! The table's rows are the call ABI's ([`ds_abi::native`]): this module attaches one body per
//! row and the dispatcher's interception markers. Pool entries name rows positionally, and a gap (a
//! retired native's slot, or a generic builtin's monomorphic slot) keeps its table position as a
//! reserved entry whose body traps exactly like an uninstalled index.
//!
//! Unit natives return the inert unit word `Value::I64(0)`: the caller of a unit callee never reads
//! the overlay word (a lowering discipline), and a tail call to a unit native still needs a return
//! word to overlay.
//!
//! The surface-level `str()` is one function in source; the core table carries its typed
//! instances, and lowering emits the `str_*` FuncRef for the operand's type.
//!
//! # Result spans and the platform tag ids
//!
//! The fallible platform ops (the file and net surface but
//! `file_exists` and the two idempotent close seams `close_raw`/
//! `net_close`) touch the world: that is a static fact (the ambient's
//! declared effect rows), so the VM needs no effect machinery — the
//! natives stay total, OS failures become tag words instead of VM traps.
//! They answer with a uniform two-word tag+payload span [BL-2] on the
//! caller's argument slots 0 and 1: word 0 is the discriminant, word 1
//! the success value or the error message `Ref`. The span width is fixed
//! per native, so callers size their overlay area to two — the same
//! RETSPAN discipline — and the call site pre-checks the width before the
//! native runs (a world side effect must not happen only for the call to
//! trap afterwards).
//!
//! The discriminants are platform tag ids, and the core and platform
//! tables share the one reservation (native results are cross-module). Tag ids are
//! per-module otherwise: lowering assigns each module's union members ids
//! counting up from 0, while the platform reservation holds the top of
//! the `u8` tag-id space, counting down from [`PLATFORM_IO_OK`]; the two
//! allocations meet only once a module needs 254 ids, at which point the
//! tag id widens. `PLATFORM_IO_OK` marks every fallible native's success
//! (the caller already knows which native it called, so one shared id
//! disambiguates nothing); an IO failure answers the io class
//! discriminant of its error kind (the `io_err` alias's members, the
//! 2026-11-21 failure-taxonomy ruling), with the message `Ref` in the
//! span's payload word.
//!
//! The net ops (`crate::net`) answer the same convention over the
//! failure taxonomy's error classes: success reuses `PLATFORM_IO_OK`,
//! and a failure carries the class discriminant of the ambient
//! `net_err` case it answers by error class —
//! [`PLATFORM_NET_TIMEOUT`] or [`PLATFORM_NET_REFUSED`] — with the
//! message `Ref` in the span's payload word.
//!
//! # The string natives (indices 11-18; `len` is generic at 10)
//!
//! The pure string surface, char-indexed: `len` counts unicode scalar
//! values, and `substring`/`char_at` index chars, never bytes, so
//! multi-byte text never cuts a code point. Char positions resolve
//! through the string object's lazily built boundary map (one
//! O(length) pass per string object, never per call), so `substring`
//! and `char_at` answer in O(range) once the bounds are known.
//! Both are checked reads in `list_get`'s span shape (the
//! checked-API convention): `substring` answers the slice between the
//! two positions when `0 <= start <= end` holds inside the string's
//! char count, `char_at` the char at an in-range position, and every
//! out-of-range request answers the [`PLATFORM_NONE`] member with the
//! rejected index as the payload — never clamping, never a trap.
//!
//! `concat` is the callable face of the `STRCAT` uop: interpolation
//! chains emit STRCAT directly, while both call spellings (`a.concat(b)`
//! and string-operand `a + b`) stage the two Ref words and dispatch this
//! native through CALL, so a pool `FuncRef` naming `concat` behaves like
//! any other builtin. `split` answers a list-object `Ref` (the same list kind
//! `fs_list` fills): the surface's `char` separator reaches runtime as a
//! single-char string `Ref`, and an empty separator splits per character.
//!
//! `parse_int` is the pure fallible one: its ambient row is `error` (the
//! caller-handled failure, standing for the record's `int | parse_err`
//! exactly as `read_file`'s row stands for `string | io_err`), and it
//! answers the same uniform two-word span — [`PLATFORM_IO_OK`] with the
//! parsed `int`, or [`PLATFORM_PARSE_ERR`] (the next id below `net_err`
//! in the platform reservation's countdown) with the position the reader
//! gave up at in the payload word.
//!
//! # The list natives (indices 20-23)
//!
//! A list is one RC'd heap object — len, capacity, and the element value
//! words inline in a single object-table entry (the header's
//! `{type_id, rc}` words are M4's precise-RC slot over the same table;
//! see [`crate::object::Objects`]). Elements are plain value words, so
//! the one kind serves every element type, and a multi-word element (a
//! struct) is stored as its whole inline word span at a per-list element
//! stride taken from `list[t]`'s compile-time shape: the first append
//! fixes the stride, every later append repeats it, and the readers
//! (`list_len`, the checked `list_get` span) slice by it — so a
//! wider checked read answers through [`NativeResult::Span`], the span
//! channel, one payload word per element word with a count per `Ref`
//! word. Construction (`list_new`, `list_concat`, and the producers
//! `split`/`fs_list`) and reads (`list_len`, `list_get`) are pure
//! natives. The one in-place mutation is `list_push` (index 27): the
//! construction append list literals lower to, over a freshly built list
//! its frame uniquely owns — its staged span is variadic (the list word
//! plus the element's every word), the one native whose argument count
//! varies by call site; every other mutation stays reserved for the
//! compiler-disciplined exclusive windows, and growth is the backing
//! buffer's amortized doubling (`list_concat` reserves the joined
//! length up front).
//!
//! # The map natives (indices 28-31)
//!
//! The map surface over the builder-call interim [B3]: a map is one RC'd
//! heap object behind one `Ref` word — an association vector of key-value
//! pairs, string keys matching by content and every other word by
//! identity (see [`crate::object::Object::Map`]; no ordering or iteration
//! promise). `map_new` constructs the empty map — the `map[k, v]()`
//! constructor's runtime half — `map_len` counts entries, and `map_get`
//! is the checked read in exactly `list_get`'s span shape: in range is
//! [`PLATFORM_IO_OK`] with the value, a miss is [`PLATFORM_NONE`] with
//! the probe key as the payload (no new platform tag id: the map reuses
//! the checked-read reservation). `map_set` is the one mutation, through
//! the checker's exclusive `mut` window; it stores fresh pairs by
//! acquiring one count per `Ref` word and drops the overwritten value's
//! count, so a dying map frees with one dec per stored `Ref` word —
//! the same retain contract the list carries.
//!
//! `list_get` is the checked read, and its bounds behavior follows the
//! crate's checked-API convention — the same two-word span `parse_int`
//! answers: in range is [`PLATFORM_IO_OK`] with the element, out of
//! range (either side) is [`PLATFORM_NONE`] with the rejected index as
//! the payload. The string ops' clamping fits no generic element type
//! (there is no empty value of every `t`), and a trap would deny the
//! caller the `t | none` extraction, so the span is the one shape both
//! the caller and the checker can see.
//!
//! # The generic container closes (indices 32-37)
//!
//! The six generic container natives the ambient declares past the map
//! block, CALLG-addressed like the container reads above them: `cons`,
//! `tail`, and `collect` are producers — fresh lists over the source's
//! element words, construction copying per the D2 ruling, the copy
//! acquiring one count per `Ref` element it adopts; `first` is the
//! one checked first-element body (the retired `head` spelling's
//! historical slot at 33 stays reserved), answering
//! `list_get`'s two-word span (the hit is [`PLATFORM_IO_OK`] with the
//! element handed out under its own count, an empty list the
//! [`PLATFORM_NONE`] member with the inert unit word as payload —
//! there is no rejected index to carry); `max` answers the greater of
//! two numeric words, int in i64's total order and float IEEE (a NaN
//! operand answers the other side), any other word kind refusing like
//! a comparison against a mismatched slot. Their table slots stay
//! reserved trap entries like every generic builtin's.
//!
//! # The reentrant combinators (indices 38-40, 46)
//!
//! The lambda-taking container combinators — `map`, `filter`,
//! `reduce`, `partition` — are the first generic bodies that call user
//! code: the staged lambda parameter is a `Fn` word (the callable-word
//! classification, F2) naming a planned function, and the body runs it
//! per element through the VM's reentrancy seam (a synthetic reentry
//! frame driven to its return; see `vm::Vm::call_hosted`). `map`
//! collects each answered word into a fresh list, `filter` keeps the
//! elements whose lambda answers true (a non-bool answer refuses like
//! a mismatched slot), and `reduce` left-folds with the borrowed init's
//! whole word span as the seed — an empty list answers the init under
//! its own count per `Ref` word.
//! `partition` runs `filter`'s predicate with two destinations — a
//! true answer pushes onto the first fresh list, a false one onto the
//! second — and the two list `Ref`s answer as the tag-less multi-word
//! total: no discriminant, the two words are the tuple itself. A
//! drive that ends the fiber or parks at the host instead of
//! returning refuses honestly: a Rust-side loop cannot suspend.
//!
//! # The fork-join combinators (indices 47-48)
//!
//! The engine seams behind the @std `par_map`/`par_reduce` library
//! fns (the ds-source spellings live in the @std bodied member; these
//! natives are what their bodies call). `fork_map` [E3, the parked-CALLG
//! fork-join]: the combinator body is
//! spawn-only — fork is one spawned child per element applying the
//! callable under a fresh nursery, the callable's own function entered
//! with the element's whole inline span, the spawn copy-in acquiring
//! one count per `Ref` word for the child's frame — and the join parks
//! the CALLG site through the interlock: the drive seam
//! (`vm::DriveSeam::Fork` over `ForkStage`) carries the assembly
//! stage, the drive runs the children in spawn order so the answers
//! assemble in input order regardless of completion order
//! (deterministic replay by construction), and the assembled result
//! list delivers onto the parked call site. An element may park at a
//! yield mid-flight where the hosted `map` loop refuses — the worklist
//! bounces the parked child back in [E2]. The first failure cancels
//! the siblings at their next preemption point and re-raises at the
//! call site [E5] — exactly a spawn-body failure at its enclosing
//! boundary. The completion channel is whole-span (a `FrameAnswer`):
//! the callable's answer rides it one word or whole (a multi-word
//! RETSPAN or tail-native answer), its width the callable's
//! checker-proved signature, and the seam assembles each answer's
//! words onto the fresh result list, the first push fixing the list's
//! element stride (an answering width the checker proves). A closure
//! callable runs the same fork — the capture block staged per spawn
//! into the child's entry frame (the CALLS copy-in discipline), one
//! acquired count per `Ref` capture. Nesting (a par_map inside a
//! par_map element) parks the element's fiber at its own CALLG site,
//! the drive frames stacking on the interlock's heap stack — depth
//! costs heap, never the Rust stack.
//!
//! `par_reduce` [E3, the same park over the fold]: the fold's left
//! data dependency chains the children — each element's application is
//! one spawned child entered with the running accumulator's whole word
//! span and the element's span, the seam's chain state carrying the
//! fold between steps (a fresh nursery per step, the consumed slot
//! recycled) — so the reduction applies strictly in input order, the
//! deterministic replayable spelling whatever a later scheduler does
//! (an associative split would reorder float sums, so it waits on an
//! associativity proof). The fold type is the call's own fact: the
//! init's whole span stages between the list and the callable (the
//! interior-span discipline), the callable's checker-proved answer
//! width matching it, and the final accumulator delivers whole onto
//! the parked call site. The accumulator's counts follow `reduce`'s
//! owned discipline (the init borrowed until the first answer adopts,
//! each delivered answer owned by the joiner, each superseded word
//! dec'd at adoption); the callable is a scalar `Fn` word or a closure
//! object's `Ref` (the capture block staged per spawn exactly
//! par_map's) over any fold width — and the first failure re-raises at
//! the call site with the owned accumulator released.
//!
//! # The fiber control natives (indices 24-26)
//!
//! The fiber substrate's debug switch [E2], appended past the list block:
//! `fiber_resume` parks the caller and enters the named pool fiber with
//! the payload. It is a native so fixtures address it with an ordinary
//! pool FuncRef through `CALL` — but the dispatcher, not the body,
//! performs the switch (the body only answers the bare [`PLATFORM_NONE`]
//! bounce tag, which is also what a `CALLT` to it yields: no switch
//! exists in tail position, and what an unreachable or dead
//! `fiber_resume` target answers on the caller's overlay). The host
//! face of the same machinery is `Vm::spawn_fiber`/`Vm::resume_fiber`;
//! the substrate's own yield vocabulary is the host's suspension
//! request (`Vm::request_suspend`), never a user-spellable native.
//!
//! `nursery_join` (26) extends the switch with the nursery boundary's
//! debug face [E2]: the caller parks, the dispatcher drives the named
//! nursery's children to death (first failure cancels the rest [E5]),
//! and the call answers the failed-children count. The full per-child
//! outcomes are the host API's ([`crate::Vm::join_nursery`]); an
//! in-language surface over them lands with the checker/lowerer rounds.

use std::{
    io::Write,
    path::Path,
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The reserved native modules, the row indices, and the discriminants: the one vocabulary the
/// lowering and this VM read.
pub use ds_abi::native::BUILTIN_MODULE;
pub use ds_abi::native::{
    BUILTIN_ABS, BUILTIN_ARENA_AT, BUILTIN_ARENA_INSERT, BUILTIN_ARENA_NEW, BUILTIN_ARENA_REMOVE,
    BUILTIN_ATAN2, BUILTIN_BYTE_AT, BUILTIN_BYTE_COMPARE, BUILTIN_BYTE_LIST, BUILTIN_BYTE_OF,
    BUILTIN_BYTES_COMPARE, BUILTIN_BYTES_EQ, BUILTIN_BYTES_HASH, BUILTIN_BYTES_OF, BUILTIN_CEIL,
    BUILTIN_CHAN_NEW, BUILTIN_CHAN_RECV, BUILTIN_CHAN_RECV_DEADLINE, BUILTIN_CHAN_SEND,
    BUILTIN_CHAN_SEND_DEADLINE, BUILTIN_CHAR_AT, BUILTIN_CHAR_COMPARE, BUILTIN_CHAR_HASH,
    BUILTIN_CHAR_OF, BUILTIN_CODE_OF, BUILTIN_COLLECT, BUILTIN_CONCAT, BUILTIN_CONS,
    BUILTIN_CONTAINS, BUILTIN_COS, BUILTIN_EXP, BUILTIN_FIBER_RESUME, BUILTIN_FIBER_YIELD,
    BUILTIN_FILTER, BUILTIN_FIRST, BUILTIN_FLOAT_HASH, BUILTIN_FLOOR, BUILTIN_HEAD,
    BUILTIN_INDEX_OF, BUILTIN_INT_OF_BYTE, BUILTIN_INT_OF_U32, BUILTIN_JOIN, BUILTIN_LEN,
    BUILTIN_LIST_CLEAR, BUILTIN_LIST_CONCAT, BUILTIN_LIST_EQ, BUILTIN_LIST_GET,
    BUILTIN_LIST_INSERT, BUILTIN_LIST_LEN, BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH,
    BUILTIN_LIST_REMOVE, BUILTIN_LIST_SET, BUILTIN_LN, BUILTIN_MAP, BUILTIN_MAP_CONTAINS,
    BUILTIN_MAP_ENTRIES, BUILTIN_MAP_EQ, BUILTIN_MAP_GET, BUILTIN_MAP_KEYS, BUILTIN_MAP_LEN,
    BUILTIN_MAP_NEW, BUILTIN_MAP_REMOVE, BUILTIN_MAP_SET, BUILTIN_MAX, BUILTIN_NURSERY_BOUNDARY,
    BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_JOIN, BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN,
    BUILTIN_PAR_MAP, BUILTIN_PAR_REDUCE, BUILTIN_PARSE_FLOAT, BUILTIN_PARSE_INT, BUILTIN_PARTITION,
    BUILTIN_POW, BUILTIN_REDUCE, BUILTIN_REPL_PARK, BUILTIN_REPLACE, BUILTIN_ROUND, BUILTIN_SIN,
    BUILTIN_SORT, BUILTIN_SPLIT, BUILTIN_SQRT, BUILTIN_STR_BOOL, BUILTIN_STR_COMPARE,
    BUILTIN_STR_FIXED, BUILTIN_STR_FLOAT, BUILTIN_STR_HASH, BUILTIN_STR_INT, BUILTIN_SUBSTRING,
    BUILTIN_TAIL, BUILTIN_TAN, BUILTIN_TIMEOUT, BUILTIN_TO_LOWER, BUILTIN_TO_UPPER,
    BUILTIN_TOTAL_COMPARE, BUILTIN_TRAP, BUILTIN_TRIM, BUILTIN_TRUNC, BUILTIN_U32_OF,
    BUILTIN_UTF8_DECODE, BUILTIN_UTF8_ENCODE, NURSERY_BOUNDARY_ERR, NURSERY_BOUNDARY_OK,
    PLATFORM_CANCELLED, PLATFORM_IO_ALREADY_EXISTS, PLATFORM_IO_NOT_FOUND, PLATFORM_IO_OK,
    PLATFORM_IO_OTHER, PLATFORM_IO_PERMISSION_DENIED, PLATFORM_IO_TIMED_OUT,
    PLATFORM_IO_UNEXPECTED_EOF, PLATFORM_NET_ADDR_IN_USE, PLATFORM_NET_OTHER, PLATFORM_NET_REFUSED,
    PLATFORM_NET_RESET, PLATFORM_NET_TIMEOUT, PLATFORM_NONE, PLATFORM_PARSE_ERR, PLATFORM_UTF8_ERR,
    STD_PLATFORM_NAME,
};
use ds_abi::{NativeModule, SlotType, Staging};

use crate::{
    fiber::FiberId,
    net::Connections,
    nursery::NurseryId,
    object::Heaps,
    value::{FuncRef, RefId, Value},
    vm::{Trap, Vm},
};

/// Whether a platform discriminant is one of the io failure classes.
#[must_use]
pub fn is_io_class(tag: u8) -> bool {
    matches!(
        tag,
        PLATFORM_IO_NOT_FOUND
            | PLATFORM_IO_PERMISSION_DENIED
            | PLATFORM_IO_ALREADY_EXISTS
            | PLATFORM_IO_TIMED_OUT
            | PLATFORM_IO_UNEXPECTED_EOF
            | PLATFORM_IO_OTHER
    )
}

/// Whether a platform discriminant is one of the net failure classes.
#[must_use]
pub fn is_net_class(tag: u8) -> bool {
    matches!(
        tag,
        PLATFORM_NET_TIMEOUT
            | PLATFORM_NET_REFUSED
            | PLATFORM_NET_ADDR_IN_USE
            | PLATFORM_NET_RESET
            | PLATFORM_NET_OTHER
    )
}

/// The control natives of the fiber substrate [E2]: the dispatcher
/// intercepts them at `CALL` sites and performs the switch itself — the
/// body only ever runs from a `CALLT` site (tail position), where no
/// switch is possible, answering the bare `PLATFORM_NONE` tag as the
/// bounce result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlNative {
    /// `fiber_resume(target, value)`: park the current fiber; enter `target`
    /// with `value`. An absent, dead, or self target answers the bare
    /// `PLATFORM_NONE` tag instead (the checked-read convention of
    /// `list_get`, one word without payload).
    Resume,
    /// `nursery_join(nursery)`: park the current fiber and drive the
    /// nursery's children to death, then re-enter the joiner with the
    /// failed-children count. An absent or consumed nursery answers the
    /// bare `PLATFORM_NONE` tag; a child joining its own nursery traps
    /// (the driver would deadlock on itself).
    NurseryJoin,
    /// `nursery_open()`: open a fresh block-scoped nursery and answer its
    /// id. No switch happens; the answer overlays like an ordinary
    /// native's.
    NurseryOpen,
    /// `nursery_spawn(nursery, child)`: spawn the callable's function as
    /// a ready fiber into the open nursery (its captures become the entry
    /// arguments). No switch happens; the answer is the inert unit word.
    NurserySpawn,
    /// `nursery_boundary(nursery, mask)`: the in-language join — park the
    /// caller, drive the nursery to death, re-enter the joiner with the
    /// boundary span (a clean join's collected answers list, or the
    /// first failure's error payload contained in the staged mask); a
    /// real child trap or an uncontained payload re-raises on the
    /// joiner.
    NurseryBoundary,
    /// `nursery_cancel(payload)`: the current fiber dies trapped carrying
    /// the payload; the trap path raises its nursery's cancel flag. The
    /// call never answers.
    NurseryCancel,
    /// `repl_park(words...)`: the REPL's resumed-frame seam (every call site
    /// is spliced into the entry function's body, never lowered from
    /// source): park the current fiber and hand the host drive the
    /// first staged word — the submission's answer. The host re-enters
    /// at the next submission's code with the inert unit as the call's
    /// answer, a word no later instruction reads.
    ReplPark,
}

/// The channel natives (the 2026-11-21 shared-state ruling): like the
/// control natives, the dispatcher intercepts them at `CALL` and `CALLT`
/// sites — the channel step needs the scheduler's run queue and the
/// pool's fiber states, which a native body never sees — so the table
/// bodies are placeholders that refuse if some path ever runs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelNative {
    /// `chan_new(cap)`: allocate the channel object on the calling
    /// fiber's store.
    New,
    /// `chan_send(ch, x)`: deposit or park on full.
    Send,
    /// `chan_recv(ch)`: take off the head or park on empty.
    Recv,
    /// `chan_send_deadline(ch, nanos, x)`: deposit, or park on full
    /// with the deadline armed on the wheel — the loser's entry
    /// deregisters from the sender queue and the parked call answers
    /// the io taxonomy's `timed_out` span.
    SendDeadline,
    /// `chan_recv_deadline(ch, nanos)`: take off the head, or park on
    /// empty with the deadline armed on the wheel — the loser's entry
    /// deregisters from the receiver queue and the parked call answers
    /// the same span.
    RecvDeadline,
}

/// The timer native: like the channel natives, the dispatcher intercepts
/// it at `CALL` and `CALLT` sites — the race needs the run queue, the
/// deadline wheel, and the cancel cut, none of which a native body sees —
/// so the table body is a placeholder that refuses if some path ever runs
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerNative {
    /// `timeout(nanos, thunk)`: spawn the callable as the racer, park the
    /// caller on the deadline.
    Timeout,
}

/// A native's result: one word on the caller's overlay slot 0, a
/// fallible native's two-word tag+payload span on overlay slots 0 and 1,
/// the span channel — a discriminant plus a multi-word payload on
/// overlay slots `1 ..= n` (the checked collection reads handing out an
/// inline element span) — or the tag-less total, N words on overlay
/// slots `0 .. n` with no discriminant (a multi-word answer that is no
/// member's payload, `partition`'s two list `Ref`s).
#[derive(Clone, Debug, PartialEq)]
pub enum NativeResult {
    /// A single value word: the total natives' only shape.
    Word(Value),
    /// The span: the discriminant word ([`PLATFORM_IO_OK`] or a failure
    /// tag) plus the one payload word.
    Tag {
        /// The span's discriminant.
        tag: u8,
        /// The success value, or the error message `Ref`.
        payload: Value,
    },
    /// The span channel: the discriminant word plus a multi-word payload
    /// — one element's whole inline word span, overlaid on slots
    /// `0 ..= payload.len()`. The one-payload-word shape stays
    /// [`NativeResult::Tag`] (no allocation on the one-word path), so
    /// this variant exists exactly where the payload spans words.
    Span {
        /// The span's discriminant.
        tag: u8,
        /// The payload words, in element span order.
        payload: Vec<Value>,
    },
    /// The tag-less total: the answer's whole word span with no
    /// discriminant, overlaid on slots `0 .. words.len()` — the
    /// degenerate sibling of [`NativeResult::Span`] (the same payload
    /// staging without the leading tag word). A multi-word answer that
    /// is no union member's payload answers here (`partition`'s two
    /// list `Ref`s; the fold's whole-span accumulator answers the same
    /// way).
    Total {
        /// The answer's words, in tuple order.
        words: Vec<Value>,
    },
    /// A fork-join combinator's park request: the spawn phase ran (the
    /// children stand in `nursery`) and the join parks — the
    /// assembly/fold stage rides `stage`, the drive seam's consumption
    /// running it and the delivery answering the parked call site. Only
    /// the CALLG generic dispatch produces it; a plain native never
    /// answers it, and a tail dispatch site refuses the fork before
    /// its spawn phase runs.
    Fork {
        /// The freshly opened nursery holding the spawned children.
        nursery: NurseryId,
        /// The assembly or fold stage the join runs.
        stage: ForkStage,
    },
    /// A world op's park request (the 2026-11-21 scheduler ruling): the
    /// op would block, so the calling fiber parks instead — the drive
    /// registers the wait, runs the next ready fiber, and the reactor's
    /// readiness (or the wait's deadline) completes the op and delivers
    /// its answer at the parked site. Only a net native parked on a
    /// VM with a reactor installed answers it; the parking site and
    /// the drivers that register or refuse the wait live in
    /// `crate::vm`.
    Park(crate::sched::WorldWait),
}

impl NativeResult {
    /// The single-word shape, or `None` for a tag+payload span.
    #[must_use]
    pub fn word(self) -> Option<Value> {
        match self {
            Self::Word(v) => Some(v),
            Self::Tag { .. }
            | Self::Span { .. }
            | Self::Total { .. }
            | Self::Fork { .. }
            | Self::Park(_) => None,
        }
    }

    /// How many overlay words the result spans, discriminant included.
    #[must_use]
    pub fn overlay_words(&self) -> usize {
        match self {
            Self::Word(_) => 1,
            Self::Tag { .. } => 2,
            Self::Span { payload, .. } => 1 + payload.len(),
            Self::Total { words } => words.len(),
            // a park request never overlays: the dispatcher parks the
            // site instead, and the CALLTG site refuses the fork before
            // its body runs
            Self::Fork { .. } => unreachable!("a park request never overlays"),
            // a world park never overlays either: the wake delivers the
            // completed op's answer at the resume point
            Self::Park(_) => unreachable!("a world park never overlays"),
        }
    }
}

/// The native calling convention: `args` are the staged word values
/// (already type-checked against `params` by the call site — a variadic
/// native sees the whole staged span, its `params` covering the named
/// prefix), the result overlays the caller's argument slots like a RET
/// (one word, a fallible native's two-word span, or a span channel's
/// discriminant plus multi-word payload).
pub type NativeFn = fn(&mut Heaps, &mut OutputSink, &[Value]) -> Result<NativeResult, Trap>;

/// One native function: a name, its parameter slot types, the overlay
/// width its result needs, the body.
#[derive(Debug)]
pub struct NativeFunction {
    /// The name, for trap messages and diagnostics.
    pub name: &'static str,
    /// The parameter slot types; the call site checks the staged words
    /// against these exactly like a uop callee's layout. A `Raw`
    /// parameter accepts every word kind — the wildcard for natives
    /// whose argument type varies by call site (`list_push`'s element).
    pub params: &'static [SlotType],
    /// Whether the call may stage more words than `params` names: the
    /// staged span past the named parameters is one element's whole
    /// inline word span, its width the call site's static fact
    /// (`list_push`'s element). The dispatcher's arity check relaxes to
    /// "at least", and the body owns the tail's contract.
    pub variadic: bool,
    /// How many overlay words the result needs: 1 for a single word, 2 for
    /// a fallible native's tag+payload span or a checked read. The static
    /// floor, pre-checked at the call site against the caller's staging
    /// width — a span-channel answer may exceed it by the element's
    /// width, which the overlay write bounds-checks at runtime (the
    /// lowering sizes the area, RETSPAN's discipline).
    pub result_words: u8,
    /// Marks the two fiber-control natives the dispatcher intercepts at
    /// `CALL` sites [E2]; `None` for every ordinary native.
    pub control: Option<ControlNative>,
    /// Marks the channel natives the dispatcher intercepts at `CALL`
    /// and `CALLT` sites (the channel step is the dispatcher's, like
    /// the control switch); `None` for every ordinary native.
    pub channel: Option<ChannelNative>,
    /// Marks the timer native the dispatcher intercepts at `CALL` and
    /// `CALLT` sites (the race is the dispatcher's, like the channel
    /// step); `None` for every ordinary native.
    pub timer: Option<TimerNative>,
    /// The body.
    pub func: NativeFn,
}

fn substring_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let start = i64_arg(args, 1)?;
    let end = i64_arg(args, 2)?;
    let n = i64::try_from(heaps.char_count(id)?).expect("string length fits i64");
    // the checked-API convention (the span shape `parse_int` answers):
    // the slice in range — `0 <= start <= end <= n`, so both offsets sit
    // on char boundaries and the cut cannot split a code point — answers
    // the payload Ref, every out-of-range shape (either side, inverted)
    // the `none` member with the rejected `start`, never clamping
    if !(0 <= start && start <= end && end <= n) {
        return Ok(NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(start),
        });
    }
    if start == end {
        return Ok(NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str_part(""),
        });
    }
    let (begin, stop) = heaps.char_byte_range(
        id,
        usize::try_from(start).expect("in-range start fits usize"),
        usize::try_from(end).expect("in-range end fits usize"),
    )?;
    let part = heaps.alloc_substr(id, begin, stop)?;
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: heaps.word(part),
    })
}

fn total_compare_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(a), Value::F64(b)] => {
            Ok(NativeResult::Word(Value::I64(match a.total_cmp(b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })))
        },
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn str_compare_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.str_ref(ref_arg(args, 0)?)?;
    let b = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::I64(
        match a.as_bytes().cmp(b.as_bytes()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    )))
}

fn char_compare_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.str_ref(ref_arg(args, 0)?)?;
    let b = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::I64(
        match a.chars().cmp(b.chars()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    )))
}

fn byte_compare_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::I64(
        match i64_arg(args, 0)?.cmp(&i64_arg(args, 1)?) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    )))
}

/// The one content-hash algorithm the hash family uses: FNV-1a over the
/// bytes (offset basis 14695981039346656037, prime 1099511628211, both
/// wrapping in u64) — the same pair the derive's fold mixes through, so
/// every primitive part hashes under one scheme.
// the fold's answer is the full 64-bit pattern reinterpreted as the
// content-hash word
#[allow(clippy::cast_possible_wrap)]
fn fnv1a(bytes: &[u8]) -> i64 {
    let mut hash = 14_695_981_039_346_656_037u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash as i64
}

fn str_hash_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the operand borrows shared, so the hash reads without a copy
    let text = heaps.str_ref(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::I64(fnv1a(text.as_bytes()))))
}

fn float_hash_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(f)] => {
            // the fold: -0.0 shares +0.0's bits and every NaN payload the
            // one canonical NaN's, so bit-structurally equal floats hash
            // equal (B9)
            let canonical = if f.is_nan() {
                f64::NAN
            } else if *f == 0.0 {
                0.0
            } else {
                *f
            };
            Ok(NativeResult::Word(Value::I64(fnv1a(
                &canonical.to_bits().to_le_bytes(),
            ))))
        },
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn char_hash_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the routed read borrows the store, so the operand copies out
    let text = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(NativeResult::Word(Value::I64(fnv1a(text.as_bytes()))))
}

fn concat_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let joined = heaps.concat(ref_arg(args, 0)?, ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(heaps.word(joined)))
}

fn to_upper_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?.to_uppercase();
    Ok(NativeResult::Word(heaps.new_str(text)))
}

fn to_lower_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?.to_lowercase();
    Ok(NativeResult::Word(heaps.new_str(text)))
}

fn trim_fn(heaps: &mut Heaps, _out: &mut OutputSink, args: &[Value]) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    // the trimmed byte range, measured off the borrowed text: the fresh
    // object copies that range once and no intermediate string exists
    let (begin, stop) = {
        let text = heaps.str_at(id)?;
        let begin = text.len() - text.trim_start().len();
        let stop = text.trim_end().len();
        (begin, stop)
    };
    // an all-whitespace subject trims to the empty range
    if begin >= stop {
        return Ok(NativeResult::Word(heaps.new_str_part("")));
    }
    let part = heaps.alloc_substr(id, begin, stop)?;
    Ok(NativeResult::Word(heaps.word(part)))
}

fn parse_int_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let text = heaps.str_at(id)?;
    Ok(match text.parse::<i64>() {
        // strict: no implicit whitespace handling (the `trim` native composes)
        Ok(v) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(v),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_PARSE_ERR,
            payload: Value::I64(int_stop(text)),
        },
    })
}

/// The position an integer reader gives up at: the first char the
/// grammar refuses, or the text's end when the shape held but the value
/// did not (an overflow) or the digits are missing past the sign.
fn int_stop(text: &str) -> i64 {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        // no digits at all: the reader stopped where the digits should
        // have started (the empty text answers 0)
        return i64::try_from(i).expect("the index fits i64");
    }
    // past the digits: either the first char the grammar refuses, or the
    // end of a well-shaped text whose value overflowed
    i += digits;
    i64::try_from(i).expect("the index fits i64")
}

/// `parse_float(s: string) -> float | parse_err` — `parse_int`'s twin
/// over the float slot: the correctly-rounded decimal reader, strict
/// about spelling, answering `parse_err` with the position it gave up at
/// as the payload on refusal. The @std json parser's float arm routes
/// through here, so a parsed float carries the same bits any wider
/// ecosystem reader would give it.
fn parse_float_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let text = heaps.str_at(id)?;
    Ok(match text.parse::<f64>() {
        // strict: no implicit whitespace handling (the `trim` native composes)
        Ok(v) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::F64(v),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_PARSE_ERR,
            payload: Value::I64(float_stop(text)),
        },
    })
}

/// The position a float reader gives up at: the first char the decimal
/// grammar refuses (sign, mantissa digits with one dot, exponent), or
/// the text's end when the shape held but the value did not. The
/// non-decimal spellings (`inf`, `nan`) scan as their word prefixes.
fn float_stop(text: &str) -> i64 {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let lower: Vec<u8> = text.to_ascii_lowercase().as_bytes()[i..].to_vec();
    for word in [b"inf".as_slice(), b"infinity".as_slice(), b"nan".as_slice()] {
        if lower.len() >= word.len() && &lower[..word.len()] == word {
            return i64::try_from(text.len()).expect("the index fits i64");
        }
    }
    // where the mantissa's first digit was required: a text with no
    // digit anywhere stops there (a bare dot is not a number)
    let digits_at = i;
    let mut mantissa = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        mantissa += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            mantissa += 1;
        }
    }
    if mantissa == 0 {
        return i64::try_from(digits_at).expect("the index fits i64");
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
            i += 1;
        }
        let exp_digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        if exp_digits == 0 {
            return i64::try_from(i).expect("the index fits i64");
        }
        i += exp_digits;
    }
    i64::try_from(i).expect("the index fits i64")
}

/// `char_of(code: int) -> char | none` — the codepoint→char reader:
/// the one seam a ds-source `\u` decoder needs (codepoint arithmetic
/// stays in ds, the scalar-width UTF-8 encoding stays native). A `code`
/// outside the scalar range answers `none`, the payload word carrying
/// the rejected code.
fn char_of_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let code = i64_arg(args, 0)?;
    Ok(match u32::try_from(code).ok().and_then(char::from_u32) {
        Some(c) => {
            let mut buf = [0u8; 4];
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: heaps.new_str_part(c.encode_utf8(&mut buf)),
            }
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(code),
        },
    })
}

/// `code_of(c: char) -> int` — `char_of`'s twin, the char→codepoint
/// reader the @std json serializer's control-char escaping spells its
/// `\u00XX` forms through. A malformed word (an empty or multi-char
/// string riding the char Ref) traps: the checker types the operand,
/// so only a hand-assembled frame reaches it.
fn code_of_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let text = heaps.str_at(id)?;
    // a char word is a one-char string by construction (the literal
    // lowering and `char_of` both build exactly that), so only a
    // hand-assembled frame reaches the empty case — the multi-char case
    // reads the first scalar rather than trap, matching how the string
    // natives treat a char word leniently
    let code = text
        .chars()
        .next()
        .ok_or(Trap::UnknownObject { obj: id.id() })?;
    Ok(NativeResult::Word(Value::I64(i64::from(u32::from(code)))))
}

/// `bytes_of(xs: list[byte]) -> bytes` — the byte string's construction
/// seam: the byte list folds into one counted block, one payload byte
/// per element word. The list keeps its own counts (a borrow exactly
/// like every native staging), and the bytes word's birth count is the
/// calling frame's. An element word outside `0..=255` traps: the
/// checker types the list, so only a hand-assembled frame reaches it.
fn bytes_of_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let elems = {
        let (elems, stride) = heaps.list_at(id)?;
        if stride > 1 {
            return Err(Trap::ElementStride {
                obj: id.id(),
                stride,
                given: 1,
            });
        }
        elems.to_vec()
    };
    let mut payload = Vec::with_capacity(elems.len());
    for word in elems {
        let Value::I64(byte) = word else {
            return Err(Trap::SlotTypeMismatch {
                slot: 0,
                expected: SlotType::I64,
                actual: word.slot_type(),
            });
        };
        payload.push(u8::try_from(byte).map_err(|_| Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: SlotType::I64,
        })?);
    }
    Ok(NativeResult::Word(
        heaps.new_bytes(payload.into_boxed_slice()),
    ))
}

/// `byte_list(b: bytes) -> list[byte]` — the bridge back out: one fresh
/// list, one word per payload byte, the bytes keeping their own count.
fn byte_list_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let elems: Vec<Value> = heaps
        .bytes_at(id)?
        .iter()
        .map(|byte| Value::I64(i64::from(*byte)))
        .collect();
    Ok(NativeResult::Word(heaps.new_list(elems)))
}

/// `byte_at(b: bytes, i: int) -> byte | none` — the checked indexed read,
/// the ok/none span shape `list_get` answers over the one-word byte
/// payload: the in-range byte under the success discriminant, every
/// out-of-range shape the `none` member with the rejected index —
/// never clamping and never a trap.
fn byte_at_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let index = i64_arg(args, 1)?;
    let payload = heaps.bytes_at(id)?;
    Ok(
        match usize::try_from(index).ok().filter(|&at| at < payload.len()) {
            Some(at) => NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(i64::from(payload[at])),
            },
            None => NativeResult::Tag {
                tag: PLATFORM_NONE,
                payload: Value::I64(index),
            },
        },
    )
}

/// `byte_of(n: int) -> byte | none` — the checked byte construction: the
/// number's word under the ok discriminant, none outside 0-255 with the
/// rejected number in the payload (byte_at's span shape).
fn byte_of_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let n = i64_arg(args, 0)?;
    Ok(match u8::try_from(n) {
        Ok(b) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::from(b)),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(n),
        },
    })
}

/// `int_of_byte(b: byte) -> int` — the byte's 0-255 number.
fn int_of_byte_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let b = i64_arg(args, 0)?;
    Ok(NativeResult::Word(Value::I64(b)))
}

/// `u32_of(n: int) -> u32 | none` — the checked u32 construction: the
/// number's word under the ok discriminant, none outside the u32 range.
fn u32_of_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let n = i64_arg(args, 0)?;
    Ok(match u32::try_from(n) {
        Ok(u) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::from(u)),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(n),
        },
    })
}

/// `int_of_u32(v: u32) -> int` — the u32's number.
fn int_of_u32_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let v = i64_arg(args, 0)?;
    Ok(NativeResult::Word(Value::I64(v)))
}

/// The float math family's one-argument shape (the 2026-11-21
/// pure-builtin round): IEEE over the f64 word, total by construction —
/// no operand shape traps (a negative `sqrt` answers NaN).
macro_rules! float_unary {
    ($name:ident, $op:expr) => {
        fn $name(
            _heaps: &mut Heaps,
            _out: &mut OutputSink,
            args: &[Value],
        ) -> Result<NativeResult, Trap> {
            Ok(NativeResult::Word(Value::F64($op(f64_arg(args, 0)?))))
        }
    };
}

float_unary!(sqrt_fn, f64::sqrt);
float_unary!(sin_fn, f64::sin);
float_unary!(cos_fn, f64::cos);
float_unary!(floor_fn, f64::floor);
float_unary!(ceil_fn, f64::ceil);
float_unary!(abs_fn, f64::abs);
float_unary!(round_fn, f64::round);
float_unary!(trunc_fn, f64::trunc);
float_unary!(log_fn, f64::ln);
float_unary!(exp_fn, f64::exp);
float_unary!(tan_fn, f64::tan);

/// `atan2(y: float, x: float) -> float` — the two-argument arctangent:
/// the quadrant reads off both operands' signs, the signed zeros
/// included (`atan2(0, -1)` answers pi, `atan2(-0, -1)` negative pi).
fn atan2_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let y = f64_arg(args, 0)?;
    let x = f64_arg(args, 1)?;
    Ok(NativeResult::Word(Value::F64(y.atan2(x))))
}

/// `pow(base: float, exp: float) -> float` — the exponentiation,
/// `powf`'s IEEE edge answers (a negative base to a non-integer
/// exponent answers NaN, never a trap).
fn pow_fn(_heaps: &mut Heaps, _out: &mut OutputSink, args: &[Value]) -> Result<NativeResult, Trap> {
    let base = f64_arg(args, 0)?;
    let exp = f64_arg(args, 1)?;
    Ok(NativeResult::Word(Value::F64(base.powf(exp))))
}

/// `contains(haystack, needle: string) -> bool` — the string search
/// family's membership read (the 2026-11-21 pure-builtin round): the
/// needle occurs in the haystack, the empty needle always occurring.
/// Both operands borrow shared, so neither side copies out.
fn contains_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let haystack = heaps.str_ref(ref_arg(args, 0)?)?;
    let needle = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::Bool(haystack.contains(needle))))
}

/// `index_of(haystack, needle: string) -> int | none` — the checked
/// search: the first occurrence's char index under the ok discriminant
/// (`substring`/`char_at`'s indexing, so the answer composes with the
/// checked slice), a miss the `none` member with the inert unit word as
/// payload. The empty needle sits at index 0.
fn index_of_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let haystack = heaps.str_ref(ref_arg(args, 0)?)?;
    let needle = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(match haystack.find(needle) {
        // find's byte offset sits on a char boundary
        Some(byte) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(
                i64::try_from(haystack[..byte].chars().count()).expect("char count fits i64"),
            ),
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
    })
}

/// `replace(haystack, needle, with: string) -> string` — every
/// non-overlapping occurrence swapped for `with` in one left-to-right
/// pass, the inserted text never re-scanned; no match answers the
/// input's own text, and the empty needle inserts `with` at every char
/// boundary.
fn replace_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let replaced = {
        let haystack = heaps.str_ref(ref_arg(args, 0)?)?;
        let needle = heaps.str_ref(ref_arg(args, 1)?)?;
        let with = heaps.str_ref(ref_arg(args, 2)?)?;
        haystack.replace(needle, with)
    };
    Ok(NativeResult::Word(heaps.new_str(replaced)))
}

/// `bytes_eq(a, b: bytes) -> bool` — the structural content compare
/// [B9]: the payloads compare byte for byte, a length mismatch a
/// certain false. Both operands stay borrowed (the native call
/// convention's staging owns no count).
fn bytes_eq_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.bytes_ref(ref_arg(args, 0)?)?;
    let b = heaps.bytes_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::Bool(a == b)))
}

/// `bytes_compare(a, b: bytes) -> int` — the bytes sort key: the
/// lexicographic order over the payloads, a shorter prefix ordering
/// first. Both operands stay borrowed.
fn bytes_compare_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.bytes_ref(ref_arg(args, 0)?)?;
    let b = heaps.bytes_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::I64(match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })))
}

/// `bytes_hash(b: bytes) -> int` — the bytes content hash, FNV-1a over
/// the payload under the hash family's one scheme.
fn bytes_hash_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the operand borrows shared, so the hash reads without a copy
    let payload = heaps.bytes_ref(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::I64(fnv1a(payload))))
}

/// `utf8_encode(s: string) -> bytes` — the string→bytes bridge: the
/// string's own UTF-8 bytes become one fresh counted block, the string
/// keeping its own count.
fn utf8_encode_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let payload = heaps.str_at(ref_arg(args, 0)?)?.as_bytes().to_vec();
    Ok(NativeResult::Word(
        heaps.new_bytes(payload.into_boxed_slice()),
    ))
}

/// `utf8_decode(b: bytes) -> string | utf8_err` — the bytes→string
/// bridge: the payload validates as UTF-8 on the way back, an invalid
/// sequence answering the taxonomy's `utf8_err` tag with the rejection
/// message in the payload word.
fn utf8_decode_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the routed read borrows the store, so the operand copies out
    let payload = heaps.bytes_at(ref_arg(args, 0)?)?.to_vec();
    match String::from_utf8(payload) {
        Ok(text) => Ok(NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(text),
        }),
        Err(_) => Ok(NativeResult::Tag {
            tag: PLATFORM_UTF8_ERR,
            payload: heaps.new_str_part("invalid utf-8 payload"),
        }),
    }
}

/// `list_eq(a, b: list) -> bool` — the list collection's structural
/// content compare [B9]: the content walk over both objects' element
/// words, every nested reference pair compared the same way. Both
/// operands stay borrowed (the native call convention's staging owns no
/// count).
fn list_eq_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Bool(
        heaps.content_eq(ref_arg(args, 0)?, ref_arg(args, 1)?)?,
    )))
}

/// `map_eq(a, b: map) -> bool` — the map collection's structural content
/// compare [B9]: the content walk over both objects' rows, key lookup the
/// face `map_get` serves and value spans compared word for word. Both
/// operands stay borrowed.
fn map_eq_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Bool(
        heaps.content_eq(ref_arg(args, 0)?, ref_arg(args, 1)?)?,
    )))
}

/// How `split` cuts the subject. The surface separator is a `char` (one
/// scalar value), and an empty one splits per character rather than trap.
enum SplitSep {
    /// An empty separator: one part per character. The lowerer types the
    /// separator `char` and every char word is a one-char string by
    /// construction, so lowered code never stages the empty spelling: the
    /// arm answers hand-assembled frames riding an empty string over the
    /// operand (the lenient seam `code_of` takes).
    PerChar,
    /// One ASCII character: the split scans the subject's bytes once.
    Byte(char),
    /// Any other separator text, owned because the store's routed string
    /// read borrows the whole store mutably.
    Text(String),
}

fn split_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text_id = ref_arg(args, 0)?;
    let sep = match heaps.str_at(ref_arg(args, 2)?)? {
        "" => SplitSep::PerChar,
        sep if sep.len() == 1 => SplitSep::Byte(sep.chars().next().expect("one byte is one char")),
        sep => SplitSep::Text(sep.to_owned()),
    };
    // the part boundaries come off one borrow of the subject; the parts
    // materialize after it ends
    let mut bounds: Vec<(usize, usize)> = Vec::new();
    {
        let text = heaps.str_at(text_id)?;
        let mut begin = 0;
        match &sep {
            SplitSep::PerChar => {
                for part in text.chars() {
                    bounds.push((begin, begin + part.len_utf8()));
                    begin += part.len_utf8();
                }
            },
            SplitSep::Byte(sep) => {
                for part in text.split(*sep) {
                    bounds.push((begin, begin + part.len()));
                    begin += part.len() + 1;
                }
            },
            SplitSep::Text(sep) => {
                for part in text.split(sep.as_str()) {
                    bounds.push((begin, begin + part.len()));
                    begin += part.len() + sep.len();
                }
            },
        }
    }
    // every boundary sits on a char boundary, so each part copies its own
    // byte range once
    let mut elems = Vec::with_capacity(bounds.len());
    for (begin, stop) in bounds {
        let id = heaps.alloc_substr(text_id, begin, stop)?;
        elems.push(heaps.word(id));
    }
    // each fresh part's birth count becomes the list's edge, so the
    // frame claim its alloc entered leaves with the adoption
    for elem in &elems {
        if let Value::Ref(word) = *elem {
            heaps.release_claim(word);
        }
    }
    Ok(NativeResult::Word(heaps.new_list(elems)))
}

fn char_at_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let index = i64_arg(args, 1)?;
    // the checked char read in `list_get`'s span shape: an in-range index
    // answers the one-scalar char word (the hit payload a one-char
    // string Ref rides, exactly the word `char_of` mints), either side
    // out of range the `none` member with the rejected index — never a
    // trap
    let n = heaps.char_count(id)?;
    if index < 0 || usize::try_from(index).unwrap_or(usize::MAX) >= n {
        return Ok(NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(index),
        });
    }
    let i = usize::try_from(index).expect("in-range index fits usize");
    let (begin, stop) = heaps.char_byte_range(id, i, i + 1)?;
    let part = heaps.alloc_substr(id, begin, stop)?;
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: heaps.word(part),
    })
}

fn join_fn(heaps: &mut Heaps, _out: &mut OutputSink, args: &[Value]) -> Result<NativeResult, Trap> {
    let sep = heaps.str_at(ref_arg(args, 2)?)?.to_owned();
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    // one string Ref per element is the surface's contract (list[string]);
    // a wider stride reaches here only hand-assembled
    if stride > 1 {
        return Err(Trap::ElementStride {
            obj: ref_arg(args, 0)?.id(),
            stride,
            given: 1,
        });
    }
    // the element words copy out of the list's borrow: the routed string
    // reads below borrow the whole store mutably
    let elems: Vec<Value> = elems.to_vec();
    // the joined text's exact byte length first, so its buffer is
    // allocated once at the final size: growth would copy the text again,
    // and the fresh object's exact-size conversion would shrink it
    let mut capacity = sep.len() * elems.len().saturating_sub(1);
    for (at, part) in elems.iter().enumerate() {
        capacity += heaps.str_at(join_element(*part, at)?)?.len();
    }
    let mut joined = String::with_capacity(capacity);
    for (at, part) in elems.iter().enumerate() {
        if at > 0 {
            joined.push_str(&sep);
        }
        joined.push_str(heaps.str_at(join_element(*part, at)?)?);
    }
    Ok(NativeResult::Word(heaps.new_str(joined)))
}

/// A join element's string word, refusing at the element's own position:
/// the checked surface guarantees `list[string]`, so only a hand-assembled
/// caller reaches the miss.
fn join_element(part: Value, at: usize) -> Result<RefId, Trap> {
    match part {
        Value::Ref(word) => Ok(word),
        other => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(at).expect("element index fits u8"),
            expected: SlotType::Ref,
            actual: other.slot_type(),
        }),
    }
}

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
fn list_new_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(heaps.new_list(Vec::new())))
}

fn list_len_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    // stride 0 iff empty, so the division is exact and the empty list
    // answers 0 whatever elements it would take
    let n = i64::try_from(elems.len() / stride.max(1)).expect("list length fits i64");
    Ok(NativeResult::Word(Value::I64(n)))
}

fn list_get_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let index = i64_arg(args, 1)?;
    // Bounds follow the crate's checked-API convention (the span shape
    // `parse_int` answers), not the string ops' clamping: a generic
    // element type has no benign total answer, and a trap would deny the
    // caller the `t | none` extraction. A negative index fails the
    // usize conversion, so both out-of-range sides meet in `None`. The
    // hit is one element's whole inline span — `stride` words — handed
    // out under one count per `Ref` word (the one-word stride keeps the
    // allocation-free Tag shape).
    let hit = usize::try_from(index)
        .ok()
        .filter(|&at| at < elems.len() / stride.max(1))
        .map(|at| &elems[at * stride..(at + 1) * stride]);
    Ok(match hit {
        Some([only]) => {
            // the one-word stride reads through the `Tag` shape: no span
            // buffer, and the slice dies before the count goes on
            let word = *only;
            native_result_word(heaps, word)?
        },
        Some(words) => {
            // the slice dies with the list borrow before the retains run
            let words = words.to_vec();
            native_result_span(heaps, words)?
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(index),
        },
    })
}

/// A checked read's one-word hit: the element word handed out under one
/// count when it is a `Ref` (the list keeps its own, C4) — the
/// allocation-free `Tag` shape.
fn native_result_word(heaps: &mut Heaps, word: Value) -> Result<NativeResult, Trap> {
    if let Value::Ref(id) = word {
        heaps.inc(id)?;
    }
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: word,
    })
}

/// A checked read's wider hit (two or more words): the element's whole
/// word span handed out with one count per `Ref` word for the reader (the
/// list keeps its own, C4). The vec moves into the span payload, so the
/// span the caller sliced out of the store is the copy's only one.
fn native_result_span(heaps: &mut Heaps, words: Vec<Value>) -> Result<NativeResult, Trap> {
    for value in &words {
        if let Value::Ref(word) = *value {
            heaps.inc(word)?;
        }
    }
    Ok(NativeResult::Span {
        tag: PLATFORM_IO_OK,
        payload: words,
    })
}

fn list_concat_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let joined = heaps.concat_lists(ref_arg(args, 0)?, ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(heaps.word(joined)))
}

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
fn list_push_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let list = ref_arg(args, 0)?;
    // the variadic tail is the element's whole inline word span; the
    // first push fixes the list's stride, a disagreeing later push traps
    heaps.push_list(list, &args[1..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
fn map_new_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(heaps.new_map()))
}

fn map_len_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let n = i64::try_from(heaps.map_len(ref_arg(args, 0)?)?).expect("map length fits i64");
    Ok(NativeResult::Word(Value::I64(n)))
}

/// `map_keys[k, v](map) -> list[k]`: the ordered key face — the map's
/// entry order (the order rows were first stored), one word per key,
/// the fresh list's edge owning one count per `Ref` key (the sources
/// keep their own, C4).
fn map_keys_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let keys = heaps.map_keys(ref_arg(args, 0)?)?;
    for key in &keys {
        if let Value::Ref(word) = *key {
            heaps.inc_edge(word)?;
        }
    }
    Ok(NativeResult::Word(heaps.new_list_strided(keys, 1)))
}

fn map_get_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let map = ref_arg(args, 0)?;
    let hit = heaps.map_at(map, args[1])?;
    Ok(match hit.as_deref() {
        Some([only]) => {
            // the one-word stride reads through the `Tag` shape: no span
            // buffer, and the slice dies before the count goes on
            if let Value::Ref(word) = *only {
                heaps.inc(word)?;
            }
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: *only,
            }
        },
        Some(words) => {
            // the read hands the value span out: a Ref word gains a count
            // for the reader per word, the map keeps its own (C4) — the
            // list checked read's own span shape
            native_result_span(heaps, words.to_vec())?
        },
        // a miss answers the `none` span member with the probe key, the
        // same checked-read shape `list_get` answers with the index
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: args[1],
        },
    })
}

fn map_set_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the variadic tail is the value's whole inline word span, the
    // `list_push` staging shape at the value position
    if args.len() < 3 {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: args.len(),
        });
    }
    heaps.set_map(ref_arg(args, 0)?, args[1], &args[2..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

fn list_set_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the staged span is the list word, the index word, then the
    // element's whole inline word span (`list_push`'s variadic shape at
    // the element position)
    if args.len() < 3 {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: args.len(),
        });
    }
    heaps.set_list(ref_arg(args, 0)?, i64_arg(args, 1)?, &args[2..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

fn list_remove_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    heaps.remove_list(ref_arg(args, 0)?, i64_arg(args, 1)?)?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

fn list_insert_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // `list_set`'s staging shape: the list word, the index word, then
    // the element's whole inline word span
    if args.len() < 3 {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: args.len(),
        });
    }
    heaps.insert_list(ref_arg(args, 0)?, i64_arg(args, 1)?, &args[2..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

fn list_clear_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    heaps.clear_list(ref_arg(args, 0)?)?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

fn map_remove_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    heaps.remove_map(ref_arg(args, 0)?, args[1])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

fn map_contains_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let hit = heaps.contains_map(ref_arg(args, 0)?, args[1])?;
    Ok(NativeResult::Word(Value::Bool(hit)))
}

fn map_entries_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the entry-order pair list: the fresh list's edge owns one count
    // per `Ref` word of key and value alike (the map keeps its own, C4),
    // the `map_keys` adoption discipline over the value words too
    let (words, stride) = heaps.map_entries(ref_arg(args, 0)?)?;
    for word in &words {
        if let Value::Ref(word) = *word {
            heaps.inc_edge(word)?;
        }
    }
    Ok(NativeResult::Word(heaps.new_list_strided(words, stride)))
}

/// The copy natives' shared finisher: the fresh list's edge owns one
/// count per `Ref` word of `elems` (taken on the one walk the copy
/// already pays), the sources keep their own (C4), and the vec moves
/// into the adopting list with no second pass. The fresh list carries
/// the source's stride (an empty copy keeps stride 0).
fn copy_into_fresh_list(
    heaps: &mut Heaps,
    elems: Vec<Value>,
    stride: usize,
) -> Result<NativeResult, Trap> {
    for value in &elems {
        if let Value::Ref(word) = *value {
            heaps.inc_edge(word)?;
        }
    }
    Ok(NativeResult::Word(heaps.new_list_strided(elems, stride)))
}

fn cons_fn(heaps: &mut Heaps, _out: &mut OutputSink, args: &[Value]) -> Result<NativeResult, Trap> {
    // the staged span is the head element's whole word span riding the
    // front (`list_push`'s variadic shape at the other end) then the
    // list `Ref` last — at least one head word plus the list
    let (list_arg, head) = args.split_last().ok_or(Trap::ArityMismatch {
        expected: 2,
        given: args.len(),
    })?;
    if head.is_empty() {
        return Err(Trap::ArityMismatch {
            expected: 2,
            given: args.len(),
        });
    }
    let list = match *list_arg {
        Value::Ref(word) => word,
        other => {
            return Err(Trap::SlotTypeMismatch {
                slot: u8::try_from(args.len() - 1).expect("slot index fits u8"),
                expected: SlotType::Ref,
                actual: other.slot_type(),
            })
        },
    };
    let (rest, stride) = heaps.list_at(list)?;
    // an empty rest list is stride-polymorphic, so the head's width fixes
    // the fresh stride; a non-empty one must repeat it exactly
    let stride = match stride {
        0 => head.len(),
        s if s == head.len() => s,
        s => {
            return Err(Trap::ElementStride {
                obj: list.id(),
                stride: s,
                given: head.len(),
            })
        },
    };
    let mut elems = Vec::with_capacity(rest.len() + head.len());
    elems.extend_from_slice(head);
    elems.extend_from_slice(rest);
    copy_into_fresh_list(heaps, elems, stride)
}

fn tail_fn(heaps: &mut Heaps, _out: &mut OutputSink, args: &[Value]) -> Result<NativeResult, Trap> {
    // an empty list skips nothing and answers the empty list
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let skipped = elems.get(stride..).unwrap_or(&[]).to_vec();
    copy_into_fresh_list(heaps, skipped, stride)
}

fn collect_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let elems = elems.to_vec();
    copy_into_fresh_list(heaps, elems, stride)
}

/// `first`'s one body: the checked first element in
/// `list_get`'s span shape — the hit is the first element's whole
/// inline span, handed out under one count per `Ref` word (C4), and an
/// empty list answers the `none` span member, never a trap. The miss
/// payload is the inert unit word: there is no rejected index to carry,
/// and callers read the payload only on the hit.
fn first_element_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let hit = (!elems.is_empty()).then(|| &elems[..stride]);
    Ok(match hit {
        Some([only]) => {
            // the one-word element reads through the `Tag` shape: no span
            // buffer, and the slice dies before the count goes on
            let word = *only;
            native_result_word(heaps, word)?
        },
        Some(words) => {
            let words = words.to_vec();
            native_result_span(heaps, words)?
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
    })
}

/// The greater of two numeric words: int compares in i64's total order;
/// float follows IEEE and never traps, a NaN operand answering the other
/// side. Any other word kind refuses like a comparison against a
/// mismatched slot — the call site's own trap shape.
fn max_fn(_heaps: &mut Heaps, _out: &mut OutputSink, args: &[Value]) -> Result<NativeResult, Trap> {
    match args {
        [Value::I64(a), Value::I64(b)] => Ok(NativeResult::Word(Value::I64((*a).max(*b)))),
        [Value::F64(a), Value::F64(b)] => Ok(NativeResult::Word(Value::F64((*a).max(*b)))),
        _ => {
            // the pair must agree on one numeric kind: name the word that
            // breaks it at its own index — the first non-numeric word, or
            // the second of a mixed int/float pair — citing the other
            // side's kind as expected
            let offender = args
                .iter()
                .position(|a| !matches!(a, Value::I64(_) | Value::F64(_)))
                .unwrap_or(1);
            let expected = args
                .iter()
                .enumerate()
                .find(|(i, a)| *i != offender && matches!(a, Value::I64(_) | Value::F64(_)))
                .map_or(SlotType::I64, |(_, a)| a.slot_type());
            Err(Trap::SlotTypeMismatch {
                slot: u8::try_from(offender).expect("slot index fits u8"),
                expected,
                actual: args.get(offender).map_or(SlotType::I64, Value::slot_type),
            })
        },
    }
}

/// The shared body of the control natives: a `CALL` site never reaches it
/// (the dispatcher intercepts and switches), so the only reachable call is
/// a `CALLT`, where no switch is possible — the bounce answer.
#[allow(clippy::unnecessary_wraps)]
fn fiber_control_bounce_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Tag(PLATFORM_NONE)))
}

/// One reserved gap entry: a call to the historical index traps exactly
/// like an uninstalled one (`UnknownCallee` naming the module and index).
/// The lowering never emits these indices; each module's table keeps them
/// so the historical index run stays positionally stable.
/// One generic builtin body [H3, phase 1]: the ONE body a CALLG site
/// dispatches, addressed by the pool FuncRef (module, native index) the
/// same way a CALL addresses a native. `params` are the real argument
/// words (checked against the staged words like any native's), and
/// `evidence` is the count of evidence words the dispatch reads from the
/// overlay after them — the constraint list travels with the callee, so
/// the uop carries no count of its own.
pub struct GenericFunction {
    /// The call name, for diagnostics.
    pub name: &'static str,
    /// The real arguments' expected word kinds — the named words of the
    /// entry's [`Staging`] discipline. A `Leading` entry's `params` cover
    /// the trailing named words (the staged words before them are one
    /// element's whole inline span, `cons`'s head); an `Interior` entry's
    /// cover the leading named word plus the final named word (a fold's
    /// accumulator span sits between them, so the final word's position
    /// is dynamic per call).
    pub params: &'static [SlotType],
    /// How the staged words map onto `params`: fixed one-to-one, one
    /// element span leading the named words, or one span between the
    /// leading and trailing named words — the arity check relaxes to
    /// "at least" for the two span forms and the kind check covers the
    /// named words alone.
    pub staging: Staging,
    /// How many evidence words follow the staged arguments.
    pub evidence: u8,
    /// Result span width on the caller's overlay.
    pub result_words: u8,
    /// The one body: `(evidence, args)`.
    pub func: GenericFn,
}

/// The body shape of a generic builtin: the whole VM, the evidence
/// words, then the real arguments. The whole-VM signature is the
/// reentrancy seam — the lambda-taking combinators call user code back
/// through `[Vm::call_hosted]` mid-body — while the pure bodies split
/// the heap and sink off themselves (`[Vm::split]`) and never touch the
/// rest.
pub type GenericFn = fn(&mut Vm, &[Value], &[Value]) -> Result<NativeResult, Trap>;

/// One body per generic row name: a name and its own body cannot drift apart, since two rows
/// can share every declarative field.
const GENERIC_BODIES: &[(&str, GenericFn)] = &[
    ("len", len_generic_fn),
    ("list_len", list_len_generic_fn),
    ("list_get", list_get_generic_fn),
    ("list_concat", list_concat_generic_fn),
    ("map_len", map_len_generic_fn),
    ("map_get", map_get_generic_fn),
    ("map_keys", map_keys_generic_fn),
    ("cons", cons_generic_fn),
    ("tail", tail_generic_fn),
    ("collect", collect_generic_fn),
    ("first", first_generic_fn),
    ("max", max_generic_fn),
    ("abs", abs_generic_fn),
    ("map", map_generic_fn),
    ("filter", filter_generic_fn),
    ("reduce", reduce_generic_fn),
    ("fork_map", par_map_generic_fn),
    ("partition", partition_generic_fn),
    ("fork_reduce", par_reduce_generic_fn),
    ("sort", sort_generic_fn),
];

/// The generic builtin vocabulary, keyed by the (module, native index) `FuncRef` address: the abi
/// rows with this VM's one body per row attached.
static GENERIC_FUNCTIONS: LazyLock<Vec<GenericFunction>> = LazyLock::new(|| {
    ds_abi::native::GENERIC_NATIVES
        .iter()
        .map(|row| GenericFunction {
            name: row.name,
            params: row.params,
            staging: row.staging,
            evidence: row.evidence,
            result_words: row.shape.result_words(),
            func: generic_body(row.name),
        })
        .collect()
});

/// The one body a generic row names.
fn generic_body(name: &str) -> GenericFn {
    GENERIC_BODIES
        .iter()
        .find_map(|(row, func)| (*row == name).then_some(*func))
        .unwrap_or_else(|| panic!("the generic `{name}` row carries a body"))
}

/// Resolves the generic builtin a CALLG `FuncRef` addresses, if any.
#[must_use]
pub fn resolve_generic(module: &str, index: u32) -> Option<&'static GenericFunction> {
    if module != BUILTIN_MODULE {
        return None;
    }
    let at = ds_abi::native::GENERIC_NATIVES
        .iter()
        .position(|row| row.index == index)?;
    Some(&GENERIC_FUNCTIONS[at])
}

/// The one `len` body: dispatch on the call's evidence word, the
/// statically-resolved builtin instance of the instantiated type
/// ([`ds_abi::BuiltinInstance`]). The string instance counts unicode
/// scalar values; the list instance counts elements; the bytes
/// instance counts payload bytes; the map instance counts pairs.
fn len_generic_fn(vm: &mut Vm, evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let (mut heaps, _out) = vm.split();
    let instance = evidence.first().copied().unwrap_or(Value::I64(-1));
    match instance {
        Value::I64(id) if id == ds_abi::BuiltinInstance::Str as i64 => {
            // char count, never bytes: multi-byte text measures its scalar values
            let n = i64::try_from(heaps.char_count(ref_arg(args, 0)?)?)
                .expect("string length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::List as i64 => {
            let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
            let n = i64::try_from(elems.len() / stride.max(1)).expect("list length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Bytes as i64 => {
            let n = i64::try_from(heaps.bytes_at(ref_arg(args, 0)?)?.len())
                .expect("bytes length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Map as i64 => {
            let n = i64::try_from(heaps.map_len(ref_arg(args, 0)?)?).expect("map length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Arena as i64 => {
            let n =
                i64::try_from(heaps.arena_len(ref_arg(args, 0)?)?).expect("arena length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        staged => Err(Trap::UnknownInstance {
            evidence: match staged {
                Value::I64(id) => id,
                _ => -1,
            },
        }),
    }
}

// The container reads and counts' one bodies [H3, phase 2]:
// element-agnostic — the container word is one `Ref` whatever the
// element, so each adapter drops the evidence words (they record the
// receiver's container instance for the dispatch registry) and runs the
// same body a native's signature would.
fn list_len_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    list_len_fn(&mut heaps, out, args)
}

fn list_get_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    list_get_fn(&mut heaps, out, args)
}

fn list_concat_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    list_concat_fn(&mut heaps, out, args)
}

fn map_len_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    map_len_fn(&mut heaps, out, args)
}

fn map_keys_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    map_keys_fn(&mut heaps, out, args)
}

fn map_get_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    map_get_fn(&mut heaps, out, args)
}

fn cons_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    cons_fn(&mut heaps, out, args)
}

fn first_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    first_element_fn(&mut heaps, out, args)
}

fn tail_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    tail_fn(&mut heaps, out, args)
}

fn collect_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    collect_fn(&mut heaps, out, args)
}

/// `max`'s one body: dispatch on the call's evidence word like `len`'s —
/// the numeric instances the body orders. The value-kind match inside
/// the shared scalar body stays the defense: a corrupted stream staging
/// a non-numeric word refuses there instead of misreading it.
fn max_generic_fn(vm: &mut Vm, evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let instance = evidence.first().copied().unwrap_or(Value::I64(-1));
    match instance {
        Value::I64(id)
            if id == ds_abi::BuiltinInstance::Int as i64
                || id == ds_abi::BuiltinInstance::Float as i64 =>
        {
            let (mut heaps, out) = vm.split();
            max_fn(&mut heaps, out, args)
        },
        staged => Err(Trap::UnknownInstance {
            evidence: match staged {
                Value::I64(id) => id,
                _ => -1,
            },
        }),
    }
}

/// `abs`'s one body: dispatch on the call's evidence word like `max`'s —
/// the numeric instances the body answers. The value-kind match inside
/// the shared scalar body stays the defense: a corrupted stream staging
/// a non-numeric word refuses there instead of misreading it.
fn abs_generic_fn(vm: &mut Vm, evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let instance = evidence.first().copied().unwrap_or(Value::I64(-1));
    match instance {
        Value::I64(id) if id == ds_abi::BuiltinInstance::Float as i64 => {
            let (mut heaps, out) = vm.split();
            abs_fn(&mut heaps, out, args)
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Int as i64 => {
            let x = i64_arg(args, 0)?;
            // the magnitude has no in-range answer at the word's floor;
            // the arithmetic family's own overflow trap names it
            let magnitude = x.checked_abs().ok_or(Trap::IntegerOverflow)?;
            Ok(NativeResult::Word(Value::I64(magnitude)))
        },
        staged => Err(Trap::UnknownInstance {
            evidence: match staged {
                Value::I64(id) => id,
                _ => -1,
            },
        }),
    }
}

// The lambda-taking container combinators: the one bodies reenter the
// dispatcher per element through `Vm::call_hosted` — the staged lambda
// word is callable-defaulted (a scalar FuncRef or a closure object's
// Ref word), and its answer word arrives under the
// RET transfer count, which the body owns. The evidence words record
// the receiver's container instance like every element-agnostic
// body's; a mid-loop trap leaks the partial result's birth count, the
// same single-leak class the producer bodies' traps leave.

/// The staged lambda word: callable-defaulted (F2) — the scalar FuncRef
/// of a non-capturing lambda or named function, or a closure object's
/// `Ref` word. The hosted call resolves either kind; every other word
/// kind refuses like a mismatched slot.
fn fn_arg(args: &[Value], index: usize) -> Result<Value, Trap> {
    match args.get(index) {
        Some(word @ (Value::Fn(_) | Value::Ref(_))) => Ok(*word),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::Fn,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// `map`'s one body: each element — its whole inline span, one slice at
/// the list's stride — runs through the staged lambda, and the answered
/// span — transfer counts and all — pushes onto the fresh result list
/// (the push acquires the list's own count per `Ref` word, fixing the
/// result's stride at the answer's width); the body then drops the
/// transferred counts it still holds.
fn map_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let res = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    for at in (0..elems.len()).step_by(stride) {
        let mapped = vm.call_hosted(f, &elems[at..at + stride])?;
        let (mut heaps, _) = vm.split();
        heaps.push_list(res, mapped.as_slice())?;
        for word in mapped.as_slice() {
            if let Value::Ref(word) = *word {
                heaps.dec(word)?;
            }
        }
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}

/// A predicate's one-word bool answer — `filter`'s and `partition`'s
/// shared contract. A non-bool answer refuses like a mismatched slot
/// (the declared predicate's word is the contract); a wider answer
/// names hand-assembled code and refuses at its first word.
fn predicate_bool(answer: &crate::vm::FrameAnswer) -> Result<bool, Trap> {
    let word = match answer.as_slice() {
        [only] => *only,
        wider => {
            return Err(Trap::SlotTypeMismatch {
                slot: 0,
                expected: SlotType::Bool,
                actual: wider[0].slot_type(),
            })
        },
    };
    match word {
        Value::Bool(keep) => Ok(keep),
        other => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::Bool,
            actual: other.slot_type(),
        }),
    }
}

/// `filter`'s one body: each element's whole span runs through the
/// predicate, whose one-word answer keeps or drops it. A non-bool
/// answer refuses like a mismatched slot — the declared predicate's
/// word is the contract. Keeping pushes the span onto the fresh result
/// list (the push acquires the list's own count per `Ref` word; the
/// source list keeps its own), so no element-side count moves at all.
fn filter_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let res = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    for at in (0..elems.len()).step_by(stride) {
        if predicate_bool(&vm.call_hosted(f, &elems[at..at + stride])?)? {
            let (mut heaps, _) = vm.split();
            heaps.push_list(res, &elems[at..at + stride])?;
        }
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}

/// `partition`'s one body: each element's whole span runs through the
/// predicate exactly as `filter`'s does, a true answer pushing onto the
/// first fresh list and a false one onto the second (each push taking
/// the destination list's own edge count per `Ref` word, so no
/// element-side count moves), and the two list `Ref`s answer as the
/// tag-less multi-word total — no discriminant, the two words are the
/// tuple itself, staged on the caller's overlay words 0 and 1.
fn partition_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let yes = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    let no = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    for at in (0..elems.len()).step_by(stride) {
        let dst =
            if predicate_bool(&vm.call_hosted(f, &elems[at..at + stride])?)? { yes } else { no };
        let (mut heaps, _) = vm.split();
        heaps.push_list(dst, &elems[at..at + stride])?;
    }
    Ok(NativeResult::Total {
        words: vec![Value::Ref(yes), Value::Ref(no)],
    })
}

/// `reduce`'s one body: the accumulator starts as the borrowed init's
/// whole word span (native arguments are a transient borrow window; the
/// span stages between the list and the callable, its width the call
/// site's fact), each step hands the accumulator words plus the
/// element's whole span to the staged lambda under the hosted call's
/// one-count-per-Ref hand-off (the lambda's exit drops or returns
/// them), and the answer's whole span replaces the owned one — the old
/// words' counts drop per `Ref` word, and the first step's old span is
/// the borrowed init, never dropped. An empty list answers the init
/// handed out under its own count per `Ref` word (the `list_get`
/// discipline); otherwise the final owned counts transfer to the
/// caller with the words through the tag-less total. The callable's
/// own signature governs the answer's width (the checker proves it the
/// folded type), so no body-side width check exists.
fn reduce_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, args.len() - 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let mut acc: Vec<Value> = args[1..args.len() - 1].to_vec();
    let mut owned = false;
    let mut call_args = Vec::with_capacity(acc.len() + stride);
    for at in (0..elems.len()).step_by(stride) {
        call_args.clear();
        call_args.extend_from_slice(&acc);
        call_args.extend_from_slice(&elems[at..at + stride]);
        let answer = vm.call_hosted(f, &call_args)?;
        if owned {
            for word in &acc {
                if let Value::Ref(id) = *word {
                    let (mut heaps, _) = vm.split();
                    heaps.dec(id)?;
                }
            }
        }
        acc.clear();
        acc.extend_from_slice(answer.as_slice());
        owned = true;
    }
    if !owned {
        for word in &acc {
            if let Value::Ref(id) = *word {
                let (mut heaps, _) = vm.split();
                heaps.inc(id)?;
            }
        }
    }
    Ok(NativeResult::Total { words: acc })
}

/// One comparison's ordering answer: the callable's one-word `int`
/// result names the first span the earlier element when negative, and
/// an equal answer keeps the left run's element (stability). A wider
/// or non-int answer refuses like a mismatched slot — the declared
/// comparison's word is the contract, the predicate-bool discipline.
fn compare_orders_first(
    vm: &mut Vm,
    cmp: Value,
    first: &[Value],
    second: &[Value],
) -> Result<bool, Trap> {
    let mut args = Vec::with_capacity(first.len() + second.len());
    args.extend_from_slice(first);
    args.extend_from_slice(second);
    let answer = vm.call_hosted(cmp, &args)?;
    match answer.as_slice() {
        [Value::I64(ordering)] => Ok(*ordering <= 0),
        wider => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: wider.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// `sort`'s one body: the stable merge sort over the staged compare
/// callable. The walk is bottom-up over element indices, so no element
/// span moves before the final pushes and no Rust recursion rides the
/// depth. Each kept element's whole span pushes onto the fresh result
/// list (the push acquiring the list's own count per `Ref` word, the
/// source list keeping its own — filter's discipline), so no
/// element-side count moves at all; a mid-sort trap leaks the fresh
/// empty list alone, the same single-leak class the other combinators'
/// traps leave.
fn sort_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let cmp = fn_arg(args, 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let res = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    let count = elems.len() / stride;
    let mut order: Vec<usize> = (0..count).collect();
    let mut scratch: Vec<usize> = vec![0; count];
    let mut width = 1;
    while width < count {
        let mut at = 0;
        while at < count {
            let mid = (at + width).min(count);
            let end = (at + 2 * width).min(count);
            let (mut left, mut right, mut out) = (at, mid, at);
            while left < mid && right < end {
                let first = order[left] * stride;
                let second = order[right] * stride;
                if compare_orders_first(
                    vm,
                    cmp,
                    &elems[first..first + stride],
                    &elems[second..second + stride],
                )? {
                    scratch[out] = order[left];
                    left += 1;
                } else {
                    scratch[out] = order[right];
                    right += 1;
                }
                out += 1;
            }
            while left < mid {
                scratch[out] = order[left];
                left += 1;
                out += 1;
            }
            while right < end {
                scratch[out] = order[right];
                right += 1;
                out += 1;
            }
            at = end;
        }
        std::mem::swap(&mut order, &mut scratch);
        width *= 2;
    }
    for &element in &order {
        let start = element * stride;
        let (mut heaps, _) = vm.split();
        heaps.push_list(res, &elems[start..start + stride])?;
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}

/// The fork-join callable word, forked per element: the scalar `Fn`
/// names the callable's own function, a closure object's `Ref` its body
/// (the capture block staged per spawn into the child's entry frame,
/// exactly the CALLS copy-in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ForkCallable {
    Scalar(FuncRef),
    Closure(RefId),
}

/// The fork-join drive seam's staged state — the assembly/fold stage
/// the join runs after the spawn phase parks the CALLG site.
/// `par_map`'s answers assemble in spawn order onto the fresh result
/// list at the seam's consumption; `par_reduce`'s chain state carries
/// the fold between the per-element joins (the element snapshot, the
/// running accumulator span with its ownership flag, and the next
/// element's offset).
#[derive(Clone, Debug, PartialEq)]
pub enum ForkStage {
    /// Each completed answer's whole word span pushes onto `result` as
    /// one element, the first push fixing the list's stride.
    Map {
        /// The fresh result list (empty until the seam assembles).
        result: RefId,
    },
    /// The next element's child spawns with the running accumulator
    /// span then `elements[at..]` as its entry arguments, the adopted
    /// answer replacing `acc` (`owned` marking the adoption).
    Reduce {
        /// The input list's element snapshot, `stride` words per
        /// element.
        elements: Vec<Value>,
        /// The element word width.
        stride: usize,
        /// The callable spawned per step.
        callable: ForkCallable,
        /// The running accumulator's whole word span.
        acc: Vec<Value>,
        /// Whether `acc` is the seam's own (an adopted answer) or the
        /// caller's borrowed init.
        owned: bool,
        /// The next element's word offset.
        at: usize,
    },
}

impl ForkStage {
    /// The answer's word width the parked call site's overlay must
    /// hold (the map's one result word, the fold's whole span).
    pub(crate) fn answer_words(&self) -> usize {
        match self {
            Self::Map { .. } => 1,
            Self::Reduce { acc, .. } => acc.len(),
        }
    }
}

/// Whether the generic builtin a dispatch site addresses is a
/// fork-join combinator (a spawn-only body that parks the site).
#[must_use]
pub fn is_fork_join(module: &str, index: u32) -> bool {
    module == BUILTIN_MODULE && (index == BUILTIN_PAR_MAP || index == BUILTIN_PAR_REDUCE)
}

/// Reads the callable word at argument `index` (the callable-defaulted
/// `Fn` slot admits both spellings, F2).
fn fork_callable(args: &[Value], index: usize) -> Result<ForkCallable, Trap> {
    match fn_arg(args, index)? {
        Value::Fn(fref) => Ok(ForkCallable::Scalar(fref)),
        Value::Ref(word) => Ok(ForkCallable::Closure(word)),
        _ => unreachable!("fn_arg admits only the Fn and Ref words"),
    }
}

/// Spawns one fork child applying the callable to `element` — the
/// element's whole word span as the entry arguments either way.
pub(crate) fn fork_spawn(
    vm: &mut Vm,
    nursery: NurseryId,
    callable: ForkCallable,
    element: &[Value],
) -> Result<FiberId, Trap> {
    match callable {
        ForkCallable::Scalar(fref) => vm.spawn_ref_into(Some(nursery), fref, element),
        ForkCallable::Closure(word) => vm.spawn_closure_into(Some(nursery), word, element),
    }
}

/// `par_map`'s spawn phase [E3, the parked-CALLG fork-join]: fork is
/// one spawned child per element applying the callable under a fresh
/// nursery — the callable's own function entered with the element's
/// whole inline span, the spawn copy-in acquiring one count per `Ref`
/// word for the child's frame — and the join parks the CALLG site
/// through the interlock: the body answers the park request, the drive
/// seam carries the assembly stage, and the answers assemble in spawn
/// order onto the fresh result list at the seam's consumption (each
/// answer's whole word span pushing as one element, the first push
/// fixing the list's stride, the push taking the list's own edge count
/// per `Ref` word, the child's delivered count ending at the joiner's
/// single dec — one per `Ref` word, a ruled cross-heap channel). The
/// first failure in spawn-drive order cancels the siblings at their
/// next preemption point [E5] and re-raises at the parked call site,
/// exactly a spawn-body failure at its enclosing boundary; a failed
/// spawn tears the already-spawned children down under the cancel
/// discipline before its trap surfaces. An empty input answers the
/// empty list without parking (no child stands to join). The callable
/// is a scalar `Fn` word or a closure object's `Ref` — a closure
/// callable's capture block is staged per spawn into the child's entry
/// frame past the element's argument span, one acquired count per
/// `Ref` capture claimed for the child (the CALLS copy-in discipline),
/// so a forked lambda is one shape with a called one; a `Ref` word
/// naming no closure object refuses at the spawn.
fn par_map_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the callable word: a scalar `Fn` names the callable's own
    // function; a closure object's `Ref` names its body, the capture
    // block staged per spawn into the child's entry frame
    let callable = fork_callable(args, 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let res = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    if elems.is_empty() {
        return Ok(NativeResult::Word(Value::Ref(res)));
    }
    let nursery = vm.open_fresh_nursery();
    for at in (0..elems.len()).step_by(stride) {
        if let Err(trap) = fork_spawn(vm, nursery, callable, &elems[at..at + stride]) {
            vm.teardown_fork_nursery(nursery)?;
            let (mut heaps, _) = vm.split();
            heaps.dec(res)?;
            return Err(trap);
        }
    }
    Ok(NativeResult::Fork {
        nursery,
        stage: ForkStage::Map { result: res },
    })
}

/// `par_reduce`'s spawn phase [E3, the parked-CALLG fork-join]: the
/// fold's left data dependency chains the children — each element's
/// application is one spawned child entered with the running
/// accumulator's whole word span and the element's span (the
/// callable's own function, the spawn copy-in acquiring one count per
/// `Ref` word for the child's frame), the seam's chain state carrying
/// the fold between the per-step joins — so the reduction applies
/// strictly in input order, the replayable spelling whatever a later
/// scheduler does. The fold type is the call's own fact: the init's
/// whole word span stages between the list and the callable (the
/// interior-span discipline), the callable's checker-proved answer
/// width matching it, and the final accumulator delivers whole onto
/// the parked call site. The accumulator's counts follow `reduce`'s
/// owned discipline: the init stays borrowed until the first delivered
/// answer adopts (the child's completion count landing on the joiner —
/// the ruled cross-heap channel), each superseded owned word dec'd at
/// adoption; an empty input answers the init, inc'd to own it. The
/// callable is a scalar `Fn` word or a closure object's `Ref` (the
/// capture block staged per spawn exactly par_map's).
fn par_reduce_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the callable word rides at the staged init span's end: a scalar
    // `Fn` or a closure object's `Ref`, the capture block staged per
    // spawn exactly par_map's
    let callable = fork_callable(args, args.len() - 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    // the fold's whole span: the init's words stage between the list
    // and the callable, its width the call site's fact
    let acc: Vec<Value> = args[1..args.len() - 1].to_vec();
    if elems.is_empty() {
        // an empty input answers the init, inc'd to own it — no child
        // stands to join, so no park
        for word in &acc {
            if let Value::Ref(id) = *word {
                let (mut heaps, _) = vm.split();
                heaps.inc(id)?;
            }
        }
        return Ok(NativeResult::Total { words: acc });
    }
    let mut child_args = Vec::with_capacity(acc.len() + stride);
    child_args.extend_from_slice(&acc);
    child_args.extend_from_slice(&elems[..stride]);
    let nursery = vm.open_fresh_nursery();
    if let Err(trap) = fork_spawn(vm, nursery, callable, &child_args) {
        // a refused spawn's child may be half-built: tear the step's
        // nursery down under the cancel discipline; the init stays
        // borrowed (nothing owned to release)
        vm.teardown_fork_nursery(nursery)?;
        return Err(trap);
    }
    Ok(NativeResult::Fork {
        nursery,
        stage: ForkStage::Reduce {
            elements: elems,
            stride,
            callable,
            acc,
            owned: false,
            at: 0,
        },
    })
}

/// Releases a park request the dispatcher refused after the spawn
/// phase ran (a whole-span answer wider than the parked site's
/// overlay): the spawned children torn down under the cancel
/// discipline, then the staged state's own words released (the result
/// list's count, the owned accumulator's span).
pub(crate) fn fork_abandon(vm: &mut Vm, nursery: NurseryId, stage: ForkStage) -> Result<(), Trap> {
    vm.teardown_fork_nursery(nursery)?;
    let (mut heaps, _) = vm.split();
    match stage {
        ForkStage::Map { result } => heaps.dec(result),
        ForkStage::Reduce { acc, owned, .. } => release_owned_span(&mut heaps, &acc, owned),
    }
}

/// Decs each `Ref` word of an owned span (a fold's superseded
/// accumulator) through the given router; a borrowed span (`owned`
/// false, the init before the first adoption) skips every word — its
/// counts belong to the caller.
pub(crate) fn release_owned_span(
    heaps: &mut Heaps<'_>,
    words: &[Value],
    owned: bool,
) -> Result<(), Trap> {
    if !owned {
        return Ok(());
    }
    for word in words {
        if let Value::Ref(id) = *word {
            heaps.dec(id)?;
        }
    }
    Ok(())
}

macro_rules! reserved {
    ($name:ident in $module:ident at $index:literal) => {
        fn $name(
            _heaps: &mut Heaps,
            _out: &mut OutputSink,
            _args: &[Value],
        ) -> Result<NativeResult, Trap> {
            Err(Trap::UnknownCallee {
                module: ($module).to_owned(),
                func: $index,
            })
        }
    };
}

reserved!(builtin_reserved_0 in BUILTIN_MODULE at 0); // was println
reserved!(builtin_reserved_1 in BUILTIN_MODULE at 1); // was print
reserved!(builtin_reserved_5 in BUILTIN_MODULE at 5); // was read_file
reserved!(builtin_reserved_6 in BUILTIN_MODULE at 6); // was write_file
reserved!(builtin_reserved_7 in BUILTIN_MODULE at 7); // was file_exists
reserved!(builtin_reserved_8 in BUILTIN_MODULE at 8); // was fs_list
reserved!(builtin_reserved_9 in BUILTIN_MODULE at 9); // was file_size
reserved!(builtin_reserved_48 in BUILTIN_MODULE at 48); // par_reduce's generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_83 in BUILTIN_MODULE at 83); // abs's generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_21 in BUILTIN_MODULE at 21); // was the monomorphic list_len; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_22 in BUILTIN_MODULE at 22); // was list_get; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_23 in BUILTIN_MODULE at 23); // was list_concat; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_24 in BUILTIN_MODULE at 24); // was fiber_yield; the user-spellable debug yield retired (the 2026-11-17 ruling), the dev host injecting suspension through its own face
reserved!(builtin_reserved_29 in BUILTIN_MODULE at 29); // was the monomorphic map_len; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_30 in BUILTIN_MODULE at 30); // was map_get; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_10 in BUILTIN_MODULE at 10); // was the string-only len; the generic len body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_32 in BUILTIN_MODULE at 32); // cons; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_33 in BUILTIN_MODULE at 33); // head; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_34 in BUILTIN_MODULE at 34); // tail; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_35 in BUILTIN_MODULE at 35); // collect; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_36 in BUILTIN_MODULE at 36); // first; shares head's generic body in GENERIC_FUNCTIONS
reserved!(builtin_reserved_37 in BUILTIN_MODULE at 37); // max; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_38 in BUILTIN_MODULE at 38); // map; the reentrant generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_39 in BUILTIN_MODULE at 39); // filter; the reentrant generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_40 in BUILTIN_MODULE at 40); // reduce; the reentrant generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_46 in BUILTIN_MODULE at 46); // partition; the reentrant generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_47 in BUILTIN_MODULE at 47); // par_map; the fork-join generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_59 in BUILTIN_MODULE at 59); // map_keys; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_90 in BUILTIN_MODULE at 90); // sort; the reentrant generic body lives in GENERIC_FUNCTIONS

/// The one reserved gap entry shape: no parameters, a one-word result
/// contract (the trap fires before any overlay write matters).
fn reserved_entry(func: NativeFn) -> NativeFunction {
    NativeFunction {
        name: "reserved",
        params: &[],
        variadic: false,
        result_words: 1,
        control: None,
        channel: None,
        timer: None,
        func,
    }
}

/// The builtin table every VM installs as [`BUILTIN_MODULE`]: the pure, irreducible primitives and
/// the fiber, channel, and timer seams the dispatcher intercepts. The rows are the abi's
/// ([`ds_abi::native`]); this module supplies one body per row and the interception markers, and
/// keeps every gap — a retired native's slot, or a generic row's monomorphic slot — a trap.
#[must_use]
pub fn builtin_module() -> Vec<NativeFunction> {
    let mut fns: Vec<NativeFunction> = BUILTIN_BODIES
        .iter()
        .map(|body| reserved_entry(*body))
        .collect();
    for row in ds_abi::native::rows(NativeModule::Builtin) {
        let slot = &mut fns[row.index as usize];
        slot.name = row.name;
        slot.params = row.params;
        slot.variadic = row.variadic;
        slot.result_words = row.shape.result_words();
    }
    for (index, dispatch) in BUILTIN_DISPATCH {
        let slot = &mut fns[*index as usize];
        match dispatch {
            Dispatch::Control(control) => slot.control = Some(*control),
            Dispatch::Channel(channel) => slot.channel = Some(*channel),
            Dispatch::Timer(timer) => slot.timer = Some(*timer),
        }
    }
    debug_assert_eq!(fns.len(), BUILTIN_CHAN_RECV_DEADLINE as usize + 1);
    fns
}

/// One body per builtin table index, in index order: a gap carries its refusal stub.
#[rustfmt::skip]
const BUILTIN_BODIES: [NativeFn; BUILTIN_CHAN_RECV_DEADLINE as usize + 1] = [
    builtin_reserved_0, builtin_reserved_1, str_int_fn, str_float_fn,
    str_bool_fn, builtin_reserved_5, builtin_reserved_6, builtin_reserved_7,
    builtin_reserved_8, builtin_reserved_9, builtin_reserved_10, substring_fn,
    concat_fn, to_upper_fn, to_lower_fn, trim_fn,
    parse_int_fn, split_fn, char_at_fn, parse_float_fn,
    list_new_fn, builtin_reserved_21, builtin_reserved_22, builtin_reserved_23,
    builtin_reserved_24, fiber_control_bounce_fn, fiber_control_bounce_fn, list_push_fn,
    map_new_fn, builtin_reserved_29, builtin_reserved_30, map_set_fn,
    builtin_reserved_32, builtin_reserved_33, builtin_reserved_34, builtin_reserved_35,
    builtin_reserved_36, builtin_reserved_37, builtin_reserved_38, builtin_reserved_39,
    builtin_reserved_40, fiber_control_bounce_fn, fiber_control_bounce_fn, fiber_control_bounce_fn,
    fiber_control_bounce_fn, join_fn, builtin_reserved_46, builtin_reserved_47,
    builtin_reserved_48, total_compare_fn, str_compare_fn, char_compare_fn,
    byte_compare_fn, str_hash_fn, float_hash_fn, char_hash_fn,
    fiber_control_bounce_fn, str_fixed_fn, char_of_fn, builtin_reserved_59,
    code_of_fn, bytes_of_fn, byte_list_fn, byte_at_fn,
    bytes_eq_fn, bytes_compare_fn, bytes_hash_fn, utf8_encode_fn,
    utf8_decode_fn, trap_fn, list_eq_fn, map_eq_fn,
    byte_of_fn, int_of_byte_fn, u32_of_fn, int_of_u32_fn,
    sqrt_fn, sin_fn, cos_fn, atan2_fn,
    floor_fn, ceil_fn, pow_fn, builtin_reserved_83,
    contains_fn, index_of_fn, replace_fn, chan_refuse_fn,
    chan_refuse_fn, chan_refuse_fn, builtin_reserved_90, list_set_fn,
    list_remove_fn, list_insert_fn, list_clear_fn, map_remove_fn,
    map_contains_fn, map_entries_fn, arena_new_fn, arena_insert_fn,
    arena_remove_fn, arena_at_fn, round_fn, trunc_fn,
    log_fn, exp_fn, tan_fn, chan_refuse_fn,
    chan_refuse_fn, chan_refuse_fn,
];

/// How the dispatcher intercepts a slot ahead of its body.
enum Dispatch {
    /// The fiber substrate's switch, performed by the dispatcher.
    Control(ControlNative),
    /// The channel step, which needs the scheduler.
    Channel(ChannelNative),
    /// The timer race, which needs the run queue and the deadline wheel.
    Timer(TimerNative),
}

/// The intercepted slots; every other slot runs its body directly.
const BUILTIN_DISPATCH: &[(u32, Dispatch)] = &[
    (
        BUILTIN_FIBER_RESUME,
        Dispatch::Control(ControlNative::Resume),
    ),
    (
        BUILTIN_NURSERY_JOIN,
        Dispatch::Control(ControlNative::NurseryJoin),
    ),
    (
        BUILTIN_NURSERY_OPEN,
        Dispatch::Control(ControlNative::NurseryOpen),
    ),
    (
        BUILTIN_NURSERY_SPAWN,
        Dispatch::Control(ControlNative::NurserySpawn),
    ),
    (
        BUILTIN_NURSERY_BOUNDARY,
        Dispatch::Control(ControlNative::NurseryBoundary),
    ),
    (
        BUILTIN_NURSERY_CANCEL,
        Dispatch::Control(ControlNative::NurseryCancel),
    ),
    (
        BUILTIN_REPL_PARK,
        Dispatch::Control(ControlNative::ReplPark),
    ),
    (BUILTIN_CHAN_NEW, Dispatch::Channel(ChannelNative::New)),
    (BUILTIN_CHAN_SEND, Dispatch::Channel(ChannelNative::Send)),
    (BUILTIN_CHAN_RECV, Dispatch::Channel(ChannelNative::Recv)),
    (BUILTIN_TIMEOUT, Dispatch::Timer(TimerNative::Timeout)),
    (
        BUILTIN_CHAN_SEND_DEADLINE,
        Dispatch::Channel(ChannelNative::SendDeadline),
    ),
    (
        BUILTIN_CHAN_RECV_DEADLINE,
        Dispatch::Channel(ChannelNative::RecvDeadline),
    ),
];

/// The intercepted natives' placeholder body (the channel steps and the
/// timer race): the dispatcher intercepts every `CALL` and `CALLT` site
/// ahead of the body, so a body that runs names hand-assembled code that
/// bypassed the interception — the same refusal the control natives'
/// bounce carries, as a trap.
fn chan_refuse_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Err(Trap::ReentrancyRefused)
}

// The arena natives (the read-side aliasing container): ordinary pure
// natives over the object store — no scheduler step, so no dispatcher
// interception; the handle packing and the slot discipline live in the
// store (`crate::object`).

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
fn arena_new_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(heaps.alloc_arena()))
}

fn arena_insert_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let arena = ref_arg(args, 0)?;
    // the one element word (the staging's one-word element policy)
    let handle = heaps.arena_insert(arena, args[1])?;
    Ok(NativeResult::Word(Value::I64(handle)))
}

fn arena_remove_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let arena = ref_arg(args, 0)?;
    let handle = i64_arg(args, 1)?;
    heaps.arena_remove(arena, handle)?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

fn arena_at_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let arena = ref_arg(args, 0)?;
    let handle = i64_arg(args, 1)?;
    // a `Ref` element answers as one retained count (the arena keeps
    // its own), a scalar word verbatim; a dangling handle traps here
    let word = heaps.arena_at(arena, handle)?;
    Ok(NativeResult::Word(word))
}

/// The native convention's second host reference (beside the object
/// store): where print/println write — process stdout by default, a
/// captured buffer for tests and embedders ([`crate::Vm::capture_output`])
/// — plus the VM-lifetime host state that is not heap state, which rides
/// this reference because a native body sees exactly the two (the store,
/// the sink). The net natives' connection table is that state (crate::net):
/// sockets are VM-lifetime like heap objects, but the store is the
/// per-fiber half a split hands a body, while this sink stays VM-level.
#[derive(Debug, Default)]
pub struct OutputSink {
    /// Where print/println write.
    target: SinkTarget,
    /// Where eprint/eprintln write.
    err_target: ErrTarget,
    /// The process arguments the `args` op answers (the CLI process
    /// surface): the host stages them, the VM never derives them — an
    /// embedder's `args()` reads exactly what its host staged.
    argv: Vec<String>,
    /// The connection table of the net natives.
    net: Connections,
    /// The listener table of the net natives' server half
    /// (`crate::net`).
    listeners: crate::net::Listeners,
    /// The process table of the capture half's natives (`crate::proc`).
    procs: crate::proc::Procs,
    /// The file-handle table of the fsys natives (`crate::fsys`).
    files: crate::fsys::Files,
    /// The close seams' wake notes (item 79): each close native notes
    /// the handle it closed, the dispatcher's native call site draining
    /// the notes to wake the parked waits the closed handles name — the
    /// one door the sink's own state reaches the wait table through,
    /// since a native body sees exactly the store and the sink.
    closed: Vec<(crate::sched::CloseSeam, i64)>,
}

/// The write target of an [`OutputSink`].
#[derive(Debug, Default, PartialEq)]
enum SinkTarget {
    /// No capture requested yet; stdout. `Default` so `Vm::new` stays
    /// cheap and the switch to `Captured` is explicit.
    #[default]
    Stdout,
    /// The in-memory sink.
    Captured(Vec<u8>),
}

/// The stderr write target of an [`OutputSink`].
#[derive(Debug, Default, PartialEq)]
enum ErrTarget {
    /// No capture requested yet; the process's stderr.
    #[default]
    Stderr,
    /// The in-memory sink, switched with [`OutputSink::capture`].
    Captured(Vec<u8>),
}

impl OutputSink {
    /// A fresh in-memory sink (tests and embedders construct the capture
    /// mode directly; the VM reaches it through [`crate::Vm::capture_output`]).
    #[must_use]
    pub fn captured() -> Self {
        Self {
            target: SinkTarget::Captured(Vec::new()),
            err_target: ErrTarget::Captured(Vec::new()),
            argv: Vec::new(),
            net: Connections::default(),
            listeners: crate::net::Listeners::default(),
            procs: crate::proc::Procs::default(),
            files: crate::fsys::Files::default(),
            closed: Vec::new(),
        }
    }

    /// Switches the write targets to fresh in-memory buffers, keeping
    /// every other field.
    pub(crate) fn capture(&mut self) {
        self.target = SinkTarget::Captured(Vec::new());
        self.err_target = ErrTarget::Captured(Vec::new());
    }

    /// The bytes captured so far, empty in stdout mode.
    #[must_use]
    pub fn captured_bytes(&self) -> &[u8] {
        match &self.target {
            SinkTarget::Captured(buf) => buf,
            SinkTarget::Stdout => &[],
        }
    }

    /// The stderr bytes captured so far, empty in stderr mode.
    #[must_use]
    pub fn captured_err_bytes(&self) -> &[u8] {
        match &self.err_target {
            ErrTarget::Captured(buf) => buf,
            ErrTarget::Stderr => &[],
        }
    }

    /// Stages the process arguments the `args` op answers — the host's
    /// own wording of the run's command line (the CLI passes its
    /// `--args` words verbatim).
    pub fn set_argv(&mut self, argv: Vec<String>) {
        self.argv = argv;
    }

    /// The staged process arguments.
    #[must_use]
    pub(crate) fn argv(&self) -> &[String] {
        &self.argv
    }

    /// The connection table of the net natives.
    pub(crate) fn net(&mut self) -> &mut Connections {
        &mut self.net
    }

    /// The listener table of the net natives' server half.
    pub(crate) fn listeners(&mut self) -> &mut crate::net::Listeners {
        &mut self.listeners
    }

    /// The process table of the capture half's natives.
    pub(crate) fn procs(&mut self) -> &mut crate::proc::Procs {
        &mut self.procs
    }

    /// The file-handle table of the fsys natives (`crate::fsys`).
    pub(crate) fn files(&mut self) -> &mut crate::fsys::Files {
        &mut self.files
    }

    /// Notes one close seam's handle for the wake door (the close
    /// native's own half of the wake-on-close contract).
    pub(crate) fn note_closed(&mut self, seam: crate::sched::CloseSeam, id: i64) {
        self.closed.push((seam, id));
    }

    /// Takes the close seams' notes: the dispatcher's native call site
    /// drains the journal after every native call, so a note never
    /// outlives the call that wrote it.
    pub(crate) fn take_closed(&mut self) -> Vec<(crate::sched::CloseSeam, i64)> {
        std::mem::take(&mut self.closed)
    }

    /// Bounds every blocking net wait ([`crate::net`]) — the connect,
    /// data, and accept waits together.
    pub(crate) fn set_net_timeout(&mut self, timeout: Duration) {
        self.net.set_timeout(timeout);
        self.listeners.set_timeout(timeout);
    }

    /// Flips the net face to the parking discipline (a reactor was
    /// installed on the VM): registered connections and bound
    /// listeners go non-blocking, `net_recv`/`net_send`/`net_accept`
    /// answering park requests on a would-block.
    pub(crate) fn enable_net_parking(&mut self) {
        self.net.enable_parking();
        self.listeners.enable_parking();
    }

    /// Flips the process face's blocking rows to the parking
    /// discipline (a reactor was installed on the VM): the read rows
    /// answer park requests on a would-block over the child's pipes,
    /// the wait row over its pidfd — Linux alone, the pipes owning no
    /// portable non-blocking shape (`crate::proc`).
    pub(crate) fn enable_proc_parking(&mut self) {
        self.procs.enable_parking();
    }

    /// Bounds every parked proc wait ([`crate::proc`]); the blocking
    /// twins stay unbounded whatever the bound.
    pub(crate) fn set_proc_timeout(&mut self, timeout: Duration) {
        self.procs.set_timeout(timeout);
    }

    /// Installs the host's blocking-drive stop flag (the 2026-11-24
    /// signal ruling's non-reactor twin): the bounded blocking accept
    /// poll and the blocking proc wait twin observe it between their
    /// poll rounds and answer the shutdown trap — a
    /// reactor-installing host's parked twin rides the registered
    /// shutdown interest instead and never reads this flag.
    pub(crate) fn set_shutdown_flag(
        &mut self,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.listeners.set_stop_flag(std::sync::Arc::clone(&stop));
        self.procs.set_stop_flag(stop);
    }

    /// Pins the TLS client of the net face's tls tier (`[crate::tls]`):
    /// custom trust roots for an embedding host or a test, the default
    /// being the webpki root set.
    pub fn set_tls_config(&mut self, config: std::sync::Arc<rustls::ClientConfig>) {
        self.net.set_tls_config(config);
    }

    /// Pins the TLS server identity of the net face's tls tier
    /// (`[crate::tls]`'s server half): no default identity exists, the
    /// unprovisioned `net_accept_tls` answering its own refusal.
    pub fn set_tls_server_config(&mut self, config: std::sync::Arc<rustls::ServerConfig>) {
        self.net.set_tls_server_config(config);
    }

    /// Advances one parked accept-tls machine one step (the machine's
    /// row and its session the connection table owns, the accept stage
    /// probing the listener table it names).
    pub(crate) fn tls_accept_wake(&mut self, accept: i64) -> crate::net::AcceptWake {
        self.net.tls_accept_wake(accept, &mut self.listeners)
    }

    /// Writes `text` with no newline.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_str(&mut self, text: &str) -> Result<(), Trap> {
        match &mut self.target {
            // Stdout is line-buffered, so println's trailing newline is
            // the flush point; print keeps its no-newline contract
            SinkTarget::Stdout => std::io::stdout()
                .write_all(text.as_bytes())
                .map_err(|e| Trap::OutputWrite { kind: e.kind() }),
            SinkTarget::Captured(buf) => {
                buf.extend_from_slice(text.as_bytes());
                Ok(())
            },
        }
    }

    /// Writes `text` followed by one newline.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_line(&mut self, text: &str) -> Result<(), Trap> {
        self.write_str(text)?;
        self.write_str("\n")
    }

    /// Writes `text` to stderr with no newline — the eprint/eprintln
    /// target, captured beside stdout in capture mode.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_err_str(&mut self, text: &str) -> Result<(), Trap> {
        match &mut self.err_target {
            ErrTarget::Stderr => std::io::stderr()
                .write_all(text.as_bytes())
                .map_err(|e| Trap::OutputWrite { kind: e.kind() }),
            ErrTarget::Captured(buf) => {
                buf.extend_from_slice(text.as_bytes());
                Ok(())
            },
        }
    }

    /// Writes `text` to stderr followed by one newline.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_err_line(&mut self, text: &str) -> Result<(), Trap> {
        self.write_err_str(text)?;
        self.write_err_str("\n")
    }
}

/// Reads the `Ref` word at `index` — the full owner-routing pair —
/// refusing like a mismatched slot with the offending argument's own
/// kind (never another argument's).
pub(crate) fn ref_arg(args: &[Value], index: usize) -> Result<RefId, Trap> {
    match args.get(index) {
        Some(Value::Ref(word)) => Ok(*word),
        // an arena temporary has no count and no object identity; a
        // native's reference argument is a count-bearing hand-off the
        // placement proof keeps designated temporaries away from
        Some(Value::Arena(_)) => Err(Trap::ArenaHandoff {
            slot: u8::try_from(index).expect("slot index fits u8"),
        }),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::Ref,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// Reads the `i64` word at `index`, refusing with the offending
/// argument's own kind.
pub(crate) fn i64_arg(args: &[Value], index: usize) -> Result<i64, Trap> {
    match args.get(index) {
        Some(Value::I64(v)) => Ok(*v),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::I64,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// Reads the `f64` word at `index`, refusing with the offending
/// argument's own kind.
pub(crate) fn f64_arg(args: &[Value], index: usize) -> Result<f64, Trap> {
    match args.get(index) {
        Some(Value::F64(v)) => Ok(*v),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::F64,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

pub(crate) fn println_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_line(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(crate) fn print_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_str(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `eprintln`: the console face's stderr twin of [`println_fn`].
pub(crate) fn eprintln_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_err_line(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `eprint`: the console face's stderr twin of [`print_fn`].
pub(crate) fn eprint_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_err_str(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `read_line`: one line from the process's stdin, the checked ok/none
/// span — end of input (or a stdin the process cannot read) answers the
/// none member, never a trap. The line answer strips its trailing
/// newline (and a CRLF pair's carriage return).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn read_line_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    Ok(match read {
        // zero bytes is end of input; a read failure folds into the
        // same none member (the checked read's convention)
        Ok(0) | Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: heaps.new_str(line),
            }
        },
    })
}

/// The bare trap primitive's body: the message rides the trap out to the
/// drive; no print, no exit decision — the host's halt policy applies
/// above the engine. Never answers.
pub(crate) fn trap_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    Err(Trap::Die {
        message: text.to_owned(),
    })
}

/// A scalar's rendered text, in a fixed stack buffer: `str_int` and
/// `str_float` hand the bytes to the fresh object as one exact-size
/// allocation, where `to_string` grows a `String` whose move into a
/// `Box<str>` then shrinks that buffer again.
struct ScalarText {
    bytes: [u8; ScalarText::CAP],
    len: usize,
}

impl ScalarText {
    /// `f64`'s Display spells every digit out, so the smallest subnormal
    /// (5e-324, 326 bytes) and its negative form are the widest.
    const CAP: usize = 384;

    fn new() -> Self {
        Self {
            bytes: [0; Self::CAP],
            len: 0,
        }
    }

    /// The rendered text; the sink only ever stores whole `&str` chunks.
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).expect("the sink stores &str chunks")
    }
}

impl core::fmt::Write for ScalarText {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.len + text.len();
        let room = self.bytes.get_mut(self.len..end).ok_or(core::fmt::Error)?;
        room.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// One scalar's fresh string object: the stack render, falling back to the
/// growing path for a rendering past the buffer.
fn scalar_str(heaps: &mut Heaps, scalar: impl core::fmt::Display) -> Value {
    let mut text = ScalarText::new();
    match core::fmt::write(&mut text, format_args!("{scalar}")) {
        Ok(()) => heaps.new_str_part(text.as_str()),
        Err(core::fmt::Error) => heaps.new_str(scalar.to_string()),
    }
}

fn str_int_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::I64(v)] => Ok(NativeResult::Word(scalar_str(heaps, *v))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn str_float_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(v)] => Ok(NativeResult::Word(scalar_str(heaps, *v))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn str_fixed_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(v), Value::I64(digits)] => {
            // the widest exact f64 fraction bounds the width; more
            // digits past it only answer trailing zeros the value
            // cannot distinguish
            // the clamp pins the width non-negative, so the usize cast
            // cannot lose the sign
            let digits = (*digits).clamp(0, 767);
            let digits = usize::try_from(digits).expect("the clamped width fits usize");
            Ok(NativeResult::Word(heaps.new_str(format!("{v:.digits$}"))))
        },
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn str_bool_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::Bool(v)] => Ok(NativeResult::Word(heaps.new_intern(if *v {
            "true"
        } else {
            "false"
        }))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::Bool,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// The io class of one OS failure: the `io_err` member the platform
/// discriminant answers by error class — every kind the classes name
/// lands on its tag, the rest folding into the `io_other` floor.
#[must_use]
pub(crate) fn io_class(err: &std::io::Error) -> u8 {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound => PLATFORM_IO_NOT_FOUND,
        ErrorKind::PermissionDenied => PLATFORM_IO_PERMISSION_DENIED,
        ErrorKind::AlreadyExists => PLATFORM_IO_ALREADY_EXISTS,
        ErrorKind::TimedOut | ErrorKind::WouldBlock => PLATFORM_IO_TIMED_OUT,
        ErrorKind::UnexpectedEof => PLATFORM_IO_UNEXPECTED_EOF,
        _ => PLATFORM_IO_OTHER,
    }
}

/// The `io_err` span: the failure's class discriminant with the OS
/// error rendered as `"<op> <path>: <error>"` in the payload word (the
/// message rides the `io_other` member alone; the other classes drop
/// it at the re-span).
pub(crate) fn io_err(
    heaps: &mut Heaps,
    op: &str,
    path: &str,
    err: &std::io::Error,
) -> NativeResult {
    NativeResult::Tag {
        tag: io_class(err),
        payload: heaps.new_str(format!("{op} {path}: {err}")),
    }
}

pub(crate) fn read_file_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::fs::read_to_string(&path) {
        Ok(text) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(text),
        },
        Err(err) => io_err(heaps, "read_file", &path, &err),
    })
}

pub(crate) fn write_file_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    // the contents stay a borrow of the store: the path is owned for the
    // error arm's message alone, and writing reads the bytes once
    let contents = heaps.str_at(ref_arg(args, 1)?)?;
    Ok(match std::fs::write(&path, contents.as_bytes()) {
        // create-or-truncate; the payload word is the inert unit word
        Ok(()) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(0),
        },
        Err(err) => io_err(heaps, "write_file", &path, &err),
    })
}

pub(crate) fn file_exists_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // total by contract: an entry the process cannot stat answers false
    let path = heaps.str_at(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::Bool(Path::new(path).exists())))
}

pub(crate) fn fs_list_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let dir = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    // sorted, so listing order never depends on the platform's readdir
    let names = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|d| d.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<String>, _>>(),
        Err(err) => Err(err),
    };
    let mut names = match names {
        Ok(names) => names,
        Err(err) => return Ok(io_err(heaps, "fs_list", &dir, &err)),
    };
    names.sort_unstable();
    let mut elems = Vec::with_capacity(names.len());
    for name in names {
        elems.push(heaps.new_str(name));
    }
    // each fresh name's birth count becomes the list's edge, so the
    // frame claim its alloc entered leaves with the adoption
    for elem in &elems {
        if let Value::Ref(word) = *elem {
            heaps.release_claim(word);
        }
    }
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: heaps.new_list(elems),
    })
}

/// `time() -> int`: the body behind the platform ABI's `time` op —
/// epoch nanoseconds, the duration since the Unix epoch clamped at
/// zero before it (the wall clock; durations measured over it bend
/// with the host's own clock adjustments).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn time_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_nanos())
        .unwrap_or_default();
    Ok(NativeResult::Word(Value::I64(
        i64::try_from(nanos).unwrap_or(i64::MAX),
    )))
}

/// The monotonic clock's origin: fixed at the process's first `mono`
/// read, an arbitrary point whose only contract is that later reads
/// never go backward — differences between two reads measure elapsed
/// time, the absolute word carries no meaning.
static MONO_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `mono() -> int`: the body behind the platform ABI's `mono` op (the
/// time face's timer capability, the 2026-11-21 ruling) — monotonic
/// nanoseconds since the process-fixed origin, never moving backward
/// and never bending with the host's wall-clock adjustments. The
/// natural pair for `sleep`: a span measured over `mono` words is the
/// span `sleep` waits.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn mono_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let nanos = MONO_ORIGIN.elapsed().as_nanos();
    Ok(NativeResult::Word(Value::I64(
        i64::try_from(nanos).unwrap_or(i64::MAX),
    )))
}

/// `sleep(nanos: int) -> ()`: the body behind the platform ABI's
/// `sleep` op — park the calling fiber for at least the requested
/// nanoseconds (a negative span answers at once), the timer-only wait
/// shape: no IO interest, the deadline wheel's firing the completion
/// itself. Under no installed reactor the blocking twin sleeps the
/// thread — the embedder's platform choice, never a language fact.
pub(crate) fn sleep_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let nanos = i64_arg(args, 0)?;
    let span = Duration::from_nanos(nanos.max(0).cast_unsigned());
    if out.net().parking() {
        let deadline = Instant::now() + span;
        return Ok(NativeResult::Park(crate::sched::WorldWait::sleep(deadline)));
    }
    std::thread::sleep(span);
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(crate) fn file_size_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::fs::metadata(&path) {
        // no filesystem carries a length past i64::MAX
        Ok(meta) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::try_from(meta.len()).expect("file size fits i64")),
        },
        Err(err) => io_err(heaps, "file_size", &path, &err),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        claims::ClaimTable,
        fiber::{FIBER_ROOT, Fiber},
        object::Objects,
    };

    /// The builtin table is the abi's row set: every row lands on its reserved slot with its own
    /// name, parameter kinds, and result width, and every slot no row names stays a trap.
    #[test]
    fn the_builtin_table_is_row_driven() {
        let fns = builtin_module();
        assert_eq!(fns.len(), BUILTIN_CHAN_RECV_DEADLINE as usize + 1);
        for row in ds_abi::native::rows(NativeModule::Builtin) {
            let slot = &fns[row.index as usize];
            assert_eq!(slot.name, row.name, "{}'s slot", row.name);
            assert_eq!(slot.params, row.params, "{}'s params", row.name);
            assert_eq!(slot.variadic, row.variadic, "{}'s staging", row.name);
            assert_eq!(
                slot.result_words,
                row.shape.result_words(),
                "{}'s result width",
                row.name
            );
        }
        // a gap keeps its position as a refused slot: the retired world
        // rows, the string block's spare, the retired yield, the generic
        // rows' monomorphic slots, and the retired `head`
        for index in [
            0u32, 1, 5, 6, 7, 8, 9, 10, 21, 22, 23, 24, 29, 30, 32, 33, 34, 35, 36, 37, 38, 39, 40,
            46, 47, 48, 59, 83, 90,
        ] {
            assert_eq!(fns[index as usize].name, "reserved", "index {index}");
            assert!(
                ds_abi::native::rows(NativeModule::Builtin).all(|row| row.index != index),
                "index {index} carries no row"
            );
        }
    }

    /// The intercepted slots are the dispatcher's: the control and channel natives and the timer
    /// race, each marked on its own row's slot.
    #[test]
    fn the_intercepted_slots_carry_their_dispatch_marker() {
        let fns = builtin_module();
        for (index, control) in [
            (BUILTIN_FIBER_RESUME, ControlNative::Resume),
            (BUILTIN_NURSERY_JOIN, ControlNative::NurseryJoin),
            (BUILTIN_NURSERY_OPEN, ControlNative::NurseryOpen),
            (BUILTIN_NURSERY_SPAWN, ControlNative::NurserySpawn),
            (BUILTIN_NURSERY_BOUNDARY, ControlNative::NurseryBoundary),
            (BUILTIN_NURSERY_CANCEL, ControlNative::NurseryCancel),
            (BUILTIN_REPL_PARK, ControlNative::ReplPark),
        ] {
            assert_eq!(fns[index as usize].control, Some(control), "index {index}");
        }
        for (index, channel) in [
            (BUILTIN_CHAN_NEW, ChannelNative::New),
            (BUILTIN_CHAN_SEND, ChannelNative::Send),
            (BUILTIN_CHAN_RECV, ChannelNative::Recv),
            (BUILTIN_CHAN_SEND_DEADLINE, ChannelNative::SendDeadline),
            (BUILTIN_CHAN_RECV_DEADLINE, ChannelNative::RecvDeadline),
        ] {
            assert_eq!(fns[index as usize].channel, Some(channel), "index {index}");
        }
        assert_eq!(
            fns[BUILTIN_TIMEOUT as usize].timer,
            Some(TimerNative::Timeout)
        );
        // no other slot carries an interception
        let count = |f: &NativeFunction| u8::from(f.control.is_some());
        assert_eq!(fns.iter().map(count).sum::<u8>(), 7, "the control natives");
        assert_eq!(
            fns.iter().filter(|f| f.channel.is_some()).count(),
            5,
            "the channel natives"
        );
        assert_eq!(
            fns.iter().filter(|f| f.timer.is_some()).count(),
            1,
            "the timer race"
        );
    }

    /// The generic rows address reserved monomorphic slots and resolve to their one body with the
    /// row's own evidence arity and result width.
    #[test]
    fn the_generic_rows_resolve_to_their_one_body() {
        let fns = builtin_module();
        for row in ds_abi::native::GENERIC_NATIVES {
            assert_eq!(
                fns[row.index as usize].name, "reserved",
                "{}'s slot",
                row.name
            );
            let body = resolve_generic(BUILTIN_MODULE, row.index)
                .unwrap_or_else(|| panic!("the generic {} body resolves", row.name));
            assert_eq!(body.name, row.name);
            assert_eq!(body.evidence, row.evidence);
            assert_eq!(body.params, row.params);
            assert_eq!(body.staging, row.staging);
            assert_eq!(body.result_words, row.shape.result_words());
        }
        assert!(
            resolve_generic(STD_PLATFORM_NAME, BUILTIN_LEN).is_none(),
            "the generic bodies live on the builtin surface alone"
        );
        assert_eq!(
            GENERIC_BODIES.len(),
            ds_abi::native::GENERIC_NATIVES.len(),
            "every generic row carries exactly one body"
        );
    }

    #[test]
    fn reserved_gap_entries_trap_like_uninstalled_indices() {
        // each module's gaps report the module's own name and their index,
        // the same UnknownCallee an out-of-range index yields at resolution
        let traps_like = |module: &str, index: u32, fns: &[NativeFunction]| {
            let mut objects = Objects::new(FIBER_ROOT);
            let mut fibers: Vec<Fiber> = Vec::new();
            let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
            let mut bare = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
            assert_eq!(
                (fns[index as usize].func)(&mut bare, &mut OutputSink::captured(), &[]),
                Err(Trap::UnknownCallee {
                    module: module.into(),
                    func: index,
                })
            );
        };
        let core = builtin_module();
        traps_like(BUILTIN_MODULE, 0, &core); // the console pair's slots
        traps_like(BUILTIN_MODULE, 5, &core); // the file-IO slots
        traps_like(BUILTIN_MODULE, 24, &core); // the retired fiber_yield slot
        traps_like(BUILTIN_MODULE, 59, &core); // the retired parse_json neighbor gap
    }

    #[test]
    fn println_writes_the_line_print_does_not() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let id = heaps.current().intern("hello");
        let mut out = OutputSink::captured();
        assert_eq!(
            println_fn(&mut heaps, &mut out, &[Value::Ref(RefId::new(
                FIBER_ROOT, id
            ))]),
            Ok(NativeResult::Word(Value::I64(0)))
        );
        assert_eq!(out.captured_bytes(), b"hello\n");
        assert_eq!(
            print_fn(&mut heaps, &mut out, &[Value::Ref(RefId::new(
                FIBER_ROOT, id
            ))]),
            Ok(NativeResult::Word(Value::I64(0)))
        );
        assert_eq!(out.captured_bytes(), b"hello\nhello");
    }

    #[test]
    fn str_conversions_cover_the_scalar_types() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let v = str_int_fn(&mut heaps, &mut out, &[Value::I64(-1)])
            .expect("int converts")
            .word()
            .expect("a single word");
        assert_eq!(
            heaps.str_at(match v {
                Value::Ref(word) => word,
                _ => panic!("a string ref"),
            }),
            Ok("-1")
        );
        let v = str_float_fn(&mut heaps, &mut out, &[Value::F64(2.5)])
            .expect("float converts")
            .word()
            .expect("a single word");
        assert_eq!(
            heaps.str_at(match v {
                Value::Ref(word) => word,
                _ => panic!("a string ref"),
            }),
            Ok("2.5")
        );
        let v = str_bool_fn(&mut heaps, &mut out, &[Value::Bool(false)])
            .expect("bool converts")
            .word()
            .expect("a single word");
        assert_eq!(
            heaps.str_at(match v {
                Value::Ref(word) => word,
                _ => panic!("a string ref"),
            }),
            Ok("false")
        );
    }

    /// The stack render covers every scalar's expansion and paints the
    /// text `to_string` paints: the widest `i64`, and `f64`'s widest
    /// Display (the smallest subnormal spells 326 bytes, 327 with the
    /// sign). Every listed rendering must fit the buffer, so a cap that
    /// stops covering a scalar fails here instead of silently taking the
    /// growing fallback.
    #[test]
    fn scalar_renderings_match_the_string_form() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let fits = |rendered: &dyn core::fmt::Display| {
            let mut text = ScalarText::new();
            assert!(
                core::fmt::write(&mut text, format_args!("{rendered}")).is_ok(),
                "{rendered} exceeds the stack buffer"
            );
        };
        for v in [i64::MIN, -1, 0, 42, i64::MAX] {
            fits(&v);
            let Value::Ref(word) = str_int_fn(&mut heaps, &mut out, &[Value::I64(v)])
                .expect("int converts")
                .word()
                .expect("a single word")
            else {
                panic!("a string ref")
            };
            let want = v.to_string();
            assert_eq!(heaps.str_at(word), Ok(want.as_str()));
        }
        for v in [
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::MIN,
            f64::MAX,
            2.5,
            f64::INFINITY,
            f64::NAN,
        ] {
            fits(&v);
            let Value::Ref(word) = str_float_fn(&mut heaps, &mut out, &[Value::F64(v)])
                .expect("float converts")
                .word()
                .expect("a single word")
            else {
                panic!("a string ref")
            };
            let want = v.to_string();
            assert_eq!(heaps.str_at(word), Ok(want.as_str()));
        }
    }

    /// `trim` copies the inner byte range into a fresh object: whitespace
    /// on either side alone, all-whitespace, the empty subject, and
    /// multi-byte whitespace around multi-byte text keep the boundaries
    /// exact and never hand back the source.
    #[test]
    fn trim_copies_the_inner_range_into_a_fresh_object() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        for (text, want) in [
            (" \t héllo \r\n", "héllo"),
            ("héllo ", "héllo"),
            (" héllo", "héllo"),
            ("\u{3000}漢字\u{3000}", "漢字"),
            ("   ", ""),
            ("", ""),
        ] {
            let word = heaps.new_intern(text);
            let Value::Ref(src) = word else {
                panic!("an interned word")
            };
            let Value::Ref(part) = trim_fn(&mut heaps, &mut out, &[word])
                .expect("trim runs")
                .word()
                .expect("a single word")
            else {
                panic!("a string ref")
            };
            assert_eq!(heaps.str_at(part), Ok(want), "trimmed {text:?}");
            assert_ne!(part, src, "a fresh object for {text:?}");
        }
    }

    /// The float math family answers IEEE over the f64 word and stays
    /// total: a negative `sqrt`, a NaN propagation, and a negative base
    /// to a non-integer exponent all answer NaN, never a trap; `floor`/
    /// `ceil` keep the signed zero and round the negative halves away
    /// from and toward zero; `abs` clears the sign bit; `round` carries
    /// the half away from zero and `trunc` keeps the sign of the zero
    /// it collapses to; `log`/`exp` answer the limit rows (0 to
    /// negative infinity, the exponentials to 0 and infinity).
    #[test]
    fn float_math_family_answers_ieee_and_stays_total() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let mut unary = |f: NativeFn, x: f64| {
            let Value::F64(v) = f(&mut heaps, &mut out, &[Value::F64(x)])
                .expect("the float family is total")
                .word()
                .expect("a single word")
            else {
                panic!("an f64 word")
            };
            v
        };
        assert_eq!(unary(sqrt_fn, 4.0), 2.0);
        assert_eq!(unary(sqrt_fn, f64::INFINITY), f64::INFINITY);
        assert!(unary(sqrt_fn, -1.0).is_nan(), "a negative sqrt answers NaN");
        assert!(unary(sqrt_fn, f64::NAN).is_nan(), "NaN propagates");
        assert_eq!(unary(sin_fn, 0.0), 0.0);
        assert_eq!(unary(cos_fn, 0.0), 1.0);
        assert!(unary(sin_fn, f64::NAN).is_nan());
        assert!(
            unary(cos_fn, f64::INFINITY).is_nan(),
            "cos of infinity is NaN"
        );
        assert_eq!(unary(floor_fn, 2.5), 2.0);
        assert_eq!(unary(ceil_fn, 2.5), 3.0);
        assert_eq!(unary(floor_fn, -2.5), -3.0, "floor rounds away from zero");
        assert_eq!(unary(ceil_fn, -2.5), -2.0, "ceil rounds toward zero");
        let floored_zero = unary(floor_fn, -0.5);
        assert_eq!(floored_zero, -1.0);
        let floored_signed = unary(floor_fn, -0.0);
        assert_eq!(floored_signed, 0.0);
        assert!(
            floored_signed.is_sign_negative(),
            "floor keeps the signed zero"
        );
        assert_eq!(unary(abs_fn, -2.5), 2.5);
        let abs_zero = unary(abs_fn, -0.0);
        assert_eq!(abs_zero, 0.0);
        assert!(abs_zero.is_sign_positive(), "abs clears the sign bit");
        assert_eq!(unary(abs_fn, f64::NEG_INFINITY), f64::INFINITY);
        assert!(unary(abs_fn, f64::NAN).is_nan());
        assert_eq!(unary(round_fn, 2.5), 3.0, "the half rounds away from zero");
        assert_eq!(unary(round_fn, 2.4), 2.0);
        assert_eq!(unary(round_fn, -2.5), -3.0);
        assert_eq!(unary(round_fn, f64::INFINITY), f64::INFINITY);
        assert!(unary(round_fn, f64::NAN).is_nan());
        assert_eq!(unary(trunc_fn, 2.9), 2.0);
        assert_eq!(
            unary(trunc_fn, -2.9),
            -2.0,
            "trunc discards the fraction, never floors"
        );
        let truncated = unary(trunc_fn, -0.5);
        assert_eq!(truncated, 0.0);
        assert!(
            truncated.is_sign_negative(),
            "trunc keeps the sign of the zero"
        );
        assert_eq!(unary(log_fn, 1.0), 0.0);
        assert_eq!(unary(log_fn, 0.0), f64::NEG_INFINITY);
        assert_eq!(unary(log_fn, f64::INFINITY), f64::INFINITY);
        assert!(unary(log_fn, -1.0).is_nan(), "a negative log answers NaN");
        assert_eq!(unary(exp_fn, 0.0), 1.0);
        assert_eq!(unary(exp_fn, 1.0), std::f64::consts::E);
        assert_eq!(unary(exp_fn, f64::NEG_INFINITY), 0.0);
        assert_eq!(unary(exp_fn, f64::INFINITY), f64::INFINITY);
        assert!(unary(exp_fn, f64::NAN).is_nan());
        assert_eq!(unary(tan_fn, 0.0), 0.0);
        assert!(
            unary(tan_fn, f64::INFINITY).is_nan(),
            "tan of infinity is NaN"
        );
        assert!(unary(tan_fn, f64::NAN).is_nan());
    }

    /// The two-argument float natives stage (y, x) for `atan2` and
    /// (base, exp) for `pow`: the quadrant reads off both signs — the
    /// signed zeros included — and `pow` answers the IEEE edges (0^0 is
    /// 1, a negative base to a non-integer exponent is NaN).
    #[test]
    fn atan2_reads_the_quadrant_and_pow_answers_the_edges() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let mut binary = |f: NativeFn, a: f64, b: f64| {
            let Value::F64(v) = f(&mut heaps, &mut out, &[Value::F64(a), Value::F64(b)])
                .expect("the float family is total")
                .word()
                .expect("a single word")
            else {
                panic!("an f64 word")
            };
            v
        };
        assert_eq!(binary(atan2_fn, 1.0, 1.0), std::f64::consts::FRAC_PI_4);
        assert_eq!(
            binary(atan2_fn, 1.0, -1.0),
            3.0 * std::f64::consts::FRAC_PI_4
        );
        assert_eq!(
            binary(atan2_fn, -1.0, -1.0),
            -3.0 * std::f64::consts::FRAC_PI_4,
            "the third quadrant stays negative"
        );
        assert_eq!(binary(atan2_fn, 0.0, -1.0), std::f64::consts::PI);
        assert_eq!(
            binary(atan2_fn, -0.0, -1.0),
            -std::f64::consts::PI,
            "the signed zero carries the quadrant"
        );
        assert_eq!(binary(atan2_fn, 0.0, 0.0), 0.0);
        assert_eq!(binary(pow_fn, 2.0, 10.0), 1024.0);
        assert_eq!(binary(pow_fn, 2.0, -1.0), 0.5);
        assert_eq!(binary(pow_fn, 0.0, 0.0), 1.0);
        assert!(
            binary(pow_fn, -8.0, 1.0 / 3.0).is_nan(),
            "a negative base to a non-integer exponent is NaN"
        );
    }

    /// The string search family: `contains` answers the membership read
    /// (the empty needle always occurring), `index_of` the first
    /// occurrence's char index under the ok discriminant — `substring`'s
    /// indexing, multi-byte text counting scalars — a miss the `none`
    /// member, and `replace` swaps every non-overlapping occurrence in
    /// one left-to-right pass, the inserted text never re-scanned and
    /// the empty needle inserting `with` at every char boundary.
    #[test]
    fn string_search_family_covers_the_checked_edges() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        for (haystack, needle, want) in [
            ("héllo", "llo", true),
            ("héllo", "él", true),
            ("héllo", "x", false),
            ("héllo", "", true),
            ("", "", true),
            ("", "a", false),
        ] {
            let args = [heaps.new_intern(haystack), heaps.new_intern(needle)];
            assert_eq!(
                contains_fn(&mut heaps, &mut out, &args).expect("contains runs"),
                NativeResult::Word(Value::Bool(want)),
                "contains({haystack:?}, {needle:?})"
            );
        }
        for (haystack, needle, want) in [
            ("héllo", "llo", Some(2)),
            ("héllo", "él", Some(1)),
            ("héllo", "héllo", Some(0)),
            ("héllo", "", Some(0)),
            ("", "", Some(0)),
            ("héllo", "x", None),
            ("ab", "abcdef", None),
        ] {
            let args = [heaps.new_intern(haystack), heaps.new_intern(needle)];
            let want = match want {
                Some(index) => NativeResult::Tag {
                    tag: PLATFORM_IO_OK,
                    payload: Value::I64(index),
                },
                None => NativeResult::Tag {
                    tag: PLATFORM_NONE,
                    payload: Value::I64(0),
                },
            };
            assert_eq!(
                index_of_fn(&mut heaps, &mut out, &args).expect("index_of runs"),
                want,
                "index_of({haystack:?}, {needle:?})"
            );
        }
        for (haystack, needle, with, want) in [
            ("banana", "an", "AN", "bANANa"),
            ("abc", "x", "y", "abc"),
            ("ab", "", "-", "-a-b-"),
            ("", "", "-", "-"),
            ("aaa", "aa", "b", "ba"),
            ("a", "a", "aa", "aa"),
            ("héllo héllo", "héllo", "hei", "hei hei"),
        ] {
            let args = [
                heaps.new_intern(haystack),
                heaps.new_intern(needle),
                heaps.new_intern(with),
            ];
            let Value::Ref(word) = replace_fn(&mut heaps, &mut out, &args)
                .expect("replace runs")
                .word()
                .expect("a single word")
            else {
                panic!("a string ref")
            };
            assert_eq!(
                heaps.str_at(word),
                Ok(want),
                "replace({haystack:?}, {needle:?}, {with:?})"
            );
        }
    }

    /// `float_hash` folds every NaN payload to the one canonical NaN and
    /// the two zeros to one before hashing, so bit-structurally equal
    /// floats hash equal (B9): a payloaded qNaN, a negative NaN, and the
    /// canonical NaN answer identically, the zero pair likewise.
    #[test]
    fn float_hash_folds_nan_payloads_and_zeros_to_one() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let mut hash = |f: f64| {
            let Value::I64(word) = float_hash_fn(&mut heaps, &mut out, &[Value::F64(f)])
                .expect("float hashes")
                .word()
                .expect("a single word")
            else {
                panic!("an int word")
            };
            word
        };
        let canonical = hash(f64::NAN);
        for f in [
            f64::from_bits(0x7ff0_0000_0000_0001),
            f64::from_bits(0xfff0_0000_0000_0001),
        ] {
            assert_eq!(hash(f), canonical, "{f} folds to the canonical NaN");
        }
        assert_eq!(hash(0.0), hash(-0.0), "the zero pair shares bits");
    }

    /// `char_hash` and `str_hash` fold the same FNV-1a over the same
    /// UTF-8 bytes, so a char and its one-char string answer identically
    /// (the hash family's char/string ruling).
    #[test]
    fn char_hash_answers_str_hash_over_the_same_bytes() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        for text in ["a", "\u{6f22}"] {
            let word = heaps.new_intern(text);
            let Value::I64(as_char) = char_hash_fn(&mut heaps, &mut out, &[word])
                .expect("char hashes")
                .word()
                .expect("a single word")
            else {
                panic!("an int word")
            };
            let Value::I64(as_str) = str_hash_fn(&mut heaps, &mut out, &[word])
                .expect("string hashes")
                .word()
                .expect("a single word")
            else {
                panic!("an int word")
            };
            assert_eq!(as_char, as_str, "{text:?} hashes the same either way");
        }
    }

    #[test]
    fn wrong_typed_arguments_name_their_own_slot() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let s = heaps.current().intern("x");
        // ref_arg at index 1: concat's second word is the offender, so
        // the trap names its kind, not the first argument's
        assert_eq!(
            concat_fn(&mut heaps, &mut out, &[
                Value::Ref(RefId::new(FIBER_ROOT, s)),
                Value::I64(5)
            ]),
            Err(Trap::SlotTypeMismatch {
                slot: 1,
                expected: SlotType::Ref,
                actual: SlotType::I64,
            })
        );
        // max's pair must agree on one numeric kind: the bool word at
        // index 1 breaks the int pair, cited with the other side's kind
        assert_eq!(
            max_fn(&mut heaps, &mut out, &[Value::I64(1), Value::Bool(true)]),
            Err(Trap::SlotTypeMismatch {
                slot: 1,
                expected: SlotType::I64,
                actual: SlotType::Bool,
            })
        );
    }

    /// The parts a leading, trailing, or doubled separator leaves empty: the
    /// boundaries come off the subject alone, so an empty part stays an
    /// empty string in both separator widths.
    #[test]
    fn split_keeps_the_empty_parts_around_doubled_separators() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        for (text, sep, want) in [
            ("a,,b", ",", &["a", "", "b"] as &[&str]),
            (",a", ",", &["", "a"]),
            ("a,", ",", &["a", ""]),
            (",", ",", &["", ""]),
            ("a—b—", "—", &["a", "b", ""]),
            ("—a", "—", &["", "a"]),
            ("ab", "—", &["ab"]),
        ] {
            let text_id = heaps.current().intern(text);
            let sep_id = heaps.current().intern(sep);
            let Value::Ref(list) = split_fn(&mut heaps, &mut out, &[
                Value::Ref(RefId::new(FIBER_ROOT, text_id)),
                Value::I64(0), // the separator's union tag word
                Value::Ref(RefId::new(FIBER_ROOT, sep_id)),
            ])
            .expect("split runs")
            .word()
            .expect("a single word") else {
                panic!("a list ref")
            };
            let words = heaps.list_at(list).expect("the list is live").0.to_vec();
            let parts: Vec<String> = words
                .iter()
                .map(|elem| match elem {
                    Value::Ref(word) => heaps.str_at(*word).expect("a string part").to_owned(),
                    other => panic!("a string part, got {other:?}"),
                })
                .collect();
            let want: Vec<String> = want.iter().map(|part| (*part).to_owned()).collect();
            assert_eq!(parts, want, "{text:?} split on {sep:?}");
        }
    }

    /// The empty separator's per-character split: the parts fall on char
    /// boundaries, so a multi-byte scalar stays whole. Lowered code never
    /// stages the empty spelling over the `char` separator (every char
    /// word is one-char by construction); the arm answers hand-assembled
    /// frames, and this pins the answer it gives them.
    #[test]
    fn an_empty_separator_splits_per_character() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let text_id = heaps.current().intern("a—b");
        let sep_id = heaps.current().intern("");
        let Value::Ref(list) = split_fn(&mut heaps, &mut out, &[
            Value::Ref(RefId::new(FIBER_ROOT, text_id)),
            Value::I64(0), // the separator's union tag word
            Value::Ref(RefId::new(FIBER_ROOT, sep_id)),
        ])
        .expect("split runs")
        .word()
        .expect("a single word") else {
            panic!("a list ref")
        };
        let words = heaps.list_at(list).expect("the list is live").0.to_vec();
        let parts: Vec<String> = words
            .iter()
            .map(|elem| match elem {
                Value::Ref(word) => heaps.str_at(*word).expect("a string part").to_owned(),
                other => panic!("a string part, got {other:?}"),
            })
            .collect();
        assert_eq!(parts, ["a", "—", "b"]);
    }

    /// `join` pastes the elements at their exact byte lengths: the buffer
    /// is sized once, so multi-byte text and separators keep the content
    /// and the fresh object's conversion never shrinks it.
    #[test]
    fn join_sizes_the_buffer_at_the_exact_length() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        for (parts, sep, want) in [
            (&["a", "bb", "ccc"] as &[&str], ",", "a,bb,ccc"),
            (&["héllo", "漢字"], "—", "héllo—漢字"),
            (&["only"], "—", "only"),
            (&[], ",", ""),
        ] {
            let mut words = Vec::with_capacity(parts.len());
            for part in parts {
                let Value::Ref(word) = heaps.new_str((*part).to_owned()) else {
                    panic!("a string ref")
                };
                // the birth count becomes the list's edge
                heaps.release_claim(word);
                words.push(Value::Ref(word));
            }
            let Value::Ref(list) = heaps.new_list(words) else {
                panic!("a list ref")
            };
            let sep_word = heaps.new_str(sep.to_owned());
            let Value::Ref(joined) = join_fn(
                &mut heaps,
                &mut out,
                &[Value::Ref(list), Value::I64(0), sep_word], // the tag rides index 1
            )
            .expect("join runs")
            .word()
            .expect("a single word") else {
                panic!("a string ref")
            };
            assert_eq!(heaps.str_at(joined), Ok(want), "joined {parts:?}");
        }
    }

    /// The io error-class mapping: every kind a class names lands on
    /// its discriminant, and the kinds no io class names portably fold
    /// into the `io_other` floor.
    #[test]
    fn io_classes_map_kinds_and_fold_the_floor() {
        use std::io::ErrorKind;
        let kind = |k: ErrorKind| io_class(&std::io::Error::from(k));
        assert_eq!(kind(ErrorKind::NotFound), PLATFORM_IO_NOT_FOUND);
        assert_eq!(
            kind(ErrorKind::PermissionDenied),
            PLATFORM_IO_PERMISSION_DENIED
        );
        assert_eq!(kind(ErrorKind::AlreadyExists), PLATFORM_IO_ALREADY_EXISTS);
        assert_eq!(kind(ErrorKind::TimedOut), PLATFORM_IO_TIMED_OUT);
        assert_eq!(kind(ErrorKind::WouldBlock), PLATFORM_IO_TIMED_OUT);
        assert_eq!(kind(ErrorKind::UnexpectedEof), PLATFORM_IO_UNEXPECTED_EOF);
        // no class names a directory read or a broken pipe portably
        assert_eq!(kind(ErrorKind::IsADirectory), PLATFORM_IO_OTHER);
        assert_eq!(kind(ErrorKind::BrokenPipe), PLATFORM_IO_OTHER);
        assert_eq!(
            io_class(&std::io::Error::other("negative length")),
            PLATFORM_IO_OTHER
        );
    }

    /// The parse readers' position payload: the offset the reader gave
    /// up at — the first offending char, the end of a well-shaped but
    /// overflowing text, and the sign-only stop.
    #[test]
    fn parse_positions_name_the_stop_offset() {
        assert_eq!(int_stop("12x34"), 2);
        assert_eq!(int_stop(""), 0);
        assert_eq!(int_stop("-"), 1);
        assert_eq!(int_stop("+7"), 2);
        assert_eq!(
            int_stop("9223372036854775808"),
            19,
            "the overflow stops at the end"
        );
        assert_eq!(float_stop("1.5x"), 3);
        assert_eq!(float_stop("."), 0);
        assert_eq!(float_stop("1e"), 2);
        assert_eq!(float_stop("1e+"), 3);
        assert_eq!(float_stop("inf"), 3);
        assert_eq!(float_stop("-Inf"), 4);
    }

    #[test]
    fn parse_int_answers_both_span_members() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let ok = heaps.current().intern("-42");
        assert_eq!(
            parse_int_fn(&mut heaps, &mut out, &[Value::Ref(RefId::new(
                FIBER_ROOT, ok
            ))]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(-42),
            })
        );
        let bad = heaps.current().intern("12x");
        assert_eq!(
            parse_int_fn(&mut heaps, &mut out, &[Value::Ref(RefId::new(
                FIBER_ROOT, bad
            ))]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_PARSE_ERR,
                payload: Value::I64(2),
            }),
            "the failure payload is the position the reader gave up at"
        );
    }

    #[test]
    fn list_natives_construct_read_and_join() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        // the empty construction
        let v = list_new_fn(&mut heaps, &mut out, &[])
            .expect("new runs")
            .word()
            .expect("a single word");
        let Value::Ref(empty) = v else {
            panic!("a list ref, got {v:?}");
        };
        assert_eq!(heaps.list_at(empty), Ok((&[][..], 0)));
        // reads over any element kinds, both span members
        let list = heaps
            .current()
            .alloc_list(vec![Value::I64(7), Value::Bool(true)]);
        assert_eq!(
            list_len_fn(&mut heaps, &mut out, &[Value::Ref(RefId::new(
                FIBER_ROOT, list
            ))]),
            Ok(NativeResult::Word(Value::I64(2)))
        );
        assert_eq!(
            list_get_fn(&mut heaps, &mut out, &[
                Value::Ref(RefId::new(FIBER_ROOT, list)),
                Value::I64(1)
            ]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::Bool(true),
            })
        );
        for index in [2_i64, -1] {
            assert_eq!(
                list_get_fn(&mut heaps, &mut out, &[
                    Value::Ref(RefId::new(FIBER_ROOT, list)),
                    Value::I64(index)
                ]),
                Ok(NativeResult::Tag {
                    tag: PLATFORM_NONE,
                    payload: Value::I64(index),
                }),
                "out of range by {index} answers the none member"
            );
        }
        // the join: a fresh object over both element runs
        let v = list_concat_fn(&mut heaps, &mut out, &[
            Value::Ref(empty),
            Value::Ref(RefId::new(FIBER_ROOT, list)),
        ])
        .expect("concat runs")
        .word()
        .expect("a single word");
        let Value::Ref(joined) = v else {
            panic!("a list ref, got {v:?}");
        };
        assert_ne!(joined.id(), list, "the join is a fresh object");
        assert_eq!(
            heaps.list_at(joined),
            Ok((&[Value::I64(7), Value::Bool(true)][..], 1))
        );
        // a non-list argument names no object
        let str_obj = heaps.current().intern("x");
        assert_eq!(
            list_len_fn(&mut heaps, &mut out, &[Value::Ref(RefId::new(
                FIBER_ROOT, str_obj
            ))]),
            Err(Trap::UnknownObject { obj: str_obj })
        );
        // the construction append: every element word kind pushes in
        // place, the answer is the inert unit word
        for value in [
            Value::I64(9),
            Value::Bool(false),
            Value::Ref(RefId::new(FIBER_ROOT, str_obj)),
        ] {
            assert_eq!(
                list_push_fn(&mut heaps, &mut out, &[Value::Ref(empty), value]),
                Ok(NativeResult::Word(Value::I64(0)))
            );
        }
        assert_eq!(
            heaps.list_at(empty),
            Ok((
                &[
                    Value::I64(9),
                    Value::Bool(false),
                    Value::Ref(RefId::new(FIBER_ROOT, str_obj))
                ][..],
                1
            )),
            "the pushes appended in order"
        );
        assert_eq!(
            list_push_fn(&mut heaps, &mut out, &[
                Value::Ref(RefId::new(FIBER_ROOT, str_obj)),
                Value::I64(0)
            ]),
            Err(Trap::UnknownObject { obj: str_obj }),
            "pushing onto a string object traps"
        );
    }

    /// The copy natives hand each copied `Ref` element's count to the
    /// fresh list's edge inside their copy loops, so the ledger holds
    /// the frame's own words only (the caller's head, the lists
    /// themselves) and the deaths drain through the edges.
    #[test]
    fn list_copies_hand_element_counts_to_the_edge() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut out = OutputSink::captured();
        let (a, b, head, src, consed, tailed, collected) = {
            let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
            let str_word = |heaps: &mut Heaps, text: &str| {
                let Value::Ref(word) = heaps.new_str(text.to_owned()) else {
                    panic!("a fresh string is its reference word");
                };
                word
            };
            // the producer-native construction shape: fresh elements whose
            // birth counts the adopting list's edge owns
            let a = str_word(&mut heaps, "a");
            let b = str_word(&mut heaps, "b");
            heaps.release_claim(a);
            heaps.release_claim(b);
            let Value::Ref(src) = heaps.new_list(vec![Value::Ref(a), Value::Ref(b)]) else {
                panic!("a fresh list is its reference word");
            };
            // the caller's own head word keeps its frame claim
            let head = str_word(&mut heaps, "h");
            let word_of = |v: Result<NativeResult, Trap>| match v.expect("the copy runs").word() {
                Some(Value::Ref(word)) => word,
                other => panic!("a list ref, got {other:?}"),
            };
            let consed = word_of(cons_fn(&mut heaps, &mut out, &[
                Value::Ref(head),
                Value::Ref(src),
            ]));
            let tailed = word_of(tail_fn(&mut heaps, &mut out, &[Value::Ref(consed)]));
            let collected = word_of(collect_fn(&mut heaps, &mut out, &[Value::Ref(src)]));
            (a, b, head, src, consed, tailed, collected)
        };
        // one edge count per copied element word (src, consed, tailed,
        // collected own the four counts of a and b) and the caller's own
        // head count rides beside consed's edge count
        assert_eq!(objects.rc_at(a.id()), Ok(4));
        assert_eq!(objects.rc_at(b.id()), Ok(4));
        assert_eq!(objects.rc_at(head.id()), Ok(2));
        // the ledger answers the frame's words only: no element claim
        let key = |(word, n): &(RefId, u32)| (word.fiber().index(), word.id(), *n);
        let mut drained = claims[0].take_all_except(&[]);
        drained.sort_unstable_by_key(key);
        let mut want = vec![
            (src, 1),
            (head, 1),
            (consed, 1),
            (tailed, 1),
            (collected, 1),
        ];
        want.sort_unstable_by_key(key);
        assert_eq!(drained, want, "the frames own the lists and the head only");
        // the deaths drain the whole graph through the edges
        for word in [head, src, consed, tailed, collected] {
            objects
                .dec(word.id())
                .expect("each frame word drops its count");
        }
        assert!(objects.is_empty(), "the edge-owned elements died inside");
    }

    /// The span channel over a stride-carrying list: pushes of a
    /// multi-word element store the whole span at the stride the first
    /// push fixed, `list_len` divides by it, the checked read answers
    /// `NativeResult::Span` with one count handed out per `Ref` word,
    /// and a disagreeing later push traps the stride contract.
    #[test]
    fn multi_word_elements_round_trip_through_the_span_channel() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let (word, left, stored, len, hit, miss, stride_trap) = {
            let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
            let mut out = OutputSink::captured();
            let Value::Ref(list) = list_new_fn(&mut heaps, &mut out, &[])
                .expect("new runs")
                .word()
                .expect("a single word")
            else {
                panic!("a list ref");
            };
            let word = RefId::new(FIBER_ROOT, list.id());
            let left = {
                let Value::Ref(word) = heaps.new_str("a".to_owned()) else {
                    panic!("a fresh string is its reference word");
                };
                word
            };
            // two two-word elements: (string Ref, int) pairs
            for rank in [Value::I64(1), Value::I64(2)] {
                assert_eq!(
                    list_push_fn(&mut heaps, &mut out, &[
                        Value::Ref(word),
                        Value::Ref(left),
                        rank
                    ]),
                    Ok(NativeResult::Word(Value::I64(0)))
                );
            }
            let stored = heaps
                .list_at(word)
                .map(|(elems, stride)| (elems.to_vec(), stride));
            let len = list_len_fn(&mut heaps, &mut out, &[Value::Ref(word)]);
            // the hit: the span channel answers the element's whole span,
            // one count per Ref word for the reader
            let hit = list_get_fn(&mut heaps, &mut out, &[Value::Ref(word), Value::I64(1)]);
            // out of range answers the none member with the index
            let miss = list_get_fn(&mut heaps, &mut out, &[Value::Ref(word), Value::I64(2)]);
            // a disagreeing push width traps the stride contract
            let stride_trap =
                list_push_fn(&mut heaps, &mut out, &[Value::Ref(word), Value::I64(9)]);
            (word, left, stored, len, hit, miss, stride_trap)
        };
        assert_eq!(
            stored,
            Ok((
                vec![
                    Value::Ref(left),
                    Value::I64(1),
                    Value::Ref(left),
                    Value::I64(2),
                ],
                2
            )),
            "the pushes stored whole element spans at stride 2"
        );
        assert_eq!(len, Ok(NativeResult::Word(Value::I64(2))));
        assert_eq!(
            hit,
            Ok(NativeResult::Span {
                tag: PLATFORM_IO_OK,
                payload: vec![Value::Ref(left), Value::I64(2)],
            })
        );
        // the element Ref's count: its birth, one edge count per stored
        // span (two pushes), and the read's handed-out count — four
        assert_eq!(
            objects.rc_at(left.id()),
            Ok(4),
            "one count per Ref word handed out"
        );
        assert_eq!(
            miss,
            Ok(NativeResult::Tag {
                tag: PLATFORM_NONE,
                payload: Value::I64(2),
            })
        );
        assert_eq!(
            stride_trap,
            Err(Trap::ElementStride {
                obj: word.id(),
                stride: 2,
                given: 1,
            })
        );
    }
}
