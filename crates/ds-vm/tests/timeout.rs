//! The timer race e2e over the `timeout` seam (builtin 107): hand-
//! assembled programs staging a callable word beside the deadline's
//! nanoseconds, the clock itself as the witness — hermetic, bounds with
//! generous headroom (a concurrent lane shares the box). What the
//! fixtures pin:
//!
//! - the two-word answer span lands at the parked call: the ok discriminant with the racer's one
//!   answer word as the payload, the `timed_out` class tag with the inert unit word on expiry.
//! - a racer that parks wakes through the scheduler and still answers first.
//! - the wheel's deadline door over a parked racer: the racer is cut at its park (the long sleep
//!   never runs out) and the call answers the failure span.
//! - a looping racer whose body parks each iteration is cut at the same park door.
//! - a racer that never parks is cut at its back-edge by the interpreter's deadline poll — the
//!   wheel fires only between drives, so the poll is the never-parking racer's door.
//! - a deadline already past answers the failure span at once, no racer spawning, no park.
//! - a trapping racer's trap re-raises on the caller.
//! - the wheel alone drives the race: with no reactor installed the identical program answers the
//!   same.

use std::time::{Duration, Instant};

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw},
        uop::Uop,
    },
    assemble,
    fiber::{FIBER_ROOT, FiberId, FiberState},
    native::{BUILTIN_TIMEOUT, PLATFORM_IO_OK, PLATFORM_IO_TIMED_OUT},
    platform::{Capability, OP_SLEEP, Platform, STD_NAME},
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn native(module: &str, func: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: module.into(),
        func,
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

/// The racer bodies, `(x: int) -> int`-shaped entry frames with no
/// parameters: the plain answer, the short and the long park, the loop
/// that parks each iteration, and the division by zero.
const THUNK: u32 = 0;
const NAPPER: u32 = 1;
const SLEEPER: u32 = 2;
const PARKED_LOOPER: u32 = 3;
const TRAPPER: u32 = 4;
const SPIN_LOOPER: u32 = 5;
/// The napper's park: far inside every generous deadline the tests arm.
const NAP: i64 = 20_000_000;
/// The sleeper's park: far past every deadline, and the bound a runaway
/// wait surfaces under.
const LONG: i64 = 2_000_000_000;

fn racer_defs() -> Vec<FunctionDef> {
    vec![
        def("thunk", &[I64], 0, 0, vec![Const::I64(42)], &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ]),
        def("napper", &[I64, Raw], 0, 1, park_pool(NAP), &park_body()),
        def("sleeper", &[I64, Raw], 0, 1, park_pool(LONG), &park_body()),
        // the parking looper: a backward branch around one park each
        // iteration, so the deadline's wheel meets it parked every time
        def("parked_looper", &[I64, Raw], 0, 1, park_pool(NAP), &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Jmp { target: 1 },
        ]),
        def(
            "trapper",
            &[I64, I64, I64],
            0,
            0,
            vec![Const::I64(42), Const::I64(0)],
            &[
                Uop::Funcf { frame_slots: 3 },
                Uop::Kimm { dst: 0, kidx: k(0) },
                Uop::Kimm { dst: 1, kidx: k(1) },
                Uop::DivI64 {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                },
                Uop::Ret { src: 2 },
            ],
        ),
        // the spinner: a backward branch per iteration and no park
        // anywhere, so no wheel door ever meets it — the back-edge
        // deadline poll is the cut it answers
        def("spin_looper", &[I64, Raw], 0, 1, vec![Const::I64(42)], &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Jmp { target: 1 },
        ]),
    ]
}

fn park_pool(span: i64) -> Vec<Const> {
    vec![native(STD_NAME, OP_SLEEP), Const::I64(span), Const::I64(42)]
}

fn park_body() -> Vec<Uop> {
    vec![
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 1, kidx: k(1) },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        Uop::Kimm { dst: 0, kidx: k(2) },
        Uop::Ret { src: 0 },
    ]
}

/// `(nanos: int) -> tag` over a plain `CALL` of the race, answering the
/// span's discriminant word: the ok tag when the racer answered, the
/// `timed_out` class tag when the deadline did. The deadline and the
/// racer's scalar function word stage into the two-word area, the
/// discriminant read back out of its first word.
fn race_disc_def(name: &str, racer: u32) -> FunctionDef {
    def(name, &[I64, Raw, Raw], 1, 2, race_pool(racer), &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kconst { dst: 2, kidx: k(1) },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(nanos: int) -> int` over the same call, answering the span's
/// payload word: the racer's answer when it landed, the inert unit word
/// on expiry.
fn race_word_def(name: &str, racer: u32) -> FunctionDef {
    def(name, &[I64, I64, Raw, Raw], 1, 2, race_pool(racer), &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kconst { dst: 3, kidx: k(1) },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::Mov { dst: 1, src: 3 },
        Uop::Ret { src: 1 },
    ])
}

fn race_pool(racer: u32) -> Vec<Const> {
    vec![native("builtin", BUILTIN_TIMEOUT), native("to", racer)]
}

/// The whole fixture: five racers, one discriminant site per racer, one
/// payload site for the plain answer.
fn defs() -> Vec<FunctionDef> {
    let mut defs = racer_defs();
    for (name, racer) in [
        ("race_thunk", THUNK),
        ("race_napper", NAPPER),
        ("race_sleeper", SLEEPER),
        ("race_parked_looper", PARKED_LOOPER),
        ("race_trapper", TRAPPER),
        ("race_spin_looper", SPIN_LOOPER),
    ] {
        defs.push(race_disc_def(name, racer));
    }
    defs.push(race_word_def("race_thunk_word", THUNK));
    defs
}

fn registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[Capability::Time]));
    reg.install(ModuleVersion::new("to", defs()).expect("valid fixture module"));
    reg
}

/// A VM over the fixture with the platform's reactor installed (the
/// parking discipline).
fn scheduled_vm() -> Vm {
    let mut vm = Vm::new(registry());
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

/// Whether this host parks (a reactor driver exists): the parked-racer
/// tests ride it, the rest run everywhere.
fn parks() -> bool {
    ds_vm::reactor::platform_default().is_some()
}

fn answer(vm: &mut Vm, func: &str, nanos: i64) -> Result<FrameAnswer, Trap> {
    let run = vm.run("to", func, &[Value::I64(nanos)]);
    if run.is_err() {
        // the trapped root's frames stay for a backtrace no test reads:
        // the release door drains them here
        vm.release_interrupted(FIBER_ROOT);
    }
    run
}

/// A deadline far past every fast fixture's completion.
const SPARE: i64 = 5_000_000_000;
/// A deadline far inside the long park and the loop's parks.
const SOON: i64 = 20_000_000;

#[test]
fn the_answer_span_lands_at_the_parked_call() {
    // no reactor installed: the wheel alone drives the race
    let mut vm = Vm::new(registry());
    assert_eq!(
        answer(&mut vm, "race_thunk", SPARE),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_OK))),
        "the ok discriminant at the span's first word"
    );
    assert_eq!(
        answer(&mut vm, "race_thunk_word", SPARE),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the racer's answer word at the span's payload word"
    );
}

#[test]
fn a_parked_racer_wakes_and_still_answers() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm();
    let started = Instant::now();
    assert_eq!(
        answer(&mut vm, "race_napper", SPARE),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_OK))),
        "the racer's park woke inside the deadline"
    );
    assert!(
        started.elapsed().as_nanos() >= u128::try_from(NAP).expect("test span fits") * 3 / 4,
        "the racer parked for its span"
    );
}

#[test]
fn the_deadline_cuts_a_parked_racer_at_its_park() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm();
    let started = Instant::now();
    assert_eq!(
        answer(&mut vm, "race_sleeper", SOON),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
        "the deadline answers the failure span"
    );
    let elapsed = started.elapsed().as_nanos();
    assert!(
        elapsed >= u128::try_from(SOON).expect("test span fits") * 3 / 4,
        "the deadline was waited out"
    );
    assert!(
        elapsed < u128::try_from(LONG).expect("test span fits") / 4,
        "the racer was cut at its park, the long sleep never ran out"
    );
    // the racer died at the cut and its slot stands free again
    assert_eq!(vm.fiber_state(FiberId(1)), Some(FiberState::Dead));
}

#[test]
fn the_deadline_cuts_a_parking_looper_at_its_park() {
    if !parks() {
        return;
    }
    // a racer that loops around a park each iteration: the wheel meets
    // it parked, so the deadline cuts it at that park and the answer is
    // the failure span, the loop never running out
    let mut vm = scheduled_vm();
    let started = Instant::now();
    assert_eq!(
        answer(&mut vm, "race_parked_looper", SOON),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
        "the deadline answers the failure span over the looping racer"
    );
    let elapsed = started.elapsed().as_nanos();
    assert!(
        elapsed >= u128::try_from(SOON).expect("test span fits") * 3 / 4,
        "the deadline was waited out"
    );
    assert!(
        elapsed < u128::try_from(LONG).expect("test span fits") / 4,
        "the loop was cut at its park, never run out"
    );
    assert_eq!(vm.fiber_state(FiberId(1)), Some(FiberState::Dead));
}

#[test]
#[test]
fn the_deadline_cuts_a_spinning_racer_at_its_back_edge() {
    // a racer that never parks: the wheel fires only between drives,
    // so the deadline answers through the back-edge poll — the same
    // failure span, the racer a cancelled death, the drive never hung
    let mut vm = Vm::new(registry());
    let started = Instant::now();
    assert_eq!(
        answer(&mut vm, "race_spin_looper", SOON),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
        "the deadline answers the failure span over the never-parking racer"
    );
    let elapsed = started.elapsed().as_nanos();
    assert!(
        elapsed >= u128::try_from(SOON).expect("test span fits") * 3 / 4,
        "the deadline was waited out"
    );
    assert!(
        elapsed < Duration::from_secs(5).as_nanos(),
        "the racer was cut at its back-edge, the loop never run out"
    );
    assert_eq!(vm.fiber_state(FiberId(1)), Some(FiberState::Dead));
}

fn a_deadline_already_past_answers_the_failure_at_once() {
    // no reactor installed, and the failure answers in place: the drive
    // never parks, no racer ever spawns
    let mut vm = Vm::new(registry());
    for nanos in [0, -7] {
        assert_eq!(
            answer(&mut vm, "race_thunk", nanos),
            Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
            "{nanos}: the failure span answered at once"
        );
        // no racer ever spawned: the pool holds the root alone
        assert_eq!(vm.fiber_state(FiberId(1)), None, "{nanos}");
    }
    assert_eq!(
        answer(&mut vm, "race_thunk_word", 0),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the failure span's payload is the inert unit word"
    );
}

#[test]
fn a_trapping_racers_trap_is_re_raised_on_the_caller() {
    let mut vm = Vm::new(registry());
    assert_eq!(
        answer(&mut vm, "race_trapper", SPARE),
        Err(Trap::DivideByZero)
    );
    assert_eq!(vm.fiber_state(FiberId(1)), Some(FiberState::Dead));
}
