//! The process face's parked e2e: hand-
//! assembled programs over the std platform's process face, a VM with
//! the platform's reactor installed, and `sh` children — hermetic, no
//! external network. Linux alone: the parked twins are the epoll
//! driver's own (the child's pipes and its pidfd the interests), every
//! other host keeping the blocking twins the proc unit tests and the
//! CLI e2e already cover. What the fixtures pin:
//!
//! - a parked `proc_read`/`proc_read_err` wakes on the child's write: the pipe's readability the
//!   interest, the wake performing the one read and delivering its answer at the parked call site —
//!   the same answer the blocking twin answers. The drained pipe's close wakes the same row: the
//!   kernel spells that end HUP alone, no IN bit, the wake answering the empty string — the pipe's
//!   orderly end.
//! - a parked `proc_wait` wakes on the child's exit: the pidfd readable, the wake reaping the
//!   status non-blockingly and answering it, a second wait the reaped word again.
//! - the close wake: a handle closed while a read or wait parks on it wakes the waiter now — the
//!   read as the pipe's orderly end (the empty string, the HUP wake's own answer over the killed
//!   child), the wait as the closed handle's io floor (the blocking twin's post-close answer), the
//!   deadline never the line.
//! - the deadline: a parked row with no wake answers the timeout io span at its configured bound —
//!   the bounded-wait discipline over the wheel, the process face's own member shape.
//! - parked-IO cancellation: a sibling's trap cuts the world-parked child, its interest gone with
//!   the cut.
//! - two proc interests waking in one window (a pipe read and a pidfd wait) both complete — the
//!   multi-event batch discipline the epoll packed-ABI fix pinned.
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use ds_vm::{
    ChildOutcome, Const, ConstFuncRef, FrameAnswer, FunctionDef, JoinOutcome, ModuleRegistry,
    ModuleVersion, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{BUILTIN_CONCAT, BUILTIN_SPLIT},
    platform::{
        Capability, OP_PROC_CLOSE, OP_PROC_READ, OP_PROC_READ_ERR, OP_PROC_SPAWN, OP_PROC_WAIT,
        Platform, STD_NAME,
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

/// The shared pool: the two string seams, the five proc rows, the
/// shell's name, the separator, the `-c` prefix, and the read bound.
fn pool() -> Vec<Const> {
    vec![
        builtin(BUILTIN_CONCAT),        // k0
        builtin(BUILTIN_SPLIT),         // k1
        Const::Str(format!("-c{SEP}")), // k2
        Const::Str(SEP.into()),         // k3
        Const::Str("sh".into()),        // k4
        platform(OP_PROC_SPAWN),        // k5
        platform(OP_PROC_READ),         // k6
        platform(OP_PROC_READ_ERR),     // k7
        platform(OP_PROC_WAIT),         // k8
        platform(OP_PROC_CLOSE),        // k9
        Const::I64(4096),               // k10: the read bound
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
    def("spawn_child", &[Ref, Ref, Raw, Ref], 1, 3, &[
        Uop::Funcf { frame_slots: 4 },
        // "-c<sep>" ++ script
        Uop::Kstr { dst: 1, kidx: k(2) },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        // split the joined argv on the separator, the union tag word
        // staging beside it
        Uop::TagNew { dst: 2, tag: 0 }, // the separator's union tag word
        Uop::Kstr { dst: 3, kidx: k(3) },
        Uop::Call {
            callee: k(1),
            nargs: 3,
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

/// `(pid: int) -> string`: one `proc_read` — the stdout parking probe.
fn read_child_def() -> FunctionDef {
    def("read_child", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm {
            dst: 2,
            kidx: k(10),
        },
        Uop::Call {
            callee: k(6),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(pid: int) -> string`: one `proc_read_err` — the stderr parking
/// probe, the stdout twin's own shape.
fn read_err_child_def() -> FunctionDef {
    def("read_err_child", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm {
            dst: 2,
            kidx: k(10),
        },
        Uop::Call {
            callee: k(7),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(pid: int) -> int`: one `proc_wait` — the exit parking probe.
fn wait_child_def() -> FunctionDef {
    def("wait_child", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(8),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(pid: int) -> int`: the total close, the cleanup every fixture's
/// child goes home through.
fn close_child_def() -> FunctionDef {
    def("close_child", &[I64, Raw, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(9),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// The `()` trap child for the cancellation fixture: 1/0 (its own
/// pool, the shared one carrying no bare constants).
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

fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[Capability::Proc]));
    reg.install(ModuleVersion::new("io", defs).expect("valid fixture module"));
    reg
}

/// A VM over `defs` with the platform's reactor installed (the parking
/// discipline) and a captured sink. The runaway bound covers a machine
/// under parallel-suite load, not any test's semantics.
fn scheduled_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    vm.capture_output();
    vm.set_proc_timeout(Duration::from_secs(90));
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
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

/// The payload word of one answer, read as its string.
fn answer_string(vm: &Vm, answer: &FrameAnswer) -> String {
    let [Value::Ref(id)] = answer.as_slice() else {
        panic!("the answer is a string ref, got {answer:?}");
    };
    let id = *id;
    vm.objects_of(id)
        .str_at(id.id())
        .expect("the answer names a string")
        .to_owned()
}

/// The payload word of one answer, read as its status int.
fn answer_i64(answer: &FrameAnswer) -> i64 {
    let [Value::I64(word)] = answer.as_slice() else {
        panic!("the answer is a status word, got {answer:?}");
    };
    *word
}

/// A parked `proc_read` wakes on the child's write: the answer is the
/// written bytes delivered at the parked call site, the stderr twin the
/// same shape on its own pipe.
#[test]
fn parked_reads_wake_on_the_childs_writes() {
    let mut vm = scheduled_vm(vec![
        spawn_def(),
        read_child_def(),
        read_err_child_def(),
        close_child_def(),
    ]);
    let out_pid = spawn(&mut vm, "sleep 0.25; echo late data");
    let answer = vm
        .run("io", "read_child", &[Value::I64(out_pid)])
        .expect("the parked read lands");
    assert_eq!(answer_string(&vm, &answer), "late data\n");
    let err_pid = spawn(&mut vm, "sleep 0.25; echo err-data 1>&2");
    let err_answer = vm
        .run("io", "read_err_child", &[Value::I64(err_pid)])
        .expect("the parked stderr read lands");
    assert_eq!(answer_string(&vm, &err_answer), "err-data\n");
    for pid in [out_pid, err_pid] {
        vm.run("io", "close_child", &[Value::I64(pid)])
            .expect("the close lands");
    }
}

/// A parked read wakes on the drained pipe's close: the child's bytes
/// read first, then the child exiting with the pipe drained reports
/// the kernel's HUP-alone spelling — the wake answering the empty
/// string, the pipe's orderly end, not a lost wake over the exit
/// window (the loop bounded, so a timeout span never reads as data
/// forever).
#[test]
fn a_parked_read_wakes_on_the_drained_pipes_close() {
    let mut vm = scheduled_vm(vec![spawn_def(), read_child_def(), close_child_def()]);
    let pid = spawn(&mut vm, "echo hi; sleep 0.3");
    let mut saw = String::new();
    for _ in 0..16 {
        let answer = vm
            .run("io", "read_child", &[Value::I64(pid)])
            .expect("the parked read lands");
        let text = answer_string(&vm, &answer);
        if text.is_empty() {
            assert_eq!(saw, "hi\n", "the child's bytes read before the close");
            vm.run("io", "close_child", &[Value::I64(pid)])
                .expect("the close lands");
            return;
        }
        saw.push_str(&text);
    }
    panic!("the drained pipe's close never woke the parked read: {saw:?}");
}

/// A parked `proc_wait` wakes on the child's exit: the pidfd's
/// readability reaps the status and answers it, a second wait the
/// reaped word again without touching the child.
#[test]
fn a_parked_wait_wakes_on_the_childs_exit() {
    let mut vm = scheduled_vm(vec![spawn_def(), wait_child_def(), close_child_def()]);
    let pid = spawn(&mut vm, "sleep 0.3; exit 7");
    let answer = vm
        .run("io", "wait_child", &[Value::I64(pid)])
        .expect("the parked wait lands");
    assert_eq!(answer_i64(&answer), 7, "the child's status answers");
    let again = vm
        .run("io", "wait_child", &[Value::I64(pid)])
        .expect("the second wait lands");
    assert_eq!(answer_i64(&again), 7, "the reaped word again");
    vm.run("io", "close_child", &[Value::I64(pid)])
        .expect("the close lands");
}

/// A handle closed while a read parks on its pipe wakes as the
/// orderly end: the close kills the child holding the write end, and
/// the wake door answers HUP's empty string at the parked
/// call site — never the closed table's io floor, and never the
/// deadline's timeout span (the bound set far past the close, so only
/// the wake can end the park).
#[test]
fn a_close_under_a_parked_read_wakes_as_the_orderly_end() {
    let mut vm = scheduled_vm(vec![spawn_def(), read_child_def(), close_child_def()]);
    let pid = spawn(&mut vm, "sleep 30");
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "io", "read_child", &[Value::I64(pid)])
        .expect("the reader child spawns");
    vm.spawn_fiber_in(nursery, "io", "close_child", &[Value::I64(pid)])
        .expect("the closing child spawns");
    let started = Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    let elapsed = started.elapsed();
    match joined {
        JoinOutcome::Joined(answers) => {
            assert_eq!(answers.len(), 2, "both children landed");
            assert_eq!(
                answer_string(&vm, &answers[0].clone()),
                "",
                "the close wake answered the pipe's orderly end"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
    }
    // the configured bound is the 90s runaway floor, so a deadline
    // wake answers past 30s: only the close wake lands this fast
    assert!(
        elapsed < Duration::from_secs(30),
        "the wake ended the park before its bound ({elapsed:?})"
    );
}

/// A handle closed while a wait parks on its exit answers the closed
/// handle's io floor — the blocking twin's post-close answer — the
/// wake door ending the park now instead of the deadline.
#[test]
fn a_close_under_a_parked_wait_answers_the_closed_handle_floor() {
    let mut vm = scheduled_vm(vec![spawn_def(), wait_child_def(), close_child_def()]);
    let pid = spawn(&mut vm, "sleep 30");
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "io", "wait_child", &[Value::I64(pid)])
        .expect("the waiting child spawns");
    vm.spawn_fiber_in(nursery, "io", "close_child", &[Value::I64(pid)])
        .expect("the closing child spawns");
    let started = Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    let elapsed = started.elapsed();
    match joined {
        JoinOutcome::Joined(answers) => {
            assert_eq!(answers.len(), 2, "both children landed");
            let text = answer_string(&vm, &answers[0].clone());
            assert!(
                text.contains("proc_wait") && text.contains("unknown process"),
                "the close wake answered the closed handle's own floor: {text}"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(30),
        "the wake ended the park before its bound ({elapsed:?})"
    );
}

/// A parked row with no wake answers the timeout io span at its
/// configured bound — the bounded-wait discipline over the wheel, both
/// the wait and the read rows.
#[test]
fn parked_rows_wake_at_their_deadline_with_the_timeout_span() {
    let mut vm = scheduled_vm(vec![
        spawn_def(),
        read_child_def(),
        wait_child_def(),
        close_child_def(),
    ]);
    vm.set_proc_timeout(Duration::from_millis(150));
    let pid = spawn(&mut vm, "sleep 30");
    let waited = vm
        .run("io", "wait_child", &[Value::I64(pid)])
        .expect("the deadline wake lands");
    let text = answer_string(&vm, &waited);
    assert!(
        text.contains("proc_wait") && text.contains("timed out"),
        "the timeout message names the wait: {text}"
    );
    let read = vm
        .run("io", "read_child", &[Value::I64(pid)])
        .expect("the read deadline wake lands");
    let text = answer_string(&vm, &read);
    assert!(
        text.contains("proc_read") && text.contains("timed out"),
        "the timeout message names the read: {text}"
    );
    vm.run("io", "close_child", &[Value::I64(pid)])
        .expect("the close kills the sleeping child");
}

/// Parked-IO cancellation: a sibling's trap raises the flag, and the
/// drive's sweep cuts the world-parked child — its interest
/// deregistered, the boundary reporting `Cancelled` beside the failing
/// `Trapped`, the sleeping child killed by the host's close.
#[test]
fn a_cancelled_nursery_cuts_its_world_parked_proc_child() {
    let mut vm = scheduled_vm(vec![
        spawn_def(),
        wait_child_def(),
        close_child_def(),
        trap_child_def(),
    ]);
    let pid = spawn(&mut vm, "sleep 30");
    let nursery = vm.open_root_nursery();
    let parked = vm
        .spawn_fiber_in(nursery, "io", "wait_child", &[Value::I64(pid)])
        .expect("the parked child spawns");
    let failing = vm
        .spawn_fiber_in(nursery, "io", "trap_child", &[])
        .expect("the failing child spawns");
    assert_ne!(parked, failing);
    let joined = vm.join_nursery(nursery).expect("the join lands");
    match joined {
        JoinOutcome::Failed(outcomes) => {
            assert_eq!(outcomes.len(), 2);
            assert!(
                matches!(outcomes[0], ChildOutcome::Cancelled),
                "the parked child cut: {:?}",
                outcomes[0]
            );
            assert!(
                matches!(outcomes[1], ChildOutcome::Trapped(_)),
                "the failing child trapped: {:?}",
                outcomes[1]
            );
        },
        other @ JoinOutcome::Joined(_) => panic!("the boundary reported {other:?}"),
    }
    vm.run("io", "close_child", &[Value::I64(pid)])
        .expect("the close kills the sleeping child");
}

/// Two proc interests waking in one window — a pipe read and a pidfd
/// wait, the two interest kinds the face mints — both complete under
/// one join, their waits overlapped (the serialized blocking twin
/// needing both sleeps back to back).
#[test]
fn proc_interests_wake_packed_into_one_window() {
    let mut vm = scheduled_vm(vec![
        spawn_def(),
        read_child_def(),
        wait_child_def(),
        close_child_def(),
    ]);
    let reader_pid = spawn(&mut vm, "sleep 0.4; echo alpha");
    let wait_pid = spawn(&mut vm, "sleep 0.4");
    let nursery = vm.open_root_nursery();
    let reader = vm
        .spawn_fiber_in(nursery, "io", "read_child", &[Value::I64(reader_pid)])
        .expect("the reader child spawns");
    let waiter = vm
        .spawn_fiber_in(nursery, "io", "wait_child", &[Value::I64(wait_pid)])
        .expect("the waiter child spawns");
    assert_ne!(reader, waiter);
    let started = Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    let elapsed = started.elapsed();
    match joined {
        JoinOutcome::Joined(answers) => {
            assert_eq!(answers.len(), 2, "both children landed");
            assert_eq!(
                answer_string(&vm, &answers[0].clone()),
                "alpha\n",
                "the read woke on the write"
            );
            let [Value::I64(status)] = answers[1].as_slice() else {
                panic!("the wait answered a status: {:?}", answers[1]);
            };
            assert_eq!(*status, 0, "the exiting child's status");
        },
        other @ JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
    }
    // the serialized twin needs both sleeps back to back (an 800ms
    // floor), while a parallel suite can overshoot a child's wake in
    // wall time — the bound tolerates one sleep of overshoot
    assert!(
        elapsed < Duration::from_millis(1200),
        "the waits overlapped ({elapsed:?} vs 800ms serialized)"
    );
    for pid in [reader_pid, wait_pid] {
        vm.run("io", "close_child", &[Value::I64(pid)])
            .expect("the close lands");
    }
}
