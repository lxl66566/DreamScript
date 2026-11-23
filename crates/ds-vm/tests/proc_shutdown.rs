//! The process face's blocking wait twin under the host's stop flag
//! (the 2026-11-24 signal ruling's non-reactor half, item 96's
//! recorded gap): a waitpid owns no portable interrupt shape, so a
//! reactor-less host's `proc_wait` polls the child non-blockingly on a
//! bounded sleep loop — a flipped stop flag cutting a blocked wait
//! within one interval, the reaped-status answers identical to the
//! plain wait's own (the idempotent reaped word, `try_wait`
//! untouched). Hand-assembled fixtures over the std platform's
//! process face with no reactor installed, `sh` children — hermetic,
//! no external network. Unix alone: the loop shape itself is the
//! portable one, the long-sleep child's shell the gate (the parked
//! suite `proc_park.rs`'s own hermeticity precedent); the blocking
//! rows' platform reach stays the unit tests' own.

#![cfg(unix)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{BUILTIN_CONCAT, BUILTIN_SPLIT, PLATFORM_IO_OK, PLATFORM_NONE},
    platform::{
        Capability, OP_PROC_CLOSE, OP_PROC_SPAWN, OP_PROC_TRY_WAIT, OP_PROC_WAIT, Platform,
        STD_NAME,
    },
};

/// The separator the spawn fixture's argument list builds over — a
/// byte no shell script spells.
const SEP: &str = "\u{1}";

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn platform(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: STD_NAME.into(),
        func: index,
    })
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// The shared pool: the two string seams, the four proc rows, the
/// shell's name, the separator, and the `-c` prefix.
fn pool() -> Vec<Const> {
    vec![
        builtin(BUILTIN_CONCAT),        // k0
        builtin(BUILTIN_SPLIT),         // k1
        Const::Str(format!("-c{SEP}")), // k2
        Const::Str(SEP.into()),         // k3
        Const::Str("sh".into()),        // k4
        platform(OP_PROC_SPAWN),        // k5
        platform(OP_PROC_WAIT),         // k6
        platform(OP_PROC_TRY_WAIT),     // k7
        platform(OP_PROC_CLOSE),        // k8
    ]
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// `(script: string) -> int`: spawn `sh -c <script>` and answer its
/// handle — the argv built over the string seams (`-c` and the script
/// one word each, the separator joining then splitting them), the
/// spawn's ok payload the handle word.
fn spawn_def() -> FunctionDef {
    def("spawn_child", &[Ref, Ref, Ref, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 4 },
        // "-c<sep>" ++ script
        Uop::Kstr { dst: 1, kidx: k(2) },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        // split the joined argv on the separator
        Uop::Kstr { dst: 2, kidx: k(3) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        // the list word (slot 1) rides as the spawn's second argument
        Uop::Mov { dst: 2, src: 1 },
        Uop::Kstr { dst: 1, kidx: k(4) },
        Uop::Call {
            callee: k(5),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(pid: int) -> int`: one `proc_wait` — the blocking twin's probe,
/// its ok payload the status word.
fn wait_child_def() -> FunctionDef {
    def("wait_child", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(6),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(pid: int) -> tag`: one `proc_try_wait` — the poll's checked span,
/// the answer the span's own discriminant (ok beside the status,
/// none while the child runs).
fn try_wait_child_def() -> FunctionDef {
    def("try_wait_child", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(7),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(pid: int) -> int`: the total close, the cleanup every fixture's
/// child goes home through.
fn close_child_def() -> FunctionDef {
    def("close_child", &[I64, Raw, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[Capability::Proc]));
    reg.install(ModuleVersion::new("io", defs).expect("valid fixture module"));
    reg
}

/// A reactor-less VM over `defs` — the blocking twins' own host, the
/// parked suite's twin without the reactor.
fn blocking_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    vm.capture_output();
    vm
}

/// One spawned `sh -c <script>` child's handle, the spawn fixture run
/// from the host.
fn spawn(vm: &mut Vm, script: &str) -> i64 {
    let staged = vm.intern_str(script);
    let answer = vm
        .run("io", "spawn_child", &[staged])
        .expect("the spawn lands");
    let [Value::I64(pid)] = answer.as_slice() else {
        panic!("the spawn answered no handle: {answer:?}");
    };
    *pid
}

/// One fixture's answer, read as its single i64 word.
fn answer_i64(answer: &FrameAnswer) -> i64 {
    let [Value::I64(word)] = answer.as_slice() else {
        panic!("the answer is a status word, got {answer:?}");
    };
    *word
}

/// One fixture's answer, read as its single tag word.
fn answer_tag(answer: &FrameAnswer) -> u8 {
    let [Value::Tag(tag)] = answer.as_slice() else {
        panic!("the answer is a tag word, got {answer:?}");
    };
    *tag
}

/// A blocked wait cut by a flipped stop flag answers the shutdown trap
/// within one poll interval — the reactor-less embedder's blocking
/// fallback, the cut recorded for the host's own exit code. The cut
/// is not a reap: the child still runs, its poll answers the none
/// member untouched, and the close kills it clean.
#[test]
fn a_flipped_stop_flag_cuts_the_blocked_wait_within_one_interval() {
    let mut vm = blocking_vm(vec![
        spawn_def(),
        wait_child_def(),
        try_wait_child_def(),
        close_child_def(),
    ]);
    let pid = spawn(&mut vm, "sleep 30");
    // the poll is untouched by the wait twin's loop: a running child
    // answers the none member
    let polled = vm
        .run("io", "try_wait_child", &[Value::I64(pid)])
        .expect("the poll lands");
    assert_eq!(answer_tag(&polled), PLATFORM_NONE);
    let stop = Arc::new(AtomicBool::new(false));
    vm.set_shutdown_flag(Arc::clone(&stop));
    let worker_stop = Arc::clone(&stop);
    let setter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        let flipped = Instant::now();
        worker_stop.store(true, Ordering::Relaxed);
        flipped
    });
    let outcome = vm.run("io", "wait_child", &[Value::I64(pid)]);
    let flipped = setter.join().expect("the flag's setter lands");
    match outcome {
        Err(Trap::Shutdown) => {},
        other => panic!("the stopped wait answers the trap, got {other:?}"),
    }
    assert!(
        vm.shutdown_observed(),
        "the observation is the host's to read"
    );
    assert!(
        flipped.elapsed() < Duration::from_secs(2),
        "the flag cut the blocked wait within one poll interval: {:?}",
        flipped.elapsed()
    );
    // the cut recorded no status: the child still runs, the none
    // member still its poll's own answer, the close still its kill
    let polled = vm
        .run("io", "try_wait_child", &[Value::I64(pid)])
        .expect("the poll lands");
    assert_eq!(answer_tag(&polled), PLATFORM_NONE);
    vm.run("io", "close_child", &[Value::I64(pid)])
        .expect("the close kills the still-running child");
}

/// The loop's reaped-status answers stay the plain wait's own: an
/// exited child's status answers under the ok word, a second wait the
/// reaped word again, and a later poll the same word — no flag
/// installed, today's behavior unchanged.
#[test]
fn the_blocking_wait_answers_the_reaped_status_idempotently() {
    let mut vm = blocking_vm(vec![
        spawn_def(),
        wait_child_def(),
        try_wait_child_def(),
        close_child_def(),
    ]);
    let pid = spawn(&mut vm, "exit 7");
    let waited = vm
        .run("io", "wait_child", &[Value::I64(pid)])
        .expect("the wait lands");
    assert_eq!(answer_i64(&waited), 7, "the child's status answers");
    let again = vm
        .run("io", "wait_child", &[Value::I64(pid)])
        .expect("the second wait lands");
    assert_eq!(answer_i64(&again), 7, "the reaped word again");
    let polled = vm
        .run("io", "try_wait_child", &[Value::I64(pid)])
        .expect("the poll lands");
    assert_eq!(answer_tag(&polled), PLATFORM_IO_OK);
    vm.run("io", "close_child", &[Value::I64(pid)])
        .expect("the close lands");
}

/// The loop reads the child before the flag, so a wait on a child that
/// exited while the flag stood answers the reaped status, never the
/// shutdown trap — the status wins over the standing stop.
#[test]
fn an_exited_childs_status_wins_over_the_standing_stop_flag() {
    let mut vm = blocking_vm(vec![
        spawn_def(),
        wait_child_def(),
        try_wait_child_def(),
        close_child_def(),
    ]);
    let pid = spawn(&mut vm, "sleep 0.4; exit 5");
    let stop = Arc::new(AtomicBool::new(true));
    vm.set_shutdown_flag(stop);
    let cut = vm.run("io", "wait_child", &[Value::I64(pid)]);
    match cut {
        Err(Trap::Shutdown) => {},
        other => panic!("the standing flag cuts the running child's wait, got {other:?}"),
    }
    // the child exits past the cut; a later wait reaps its status
    // through the standing flag
    thread::sleep(Duration::from_millis(600));
    let waited = vm
        .run("io", "wait_child", &[Value::I64(pid)])
        .expect("the reaped wait lands");
    assert_eq!(answer_i64(&waited), 5, "the status wins over the flag");
    vm.run("io", "close_child", &[Value::I64(pid)])
        .expect("the close lands");
}
