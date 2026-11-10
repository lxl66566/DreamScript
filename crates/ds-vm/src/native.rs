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
//! | 10    | `len[t] (xs: t) -> int` (generic; CALLG, evidence-dispatched) | reserved |
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
//! | 21    | `list_len[t] (list) -> int` (generic; CALLG) | `net_send (conn: int, data: string) -> unit \| net_err` |
//! | 22    | `list_get[t] (list, i: int) -> t \| none` (generic; CALLG) | `net_recv (conn, max_bytes: int) -> string \| net_err` |
//! | 23    | `list_concat[t] (a, b: list) -> list` (generic; CALLG) | `net_close (conn: int) -> unit \| net_err` |
//! | 24    | `fiber_yield (value: int) -> int` | `http_get (url: string) -> string \| net_err` |
//! | 25    | `fiber_resume (fiber, value: int) -> int` | reserved |
//! | 26    | `nursery_join (nursery: int) -> int` | reserved |
//! | 27    | `list_push (xs: list, x: value) -> unit` | reserved |
//! | 28    | `map_new () -> map` | reserved |
//! | 29    | `map_len[k, v] (map) -> int` (generic; CALLG) | reserved |
//! | 30    | `map_get[k, v] (map, key: value) -> v \| none` (generic; CALLG) | reserved |
//! | 31    | `map_set (map, key, value) -> unit` | reserved |

//! | 32    | `cons[t] (x: value, xs: list) -> list` (generic; CALLG) | reserved |
//! | 33    | `head[t] (xs: list) -> t \| none` (generic; CALLG) | reserved |
//! | 34    | `tail[t] (xs: list) -> list` (generic; CALLG) | reserved |
//! | 35    | `collect[t] (xs: list) -> list` (generic; CALLG) | reserved |
//! | 36    | `first[t] (xs: list) -> t \| none` (generic; CALLG) | reserved |
//! | 37    | `max[t] (a, b: value) -> value` (generic; CALLG) | reserved |
//! | 38    | `map[t, r] (xs: list, f: fn(t) -> r) -> list[r]` (generic; CALLG, reentrant) | reserved |
//! | 39    | `filter[t] (xs: list, f: fn(t) -> bool) -> list[t]` (generic; CALLG, reentrant) | reserved |
//! | 40    | `reduce[t] (xs: list, init: t, f: fn(t, t) -> t) -> t` (generic; CALLG, reentrant) | reserved |
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
//! # The string natives (indices 11-18; `len` is generic at 10)
//!
//! The pure string surface, char-indexed: `len` counts unicode scalar
//! values, and `substring`/`char_at` index chars, never bytes, so
//! multi-byte text never cuts a code point. Char positions resolve
//! through the string object's lazily built boundary map (one
//! O(length) pass per string object, never per call), so `substring`
//! and `char_at` answer in O(range) once the bounds are clamped.
//! Out-of-range access stays
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
//! acquiring one count per `Ref` element it adopts; `head` and `first`
//! are two spellings of one checked first-element body, both answering
//! `list_get`'s two-word span (the hit is [`PLATFORM_IO_OK`] with the
//! element handed out under its own count, an empty list the
//! [`PLATFORM_NONE`] member with the inert unit word as payload —
//! there is no rejected index to carry); `max` answers the greater of
//! two numeric words, int in i64's total order and float IEEE (a NaN
//! operand answers the other side), any other word kind refusing like
//! a comparison against a mismatched slot. Their table slots stay
//! reserved trap entries like every generic builtin's.
//!
//! # The reentrant combinators (indices 38-40)
//!
//! The lambda-taking container combinators — `map`, `filter`,
//! `reduce` — are the first generic bodies that call user code: the
//! staged lambda parameter is a `Fn` word (the callable-word
//! classification, F2) naming a planned function, and the body runs it
//! per element through the VM's reentrancy seam (a synthetic reentry
//! frame driven to its return; see `vm::Vm::call_hosted`). `map`
//! collects each answered word into a fresh list, `filter` keeps the
//! elements whose lambda answers true (a non-bool answer refuses like
//! a mismatched slot), and `reduce` left-folds with the borrowed init
//! as the seed — an empty list answers the init under its own count.
//! A drive that ends the fiber or parks at the host instead of
//! returning refuses honestly: a Rust-side loop cannot suspend.
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

use crate::{
    object::Objects,
    value::Value,
    vm::{Trap, Vm},
};

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
/// The generic `len[t] (xs: t) -> int`: CALLG-addressed with one evidence
/// word, its one body dispatches on the instance ([`GENERIC_FUNCTIONS`]);
/// the table slot itself stays reserved, so a plain CALL cannot reach a
/// monomorphic instance.
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
/// `list_len[t](list) -> int` (generic; the ONE body lives in
/// [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_LIST_LEN: u32 = 21;
/// `list_get[t](list, i: int) -> t | none` (generic; the checked read,
/// out of range answers the `none` span member, never a trap; the ONE
/// body lives in [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_LIST_GET: u32 = 22;
/// `list_concat[t](a, b: list) -> list` (generic; a fresh list over
/// both element runs; the ONE body lives in [`GENERIC_FUNCTIONS`],
/// CALLG-addressed).
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
/// `map_new() -> map` (the empty map object; the `map[k, v]()`
/// constructor's runtime half).
pub const BUILTIN_MAP_NEW: u32 = 28;
/// `map_len[k, v](map) -> int` (generic; the entry count; the ONE body
/// lives in [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_MAP_LEN: u32 = 29;
/// `map_get[k, v](map, key: value) -> v | none` (generic; the checked
/// read in `list_get`'s span shape, a miss answers the `none` span
/// member with the probe key as the payload, never a trap; the ONE body
/// lives in [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_MAP_GET: u32 = 30;
/// `map_set(map, key, value: value) -> unit` — the one mutation, through
/// the checker's exclusive `mut` window. The key and value parameters
/// are `Raw`, their kinds varying by call site like `list_push`'s
/// element. A non-map first argument traps; the unit result is the inert
/// unit word.
pub const BUILTIN_MAP_SET: u32 = 31;

/// `cons[t](x: value, xs: list) -> list` (generic; the one-element
/// prepend, a fresh list over `x` then `xs`'s element words —
/// construction copies, the copy acquiring one count per `Ref` element
/// it adopts; the ONE body lives in [`GENERIC_FUNCTIONS`],
/// CALLG-addressed).
pub const BUILTIN_CONS: u32 = 32;
/// `head[t](xs: list) -> t | none` (generic; the checked first element
/// in `list_get`'s span shape — an empty list answers the `none` span
/// member, never a trap; the ONE body lives in [`GENERIC_FUNCTIONS`],
/// CALLG-addressed).
pub const BUILTIN_HEAD: u32 = 33;
/// `tail[t](xs: list) -> list` (generic; a fresh list over the elements
/// after the first — construction copies; an empty list answers the
/// empty list; the ONE body lives in [`GENERIC_FUNCTIONS`],
/// CALLG-addressed).
pub const BUILTIN_TAIL: u32 = 34;
/// `collect[t](xs: list) -> list` (generic; the pipeline closer, a plain
/// fresh copy of its list; the ONE body lives in
/// [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_COLLECT: u32 = 35;
/// `first[t](xs: list) -> t | none` (generic; the checked first element
/// under the corpus's other spelling — `head`'s one shared body; the
/// ONE body lives in [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_FIRST: u32 = 36;
/// `max[t](a, b: value) -> value` (generic; the greater of the two
/// numeric words — int compares in i64's total order, float follows
/// IEEE and never traps; any other word kind refuses like a comparison
/// against a mismatched slot; the ONE body lives in
/// [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_MAX: u32 = 37;
/// `map[t, r](xs: list, f: fn) -> list` (generic; the one body reenters
/// the dispatcher per element through the VM's hosted-call seam,
/// running the staged lambda word over each element — the reentrant
/// CALLG seam; the ONE body lives in [`GENERIC_FUNCTIONS`],
/// CALLG-addressed).
pub const BUILTIN_MAP: u32 = 38;
/// `filter[t](xs: list, f: fn) -> list` (generic; the predicate keeps
/// or drops each element through the same reentrant per-element call;
/// the ONE body lives in [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_FILTER: u32 = 39;
/// `reduce[t](xs: list, init: value, f: fn) -> value` (generic; the
/// left fold over the same reentrant per-element call, the borrowed
/// init seeding the accumulator and an empty list answering it; the ONE
/// body lives in [`GENERIC_FUNCTIONS`], CALLG-addressed).
pub const BUILTIN_REDUCE: u32 = 40;
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

fn substring_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let n = i64::try_from(objects.char_count(id)?).expect("string length fits i64");
    // clamped to [0, n], so the narrowed index is a valid char position
    let start = usize::try_from(i64_arg(args, 1)?.clamp(0, n)).expect("index fits usize");
    let end = usize::try_from(i64_arg(args, 2)?.clamp(0, n)).expect("index fits usize");
    if start >= end {
        return Ok(NativeResult::Word(Value::Ref(objects.alloc_str_part(""))));
    }
    // both offsets sit on char boundaries, so the slice cannot cut a code point
    let (begin, stop) = objects.char_byte_range(id, start, end)?;
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_substr(id, begin, stop)?,
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
    let id = ref_arg(args, 0)?;
    let index = i64_arg(args, 1)?;
    // out of range by either side: the empty string, never a trap
    if index < 0 {
        return Ok(NativeResult::Word(Value::Ref(objects.alloc_str_part(""))));
    }
    let i = usize::try_from(index).expect("checked index fits usize");
    if i >= objects.char_count(id)? {
        return Ok(NativeResult::Word(Value::Ref(objects.alloc_str_part(""))));
    }
    let (begin, stop) = objects.char_byte_range(id, i, i + 1)?;
    Ok(NativeResult::Word(Value::Ref(
        objects.alloc_substr(id, begin, stop)?,
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

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
fn map_new_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Ref(objects.alloc_map())))
}

fn map_len_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let n =
        i64::try_from(objects.map_pairs(ref_arg(args, 0)?)?.len()).expect("map length fits i64");
    Ok(NativeResult::Word(Value::I64(n)))
}

fn map_get_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let map = ref_arg(args, 0)?;
    let hit = objects.map_at(map, args[1])?;
    Ok(match hit {
        Some(value) => {
            // the read hands the value word out: a Ref value gains a
            // count for the reader, the map keeps its own (C4)
            if let Value::Ref(id) = value {
                objects.inc(id)?;
            }
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: value,
            }
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
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    objects.set_map(ref_arg(args, 0)?, args[1], args[2])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

/// Acquires one count per `Ref` word of `words` (the producer retain
/// contract): a fresh list adopting the words owns its element words,
/// the sources keep their own (C4).
fn retain_copied(objects: &mut Objects, words: &[Value]) -> Result<(), Trap> {
    for &value in words {
        if let Value::Ref(id) = value {
            objects.inc(id)?;
        }
    }
    Ok(())
}

fn cons_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let head = args[0];
    let rest: Vec<Value> = objects.list_at(ref_arg(args, 1)?)?.to_vec();
    let mut elems = Vec::with_capacity(rest.len() + 1);
    elems.push(head);
    elems.extend(rest);
    retain_copied(objects, &elems)?;
    Ok(NativeResult::Word(Value::Ref(objects.alloc_list(elems))))
}

fn tail_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // an empty list skips nothing and answers the empty list
    let rest: Vec<Value> = objects
        .list_at(ref_arg(args, 0)?)?
        .iter()
        .skip(1)
        .copied()
        .collect();
    retain_copied(objects, &rest)?;
    Ok(NativeResult::Word(Value::Ref(objects.alloc_list(rest))))
}

fn collect_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let words: Vec<Value> = objects.list_at(ref_arg(args, 0)?)?.to_vec();
    retain_copied(objects, &words)?;
    Ok(NativeResult::Word(Value::Ref(objects.alloc_list(words))))
}

/// `head`'s and `first`'s one body: the checked first element in
/// `list_get`'s span shape. The hit hands the element word out — a
/// `Ref` element gains a count for the reader, the list keeps its own
/// (C4) — and an empty list answers the `none` span member, never a
/// trap. The miss payload is the inert unit word: there is no rejected
/// index to carry, and callers read the payload only on the hit.
fn first_element_fn(
    objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let elems = objects.list_at(ref_arg(args, 0)?)?;
    Ok(match elems.first() {
        Some(&value) => {
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
            payload: Value::I64(0),
        },
    })
}

/// The greater of two numeric words: int compares in i64's total order;
/// float follows IEEE and never traps, a NaN operand answering the other
/// side. Any other word kind refuses like a comparison against a
/// mismatched slot — the call site's own trap shape.
fn max_fn(
    _objects: &mut Objects,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::I64(a), Value::I64(b)] => Ok(NativeResult::Word(Value::I64((*a).max(*b)))),
        [Value::F64(a), Value::F64(b)] => Ok(NativeResult::Word(Value::F64((*a).max(*b)))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
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
    /// The real arguments' expected word kinds.
    pub params: &'static [SlotType],
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
/// through [`Vm::call_hosted`] mid-body — while the pure bodies split
/// the heap and sink off themselves ([`Vm::split`]) and never touch the
/// rest.
pub type GenericFn = fn(&mut Vm, &[Value], &[Value]) -> Result<NativeResult, Trap>;

/// The generic builtin vocabulary, keyed by the (module, native index)
/// FuncRef address [H3]. `len[t]`'s one body dispatches on the single
/// evidence word and answers the string char count or the list element
/// count. The container reads and counts (`list_len`, `list_get`,
/// `list_concat`, `map_len`, `map_get`) share the same discipline with
/// element-agnostic bodies: the container word is one `Ref` whatever the
/// element, so the evidence words (one per constraint-list entry, each
/// naming the receiver's container instance) carry no dispatch — they
/// record the instance for the future registry. No monomorphic instance
/// exists at runtime, and the table slots the entries address stay
/// reserved, so a plain CALL never reaches a monomorphic fallback. The
/// container closes past the map block — `cons`, `tail`, `collect`,
/// `max` — join them with element-agnostic bodies, and `head`/`first`
/// share the one first-element body; `max` dispatches on the numeric
/// instance its evidence word names. The lambda-taking combinators —
/// `map`, `filter`, `reduce` — append past the closes with reentrant
/// bodies that call the staged `Fn` word per element through the VM's
/// hosted-call seam; their evidence words record the receiver's list
/// instance like every element-agnostic body's.
pub const GENERIC_FUNCTIONS: &[(&str, u32, GenericFunction)] = &[
    (BUILTIN_MODULE, BUILTIN_LEN, GenericFunction {
        name: "len",
        params: &[SlotType::Ref],
        evidence: 1,
        result_words: 1,
        func: len_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_LIST_LEN, GenericFunction {
        name: "list_len",
        params: &[SlotType::Ref],
        evidence: 1,
        result_words: 1,
        func: list_len_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_LIST_GET, GenericFunction {
        name: "list_get",
        params: &[SlotType::Ref, SlotType::I64],
        evidence: 1,
        result_words: 2,
        func: list_get_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_LIST_CONCAT, GenericFunction {
        name: "list_concat",
        params: &[SlotType::Ref, SlotType::Ref],
        evidence: 1,
        result_words: 1,
        func: list_concat_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_MAP_LEN, GenericFunction {
        name: "map_len",
        params: &[SlotType::Ref],
        evidence: 2,
        result_words: 1,
        func: map_len_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_MAP_GET, GenericFunction {
        name: "map_get",
        params: &[SlotType::Ref, SlotType::Raw],
        evidence: 2,
        result_words: 2,
        func: map_get_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_CONS, GenericFunction {
        name: "cons",
        params: &[SlotType::Raw, SlotType::Ref],
        evidence: 1,
        result_words: 1,
        func: cons_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_HEAD, GenericFunction {
        name: "head",
        params: &[SlotType::Ref],
        evidence: 1,
        result_words: 2,
        func: head_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_TAIL, GenericFunction {
        name: "tail",
        params: &[SlotType::Ref],
        evidence: 1,
        result_words: 1,
        func: tail_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_COLLECT, GenericFunction {
        name: "collect",
        params: &[SlotType::Ref],
        evidence: 1,
        result_words: 1,
        func: collect_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_FIRST, GenericFunction {
        name: "first",
        params: &[SlotType::Ref],
        evidence: 1,
        result_words: 2,
        func: head_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_MAX, GenericFunction {
        name: "max",
        params: &[SlotType::Raw, SlotType::Raw],
        evidence: 1,
        result_words: 1,
        func: max_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_MAP, GenericFunction {
        name: "map",
        params: &[SlotType::Ref, SlotType::Fn],
        evidence: 2,
        result_words: 1,
        func: map_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_FILTER, GenericFunction {
        name: "filter",
        params: &[SlotType::Ref, SlotType::Fn],
        evidence: 1,
        result_words: 1,
        func: filter_generic_fn,
    }),
    (BUILTIN_MODULE, BUILTIN_REDUCE, GenericFunction {
        name: "reduce",
        params: &[SlotType::Ref, SlotType::Raw, SlotType::Fn],
        evidence: 1,
        result_words: 1,
        func: reduce_generic_fn,
    }),
];

/// Resolves the generic builtin a CALLG FuncRef addresses, if any.
#[must_use]
pub fn resolve_generic(module: &str, index: u32) -> Option<&'static GenericFunction> {
    GENERIC_FUNCTIONS
        .iter()
        .find(|(m, i, _)| *m == module && *i == index)
        .map(|(_, _, g)| g)
}

/// The one `len` body: dispatch on the call's evidence word, the
/// statically-resolved builtin instance of the instantiated type
/// ([`ds_abi::BuiltinInstance`]). The string instance counts unicode
/// scalar values; the list instance counts elements.
fn len_generic_fn(vm: &mut Vm, evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let (objects, _out) = vm.split();
    let instance = evidence.first().copied().unwrap_or(Value::I64(-1));
    match instance {
        Value::I64(id) if id == ds_abi::BuiltinInstance::Str as i64 => {
            // char count, never bytes: multi-byte text measures its scalar values
            let n = i64::try_from(objects.char_count(ref_arg(args, 0)?)?)
                .expect("string length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::List as i64 => {
            let elems = objects.list_at(ref_arg(args, 0)?)?;
            let n = i64::try_from(elems.len()).expect("list length fits i64");
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
    let (objects, out) = vm.split();
    list_len_fn(objects, out, args)
}

fn list_get_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    list_get_fn(objects, out, args)
}

fn list_concat_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    list_concat_fn(objects, out, args)
}

fn map_len_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    map_len_fn(objects, out, args)
}

fn map_get_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    map_get_fn(objects, out, args)
}

fn cons_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    cons_fn(objects, out, args)
}

fn head_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    first_element_fn(objects, out, args)
}

fn tail_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    tail_fn(objects, out, args)
}

fn collect_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (objects, out) = vm.split();
    collect_fn(objects, out, args)
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
            let (objects, out) = vm.split();
            max_fn(objects, out, args)
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

/// `map`'s one body: each element runs through the staged lambda, and
/// the answered word — transfer count and all — pushes onto the fresh
/// result list (the push acquires the list's own count per `Ref`
/// word); the body then drops the transferred count it still holds.
fn map_generic_fn(vm: &mut Vm, _evidence: &[Value], args: &[Value]) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
    let elems = {
        let (objects, _) = vm.split();
        objects.list_at(ref_arg(args, 0)?)?.to_vec()
    };
    let res = {
        let (objects, _) = vm.split();
        objects.alloc_list(Vec::new())
    };
    for x in elems {
        let mapped = vm.call_hosted(f, &[x])?;
        let (objects, _) = vm.split();
        objects.push_list(res, mapped)?;
        if let Value::Ref(id) = mapped {
            objects.dec(id)?;
        }
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}

/// `filter`'s one body: each element's predicate answer keeps or drops
/// it. A non-bool answer refuses like a mismatched slot — the declared
/// predicate's word is the contract. Keeping pushes the element onto
/// the fresh result list (the push acquires the list's own count; the
/// source list keeps its own), so no element-side count moves at all.
fn filter_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
    let elems = {
        let (objects, _) = vm.split();
        objects.list_at(ref_arg(args, 0)?)?.to_vec()
    };
    let res = {
        let (objects, _) = vm.split();
        objects.alloc_list(Vec::new())
    };
    for x in elems {
        let keep = vm.call_hosted(f, &[x])?;
        match keep {
            Value::Bool(true) => {
                let (objects, _) = vm.split();
                objects.push_list(res, x)?;
            },
            Value::Bool(false) => {},
            other => {
                return Err(Trap::SlotTypeMismatch {
                    slot: 0,
                    expected: SlotType::Bool,
                    actual: other.slot_type(),
                })
            },
        }
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}

/// `reduce`'s one body: the accumulator starts as the borrowed init
/// word (native arguments are a transient borrow window), each step
/// hands the accumulator and element to the staged lambda under the
/// hosted call's one-count-per-Ref hand-off (the lambda's exit drops
/// or returns them), and the answer's transfer count replaces the
/// owned one — the first step's old value is the borrowed init and is
/// never dropped. An empty list answers the init handed out under its
/// own count (the `list_get` discipline); otherwise the final owned
/// count transfers to the caller with the word.
fn reduce_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 2)?;
    let elems = {
        let (objects, _) = vm.split();
        objects.list_at(ref_arg(args, 0)?)?.to_vec()
    };
    let mut acc = args[1];
    let mut owned = false;
    for x in elems {
        let next = vm.call_hosted(f, &[acc, x])?;
        if owned {
            if let Value::Ref(id) = acc {
                let (objects, _) = vm.split();
                objects.dec(id)?;
            }
        }
        acc = next;
        owned = true;
    }
    if !owned {
        if let Value::Ref(id) = acc {
            let (objects, _) = vm.split();
            objects.inc(id)?;
        }
    }
    Ok(NativeResult::Word(acc))
}

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
reserved!(builtin_reserved_21 in BUILTIN_MODULE at 21); // was the monomorphic list_len; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_22 in BUILTIN_MODULE at 22); // was list_get; the generic body lives in GENERIC_FUNCTIONS
reserved!(builtin_reserved_23 in BUILTIN_MODULE at 23); // was list_concat; the generic body lives in GENERIC_FUNCTIONS
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
/// the string ops (11-18), and the list and map construction and
/// mutation surfaces — pure with no world effects. The historical
/// world-surface indices (0-1, 5-9), the string block's spare slot (19),
/// and every generic builtin's slot (10, 21-23, 29-30, 32-37 — their ONE
/// bodies live in [`GENERIC_FUNCTIONS`], CALLG-addressed) stay as
/// reserved trap entries; the list block appends past the historical
/// run (indices freeze at M7), the fiber substrate's control pair
/// ([`BUILTIN_FIBER_YIELD`], [`BUILTIN_FIBER_RESUME`]) appends past the
/// list block, and the nursery boundary's debug join
/// ([`BUILTIN_NURSERY_JOIN`]) appends past the pair, the generic
/// container closes (`cons`, `head`, `tail`, `collect`, `first`,
/// `max`) append past the map block, and the reentrant combinators
/// (`map`, `filter`, `reduce`) append past the closes.
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
        reserved_entry(builtin_reserved_10),
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
        reserved_entry(builtin_reserved_21),
        reserved_entry(builtin_reserved_22),
        reserved_entry(builtin_reserved_23),
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
        NativeFunction {
            name: "map_new",
            params: &[],
            result_words: 1,
            control: None,
            func: map_new_fn,
        },
        reserved_entry(builtin_reserved_29),
        reserved_entry(builtin_reserved_30),
        NativeFunction {
            name: "map_set",
            params: &[SlotType::Ref, SlotType::Raw, SlotType::Raw],
            result_words: 1,
            control: None,
            func: map_set_fn,
        },
        reserved_entry(builtin_reserved_32),
        reserved_entry(builtin_reserved_33),
        reserved_entry(builtin_reserved_34),
        reserved_entry(builtin_reserved_35),
        reserved_entry(builtin_reserved_36),
        reserved_entry(builtin_reserved_37),
        reserved_entry(builtin_reserved_38),
        reserved_entry(builtin_reserved_39),
        reserved_entry(builtin_reserved_40),
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
        // the generic builtins' table slots stay reserved (a plain CALL
        // must not reach a monomorphic instance), and the ONE bodies live
        // in the generic table keyed by the same FuncRef
        for (index, name, evidence, words) in [
            (BUILTIN_LEN, "len", 1, 1),
            (BUILTIN_LIST_LEN, "list_len", 1, 1),
            (BUILTIN_LIST_GET, "list_get", 1, 2),
            (BUILTIN_LIST_CONCAT, "list_concat", 1, 1),
            (BUILTIN_MAP_LEN, "map_len", 2, 1),
            (BUILTIN_MAP_GET, "map_get", 2, 2),
            (BUILTIN_CONS, "cons", 1, 1),
            (BUILTIN_HEAD, "head", 1, 2),
            (BUILTIN_TAIL, "tail", 1, 1),
            (BUILTIN_COLLECT, "collect", 1, 1),
            (BUILTIN_FIRST, "first", 1, 2),
            (BUILTIN_MAX, "max", 1, 1),
            (BUILTIN_MAP, "map", 2, 1),
            (BUILTIN_FILTER, "filter", 1, 1),
            (BUILTIN_REDUCE, "reduce", 1, 1),
        ] {
            assert_eq!(fns[index as usize].name, "reserved", "{name}'s slot");
            let body = resolve_generic(BUILTIN_MODULE, index)
                .unwrap_or_else(|| panic!("the generic {name} body resolves"));
            assert_eq!(body.name, name);
            assert_eq!(body.evidence, evidence);
            assert_eq!(body.result_words, words);
        }
        assert_eq!(fns[BUILTIN_SUBSTRING as usize].name, "substring");
        assert_eq!(fns[BUILTIN_CONCAT as usize].name, "concat");
        assert_eq!(fns[BUILTIN_TO_UPPER as usize].name, "to_upper");
        assert_eq!(fns[BUILTIN_TO_LOWER as usize].name, "to_lower");
        assert_eq!(fns[BUILTIN_TRIM as usize].name, "trim");
        assert_eq!(fns[BUILTIN_PARSE_INT as usize].name, "parse_int");
        assert_eq!(fns[BUILTIN_SPLIT as usize].name, "split");
        assert_eq!(fns[BUILTIN_CHAR_AT as usize].name, "char_at");
        assert_eq!(fns[BUILTIN_LIST_NEW as usize].name, "list_new");
        assert_eq!(fns[BUILTIN_FIBER_YIELD as usize].name, "fiber_yield");
        assert_eq!(fns[BUILTIN_FIBER_RESUME as usize].name, "fiber_resume");
        assert_eq!(fns[BUILTIN_NURSERY_JOIN as usize].name, "nursery_join");
        assert_eq!(fns[BUILTIN_LIST_PUSH as usize].name, "list_push");
        assert_eq!(fns[BUILTIN_MAP_NEW as usize].name, "map_new");
        assert_eq!(fns[BUILTIN_MAP_SET as usize].name, "map_set");
        assert_eq!(fns.len(), 41);
        // every world index, the spare slot, and the generic builtins'
        // slots stay a reserved trap
        for index in [
            0u32, 1, 5, 6, 7, 8, 9, 10, 19, 21, 22, 23, 29, 30, 32, 33, 34, 35, 36, 37, 38, 39, 40,
        ] {
            assert_eq!(fns[index as usize].name, "reserved", "index {index}");
        }
        // the fallible ones need a two-word overlay; the rest one word
        for (index, want) in [
            (BUILTIN_STR_INT, 1),
            (BUILTIN_STR_FLOAT, 1),
            (BUILTIN_STR_BOOL, 1),
            (BUILTIN_SUBSTRING, 1),
            (BUILTIN_CONCAT, 1),
            (BUILTIN_TO_UPPER, 1),
            (BUILTIN_TO_LOWER, 1),
            (BUILTIN_TRIM, 1),
            (BUILTIN_PARSE_INT, 2),
            (BUILTIN_SPLIT, 1),
            (BUILTIN_CHAR_AT, 1),
            (BUILTIN_LIST_NEW, 1),
            (BUILTIN_FIBER_YIELD, 1),
            (BUILTIN_FIBER_RESUME, 1),
            (BUILTIN_NURSERY_JOIN, 1),
            (BUILTIN_MAP_NEW, 1),
            (BUILTIN_MAP_SET, 1),
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
