//! The native vocabulary the lowering contract covers.
//!
//! `ds_vm::native` owns the builtin core's table and index assignment,
//! and `ds_vm::platform` the platform ABI's fixed world-op table — the
//! H5.5 layering after its M7 migration; this module is the lowerer's
//! mirror of both. The tables must stay aligned — the drift tests pin
//! the pairing, and the platform tag id below is pinned against the
//! VM's `PLATFORM_IO_OK`.

use ds_abi::{BuiltinInstance, SlotType, Staging};

/// The success discriminant of every fallible native's result span: the
/// top of the platform tag-id reservation both native modules share.
pub(crate) const PLATFORM_IO_OK: u8 = u8::MAX;
/// The clean-join discriminant of the nursery boundary seam's answer
/// span. The lowering never tests it (a clean join is the fall-through),
/// so it stays mirror-only and pinned by the drift test.
#[cfg_attr(not(test), expect(dead_code))]
pub const NURSERY_BOUNDARY_OK: u8 = u8::MAX - 6;
/// The first-failure discriminant of the nursery boundary seam's answer
/// span; word 1 carries the failing child's error payload.
pub const NURSERY_BOUNDARY_ERR: u8 = u8::MAX - 7;

/// The reserved FuncRef module name a runtime-resolved trait dispatch
/// site inside a generic impl body carries [H3]: the FuncRef's `func`
/// field names the calling frame's evidence slot, whose staged word
/// holds the serving member body's function-table index in the
/// enclosing module. `$` sits outside the identifier charset, so no
/// user package can collide.
pub(crate) const EVIDENCE_MODULE: &str = "$evidence";

/// The failure classes a classed fallible native answers: each platform
/// failure discriminant paired with the ambient failure-taxonomy member
/// name the call-site union carries — the error-class mapping (ruled
/// 2026-11-12, widened to the alias class sets 2026-11-21), each failure
/// landing on the member the name names. Mirrors the VM's tag
/// reservation, pinned by the drift test.
pub(crate) const NET_TIMEOUT: u8 = u8::MAX - 10;
pub(crate) const NET_REFUSED: u8 = u8::MAX - 11;
pub(crate) const UTF8_ERR: u8 = u8::MAX - 12;
pub(crate) const PARSE_ERR: u8 = u8::MAX - 3;
/// The predeclared `none` atom's tag id: the platform's own none
/// discriminant, so every module's `t | none` unions and bare `none`
/// values carry the same immediate — a checked read's re-span target
/// and a linked struct's optional field agree with no seeding. Pinned
/// against the VM's reservation by the drift test.
pub(crate) const NONE_TAG: u8 = u8::MAX - 4;
pub(crate) const IO_NOT_FOUND: u8 = u8::MAX - 13;
pub(crate) const IO_PERMISSION_DENIED: u8 = u8::MAX - 14;
pub(crate) const IO_ALREADY_EXISTS: u8 = u8::MAX - 15;
pub(crate) const IO_TIMED_OUT: u8 = u8::MAX - 16;
pub(crate) const IO_UNEXPECTED_EOF: u8 = u8::MAX - 17;
pub(crate) const IO_OTHER: u8 = u8::MAX - 18;
pub(crate) const NET_ADDR_IN_USE: u8 = u8::MAX - 19;
pub(crate) const NET_RESET: u8 = u8::MAX - 20;
pub(crate) const NET_OTHER: u8 = u8::MAX - 21;
/// The io classes: the `io_err` alias's member set, `io_other` the floor.
pub(crate) const IO_CLASSES: &[(u8, &str)] = &[
    (IO_NOT_FOUND, "not_found"),
    (IO_PERMISSION_DENIED, "permission_denied"),
    (IO_ALREADY_EXISTS, "already_exists"),
    (IO_TIMED_OUT, "timed_out"),
    (IO_UNEXPECTED_EOF, "unexpected_eof"),
    (IO_OTHER, "io_other"),
];
/// The net classes: the `net_err` alias's member set, `net_other` the
/// floor.
pub(crate) const NET_CLASSES: &[(u8, &str)] = &[
    (NET_TIMEOUT, "timeout"),
    (NET_REFUSED, "refused"),
    (NET_ADDR_IN_USE, "addr_in_use"),
    (NET_RESET, "reset"),
    (NET_OTHER, "net_other"),
];
/// The parse classes: `parse_err` alone, its member carrying the
/// position payload.
pub(crate) const PARSE_CLASSES: &[(u8, &str)] = &[(PARSE_ERR, "parse_err")];
/// The utf8 classes: the one bare tag.
pub(crate) const UTF8_CLASSES: &[(u8, &str)] = &[(UTF8_ERR, "utf8_err")];

/// The builtin core's reserved module name, as pool FuncRefs address it.
pub(crate) const BUILTIN_MODULE_NAME: &str = "builtin";

/// The default platform's name (H5, E1): what the world natives address
/// when the package declares no platform of its own — the `ds` CLI's
/// provider. The VM's platform table carries the same name; the drift
/// test pins the pair.
pub(crate) const DEFAULT_PLATFORM_NAME: &str = "std";

/// Which native surface a covered native's pool FuncRef addresses: the
/// VM's `builtin` core, or the platform handler the host installs behind
/// the source's platform declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeModule {
    /// The `builtin` core: the pure primitives every VM carries.
    Builtin,
    /// The platform handler: the world surface, served by the host's
    /// installed platform under the declaration's name.
    Platform,
    /// The comptime reflection module (G1, G2): served only by the
    /// comptime evaluator's fenced VMs, never installed at runtime —
    /// the checker refuses the builtins outside comptime functions, so
    /// a runtime module never carries a reference to it.
    Comptime,
}

/// The comptime reflection module's reserved name, matching the VM's
/// `ds_vm::comptime::COMPTIME_MODULE`.
pub(crate) const COMPTIME_MODULE_NAME: &str = "comptime";

/// A native's result shape on the caller's overlay words.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeShape {
    /// One result word of this kind.
    Total(SlotType),
    /// The two-word platform span: discriminant plus one payload word.
    /// The payload carries the success value; on failure the same word
    /// holds the class's failure value — the OS-error message `Ref` of an
    /// io or net class, or the parse reader's position `I64`. Failure
    /// answers one of the error-class discriminants in `classes`, each
    /// landing on the union member the ambient name names (the
    /// error-class mapping), so the call's checked union may flatten the
    /// failure taxonomy's whole member set. `err_prefix` heads the
    /// printed unhandled-failure line of a total spelling.
    Fallible {
        /// The success payload's word kind.
        payload: SlotType,
        /// The failure payload's word kind: the message `Ref` of an
        /// io/net class, or the position `I64` of the parse reader.
        fail_payload: SlotType,
        /// The unhandled-failure message prefix, e.g. `io error: `.
        err_prefix: &'static str,
        /// The error-class mapping: one discriminant per failure class,
        /// paired with the ambient member name it lands on.
        classes: &'static [(u8, &'static str)],
    },
    /// The two-word ok/none span of a checked collection read: word 0
    /// the discriminant (the platform IO_OK or NONE tag), word 1 the
    /// element payload or the rejected index. Neither side is a failure
    /// — the span re-spans into the call's own `t | none` union, so the
    /// element's word kind comes from the call site, not the table.
    Checked,
    /// One result word whose kind follows the receiver's numeric builtin
    /// instance (`max`): Int answers I64 and Float F64. The kind is a
    /// compile-time fact of the instance the evidence word stages, so
    /// the table records the keying, not a constant kind.
    TotalByInstance,
    /// One result word whose kind is the call's own checked result
    /// type's (`reduce`): the body folds the element words, so the
    /// answer's word kind is the folded type's — a per-call-site fact
    /// the table records as the keying, resolved from the call's result
    /// type like the instance keying resolves from the evidence.
    TotalByResultType,
    /// The tag-less multi-word total: the answer's N words carry no
    /// discriminant, their kinds the call's own checked result type's
    /// (`partition`'s two-list tuple, one `Ref` per list) — the
    /// degenerate sibling of [`NativeShape::Checked`]'s span (the span
    /// channel fronts its payload with a discriminant; the total's
    /// words ARE the answer). The resident body's static floor is two
    /// words; a wider fold joining later widens its own floor.
    TaglessTotal,
}

/// One covered native: its call name, module, reserved index, result
/// shape.
#[derive(Debug)]
pub(crate) struct Native {
    /// The ambient's call name.
    pub name: &'static str,
    /// The reserved module the native's FuncRef addresses.
    pub module: NativeModule,
    /// The native's index in its module's table.
    pub index: u32,
    /// The result shape.
    pub shape: NativeShape,
}

/// The natives the M3 lowering contract routes: the console and scalar
/// conversions, the pure string ops (the surfaces UFCS chains feed —
/// `s.trim()`, `s.to_upper()`, `"42".parse_int()` [F3]), the float sort
/// key `total_compare` (the `ord` derive's ruled spelling [B7], two
/// f64 words to one i64), the three primitive sort keys `str_compare`,
/// `char_compare`, and `byte_compare` (the `ord` derive's string, char,
/// and byte spellings, each two words to one i64), the three content
/// hashes `str_hash`, `float_hash`, and `char_hash` (the `hash`
/// derive's string, float, and char spellings, each one word to one
/// i64), the bytes collection's face (the `bytes_of` construction seam
/// and `byte_list` bridge over `list[byte]`, the checked `byte_at`
/// indexed read, the `bytes_eq` content compare the `==`/`!=` chain
/// dispatches, the `bytes_compare` sort key and `bytes_hash` content
/// hash the derives order and fold bytes parts through, and the two
/// UTF-8 bridges `utf8_encode`/`utf8_decode` — the decode answering the
/// taxonomy's `utf8_err` tag), the byte/u32 width bridges
/// `byte_of`/`int_of_byte`/`u32_of`/`int_of_u32` (the json face's scalar
/// readers over the widths ds source cannot cross, each checked in
/// answering a `t | none` span), the pure-builtin round's float math
/// family and string search triple (one f64 word per float op, total by
/// construction; `contains` the membership bit, the checked
/// char-indexed `index_of`, `replace` the one-pass swap's fresh
/// string), the list and
/// map construction and mutation surfaces (one heap-object `Ref` per
/// value, element and pair words opaque to the natives), the file-IO
/// surface including `fs_list`, and the TCP surface. The ambient's
/// generic reads and counts (`len`, `list_len`, `list_get`,
/// `list_concat`, `map_len`, `map_get`) stay out of the monomorphic
/// table on purpose: their ambient spellings are generic, so a
/// monomorphic-looking route would misroute receivers — their call
/// sites route through `GENERIC_NATIVES` and CALLG instead, carrying
/// the receiver's builtin instance as the evidence words [H3].
/// Name-routing the constructors and mutators is safe: every list and
/// map is the one Ref word whatever the element type, and
/// `list_push`'s element stages as one guarded word. The fiber
/// substrate's `fiber_resume` keeps its monomorphic row (the debug
/// switch, its answer word raw — an unreachable target's bounce
/// delivers the none tag, never an i64); the debug yield's row is
/// gone with the spelling (the 2026-11-17 ruling), the dev host
/// injecting suspension through its own capability face.
pub(crate) const NATIVES: &[Native] = &[
    Native {
        name: "println",
        module: NativeModule::Platform,
        index: 0,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "print",
        module: NativeModule::Platform,
        index: 1,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "str_int",
        module: NativeModule::Builtin,
        index: 2,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "str_float",
        module: NativeModule::Builtin,
        index: 3,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "str_bool",
        module: NativeModule::Builtin,
        index: 4,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        // B8's checked slice: the ok/none span re-spans into the call's
        // own `string | none` union, the payload the in-range slice's
        // Ref (an out-of-range request answers the none member)
        name: "substring",
        module: NativeModule::Builtin,
        index: 11,
        shape: NativeShape::Checked,
    },
    Native {
        name: "concat",
        module: NativeModule::Builtin,
        index: 12,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "to_upper",
        module: NativeModule::Builtin,
        index: 13,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "to_lower",
        module: NativeModule::Builtin,
        index: 14,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "trim",
        module: NativeModule::Builtin,
        index: 15,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "parse_int",
        module: NativeModule::Builtin,
        index: 16,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::I64,
            err_prefix: "parse error: at position ",
            classes: PARSE_CLASSES,
        },
    },
    Native {
        name: "parse_float",
        module: NativeModule::Builtin,
        index: 19,
        shape: NativeShape::Fallible {
            payload: SlotType::F64,
            fail_payload: SlotType::I64,
            err_prefix: "parse error: at position ",
            classes: PARSE_CLASSES,
        },
    },
    Native {
        name: "split",
        module: NativeModule::Builtin,
        index: 17,
        shape: NativeShape::Total(SlotType::Ref), // the list object
    },
    Native {
        name: "list_new",
        module: NativeModule::Builtin,
        index: 20,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "list_push",
        module: NativeModule::Builtin,
        index: 27,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "map_new",
        module: NativeModule::Builtin,
        index: 28,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "map_set",
        module: NativeModule::Builtin,
        index: 31,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        // B8's checked char read: the ok/none span re-spans into the
        // call's own `char | none` union (char rides the Ref, the hit
        // payload the one-scalar word `char_of` mints); an out-of-range
        // index answers the none member
        name: "char_at",
        module: NativeModule::Builtin,
        index: 18,
        shape: NativeShape::Checked,
    },
    Native {
        // the codepoint→char reader: the ok/none span the @std json
        // parser's `\u` decoding extracts, the payload kind the call
        // site's own `char | none` union holds (char rides the Ref)
        name: "char_of",
        module: NativeModule::Builtin,
        index: 58,
        shape: NativeShape::Checked,
    },
    Native {
        name: "code_of",
        module: NativeModule::Builtin,
        index: 60,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        // the byte string's construction seam: the `list[byte]` bridge
        // in, one payload byte per element word
        name: "bytes_of",
        module: NativeModule::Builtin,
        index: 61,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        // the bridge back out: one fresh list word per payload byte
        name: "byte_list",
        module: NativeModule::Builtin,
        index: 62,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        // the checked indexed read, the ok/none span re-spanning into
        // the call's own `byte | none` union, the payload the in-range
        // byte's shared integer word
        name: "byte_at",
        module: NativeModule::Builtin,
        index: 63,
        shape: NativeShape::Checked,
    },
    Native {
        // the bytes collection's structural content compare [B9]: the
        // `==`/`!=` chain over bytes operands routes here through the
        // synthesized CALL (no source spelling names it), the string's
        // EqStr uop reading string objects alone
        name: "bytes_eq",
        module: NativeModule::Builtin,
        index: 64,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        // the bytes sort key the `ord` derive orders bytes parts
        // through: lexicographic over the payloads, a shorter prefix
        // first
        name: "bytes_compare",
        module: NativeModule::Builtin,
        index: 65,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        // the bytes content hash the `hash` derive folds bytes parts
        // through: FNV-1a over the payload
        name: "bytes_hash",
        module: NativeModule::Builtin,
        index: 66,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        // the string->bytes bridge: the string's own UTF-8 bytes
        name: "utf8_encode",
        module: NativeModule::Builtin,
        index: 67,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        // the bytes->string bridge: UTF-8 validated on the way back,
        // invalid sequences answering the taxonomy's utf8_err tag
        name: "utf8_decode",
        module: NativeModule::Builtin,
        index: 68,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            err_prefix: "utf8 error: ",
            classes: UTF8_CLASSES,
        },
    },
    Native {
        // the list collection's structural content compare [B9]: the
        // `==`/`!=` chain over list operands (a struct or tuple field, a
        // union payload, `in` membership, the plain pair) routes here
        // through the synthesized CALL, the string's EqStr uop reading
        // string objects alone
        name: "list_eq",
        module: NativeModule::Builtin,
        index: 70,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        // the map collection's structural content compare [B9]: the
        // `==`/`!=` chain over map operands routes here through the
        // synthesized CALL
        name: "map_eq",
        module: NativeModule::Builtin,
        index: 71,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        // the byte width bridge in: the ok/none span re-spanning into
        // the call's own `byte | none` union, the payload the in-range
        // byte's shared integer word
        name: "byte_of",
        module: NativeModule::Builtin,
        index: 72,
        shape: NativeShape::Checked,
    },
    Native {
        // the byte width bridge read back: the 0-255 number for the
        // json face's json_int construction
        name: "int_of_byte",
        module: NativeModule::Builtin,
        index: 73,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        // the u32 width bridge in: the ok/none span re-spanning into
        // the call's own `u32 | none` union, the payload the in-range
        // number's word
        name: "u32_of",
        module: NativeModule::Builtin,
        index: 74,
        shape: NativeShape::Checked,
    },
    Native {
        // the u32 width bridge read back: the word's number for the
        // json face's json_int construction
        name: "int_of_u32",
        module: NativeModule::Builtin,
        index: 75,
        shape: NativeShape::Total(SlotType::I64),
    },
    // The pure-builtin round's families: the float math answers one
    // f64 word each, total by construction (IEEE edge answers — a
    // negative sqrt NaN, never a trap), and the string search triple
    // rides the string surface's own channels — the membership bit, the
    // checked first-occurrence char index (substring's indexing, so the
    // answer composes with the checked slice), the one-pass swap's
    // fresh string.
    Native {
        name: "sqrt",
        module: NativeModule::Builtin,
        index: 76,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "sin",
        module: NativeModule::Builtin,
        index: 77,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "cos",
        module: NativeModule::Builtin,
        index: 78,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        // (y, x): the quadrant reads off both operands' signs
        name: "atan2",
        module: NativeModule::Builtin,
        index: 79,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "floor",
        module: NativeModule::Builtin,
        index: 80,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        name: "ceil",
        module: NativeModule::Builtin,
        index: 81,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        // (base, exp): powf's IEEE edge answers
        name: "pow",
        module: NativeModule::Builtin,
        index: 82,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        // the float width alone; no int sibling exists
        name: "abs",
        module: NativeModule::Builtin,
        index: 83,
        shape: NativeShape::Total(SlotType::F64),
    },
    Native {
        // the membership read, the empty needle always occurring
        name: "contains",
        module: NativeModule::Builtin,
        index: 84,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        // the checked search: the ok/none span re-spans into the call's
        // own `int | none` union, the payload the first occurrence's
        // char index; a miss answers the none member, the empty needle
        // index 0
        name: "index_of",
        module: NativeModule::Builtin,
        index: 85,
        shape: NativeShape::Checked,
    },
    Native {
        // the one-pass swap: every non-overlapping occurrence replaced
        // left to right, the inserted text never re-scanned
        name: "replace",
        module: NativeModule::Builtin,
        index: 86,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        // the bounded channel's seams (the 2026-11-21 shared-state
        // ruling): the constructor answers the channel's Ref word, send
        // the unit word, and recv's answer kind is the call's own
        // result type's — the `reduce` keying, the payload's one word
        name: "chan_new",
        module: NativeModule::Builtin,
        index: 87,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "chan_send",
        module: NativeModule::Builtin,
        index: 88,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "chan_recv",
        module: NativeModule::Builtin,
        index: 89,
        shape: NativeShape::TotalByResultType,
    },
    Native {
        name: "join",
        module: NativeModule::Builtin,
        index: 45,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "total_compare",
        module: NativeModule::Builtin,
        index: 49,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "str_compare",
        module: NativeModule::Builtin,
        index: 50,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "char_compare",
        module: NativeModule::Builtin,
        index: 51,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "byte_compare",
        module: NativeModule::Builtin,
        index: 52,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "str_hash",
        module: NativeModule::Builtin,
        index: 53,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "float_hash",
        module: NativeModule::Builtin,
        index: 54,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "char_hash",
        module: NativeModule::Builtin,
        index: 55,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "fields",
        module: NativeModule::Comptime,
        index: 0,
        shape: NativeShape::Total(SlotType::Ref), // the record list
    },
    Native {
        name: "type_name",
        module: NativeModule::Comptime,
        index: 1,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "read_file",
        module: NativeModule::Platform,
        index: 2,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "write_file",
        module: NativeModule::Platform,
        index: 3,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "file_exists",
        module: NativeModule::Platform,
        index: 4,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        name: "fs_list",
        module: NativeModule::Platform,
        index: 5,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref, // the list object
            fail_payload: SlotType::Ref,
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "file_size",
        module: NativeModule::Platform,
        index: 6,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "net_connect",
        module: NativeModule::Platform,
        index: 7,
        // the net natives answer the failure classes: the classed shape
        // keeps the total ambient row on the shared printing-halt path
        // while a union row pairs by the class names
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    Native {
        name: "net_send",
        module: NativeModule::Platform,
        index: 8,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    Native {
        name: "net_recv",
        module: NativeModule::Platform,
        index: 9,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    Native {
        name: "net_close",
        module: NativeModule::Platform,
        index: 10,
        // the raw idempotent-close seam (the stdlib's `net_handle`
        // deinit pairs with it, `close_raw`'s contract)
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "http_get",
        module: NativeModule::Platform,
        index: 11,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    Native {
        name: "https_get",
        module: NativeModule::Platform,
        index: 16,
        // the tls capability's own op: no ambient spelling declares it
        // (the net face's http_get routes https URLs onto the tier), so
        // no call site resolves it by name — a package wanting the
        // explicit op declares its own extern. The entry sits here so
        // the drift test pins the mirror against the VM tables.
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    Native {
        name: "open_file",
        module: NativeModule::Platform,
        index: 12,
        // the success payload is the package-owned file handle's one
        // inline word (the struct's fd field)
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "read_raw",
        module: NativeModule::Platform,
        index: 13,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            fail_payload: SlotType::Ref,
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "close_raw",
        module: NativeModule::Platform,
        index: 14,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "time",
        module: NativeModule::Platform,
        index: 15,
        shape: NativeShape::Total(SlotType::I64), // epoch nanoseconds
    },
    Native {
        // the stdin read's checked span: one line under the ok member,
        // the none member at end of input
        name: "read_line",
        module: NativeModule::Platform,
        index: 17,
        shape: NativeShape::Checked,
    },
    Native {
        name: "eprintln",
        module: NativeModule::Platform,
        index: 18,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "eprint",
        module: NativeModule::Platform,
        index: 19,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        // the host-staged process arguments, one list object
        name: "args",
        module: NativeModule::Platform,
        index: 20,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        // the environment read's checked span: the value under the ok
        // member, the none member when unset
        name: "env",
        module: NativeModule::Platform,
        index: 21,
        shape: NativeShape::Checked,
    },
    Native {
        name: "proc_run",
        module: NativeModule::Platform,
        index: 22,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,      // the child's exit status
            fail_payload: SlotType::Ref, // the OS error message of an io class
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        // the `exit(code)` host member's seam (the CLI process surface):
        // the unit shape only satisfies the call site's width check,
        // the native never answers — the status rides the trap out
        name: "exit",
        module: NativeModule::Platform,
        index: 23,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        // the monotonic clock read: nanoseconds since a process-fixed
        // origin, never moving backward
        name: "mono",
        module: NativeModule::Platform,
        index: 24,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        // the parking sleep: the unit word on wake (the timer-only
        // wait under an installed reactor, the thread's own sleep
        // otherwise)
        name: "sleep",
        module: NativeModule::Platform,
        index: 25,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        // the listener face: `net_listen` and `net_accept` answer the
        // two-word span (the bound/accepted handle's word on success,
        // the net error class on failure), while the ambient declares
        // the total spellings over the deinit-carrying `net_listener`
        // and `net_handle` — the printing-halt pairing `net_connect`
        // keeps, a failed bind or accept halting on the error line
        name: "net_listen",
        module: NativeModule::Platform,
        index: 26,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    Native {
        name: "net_accept",
        module: NativeModule::Platform,
        index: 27,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    Native {
        // the raw idempotent-close seam (the `net_listener` deinit
        // pairs with it, `net_close`'s contract)
        name: "net_listener_close",
        module: NativeModule::Platform,
        index: 28,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        // the capture half: `proc_spawn` answers the two-word span
        // (the piped child's handle word on success, the io error
        // class on failure), while the ambient declares the total
        // spelling over the deinit-carrying `proc_handle` — the
        // printing-halt pairing `net_listen` keeps, a failed spawn
        // halting on the io error line
        name: "proc_spawn",
        module: NativeModule::Platform,
        index: 29,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "proc_write",
        module: NativeModule::Platform,
        index: 30,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,      // the inert unit word
            fail_payload: SlotType::Ref, // the OS error message of an io class
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "proc_read",
        module: NativeModule::Platform,
        index: 31,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,      // the captured output
            fail_payload: SlotType::Ref, // the OS error message of an io class
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        name: "proc_wait",
        module: NativeModule::Platform,
        index: 32,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,      // the child's exit status
            fail_payload: SlotType::Ref, // the OS error message of an io class
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        // the raw idempotent-close seam (the `proc_handle` deinit pairs
        // with it — kills and reaps a still-running child)
        name: "proc_close",
        module: NativeModule::Platform,
        index: 33,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        // the stdin-close seam (the write row's own close): the child's
        // end-of-input while the handle stays live, total and
        // idempotent like the close it narrows
        name: "proc_close_in",
        module: NativeModule::Platform,
        index: 34,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        // the stderr read row, `proc_read`'s own discipline over the
        // child's captured diagnostics
        name: "proc_read_err",
        module: NativeModule::Platform,
        index: 35,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,      // the captured diagnostics
            fail_payload: SlotType::Ref, // the OS error message of an io class
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
    },
    Native {
        // the non-blocking poll's checked span: the already-reaped
        // status under the ok member, the none member while the child
        // runs
        name: "proc_try_wait",
        module: NativeModule::Platform,
        index: 36,
        shape: NativeShape::Checked,
    },
    Native {
        // the bound listener's local address, the port-0 bind's report
        // (the ds-source `local_addr` handler over the raw seam) — a
        // total string word like `args`
        name: "net_local_addr",
        module: NativeModule::Platform,
        index: 37,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        // the listener face's tls upgrade (the tls capability's own
        // op): the accepted connection's handle word on success, the
        // net error class on failure — the ambient's total `accept_tls`
        // spelling over the deinit-carrying `net_handle`, the
        // printing-halt pairing `net_accept` keeps (a failed accept or
        // handshake halting on the net error line)
        name: "net_accept_tls",
        module: NativeModule::Platform,
        index: 38,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            fail_payload: SlotType::Ref,
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
    },
    // The nursery surface's internal seams [E2]: no ambient spelling
    // declares these, so no call site resolves them by name — the
    // `nursery`/`spawn`/`cancel` emissions synthesize the pool FuncRefs.
    // They sit here so the drift test pins the mirror against the VM
    // tables. The boundary seam's two-word ok/err span is the checked
    // shape (word 0 the discriminant, word 1 the payload); the nursery
    // emission reads it directly, so no shared machinery ever stages it.
    Native {
        name: "nursery_open",
        module: NativeModule::Builtin,
        index: 41,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "nursery_spawn",
        module: NativeModule::Builtin,
        index: 42,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "nursery_boundary",
        module: NativeModule::Builtin,
        index: 43,
        shape: NativeShape::Checked,
    },
    Native {
        name: "nursery_cancel",
        module: NativeModule::Builtin,
        index: 44,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        // the bare trap primitive (the 2026-11-17 die split): the
        // `die(msg)` spelling's seam — the unit shape only satisfies the
        // call site's width check, the native never answers
        name: "trap",
        module: NativeModule::Builtin,
        index: 69,
        shape: NativeShape::Total(SlotType::I64),
    },
];

/// The covered native a call name addresses, if any.
/// A generic builtin the lowering contract covers: the ONE body a
/// CALLG site dispatches, living in the VM's generic table and addressed
/// by the same pool FuncRef shape a monomorphic native's is. The call
/// site stages the instantiated types as evidence words after the real
/// arguments [H3].
#[derive(Debug)]
pub(crate) struct GenericNative {
    /// The ambient's call name.
    pub name: &'static str,
    /// The reserved module the generic's FuncRef addresses.
    pub module: NativeModule,
    /// The index in that module the FuncRef names.
    pub index: u32,
    /// The evidence-word count the callee's constraint list carries.
    pub evidence: u8,
    /// The real arguments' expected word kinds — the named words of the
    /// entry's `staging` discipline, the VM table's mirror, so a staged
    /// closure word can be checked against a callable-defaulted `Fn`
    /// parameter before dispatch. A `Leading` entry's `params` cover the
    /// trailing named words (`cons`: the head element's whole span rides
    /// the front, the list `Ref` is the trailing named word); an
    /// `Interior` entry's cover the leading named word plus the final
    /// named word (a fold's accumulator span sits between them).
    pub params: &'static [SlotType],
    /// How the staged words map onto `params` — the VM table's mirror,
    /// pinned by the drift test; the call site reads it to place a
    /// closure word against the dynamic span an `Interior` entry stages.
    pub staging: Staging,
    /// The result shape.
    pub shape: NativeShape,
    /// The builtin instances the one body implements: a receiver whose
    /// instance sits outside the set is a lowering error, never a
    /// runtime misdispatch. The receiver is the call's first argument —
    /// the container for the reads and counts, the element for `cons`,
    /// the operand for `max`.
    pub instances: &'static [BuiltinInstance],
}

/// The generic builtins the lowering contract routes [H3, phase 2]:
/// the ambient `len[t]` (reserved builtin index 10), whose one body
/// answers the string char count, the bytes payload count, the list
/// element count, or the map pair count on the evidence word, and the
/// container reads and counts — `list_len`,
/// `list_get`, `list_concat` (indices 21-23) and `map_len`, `map_get`
/// (29-30) — whose one bodies are element-agnostic: the evidence words
/// (one per constraint-list entry) name the receiver's container
/// instance. The container closes (32-37) join them: `cons`, `tail`,
/// `collect`, `first` are element-agnostic over the list
/// instance, and `max` dispatches on the numeric instance its evidence
/// word names, its result word keyed to that instance. The
/// lambda-taking combinators (38-40, and `partition` at 46) join them
/// over the list instance too: the staged lambda parameter is the
/// callable-defaulted `Fn` word (the scalar FuncRef or a closure
/// object's `Ref` word), and the VM body reenters the dispatcher per
/// element. The fork-join combinators (47 and 48) join them over the
/// list instance with the same callable-defaulted word: the fork
/// spawns the callable's own function per element (a closure's body,
/// the capture block staged per spawn), the callable's checker-proved
/// signature governing its answer's width. The table
/// mirrors the VM's generic table exactly (the drift tests pin the
/// pairing); adding an entry there without one here (or vice versa) is
/// a contract break.
pub(crate) const GENERIC_NATIVES: &[GenericNative] = &[
    GenericNative {
        name: "len",
        module: NativeModule::Builtin,
        index: 10,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::I64),
        instances: &[
            BuiltinInstance::Str,
            BuiltinInstance::Bytes,
            BuiltinInstance::List,
            BuiltinInstance::Map,
        ],
    },
    GenericNative {
        name: "list_len",
        module: NativeModule::Builtin,
        index: 21,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::I64),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "list_get",
        module: NativeModule::Builtin,
        index: 22,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::I64],
        staging: Staging::Fixed,
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "list_concat",
        module: NativeModule::Builtin,
        index: 23,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "map_len",
        module: NativeModule::Builtin,
        index: 29,
        evidence: 2,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::I64),
        instances: &[BuiltinInstance::Map],
    },
    GenericNative {
        name: "map_get",
        module: NativeModule::Builtin,
        index: 30,
        evidence: 2,
        params: &[SlotType::Ref, SlotType::Raw],
        staging: Staging::Fixed,
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::Map],
    },
    GenericNative {
        // the ordered key face: the entry-order key list, one word per
        // key — the constraint list is [k, v] like map_len's, two
        // evidence words recording the receiver's map instance
        name: "map_keys",
        module: NativeModule::Builtin,
        index: 59,
        evidence: 2,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref), // the list object
        instances: &[BuiltinInstance::Map],
    },
    GenericNative {
        // the receiver is the element: the evidence word names the
        // element's instance (the constraint list's `t`), and the gate
        // is the full vocabulary — the one body prepends any element's
        // whole word span (the record instance covers the wider head),
        // and an element type no instance names refuses before staging
        name: "cons",
        module: NativeModule::Builtin,
        index: 32,
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
        index: 34,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "collect",
        module: NativeModule::Builtin,
        index: 35,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "first",
        module: NativeModule::Builtin,
        index: 36,
        evidence: 1,
        params: &[SlotType::Ref],
        staging: Staging::Fixed,
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "max",
        module: NativeModule::Builtin,
        index: 37,
        evidence: 1,
        params: &[SlotType::Raw, SlotType::Raw],
        staging: Staging::Fixed,
        shape: NativeShape::TotalByInstance,
        instances: &[BuiltinInstance::Int, BuiltinInstance::Float],
    },
    GenericNative {
        // the constraint list is [t, r] — two evidence words recording
        // the receiver's list instance, the map_len precedent — and the
        // lambda parameter stages its word into the callable-defaulted
        // `Fn` slot: the scalar FuncRef or a closure object's `Ref`
        // word (a borrow-window staging, the hosted call resolving either)
        name: "map",
        module: NativeModule::Builtin,
        index: 38,
        evidence: 2,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "filter",
        module: NativeModule::Builtin,
        index: 39,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        // the result is the folded value's whole word span: its words
        // are the call's own result type's, a per-call-site fact (the
        // receiver's list instance says nothing about the element), and
        // the init's whole span stages between the list and the
        // callable — the interior span, the callable word riding at the
        // staged span's end
        name: "reduce",
        module: NativeModule::Builtin,
        index: 40,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Interior,
        shape: NativeShape::TotalByResultType,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        // the fork-join engine seam behind the @std `par_map` library
        // fn (its bodied member calls this seam; the combinator's
        // user-facing spelling lives in the ds-source package); the
        // constraint list is [t, r] like `map`'s — two evidence words
        // recording the receiver's list instance — and the callable is
        // the callable-defaulted `Fn` word like the hosted
        // combinators': the fork spawns the callable's own function per
        // element (a closure's body, the capture block staged per
        // spawn), its answer the whole span the spawned child's
        // completion carries, the width the callable's checker-proved
        // signature
        name: "fork_map",
        module: NativeModule::Builtin,
        index: 47,
        evidence: 2,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        // the result is the two-list tuple — the tag-less multi-word
        // total: two `Ref` words with no discriminant, their kinds the
        // call's own result type's; the predicate stages like filter's
        // (the callable-defaulted `Fn` word, one evidence word naming
        // the receiver's list instance)
        name: "partition",
        module: NativeModule::Builtin,
        index: 46,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Fixed,
        shape: NativeShape::TaglessTotal,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        // the fork-join fold's engine seam behind the @std `par_reduce`
        // library fn; reduce's interior staging over the fork: the
        // init's whole word span stages between the leading list `Ref`
        // and the trailing callable word, the fold chains one spawned
        // child per element answering the next accumulator whole (the
        // callable the callable-defaulted `Fn` word, its checker-proved
        // answer width the fold's own)
        name: "fork_reduce",
        module: NativeModule::Builtin,
        index: 48,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        staging: Staging::Interior,
        shape: NativeShape::TotalByResultType,
        instances: &[BuiltinInstance::List],
    },
];

/// Finds the generic builtin a call name addresses, if the contract
/// covers one.
pub(crate) fn lookup_generic(name: &str) -> Option<&'static GenericNative> {
    GENERIC_NATIVES.iter().find(|g| g.name == name)
}

pub(crate) fn lookup(name: &str) -> Option<&'static Native> {
    NATIVES.iter().find(|native| native.name == name)
}

#[cfg(test)]
mod drift {
    use super::*;

    /// The resident tag-less total's static result floor: partition
    /// answers two list `Ref`s. A wider fold joining the shape widens
    /// its own floor.
    const TAGLESS_TOTAL_FLOOR: usize = 2;

    /// The mirror stays aligned with the VM's two tables: every entry's
    /// name sits at its reserved index in its own surface's table — the
    /// builtin core's, or the default platform handler's — and the
    /// fallible and checked natives' result spans are two words wide.
    /// `len` stays deliberately absent from NATIVES (its ambient spelling
    /// is generic); it lives in the generic mirror `natives_match` pins
    /// below.
    #[test]
    fn natives_match_the_vm_native_tables() {
        let core = ds_vm::native::builtin_module();
        let platform = ds_vm::platform::platform_module(&ds_vm::platform::Platform::std());
        let comptime = ds_vm::comptime::comptime_module();
        for native in NATIVES {
            let table = match native.module {
                NativeModule::Builtin => &core,
                NativeModule::Platform => &platform,
                NativeModule::Comptime => &comptime,
            };
            assert!(
                (native.index as usize) < table.len(),
                "{} index {} is outside its table",
                native.name,
                native.index
            );
            let fns = &table[native.index as usize];
            assert_eq!(
                fns.name, native.name,
                "{} index {} in its surface's table drifted",
                native.name, native.index
            );
            let words = match native.shape {
                NativeShape::Total(_)
                | NativeShape::TotalByInstance
                | NativeShape::TotalByResultType => 1,
                NativeShape::Fallible { .. } | NativeShape::Checked => 2,
                // no monomorphic resident yet; the floor is the resident
                // generic body's two list refs
                NativeShape::TaglessTotal => TAGLESS_TOTAL_FLOOR,
            };
            assert_eq!(
                usize::from(fns.result_words),
                words,
                "{}'s result span width drifted",
                native.name
            );
        }
    }

    /// The generic mirror stays aligned with the VM's generic table: the
    /// FuncRef the lowerer mints resolves to the one body, whose evidence
    /// arity and result width agree with the lowering contract, and whose
    /// table slot stays reserved (a plain CALL must not reach a
    /// monomorphic instance).
    #[test]
    fn generic_natives_match_the_vm_generic_table() {
        let core = ds_vm::native::builtin_module();
        for g in GENERIC_NATIVES {
            assert_eq!(
                g.module,
                NativeModule::Builtin,
                "the generic mirror covers builtin-module callees only"
            );
            let fns = &core[g.index as usize];
            assert_eq!(
                fns.name, "reserved",
                "{}'s slot must stay reserved in the monomorphic table",
                g.name
            );
            let resolved = ds_vm::native::resolve_generic(ds_vm::native::BUILTIN_MODULE, g.index)
                .unwrap_or_else(|| panic!("{} resolves no generic body", g.name));
            assert_eq!(
                resolved.name, g.name,
                "{}'s generic body name drifted",
                g.name
            );
            assert_eq!(
                resolved.evidence, g.evidence,
                "{}'s evidence arity drifted",
                g.name
            );
            assert_eq!(
                resolved.params, g.params,
                "{}'s real-argument word kinds drifted",
                g.name
            );
            assert_eq!(
                resolved.staging, g.staging,
                "{}'s staged-span discipline drifted",
                g.name
            );
            let words = match g.shape {
                NativeShape::Total(_)
                | NativeShape::TotalByInstance
                | NativeShape::TotalByResultType => 1,
                NativeShape::Fallible { .. } | NativeShape::Checked => 2,
                // the resident tag-less total answers two list refs; a
                // wider fold joining the shape widens its own floor
                NativeShape::TaglessTotal => TAGLESS_TOTAL_FLOOR,
            };
            assert_eq!(
                usize::from(resolved.result_words),
                words,
                "{}'s result span width drifted",
                g.name
            );
        }
    }

    /// The module names, the success discriminant the lowering emits,
    /// and the tag-mint ceiling are the VM's reservations.
    #[test]
    fn module_names_and_success_tag_match_the_vm_reservation() {
        assert_eq!(
            EVIDENCE_MODULE,
            ds_vm::EVIDENCE_MODULE,
            "the runtime-evidence sentinel matches the VM's reservation"
        );
        // the module names the lowering emits are the VM's reservations
        assert_eq!(BUILTIN_MODULE_NAME, ds_vm::native::BUILTIN_MODULE);
        assert_eq!(DEFAULT_PLATFORM_NAME, ds_vm::platform::STD_NAME);
        assert_eq!(
            ds_vm::platform::Platform::std().capabilities().len(),
            6,
            "the default platform serves every capability"
        );
        assert_eq!(PLATFORM_IO_OK, ds_vm::native::PLATFORM_IO_OK);
        // the mint ceiling covers the whole countdown: the nursery
        // boundary discriminants and the claim-band pair sit above it
        const {
            assert!(
                crate::shape::FIRST_RESERVED_TAG < ds_vm::native::NURSERY_BOUNDARY_ERR,
                "the mint ceiling must cover every reserved tag"
            );
            assert!(crate::shape::CLAIM_BAND_CLEAR < ds_vm::native::NURSERY_BOUNDARY_ERR);
        }
        assert_eq!(NURSERY_BOUNDARY_OK, ds_vm::native::NURSERY_BOUNDARY_OK);
        assert_eq!(NURSERY_BOUNDARY_ERR, ds_vm::native::NURSERY_BOUNDARY_ERR);
        // the failure classes mirror the VM's tag reservation: every
        // discriminant the class tables carry, one assertion per family
        assert_eq!(NET_TIMEOUT, ds_vm::native::PLATFORM_NET_TIMEOUT);
        assert_eq!(NET_REFUSED, ds_vm::native::PLATFORM_NET_REFUSED);
        assert_eq!(UTF8_ERR, ds_vm::native::PLATFORM_UTF8_ERR);
        assert_eq!(PARSE_ERR, ds_vm::native::PLATFORM_PARSE_ERR);
        assert_eq!(NONE_TAG, ds_vm::native::PLATFORM_NONE);
        assert_eq!(IO_NOT_FOUND, ds_vm::native::PLATFORM_IO_NOT_FOUND);
        assert_eq!(
            IO_PERMISSION_DENIED,
            ds_vm::native::PLATFORM_IO_PERMISSION_DENIED
        );
        assert_eq!(IO_ALREADY_EXISTS, ds_vm::native::PLATFORM_IO_ALREADY_EXISTS);
        assert_eq!(IO_TIMED_OUT, ds_vm::native::PLATFORM_IO_TIMED_OUT);
        assert_eq!(IO_UNEXPECTED_EOF, ds_vm::native::PLATFORM_IO_UNEXPECTED_EOF);
        assert_eq!(IO_OTHER, ds_vm::native::PLATFORM_IO_OTHER);
        assert_eq!(NET_ADDR_IN_USE, ds_vm::native::PLATFORM_NET_ADDR_IN_USE);
        assert_eq!(NET_RESET, ds_vm::native::PLATFORM_NET_RESET);
        assert_eq!(NET_OTHER, ds_vm::native::PLATFORM_NET_OTHER);
    }
}
