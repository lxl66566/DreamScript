//! The M5 yield bound [E2, E5]: the join/teardown driver's re-entry of a
//! parked child is a preemption-shaped point — under the nursery's raised
//! cancel flag the re-entry delivers the cut, so explicit cancellation
//! always terminates a yielding child at its next resume (the one bound on
//! a child that parks with no taken backward jump left between parks; an
//! uncancelled yield loop may hang exactly like any infinite loop, and a
//! self-tail-loop child that never parks never observes — the CALLT
//! ruling). The pins:
//!
//! - a yield-in-tail-loop child under a cancelled nursery dies at its next resume, its kept-frame
//!   deinit firing through the cancel-cut door exactly once, and the join completes (without the
//!   cut this drive bounces the parked child forever);
//! - the cut is deterministic across repeated runs;
//! - a parked child of an uncancelled nursery resumes normally and lands (no new interference).

use ds_vm::{
    Const, FIBER_ROOT, FiberId, FiberState, FrameAnswer, FunctionDef, JoinOutcome, ModuleRegistry,
    ModuleVersion, NurseryId, Platform, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble, native,
    native::{
        BUILTIN_FIBER_YIELD, BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL,
        BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN,
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

/// A same-module callee by function-table index (the fixture module "yb").
fn yb_ref(index: u32) -> Const {
    Const::FuncRef {
        module: "yb".into(),
        func: index,
    }
}

fn def(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    pool: Vec<Const>,
    body: &[Inst],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
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
        Vec::new(),
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
        Const::FuncRef {
            module: "std".into(),
            func: 0,
        },
        Const::Bool(false),
    ];
    hook
}

/// The boundary parent: open, spawn the flag raiser first and the yielding
/// child second (spawn order decides the drive order), then boundary with
/// a containing mask.
fn parent_def() -> FunctionDef {
    def(
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
        // [0] open, [1] spawn, [2] boundary, [4] the flag raiser, [5] the
        // yielding child, [6] the landing answer, [7] the mask
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            Const::I64(0),
            yb_ref(1),
            yb_ref(2),
            Const::I64(0),
            Const::I64(1),
        ],
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
    )
}

/// The flag raiser: cancel immediately — the trapped death raises the
/// nursery's cancel flag before the yielding child is ever entered.
fn flag_child_def() -> FunctionDef {
    def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)],
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
    )
}

/// The unbound shape: establish a kept deinit value, yield once, then loop
/// by tail call — every backward edge is a CALLT pc-reset (deliberately
/// not a preemption point), so between parks the child never observes the
/// flag. Without the resume-shaped cut the join driver bounces it forever.
/// The drop shape rides unreachable past the tail call.
fn tail_yield_def() -> FunctionDef {
    def(
        "tail_yield",
        &[SlotType::Ref, SlotType::I64, SlotType::Raw, SlotType::Raw],
        0,
        1,
        // [0] the kept value, [1] the yield payload, [2] yield, [3] self,
        // [4] the hook
        vec![
            Const::Str("tail value".into()),
            Const::I64(0),
            builtin(BUILTIN_FIBER_YIELD),
            yb_ref(2),
            yb_ref(3),
        ],
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Kimm { dst: 1, kidx: k(1) }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::Callt {
                callee: k(3),
                nargs: 0,
            }
            .into(),
            Uop::Ret { src: 1 }.into(),
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::Call {
                callee: k(4),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
        ],
    )
}

/// `(v: int) -> int`: two straight-line yields, then land with `v` — the
/// finite yielder an uncancelled join must drive through its parks.
fn yield_land_def() -> FunctionDef {
    def(
        "yield_land",
        &[SlotType::I64, SlotType::Raw, SlotType::Raw],
        1,
        1,
        vec![builtin(BUILTIN_FIBER_YIELD)],
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 0 }.into(),
        ],
    )
}

fn registry(functions: Vec<FunctionDef>) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install_native(native::BUILTIN_MODULE, native::builtin_module());
    reg.install_platform(Platform::std());
    reg.install(ModuleVersion::new("yb", functions).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    vm.capture_output();
    vm
}

/// The main pin: the tail-yield child parks at its first yield, and the
/// driver's next bounce — the flag already up — cuts it there. The cut
/// rides the cancel evidence machinery: the kept-frame deinit fires
/// through the cancel-cut door exactly once, the heap drains behind the
/// hook, and the boundary lands.
fn cut_once() -> Vm {
    let mut vm = registry(vec![
        parent_def(),
        flag_child_def(),
        tail_yield_def(),
        observer_deinit(),
    ]);
    assert_eq!(
        vm.run("yb", "cut_parent", &[]),
        Ok(Value::I64(0)),
        "the boundary unions the cancel out and the parent lands — the join completed"
    );
    vm
}

#[test]
fn a_tail_yielding_child_under_a_cancelled_nursery_dies_at_its_next_resume() {
    let vm = cut_once();
    assert_eq!(
        vm.captured_output(),
        b"tail value\n",
        "the cut at the resume fired the kept deinit exactly once"
    );
    assert_eq!(vm.fiber_state(FiberId(2)), Some(FiberState::Dead));
    assert_eq!(
        vm.nursery_cancelled(NurseryId(0)),
        Some(true),
        "the flag stays up behind the completed boundary"
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

/// Determinism: the cut, its deinit fire, and the drains behind it are
/// identical across repeated runs.
#[test]
fn the_resume_cut_fires_deterministically_across_repeated_runs() {
    for _ in 0..5 {
        let vm = cut_once();
        assert_eq!(
            vm.captured_output(),
            b"tail value\n",
            "the cut fired the kept deinit exactly once, identically per run"
        );
        assert!(
            vm.objects_at(FiberId(2)).is_empty(),
            "the value drained behind its hook, identically per run"
        );
    }
}

/// No new interference: with the flag never raised, a parked child is
/// bounced normally through each of its parks and lands — the resume
/// point cuts nothing.
#[test]
fn an_uncancelled_parked_child_resumes_normally_and_lands() {
    let mut vm = registry(vec![yield_land_def()]);
    let n = vm.open_root_nursery();
    let child = vm
        .spawn_fiber_in(n, "yb", "yield_land", &[Value::I64(41)])
        .expect("spawn");
    // the host drive parks the child at its first yield (payload 41)
    assert_eq!(vm.resume_fiber(child, Value::I64(0)), Ok(Value::I64(41)));
    assert_eq!(vm.fiber_state(child), Some(FiberState::Suspended));
    // the join bounces it through its second park and it lands
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Joined(vec![FrameAnswer::one(Value::I64(41))]))
    );
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
}
