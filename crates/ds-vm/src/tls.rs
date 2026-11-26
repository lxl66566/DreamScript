//! The net face's TLS tier: https over
//! rustls as a platform capability the host selects or omits — never
//! builtin and never @std. `platform_module` installs these bodies only
//! where the platform provides the tls capability: the `http_get` slot
//! carries the https-routing body instead of the plain body that
//! refuses https URLs, and the capability's own `https_get` op is
//! served or keeps its reserved trap under the install-time capability
//! validation, like every unserved op.
//!
//! Provider and roots: ring (never aws-lc-rs, whose native toolchain
//! would break the Windows build), the provider constructed explicitly
//! rather than installed as the process default so concurrent VMs in
//! one process share no global crypto choice, and webpki-roots' static
//! Mozilla set as the trust anchors — no system store and no client
//! certs, an embedding host or test pinning its own roots
//! through [`crate::Vm::set_tls_config`].
//!
//! The blocking discipline is the net ops' own ([`crate::net`]): every
//! wait rides the connection table's bounded socket timeouts, a
//! blocking foreign call pinning its fiber. Both legs park under an
//! installed reactor on Linux: the plain leg through the
//! staged exchange machine, the rustls leg through the parked twin
//! below — the session is a synchronous state machine, so the
//! transport drives it a step per readiness wake, the session's own
//! wants-write/wants-read naming the interest each step watches.
//!
//! The server half rides the same capability: the
//! host or test pins the rustls [`ServerConfig`] through
//! [`crate::Vm::set_tls_server_config`] (no default identity — a
//! server certificate is the host's own to pin, the unprovisioned
//! call answering its own refusal), and `net_accept_tls` takes one
//! pending connection off the bound listener and upgrades it over
//! that identity — the staged handshake the client leg's own shape
//! (the same wants naming each wake's interest), the completion
//! registering the accepted stream with its session so the connection
//! owns the full `net_handle` face over the tls records, and a
//! non-Linux host or a no-reactor embedder keeping the blocking twin.

use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::{Arc, LazyLock},
    time::Duration,
};

use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConnection, StreamOwned,
    pki_types::ServerName,
};

use crate::{
    native::{NativeResult, OutputSink, i64_arg, ref_arg},
    net::{self, Scheme},
    object::Heaps,
    value::Value,
    vm::Trap,
};

/// The default TLS client: the webpki root set over the ring provider,
/// shared by every VM that provides the tls capability without pinning
/// its own roots.
static WEBPKI_CLIENT: LazyLock<Arc<ClientConfig>> = LazyLock::new(|| {
    // ring over aws-lc-rs keeps the build free of native tooling; the
    // explicit provider construction (never `CryptoProvider::
    // install_default`) keeps concurrent VMs off any process global
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring serves the safe default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
});

/// The webpki-rooted default client ([`WEBPKI_CLIENT`]).
pub(crate) fn webpki_client() -> Arc<ClientConfig> {
    Arc::clone(&WEBPKI_CLIENT)
}

/// `http_get(url: string) -> string | net_err` on a tls-providing host:
/// the routing body — an `http://` URL takes the plain exchange, an
/// `https://` URL the rustls one. Installed at the `http_get` slot only
/// where the platform provides the tls capability.
pub(crate) fn http_get_tls_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let url = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    // both legs ride their parked machines under a reactor (the staged
    // connect `net_connect` parks on): the plain exchange its own, the
    // rustls session the tls tier's readiness-driven twin
    #[cfg(target_os = "linux")]
    {
        let (scheme, rest) = net::scheme_of(&url);
        if out.net().parking() {
            match scheme {
                Scheme::Http => return net::parked_plain_fetch(heaps, out, rest, &url),
                Scheme::Https => return parked_tls_fetch(heaps, out, rest, &url, "http_get"),
                Scheme::Other => {},
            }
        }
    }
    match fetch(out, &url, false) {
        Ok(body) => Ok(net::ok(heaps.new_str(body))),
        Err(err) => Ok(net::net_err(
            heaps,
            net::NetClass::of(&err),
            "http_get",
            &url,
            &err.to_string(),
        )),
    }
}

/// `https_get(url: string) -> string | net_err`: the tls capability's
/// own op, https only — the explicit seam a hand-assembled call site or
/// a package-declared extern addresses. No ambient spelling declares it
/// (the net face's `http_get` routes https URLs onto the tier), the
/// nursery-seam precedent for table slots without a surface name.
pub(crate) fn https_get_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let url = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    #[cfg(target_os = "linux")]
    {
        let (scheme, rest) = net::scheme_of(&url);
        if out.net().parking() && matches!(scheme, Scheme::Https) {
            return parked_tls_fetch(heaps, out, rest, &url, "https_get");
        }
    }
    match fetch(out, &url, true) {
        Ok(body) => Ok(net::ok(heaps.new_str(body))),
        Err(err) => Ok(net::net_err(
            heaps,
            net::NetClass::of(&err),
            "https_get",
            &url,
            &err.to_string(),
        )),
    }
}

/// The fetch body both ops share: split the URL, take the plain or the
/// rustls exchange by scheme, answer the response body or the failure
/// the class mapping reads its class off. `https_only` is the
/// `https_get` op's scheme discipline.
fn fetch(out: &mut OutputSink, url: &str, https_only: bool) -> Result<String, io::Error> {
    let (scheme, rest) = net::scheme_of(url);
    let tls = match scheme {
        Scheme::Https => true,
        Scheme::Http if https_only => {
            return Err(io::Error::other("unsupported URL scheme (https:// only)"));
        },
        Scheme::Http => false,
        Scheme::Other => {
            return Err(io::Error::other(format!(
                "unsupported URL scheme {}",
                if https_only { "(https:// only)" } else { "(http:// and https:// only)" }
            )));
        },
    };
    let (host, port, authority, path) = net::split_target(rest).map_err(io::Error::other)?;
    let timeout = out.net().timeout();
    if !tls {
        let mut stream = out.net().open(&host, port)?;
        return net::exchange(&mut stream, &net::request(&authority, &path), timeout);
    }
    let config = out.net().tls_config();
    let name = ServerName::try_from(host.clone())
        .map_err(|_| io::Error::other("invalid server name in the URL"))?;
    let sock = out.net().open(&host, port)?;
    let conn = ClientConnection::new(config, name).map_err(io::Error::other)?;
    let mut stream = CloseTolerant(StreamOwned::new(conn, sock));
    net::exchange(&mut stream, &net::request(&authority, &path), timeout)
}

/// The rustls stream under the tier's close-delimited HTTP/1.0
/// discipline: a peer ending the connection without TLS close_notify
/// (the common HTTP/1.0 server behavior) reads as an orderly close —
/// the same EOF the plain exchange takes — never an error.
struct CloseTolerant(StreamOwned<ClientConnection, TcpStream>);

impl Read for CloseTolerant {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
            other => other,
        }
    }
}

impl Write for CloseTolerant {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// The parked TLS exchange's machine: the rustls session over the staged connect's
/// transport, one `step` per readiness wake. The session is a
/// synchronous state machine — `wants_write` names encrypted bytes it
/// owes the socket, `wants_read` ciphertext it awaits, and its
/// plaintext buffers apply their own backpressure (`wants_read` going
/// false while received plaintext stands undrained) — so the parked
/// transport maps the session's own wants onto epoll interests: each
/// wake runs the session against the non-blocking socket until the OS
/// answers would-block, the answer naming the direction the next wake
/// watches. The request stages through the session's writer (bytes
/// staged mid-handshake ride the post-handshake records), the response
/// plaintext drains through the reader between the ciphertext reads,
/// and the peer's TCP close ends the exchange the blocking twin's
/// close-tolerant way — the finish reading the body after the header
/// terminator, never an error.
#[derive(Debug)]
pub(crate) struct TlsFetch {
    /// The transport: the pending connect's row first, the registered
    /// connection once connected.
    pub(crate) conn: i64,
    /// Whether the connect completed and the transport registered.
    pub(crate) connected: bool,
    /// The rustls session, the machine's own state.
    session: ClientConnection,
    /// The request bytes still to stage through the session's writer.
    out: Vec<u8>,
    /// The response plaintext read so far.
    seen: Vec<u8>,
}

impl TlsFetch {
    /// The machine's construction (the row's storage the connection
    /// table owns, `net::start_tls_fetch`).
    pub(crate) fn new(conn: i64, connected: bool, session: ClientConnection, out: Vec<u8>) -> Self {
        Self {
            conn,
            connected,
            session,
            out,
            seen: Vec::new(),
        }
    }
}

/// Advances one parked TLS exchange as far as the readiness takes it:
/// the staged connect's probe first, then the session loop — drain
/// what the session owes the socket, feed what it awaits, stage the
/// request, drain the plaintext — until the OS answers would-block
/// (the interest named) or the exchange answers. Every terminal runs
/// the same finish the plain machine and the blocking twin share.
pub(crate) fn step(machine: &mut TlsFetch, net: &mut net::Connections) -> net::FetchWake {
    use crate::reactor::Direction;
    // the connect stage: the plain machine's own probe (the resolve
    // step first — a resolving row advancing at its posted eventfd, the
    // direction passing through)
    if !machine.connected {
        match net.connect_wake(machine.conn) {
            net::ConnectWake::Pending { watch } => return net::FetchWake::Pending { watch },
            net::ConnectWake::Done(Err(err)) => return net::FetchWake::Done(Err(err)),
            net::ConnectWake::Done(Ok(conn)) => {
                machine.conn = conn;
                machine.connected = true;
            },
        }
    }
    loop {
        // the session's outgoing bytes — handshake flights first, then
        // the request's records — drain to the socket before anything
        // else (the session buffers nothing it could send)
        while machine.session.wants_write() {
            let sock = match net.stream(machine.conn) {
                Ok(sock) => sock,
                Err(err) => return net::FetchWake::Done(Err(err)),
            };
            match machine.session.write_tls(sock) {
                Ok(0) => {
                    return net::FetchWake::Done(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "the transport closed under the session",
                    )))
                },
                Ok(_) => {},
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    return net::FetchWake::Pending {
                        watch: Direction::Write,
                    }
                },
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
                Err(err) => return net::FetchWake::Done(Err(err)),
            }
        }
        // the handshake awaits the peer's flights
        if machine.session.is_handshaking() {
            // mid-handshake with neither want naming work is the
            // session's own backpressure state; the read interest is
            // the safe wait, level-triggered readiness correcting a
            // wrong guess at the next poll
            if !machine.session.wants_read() {
                return net::FetchWake::Pending {
                    watch: Direction::Read,
                };
            }
            if let Some(wake) = read_once(machine, net) {
                return wake;
            }
            continue;
        }
        // the request stages through the writer once the handshake
        // allows application data (a partial staging re-drains the
        // records it encrypted, the writer's buffer bounded)
        if !machine.out.is_empty() {
            let took = match machine.session.writer().write(&machine.out) {
                Ok(took) => took,
                Err(err) => return net::FetchWake::Done(Err(err)),
            };
            if took == 0 && !machine.session.wants_write() {
                return net::FetchWake::Done(Err(io::Error::other(
                    "the session accepted no request bytes",
                )));
            }
            machine.out.drain(..took);
            if machine.out.is_empty()
                && let Err(err) = machine.session.writer().flush()
            {
                return net::FetchWake::Done(Err(err));
            }
            continue;
        }
        // the response: ciphertext in while the session awaits it, the
        // plaintext drained between the reads (draining is what arms
        // the next read — the backpressure state above)
        if machine.session.wants_read() {
            if let Some(wake) = read_once(machine, net) {
                return wake;
            }
            continue;
        }
        loop {
            let mut chunk = [0; net::HTTP_CHUNK];
            match machine.session.reader().read(&mut chunk) {
                // the clean close: the session received close_notify
                // and its plaintext is drained
                Ok(0) => {
                    return net::FetchWake::Done(net::finish_response(&machine.seen));
                },
                Ok(n) => {
                    machine.seen.extend_from_slice(&chunk[..n]);
                    if machine.seen.len() > net::NET_CAP {
                        return net::FetchWake::Done(Err(io::Error::other(
                            "response exceeds the 1 MiB cap",
                        )));
                    }
                },
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                // the peer's close without close_notify: the blocking
                // twin's CloseTolerant mapping, the orderly finish
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                    return net::FetchWake::Done(net::finish_response(&machine.seen));
                },
                Err(err) => return net::FetchWake::Done(Err(err)),
            }
        }
    }
}

/// One ciphertext read attempt: `None` the read made progress (the
/// caller loops), `Some` the wake that ends or parks the step. A TCP
/// EOF answers the close-tolerant finish — the blocking twin maps the
/// session's UnexpectedEof to the orderly close, and the parked twin
/// reads the socket's own EOF the same way; a TLS protocol failure
/// carries the InvalidData wrap rustls's own `complete_io` gives it
/// (the net class mapping folds it into the `net_other` floor).
fn read_once(machine: &mut TlsFetch, net: &mut net::Connections) -> Option<net::FetchWake> {
    use crate::reactor::Direction;
    let sock = match net.stream(machine.conn) {
        Ok(sock) => sock,
        Err(err) => return Some(net::FetchWake::Done(Err(err))),
    };
    match machine.session.read_tls(sock) {
        Ok(0) => Some(net::FetchWake::Done(net::finish_response(&machine.seen))),
        Ok(_) => match machine.session.process_new_packets() {
            Err(err) => Some(net::FetchWake::Done(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                err,
            )))),
            Ok(_) => None,
        },
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Some(net::FetchWake::Pending {
            watch: Direction::Read,
        }),
        Err(err) if err.kind() == io::ErrorKind::Interrupted => None,
        Err(err) => Some(net::FetchWake::Done(Err(err))),
    }
}

/// The parked TLS-fetch entry both tls bodies share (the routing
/// `http_get` and the direct `https_get`): the URL's pieces resolve,
/// the rustls session builds over the configured client, and the
/// staged connect plus the pre-staged request form the exchange
/// machine — the wakes (or the deadline) answering, an immediate
/// failure here answered as its own error span exactly the blocking
/// body's message.
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_wraps)]
fn parked_tls_fetch(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    rest: &str,
    url: &str,
    op: &'static str,
) -> Result<NativeResult, Trap> {
    let (host, port, authority, path) = match net::split_target(rest) {
        Ok(parts) => parts,
        Err(detail) => return Ok(net::net_err(heaps, net::NetClass::Other, op, url, &detail)),
    };
    let config = out.net().tls_config();
    let Ok(name) = ServerName::try_from(host.clone()) else {
        return Ok(net::net_err(
            heaps,
            net::NetClass::Other,
            op,
            url,
            "invalid server name in the URL",
        ));
    };
    let session = match ClientConnection::new(config, name) {
        Ok(session) => session,
        Err(err) => {
            return Ok(net::net_err(
                heaps,
                net::NetClass::Other,
                op,
                url,
                &err.to_string(),
            ))
        },
    };
    let request = net::request(&authority, &path).into_bytes();
    match out.net().start_tls_fetch(&host, port, session, request) {
        Ok(fetch) => {
            let deadline = std::time::Instant::now() + out.net().timeout();
            Ok(NativeResult::Park(crate::sched::WorldWait::tls_fetch(
                fetch,
                op,
                url.to_owned(),
                deadline,
            )))
        },
        Err(err) => Ok(net::net_err(
            heaps,
            net::NetClass::of(&err),
            op,
            url,
            &err.to_string(),
        )),
    }
}

/// `net_accept_tls(lid: int) -> int | net_err` (the raw seam behind the
/// ambient's total `accept_tls`): takes one pending connection off the
/// bound listener and upgrades it over the tls capability's server
/// half — the host-pinned rustls server identity
/// ([`crate::Vm::set_tls_server_config`], no default) answering the
/// accepted connection's handle, the stream registering with its
/// session so the connection owns the full `net_handle` face over
/// the tls records. The ambient keeps the total spelling the
/// deinit-carrying handle forces, so a failed accept or
/// handshake halts on the net error line — `net_listen`'s own
/// printing-halt pairing.
pub(crate) fn net_accept_tls_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let lid = i64_arg(args, 0)?;
    let target = lid.to_string();
    let Some(config) = out.net().tls_server_config() else {
        return Ok(net::net_err(
            heaps,
            net::NetClass::Other,
            "net_accept_tls",
            &target,
            "tls refused: the host installed no server config",
        ));
    };
    let mut session = match ServerConnection::new(config) {
        Ok(session) => session,
        Err(err) => {
            return Ok(net::net_err(
                heaps,
                net::NetClass::Other,
                "net_accept_tls",
                &target,
                &err.to_string(),
            ))
        },
    };
    // the parked machine: the accept parks on the listener's own
    // readable door (`net_accept`'s own), the rustls handshake
    // advancing one step per readiness wake over the accepted stream
    // — the client tier's own discipline, the session's wants naming
    // the interest each wake watches. The linux driver's own; any
    // other host keeps the blocking twin under any reactor.
    #[cfg(target_os = "linux")]
    {
        if out.listeners().parking() {
            let deadline = std::time::Instant::now() + out.listeners().timeout();
            return match out.listeners().try_accept(lid) {
                net::NetAttempt::Done(Err(err)) => Ok(net::net_err(
                    heaps,
                    net::NetClass::of(&err),
                    "net_accept_tls",
                    &target,
                    &err.to_string(),
                )),
                net::NetAttempt::Done(Ok(stream)) => {
                    let conn = out.net().start_tls_accept(lid, None, session, Some(stream));
                    Ok(NativeResult::Park(crate::sched::WorldWait::tls_accept(
                        conn, target, deadline,
                    )))
                },
                net::NetAttempt::Park => {
                    let listener_handle = out.listeners().raw_handle(lid);
                    let conn = out
                        .net()
                        .start_tls_accept(lid, listener_handle, session, None);
                    Ok(NativeResult::Park(crate::sched::WorldWait::tls_accept(
                        conn, target, deadline,
                    )))
                },
            };
        }
    }
    // the blocking twin: the bounded accept, then the handshake over
    // the stream's socket timeouts, the registration carrying the
    // session alongside the stream. A stopped drive (the host's
    // signal helper flipping the stop flag) answers the shutdown trap
    // exactly the plain accept's way.
    let timeout = out.net().timeout();
    let mut stream = match out
        .listeners()
        .accept(lid)
        .and_then(|stream| out.net().bounded(stream))
    {
        Ok(stream) => stream,
        Err(err) if err.kind() == io::ErrorKind::Interrupted => {
            return Err(Trap::Shutdown);
        },
        Err(err) => {
            return Ok(net::net_err(
                heaps,
                net::NetClass::of(&err),
                "net_accept_tls",
                &target,
                &err.to_string(),
            ))
        },
    };
    if let Err(err) = blocking_handshake(&mut stream, &mut session, timeout) {
        return Ok(net::net_err(
            heaps,
            net::NetClass::of(&err),
            "net_accept_tls",
            &target,
            &err.to_string(),
        ));
    }
    let conn = out.net().adopt_blocking_tls(stream, session);
    Ok(net::ok(Value::I64(conn)))
}

/// The blocking twin's handshake: the same session state machine the
/// parked steps drive, run to its end over the stream's socket
/// timeouts — a silent peer's expired bound answering the same
/// timeout span the parked steps re-arm per wake, a TCP EOF before
/// the handshake completes the peer-close refusal.
fn blocking_handshake(
    stream: &mut TcpStream,
    session: &mut ServerConnection,
    timeout: Duration,
) -> Result<(), io::Error> {
    loop {
        while session.wants_write() {
            match session.write_tls(stream) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "the transport closed under the session",
                    ))
                },
                Ok(_) => {},
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("timed out after {timeout:?}"),
                    ))
                },
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
                Err(err) => return Err(err),
            }
        }
        if !session.is_handshaking() {
            return Ok(());
        }
        match session.read_tls(stream) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the peer closed before the tls handshake completed",
                ))
            },
            Ok(_) => {
                session
                    .process_new_packets()
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out after {timeout:?}"),
                ))
            },
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
            Err(err) => return Err(err),
        }
    }
}

/// The parked accept-tls machine (the server twin of [`TlsFetch`]):
/// the accepted stream and its rustls session advancing one step per
/// readiness wake, the session's own wants-write/wants-read naming
/// the interest each step watches — the listener's readable door first
/// (a pending connection stands), the accepted stream's flights after.
/// The completion registers the stream with its session under the
/// pre-minted id the parked caller saw; every failure closes the
/// stream with the row's drop. The linux driver's own machine; no
/// other host mints the wait.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct TlsAccept {
    /// The pre-minted connection id the machine registers under — the
    /// row's own key, the completion's answer.
    conn: i64,
    /// The bound listener the accept stage takes from.
    listener: i64,
    /// The accepted stream once the accept stage ran.
    stream: Option<TcpStream>,
    /// The current transport's registration key: the listener's fd
    /// until a connection stands, the accepted stream's after.
    handle: Option<crate::reactor::IoHandle>,
    /// The rustls server session, the machine's own state.
    pub(crate) session: ServerConnection,
}

#[cfg(target_os = "linux")]
impl TlsAccept {
    /// The machine's construction (the row's storage the connection
    /// table owns, `net::start_tls_accept`): the pre-minted id it
    /// registers under, `stream` a connection the accept already
    /// took, `listener_handle` the parked accept's own key otherwise.
    pub(crate) fn new(
        conn: i64,
        listener: i64,
        listener_handle: Option<crate::reactor::IoHandle>,
        session: ServerConnection,
        stream: Option<TcpStream>,
    ) -> Self {
        let handle = match stream.as_ref() {
            Some(stream) => net::stream_handle(stream),
            None => listener_handle,
        };
        Self {
            conn,
            listener,
            stream,
            handle,
            session,
        }
    }

    /// The current transport's registration key, the park's interest.
    pub(crate) fn handle(&self) -> Option<crate::reactor::IoHandle> {
        self.handle
    }

    /// The accepted stream, the completion's registration handoff.
    pub(crate) fn take_stream(&mut self) -> Option<TcpStream> {
        self.stream.take()
    }
}

/// Advances one parked accept-tls machine as far as the readiness
/// takes it: the accept stage's readable door first (a pending
/// connection stands), then the session loop — drain what the
/// session owes the socket, feed what it awaits — until the OS
/// answers would-block (the interest named) or the handshake
/// answers. The completion registers; every failure closes the
/// stream with the machine's row.
#[cfg(target_os = "linux")]
pub(crate) fn accept_step(
    machine: &mut TlsAccept,
    listeners: &mut net::Listeners,
) -> net::AcceptWake {
    use crate::reactor::Direction;
    loop {
        if machine.stream.is_none() {
            match listeners.try_accept(machine.listener) {
                net::NetAttempt::Park => {
                    return net::AcceptWake::Pending {
                        watch: Direction::Read,
                    }
                },
                net::NetAttempt::Done(Err(err)) => return net::AcceptWake::Done(Err(err)),
                net::NetAttempt::Done(Ok(stream)) => {
                    // the reactor owns readiness from here: the stream
                    // answers would-block to the thread
                    let _ = stream.set_nonblocking(true);
                    machine.handle = net::stream_handle(&stream);
                    machine.stream = Some(stream);
                },
            }
        }
        let Some(stream) = machine.stream.as_mut() else {
            unreachable!("the accept stage set the stream")
        };
        while machine.session.wants_write() {
            match machine.session.write_tls(stream) {
                Ok(0) => {
                    return net::AcceptWake::Done(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "the transport closed under the session",
                    )))
                },
                Ok(_) => {},
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    return net::AcceptWake::Pending {
                        watch: Direction::Write,
                    }
                },
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
                Err(err) => return net::AcceptWake::Done(Err(err)),
            }
        }
        if machine.session.is_handshaking() {
            // mid-handshake with neither want naming work is the
            // session's own backpressure state; the read interest is
            // the safe wait (level-triggered readiness corrects a wrong
            // guess at the next poll)
            if !machine.session.wants_read() {
                return net::AcceptWake::Pending {
                    watch: Direction::Read,
                };
            }
            match machine.session.read_tls(stream) {
                Ok(0) => {
                    return net::AcceptWake::Done(Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "the peer closed before the tls handshake completed",
                    )))
                },
                Ok(_) => {
                    if let Err(err) = machine.session.process_new_packets() {
                        return net::AcceptWake::Done(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            err,
                        )));
                    }
                },
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    return net::AcceptWake::Pending {
                        watch: Direction::Read,
                    }
                },
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
                Err(err) => return net::AcceptWake::Done(Err(err)),
            }
            continue;
        }
        return net::AcceptWake::Done(Ok(machine.conn));
    }
}

/// One session-backed connection's recv step: the session's buffered
/// plaintext first (a record larger than the read leaves its
/// remainder for the next call), then the ciphertext reads that
/// refill it — the close-tolerant EOF discipline the client tier
/// keeps (the peer's TCP EOF without close_notify and the clean
/// close_notify both the orderly close), a socket would-block the
/// park. The connection table's own twins route here; the blocking
/// one answers the park as its expired bound.
pub(crate) fn server_recv_step(
    session: &mut ServerConnection,
    sock: &mut TcpStream,
    max: usize,
) -> net::NetAttempt<Vec<u8>> {
    loop {
        let mut buf = vec![0; max];
        match session.reader().read(&mut buf) {
            // the clean close: the session received close_notify and
            // its plaintext is drained
            Ok(0) => return net::NetAttempt::Done(Ok(Vec::new())),
            Ok(n) => {
                buf.truncate(n);
                return net::NetAttempt::Done(Ok(buf));
            },
            // the peer's close without close_notify: the client
            // tier's CloseTolerant mapping, the orderly close
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                return net::NetAttempt::Done(Ok(Vec::new()));
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {},
            Err(err) => return net::NetAttempt::Done(Err(err)),
        }
        match session.read_tls(sock) {
            // a TCP EOF the same orderly close
            Ok(0) => return net::NetAttempt::Done(Ok(Vec::new())),
            Ok(_) => {
                if let Err(err) = session.process_new_packets() {
                    return net::NetAttempt::Done(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        err,
                    )));
                }
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return net::NetAttempt::Park,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
            Err(err) => return net::NetAttempt::Done(Err(err)),
        }
    }
}

/// One session-backed connection's send step: the session's owed
/// records drain to the socket before any plaintext stages — a park
/// always carries the unstaged remainder, the next wake draining
/// what the session already sealed, so no answer lands while bytes
/// the session owes stand. The staging flushes into records the
/// drain takes.
pub(crate) fn server_send_step(
    session: &mut ServerConnection,
    sock: &mut TcpStream,
    data: &mut Vec<u8>,
) -> net::NetAttempt<()> {
    loop {
        while session.wants_write() {
            match session.write_tls(sock) {
                Ok(0) => {
                    return net::NetAttempt::Done(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "the transport closed under the session",
                    )))
                },
                Ok(_) => {},
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return net::NetAttempt::Park,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
                Err(err) => return net::NetAttempt::Done(Err(err)),
            }
        }
        if data.is_empty() {
            return net::NetAttempt::Done(Ok(()));
        }
        match session.writer().write(data) {
            Ok(0) => {
                return net::NetAttempt::Done(Err(io::Error::other("the session accepted no bytes")))
            },
            Ok(took) => {
                data.drain(..took);
            },
            Err(err) => return net::NetAttempt::Done(Err(err)),
        }
        if let Err(err) = session.writer().flush() {
            return net::NetAttempt::Done(Err(err));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default client constructs: the pinned provider and root set
    /// are a build-time contract, so a provider- or feature-level drift
    /// that breaks construction fails here before any socket opens.
    #[test]
    fn the_webpki_client_constructs() {
        let _client = webpki_client();
    }
}
