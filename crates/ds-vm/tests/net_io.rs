//! Net-IO natives: hand-assembled fixtures over the std platform native
//! module's TCP surface (`net_connect`, `net_send`, `net_recv`,
//! `net_close`, `http_get`). Golden coverage: a full connect/send/recv/
//! close round trip against a local listener (hermetic — no external
//! network), the orderly-close empty string, the bounded-timeout path
//! against a socket that never answers, refused ports, https and
//! bad-scheme rejection, the http_get response-size cap, handle
//! invalidation after `net_close`, and the drop discipline (a connection
//! left open closes when the VM drops).
//!
//! Layout conventions follow the file-IO fixtures: every net native's
//! result is the two-word tag+payload span on the caller's overlay slots
//! 0 and 1 — the failure tag is `PLATFORM_NET_ERR`, success reuses
//! `PLATFORM_IO_OK` — and the overlay slots declare `Raw`.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        PLATFORM_IO_OK, PLATFORM_NET_ERR, STD_HTTP_GET, STD_NET_CLOSE, STD_NET_CONNECT,
        STD_NET_RECV, STD_NET_SEND, STD_PLATFORM_MODULE, std_platform_module,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn platform(index: u32) -> Const {
    Const::FuncRef {
        module: STD_PLATFORM_MODULE.into(),
        func: index,
    }
}

/// The shared pool, copied per function: the five net natives, then the
/// recv cap immediate.
fn pool() -> Vec<Const> {
    vec![
        platform(STD_NET_CONNECT), // k0
        platform(STD_NET_SEND),    // k1
        platform(STD_NET_RECV),    // k2
        platform(STD_NET_CLOSE),   // k3
        platform(STD_HTTP_GET),    // k4
        Const::I64(4096),          // k5: net_recv max_bytes in `echo`
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
    // the whole surface here is the platform's: the std platform table is
    // the host's to install (`Vm::new` only adds the builtin core)
    reg.install_native(STD_PLATFORM_MODULE, std_platform_module());
    reg.install(ModuleVersion::new("net", defs).expect("valid fixture module"));
    reg
}

/// `(host: string, port: int, data: string) -> string`: the golden round
/// trip — connect, send, recv, close — halting on the `net_err` tag word
/// at any failure so the run result itself is the failure shape.
///
/// Slots: [host, port, data, conn, got, t, overlay tag, overlay payload].
fn echo() -> FunctionDef {
    def("echo", &[Ref, I64, Ref, I64, Ref, Bool, Raw, Raw], 3, 2, &[
        Uop::Funcf { frame_slots: 8 },
        Uop::Mov { dst: 6, src: 0 },
        Uop::Mov { dst: 7, src: 1 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::TagTest {
            dst: 5,
            src: 6,
            tag: PLATFORM_NET_ERR,
        },
        Uop::Jmpif {
            cond: 5,
            target: 23,
        },
        Uop::Mov { dst: 3, src: 7 },
        Uop::Mov { dst: 6, src: 3 },
        Uop::Mov { dst: 7, src: 2 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::TagTest {
            dst: 5,
            src: 6,
            tag: PLATFORM_NET_ERR,
        },
        Uop::Jmpif {
            cond: 5,
            target: 23,
        },
        Uop::Mov { dst: 6, src: 3 },
        Uop::Kimm { dst: 7, kidx: k(5) },
        Uop::Call {
            callee: k(2),
            nargs: 2,
        },
        Uop::TagTest {
            dst: 5,
            src: 6,
            tag: PLATFORM_NET_ERR,
        },
        Uop::Jmpif {
            cond: 5,
            target: 23,
        },
        Uop::Mov { dst: 4, src: 7 },
        Uop::Mov { dst: 6, src: 3 },
        Uop::Call {
            callee: k(3),
            nargs: 1,
        },
        Uop::TagTest {
            dst: 5,
            src: 6,
            tag: PLATFORM_NET_ERR,
        },
        Uop::Jmpif {
            cond: 5,
            target: 23,
        },
        Uop::Ret { src: 4 },
        Uop::Halt { slot: 6 },
    ])
}

/// `(url: string) -> string`: the http_get payload word either way — the
/// response body on success, the `net_err` message on failure.
fn fetch() -> FunctionDef {
    def("fetch", &[Ref, Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(4),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 3 },
        Uop::Ret { src: 1 },
    ])
}

/// `(host: string, port: int) -> word`: the connect payload — the handle
/// `I64` on success, the `net_err` message `Ref` on failure.
fn connect_payload() -> FunctionDef {
    def("connect_payload", &[Ref, I64, Raw, Raw], 2, 2, &[
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

/// `(host: string, port: int, max_bytes: int) -> string`: connect, recv,
/// close, returning the recv payload — the received text (empty on an
/// orderly close) or, on any failure, the failing call's message.
///
/// Slots: [host, port, max, conn, got, t, overlay tag, overlay payload].
fn recv_payload() -> FunctionDef {
    def(
        "recv_payload",
        &[Ref, I64, I64, I64, Ref, Bool, Raw, Raw],
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
            Uop::TagTest {
                dst: 5,
                src: 6,
                tag: PLATFORM_NET_ERR,
            },
            Uop::Jmpif {
                cond: 5,
                target: 18,
            },
            Uop::Mov { dst: 3, src: 7 },
            Uop::Mov { dst: 6, src: 3 },
            Uop::Mov { dst: 7, src: 2 },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 5,
                src: 6,
                tag: PLATFORM_NET_ERR,
            },
            Uop::Jmpif {
                cond: 5,
                target: 18,
            },
            Uop::Mov { dst: 4, src: 7 },
            Uop::Mov { dst: 6, src: 3 },
            Uop::Call {
                callee: k(3),
                nargs: 1,
            },
            Uop::TagTest {
                dst: 5,
                src: 6,
                tag: PLATFORM_NET_ERR,
            },
            Uop::Jmpif {
                cond: 5,
                target: 18,
            },
            Uop::Ret { src: 4 },
            Uop::Ret { src: 7 }, // the failing call's message payload
        ],
    )
}

/// `(host: string, port: int, data: string) -> string`: connect, close,
/// then send — the send on a closed handle yields the `net_err` message.
///
/// Slots: [host, port, data, conn, t, overlay tag, overlay payload].
fn stale_send() -> FunctionDef {
    def(
        "stale_send",
        &[Ref, I64, Ref, I64, Bool, Raw, Raw],
        3,
        2,
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Mov { dst: 6, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 4,
                src: 5,
                tag: PLATFORM_NET_ERR,
            },
            Uop::Jmpif {
                cond: 4,
                target: 17,
            },
            Uop::Mov { dst: 3, src: 6 },
            Uop::Mov { dst: 5, src: 3 },
            Uop::Call {
                callee: k(3),
                nargs: 1,
            },
            Uop::TagTest {
                dst: 4,
                src: 5,
                tag: PLATFORM_NET_ERR,
            },
            Uop::Jmpif {
                cond: 4,
                target: 17,
            },
            Uop::Mov { dst: 5, src: 3 },
            Uop::Mov { dst: 6, src: 2 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 4,
                src: 5,
                tag: PLATFORM_NET_ERR,
            },
            Uop::Jmpif {
                cond: 4,
                target: 17,
            },
            Uop::Ret { src: 2 }, // unreachable: the stale send fails
            Uop::Ret { src: 6 }, // the net_err message payload
        ],
    )
}

/// `(conn: int) -> tag`: the close result's tag word alone — `io_ok` on a
/// closed handle, `net_err` on an unknown one.
fn close_tag() -> FunctionDef {
    def("close_tag", &[I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(3),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(host: string, port: int) -> int`: connect and return without
/// closing — the connection a dropped VM must close.
fn leak() -> FunctionDef {
    def("leak", &[Ref, I64, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(0),
            nargs: 2,
        },
        Uop::Ret { src: 3 }, // the handle on success
    ])
}

fn net_registry() -> ModuleRegistry {
    registry(vec![
        echo(),
        fetch(),
        connect_payload(),
        recv_payload(),
        stale_send(),
        close_tag(),
        leak(),
    ])
}

fn string(vm: &mut Vm, text: &str) -> Value {
    vm.intern_str(text)
}

fn local_listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    let port = listener.local_addr().expect("bound address").port();
    (listener, port)
}

/// A port with nothing listening on it: bind, note, drop.
fn closed_port() -> u16 {
    let (listener, port) = local_listener();
    drop(listener);
    port
}

/// Serves `expected` bytes read exactly, echoed back, then closed.
fn echo_server(listener: TcpListener, expected: usize) {
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut buf = vec![0; expected];
        sock.read_exact(&mut buf).expect("read the payload");
        sock.write_all(&buf).expect("echo it back");
    });
}

/// Serves a canned HTTP/1.0 response after a full request head, and
/// reports the request bytes it read.
fn http_server(listener: TcpListener, response: &'static [u8]) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut req = Vec::new();
        let mut byte = [0; 1];
        while !req.ends_with(b"\r\n\r\n") {
            sock.read_exact(&mut byte).expect("request bytes");
            req.push(byte[0]);
        }
        sock.write_all(response).expect("write the response");
        tx.send(req).expect("report the request");
    });
    rx
}

/// Accepts, then never answers: the hostile host for the timeout path.
fn silent_server(listener: TcpListener) {
    thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
        thread::sleep(Duration::from_secs(30));
    });
}

fn payload_string(vm: &mut Vm, result: Value) -> String {
    let Value::Ref(id) = result else {
        panic!("the payload is a string ref, got {result:?}");
    };
    vm.objects()
        .str_at(id)
        .expect("the payload names a string")
        .to_owned()
}

#[test]
fn tcp_round_trips_against_a_local_listener() {
    let (listener, port) = local_listener();
    let data = "ping — ünïcödé ✓\n\0";
    echo_server(listener, data.len());
    let mut vm = Vm::new(net_registry());
    let args = [
        string(&mut vm, "127.0.0.1"),
        Value::I64(i64::from(port)),
        string(&mut vm, data),
    ];
    let result = vm
        .run("net", "echo", &args)
        .expect("the golden round trip succeeds");
    assert_eq!(payload_string(&mut vm, result), data);
}

#[test]
fn an_orderly_close_recvs_the_empty_string() {
    let (listener, port) = local_listener();
    // serve an immediate close with no bytes
    thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
    });
    let mut vm = Vm::new(net_registry());
    let args = [
        string(&mut vm, "127.0.0.1"),
        Value::I64(i64::from(port)),
        Value::I64(64),
    ];
    let result = vm
        .run("net", "recv_payload", &args)
        .expect("connect, recv, close succeed");
    assert_eq!(payload_string(&mut vm, result), "", "close yields empty");
}

#[test]
fn a_refused_port_yields_the_net_err_message() {
    let port = closed_port();
    let mut vm = Vm::new(net_registry());
    let args = [string(&mut vm, "127.0.0.1"), Value::I64(i64::from(port))];
    let result = vm
        .run("net", "connect_payload", &args)
        .expect("the span overlays without a trap");
    let message = payload_string(&mut vm, result);
    // Only the OS message text is locale-dependent; the operation+target
    // prefix and the `(os error N)` code suffix are stable. WSAECONNREFUSED
    // is 10061, ECONNREFUSED is 111.
    let refused = if cfg!(windows) { "(os error 10061)" } else { "(os error 111)" };
    assert!(
        message.starts_with("net_connect 127.0.0.1:") && message.ends_with(refused),
        "the message names the operation, target, and refusal code: {message:?}"
    );
}

#[test]
fn a_silent_socket_times_out_within_the_bound() {
    let (listener, port) = local_listener();
    silent_server(listener);
    let mut vm = Vm::new(net_registry());
    vm.set_net_timeout(Duration::from_millis(200));
    let args = [
        string(&mut vm, "127.0.0.1"),
        Value::I64(i64::from(port)),
        Value::I64(64),
    ];
    let started = Instant::now();
    let result = vm
        .run("net", "recv_payload", &args)
        .expect("the timeout is a net_err, never a trap");
    let elapsed = started.elapsed();
    let message = payload_string(&mut vm, result);
    assert_eq!(message, "net_recv 0: timed out after 200ms");
    assert!(
        elapsed < Duration::from_secs(5),
        "the interpreter cannot hang on a silent host: {elapsed:?}"
    );
}

#[test]
fn https_and_other_schemes_are_rejected_without_sockets() {
    let mut vm = Vm::new(net_registry());
    let https = [string(&mut vm, "https://example.com/path")];
    let result = vm
        .run("net", "fetch", &https)
        .expect("rejection is a net_err, not a trap");
    assert_eq!(
        payload_string(&mut vm, result),
        "http_get https://example.com/path: https unsupported at this platform tier (no TLS)"
    );
    let ftp = [string(&mut vm, "ftp://example.com/path")];
    let result = vm
        .run("net", "fetch", &ftp)
        .expect("rejection is a net_err, not a trap");
    assert_eq!(
        payload_string(&mut vm, result),
        "http_get ftp://example.com/path: unsupported URL scheme (http:// only)"
    );
}

#[test]
fn http_get_serves_the_local_body() {
    const BODY: &str = "hello world";
    let response: &'static [u8] =
        b"HTTP/1.0 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world";
    let (listener, port) = local_listener();
    let request = http_server(listener, response);
    let mut vm = Vm::new(net_registry());
    let url = [string(&mut vm, &format!("http://127.0.0.1:{port}/index"))];
    let result = vm
        .run("net", "fetch", &url)
        .expect("the local exchange succeeds");
    assert_eq!(payload_string(&mut vm, result), BODY);
    let head = request
        .recv_timeout(Duration::from_secs(5))
        .expect("the server saw the request");
    assert!(
        head.starts_with(b"GET /index HTTP/1.0\r\nHost: 127.0.0.1") && head.ends_with(b"\r\n\r\n"),
        "the request is a well-formed HTTP/1.0 GET: {head:?}"
    );
}

#[test]
fn http_get_rejects_an_over_cap_response() {
    // a header terminator plus more than the 1 MiB cap of body
    let (listener, port) = local_listener();
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut req = Vec::new();
        let mut byte = [0; 1];
        while !req.ends_with(b"\r\n\r\n") {
            sock.read_exact(&mut byte).expect("request bytes");
            req.push(byte[0]);
        }
        sock.write_all(b"HTTP/1.0 200 OK\r\n\r\n").expect("head");
        let body = vec![b'x'; 1024 * 1024 + 1];
        sock.write_all(&body).expect("over-cap body");
    });
    let mut vm = Vm::new(net_registry());
    let url = format!("http://127.0.0.1:{port}/big");
    let args = [string(&mut vm, &url)];
    let result = vm
        .run("net", "fetch", &args)
        .expect("the cap is a net_err, not a trap");
    let expected = format!("http_get {url}: response exceeds the 1 MiB cap");
    assert_eq!(payload_string(&mut vm, result), expected);
}

#[test]
fn a_stale_handle_is_unknown_after_close() {
    let (listener, port) = local_listener();
    echo_server(listener, 0); // accepts; the send never happens
    let mut vm = Vm::new(net_registry());
    let args = [
        string(&mut vm, "127.0.0.1"),
        Value::I64(i64::from(port)),
        string(&mut vm, "late"),
    ];
    let result = vm
        .run("net", "stale_send", &args)
        .expect("the stale send is a net_err, not a trap");
    let message = payload_string(&mut vm, result);
    assert_eq!(message, "net_send 0: unknown connection 0");
    // and a close of the same handle fails the same way
    assert_eq!(
        vm.run("net", "close_tag", &[Value::I64(0)]),
        Ok(Value::Tag(PLATFORM_NET_ERR))
    );
    // a handle that was never opened too
    assert_eq!(
        vm.run("net", "close_tag", &[Value::I64(999)]),
        Ok(Value::Tag(PLATFORM_NET_ERR))
    );
}

#[test]
fn dropping_the_vm_closes_the_open_connection() {
    let (listener, port) = local_listener();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut byte = [0; 1];
        let read = sock.read(&mut byte).expect("the socket stays readable");
        tx.send(read).expect("report the outcome");
    });
    let mut vm = Vm::new(net_registry());
    let args = [string(&mut vm, "127.0.0.1"), Value::I64(i64::from(port))];
    assert_eq!(
        vm.run("net", "leak", &args),
        Ok(Value::I64(0)),
        "the leak fixture connects without closing"
    );
    drop(vm);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the read ends"),
        0,
        "the peer sees an orderly EOF when the VM drops"
    );
}

#[test]
fn connection_handles_are_vm_state_not_run_state() {
    let (first, port_a) = local_listener();
    let (second, port_b) = local_listener();
    echo_server(first, 0);
    echo_server(second, 0);
    let mut vm = Vm::new(net_registry());
    let a = [string(&mut vm, "127.0.0.1"), Value::I64(i64::from(port_a))];
    let b = [string(&mut vm, "127.0.0.1"), Value::I64(i64::from(port_b))];
    assert_eq!(vm.run("net", "leak", &a), Ok(Value::I64(0)));
    // the table survives the run boundary: ids keep counting up
    assert_eq!(vm.run("net", "leak", &b), Ok(Value::I64(1)));
    // and closing the second handle succeeds
    assert_eq!(
        vm.run("net", "close_tag", &[Value::I64(1)]),
        Ok(Value::Tag(PLATFORM_IO_OK))
    );
}
