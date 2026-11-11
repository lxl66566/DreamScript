//! The native vocabulary the lowering contract covers.
//!
//! `ds_vm::native` owns the two reserved native modules' tables and
//! index assignment (the `builtin` core and the `std` platform module,
//! the H5.5 layering); this module is the lowerer's mirror of both. The
//! tables must stay aligned — the drift tests pin the pairing, and the
//! platform tag id below is pinned against the VM's `PLATFORM_IO_OK`.

use ds_abi::{BuiltinInstance, SlotType};

/// The success discriminant of every fallible native's result span: the
/// top of the platform tag-id reservation both native modules share.
pub(crate) const PLATFORM_IO_OK: u8 = u8::MAX;
/// The clean-join discriminant of the nursery boundary seam's answer
/// span. The lowering never tests it (a clean join is the fall-through),
/// so it stays mirror-only and pinned by the drift test.
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

/// Which reserved native module a covered native's pool FuncRef
/// addresses: the VM's `builtin` core or the `std` platform module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeModule {
    /// The `builtin` core: the pure primitives every VM carries.
    Builtin,
    /// The `std` platform native module: the world surface, installed by
    /// the host that provides the platform.
    StdPlatform,
}

impl NativeModule {
    /// The module's name, as pool FuncRefs address it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::StdPlatform => "std",
        }
    }
}

/// A native's result shape on the caller's overlay words.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeShape {
    /// One result word of this kind.
    Total(SlotType),
    /// The two-word platform span: discriminant plus one payload word.
    /// The payload carries the success value; on failure the same word
    /// holds the OS-error message `Ref`. `err_prefix` heads the printed
    /// unhandled-failure line.
    Fallible {
        /// The success payload's word kind.
        payload: SlotType,
        /// The unhandled-failure message prefix, e.g. `io error: `.
        err_prefix: &'static str,
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
/// `s.trim()`, `s.to_upper()`, `"42".parse_int()` [F3]), the list and
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
/// `list_push`'s element stages as one guarded word.
pub(crate) const NATIVES: &[Native] = &[
    Native {
        name: "println",
        module: NativeModule::StdPlatform,
        index: 0,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "print",
        module: NativeModule::StdPlatform,
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
        name: "substring",
        module: NativeModule::Builtin,
        index: 11,
        shape: NativeShape::Total(SlotType::Ref),
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
            err_prefix: "parse error: ",
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
        name: "char_at",
        module: NativeModule::Builtin,
        index: 18,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "read_file",
        module: NativeModule::StdPlatform,
        index: 5,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            err_prefix: "io error: ",
        },
    },
    Native {
        name: "write_file",
        module: NativeModule::StdPlatform,
        index: 6,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "io error: ",
        },
    },
    Native {
        name: "file_exists",
        module: NativeModule::StdPlatform,
        index: 7,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        name: "fs_list",
        module: NativeModule::StdPlatform,
        index: 8,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref, // the list object
            err_prefix: "io error: ",
        },
    },
    Native {
        name: "file_size",
        module: NativeModule::StdPlatform,
        index: 9,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "io error: ",
        },
    },
    Native {
        name: "net_connect",
        module: NativeModule::StdPlatform,
        index: 20,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "net_send",
        module: NativeModule::StdPlatform,
        index: 21,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "net_recv",
        module: NativeModule::StdPlatform,
        index: 22,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "net_close",
        module: NativeModule::StdPlatform,
        index: 23,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "http_get",
        module: NativeModule::StdPlatform,
        index: 24,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            err_prefix: "net error: ",
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
    /// The real arguments' expected word kinds — the VM table's mirror,
    /// so a staged closure word can be checked against a callable-defaulted
    /// `Fn` parameter before dispatch.
    pub params: &'static [SlotType],
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
/// answers the string char count or the list element count on the
/// evidence word, and the container reads and counts — `list_len`,
/// `list_get`, `list_concat` (indices 21-23) and `map_len`, `map_get`
/// (29-30) — whose one bodies are element-agnostic: the evidence words
/// (one per constraint-list entry) name the receiver's container
/// instance. The container closes (32-37) join them: `cons`, `tail`,
/// `collect`, `head`, `first` are element-agnostic over the list
/// instance, and `max` dispatches on the numeric instance its evidence
/// word names, its result word keyed to that instance. The
/// lambda-taking combinators (38-40) join them over the list instance
/// too: the staged lambda parameter is the callable-defaulted `Fn` word
/// (the scalar FuncRef or a closure object's `Ref` word), and the VM
/// body reenters the dispatcher per element. The table
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
        shape: NativeShape::Total(SlotType::I64),
        instances: &[BuiltinInstance::Str, BuiltinInstance::List],
    },
    GenericNative {
        name: "list_len",
        module: NativeModule::Builtin,
        index: 21,
        evidence: 1,
        params: &[SlotType::Ref],
        shape: NativeShape::Total(SlotType::I64),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "list_get",
        module: NativeModule::Builtin,
        index: 22,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::I64],
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "list_concat",
        module: NativeModule::Builtin,
        index: 23,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Ref],
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "map_len",
        module: NativeModule::Builtin,
        index: 29,
        evidence: 2,
        params: &[SlotType::Ref],
        shape: NativeShape::Total(SlotType::I64),
        instances: &[BuiltinInstance::Map],
    },
    GenericNative {
        name: "map_get",
        module: NativeModule::Builtin,
        index: 30,
        evidence: 2,
        params: &[SlotType::Ref, SlotType::Raw],
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::Map],
    },
    GenericNative {
        // the receiver is the element: the evidence word names the
        // element's instance (the constraint list's `t`), and the gate
        // is the full vocabulary — the body prepends any single word,
        // and an element type no instance names refuses before staging
        name: "cons",
        module: NativeModule::Builtin,
        index: 32,
        evidence: 1,
        params: &[SlotType::Raw, SlotType::Ref],
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[
            BuiltinInstance::Str,
            BuiltinInstance::List,
            BuiltinInstance::Map,
            BuiltinInstance::Int,
            BuiltinInstance::Float,
        ],
    },
    GenericNative {
        name: "head",
        module: NativeModule::Builtin,
        index: 33,
        evidence: 1,
        params: &[SlotType::Ref],
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "tail",
        module: NativeModule::Builtin,
        index: 34,
        evidence: 1,
        params: &[SlotType::Ref],
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "collect",
        module: NativeModule::Builtin,
        index: 35,
        evidence: 1,
        params: &[SlotType::Ref],
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "first",
        module: NativeModule::Builtin,
        index: 36,
        evidence: 1,
        params: &[SlotType::Ref],
        shape: NativeShape::Checked,
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "max",
        module: NativeModule::Builtin,
        index: 37,
        evidence: 1,
        params: &[SlotType::Raw, SlotType::Raw],
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
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        name: "filter",
        module: NativeModule::Builtin,
        index: 39,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Fn],
        shape: NativeShape::Total(SlotType::Ref),
        instances: &[BuiltinInstance::List],
    },
    GenericNative {
        // the result is the folded element word: its kind is the call's
        // own result type's, a per-call-site fact (the receiver's list
        // instance says nothing about the element)
        name: "reduce",
        module: NativeModule::Builtin,
        index: 40,
        evidence: 1,
        params: &[SlotType::Ref, SlotType::Raw, SlotType::Fn],
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

    /// The mirror stays aligned with the VM's two tables: every entry's
    /// name sits at its reserved index in its own module's table, and the
    /// fallible and checked natives' result spans are two words wide.
    /// `len` stays deliberately absent from NATIVES (its ambient spelling
    /// is generic); it lives in the generic mirror `natives_match` pins
    /// below.
    #[test]
    fn natives_match_the_vm_native_tables() {
        let core = ds_vm::native::builtin_module();
        let platform = ds_vm::native::std_platform_module();
        for native in NATIVES {
            let table = match native.module {
                NativeModule::Builtin => &core,
                NativeModule::StdPlatform => &platform,
            };
            let fns = &table[native.index as usize];
            assert_eq!(
                fns.name,
                native.name,
                "{} index {} in `{}` drifted",
                native.name,
                native.index,
                native.module.name()
            );
            let words = match native.shape {
                NativeShape::Total(_)
                | NativeShape::TotalByInstance
                | NativeShape::TotalByResultType => 1,
                NativeShape::Fallible { .. } | NativeShape::Checked => 2,
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
            let words = match g.shape {
                NativeShape::Total(_)
                | NativeShape::TotalByInstance
                | NativeShape::TotalByResultType => 1,
                NativeShape::Fallible { .. } | NativeShape::Checked => 2,
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
        assert_eq!(NativeModule::Builtin.name(), ds_vm::native::BUILTIN_MODULE);
        assert_eq!(
            NativeModule::StdPlatform.name(),
            ds_vm::native::STD_PLATFORM_MODULE
        );
        assert_eq!(PLATFORM_IO_OK, ds_vm::native::PLATFORM_IO_OK);
        assert_eq!(
            crate::shape::FIRST_RESERVED_TAG,
            ds_vm::native::PLATFORM_CANCELLED
        );
        assert_eq!(NURSERY_BOUNDARY_OK, ds_vm::native::NURSERY_BOUNDARY_OK);
        assert_eq!(NURSERY_BOUNDARY_ERR, ds_vm::native::NURSERY_BOUNDARY_ERR);
    }
}
