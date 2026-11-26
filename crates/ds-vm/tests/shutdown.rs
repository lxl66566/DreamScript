//! The host's shutdown cut and the quiet accept's bound over hand-assembled fixtures:
//!
//! - the shutdown cut: a parked accept under the reactor — its wait now unbounded, the signal its
//!   backstop — observes the host's registered shutdown door (here a socket standing in for the
//!   CLI's signalfd, the reactor's own interest either way) and answers [`ds_vm::Trap::Shutdown`]
//!   with the observation recorded, the engine deciding nothing. The real-signal end to end (the
//!   exit code 128+signum beside the deinit run) lives in the CLI integration suite, which owns a
//!   process.
//! - the nursery-cancel cut reaching the unbounded park: a flagged nursery's parked child dies with
//!   the sentinel through the executor's cut pass — no drive stands over it — so its unbounded wait
//!   cannot strand the honest channel deadlock behind it.
//! - the join sweep's own cut of an unbounded accept child under a raised flag — the parked-IO
//!   cancellation at the root-nursery host-driven shape.
//! - the blocking twin's stop flag: a reactor-less VM keeps the bounded blocking poll, and the
//!   host's flag answers the shutdown trap between rounds — the non-Linux and reactor-less
//!   embedder's door, pinned on every platform.
//!
//! The parked shapes need a reactor driver and early-return without
//! one (the suite's conditional-skip pattern); the blocking twin runs
//! everywhere.

use std::{
    io::Write,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use ds_vm::{
    ChildOutcome, Const, ConstFuncRef, FunctionDef, JoinOutcome, ModuleRegistry, ModuleVersion,
    Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{BUILTIN_CHAN_NEW, BUILTIN_CHAN_RECV, BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN},
    platform::{Capability, OP_NET_ACCEPT, OP_NET_LISTEN, Platform},
    reactor::IoHandle,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn platform(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "std".into(),
        func: index,
    })
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// The shared pool, copied per function: the listener pair, the nursery
/// and channel seams, the bind host, and the trap's operands.
fn pool() -> Vec<Const> {
    vec![
        platform(OP_NET_LISTEN),        // k0
        platform(OP_NET_ACCEPT),        // k1
        builtin(BUILTIN_NURSERY_OPEN),  // k2
        builtin(BUILTIN_NURSERY_SPAWN), // k3
        builtin(BUILTIN_CHAN_NEW),      // k4
        builtin(BUILTIN_CHAN_RECV),     // k5
        Const::Str("127.0.0.1".into()), // k6: the bind host
        Const::I64(1),                  // k7: the trap's dividend
        Const::I64(0),                  // k8: the trap's divisor
        Const::I64(1),                  // k9: the channel's bound
    ]
}

/// The block fixture's own pool: the shared set plus the spawned
/// children's function words (the installed module's own indices).
fn block_pool(accept_child: u32, trap_child: u32) -> Vec<Const> {
    let mut pool = pool();
    pool.push(Const::FuncRef(ConstFuncRef::Named {
        module: "shut".into(),
        func: accept_child,
    })); // k10: the accept child
    pool.push(Const::FuncRef(ConstFuncRef::Named {
        module: "shut".into(),
        func: trap_child,
    })); // k11: the trapping child
    pool
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

/// `(host: string, port: int) -> word`: the bind alone, answering the
/// bound listener's handle — the two-step tests' staging step (the
/// failure message `Ref` answers a stolen port, the retry's cue).
fn bind_def() -> FunctionDef {
    def("bind", &[Ref, I64, Raw, Raw], 2, 2, pool(), &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::Ret { src: 3 }, // the handle on success, the message on failure
    ])
}

/// `(lid: int) -> word`: one accept on the bound listener — the quiet
/// park (unbounded under the reactor, the bounded poll without one),
/// answering the accepted connection's handle on success.
fn accept_child_def() -> FunctionDef {
    def("accept_child", &[I64, Raw, Raw], 1, 2, pool(), &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(1),
            nargs: 1,
        },
        Uop::Ret { src: 2 }, // the accepted connection's handle
    ])
}

/// `() -> int`: 1/0 — the trapping child.
fn trap_child_def() -> FunctionDef {
    def("trap_child", &[I64, I64], 0, 0, pool(), &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 0, kidx: k(7) },
        Uop::Kimm { dst: 1, kidx: k(8) },
        Uop::DivI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Ret { src: 0 },
    ])
}

/// `(lid: int) -> word`: the in-language nursery block — spawn the
/// accept child and the trapping child, then park the joiner on a
/// channel no fiber ever sends to. The trapping child raises the flag
/// mid-park; the accept child's unbounded park must die to the
/// executor's cut so the channel's honest deadlock stays reachable —
/// the stall guard answers, never the parked accept's silence.
fn block_on_chan_def(accept_child: u32, trap_child: u32) -> FunctionDef {
    def(
        "block_on_chan",
        &[I64, I64, SlotType::Fn, Ref, Ref, Raw, Raw],
        1,
        2,
        block_pool(accept_child, trap_child),
        &[
            Uop::Funcf { frame_slots: 7 },
            // the open: the nursery id lands on overlay slot 5
            Uop::Call {
                callee: k(2),
                nargs: 0,
            },
            Uop::Mov { dst: 1, src: 5 },
            // the accept child closure over the listener, then its spawn
            Uop::Kconst {
                dst: 2,
                kidx: k(10),
            },
            Uop::Mov { dst: 5, src: 0 },
            Uop::ClosureNew {
                dst: 3,
                func: 2,
                n: 1,
            },
            Uop::Mov { dst: 5, src: 1 },
            Uop::Mov { dst: 6, src: 3 },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            // the trapping child's spawn (a plain function word)
            Uop::Kconst {
                dst: 2,
                kidx: k(11),
            },
            Uop::Mov { dst: 5, src: 1 },
            Uop::Mov { dst: 6, src: 2 },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            // the joiner parks on a channel nothing sends to
            Uop::Kimm { dst: 5, kidx: k(9) },
            Uop::Call {
                callee: k(4),
                nargs: 1,
            },
            Uop::Mov { dst: 4, src: 5 },
            Uop::Mov { dst: 5, src: 4 },
            Uop::Call {
                callee: k(5),
                nargs: 1,
            },
            Uop::Ret { src: 5 }, // the recv never answers: unreachable
        ],
    )
}

/// The registry over the fixture set the test names; `with_block`
/// installs the in-language block beside the children it spawns.
fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[Capability::Net]));
    reg.install(ModuleVersion::new("shut", defs).expect("valid fixture module"));
    reg
}

/// A VM with the platform's reactor installed when the platform carries
/// a driver (the parking discipline), else blocking.
fn scheduled_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

fn string(vm: &mut Vm, text: &str) -> Value {
    vm.intern_str(text)
}

/// A port with nothing bound on it: bind, note, drop.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    listener.local_addr().expect("bound address").port()
}

/// Binds over fresh free ports until the bind lands, answering the
/// bound listener's handle (the parallel suite can steal a port in
/// the gap, so one retry round covers it).
fn bind_until_free(make: impl Fn() -> Vm) -> (u16, i64, Vm) {
    for _ in 0..8 {
        let port = free_port();
        let mut vm = make();
        let argv = [string(&mut vm, "127.0.0.1"), Value::I64(i64::from(port))];
        match vm.run("shut", "bind", &argv) {
            Ok(answer) => {
                if let [Value::I64(lid)] = answer.as_slice() {
                    return (port, *lid, vm);
                }
                // the bind's failure message: the port was stolen
            },
            Err(trap) => panic!("the bind fixture traps: {trap:?}"),
        }
    }
    panic!("no free loopback port answered the bind");
}

/// A connected loopback pair — the shutdown door's transport: the
/// accepted side registers as the reactor's own interest, the dialer
/// the writer thread signals through.
fn door_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the door's listener");
    let addr: SocketAddr = listener.local_addr().expect("the door's address");
    let dialer = TcpStream::connect(addr).expect("the door dials");
    let (accepted, _) = listener.accept().expect("the door accepts");
    (dialer, accepted)
}

#[cfg(unix)]
fn door_handle(stream: &TcpStream) -> IoHandle {
    use std::os::fd::AsRawFd;
    IoHandle(u64::try_from(stream.as_raw_fd()).expect("the fd fits the handle space"))
}

#[cfg(not(unix))]
fn door_handle(_stream: &TcpStream) -> IoHandle {
    // the reactor door exists only where a reactor does; the parked
    // tests early-return before reaching here
    IoHandle(0)
}

/// A parked accept with the configured bound long past observes the
/// host's shutdown door and answers the shutdown trap — the drive cut
/// at the resume-shaped point, the engine deciding nothing. The old
/// bounded park would have answered the timeout span at the configured
/// bound instead. Needs a reactor driver and early-returns without
/// one.
#[test]
fn a_parked_accept_observes_the_shutdown_door_and_answers_the_trap() {
    if ds_vm::reactor::platform_default().is_none() {
        return; // the parked twin is the reactor's own
    }
    let (_port, lid, mut vm) = bind_until_free(|| {
        let mut vm = scheduled_vm(vec![bind_def(), accept_child_def()]);
        vm.set_net_timeout(Duration::from_millis(200));
        vm
    });
    let (mut dialer, accepted) = door_pair();
    vm.watch_shutdown(door_handle(&accepted))
        .expect("the door registers");
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(400));
        dialer.write_all(b"s").expect("the door writes");
    });
    let started = std::time::Instant::now();
    let outcome = vm.run("shut", "accept_child", &[Value::I64(lid)]);
    let elapsed = started.elapsed();
    match outcome {
        Err(Trap::Shutdown) => {},
        other => panic!("the shutdown cut answers its trap, got {other:?}"),
    }
    assert!(
        vm.shutdown_observed(),
        "the observation is the host's to read"
    );
    assert!(
        elapsed >= Duration::from_millis(300),
        "the park held past its configured bound (200ms): {elapsed:?}"
    );
    writer.join().expect("the door's writer lands");
    drop(accepted);
}

/// A flagged nursery's unbounded accept park dies through the
/// executor's cut — no drive stands over the child while its joiner
/// parks — so the channel's honest deadlock stays reachable: the
/// stall guard answers, never the parked accept's silence. Needs a
/// reactor driver and early-returns without one.
#[test]
fn a_flagged_nurserys_unbounded_accept_dies_to_the_executor_cut() {
    if ds_vm::reactor::platform_default().is_none() {
        return; // the parked twin is the reactor's own
    }
    // the installed module: bind (0), block_on_chan (1), accept_child
    // (2), trap_child (3) — the block's pool words name the children's
    // own table indices
    let (_port, lid, mut vm) = bind_until_free(|| {
        let mut vm = scheduled_vm(vec![
            bind_def(),
            block_on_chan_def(2, 3),
            accept_child_def(),
            trap_child_def(),
        ]);
        vm.set_net_timeout(Duration::from_secs(90));
        vm
    });
    let started = std::time::Instant::now();
    let outcome = vm.run("shut", "block_on_chan", &[Value::I64(lid)]);
    let elapsed = started.elapsed();
    match outcome {
        Err(Trap::SchedulerDeadlock) => {},
        other => panic!(
            "the parked accept died to the flag and the channel's deadlock answers, got {other:?} \
             after {elapsed:?}"
        ),
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "the cut is immediate, the honest deadlock the only wait: {elapsed:?}"
    );
}

/// The join sweep's cut of an unbounded accept child under a raised
/// flag — the parked-IO cancellation, the host-driven root-nursery shape: the boundary reports the
/// parked child `Cancelled` beside the failing `Trapped`, the long net bound
/// proving the cut (never a deadline) answered. Needs a reactor
/// driver and early-returns without one.
#[test]
fn a_flagged_root_nursery_cuts_its_unbounded_accept_child() {
    if ds_vm::reactor::platform_default().is_none() {
        return; // the parked twin is the reactor's own
    }
    let (_port, lid, mut vm) = bind_until_free(|| {
        let mut vm = scheduled_vm(vec![bind_def(), accept_child_def(), trap_child_def()]);
        vm.set_net_timeout(Duration::from_secs(90));
        vm
    });
    let nursery = vm.open_root_nursery();
    let parked = vm
        .spawn_fiber_in(nursery, "shut", "accept_child", &[Value::I64(lid)])
        .expect("the parked child spawns");
    let failing = vm
        .spawn_fiber_in(nursery, "shut", "trap_child", &[])
        .expect("the failing child spawns");
    assert_ne!(parked, failing);
    let started = std::time::Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the cut answers the long bound away: {:?}",
        started.elapsed()
    );
    match joined {
        JoinOutcome::Failed(outcomes) => {
            assert_eq!(outcomes.len(), 2);
            assert!(
                matches!(outcomes[0], ChildOutcome::Cancelled),
                "the unbounded accept died to the sentinel: {:?}",
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
}

/// The blocking twin's stop flag: a reactor-less VM keeps the bounded
/// blocking poll, and the host's flag — the non-Linux helper's door —
/// answers the shutdown trap between rounds, the observation recorded
/// for the host's own exit code. Runs on every platform: the flag is
/// the reactor-less shape whatever the host.
#[test]
fn the_blocking_twins_stop_flag_answers_the_shutdown_trap() {
    let (_port, lid, mut vm) = bind_until_free(|| {
        let mut vm = Vm::new(registry(vec![bind_def(), accept_child_def()]));
        vm.set_net_timeout(Duration::from_secs(90));
        vm
    });
    let stop = Arc::new(AtomicBool::new(false));
    vm.set_shutdown_flag(Arc::clone(&stop));
    let worker_stop = Arc::clone(&stop);
    let setter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        worker_stop.store(true, Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    let outcome = vm.run("shut", "accept_child", &[Value::I64(lid)]);
    let elapsed = started.elapsed();
    match outcome {
        Err(Trap::Shutdown) => {},
        other => panic!("the stopped blocking twin answers the trap, got {other:?}"),
    }
    assert!(
        vm.shutdown_observed(),
        "the observation is the host's to read"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the flag answers between poll rounds: {elapsed:?}"
    );
    setter.join().expect("the flag's setter lands");
}
