//! The park bound: the join/teardown driver's re-entry of a
//! parked child is a preemption-shaped point — under the nursery's raised
//! cancel flag the re-entry delivers the cut, so explicit cancellation
//! always terminates a parked child at its next resume (the one bound on
//! a child whose park leaves no taken backward jump before its loop; an
//! uncancelled park-then-loop may hang exactly like any infinite loop, and
//! a self-tail-loop child that never parks never observes it). The child
//! parks through the host's suspension request
//! (`Vm::request_suspend`, the retired debug yield's host-face
//! replacement): the request is one-shot, so the child parks once and
//! then tail-loops without ever re-parking. The pins:
//!
//! - a parked-then-tail-looping child under a cancelled nursery dies at its next resume, its
//!   kept-frame deinit firing through the cancel-cut door exactly once, and the join completes
//!   (without the cut this drive never returns past the child's loop);
//! - the cut is deterministic across repeated runs;
//! - a parked child of an uncancelled nursery resumes normally and lands (no new interference).

use ds_vm::{
    Const, ConstFuncRef, FIBER_ROOT, FiberId, FiberState, FrameAnswer, FunctionDef, JoinOutcome,
    ModuleRegistry, ModuleVersion, NurseryId, Platform, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble, native,
    native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN, PLATFORM_NONE,
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

/// A same-module callee by function-table index (the fixture module "yb").
fn yb_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "yb".into(),
        func: index,
    })
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
        Const::FuncRef(ConstFuncRef::Named {
            module: "std".into(),
            func: 0,
        }),
        Const::Bool(false),
    ];
    hook
}

/// The boundary parent: open, spawn the flag raiser first and the parked
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
        // parked child, [6] the landing answer, [7] the mask
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
/// nursery's cancel flag before the parked child is ever entered.
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

/// The unbound shape: establish a kept deinit value, park once at the
/// one-shot taken branch (the host's armed suspension request), then loop
/// by tail call — the CALLT pc-reset is deliberately not a preemption
/// point, so past the park the child never observes the flag. Without the
/// resume-shaped cut the join driver never returns past the child's loop.
/// The drop shape rides unreachable past the tail call.
fn tail_yield_def() -> FunctionDef {
    def(
        "tail_yield",
        &[
            SlotType::Ref,
            SlotType::I64,
            SlotType::I64,
            SlotType::Bool,
            SlotType::I64,
            SlotType::Raw,
        ],
        0,
        1,
        // [0] the kept value, [1] one, [2] two, [3] zero, [4] self,
        // [5] the hook; body slots 0=kept, 1=one, 2=cnt, 3=c, 4=two,
        // 5=the drop shape's overlay word
        vec![
            Const::Str("tail value".into()),
            Const::I64(1),
            Const::I64(2),
            Const::I64(0),
            yb_ref(2),
            yb_ref(3),
        ],
        &[
            Uop::Funcf { frame_slots: 6 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Kimm { dst: 2, kidx: k(3) }.into(), // zero the counter
            Uop::Kimm { dst: 1, kidx: k(1) }.into(), // one
            Uop::AddI64 {
                dst: 2,
                lhs: 2,
                rhs: 1,
            }
            .into(),
            Uop::Kimm { dst: 4, kidx: k(2) }.into(), // two
            Uop::LtI64 {
                dst: 3,
                lhs: 2,
                rhs: 4,
            }
            .into(),
            Uop::Jmpif { cond: 3, target: 4 }.into(), // the one-shot park point
            Uop::Callt {
                callee: k(4),
                nargs: 0,
            }
            .into(),
            Uop::Ret { src: 1 }.into(),
            Uop::Mov { dst: 5, src: 0 }.into(),
            Uop::Call {
                callee: k(5),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
        ],
    )
}

/// `(v: int) -> int`: park once at the one-shot taken branch (the host's
/// armed suspension request), then land with `v` — the finite parked
/// child an uncancelled join must drive through its park.
fn yield_land_def() -> FunctionDef {
    // pool [0]=one, [1]=two, [2]=zero
    def(
        "yield_land",
        &[SlotType::I64, SlotType::I64, SlotType::Bool, SlotType::I64],
        1,
        0,
        vec![Const::I64(1), Const::I64(2), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kimm { dst: 3, kidx: k(2) }.into(), // zero the counter
            Uop::Kimm { dst: 1, kidx: k(0) }.into(), // one
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 1,
            }
            .into(),
            Uop::Kimm { dst: 1, kidx: k(1) }.into(), // two
            Uop::LtI64 {
                dst: 2,
                lhs: 3,
                rhs: 1,
            }
            .into(),
            Uop::Jmpif { cond: 2, target: 3 }.into(), // the one-shot park point
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

/// The main pin: the armed request parks the tail-looping child at its
/// one-shot branch, and the driver's next bounce — the flag already up —
/// cuts it there. The cut rides the cancel evidence machinery: the
/// kept-frame deinit fires through the cancel-cut door exactly once, the
/// heap drains behind the hook, and the boundary lands.
fn cut_once() -> Vm {
    let mut vm = registry(vec![
        parent_def(),
        flag_child_def(),
        tail_yield_def(),
        observer_deinit(),
    ]);
    // the boundary parent spawns the flag raiser first (id 1) and the
    // tail-looping child second (id 2); the host arms the request for the
    // child before the run, and the join driver's entry observes it
    vm.request_suspend(FiberId(2));
    assert_eq!(
        vm.run("yb", "cut_parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the boundary unions the cancel out and the parent lands — the join completed"
    );
    vm
}

#[test]
fn a_parked_tail_looping_child_under_a_cancelled_nursery_dies_at_its_next_resume() {
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
    // the host drive parks the child at its one-shot branch under the
    // armed request; the suspension outcome carries the bare bounce tag
    vm.request_suspend(child);
    assert_eq!(
        vm.resume_fiber(child, Value::I64(0)),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)))
    );
    assert_eq!(vm.fiber_state(child), Some(FiberState::Preempted));
    // the join bounces it through its park and it lands
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Joined(vec![FrameAnswer::one(Value::I64(41))]))
    );
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
}
