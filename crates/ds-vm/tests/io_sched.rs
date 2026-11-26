//! The IO scheduler e2e (the 2026-11-21 scheduler rulings): hand-
//! assembled programs over the std platform's net face, a VM with a
//! reactor installed, and local listeners — hermetic, no external
//! network. What the fixtures pin:
//!
//! - park-on-world-op: a `net_recv` with no data parks the calling fiber (the drive never blocks),
//!   the peer's write wakes it through the reactor, and the completed op's answer lands at the
//!   parked call site — the same answer the blocking twin answers.
//! - the drive runs the next ready fiber: two nursery children both parked on their own connections
//!   complete under one join, their waits overlapping (the serialized blocking twin would take
//!   twice the wait), the answers collected in spawn order.
//! - the deadline: a parked recv with no data wakes at its configured bound answering the timeout
//!   error span — the bounded-wait discipline restated over the wheel.
//! - parked-IO cancellation: a sibling's trap raises the flag and the drive's next sweep cuts the
//!   world-parked child at the resume-shaped point — its reactor interest deregistered, the
//!   boundary reporting `Cancelled` beside the failing `Trapped`.
//! - the close wake (item 79): a connection closed while a recv parks on it wakes the waiter now —
//!   the close deregisters the interest and delivers the closed handle's own error span at the
//!   parked call site, the deadline never the line.
//! - tail-position parks: a helper whose tail call is the recv parks through the frame pop and
//!   lands its answer on the caller, and the entry frame's own tail park delivers the completed
//!   answer as the run's own result.
//! - a full send buffer parks too: the writer drains what fits, parks on the remainder, and the
//!   peer's draining wakes it to completion.
//! - the eager start: a spawned-but-unentered nursery child runs while its joiner parks on a world
//!   op — the in-language nursery block's child feeds the parked recv's wake, its own send the
//!   signal the joiner's peer writes behind, the sockets ordering the handshake with no clock.
//! - the parked DNS twin (item 79, the 2026-11-24 ruling): a resolving connect parks its fiber on
//!   the resolver helper's posted eventfd — a sibling runs across the resolution, socket-ordered
//!   behind the resolving child's own transport — the failed lookup answers its own net_err, the
//!   deadline the same bound every parked wait keeps, and a resolving fetch rides the same staged
//!   machine. The shapes early-return on a host with no reactor driver.
//! - no reactor installed: the identical program keeps the blocking semantics (the embedder's
//!   platform choice, never a language fact).
//! - the REPL drive's parked submission (the host executor seam): a spliced session main whose one
//!   statement is a recv parks the drive (`WorldParked`, never a block), the host's pump between
//!   its own waits answers the peer's late write exactly `ds run`'s executor would, and the landing
//!   parks at the splice with the answer staged — while a pump ahead of the wake idles with the
//!   wait standing.

use std::{
    io::{Read, Write},
    net::{Ipv6Addr, SocketAddr, TcpListener},
    thread,
    time::{Duration, Instant},
};

use ds_mod::{Constant, FuncRefKey, Function, Module, TargetClass};
use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, ReplOutcome,
    Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{BUILTIN_NURSERY_JOIN, BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN},
    platform::{
        Capability, OP_HTTP_GET, OP_NET_CLOSE, OP_NET_CONNECT, OP_NET_RECV, OP_NET_SEND, Platform,
        STD_NAME,
    },
    splice_park,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn platform(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: STD_NAME.into(),
        func: index,
    })
}

fn builtin_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

fn nb_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "io".into(),
        func: index,
    })
}

/// The shared pool: the four handle natives, then the recv cap.
fn pool() -> Vec<Const> {
    vec![
        platform(OP_NET_CONNECT), // k0
        platform(OP_NET_SEND),    // k1
        platform(OP_NET_RECV),    // k2
        platform(OP_NET_CLOSE),   // k3
        Const::I64(4096),         // k4: net_recv max_bytes
    ]
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    def_pool(name, slots, params, args, pool(), body)
}

fn def_pool(
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

/// A local listener answering its address and the accepted peer's writer
/// half: the test's transport.
fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let addr = listener.local_addr().expect("local address");
    (listener, addr)
}

/// One accepted peer (the writer side the fixture's connection talks to).
fn accept(listener: &TcpListener) -> std::net::TcpStream {
    listener.accept().expect("acceptor accepted").0
}

fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[
        Capability::Net,
        Capability::Console,
    ]));
    reg.install(ModuleVersion::new("io", defs).expect("valid fixture module"));
    reg
}

/// A VM over `defs` with the platform's reactor installed (the parking
/// discipline) and a captured sink.
fn scheduled_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    vm.capture_output();
    // the wake family's runaway bound, not any test's semantics: a
    // machine carrying several parallel compile lanes starves writer
    // threads past the tens of seconds, and a deadline that fires there
    // answers the timeout sentinel instead of the written bytes — kept
    // under nextest's slow-terminate line so the assertion survives
    vm.set_net_timeout(Duration::from_secs(90));
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

/// `(host: string, port: int) -> int`: connect and answer the handle —
/// the fixture's Rust-side staging step.
fn connect_def() -> FunctionDef {
    def("connect", &[Ref, I64, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `(conn: int) -> string`: one recv, the payload (or the failure span's
/// message word) answered — the parking probe.
fn recv_child_def() -> FunctionDef {
    def("recv_child", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(4) },
        Uop::Call {
            callee: k(2),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(conn: int) -> string`: the entry frame's own tail call is the recv
/// — the entry-frame park (the final-delivery sentinel).
fn recv_tail_entry_def() -> FunctionDef {
    def("recv_tail_entry", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(4) },
        Uop::Callt {
            callee: k(2),
            nargs: 2,
        },
    ])
}

/// `(conn: int) -> string`: call `recv_tail_entry` plainly, answer its
/// word — the parked helper's answer landing on this caller.
fn recv_tail_caller_def() -> FunctionDef {
    def_pool(
        "recv_tail_caller",
        &[I64, Raw, Raw, Raw],
        1,
        2,
        vec![nb_ref(2)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(conn: int, data: string) -> word`: one send of `data` on the
/// staged handle — the send-park probe (the payload is 1 MiB, far past
/// any socket buffer, with a peer that drains late).
fn send_probe_def() -> FunctionDef {
    def("send_probe", &[I64, Ref, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(conn: int) -> int`: one close of the staged handle — the total
/// seam, its unit word answered.
fn close_conn_def() -> FunctionDef {
    def("close_conn", &[I64, Raw, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(3),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(url: string) -> string`: one plain `http_get` — the parked
/// exchange probe (the staged connect, the request drain, the response
/// reads to the close).
fn fetch_child_def() -> FunctionDef {
    def_pool(
        "fetch_child",
        &[Ref, Raw, Raw],
        1,
        2,
        vec![platform(OP_HTTP_GET)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )
}

/// One scheduled connect over a fresh accepted peer, answering the
/// handle and the peer.
fn connect(vm: &mut Vm, listener: &TcpListener, addr: &SocketAddr) -> (i64, std::net::TcpStream) {
    // the listener's backlog holds the connection: the VM connects
    // first, the peer accepts after (an accept before the connect
    // would deadlock the one thread that drives the VM)
    let host = vm.intern_str(&addr.ip().to_string());
    let port = i64::from(addr.port());
    let answer = vm
        .run("io", "connect", &[host, Value::I64(port)])
        .expect("connect lands");
    let conn = match answer.as_slice() {
        [Value::I64(conn)] => *conn,
        other => panic!("connect answered no handle: {other:?}"),
    };
    (conn, accept(listener))
}

/// Whether this host parks (its platform ships a reactor driver —
/// the parked DNS twin is the linux driver's own, so its shapes
/// early-return on a host without one, the suite's skip pattern).
fn parks() -> bool {
    ds_vm::reactor::platform_default().is_some()
}

/// A dual-stack loopback listener: "localhost" resolves to either
/// loopback family in the resolver's own order, and both reach the
/// one bound socket, so whichever address the staged connect tries
/// first connects.
fn dualstack_listener() -> (TcpListener, SocketAddr) {
    let listener =
        TcpListener::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))).expect("dual-stack bind");
    let addr = listener.local_addr().expect("local address");
    (listener, addr)
}

/// `(host: string, port: int) -> word`: resolve the host name,
/// connect over the resolution, and send the go bytes — the parked
/// resolve's own probe (the staged machine's step 0 through its
/// send), answering the send's tag word.
fn resolving_child_def() -> FunctionDef {
    def_pool(
        "resolving_child",
        &[Ref, I64, Raw, Raw, Raw, Raw],
        2,
        2,
        vec![
            platform(OP_NET_CONNECT), // k0
            platform(OP_NET_SEND),    // k1
            Const::Str("a".into()),   // k2
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Mov { dst: 5, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            // the connect's span overlays the arg area (the tag at 4,
            // the handle at 5); the send stages the handle beside its
            // data
            Uop::Mov { dst: 4, src: 5 },
            Uop::Kstr { dst: 5, kidx: k(2) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Ret { src: 4 },
        ],
    )
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

/// A parked recv wakes on the peer's write: the answer is the written
/// bytes, delivered at the parked call site — the blocking twin's own
/// answer, through the reactor.
#[test]
fn a_parked_recv_wakes_on_the_peers_write() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), recv_child_def()]);
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        let mut peer = peer;
        peer.write_all(b"late data").expect("peer writes");
        peer.flush().expect("peer flushes");
        peer
    });
    let answer = vm
        .run("io", "recv_child", &[Value::I64(conn)])
        .expect("the parked recv lands");
    assert_eq!(answer_string(&vm, &answer), "late data");
    let peer = writer.join().expect("writer lands");
    drop(peer);
}

/// The entry frame's own tail park: the entry function's tail call is
/// the recv, the run parks with the final-delivery sentinel, and the
/// completed answer surfaces as the run's own result.
#[test]
fn the_entry_frames_tail_park_delivers_the_runs_answer() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), recv_tail_entry_def()]);
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        let mut peer = peer;
        peer.write_all(b"entry tail").expect("peer writes");
        peer
    });
    let answer = vm
        .run("io", "recv_tail_entry", &[Value::I64(conn)])
        .expect("the entry tail park lands");
    // the tail native's root answer is its whole span (tag + payload),
    // the blocking twin's own shape
    let words = answer.as_slice();
    assert!(
        matches!(
            words.first(),
            Some(Value::Tag(ds_vm::native::PLATFORM_IO_OK))
        ),
        "the entry tail answered {words:?}"
    );
    let payload = FrameAnswer::one(words[1]);
    assert_eq!(answer_string(&vm, &payload), "entry tail");
    drop(writer.join().expect("writer lands"));
}

/// A helper's tail park pops its frame and lands the answer on the
/// caller's overlay — the parked twin of the native tail return.
#[test]
fn a_helper_tail_park_lands_on_its_caller() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![
        connect_def(),
        recv_child_def(),
        recv_tail_entry_def(),
        recv_tail_caller_def(),
    ]);
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        let mut peer = peer;
        peer.write_all(b"helper tail").expect("peer writes");
        peer
    });
    let answer = vm
        .run("io", "recv_tail_caller", &[Value::I64(conn)])
        .expect("the helper tail park lands");
    assert_eq!(answer_string(&vm, &answer), "helper tail");
    drop(writer.join().expect("writer lands"));
}

/// Two nursery children both parked on their own connections complete
/// under one join with their waits overlapped — the serialized blocking
/// twin needs both waits back to back, the scheduler needs the longer
/// one alone. The answers collect in spawn order.
#[test]
fn parked_siblings_overlap_their_waits_and_join_in_spawn_order() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), recv_child_def()]);
    let (conn_a, peer_a) = connect(&mut vm, &listener, &addr);
    let (conn_b, peer_b) = connect(&mut vm, &listener, &addr);
    let wait = Duration::from_millis(250);
    let writer = |peer: std::net::TcpStream, tag: &'static str| {
        thread::spawn(move || {
            thread::sleep(wait);
            let mut peer = peer;
            peer.write_all(tag.as_bytes()).expect("peer writes");
            peer
        })
    };
    let writer_a = writer(peer_a, "alpha");
    let writer_b = writer(peer_b, "beta");
    let nursery = vm.open_root_nursery();
    let a = vm
        .spawn_fiber_in(nursery, "io", "recv_child", &[Value::I64(conn_a)])
        .expect("child a spawns");
    let b = vm
        .spawn_fiber_in(nursery, "io", "recv_child", &[Value::I64(conn_b)])
        .expect("child b spawns");
    assert!(a != b, "two children, two slots");
    let started = Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    let elapsed = started.elapsed();
    match joined {
        ds_vm::JoinOutcome::Joined(answers) => {
            assert_eq!(answers.len(), 2, "both children landed");
            let alpha = answers[0].clone();
            let beta = answers[1].clone();
            assert_eq!(answer_string(&vm, &alpha), "alpha", "spawn order");
            assert_eq!(answer_string(&vm, &beta), "beta", "spawn order");
        },
        other @ ds_vm::JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
    }
    // the serialized twin needs both sleeps back to back (a 2*wait
    // floor), while a parallel suite can overshoot a writer thread's
    // sleep in wall time — the bound tolerates one wait of overshoot
    assert!(
        elapsed < 3 * wait,
        "the waits overlapped ({elapsed:?} vs {:?} serialized)",
        2 * wait
    );
    drop(writer_a.join().expect("writer a lands"));
    drop(writer_b.join().expect("writer b lands"));
}

/// A parked recv with no data wakes at its configured bound answering
/// the timeout error span — the bounded-wait discipline over the wheel.
#[test]
fn a_parked_recv_wakes_at_its_deadline_with_the_timeout_span() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), recv_child_def()]);
    vm.set_net_timeout(Duration::from_millis(150));
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    let answer = vm
        .run("io", "recv_child", &[Value::I64(conn)])
        .expect("the deadline wake lands");
    // the fixture answers the span's payload word; the timeout's message
    // names the wait it drew
    let text = answer_string(&vm, &answer);
    assert!(text.contains("timed out"), "the timeout message: {text}");
    drop(peer);
}

/// A connection closed while a recv parks on it wakes the waiter now:
/// the close's wake door deregisters the interest and delivers the
/// closed handle's own span at the parked call site — the
/// unknown-connection net floor, never the deadline's timeout span
/// (the bound set far past the close, so only the wake can end the
/// park). The parked twin is the reactor's own; a driver-less host
/// keeps the blocking recv, whose serialized drive the suite leaves
/// to the blocking fixtures.
#[test]
fn a_conn_closed_under_a_parked_recv_wakes_before_its_deadline() {
    if !parks() {
        return;
    }
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), recv_child_def(), close_conn_def()]);
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    let nursery = vm.open_root_nursery();
    let parked = vm
        .spawn_fiber_in(nursery, "io", "recv_child", &[Value::I64(conn)])
        .expect("the parked child spawns");
    let closer = vm
        .spawn_fiber_in(nursery, "io", "close_conn", &[Value::I64(conn)])
        .expect("the closing child spawns");
    assert_ne!(parked, closer);
    let started = Instant::now();
    let joined = vm.join_nursery(nursery).expect("the join lands");
    let elapsed = started.elapsed();
    match joined {
        ds_vm::JoinOutcome::Joined(answers) => {
            assert_eq!(answers.len(), 2, "both children landed");
            let text = answer_string(&vm, &answers[0].clone());
            assert!(
                text.contains("net_recv") && text.contains("unknown connection"),
                "the close wake answered the closed handle's own span: {text}"
            );
        },
        other @ ds_vm::JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
    }
    // the configured bound is the 90s runaway floor, so a deadline
    // wake answers past 30s: only the close wake lands this fast
    assert!(
        elapsed < Duration::from_secs(30),
        "the wake ended the park before its bound ({elapsed:?})"
    );
    drop(peer);
}

/// A parked plain fetch wakes through its whole machine: the staged
/// connect, the request drain, and the response reads to the server's
/// close — the body after the header terminator delivered at the
/// parked call site, the same answer the blocking twin answers.
#[test]
fn a_parked_plain_fetch_answers_the_local_server() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let addr = listener.local_addr().expect("local address");
    // the local HTTP peer: one accept, the request read to its blank
    // line, then the close-delimited response (the peer's own exit the
    // close the machine reads)
    let server = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("the server accepts");
        sock.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("timeout");
        let mut seen = vec![0; 1024];
        let mut got = 0;
        while got < seen.len() {
            match sock.read(&mut seen[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    got += n;
                    if seen[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                },
            }
        }
        sock.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nhello parked fetch")
            .expect("the server writes");
    });
    let mut vm = scheduled_vm(vec![fetch_child_def()]);
    let url = vm.intern_str(&format!("http://127.0.0.1:{}/hello", addr.port()));
    let answer = vm
        .run("io", "fetch_child", &[url])
        .expect("the parked fetch lands");
    assert_eq!(answer_string(&vm, &answer), "hello parked fetch");
    server.join().expect("the server lands");
}

/// Parked-IO cancellation: a sibling's trap raises the flag, and the
/// drive's next sweep cuts the world-parked child at the
/// resume-shaped point — its interest deregistered, the boundary
/// reporting `Cancelled` beside the failing `Trapped`.
#[test]
fn a_cancelled_nursery_cuts_its_world_parked_child() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), recv_child_def(), trap_child_def()]);
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    // the peer never writes: the child parks with no wake but the cut
    let nursery = vm.open_root_nursery();
    let parked = vm
        .spawn_fiber_in(nursery, "io", "recv_child", &[Value::I64(conn)])
        .expect("the parked child spawns");
    let failing = vm
        .spawn_fiber_in(nursery, "io", "trap_child", &[])
        .expect("the failing child spawns");
    assert_ne!(parked, failing);
    let joined = vm.join_nursery(nursery).expect("the join lands");
    match joined {
        ds_vm::JoinOutcome::Failed(outcomes) => {
            assert_eq!(outcomes.len(), 2);
            assert!(
                matches!(outcomes[0], ds_vm::ChildOutcome::Cancelled),
                "the parked child cut: {:?}",
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
    drop(peer);
}

/// A full send buffer parks too: the writer drains what fits, parks on
/// the remainder, and the peer's late draining wakes it to completion —
/// 1 MiB across one send call.
#[test]
fn a_full_send_buffer_fills_and_drains_across_the_park() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), send_probe_def()]);
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    let total = 1024 * 1024;
    let reader = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        let mut peer = peer;
        let mut buf = [0; 16384];
        let mut got = 0;
        while got < total {
            let n = peer.read(&mut buf).expect("peer reads");
            assert!(n > 0, "the peer saw an orderly close early");
            got += n;
        }
        got
    });
    let data = "x".repeat(total);
    let data_arg = vm.intern_str(&data);
    let answer = vm
        .run("io", "send_probe", &[Value::I64(conn), data_arg])
        .expect("the parked send lands");
    // the send probe answers the span's tag word: the shared success
    // marker of every fallible native
    assert!(
        matches!(
            answer.as_slice().first(),
            Some(Value::Tag(ds_vm::native::PLATFORM_IO_OK))
        ),
        "the send answered {:?}",
        answer.as_slice()
    );
    assert_eq!(reader.join().expect("reader lands"), total);
    let _ = conn;
}

/// No reactor installed: the identical parking shape keeps the blocking
/// semantics and answers the same — the embedder's platform choice.
#[test]
fn without_a_reactor_the_same_program_blocks_and_lands() {
    let (listener, addr) = listener();
    let mut vm = Vm::new(registry(vec![connect_def(), recv_child_def()]));
    vm.capture_output();
    let (conn, peer) = connect(&mut vm, &listener, &addr);
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        let mut peer = peer;
        peer.write_all(b"blocking twin").expect("peer writes");
        peer
    });
    let answer = vm
        .run("io", "recv_child", &[Value::I64(conn)])
        .expect("the blocking recv lands");
    assert_eq!(answer_string(&vm, &answer), "blocking twin");
    drop(writer.join().expect("writer lands"));
}

/// The `()` trap child for the cancellation fixture: 1/0.
fn trap_child_def() -> FunctionDef {
    def_pool(
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

/// The eager-start fixtures' pool: the net seams beside the nursery
/// seams, the recv cap, the child's go bytes, and the sender's word.
fn eager_pool(sender: u32) -> Vec<Const> {
    vec![
        platform(OP_NET_CONNECT),           // k0
        platform(OP_NET_SEND),              // k1
        platform(OP_NET_RECV),              // k2
        platform(OP_NET_CLOSE),             // k3
        Const::I64(4096),                   // k4: net_recv max_bytes
        Const::Str("go".into()),            // k5: the child's signal
        builtin_ref(BUILTIN_NURSERY_OPEN),  // k6
        builtin_ref(BUILTIN_NURSERY_SPAWN), // k7
        builtin_ref(BUILTIN_NURSERY_JOIN),  // k8
        Const::FuncRef(ConstFuncRef::Named {
            module: "io".into(),
            func: sender,
        }), // k9: the sender's function word
    ]
}

/// `(conn: int) -> int`: one send of the go bytes on the child's own
/// connection — the signal the parked joiner's wake rides behind.
fn eager_sender_def() -> FunctionDef {
    def_pool(
        "eager_sender",
        &[I64, Ref, Raw, Raw],
        1,
        2,
        eager_pool(1),
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kstr { dst: 3, kidx: k(5) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Ret { src: 2 },
        ],
    )
}

/// `(conn_a: int, conn_b: int) -> string`: the in-language nursery block
/// — open, spawn the sender closure (conn_b its one capture), park on
/// conn_a's recv, then the debug join. Nothing feeds conn_a until the
/// spawned-but-unentered child runs, so the parked joiner wakes only
/// through the eager start.
fn eager_main_def() -> FunctionDef {
    def_pool(
        "eager_main",
        &[I64, I64, I64, SlotType::Fn, Ref, Ref, Raw, Raw],
        2,
        2,
        eager_pool(1),
        &[
            Uop::Funcf { frame_slots: 8 },
            // the open: the nursery id lands on overlay word 6
            Uop::Call {
                callee: k(6),
                nargs: 0,
            },
            Uop::Mov { dst: 2, src: 6 },
            // the sender closure over conn_b, then its spawn
            Uop::Kconst { dst: 3, kidx: k(9) },
            Uop::Mov { dst: 6, src: 1 },
            Uop::ClosureNew {
                dst: 4,
                func: 3,
                n: 1,
            },
            Uop::Mov { dst: 6, src: 2 },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Call {
                callee: k(7),
                nargs: 2,
            },
            // the joiner parks on conn_a's recv
            Uop::Mov { dst: 6, src: 0 },
            Uop::Kimm { dst: 7, kidx: k(4) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 5, src: 7 },
            // the scope-exit join: the failed-children count answers
            Uop::Mov { dst: 6, src: 2 },
            Uop::Call {
                callee: k(8),
                nargs: 1,
            },
            Uop::Ret { src: 5 },
        ],
    )
}

/// The eagerly started child feeds the parked joiner's world wake: the
/// joiner parks on a recv whose peer writes only behind the child's own
/// send, the sockets themselves ordering the handshake — no clock, no
/// external network, the 30s runaway bound alone covering a failure.
#[test]
fn an_eagerly_started_child_feeds_the_parked_joiners_wake() {
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def(), eager_sender_def(), eager_main_def()]);
    let (conn_a, peer_a) = connect(&mut vm, &listener, &addr);
    let (conn_b, peer_b) = connect(&mut vm, &listener, &addr);
    let feeder = thread::spawn(move || {
        let mut peer_a = peer_a;
        let mut peer_b = peer_b;
        // the child's go lands first, then the joiner's wake writes
        let mut go = [0; 2];
        peer_b.read_exact(&mut go).expect("the child's go lands");
        assert_eq!(&go, b"go", "the eagerly started child signalled");
        peer_a.write_all(b"late data").expect("the wake writes");
        peer_a.flush().expect("the wake flushes");
    });
    let answer = vm
        .run("io", "eager_main", &[
            Value::I64(conn_a),
            Value::I64(conn_b),
        ])
        .expect("the eagerly started child fed the parked recv");
    assert_eq!(answer_string(&vm, &answer), "late data");
    feeder.join().expect("feeder lands");
}

/// The parked DNS twin's overlap (item 79, ruled 2026-11-24): a
/// resolving connect parks the calling fiber — the drive never blocks
/// in getaddrinfo — and the sibling runs across the resolution, its
/// own recv parked until the resolving child's transport exists: the
/// feeder thread accepts the resolved connect, reads the go bytes,
/// and only then writes the sibling's wake. The sockets alone order
/// the handshake — no clock, no external network.
#[test]
fn a_resolving_connect_parks_while_its_sibling_runs() {
    if !parks() {
        return;
    }
    let (listener, addr) = dualstack_listener();
    let mut vm = scheduled_vm(vec![connect_def(), recv_child_def(), resolving_child_def()]);
    // the sibling's own transport, pre-connected and accepted so the
    // listener's backlog carries only the resolving child's connect
    let (conn_b, peer_b) = connect(&mut vm, &listener, &addr);
    let feeder = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("the resolved connect accepts");
        let mut go = [0; 1];
        sock.read_exact(&mut go)
            .expect("the resolving child's go lands");
        assert_eq!(&go, b"a", "the connect behind the resolution carried data");
        let mut peer_b = peer_b;
        peer_b
            .write_all(b"behind the resolve")
            .expect("the sibling's wake writes");
        peer_b.flush().expect("the wake flushes");
    });
    let host = vm.intern_str("localhost");
    let nursery = vm.open_root_nursery();
    let resolving = vm
        .spawn_fiber_in(nursery, "io", "resolving_child", &[
            host,
            Value::I64(i64::from(addr.port())),
        ])
        .expect("the resolving child spawns");
    let sibling = vm
        .spawn_fiber_in(nursery, "io", "recv_child", &[Value::I64(conn_b)])
        .expect("the sibling spawns");
    assert_ne!(resolving, sibling);
    let joined = vm.join_nursery(nursery).expect("the join lands");
    match joined {
        ds_vm::JoinOutcome::Joined(answers) => {
            assert_eq!(answers.len(), 2, "both children landed");
            assert!(
                matches!(
                    answers[0].as_slice().first(),
                    Some(Value::Tag(ds_vm::native::PLATFORM_IO_OK))
                ),
                "the resolving child sent over its resolved connection: {:?}",
                answers[0].as_slice()
            );
            let recv = answers[1].clone();
            assert_eq!(
                answer_string(&vm, &recv),
                "behind the resolve",
                "spawn order"
            );
        },
        other @ ds_vm::JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
    }
    feeder.join().expect("feeder lands");
}

/// A failed resolution answers its own net_err (NXDOMAIN, no
/// addresses): the parked resolve wakes through the helper's posted
/// failure, the span naming its own op and target — the same span
/// the blocking twin answers.
#[test]
fn a_failed_resolution_answers_its_own_net_err_span() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![connect_def()]);
    let host = vm.intern_str("example.invalid");
    let answer = vm
        .run("io", "connect", &[host, Value::I64(80)])
        .expect("the failed lookup lands");
    let text = answer_string(&vm, &answer);
    assert!(
        text.starts_with("net_connect example.invalid:80:"),
        "the span names its op and target: {text}"
    );
}

/// The parked resolve keeps the configured net-timeout bound: a
/// deadline already due fires from the wheel before any helper post
/// can land, answering the same timeout span the connect path
/// answers — the bound is semantic, no clock assertion.
#[test]
fn a_parked_resolve_wakes_at_its_deadline_with_the_timeout_span() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![connect_def()]);
    vm.set_net_timeout(Duration::ZERO);
    let host = vm.intern_str("localhost");
    let answer = vm
        .run("io", "connect", &[host, Value::I64(80)])
        .expect("the deadline wake lands");
    let text = answer_string(&vm, &answer);
    assert!(text.contains("timed out"), "the timeout message: {text}");
    assert!(
        text.contains("net_connect localhost:80"),
        "the span names its op and target: {text}"
    );
}

/// A resolving fetch rides the same staged machine's step 0: the
/// hostname in the URL resolves on the helper, the wake starts the
/// connect, and the exchange answers the local server's body — the
/// landed plain-fetch shape over a host name instead of a literal.
#[test]
fn a_resolving_fetch_answers_the_local_server() {
    if !parks() {
        return;
    }
    let (listener, addr) = dualstack_listener();
    // the local HTTP peer: one accept, the request read to its blank
    // line, then the close-delimited response (the peer's own exit
    // the close the machine reads)
    let server = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("the server accepts");
        sock.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("timeout");
        let mut seen = vec![0; 1024];
        let mut got = 0;
        while got < seen.len() {
            match sock.read(&mut seen[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    got += n;
                    if seen[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                },
            }
        }
        sock.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nhello resolving fetch")
            .expect("the server writes");
    });
    let mut vm = scheduled_vm(vec![fetch_child_def()]);
    let url = vm.intern_str(&format!("http://localhost:{}/hello", addr.port()));
    let answer = vm
        .run("io", "fetch_child", &[url])
        .expect("the resolving fetch lands");
    assert_eq!(answer_string(&vm, &answer), "hello resolving fetch");
    server.join().expect("the server lands");
}

/// The spliced session main: one statement — a recv on the staged
/// connection — with the park spliced before its `RET`, exactly the
/// shape the CLI's REPL session lowers. The splice widens nothing (the
/// one-word answer fits the call's own two-word area).
fn repl_session(conn: i64) -> Module {
    let main = Function::new(
        "main".to_owned(),
        FrameLayout::new(vec![Raw, Raw, Raw], 0, 2).expect("layout"),
        vec![
            Constant::I64(conn), // k0: the connection
            Constant::I64(4096), // k1: net_recv max bytes
            Constant::FuncRef {
                key: FuncRefKey::Native(STD_NAME.into()),
                func: OP_NET_RECV,
            }, // k2: the recv
        ],
        vec![
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm {
                dst: 1,
                kidx: ConstIdx(0),
            },
            Uop::Kimm {
                dst: 2,
                kidx: ConstIdx(1),
            },
            Uop::Call {
                callee: ConstIdx(2),
                nargs: 2,
            },
            Uop::Ret { src: 2 },
        ],
    )
    .expect("valid");
    let module = Module::new(
        "repl",
        TargetClass::Bytecode,
        ds_mod::Hash128::compute(b"repl"),
    );
    splice_park(module.with_function(main), "main", 4).expect("the splice lands")
}

/// The REPL drive's parked submission: the drive ends `WorldParked` (the
/// prompt keeps serving, the wait registered), a pump ahead of the wake
/// idles with the wait standing, and the peer's write wakes the parked
/// recv at the next pump — the landing parks at the splice with the
/// answer staged, the same word the blocking twin answers.
#[test]
fn a_repl_submission_parks_and_lands_at_the_host_pump() {
    if !parks() {
        return;
    }
    let (listener, addr) = listener();
    let mut vm = scheduled_vm(vec![connect_def()]);
    let (conn, mut peer) = connect(&mut vm, &listener, &addr);
    vm.install_dsmod(repl_session(conn))
        .expect("the session module installs");
    // the submission's drive: the recv parks (no peer write yet), the
    // drive never blocks, and the frame holds its partial run
    assert_eq!(
        vm.repl_start("repl", "main"),
        Ok(ReplOutcome::WorldParked),
        "the parked submission ends its drive in flight"
    );
    // a pump between the host's reads: nothing woke, the wait stands
    assert_eq!(
        vm.repl_continue(Some(Duration::ZERO)),
        Ok(ReplOutcome::WorldParked),
        "an idle pump leaves the wait standing"
    );
    peer.write_all(b"ping").expect("the peer writes");
    peer.flush().expect("the peer flushes");
    // the pump lands: the splice parks with the recv's answer staged
    let answer = match vm.repl_continue(Some(Duration::ZERO)) {
        Ok(ReplOutcome::Parked(answer)) => answer,
        other => panic!("the pump lands the submission: {other:?}"),
    };
    assert_eq!(answer_string(&vm, &answer), "ping");
    vm.repl_teardown();
}
