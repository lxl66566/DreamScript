//! Native (Rust-implemented) functions: the two native modules of the
//! H5.5 layering — the VM core's `builtin` module and the platform's
//! `std` module.
//!
//! Builtins are functions in the source (`println(...)`, `str(x)`), so the
//! tier-1 mechanism routes them as functions, not uops: a VM installs
//! reserved native modules into its registry, and a pool
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
//! # The two modules
//!
//! The natives split along the H5.5 layering seam: the pure, irreducible
//! primitives every VM carries ([`BUILTIN_MODULE`], installed by
//! [`crate::Vm`] itself), and the world surface — console, file, net —
//! which is a platform's to provide ([`STD_PLATFORM_MODULE`], installed
//! by the host; the `ds` CLI is the default provider). A library embedder
//! gets no world effects by default. The `@std` ds-source package layer
//! above both is future work.
//!
//! Function indices are contract: pool entries name them positionally, and
//! the split keeps every historical index in its module of record, so the
//! two tables are complementary slices of the one historical 0-24 run. A
//! gap (an index the other module owns, or a spare slot) keeps its table
//! position as a reserved entry whose body traps exactly like an
//! uninstalled index — the lowering never emits these.
//!
//! | index | `builtin` (core) | `std` (platform) |
//! |-------|------------------|------------------|
//! | 0     | reserved         | `println (msg: string) -> unit` |
//! | 1     | reserved         | `print (msg: string) -> unit` |
//! | 2     | `str_int (x: int) -> string` | reserved |
//! | 3     | `str_float (x: float) -> string` | reserved |
//! | 4     | `str_bool (x: bool) -> string` | reserved |
//! | 5     | reserved         | `read_file (path: string) -> string \| io_err` |
//! | 6     | reserved         | `write_file (path, contents: string) -> unit \| io_err` |
//! | 7     | reserved         | `file_exists (path: string) -> bool` |
//! | 8     | reserved         | `fs_list (dir: string) -> list[string] \| io_err` |
//! | 9     | reserved         | `file_size (path: string) -> int \| io_err` |
//! | 10    | `len (s: string) -> int` | reserved |
//! | 11    | `substring (s, start, end: int) -> string` | reserved |
//! | 12    | `concat (a, b: string) -> string` | reserved |
//! | 13    | `to_upper (s: string) -> string` | reserved |
//! | 14    | `to_lower (s: string) -> string` | reserved |
//! | 15    | `trim (s: string) -> string` | reserved |
//! | 16    | `parse_int (s: string) -> int \| parse_err` | reserved |
//! | 17    | `split (s, sep: string) -> list[string]` | reserved |
//! | 18    | `char_at (s: string, i: int) -> string` | reserved |
//! | 19    | reserved (the string block's spare slot) | reserved |
//! | 20    | `list_new () -> list` | `net_connect (host: string, port: int) -> int \| net_err` |
//! | 21    | `list_len (list) -> int` | `net_send (conn: int, data: string) -> unit \| net_err` |
//! | 22    | `list_get (list, i: int) -> t \| none` | `net_recv (conn, max_bytes: int) -> string \| net_err` |
//! | 23    | `list_concat (a, b: list) -> list` | `net_close (conn: int) -> unit \| net_err` |
//! | 24    | `fiber_yield (value: int) -> int` | `http_get (url: string) -> string \| net_err` |
//! | 25    | `fiber_resume (fiber, value: int) -> int` | reserved |
//! | 26    | `nursery_join (nursery: int) -> int` | reserved |
//! | 27    | `list_push (xs: list, x: value) -> unit` | reserved |
//!
//! The surface-level `str()` is one function in source; the core table
//! carries its typed instances, and lowering emits the `str_*` FuncRef for
//! the operand's type [B7's explicit conversions].
//!
//! Unit natives return the inert unit word `Value::I64(0)`: the caller of a
//! unit callee never reads the overlay word (a lowering discipline), and a
//! tail call to a unit native still needs a return word to overlay.
//!
//! # Result spans and the platform tag ids
//!
//! The fallible platform natives (all of the file and net surface but
//! `file_exists`) touch the world: that is a static fact (the ambient's
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
//! The discriminants are platform tag ids, and both native modules share
//! the one reservation (native results are cross-module). Tag ids are
//! per-module otherwise: lowering assigns each module's union members ids
//! counting up from 0, while the platform reservation holds the top of
//! the `u8` tag-id space, counting down from [`PLATFORM_IO_OK`]; the two
//! allocations meet only once a module needs 254 ids, at which point the
//! tag id widens. `PLATFORM_IO_OK` marks every fallible native's success
//! (the caller already knows which native it called, so one shared id
//! disambiguates nothing); [`PLATFORM_IO_ERR`] marks IO failure, with the
//! message `Ref` in the span's payload word.
//!
//! The net natives (indices 20-24, `crate::net`) extend the same
//! convention: success reuses `PLATFORM_IO_OK`, and failure carries the
//! platform tag [`PLATFORM_NET_ERR`] — the file-owned `net_err` type
//! needs its own cross-module tag, reserved one step below `io_err` in
//! the countdown.
//!
//! # The string natives (indices 10-18)
//!
//! The pure string surface, char-indexed: `len` counts unicode scalar
//! values, and `substring`/`char_at` index chars, never bytes, so
//! multi-byte text never cuts a code point. Out-of-range access stays
//! total: `substring` clamps both bounds to the string and answers the
//! empty string when the clamped range is empty or inverted, and
//! `char_at` answers the empty string past either end.
//!
//! `concat` is the callable face of the `STRCAT` uop: lowering emits
//! STRCAT at compiled call sites (one dispatch, no native call), and the
//! native exists so a pool `FuncRef` naming `concat` behaves like any
//! other builtin. `split` answers a list-object `Ref` (the same list kind
//! `fs_list` fills): the surface's `char` separator reaches runtime as a
//! single-char string `Ref`, and an empty separator splits per character.
//!
//! `parse_int` is the pure fallible one: its ambient row is `error` (the
//! caller-handled failure, standing for the record's `int | parse_err`
//! exactly as `read_file`'s row stands for `string | io_err`), and it
//! answers the same uniform two-word span — [`PLATFORM_IO_OK`] with the
//! parsed `int`, or [`PLATFORM_PARSE_ERR`] (the next id below `net_err`
//! in the platform reservation's countdown) with the input text `Ref` in
//! the payload word.
//!
//! # The list natives (indices 20-23)
//!
//! A list is one RC'd heap object — len, capacity, and the element value
//! words inline in a single object-table entry (the header's
//! `{type_id, rc}` words are M4's precise-RC slot over the same table;
//! see [`crate::object::Objects`]). Elements are plain value words, so
//! the one kind serves every element type. Construction (`list_new`,
//! `list_concat`, and the producers `split`/`fs_list`) and reads
//! (`list_len`, `list_get`) are pure natives. The one in-place mutation
//! is `list_push` (index 27): the construction append list literals lower
//! to, over a freshly built list its frame uniquely owns; every other
//! mutation stays reserved for the compiler-disciplined exclusive
//! windows, and growth is the backing buffer's amortized doubling
//! (`list_concat` reserves the joined length up front).
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
//! # The fiber control natives (indices 24-26)
//!
//! The fiber substrate's debug pair [E2], appended past the list block:
//! `fiber_yield` parks the running fiber and hands control to its resumer
//! with a one-word payload; `fiber_resume` parks the caller and enters the
//! named pool fiber with the payload. They are natives so fixtures address
//! them with ordinary pool FuncRefs through `CALL` — but the dispatcher,
//! not the body, performs the switch (the body only answers the bare
//! [`PLATFORM_NONE`] bounce tag, which is also what a `CALLT` to them
//! yields: no switch exists in tail position, and what an unreachable or
//! dead `fiber_resume` target answers on the caller's overlay). The host
//! face of the same machinery is `Vm::spawn_fiber`/`Vm::resume_fiber`.
//!
//! `nursery_join` (26) extends the pair with the nursery boundary's debug
//! face [E2]: the caller parks, the dispatcher drives the named nursery's
//! children to death (first failure cancels the rest [E5]), and the call
//! answers the failed-children count. The full per-child outcomes are the
//! host API's ([`crate::Vm::join_nursery`]); an in-language surface over
//! them lands with the checker/lowerer rounds.

use std::{io::Write, path::Path};

use ds_abi::SlotType;

use crate::{object::Objects, value::Value, vm::Trap};

/// The reserved core native module every VM pre-installs: the pure,
/// irreducible primitives (the `str_*` conversions and the string ops).
pub const BUILTIN_MODULE: &str = "builtin";

/// The std platform native module: the world surface (console, file IO,
/// net IO). The host that provides a platform installs it explicitly —
/// [`crate::Vm`] never does — so a library embedder gets no world
/// effects by default.
pub const STD_PLATFORM_MODULE: &str = "std";

/// `str_int(x: int) -> string`.
pub const BUILTIN_STR_INT: u32 = 2;
/// `str_float(x: float) -> string`.
pub const BUILTIN_STR_FLOAT: u32 = 3;
/// `str_bool(x: bool) -> string`.
pub const BUILTIN_STR_BOOL: u32 = 4;
/// `len(s: string) -> int` (the string instance of the ambient's generic `len`).
pub const BUILTIN_LEN: u32 = 10;
/// `substring(s: string, start, end: int) -> string` (char-indexed, clamped).
pub const BUILTIN_SUBSTRING: u32 = 11;
/// `concat(a, b: string) -> string` (the callable face of the STRCAT uop).
pub const BUILTIN_CONCAT: u32 = 12;
/// `to_upper(s: string) -> string`.
pub const BUILTIN_TO_UPPER: u32 = 13;
/// `to_lower(s: string) -> string`.
pub const BUILTIN_TO_LOWER: u32 = 14;
/// `trim(s: string) -> string` (leading and trailing whitespace dropped).
pub const BUILTIN_TRIM: u32 = 15;
/// `parse_int(s: string) -> int | parse_err` (the input rides the error payload).
pub const BUILTIN_PARSE_INT: u32 = 16;
/// `split(s, sep: string) -> list[string]` (a list object of part refs).
pub const BUILTIN_SPLIT: u32 = 17;
/// `char_at(s: string, i: int) -> string` (empty when out of range).
pub const BUILTIN_CHAR_AT: u32 = 18;
/// `list_new() -> list` (the empty list object).
pub const BUILTIN_LIST_NEW: u32 = 20;
/// `list_len(list) -> int`.
pub const BUILTIN_LIST_LEN: u32 = 21;
/// `list_get(list, i: int) -> t | none` (the checked read; out of range
/// answers the `none` span member, never a trap).
pub const BUILTIN_LIST_GET: u32 = 22;
/// `list_concat(a, b: list) -> list` (a fresh list over both element runs).
pub const BUILTIN_LIST_CONCAT: u32 = 23;
/// `fiber_yield(value: int) -> int` — the fiber substrate's debug yield
/// [E2]: parks the current fiber, control returns to its resumer with
/// `value`, and the call answers the payload of whichever switch re-enters
/// the fiber. Appended at the end of the table, past the list block.
pub const BUILTIN_FIBER_YIELD: u32 = 24;
/// `fiber_resume(fiber: int, value: int) -> int` — the substrate's debug
/// switch [E2]: parks the current fiber and enters `fiber` with `value`
/// (a never-started fiber drops the payload); the call answers the value
/// the entered fiber next yields or completes with. An absent, dead, or
/// self target answers the bare `PLATFORM_NONE` tag (no switch), the
/// checked-read convention of `list_get` at one word.
pub const BUILTIN_FIBER_RESUME: u32 = 25;
/// `nursery_join(nursery: int) -> int` — the nursery boundary's debug face
/// [E2]: parks the caller, drives the named nursery's children to death in
/// spawn order (first failure raises the cancel flag for the rest [E5]),
/// and answers the failed-children count (0 = all joined). Per-child
/// outcomes union out through [`crate::Vm::join_nursery`]. An absent or
/// consumed nursery answers the bare [`PLATFORM_NONE`] tag (the
/// checked-read convention); a child joining its own nursery traps — the
/// single-threaded driver would deadlock on itself.
pub const BUILTIN_NURSERY_JOIN: u32 = 26;

/// `println(msg: string) -> unit`.
/// `list_push(xs: list, x: value) -> unit` — the construction append
/// list literals lower to: one element word onto a list the constructing
/// frame uniquely owns. The element parameter is `Raw`, the one native
/// whose argument kind varies by call site, so it accepts every kind. A
/// non-list first argument traps; the unit result is the inert unit word.
pub const BUILTIN_LIST_PUSH: u32 = 27;
pub const STD_PRINTLN: u32 = 0;
/// `print(msg: string) -> unit`.
pub const STD_PRINT: u32 = 1;
/// `read_file(path: string) -> string | io_err`.
pub const STD_READ_FILE: u32 = 5;
/// `write_file(path, contents: string) -> unit | io_err` (create-or-truncate).
pub const STD_WRITE_FILE: u32 = 6;
/// `file_exists(path: string) -> bool` (unreachable paths answer false).
pub const STD_FILE_EXISTS: u32 = 7;
/// `fs_list(dir: string) -> list[string] | io_err` (names, sorted).
pub const STD_FS_LIST: u32 = 8;
/// `file_size(path: string) -> int | io_err` (bytes).
pub const STD_FILE_SIZE: u32 = 9;
/// `net_connect(host: string, port: int) -> int | net_err`.
pub const STD_NET_CONNECT: u32 = 20;
/// `net_send(conn: int, data: string) -> unit | net_err`.
pub const STD_NET_SEND: u32 = 21;
/// `net_recv(conn: int, max_bytes: int) -> string | net_err`.
pub const STD_NET_RECV: u32 = 22;
/// `net_close(conn: int) -> unit | net_err`.
pub const STD_NET_CLOSE: u32 = 23;
/// `http_get(url: string) -> string | net_err` (HTTP/1.0, no TLS).
pub const STD_HTTP_GET: u32 = 24;

/// The success discriminant of every fallible native's result span: the
/// top of the platform tag-id reservation both native modules share (see
/// the module docs).
pub const PLATFORM_IO_OK: u8 = u8::MAX;
/// The failure discriminant: the `io_err` tag, payload word holding the
/// OS-error message `Ref`.
pub const PLATFORM_IO_ERR: u8 = u8::MAX - 1;
/// The net natives' failure discriminant: the `net_err` tag, payload word
/// holding the message `Ref` — one step below `io_err` in the platform
/// reservation's countdown.
pub const PLATFORM_NET_ERR: u8 = u8::MAX - 2;
/// The `parse_int` failure discriminant: the `parse_err` tag, the next id
/// below `net_err` in the platform reservation's countdown; the payload
/// word holds the input text `Ref`.
pub const PLATFORM_PARSE_ERR: u8 = u8::MAX - 3;
/// The `none` discriminant of a checked collection read (`list_get`): the
/// `t | none` member callers extract, the next id below `parse_err` in the
/// platform reservation's countdown; the payload word holds the
/// rejected index.
pub const PLATFORM_NONE: u8 = u8::MAX - 4;
/// The cancel sentinel of the nursery substrate [E5]: the death value of a
/// fiber stopped at a preemption point under its nursery's cancel flag,
/// delivered to its resumer and to a host driver — the next id below
/// `none` in the platform reservation's countdown.
pub const PLATFORM_CANCELLED: u8 = u8::MAX - 5;

/// The control natives of the fiber substrate [E2]: the dispatcher
/// intercepts them at `CALL` sites and performs the switch itself — the
/// body only ever runs from a `CALLT` site (tail position), where no
/// switch is possible, answering the bare `PLATFORM_NONE` tag as the
/// bounce result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlNative {
    /// `fiber_yield(value)`: park the current fiber; control returns to its
    /// resumer with `value`.
    Yield,
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
}

/// A native's result: one word on the caller's overlay slot 0, or a
/// fallible native's two-word tag+payload span on overlay slots 0 and 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NativeResult {
    /// A single value word: the total natives' only shape.
    Word(Value),
    /// The span: the discriminant word ([`PLATFORM_IO_OK`] or a failure
    /// tag) plus the payload word.
    Tag {
        /// The span's discriminant.
        tag: u8,
        /// The success value, or the error message `Ref`.
        payload: Value,
    },
}

impl NativeResult {
    /// The single-word shape, or `None` for a tag+payload span.
    #[must_use]
    pub fn word(self) -> Option<Value> {
        match self {
            Self::Word(v) => Some(v),
            Self::Tag { .. } => None,
        }
    }
}

/// The native calling convention: `args` are the staged word values
/// (already type-checked against `params` by the call site), the result
/// overlays the caller's argument slots like a RET (one word, or the
/// two-word span of a fallible native).
pub type NativeFn = fn(&mut Objects, &mut OutputSink, &[Value]) -> Result<NativeResult, Trap>;

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
    /// How many overlay words the result needs: 1 for a single word, 2 for
    /// a fallible native's tag+payload span. Pre-checked at the call site
    /// against the caller's staging width, so the native never runs only
    /// for its call to trap on the result width.
    pub result_words: u8,
    /// Marks the two fiber-control natives the dispatcher intercepts at
    /// `CALL` sites [E2]; `None` for every ordinary native.
    pub control: Option<ControlNative>,
    /// The body.
    pub func: NativeFn,
}

fn len_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    // char count, never bytes: multi-byte text measures its scalar values
    let n = i64::try_from(text.chars().count()).expect("string length fits i64");
    Ok(NativeResult::Word(Value::I64(n)))
}

fn substring_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    let n = i64::try_from(text.chars().count()).expect("string length fits i64");
    // clamped to [0, n], so the narrowed index is a valid char position
    let start = usize::try_from(i64_arg(args, 1)?.clamp(0, n)).expect("index fits usize");
    let end = usize::try_from(i64_arg(args, 2)?.clamp(0, n)).expect("index fits usize");
    let slice = if start >= end {
        ""
    } else {
        // both offsets sit on char boundaries, so the slice cannot cut a code point
        let begin = text
            .char_indices()
            .nth(start)
            .map_or(text.len(), |(o, _)| o);
        let stop = text.char_indices().nth(end).map_or(text.len(), |(o, _)| o);
        &text[begin..stop]
    };
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_str(slice.to_owned()),
    )))
}

fn concat_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let joined = objects.concat(ref_arg(args, 0)?, ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::Ref(joined)))
}

fn to_upper_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_str(text.to_uppercase()),
    )))
}

fn to_lower_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_str(text.to_lowercase()),
    )))
}

fn trim_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_str(text.trim().to_owned()),
    )))
}

fn parse_int_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let text = objects.str_at(id)?;
    Ok(match text.parse::<i64>() {
        // strict: no implicit whitespace handling (the `trim` native composes)
        Ok(v) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(v),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_PARSE_ERR,
            payload: Value::Ref(id),
        },
    })
}

fn split_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    let sep = objects.str_at(ref_arg(args, 1)?)?;
    // the surface separator is a `char` (one scalar value); an empty
    // separator falls back to the per-character split rather than trap
    let parts: Vec<String> = if sep.is_empty() {
        text.chars().map(String::from).collect()
    } else {
        text.split(sep).map(str::to_owned).collect()
    };
    let elems = parts
        .into_iter()
        .map(|part| Value::Ref(objects.alloc_str(part)))
        .collect::<Vec<_>>();
    Ok(NativeResult::Word(Value::Ref(objects.alloc_list(elems))))
}

fn char_at_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    let index = i64_arg(args, 1)?;
    // out of range by either side: the empty string, never a trap
    let part = if index < 0 {
        None
    } else {
        text.chars()
            .nth(usize::try_from(index).expect("checked index fits usize"))
    };
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_str(part.map_or_else(String::new, String::from)),
    )))
}

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
fn list_new_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_list(Vec::new()),
    )))
}

fn list_len_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let elems = objects.list_at(ref_arg(args, 0)?)?;
    let n = i64::try_from(elems.len()).expect("list length fits i64");
    Ok(NativeResult::Word(Value::I64(n)))
}

fn list_get_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let elems = objects.list_at(ref_arg(args, 0)?)?;
    let index = i64_arg(args, 1)?;
    // Bounds follow the crate's checked-API convention (the span shape
    // `parse_int` answers), not the string ops' clamping: a generic
    // element type has no benign total answer, and a trap would deny the
    // caller the `t | none` extraction. A negative index fails the
    // usize conversion, so both out-of-range sides meet in `None`.
    let hit = usize::try_from(index).ok().and_then(|at| elems.get(at));
    Ok(match hit {
        Some(&value) => {
            // the read hands the element word out: a Ref element gains a
            // count for the reader, the list keeps its own (C4)
            if let Value::Ref(id) = value {
                objects.inc(id)?;
            }
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: value,
            }
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(index),
        },
    })
}

fn list_concat_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let joined = objects.concat_lists(ref_arg(args, 0)?, ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::Ref(joined)))
}

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
fn list_push_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let list = ref_arg(args, 0)?;
    objects.push_list(list, args[1])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

/// The shared body of the control natives: a `CALL` site never reaches it
/// (the dispatcher intercepts and switches), so the only reachable call is
/// a `CALLT`, where no switch is possible — the bounce answer.
#[allow(clippy::unnecessary_wraps)]
fn fiber_control_bounce_fn(
    _objects: &mut Objects,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Tag(PLATFORM_NONE)))
}

/// One reserved gap entry: a call to the historical index traps exactly
/// like an uninstalled one (`UnknownCallee` naming the module and index).
/// The lowering never emits these indices; each module's table keeps them
/// so the historical index run stays positionally stable.
macro_rules! reserved {
    ($name:ident in $module:ident at $index:literal) => {
        fn $name(
            _objects: &mut Objects,
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
reserved!(builtin_reserved_19 in BUILTIN_MODULE at 19); // the spare slot
reserved!(std_reserved_2 in STD_PLATFORM_MODULE at 2); // was str_int
reserved!(std_reserved_3 in STD_PLATFORM_MODULE at 3); // was str_float
reserved!(std_reserved_4 in STD_PLATFORM_MODULE at 4); // was str_bool
reserved!(std_reserved_10 in STD_PLATFORM_MODULE at 10); // 10-18 were the
reserved!(std_reserved_11 in STD_PLATFORM_MODULE at 11); // string ops,
reserved!(std_reserved_12 in STD_PLATFORM_MODULE at 12); // 19 the block's
reserved!(std_reserved_13 in STD_PLATFORM_MODULE at 13); // spare slot
reserved!(std_reserved_14 in STD_PLATFORM_MODULE at 14);
reserved!(std_reserved_15 in STD_PLATFORM_MODULE at 15);
reserved!(std_reserved_16 in STD_PLATFORM_MODULE at 16);
reserved!(std_reserved_17 in STD_PLATFORM_MODULE at 17);
reserved!(std_reserved_18 in STD_PLATFORM_MODULE at 18);
reserved!(std_reserved_19 in STD_PLATFORM_MODULE at 19);

/// The one reserved gap entry shape: no parameters, a one-word result
/// contract (the trap fires before any overlay write matters).
fn reserved_entry(func: NativeFn) -> NativeFunction {
    NativeFunction {
        name: "reserved",
        params: &[],
        result_words: 1,
        control: None,
        func,
    }
}

/// The core native table every VM installs as [`BUILTIN_MODULE`]: the
/// pure, irreducible primitives — the typed `str_*` conversions (2-4),
/// the string ops (10-18), and the list constructors/reads (20-23) —
/// pure with no world effects. The historical world-surface indices
/// (0-1, 5-9) and the string block's spare slot (19) stay as reserved
/// trap entries; the list block appends past the historical run (indices
/// freeze at M7), the fiber substrate's control pair
/// ([`BUILTIN_FIBER_YIELD`], [`BUILTIN_FIBER_RESUME`]) appends past the
/// list block, and the nursery boundary's debug join
/// ([`BUILTIN_NURSERY_JOIN`]) appends past the pair.
#[must_use]
pub fn builtin_module() -> Vec<NativeFunction> {
    vec![
        reserved_entry(builtin_reserved_0),
        reserved_entry(builtin_reserved_1),
        NativeFunction {
            name: "str_int",
            params: &[SlotType::I64],
            result_words: 1,
            control: None,
            func: str_int_fn,
        },
        NativeFunction {
            name: "str_float",
            params: &[SlotType::F64],
            result_words: 1,
            control: None,
            func: str_float_fn,
        },
        NativeFunction {
            name: "str_bool",
            params: &[SlotType::Bool],
            result_words: 1,
            control: None,
            func: str_bool_fn,
        },
        reserved_entry(builtin_reserved_5),
        reserved_entry(builtin_reserved_6),
        reserved_entry(builtin_reserved_7),
        reserved_entry(builtin_reserved_8),
        reserved_entry(builtin_reserved_9),
        NativeFunction {
            name: "len",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: len_fn,
        },
        NativeFunction {
            name: "substring",
            params: &[SlotType::Ref, SlotType::I64, SlotType::I64],
            result_words: 1,
            control: None,
            func: substring_fn,
        },
        NativeFunction {
            name: "concat",
            params: &[SlotType::Ref, SlotType::Ref],
            result_words: 1,
            control: None,
            func: concat_fn,
        },
        NativeFunction {
            name: "to_upper",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: to_upper_fn,
        },
        NativeFunction {
            name: "to_lower",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: to_lower_fn,
        },
        NativeFunction {
            name: "trim",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: trim_fn,
        },
        NativeFunction {
            name: "parse_int",
            params: &[SlotType::Ref],
            result_words: 2,
            control: None,
            func: parse_int_fn,
        },
        NativeFunction {
            name: "split",
            params: &[SlotType::Ref, SlotType::Ref],
            result_words: 1,
            control: None,
            func: split_fn,
        },
        NativeFunction {
            name: "char_at",
            params: &[SlotType::Ref, SlotType::I64],
            result_words: 1,
            control: None,
            func: char_at_fn,
        },
        reserved_entry(builtin_reserved_19),
        NativeFunction {
            name: "list_new",
            params: &[],
            result_words: 1,
            control: None,
            func: list_new_fn,
        },
        NativeFunction {
            name: "list_len",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: list_len_fn,
        },
        NativeFunction {
            name: "list_get",
            params: &[SlotType::Ref, SlotType::I64],
            result_words: 2,
            control: None,
            func: list_get_fn,
        },
        NativeFunction {
            name: "list_concat",
            params: &[SlotType::Ref, SlotType::Ref],
            result_words: 1,
            control: None,
            func: list_concat_fn,
        },
        NativeFunction {
            name: "fiber_yield",
            params: &[SlotType::I64],
            result_words: 1,
            control: Some(ControlNative::Yield),
            func: fiber_control_bounce_fn,
        },
        NativeFunction {
            name: "fiber_resume",
            params: &[SlotType::I64, SlotType::I64],
            result_words: 1,
            control: Some(ControlNative::Resume),
            func: fiber_control_bounce_fn,
        },
        NativeFunction {
            name: "nursery_join",
            params: &[SlotType::I64],
            result_words: 1,
            control: Some(ControlNative::NurseryJoin),
            func: fiber_control_bounce_fn,
        },
        NativeFunction {
            name: "list_push",
            params: &[SlotType::Ref, SlotType::Raw],
            result_words: 1,
            control: None,
            func: list_push_fn,
        },
    ]
}

/// The std platform native table, the world surface a platform-providing
/// host installs as [`STD_PLATFORM_MODULE`]: the console pair (0-1), the
/// file-IO five (5-9), and the net-IO five (20-24). The historical core
/// indices (2-4, 10-19) stay as reserved trap entries.
#[must_use]
pub fn std_platform_module() -> Vec<NativeFunction> {
    vec![
        NativeFunction {
            name: "println",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: println_fn,
        },
        NativeFunction {
            name: "print",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: print_fn,
        },
        reserved_entry(std_reserved_2),
        reserved_entry(std_reserved_3),
        reserved_entry(std_reserved_4),
        NativeFunction {
            name: "read_file",
            params: &[SlotType::Ref],
            result_words: 2,
            control: None,
            func: read_file_fn,
        },
        NativeFunction {
            name: "write_file",
            params: &[SlotType::Ref, SlotType::Ref],
            result_words: 2,
            control: None,
            func: write_file_fn,
        },
        NativeFunction {
            name: "file_exists",
            params: &[SlotType::Ref],
            result_words: 1,
            control: None,
            func: file_exists_fn,
        },
        NativeFunction {
            name: "fs_list",
            params: &[SlotType::Ref],
            result_words: 2,
            control: None,
            func: fs_list_fn,
        },
        NativeFunction {
            name: "file_size",
            params: &[SlotType::Ref],
            result_words: 2,
            control: None,
            func: file_size_fn,
        },
        reserved_entry(std_reserved_10),
        reserved_entry(std_reserved_11),
        reserved_entry(std_reserved_12),
        reserved_entry(std_reserved_13),
        reserved_entry(std_reserved_14),
        reserved_entry(std_reserved_15),
        reserved_entry(std_reserved_16),
        reserved_entry(std_reserved_17),
        reserved_entry(std_reserved_18),
        reserved_entry(std_reserved_19),
        NativeFunction {
            name: "net_connect",
            params: &[SlotType::Ref, SlotType::I64],
            result_words: 2,
            control: None,
            func: crate::net::net_connect_fn,
        },
        NativeFunction {
            name: "net_send",
            params: &[SlotType::I64, SlotType::Ref],
            result_words: 2,
            control: None,
            func: crate::net::net_send_fn,
        },
        NativeFunction {
            name: "net_recv",
            params: &[SlotType::I64, SlotType::I64],
            result_words: 2,
            control: None,
            func: crate::net::net_recv_fn,
        },
        NativeFunction {
            name: "net_close",
            params: &[SlotType::I64],
            result_words: 2,
            control: None,
            func: crate::net::net_close_fn,
        },
        NativeFunction {
            name: "http_get",
            params: &[SlotType::Ref],
            result_words: 2,
            control: None,
            func: crate::net::http_get_fn,
        },
    ]
}

/// Where print/println write: process stdout by default, a captured buffer
/// for tests and embedders ([`crate::Vm::capture_output`]).
#[derive(Debug, Default, PartialEq)]
pub enum OutputSink {
    /// No capture requested yet; stdout. `Default` so `Vm::new` stays
    /// cheap and the switch to `Captured` is explicit.
    #[default]
    Stdout,
    /// The in-memory sink.
    Captured(Vec<u8>),
}

impl OutputSink {
    /// Writes `text` with no newline.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_str(&mut self, text: &str) -> Result<(), Trap> {
        match self {
            // Stdout is line-buffered, so println's trailing newline is
            // the flush point; print keeps its no-newline contract
            Self::Stdout => std::io::stdout()
                .write_all(text.as_bytes())
                .map_err(|e| Trap::OutputWrite { kind: e.kind() }),
            Self::Captured(buf) => {
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
}

fn ref_arg(args: &[Value], index: usize) -> Result<u32, Trap> {
    match args.get(index) {
        Some(Value::Ref(id)) => Ok(*id),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::Ref,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn i64_arg(args: &[Value], index: usize) -> Result<i64, Trap> {
    match args.get(index) {
        Some(Value::I64(v)) => Ok(*v),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::I64,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn println_fn(
    objects: &mut Objects,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    out.write_line(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

fn print_fn(
    objects: &mut Objects,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = objects.str_at(ref_arg(args, 0)?)?;
    out.write_str(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

fn str_int_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::I64(v)] => Ok(NativeResult::Word(Value::Ref(
            objects.alloc_str(v.to_string()),
        ))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn str_float_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(v)] => Ok(NativeResult::Word(Value::Ref(
            objects.alloc_str(v.to_string()),
        ))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

fn str_bool_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::Bool(v)] => Ok(NativeResult::Word(Value::Ref(objects.intern(if *v {
            "true"
        } else {
            "false"
        })))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::Bool,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// The `io_err` span: the OS error rendered as `"<op> <path>: <error>"`.
fn io_err(objects: &mut Objects, op: &str, path: &str, err: &std::io::Error) -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_IO_ERR,
        payload: Value::Ref(objects.alloc_str(format!("{op} {path}: {err}"))),
    }
}

fn read_file_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = objects.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::fs::read_to_string(&path) {
        Ok(text) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::Ref(objects.alloc_str(text)),
        },
        Err(err) => io_err(objects, "read_file", &path, &err),
    })
}

fn write_file_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = objects.str_at(ref_arg(args, 0)?)?.to_owned();
    let contents = objects.str_at(ref_arg(args, 1)?)?.to_owned();
    Ok(match std::fs::write(&path, contents.as_bytes()) {
        // create-or-truncate; the payload word is the inert unit word
        Ok(()) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(0),
        },
        Err(err) => io_err(objects, "write_file", &path, &err),
    })
}

fn file_exists_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // total by contract: an entry the process cannot stat answers false
    let path = objects.str_at(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::Bool(Path::new(path).exists())))
}

fn fs_list_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let dir = objects.str_at(ref_arg(args, 0)?)?.to_owned();
    // sorted, so listing order never depends on the platform's readdir
    let names = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|d| d.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<String>, _>>(),
        Err(err) => Err(err),
    };
    let mut names = match names {
        Ok(names) => names,
        Err(err) => return Ok(io_err(objects, "fs_list", &dir, &err)),
    };
    names.sort_unstable();
    let mut elems = Vec::with_capacity(names.len());
    for name in names {
        elems.push(Value::Ref(objects.alloc_str(name)));
    }
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: Value::Ref(objects.alloc_list(elems)),
    })
}

fn file_size_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = objects.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::fs::metadata(&path) {
        // no filesystem carries a length past i64::MAX
        Ok(meta) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::try_from(meta.len()).expect("file size fits i64")),
        },
        Err(err) => io_err(objects, "file_size", &path, &err),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_core_table_indices_are_contract() {
        let fns = builtin_module();
        assert_eq!(fns[BUILTIN_STR_INT as usize].name, "str_int");
        assert_eq!(fns[BUILTIN_STR_FLOAT as usize].name, "str_float");
        assert_eq!(fns[BUILTIN_STR_BOOL as usize].name, "str_bool");
        assert_eq!(fns[BUILTIN_LEN as usize].name, "len");
        assert_eq!(fns[BUILTIN_SUBSTRING as usize].name, "substring");
        assert_eq!(fns[BUILTIN_CONCAT as usize].name, "concat");
        assert_eq!(fns[BUILTIN_TO_UPPER as usize].name, "to_upper");
        assert_eq!(fns[BUILTIN_TO_LOWER as usize].name, "to_lower");
        assert_eq!(fns[BUILTIN_TRIM as usize].name, "trim");
        assert_eq!(fns[BUILTIN_PARSE_INT as usize].name, "parse_int");
        assert_eq!(fns[BUILTIN_SPLIT as usize].name, "split");
        assert_eq!(fns[BUILTIN_CHAR_AT as usize].name, "char_at");
        assert_eq!(fns[BUILTIN_LIST_NEW as usize].name, "list_new");
        assert_eq!(fns[BUILTIN_LIST_LEN as usize].name, "list_len");
        assert_eq!(fns[BUILTIN_LIST_GET as usize].name, "list_get");
        assert_eq!(fns[BUILTIN_LIST_CONCAT as usize].name, "list_concat");
        assert_eq!(fns[BUILTIN_FIBER_YIELD as usize].name, "fiber_yield");
        assert_eq!(fns[BUILTIN_FIBER_RESUME as usize].name, "fiber_resume");
        assert_eq!(fns[BUILTIN_NURSERY_JOIN as usize].name, "nursery_join");
        assert_eq!(fns[BUILTIN_LIST_PUSH as usize].name, "list_push");
        assert_eq!(fns.len(), 28);
        // every world index and the spare slot stays a reserved trap
        for index in [0u32, 1, 5, 6, 7, 8, 9, 19] {
            assert_eq!(fns[index as usize].name, "reserved", "index {index}");
        }
        // the fallible ones need a two-word overlay; the rest one word
        for (index, want) in [
            (BUILTIN_STR_INT, 1),
            (BUILTIN_STR_FLOAT, 1),
            (BUILTIN_STR_BOOL, 1),
            (BUILTIN_LEN, 1),
            (BUILTIN_SUBSTRING, 1),
            (BUILTIN_CONCAT, 1),
            (BUILTIN_TO_UPPER, 1),
            (BUILTIN_TO_LOWER, 1),
            (BUILTIN_TRIM, 1),
            (BUILTIN_PARSE_INT, 2),
            (BUILTIN_SPLIT, 1),
            (BUILTIN_CHAR_AT, 1),
            (BUILTIN_LIST_NEW, 1),
            (BUILTIN_LIST_LEN, 1),
            (BUILTIN_LIST_GET, 2),
            (BUILTIN_LIST_CONCAT, 1),
            (BUILTIN_FIBER_YIELD, 1),
            (BUILTIN_FIBER_RESUME, 1),
            (BUILTIN_NURSERY_JOIN, 1),
        ] {
            assert_eq!(fns[index as usize].result_words, want);
        }
        // the control pair is the dispatcher-intercepted tail of the table
        assert_eq!(
            fns[BUILTIN_FIBER_YIELD as usize].control,
            Some(ControlNative::Yield)
        );
        assert_eq!(
            fns[BUILTIN_FIBER_RESUME as usize].control,
            Some(ControlNative::Resume)
        );
        assert_eq!(
            fns[BUILTIN_NURSERY_JOIN as usize].control,
            Some(ControlNative::NurseryJoin)
        );
        assert!(
            fns.iter()
                .take(BUILTIN_FIBER_YIELD as usize)
                .all(|f| f.control.is_none()),
            "no earlier entry is a control native"
        );
    }

    #[test]
    fn std_platform_table_indices_are_contract() {
        let fns = std_platform_module();
        assert_eq!(fns[STD_PRINTLN as usize].name, "println");
        assert_eq!(fns[STD_PRINT as usize].name, "print");
        assert_eq!(fns[STD_READ_FILE as usize].name, "read_file");
        assert_eq!(fns[STD_WRITE_FILE as usize].name, "write_file");
        assert_eq!(fns[STD_FILE_EXISTS as usize].name, "file_exists");
        assert_eq!(fns[STD_FS_LIST as usize].name, "fs_list");
        assert_eq!(fns[STD_FILE_SIZE as usize].name, "file_size");
        assert_eq!(fns[STD_NET_CONNECT as usize].name, "net_connect");
        assert_eq!(fns[STD_NET_SEND as usize].name, "net_send");
        assert_eq!(fns[STD_NET_RECV as usize].name, "net_recv");
        assert_eq!(fns[STD_NET_CLOSE as usize].name, "net_close");
        assert_eq!(fns[STD_HTTP_GET as usize].name, "http_get");
        assert_eq!(fns.len(), 25);
        // every core index stays a reserved trap: the str_* trio and the
        // whole string block (10-19)
        for index in [2u32, 3, 4].into_iter().chain(10..=19) {
            assert_eq!(fns[index as usize].name, "reserved", "index {index}");
        }
        // the fallible ones need a two-word overlay; the rest one word
        for (index, want) in [
            (STD_PRINTLN, 1),
            (STD_PRINT, 1),
            (STD_READ_FILE, 2),
            (STD_WRITE_FILE, 2),
            (STD_FILE_EXISTS, 1),
            (STD_FS_LIST, 2),
            (STD_FILE_SIZE, 2),
            (STD_NET_CONNECT, 2),
            (STD_NET_SEND, 2),
            (STD_NET_RECV, 2),
            (STD_NET_CLOSE, 2),
            (STD_HTTP_GET, 2),
        ] {
            assert_eq!(fns[index as usize].result_words, want);
        }
    }

    #[test]
    fn reserved_gap_entries_trap_like_uninstalled_indices() {
        // each module's gaps report the module's own name and their index,
        // the same UnknownCallee an out-of-range index yields at resolution
        let traps_like = |module: &str, index: u32, fns: &[NativeFunction]| {
            assert_eq!(
                (fns[index as usize].func)(
                    &mut Objects::new(),
                    &mut OutputSink::Captured(Vec::new()),
                    &[]
                ),
                Err(Trap::UnknownCallee {
                    module: module.into(),
                    func: index,
                })
            );
        };
        let core = builtin_module();
        traps_like(BUILTIN_MODULE, 0, &core); // the console pair's slots
        traps_like(BUILTIN_MODULE, 5, &core); // the file-IO slots
        traps_like(BUILTIN_MODULE, 19, &core); // the spare slot
        let platform = std_platform_module();
        traps_like(STD_PLATFORM_MODULE, 2, &platform); // the str_* slots
        traps_like(STD_PLATFORM_MODULE, 16, &platform); // the string block
    }

    #[test]
    fn println_writes_the_line_print_does_not() {
        let mut objects = Objects::new();
        let id = objects.intern("hello");
        let mut out = OutputSink::Captured(Vec::new());
        assert_eq!(
            println_fn(&mut objects, &mut out, &[Value::Ref(id)]),
            Ok(NativeResult::Word(Value::I64(0)))
        );
        assert_eq!(out, OutputSink::Captured(b"hello\n".to_vec()));
        assert_eq!(
            print_fn(&mut objects, &mut out, &[Value::Ref(id)]),
            Ok(NativeResult::Word(Value::I64(0)))
        );
        assert_eq!(out, OutputSink::Captured(b"hello\nhello".to_vec()));
    }

    #[test]
    fn str_conversions_cover_the_scalar_types() {
        let mut objects = Objects::new();
        let mut out = OutputSink::Captured(Vec::new());
        let v = str_int_fn(&mut objects, &mut out, &[Value::I64(-1)])
            .expect("int converts")
            .word()
            .expect("a single word");
        assert_eq!(
            objects.str_at(match v {
                Value::Ref(id) => id,
                _ => panic!("a string ref"),
            }),
            Ok("-1")
        );
        let v = str_float_fn(&mut objects, &mut out, &[Value::F64(2.5)])
            .expect("float converts")
            .word()
            .expect("a single word");
        assert_eq!(
            objects.str_at(match v {
                Value::Ref(id) => id,
                _ => panic!("a string ref"),
            }),
            Ok("2.5")
        );
        let v = str_bool_fn(&mut objects, &mut out, &[Value::Bool(false)])
            .expect("bool converts")
            .word()
            .expect("a single word");
        assert_eq!(
            objects.str_at(match v {
                Value::Ref(id) => id,
                _ => panic!("a string ref"),
            }),
            Ok("false")
        );
    }

    #[test]
    fn parse_int_answers_both_span_members() {
        let mut objects = Objects::new();
        let mut out = OutputSink::Captured(Vec::new());
        let ok = objects.intern("-42");
        assert_eq!(
            parse_int_fn(&mut objects, &mut out, &[Value::Ref(ok)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(-42),
            })
        );
        let bad = objects.intern("12x");
        assert_eq!(
            parse_int_fn(&mut objects, &mut out, &[Value::Ref(bad)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_PARSE_ERR,
                payload: Value::Ref(bad),
            }),
            "the failure payload names the input text"
        );
    }

    #[test]
    fn list_natives_construct_read_and_join() {
        let mut objects = Objects::new();
        let mut out = OutputSink::Captured(Vec::new());
        // the empty construction
        let v = list_new_fn(&mut objects, &mut out, &[])
            .expect("new runs")
            .word()
            .expect("a single word");
        let Value::Ref(empty) = v else {
            panic!("a list ref, got {v:?}");
        };
        assert_eq!(objects.list_at(empty), Ok(&[][..]));
        // reads over any element kinds, both span members
        let list = objects.alloc_list(vec![Value::I64(7), Value::Bool(true)]);
        assert_eq!(
            list_len_fn(&mut objects, &mut out, &[Value::Ref(list)]),
            Ok(NativeResult::Word(Value::I64(2)))
        );
        assert_eq!(
            list_get_fn(&mut objects, &mut out, &[Value::Ref(list), Value::I64(1)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::Bool(true),
            })
        );
        for index in [2_i64, -1] {
            assert_eq!(
                list_get_fn(&mut objects, &mut out, &[
                    Value::Ref(list),
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
        let v = list_concat_fn(&mut objects, &mut out, &[
            Value::Ref(empty),
            Value::Ref(list),
        ])
        .expect("concat runs")
        .word()
        .expect("a single word");
        let Value::Ref(joined) = v else {
            panic!("a list ref, got {v:?}");
        };
        assert_ne!(joined, list, "the join is a fresh object");
        assert_eq!(
            objects.list_at(joined),
            Ok(&[Value::I64(7), Value::Bool(true)][..])
        );
        // a non-list argument names no object
        let str_obj = objects.intern("x");
        assert_eq!(
            list_len_fn(&mut objects, &mut out, &[Value::Ref(str_obj)]),
            Err(Trap::UnknownObject { obj: str_obj })
        );
        // the construction append: every element word kind pushes in
        // place, the answer is the inert unit word
        for value in [Value::I64(9), Value::Bool(false), Value::Ref(str_obj)] {
            assert_eq!(
                list_push_fn(&mut objects, &mut out, &[Value::Ref(empty), value]),
                Ok(NativeResult::Word(Value::I64(0)))
            );
        }
        assert_eq!(
            objects.list_at(empty),
            Ok(&[Value::I64(9), Value::Bool(false), Value::Ref(str_obj)][..]),
            "the pushes appended in order"
        );
        assert_eq!(
            list_push_fn(&mut objects, &mut out, &[
                Value::Ref(str_obj),
                Value::I64(0)
            ]),
            Err(Trap::UnknownObject { obj: str_obj }),
            "pushing onto a string object traps"
        );
    }
}
