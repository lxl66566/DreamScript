//! The builtin native vocabulary the lowering contract covers.
//!
//! `ds_vm::native` owns the reserved `builtin` module's table and
//! index assignment; this module is the lowerer's mirror of it. The
//! two must stay aligned — the drift tests pin the pairing, and the
//! platform tag id below is pinned against the VM's `BUILTIN_IO_OK`.

use ds_abi::SlotType;

/// The success discriminant of every fallible native's result span: the
/// top of the builtin module's platform tag-id reservation.
pub(crate) const PLATFORM_IO_OK: u8 = u8::MAX;

/// The reserved `builtin` module's name, as pool FuncRefs address it.
pub(crate) const BUILTIN_MODULE: &str = "builtin";

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
}

/// One covered native: its call name, reserved index, result shape.
#[derive(Debug)]
pub(crate) struct Native {
    /// The ambient's call name.
    pub name: &'static str,
    /// The reserved `builtin` module index.
    pub index: u32,
    /// The result shape.
    pub shape: NativeShape,
}

/// The natives the M3 lowering contract routes: the console and scalar
/// conversions, the file-IO surface, and the TCP surface. `fs_list` is
/// out: `list` values have no runtime shape yet (the lowered result span
/// would miscompile), so a call to it fails with the extern-callee error.
pub(crate) const NATIVES: &[Native] = &[
    Native {
        name: "println",
        index: 0,
        shape: NativeShape::Total(SlotType::I64), // the unit word
    },
    Native {
        name: "print",
        index: 1,
        shape: NativeShape::Total(SlotType::I64),
    },
    Native {
        name: "str_int",
        index: 2,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "str_float",
        index: 3,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "str_bool",
        index: 4,
        shape: NativeShape::Total(SlotType::Ref),
    },
    Native {
        name: "read_file",
        index: 5,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            err_prefix: "io error: ",
        },
    },
    Native {
        name: "write_file",
        index: 6,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "io error: ",
        },
    },
    Native {
        name: "file_exists",
        index: 7,
        shape: NativeShape::Total(SlotType::Bool),
    },
    Native {
        name: "file_size",
        index: 9,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "io error: ",
        },
    },
    Native {
        name: "net_connect",
        index: 20,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "net_send",
        index: 21,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "net_recv",
        index: 22,
        shape: NativeShape::Fallible {
            payload: SlotType::Ref,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "net_close",
        index: 23,
        shape: NativeShape::Fallible {
            payload: SlotType::I64,
            err_prefix: "net error: ",
        },
    },
    Native {
        name: "http_get",
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

    /// The mirror stays aligned with the VM's builtin table: every entry's
    /// name sits at its reserved index, and the fallible natives'
    /// result spans are two words wide. `fs_list` stays deliberately
    /// absent (its list payload has no runtime shape); `reserved` holds
    /// the string natives' block, which no lowering routes yet.
    #[test]
    fn natives_match_the_vm_builtin_table() {
        let table = ds_vm::native::builtin_module();
        for native in NATIVES {
            let fns = &table[native.index as usize];
            assert_eq!(fns.name, native.name, "index {} drifted", native.index);
            let words = match native.shape {
                NativeShape::Total(_) => 1,
                NativeShape::Fallible { .. } => 2,
            };
            assert_eq!(
                usize::from(fns.result_words),
                words,
                "{}'s result span width drifted",
                native.name
            );
        }
    }

    /// The success discriminant the lowering tests against is the VM's
    /// reserved platform tag.
    #[test]
    fn success_tag_matches_the_vm_reservation() {
        assert_eq!(PLATFORM_IO_OK, ds_vm::native::BUILTIN_IO_OK);
        assert_eq!(BUILTIN_MODULE, ds_vm::native::BUILTIN_MODULE);
    }
}
