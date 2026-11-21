//! File-IO natives: hand-assembled fixtures over the std platform native
//! module's `world`-effect surface (`read_file`, `write_file`,
//! `file_exists`, `fs_list`, `file_size`). Golden coverage: byte-exact
//! string round trips through the two-word tag+payload spans, the
//! io-class span shape on OS failure (both through CALL and through a
//! tail call's reused frame), `file_exists` both ways, and the
//! overlay-width pre-check that keeps a world side effect from running
//! only for its call to trap on the result width.
//!
//! Layout conventions (the contract ds-lower targets): every fallible
//! native's result is a uniform two-word span [BL-2] on the caller's
//! argument slots 0 and 1 — word 0 the discriminant (`PLATFORM_IO_OK`
//! or the failure's io class), word 1 the payload — so a caller of one sizes its
//! overlay area to two; the overlay slots declare `Raw` because the same
//! words stage arguments first.

use std::fs;

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{PLATFORM_IO_NOT_FOUND, PLATFORM_IO_OK},
    platform::{
        OP_FILE_EXISTS, OP_FILE_SIZE, OP_FS_LIST, OP_PRINTLN, OP_READ_FILE, OP_WRITE_FILE,
        Platform, STD_NAME,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn platform(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: STD_NAME.into(),
        func: index,
    })
}

/// The shared pool, copied per function (the payload granularity): the
/// five file natives, println, and this module's own `tail_read`
/// wrapper (function-table index 5).
fn pool() -> Vec<Const> {
    vec![
        platform(OP_READ_FILE),   // k0
        platform(OP_WRITE_FILE),  // k1
        platform(OP_FILE_EXISTS), // k2
        platform(OP_FS_LIST),     // k3
        platform(OP_FILE_SIZE),   // k4
        platform(OP_PRINTLN),     // k5
        Const::FuncRef(ConstFuncRef::Named {
            module: "io".into(),
            func: 5, // k6: tail_read
        }),
    ]
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    // the whole surface here is the platform's: the default platform is
    // the host's to install (`Vm::new` only adds the builtin core)
    reg.install_platform(Platform::std());
    reg.install(ModuleVersion::new("io", defs).expect("valid fixture module"));
    reg
}

/// `(path: string) -> string | io_err`: stage, call, dispatch on the span's
/// tag word; success returns the payload `Ref`, failure halts on the tag
/// word so the run result itself is the `io_err` shape.
///
/// Slots: [path, result, t, overlay tag, overlay payload].
fn read_text() -> FunctionDef {
    def("read_text", &[Ref, Ref, Bool, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        Uop::TagTest {
            dst: 2,
            src: 3,
            tag: PLATFORM_IO_NOT_FOUND,
        },
        Uop::Jmpif { cond: 2, target: 7 },
        Uop::Mov { dst: 1, src: 4 },
        Uop::Ret { src: 1 },
        Uop::Halt { slot: 3 },
    ])
}

/// `(path, contents: string) -> bool | io_err`: write, read back, compare
/// the payload with the staged contents by content (EQSTR). Any failure
/// halts on the tag word; the disk file is the test's own to check.
///
/// Slots: [path, contents, t, overlay tag, overlay payload].
fn roundtrip() -> FunctionDef {
    def("roundtrip", &[Ref, Ref, Bool, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::TagTest {
            dst: 2,
            src: 3,
            tag: PLATFORM_IO_NOT_FOUND,
        },
        Uop::Jmpif {
            cond: 2,
            target: 12,
        },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        Uop::TagTest {
            dst: 2,
            src: 3,
            tag: PLATFORM_IO_NOT_FOUND,
        },
        Uop::Jmpif {
            cond: 2,
            target: 12,
        },
        Uop::EqStr {
            dst: 2,
            lhs: 4,
            rhs: 1,
        },
        Uop::Ret { src: 2 },
        Uop::Halt { slot: 3 },
    ])
}

/// `(path: string) -> bool`: `file_exists` overlays one word.
fn exists() -> FunctionDef {
    def("exists", &[Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(dir: string) -> list[string] | io_err`: the payload `Ref` names the
/// list object.
///
/// Slots: [dir, result, t, overlay tag, overlay payload].
fn list_names() -> FunctionDef {
    def("list_names", &[Ref, Ref, Bool, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(3),
            nargs: 1,
        },
        Uop::TagTest {
            dst: 2,
            src: 3,
            tag: PLATFORM_IO_NOT_FOUND,
        },
        Uop::Jmpif { cond: 2, target: 7 },
        Uop::Mov { dst: 1, src: 4 },
        Uop::Ret { src: 1 },
        Uop::Halt { slot: 3 },
    ])
}

/// `(path: string) -> int | io_err`: the payload word is the byte length.
///
/// Slots: [path, size, t, overlay tag, overlay payload].
fn size() -> FunctionDef {
    def("size", &[Ref, I64, Bool, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(4),
            nargs: 1,
        },
        Uop::TagTest {
            dst: 2,
            src: 3,
            tag: PLATFORM_IO_NOT_FOUND,
        },
        Uop::Jmpif { cond: 2, target: 7 },
        Uop::Mov { dst: 1, src: 4 },
        Uop::Ret { src: 1 },
        Uop::Halt { slot: 3 },
    ])
}

/// `(path: string)`: a CALL-only wrapper whose body tail-calls
/// `read_file` — the span a CALLT-target native produces must reach the
/// ORIGINAL caller's overlay words through the reused frame.
fn tail_read() -> FunctionDef {
    def("tail_read", &[Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Callt {
            callee: k(0),
            nargs: 1,
        },
        Uop::Halt { slot: 0 }, // unreachable; keeps the body closed
    ])
}

/// `(path: string) -> tag`: calls `tail_read` (non-tail), so the
/// native's span lands on THIS frame's overlay words; saves the tag word
/// before the println call reuses the staging slot, prints the payload
/// word (the content on success, the OS error message on failure), and
/// returns the tag word. The run result distinguishes the member; the
/// captured stdout carries the payload.
///
/// Slots: [path, msg, tag, overlay tag, overlay payload].
fn ask() -> FunctionDef {
    def("ask", &[Ref, Ref, SlotType::Tag, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(6),
            nargs: 1,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Mov { dst: 1, src: 4 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(path: string)`: a one-word overlay area staging `read_file` — the
/// width pre-check traps before the native runs.
fn narrow() -> FunctionDef {
    def("narrow", &[Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        Uop::Halt { slot: 1 },
    ])
}

fn io_registry() -> ModuleRegistry {
    registry(vec![
        read_text(),
        roundtrip(),
        exists(),
        list_names(),
        size(),
        tail_read(),
        ask(),
        narrow(),
    ])
}

fn string(vm: &mut Vm, text: &str) -> Value {
    vm.intern_str(text)
}

#[test]
fn write_then_read_round_trips_byte_exact() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("note.txt");
    let contents = "hello\nworld — ünïcödé ✓\0\n"; // multi-byte, newline, NUL
    let mut vm = Vm::new(io_registry());
    let args = [
        string(&mut vm, path.to_str().unwrap()),
        string(&mut vm, contents),
    ];
    assert_eq!(
        vm.run("io", "roundtrip", &args),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the read-back payload equals the written contents"
    );
    // and the file on disk holds exactly the written bytes
    assert_eq!(
        fs::read(&path).expect("write_file created the file"),
        contents.as_bytes()
    );
}

#[test]
fn write_file_truncates_an_existing_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("t.txt");
    fs::write(&path, b"the original long contents").expect("seed");
    let mut vm = Vm::new(io_registry());
    let args = [
        string(&mut vm, path.to_str().unwrap()),
        string(&mut vm, "short"),
    ];
    assert_eq!(
        vm.run("io", "roundtrip", &args),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert_eq!(fs::read(&path).expect("readable"), b"short");
}

#[test]
fn read_file_of_a_missing_path_yields_the_io_err_span() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("absent.txt");
    let mut vm = Vm::new(io_registry());
    let args = [string(&mut vm, missing.to_str().unwrap())];
    // the fixture halts on the tag word: the run result is the io_err tag
    assert_eq!(
        vm.run("io", "read_text", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_NOT_FOUND)))
    );

    // through `ask` the payload reaches the caller: the OS error message
    // is a nonempty string naming the operation and the path. Ref words
    // name THIS VM's objects, so the path re-interns per VM.
    let mut vm = Vm::new(io_registry());
    vm.capture_output();
    let args = [string(&mut vm, missing.to_str().unwrap())];
    assert_eq!(
        vm.run("io", "ask", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_NOT_FOUND)))
    );
    let printed = String::from_utf8_lossy(vm.captured_output()).into_owned();
    assert!(
        printed.starts_with("read_file ") && printed.contains("absent.txt"),
        "the payload describes the failure: {printed:?}"
    );
    assert!(printed.ends_with('\n'), "println ends the line");
}

#[test]
fn read_file_through_a_tail_call_delivers_the_full_span_to_the_caller() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("here.txt");
    fs::write(&file, b"tail content").expect("seed");
    let mut vm = Vm::new(io_registry());
    vm.capture_output();
    let args = [string(&mut vm, file.to_str().unwrap())];
    assert_eq!(
        vm.run("io", "ask", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_OK)))
    );
    assert_eq!(vm.captured_output(), b"tail content\n");
}

#[test]
fn a_root_tail_call_to_a_fallible_native_answers_the_whole_span() {
    // the span-answer seam: a root tail call's two-word span crosses to
    // the host whole, the payload word riding beside the tag
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("root.txt");
    fs::write(&file, b"surviving payload").expect("seed");
    let mut vm = Vm::new(io_registry());
    let args = [string(&mut vm, file.to_str().unwrap())];
    let answer = vm
        .run("io", "tail_read", &args)
        .expect("the tail read answers");
    let [Value::Tag(tag), Value::Ref(word)] = answer.as_slice() else {
        panic!("the whole span answers, got {answer:?}")
    };
    assert_eq!(*tag, PLATFORM_IO_OK);
    let word = *word;
    assert_eq!(vm.objects().str_at(word.id()), Ok("surviving payload"));
}

#[test]
fn file_exists_answers_both_ways() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("present.txt");
    fs::write(&file, b"x").expect("seed");
    let mut vm = Vm::new(io_registry());
    let present = [string(&mut vm, file.to_str().unwrap())];
    let absent = [string(&mut vm, dir.path().join("no.txt").to_str().unwrap())];
    let itself = [string(&mut vm, dir.path().to_str().unwrap())];
    assert_eq!(
        vm.run("io", "exists", &present),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert_eq!(
        vm.run("io", "exists", &absent),
        Ok(FrameAnswer::one(Value::Bool(false)))
    );
    // a directory names an entry too
    assert_eq!(
        vm.run("io", "exists", &itself),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
}

#[test]
fn fs_lists_sorted_entry_names() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join("zebra"), b"").expect("seed");
    fs::write(dir.path().join("alpha"), b"").expect("seed");
    fs::create_dir(dir.path().join("mid")).expect("dir entry");
    let mut vm = Vm::new(io_registry());
    let args = [string(&mut vm, dir.path().to_str().unwrap())];
    let result = vm.run("io", "list_names", &args).expect("listing succeeds");
    let [Value::Ref(list)] = result.as_slice() else {
        panic!("the success payload is a list ref, got {result:?}");
    };
    let list = *list;
    let elems = vm
        .objects()
        .list_at(list.id())
        .expect("a list object")
        .to_vec();
    let names = elems
        .iter()
        .map(|e| match e {
            Value::Ref(id) => vm
                .objects()
                .str_at(id.id())
                .expect("string elements")
                .to_owned(),
            other => panic!("string elements, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["alpha", "mid", "zebra"],
        "sorted, directories included"
    );

    // listing a missing directory is the io_err tag word
    let args = [string(&mut vm, dir.path().join("nope").to_str().unwrap())];
    assert_eq!(
        vm.run("io", "list_names", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_NOT_FOUND)))
    );
}

#[test]
fn file_size_reports_bytes_and_io_err() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("blob");
    fs::write(&file, [0u8; 1024]).expect("seed");
    let mut vm = Vm::new(io_registry());
    let present = [string(&mut vm, file.to_str().unwrap())];
    let absent = [string(&mut vm, dir.path().join("gone").to_str().unwrap())];
    assert_eq!(
        vm.run("io", "size", &present),
        Ok(FrameAnswer::one(Value::I64(1024)))
    );
    assert_eq!(
        vm.run("io", "size", &absent),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_NOT_FOUND)))
    );
}

#[test]
fn a_narrow_overlay_area_traps_before_the_native_runs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("side.txt");
    let mut vm = Vm::new(io_registry());
    let args = [string(&mut vm, file.to_str().unwrap())];
    assert_eq!(
        vm.run("io", "narrow", &args),
        Err(Trap::RetSpanTooWide {
            span: 2,
            caller_area: 1
        })
    );
    // the width pre-check fires before the native runs: no world side
    // effect happened for the trapping call
    assert!(!file.exists(), "read_file never touched the path");
}
