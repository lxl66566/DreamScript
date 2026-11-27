//! The generic container natives past the map block: `cons` (32),
//! `tail` (34), `collect` (35), and `first` (36) — the retired `head`
//! spelling's slot at 33 stays reserved — each addressed through CALLG
//! with one evidence word staged after the real arguments. The producers
//! answer fresh lists, and `first` carries the checked first-element
//! body in `list_get`'s ok/none span shape. The retain contract's balance
//! pins live in rc_diag.rs under the diag feature.

use ds_vm::{
    Const, ConstFuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, Inst, RcOp, SlotType,
        SlotType::{Bool, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_COLLECT, BUILTIN_CONS, BUILTIN_FIRST, BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH,
        BUILTIN_TAIL, PLATFORM_IO_OK, PLATFORM_NONE,
    },
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

/// The shared pool, copied per function (the payload granularity): the
/// evidence words (list k0, int k8, float k9) and the seven
/// container-facing natives. String elements stage as host-passed `Ref`
/// words.
fn pool() -> Vec<Const> {
    vec![
        Const::I64(BuiltinInstance::List as i64), // k0: the list evidence word
        builtin(BUILTIN_LIST_NEW),                // k1
        builtin(BUILTIN_LIST_PUSH),               // k2
        builtin(BUILTIN_CONS),                    // k3
        builtin(BUILTIN_TAIL),                    // k4
        builtin(BUILTIN_COLLECT),                 // k5
        builtin(BUILTIN_FIRST),                   // k6
    ]
}

fn def<I: Into<Inst> + Copy>(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    body: &[I],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn gn_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    // the container surface is pure core: `Vm::new` installs the tables
    reg.install(
        ModuleVersion::new("gn", vec![
            empty(),
            push(),
            cons(),
            first(),
            first_tag(),
            tail(),
            collect(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// `() -> list`: the empty list.
fn empty() -> FunctionDef {
    def("empty", &[Ref, Raw], 0, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Call {
            callee: k(1),
            nargs: 0,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(list, x: value) -> list`: the in-place append, for building
/// fixtures with more than one element.
fn push() -> FunctionDef {
    def("push", &[Ref, Raw, Ref, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 5 }.into(),
        Uop::Mov { dst: 3, src: 0 }.into(),
        Uop::Mov { dst: 4, src: 1 }.into(),
        Uop::Call {
            callee: k(2),
            nargs: 2,
        }
        .into(),
        RcOp::Dec { src: 3 }.into(),
        Uop::Mov { dst: 2, src: 0 }.into(),
        Uop::Ret { src: 2 }.into(),
    ] as &[Inst])
}

/// `(x: value, xs: list) -> list`: `cons` through CALLG, the element
/// staged ahead of the receiver and the evidence word behind them.
fn cons() -> FunctionDef {
    def("cons", &[Raw, Ref, Ref, Raw, Raw, Raw], 2, 3, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Kimm { dst: 5, kidx: k(0) },
        Uop::Callg {
            callee: k(3),
            nargs: 2,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Ret { src: 2 },
    ])
}

/// `(xs: list) -> t | none`: `first`, the checked first element,
/// dispatched on the span's tag word like `list_get`'s fixture callers —
/// success returns the payload element, an empty list halts on the tag
/// word so the run result itself is the `none` shape.
///
/// Slots: [list, elem, hit, overlay words: xs, evidence].
fn first() -> FunctionDef {
    def("first", &[Ref, Raw, Bool, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Kimm { dst: 4, kidx: k(0) },
        Uop::Callg {
            callee: k(6),
            nargs: 1,
        },
        Uop::TagTest {
            dst: 2,
            src: 3,
            tag: PLATFORM_NONE,
        },
        Uop::Jmpif { cond: 2, target: 8 },
        Uop::Mov { dst: 1, src: 4 },
        Uop::Ret { src: 1 },
        Uop::Halt { slot: 3 },
    ])
}

/// `(xs: list) -> word`: the span's raw discriminant word, pinning the
/// checked shape itself.
fn first_tag() -> FunctionDef {
    def("first_tag", &[Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(0) },
        Uop::Callg {
            callee: k(6),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(xs: list) -> list`: the elements after the first, as a fresh list.
fn tail() -> FunctionDef {
    def("tail", &[Ref, Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kimm { dst: 3, kidx: k(0) },
        Uop::Callg {
            callee: k(4),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(xs: list) -> list`: the plain fresh copy.
fn collect() -> FunctionDef {
    def("collect", &[Ref, Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kimm { dst: 3, kidx: k(0) },
        Uop::Callg {
            callee: k(5),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

fn run_value(vm: &mut Vm, func: &str, args: &[Value]) -> Value {
    let answer = vm.run("gn", func, args).expect("the fixture runs");
    let [word] = answer.as_slice() else {
        panic!("the fixture answers one word")
    };
    *word
}

fn first_string(vm: &mut Vm, list: Value) -> String {
    match run_value(vm, "first", &[list]) {
        Value::Ref(id) => vm
            .objects()
            .str_at(id.id())
            .expect("string elements")
            .to_owned(),
        other => panic!("a string ref, got {other:?}"),
    }
}

#[test]
fn cons_prepends_into_a_fresh_list_over_int_and_string() {
    let mut vm = Vm::new(gn_registry());
    let empty = run_value(&mut vm, "empty", &[]);
    let one = run_value(&mut vm, "cons", &[Value::I64(2), empty]);
    let two = run_value(&mut vm, "cons", &[Value::I64(1), one]);
    assert_eq!(run_value(&mut vm, "first", &[two]), Value::I64(1));
    let rest = run_value(&mut vm, "tail", &[two]);
    assert_eq!(run_value(&mut vm, "first", &[rest]), Value::I64(2));
    // the prepend answers a fresh object, never the operand
    assert_ne!(two, one, "cons constructs, it does not alias");
    // the elements after the first run out: the none member, no trap
    let empty_tail = run_value(&mut vm, "tail", &[rest]);
    assert_eq!(
        run_value(&mut vm, "first", &[empty_tail]),
        Value::Tag(PLATFORM_NONE)
    );
    // the string elements share identity with the interned words
    let a = vm.intern_str("a");
    let b = vm.intern_str("b");
    let one_str = run_value(&mut vm, "cons", &[a, empty]);
    let two_str = run_value(&mut vm, "cons", &[b, one_str]);
    assert_eq!(run_value(&mut vm, "first", &[two_str]), b);
    let tail_str = run_value(&mut vm, "tail", &[two_str]);
    assert_eq!(first_string(&mut vm, tail_str), "a");
}

#[test]
fn first_answers_the_checked_first_element() {
    let mut vm = Vm::new(gn_registry());
    // the empty cases first: list_push mutates in place, so a list bound
    // before the pushes is non-empty after them
    let empty = run_value(&mut vm, "empty", &[]);
    assert_eq!(
        run_value(&mut vm, "first", &[empty]),
        Value::Tag(PLATFORM_NONE)
    );
    assert_eq!(
        run_value(&mut vm, "first_tag", &[empty]),
        Value::Tag(PLATFORM_NONE)
    );
    // the hit cases over a two-element list
    let x = vm.intern_str("x");
    let y = vm.intern_str("y");
    let ys = run_value(&mut vm, "push", &[empty, y]);
    let xs = run_value(&mut vm, "push", &[ys, x]);
    assert_eq!(run_value(&mut vm, "first", &[xs]), y);
    // the raw span shape: the shared ok discriminant on the hit
    assert_eq!(
        run_value(&mut vm, "first_tag", &[xs]),
        Value::Tag(PLATFORM_IO_OK)
    );
}

#[test]
fn tail_and_collect_answer_fresh_copies() {
    let mut vm = Vm::new(gn_registry());
    // the empty cases first: list_push mutates in place
    let empty = run_value(&mut vm, "empty", &[]);
    let empty_tail = run_value(&mut vm, "tail", &[empty]);
    assert_eq!(
        run_value(&mut vm, "first", &[empty_tail]),
        Value::Tag(PLATFORM_NONE)
    );
    let empty_copy = run_value(&mut vm, "collect", &[empty]);
    assert_eq!(
        run_value(&mut vm, "first", &[empty_copy]),
        Value::Tag(PLATFORM_NONE)
    );
    // the copy cases over a two-element list
    let a = vm.intern_str("a");
    let b = vm.intern_str("b");
    let as_list = run_value(&mut vm, "push", &[empty, a]);
    let ab = run_value(&mut vm, "push", &[as_list, b]);
    let rest = run_value(&mut vm, "tail", &[ab]);
    assert_eq!(first_string(&mut vm, rest), "b");
    // collect answers a plain fresh copy: distinct object, same elements
    let copy = run_value(&mut vm, "collect", &[ab]);
    assert_ne!(copy, ab, "the copy is a fresh object");
    let (Value::Ref(copy_id), Value::Ref(ab_id)) = (copy, ab) else {
        unreachable!("list results are refs");
    };
    assert_eq!(
        vm.objects().list_at(copy_id.id()).expect("the copy"),
        vm.objects().list_at(ab_id.id()).expect("the source"),
        "elements shared by identity"
    );
}
