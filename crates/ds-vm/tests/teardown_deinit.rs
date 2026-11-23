//! The E5 teardown deinit pass: a fiber dying at a drain door (the
//! cancel-cut preemption, the nursery walk's interrupt marking, the root
//! reset) fires its kept frames' deinit hooks exactly once, in a
//! deterministic order, through the hosted live drive — and a teardown
//! hook that parks, dies, observes a raised cancel flag, or calls a
//! nursery control seam refuses without hanging or killing the walk. Each
//! fixture hand-assembles the drop shape the placement pass emits (`MOV`
//! staging run + `CALL` of the module's own `$deinit` function), which is
//! also what the runtime derives the claim directory from. The band-gated
//! pins add the establishment marker the lowered shape carries: the
//! `TagNew` clear riding right after the hook call (the reserved band
//! tags, mirrored from crates/ds-vm/src/claims.rs), whose body word the
//! establishment sets — the doors' witness for claims no ledger count can
//! name.

use ds_vm::{
    Const, ConstFuncRef, FIBER_ROOT, FiberId, FrameAnswer, FunctionDef, ModuleRegistry,
    ModuleVersion, Platform, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble, native,
};

/// The reserved establishment-band tags (crates/ds-vm/src/claims.rs):
/// hand-assembled fixtures spell them literally, exactly like the pool
/// indices and FuncRefs they stage around.
const BAND_SET: u8 = u8::MAX - 8;
const BAND_CLEAR: u8 = u8::MAX - 9;

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// The observer hook `res$deinit`: one borrowed `Ref` parameter (the
/// dying value's span), printed through the platform console, its
/// hand-off count balanced at the exit, answering a scalar word.
fn observer_deinit() -> FunctionDef {
    let mut hook = def(
        "res$deinit",
        &[SlotType::Ref, SlotType::Bool, SlotType::Raw],
        1,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Kconst { dst: 1, kidx: k(1) }.into(),
            Uop::Ret { src: 1 }.into(),
        ],
    );
    hook.pool = vec![
        Const::FuncRef(ConstFuncRef::Named {
            module: "std".into(),
            func: 0,
        }),
        Const::Bool(false),
    ];
    hook
}

fn registry(version: ModuleVersion) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install_native(native::BUILTIN_MODULE, native::builtin_module());
    reg.install_platform(Platform::std());
    reg.install(version);
    let mut vm = Vm::new(reg);
    vm.capture_output();
    vm
}

/// The cancel-cut door: the flag raiser cancels first, the cut child is
/// cut at its first taken backward branch holding an established deinit
/// value (its own interned word, count on its ledger), and the hook fires
/// at the cut — exactly once, the captured output naming the value —
/// while every heap still drains (the hook's hand-off count balances
/// inside the drive).
#[test]
fn a_cancel_cut_child_fires_its_kept_deinit_at_the_cut() {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    // cut_parent: spawn the flag raiser first and the cut child second
    // (spawn order decides the drive order), then boundary with a
    // containing mask
    let parent = def(
        "cut_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    // [0] open, [1] spawn, [2] boundary, [4] the flag raiser, [5] the cut
    // child, [6] the landing answer, [7] the mask
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
        Const::I64(0),
        Const::I64(1),
    ];

    // flag_child: cancel immediately — the trapped death raises the flag
    let flag_child = def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::TagNew { dst: 0, tag: 0 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut flag_child = flag_child;
    flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

    // cut_child: establish the deinit value (one Ref word at slot 1), then
    // loop on a taken backward branch; the drop shape rides unreachable
    // past the RET — the directory's source, never executed before the cut
    let cut_child = def(
        "cut_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::I64,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::Kconst { dst: 2, kidx: k(1) }.into(),
            Uop::Jmpif { cond: 2, target: 3 }.into(),
            Uop::Ret { src: 3 }.into(),
            Uop::Mov { dst: 4, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    // [0] the value's word, [1] the loop condition, [2] the hook, [3] the
    // unreachable answer
    cut_child.pool = vec![
        Const::Str("cut value".into()),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 3,
        }),
        Const::I64(0),
    ];

    let version = ModuleVersion::new("dwh", vec![
        parent,
        flag_child,
        cut_child,
        observer_deinit(),
    ])
    .expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dwh", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the boundary unions the cancel out and the parent lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"cut value\n",
        "the cut fired the kept deinit exactly once"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the parent's heap drains"
    );
    assert!(
        vm.objects_at(FiberId(2)).is_empty(),
        "the hook's hand-off count balanced and the cut drained the value — the child heap drains"
    );
}

/// The interrupt door: a child dying trapped mid-drive (frames kept) is
/// walked dead by the nursery join, and the walk fires its kept frames'
/// deinit at the marking — the hosted drive takes over the parked
/// context, runs the hook, and the drain still empties every heap.
#[test]
fn a_trapped_child_fires_its_kept_deinit_at_the_death_walk() {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    let parent = def(
        "trap_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(3) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(4) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::I64(0),
        Const::I64(1),
    ];

    // trap_child: establish the deinit value, then cancel — the call never
    // answers, the drive ends trapped mid-frame with the value live; the
    // drop shape rides unreachable past the RET
    let child = def(
        "trap_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Tag,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::TagNew { dst: 2, tag: 0 }.into(),
            Uop::Mov { dst: 4, src: 2 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 4 }.into(),
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
        ],
    );
    let mut child = child;
    // [0] the kept value, [1] cancel, [2] the hook
    child.pool = vec![
        Const::Str("trapped value".into()),
        builtin(BUILTIN_NURSERY_CANCEL),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
    ];

    let version =
        ModuleVersion::new("dwh", vec![parent, child, observer_deinit()]).expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dwh", "trap_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the boundary unions the cancel out and the parent lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"trapped value\n",
        "the death walk fired the trapped child's kept deinit exactly once"
    );
    assert!(
        vm.objects_at(FiberId(1)).is_empty(),
        "the walk drained the trapped child's heap after the hook"
    );
}

/// The drive rule: a teardown hook that dies (`HALT`) refuses — its
/// effects stand (here: none), the pass ends at it, the remaining hooks'
/// claims drain unfired, and no heap leaks.
#[test]
fn a_teardown_hook_that_dies_refuses_and_the_drain_still_empties() {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    let parent = def(
        "cut_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
        Const::I64(0),
        Const::I64(1),
    ];

    let flag_child = def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::TagNew { dst: 0, tag: 0 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut flag_child = flag_child;
    flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

    let cut_child = def(
        "cut_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::I64,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::Kconst { dst: 2, kidx: k(1) }.into(),
            Uop::Jmpif { cond: 2, target: 3 }.into(),
            Uop::Ret { src: 3 }.into(),
            Uop::Mov { dst: 4, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    cut_child.pool = vec![
        Const::Str("never printed".into()),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 3,
        }),
        Const::I64(0),
    ];

    // halt$deinit: the hook that dies instead of returning — its `HALT`
    // ends the drive, the hosted call refuses, the pass ends
    let halt_hook = def("res$deinit", &[SlotType::Ref], 1, 0, &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Halt { slot: 0 }.into(),
    ]);

    let version = ModuleVersion::new("dwh", vec![parent, flag_child, cut_child, halt_hook])
        .expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dwh", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the refused hook ends the pass and the boundary still lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the dying hook printed nothing and no hook ran twice"
    );
    assert!(
        vm.objects_at(FiberId(2)).is_empty(),
        "the refused drive's staged claims drained with the cut"
    );
}

/// The drive rule under cancellation: a teardown hook whose body loops
/// observes the raised cancel flag at its first backward branch and the
/// drive refuses there — the cut never re-enters the nursery through the
/// hook, and the teardown stays bounded (no hang, heaps drain).
#[test]
fn a_looping_teardown_hook_under_a_raised_flag_refuses_at_the_back_edge() {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    let parent = def(
        "cut_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
        Const::I64(0),
        Const::I64(1),
    ];

    let flag_child = def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::TagNew { dst: 0, tag: 0 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut flag_child = flag_child;
    flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

    let cut_child = def(
        "cut_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::I64,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::Kconst { dst: 2, kidx: k(1) }.into(),
            Uop::Jmpif { cond: 2, target: 3 }.into(),
            Uop::Ret { src: 3 }.into(),
            Uop::Mov { dst: 4, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    cut_child.pool = vec![
        Const::Str("loop value".into()),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 3,
        }),
        Const::I64(0),
    ];

    // loop$deinit (named for the scan): a hook whose body is one infinite
    // backward self-jump — under the raised flag the drive refuses at the
    // first back edge
    let loop_hook = def("res$deinit", &[SlotType::Ref, SlotType::Bool], 1, 0, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kconst { dst: 1, kidx: k(0) }.into(),
        Uop::Jmpif { cond: 1, target: 1 }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut loop_hook = loop_hook;
    loop_hook.pool = vec![Const::Bool(true)];

    let version = ModuleVersion::new("dwh", vec![parent, flag_child, cut_child, loop_hook])
        .expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dwh", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the looping hook refused at its back edge and the teardown stayed bounded"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the looping hook never completed a body"
    );
    assert!(
        vm.objects_at(FiberId(2)).is_empty(),
        "the refused drive's claims drained with the cut"
    );
}

/// The drive rule's nursery half: a teardown hook that spawns refuses —
/// the spawned child would live outside every standing drive's child
/// snapshot, and the spawn's pool-slot recycle is a door. The already-run
/// effects stand (the hook's leading print), the pass ends at the spawn
/// call, the later directory site's claim drains unfired, and every heap
/// still empties.
#[test]
fn a_teardown_hook_that_spawns_refuses_and_the_drain_still_empties() {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    let parent = def(
        "cut_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
        Const::I64(0),
        Const::I64(1),
    ];

    let flag_child = def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::TagNew { dst: 0, tag: 0 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut flag_child = flag_child;
    flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

    // two established values: the first's hook spawns (it refuses, ending
    // the pass), the second's is the ordinary observer — its claim drains
    // unfired, pinning that the pass ended at the refusal and not before
    let cut_child = def(
        "cut_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::I64,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 6 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::Kstr { dst: 2, kidx: k(1) }.into(),
            Uop::Kconst { dst: 3, kidx: k(2) }.into(),
            Uop::Jmpif { cond: 3, target: 4 }.into(),
            Uop::Ret { src: 4 }.into(),
            Uop::Mov { dst: 5, src: 1 }.into(),
            Uop::Call {
                callee: k(3),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
            Uop::Mov { dst: 5, src: 2 }.into(),
            Uop::Call {
                callee: k(4),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 2 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    cut_child.pool = vec![
        Const::Str("spawn value".into()),
        Const::Str("second value".into()),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 3,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 4,
        }),
        Const::I64(0),
    ];

    // spawn$deinit: prints, then spawns into the cut child's own open
    // nursery (id 0, still open mid-drive) — the seam refuses there
    let spawn_hook = def(
        "spawn$deinit",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::Raw,
            SlotType::Raw,
        ],
        1,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 1, kidx: k(1) }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Kimm { dst: 3, kidx: k(3) }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kstr { dst: 1, kidx: k(5) }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Kconst { dst: 2, kidx: k(6) }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut spawn_hook = spawn_hook;
    spawn_hook.pool = vec![
        Const::FuncRef(ConstFuncRef::Named {
            module: "std".into(),
            func: 0,
        }),
        Const::Str("before spawn".into()),
        builtin(BUILTIN_NURSERY_SPAWN),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 5,
        }),
        Const::Str("after spawn".into()),
        Const::Bool(false),
    ];

    let plain = def("plain_child", &[SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kimm { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut plain = plain;
    plain.pool = vec![Const::I64(0)];

    let version = ModuleVersion::new("dwh", vec![
        parent,
        flag_child,
        cut_child,
        spawn_hook,
        observer_deinit(),
        plain,
    ])
    .expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dwh", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the refused spawn ends the pass and the boundary still lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"before spawn\n",
        "the hook's already-run print stands; the spawn and everything past it refused, and the \
         second claim never fired"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the parent's heap drains"
    );
    assert!(
        vm.objects_at(FiberId(2)).is_empty(),
        "both values' claims drained behind the refused pass"
    );
}

/// The drive rule's nursery half, the join shape: a teardown hook that
/// joins another open nursery refuses — the join would drive that
/// nursery's children to death inside the teardown drive, their
/// death-walk doors firing under the pass. The hook's leading print
/// stands, the driven-side child never runs under the drive, the hook's
/// tail never runs, and the heaps still drain.
#[test]
fn a_teardown_hook_that_joins_refuses_and_the_drive_stays_silent() {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_JOIN,
        BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN,
    };

    // nursery one (id 0): the flag raiser then the cut child; nursery two
    // (id 1): one plain child nobody but the hook's join would drive
    let parent = def(
        "cut_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            // the second nursery: open (the id lands on staging word 0),
            // then one plain child spawned into it
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Kconst { dst: 4, kidx: k(8) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
        Const::I64(0),
        Const::I64(1),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 4,
        }),
    ];

    let flag_child = def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::TagNew { dst: 0, tag: 0 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut flag_child = flag_child;
    flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

    let cut_child = def(
        "cut_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::I64,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::Kconst { dst: 2, kidx: k(1) }.into(),
            Uop::Jmpif { cond: 2, target: 3 }.into(),
            Uop::Ret { src: 3 }.into(),
            Uop::Mov { dst: 4, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    cut_child.pool = vec![
        Const::Str("join value".into()),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 3,
        }),
        Const::I64(0),
    ];

    // join$deinit: prints, then joins the second nursery (id 1) — the
    // seam refuses there, before the park, so the dying fiber's takeover
    // never suspends mid-pass
    let join_hook = def(
        "join$deinit",
        &[SlotType::Ref, SlotType::Ref, SlotType::Bool, SlotType::Raw],
        1,
        1,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 1, kidx: k(1) }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Kimm { dst: 3, kidx: k(3) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::Kstr { dst: 1, kidx: k(4) }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Kconst { dst: 2, kidx: k(5) }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut join_hook = join_hook;
    join_hook.pool = vec![
        Const::FuncRef(ConstFuncRef::Named {
            module: "std".into(),
            func: 0,
        }),
        Const::Str("before join".into()),
        builtin(BUILTIN_NURSERY_JOIN),
        Const::I64(1),
        Const::Str("after join".into()),
        Const::Bool(false),
    ];

    // the second nursery's child: it prints when driven — only the hook's
    // join would drive it inside the pass, so its silence pins the refusal
    let plain = def(
        "plain_child",
        &[SlotType::Ref, SlotType::I64, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            Uop::Kimm { dst: 1, kidx: k(2) }.into(),
            Uop::Ret { src: 1 }.into(),
        ],
    );
    let mut plain = plain;
    plain.pool = vec![
        Const::Str("b ran".into()),
        Const::FuncRef(ConstFuncRef::Named {
            module: "std".into(),
            func: 0,
        }),
        Const::I64(0),
    ];

    let version = ModuleVersion::new("dwh", vec![parent, flag_child, cut_child, join_hook, plain])
        .expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dwh", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the refused join ends the pass and the boundary still lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"before join\n",
        "the join refused before any nursery re-entry: the second nursery's child never ran under \
         the drive and the hook's tail never ran"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the parent's heap drains"
    );
    assert!(
        vm.objects_at(FiberId(2)).is_empty(),
        "the refused pass drained the cut child's claims"
    );
    assert!(
        vm.objects_at(FiberId(3)).is_empty(),
        "the never-driven child holds nothing"
    );
}

/// The root reset door: a root drive that traps holding a live deinit
/// value keeps its frames; the next run's reset fires the hook before
/// discarding them, once, and the root heap drains — the reset closes the
/// leak instead of deferring it to the VM's drop.
#[test]
fn a_trapped_root_fires_its_kept_deinit_at_the_next_runs_reset() {
    // boom_root: establish the deinit value, then trap on a zero divide;
    // the drop shape rides unreachable past the RET
    let boom = def(
        "boom_root",
        &[SlotType::Ref, SlotType::I64, SlotType::I64, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Kimm { dst: 1, kidx: k(2) }.into(),
            Uop::DivI64 {
                dst: 2,
                lhs: 1,
                rhs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
        ],
    );
    let mut boom = boom;
    boom.pool = vec![
        Const::Str("root value".into()),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::I64(0),
    ];

    let ok = def("ok_root", &[SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kimm { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut ok = ok;
    ok.pool = vec![Const::I64(7)];

    let version =
        ModuleVersion::new("dwh", vec![boom, observer_deinit(), ok]).expect("valid module");
    let mut vm = registry(version);
    assert!(
        vm.run("dwh", "boom_root", &[]).is_err(),
        "the root traps on the zero divide"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the trap itself fires nothing — the frames wait for the reset"
    );
    assert_eq!(
        vm.run("dwh", "ok_root", &[]),
        Ok(FrameAnswer::one(Value::I64(7))),
        "the next run lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"root value\n",
        "the reset fired the trapped root's kept deinit exactly once"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the reset drained the trapped root's heap after the hook"
    );
}

/// Determinism: two deinit values held by one dying frame fire in the
/// directory's order (drop-site stream order), and the order — with the
/// free order behind it — is identical across repeated runs.
#[test]
fn two_kept_deinits_fire_in_directory_order_across_repeated_runs() {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    fn once() -> Vm {
        let parent = def(
            "cut_parent",
            &[
                SlotType::Raw,
                SlotType::Raw,
                SlotType::Raw,
                SlotType::Raw,
                SlotType::Raw,
            ],
            0,
            2,
            &[
                Uop::Funcf { frame_slots: 5 }.into(),
                Uop::Call {
                    callee: k(0),
                    nargs: 0,
                }
                .into(),
                Uop::Mov { dst: 1, src: 3 }.into(),
                Uop::Mov { dst: 3, src: 1 }.into(),
                Uop::Kconst { dst: 4, kidx: k(4) }.into(),
                Uop::Call {
                    callee: k(1),
                    nargs: 2,
                }
                .into(),
                Uop::Mov { dst: 3, src: 1 }.into(),
                Uop::Kconst { dst: 4, kidx: k(5) }.into(),
                Uop::Call {
                    callee: k(1),
                    nargs: 2,
                }
                .into(),
                Uop::Mov { dst: 3, src: 1 }.into(),
                Uop::Kimm { dst: 4, kidx: k(7) }.into(),
                Uop::Call {
                    callee: k(2),
                    nargs: 2,
                }
                .into(),
                Uop::Kimm { dst: 4, kidx: k(6) }.into(),
                Uop::Ret { src: 4 }.into(),
            ],
        );
        let mut parent = parent;
        parent.pool = vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            Const::I64(0),
            Const::FuncRef(ConstFuncRef::Named {
                module: "dwh".into(),
                func: 1,
            }),
            Const::FuncRef(ConstFuncRef::Named {
                module: "dwh".into(),
                func: 2,
            }),
            Const::I64(0),
            Const::I64(1),
        ];

        let flag_child = def(
            "flag_child",
            &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
            0,
            1,
            &[
                Uop::Funcf { frame_slots: 3 }.into(),
                Uop::TagNew { dst: 0, tag: 0 }.into(),
                Uop::Mov { dst: 2, src: 0 }.into(),
                Uop::Call {
                    callee: k(0),
                    nargs: 1,
                }
                .into(),
                Uop::Ret { src: 2 }.into(),
            ],
        );
        let mut flag_child = flag_child;
        flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

        // two established values (slots 1 and 2), two drop shapes in a
        // fixed stream order — second before first — pinning that the fire
        // order follows the directory, not the slot numbering
        let cut_child = def(
            "cut_child",
            &[
                SlotType::Ref,
                SlotType::Ref,
                SlotType::Ref,
                SlotType::Bool,
                SlotType::I64,
                SlotType::Raw,
            ],
            0,
            1,
            &[
                Uop::Funcf { frame_slots: 6 }.into(),
                Uop::Kstr { dst: 1, kidx: k(0) }.into(),
                Uop::Kstr { dst: 2, kidx: k(1) }.into(),
                Uop::Kconst { dst: 3, kidx: k(2) }.into(),
                Uop::Jmpif { cond: 3, target: 4 }.into(),
                Uop::Ret { src: 4 }.into(),
                Uop::Mov { dst: 5, src: 2 }.into(),
                Uop::Call {
                    callee: k(3),
                    nargs: 1,
                }
                .into(),
                RcOp::Dec { src: 2 }.into(),
                Uop::Mov { dst: 5, src: 1 }.into(),
                Uop::Call {
                    callee: k(3),
                    nargs: 1,
                }
                .into(),
                RcOp::Dec { src: 1 }.into(),
            ],
        );
        let mut cut_child = cut_child;
        cut_child.pool = vec![
            Const::Str("first".into()),
            Const::Str("second".into()),
            Const::Bool(true),
            Const::FuncRef(ConstFuncRef::Named {
                module: "dwh".into(),
                func: 3,
            }),
            Const::I64(0),
        ];

        let version = ModuleVersion::new("dwh", vec![
            parent,
            flag_child,
            cut_child,
            observer_deinit(),
        ])
        .expect("valid module");
        let mut vm = registry(version);
        assert_eq!(
            vm.run("dwh", "cut_parent", &[]),
            Ok(FrameAnswer::one(Value::I64(0))),
            "the boundary lands"
        );
        vm
    }

    let expected = b"second\nfirst\n";
    for _ in 0..5 {
        let vm = once();
        assert_eq!(
            vm.captured_output(),
            expected,
            "both kept deinits fired, in directory order, identically per run"
        );
        assert!(
            vm.objects_at(FiberId(2)).is_empty(),
            "both values drained behind their hooks"
        );
    }
}

// ---- the establishment band (the exactness residual of todo 25) --------------

/// The cut-door frame shared by the band pins: the flag raiser and the
/// cut child spawn into one nursery (spawn order decides the drive
/// order), the boundary unions the cancel out — then a second nursery's
/// spawn recycles the cut child's pool slot, so the recycle door walks
/// the same kept frames the cut already fired. `extra` lands at function
/// index 5 for fixtures whose cut child calls on (the mover pin).
fn band_cut_module(cut_child: FunctionDef, hook: FunctionDef, extra: Option<FunctionDef>) -> Vm {
    use native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    let parent = def(
        "cut_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            // nursery one: the flag raiser first, the cut child second
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            // nursery two: one plain child, whose spawn recycles the cut
            // child's slot and fires the recycle door over its frames
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(8) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            // the second boundary joins clean, so its collected answers
            // list rides overlay word 1 (slot 4): the hand parent takes
            // the channel's discard dec before answering
            RcOp::Dec { src: 4 }.into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    // [0] open, [1] spawn, [2] boundary, [4] the flag raiser, [5] the cut
    // child, [6] the answer, [7] the mask, [8] the plain child
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
        Const::I64(0),
        Const::I64(1),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 4,
        }),
    ];

    let flag_child = def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::TagNew { dst: 0, tag: 0 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut flag_child = flag_child;
    flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

    let plain = def("plain_child", &[SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kimm { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut plain = plain;
    plain.pool = vec![Const::I64(0)];

    let mut functions = vec![parent, flag_child, cut_child, hook, plain];
    if let Some(extra) = extra {
        functions.push(extra);
    }
    let version = ModuleVersion::new("dwh", functions).expect("valid module");
    registry(version)
}

/// The observer hook for an int-only span: one borrowed I64 parameter,
/// printing a fixed marker through the platform console — the fire is
/// the observable, and a Ref-free span is exactly what no ledger count
/// can witness.
fn marker_deinit() -> FunctionDef {
    let mut hook = def(
        "res$deinit",
        &[SlotType::I64, SlotType::Ref, SlotType::Bool, SlotType::Raw],
        1,
        1,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 1, kidx: k(1) }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Kconst { dst: 2, kidx: k(2) }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    hook.pool = vec![
        Const::FuncRef(ConstFuncRef::Named {
            module: "std".into(),
            func: 0,
        }),
        Const::Str("counted value".into()),
        Const::Bool(false),
    ];
    hook
}

/// An int-only claim (no `Ref` word ever enters the ledger) fires at the
/// cut exactly once through its establishment band, the recycle door over
/// the same kept frames stays silent (the fire cleared the band in the
/// frame word), and both facts hold identically across repeated runs.
#[test]
fn an_int_only_claim_fires_once_at_the_cut_and_the_recycle_stays_silent() {
    // body_len 3: [0] the value, [1] the band, [2] the loop condition;
    // the overlay word at 3 stages the hook call
    let cut_child = def(
        "cut_child",
        &[SlotType::I64, SlotType::Tag, SlotType::Bool, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            // the establishment: the band sets, the value loads
            Uop::TagNew {
                dst: 1,
                tag: BAND_SET,
            }
            .into(),
            Uop::Kimm { dst: 0, kidx: k(0) }.into(),
            Uop::Kconst { dst: 2, kidx: k(1) }.into(),
            Uop::Jmpif { cond: 2, target: 4 }.into(),
            Uop::Ret { src: 0 }.into(),
            // the drop shape: stage, call, clear the band
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_CLEAR,
            }
            .into(),
        ],
    );
    let mut cut_child = cut_child;
    // [0] the value, [1] the loop condition, [2] the hook
    cut_child.pool = vec![
        Const::I64(7),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 3,
        }),
    ];

    let expected = b"counted value\n";
    for _ in 0..5 {
        let mut vm = band_cut_module(cut_child.clone(), marker_deinit(), None);
        assert_eq!(
            vm.run("dwh", "cut_parent", &[]),
            Ok(FrameAnswer::one(Value::I64(0))),
            "both boundaries land"
        );
        assert_eq!(
            vm.captured_output(),
            expected,
            "the int-only claim fired once at the cut; the recycle door stayed silent"
        );
        assert!(
            vm.objects_at(FIBER_ROOT).is_empty(),
            "the parent's heap drains"
        );
        assert!(
            vm.objects_at(FiberId(2)).is_empty(),
            "the cut child's heap drains behind its hook"
        );
    }
}

/// A dead claim whose field word survives via an outstanding borrow: the
/// ordinary death fired the hook and released the claim's own count, but
/// the borrow's count keeps the object alive on the ledger — the band,
/// cleared at that death, refuses the door's second fire, and the drain
/// releases the borrow's count.
#[test]
fn a_dead_claim_with_an_outstanding_borrow_fires_only_its_ordinary_death() {
    // body_len 4: [0] the claim's Ref word, [1] the borrow's copy, [2]
    // the band, [3] the loop condition; the overlay word at 4 stages
    let cut_child = def(
        "cut_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Tag,
            SlotType::Bool,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            // the establishment: the band sets, the value lands
            Uop::TagNew {
                dst: 2,
                tag: BAND_SET,
            }
            .into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            // the borrow copy: the destination owns its fresh count
            Uop::Mov { dst: 1, src: 0 }.into(),
            RcOp::Inc { src: 0 }.into(),
            // the claim's ordinary death: the hook fires here, once
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::TagNew {
                dst: 2,
                tag: BAND_CLEAR,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
            // the cut point: the borrow's count still on the ledger
            Uop::Kconst { dst: 3, kidx: k(1) }.into(),
            Uop::Jmpif {
                cond: 3,
                target: 10,
            }
            .into(),
            Uop::Ret { src: 3 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    // [0] the value, [1] the loop condition, [2] the hook
    cut_child.pool = vec![
        Const::Str("died value".into()),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 3,
        }),
    ];

    let mut vm = band_cut_module(cut_child, observer_deinit(), None);
    assert_eq!(
        vm.run("dwh", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "both boundaries land"
    );
    assert_eq!(
        vm.captured_output(),
        b"died value\n",
        "the ordinary death fired the hook once; the cut did not fire it again"
    );
    assert!(
        vm.objects_at(FiberId(2)).is_empty(),
        "the drain released the borrow's count and the heap emptied"
    );
}

/// A consumed claim's establishment clears at the consuming staging (the
/// pinned-transfer rule): the value rides into the callee, which loops to
/// its own cut — the parent frame's site reads a cleared band and stays
/// silent, and nothing fires anywhere.
#[test]
fn a_moved_claim_stays_silent_at_the_cut() {
    // body_len 3: [0] the claim's Ref word, [1] the band, [2] unused; the
    // overlay word at 3 stages the consuming call
    let cut_child = def(
        "cut_child",
        &[SlotType::Ref, SlotType::Tag, SlotType::Bool, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_SET,
            }
            .into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            // the consuming staging: the word rides, the band clears
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_CLEAR,
            }
            .into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            // unreachable: the callee never returns
            Uop::Kconst { dst: 2, kidx: k(1) }.into(),
            Uop::Jmpif { cond: 2, target: 7 }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    // [0] the value, [1] the loop condition, [2] the mover
    cut_child.pool = vec![
        Const::Str("moved value".into()),
        Const::Bool(true),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 5,
        }),
    ];

    // the mover: holds the word and loops on a backward branch, so the
    // cut lands inside it with both frames kept
    let mover = def("mover", &[SlotType::Ref, SlotType::Bool], 1, 0, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kconst { dst: 1, kidx: k(0) }.into(),
        Uop::Jmpif { cond: 1, target: 1 }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut mover = mover;
    mover.pool = vec![Const::Bool(true)];

    let mut vm = band_cut_module(cut_child, marker_deinit(), Some(mover));
    assert_eq!(
        vm.run("dwh", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the boundary unions the cancel out and the parent lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the moved claim fired nowhere: the parent's band cleared, the mover holds no claim"
    );
    assert!(
        vm.objects_at(FiberId(2)).is_empty(),
        "the staged count drained with the cut"
    );
}

/// The interrupt door: a child dying trapped mid-drive holding an
/// int-only claim (no ledger witness at all) fires it at the nursery
/// walk's death marking through its band.
#[test]
fn an_int_only_trapped_child_fires_at_the_death_marking() {
    // body_len 4: [0] the value, [1] the band, [2] the cancel tag; the
    // overlay word at 4 stages
    let child = def(
        "trap_child",
        &[
            SlotType::I64,
            SlotType::Tag,
            SlotType::Tag,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_SET,
            }
            .into(),
            Uop::Kimm { dst: 0, kidx: k(0) }.into(),
            Uop::TagNew { dst: 2, tag: 0 }.into(),
            Uop::Mov { dst: 4, src: 2 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 4 }.into(),
            // the drop shape, unreachable past the trapped cancel
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_CLEAR,
            }
            .into(),
        ],
    );
    let mut child = child;
    // [0] the value, [1] cancel, [2] the hook
    child.pool = vec![
        Const::I64(9),
        builtin(native::BUILTIN_NURSERY_CANCEL),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 2,
        }),
    ];

    let parent = def(
        "trap_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(3) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(4) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    parent.pool = vec![
        builtin(native::BUILTIN_NURSERY_OPEN),
        builtin(native::BUILTIN_NURSERY_SPAWN),
        builtin(native::BUILTIN_NURSERY_BOUNDARY),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::I64(0),
        Const::I64(1),
    ];

    let version =
        ModuleVersion::new("dwh", vec![parent, child, marker_deinit()]).expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dwh", "trap_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the boundary unions the cancel out and the parent lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"counted value\n",
        "the death marking fired the int-only claim exactly once"
    );
    assert!(
        vm.objects_at(FiberId(1)).is_empty(),
        "the walk drained the trapped child's heap after the hook"
    );
}

/// The root reset door: a root drive that traps holding an int-only
/// claim keeps its frames; the next run's reset fires the hook through
/// the band before discarding them, once, and the root heap drains.
#[test]
fn an_int_only_root_claim_fires_at_the_next_runs_reset() {
    // body_len 4: [0] the value, [1] the band, [2]-[3] the divide; the
    // overlay word at 4 stages
    let boom = def(
        "boom_root",
        &[
            SlotType::I64,
            SlotType::Tag,
            SlotType::I64,
            SlotType::I64,
            SlotType::Raw,
        ],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_SET,
            }
            .into(),
            Uop::Kimm { dst: 0, kidx: k(0) }.into(),
            Uop::Kimm { dst: 2, kidx: k(2) }.into(),
            Uop::DivI64 {
                dst: 3,
                lhs: 2,
                rhs: 2,
            }
            .into(),
            Uop::Ret { src: 3 }.into(),
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_CLEAR,
            }
            .into(),
        ],
    );
    let mut boom = boom;
    // [0] the value, [1] the hook, [2] the zero divisor
    boom.pool = vec![
        Const::I64(5),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dwh".into(),
            func: 1,
        }),
        Const::I64(0),
    ];

    let ok = def("ok_root", &[SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kimm { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut ok = ok;
    ok.pool = vec![Const::I64(7)];

    let version = ModuleVersion::new("dwh", vec![boom, marker_deinit(), ok]).expect("valid module");
    let mut vm = registry(version);
    assert!(
        vm.run("dwh", "boom_root", &[]).is_err(),
        "the root traps on the zero divide"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the trap itself fires nothing — the frames wait for the reset"
    );
    assert_eq!(
        vm.run("dwh", "ok_root", &[]),
        Ok(FrameAnswer::one(Value::I64(7))),
        "the next run lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"counted value\n",
        "the reset fired the int-only root claim exactly once"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the reset drained the trapped root's heap after the hook"
    );
}

/// The takeover over a live spawner: a trapped child's cancel payload is
/// itself a deinit claim, so the join's death walk holds that site back
/// (its band staying set in the kept frames) and marks the child dead —
/// and the recycle door the parent's next in-language spawn hits fires
/// it through the takeover. The takeover must park the spawner's live
/// context, run the pass on the dying fiber's parked one, and return to
/// the spawner's own frames: the parent continues and lands, the held
/// claim fires exactly once.
#[test]
fn a_recycle_door_over_a_live_spawner_returns_to_the_spawner() {
    use native::{BUILTIN_NURSERY_JOIN, BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN};

    let parent = def(
        "trap_parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            // nursery one: one child, trapped cancelling with its claim's
            // word as the payload, joined in-language
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            // nursery two: one plain child, whose spawn recycles the dead
            // slot and fires the held claim over the live spawner
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    // [0] open, [1] spawn, [2] join, [4] the trapped child, [5] the plain
    // child, [6] the answer
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_JOIN),
        Const::I64(0),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dws".into(),
            func: 1,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dws".into(),
            func: 3,
        }),
        Const::I64(0),
    ];

    // body_len 2: [0] the held payload's Ref word, [1] its band; overlay
    // at 2 stages the cancel and the drop shape
    let trapped_child = def(
        "trapped_child",
        &[SlotType::Ref, SlotType::Tag, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_SET,
            }
            .into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 0 }.into(),
            // the drop shape: the staging run names the payload word, the
            // band clears right after the hook call
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::TagNew {
                dst: 1,
                tag: BAND_CLEAR,
            }
            .into(),
        ],
    );
    let mut trapped_child = trapped_child;
    // [0] the payload word, [1] the cancel native, [2] the hook
    trapped_child.pool = vec![
        Const::Str("held value".into()),
        builtin(native::BUILTIN_NURSERY_CANCEL),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dws".into(),
            func: 2,
        }),
    ];

    let plain_child = def("plain_child", &[SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kimm { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut plain_child = plain_child;
    plain_child.pool = vec![Const::I64(0)];

    let version = ModuleVersion::new("dws", vec![
        parent,
        trapped_child,
        observer_deinit(),
        plain_child,
    ])
    .expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("dws", "trap_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the takeover returns to the spawner's frames and the parent lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"held value\n",
        "the recycle door fired the held claim exactly once"
    );
}
