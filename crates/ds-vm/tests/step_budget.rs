//! The budgeted run's step charge ([`Vm::run_with_budget`], the comptime
//! fence's runaway bound): the fueled instantiation of the dispatch loop
//! answers [`Trap::OutOfFuel`] when a drive consumes its budget, while
//! the unbounded [`Vm::run`] and every ordinary drive keep their dispatch
//! and behavior untouched — a user's runtime loop is the program's own.

use ds_mod::{Constant, Function, Hash128, Module, TargetClass, read, write};
use ds_vm::{
    FrameAnswer, ModuleRegistry, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64},
        uop::Uop,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn layout(slots: &[SlotType], params: usize, args: usize) -> FrameLayout {
    FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout")
}

/// `spin()`: i = 0; loop { i += 1 } — never terminates, its stack flat,
/// so only the step budget answers it.
fn spin_fn() -> Function {
    Function::new(
        "spin",
        layout(&[I64, I64], 0, 0),
        vec![Constant::I64(0), Constant::I64(1)],
        vec![
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmp { target: 3 },
        ],
    )
    .expect("valid function")
}

/// `count(n)`: acc = 0; for i = 0; i <= n; i += 1 { acc += i }; acc —
/// the ordinary loop shape both run entries drive.
fn count_fn(limit: i64) -> Function {
    Function::new(
        "count",
        layout(&[I64, I64, Bool, I64, I64], 0, 0),
        vec![Constant::I64(0), Constant::I64(1), Constant::I64(limit)],
        vec![
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Kimm { dst: 3, kidx: k(1) },
            Uop::Kimm { dst: 4, kidx: k(2) },
            Uop::LtI64 {
                dst: 2,
                lhs: 4,
                rhs: 0,
            },
            Uop::Jmpif {
                cond: 2,
                target: 10,
            },
            Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 0,
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 3,
            },
            Uop::Jmp { target: 5 },
            Uop::Ret { src: 1 },
        ],
    )
    .expect("valid function")
}

fn vm_over(functions: Vec<Function>) -> Vm {
    let registry = ModuleRegistry::new();
    let mut vm = Vm::new(registry);
    let mut module = Module::new(
        "budget",
        TargetClass::Bytecode,
        Hash128::compute(b"step budget"),
    );
    for function in functions {
        module = module.with_function(function);
    }
    vm.install_dsmod(read(&write(&module)).expect("the module round-trips"))
        .expect("the module installs");
    vm
}

#[test]
fn a_runaway_drive_answers_the_fuel_trap() {
    let mut vm = vm_over(vec![spin_fn()]);
    assert_eq!(
        vm.run_with_budget("budget", "spin", &[], 1_000),
        Err(Trap::OutOfFuel),
        "the runaway loop exhausts the budget instead of hanging"
    );
}

/// The fueled charge counts decoded instructions, so a budget tighter
/// than one straight-line run answers the same trap.
#[test]
fn a_budget_below_one_run_refuses_the_entry() {
    let mut vm = vm_over(vec![count_fn(100)]);
    assert_eq!(
        vm.run_with_budget("budget", "count", &[], 3),
        Err(Trap::OutOfFuel),
        "the budget runs out before the body finishes"
    );
}

#[test]
fn a_generous_budget_drives_to_completion() {
    let mut vm = vm_over(vec![count_fn(100)]);
    assert_eq!(
        vm.run_with_budget("budget", "count", &[], 1 << 32),
        Ok(FrameAnswer::one(Value::I64(5050))),
        "the ordinary loop answers under a generous budget"
    );
}

/// The budget is per drive, never a property of the code: after a
/// fueled run answers its trap, the unbounded `run` drives an ordinary
/// loop to completion on the same VM, and a second budgeted drive
/// starts from its own fresh budget.
#[test]
fn the_budget_leaves_the_unbounded_run_untouched() {
    let mut vm = vm_over(vec![spin_fn(), count_fn(100)]);
    assert_eq!(
        vm.run_with_budget("budget", "spin", &[], 10),
        Err(Trap::OutOfFuel),
    );
    assert_eq!(
        vm.run("budget", "count", &[]),
        Ok(FrameAnswer::one(Value::I64(5050))),
        "the ordinary run keeps its unbounded drive"
    );
    assert_eq!(
        vm.run_with_budget("budget", "count", &[], 1 << 32),
        Ok(FrameAnswer::one(Value::I64(5050))),
        "a later budgeted drive sees a fresh budget"
    );
}
