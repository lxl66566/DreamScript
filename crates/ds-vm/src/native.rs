//! Native (Rust-implemented) functions: the tier layering at the platform
//! seam — the VM core's `builtin` module (this file) and the
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
//! The layering: the pure, irreducible
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
//! They answer with a uniform two-word tag+payload span on the
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
//! discriminant of its error kind, with the message `Ref` in the
//! span's payload word.
//!
//! The net ops (`crate::net`) answer the same convention over the
//! failure taxonomy's error classes: success reuses `PLATFORM_IO_OK`,
//! and a failure carries the class discriminant of the ambient
//! `net_err` case it answers by error class —
//! [`PLATFORM_NET_TIMEOUT`] or [`PLATFORM_NET_REFUSED`] — with the
//! message `Ref` in the span's payload word.
//!
//! # The fiber control natives (indices 24-26)
//!
//! The fiber substrate's debug switch, appended past the list block:
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
//! debug face: the caller parks, the dispatcher drives the named
//! nursery's children to death (first failure cancels the rest),
//! and the call answers the failed-children count. The full per-child
//! outcomes are the host API's ([`crate::Vm::join_nursery`]); an
//! in-language surface over them lands with the checker/lowerer rounds.

use cell::{box_get_fn, box_new_fn, box_set_fn};
/// The reserved native modules, the row indices, and the discriminants: the one vocabulary the
/// lowering and this VM read.
pub use ds_abi::native::BUILTIN_MODULE;
pub use ds_abi::native::{
    BUILTIN_ABS, BUILTIN_ARENA_AT, BUILTIN_ARENA_INSERT, BUILTIN_ARENA_NEW, BUILTIN_ARENA_REMOVE,
    BUILTIN_ATAN2, BUILTIN_BOX_GET, BUILTIN_BOX_NEW, BUILTIN_BOX_SET, BUILTIN_BYTE_AT,
    BUILTIN_BYTE_COMPARE, BUILTIN_BYTE_LIST, BUILTIN_BYTE_OF, BUILTIN_BYTES_COMPARE,
    BUILTIN_BYTES_EQ, BUILTIN_BYTES_HASH, BUILTIN_BYTES_OF, BUILTIN_CEIL, BUILTIN_CHAN_NEW,
    BUILTIN_CHAN_RECV, BUILTIN_CHAN_RECV_DEADLINE, BUILTIN_CHAN_SEND, BUILTIN_CHAN_SEND_DEADLINE,
    BUILTIN_CHAR_AT, BUILTIN_CHAR_COMPARE, BUILTIN_CHAR_HASH, BUILTIN_CHAR_OF, BUILTIN_CODE_OF,
    BUILTIN_COLLECT, BUILTIN_CONCAT, BUILTIN_CONS, BUILTIN_CONTAINS, BUILTIN_COS, BUILTIN_EXP,
    BUILTIN_FIBER_RESUME, BUILTIN_FIBER_YIELD, BUILTIN_FILTER, BUILTIN_FIRST, BUILTIN_FLOAT_HASH,
    BUILTIN_FLOOR, BUILTIN_HEAD, BUILTIN_INDEX_OF, BUILTIN_INT_OF_BYTE, BUILTIN_INT_OF_U32,
    BUILTIN_JOIN, BUILTIN_LEN, BUILTIN_LIST_CLEAR, BUILTIN_LIST_CONCAT, BUILTIN_LIST_CONTAINS,
    BUILTIN_LIST_EQ, BUILTIN_LIST_GET, BUILTIN_LIST_INSERT, BUILTIN_LIST_LEN, BUILTIN_LIST_NEW,
    BUILTIN_LIST_PUSH, BUILTIN_LIST_REMOVE, BUILTIN_LIST_SET, BUILTIN_LN, BUILTIN_MAP,
    BUILTIN_MAP_CONTAINS, BUILTIN_MAP_ENTRIES, BUILTIN_MAP_EQ, BUILTIN_MAP_GET, BUILTIN_MAP_KEYS,
    BUILTIN_MAP_LEN, BUILTIN_MAP_NEW, BUILTIN_MAP_REMOVE, BUILTIN_MAP_SET, BUILTIN_MAX,
    BUILTIN_MIN, BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_JOIN,
    BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN, BUILTIN_PAR_MAP, BUILTIN_PAR_REDUCE,
    BUILTIN_PARSE_FLOAT, BUILTIN_PARSE_INT, BUILTIN_PARTITION, BUILTIN_POW, BUILTIN_REDUCE,
    BUILTIN_REPL_PARK, BUILTIN_REPLACE, BUILTIN_ROUND, BUILTIN_SIN, BUILTIN_SORT, BUILTIN_SORT_BY,
    BUILTIN_SPLIT, BUILTIN_SQRT, BUILTIN_STR_BOOL, BUILTIN_STR_CHAR, BUILTIN_STR_COMPARE,
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
use ds_abi::{NativeModule, SlotType};
use fork::{chan_refuse_fn, fiber_control_bounce_fn};
#[cfg(test)]
use generic::GENERIC_BODIES;
#[cfg(test)]
use io::io_class;
use list::{
    arena_at_fn, arena_insert_fn, arena_new_fn, arena_remove_fn, list_clear_fn, list_contains_fn,
    list_insert_fn, list_new_fn, list_push_fn, list_remove_fn, list_set_fn,
};
#[cfg(test)]
use list::{collect_fn, cons_fn, list_concat_fn, list_get_fn, list_len_fn, tail_fn};
use map::{
    list_eq_fn, map_contains_fn, map_entries_fn, map_eq_fn, map_new_fn, map_remove_fn, map_set_fn,
};
#[cfg(test)]
use scalar::{ScalarText, abs_fn};
use scalar::{
    atan2_fn, byte_compare_fn, byte_of_fn, ceil_fn, char_compare_fn, char_hash_fn, cos_fn, exp_fn,
    float_hash_fn, floor_fn, int_of_byte_fn, int_of_u32_fn, log_fn, pow_fn, round_fn, sin_fn,
    sqrt_fn, str_bool_fn, str_char_fn, str_compare_fn, str_fixed_fn, str_float_fn, str_hash_fn,
    str_int_fn, tan_fn, total_compare_fn, trunc_fn, u32_of_fn,
};
use sink::trap_fn;
use text::{
    byte_at_fn, byte_list_fn, bytes_compare_fn, bytes_eq_fn, bytes_hash_fn, bytes_of_fn,
    char_at_fn, char_of_fn, code_of_fn, concat_fn, contains_fn, index_of_fn, join_fn,
    parse_float_fn, parse_int_fn, replace_fn, split_fn, substring_fn, to_lower_fn, to_upper_fn,
    trim_fn, utf8_decode_fn, utf8_encode_fn,
};
#[cfg(test)]
use text::{float_stop, int_stop};

#[cfg(test)]
use crate::value::RefId;
use crate::{nursery::NurseryId, object::Heaps, value::Value, vm::Trap};

mod cell;
mod fork;
mod generic;
mod io;
mod list;
mod map;
mod scalar;
mod sink;
mod text;

pub use fork::{ForkCallable, ForkStage, is_fork_join};
pub(crate) use fork::{fork_abandon, fork_spawn, release_owned_span};
pub use generic::{GenericFn, GenericFunction, resolve_generic};
pub(crate) use io::{
    file_exists_fn, file_size_fn, fs_list_fn, io_err, mono_fn, read_file_fn, sleep_fn, time_fn,
    write_file_fn,
};
pub use io::{is_io_class, is_net_class};
pub use sink::OutputSink;
pub(crate) use sink::{
    eprint_fn, eprintln_fn, i64_arg, print_fn, println_fn, read_line_fn, ref_arg,
};

/// The control natives of the fiber substrate: the dispatcher
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

/// The channel natives: like the
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
    /// A world op's park request: the
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
    /// `CALL` sites; `None` for every ordinary native.
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
reserved!(builtin_reserved_115 in BUILTIN_MODULE at 115); // sort_by; the reentrant generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_116 in BUILTIN_MODULE at 116); // min; the reentrant generic body lives in GENERIC_FUNCTIONS

/// One reserved gap entry: a call to the historical index traps exactly
/// like an uninstalled one (`UnknownCallee` naming the module and index).
/// The lowering never emits these indices; each module's table keeps them
/// so the historical index run stays positionally stable. No parameters, a one-word result
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
    debug_assert_eq!(fns.len(), BUILTIN_MIN as usize + 1);
    fns
}

/// One body per builtin table index, in index order: a gap carries its refusal stub.
#[rustfmt::skip]
const BUILTIN_BODIES: [NativeFn; BUILTIN_MIN as usize + 1] = [
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
    chan_refuse_fn, chan_refuse_fn, list_contains_fn, str_char_fn, box_new_fn,
    box_get_fn, box_set_fn, builtin_reserved_115, builtin_reserved_116,
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
        assert_eq!(fns.len(), BUILTIN_MIN as usize + 1);
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
    /// floats hash equal: a payloaded qNaN, a negative NaN, and the
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

    /// `char_hash` folds the code point's UTF-8 bytes, so a char and its
    /// one-char string answer identically; the char word itself is the
    /// code point integer.
    #[test]
    fn char_hash_answers_str_hash_over_the_same_bytes() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        for (text, code) in [("a", i64::from(u32::from('a'))), ("\u{6f22}", 0x6f22)] {
            let char_word = Value::I64(code);
            let Value::I64(as_char) = char_hash_fn(&mut heaps, &mut out, &[char_word])
                .expect("char hashes")
                .word()
                .expect("a single word")
            else {
                panic!("an int word")
            };
            let string_word = heaps.new_intern(text);
            let Value::I64(as_str) = str_hash_fn(&mut heaps, &mut out, &[string_word])
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
                Value::Tag(0), // the separator's union tag word
                Value::Ref(RefId::new(FIBER_ROOT, sep_id)),
                Value::I64(0), // the char member's dead region word
                Value::I64(9), // the staged char tag constant (the string member staged)
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

    /// The char separator's staged span: the tag word matches the staged
    /// char tag constant, so the code point reads through the char
    /// member's region word and the multi-byte scalar splits like the
    /// spelled text would.
    #[test]
    fn a_char_separator_splits_through_its_region_word() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims: Vec<ClaimTable> = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let text_id = heaps.current().intern("a\u{2014}b\u{2014}");
        let code = i64::from(u32::from('\u{2014}'));
        let Value::Ref(list) = split_fn(&mut heaps, &mut out, &[
            Value::Ref(RefId::new(FIBER_ROOT, text_id)),
            Value::Tag(9), // the separator's union tag word: the char member staged
            Value::Ref(RefId::new(FIBER_ROOT, 0)), // the string member's dead word
            Value::I64(code),
            Value::I64(9), // the staged char tag constant
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
        assert_eq!(parts, ["a", "b", ""]);
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
            Value::Tag(0), // the separator's union tag word
            Value::Ref(RefId::new(FIBER_ROOT, sep_id)),
            Value::I64(0), // the char member's dead region word
            Value::I64(9), // the staged char tag constant (the string member staged)
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
            let Value::Ref(joined) = join_fn(&mut heaps, &mut out, &[
                Value::Ref(list),
                Value::Tag(0), // the tag rides index 1
                sep_word,
                Value::I64(0), // the char member's dead region word
                Value::I64(9), // the staged char tag constant (the string member staged)
            ])
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
        // place over the uniquely-owned list, each answer the receiver's
        // own word under one count the call site's re-store takes over
        // (the raw test releases it here)
        for value in [
            Value::I64(9),
            Value::Bool(false),
            Value::Ref(RefId::new(FIBER_ROOT, str_obj)),
        ] {
            assert_eq!(
                list_push_fn(&mut heaps, &mut out, &[Value::Ref(empty), value]),
                Ok(NativeResult::Word(Value::Ref(empty)))
            );
            heaps.dec(empty).expect("the answer's claim releases");
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
            // two two-word elements: (string Ref, int) pairs, each
            // answer's claim released the way the call site's re-store
            // takes it over
            for rank in [Value::I64(1), Value::I64(2)] {
                assert_eq!(
                    list_push_fn(&mut heaps, &mut out, &[
                        Value::Ref(word),
                        Value::Ref(left),
                        rank
                    ]),
                    Ok(NativeResult::Word(Value::Ref(word)))
                );
                heaps.dec(word).expect("the answer's claim releases");
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
