//! The M4 RC activation under the diag layer (C4): the 0xE words execute
//! over counted heap blocks, and the balance tripwires (hard rc
//! underflow/overflow asserts, the ds-rt diag discipline) guard a fixture
//! exercising refcounts above one — a call-staging dup hands the callee a
//! second owner, the callee's drop returns it, and the heap drains on a
//! scalar-returning program. Only compiled with `--features diag`.

#![cfg(feature = "diag")]

use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: Vec<Inst>) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(&body).expect("fixture body encodes"),
    }
}

/// The fixture module: `main` dups a string into a call, `borrow` drops
/// its handed word at its own exit, `witness` raises a host-staged count
/// above one for direct observation, `killer` decs past one.
fn registry() -> ModuleRegistry {
    let main = def(
        "main",
        &[SlotType::Ref, SlotType::I64, SlotType::Raw],
        0,
        1,
        vec![
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            // the call-staging dup: the callee owns one count, the frame
            // keeps its own
            RcOp::Inc { src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::Kimm { dst: 1, kidx: k(1) }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Ret { src: 1 }.into(),
        ],
    );
    let mut main = main;
    main.pool = vec![Const::Str("shared".into()), Const::I64(0), Const::FuncRef {
        module: "rc".into(),
        func: 1,
    }];
    let borrow = def("borrow", &[SlotType::Ref, SlotType::I64], 1, 1, vec![
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        // the borrow parameter drops the handed count at its exit
        RcOp::Dec { src: 0 }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut borrow = borrow;
    borrow.pool = vec![Const::I64(0)];
    let witness = def("witness", &[SlotType::Ref, SlotType::I64], 1, 0, vec![
        Uop::Funcf { frame_slots: 2 }.into(),
        RcOp::Inc { src: 0 }.into(),
        RcOp::Inc { src: 0 }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut witness = witness;
    witness.pool = vec![Const::I64(0)];
    let killer = def("killer", &[SlotType::Ref, SlotType::I64], 0, 0, vec![
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kstr { dst: 0, kidx: k(0) }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kimm { dst: 1, kidx: k(1) }.into(),
        // the second dec names the freed slot
        RcOp::Dec { src: 0 }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut killer = killer;
    killer.pool = vec![Const::Str("doomed".into()), Const::I64(0)];
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("rc", vec![main, borrow, witness, killer])
            .expect("valid fixture module"),
    );
    reg
}

#[test]
fn a_call_staging_dup_balances_and_the_heap_drains() {
    let mut vm = Vm::new(registry());
    assert_eq!(
        vm.run("rc", "main", &[]),
        Ok(Value::I64(0)),
        "the dup/drop pair balances through the call"
    );
    // main returns a scalar and every claimed count dropped: the freed
    // slot waits for reuse, the heap holds nothing live
    assert!(vm.objects().is_empty(), "the heap drains when the run ends");
}

#[test]
fn a_raised_count_is_observable_and_returns_to_one() {
    let mut vm = Vm::new(registry());
    let text = vm.intern_str("x"); // the host's claim: rc 1
    assert_eq!(
        vm.run("rc", "witness", &[text]),
        Ok(Value::I64(0)),
        "the raised counts execute"
    );
    let Value::Ref(id) = text else {
        panic!("an interned string ref");
    };
    // two incs and one dec over the host's single claim: 3 - 1 = 2, and
    // the object is alive to tell about it
    assert_eq!(
        vm.objects().rc_at(id),
        Ok(2),
        "the count rose above one and settled at two"
    );
}

#[test]
fn a_dec_naming_a_freed_object_traps() {
    let mut vm = Vm::new(registry());
    assert_eq!(
        vm.run("rc", "killer", &[]),
        Err(Trap::UnknownObject { obj: 0 }),
        "the second dec names the freed slot"
    );
}
