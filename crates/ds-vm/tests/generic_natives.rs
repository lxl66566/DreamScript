//! The five generic container natives past the map block (indices
//! 32-37): `cons` (32), `tail` (34), `collect` (35), `first` (36), `max`
//! (37) — the retired `head` spelling's slot at 33 stays reserved —
//! each addressed through CALLG with one evidence word staged after the
//! real arguments (the container adapters drop it —
//! the container word is one `Ref` whatever the element). The producers
//! answer fresh lists (construction copies), `first` carries
//! the one checked first-element body in `list_get`'s ok/none span
//! shape, and `max` dispatches on the numeric instance the evidence
//! word names — int in i64's total order and float per IEEE, refusing
//! every other word kind. The retain contract's balance pins live in
//! rc_diag.rs under the diag feature.

use ds_vm::{
    Const, ConstFuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_COLLECT, BUILTIN_CONS, BUILTIN_FIRST, BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH,
        BUILTIN_MAX, BUILTIN_TAIL, PLATFORM_IO_OK, PLATFORM_NONE,
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
        builtin(BUILTIN_MAX),                     // k7
        Const::I64(BuiltinInstance::Int as i64),  // k8: the int evidence word
        Const::I64(BuiltinInstance::Float as i64), // k9: the float evidence word
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
            max_of(),
            max_float_of(),
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
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(2),
            nargs: 2,
        },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Ret { src: 2 },
    ])
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

/// `(a, b: i64) -> i64`: the greater of the two staged words, the int
/// instance staged as the evidence word.
fn max_of() -> FunctionDef {
    max_with_evidence("max_of", k(8))
}

/// `(a, b: f64) -> f64`: the float spelling over the one body, the
/// float instance staged as the evidence word.
fn max_float_of() -> FunctionDef {
    max_with_evidence("max_float_of", k(9))
}

/// The shared `max` site: two staged operands plus the named evidence
/// word behind them.
fn max_with_evidence(name: &str, evidence: ConstIdx) -> FunctionDef {
    def(name, &[Raw, Raw, Raw, Raw, Raw], 2, 3, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Kimm {
            dst: 4,
            kidx: evidence,
        },
        Uop::Callg {
            callee: k(7),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
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

#[test]
fn max_orders_int_and_float_and_refuses_the_rest() {
    let mut vm = Vm::new(gn_registry());
    assert_eq!(
        run_value(&mut vm, "max_of", &[Value::I64(3), Value::I64(7)]),
        Value::I64(7)
    );
    assert_eq!(
        run_value(&mut vm, "max_of", &[Value::I64(-2), Value::I64(-5)]),
        Value::I64(-2)
    );
    assert_eq!(
        run_value(&mut vm, "max_float_of", &[
            Value::F64(2.25),
            Value::F64(2.5)
        ]),
        Value::F64(2.5)
    );
    // IEEE: floats never trap, a NaN operand answers the other side
    assert_eq!(
        run_value(&mut vm, "max_float_of", &[
            Value::F64(f64::NAN),
            Value::F64(2.5)
        ]),
        Value::F64(2.5)
    );
    // a non-numeric or mixed pair refuses like a comparison against a
    // mismatched slot — the value-kind branch stays the body's defense
    // under a covered instance
    let a = vm.intern_str("a");
    let b = vm.intern_str("b");
    assert!(matches!(
        vm.run("gn", "max_of", &[a, b]),
        Err(Trap::SlotTypeMismatch { .. })
    ));
    assert!(matches!(
        vm.run("gn", "max_of", &[Value::I64(1), Value::F64(2.0)]),
        Err(Trap::SlotTypeMismatch { .. })
    ));
    // the dispatch is evidence-keyed like `len`'s: an instance the body
    // orders nothing of traps before the operands are read
    let mut reg = gn_registry();
    let body = [
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Kimm { dst: 4, kidx: k(0) }, // the list instance
        Uop::Callg {
            callee: k(7),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ];
    let def = FunctionDef {
        name: "max_bad".into(),
        layout: FrameLayout::new(vec![Raw, Raw, Raw, Raw, Raw], 2, 3).expect("valid layout"),
        pool: pool(),
        code: assemble(&body).expect("fixture body encodes"),
    };
    reg.install(ModuleVersion::new("bad", vec![def]).expect("valid fixture module"));
    let mut other = Vm::new(reg);
    assert!(matches!(
        other.run("bad", "max_bad", &[Value::I64(1), Value::I64(2)]),
        Err(Trap::UnknownInstance { .. })
    ));
}
