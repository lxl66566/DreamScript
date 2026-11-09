//! Native (Rust-implemented) functions: the builtin mechanism.
//!
//! Builtins are functions in the source (`println(...)`, `str(x)`), so the
//! tier-1 mechanism routes them as functions, not uops: every VM installs
//! one reserved native module ([`BUILTIN_MODULE`]) into its registry, and a
//! pool `FuncRef { module: "builtin", func: i }` resolves through the same
//! path as any uop callee. A `CALL` to a native never pushes a frame: the
//! native reads the caller's staged overlay words and its result overlays
//! argument slot 0 exactly where a `RET` would put it. This was chosen over
//! a dedicated uop per builtin because `CALL` is already the ISA's
//! function-call mechanism — a uop per builtin would grow the locked set
//! for every stdlib addition, while the module route adds builtins with no
//! opcode growth, keeps arity/typing checks in one place, and lets
//! higher-milestone builtins (list/map/reduce) land as table entries.
//!
//! Function indices are contract: pool entries name them positionally.
//!
//! | index | name          | signature                       | returns |
//! |-------|---------------|---------------------------------|---------|
//! | 0     | `println`     | `(msg: string)`                 | unit |
//! | 1     | `print`       | `(msg: string)`                 | unit |
//! | 2     | `str_int`     | `(x: int)`                      | the string form of `x` |
//! | 3     | `str_float`   | `(x: float)`                    | the string form of `x` |
//! | 4     | `str_bool`    | `(x: bool)`                     | the string form of `x` |
//! | 5     | `read_file`   | `(path: string)`                | the file's text, or `io_err` |
//! | 6     | `write_file`  | `(path, contents: string)`      | unit, or `io_err` |
//! | 7     | `file_exists` | `(path: string)`                | whether `path` names an entry |
//! | 8     | `fs_list`     | `(dir: string)`                 | the sorted entry names, or `io_err` |
//! | 9     | `file_size`   | `(path: string)`                | the byte length, or `io_err` |
//! | 10-19 | —             | reserved                        | the string natives' block |
//! | 20    | `net_connect` | `(host: string, port: int)`     | the connection handle, or `net_err` |
//! | 21    | `net_send`    | `(conn: int, data: string)`     | unit, or `net_err` |
//! | 22    | `net_recv`    | `(conn, max_bytes: int)`        | the bytes read (empty on close), or `net_err` |
//! | 23    | `net_close`   | `(conn: int)`                   | unit, or `net_err` |
//! | 24    | `http_get`    | `(url: string)`                 | the response body, or `net_err` |
//!
//! The surface-level `str()` is one function in source; the table carries
//! its typed instances, and lowering emits the `str_*` FuncRef for the
//! operand's type [B7's explicit conversions].
//!
//! Unit natives return the inert unit word `Value::I64(0)`: the caller of a
//! unit callee never reads the overlay word (a lowering discipline), and a
//! tail call to a unit native still needs a return word to overlay.
//!
//! # Result spans and the platform tag ids
//!
//! The file-IO natives (indices 5-9) are the first `world`-effect platform
//! surface: touching the filesystem is a static fact (the ambient's declared
//! effect rows), so the VM needs no effect machinery — the natives stay
//! total, OS failures become tag words instead of VM traps. The fallible
//! four (all but `file_exists`) answer with a uniform two-word tag+payload
//! span [BL-2] on the caller's argument slots 0 and 1: word 0 is the
//! discriminant, word 1 the success value or the OS-error message `Ref`.
//! The span width is fixed per native, so callers size their overlay area
//! to two — the same RETSPAN discipline — and the call site pre-checks the
//! width before the native runs (a world side effect must not happen only
//! for the call to trap afterwards).
//!
//! The discriminants are platform tag ids. Tag ids are per-module: lowering
//! assigns each module's union members ids counting up from 0. Native
//! results are cross-module, so the builtin module reserves the top of the
//! `u8` tag-id space, counting down from [`BUILTIN_IO_OK`]; per-module
//! allocation and the platform reservation meet only once a module needs
//! 254 ids, at which point the tag id widens. `BUILTIN_IO_OK` marks every
//! fallible native's success (the caller already knows which native it
//! called, so one shared id disambiguates nothing); [`BUILTIN_IO_ERR`]
//! marks failure, with the message `Ref` in the span's payload word.
//!
//! The net natives (indices 20-24, `crate::net`) extend the same
//! convention: success reuses `BUILTIN_IO_OK`, and failure carries the
//! platform tag [`BUILTIN_NET_ERR`] — the file-owned `net_err` type needs
//! its own cross-module tag, reserved one step below `io_err` in the
//! countdown. Indices 10-19 are the string natives' reserved block (in
//! flight): the table reaches index 20 through placeholder entries so no
//! net index can collide, and a call to a reserved slot traps exactly like
//! an uninstalled index (the lowering never emits these indices).

use std::{io::Write, path::Path};

use ds_abi::SlotType;

use crate::{object::Objects, value::Value, vm::Trap};

/// The reserved native module every VM pre-installs.
pub const BUILTIN_MODULE: &str = "builtin";

/// `println(msg: string) -> unit`.
pub const BUILTIN_PRINTLN: u32 = 0;
/// `print(msg: string) -> unit`.
pub const BUILTIN_PRINT: u32 = 1;
/// `str_int(x: int) -> string`.
pub const BUILTIN_STR_INT: u32 = 2;
/// `str_float(x: float) -> string`.
pub const BUILTIN_STR_FLOAT: u32 = 3;
/// `str_bool(x: bool) -> string`.
pub const BUILTIN_STR_BOOL: u32 = 4;
/// `read_file(path: string) -> string | io_err`.
pub const BUILTIN_READ_FILE: u32 = 5;
/// `write_file(path, contents: string) -> unit | io_err` (create-or-truncate).
pub const BUILTIN_WRITE_FILE: u32 = 6;
/// `file_exists(path: string) -> bool` (unreachable paths answer false).
pub const BUILTIN_FILE_EXISTS: u32 = 7;
/// `fs_list(dir: string) -> list[string] | io_err` (names, sorted).
pub const BUILTIN_FS_LIST: u32 = 8;
/// `file_size(path: string) -> int | io_err` (bytes).
pub const BUILTIN_FILE_SIZE: u32 = 9;
/// `net_connect(host: string, port: int) -> int | net_err`.
pub const BUILTIN_NET_CONNECT: u32 = 20;
/// `net_send(conn: int, data: string) -> unit | net_err`.
pub const BUILTIN_NET_SEND: u32 = 21;
/// `net_recv(conn: int, max_bytes: int) -> string | net_err`.
pub const BUILTIN_NET_RECV: u32 = 22;
/// `net_close(conn: int) -> unit | net_err`.
pub const BUILTIN_NET_CLOSE: u32 = 23;
/// `http_get(url: string) -> string | net_err` (HTTP/1.0, no TLS).
pub const BUILTIN_HTTP_GET: u32 = 24;

/// The success discriminant of every fallible native's result span: the
/// top of the platform tag-id reservation (see the module docs).
pub const BUILTIN_IO_OK: u8 = u8::MAX;
/// The failure discriminant: the `io_err` tag, payload word holding the
/// OS-error message `Ref`.
pub const BUILTIN_IO_ERR: u8 = u8::MAX - 1;
/// The net natives' failure discriminant: the `net_err` tag, payload word
/// holding the message `Ref` — one step below `io_err` in the platform
/// reservation's countdown.
pub const BUILTIN_NET_ERR: u8 = u8::MAX - 2;

/// A native's result: one word on the caller's overlay slot 0, or a
/// fallible native's two-word tag+payload span on overlay slots 0 and 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NativeResult {
    /// A single value word: the total natives' only shape.
    Word(Value),
    /// The span: the discriminant word ([`BUILTIN_IO_OK`] or
    /// [`BUILTIN_IO_ERR`]) plus the payload word.
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
    /// against these exactly like a uop callee's layout.
    pub params: &'static [SlotType],
    /// How many overlay words the result needs: 1 for a single word, 2 for
    /// a fallible native's tag+payload span. Pre-checked at the call site
    /// against the caller's staging width, so the native never runs only
    /// for its call to trap on the result width.
    pub result_words: u8,
    /// The body.
    pub func: NativeFn,
}

/// The reserved block 10-19: one trap body per index, so a call to a
/// reserved slot reports its own index with the same trap an uninstalled
/// index yields. The string natives (in flight) replace these entries.
macro_rules! reserved_native {
    ($($name:ident = $index:literal),* $(,)?) => {
        $(
            fn $name(
                _objects: &mut Objects,
                _out: &mut OutputSink,
                _args: &[Value],
            ) -> Result<NativeResult, Trap> {
                Err(Trap::UnknownCallee {
                    module: BUILTIN_MODULE.to_owned(),
                    func: $index,
                })
            }
        )*
    };
}

reserved_native!(
    reserved_10 = 10,
    reserved_11 = 11,
    reserved_12 = 12,
    reserved_13 = 13,
    reserved_14 = 14,
    reserved_15 = 15,
    reserved_16 = 16,
    reserved_17 = 17,
    reserved_18 = 18,
    reserved_19 = 19,
);

/// The reserved slots' trap bodies, position 0 answering index 10.
const RESERVED_FUNCS: [NativeFn; 10] = [
    reserved_10,
    reserved_11,
    reserved_12,
    reserved_13,
    reserved_14,
    reserved_15,
    reserved_16,
    reserved_17,
    reserved_18,
    reserved_19,
];

/// The builtin table every VM installs as [`BUILTIN_MODULE`].
#[must_use]
pub fn builtin_module() -> Vec<NativeFunction> {
    let mut fns = vec![
        NativeFunction {
            name: "println",
            params: &[SlotType::Ref],
            result_words: 1,
            func: println_fn,
        },
        NativeFunction {
            name: "print",
            params: &[SlotType::Ref],
            result_words: 1,
            func: print_fn,
        },
        NativeFunction {
            name: "str_int",
            params: &[SlotType::I64],
            result_words: 1,
            func: str_int_fn,
        },
        NativeFunction {
            name: "str_float",
            params: &[SlotType::F64],
            result_words: 1,
            func: str_float_fn,
        },
        NativeFunction {
            name: "str_bool",
            params: &[SlotType::Bool],
            result_words: 1,
            func: str_bool_fn,
        },
        NativeFunction {
            name: "read_file",
            params: &[SlotType::Ref],
            result_words: 2,
            func: read_file_fn,
        },
        NativeFunction {
            name: "write_file",
            params: &[SlotType::Ref, SlotType::Ref],
            result_words: 2,
            func: write_file_fn,
        },
        NativeFunction {
            name: "file_exists",
            params: &[SlotType::Ref],
            result_words: 1,
            func: file_exists_fn,
        },
        NativeFunction {
            name: "fs_list",
            params: &[SlotType::Ref],
            result_words: 2,
            func: fs_list_fn,
        },
        NativeFunction {
            name: "file_size",
            params: &[SlotType::Ref],
            result_words: 2,
            func: file_size_fn,
        },
    ];
    // the string natives' reserved block, then the net natives' five
    for func in RESERVED_FUNCS {
        fns.push(NativeFunction {
            name: "reserved",
            params: &[],
            result_words: 1,
            func,
        });
    }
    fns.extend([
        NativeFunction {
            name: "net_connect",
            params: &[SlotType::Ref, SlotType::I64],
            result_words: 2,
            func: crate::net::net_connect_fn,
        },
        NativeFunction {
            name: "net_send",
            params: &[SlotType::I64, SlotType::Ref],
            result_words: 2,
            func: crate::net::net_send_fn,
        },
        NativeFunction {
            name: "net_recv",
            params: &[SlotType::I64, SlotType::I64],
            result_words: 2,
            func: crate::net::net_recv_fn,
        },
        NativeFunction {
            name: "net_close",
            params: &[SlotType::I64],
            result_words: 2,
            func: crate::net::net_close_fn,
        },
        NativeFunction {
            name: "http_get",
            params: &[SlotType::Ref],
            result_words: 2,
            func: crate::net::http_get_fn,
        },
    ]);
    fns
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
        tag: BUILTIN_IO_ERR,
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
            tag: BUILTIN_IO_OK,
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
            tag: BUILTIN_IO_OK,
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
        elems.push(objects.alloc_str(name));
    }
    Ok(NativeResult::Tag {
        tag: BUILTIN_IO_OK,
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
            tag: BUILTIN_IO_OK,
            payload: Value::I64(i64::try_from(meta.len()).expect("file size fits i64")),
        },
        Err(err) => io_err(objects, "file_size", &path, &err),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_indices_are_contract() {
        let fns = builtin_module();
        assert_eq!(fns[BUILTIN_PRINTLN as usize].name, "println");
        assert_eq!(fns[BUILTIN_PRINT as usize].name, "print");
        assert_eq!(fns[BUILTIN_STR_INT as usize].name, "str_int");
        assert_eq!(fns[BUILTIN_STR_FLOAT as usize].name, "str_float");
        assert_eq!(fns[BUILTIN_STR_BOOL as usize].name, "str_bool");
        assert_eq!(fns[BUILTIN_READ_FILE as usize].name, "read_file");
        assert_eq!(fns[BUILTIN_WRITE_FILE as usize].name, "write_file");
        assert_eq!(fns[BUILTIN_FILE_EXISTS as usize].name, "file_exists");
        assert_eq!(fns[BUILTIN_FS_LIST as usize].name, "fs_list");
        assert_eq!(fns[BUILTIN_FILE_SIZE as usize].name, "file_size");
        assert_eq!(fns[BUILTIN_NET_CONNECT as usize].name, "net_connect");
        assert_eq!(fns[BUILTIN_NET_SEND as usize].name, "net_send");
        assert_eq!(fns[BUILTIN_NET_RECV as usize].name, "net_recv");
        assert_eq!(fns[BUILTIN_NET_CLOSE as usize].name, "net_close");
        assert_eq!(fns[BUILTIN_HTTP_GET as usize].name, "http_get");
        assert_eq!(fns.len(), 25);
        // the reserved block traps with its own index, the uninstalled trap
        for index in 10..20 {
            let func = &fns[index as usize];
            assert_eq!(func.name, "reserved");
            assert_eq!(
                (func.func)(
                    &mut Objects::new(),
                    &mut OutputSink::Captured(Vec::new()),
                    &[]
                ),
                Err(Trap::UnknownCallee {
                    module: "builtin".into(),
                    func: index,
                })
            );
        }
        // the fallible four need a two-word overlay; the rest one word
        for (index, want) in [
            (BUILTIN_PRINTLN, 1),
            (BUILTIN_READ_FILE, 2),
            (BUILTIN_WRITE_FILE, 2),
            (BUILTIN_FILE_EXISTS, 1),
            (BUILTIN_FS_LIST, 2),
            (BUILTIN_FILE_SIZE, 2),
            (BUILTIN_NET_CONNECT, 2),
            (BUILTIN_NET_SEND, 2),
            (BUILTIN_NET_RECV, 2),
            (BUILTIN_NET_CLOSE, 2),
            (BUILTIN_HTTP_GET, 2),
        ] {
            assert_eq!(fns[index as usize].result_words, want);
        }
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
}
