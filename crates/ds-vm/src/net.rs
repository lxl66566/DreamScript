//! The TCP net natives: the network half of the platform surface.
//!
//! The net ops of the platform ABI's fixed table (the table contract in
//! `platform`'s module docs): `net_connect`, `net_send`,
//! `net_recv`, `net_close`, `http_get`, and the listener face
//! `net_listen`/`net_accept`/`net_listener_close`. The fallible ops
//! answer with the uniform two-word tag+payload span
//! — the shared success marker on word 0, the payload on word 1 — and
//! failures carry the class discriminant of the ambient `net_err` case
//! they answer by error class: a bounded wait that
//! expired answers `timeout`, every other failure `refused`, the
//! message `Ref` as payload shaped `"op target: detail"` like the file
//! natives' `io_err` messages. `net_close` and `net_listener_close`
//! alone answer the inert unit word (the idempotent close seams the
//! handle deinits pair with, `close_raw`'s contract).
//!
//! # Connection handles and lifetime
//!
//! A connection is VM state, not a heap object: the native calling
//! convention hands a body exactly two host references (the object store,
//! the output sink), so the connection table rides the sink the VM owns
//! ([`crate::native::OutputSink`], the store is per-fiber state) — the
//! same lifetime rule as the heap, and the sink's drop closes every
//! socket still open when the VM goes away.
//! `net_connect` answers an `i64` handle; handles are monotonic and never
//! reused, so a closed (or never-opened) id names no connection and every
//! later native on it answers `net_err` "unknown connection". `net_close`
//! removes the table entry and closes the socket; a connection left open
//! stays open until the VM drops. The close is total and idempotent: an
//! unknown handle is a no-op, not an error. The stdlib's `net_handle`
//! deinit pairs with that contract — the value's scope exit fires the
//! deinit on the id word even after the owner closed the connection
//! explicitly, and a second close must not turn a clean scope exit into a
//! halt.
//!
//! # Blocking discipline
//!
//! The natives block (the VM is single-fiber at this tier), but every
//! blocking wait is bounded: `net_connect` connects with a timeout, and
//! each socket's reads and writes carry read/write timeouts, all the same
//! configured duration ([`NET_TIMEOUT`], 3s by default, [`Vm::set_net_timeout`]).
//! A hostile host cannot hang the interpreter: a socket that never
//! answers yields `net_err` "timed out after ...".
//!
//! # Parking discipline
//!
//! Installing a reactor on the VM ([`crate::Vm::install_reactor`]) flips
//! the registered-connection surface to non-blocking and the data
//! natives to parking: `net_recv`/`net_send` attempt the op once
//! non-blocking and, when it would block, answer a park request the
//! dispatcher registers with the scheduler — the fiber parks, the drive
//! runs the next ready fiber, and the reactor's readiness (or the same
//! configured timeout, restated as the wait's deadline) completes the op
//! and delivers its answer at the parked site. `net_accept` parks the
//! same way on the listener's own readability (a pending connection is
//! readable), the wake performing the accept and registering the
//! accepted stream like a connecting one — but its park carries no
//! deadline: the signal is the quiet
//! accept's backstop, so the host's registered shutdown interest (or
//! the blocking twin's stop flag below) ends a quiet wait, every other
//! op's deadline discipline unchanged. Blocking stays the
//! embedder's platform choice: no reactor installed, the natives below
//! block exactly as before. `net_connect` and the plain `http_get`
//! exchange park too: std cannot open a
//! non-connecting socket, so the parked connect rides the raw OS
//! socket — libc's own `socket`/`fcntl(O_NONBLOCK)`/`connect` and
//! completion probe over no socket library, the epoll driver's
//! OS-own-symbols precedent — the writability interest parked on, the
//! wake probing completion and registering the stream like any other
//! handle. The plain fetch rides the same staged machine (connect,
//! request drain, response reads to the close), a step per wake, and
//! the https exchange rides the tls tier's parked twin — the rustls
//! session itself driven a step per readiness wake over the same
//! staged connect (its wants-write/wants-read naming the interest each
//! step watches); a non-Linux host keeps the blocking twins under any
//! reactor until its driver round owns the raw seam. The staged
//! connect's first step is the hostname resolution: under the reactor an IP-literal host connects
//! inline exactly the blocking twin's own resolution, a name resolves on the
//! connection table's resolver helper thread — std's own
//! `to_socket_addrs`, its completion posting the eventfd the reactor
//! watches ([`crate::resolver`]) — and the wake starts the connect
//! stage over the resolved addresses, a failed lookup answering its
//! own `net_err`; the resolution's parked wait keeps the configured
//! net-timeout bound like every other, and a reactor-less embedder or
//! non-Linux host keeps the inline blocking resolution.
//!
//! # Caps
//!
//! One `net_recv` reads at most `max_bytes` bytes, clamped to the 1 MiB
//! [`NET_CAP`] per call (a larger request streams across calls), and
//! `http_get` reads at most 1 MiB of response, answering `net_err` past
//! the cap.
//!
//! # The TLS tier
//!
//! `http_get` speaks HTTP/1.0 over a plain `std::net::TcpStream` — no
//! async runtime. The tls capability is a
//! host-level selection: where the platform provides it, the `http_get`
//! slot carries the routing body of [`crate::tls`] (an `https://` URL
//! rides rustls) and the capability's own `https_get` op is served;
//! where the host omits it, the plain body below stays and an `https://`
//! URL answers `net_err` naming the unprovided capability. The response
//! body (after the `\r\n\r\n` header terminator) is the success payload;
//! the status line is not inspected, and only close-delimited HTTP/1.0
//! responses decode (no chunked transfer).
//!
//! The server half rides the same capability: the
//! host or test pins the rustls server identity
//! ([`crate::Vm::set_tls_server_config`], no default), and
//! `net_accept_tls` upgrades the accepted connection over it — the
//! staged handshake parking a step per wake under the reactor (the
//! linux driver's own, the blocking twin anywhere else), the
//! completion registering the stream with its session so the
//! connection's `net_send`/`net_recv` ride the tls records.

use std::{
    io::{self, ErrorKind, Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::{
    native::{
        NativeResult, OutputSink, PLATFORM_IO_OK, PLATFORM_NET_ADDR_IN_USE, PLATFORM_NET_OTHER,
        PLATFORM_NET_REFUSED, PLATFORM_NET_RESET, PLATFORM_NET_TIMEOUT, i64_arg, ref_arg,
    },
    object::Heaps,
    value::Value,
    vm::Trap,
};

mod conn;
mod raw;

pub(crate) use conn::Connections;
pub(crate) use raw::stream_handle;

/// The default bound on every blocking net wait.
pub const NET_TIMEOUT: Duration = Duration::from_secs(3);

/// The per-call byte cap of `net_recv` and of an `http_get` response.
pub const NET_CAP: usize = 1024 * 1024;

/// The default HTTP port when a URL names none.
const HTTP_PORT: u16 = 80;

/// The read size of the `http_get` response loop (both the blocking
/// exchange and the parked machines' plaintext drains).
pub(crate) const HTTP_CHUNK: usize = 8192;

/// The poll interval of the blocking accept's bounded wait (std's
/// listener carries no OS accept timeout, so the non-blocking socket
/// polls to its deadline).
const ACCEPT_POLL: Duration = Duration::from_millis(1);

/// One non-blocking attempt's outcome: the completed (or failed) op, or
/// the would-block the caller parks on.
#[derive(Debug)]
pub(crate) enum NetAttempt<T> {
    /// The op ran: its result, success or failure.
    Done(Result<T, io::Error>),
    /// The op would block — the caller parks (the reactor owns the wait).
    Park,
}

/// One started connect's outcome: the attempt that ran to an immediate
/// completion (loopback often connects at once), or the row that stands
/// in progress — the park's registration key.
#[derive(Debug)]
pub(crate) enum NetConnectStart {
    /// The connect ran: the registered handle on success, the failure
    /// otherwise.
    Done(Result<i64, io::Error>),
    /// The connect stands in progress: the pending row's id.
    Pending(i64),
}

/// One parked connect's wake outcome (the staged machine's own, the
/// fetch machines' connect stage the same): the direction the row's
/// current stage still watches (the wait stands, its interest
/// re-aimed and its bound re-armed), or the machine's end — the
/// registered handle on success, the failure the class mapping reads
/// its class off.
#[derive(Debug)]
pub(crate) enum ConnectWake {
    /// The row's stage still waits: `watch` names the direction the
    /// next wake watches (the resolve stage's eventfd readability,
    /// the connect stage's writability).
    Pending { watch: crate::reactor::Direction },
    /// The connect answered: the registered handle on success, the
    /// failure otherwise.
    Done(Result<i64, io::Error>),
}

/// One parked plain-http exchange's wake outcome: the direction the
/// next step watches (the wait stands, its interest re-aimed and its
/// bound re-armed), or the finished exchange — the body, or the
/// failure the class mapping reads its class off.
#[derive(Debug)]
pub(crate) enum FetchWake {
    /// The machine advanced as far as the readiness took it; `watch`
    /// names the direction the next step waits on.
    Pending { watch: crate::reactor::Direction },
    /// The exchange answered: the body after the header terminator, or
    /// the failure (the transport closed either way).
    Done(Result<String, io::Error>),
}

/// One parked accept-tls machine's wake outcome: the direction the
/// next step watches (the staged handshake [`crate::tls`]'s own
/// discipline — the accept's readable door first, the session's wants
/// after), or the machine's end — the accepted connection's registered
/// handle on success, the failure the class mapping reads its class
/// off (the accepted stream closing with the row either way).
#[derive(Debug)]
pub(crate) enum AcceptWake {
    /// The machine advanced as far as the readiness took it; `watch`
    /// names the direction the next step waits on.
    Pending { watch: crate::reactor::Direction },
    /// The handshake answered: the registered connection on success,
    /// the failure otherwise.
    Done(Result<i64, io::Error>),
}

/// One parked plain-http exchange's machine: the staged connect (the
/// same row [`Connections::start_connect`] answers), the request bytes
/// still to write, and the response bytes read so far. The transport
/// is the machine's own — a connection id no binding owns, closed when
/// the exchange answers and closed by the table's drop if the VM goes
/// away first.
#[derive(Debug)]
struct Fetch {
    /// The transport: the pending connect's row first, the registered
    /// connection once connected.
    conn: i64,
    /// Whether the connect completed and the transport registered.
    connected: bool,
    /// The request bytes still to write.
    out: Vec<u8>,
    /// The response bytes read so far.
    seen: Vec<u8>,
}

/// The server half's listener table: the sockets `net_listen` binds,
/// keyed by the monotonic handle ids it hands out. The same lifetime
/// rule as the connection table (see the module docs): VM state riding
/// the output sink, the sink's drop closing every listener still open
/// when the VM goes away. An accepted stream registers into the
/// connection table like a connecting one, so the accepted connection
/// owns the `net_handle` face in full — send, recv, and the closing
/// deinit.
#[derive(Debug)]
pub(crate) struct Listeners {
    bound: Vec<(i64, TcpListener)>,
    next_id: i64,
    timeout: Duration,
    /// Whether the parking discipline owns `net_accept`: a reactor
    /// installed on the VM flips this, and a bound listener answers
    /// would-block to the thread — never a block.
    parking: bool,
    /// The host's blocking-drive stop flag: a host without a reactor keeps the
    /// blocking rows, so its own signal helper flips this flag and the
    /// bounded poll observes it between rounds. A host that installs
    /// none keeps today's behavior — the flag stands false forever.
    stop: Arc<AtomicBool>,
}

impl Default for Listeners {
    fn default() -> Self {
        Self {
            bound: Vec::new(),
            next_id: 0,
            timeout: NET_TIMEOUT,
            parking: false,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Listeners {
    /// Bounds the blocking accept wait from now on (the connection
    /// table's bound and this one set together, one `set_net_timeout`).
    pub(crate) fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Installs the host's blocking-drive stop flag
    /// ([`crate::Vm::set_shutdown_flag`]).
    pub(crate) fn set_stop_flag(&mut self, stop: Arc<AtomicBool>) {
        self.stop = stop;
    }

    /// The configured accept bound.
    #[must_use]
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Flips `net_accept` to the parking discipline (a reactor was
    /// installed on the VM): bound listeners go non-blocking now and
    /// at every later bind.
    pub(crate) fn enable_parking(&mut self) {
        self.parking = true;
        for (_, listener) in &mut self.bound {
            let _ = listener.set_nonblocking(true);
        }
    }

    /// Whether `net_accept` parks (a reactor is installed).
    pub(crate) fn parking(&self) -> bool {
        self.parking
    }

    /// Binds a listener on `host:port` without registering it: the
    /// first address that resolves and binds wins, the last bind error
    /// the report.
    pub(crate) fn open(host: &str, port: u16) -> io::Result<TcpListener> {
        let mut last = io::Error::new(ErrorKind::NotFound, "no address resolved");
        for addr in (host, port).to_socket_addrs()? {
            match TcpListener::bind(addr) {
                Ok(listener) => return Ok(listener),
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    /// Registers `listener` and answers its handle. Used by
    /// `net_listen`.
    pub(crate) fn register(&mut self, listener: TcpListener) -> i64 {
        if self.parking {
            // the reactor owns readiness: a bound listener answers
            // would-block to the thread, never a block
            let _ = listener.set_nonblocking(true);
        }
        let id = self.next_id;
        self.next_id = id + 1;
        self.bound.push((id, listener));
        id
    }

    /// One non-blocking accept attempt on handle `lid`, `Park` when no
    /// connection stands ready. The parked twin of [`Listeners::accept`].
    pub(crate) fn try_accept(&mut self, lid: i64) -> NetAttempt<TcpStream> {
        let timeout = self.timeout;
        let Ok(listener) = self.listener(lid) else {
            return NetAttempt::Done(Err(unknown_listener(lid)));
        };
        let _ = listener.set_nonblocking(true);
        match listener.accept() {
            Ok((stream, _)) => NetAttempt::Done(Ok(stream)),
            Err(err) if err.kind() == ErrorKind::WouldBlock => NetAttempt::Park,
            Err(err) => NetAttempt::Done(Err(normalize(err, timeout))),
        }
    }

    /// The bounded blocking accept on handle `lid`: std's listener carries no OS accept timeout, so
    /// the non-blocking socket polls to the configured deadline — the same bound every blocking net
    /// wait keeps, polled rather than waited because that is the only portable shape. The host's
    /// stop flag answers between rounds: a stopped drive answers the `Interrupted` marker the
    /// accept rows map to the shutdown trap, never a net failure span.
    pub(crate) fn accept(&mut self, lid: i64) -> io::Result<TcpStream> {
        let timeout = self.timeout;
        let stopped = self.stop.clone();
        let listener = self.listener(lid)?;
        let _ = listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if stopped.load(Ordering::Relaxed) {
                return Err(io::Error::new(ErrorKind::Interrupted, "host shutdown"));
            }
            match listener.accept() {
                Ok((stream, _)) => return Ok(stream),
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            ErrorKind::TimedOut,
                            format!("timed out after {timeout:?}"),
                        ));
                    }
                    std::thread::sleep(ACCEPT_POLL);
                },
                Err(err) => return Err(normalize(err, timeout)),
            }
        }
    }

    /// Closes handle `lid` and removes it from the table; later uses of
    /// the handle answer "unknown listener". Total and idempotent: an
    /// unknown handle is a no-op (the deinit pairing's contract).
    pub(crate) fn close(&mut self, lid: i64) {
        self.bound.retain(|(id, _)| *id != lid);
    }

    /// The OS-level handle of one bound listener, the reactor's
    /// registration key; `None` for an unknown handle (the caller's
    /// park then rides its deadline alone).
    pub(crate) fn raw_handle(&mut self, lid: i64) -> Option<crate::reactor::IoHandle> {
        let listener = self.listener(lid).ok()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            Some(crate::reactor::IoHandle(
                u64::try_from(listener.as_raw_fd()).ok()?,
            ))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            Some(crate::reactor::IoHandle(
                u64::try_from(listener.as_raw_socket()).ok()?,
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    /// The listener of handle `lid`.
    fn listener(&mut self, lid: i64) -> io::Result<&mut TcpListener> {
        self.bound
            .iter_mut()
            .find(|(id, _)| *id == lid)
            .map(|(_, listener)| listener)
            .ok_or_else(|| unknown_listener(lid))
    }

    /// The bound listener's local address rendered "host:port" — the
    /// port-0 bind's report. Total: an unknown handle answers the
    /// empty string (the face's member never reaches it with a dead
    /// binding, the idempotent-close precedent's inert answer).
    #[must_use]
    pub(crate) fn local_addr(&mut self, lid: i64) -> String {
        match self.listener(lid) {
            Ok(listener) => listener
                .local_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_default(),
            Err(_) => String::new(),
        }
    }
}

/// The "unknown connection" error, shared by every handle lookup.
fn unknown(conn: i64) -> io::Error {
    io::Error::other(format!("unknown connection {conn}"))
}

/// The "unknown listener" error, shared by every listener lookup.
fn unknown_listener(lid: i64) -> io::Error {
    io::Error::other(format!("unknown listener {lid}"))
}

/// The "unknown fetch" error: a parked exchange's row is gone (its
/// wait already ended through another door).
fn unknown_fetch(fetch: i64) -> io::Error {
    io::Error::other(format!("unknown fetch {fetch}"))
}

/// The "unknown accept" error: a parked accept-tls machine's row is
/// gone (its wait already ended through another door).
fn unknown_accept(accept: i64) -> io::Error {
    io::Error::other(format!("unknown accept {accept}"))
}

/// Renders a timed-out wait as its own detail instead of the OS's
/// "resource temporarily unavailable", so the `net_err` message says
/// what happened. The `TimedOut` kind survives the rewrite, so the
/// class mapping reads the class off the rendered error too.
fn normalize(err: io::Error, timeout: Duration) -> io::Error {
    match err.kind() {
        // read/write timeouts surface as EAGAIN on Unix, EWOULDBLOCK on
        // Windows; connect timeouts as ETIMEDOUT
        ErrorKind::TimedOut | ErrorKind::WouldBlock => {
            io::Error::new(ErrorKind::TimedOut, format!("timed out after {timeout:?}"))
        },
        _ => err,
    }
}

/// The class of one net failure: the `net_err` member the platform
/// discriminant answers by error class — every kind the classes
/// name lands on its tag, the rest folding into the `net_other` floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NetClass {
    /// A bounded wait expired (the `net_err` member `timeout`).
    Timeout,
    /// The peer refused the connection (the `net_err` member `refused`).
    Refused,
    /// The local address is already bound (`addr_in_use`).
    AddrInUse,
    /// A connection torn down mid-flight, reset or aborted (`reset`).
    Reset,
    /// Every failure no net class names portably (the `net_other`
    /// floor, alone carrying the OS message).
    Other,
}

impl NetClass {
    /// The platform discriminant the class answers.
    #[must_use]
    pub(crate) fn tag(self) -> u8 {
        match self {
            Self::Timeout => PLATFORM_NET_TIMEOUT,
            Self::Refused => PLATFORM_NET_REFUSED,
            Self::AddrInUse => PLATFORM_NET_ADDR_IN_USE,
            Self::Reset => PLATFORM_NET_RESET,
            Self::Other => PLATFORM_NET_OTHER,
        }
    }

    /// The class of one rendered failure: read, write, and connect
    /// timeouts are `timeout`, a refused connection `refused`, a bound
    /// address `addr_in_use`, a mid-flight teardown `reset`, and every
    /// other failure the `net_other` floor.
    #[must_use]
    pub(crate) fn of(err: &io::Error) -> Self {
        match err.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => Self::Timeout,
            ErrorKind::ConnectionRefused => Self::Refused,
            ErrorKind::AddrInUse => Self::AddrInUse,
            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => Self::Reset,
            _ => Self::Other,
        }
    }
}

/// The net failure span: the class's discriminant with the message
/// `Ref` as the payload word.
pub(crate) fn net_err(
    heaps: &mut Heaps,
    class: NetClass,
    op: &str,
    target: &str,
    detail: &str,
) -> NativeResult {
    NativeResult::Tag {
        tag: class.tag(),
        payload: heaps.new_str(format!("{op} {target}: {detail}")),
    }
}

/// The `ok` span: the success value as the payload word.
pub(crate) fn ok(payload: Value) -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload,
    }
}

/// The URL schemes the fetch bodies route on.
pub(crate) enum Scheme {
    /// Plain HTTP.
    Http,
    /// The tls capability's tier.
    Https,
    /// Neither.
    Other,
}

/// Splits `url` into its scheme and the rest (authority and path).
pub(crate) fn scheme_of(url: &str) -> (Scheme, &str) {
    if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else {
        (Scheme::Other, url)
    }
}

/// Splits a schemeless URL rest into host, port, authority, and path —
/// the pieces both the plain and the TLS exchange address.
pub(crate) fn split_target(rest: &str) -> Result<(String, u16, String, String), String> {
    let (authority, path) = match rest.find('/') {
        Some(at) => (rest[..at].to_owned(), rest[at..].to_owned()),
        None => (rest.to_owned(), "/".to_owned()),
    };
    let host = authority.split(':').next().unwrap_or_default().to_owned();
    let port = match authority.split_once(':') {
        // IPv6-literal authorities are out of scope at this tier
        Some((_, port)) => port
            .parse::<u16>()
            .map_err(|_| "invalid port in the URL".to_owned())?,
        None => HTTP_PORT,
    };
    Ok((host, port, authority, path))
}

/// The HTTP/1.0 request both exchanges write.
pub(crate) fn request(authority: &str, path: &str) -> String {
    format!("GET {path} HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
}

/// The shared request/response exchange: write the request, read to the
/// server's close-delimited end or past the cap, answer the body after
/// the header terminator. Generic over the stream so the TLS tier rides
/// the same discipline (its rustls stream honors the socket timeouts
/// the connection table installs). The failure channel keeps the
/// `io::Error` whole, so the caller reads the error class off it.
pub(crate) fn exchange<S>(
    stream: &mut S,
    request: &str,
    timeout: Duration,
) -> Result<String, io::Error>
where
    S: Read + Write,
{
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|err| normalize(err, timeout))?;
    let mut response = Vec::new();
    loop {
        let mut chunk = [0; HTTP_CHUNK];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&chunk[..n]);
                if response.len() > NET_CAP {
                    return Err(io::Error::other("response exceeds the 1 MiB cap"));
                }
            },
            Err(err) => return Err(normalize(err, timeout)),
        }
    }
    finish_response(&response)
}

/// The response's decode, the blocking exchange's and the parked
/// machine's shared finish: the body after the header terminator,
/// refused without the terminator or past the cap.
pub(crate) fn finish_response(seen: &[u8]) -> Result<String, io::Error> {
    let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err(io::Error::other("malformed response: no header terminator"));
    };
    String::from_utf8(seen[at + 4..].to_owned())
        .map_err(|_| io::Error::other("invalid UTF-8 in the response body"))
}

/// `net_connect(host: string, port: int) -> int | net_err`: opens a
/// timeout-bounded TCP connection and answers its handle.
pub(crate) fn net_connect_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let host = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    let port = i64_arg(args, 1)?;
    let Some(port) = u16::try_from(port).ok() else {
        return Ok(net_err(
            heaps,
            NetClass::Refused,
            "net_connect",
            &format!("{host}:{port}"),
            "port out of range",
        ));
    };
    let target = format!("{host}:{port}");
    // the parked twin: a reactor owns
    // readiness, so the connect rides the raw OS socket —
    // non-blocking, EINPROGRESS parked on the writability the
    // completion reports, SO_ERROR the wake's probe. The linux
    // driver's own; a non-linux host keeps the blocking twin under any
    // reactor until its driver round owns the raw seam.
    #[cfg(target_os = "linux")]
    {
        if out.net().parking() {
            return match out.net().start_connect(&host, port) {
                NetConnectStart::Done(Ok(conn)) => Ok(ok(Value::I64(conn))),
                NetConnectStart::Done(Err(err)) => Ok(net_err(
                    heaps,
                    NetClass::of(&err),
                    "net_connect",
                    &target,
                    &err.to_string(),
                )),
                NetConnectStart::Pending(conn) => {
                    let deadline = std::time::Instant::now() + out.net().timeout();
                    Ok(NativeResult::Park(crate::sched::WorldWait::connect(
                        conn, target, deadline,
                    )))
                },
            };
        }
    }
    match out.net().open(&host, port) {
        Ok(stream) => {
            let id = out.net().register(stream);
            Ok(ok(Value::I64(id)))
        },
        Err(err) => Ok(net_err(
            heaps,
            NetClass::of(&err),
            "net_connect",
            &target,
            &err.to_string(),
        )),
    }
}

/// `net_send(conn: int, data: string) -> unit | net_err`.
pub(crate) fn net_send_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let conn = i64_arg(args, 0)?;
    let data = heaps.str_at(ref_arg(args, 1)?)?.to_owned();
    if out.net().parking() {
        // the parking discipline: drain what the buffer takes now, park
        // on the remainder (the staged bytes copied once; the frame's
        // own word keeps its count where it stood)
        let mut pending = data.into_bytes();
        return match out.net().try_send(conn, &mut pending) {
            NetAttempt::Done(Ok(())) => Ok(ok(Value::I64(0))),
            NetAttempt::Done(Err(err)) => Ok(net_err(
                heaps,
                NetClass::of(&err),
                "net_send",
                &conn.to_string(),
                &err.to_string(),
            )),
            NetAttempt::Park => {
                let deadline = std::time::Instant::now() + out.net().timeout();
                Ok(NativeResult::Park(crate::sched::WorldWait::send(
                    conn, pending, deadline,
                )))
            },
        };
    }
    match out.net().send(conn, data.as_bytes()) {
        // the payload word is the inert unit word, like write_file's
        Ok(()) => Ok(ok(Value::I64(0))),
        Err(err) => Ok(net_err(
            heaps,
            NetClass::of(&err),
            "net_send",
            &conn.to_string(),
            &err.to_string(),
        )),
    }
}

/// `net_recv(conn: int, max_bytes: int) -> string | net_err`: one read of
/// at most `max_bytes` bytes (clamped to [`NET_CAP`]); the empty string
/// is an orderly close.
pub(crate) fn net_recv_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let conn = i64_arg(args, 0)?;
    let max = i64_arg(args, 1)?;
    if max < 1 {
        return Ok(net_err(
            heaps,
            NetClass::Refused,
            "net_recv",
            &conn.to_string(),
            "max_bytes must be positive",
        ));
    }
    let max = usize::try_from(max).unwrap_or(NET_CAP).min(NET_CAP);
    if out.net().parking() {
        // the parking discipline: one non-blocking attempt, the
        // would-block answered as the park request (the scheduler owns
        // the wait, the deadline the same configured bound)
        return match out.net().try_recv(conn, max) {
            NetAttempt::Done(Ok(bytes)) => Ok(recv_answer(heaps, conn, bytes)),
            NetAttempt::Done(Err(err)) => Ok(net_err(
                heaps,
                NetClass::of(&err),
                "net_recv",
                &conn.to_string(),
                &err.to_string(),
            )),
            NetAttempt::Park => {
                let deadline = std::time::Instant::now() + out.net().timeout();
                Ok(NativeResult::Park(crate::sched::WorldWait::recv(
                    conn, max, deadline,
                )))
            },
        };
    }
    let bytes = match out.net().recv(conn, max) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Ok(net_err(
                heaps,
                NetClass::of(&err),
                "net_recv",
                &conn.to_string(),
                &err.to_string(),
            ))
        },
    };
    Ok(recv_answer(heaps, conn, bytes))
}

/// `net_recv`'s answer shape, shared by the blocking path and the
/// parked wake's completion: a string native answers a string, so
/// mid-code-point truncation and binary protocols surface as errors,
/// not as silently mangled text.
pub(crate) fn recv_answer(heaps: &mut Heaps, conn: i64, bytes: Vec<u8>) -> NativeResult {
    match String::from_utf8(bytes) {
        Ok(text) => ok(heaps.new_str(text)),
        Err(_) => net_err(
            heaps,
            NetClass::Refused,
            "net_recv",
            &conn.to_string(),
            "invalid UTF-8 in the received bytes",
        ),
    }
}

/// `net_close(conn: int) -> ()`: the raw close seam the stdlib's
/// ds-source `close_conn` handler and the `net_handle` struct's deinit
/// call — removes the handle, closing the socket. Total and idempotent:
/// an unknown handle is a no-op (mirroring `close_raw`, so a deinit
/// firing on an already-closed handle stays clean). The close notes
/// the handle for the wake door: a parked wait on it wakes now, its deadline
/// never the line.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn net_close_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(conn) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.net().close(conn);
        out.note_closed(crate::sched::CloseSeam::NetConn, conn);
    }
    // the inert unit word
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `net_listen(host: string, port: int) -> int | net_err`: binds a
/// listener on `host:port` and answers its handle. The bound failure
/// classes land on their tags (`addr_in_use` among them); the ambient
/// declares the total `net_listener` spelling over this span, so a
/// failed bind prints the net error line and halts (`net_connect`'s
/// own printing-halt pairing).
pub(crate) fn net_listen_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let host = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    let port = i64_arg(args, 1)?;
    let Some(port) = u16::try_from(port).ok() else {
        return Ok(net_err(
            heaps,
            NetClass::Refused,
            "net_listen",
            &format!("{host}:{port}"),
            "port out of range",
        ));
    };
    let target = format!("{host}:{port}");
    match Listeners::open(&host, port) {
        Ok(listener) => Ok(ok(Value::I64(out.listeners().register(listener)))),
        Err(err) => Ok(net_err(
            heaps,
            NetClass::of(&err),
            "net_listen",
            &target,
            &err.to_string(),
        )),
    }
}

/// `net_accept(lid: int) -> int | net_err`: takes one pending
/// connection off the bound listener, registering the accepted stream
/// into the connection table so it owns the `net_handle` face in full.
/// Under an installed reactor the accept parks on the listener's
/// readability (the wake performing the accept) with no deadline — the
/// signal is the quiet accept's backstop, every other op's deadline
/// discipline unchanged; otherwise
/// the wait blocks the thread, bounded by the configured net timeout,
/// and the host's stop flag answers the shutdown trap between the
/// poll's rounds.
pub(crate) fn net_accept_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let lid = i64_arg(args, 0)?;
    let target = lid.to_string();
    if out.listeners().parking() {
        return match out.listeners().try_accept(lid) {
            NetAttempt::Done(Ok(stream)) => Ok(ok(Value::I64(out.net().register(stream)))),
            NetAttempt::Done(Err(err)) => Ok(net_err(
                heaps,
                NetClass::of(&err),
                "net_accept",
                &target,
                &err.to_string(),
            )),
            NetAttempt::Park => Ok(NativeResult::Park(crate::sched::WorldWait::accept(lid))),
        };
    }
    match out.listeners().accept(lid) {
        Ok(stream) => Ok(ok(Value::I64(out.net().register(stream)))),
        Err(err) if err.kind() == ErrorKind::Interrupted => Err(Trap::Shutdown),
        Err(err) => Ok(net_err(
            heaps,
            NetClass::of(&err),
            "net_accept",
            &target,
            &err.to_string(),
        )),
    }
}

/// `net_listener_close(lid: int) -> ()`: the raw idempotent-close seam
/// the `net_listener` deinit pairs with, `net_close`'s own contract —
/// the close noting the listener for the wake door, so a parked
/// accept on it ends now instead of at its (unbounded) park.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn net_listener_close_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(lid) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.listeners().close(lid);
        out.note_closed(crate::sched::CloseSeam::NetListener, lid);
    }
    // the inert unit word
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `net_local_addr(lid: int) -> string`: the bound listener's local
/// address ("host:port") — the raw seam the ds-source `local_addr`
/// handler reads the id word out to call, the port-0 bind's report
/// (the face's one address op).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn net_local_addr_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let lid = i64_arg(args, 0)?;
    let addr = out.listeners().local_addr(lid);
    Ok(NativeResult::Word(heaps.new_str(addr)))
}

/// `http_get(url: string) -> string | net_err`: the plain body a
/// tls-omitting host serves — HTTP/1.0 over TCP only, an `https://` URL
/// answered by the unprovided-capability `net_err`. See the module docs
/// for the tier decision (body-only payload, close-delimited responses)
/// and [`crate::tls`] for the routing body a tls-providing host serves.
pub(crate) fn http_get_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let url = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    // the plain exchange rides the parked machine under a reactor (the
    // staged connect `net_connect` parks on); https keeps the tls
    // tier's own body
    #[cfg(target_os = "linux")]
    {
        let (scheme, rest) = scheme_of(&url);
        if out.net().parking() && matches!(scheme, Scheme::Http) {
            return parked_plain_fetch(heaps, out, rest, &url);
        }
    }
    let (scheme, rest) = scheme_of(&url);
    let fetched = match scheme {
        Scheme::Http => plain_fetch(out, rest),
        Scheme::Https => Err(io::Error::other(
            "https refused: the platform does not provide the tls capability",
        )),
        Scheme::Other => Err(io::Error::other("unsupported URL scheme (http:// only)")),
    };
    match fetched {
        Ok(body) => Ok(ok(heaps.new_str(body))),
        Err(err) => Ok(net_err(
            heaps,
            NetClass::of(&err),
            "http_get",
            &url,
            &err.to_string(),
        )),
    }
}

/// The plain exchange a scheme-split URL rest names: open the bounded
/// TCP connection, run the one request/response round, answer the body.
fn plain_fetch(out: &mut OutputSink, rest: &str) -> Result<String, io::Error> {
    let (host, port, authority, path) = split_target(rest).map_err(io::Error::other)?;
    let net = out.net();
    let timeout = net.timeout();
    let mut stream = net.open(&host, port)?;
    exchange(&mut stream, &request(&authority, &path), timeout)
}

/// The parked plain-fetch entry both fetch bodies share (the plain
/// `http_get` and the tls routing body's http leg): the URL's pieces
/// resolve, the staged connect and the request bytes form the exchange
/// machine, and the wakes (or the deadline) answer — an immediate
/// start failure here, everything else through the scheduler.
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn parked_plain_fetch(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    rest: &str,
    url: &str,
) -> Result<NativeResult, Trap> {
    let (host, port, authority, path) = match split_target(rest) {
        Ok(parts) => parts,
        Err(detail) => return Ok(net_err(heaps, NetClass::Other, "http_get", url, &detail)),
    };
    let request = request(&authority, &path);
    match out.net().start_fetch(&host, port, request) {
        Ok(fetch) => {
            let deadline = std::time::Instant::now() + out.net().timeout();
            Ok(NativeResult::Park(crate::sched::WorldWait::fetch(
                fetch,
                url.to_owned(),
                deadline,
            )))
        },
        Err(err) => Ok(net_err(
            heaps,
            NetClass::of(&err),
            "http_get",
            url,
            &err.to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use ds_abi::SlotType;

    use super::*;
    use crate::{fiber::FIBER_ROOT, object::Objects, value::RefId};

    /// The timeout discipline: a never-answering socket yields the
    /// timeout detail, not the OS's EAGAIN rendering, and the class
    /// mapping reads the timeout class off the rendered error.
    #[test]
    fn timeouts_render_as_timed_out() {
        let err = normalize(
            io::Error::from(ErrorKind::WouldBlock),
            Duration::from_millis(250),
        );
        assert_eq!(err.to_string(), "timed out after 250ms");
        assert_eq!(NetClass::of(&err), NetClass::Timeout);
    }

    /// The widened error-class mapping: every kind a class
    /// names lands on its own discriminant, and the kinds no class names
    /// portably fold into the `net_other` floor.
    #[test]
    fn net_classes_map_kinds_and_fold_the_floor() {
        assert_eq!(
            NetClass::of(&io::Error::from(ErrorKind::TimedOut)),
            NetClass::Timeout
        );
        assert_eq!(
            NetClass::of(&io::Error::from(ErrorKind::ConnectionRefused)),
            NetClass::Refused
        );
        assert_eq!(
            NetClass::of(&io::Error::from(ErrorKind::AddrInUse)),
            NetClass::AddrInUse
        );
        assert_eq!(
            NetClass::of(&io::Error::from(ErrorKind::ConnectionReset)),
            NetClass::Reset
        );
        assert_eq!(
            NetClass::of(&io::Error::from(ErrorKind::ConnectionAborted)),
            NetClass::Reset
        );
        // no class names a DNS miss or an unknown handle portably
        assert_eq!(
            NetClass::of(&io::Error::from(ErrorKind::NotFound)),
            NetClass::Other
        );
        assert_eq!(
            NetClass::of(&io::Error::other("unknown connection 7")),
            NetClass::Other
        );
    }

    /// An unknown handle is the same error everywhere a handle is used —
    /// and the close of one stays the idempotent no-op.
    #[test]
    fn unknown_handles_answer_unknown_connection() {
        let mut conns = Connections::default();
        let err = conns.send(7, b"x").expect_err("handle 7 was never opened");
        assert_eq!(err.to_string(), "unknown connection 7");
        conns.close(7);
    }

    /// A wrong-typed argument names its own slot: the trap cites the
    /// offending index's word kind (never another argument's), and a
    /// missing argument answers the trap instead of indexing out of
    /// bounds. No case reaches a socket.
    #[test]
    fn type_mismatches_name_the_offending_slot() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<crate::fiber::Fiber> = Vec::new();
        let mut claims: Vec<crate::claims::ClaimTable> = vec![crate::claims::ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        // net_connect(host, port): the port word is wrong, not the host's
        let host = heaps.current().intern("example.invalid");
        assert_eq!(
            net_connect_fn(&mut heaps, &mut out, &[
                Value::Ref(RefId::new(FIBER_ROOT, host)),
                Value::Bool(true)
            ]),
            Err(Trap::SlotTypeMismatch {
                slot: 1,
                expected: SlotType::I64,
                actual: SlotType::Bool,
            })
        );
        // net_send(conn, data): the data word is wrong, and a missing one
        // answers the same trap instead of panicking on args[1]
        for args in [vec![Value::I64(0), Value::I64(7)], vec![Value::I64(0)]] {
            assert_eq!(
                net_send_fn(&mut heaps, &mut out, &args),
                Err(Trap::SlotTypeMismatch {
                    slot: 1,
                    expected: SlotType::Ref,
                    actual: SlotType::I64,
                })
            );
        }
        // http_get(url): the single argument names itself
        assert_eq!(
            http_get_fn(&mut heaps, &mut out, &[Value::I64(1)]),
            Err(Trap::SlotTypeMismatch {
                slot: 0,
                expected: SlotType::Ref,
                actual: SlotType::I64,
            })
        );
        // net_accept(lid) and net_listen(host, port) name their own slots
        // the same way
        assert_eq!(
            net_accept_fn(&mut heaps, &mut out, &[Value::Ref(RefId::new(
                FIBER_ROOT, host
            ))]),
            Err(Trap::SlotTypeMismatch {
                slot: 0,
                expected: SlotType::I64,
                actual: SlotType::Ref,
            })
        );
        assert_eq!(
            net_listen_fn(&mut heaps, &mut out, &[Value::I64(1), Value::I64(80)]),
            Err(Trap::SlotTypeMismatch {
                slot: 0,
                expected: SlotType::Ref,
                actual: SlotType::I64,
            })
        );
    }

    /// The parked connect's raw machine: a
    /// loopback connect either completes at once or stands in progress
    /// with the wake's SO_ERROR probe finishing it — the registered
    /// handle live either way — and a refused port answers its class
    /// without any park surviving. The retry loop is the test's own
    /// poll: an in-progress connect on loopback completes in
    /// milliseconds, and the machine's own caller (the reactor) polls
    /// the same way.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_raw_connect_machine_registers_through_either_door() {
        let mut conns = Connections::default();
        conns.enable_parking();
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        let addr = listener.local_addr().expect("local address");
        let conn = match conns.start_connect(&addr.ip().to_string(), addr.port()) {
            NetConnectStart::Done(Ok(conn)) => conn,
            NetConnectStart::Pending(conn) => {
                let mut answered = None;
                for _ in 0..500 {
                    match conns.connect_wake(conn) {
                        ConnectWake::Pending { .. } => std::thread::sleep(Duration::from_millis(1)),
                        wake @ ConnectWake::Done(_) => {
                            answered = Some(wake);
                            break;
                        },
                    }
                }
                match answered.expect("the loopback connect completes") {
                    ConnectWake::Done(Ok(done)) => {
                        assert_eq!(done, conn, "the wake registers under the row's id");
                        conn
                    },
                    other => panic!("the parked wake answered wrong: {other:?}"),
                }
            },
            other @ NetConnectStart::Done(_) => {
                panic!("a live listener's connect: {other:?}")
            },
        };
        // the registered handle carries data: the accepted peer reads
        // what the machine's connection wrote
        let mut pending = b"ping".to_vec();
        match conns.try_send(conn, &mut pending) {
            NetAttempt::Done(Ok(())) => {},
            other => panic!("the fresh loopback buffer takes the ping: {other:?}"),
        }
        let (mut peer, _) = listener.accept().expect("the peer accepts");
        let mut got = [0; 4];
        peer.read_exact(&mut got).expect("the peer reads the ping");
        assert_eq!(&got, b"ping");
        conns.close(conn);
        conns.cancel_connect(conn);
        assert!(conns.raw_connect_handle(conn).is_none(), "the row is gone");

        // a refused port: the listener dropped, the machine answers the
        // refused class through whichever door the OS picks
        drop(peer);
        let port = addr.port();
        let mut refused = None;
        match conns.start_connect("127.0.0.1", port) {
            NetConnectStart::Done(Err(err)) => refused = Some(err),
            NetConnectStart::Pending(row) => {
                for _ in 0..500 {
                    match conns.connect_wake(row) {
                        ConnectWake::Pending { .. } => std::thread::sleep(Duration::from_millis(1)),
                        ConnectWake::Done(Err(err)) => {
                            refused = Some(err);
                            break;
                        },
                        ConnectWake::Done(Ok(conn)) => {
                            // a parallel test re-bound the port: not a
                            // refused probe, the live-listener leg above
                            // already covered that door
                            conns.close(conn);
                            break;
                        },
                    }
                }
            },
            NetConnectStart::Done(Ok(conn)) => conns.close(conn),
        }
        if let Some(err) = refused {
            assert_eq!(NetClass::of(&err), NetClass::Refused, "the class: {err}");
        }
    }

    /// The parked DNS twin's machine: a
    /// name answers the pre-minted pending row at once — the drive
    /// thread never blocks in the resolution — the eventfd the row's
    /// interest and its readability the stage's direction; the wake
    /// takes the helper's answer, starts the connect stage over the
    /// resolved addresses, and registers under the very id the parked
    /// caller saw. An IP literal keeps the inline path: no helper
    /// thread ever starts for it. The retry loops are the test's own
    /// poll, the reactor's discipline.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_resolving_connect_parks_on_the_helper_and_wakes_through_it() {
        let mut conns = Connections::default();
        conns.enable_parking();
        // a dual-stack loopback listener: "localhost" resolves to either
        // loopback family (in the resolver's own order), and both reach
        // the same bound socket, so whichever address the machine tries
        // first connects
        let listener = TcpListener::bind(SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)))
            .expect("the dual-stack loopback bind");
        let addr = listener.local_addr().expect("local address");
        let port = addr.port();
        // the literal keeps the inline path: the machine starts at its
        // connect stage, no resolver row, no helper thread (either door
        // of the landed machine's own — a loopback connect connects at
        // once or stands in progress)
        let literal = match conns.start_connect("127.0.0.1", port) {
            NetConnectStart::Done(Ok(conn)) => conn,
            NetConnectStart::Pending(row) => {
                let mut registered = None;
                for _ in 0..500 {
                    match conns.connect_wake(row) {
                        ConnectWake::Pending { .. } => std::thread::sleep(Duration::from_millis(1)),
                        ConnectWake::Done(Ok(conn)) => {
                            registered = Some(conn);
                            break;
                        },
                        ConnectWake::Done(Err(err)) => panic!("a live listener's literal: {err}"),
                    }
                }
                registered.expect("the loopback literal connect completes")
            },
            other @ NetConnectStart::Done(_) => panic!("a literal connect: {other:?}"),
        };
        assert!(
            conns.resolver.is_none(),
            "the inline literal path starts no helper thread"
        );
        conns.close(literal);
        // the name parks at its resolve stage: the row stands before
        // any answer, the eventfd the interest the wake reads
        let conn = match conns.start_connect("localhost", port) {
            NetConnectStart::Pending(conn) => conn,
            other @ NetConnectStart::Done(_) => panic!("a resolving name parks: {other:?}"),
        };
        assert!(conns.resolver.is_some(), "the helper thread stands");
        assert_eq!(
            conns.connect_interest(conn),
            crate::reactor::Direction::Read,
            "the resolve stage watches the eventfd's readability"
        );
        assert!(
            conns.raw_connect_handle(conn).is_some(),
            "the posted eventfd is the registration key"
        );
        let mut registered = None;
        for _ in 0..1000 {
            match conns.connect_wake(conn) {
                ConnectWake::Pending { .. } => std::thread::sleep(Duration::from_millis(1)),
                ConnectWake::Done(Ok(done)) => {
                    registered = Some(done);
                    break;
                },
                ConnectWake::Done(Err(err)) => panic!("localhost resolves: {err}"),
            }
        }
        assert_eq!(registered.expect("the resolved connect lands"), conn);
        // the registered handle carries data: the accepted peer reads
        // what the machine's connection wrote
        let mut pending = b"ping".to_vec();
        match conns.try_send(conn, &mut pending) {
            NetAttempt::Done(Ok(())) => {},
            other => panic!("the fresh loopback buffer takes the ping: {other:?}"),
        }
        // the literal's already closed connection sits first in the
        // backlog, so the accepts drain to the live one
        // deterministically
        let mut got = [0; 4];
        let peer = loop {
            let (mut sock, _) = listener.accept().expect("the peer accepts");
            // the closed literal's stale backlog entry answers EOF
            // and drains; the live connection reads the ping
            if sock.read_exact(&mut got).is_ok_and(|()| &got == b"ping") {
                break sock;
            }
        };
        assert_eq!(&got, b"ping");
        drop(peer);
        // a canceled resolve row releases clean: the helper's late
        // posting lands on a dropped row and a live fd, the interest
        // the cancel already deregistered elsewhere
        let canceled = match conns.start_connect("localhost", port) {
            NetConnectStart::Pending(conn) => conn,
            other @ NetConnectStart::Done(_) => {
                panic!("a second resolving name parks: {other:?}")
            },
        };
        conns.cancel_connect(canceled);
        assert!(
            conns.raw_connect_handle(canceled).is_none(),
            "the canceled row is gone"
        );
        match conns.connect_wake(canceled) {
            ConnectWake::Done(Err(err)) => {
                assert_eq!(
                    err.to_string(),
                    format!("unknown connection {canceled}"),
                    "a canceled row answers the unknown-handle floor"
                );
            },
            other => panic!("the canceled row answered: {other:?}"),
        }
        // the drop joins the helper: a leaked worker would hang it
        drop(conns);
    }

    /// A failed resolution answers its own error (NXDOMAIN, no
    /// addresses): the helper posts the lookup's failure, the wake
    /// answering the `net_other` floor the class mapping reads. The
    /// bounded poll answers an environment whose lookup stalls past
    /// the bound loudly, never hanging the drive.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_failed_resolution_answers_its_own_error() {
        let mut conns = Connections::default();
        conns.enable_parking();
        let conn = match conns.start_connect("example.invalid", 80) {
            NetConnectStart::Pending(conn) => conn,
            other @ NetConnectStart::Done(_) => panic!("an unresolvable name parks: {other:?}"),
        };
        let mut failed = None;
        for _ in 0..1000 {
            match conns.connect_wake(conn) {
                ConnectWake::Pending { .. } => std::thread::sleep(Duration::from_millis(10)),
                ConnectWake::Done(Err(err)) => {
                    failed = Some(err);
                    break;
                },
                ConnectWake::Done(Ok(conn)) => {
                    panic!("example.invalid resolves nowhere: {conn}")
                },
            }
        }
        let err = failed.expect("the lookup fails");
        assert_eq!(NetClass::of(&err), NetClass::Other, "the class: {err}");
        assert_eq!(
            conns.connect_interest(conn),
            crate::reactor::Direction::Write,
            "the failed row is gone, the connect's own direction the default"
        );
    }

    /// The local-address op: a port-0 bind reports its bound address
    /// ("host:port", a real port), and the unknown handle answers the
    /// inert empty string (the total seam's answer).
    #[test]
    fn the_listener_reports_its_local_address() {
        let mut listeners = Listeners::default();
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        let bound = listener.local_addr().expect("local address");
        let lid = listeners.register(listener);
        let addr = listeners.local_addr(lid);
        let port = addr.rsplit(':').next().unwrap_or_default();
        assert_eq!(
            port.parse::<u16>().expect("the rendered port"),
            bound.port(),
            "the port-0 bind reports its bound port: {addr}"
        );
        assert!(addr.starts_with("127.0.0.1:"), "the rendering: {addr}");
        assert_eq!(listeners.local_addr(9), "", "the unknown handle is inert");
    }

    /// The server half's spans: a port out of range answers the refused
    /// class before any socket opens, a bind against a held port answers
    /// the addr_in_use class, and an unknown listener is the same floor
    /// every unknown handle is (the close of one the idempotent no-op).
    #[test]
    fn the_listener_table_names_its_classes() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<crate::fiber::Fiber> = Vec::new();
        let mut claims: Vec<crate::claims::ClaimTable> = vec![crate::claims::ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let host = heaps.current().intern("127.0.0.1");
        let held = TcpListener::bind("127.0.0.1:0").expect("the held probe");
        let port = held.local_addr().expect("bound address").port();
        let listen_tag =
            |heaps: &mut Heaps, out: &mut OutputSink, port: i64| match net_listen_fn(heaps, out, &[
                Value::Ref(RefId::new(FIBER_ROOT, host)),
                Value::I64(port),
            ]) {
                Ok(NativeResult::Tag { tag, .. }) => tag,
                other => panic!("the listen answered no span: {other:?}"),
            };
        assert_eq!(
            listen_tag(&mut heaps, &mut out, 65536),
            PLATFORM_NET_REFUSED,
            "a port out of range names the refused class"
        );
        assert_eq!(
            listen_tag(&mut heaps, &mut out, i64::from(port)),
            PLATFORM_NET_ADDR_IN_USE,
            "a held port names the addr_in_use class"
        );
        let mut listeners = Listeners::default();
        match listeners.try_accept(7) {
            NetAttempt::Done(Err(err)) => {
                assert_eq!(err.to_string(), "unknown listener 7");
                assert_eq!(NetClass::of(&err), NetClass::Other);
            },
            other => panic!("an unknown handle answers its own error: {other:?}"),
        }
        listeners.close(7);
    }
}
