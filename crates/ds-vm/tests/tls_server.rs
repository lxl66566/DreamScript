//! The net face's server-half TLS tier end to end: `net_accept_tls` upgrading the accepted
//! connection over a host-pinned rustls server identity, the connection owning the full
//! `net_handle` face over the tls records — hermetic, loopback only,
//! the server certificate pinned through
//! [`Vm::set_tls_server_config`] and the client roots through
//! [`Vm::set_tls_config`]. Golden coverage: the blocking twin against
//! a local rustls client thread, the parked machine under an installed
//! reactor (a server fiber and a client fiber overlapping their
//! parked waits), the failed handshake's span (a plain peer closing
//! before it speaks tls), the unprovisioned refusal, and the
//! configured bound's expiry on a listener no peer dials. The
//! deterministic overlap proof is the parked drive's alone: one server
//! fiber serving two client fetches sequentially — a serialized drive
//! would block the server's first recv on a client that never runs.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
    time::Duration,
};

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::PLATFORM_IO_OK,
    platform::{
        OP_HTTPS_GET, OP_NET_ACCEPT_TLS, OP_NET_CLOSE, OP_NET_LISTEN, OP_NET_LISTENER_CLOSE,
        OP_NET_RECV, OP_NET_SEND, Platform,
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

/// The shared pool, copied per function: the listener trio, the accept
/// upgrade, the connection trio, then the recv cap immediate.
fn pool() -> Vec<Const> {
    vec![
        platform(OP_NET_LISTEN),         // k0
        platform(OP_NET_ACCEPT_TLS),     // k1
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
    // the tls capability's op rides the full std platform (a
    // tls-omitting host keeps it a reserved trap)
    reg.install_platform(Platform::std());
    reg.install(ModuleVersion::new("server", defs).expect("valid fixture module"));
    reg
}

fn string(vm: &mut Vm, text: &str) -> Value {
    vm.intern_str(text)
}

/// A port with nothing bound on it: bind, note, drop. The parallel
/// suite can steal it in the gap, so every test tolerates one
/// addr-in-use answer by picking a fresh port and re-running.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
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

/// A self-signed server certificate for `127.0.0.1` and the client
/// config pinning it as the sole trust anchor (the client-side
/// fixture pattern).
fn pinned_pair() -> (rustls::ServerConfig, Arc<rustls::ClientConfig>) {
    let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
        .expect("the self-signed certificate generates");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring serves the safe default protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![certified.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into()),
        )
        .expect("the server certificate loads");
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(certified.cert.der().clone())
        .expect("the anchor adds");
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring serves the safe default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    (server, Arc::new(client))
}

/// A VM over `defs` with the platform's reactor installed when the
/// platform carries a driver (the parking discipline; a driver-less
/// platform keeps the blocking twins — the embedder's choice, the
/// same fixtures pinning both ways). The net bound is the wake
/// family's runaway guard, not any test's semantics; the test that
/// measures the deadline narrows it itself.
fn scheduled_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    vm.set_net_timeout(Duration::from_secs(30));
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

/// `(host: string, port: int, response: string) -> word`: the golden
/// server round trip — listen, accept the upgraded connection, recv
/// the request, send `response`, close the connection and the
/// listener — answering the request on success, the failing call's
/// message on failure.
///
/// Slots: [host, port, response, lid, conn, t, request, tag, payload].
fn serve_tls() -> FunctionDef {
    def(
        "serve_tls",
        &[Ref, I64, Ref, I64, I64, Bool, Ref, Raw, Raw],
        3,
        2,
        &[
            Uop::Funcf { frame_slots: 9 },
            Uop::Mov { dst: 7, src: 0 },
            Uop::Mov { dst: 8, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 28,
            },
            Uop::Mov { dst: 3, src: 8 },
            Uop::Mov { dst: 7, src: 3 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 28,
            },
            Uop::Mov { dst: 4, src: 8 },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Kimm { dst: 8, kidx: k(6) },
            Uop::Call {
                callee: k(4),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 28,
            },
            Uop::Mov { dst: 6, src: 8 },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Mov { dst: 8, src: 2 },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 28,
            },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Call {
                callee: k(5),
                nargs: 1,
            },
            Uop::Mov { dst: 7, src: 3 },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Ret { src: 6 }, // the request on success
            Uop::Ret { src: 8 }, // 28: the failing call's message
        ],
    )
}

/// `(host: string, port: int, response: string) -> word`: two upgrades
/// over one listener — the overlap proof's server, the second connection
/// standing in the accept queue while the first is served — answering the
/// queued connection's request on success, the failing call's message on
/// failure.
///
/// Slots: [host, port, response, lid, conn, t, request, tag, payload].
fn serve_tls_two() -> FunctionDef {
    def(
        "serve_tls_two",
        &[Ref, I64, Ref, I64, I64, Bool, Ref, Raw, Raw],
        3,
        2,
        &[
            Uop::Funcf { frame_slots: 9 },
            Uop::Mov { dst: 7, src: 0 },
            Uop::Mov { dst: 8, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 45,
            },
            Uop::Mov { dst: 3, src: 8 },
            Uop::Mov { dst: 7, src: 3 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 45,
            },
            Uop::Mov { dst: 4, src: 8 },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Kimm { dst: 8, kidx: k(6) },
            Uop::Call {
                callee: k(4),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 45,
            },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Mov { dst: 8, src: 2 },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 45,
            },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Call {
                callee: k(5),
                nargs: 1,
            },
            Uop::Mov { dst: 7, src: 3 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 45,
            },
            Uop::Mov { dst: 4, src: 8 },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Kimm { dst: 8, kidx: k(6) },
            Uop::Call {
                callee: k(4),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 45,
            },
            Uop::Mov { dst: 6, src: 8 },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Mov { dst: 8, src: 2 },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            Uop::TagTestNot {
                dst: 5,
                src: 7,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 5,
                target: 45,
            },
            Uop::Mov { dst: 7, src: 4 },
            Uop::Call {
                callee: k(5),
                nargs: 1,
            },
            Uop::Mov { dst: 7, src: 3 },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Ret { src: 6 }, // the second round's request
            Uop::Ret { src: 8 }, // 45: the failing call's message
        ],
    )
}

/// `(host: string, port: int) -> word`: listen, then the upgraded
/// accept alone, answering the accept's payload word — the accepted
/// connection's handle on success, the failure message on the bound's
/// expiry.
///
/// Slots: [host, port, lid, t, overlay tag, overlay payload].
fn accept_tls_payload() -> FunctionDef {
    def(
        "accept_tls_payload",
        &[Ref, I64, I64, Bool, Raw, Raw],
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
            Uop::Ret { src: 5 }, // 10: the bind failure's message
        ],
    )
}

/// `(url: string) -> string`: the client fiber's https fetch — the
/// tls capability's own op, its answer the response body or the
/// net_err message.
fn fetch(name: &str) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(vec![Ref, Ref, Raw, Raw], 1, 2).expect("valid fixture layout"),
        pool: vec![platform(OP_HTTPS_GET)],
        code: assemble(&[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 1, src: 3 },
            Uop::Ret { src: 1 },
        ])
        .expect("fixture body encodes"),
    }
}

/// Runs `entry` over a fresh VM until the program's own answer is not
/// the bind's failure message, answering (the port, the answer, the
/// VM). A stolen port aborts the program before any socket the peer
/// could hold, so the retry is clean.
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

/// A local rustls client thread: connects after `delay` (the program's
/// accept parks or blocks first), speaks the tls handshake over the
/// pinned client roots, writes `request`, reads the response to the
/// server's close, and answers (the request it sent, the body it
/// read).
fn rustls_client(
    port: u16,
    delay: Duration,
    client: Arc<rustls::ClientConfig>,
    request: &'static [u8],
) -> thread::JoinHandle<(Vec<u8>, Vec<u8>)> {
    thread::spawn(move || {
        thread::sleep(delay);
        for _ in 0..200 {
            if let Ok(sock) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                let name = "127.0.0.1".try_into().expect("the server name parses");
                let conn =
                    rustls::ClientConnection::new(client, name).expect("the client session opens");
                let mut stream = rustls::StreamOwned::new(conn, sock);
                stream.write_all(request).expect("the request sends");
                stream.flush().expect("the request flushes");
                let mut body = Vec::new();
                let mut chunk = [0u8; 4096];
                // the server closes without close_notify (the plain
                // net_close), so the unclean EOF reads as the end the
                // tier's own close-tolerant discipline takes
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
                        Err(err) => panic!("the body reads: {err}"),
                    }
                }
                return (request.to_vec(), body);
            }
            thread::sleep(Duration::from_millis(25));
        }
        (Vec::new(), Vec::new())
    })
}

/// The blocking twin against a local rustls client: no reactor
/// installed, the accept and handshake blocking the thread to the
/// configured bound, the upgraded connection's recv and send riding
/// the tls records end to end.
#[test]
fn a_blocking_tls_server_serves_a_rustls_client() {
    for _ in 0..8 {
        let port = free_port();
        let (server, client) = pinned_pair();
        let peer = rustls_client(port, Duration::ZERO, client, b"GET / HTTP/1.0\r\n\r\n");
        let mut vm = Vm::new(registry(vec![serve_tls()]));
        vm.set_tls_server_config(Arc::new(server));
        vm.set_net_timeout(Duration::from_secs(30));
        let argv = [
            string(&mut vm, "127.0.0.1"),
            Value::I64(i64::from(port)),
            string(&mut vm, "HTTP/1.0 200 OK\r\n\r\nblocking body"),
        ];
        let answer = vm
            .run("server", "serve_tls", &argv)
            .expect("the fixture runs");
        let message = answer_string(&vm, &answer);
        if message.starts_with(&format!("net_listen 127.0.0.1:{port}")) {
            continue; // the parallel suite stole the port
        }
        assert!(
            message.starts_with("GET / HTTP/1.0") && message.ends_with("\r\n\r\n"),
            "the upgraded connection received the request: {message:?}"
        );
        assert_eq!(
            peer.join().expect("the client lands").1,
            b"HTTP/1.0 200 OK\r\n\r\nblocking body",
            "the served answer reached the tls client"
        );
        return;
    }
    panic!("no free loopback port answered the bind");
}

/// The parked tier end to end: a server fiber and a client fiber in
/// one VM over the reactor — the server's accept and the client's
/// exchange overlapping their parked waits, the client fiber reading
/// the served answer through its own https fetch. The parked machine
/// is the linux driver's own; off linux no reactor installs and the
/// drive could not park the server's accept beside the client's
/// exchange, so the blocking twin's coverage is the rustls-client
/// round above.
#[test]
#[cfg(target_os = "linux")]
fn a_parked_tls_server_answers_a_client_fiber() {
    for _ in 0..8 {
        let port = free_port();
        let (server, client) = pinned_pair();
        let mut vm = scheduled_vm(vec![serve_tls(), fetch("fetch")]);
        vm.set_tls_server_config(Arc::new(server));
        vm.set_tls_config(client);
        let url = string(&mut vm, &format!("https://127.0.0.1:{port}/parked"));
        let host = string(&mut vm, "127.0.0.1");
        let response = string(&mut vm, "HTTP/1.0 200 OK\r\n\r\nparked body");
        let nursery = vm.open_root_nursery();
        vm.spawn_fiber_in(nursery, "server", "serve_tls", &[
            host,
            Value::I64(i64::from(port)),
            response,
        ])
        .expect("the server fiber spawns");
        vm.spawn_fiber_in(nursery, "server", "fetch", &[url])
            .expect("the client fiber spawns");
        match vm.join_nursery(nursery).expect("the join lands") {
            ds_vm::JoinOutcome::Joined(answers) => {
                let child_string = |vm: &mut Vm, answer: &FrameAnswer| {
                    let [Value::Ref(id)] = answer.as_slice() else {
                        panic!("the child's answer is a string ref, got {answer:?}");
                    };
                    let id = *id;
                    vm.objects_of(id)
                        .str_at(id.id())
                        .expect("the answer names a string")
                        .to_owned()
                };
                let request = child_string(&mut vm, &answers[0].clone());
                if request.starts_with(&format!("net_listen 127.0.0.1:{port}")) {
                    continue; // the parallel suite stole the port
                }
                assert!(
                    request.starts_with("GET /parked HTTP/1.0") && request.ends_with("\r\n\r\n"),
                    "the upgraded connection received the request: {request:?}"
                );
                assert_eq!(
                    child_string(&mut vm, &answers[1].clone()),
                    "parked body",
                    "the client fiber read the served answer"
                );
            },
            other @ ds_vm::JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
        }
        return;
    }
    panic!("no free loopback port answered the bind");
}

/// The deterministic overlap proof: one server fiber serves two client
/// fibers sequentially — the second connection standing in the accept
/// queue while the server's first recv parks, so a serialized drive
/// would block the server's first recv on a client fetch that never
/// runs and stall at the runaway bound, while the overlapped parks
/// complete with no clock involved. The parked machines are the linux
/// driver's own.
#[test]
#[cfg(target_os = "linux")]
fn parked_tls_serving_overlaps_two_client_fetches() {
    for _ in 0..8 {
        let port = free_port();
        let (server, client) = pinned_pair();
        let mut vm = scheduled_vm(vec![serve_tls_two(), fetch("fetch_a"), fetch("fetch_b")]);
        vm.set_tls_server_config(Arc::new(server));
        vm.set_tls_config(client);
        let host = string(&mut vm, "127.0.0.1");
        let response = string(&mut vm, "HTTP/1.0 200 OK\r\n\r\nserved body");
        let url_a = string(&mut vm, &format!("https://127.0.0.1:{port}/alpha"));
        let url_b = string(&mut vm, &format!("https://127.0.0.1:{port}/beta"));
        let nursery = vm.open_root_nursery();
        vm.spawn_fiber_in(nursery, "server", "serve_tls_two", &[
            host,
            Value::I64(i64::from(port)),
            response,
        ])
        .expect("the server fiber spawns");
        vm.spawn_fiber_in(nursery, "server", "fetch_a", &[url_a])
            .expect("client a spawns");
        vm.spawn_fiber_in(nursery, "server", "fetch_b", &[url_b])
            .expect("client b spawns");
        match vm.join_nursery(nursery).expect("the join lands") {
            ds_vm::JoinOutcome::Joined(answers) => {
                let child_string = |vm: &mut Vm, answer: &FrameAnswer| {
                    let [Value::Ref(id)] = answer.as_slice() else {
                        panic!("the child's answer is a string ref, got {answer:?}");
                    };
                    let id = *id;
                    vm.objects_of(id)
                        .str_at(id.id())
                        .expect("the answer names a string")
                        .to_owned()
                };
                let second_request = child_string(&mut vm, &answers[0].clone());
                if second_request.starts_with(&format!("net_listen 127.0.0.1:{port}")) {
                    continue; // the parallel suite stole the port
                }
                assert!(
                    second_request.starts_with("GET /") && second_request.ends_with("\r\n\r\n"),
                    "the second round's request arrived over the upgraded connection: \
                     {second_request:?}"
                );
                assert_eq!(
                    child_string(&mut vm, &answers[1].clone()),
                    "served body",
                    "client a's answer"
                );
                assert_eq!(
                    child_string(&mut vm, &answers[2].clone()),
                    "served body",
                    "client b's answer"
                );
            },
            other @ ds_vm::JoinOutcome::Failed(_) => panic!("the join failed: {other:?}"),
        }
        return;
    }
    panic!("no free loopback port answered the bind");
}

/// The failed handshake's span: a plain peer connects and closes
/// without speaking tls, the upgraded accept answering the peer-close
/// refusal — the parked machine through its read wake (linux, the
/// reactor installed), the blocking twin through its bounded
/// handshake, the same span either way.
#[test]
fn a_failed_handshake_answers_the_peer_close_span() {
    for _ in 0..8 {
        let port = free_port();
        thread::spawn(move || {
            // the plain peer: connects late, closes at once — no tls
            // flight, so the handshake reads the TCP EOF
            thread::sleep(Duration::from_millis(150));
            for _ in 0..200 {
                if let Ok(sock) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                    drop(sock);
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let (server, _client) = pinned_pair();
        let mut vm = scheduled_vm(vec![accept_tls_payload()]);
        vm.set_tls_server_config(Arc::new(server));
        let argv = [string(&mut vm, "127.0.0.1"), Value::I64(i64::from(port))];
        let answer = vm
            .run("server", "accept_tls_payload", &argv)
            .expect("the fixture runs");
        let message = answer_string(&vm, &answer);
        if message.starts_with(&format!("net_listen 127.0.0.1:{port}")) {
            continue; // the parallel suite stole the port
        }
        assert!(
            message.starts_with("net_accept_tls 0: ")
                && message.contains("the peer closed before the tls handshake completed"),
            "the span names the op and the peer-close refusal: {message:?}"
        );
        return;
    }
    panic!("no free loopback port answered the bind");
}

/// The unprovisioned refusal: no server identity pinned, the upgrade
/// answering its own net_err before any socket opens — the raw seam's
/// span (the ambient's total spelling halting on the net error line,
/// the CLI round's pin).
#[test]
fn an_unconfigured_host_refuses_the_upgraded_accept() {
    let (server, _client) = pinned_pair();
    let _ = server; // nothing pinned: the refusal is the whole point
    let mut vm = scheduled_vm(vec![accept_tls_payload()]);
    let argv = [string(&mut vm, "127.0.0.1"), Value::I64(0)];
    let answer = vm
        .run("server", "accept_tls_payload", &argv)
        .expect("the refusal is a net_err, not a trap");
    assert_eq!(
        answer_string(&vm, &answer),
        "net_accept_tls 0: tls refused: the host installed no server config"
    );
}

/// The configured bound on a listener no peer dials: the upgraded
/// accept waits its bound and answers the timeout class — the parked
/// machine through the wheel, the blocking twin through the poll, the
/// same span either way.
#[test]
fn a_silent_listener_times_out_within_the_bound() {
    let started = std::time::Instant::now();
    let (server, _client) = pinned_pair();
    let make = || {
        let mut vm = scheduled_vm(vec![accept_tls_payload()]);
        vm.set_tls_server_config(Arc::new(server.clone()));
        vm.set_net_timeout(Duration::from_millis(200));
        vm
    };
    let (_port, answer, vm) = run_until_bound(make, "accept_tls_payload", |port, vm| {
        vec![string(vm, "127.0.0.1"), Value::I64(i64::from(port))]
    });
    let elapsed = started.elapsed();
    assert_eq!(
        answer_string(&vm, &answer),
        "net_accept_tls 0: timed out after 200ms"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the interpreter cannot hang on a silent listener: {elapsed:?}"
    );
}
