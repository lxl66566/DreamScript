//! The net face's server half: hand-assembled fixtures over the std
//! platform native module's listener surface (`net_listen`,
//! `net_accept`, `net_listener_close`). Golden coverage: the blocking
//! accept round trip against a local peer (listen, accept, send, recv,
//! the two closes — hermetic, no external network), the parked accept
//! under an installed reactor (the peer's late connection is the wake,
//! the accepted stream owning the connection face in full), the parked
//! accept holding past its old deadline for a late peer (the
//! 2026-11-24 signal ruling: the quiet accept parks unbounded, the
//! signal its backstop — a non-reactor host keeping the bounded poll
//! twin), the deadline bound on a listener no peer ever dials under
//! the blocking twin (std's listener carries no OS accept timeout,
//! so the poll draws the line the parked twin no longer does), the
//! idempotent listener close with a later accept on the
//! closed handle answering the unknown-listener floor, and the
//! addr_in_use class of a bind against a port another socket holds.
//!
//! Layout conventions follow the net-IO fixtures: every fallible
//! native's result is the two-word tag+payload span on the caller's
//! overlay slots, success reusing `PLATFORM_IO_OK`; the fixtures answer
//! the payload word (the message `Ref` on failure), or the tag word
//! where the test pins the class itself.

use std::{
    io::{Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
};

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{PLATFORM_IO_OK, PLATFORM_NET_ADDR_IN_USE},
    platform::{
        Capability, OP_NET_ACCEPT, OP_NET_CLOSE, OP_NET_LISTEN, OP_NET_LISTENER_CLOSE, OP_NET_RECV,
        OP_NET_SEND, Platform,
    },
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

/// The shared pool, copied per function: the listener trio, the
/// connection trio, then the recv cap immediate.
fn pool() -> Vec<Const> {
    vec![
        platform(OP_NET_LISTEN),         // k0
        platform(OP_NET_ACCEPT),         // k1
        platform(OP_NET_LISTENER_CLOSE), // k2
        platform(OP_NET_SEND),           // k3
        platform(OP_NET_RECV),           // k4
        platform(OP_NET_CLOSE),          // k5
        Const::I64(4096),                // k6: net_recv max_bytes
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

fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[Capability::Net]));
    reg.install(ModuleVersion::new("server", defs).expect("valid fixture module"));
    reg
}

/// A VM over `defs` with the platform's reactor installed when the
/// platform carries a driver (the parking discipline; a driver-less
/// platform keeps the blocking natives — the embedder's choice, the
/// same fixtures pinning both ways).
fn scheduled_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    // the wake family's runaway bound, not any test's semantics: a
    // full parallel suite starves peer threads past a few seconds
    vm.set_net_timeout(Duration::from_secs(30));
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

/// `(host: string, port: int, data: string) -> word`: the golden server
/// round trip — listen, accept, send `data`, recv the reply, close the
/// connection and the listener — answering the reply on success, the
/// failing call's message on failure.
///
/// Slots: [host, port, data, lid, conn, t, overlay tag, overlay payload].
fn serve() -> FunctionDef {
    def(
        "serve",
        &[Ref, I64, Ref, I64, I64, SlotType::Bool, Raw, Raw],
        3,
        2,
        &[
            Uop::Funcf { frame_slots: 8 },
            Uop::Mov { dst: 6, src: 0 },
            Uop::Mov { dst: 7, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 6,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 27,
            },
            Uop::Mov { dst: 3, src: 7 },
            Uop::Mov { dst: 6, src: 3 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 6,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 27,
            },
            Uop::Mov { dst: 4, src: 7 },
            Uop::Mov { dst: 6, src: 4 },
            Uop::Mov { dst: 7, src: 2 },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 6,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 27,
            },
            Uop::Mov { dst: 6, src: 4 },
            Uop::Kimm { dst: 7, kidx: k(6) },
            Uop::Call {
                callee: k(4),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 6,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 27,
            },
            Uop::Mov { dst: 6, src: 4 },
            Uop::Call {
                callee: k(5),
                nargs: 1,
            },
            Uop::Mov { dst: 6, src: 3 },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Ret { src: 7 }, // the reply on success
            Uop::Ret { src: 7 }, // the failing call's message payload
        ],
    )
}

/// `(host: string, port: int) -> word`: listen then accept alone,
/// answering the accept's payload word — the accepted connection's
/// handle on success, the failure message on the bound's expiry.
///
/// Slots: [host, port, lid, t, overlay tag, overlay payload].
fn accept_payload() -> FunctionDef {
    def(
        "accept_payload",
        &[Ref, I64, I64, SlotType::Bool, Raw, Raw],
        2,
        2,
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Mov { dst: 5, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 3,
                src: 4,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 3,
                target: 10,
            },
            Uop::Mov { dst: 2, src: 5 },
            Uop::Mov { dst: 4, src: 2 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Ret { src: 5 },
            Uop::Ret { src: 5 }, // the bind failure's message payload
        ],
    )
}

/// `(host: string, port: int) -> word`: listen, close the listener
/// twice (the idempotent seam), then accept on the closed handle —
/// the unknown-listener floor's message.
fn closed_accept() -> FunctionDef {
    def(
        "closed_accept",
        &[Ref, I64, I64, SlotType::Bool, Raw, Raw],
        2,
        2,
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Mov { dst: 5, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 3,
                src: 4,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 3,
                target: 14,
            },
            Uop::Mov { dst: 2, src: 5 },
            Uop::Mov { dst: 4, src: 2 },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Mov { dst: 4, src: 2 },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Mov { dst: 4, src: 2 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Ret { src: 5 },
            Uop::Ret { src: 5 }, // the bind failure's message payload
        ],
    )
}

/// `(host: string, port: int) -> word`: the bind's tag word alone —
/// the class the in-use port pins.
fn bind_tag() -> FunctionDef {
    def("bind_tag", &[Ref, I64, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::Ret { src: 2 }, // the tag word
    ])
}

fn server_registry() -> ModuleRegistry {
    registry(vec![serve(), accept_payload(), closed_accept(), bind_tag()])
}

fn string(vm: &mut Vm, text: &str) -> Value {
    vm.intern_str(text)
}

/// A port with nothing bound on it: bind, note, drop. The parallel
/// suite can steal it in the gap, so every test tolerates one
/// addr-in-use answer by picking a fresh port and re-running.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    listener.local_addr().expect("bound address").port()
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

/// Runs `entry` over `make`'s fresh VMs with a fresh free port until
/// the program's own answer is not the bind's failure message,
/// answering (the port, the run's answer, the VM). A stolen port
/// aborts the program before any socket the peer could hold, so the
/// retry is clean.
fn run_until_bound(
    make: impl Fn() -> Vm,
    entry: &'static str,
    args: impl Fn(u16, &mut Vm) -> Vec<Value>,
) -> (u16, FrameAnswer, Vm) {
    for _ in 0..8 {
        let port = free_port();
        let mut vm = make();
        let call_args = args(port, &mut vm);
        let answer = vm
            .run("server", entry, &call_args)
            .expect("the fixture runs");
        if answer_string(&vm, &answer).starts_with(&format!("net_listen 127.0.0.1:{port}")) {
            continue;
        }
        return (port, answer, vm);
    }
    panic!("no free loopback port answered the bind");
}

/// Serves the reply `pong` back — reading `expected` bytes exactly
/// first, then closing — dialing after `delay` so the program's accept
/// parks (or blocks) before the connection stands. The reply distinct
/// from the request, so the recv answer proves the peer's write
/// arrived (never a leftover of the program's own send).
fn late_pong_peer(port: u16, delay: Duration, expected: &[u8]) -> thread::JoinHandle<Vec<u8>> {
    let expected = expected.to_vec();
    thread::spawn(move || {
        thread::sleep(delay);
        for _ in 0..200 {
            if let Ok(mut sock) = TcpStream::connect(("127.0.0.1", port)) {
                let mut buf = vec![0; expected.len()];
                sock.read_exact(&mut buf).expect("read the served bytes");
                sock.write_all(b"pong").expect("write the reply");
                return buf;
            }
            thread::sleep(Duration::from_millis(25));
        }
        Vec::new()
    })
}

/// The blocking round trip: no reactor installed, the accept parks
/// nothing — the thread polls the non-blocking listener to the
/// peer's connection, and the accepted stream owns the connection
/// face (the send/recv over it landing the same answers a connecting
/// one gives).
#[test]
fn a_blocking_accept_round_trips() {
    for _ in 0..8 {
        let port = free_port();
        let peer = late_pong_peer(port, Duration::ZERO, b"served");
        let mut vm = Vm::new(server_registry());
        // the wake family's runaway bound: a full parallel suite can
        // starve the peer thread past the default bound
        vm.set_net_timeout(Duration::from_secs(30));
        let argv = [
            string(&mut vm, "127.0.0.1"),
            Value::I64(i64::from(port)),
            string(&mut vm, "served"),
        ];
        let answer = vm.run("server", "serve", &argv).expect("the fixture runs");
        let message = answer_string(&vm, &answer);
        if message.starts_with(&format!("net_listen 127.0.0.1:{port}")) {
            continue; // the parallel suite stole the port
        }
        assert_eq!(message, "pong", "the accepted connection echo round trips");
        assert_eq!(peer.join().expect("peer lands"), b"served");
        return;
    }
    panic!("no free loopback port answered the bind");
}

/// The parked accept under the reactor: the peer's late connection is
/// the wake, the drive never blocking — the answer runs through the
/// reactor's readiness, a lost park leaving the wait to the 30s
/// bound and the test to its failure line.
#[test]
fn a_scheduled_accept_wakes_on_the_peers_connection() {
    for _ in 0..8 {
        let port = free_port();
        let peer = late_pong_peer(port, Duration::from_millis(200), b"served");
        let mut vm = scheduled_vm(vec![serve()]);
        let argv = [
            string(&mut vm, "127.0.0.1"),
            Value::I64(i64::from(port)),
            string(&mut vm, "served"),
        ];
        let answer = vm.run("server", "serve", &argv).expect("the fixture runs");
        let message = answer_string(&vm, &answer);
        if message.starts_with(&format!("net_listen 127.0.0.1:{port}")) {
            continue; // the parallel suite stole the port
        }
        assert_eq!(message, "pong", "the parked accept wakes and serves");
        assert_eq!(peer.join().expect("peer lands"), b"served");
        return;
    }
    panic!("no free loopback port answered the bind");
}

/// The parked accept holds past its old deadline for a late peer
/// (the 2026-11-24 signal ruling): the quiet accept parks with no
/// wheel entry — the signal its backstop — so a peer dialing past the
/// configured bound still wakes it, while the reactor-less twin keeps
/// the bounded poll below. Needs a reactor driver; a host without one
/// keeps the bounded blocking twin and early-returns.
#[test]
fn a_scheduled_accept_holds_past_its_deadline_for_a_late_peer() {
    if ds_vm::reactor::platform_default().is_none() {
        return; // the parked twin is the reactor's own; the twin below pins the bound
    }
    for _ in 0..8 {
        let port = free_port();
        // the peer dials 3x the configured bound late: a bounded park
        // would have answered the timeout span long before it
        let peer = late_pong_peer(port, Duration::from_millis(600), b"served");
        let mut vm = scheduled_vm(vec![serve()]);
        vm.set_net_timeout(Duration::from_millis(200));
        let argv = [
            string(&mut vm, "127.0.0.1"),
            Value::I64(i64::from(port)),
            string(&mut vm, "served"),
        ];
        let answer = vm.run("server", "serve", &argv).expect("the fixture runs");
        let message = answer_string(&vm, &answer);
        if message.starts_with(&format!("net_listen 127.0.0.1:{port}")) {
            continue; // the parallel suite stole the port
        }
        assert_eq!(message, "pong", "the parked accept held past its bound");
        assert_eq!(peer.join().expect("peer lands"), b"served");
        return;
    }
    panic!("no free loopback port answered the bind");
}

/// The deadline bound on a listener no peer dials under the blocking
/// twin: the accept waits its configured bound and answers the timeout
/// class through the poll — std's listener carries no OS accept
/// timeout, and without a reactor the configured bound stays the
/// line the blocking twin draws (a reactor-less or non-Linux embedder
/// keeps exactly this twin).
#[test]
fn a_silent_listener_times_out_within_the_bound_under_the_blocking_twin() {
    let started = std::time::Instant::now();
    let make = || {
        let mut vm = Vm::new(server_registry());
        vm.set_net_timeout(Duration::from_millis(200));
        vm
    };
    let (_port, answer, vm) = run_until_bound(make, "accept_payload", |port, vm| {
        vec![string(vm, "127.0.0.1"), Value::I64(i64::from(port))]
    });
    let elapsed = started.elapsed();
    assert_eq!(
        answer_string(&vm, &answer),
        "net_accept 0: timed out after 200ms"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the interpreter cannot hang on a silent listener: {elapsed:?}"
    );
}

/// The idempotent close: two closes never trap, and the accept on the
/// closed handle answers the unknown-listener floor.
#[test]
fn a_closed_listener_answers_the_unknown_listener_floor() {
    let make = || Vm::new(server_registry());
    let (_port, answer, vm) = run_until_bound(make, "closed_accept", |port, vm| {
        vec![string(vm, "127.0.0.1"), Value::I64(i64::from(port))]
    });
    assert!(
        answer_string(&vm, &answer).starts_with("net_accept 0: unknown listener 0"),
        "the closed handle names itself"
    );
}

/// The bind against a port another socket holds answers its own class.
#[test]
fn an_in_use_port_answers_the_addr_in_use_class() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    let port = listener.local_addr().expect("bound address").port();
    let mut vm = Vm::new(server_registry());
    let argv = [string(&mut vm, "127.0.0.1"), Value::I64(i64::from(port))];
    let answer = vm
        .run("server", "bind_tag", &argv)
        .expect("the bind refusal is a span, never a trap");
    assert_eq!(
        answer.as_slice(),
        [Value::Tag(PLATFORM_NET_ADDR_IN_USE)],
        "the held port names its own class"
    );
}
