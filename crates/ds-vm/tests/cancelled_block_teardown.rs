//! A cancel-cut owner's own block nursery tears down at the cut: the cancel-cut door runs the same
//! release fixpoint the release doors run, so a cancelled block's
//! children never hold a wait past the cut — the race nursery's prompt
//! close, generalized over the ordinary blocks. What the fixture pins:
//!
//! - a child of the enclosing nursery that owns a nested block is cut at its park when a first
//!   sibling's trap raises the flag, and the nested block's slot closes and its never-started child
//!   releases cold at that cut, before any release door runs.
//! - the first failure still counts at the enclosing join, and after the run the pool holds no open
//!   nursery and no live fiber: nothing of the cancelled block survives the cut.

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{ConstIdx, FrameLayout, SlotType, uop::Uop},
    assemble,
    fiber::{FiberId, FiberState},
    native::{
        BUILTIN_CHAN_NEW, BUILTIN_CHAN_RECV, BUILTIN_NURSERY_JOIN, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    },
    nursery::{NurseryId, NurseryState},
    platform::{Capability, OP_SLEEP, Platform, STD_NAME},
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

fn std_sleep() -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: STD_NAME.into(),
        func: OP_SLEEP,
    })
}

fn def(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    pool: Vec<Const>,
    body: &[Uop],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// `trapper() -> int`: one short sleep, then a division by zero — the
/// first failure whose trap raises the enclosing nursery's flag while the
/// sibling sits parked.
fn trapper_def() -> FunctionDef {
    def(
        "trapper",
        &[SlotType::I64, SlotType::I64, SlotType::I64],
        0,
        1,
        vec![
            std_sleep(),
            Const::I64(20_000_000),
            Const::I64(1),
            Const::I64(0),
        ],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Kimm { dst: 0, kidx: k(2) },
            Uop::Kimm { dst: 1, kidx: k(3) },
            Uop::DivI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )
}

/// `blocker() -> int`: opens its own block nursery, spawns a child into
/// it, then parks on a long sleep — the park the raised flag cuts, with
/// the block still standing.
fn blocker_def() -> FunctionDef {
    def(
        "blocker",
        &[SlotType::I64, SlotType::Raw, SlotType::Raw],
        0,
        2,
        vec![
            std_sleep(),                    // k0
            Const::I64(2_000_000_000),      // k1: the long park
            builtin(BUILTIN_NURSERY_OPEN),  // k2
            builtin(BUILTIN_NURSERY_SPAWN), // k3
            Const::FuncRef(ConstFuncRef::Named {
                module: "cb".into(),
                func: 3, // the block child
            }), // k4
        ],
        &[
            Uop::Funcf { frame_slots: 3 },
            // the nested block's open: the id lands on body slot 0
            Uop::Call {
                callee: k(2),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 1 },
            // the block child's spawn: (nursery, callable) at the overlay
            Uop::Mov { dst: 1, src: 0 },
            Uop::Kconst { dst: 2, kidx: k(4) },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            // the long park the flag cuts
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `block_child() -> int`: builds its own channel and parks on its empty
/// recv — the wait a prompt teardown cancels at the cut.
fn block_child_def() -> FunctionDef {
    def(
        "block_child",
        &[SlotType::Ref, SlotType::Raw],
        0,
        1,
        vec![
            builtin(BUILTIN_CHAN_NEW),  // k0
            builtin(BUILTIN_CHAN_RECV), // k1
            Const::I64(1),              // k2
        ],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(2) },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `main(cap: int) -> int`: the enclosing nursery open, the trapper
/// spawned first (the first failure in spawn order), the blocker spawned
/// second, then the scope-exit join answering the trapped-children
/// count. The fixture's one channel word exists for the layout's Ref
/// slot alone; nothing parks on it.
fn main_def() -> FunctionDef {
    def(
        "main",
        &[
            SlotType::I64, // 0: the nursery id
            SlotType::Ref, // 1: the unused channel
            SlotType::Fn,  // 2: the trapper word
            SlotType::Fn,  // 3: the blocker word
            SlotType::Raw, // 4
            SlotType::Raw, // 5: overlay arg 0
            SlotType::Raw, // 6: overlay arg 1
        ],
        1,
        2,
        vec![
            builtin(BUILTIN_CHAN_NEW),      // k0
            builtin(BUILTIN_NURSERY_OPEN),  // k1
            builtin(BUILTIN_NURSERY_SPAWN), // k2
            builtin(BUILTIN_NURSERY_JOIN),  // k3
            Const::FuncRef(ConstFuncRef::Named {
                module: "cb".into(),
                func: 1, // the trapper
            }), // k4
            Const::FuncRef(ConstFuncRef::Named {
                module: "cb".into(),
                func: 2, // the blocker
            }), // k5
        ],
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 1, src: 5 },
            Uop::Call {
                callee: k(1),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 5 },
            Uop::Kconst { dst: 2, kidx: k(4) },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Mov { dst: 6, src: 2 },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Kconst { dst: 3, kidx: k(5) },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Mov { dst: 6, src: 3 },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Call {
                callee: k(3),
                nargs: 1,
            },
            Uop::Ret { src: 5 },
        ],
    )
}

fn registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[Capability::Time]));
    reg.install(
        ModuleVersion::new("cb", vec![
            main_def(),
            trapper_def(),
            blocker_def(),
            block_child_def(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// A reactor-driver host parks the sleeps, so the flag cuts the parked
/// blocker at its resume point; a blocking-twin host never parks (the
/// drive's own sleep blocks), so the parked-cut shape cannot arise there.
fn scheduled_vm() -> Option<Vm> {
    let mut vm = Vm::new(registry());
    ds_vm::reactor::platform_default().map(|reactor| {
        vm.install_reactor(reactor);
        vm
    })
}

/// The cut door closes the cut owner's nested block: the wheel fires the
/// trapper's short sleep, its trap raises the flag, the sweep cuts the
/// parked blocker at its park, and the cut tears the nested block down —
/// its slot Dead and its never-started child released cold — before any
/// release door. The first failure still raises at the enclosing join.
#[test]
fn a_cancelled_block_closes_at_the_cut_not_at_a_release_door() {
    // the parked-cut shape needs the reactor: the sleeps must park
    let Some(mut vm) = scheduled_vm() else {
        return;
    };
    // the scope-exit join answers the trapped-children count: the trapper's
    // trap is the boundary's one failure (the blocker's cut is a
    // cancellation, never a count)
    let run = vm.run("cb", "main", &[Value::I64(1)]);
    assert_eq!(
        run,
        Ok(FrameAnswer::one(Value::I64(1))),
        "the first failure counted at the enclosing join"
    );
    // every spawned fiber reached a death: the trapper walked at its
    // trap, the blocker cut at its park, the block child released cold —
    // its spawn riding the first free slot (the walked trapper's own, or
    // the pool's tail) per the reuse door
    for slot in 1..=3u32 {
        assert_eq!(
            vm.fiber_state(FiberId(slot)),
            Some(FiberState::Dead),
            "pool slot {slot} holds no live fiber past the cut"
        );
    }
    // nursery slots: the enclosing block 0, the nested block 1
    assert_eq!(
        vm.nursery_state(NurseryId(0)),
        Some(NurseryState::Dead),
        "the enclosing block consumed at the join"
    );
    assert_eq!(
        vm.nursery_state(NurseryId(1)),
        Some(NurseryState::Dead),
        "the cancelled block closed at the cut, not at a release door"
    );
}
