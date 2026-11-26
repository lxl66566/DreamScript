//! The time face's timer e2e: hand-
//! assembled programs over the std platform's time face, a VM with a
//! reactor installed, and the clock itself as the witness — hermetic,
//! no external network, no wall-clock assumptions beyond bounds with
//! generous headroom (a concurrent lane shares the box). What the
//! fixtures pin:
//!
//! - the parking sleep: `sleep(nanos)` parks the calling fiber on the timer-only wait (no IO
//!   interest), the deadline wheel's firing delivering the unit word at the parked call site, and
//!   the span measured over `mono` words covering at least the request.
//! - the degenerate spans: zero and negative requests answer at once (the deadline already due).
//! - the monotonic clock: differences of `mono` readings measure elapsed time and never go backward
//!   — the word `sleep` waits over.
//! - overlap: two nursery children sleeping concurrently complete under one join in the longer wait
//!   alone (the serialized twin needs both back to back).
//! - the entry frame's own tail park on a sleep delivers the unit word as the run's result.
//! - parked-sleep cancellation: a sibling's trap cuts the sleeping child, its wait dropped from the
//!   wheel's table without a reactor interest to deregister.
//! - no reactor installed: the identical program keeps the blocking semantics (the embedder's
//!   platform choice, never a language fact). The parking-shape tests early-return on a host with
//!   no reactor driver — the Linux path is primary, the gap recorded in the docs.

use std::time::{Duration, Instant};

use ds_vm::{
    Const, ConstFuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw},
        uop::Uop,
    },
    assemble,
    platform::{Capability, OP_MONO, OP_SLEEP, Platform, STD_NAME},
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

/// The shared pool: the monotonic clock read, then the sleep op.
fn pool() -> Vec<Const> {
    vec![
        platform(OP_MONO),  // k0
        platform(OP_SLEEP), // k1
    ]
}

fn platform(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: STD_NAME.into(),
        func: index,
    })
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[Capability::Time]));
    reg.install(ModuleVersion::new("io", defs).expect("valid fixture module"));
    reg
}

/// A VM over `defs` with the platform's reactor installed (the parking
/// discipline) and a captured sink.
fn scheduled_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    vm.capture_output();
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

/// Whether this host parks (a reactor driver exists): the parking-shape
/// tests ride it, the rest keep the blocking twin.
fn parks() -> bool {
    ds_vm::reactor::platform_default().is_some()
}

/// `(nanos: int) -> int`: mono, sleep, mono — the elapsed nanoseconds
/// the sleep actually waited, measured over the clock's own words.
fn sleep_probe_def() -> FunctionDef {
    // the outgoing-argument area is the frame's tail slot (slot 3):
    // every native answer overlays it, so the bracket reads it out
    // between the calls
    def("sleep_probe", &[I64, Raw, Raw, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Call {
            callee: k(0),
            nargs: 0,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(1),
            nargs: 1,
        },
        Uop::Call {
            callee: k(0),
            nargs: 0,
        },
        Uop::SubI64 {
            dst: 1,
            lhs: 3,
            rhs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(nanos: int) -> ()`: the entry frame's own tail call is the sleep —
/// the entry-frame park (the final-delivery sentinel).
fn sleep_tail_entry_def() -> FunctionDef {
    def("sleep_tail_entry", &[I64, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Callt {
            callee: k(1),
            nargs: 1,
        },
    ])
}

/// The `()` trap child for the cancellation fixture: 1/0.
fn trap_child_def() -> FunctionDef {
    FunctionDef {
        name: "trap_child".into(),
        layout: FrameLayout::new(vec![I64, I64], 0, 0).expect("valid fixture layout"),
        pool: vec![Const::I64(1), Const::I64(0)],
        code: assemble(&[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ])
        .expect("fixture body encodes"),
    }
}

/// One probe run, answering the elapsed nanoseconds the program
/// measured over `mono` around its own sleep.
fn probe(vm: &mut Vm, nanos: i64) -> i64 {
    let answer = vm
        .run("io", "sleep_probe", &[Value::I64(nanos)])
        .expect("the probe lands");
    let [Value::I64(elapsed)] = answer.as_slice() else {
        panic!("the probe answers one word, got {answer:?}");
    };
    *elapsed
}

/// A parked sleep wakes at its deadline: the span measured over `mono`
/// covers at least the request (the deadline wheel drew the line, no
/// thread blocked), the generous ceiling only guarding a runaway.
#[test]
fn a_parked_sleep_waits_at_least_its_span() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![sleep_probe_def()]);
    let started = Instant::now();
    let elapsed = probe(&mut vm, 80_000_000);
    assert!(
        elapsed >= 80_000_000,
        "the sleep waited {elapsed}ns under the request"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a runaway wait: {:?}",
        started.elapsed()
    );
}

/// The degenerate spans answer at once: a zero or negative request is
/// a deadline already due, the next wheel sweep delivering the unit.
#[test]
fn degenerate_spans_answer_at_once() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![sleep_probe_def()]);
    let started = Instant::now();
    let zero = probe(&mut vm, 0);
    assert!(zero >= 0, "mono never moves backward: {zero}");
    let negative = probe(&mut vm, -1_000_000_000);
    assert!(negative >= 0, "mono never moves backward: {negative}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the degenerate spans ran away: {:?}",
        started.elapsed()
    );
}

/// The monotonic clock measures elapsed time: a sleep bracketed by two
/// `mono` reads answers the span between them, never a negative word —
/// the probe's own arithmetic IS the non-regression witness, run twice
/// so the second bracket starts strictly after the first one closed.
#[test]
fn mono_reads_never_move_backward() {
    let mut vm = scheduled_vm(vec![sleep_probe_def()]);
    let first = probe(&mut vm, 1_000_000);
    let second = probe(&mut vm, 1_000_000);
    assert!(
        first >= 1_000_000,
        "the first wait covered its span: {first}"
    );
    assert!(
        second >= 1_000_000,
        "the second wait covered its span: {second}"
    );
}

/// Two nursery children sleeping concurrently complete under one join
/// with their waits overlapped — the serialized twin needs both waits
/// back to back, the scheduler needs the longer one alone.
#[test]
fn parked_sleep_siblings_overlap_their_waits() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![sleep_probe_def()]);
    let wait = Duration::from_millis(300);
    let nanos: i64 = 300_000_000;
    let nursery = vm.open_root_nursery();
    let a = vm
        .spawn_fiber_in(nursery, "io", "sleep_probe", &[Value::I64(nanos)])
        .expect("child a spawns");
    let b = vm
        .spawn_fiber_in(nursery, "io", "sleep_probe", &[Value::I64(nanos)])
        .expect("child b spawns");
    assert!(a != b, "two children, two slots");
    let started = Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    let elapsed = started.elapsed();
    match joined {
        ds_vm::JoinOutcome::Joined(answers) => {
            assert_eq!(answers.len(), 2, "both children landed");
            for answer in &answers {
                let [Value::I64(each)] = answer.as_slice() else {
                    panic!("a child answers one word, got {answer:?}");
                };
                assert!(
                    *each >= nanos,
                    "each wait covered its span: {each} under {nanos}"
                );
            }
        },
        other @ ds_vm::JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
    }
    assert!(
        elapsed < 2 * wait,
        "the waits overlapped ({elapsed:?} vs {:?} serialized)",
        2 * wait
    );
}

/// The entry frame's own tail park on a sleep: the run parks with the
/// final-delivery sentinel and the wheel's firing completes it with
/// the delivered unit word as the run's own result.
#[test]
fn the_entry_frames_tail_park_delivers_the_unit_word() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![sleep_tail_entry_def()]);
    let started = Instant::now();
    let answer = vm
        .run("io", "sleep_tail_entry", &[Value::I64(40_000_000)])
        .expect("the entry tail park lands");
    assert_eq!(
        answer.as_slice(),
        &[Value::I64(0)],
        "the sleep's delivered answer is the unit word"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(40),
        "the tail park waited its span"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a runaway wait: {:?}",
        started.elapsed()
    );
}

/// Parked-sleep cancellation: a sibling's trap raises the flag and the
/// drive's next sweep cuts the sleeping child at the resume-shaped
/// point — its wait dropped from the table, no reactor interest ever
/// having stood to deregister.
#[test]
fn a_cancelled_nursery_cuts_its_sleeping_child() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![sleep_probe_def(), trap_child_def()]);
    let nursery = vm.open_root_nursery();
    let parked = vm
        .spawn_fiber_in(nursery, "io", "sleep_probe", &[Value::I64(5_000_000_000)])
        .expect("the sleeping child spawns");
    let failing = vm
        .spawn_fiber_in(nursery, "io", "trap_child", &[])
        .expect("the failing child spawns");
    assert_ne!(parked, failing);
    let started = Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the cut landed long before the slept span: {:?}",
        started.elapsed()
    );
    match joined {
        ds_vm::JoinOutcome::Failed(outcomes) => {
            assert_eq!(outcomes.len(), 2);
            assert!(
                matches!(outcomes[0], ds_vm::ChildOutcome::Cancelled),
                "the sleeping child cut: {:?}",
                outcomes[0]
            );
            assert!(
                matches!(outcomes[1], ds_vm::ChildOutcome::Trapped(_)),
                "the failing child trapped: {:?}",
                outcomes[1]
            );
        },
        other @ ds_vm::JoinOutcome::Joined(_) => panic!("the boundary reported {other:?}"),
    }
}

/// No reactor installed: the identical program keeps the blocking
/// semantics — the thread itself sleeps the span, the embedder's
/// platform choice.
#[test]
fn without_a_reactor_the_sleep_blocks_the_thread() {
    let mut vm = Vm::new(registry(vec![sleep_probe_def()]));
    vm.capture_output();
    let started = Instant::now();
    let elapsed = probe(&mut vm, 60_000_000);
    assert!(
        elapsed >= 60_000_000,
        "the blocking twin waited its span: {elapsed}ns"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a runaway wait: {:?}",
        started.elapsed()
    );
}
