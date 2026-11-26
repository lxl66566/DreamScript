//! The native vocabulary: the builtin core's rows, the platform world-op table, and the comptime
//! reflection module, plus the reserved discriminants and module names they share.
//!
//! A row is declarative: the name, the surface and reserved index a pool `FuncRef` addresses
//! positionally, the parameter word kinds, the staging discipline, and the result shape. The VM
//! attaches a body to each row and installs the tables; the lowering resolves source call sites
//! onto the rows and attaches the ambient failure spelling a classed span needs there. Neither
//! side keeps its own copy of a row.
//!
//! A generic row describes the one CALLG-dispatched body a call's instantiation selects: the
//! evidence-word count, the staged-span discipline, and the builtin instances the one body serves.
//!
//! # The reserved discriminants
//!
//! Tag ids are per-module, counting up from 0 over a module's own union members. The top of the
//! `u8` space is reserved instead, counting down, so a result span's discriminant is one value
//! whatever surface answers it. A module mints its own ids upward, below the countdown's floor: a
//! mint reaching the reserved run would alias a user member with a platform discriminant, and
//! dispatch compares tags by byte equality.
//!
//! # Reserved indices
//!
//! Pool `FuncRef`s address rows positionally, so an index never changes value once published. A
//! gap — a retired native's slot, or a generic row's monomorphic slot — keeps its position in the
//! installed table as an entry that traps like an uninstalled index.

use crate::{BuiltinInstance, SlotType, Staging};

// The reserved discriminants, counting down from the top of the tag-id space.

/// The success discriminant of every fallible native's result span: the top of the reservation. One
/// shared value disambiguates nothing, since the caller knows which native it called.
pub const PLATFORM_IO_OK: u8 = u8::MAX;
/// The parse reader's failure discriminant; the payload word carries the position it gave up at.
pub const PLATFORM_PARSE_ERR: u8 = u8::MAX - 3;
/// The predeclared `none` atom's tag id: every checked read's miss answer and every optional
/// field's absent case.
pub const PLATFORM_NONE: u8 = u8::MAX - 4;
/// The cancel sentinel: the death value a fiber stopped under its nursery's cancel flag delivers to
/// its resumer.
pub const PLATFORM_CANCELLED: u8 = u8::MAX - 5;
/// The nursery boundary span's clean-join discriminant.
pub const NURSERY_BOUNDARY_OK: u8 = u8::MAX - 6;
/// The nursery boundary span's first-failure discriminant; the payload word carries the failing
/// child's error value.
pub const NURSERY_BOUNDARY_ERR: u8 = u8::MAX - 7;
// The failure classes: each a discriminant with a fixed ambient member name at the call site.

/// `net_timeout`.
pub const PLATFORM_NET_TIMEOUT: u8 = u8::MAX - 10;
/// `net_refused`.
pub const PLATFORM_NET_REFUSED: u8 = u8::MAX - 11;
/// `utf8_err`.
pub const PLATFORM_UTF8_ERR: u8 = u8::MAX - 12;
/// `io_not_found`.
pub const PLATFORM_IO_NOT_FOUND: u8 = u8::MAX - 13;
/// `io_permission_denied`.
pub const PLATFORM_IO_PERMISSION_DENIED: u8 = u8::MAX - 14;
/// `io_already_exists`.
pub const PLATFORM_IO_ALREADY_EXISTS: u8 = u8::MAX - 15;
/// `io_timed_out`, and the deadline rows' only class.
pub const PLATFORM_IO_TIMED_OUT: u8 = u8::MAX - 16;
/// `io_unexpected_eof`.
pub const PLATFORM_IO_UNEXPECTED_EOF: u8 = u8::MAX - 17;
/// `io_other`, the io family's floor.
pub const PLATFORM_IO_OTHER: u8 = u8::MAX - 18;
/// `net_addr_in_use`.
pub const PLATFORM_NET_ADDR_IN_USE: u8 = u8::MAX - 19;
/// `net_reset`.
pub const PLATFORM_NET_RESET: u8 = u8::MAX - 20;
/// `net_other`, the net family's floor.
pub const PLATFORM_NET_OTHER: u8 = u8::MAX - 21;

// The module names two surfaces reserve, and the platform default.

/// The builtin core's reserved module name.
pub const BUILTIN_MODULE: &str = "builtin";
/// The comptime reflection module's reserved name.
pub const COMPTIME_MODULE: &str = "comptime";
/// The default platform's name: what a source that declares no platform of its own addresses.
pub const STD_PLATFORM_NAME: &str = "std";
/// The reserved module name a runtime-resolved trait dispatch site inside a generic impl body
/// carries, its `func` field naming the calling frame's evidence slot.
pub const EVIDENCE_MODULE: &str = "$evidence";

// The reserved indices.

/// `str_int(x: int) -> string`.
pub const BUILTIN_STR_INT: u32 = 2;
/// `str_float(x: float) -> string`.
pub const BUILTIN_STR_FLOAT: u32 = 3;
/// `str_bool(x: bool) -> string`.
pub const BUILTIN_STR_BOOL: u32 = 4;
/// The generic `len[t] (xs: t) -> int`; CALLG-addressed with one evidence word.
pub const BUILTIN_LEN: u32 = 10;
/// `substring(s: string, start, end: int) -> string | none`: the checked char-indexed slice, every
/// out-of-range request the `none` tag, never clamped.
pub const BUILTIN_SUBSTRING: u32 = 11;
/// `concat(a, b: string) -> string` (the callable face of the STRCAT uop).
pub const BUILTIN_CONCAT: u32 = 12;
/// `to_upper(s: string) -> string`.
pub const BUILTIN_TO_UPPER: u32 = 13;
/// `to_lower(s: string) -> string`.
pub const BUILTIN_TO_LOWER: u32 = 14;
/// `trim(s: string) -> string` (leading and trailing whitespace dropped).
pub const BUILTIN_TRIM: u32 = 15;
/// `parse_int(s: string) -> int | parse_err` (the position the reader gave up at rides the error
/// payload).
pub const BUILTIN_PARSE_INT: u32 = 16;
/// `split(s, sep: string) -> list[string]` (a list object of part refs).
pub const BUILTIN_SPLIT: u32 = 17;
/// `char_at(s: string, i: int) -> char | none`: the checked char read, the miss answer the `none`
/// tag with the rejected index.
pub const BUILTIN_CHAR_AT: u32 = 18;
/// `parse_float(s: string) -> float | parse_err`: `parse_int`'s twin over the float slot.
pub const BUILTIN_PARSE_FLOAT: u32 = 19;
/// `list_new -> list` (the empty list object).
pub const BUILTIN_LIST_NEW: u32 = 20;
/// The generic `list_len[t] (xs: list[t]) -> int`.
pub const BUILTIN_LIST_LEN: u32 = 21;
/// The generic `list_get[t] (xs: list[t], i: int) -> t \| none`.
pub const BUILTIN_LIST_GET: u32 = 22;
/// The generic `list_concat[t] (a, b: list[t]) -> list[t]`.
pub const BUILTIN_LIST_CONCAT: u32 = 23;
/// Reserved: the retired `fiber_yield` slot.
pub const BUILTIN_FIBER_YIELD: u32 = 24;
/// `fiber_resume(fiber: int, value: int) -> int` — the substrate's debug switch: parks the current
/// fiber and enters `fiber` with `value` (a never-started fiber drops the payload); the call
/// answers the value the entered fiber next parks with or completes with. An absent, dead, or self
/// target answers the bare `PLATFORM_NONE` tag (no switch), the checked-read convention of
/// `list_get` at one word.
pub const BUILTIN_FIBER_RESUME: u32 = 25;
/// `nursery_join(nursery: int) -> int` — the nursery boundary's debug face: parks the caller,
/// drives the named nursery's children to death in spawn order (first failure raises the cancel
/// flag for the rest), and answers the failed-children count (0 = all joined). Per-child outcomes
/// union out through `join_nursery`.
pub const BUILTIN_NURSERY_JOIN: u32 = 26;
/// `list_push(xs: list, x: value) -> unit` — the construction append list literals lower to: one
/// element word onto a list the constructing frame uniquely owns. The element parameter is `Raw`,
/// the one native whose argument kind varies by call site, so it accepts every kind.
pub const BUILTIN_LIST_PUSH: u32 = 27;
/// `map_new -> map` (the empty map object; the `map[k, v]` constructor's runtime half).
pub const BUILTIN_MAP_NEW: u32 = 28;
/// The generic `map_len[k, v] (m: map[k, v]) -> int`.
pub const BUILTIN_MAP_LEN: u32 = 29;
/// The generic `map_get[k, v] (m: map[k, v], key: k) -> v \| none`.
pub const BUILTIN_MAP_GET: u32 = 30;
/// `map_set(map, key, value: value) -> unit` — the one mutation, through the checker's exclusive
/// `mut` window. The key and value parameters are `Raw`, their kinds varying by call site like
/// `list_push`'s element.
pub const BUILTIN_MAP_SET: u32 = 31;
/// The generic `cons[t] (x: t, xs: list[t]) -> list[t]`: the head element stages as one whole
/// inline span before the trailing list word.
pub const BUILTIN_CONS: u32 = 32;
/// Reserved: the retired `head` slot; `first` is the one list-front spelling.
pub const BUILTIN_HEAD: u32 = 33;
/// The generic `tail[t] (xs: list[t]) -> list[t]`.
pub const BUILTIN_TAIL: u32 = 34;
/// The generic `collect[t] (xs: list[t]) -> list[t]`.
pub const BUILTIN_COLLECT: u32 = 35;
/// The generic `first[t] (xs: list[t]) -> t \| none`, the checked first-element read.
pub const BUILTIN_FIRST: u32 = 36;
/// The generic `max[t] (a, b: t) -> t` over the numeric instances.
pub const BUILTIN_MAX: u32 = 37;
/// The generic `map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]`, reentrant.
pub const BUILTIN_MAP: u32 = 38;
/// The generic `filter[t] (xs: list[t], f: fn(t) -> bool) -> list[t]`, reentrant.
pub const BUILTIN_FILTER: u32 = 39;
/// The generic `reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t`, reentrant.
pub const BUILTIN_REDUCE: u32 = 40;
/// `nursery_open -> int` — the in-language `nursery` block's open seam: opens a fresh block-scoped
/// nursery (not the root) and answers its pool id. Never user-callable: the lowerer synthesizes the
/// call.
pub const BUILTIN_NURSERY_OPEN: u32 = 41;
/// `nursery_spawn(nursery: int, child: fn) -> unit` — the `spawn` expression's seam: the callable
/// word names the synthesized child (a scalar `Fn` word or a closure object's `Ref` whose captures
/// become the entry arguments); the fiber is born ready into the open nursery — the join at the
/// block's exit drives it. Never user-callable.
pub const BUILTIN_NURSERY_SPAWN: u32 = 42;
/// `nursery_boundary(nursery: int, mask: int) -> tag+payload` — the in-language join seam: parks
/// the caller, drives the named nursery's children to death in spawn order (first failure raises
/// the cancel flag for the rest), and answers the boundary result: a two-word span whose word 0 is
/// `NURSERY_BOUNDARY_OK` on a clean join or `NURSERY_BOUNDARY_ERR` on a first failure. A clean
/// join's word 1 is the collected answers list's `Ref` — one element per child, each child's whole
/// answer span in spawn order, the list living in the joiner's own heap; a failure's word 1 begins
/// the failing child's error payload.
pub const BUILTIN_NURSERY_BOUNDARY: u32 = 43;
/// `nursery_cancel(e: word) -> unit` — the `cancel(e)` call's seam: the current fiber dies trapped
/// carrying the payload, the trap path ledgers the outcome and raises its nursery's cancel flag.
/// Never returns; the unit result shape only satisfies the call site's width check.
pub const BUILTIN_NURSERY_CANCEL: u32 = 44;
/// `join (parts: list[string], sep: string) -> string` — the list of strings rendered back to one
/// string, the separator between neighbors. An empty list answers the empty string; the elements
/// and the separator stay borrowed (the `list_len` discipline), the answer one fresh string object.
pub const BUILTIN_JOIN: u32 = 45;
/// The generic `partition[t] (xs: list[t], f: fn(t) -> bool) -> (list[t], list[t])`, reentrant and
/// the tag-less two-word total.
pub const BUILTIN_PARTITION: u32 = 46;
/// The generic `fork_map[t, r]`: the pooled fork-join engine seam.
pub const BUILTIN_PAR_MAP: u32 = 47;
/// The generic `fork_reduce[t]`: the pooled fold's fork-join engine seam.
pub const BUILTIN_PAR_REDUCE: u32 = 48;
/// `total_compare(a: float, b: float) -> int` - the explicit float sort key the `ord` derive orders
/// float parts through : the IEEE-754 total order over f64 (negative NaN least, positive NaN
/// greatest, `-0.0` before `+0.0`), answered as one i64 word of -1, 0, or 1. Every pair is ordered,
/// so it is the one total comparison the op set's trapping `<`/`>` cannot spell.
pub const BUILTIN_TOTAL_COMPARE: u32 = 49;
/// `str_compare(a: string, b: string) -> int` - the string sort key the `ord` derive orders string
/// parts through: the lexicographic order over the UTF-8 bytes (the code-point order), answered as
/// one i64 word of -1, 0, or 1.
pub const BUILTIN_STR_COMPARE: u32 = 50;
/// `char_compare(a: char, b: char) -> int` - the char sort key the `ord` derive orders char parts
/// through: the Unicode scalar order over the two code point words, answered as one i64 word of
/// -1, 0, or 1.
pub const BUILTIN_CHAR_COMPARE: u32 = 51;
/// `byte_compare(a: byte, b: byte) -> int` - the byte sort key the `ord` derive orders byte parts
/// through: the u8 order over the two byte words, answered as one i64 word of -1, 0, or 1.
pub const BUILTIN_BYTE_COMPARE: u32 = 52;
/// `str_hash(s: string) -> int` - the content hash the `hash` derive folds string parts through:
/// FNV-1a over the UTF-8 bytes (offset basis 14695981039346656037, prime 1099511628211, the same
/// pair the derive's fold mixes through), answered as one i64 word. The value is an implementation
/// detail; consistency with structural `==` is the contract.
pub const BUILTIN_STR_HASH: u32 = 53;
/// `float_hash(f: float) -> int` - the content hash the `hash` derive folds float parts through:
/// the FNV-1a over the f64's canonical bits — the fold normalizes `-0.0` to `+0.0` and every NaN
/// payload to the one canonical NaN, so bit-structurally equal floats hash equal.
pub const BUILTIN_FLOAT_HASH: u32 = 54;
/// `char_hash(c: char) -> int` - the content hash the `hash` derive folds char parts through:
/// FNV-1a over the code point's UTF-8 bytes, so a char hashes like its one-character string
/// spells.
pub const BUILTIN_CHAR_HASH: u32 = 55;
/// `repl_park(words...)` — the control seam behind the REPL's resumed frame: no source spelling
/// reaches it, and the only call sites are the ones that module splices into the entry function's
/// body, so the table row exists for the splice's pool reference and the dispatcher's interception.
pub const BUILTIN_REPL_PARK: u32 = 56;
/// `str_fixed(x: float, digits: int) -> string` — the fixed-point float formatter: exactly `digits`
/// fraction digits, half-to-even rounding at the cut, `digits` riding 0..=767 (the widest exact f64
/// fraction; a negative width clamps to 0, a wider one clamps to 767). NaN and the infinities
/// answer `str_float`'s own spellings.
pub const BUILTIN_STR_FIXED: u32 = 57;
/// `char_of(code: int) -> char | none` — the codepoint→char reader: the one native seam the @std
/// json parser's `\u` decoding needs.
pub const BUILTIN_CHAR_OF: u32 = 58;
/// The generic `map_keys[k, v] (m: map[k, v]) -> list[k]`.
pub const BUILTIN_MAP_KEYS: u32 = 59;
/// `code_of(c: char) -> int` — the char→codepoint reader, `char_of`'s twin, the one seam the @std
/// json serializer's control-char escaping needs.
pub const BUILTIN_CODE_OF: u32 = 60;
/// `bytes_of(xs: list[byte]) -> bytes` — the byte string's construction seam, the `list[byte]`
/// bridge in: one word per byte element folds into one counted block.
pub const BUILTIN_BYTES_OF: u32 = 61;
/// `byte_list(b: bytes) -> list[byte]` — the bridge back out, one word per byte.
pub const BUILTIN_BYTE_LIST: u32 = 62;
/// `byte_at(b: bytes, i: int) -> byte | none` — the checked indexed read, the ok/none span shape
/// over the one-word byte payload.
pub const BUILTIN_BYTE_AT: u32 = 63;
/// `bytes_eq(a, b: bytes) -> bool` — the bytes collection's structural content compare: equal
/// payloads answer true, a different length or content false. The `==`/`!=` chain over bytes
/// operands dispatches here — the string's EqStr uop reads string objects, so the one Ref word's
/// shape needs its own body over the raw payload.
pub const BUILTIN_BYTES_EQ: u32 = 64;
/// `bytes_compare(a, b: bytes) -> int` — the bytes sort key the `ord` derive orders bytes parts
/// through: the lexicographic order over the payloads, a shorter prefix ordering first, answered as
/// one i64 word of -1, 0, or 1.
pub const BUILTIN_BYTES_COMPARE: u32 = 65;
/// `bytes_hash(b: bytes) -> int` — the content hash the `hash` derive folds bytes parts through:
/// FNV-1a over the payload bytes (the same pair the derive's fold mixes through), answered as one
/// i64 word. The value is an implementation detail; consistency with structural `==` is the
/// contract.
pub const BUILTIN_BYTES_HASH: u32 = 66;
/// `utf8_encode(s: string) -> bytes` — the string→bytes bridge: the string's own UTF-8 bytes become
/// one counted block, no re-encoding.
pub const BUILTIN_UTF8_ENCODE: u32 = 67;
/// `utf8_decode(b: bytes) -> string | utf8_err` — the bytes→string bridge: the payload validates as
/// UTF-8 on the way back, an invalid sequence answering the taxonomy's `utf8_err` tag with the
/// rejection message in the payload word.
pub const BUILTIN_UTF8_DECODE: u32 = 68;
/// `trap(msg: string) -> unit` — the bare trap primitive: the message-bearing deliberate trap a
/// `die` spelling's lowering routes to. Never returns and never prints — the trap carries the
/// message out to the drive, and what a die trap reports (the printing and the exit code) is the
/// host's own policy; the unit result shape only satisfies the call site's width check.
pub const BUILTIN_TRAP: u32 = 69;
/// `list_eq(a, b: list) -> bool` — the list collection's structural content compare: equal lengths
/// and pairwise-equal element words, every nested reference pair compared by its content. The
/// `==`/`!=` chain over list operands dispatches here — the string's EqStr uop reads string objects
/// alone, so the one Ref word's shape needs its own body.
pub const BUILTIN_LIST_EQ: u32 = 70;
/// `map_eq(a, b: map) -> bool` — the map collection's structural content compare: equal row counts
/// and, per key of one side, the same row in the other through the lookup face `map_get` serves
/// (string keys by content, every other word by identity), the value spans pairwise equal. The
/// `==`/`!=` chain over map operands dispatches here.
pub const BUILTIN_MAP_EQ: u32 = 71;
/// `byte_of(n: int) -> byte | none` — the byte width bridge: the in-range number's word under the
/// ok discriminant, none outside 0-255 (the checked-read convention). ds source cannot cross the
/// width (the atom sits outside the int conversion family), so the json face's byte readers build
/// their values through this.
pub const BUILTIN_BYTE_OF: u32 = 72;
/// `int_of_byte(b: byte) -> int` — the byte width bridge read back: the word's 0-255 number, for
/// the json_int construction.
pub const BUILTIN_INT_OF_BYTE: u32 = 73;
/// `u32_of(n: int) -> u32 | none` — the u32 width bridge: the in-range number's word under the ok
/// discriminant, none outside the u32 range. The width carries no literal or constructor, so the
/// json face's u32 readers build their values through this.
pub const BUILTIN_U32_OF: u32 = 74;
/// `int_of_u32(v: u32) -> int` — the u32 width bridge read back: the word's number, for the
/// json_int construction.
pub const BUILTIN_INT_OF_U32: u32 = 75;
/// `sqrt(x: float) -> float` — the float math family's square root: IEEE over the f64 word, total
/// by construction — a negative operand answers NaN, never a trap.
pub const BUILTIN_SQRT: u32 = 76;
/// `sin(x: float) -> float`.
pub const BUILTIN_SIN: u32 = 77;
/// `cos(x: float) -> float`.
pub const BUILTIN_COS: u32 = 78;
/// `atan2(y: float, x: float) -> float` — the two-argument arctangent, the quadrant read off both
/// signs.
pub const BUILTIN_ATAN2: u32 = 79;
/// `floor(x: float) -> float` — the greatest integer not above `x`, as a float.
pub const BUILTIN_FLOOR: u32 = 80;
/// `ceil(x: float) -> float` — the least integer not below `x`, as a float.
pub const BUILTIN_CEIL: u32 = 81;
/// `pow(base: float, exp: float) -> float`.
pub const BUILTIN_POW: u32 = 82;
/// The generic `abs[t] (x: t) -> t` over the numeric instances.
pub const BUILTIN_ABS: u32 = 83;
/// `contains(haystack, needle: string) -> bool` — the string search family's membership read: the
/// needle occurs in the haystack, the empty needle always occurring.
pub const BUILTIN_CONTAINS: u32 = 84;
/// `index_of(haystack, needle: string) -> int | none` — the checked search: the first occurrence's
/// char index under the ok discriminant (`substring`'s indexing), a miss the `none` member with the
/// inert unit word as payload.
pub const BUILTIN_INDEX_OF: u32 = 85;
/// `replace(haystack, needle, with: string) -> string` — every non-overlapping occurrence swapped
/// for `with` in one left-to-right pass, the inserted text never re-scanned; the empty needle
/// inserts `with` at every char boundary.
pub const BUILTIN_REPLACE: u32 = 86;
/// `chan_new(cap: int) -> chan[t]` — the bounded channel's construction seam: a fresh channel
/// object on the calling fiber's store, the one owning heap. Intercepted by the dispatcher like the
/// control natives (the channel step needs the scheduler); the body is the never-run placeholder.
pub const BUILTIN_CHAN_NEW: u32 = 87;
/// `chan_send(ch: mut chan[t], x: t) -> ` — deposit one payload word, parking on full. Intercepted;
/// the body never runs from a `CALL` site.
pub const BUILTIN_CHAN_SEND: u32 = 88;
/// `chan_recv(ch: mut chan[t]) -> t` — take one payload word off the buffer head, parking on empty.
/// Intercepted; the body never runs from a `CALL` site.
pub const BUILTIN_CHAN_RECV: u32 = 89;
/// The generic `sort[t] (xs: list[t], f: fn(t, t) -> int) -> list[t]`, its comparator staged from
/// the solved `ord` instance.
pub const BUILTIN_SORT: u32 = 90;
/// `list_set (xs: list, i: int, x: element words) -> unit` — the in-place element replacement at
/// the index, variadic at the element span (`list_push`'s staging shape), the store acquiring one
/// count per incoming `Ref` word and the replaced span releasing its own. The member tier's
/// `list[t].set` mut-window member routes here; an out-of-range index traps (the store carries no
/// miss answer).
pub const BUILTIN_LIST_SET: u32 = 91;
/// `list_remove (xs: list, i: int) -> unit` — the in-place element removal, the removed span
/// releasing one count per `Ref` word. The member tier's `list[t].remove` mut-window member routes
/// here; an out-of-range index traps.
pub const BUILTIN_LIST_REMOVE: u32 = 92;
/// `list_insert (xs: list, i: int, x: element words) -> unit` — the in-place splice before the
/// index (`index == len` appends), variadic at the element span. The member tier's `list[t].insert`
/// mut-window member routes here; an index outside `0..=len` traps.
pub const BUILTIN_LIST_INSERT: u32 = 93;
/// `list_clear (xs: list) -> unit` — the in-place emptying, every element word releasing its count.
/// The member tier's `list[t].clear` mut-window member routes here.
pub const BUILTIN_LIST_CLEAR: u32 = 94;
/// `map_remove (m: map, key: value) -> unit` — the in-place row removal, the removed key and value
/// span releasing their counts. The member tier's `map[k, v].remove` mut-window member routes here;
/// a missing key is a no-op.
pub const BUILTIN_MAP_REMOVE: u32 = 95;
/// `map_contains (m: map, key: value) -> bool` — the membership read, no value handed out. The
/// member tier's `map[k, v].contains` routes here.
pub const BUILTIN_MAP_CONTAINS: u32 = 96;
/// `map_entries (m: map) -> list` — the entry-order pair list: one key word then its value span per
/// row, one count per `Ref` word acquired for the fresh list's edge (the map keeps its own).
/// The member tier's `map[k, v].entries` routes here.
pub const BUILTIN_MAP_ENTRIES: u32 = 97;
/// `arena_new -> arena[t]` — the read-side aliasing container's construction seam: a fresh empty
/// arena object on the calling fiber's store. The element type never reaches the code (every arena
/// one Ref word).
pub const BUILTIN_ARENA_NEW: u32 = 98;
/// `arena_insert(a: mut arena[t], x: t) -> int` — store one element word, answer the packed
/// `(index, generation)` handle. The element stages as exactly one word (the one-word element
/// policy the call's staging enforces).
pub const BUILTIN_ARENA_INSERT: u32 = 99;
/// `arena_remove(a: mut arena[t], h: int) -> ` — retire the element behind a live handle, the slot
/// tombstoning onto the free list with a generation bump at reuse.
pub const BUILTIN_ARENA_REMOVE: u32 = 100;
/// `arena_at(a: mut arena[t], h: int) -> t` — the element word behind a live handle, a `Ref`
/// element answering as one retained count; a dangling handle traps deterministically.
pub const BUILTIN_ARENA_AT: u32 = 101;
/// `round(x: float) -> float` — the nearest integer, the half away from zero, as a float.
pub const BUILTIN_ROUND: u32 = 102;
/// `trunc(x: float) -> float` — the integer-valued float (the fraction discarded).
pub const BUILTIN_TRUNC: u32 = 103;
/// `ln(x: float) -> float` — the natural logarithm (`ln(0.0)` answers negative infinity, a negative
/// operand NaN).
pub const BUILTIN_LN: u32 = 104;
/// `exp(x: float) -> float` — the natural exponential.
pub const BUILTIN_EXP: u32 = 105;
/// `tan(x: float) -> float`.
pub const BUILTIN_TAN: u32 = 106;
/// `timeout(nanos: int, thunk: fn -> t) -> t` — the timer capability's race: the callable runs as
/// the calling fiber's racer while the caller parks on the deadline, the first to finish winning
/// (the two-word platform span delivered at the parked call site: the racer's one answer word under
/// the ok discriminant, the `timed_out` class tag with the inert unit word on expiry, so the call's
/// `t | timed_out` union handles the deadline through the ordinary error machinery). Intercepted at
/// `CALL` sites like the channel natives; the body is the never-run placeholder.
pub const BUILTIN_TIMEOUT: u32 = 107;
/// `chan_send_deadline(ch, nanos, x)`: the bounded channel's deadline send, the loser the io
/// taxonomy's own `timed_out` member.
pub const BUILTIN_CHAN_SEND_DEADLINE: u32 = 108;
/// `chan_recv_deadline(ch, nanos)`: the bounded channel's deadline recv, the loser the same member.
pub const BUILTIN_CHAN_RECV_DEADLINE: u32 = 109;
/// `list_contains (xs: list, x: element words) -> bool` — the list membership read, no element
/// handed out, variadic at the element span. The member tier's `list[t].contains` routes here; the
/// probe compares against every element through the structural content walk (`list_eq`'s judge).
pub const BUILTIN_LIST_CONTAINS: u32 = 110;
/// `str_char(c: char) -> string`: the scalar-to-text conversion, the UTF-8 encoding of the code
/// point (the interpolation hole and the `show` impl stage a char through it, the same seat
/// `str_int` gives the integer width).
pub const BUILTIN_STR_CHAR: u32 = 111;
/// `box_new(x: element words) -> box[t]` — the owning container's construction seam: one heap
/// object holding one value's whole word span inline, one owned count per `Ref` word. The value
/// stages variadic over its whole span.
pub const BUILTIN_BOX_NEW: u32 = 112;
/// `box_get(b: box[t]) -> t` — the copy-out read: the held value's whole word span handed out
/// under one fresh count per `Ref` word, the container keeping its own.
pub const BUILTIN_BOX_GET: u32 = 113;
/// `box_set(b: mut box[t], x: element words) -> ` — the in-place store: the held words'
/// `Ref` counts release, the incoming span stores under its own counts. The member tier's
/// `box[t].set` re-answers the receiver's word after the unit answer.
pub const BUILTIN_BOX_SET: u32 = 114;

// The comptime reflection module's rows.

/// `fields(t: tag) -> list[field record]`: one record per declared field (or enum case) of the
/// reflected type.
pub const COMPTIME_FIELDS: u32 = 0;
/// `type_name(t: tag) -> string`.
pub const COMPTIME_TYPE_NAME: u32 = 1;

// The platform world-call ABI: the fixed, densely indexed op run.
pub const OP_PRINTLN: u32 = 0;
pub const OP_PRINT: u32 = 1;
pub const OP_READ_FILE: u32 = 2;
pub const OP_WRITE_FILE: u32 = 3;
pub const OP_FILE_EXISTS: u32 = 4;
pub const OP_FS_LIST: u32 = 5;
pub const OP_FILE_SIZE: u32 = 6;
pub const OP_NET_CONNECT: u32 = 7;
pub const OP_NET_SEND: u32 = 8;
pub const OP_NET_RECV: u32 = 9;
pub const OP_NET_CLOSE: u32 = 10;
pub const OP_HTTP_GET: u32 = 11;
pub const OP_OPEN_FILE: u32 = 12;
pub const OP_READ_RAW: u32 = 13;
pub const OP_CLOSE_RAW: u32 = 14;
pub const OP_TIME: u32 = 15;
pub const OP_HTTPS_GET: u32 = 16;
pub const OP_READ_LINE: u32 = 17;
pub const OP_EPRINTLN: u32 = 18;
pub const OP_EPRINT: u32 = 19;
pub const OP_ARGS: u32 = 20;
pub const OP_ENV: u32 = 21;
pub const OP_PROC_RUN: u32 = 22;
pub const OP_EXIT: u32 = 23;
pub const OP_MONO: u32 = 24;
pub const OP_SLEEP: u32 = 25;
pub const OP_NET_LISTEN: u32 = 26;
pub const OP_NET_ACCEPT: u32 = 27;
pub const OP_NET_LISTENER_CLOSE: u32 = 28;
pub const OP_PROC_SPAWN: u32 = 29;
pub const OP_PROC_WRITE: u32 = 30;
pub const OP_PROC_READ: u32 = 31;
pub const OP_PROC_WAIT: u32 = 32;
pub const OP_PROC_CLOSE: u32 = 33;
pub const OP_PROC_CLOSE_IN: u32 = 34;
pub const OP_PROC_READ_ERR: u32 = 35;
pub const OP_PROC_TRY_WAIT: u32 = 36;
pub const OP_NET_LOCAL_ADDR: u32 = 37;
pub const OP_NET_ACCEPT_TLS: u32 = 38;
pub const OP_MKDIR: u32 = 39;
pub const OP_REMOVE: u32 = 40;
pub const OP_FS_SIZE: u32 = 41;
pub const OP_UTC_OFFSET: u32 = 42;

/// The platform world-call ABI's width: the fixed op table's length.
pub const OP_COUNT: u32 = 43;

/// Which surface a row belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeModule {
    /// The builtin core: the pure primitives every VM carries.
    Builtin,
    /// The platform handler: the world surface the host's platform serves under the source's
    /// declaration name.
    Platform,
    /// The comptime reflection module: served only by the comptime evaluator's fenced VMs, never
    /// installed at runtime.
    Comptime,
}

/// How a row reaches its call sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRouting {
    /// A source call the lowering resolves by name onto this row.
    Lowered,
    /// A row only the VM installs: the host splices the call site, or a declared extern binds the
    /// name at install.
    Installed,
}

/// The failure classes a fallible row's span answers on the failure side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClasses {
    /// The io classes, `PLATFORM_IO_OTHER` the floor.
    Io,
    /// The net classes, `PLATFORM_NET_OTHER` the floor.
    Net,
    /// The parse reader's position failure alone.
    Parse,
    /// The UTF-8 decode failure alone.
    Utf8,
    /// The deadline alone: the io taxonomy's `timed_out` class, which io-handling callers absorb by
    /// set containment.
    TimedOut,
}

/// A native's result shape on the caller's overlay words.
#[derive(Clone, Copy, Debug)]
pub enum NativeShape {
    /// One result word of this kind.
    Total(SlotType),
    /// The two-word platform span: a discriminant plus one payload word.
    Fallible {
        /// The success payload's word kind.
        payload: SlotType,
        /// The failure payload's word kind: the message `Ref` of an io or net class, or the parse
        /// reader's position `I64`.
        fail_payload: SlotType,
        /// The classes the failure side answers.
        family: FailureClasses,
    },
    /// The two-word ok/none span of a checked read: the discriminant (the success tag or the `none`
    /// tag) and the payload. Neither side is a failure.
    Checked,
    /// One result word whose kind follows the receiver's numeric builtin instance (`max`, `abs`).
    TotalByInstance,
    /// One result word whose kind follows the call's own result type (`reduce`, `chan_recv`,
    /// `arena_at`).
    TotalByResultType,
    /// The two-word platform span whose success payload's word kind follows the call's own result
    /// union's success member, over the same failure classes a `Fallible` span answers.
    FallibleByResultType {
        /// The failure payload's word kind: the inert unit word beside a bare class tag.
        fail_payload: SlotType,
        /// The classes the failure side answers.
        family: FailureClasses,
    },
    /// The tag-less multi-word total: the answer's words carry no discriminant, their kinds the
    /// call's own result type's.
    TaglessTotal,
}

impl NativeShape {
    /// The overlay words the answer spans, discriminant included: the static floor a call site
    /// sizes its area to.
    #[must_use]
    pub const fn result_words(self) -> u8 {
        match self {
            Self::Total(_) | Self::TotalByInstance | Self::TotalByResultType => 1,
            // the tag-less total's static floor is the resident body's two list refs; a wider fold
            // joining the shape widens its own floor
            Self::Fallible { .. }
            | Self::FallibleByResultType { .. }
            | Self::Checked
            | Self::TaglessTotal => 2,
        }
    }
}

/// One native row.
#[derive(Debug)]
pub struct Native {
    /// The call name: the source spelling the lowering resolves, or the extern name a host binds.
    pub name: &'static str,
    /// How the row reaches its call sites.
    pub routing: NativeRouting,
    /// The surface the row's pool `FuncRef` addresses.
    pub module: NativeModule,
    /// The row's slot in its module's table.
    pub index: u32,
    /// The parameter word kinds a call site checks its staged words against; `Raw` accepts every
    /// word kind.
    pub params: &'static [SlotType],
    /// Whether the staged span may carry more words than `params` names: the tail is one element's
    /// whole inline word span (`list_push`, `list_set`).
    pub variadic: bool,
    /// The result shape.
    pub shape: NativeShape,
}

/// A generic row: the one CALLG-dispatched body a call's instantiation selects, addressed by the
/// same pool `FuncRef` shape a monomorphic row's is. The call site stages the instantiated types as
/// evidence words after the real arguments.
#[derive(Debug)]
pub struct GenericNative {
    /// The call name.
    pub name: &'static str,
    /// The surface the row's pool `FuncRef` addresses.
    pub module: NativeModule,
    /// The row's slot in its module's table; the slot stays reserved, so a plain CALL never
    /// reaches the body.
    pub index: u32,
    /// The evidence-word count: one per constraint-list entry.
    pub evidence: u8,
    /// The real arguments' expected word kinds. A `Leading` row's `params` cover the trailing
    /// named words (the staged words before them are one element's whole span); an `Interior`
    /// row's cover the leading named word and the final named word (a fold's accumulator span sits
    /// between them).
    pub params: &'static [SlotType],
    /// How the staged words map onto `params`.
    pub staging: Staging,
    /// The result shape.
    pub shape: NativeShape,
    /// The builtin instances the one body implements: a receiver whose instance sits outside the
    /// set is a lowering error, never a runtime misdispatch.
    pub instances: &'static [BuiltinInstance],
}

/// Every native row: the builtin core's, the comptime module's, then the platform world ops'.
pub const NATIVES: &[Native] = &[
    Native {
        name: "str_int",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_STR_INT,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "str_float",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_STR_FLOAT,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "str_bool",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_STR_BOOL,
        params: &[SlotType::Bool],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the ok/none span re-spans into the call's own `string | none` union, the payload the
    // in-range slice's Ref (an out-of-range request answers the none member)
    Native {
        name: "substring",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_SUBSTRING,
        params: &[SlotType::Ref, SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    Native {
        name: "concat",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CONCAT,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "to_upper",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TO_UPPER,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "to_lower",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TO_LOWER,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "trim",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TRIM,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "parse_int",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_PARSE_INT,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::I64,
            family: FailureClasses::Parse,
        },
    },
    // the separator union's staged span (the tag word the checker's tag tests read, then the
    // region's one word per member kind: the text Ref beside the char's code point I64, the live
    // member's word carrying the value) beside the char member's own tag id — the dispatch
    // constant the body reads the span through, tag ids being per-compile facts no row can name
    Native {
        name: "split",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_SPLIT,
        params: &[
            SlotType::Ref,
            SlotType::Tag,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::I64,
        ],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the ok/none span re-spans into the call's own `char | none` union (the hit payload the
    // code point in the integer slot, exactly the word `char_of` mints); an out-of-range index
    // answers the none member
    Native {
        name: "char_at",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAR_AT,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    Native {
        name: "parse_float",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_PARSE_FLOAT,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::F64,
            fail_payload: SlotType::I64,
            family: FailureClasses::Parse,
        },
    },
    Native {
        name: "list_new",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_NEW,
        params: &[],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "fiber_resume",
        routing: NativeRouting::Installed,
        module: NativeModule::Builtin,
        index: BUILTIN_FIBER_RESUME,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::Raw),
    },
    Native {
        name: "nursery_join",
        routing: NativeRouting::Installed,
        module: NativeModule::Builtin,
        index: BUILTIN_NURSERY_JOIN,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "list_push",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_PUSH,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "map_new",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_NEW,
        params: &[],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "map_set",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_SET,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::I64),
    },
    // The nursery surface's internal seams: no ambient spelling declares these, so no call site
    // resolves them by name — the `nursery`/`spawn`/`cancel` emissions synthesize the pool
    // FuncRefs.
    Native {
        name: "nursery_open",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_NURSERY_OPEN,
        params: &[],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "nursery_spawn",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_NURSERY_SPAWN,
        params: &[SlotType::I64, SlotType::Fn],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "nursery_boundary",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_NURSERY_BOUNDARY,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    Native {
        name: "nursery_cancel",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_NURSERY_CANCEL,
        params: &[SlotType::Tag],
        variadic: true,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "join",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_JOIN,
        params: &[
            SlotType::Ref,
            SlotType::Tag,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::I64,
        ],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "total_compare",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TOTAL_COMPARE,
        params: &[SlotType::F64, SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "str_compare",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_STR_COMPARE,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "char_compare",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAR_COMPARE,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "byte_compare",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTE_COMPARE,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "str_hash",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_STR_HASH,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "float_hash",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_FLOAT_HASH,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "char_hash",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAR_HASH,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "repl_park",
        routing: NativeRouting::Installed,
        module: NativeModule::Builtin,
        index: BUILTIN_REPL_PARK,
        params: &[],
        variadic: true,
        shape: NativeShape::Total(SlotType::Raw),
    },
    // the fixed-point float formatter: the digits word clamps into the honest f64 fraction window,
    // the answer one string word
    Native {
        name: "str_fixed",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_STR_FIXED,
        params: &[SlotType::F64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the codepoint→char reader: the ok/none span the @std json parser's `\u` decoding extracts,
    // the payload kind the call site's own `char | none` union holds (the code point in the
    // integer slot)
    Native {
        name: "char_of",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAR_OF,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    Native {
        name: "code_of",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CODE_OF,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the byte string's construction seam: the `list[byte]` bridge in, one payload byte per
    // element word
    Native {
        name: "bytes_of",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTES_OF,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the bridge back out: one fresh list word per payload byte
    Native {
        name: "byte_list",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTE_LIST,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the checked indexed read, the ok/none span re-spanning into the call's own `byte | none`
    // union, the payload the in-range byte's shared integer word
    Native {
        name: "byte_at",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTE_AT,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    // the bytes collection's structural content compare: the `==`/`!=` chain over bytes operands
    // routes here through the synthesized CALL (no source spelling names it), the string's EqStr
    // uop reading string objects alone
    Native {
        name: "bytes_eq",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTES_EQ,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Bool),
    },
    // the bytes sort key the `ord` derive orders bytes parts through: lexicographic over the
    // payloads, a shorter prefix first
    Native {
        name: "bytes_compare",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTES_COMPARE,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the bytes content hash the `hash` derive folds bytes parts through: FNV-1a over the payload
    Native {
        name: "bytes_hash",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTES_HASH,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the string->bytes bridge: the string's own UTF-8 bytes
    Native {
        name: "utf8_encode",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_UTF8_ENCODE,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the bytes->string bridge: UTF-8 validated on the way back, invalid sequences answering the
    // taxonomy's utf8_err tag
    Native {
        name: "utf8_decode",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_UTF8_DECODE,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Utf8,
        },
    },
    // the bare trap primitive: the `die(msg)` spelling's seam — the unit shape only satisfies the
    // call site's width check, the native never answers
    Native {
        name: "trap",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TRAP,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the list collection's structural content compare: the `==`/`!=` chain over list operands (a
    // struct or tuple field, a union payload, `in` membership, the plain pair) routes here through
    // the synthesized CALL, the string's EqStr uop reading string objects alone
    Native {
        name: "list_eq",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_EQ,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Bool),
    },
    // the map collection's structural content compare: the `==`/`!=` chain over map operands
    // routes here through the synthesized CALL
    Native {
        name: "map_eq",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_EQ,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Bool),
    },
    // the byte width bridge in: the ok/none span re-spanning into the call's own `byte | none`
    // union, the payload the in-range byte's shared integer word
    Native {
        name: "byte_of",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BYTE_OF,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    // the byte width bridge read back: the 0-255 number for the json face's json_int construction
    Native {
        name: "int_of_byte",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_INT_OF_BYTE,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the u32 width bridge in: the ok/none span re-spanning into the call's own `u32 | none`
    // union, the payload the in-range number's word
    Native {
        name: "u32_of",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_U32_OF,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    // the u32 width bridge read back: the word's number for the json face's json_int construction
    Native {
        name: "int_of_u32",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_INT_OF_U32,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // The pure-builtin round's families: the float math answers one f64 word each, total by
    // construction (IEEE edge answers — a negative sqrt NaN, never a trap), and the string search
    // triple rides the string surface's own channels — the membership bit, the checked
    // first-occurrence char index (substring's indexing, so the answer composes with the checked
    // slice), the one-pass swap's fresh string.
    Native {
        name: "sqrt",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_SQRT,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "sin",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_SIN,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "cos",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_COS,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    // (y, x): the quadrant reads off both operands' signs
    Native {
        name: "atan2",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_ATAN2,
        params: &[SlotType::F64, SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "floor",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_FLOOR,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "ceil",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CEIL,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    // (base, exp): powf's IEEE edge answers
    Native {
        name: "pow",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_POW,
        params: &[SlotType::F64, SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    // the membership read, the empty needle always occurring
    Native {
        name: "contains",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CONTAINS,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Bool),
    },
    // the checked search: the ok/none span re-spans into the call's own `int | none` union, the
    // payload the first occurrence's char index; a miss answers the none member, the empty needle
    // index 0
    Native {
        name: "index_of",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_INDEX_OF,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Checked,
    },
    // the one-pass swap: every non-overlapping occurrence replaced left to right, the inserted
    // text never re-scanned
    Native {
        name: "replace",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_REPLACE,
        params: &[SlotType::Ref, SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the bounded channel's seams: the constructor answers the channel's Ref word, send the unit
    // word, and recv's answer kind is the call's own result type's — the `reduce` keying, the
    // payload's one word
    Native {
        name: "chan_new",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAN_NEW,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "chan_send",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAN_SEND,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "chan_recv",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAN_RECV,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::TotalByResultType,
    },
    // the in-place element replacement at the index: the element's whole word span stages variadic
    // (`list_push`'s shape), an out-of-range index trapping — the checked read keeps the `t |
    // none` miss answer because the caller can extract it
    Native {
        name: "list_set",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_SET,
        params: &[SlotType::Ref, SlotType::I64, SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the in-place element removal, the removed words releasing their counts; an out-of-range
    // index traps like `list_set`'s
    Native {
        name: "list_remove",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_REMOVE,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the in-place splice before the index, `index == len` appending; the element stages variadic
    // like `list_set`'s
    Native {
        name: "list_insert",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_INSERT,
        params: &[SlotType::Ref, SlotType::I64, SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the in-place emptying, every element word releasing its count
    Native {
        name: "list_clear",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_CLEAR,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the in-place row removal, a missing key a no-op
    Native {
        name: "map_remove",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_REMOVE,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the membership read, no value handed out
    Native {
        name: "map_contains",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_CONTAINS,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: false,
        shape: NativeShape::Total(SlotType::Bool),
    },
    // the entry-order pair list: one key word then the value span per row, the fresh list's edge
    // owning one count per `Ref` word
    Native {
        name: "map_entries",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_ENTRIES,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the read-side aliasing container's seams: the constructor answers the arena's Ref word,
    // insert the packed (index, generation) handle as one i64 word, remove the unit word, and at's
    // answer kind is the call's own result type's (the element's one word — the staging's one-word
    // element policy keeps it there)
    Native {
        name: "arena_new",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_ARENA_NEW,
        params: &[],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "arena_insert",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_ARENA_INSERT,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "arena_remove",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_ARENA_REMOVE,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "arena_at",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_ARENA_AT,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::TotalByResultType,
    },
    // the half rounds away from zero, the IEEE edges answering (a NaN or infinite operand
    // answering itself)
    Native {
        name: "round",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_ROUND,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    // the integer-valued float, the fraction discarded toward zero
    Native {
        name: "trunc",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TRUNC,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    // the natural logarithm (Rust's `ln` spelling: the flat `log` name stays the corpus files' own
    // extern), 0.0 to negative infinity, a negative operand to NaN
    Native {
        name: "ln",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LN,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    // the natural exponential
    Native {
        name: "exp",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_EXP,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "tan",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TAN,
        params: &[SlotType::F64],
        variadic: false,
        shape: NativeShape::Total(SlotType::F64),
    },
    // the timer race: the deadline's failure class the io taxonomy's own `timed_out` tag, the
    // success payload's kind the call's own result union's success member (the keying `reduce`
    // takes from the result type); the call site re-spans the two-word platform span into the `t |
    // timed_out` union so `?`, `.err`, and match handle the deadline through the ordinary error
    // machinery
    Native {
        name: "timeout",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_TIMEOUT,
        params: &[SlotType::I64, SlotType::Fn],
        variadic: false,
        shape: NativeShape::FallibleByResultType {
            fail_payload: SlotType::I64,
            family: FailureClasses::TimedOut,
        },
    },
    // the bounded channel's deadline twins (the timer race's loser shape over the channel's own
    // waiter queues): the winner the ok unit (the send) or the element's word under the ok
    // discriminant (the recv), the loser the io taxonomy's own `timed_out` tag over the inert unit
    // word; the call site re-spans the two-word platform span into the call's own union so `?`,
    // `.err`, and match handle the deadline through the ordinary error machinery
    Native {
        name: "chan_send_deadline",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAN_SEND_DEADLINE,
        params: &[SlotType::Ref, SlotType::I64, SlotType::Raw],
        variadic: true,
        shape: NativeShape::FallibleByResultType {
            fail_payload: SlotType::I64,
            family: FailureClasses::TimedOut,
        },
    },
    Native {
        name: "chan_recv_deadline",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_CHAN_RECV_DEADLINE,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::FallibleByResultType {
            fail_payload: SlotType::I64,
            family: FailureClasses::TimedOut,
        },
    },
    // the list membership read: the probe's whole element span against every element, the
    // structural content walk the `==` chain's list compare serves
    Native {
        name: "list_contains",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_CONTAINS,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::Bool),
    },
    // the scalar-to-text conversion over the char width: the code point's UTF-8 encoding, the
    // interpolation hole's and the `show` impl's seat for a char word
    Native {
        name: "str_char",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_STR_CHAR,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "fields",
        routing: NativeRouting::Lowered,
        module: NativeModule::Comptime,
        index: COMPTIME_FIELDS,
        params: &[SlotType::Tag],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "type_name",
        routing: NativeRouting::Lowered,
        module: NativeModule::Comptime,
        index: COMPTIME_TYPE_NAME,
        params: &[SlotType::Tag],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "println",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PRINTLN,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "print",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PRINT,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "read_file",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_READ_FILE,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "write_file",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_WRITE_FILE,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "file_exists",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_FILE_EXISTS,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        name: "fs_list",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_FS_LIST,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "file_size",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_FILE_SIZE,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "net_connect",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_CONNECT,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    Native {
        name: "net_send",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_SEND,
        params: &[SlotType::I64, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    Native {
        name: "net_recv",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_RECV,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    Native {
        name: "net_close",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_CLOSE,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "http_get",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_HTTP_GET,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    Native {
        name: "open_file",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_OPEN_FILE,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "read_raw",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_READ_RAW,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "close_raw",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_CLOSE_RAW,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "time",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_TIME,
        params: &[],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "https_get",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_HTTPS_GET,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    // the stdin read's checked span: one line under the ok member, the none member at end of input
    Native {
        name: "read_line",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_READ_LINE,
        params: &[],
        variadic: false,
        shape: NativeShape::Checked,
    },
    Native {
        name: "eprintln",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_EPRINTLN,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "eprint",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_EPRINT,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the host-staged process arguments, one list object
    Native {
        name: "args",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_ARGS,
        params: &[],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the environment read's checked span: the value under the ok member, the none member when
    // unset
    Native {
        name: "env",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_ENV,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Checked,
    },
    Native {
        name: "proc_run",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_RUN,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    // the `exit(code)` spelling's seam (the proc capability's deliberate-status row): the unit
    // shape only satisfies the call site's width check, the native never answers — the status
    // rides the trap out
    Native {
        name: "exit",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_EXIT,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the monotonic clock read: nanoseconds since a process-fixed origin, never moving backward
    Native {
        name: "mono",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_MONO,
        params: &[],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the parking sleep: the unit word on wake (the timer-only wait under an installed reactor,
    // the thread's own sleep otherwise)
    Native {
        name: "sleep",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_SLEEP,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the listener face: `net_listen` and `net_accept` answer the two-word span (the
    // bound/accepted handle's word on success, the net error class on failure), while the ambient
    // declares the total spellings over the deinit-carrying `net_listener` and `net_handle` — the
    // printing-halt pairing `net_connect` keeps, a failed bind or accept halting on the error line
    Native {
        name: "net_listen",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_LISTEN,
        params: &[SlotType::Ref, SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    Native {
        name: "net_accept",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_ACCEPT,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    // the raw idempotent-close seam (the `net_listener` deinit pairs with it, `net_close`'s
    // contract)
    Native {
        name: "net_listener_close",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_LISTENER_CLOSE,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the capture half: `proc_spawn` answers the two-word span (the piped child's handle word on
    // success, the io error class on failure), while the ambient declares the total spelling over
    // the deinit-carrying `proc_handle` — the printing-halt pairing `net_listen` keeps, a failed
    // spawn halting on the io error line
    Native {
        name: "proc_spawn",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_SPAWN,
        params: &[SlotType::Ref, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "proc_write",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_WRITE,
        params: &[SlotType::I64, SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "proc_read",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_READ,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    Native {
        name: "proc_wait",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_WAIT,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    // the raw idempotent-close seam (the `proc_handle` deinit pairs with it — kills and reaps a
    // still-running child)
    Native {
        name: "proc_close",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_CLOSE,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the stdin-close seam (the write row's own close): the child's end-of-input while the handle
    // stays live, total and idempotent like the close it narrows
    Native {
        name: "proc_close_in",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_CLOSE_IN,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the stderr read row, `proc_read`'s own discipline over the child's captured diagnostics
    Native {
        name: "proc_read_err",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_READ_ERR,
        params: &[SlotType::I64, SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    // the non-blocking poll's checked span: the already-reaped status under the ok member, the
    // none member while the child runs
    Native {
        name: "proc_try_wait",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_PROC_TRY_WAIT,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Checked,
    },
    // the bound listener's local address, the port-0 bind's report (the ds-source `local_addr`
    // handler over the raw seam) — a total string word like `args`
    Native {
        name: "net_local_addr",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_LOCAL_ADDR,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::Ref),
    },
    // the listener face's tls upgrade (the tls capability's own op): the accepted connection's
    // handle word on success, the net error class on failure — the ambient's total `accept_tls`
    // spelling over the deinit-carrying `net_handle`, the printing-halt pairing `net_accept` keeps
    // (a failed accept or handshake halting on the net error line)
    Native {
        name: "net_accept_tls",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_NET_ACCEPT_TLS,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Net,
        },
    },
    // the fsys path round (@std's ambient path face declares the spellings beside the file
    // readers): `mkdir` creates one directory, a single level — `already_exists` the branchable
    // outcome — and answers the unit word over the ok member
    Native {
        name: "mkdir",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_MKDIR,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    // `remove` removes one entry — a file or link over remove_file, a directory over the
    // empty-only remove_dir (the platform's not-empty kind folding into the io_other floor)
    Native {
        name: "remove",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_REMOVE,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    // `fs_size` answers the entry's byte count — the same read as `file_size`, the caller
    // branching on the missing path instead of halting
    Native {
        name: "fs_size",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_FS_SIZE,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            family: FailureClasses::Io,
        },
    },
    // the datetime face's one world touch (the time capability): the host's local UTC offset for
    // the timestamp, seconds east — total, the calendar math and formatting composing in @std ds
    // source over the word
    Native {
        name: "utc_offset",
        routing: NativeRouting::Lowered,
        module: NativeModule::Platform,
        index: OP_UTC_OFFSET,
        params: &[SlotType::I64],
        variadic: false,
        shape: NativeShape::Total(SlotType::I64),
    },
    // the owning container's seams: the constructor adopts the value's whole word span into one
    // fresh object (acquiring one count per `Ref` word, the staged words' own counts dying at the
    // call's window flush), the read hands the span out under fresh counts, and the store releases
    // the held span's counts before adopting the incoming one
    Native {
        name: "box_new",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BOX_NEW,
        params: &[SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "box_get",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BOX_GET,
        params: &[SlotType::Ref],
        variadic: false,
        shape: NativeShape::TotalByResultType,
    },
    Native {
        name: "box_set",
        routing: NativeRouting::Lowered,
        module: NativeModule::Builtin,
        index: BUILTIN_BOX_SET,
        params: &[SlotType::Ref, SlotType::Raw],
        variadic: true,
        shape: NativeShape::Total(SlotType::I64),
    },
];

/// The generic rows: the one body per call name a CALLG site dispatches.
pub const GENERIC_NATIVES: &[GenericNative] = &[
    GenericNative {
        name: "len",
        module: NativeModule::Builtin,
        index: BUILTIN_LEN,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::I64),
        instances: &[
            BuiltinInstance::Str,
            BuiltinInstance::Bytes,
            BuiltinInstance::List,
            BuiltinInstance::Map,
            BuiltinInstance::Arena,
        ],
    },
    GenericNative {
        name: "list_len",
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_LEN,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::I64),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "list_get",
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_GET,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::I64],
        staging: Staging::Fixed,
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "list_concat",
        module: NativeModule::Builtin,
        index: BUILTIN_LIST_CONCAT,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "map_len",
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_LEN,
        evidence: 2,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::I64),
        instances: &[BuiltinInstance::Map],
    },
    GenericNative {
        name: "map_get",
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_GET,
        evidence: 2,
        params: &[SlotType::Ref, SlotType::Raw],
        staging: Staging::Fixed,
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::Map],
    },
    // the ordered key face: the entry-order key list, one word per
    // key — the constraint list is [k, v] like map_len's, two
    // evidence words recording the receiver's map instance
    GenericNative {
        name: "map_keys",
        module: NativeModule::Builtin,
        index: BUILTIN_MAP_KEYS,
        evidence: 2,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::Map],
    },
    // the receiver is the element: the evidence word names the
    // element's instance (the constraint list's `t`), and the gate
    // is the full vocabulary — the one body prepends any element's
    // whole word span (the record instance covers the wider head),
    // and an element type no instance names refuses before staging
    GenericNative {
        name: "cons",
        module: NativeModule::Builtin,
        index: BUILTIN_CONS,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Leading,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[
            BuiltinInstance::Str,
            BuiltinInstance::List,
            BuiltinInstance::Map,
            BuiltinInstance::Int,
            BuiltinInstance::Float,
            BuiltinInstance::Record,
        ],
    },
    GenericNative {
        name: "tail",
        module: NativeModule::Builtin,
        index: BUILTIN_TAIL,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "collect",
        module: NativeModule::Builtin,
        index: BUILTIN_COLLECT,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "first",
        module: NativeModule::Builtin,
        index: BUILTIN_FIRST,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "max",
        module: NativeModule::Builtin,
        index: BUILTIN_MAX,
        evidence: 1,
        params: &[SlotType::Raw, SlotType::Raw],
        staging: Staging::Fixed,
        shape: NativeShape::TotalByInstance,
        instances: &[BuiltinInstance::Int, BuiltinInstance::Float],
    },
    // abs joins the int and float widths on one spelling: the solved
    // argument type names the numeric instance the evidence word
    // stages, the result word keying off it (`max`'s shape)
    GenericNative {
        name: "abs",
        module: NativeModule::Builtin,
        index: BUILTIN_ABS,
        evidence: 1,
        params: &[SlotType::Raw],
        staging: Staging::Fixed,
        shape: NativeShape::TotalByInstance,
        instances: &[BuiltinInstance::Int, BuiltinInstance::Float],
    },
    // the constraint list is [t, r] — two evidence words recording
    // the receiver's list instance, the map_len precedent — and the
    // lambda parameter stages its word into the callable-defaulted
    // `Fn` slot: the scalar FuncRef or a closure object's `Ref`
    // word (a borrow-window staging, the hosted call resolving either)
    GenericNative {
        name: "map",
        module: NativeModule::Builtin,
        index: BUILTIN_MAP,
        evidence: 2,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "filter",
        module: NativeModule::Builtin,
        index: BUILTIN_FILTER,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    // the result is the folded value's whole word span: its words
    // are the call's own result type's, a per-call-site fact (the
    // receiver's list instance says nothing about the element), and
    // the init's whole span stages between the list and the
    // callable — the interior span, the callable word riding at the
    // staged span's end
    GenericNative {
        name: "reduce",
        module: NativeModule::Builtin,
        index: BUILTIN_REDUCE,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Interior,
        shape: NativeShape::TotalByResultType,
        instances: &[BuiltinInstance::List],
    },
    // the pooled fork-join engine seam (the combinator's
    // user-facing spelling is retired from the ds-source package
    // until the fork-join admission round); the
    // constraint list is [t, r] like `map`'s — two evidence words
    // recording the receiver's list instance — and the callable is
    // the callable-defaulted `Fn` word like the hosted
    // combinators': the fork spawns the callable's own function per
    // element (a closure's body, the capture block staged per
    // spawn), its answer the whole span the spawned child's
    // completion carries, the width the callable's checker-proved
    // signature
    GenericNative {
        name: "fork_map",
        module: NativeModule::Builtin,
        index: BUILTIN_PAR_MAP,
        evidence: 2,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    // the result is the two-list tuple — the tag-less multi-word
    // total: two `Ref` words with no discriminant, their kinds the
    // call's own result type's; the predicate stages like filter's
    // (the callable-defaulted `Fn` word, one evidence word naming
    // the receiver's list instance)
    GenericNative {
        name: "partition",
        module: NativeModule::Builtin,
        index: BUILTIN_PARTITION,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::TaglessTotal,
        instances: &[BuiltinInstance::List],
    },
    // the pooled fold's fork-join engine seam;
    // reduce's interior staging over the fork: the
    // init's whole word span stages between the leading list `Ref`
    // and the trailing callable word, the fold chains one spawned
    // child per element answering the next accumulator whole (the
    // callable the callable-defaulted `Fn` word, its checker-proved
    // answer width the fold's own)
    GenericNative {
        name: "fork_reduce",
        module: NativeModule::Builtin,
        index: BUILTIN_PAR_REDUCE,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Interior,
        shape: NativeShape::TotalByResultType,
        instances: &[BuiltinInstance::List],
    },
    // the ord-routed list sort's engine seam: the source spelling
    // carries the list alone (the `ord` bound supplies the
    // comparator), so the free-generic dispatch never routes here —
    // the sort call site's recognition reads this entry for the
    // CALLG addressing and stages the solved instance's compare
    // callable into the `Fn` word beside the list `Ref`, the
    // receiver's list-instance evidence word trailing them like
    // filter's
    GenericNative {
        name: "sort",
        module: NativeModule::Builtin,
        index: BUILTIN_SORT,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
];

/// The rows of one surface, in reserved-index order.
pub fn rows(module: NativeModule) -> impl Iterator<Item = &'static Native> {
    NATIVES.iter().filter(move |row| row.module == module)
}

/// The row a call name addresses, if any.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static Native> {
    NATIVES
        .iter()
        .find(|row| row.routing == NativeRouting::Lowered && row.name == name)
}

/// The generic row a call name addresses, if any.
#[must_use]
pub fn lookup_generic(name: &str) -> Option<&'static GenericNative> {
    GENERIC_NATIVES.iter().find(|row| row.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every module's rows are one-to-one with their reserved indices, in index order, with unique
    /// names.
    #[test]
    fn the_rows_are_dense_and_unique() {
        for module in [
            NativeModule::Builtin,
            NativeModule::Platform,
            NativeModule::Comptime,
        ] {
            let table: Vec<&Native> = rows(module).collect();
            assert!(!table.is_empty(), "{module:?} carries rows");
            for pair in table.windows(2) {
                assert!(
                    pair[0].index < pair[1].index,
                    "{} and {} are not in index order",
                    pair[0].name,
                    pair[1].name
                );
            }
            for row in &table {
                assert_eq!(
                    table.iter().filter(|other| other.name == row.name).count(),
                    1,
                    "{} is tabled once",
                    row.name
                );
                // a lowered name resolves onto its row; an installed-only
                // one never does, so the lowering cannot route a splice
                match row.routing {
                    NativeRouting::Lowered => {
                        let found = lookup(row.name).expect("a lowered row resolves");
                        assert_eq!(found.index, row.index, "{}'s index", row.name);
                    },
                    NativeRouting::Installed => {
                        assert!(lookup(row.name).is_none(), "{} stays unrouted", row.name);
                    },
                }
            }
        }
        // the platform rows are the whole fixed run, densely indexed: an
        // op outside it would be unservable, a gap inside it unaddressable
        let platform: Vec<&Native> = rows(NativeModule::Platform).collect();
        assert_eq!(platform.len(), OP_COUNT as usize);
        for (at, row) in platform.iter().enumerate() {
            assert_eq!(row.index as usize, at, "{}'s op index", row.name);
        }
    }

    /// The generic rows address the builtin core alone, and no generic row shares its slot with a
    /// lowered row: the slot a CALLG site addresses must stay a trap.
    #[test]
    fn the_generic_rows_stay_on_reserved_builtin_slots() {
        for row in GENERIC_NATIVES {
            assert_eq!(row.module, NativeModule::Builtin, "{}'s surface", row.name);
            let found = lookup_generic(row.name).expect("a generic row resolves");
            assert_eq!(found.index, row.index, "{}'s index", row.name);
            assert!(
                rows(NativeModule::Builtin).all(|other| other.index != row.index),
                "{}'s slot carries a monomorphic row",
                row.name
            );
        }
    }

    /// A fallible row's failure word is its class's own: the io, net, and utf8 classes carry a
    /// message `Ref`, the parse reader and the deadline an inert number, and every fallible answer
    /// spans the two-word platform span.
    #[test]
    fn the_failure_words_match_their_classes() {
        for row in NATIVES {
            let (NativeShape::Fallible {
                family,
                fail_payload,
                ..
            }
            | NativeShape::FallibleByResultType {
                family,
                fail_payload,
            }) = row.shape
            else {
                continue;
            };
            let want = match family {
                FailureClasses::Io | FailureClasses::Net | FailureClasses::Utf8 => SlotType::Ref,
                FailureClasses::Parse | FailureClasses::TimedOut => SlotType::I64,
            };
            assert_eq!(fail_payload, want, "{}'s failure word", row.name);
            assert_eq!(row.shape.result_words(), 2, "{}'s span", row.name);
        }
    }
}
