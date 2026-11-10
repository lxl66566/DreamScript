//! The native vocabulary the lowering contract covers.
//!
//! `ds_vm::native` owns the two reserved native modules' tables and
//! index assignment (the `builtin` core and the `std` platform module,
//! the H5.5 layering); this module is the lowerer's mirror of both. The
//! tables must stay aligned — the drift tests pin the pairing, and the
//! platform tag id below is pinned against the VM's `PLATFORM_IO_OK`.

use ds_abi::SlotType;

/// The success discriminant of every fallible native's result span: the
/// top of the platform tag-id reservation both native modules share.
pub(crate) const PLATFORM_IO_OK: u8 = u8::MAX;

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
/// `s.trim()`, `s.to_upper()`, `"42".parse_int()` [F3]), the list
/// surface (one heap-object `Ref` per value, element words opaque to
/// the natives), the file-IO surface including `fs_list`, and the TCP
/// surface. `len` stays out on purpose: its ambient spelling is generic
/// (`len[t]`) — the lowerer routes by name, so a monomorphic-looking
/// route would misroute non-string receivers; it waits for the M6
/// CALL_GENERIC seam like every generic callee. Name-routing the list
/// surface is safe: every list is the one Ref word whatever the element
/// type, and `list_get`'s element kind comes from the call's checked
/// type, not the table.
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
        name: "list_len",
        module: NativeModule::Builtin,
        index: 21,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "list_get",
        module: NativeModule::Builtin,
        index: 22,
        shape: NativeShape::Checked,
    },
    Native {
        name: "list_concat",
        module: NativeModule::Builtin,
        index: 23,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "list_push",
        module: NativeModule::Builtin,
        index: 27,
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
];

/// The covered native a call name addresses, if any.
pub(crate) fn lookup(name: &str) -> Option<&'static Native> {
    NATIVES.iter().find(|native| native.name == name)
}

#[cfg(test)]
mod drift {
    use super::*;

    /// The mirror stays aligned with the VM's two tables: every entry's
    /// name sits at its reserved index in its own module's table, and the
    /// fallible and checked natives' result spans are two words wide.
    /// `len` stays deliberately absent (its ambient spelling is generic).
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
                NativeShape::Total(_) => 1,
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

    /// The module names and the success discriminant the lowering emits
    /// are the VM's reservations.
    #[test]
    fn module_names_and_success_tag_match_the_vm_reservation() {
        assert_eq!(NativeModule::Builtin.name(), ds_vm::native::BUILTIN_MODULE);
        assert_eq!(
            NativeModule::StdPlatform.name(),
            ds_vm::native::STD_PLATFORM_MODULE
        );
        assert_eq!(PLATFORM_IO_OK, ds_vm::native::PLATFORM_IO_OK);
    }
}
