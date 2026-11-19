//! The net handle face end to end: the fixture parses, checks clean,
//! lowers, round-trips through the ds-mod writer, installs into the VM,
//! and runs against a local listener — `net_connect` answering the
//! package-owned `net_handle` struct's one inline word (the connection
//! table's opaque key), the ds-source `send`/`recv`/`close_conn`
//! handlers reading the id word out and passing the raw seams an int (a
//! native call cannot borrow a deinit-carrying argument), and the deinit
//! pairing: the scope end closes a connection the owner never closed,
//! and the close after an explicit `close_conn` stays a clean no-op
//! (`net_close` total and idempotent). The fallible reads answer the
//! failure taxonomy's union: each class discriminant lands on the
//! `net_err` case the error class names, so match, `?`, and the `.err`
//! handler extract real enum cases — a closed handle answers `refused`,
//! a silent socket's bounded wait `timeout`. A refused connect keeps
//! the total spelling's printing halt (the handle is deinit-carrying,
//! so a union member holding it refuses), so a failed connect run
//! answers the tag.

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// A silent server thread: accepts, then never answers — the hostile
/// host for the timeout class.
fn silent_server(listener: TcpListener) {
    thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
        thread::sleep(Duration::from_secs(30));
    });
}

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables); the std
/// platform table is this test's to provide, as any platform host must.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/net_handle_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("net_handle_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "net_handle",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

/// Binds a loopback listener and answers its port.
fn local_listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    let port = listener.local_addr().expect("bound address").port();
    (listener, port)
}

/// The run arguments of one listener's address.
fn address(vm: &mut Vm, port: u16) -> [Value; 2] {
    [vm.intern_str("127.0.0.1"), Value::I64(i64::from(port))]
}

/// A port with nothing listening on it: bind, note, drop.
fn closed_port() -> u16 {
    let (listener, port) = local_listener();
    drop(listener);
    port
}

/// Serves the four ping bytes echoed back, then closed.
fn echo_server(listener: TcpListener) {
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4];
        sock.read_exact(&mut buf).expect("read the payload");
        sock.write_all(&buf).expect("echo it back");
    });
}

/// The payload string of a one-word `Ref` answer.
fn payload_string(vm: &mut Vm, result: &FrameAnswer) -> String {
    let [Value::Ref(id)] = result.as_slice() else {
        panic!("the answer is a string ref, got {result:?}");
    };
    let id = *id;
    vm.objects()
        .str_at(id.id())
        .expect("the answer names a string")
        .to_owned()
}

#[test]
fn the_send_recv_close_round_trip_answers_the_reply() {
    let (listener, port) = local_listener();
    echo_server(listener);
    let (mut vm, _lowered) = build_vm();
    vm.capture_output();
    let args = address(&mut vm, port);
    let result = vm
        .run("net_handle", "main", &args)
        .expect("the connect, send, recv, and close all answered");
    assert_eq!(payload_string(&mut vm, &result), "ping");
    assert_eq!(vm.captured_output(), b"", "the clean run prints nothing");
}

#[test]
fn the_scope_end_deinit_closes_the_unclosed_connection() {
    let (listener, port) = local_listener();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4];
        sock.read_exact(&mut buf).expect("the ping arrives");
        // the binding's scope end fired the deinit: the peer sees EOF
        let mut probe = [0u8; 1];
        let read = sock.read(&mut probe).expect("the socket stays readable");
        tx.send(read).expect("report the outcome");
    });
    let (mut vm, _) = build_vm();
    vm.capture_output();
    let args = address(&mut vm, port);
    assert_eq!(
        vm.run("net_handle", "drop_closes", &args),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the deinit path runs clean"
    );
    assert_eq!(vm.captured_output(), b"", "no failure line prints");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the read ends"),
        0,
        "the deinit closed the connection: the peer saw an orderly EOF"
    );
}

#[test]
fn the_deinit_after_an_explicit_close_stays_a_clean_no_op() {
    let (listener, port) = local_listener();
    thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
    });
    let (mut vm, _) = build_vm();
    vm.capture_output();
    let args = address(&mut vm, port);
    assert_eq!(
        vm.run("net_handle", "closed_then_dropped", &args),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the second close (the deinit's) is the idempotent no-op"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "an idempotent close prints no net error line and never halts"
    );
}

#[test]
fn a_refused_connect_prints_the_message_and_halts_on_the_tag() {
    let port = closed_port();
    let (mut vm, _) = build_vm();
    vm.capture_output();
    let args = address(&mut vm, port);
    assert_eq!(
        vm.run("net_handle", "fail_connect", &args),
        Ok(FrameAnswer::one(Value::Tag(native::PLATFORM_NET_REFUSED))),
        "the total spelling halts on the discriminant, the run result is the tag"
    );
    let out = String::from_utf8_lossy(vm.captured_output()).into_owned();
    assert!(
        out.starts_with("net error: net_connect 127.0.0.1:"),
        "the printed failure line was {out:?}"
    );
}

/// The fallible reads answer the taxonomy's members: a closed handle's
/// recv answers the `refused` case, a silent socket's bounded wait the
/// `timeout` case — never the printing halt of the total spellings.
#[test]
fn a_closed_handle_answers_the_refused_case() {
    let (listener, port) = local_listener();
    thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
    });
    let (mut vm, _) = build_vm();
    vm.capture_output();
    let args = address(&mut vm, port);
    let result = vm
        .run("net_handle", "recv_after_close", &args)
        .expect("the closed-handle recv answers its union");
    assert_eq!(payload_string(&mut vm, &result), "refused");
    assert_eq!(
        vm.captured_output(),
        b"",
        "no halt: the failure rode the union"
    );
}

#[test]
fn a_silent_socket_answers_the_timeout_case() {
    let (listener, port) = local_listener();
    silent_server(listener);
    let (mut vm, _) = build_vm();
    vm.set_net_timeout(Duration::from_millis(200));
    let args = address(&mut vm, port);
    let result = vm
        .run("net_handle", "recv_silent", &args)
        .expect("the bounded wait answers its union");
    assert_eq!(payload_string(&mut vm, &result), "timed out");
}

#[test]
fn a_stale_send_answers_the_refused_case() {
    let (listener, port) = local_listener();
    thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
    });
    let (mut vm, _) = build_vm();
    let args = [
        vm.intern_str("127.0.0.1"),
        Value::I64(i64::from(port)),
        vm.intern_str("late"),
    ];
    let result = vm
        .run("net_handle", "send_after_close", &args)
        .expect("the stale send answers its union");
    assert_eq!(payload_string(&mut vm, &result), "refused");
}

/// The payload `Ref` of a two-word union answer: the string member's
/// span word, tag word first.
fn union_string(vm: &mut Vm, result: &FrameAnswer) -> String {
    let [Value::Tag(_), Value::Ref(id)] = result.as_slice() else {
        panic!(
            "the answer is the string member's span, got {:?}",
            result.as_slice()
        );
    };
    let id = *id;
    vm.objects()
        .str_at(id.id())
        .expect("the answer names a string")
        .to_owned()
}

/// `?` propagation end to end: the failure arm rides the return union
/// out to the host — the recv's answer is the union span's tag word,
/// never a halt — and the success side answers the payload member.
#[test]
fn the_recv_failure_rides_the_return_union_under_propagation() {
    let (listener, port) = local_listener();
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        sock.write_all(b"hello").expect("the reply arrives");
    });
    let (mut vm, _) = build_vm();
    let args = address(&mut vm, port);
    let result = vm
        .run("net_handle", "pass_recv", &args)
        .expect("the propagated recv answers its union");
    assert_eq!(union_string(&mut vm, &result), "hello");

    let (listener, port) = local_listener();
    silent_server(listener);
    let (mut vm, _) = build_vm();
    vm.set_net_timeout(Duration::from_millis(200));
    let args = address(&mut vm, port);
    let result = vm
        .run("net_handle", "pass_recv", &args)
        .expect("the propagated timeout answers its union");
    let words = result.as_slice();
    assert!(
        matches!(words.first(), Some(Value::Tag(_))),
        "the failure arm is the union span's tag word, got {words:?}"
    );
}

/// `?` propagation over the unit success: the send's failure arm rides
/// the return union (the tag word alone — `() | net_err` spans no
/// payload), the success side the unit value.
#[test]
fn the_send_failure_rides_the_return_union_under_propagation() {
    let (listener, port) = local_listener();
    echo_server(listener);
    let (mut vm, _) = build_vm();
    let args = address(&mut vm, port);
    let result = vm
        .run("net_handle", "pass_send", &args)
        .expect("the propagated send answers its union");
    assert_eq!(
        result.as_slice().len(),
        1,
        "the unit success is one tag word"
    );

    let (listener, port) = local_listener();
    thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
    });
    let (mut vm, _) = build_vm();
    let args = address(&mut vm, port);
    let result = vm
        .run("net_handle", "pass_send", &args)
        .expect("the propagated stale send answers its union");
    let words = result.as_slice();
    assert!(
        matches!(words.first(), Some(Value::Tag(_))),
        "the failure arm is the union's tag word, got {words:?}"
    );
}

/// A probed `io_err` row over a net native has no class mapping: the
/// net natives answer the taxonomy's classes, so the probe that once
/// ran mechanically mis-tagging net failures refuses at lowering.
#[test]
fn a_probed_io_err_row_over_a_net_native_refuses() {
    let source = "\
tag int
tag string
tag io_err
extern fn net_recv (conn: int, max_bytes: int) -> string | io_err
fn main (conn: int) -> string | io_err {
  const r = net_recv(conn, 64)
  r
}";
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the io_err probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    );
    let Err(err) = lowered else {
        panic!("the mis-tagging probe refuses at lowering")
    };
    assert!(
        err.to_string()
            .contains("ambient case names (timeout, refused)"),
        "the refusal names the class members: {err}"
    );
}
