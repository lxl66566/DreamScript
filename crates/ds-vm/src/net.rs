//! The TCP net natives: the network half of the platform surface.
//!
//! The net ops of the platform ABI's fixed table (the table contract in
//! `platform`'s module docs): `net_connect`, `net_send`,
//! `net_recv`, `net_close`, and `http_get`. The fallible four answer with
//! the uniform two-word tag+payload span [BL-2]
//! — the shared success marker on word 0, the payload on word 1 — and
//! failures carry the class discriminant of the ambient `net_err` case
//! they answer by error class (ruled 2026-11-12): a bounded wait that
//! expired answers `timeout`, every other failure `refused`, the
//! message `Ref` as payload shaped `"op target: detail"` like the file
//! natives' `io_err` messages. `net_close` alone answers the inert unit
//! word (the idempotent close seam the handle deinit pairs with,
//! `close_raw`'s contract).
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
//! # Parking discipline (the 2026-11-21 scheduler ruling)
//!
//! Installing a reactor on the VM ([`crate::Vm::install_reactor`]) flips
//! the registered-connection surface to non-blocking and the two data
//! natives to parking: `net_recv`/`net_send` attempt the op once
//! non-blocking and, when it would block, answer a park request the
//! dispatcher registers with the scheduler — the fiber parks, the drive
//! runs the next ready fiber, and the reactor's readiness (or the same
//! configured timeout, restated as the wait's deadline) completes the op
//! and delivers its answer at the parked site. Blocking stays the
//! embedder's platform choice: no reactor installed, the natives below
//! block exactly as before. The parking surface is the handle face alone
//! this round — `net_connect` and the `http_get` exchanges keep their
//! bounded blocking waits (std cannot open a non-connecting socket), a
//! boundary the item 76 server round revisits.
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
//! async runtime. The tls capability (H5.5's 2026-11-17 ruling) is a
//! host-level selection: where the platform provides it, the `http_get`
//! slot carries the routing body of [`crate::tls`] (an `https://` URL
//! rides rustls) and the capability's own `https_get` op is served;
//! where the host omits it, the plain body below stays and an `https://`
//! URL answers `net_err` naming the unprovided capability. The response
//! body (after the `\r\n\r\n` header terminator) is the success payload;
//! the status line is not inspected, and only close-delimited HTTP/1.0
//! responses decode (no chunked transfer).

use std::{
    io::{self, ErrorKind, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
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

/// The default bound on every blocking net wait.
pub const NET_TIMEOUT: Duration = Duration::from_secs(3);

/// The per-call byte cap of `net_recv` and of an `http_get` response.
pub const NET_CAP: usize = 1024 * 1024;

/// The default HTTP port when a URL names none.
const HTTP_PORT: u16 = 80;

/// The read size of the `http_get` response loop.
const HTTP_CHUNK: usize = 8192;

/// The VM-side connection table: the sockets `net_connect` opens, keyed by
/// the monotonic handle ids it hands out. VM state, not heap state: the
/// table rides the output sink (see the module docs), and dropping it
/// closes every socket still open.
#[derive(Debug)]
pub(crate) struct Connections {
    streams: Vec<(i64, TcpStream)>,
    next_id: i64,
    timeout: Duration,
    /// The pinned TLS client of a host or test that overrides the
    /// webpki-rooted default ([`crate::tls`]); `None` serves the default.
    tls: Option<Arc<rustls::ClientConfig>>,
    /// Whether the parking discipline owns the data natives: a reactor
    /// installed on the VM flips this, new registrations go
    /// non-blocking, and `net_recv`/`net_send` answer park requests on
    /// a would-block instead of blocking the thread.
    parking: bool,
}

impl Default for Connections {
    fn default() -> Self {
        Self {
            streams: Vec::new(),
            next_id: 0,
            timeout: NET_TIMEOUT,
            tls: None,
            parking: false,
        }
    }
}

/// One non-blocking attempt's outcome: the completed (or failed) op, or
/// the would-block the caller parks on.
pub(crate) enum NetAttempt<T> {
    /// The op ran: its result, success or failure.
    Done(Result<T, io::Error>),
    /// The op would block — the caller parks (the reactor owns the wait).
    Park,
}

impl Connections {
    /// Bounds every blocking net wait from now on (connect, read, write).
    pub(crate) fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The configured blocking-wait bound.
    #[must_use]
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Pins the TLS client (custom roots), overriding the webpki-rooted
    /// default — the embedder and test seam ([`crate::Vm::set_tls_config`]).
    pub(crate) fn set_tls_config(&mut self, config: Arc<rustls::ClientConfig>) {
        self.tls = Some(config);
    }

    /// The TLS client in force: the pinned one when a host set it, the
    /// webpki-rooted default otherwise ([`crate::tls`]).
    #[must_use]
    pub(crate) fn tls_config(&self) -> Arc<rustls::ClientConfig> {
        self.tls.clone().unwrap_or_else(crate::tls::webpki_client)
    }

    /// Opens a connected, timeout-bounded stream to `host:port` without
    /// registering it (the `http_get` ephemeral path).
    pub(crate) fn open(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        let mut last = io::Error::new(ErrorKind::NotFound, "no address resolved");
        for addr in (host, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, self.timeout) {
                Ok(stream) => return self.bounded(stream),
                // try the next resolved address; the last error is the report
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    /// Registers `stream` and answers its handle. Used by `net_connect`.
    pub(crate) fn register(&mut self, stream: TcpStream) -> i64 {
        if self.parking {
            // the reactor owns readiness: a registered socket answers
            // would-block to the thread, never a block — the flag's
            // whole job at registration time
            let _ = stream.set_nonblocking(true);
        }
        let id = self.next_id;
        self.next_id = id + 1;
        self.streams.push((id, stream));
        id
    }

    /// Flips the data natives to the parking discipline (a reactor was
    /// installed on the VM): registered sockets go non-blocking now and
    /// at every later registration.
    pub(crate) fn enable_parking(&mut self) {
        self.parking = true;
        for (_, stream) in &mut self.streams {
            let _ = stream.set_nonblocking(true);
        }
    }

    /// Whether the data natives park (a reactor is installed).
    pub(crate) fn parking(&self) -> bool {
        self.parking
    }

    /// One non-blocking read attempt on handle `conn`: at most `max`
    /// bytes, one read (the empty vec the orderly close), `Park` when
    /// the socket would block. The parked twin of [`Connections::recv`].
    pub(crate) fn try_recv(&mut self, conn: i64, max: usize) -> NetAttempt<Vec<u8>> {
        let timeout = self.timeout;
        let Ok(stream) = self.stream(conn) else {
            return NetAttempt::Done(Err(unknown(conn)));
        };
        let _ = stream.set_nonblocking(true);
        let mut buf = vec![0; max];
        match stream.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                NetAttempt::Done(Ok(buf))
            },
            Err(err) if err.kind() == ErrorKind::WouldBlock => NetAttempt::Park,
            Err(err) => NetAttempt::Done(Err(normalize(err, timeout))),
        }
    }

    /// One non-blocking write attempt on handle `conn`, draining `data`
    /// as far as the socket buffer takes it: the loop stops at the first
    /// would-block with the remainder kept in `data` (level-triggered
    /// readiness reports again once the buffer drains), `Park` while
    /// bytes remain. The parked twin of [`Connections::send`].
    pub(crate) fn try_send(&mut self, conn: i64, data: &mut Vec<u8>) -> NetAttempt<()> {
        let timeout = self.timeout;
        let Ok(stream) = self.stream(conn) else {
            return NetAttempt::Done(Err(unknown(conn)));
        };
        let _ = stream.set_nonblocking(true);
        let mut sent = 0;
        while sent < data.len() {
            match stream.write(&data[sent..]) {
                Ok(0) => {
                    return NetAttempt::Done(Err(io::Error::other("the socket accepted no bytes")))
                },
                Ok(n) => sent += n,
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => return NetAttempt::Done(Err(normalize(err, timeout))),
            }
        }
        if sent > 0 {
            data.drain(..sent);
        }
        if data.is_empty() { NetAttempt::Done(Ok(())) } else { NetAttempt::Park }
    }

    /// The OS-level handle of one registered connection, the reactor's
    /// registration key; `None` for an unknown handle (the caller's
    /// park then rides its deadline alone).
    pub(crate) fn raw_handle(&mut self, conn: i64) -> Option<crate::reactor::IoHandle> {
        let stream = self.stream(conn).ok()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            Some(crate::reactor::IoHandle(
                u64::try_from(stream.as_raw_fd()).ok()?,
            ))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            Some(crate::reactor::IoHandle(
                u64::try_from(stream.as_raw_socket()).ok()?,
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    /// Installs this table's read/write timeouts on `stream`.
    fn bounded(&self, stream: TcpStream) -> io::Result<TcpStream> {
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        Ok(stream)
    }

    /// The stream of handle `conn`.
    fn stream(&mut self, conn: i64) -> io::Result<&mut TcpStream> {
        self.streams
            .iter_mut()
            .find(|(id, _)| *id == conn)
            .map(|(_, stream)| stream)
            .ok_or_else(|| unknown(conn))
    }

    /// Writes `data` on handle `conn`'s socket.
    pub(crate) fn send(&mut self, conn: i64, data: &[u8]) -> io::Result<()> {
        let timeout = self.timeout;
        let stream = self.stream(conn)?;
        stream
            .write_all(data)
            .map_err(|err| normalize(err, timeout))
    }

    /// Reads at most `max` bytes on handle `conn`'s socket: one read, so
    /// an empty answer means an orderly close.
    pub(crate) fn recv(&mut self, conn: i64, max: usize) -> io::Result<Vec<u8>> {
        let timeout = self.timeout;
        let stream = self.stream(conn)?;
        let mut buf = vec![0; max];
        match stream.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Ok(buf)
            },
            Err(err) => Err(normalize(err, timeout)),
        }
    }

    /// Closes handle `conn` and removes it from the table; later uses of
    /// the handle answer "unknown connection". Total and idempotent: an
    /// unknown handle is a no-op (the deinit pairing's contract).
    pub(crate) fn close(&mut self, conn: i64) {
        self.streams.retain(|(id, _)| *id != conn);
    }
}

/// The "unknown connection" error, shared by every handle lookup.
fn unknown(conn: i64) -> io::Error {
    io::Error::other(format!("unknown connection {conn}"))
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
/// discriminant answers by error class (ruled 2026-11-12, widened
/// 2026-11-21 to the alias's whole class set) — every kind the classes
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
    let Some(at) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err(io::Error::other("malformed response: no header terminator"));
    };
    String::from_utf8(response[at + 4..].to_owned())
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
    match out.net().open(&host, port) {
        Ok(stream) => {
            let id = out.net().register(stream);
            Ok(ok(Value::I64(id)))
        },
        Err(err) => Ok(net_err(
            heaps,
            NetClass::of(&err),
            "net_connect",
            &format!("{host}:{port}"),
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
/// firing on an already-closed handle stays clean).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn net_close_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(conn) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.net().close(conn);
    }
    // the inert unit word
    Ok(NativeResult::Word(Value::I64(0)))
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

#[cfg(test)]
mod tests {
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

    /// The widened error-class mapping (2026-11-21): every kind a class
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
    }
}
