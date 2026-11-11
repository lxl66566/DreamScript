//! The nursery joiner re-entry regression: a child that calls
//! `fiber_resume` on the parked joiner's pool id during a
//! `nursery_join`/`nursery_boundary` drive makes the joiner re-enter
//! mid-drive and complete at its entry frame, and the drive's post-pass
//! re-entry must not switch into the now-continuation-less fiber (the
//! shrunk shape of the panic the VM fuzz leg found: `switch_in`'s
//! validated-target expect fired on the dead joiner). The drive surfaces
//! the boundary's answer to its own host instead; every stop below is
//! structured.

use ds_abi::{
    FrameLayout, SlotType,
    uop::{ConstIdx, Inst, Uop},
};
use ds_mod::{Constant, Function, Hash128, Module, TargetClass, read, write};
use ds_vm::{ModuleRegistry, Trap, Value, Vm, native};

fn func(
    name: &str,
    slots: Vec<SlotType>,
    params: usize,
    arg_area: usize,
    pool: Vec<Constant>,
    code: Vec<Inst>,
) -> Function {
    Function::new(
        name.to_owned(),
        FrameLayout::new(slots, params, arg_area).expect("layout"),
        pool,
        code,
    )
    .expect("valid")
}

/// Runs the shrunk shape once: a child whose whole body parks at
/// `fiber_resume(0, 1)` — target 0 is the joiner parked at main's
/// `nursery_boundary` call — and dies after being re-entered by main's
/// own entry-frame `RET`, all before the boundary drive finishes.
fn run_joiner_reentry(callee: u32, tail: Uop) -> Result<Value, Trap> {
    // f0: one body word plus a two-word Raw overlay; the child stages the
    // resume pair (joiner id, payload) and parks at the resume call
    let child = func(
        "f0",
        vec![SlotType::Raw, SlotType::Raw, SlotType::Raw],
        0,
        2,
        vec![
            Constant::I64(0), // the joiner's pool id
            Constant::I64(1), // the resume payload
            Constant::FuncRef {
                module: "builtin".into(),
                func: 25,
            }, // fiber_resume
        ],
        vec![
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kimm {
                dst: 1,
                kidx: ConstIdx(0),
            }
            .into(),
            Uop::Kimm {
                dst: 2,
                kidx: ConstIdx(1),
            }
            .into(),
            Uop::Call {
                callee: ConstIdx(2),
                nargs: 2,
            }
            .into(),
            Uop::Ret { src: 0 }.into(),
        ],
    );
    // main: body word 0 keeps the open nursery's id; overlay 2..=4
    let main = func(
        "main",
        vec![
            SlotType::Raw, // 0: nursery id
            SlotType::Raw, // 1: spare body word
            SlotType::Raw, // 2: overlay word 0
            SlotType::Raw, // 3: overlay word 1
            SlotType::Raw, // 4: overlay word 2
        ],
        0,
        3,
        vec![
            Constant::FuncRef {
                module: String::new(),
                func: 0,
            }, // 0: the f0 word
            Constant::I64(0), // 1
            Constant::I64(1), // 2: boundary mask
            Constant::FuncRef {
                module: "builtin".into(),
                func: 41,
            },
            Constant::FuncRef {
                module: "builtin".into(),
                func: 42,
            },
            Constant::FuncRef {
                module: "builtin".into(),
                func: callee,
            },
        ],
        vec![
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: ConstIdx(3),
                nargs: 0,
            }
            .into(), // nursery_open
            Uop::Mov { dst: 0, src: 2 }.into(), // keep the id
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Kconst {
                dst: 3,
                kidx: ConstIdx(0),
            }
            .into(),
            Uop::Call {
                callee: ConstIdx(4),
                nargs: 2,
            }
            .into(), // spawn f0
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Kimm {
                dst: 3,
                kidx: ConstIdx(2),
            }
            .into(),
            Uop::Call {
                callee: ConstIdx(5),
                nargs: 2,
            }
            .into(), // the seam
            tail.into(), // the joiner's stop
        ],
    );
    let mut module = Module::new("probe", TargetClass::Bytecode, Hash128::compute(b"probe"));
    module = module.with_function(child).with_function(main);
    let bytes = write(&module);
    let back = read(&bytes).expect("the writer's output is canonical");

    let mut registry = ModuleRegistry::new();
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    let mut vm = Vm::with_limit(registry, 128);
    assert!(vm.install_dsmod(&back).is_ok(), "the probe module installs");
    // never a panic: the drive ends on a structured value or trap
    vm.run("probe", "main", &[])
}

#[test]
fn boundary_survives_a_joiner_killing_child_resume() {
    // the boundary drive (nursery_boundary, builtin 43): the child's
    // resume re-enters the parked joiner mid-drive; main dies at its
    // entry Ret before the drive's post-pass re-entry
    let outcome = run_joiner_reentry(43, Uop::Ret { src: 0 });
    let _ = std::hint::black_box(&outcome); // a structured stop, whatever it is
}

#[test]
fn count_join_survives_a_joiner_killing_child_resume() {
    // the count join (nursery_join, builtin 26) shares the re-entry shape
    let outcome = run_joiner_reentry(26, Uop::Ret { src: 0 });
    let _ = std::hint::black_box(&outcome);
}

/// The gone-joiner trap re-raise: the drive's first child traps after
/// killing the joiner (the boundary's re-raised trap then attributes to
/// the dead child the drive ended on, a duplicate of the ledger written
/// at the child's own death — the drained ledger must not panic).
#[test]
fn boundary_survives_a_gone_joiner_trap_re_raise() {
    // f0: resume the parked joiner, then die on a divide by zero
    let f0 = func(
        "f0",
        vec![SlotType::Raw, SlotType::Raw, SlotType::Raw],
        0,
        2,
        vec![Constant::I64(0), Constant::I64(1), Constant::FuncRef {
            module: "builtin".into(),
            func: 25,
        }],
        vec![
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kimm {
                dst: 1,
                kidx: ConstIdx(0),
            }
            .into(),
            Uop::Kimm {
                dst: 2,
                kidx: ConstIdx(1),
            }
            .into(),
            Uop::Call {
                callee: ConstIdx(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm {
                dst: 0,
                kidx: ConstIdx(0),
            }
            .into(),
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            }
            .into(),
        ],
    );
    // f1: lands nothing; the first child's trap raises the cancel flag
    let f1 = func(
        "f1",
        vec![SlotType::Raw],
        0,
        0,
        vec![Constant::I64(0)],
        vec![
            Uop::Funcf { frame_slots: 1 }.into(),
            Uop::Ret { src: 0 }.into(),
        ],
    );
    let main = func(
        "main",
        vec![
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        3,
        vec![
            Constant::FuncRef {
                module: String::new(),
                func: 0,
            }, // 0: the f0 word
            Constant::FuncRef {
                module: String::new(),
                func: 1,
            }, // 1: the f1 word
            Constant::I64(1), // 2: boundary mask
            Constant::FuncRef {
                module: "builtin".into(),
                func: 41,
            },
            Constant::FuncRef {
                module: "builtin".into(),
                func: 42,
            },
            Constant::FuncRef {
                module: "builtin".into(),
                func: 43,
            },
        ],
        vec![
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: ConstIdx(3),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 0, src: 2 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Kconst {
                dst: 3,
                kidx: ConstIdx(0),
            }
            .into(),
            Uop::Call {
                callee: ConstIdx(4),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Kconst {
                dst: 3,
                kidx: ConstIdx(1),
            }
            .into(),
            Uop::Call {
                callee: ConstIdx(4),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Kimm {
                dst: 3,
                kidx: ConstIdx(2),
            }
            .into(),
            Uop::Call {
                callee: ConstIdx(5),
                nargs: 2,
            }
            .into(),
            Uop::Ret { src: 0 }.into(),
        ],
    );
    let mut module = Module::new("probe2", TargetClass::Bytecode, Hash128::compute(b"probe2"));
    module = module
        .with_function(f0)
        .with_function(f1)
        .with_function(main);
    let bytes = write(&module);
    let back = read(&bytes).expect("the writer's output is canonical");

    let mut registry = ModuleRegistry::new();
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    let mut vm = Vm::with_limit(registry, 128);
    assert!(vm.install_dsmod(&back).is_ok(), "the probe module installs");
    // the child's divide-by-zero surfaces at the boundary as a structured
    // trap; the duplicate attribution onto the drained ledger drops
    let result = vm.run("probe2", "main", &[]);
    assert!(
        matches!(result, Err(Trap::DivideByZero)),
        "the trapped child's trap surfaces, got {result:?}"
    );
}
