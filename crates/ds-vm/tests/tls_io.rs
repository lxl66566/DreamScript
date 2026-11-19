//! The net face's TLS tier end to end (H5.5's 2026-11-17 ruling): https
//! over rustls against a local TLS server — hermetic, no external
//! network — with the server's self-signed certificate pinned as the
//! client's trust anchor through [`Vm::set_tls_config`]. Golden
//! coverage: an `https://` URL routes through the net face's `http_get`
//! (the routing body a tls-providing host serves), the capability's own
//! `https_get` op answers directly, plain URLs stay refused at that op,
//! the default webpki roots refuse the self-signed peer, and a
//! tls-omitting host refuses https URLs with the unprovided-capability
//! `net_err`.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        ConstIdx, FrameLayout,
        SlotType::{Raw, Ref},
        uop::Uop,
    },
    assemble,
    platform::{Capability, OP_HTTP_GET, OP_HTTPS_GET, Platform, STD_NAME},
};

/// The fixtures' shared builder: a one-argument `(url: string) ->
/// string` fetch whose payload word is the response body on success or
/// the `net_err` message on failure (the net-IO fixtures' shape).
fn fetch(name: &str, op: u32) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(vec![Ref, Ref, Raw, Raw], 1, 2).expect("valid fixture layout"),
        pool: vec![Const::FuncRef(ConstFuncRef::Named {
            module: STD_NAME.into(),
            func: op,
        })],
        code: assemble(&[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: ConstIdx(0),
                nargs: 1,
            },
            Uop::Mov { dst: 1, src: 3 },
            Uop::Ret { src: 1 },
        ])
        .expect("fixture body encodes"),
    }
}

/// The registry serving the whole std platform (tls included).
fn std_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::std());
    reg.install(
        ModuleVersion::new("tls", vec![
            fetch("fetch", OP_HTTP_GET),
            fetch("direct", OP_HTTPS_GET),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// The registry of a tls-omitting host: the std platform minus the tls
/// capability (the "selected or omitted per host" ruling).
fn no_tls_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::new("std", &[
        Capability::Console,
        Capability::Fsys,
        Capability::Net,
        Capability::Time,
    ]));
    reg.install(
        ModuleVersion::new("tls", vec![fetch("fetch", OP_HTTP_GET)]).expect("valid fixture module"),
    );
    reg
}

fn string(vm: &mut Vm, text: &str) -> Value {
    vm.intern_str(text)
}

fn payload_string(vm: &mut Vm, result: &FrameAnswer) -> String {
    let [Value::Ref(id)] = result.as_slice() else {
        panic!("the payload is a string ref, got {result:?}");
    };
    let id = *id;
    vm.objects()
        .str_at(id.id())
        .expect("the payload names a string")
        .to_owned()
}

/// A self-signed server certificate for `127.0.0.1` and the client
/// config pinning it as the sole trust anchor.
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

/// Serves `response` over TLS after a full request head — no close_notify
/// before the drop, pinning the close-tolerant read discipline — and
/// reports the request bytes it read.
fn tls_server(
    listener: TcpListener,
    server: rustls::ServerConfig,
    response: &'static [u8],
) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (sock, _) = listener.accept().expect("accept");
        let conn = rustls::ServerConnection::new(Arc::new(server)).expect("the TLS server opens");
        let mut stream = rustls::StreamOwned::new(conn, sock);
        let mut req = Vec::new();
        let mut byte = [0; 1];
        while !req.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("request bytes");
            req.push(byte[0]);
        }
        stream.write_all(response).expect("write the response");
        stream.flush().expect("flush the response");
        tx.send(req).expect("report the request");
        // the drop closes without close_notify: the HTTP/1.0 close
    });
    rx
}

fn local_listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    let port = listener.local_addr().expect("bound address").port();
    (listener, port)
}

/// An https URL answers through the net face's `http_get` — the routing
/// body a tls-providing host serves — and the request the server read
/// is a well-formed HTTP/1.0 GET.
#[test]
fn https_routes_through_the_net_face() {
    const BODY: &str = "secure hello";
    let (listener, port) = local_listener();
    let (server, client) = pinned_pair();
    let response: &'static [u8] = b"HTTP/1.0 200 OK\r\nContent-Length: 12\r\n\r\nsecure hello";
    let request = tls_server(listener, server, response);
    let mut vm = Vm::new(std_registry());
    vm.set_tls_config(client);
    let url = [string(&mut vm, &format!("https://127.0.0.1:{port}/index"))];
    let result = vm
        .run("tls", "fetch", &url)
        .expect("the TLS exchange succeeds");
    assert_eq!(payload_string(&mut vm, &result), BODY);
    let head = request
        .recv_timeout(Duration::from_secs(5))
        .expect("the server saw the request");
    assert!(
        head.starts_with(b"GET /index HTTP/1.0\r\nHost: 127.0.0.1") && head.ends_with(b"\r\n\r\n"),
        "the request is a well-formed HTTP/1.0 GET: {head:?}"
    );
}

/// The tls capability's own op answers directly, and a plain URL at it
/// is the scheme refusal.
#[test]
fn the_https_op_answers_and_refuses_plain_urls() {
    let (listener, port) = local_listener();
    let (server, client) = pinned_pair();
    let response: &'static [u8] = b"HTTP/1.0 200 OK\r\n\r\ndirect body";
    let _request = tls_server(listener, server, response);
    let mut vm = Vm::new(std_registry());
    vm.set_tls_config(client);
    let url = [string(&mut vm, &format!("https://127.0.0.1:{port}/direct"))];
    let result = vm
        .run("tls", "direct", &url)
        .expect("the op's TLS exchange succeeds");
    assert_eq!(payload_string(&mut vm, &result), "direct body");
    let plain = [string(&mut vm, "http://127.0.0.1:1/path")];
    let result = vm
        .run("tls", "direct", &plain)
        .expect("the scheme refusal is a net_err, not a trap");
    assert_eq!(
        payload_string(&mut vm, &result),
        "https_get http://127.0.0.1:1/path: unsupported URL scheme (https:// only)"
    );
}

/// The default roots are the webpki set: the self-signed peer fails
/// verification, so the exchange answers the certificate `net_err`.
#[test]
fn the_default_roots_refuse_the_self_signed_peer() {
    let (listener, port) = local_listener();
    let (server, _client) = pinned_pair();
    let response: &'static [u8] = b"HTTP/1.0 200 OK\r\n\r\nshould not arrive";
    let _request = tls_server(listener, server, response);
    let mut vm = Vm::new(std_registry());
    let url = [string(&mut vm, &format!("https://127.0.0.1:{port}/index"))];
    let result = vm
        .run("tls", "fetch", &url)
        .expect("the verification failure is a net_err, not a trap");
    let message = payload_string(&mut vm, &result);
    assert!(
        message.starts_with("http_get https://127.0.0.1") && message.contains("certificate"),
        "the message names the operation and the verification failure: {message:?}"
    );
}

/// A tls-omitting host keeps the plain `http_get` body: an https URL is
/// the unprovided-capability refusal, no socket opened.
#[test]
fn a_tls_omitting_host_refuses_https_urls() {
    let mut vm = Vm::new(no_tls_registry());
    let url = [string(&mut vm, "https://example.com/path")];
    let result = vm
        .run("tls", "fetch", &url)
        .expect("the refusal is a net_err, not a trap");
    assert_eq!(
        payload_string(&mut vm, &result),
        "http_get https://example.com/path: https refused: the platform does not provide the tls \
         capability"
    );
}
