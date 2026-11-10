//! List natives: hand-assembled fixtures over the builtin core native
//! module's list surface — `list_new` (index 20) and the generic reads
//! `list_len`, `list_get`, `list_concat` (indices 21-23, CALLG-addressed
//! with one evidence word naming the list instance) — composed with
//! `split`, the string family's list producer. The round trips run
//! through CALLG for the generic reads and CALL for the monomorphic
//! constructors: construction, length, the checked read's both members,
//! and the fresh-object join.
//!
//! Layout conventions (the contract ds-lower targets): `list_get` is the
//! fallible shape — a two-word tag+payload span [BL-2] on the caller's
//! argument slots 0 and 1, word 0 the discriminant (`PLATFORM_IO_OK`
//! with the element, `PLATFORM_NONE` with the rejected index) — so a
//! caller sizes its overlay area to two; the other list natives overlay
//! one word. The overlay slots declare `Raw` because the same words
//! stage arguments first.

use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_LIST_CONCAT, BUILTIN_LIST_GET, BUILTIN_LIST_LEN, BUILTIN_LIST_NEW, BUILTIN_SPLIT,
        PLATFORM_NONE,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef {
        module: "builtin".into(),
        func: index,
    }
}

/// The shared pool, copied per function (the payload granularity): the
/// space separator (KSTR-staged) and the five list-facing natives.
fn pool() -> Vec<Const> {
    vec![
        Const::Str(" ".into()),                   // k0
        builtin(BUILTIN_SPLIT),                   // k1
        builtin(BUILTIN_LIST_NEW),                // k2
        builtin(BUILTIN_LIST_LEN),                // k3
        builtin(BUILTIN_LIST_GET),                // k4
        builtin(BUILTIN_LIST_CONCAT),             // k5
        Const::I64(BuiltinInstance::List as i64), // k6: the evidence word
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

fn li_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    // the list surface is pure core: `Vm::new` installs the builtin table
    reg.install(
        ModuleVersion::new("li", vec![fresh(), words(), parts(), count(), at(), glue()])
            .expect("valid fixture module"),
    );
    reg
}

/// `() -> list`: `list_new` with nothing staged; the result overlays the
/// one-word argument area.
fn fresh() -> FunctionDef {
    def("fresh", &[Ref, Raw], 0, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Call {
            callee: k(2),
            nargs: 0,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(s: string) -> list`: splits on the KSTR-staged space.
fn words() -> FunctionDef {
    def("words", &[Ref, Ref, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kstr { dst: 2, kidx: k(0) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(s, sep: string) -> list`: `split` over an arbitrary separator.
fn parts() -> FunctionDef {
    def("parts", &[Ref, Ref, Ref, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `(list) -> int`: `list_len` through CALLG, one evidence word staged
/// after the receiver.
fn count() -> FunctionDef {
    def("count", &[Ref, I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kimm { dst: 3, kidx: k(6) },
        Uop::Callg {
            callee: k(3),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(list, i: int) -> string | none`: stage the receiver and index, the
/// evidence word behind them, call through CALLG, then dispatch on the
/// span's tag word; success returns the payload element, an out-of-range
/// index halts on the tag word so the run result itself is the `none`
/// shape.
///
/// Slots: [list, i, elem, t, overlay words: args, evidence, span].
fn at() -> FunctionDef {
    def("at", &[Ref, I64, Ref, Bool, Raw, Raw, Raw], 2, 3, &[
        Uop::Funcf { frame_slots: 7 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Mov { dst: 5, src: 1 },
        Uop::Kimm { dst: 6, kidx: k(6) },
        Uop::Callg {
            callee: k(4),
            nargs: 2,
        },
        Uop::TagTest {
            dst: 3,
            src: 4,
            tag: PLATFORM_NONE,
        },
        Uop::Jmpif { cond: 3, target: 9 },
        Uop::Mov { dst: 2, src: 5 },
        Uop::Ret { src: 2 },
        Uop::Halt { slot: 4 },
    ])
}

/// `(a, b: list) -> list`: `list_concat` through CALLG, one evidence
/// word staged after the two lists.
fn glue() -> FunctionDef {
    def("glue", &[Ref, Ref, Ref, Raw, Raw, Raw], 2, 3, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Kimm { dst: 5, kidx: k(6) },
        Uop::Callg {
            callee: k(5),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

fn run_value(vm: &mut Vm, func: &str, args: &[Value]) -> Value {
    vm.run("li", func, args).expect("the fixture runs")
}

/// Runs `count` over a list-value argument.
fn count_of(vm: &mut Vm, list: Value) -> i64 {
    match run_value(vm, "count", &[list]) {
        Value::I64(n) => n,
        other => panic!("an int, got {other:?}"),
    }
}

/// Runs `at`; the answer is the element `Ref` or the `none` tag word.
fn at_value(vm: &mut Vm, list: Value, index: i64) -> Value {
    run_value(vm, "at", &[list, Value::I64(index)])
}

fn at_string(vm: &mut Vm, list: Value, index: i64) -> String {
    match at_value(vm, list, index) {
        Value::Ref(id) => vm.objects().str_at(id).expect("string elements").to_owned(),
        other => panic!("a string ref, got {other:?}"),
    }
}

#[test]
fn list_new_answers_the_empty_list() {
    let mut vm = Vm::new(li_registry());
    let list = run_value(&mut vm, "fresh", &[]);
    let Value::Ref(id) = list else {
        panic!("a list ref, got {list:?}");
    };
    assert_eq!(vm.objects().list_at(id), Ok(&[][..]));
    assert_eq!(count_of(&mut vm, list), 0);
    // the empty list is a fresh object per call, never interned
    let again = run_value(&mut vm, "fresh", &[]);
    assert_ne!(list, again, "identity differs");
}

#[test]
fn split_then_len_and_get_round_trip() {
    let mut vm = Vm::new(li_registry());
    let text = vm.intern_str("alpha beta gamma");
    let list = run_value(&mut vm, "words", &[text]);
    assert_eq!(count_of(&mut vm, list), 3);
    assert_eq!(at_string(&mut vm, list, 0), "alpha");
    assert_eq!(at_string(&mut vm, list, 1), "beta");
    assert_eq!(at_string(&mut vm, list, 2), "gamma");
    // out of range by either side: the none member, never a trap
    assert_eq!(at_value(&mut vm, list, 3), Value::Tag(PLATFORM_NONE));
    assert_eq!(at_value(&mut vm, list, -1), Value::Tag(PLATFORM_NONE));
    // an empty separator splits per character, multi-byte included
    let text = vm.intern_str("hé");
    let sep = vm.intern_str("");
    let chars = run_value(&mut vm, "parts", &[text, sep]);
    assert_eq!(count_of(&mut vm, chars), 2);
    assert_eq!(at_string(&mut vm, chars, 0), "h");
    assert_eq!(at_string(&mut vm, chars, 1), "é");
}

#[test]
fn concat_joins_into_a_fresh_object_sharing_elements() {
    let mut vm = Vm::new(li_registry());
    let space = vm.intern_str(" ");
    let ab = vm.intern_str("a b");
    let cd = vm.intern_str("c d");
    let a = run_value(&mut vm, "parts", &[ab, space]);
    let b = run_value(&mut vm, "parts", &[cd, space]);
    let glued = run_value(&mut vm, "glue", &[a, b]);
    assert_eq!(count_of(&mut vm, glued), 4);
    assert_eq!(at_string(&mut vm, glued, 3), "d");
    let (Value::Ref(glued_id), Value::Ref(a_id), Value::Ref(b_id)) = (glued, a, b) else {
        unreachable!("list results are refs");
    };
    assert_ne!(glued_id, a_id);
    assert_ne!(glued_id, b_id, "the join is a fresh object");
    // element words are copied, not the objects they name: the glued
    // list shares the operand elements by identity
    let shared = vm.objects().list_at(glued_id).expect("glued").to_vec();
    let front = vm.objects().list_at(a_id).expect("a").to_vec();
    assert_eq!(shared[..front.len()], front[..], "elements shared");
}
