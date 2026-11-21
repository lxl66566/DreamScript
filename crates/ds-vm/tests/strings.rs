//! String natives: hand-assembled fixtures over the builtin core native
//! module's pure string surface (`substring`, `concat`, `to_upper`,
//! `to_lower`, `trim`, `parse_int`, `split`, `char_at`, and the generic
//! `len` through CALLG with its evidence word). Direct value
//! assertions run each native through `CALL` and read the result word (or
//! the list object's parts) back through the VM's heap; the golden fixture
//! prints the composed pipeline byte-exact.
//!
//! Layout conventions (the contract ds-lower targets): every total pure
//! string native overlays one word on the caller's argument slot 0;
//! `parse_int` answers the fallible two-word span [BL-2] and the checked
//! reads `substring`/`char_at` their ok/none span on argument slots 0 and
//! 1 — word 0 the discriminant (`PLATFORM_IO_OK` / `PLATFORM_PARSE_ERR` /
//! `PLATFORM_NONE`), word 1 the payload — so a caller of one sizes its
//! overlay area to two; the overlay slots declare `Raw` because the same
//! words stage arguments first.

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, Inst, RcOp, SlideError, SlotType,
        SlotType::{Bool, I64, Raw, Ref, Tag},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_CHAR_AT, BUILTIN_CONCAT, BUILTIN_JOIN, BUILTIN_LEN, BUILTIN_LIST_NEW,
        BUILTIN_LIST_PUSH, BUILTIN_PARSE_INT, BUILTIN_SPLIT, BUILTIN_STR_INT, BUILTIN_SUBSTRING,
        BUILTIN_TO_LOWER, BUILTIN_TO_UPPER, BUILTIN_TRIM, PLATFORM_NONE, PLATFORM_PARSE_ERR,
    },
    platform::{OP_PRINTLN, Platform, STD_NAME},
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

fn platform(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: STD_NAME.into(),
        func: index,
    })
}

/// The shared pool, copied per function (the payload granularity). The
/// pure natives address the builtin core; println is the platform's.
fn pool() -> Vec<Const> {
    vec![
        Const::I64(0),              // k0: substring/char_at base index
        Const::I64(3),              // k1: substring width
        builtin(BUILTIN_TO_UPPER),  // k2
        builtin(BUILTIN_SUBSTRING), // k3
        builtin(BUILTIN_CHAR_AT),   // k4
        builtin(BUILTIN_TRIM),      // k5
        builtin(BUILTIN_LEN),       // k6
        builtin(BUILTIN_STR_INT),   // k7
        platform(OP_PRINTLN),       // k8
        builtin(BUILTIN_CONCAT),    // k9
        builtin(BUILTIN_TO_LOWER),  // k10
        builtin(BUILTIN_SPLIT),     // k11
        builtin(BUILTIN_PARSE_INT), // k12
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
    // the fixtures call println: the std platform table is the host's to
    // install (`Vm::new` only adds the builtin core)
    reg.install_platform(Platform::std());
    reg.install(ModuleVersion::new("st", defs).expect("valid fixture module"));
    reg
}

fn st_registry() -> ModuleRegistry {
    registry(vec![
        measure(),
        clip(),
        glue(),
        upper(),
        lower(),
        strip(),
        parse(),
        ask_parse(),
        parts(),
        pick(),
        report(),
    ])
}

/// `(s: string) -> int`: the generic `len` overlays one word. The call is
/// a CALLG — the receiver stages at overlay word 0 and the evidence word
/// (k0, the string instance id) at overlay word 1.
fn measure() -> FunctionDef {
    def("measure", &[Ref, I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kimm { dst: 3, kidx: k(0) },
        Uop::Callg {
            callee: k(6),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(s: string, start, end: int) -> string | none`: `substring`'s checked
/// span — word 0 the discriminant, word 1 the payload `Ref` on the hit
/// and the rejected `start` on the miss. The hit returns the payload;
/// the miss halts on the tag word, so the run result answers both arms.
fn clip() -> FunctionDef {
    def("clip", &[Ref, I64, I64, Bool, Raw, Raw, Raw], 3, 3, &[
        Uop::Funcf { frame_slots: 7 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Mov { dst: 5, src: 1 },
        Uop::Mov { dst: 6, src: 2 },
        Uop::Call {
            callee: k(3),
            nargs: 3,
        },
        Uop::TagTest {
            dst: 3,
            src: 4,
            tag: PLATFORM_NONE,
        },
        Uop::Jmpif { cond: 3, target: 8 },
        Uop::Ret { src: 5 },
        Uop::Halt { slot: 4 },
    ])
}

/// `(a, b: string) -> string`: `concat`.
fn glue() -> FunctionDef {
    def("glue", &[Ref, Ref, Ref, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(9),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `(s: string) -> string`: `to_upper`.
fn upper() -> FunctionDef {
    def("upper", &[Ref, Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(s: string) -> string`: `to_lower`.
fn lower() -> FunctionDef {
    def("lower", &[Ref, Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(10),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(s: string) -> string`: `trim`.
fn strip() -> FunctionDef {
    def("strip", &[Ref, Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(s: string) -> int | parse_err`: stage, call, dispatch on the span's
/// tag word; success returns the payload `int`, failure halts on the tag
/// word so the run result itself is the `parse_err` shape.
fn parse() -> FunctionDef {
    def("parse", &[Ref, I64, Bool, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(12),
            nargs: 1,
        },
        Uop::TagTest {
            dst: 2,
            src: 3,
            tag: PLATFORM_PARSE_ERR,
        },
        Uop::Jmpif { cond: 2, target: 7 },
        Uop::Mov { dst: 1, src: 4 },
        Uop::Ret { src: 1 },
        Uop::Halt { slot: 3 },
    ])
}

/// `(s: string) -> tag`: saves the span's tag word before the render
/// reuses the staging slot, renders the payload word — the position the
/// reader gave up at — through `str_int`, prints the rendered position,
/// and returns the tag word.
fn ask_parse() -> FunctionDef {
    def("ask_parse", &[Ref, Ref, Tag, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(12),
            nargs: 1,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Mov { dst: 3, src: 4 },
        Uop::Call {
            callee: k(7),
            nargs: 1,
        },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(s, sep: string) -> list[string]`: the payload `Ref` names the list
/// object.
fn parts() -> FunctionDef {
    def("parts", &[Ref, Ref, Ref, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(11),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `(s: string, i: int) -> string | none`: `char_at`'s checked span — the
/// hit returns the one-char payload `Ref`, the miss halts on the `none`
/// tag word.
fn pick() -> FunctionDef {
    def("pick", &[Ref, I64, Bool, Raw, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Mov { dst: 5, src: 1 },
        Uop::Call {
            callee: k(4),
            nargs: 2,
        },
        Uop::TagTest {
            dst: 3,
            src: 4,
            tag: PLATFORM_NONE,
        },
        Uop::Jmpif { cond: 3, target: 7 },
        Uop::Ret { src: 5 },
        Uop::Halt { slot: 4 },
    ])
}

/// `(s: string)`: the composed pipeline — trim, println, to_upper,
/// substring, char_at, len through str_int — one golden stdout. The same
/// overlay slots restage every native call in sequence.
fn report() -> FunctionDef {
    def("report", &[Ref, I64, Ref, Ref, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 7 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::Mov { dst: 2, src: 4 },
        Uop::Mov { dst: 4, src: 2 },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Mov { dst: 4, src: 2 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Mov { dst: 4, src: 2 },
        Uop::Kimm { dst: 5, kidx: k(0) },
        Uop::Kimm { dst: 6, kidx: k(1) },
        Uop::Call {
            callee: k(3),
            nargs: 3,
        },
        Uop::Mov { dst: 4, src: 5 },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Mov { dst: 4, src: 2 },
        Uop::Kimm { dst: 5, kidx: k(0) },
        Uop::Call {
            callee: k(4),
            nargs: 2,
        },
        Uop::Mov { dst: 4, src: 5 },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Mov { dst: 4, src: 2 },
        Uop::Kimm { dst: 5, kidx: k(0) },
        Uop::Callg {
            callee: k(6),
            nargs: 1,
        },
        Uop::Call {
            callee: k(7),
            nargs: 1,
        },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Halt { slot: 2 },
    ])
}

fn string(vm: &mut Vm, text: &str) -> Value {
    vm.intern_str(text)
}

/// Runs a fixture and reads the string content of its `Ref` result.
fn run_str(vm: &mut Vm, func: &str, args: &[Value]) -> String {
    let result = vm.run("st", func, args).expect("the fixture runs");
    let [Value::Ref(id)] = result.as_slice() else {
        panic!("a string ref, got {result:?}");
    };
    let id = *id;
    vm.objects()
        .str_at(id.id())
        .expect("the result names a string object")
        .to_owned()
}

/// Runs a fixture and asserts the checked span's `none` answer: the miss
/// member halts the run with the tag word.
fn run_none(vm: &mut Vm, func: &str, args: &[Value]) {
    assert_eq!(
        vm.run("st", func, args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)))
    );
}

/// Runs a fixture and reads the parts of its list-object result.
fn run_parts(vm: &mut Vm, func: &str, args: &[Value]) -> Vec<String> {
    let result = vm.run("st", func, args).expect("the fixture runs");
    let [Value::Ref(list)] = result.as_slice() else {
        panic!("a list ref, got {result:?}");
    };
    let list = *list;
    vm.objects()
        .list_at(list.id())
        .expect("the result names a list object")
        .iter()
        .map(|elem| match elem {
            Value::Ref(id) => vm
                .objects()
                .str_at(id.id())
                .expect("list elements are strings")
                .to_owned(),
            other => panic!("string elements, got {other:?}"),
        })
        .collect()
}

#[test]
fn len_counts_chars_not_bytes() {
    let mut vm = Vm::new(st_registry());
    let ascii = [string(&mut vm, "hello")];
    let utf8 = [string(&mut vm, "héllo")];
    let empty = [string(&mut vm, "")];
    assert_eq!(
        vm.run("st", "measure", &ascii),
        Ok(FrameAnswer::one(Value::I64(5)))
    );
    assert_eq!(
        vm.run("st", "measure", &utf8),
        Ok(FrameAnswer::one(Value::I64(5)))
    );
    assert_eq!(
        vm.run("st", "measure", &empty),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
}

/// The generic `len` dispatches its ONE body on the evidence word: the
/// list instance counts elements where the string instance counts chars,
/// the same FuncRef addressing both. A standalone module, because the
/// shared fixture pool has no string separator or instance constants.
#[test]
fn len_dispatches_the_one_body_on_the_evidence_word() {
    let pool = vec![
        Const::I64(1),          // k0: the list instance id
        Const::Str(";".into()), // k1: the split separator
        builtin(BUILTIN_SPLIT), // k2
        builtin(BUILTIN_LEN),   // k3
    ];
    let body = [
        Uop::Funcf { frame_slots: 3 },
        // overlay 0 stages the text, overlay 1 the separator
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kstr { dst: 2, kidx: k(1) },
        Uop::Call {
            callee: k(2),
            nargs: 2,
        },
        // the split result sits on overlay 0; the evidence word restages
        // overlay 1 as the list instance
        Uop::Kimm { dst: 2, kidx: k(0) },
        Uop::Callg {
            callee: k(3),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ];
    let def = FunctionDef {
        name: "count_list".into(),
        layout: FrameLayout::new(vec![Raw, Raw, Raw], 1, 2).expect("valid fixture layout"),
        pool,
        code: assemble(&body).expect("fixture body encodes"),
    };
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("st", vec![def]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    let args = [string(&mut vm, "alpha;beta;gamma")];
    assert_eq!(
        vm.run("st", "count_list", &args),
        Ok(FrameAnswer::one(Value::I64(3)))
    );
    let args = [string(&mut vm, "solo")];
    assert_eq!(
        vm.run("st", "count_list", &args),
        Ok(FrameAnswer::one(Value::I64(1)))
    );
}

/// A CALLG site must stage the callee's whole constraint list: an overlay
/// too narrow for the evidence word traps before the body runs, and a
/// body handed an unknown instance id traps too (lowering only mints the
/// builtin instance vocabulary).
#[test]
fn callg_sites_trap_on_missing_or_unknown_evidence() {
    // evidence word staged past a one-word overlay area
    let pool = vec![Const::I64(1), builtin(BUILTIN_LEN)];
    let body = [
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Callg {
            callee: k(1),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ];
    let def = FunctionDef {
        name: "narrow".into(),
        layout: FrameLayout::new(vec![Raw, Raw], 1, 1).expect("valid fixture layout"),
        pool,
        code: assemble(&body).expect("fixture body encodes"),
    };
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("st", vec![def]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    let args = [string(&mut vm, "hello")];
    assert!(matches!(
        vm.run("st", "narrow", &args),
        Err(Trap::CallLayout(SlideError::StagingTooSmall { .. }))
    ));

    // an evidence word naming no builtin instance
    let pool = vec![Const::I64(99), builtin(BUILTIN_LEN)];
    let body = [
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(0) },
        Uop::Callg {
            callee: k(1),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ];
    let def = FunctionDef {
        name: "wild".into(),
        layout: FrameLayout::new(vec![Raw, Raw, Raw], 1, 2).expect("valid fixture layout"),
        pool,
        code: assemble(&body).expect("fixture body encodes"),
    };
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("st", vec![def]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    let args = [string(&mut vm, "hello")];
    assert!(matches!(
        vm.run("st", "wild", &args),
        Err(Trap::UnknownInstance { evidence: 99 })
    ));
}

#[test]
fn substring_answers_the_checked_span_over_char_boundaries() {
    let mut vm = Vm::new(st_registry());
    let text = [string(&mut vm, "héllo wörld"), Value::I64(0), Value::I64(5)];
    assert_eq!(run_str(&mut vm, "clip", &text), "héllo");
    // the upper bound past the end answers none
    let text = [string(&mut vm, "héllo"), Value::I64(2), Value::I64(100)];
    run_none(&mut vm, "clip", &text);
    // the lower bound below zero answers none
    let text = [string(&mut vm, "héllo"), Value::I64(-5), Value::I64(2)];
    run_none(&mut vm, "clip", &text);
    // an in-range empty range answers the empty string; an inverted
    // range and one wholly past the end answer none
    let text = [string(&mut vm, "héllo"), Value::I64(3), Value::I64(3)];
    assert_eq!(run_str(&mut vm, "clip", &text), "");
    let text = [string(&mut vm, "héllo"), Value::I64(4), Value::I64(2)];
    run_none(&mut vm, "clip", &text);
    let text = [string(&mut vm, "héllo"), Value::I64(9), Value::I64(12)];
    run_none(&mut vm, "clip", &text);
}

#[test]
fn concat_joins_two_strings_into_a_fresh_object() {
    let mut vm = Vm::new(st_registry());
    let args = [string(&mut vm, "foo"), string(&mut vm, "bar")];
    let result = vm.run("st", "glue", &args).expect("the fixture runs");
    let [Value::Ref(id)] = result.as_slice() else {
        panic!("a string ref, got {result:?}");
    };
    let id = *id;
    assert_eq!(
        vm.objects().str_at(id.id()),
        Ok("foobar"),
        "the joined content"
    );
    // the native's results are transient by construction, so they never
    // intern: the same content under a distinct object id
    let interned = vm.intern_str("foobar");
    let Value::Ref(shared) = interned else {
        unreachable!("intern hands back a ref")
    };
    assert_ne!(id, shared, "identity differs");
    assert_eq!(
        vm.objects().str_at(id.id()),
        vm.objects().str_at(shared.id()),
        "content matches"
    );
}

#[test]
fn case_conversions_and_trim_cover_multi_byte_text() {
    let mut vm = Vm::new(st_registry());
    let args = [string(&mut vm, "héllo wörld")];
    assert_eq!(run_str(&mut vm, "upper", &args), "HÉLLO WÖRLD");
    let args = [string(&mut vm, "HÉLLO WÖRLD")];
    assert_eq!(run_str(&mut vm, "lower", &args), "héllo wörld");
    let args = [string(&mut vm, " \t héllo \r\n")];
    assert_eq!(run_str(&mut vm, "strip", &args), "héllo");
}

#[test]
fn parse_int_answers_the_success_int_and_the_parse_err_span() {
    let mut vm = Vm::new(st_registry());
    // success: the payload int returns
    let args = [string(&mut vm, "-42")];
    assert_eq!(
        vm.run("st", "parse", &args),
        Ok(FrameAnswer::one(Value::I64(-42)))
    );
    // failure: the run result is the parse_err tag word
    let args = [string(&mut vm, "12x")];
    assert_eq!(
        vm.run("st", "parse", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_PARSE_ERR)))
    );
    // strict: no implicit whitespace trim
    let args = [string(&mut vm, " 42")];
    assert_eq!(
        vm.run("st", "parse", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_PARSE_ERR)))
    );
    // and the payload reaches the caller: the failure payload is the
    // position the reader gave up at, so the rendered golden stdout is
    // the offending offset ("not-a-number" stops at 0, "12x" at 2)
    let mut vm = Vm::new(st_registry());
    vm.capture_output();
    let args = [string(&mut vm, "not-a-number")];
    assert_eq!(
        vm.run("st", "ask_parse", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_PARSE_ERR)))
    );
    assert_eq!(vm.captured_output(), b"0\n");
    let mut vm = Vm::new(st_registry());
    vm.capture_output();
    let args = [string(&mut vm, "12x")];
    assert_eq!(
        vm.run("st", "ask_parse", &args),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_PARSE_ERR)))
    );
    assert_eq!(vm.captured_output(), b"2\n");
}

#[test]
fn split_answers_parts_as_a_list_object() {
    let mut vm = Vm::new(st_registry());
    let args = [string(&mut vm, "alpha beta gamma"), string(&mut vm, " ")];
    assert_eq!(run_parts(&mut vm, "parts", &args), [
        "alpha", "beta", "gamma"
    ]);
    // a separator that never occurs: the whole text is one part
    let args = [string(&mut vm, "alpha"), string(&mut vm, ",")];
    assert_eq!(run_parts(&mut vm, "parts", &args), ["alpha"]);
    // an empty separator splits per character, multi-byte included
    let args = [string(&mut vm, "hé"), string(&mut vm, "")];
    assert_eq!(run_parts(&mut vm, "parts", &args), ["h", "é"]);
    // splitting the empty text with an empty separator: no parts
    let args = [string(&mut vm, ""), string(&mut vm, "")];
    assert_eq!(
        run_parts(&mut vm, "parts", &args),
        Vec::<String>::new(),
        "no parts"
    );
}

#[test]
fn char_at_answers_the_checked_char_read() {
    let mut vm = Vm::new(st_registry());
    let args = [string(&mut vm, "héllo"), Value::I64(1)];
    assert_eq!(run_str(&mut vm, "pick", &args), "é");
    let args = [string(&mut vm, "héllo"), Value::I64(0)];
    assert_eq!(run_str(&mut vm, "pick", &args), "h");
    // past either end: the none member, never a trap
    let args = [string(&mut vm, "héllo"), Value::I64(5)];
    run_none(&mut vm, "pick", &args);
    let args = [string(&mut vm, "héllo"), Value::I64(-1)];
    run_none(&mut vm, "pick", &args);
}

#[test]
fn the_composed_pipeline_prints_the_golden_stdout() {
    let mut vm = Vm::new(st_registry());
    vm.capture_output();
    let args = [string(&mut vm, "  DreamScript rocks ")];
    vm.run("st", "report", &args).expect("the fixture runs");
    assert_eq!(
        vm.captured_output(),
        b"DreamScript rocks\nDREAMSCRIPT ROCKS\nDre\nD\n17\n",
    );
}

/// Multi-byte text through the checked surface: cuts land between
/// scalars (CJK, emoji, mixed widths), and out-of-range bounds by either
/// side — huge indexes included — answer the `none` member.
#[test]
fn substring_and_char_at_pin_multi_byte_boundaries_and_huge_indexes() {
    let mut vm = Vm::new(st_registry());
    // CJK: three bytes per scalar
    let args = [string(&mut vm, "漢字語言夢"), Value::I64(1), Value::I64(3)];
    assert_eq!(run_str(&mut vm, "clip", &args), "字語");
    let args = [string(&mut vm, "漢字語言夢"), Value::I64(0), Value::I64(5)];
    assert_eq!(run_str(&mut vm, "clip", &args), "漢字語言夢");
    // four-byte emoji scalars mixed with ASCII
    let args = [string(&mut vm, "a👍b🚀"), Value::I64(1), Value::I64(3)];
    assert_eq!(run_str(&mut vm, "clip", &args), "👍b");
    let args = [string(&mut vm, "a👍b🚀"), Value::I64(2), Value::I64(4)];
    assert_eq!(run_str(&mut vm, "clip", &args), "b🚀");
    // boundary indexes: the empty range at zero and at the count
    let args = [string(&mut vm, "漢字"), Value::I64(2), Value::I64(2)];
    assert_eq!(run_str(&mut vm, "clip", &args), "");
    let args = [string(&mut vm, "漢字"), Value::I64(0), Value::I64(0)];
    assert_eq!(run_str(&mut vm, "clip", &args), "");
    // huge bounds answer none
    let args = [string(&mut vm, "漢字"), Value::I64(0), Value::I64(i64::MAX)];
    run_none(&mut vm, "clip", &args);
    let args = [
        string(&mut vm, "漢字"),
        Value::I64(i64::MAX),
        Value::I64(i64::MAX),
    ];
    run_none(&mut vm, "clip", &args);
    // the empty text: every slice answers none
    let args = [string(&mut vm, ""), Value::I64(-1), Value::I64(3)];
    run_none(&mut vm, "clip", &args);
    // char_at over the same strings: every scalar, none past both ends
    let args = [string(&mut vm, "a👍b🚀"), Value::I64(1)];
    assert_eq!(run_str(&mut vm, "pick", &args), "👍");
    let args = [string(&mut vm, "a👍b🚀"), Value::I64(3)];
    assert_eq!(run_str(&mut vm, "pick", &args), "🚀");
    let args = [string(&mut vm, "漢字"), Value::I64(1)];
    assert_eq!(run_str(&mut vm, "pick", &args), "字");
    let args = [string(&mut vm, "漢字"), Value::I64(2)];
    run_none(&mut vm, "pick", &args);
    let args = [string(&mut vm, "漢字"), Value::I64(i64::MAX)];
    run_none(&mut vm, "pick", &args);
}

/// One interned string hit by every char-indexed native in turn: the
/// lazily built boundary map must answer every later call exactly like
/// the first.
#[test]
fn repeated_char_indexed_access_serves_one_boundary_map() {
    let mut vm = Vm::new(st_registry());
    let text = string(&mut vm, "夢見漢字");
    let expect: Vec<char> = "夢見漢字".chars().collect();
    for i in 0..3_usize {
        let index = i64::try_from(i).expect("loop index fits i64");
        let args = [text, Value::I64(index), Value::I64(index + 2)];
        let want: String = expect[i..i + 2].iter().collect();
        assert_eq!(run_str(&mut vm, "clip", &args), want);
    }
    let args = [text, Value::I64(3), Value::I64(4)];
    assert_eq!(run_str(&mut vm, "clip", &args), "字");
    for (i, want) in expect.iter().enumerate() {
        let index = i64::try_from(i).expect("loop index fits i64");
        let args = [text, Value::I64(index)];
        assert_eq!(run_str(&mut vm, "pick", &args), want.to_string());
    }
    // and len agrees over the same map after it exists
    assert_eq!(
        vm.run("st", "measure", &[text]),
        Ok(FrameAnswer::one(Value::I64(4)))
    );
}

/// KSTR over the pool-const intern cache: two loads of one pool constant
/// share a single object within the run, and the cache holds no claim of
/// its own — after the body's own decs balance the loads, the heap drains
/// exactly as an uncached run would, on the first run and again on the
/// refill of the second (a dead cached id re-validates to a miss).
#[test]
fn kstr_pool_consts_share_one_object_and_the_cache_claim_releases() {
    let pool = vec![Const::Str("kstr".into())];
    let body: [Inst; 7] = [
        Uop::Funcf { frame_slots: 3 }.into(),
        Uop::Kstr { dst: 1, kidx: k(0) }.into(),
        Uop::Kstr { dst: 2, kidx: k(0) }.into(),
        Uop::EqStr {
            dst: 0,
            lhs: 1,
            rhs: 2,
        }
        .into(),
        RcOp::Dec { src: 1 }.into(),
        RcOp::Dec { src: 2 }.into(),
        Uop::Ret { src: 0 }.into(),
    ];
    let def = FunctionDef {
        name: "shared_k".into(),
        layout: FrameLayout::new(vec![Bool, Ref, Ref], 0, 0).expect("valid fixture layout"),
        pool,
        code: assemble(&body).expect("fixture body encodes"),
    };
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("st", vec![def]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("st", "shared_k", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the two loads of one pool constant are one object"
    );
    assert!(
        vm.objects().is_empty(),
        "no claim outlives the drive: the heap drained"
    );
    // the refill of the second run releases again
    assert_eq!(
        vm.run("st", "shared_k", &[]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert!(vm.objects().is_empty());
}

/// `join`'s fixture pool: the string family's producer and renderer plus
/// the empty-list constructor, the plain comma separator, and one
/// non-string element word.
fn jn_pool() -> Vec<Const> {
    vec![
        builtin(BUILTIN_SPLIT),     // k0
        builtin(BUILTIN_JOIN),      // k1
        builtin(BUILTIN_LIST_NEW),  // k2
        Const::Str(",".into()),     // k3
        Const::I64(7),              // k4: the non-string element
        builtin(BUILTIN_LIST_PUSH), // k5
    ]
}

fn jn_def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: jn_pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn jn_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("st", vec![render(), blanks(), tainted()])
            .expect("valid fixture module"),
    );
    reg
}

/// `(s, split_sep, join_sep: string) -> string`: split builds the parts
/// list, join renders it back with the second separator. The two CALLs
/// chain on one overlay area: split's result overlays word 0 while the
/// join separator survives on word 1.
fn render() -> FunctionDef {
    jn_def("render", &[Ref, Ref, Ref, Raw, Raw, Raw], 3, 3, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Mov { dst: 5, src: 2 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::Mov { dst: 4, src: 5 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `() -> string`: the empty list joins to the empty string.
fn blanks() -> FunctionDef {
    jn_def("blanks", &[Raw, Raw], 0, 2, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Call {
            callee: k(2),
            nargs: 0,
        },
        Uop::Kstr { dst: 1, kidx: k(3) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 0 },
    ])
}

/// `() -> string`: a list holding one i64 element refuses at its own
/// position — the checked surface guarantees `list[string]`, so only a
/// hand-assembled caller reaches the trap.
fn tainted() -> FunctionDef {
    // slot 0 is the body word holding the list across both calls; the
    // two overlay words are slots 1 and 2
    jn_def("tainted", &[Raw, Raw, Raw], 0, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Call {
            callee: k(2),
            nargs: 0,
        },
        Uop::Mov { dst: 0, src: 1 },
        Uop::Kimm { dst: 2, kidx: k(4) },
        Uop::Call {
            callee: k(5),
            nargs: 2,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kstr { dst: 2, kidx: k(3) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `join` renders the parts list back to one string: multiple parts
/// carry the separator between neighbors, a single part answers alone,
/// a unicode separator round-trips, an empty separator concatenates,
/// and the empty list answers the empty string.
#[test]
fn join_renders_the_parts_back_to_one_string() {
    let mut vm = Vm::new(jn_registry());
    let args = [
        string(&mut vm, "a:b:c"),
        string(&mut vm, ":"),
        string(&mut vm, ","),
    ];
    assert_eq!(run_str(&mut vm, "render", &args), "a,b,c");
    let args = [
        string(&mut vm, "solo"),
        string(&mut vm, ":"),
        string(&mut vm, ","),
    ];
    assert_eq!(run_str(&mut vm, "render", &args), "solo");
    let args = [
        string(&mut vm, "\u{3b1}\u{2192}\u{3b2}"),
        string(&mut vm, "\u{2192}"),
        string(&mut vm, "\u{2192}"),
    ];
    assert_eq!(run_str(&mut vm, "render", &args), "\u{3b1}\u{2192}\u{3b2}");
    let args = [
        string(&mut vm, "a:b"),
        string(&mut vm, ":"),
        string(&mut vm, ""),
    ];
    assert_eq!(run_str(&mut vm, "render", &args), "ab");
    assert_eq!(run_str(&mut vm, "blanks", &[]), "");
}

/// A non-string element refuses at its own position: the checked surface
/// guarantees `list[string]`, so the trap names the offending element.
#[test]
fn join_refuses_a_non_string_element_at_its_position() {
    let mut vm = Vm::new(jn_registry());
    assert_eq!(
        vm.run("st", "tainted", &[]),
        Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: Ref,
            actual: I64,
        })
    );
}
