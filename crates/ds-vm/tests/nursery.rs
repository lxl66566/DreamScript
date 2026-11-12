//! Nursery fixtures [E2, E5]: hand-assembled programs over the host face
//! (`open_root_nursery`/`spawn_fiber_in`/`join_nursery`/`teardown_nursery`)
//! and the debug join native (`nursery_join`, builtin 26). What the
//! fixtures pin:
//!
//! - join: children run in spawn order, their final values come back in spawn order, and the
//!   consumed nursery refuses a second join.
//! - first failure: a child dying trapped raises the cancel flag; a sibling looping on a backward
//!   jump stops at its next back-edge (`Cancelled`), and the boundary reports per-child outcomes.
//! - preemption points: the flag is observed at taken backward branches (a straight-line child
//!   finishes its in-flight work and lands `Completed` under the flag) and at the driver's re-entry
//!   of a parked child (the resume-shaped point: the parked child is cut there).
//! - the ledger: a child that trapped under an earlier host drive is unioned out at the join and
//!   cancels the rest.
//! - trap attribution: a trap escaping a child's drive (the child resumed a fiber that trapped)
//!   ledgers the fiber the drive ended on; the parked resumer is innocent of the trap — it is cut
//!   at its resume under the flag its resumee raised — under the join driver and under a host
//!   `resume_fiber` alike.
//! - bare detach: a bare host spawn routes into the live root nursery; with none live the host is
//!   the supervisor and the spawn detaches.
//! - the debug join native: parks the joiner, drives the nursery, answers the failed-children
//!   count; a child joining its own nursery traps; a bogus id bounces the none tag.
//! - teardown: a nursery with a parked child cuts it at its resume under the cancel flag,
//!   deterministically, before releasing slots.
//! - per-fiber TCO: a tail-looping nursery child runs unbounded iterations at a constant segment.

use ds_vm::{
    ChildOutcome, Const, FiberId, FiberState, FrameAnswer, FunctionDef, JoinOutcome,
    ModuleRegistry, ModuleVersion, NurseryId, NurseryState, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_FIBER_RESUME, BUILTIN_FIBER_YIELD, BUILTIN_NURSERY_JOIN, PLATFORM_CANCELLED,
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

/// A same-module callee by function-table index (the fixture module "nb").
fn nb_ref(index: u32) -> Const {
    Const::FuncRef {
        module: "nb".into(),
        func: index,
    }
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

/// `() -> int`: 1 / 0 — dies trapped at the DIVI64.
fn trap_child_def() -> FunctionDef {
    def(
        "trap_child",
        &[I64, I64],
        0,
        0,
        vec![Const::I64(1), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> int`: add one and jump back — the backward-branch looper only
/// the cancel flag can stop.
fn loop_forever_def() -> FunctionDef {
    def("loop_forever", &[I64, I64], 0, 0, vec![Const::I64(1)], &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 1, kidx: k(0) },
        Uop::AddI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Jmp { target: 2 },
    ])
}

/// How many straight-line adds `straight_def` runs: enough work that a
/// mid-flight cut would change the answer.
const STRAIGHT_STEPS: u32 = 40;

/// `(v: int) -> int`: `v + STRAIGHT_STEPS`, no jump at all — the linear
/// consume in flight.
fn straight_def() -> FunctionDef {
    let mut body = vec![Uop::Funcf { frame_slots: 2 }, Uop::Kimm {
        dst: 1,
        kidx: k(0),
    }];
    for _ in 0..STRAIGHT_STEPS {
        body.push(Uop::AddI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        });
    }
    body.push(Uop::Ret { src: 0 });
    def("straight", &[I64, I64], 1, 0, vec![Const::I64(1)], &body)
}

/// `(n, acc: int) -> int`: the tail loop — CALLT self, frame reused.
fn spin_def() -> FunctionDef {
    def(
        "spin",
        &[I64, I64, I64, SlotType::Bool, Raw, Raw],
        2,
        2,
        vec![Const::I64(1), Const::I64(0), nb_ref(3)],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::EqI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif {
                cond: 3,
                target: 10,
            },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::SubI64 {
                dst: 4,
                lhs: 0,
                rhs: 2,
            },
            Uop::AddI64 {
                dst: 5,
                lhs: 1,
                rhs: 2,
            },
            Uop::Callt {
                callee: k(2),
                nargs: 2,
            },
            Uop::Ret { src: 0 }, // unreachable under the tail call
            Uop::Ret { src: 0 },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `(v: int) -> int`: yield `v` (park), then return `v + 1` — the re-entry
/// answer slot stays `Raw`, so the bounce tag lands without a trap.
fn half_def() -> FunctionDef {
    def(
        "half",
        &[I64, I64, Raw],
        1,
        1,
        vec![Const::I64(1), builtin(BUILTIN_FIBER_YIELD)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(nursery: int) -> t`: call `nursery_join(nursery)` and return its
/// answer word — the failed count, or the bounce tag. Spawned detached it
/// is the in-language joiner; spawned into the same nursery it is the
/// self-join trap.
fn joiner_def() -> FunctionDef {
    def(
        "joiner",
        &[I64, Raw],
        1,
        1,
        vec![builtin(BUILTIN_NURSERY_JOIN)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `(v: int) -> int`: echo.
fn plain_def() -> FunctionDef {
    def("plain", &[I64], 1, 0, vec![], &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Ret { src: 0 },
    ])
}

/// `(fiber: int) -> int`: resume `fiber` with 0 and return its answer word
/// — the in-language resumer that can outlive its resumee.
fn resume_like_def() -> FunctionDef {
    def(
        "resume_like",
        &[I64, Raw, Raw],
        1,
        2,
        vec![Const::I64(0), builtin(BUILTIN_FIBER_RESUME)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Ret { src: 1 },
        ],
    )
}

/// The fixture module. Function indices are pool-FuncRef contract:
/// 0 trap_child, 1 loop_forever, 2 straight, 3 spin, 4 half, 5 joiner,
/// 6 plain, 7 resume_like.
fn nb_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("nb", vec![
            trap_child_def(),
            loop_forever_def(),
            straight_def(),
            spin_def(),
            half_def(),
            joiner_def(),
            plain_def(),
            resume_like_def(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// The first nursery a fresh VM opens: pool slot 0.
fn root_id(nursery: NurseryId) -> i64 {
    i64::from(nursery.0)
}

#[test]
fn join_returns_final_values_in_spawn_order() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    let c1 = vm
        .spawn_fiber_in(n, "nb", "plain", &[Value::I64(11)])
        .expect("spawn c1");
    let c2 = vm
        .spawn_fiber_in(n, "nb", "plain", &[Value::I64(22)])
        .expect("spawn c2");
    let c3 = vm
        .spawn_fiber_in(n, "nb", "plain", &[Value::I64(33)])
        .expect("spawn c3");
    assert_eq!((c1, c2, c3), (FiberId(1), FiberId(2), FiberId(3)));
    // spawn order drives everything: entry order and value order match it
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Joined(vec![
            FrameAnswer::one(Value::I64(11)),
            FrameAnswer::one(Value::I64(22)),
            FrameAnswer::one(Value::I64(33))
        ]))
    );
    for c in [c1, c2, c3] {
        assert_eq!(vm.fiber_state(c), Some(FiberState::Dead));
    }
    assert_eq!(vm.nursery_state(n), Some(NurseryState::Dead));
    // the consumed nursery refuses a second join
    assert_eq!(
        vm.join_nursery(n),
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: n.0,
        })
    );
}

#[test]
fn first_failure_cancels_a_looping_sibling() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    let failing = vm
        .spawn_fiber_in(n, "nb", "trap_child", &[])
        .expect("spawn");
    let looper = vm
        .spawn_fiber_in(n, "nb", "loop_forever", &[])
        .expect("spawn looper");
    vm.spawn_fiber_in(n, "nb", "plain", &[Value::I64(7)])
        .expect("spawn lander");
    // the looper has no exit but the cancel flag: a passing join proves it
    // stopped at its first back-edge after the failure
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Failed(vec![
            ChildOutcome::Trapped(Trap::DivideByZero),
            ChildOutcome::Cancelled,
            ChildOutcome::Completed(FrameAnswer::one(Value::I64(7))),
        ]))
    );
    assert_eq!(vm.fiber_state(failing), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(looper), Some(FiberState::Dead));
    assert_eq!(vm.nursery_cancelled(n), Some(true), "the flag stays up");
}

#[test]
fn cancellation_is_observed_only_at_backward_branches() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    vm.spawn_fiber_in(n, "nb", "trap_child", &[])
        .expect("spawn");
    let linear = vm
        .spawn_fiber_in(n, "nb", "straight", &[Value::I64(5)])
        .expect("spawn straight");
    // the straight-line child runs after the flag is up: no backward
    // branch anywhere in it, so the in-flight consume finishes and lands
    let expect = Value::I64(5 + i64::from(STRAIGHT_STEPS));
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Failed(vec![
            ChildOutcome::Trapped(Trap::DivideByZero),
            ChildOutcome::Completed(FrameAnswer::one(expect)),
        ]))
    );
    assert_eq!(vm.fiber_state(linear), Some(FiberState::Dead));
}

#[test]
fn a_child_trapped_before_join_cancels_the_rest() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    let failing = vm
        .spawn_fiber_in(n, "nb", "trap_child", &[])
        .expect("spawn");
    let looper = vm
        .spawn_fiber_in(n, "nb", "loop_forever", &[])
        .expect("spawn looper");
    // the host drives the child to its trap before any join: the ledger
    // records it and the flag goes up at death
    assert_eq!(
        vm.resume_fiber(failing, Value::I64(0)),
        Err(Trap::DivideByZero)
    );
    // a child spawned after the flag is up and driven directly by the host
    // dies at its first back-edge with the cancel sentinel
    let late = vm
        .spawn_fiber_in(n, "nb", "loop_forever", &[])
        .expect("spawn late looper");
    assert_eq!(
        vm.resume_fiber(late, Value::I64(0)),
        Ok(Value::Tag(PLATFORM_CANCELLED))
    );
    assert_eq!(vm.fiber_state(late), Some(FiberState::Dead));
    // the join collects all three from the ledger in spawn order
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Failed(vec![
            ChildOutcome::Trapped(Trap::DivideByZero),
            ChildOutcome::Cancelled,
            ChildOutcome::Cancelled,
        ]))
    );
    assert_eq!(vm.fiber_state(failing), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(looper), Some(FiberState::Dead));
}

#[test]
fn bare_host_spawns_route_into_the_live_root_nursery() {
    let mut vm = Vm::new(nb_registry());
    // no live root nursery: the host is the supervisor, the spawn detaches
    let detached = vm
        .spawn_fiber("nb", "plain", &[Value::I64(7)])
        .expect("bare spawn");
    assert_eq!(vm.fiber_nursery(detached), None);
    assert_eq!(vm.resume_fiber(detached, Value::I64(0)), Ok(Value::I64(7)));
    // a live root nursery: the same bare spawn is born its child
    let n = vm.open_root_nursery();
    let routed = vm
        .spawn_fiber("nb", "plain", &[Value::I64(9)])
        .expect("bare spawn under a root");
    assert_eq!(vm.fiber_nursery(routed), Some(n));
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Joined(vec![FrameAnswer::one(Value::I64(9))]))
    );
    // consumed: bare spawns detach again
    let after = vm
        .spawn_fiber("nb", "plain", &[Value::I64(1)])
        .expect("bare spawn after close");
    assert_eq!(vm.fiber_nursery(after), None);
}

#[test]
fn the_debug_join_native_answers_the_failed_count() {
    let mut vm = Vm::new(nb_registry());
    // the joiner spawns before the root opens: bare, so it joins n rather
    // than being canceled as a child of it
    let joiner = vm
        .spawn_fiber("nb", "joiner", &[Value::I64(root_id(NurseryId(0)))])
        .expect("spawn joiner");
    let n = vm.open_root_nursery();
    assert_eq!(n, NurseryId(0));
    let failing = vm
        .spawn_fiber_in(n, "nb", "trap_child", &[])
        .expect("spawn");
    vm.spawn_fiber_in(n, "nb", "plain", &[Value::I64(1)])
        .expect("spawn lander");
    // in-language: the joiner parks at nursery_join, the dispatcher drives
    // the children (first failure cancels the rest), and the count rides
    // the re-entry back to the parked call
    assert_eq!(vm.resume_fiber(joiner, Value::I64(0)), Ok(Value::I64(1)));
    assert_eq!(vm.fiber_state(joiner), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(failing), Some(FiberState::Dead));
    assert_eq!(vm.nursery_state(n), Some(NurseryState::Dead));
}

#[test]
fn the_debug_join_native_answers_zero_when_all_join() {
    let mut vm = Vm::new(nb_registry());
    let joiner = vm
        .spawn_fiber("nb", "joiner", &[Value::I64(root_id(NurseryId(0)))])
        .expect("spawn joiner");
    let n = vm.open_root_nursery();
    vm.spawn_fiber_in(n, "nb", "plain", &[Value::I64(3)])
        .expect("spawn");
    vm.spawn_fiber_in(n, "nb", "plain", &[Value::I64(4)])
        .expect("spawn");
    assert_eq!(vm.resume_fiber(joiner, Value::I64(0)), Ok(Value::I64(0)));
    assert_eq!(
        vm.nursery_state(n),
        Some(NurseryState::Dead),
        "the native join consumes the nursery like the host face"
    );
}

#[test]
fn a_fiber_joining_its_own_nursery_traps() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    let child = vm
        .spawn_fiber_in(n, "nb", "joiner", &[Value::I64(root_id(n))])
        .expect("spawn self-joiner");
    // the child's own join attempt would deadlock the driver on itself:
    // the trap rides the boundary out like any child failure
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Failed(vec![ChildOutcome::Trapped(
            Trap::UnknownCallee {
                module: "nursery".into(),
                func: n.0,
            }
        )]))
    );
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
}

#[test]
fn a_bogus_nursery_id_bounces_the_none_tag() {
    let mut vm = Vm::new(nb_registry());
    let joiner = vm
        .spawn_fiber("nb", "joiner", &[Value::I64(99)])
        .expect("spawn joiner");
    // the checked-read convention: no switch, the bounce word is the call's
    // answer and the fiber returns it
    assert_eq!(
        vm.resume_fiber(joiner, Value::I64(0)),
        Ok(Value::Tag(PLATFORM_NONE))
    );
}

#[test]
fn teardown_cuts_a_parked_child_at_its_resume_under_the_flag() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    let half = vm
        .spawn_fiber_in(n, "nb", "half", &[Value::I64(5)])
        .expect("spawn half");
    let looper = vm
        .spawn_fiber_in(n, "nb", "loop_forever", &[])
        .expect("spawn looper");
    // park the first child mid-flight at its yield
    assert_eq!(vm.resume_fiber(half, Value::I64(0)), Ok(Value::I64(5)));
    assert_eq!(vm.fiber_state(half), Some(FiberState::Suspended));
    // teardown: the flag goes up first, then each child in spawn order —
    // the parked one is cut at its next resume (never resuming: its
    // straight-line tail no longer lands), the looper stops at its first
    // back-edge
    assert_eq!(
        vm.teardown_nursery(n),
        Ok(vec![ChildOutcome::Cancelled, ChildOutcome::Cancelled])
    );
    assert_eq!(vm.fiber_state(half), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(looper), Some(FiberState::Dead));
    assert_eq!(vm.nursery_state(n), Some(NurseryState::Dead));
    assert_eq!(
        vm.teardown_nursery(n),
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: n.0,
        })
    );
}

#[test]
fn per_child_tco_stays_intact_inside_a_nursery() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    let spin = vm
        .spawn_fiber_in(n, "nb", "spin", &[Value::I64(1_000_000), Value::I64(0)])
        .expect("spawn spin");
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Joined(vec![FrameAnswer::one(Value::I64(
            1_000_000
        ))]))
    );
    // the tail loop never grew the segment: high-water == entry frame, one
    // chunk — and the join observed no cancel (there was no back-edge)
    assert_eq!(vm.fiber_max_len(spin), Some(8));
    assert_eq!(vm.fiber_chunk_count(spin), Some(1));
    assert_eq!(vm.fiber_state(spin), Some(FiberState::Dead));
}

#[test]
fn join_survives_a_child_left_parked_on_a_dead_resumer() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    // the resumer is driven first: it resumes the half-fiber (which yields
    // back), returns, and dies — leaving the half parked at its yield with
    // a dead resumer, the leftover pattern
    let resumer = vm
        .spawn_fiber_in(n, "nb", "resume_like", &[Value::I64(2)])
        .expect("spawn resumer");
    assert_eq!(resumer, FiberId(1));
    let half = vm
        .spawn_fiber_in(n, "nb", "half", &[Value::I64(5)])
        .expect("spawn half");
    assert_eq!(half, FiberId(2));
    // the join drives the leftover next: its yield and its completion both
    // deliver to the driver (the resumer is gone), never into a corpse
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Joined(vec![
            FrameAnswer::one(Value::I64(5)),
            FrameAnswer::one(Value::I64(6))
        ]))
    );
}

#[test]
fn a_trap_under_a_child_resume_blames_the_trapped_fiber() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    // the resumer is driven first (spawn order): it parks at its resume of
    // the victim, and the victim traps mid-drive — the trap escapes the
    // drive attributed to the victim, never to the parked resumer
    let resumer = vm
        .spawn_fiber_in(n, "nb", "resume_like", &[Value::I64(2)])
        .expect("spawn resumer");
    let victim = vm
        .spawn_fiber_in(n, "nb", "trap_child", &[])
        .expect("spawn victim");
    assert_eq!((resumer, victim), (FiberId(1), FiberId(2)));
    // the victim ledgers Trapped (flag up); the innocent resumer is not
    // blamed with the trap — parked at its resume call under the flag its
    // resumee raised, it is cut at its next resume — no misattributed
    // kill, no never-ledgered Running zombie, no drain panic
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Failed(vec![
            ChildOutcome::Cancelled,
            ChildOutcome::Trapped(Trap::DivideByZero),
        ]))
    );
    assert_eq!(vm.fiber_state(resumer), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(victim), Some(FiberState::Dead));
    assert_eq!(vm.nursery_cancelled(n), Some(true));
}

#[test]
fn a_trap_under_a_host_resume_blames_the_trapped_fiber() {
    let mut vm = Vm::new(nb_registry());
    let n = vm.open_root_nursery();
    let resumer = vm
        .spawn_fiber_in(n, "nb", "resume_like", &[Value::I64(2)])
        .expect("spawn resumer");
    let victim = vm
        .spawn_fiber_in(n, "nb", "trap_child", &[])
        .expect("spawn victim");
    // the host drive ends on the victim's trap: the ledger records the
    // victim (flag up), the request fiber keeps its parked continuation —
    // it is innocent, not the trapper
    assert_eq!(
        vm.resume_fiber(resumer, Value::I64(0)),
        Err(Trap::DivideByZero)
    );
    assert_eq!(vm.fiber_state(resumer), Some(FiberState::Suspended));
    assert_eq!(vm.fiber_state(victim), Some(FiberState::Running));
    // the boundary still completes: the parked resumer is cut at its
    // resume under the flag, the interrupted victim reports trapped
    assert_eq!(
        vm.join_nursery(n),
        Ok(JoinOutcome::Failed(vec![
            ChildOutcome::Cancelled,
            ChildOutcome::Trapped(Trap::DivideByZero),
        ]))
    );
    assert_eq!(vm.fiber_state(resumer), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(victim), Some(FiberState::Dead));
}

#[test]
fn absent_nurseries_name_themselves_in_the_trap() {
    let mut vm = Vm::new(nb_registry());
    let bogus = NurseryId(99);
    assert_eq!(
        vm.join_nursery(bogus),
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: 99,
        })
    );
    assert_eq!(
        vm.teardown_nursery(bogus),
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: 99,
        })
    );
    assert_eq!(
        vm.spawn_fiber_in(bogus, "nb", "plain", &[]),
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: 99,
        })
    );
}

// ---- the in-language control seams [E2] --------------------------------
//
// The lowerer synthesizes every call site, so these fixtures pin the
// dispatcher-intercepted seams directly: the open and spawn pair stages a
// ready child, the boundary answers its span on the caller's overlay,
// and the mask refuses an uncontained payload instead of corrupting the
// union's tag space.

use ds_abi::SlotType::Tag;
use ds_vm::native::{
    BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN,
};

/// The payload tag the cancel child hands the boundary.
const SEAM_TAG: u8 = 5;

/// `() -> int`: the inert plain child (a clean join's contribution).
fn seam_plain_def() -> FunctionDef {
    def("seam_plain", &[I64], 0, 0, vec![Const::I64(7)], &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Kimm { dst: 0, kidx: k(0) },
        Uop::Ret { src: 0 },
    ])
}

/// `() -> ()`: construct one bare member word and hand it to the cancel
/// seam — the `cancel(e)` spelling's hand-assembled form. The call never
/// answers; the RET is unreachable.
fn seam_cancel_def() -> FunctionDef {
    def(
        "seam_cancel",
        &[Tag, Tag, Raw],
        0,
        1,
        vec![builtin(BUILTIN_NURSERY_CANCEL)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::TagNew {
                dst: 0,
                tag: SEAM_TAG,
            },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> word`: open a fresh nursery, spawn `child` through the seam,
/// join through the boundary with `mask` staged, and return the answer
/// span's payload word.
fn seam_parent_def(child: u32, mask: i64) -> FunctionDef {
    def(
        "seam_parent",
        &[Raw, I64, Raw],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            Const::FuncRef {
                module: "sb".into(),
                func: child,
            },
            Const::I64(mask),
        ],
        &[
            Uop::Funcf { frame_slots: 3 },
            // open: the fresh nursery's id lands on overlay word 0
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 1 },
            // spawn: (id, the child's scalar callable word)
            Uop::Kconst { dst: 2, kidx: k(3) },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // boundary: (id, the containment mask); the answer span
            // lands on the same overlay words
            Uop::Mov { dst: 1, src: 0 },
            Uop::Kimm { dst: 2, kidx: k(4) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> ()`: stage the member's tag word plus one payload word — the
/// payloaded `cancel(e)` spelling's hand-assembled form. The call never
/// answers; the RET is unreachable.
fn seam_payload_cancel_def() -> FunctionDef {
    def(
        "seam_payload_cancel",
        &[Tag, Tag, Raw, Raw],
        0,
        2,
        vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(42)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::TagNew {
                dst: 0,
                tag: SEAM_TAG,
            },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kimm { dst: 3, kidx: k(1) },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> word`: the payloaded seam parent — the boundary's answer span
/// rides four overlay words (the discriminant, the member's tag, the
/// payload word), so the arg area widens and the parent returns the
/// payload word the delivery wrote.
fn seam_payload_parent_def(mask: i64) -> FunctionDef {
    def(
        "seam_parent",
        &[Raw, I64, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            Const::FuncRef {
                module: "sb".into(),
                func: 1,
            },
            Const::I64(mask),
        ],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 1 },
            Uop::Kconst { dst: 2, kidx: k(3) },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Kimm { dst: 2, kidx: k(4) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 0, src: 3 },
            Uop::Ret { src: 0 },
        ],
    )
}

/// One seam module over the payloaded variants: [0] = the parent, [1] =
/// the payloaded cancel child.
fn seam_payload_vm(mask: i64) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("sb", vec![
            seam_payload_parent_def(mask),
            seam_payload_cancel_def(),
        ])
        .expect("valid seam module"),
    );
    Vm::new(reg)
}

/// One seam module over the given parent variant: [0] = the parent, [1]
/// = the plain child, [2] = the cancel child.
fn seam_vm(child: u32, mask: i64) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("sb", vec![
            seam_parent_def(child, mask),
            seam_plain_def(),
            seam_cancel_def(),
        ])
        .expect("valid seam module"),
    );
    Vm::new(reg)
}

/// A clean join answers the ok discriminant with the inert payload word:
/// the parent runs to its entry RET with the boundary's payload.
#[test]
fn the_boundary_seam_answers_a_clean_join_span() {
    let mut vm = seam_vm(1, 1);
    assert_eq!(vm.run("sb", "seam_parent", &[]), Ok(Value::I64(0)));
}

/// A cancel child's payload unions out at the boundary: the span answers
/// the failure discriminant, and the payload word carries the failing
/// member's own tag.
#[test]
fn the_boundary_seam_unions_a_cancelled_payload_out() {
    let mut vm = seam_vm(2, 1 << SEAM_TAG);
    assert_eq!(vm.run("sb", "seam_parent", &[]), Ok(Value::Tag(SEAM_TAG)));
}

/// A payloaded cancel span unions out whole: the mask admits the member
/// by its tag word, the delivery writes the flat span (the discriminant,
/// the member's tag, the payload words), and the parent reads the payload
/// word the child staged.
#[test]
fn a_payloaded_cancel_span_unions_out_whole() {
    let mut vm = seam_payload_vm(1 << SEAM_TAG);
    assert_eq!(vm.run("sb", "seam_parent", &[]), Ok(Value::I64(42)));
}

/// A payloaded span outside the staged mask refuses whole: the trap
/// carries the flat span, so a hand-assembled caller cannot lose the
/// payload's rows by a partial write.
#[test]
fn an_uncontained_payloaded_span_refuses_whole() {
    let mut vm = seam_payload_vm(0);
    assert_eq!(
        vm.run("sb", "seam_parent", &[]),
        Err(Trap::NurseryUncontained {
            payload: vec![Value::Tag(SEAM_TAG), Value::I64(42)]
        })
    );
}

/// A payload outside the staged containment mask refuses: the boundary
/// traps instead of writing a foreign tag into the caller's union.
#[test]
fn an_uncontained_boundary_payload_refuses() {
    let mut vm = seam_vm(2, 0);
    assert_eq!(
        vm.run("sb", "seam_parent", &[]),
        Err(Trap::NurseryUncontained {
            payload: vec![Value::Tag(SEAM_TAG)]
        })
    );
}
