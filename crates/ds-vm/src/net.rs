//! The TCP net natives: the network half of the platform surface.
//!
//! The net ops of the platform ABI's fixed table (the table contract in
//! `platform`'s module docs): `net_connect`, `net_send`,
//! `net_recv`, `net_close`, `http_get`, and the listener face
//! `net_listen`/`net_accept`/`net_listener_close`. The fallible ops
//! answer with the uniform two-word tag+payload span [BL-2]
//! — the shared success marker on word 0, the payload on word 1 — and
//! failures carry the class discriminant of the ambient `net_err` case
//! they answer by error class (ruled 2026-11-12): a bounded wait that
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
//! # Parking discipline (the 2026-11-21 scheduler ruling)
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
//! accepted stream like a connecting one. Blocking stays the
//! embedder's platform choice: no reactor installed, the natives below
//! block exactly as before. `net_connect` and the plain `http_get`
//! exchange park too (the 2026-11-23 ruling): std cannot open a
//! non-connecting socket, so the parked connect rides the raw OS
//! socket — libc's own `socket`/`fcntl(O_NONBLOCK)`/`connect` and
//! completion probe over no socket library, the epoll driver's
//! OS-own-symbols precedent — the writability interest parked on, the
//! wake probing completion and registering the stream like any other
//! handle. The plain fetch rides the same staged machine (connect,
//! request drain, response reads to the close), a step per wake; the
//! https exchange keeps the rustls tier's blocking transport (its
//! session steers its own wants-read/wants-write discipline, a
//! transport rework riding the tls round), and a non-Linux host keeps
//! the blocking twin under any reactor until its driver round owns the
//! raw seam.
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
    net::{TcpListener, TcpStream, ToSocketAddrs},
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

/// The poll interval of the blocking accept's bounded wait (std's
/// listener carries no OS accept timeout, so the non-blocking socket
/// polls to its deadline).
const ACCEPT_POLL: Duration = Duration::from_millis(1);

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
    /// The in-progress raw connects, keyed by the pre-minted handle ids
    /// their completion registers under (the fd and the address it
    /// dials closing with the row — the parked connect's own storage,
    /// the linux driver's machine; the wake's completion probe dials
    /// the address a second time).
    #[cfg(target_os = "linux")]
    connecting: Vec<(i64, std::os::fd::OwnedFd, std::net::SocketAddr)>,
    /// The parked plain-http exchanges, keyed by their own minted ids
    /// (the staged connect plus the request/response buffers).
    #[cfg(target_os = "linux")]
    fetches: Vec<(i64, Fetch)>,
}

impl Default for Connections {
    fn default() -> Self {
        Self {
            streams: Vec::new(),
            next_id: 0,
            timeout: NET_TIMEOUT,
            tls: None,
            parking: false,
            #[cfg(target_os = "linux")]
            connecting: Vec::new(),
            #[cfg(target_os = "linux")]
            fetches: Vec::new(),
        }
    }
}

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

/// The raw OS socket binding of the parked connect (the 2026-11-23
/// ruling): libc's own symbols — `socket`, `fcntl(O_NONBLOCK)`,
/// `connect`, and the completion probe — over no socket library, the
/// epoll driver's OS-own-symbols precedent. The parked connect is the
/// linux driver's own this round; a non-linux host keeps the blocking
/// twin under any reactor, riding the kqueue/IOCP driver rounds.
#[cfg(target_os = "linux")]
mod raw_sock {
    use std::{
        ffi::c_int,
        io,
        net::SocketAddr,
        os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
    };

    // the kernel ABI constants: address families, the stream socket
    // kind with its exec-closed flag, the fcntl command and the
    // non-blocking flag, the option level and name, the in-progress
    // errno — stable ABI values, not headers
    pub const AF_INET: c_int = 2;
    pub const AF_INET6: c_int = 10;
    pub const SOCK_STREAM: c_int = 1;
    pub const SOCK_CLOEXEC: c_int = 0o2_000_000;
    pub const F_SETFL: c_int = 4;
    pub const O_NONBLOCK: c_int = 0o4000;
    pub const EINPROGRESS: c_int = 115;
    pub const EISCONN: c_int = 106;
    pub const EALREADY: c_int = 114;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SockAddrIn {
        pub family: u16,
        pub port: u16,
        pub addr: [u8; 4],
        pub zero: [u8; 8],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SockAddrIn6 {
        pub family: u16,
        pub port: u16,
        pub flowinfo: u32,
        pub addr: [u8; 16],
        pub scope_id: u32,
    }

    /// The sockaddr the two families address; only written, then passed
    /// by pointer to the one call that reads it.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union SockAddr {
        pub v4: SockAddrIn,
        pub v6: SockAddrIn6,
    }

    unsafe extern "C" {
        fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        #[link_name = "connect"]
        fn sys_connect(fd: c_int, addr: *const SockAddr, len: u32) -> c_int;
    }

    /// One address's sockaddr and its length, filled per family.
    fn fill(addr: &SocketAddr) -> (SockAddr, u32) {
        match addr {
            SocketAddr::V4(a) => (
                SockAddr {
                    v4: SockAddrIn {
                        family: u16::try_from(AF_INET).expect("the v4 family fits u16"),
                        port: a.port().to_be(),
                        addr: a.ip().octets(),
                        zero: [0; 8],
                    },
                },
                16,
            ),
            SocketAddr::V6(a) => (
                SockAddr {
                    v6: SockAddrIn6 {
                        family: u16::try_from(AF_INET6).expect("the v6 family fits u16"),
                        port: a.port().to_be(),
                        flowinfo: 0,
                        addr: a.ip().octets(),
                        scope_id: a.scope_id(),
                    },
                },
                28,
            ),
        }
    }

    /// Opens one exec-closed, non-blocking TCP socket of `family`.
    pub fn open_stream(family: c_int) -> io::Result<OwnedFd> {
        // SAFETY: the plain socket call; a negative answer is the OS
        // refusal, a nonnegative one the fd the owned handle adopts
        let fd = unsafe { socket(family, SOCK_STREAM | SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the fd is owned from here; the flag call only flips
        // its blocking disposition
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: the plain fcntl flag call
        if unsafe { fcntl(fd, F_SETFL, O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(owned)
    }

    /// Starts one non-blocking connect: `Ok(true)` the attempt stands
    /// in progress (the wake probes its completion), `Ok(false)` it
    /// connected at once, `Err` the immediate refusal.
    pub fn start_connect(fd: &OwnedFd, addr: &SocketAddr) -> io::Result<bool> {
        let (sockaddr, len) = fill(addr);
        // SAFETY: the plain connect call over the stack address; the
        // union was just written with the family the address names
        let rc = unsafe { sys_connect(fd.as_raw_fd(), &raw const sockaddr, len) };
        if rc == 0 {
            return Ok(false);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(EINPROGRESS) {
            return Ok(true);
        }
        Err(err)
    }

    /// The pending connect's completion probe: a second connect reads
    /// the attempt's own state — 0 or EISCONN connected, EALREADY
    /// still pending, any other errno the failure the attempt met.
    /// The textbook probe is getsockopt(SO_ERROR), but a host mediating
    /// socket calls through a security broker can leave the errno
    /// queue polluted (EPERM reported on a connected socket), and the
    /// second connect asks the socket's own state, not the queue.
    pub fn connect_done(fd: &OwnedFd, addr: &SocketAddr) -> io::Result<bool> {
        let (sockaddr, len) = fill(addr);
        // SAFETY: the plain connect call over the stack address; the
        // union was just written with the family the address names
        let rc = unsafe { sys_connect(fd.as_raw_fd(), &raw const sockaddr, len) };
        if rc == 0 {
            return Ok(false);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(EISCONN) => Ok(false),
            Some(EALREADY) => Ok(true),
            _ => Err(err),
        }
    }
}

/// Adopts one raw fd into the std stream it names (the parked
/// connect's completion): the fd's non-blocking disposition lives on
/// the open file description, so the stream keeps it.
#[cfg(target_os = "linux")]
fn fd_into_stream(fd: std::os::fd::OwnedFd) -> TcpStream {
    use std::os::fd::{FromRawFd as _, IntoRawFd as _};
    // SAFETY: the fd is owned and closed exactly once — adopted by the
    // stream that takes it over
    unsafe { TcpStream::from_raw_fd(fd.into_raw_fd()) }
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

    /// Starts one non-blocking connect over the raw OS socket (the
    /// 2026-11-23 ruling): the addresses resolve, the first that either
    /// connects at once or stands in progress wins, and an immediate
    /// refusal falls to the next address with the last failure the
    /// report. [`NetConnectStart::Pending`] names the connecting row
    /// the park's completion reads — the id pre-minted, so the wake
    /// registers the stream under the very id the parked caller saw.
    #[cfg(target_os = "linux")]
    pub(crate) fn start_connect(&mut self, host: &str, port: u16) -> NetConnectStart {
        let addrs = match (host, port).to_socket_addrs() {
            Ok(addrs) => addrs.collect::<Vec<_>>(),
            Err(err) => return NetConnectStart::Done(Err(err)),
        };
        let mut last = io::Error::new(ErrorKind::NotFound, "no address resolved");
        for addr in addrs {
            let family = if addr.is_ipv4() { raw_sock::AF_INET } else { raw_sock::AF_INET6 };
            let fd = match raw_sock::open_stream(family) {
                Ok(fd) => fd,
                Err(err) => {
                    last = err;
                    continue;
                },
            };
            match raw_sock::start_connect(&fd, &addr) {
                // connected at once: adopt and register like any other
                // handle (the parking flag keeps the stream honest)
                Ok(false) => {
                    let stream = fd_into_stream(fd);
                    return NetConnectStart::Done(Ok(self.register(stream)));
                },
                // in progress: the row owns the fd and the address,
                // the pre-minted id the wake registers under
                Ok(true) => {
                    let conn = self.next_id;
                    self.next_id = conn + 1;
                    self.connecting.push((conn, fd, addr));
                    return NetConnectStart::Pending(conn);
                },
                // the refusal falls to the next address; the fd's drop
                // closes the attempt
                Err(err) => last = err,
            }
        }
        NetConnectStart::Done(Err(last))
    }

    /// One parked connect's completion probe: the wake the writability
    /// reports dials the row's address a second time — connected (the
    /// raw fd adopted into the stream registered under the row's id),
    /// still pending (a spurious wake, the wait standing), or the
    /// errno the attempt met.
    #[cfg(target_os = "linux")]
    pub(crate) fn connect_wake(&mut self, conn: i64) -> NetAttempt<i64> {
        let Some(at) = self.connecting.iter().position(|(id, ..)| *id == conn) else {
            return NetAttempt::Done(Err(unknown(conn)));
        };
        let done = raw_sock::connect_done(&self.connecting[at].1, &self.connecting[at].2);
        match done {
            Ok(true) => NetAttempt::Park,
            Ok(false) => {
                let (conn, fd, _) = self.connecting.remove(at);
                let stream = fd_into_stream(fd);
                self.adopt(conn, stream);
                NetAttempt::Done(Ok(conn))
            },
            Err(err) => {
                self.connecting.remove(at);
                NetAttempt::Done(Err(err))
            },
        }
    }

    /// Registers `stream` under the pre-minted id the pending connect
    /// parked with (the mint advanced past the id at park time, so the
    /// slot is fresh by construction).
    fn adopt(&mut self, conn: i64, stream: TcpStream) {
        if self.parking {
            let _ = stream.set_nonblocking(true);
        }
        self.streams.push((conn, stream));
    }

    /// Drops one pending connect's row, the raw fd closing with it —
    /// the parked-IO cancellation's cleanup, total over an unknown id
    /// (the wait's every other end door rides the same release).
    pub(crate) fn cancel_connect(&mut self, conn: i64) {
        #[cfg(target_os = "linux")]
        self.connecting.retain(|(id, ..)| *id != conn);
        #[cfg(not(target_os = "linux"))]
        {
            let _ = conn;
        }
    }

    /// The OS-level handle of one pending connect's raw socket, the
    /// park's registration key; `None` for an unknown row (and on the
    /// hosts that keep the blocking twin — no row ever stands there).
    pub(crate) fn raw_connect_handle(&mut self, conn: i64) -> Option<crate::reactor::IoHandle> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            let fd = self
                .connecting
                .iter()
                .find(|(id, ..)| *id == conn)
                .map(|(_, fd, _)| fd)?;
            Some(crate::reactor::IoHandle(
                u64::try_from(fd.as_raw_fd()).ok()?,
            ))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = conn;
            None
        }
    }

    /// Starts one parked plain-http exchange: the staged connect plus
    /// the request bytes, answering the fetch row's id. The machine
    /// rides the same connect discipline `net_connect` parks on; every
    /// later step advances at the wakes.
    #[cfg(target_os = "linux")]
    pub(crate) fn start_fetch(
        &mut self,
        host: &str,
        port: u16,
        request: String,
    ) -> Result<i64, io::Error> {
        let (conn, connected) = match self.start_connect(host, port) {
            NetConnectStart::Done(Ok(conn)) => (conn, true),
            NetConnectStart::Done(Err(err)) => return Err(err),
            NetConnectStart::Pending(row) => (row, false),
        };
        let fetch = self.next_id;
        self.next_id = fetch + 1;
        self.fetches.push((fetch, Fetch {
            conn,
            connected,
            out: request.into_bytes(),
            seen: Vec::new(),
        }));
        Ok(fetch)
    }

    /// Advances one parked exchange as far as the readiness takes it,
    /// the OS's own would-block answers the stage gates (the event is
    /// advisory, the probes the truth): the connect probe first, then
    /// the request drain, then the response reads to the peer's close
    /// — the same close-delimited round the blocking exchange runs.
    /// The linux driver's own machine; the other hosts never mint the
    /// wait.
    pub(crate) fn fetch_wake(&mut self, fetch: i64) -> FetchWake {
        #[cfg(target_os = "linux")]
        {
            let Some(at) = self.fetches.iter().position(|(id, _)| *id == fetch) else {
                return FetchWake::Done(Err(unknown_fetch(fetch)));
            };
            // the row comes out for the step: the probes below borrow
            // the whole table (the try_* twins are this table's own)
            let (_, mut machine) = self.fetches.remove(at);
            if !machine.connected {
                match self.connect_wake(machine.conn) {
                    NetAttempt::Park => {
                        self.fetches.push((fetch, machine));
                        return FetchWake::Pending {
                            watch: crate::reactor::Direction::Write,
                        };
                    },
                    NetAttempt::Done(Err(err)) => return FetchWake::Done(Err(err)),
                    NetAttempt::Done(Ok(conn)) => {
                        machine.conn = conn;
                        machine.connected = true;
                    },
                }
            }
            if !machine.out.is_empty() {
                match self.try_send(machine.conn, &mut machine.out) {
                    NetAttempt::Park => {
                        self.fetches.push((fetch, machine));
                        return FetchWake::Pending {
                            watch: crate::reactor::Direction::Write,
                        };
                    },
                    NetAttempt::Done(Err(err)) => {
                        self.close(machine.conn);
                        return FetchWake::Done(Err(err));
                    },
                    NetAttempt::Done(Ok(())) => {},
                }
            }
            loop {
                match self.try_recv(machine.conn, HTTP_CHUNK) {
                    NetAttempt::Park => {
                        self.fetches.push((fetch, machine));
                        return FetchWake::Pending {
                            watch: crate::reactor::Direction::Read,
                        };
                    },
                    NetAttempt::Done(Err(err)) => {
                        self.close(machine.conn);
                        return FetchWake::Done(Err(err));
                    },
                    // the peer's close ends the exchange
                    NetAttempt::Done(Ok(bytes)) if bytes.is_empty() => {
                        self.close(machine.conn);
                        return FetchWake::Done(finish_response(&machine.seen));
                    },
                    NetAttempt::Done(Ok(bytes)) => {
                        machine.seen.extend_from_slice(&bytes);
                        if machine.seen.len() > NET_CAP {
                            self.close(machine.conn);
                            return FetchWake::Done(Err(io::Error::other(
                                "response exceeds the 1 MiB cap",
                            )));
                        }
                    },
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = fetch;
            FetchWake::Done(Err(io::Error::other(
                "the parked exchange is the linux driver's own",
            )))
        }
    }

    /// Drops one parked exchange (a cancel, a deadline — the row's
    /// transport closing with it), total over an unknown id.
    pub(crate) fn cancel_fetch(&mut self, fetch: i64) {
        #[cfg(target_os = "linux")]
        if let Some(at) = self.fetches.iter().position(|(id, _)| *id == fetch) {
            let (_, machine) = self.fetches.remove(at);
            if machine.connected {
                self.close(machine.conn);
            } else {
                self.cancel_connect(machine.conn);
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = fetch;
        }
    }

    /// The OS-level handle of one parked exchange's transport — the
    /// registered stream's fd once connected, the pending connect's raw
    /// fd first — the park's registration key.
    pub(crate) fn raw_fetch_handle(&mut self, fetch: i64) -> Option<crate::reactor::IoHandle> {
        #[cfg(target_os = "linux")]
        {
            let (_, machine) = self.fetches.iter().find(|(id, _)| *id == fetch)?;
            if machine.connected {
                self.raw_handle(machine.conn)
            } else {
                self.raw_connect_handle(machine.conn)
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = fetch;
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
}

impl Default for Listeners {
    fn default() -> Self {
        Self {
            bound: Vec::new(),
            next_id: 0,
            timeout: NET_TIMEOUT,
            parking: false,
        }
    }
}

impl Listeners {
    /// Bounds the blocking accept wait from now on (the connection
    /// table's bound and this one set together, one `set_net_timeout`).
    pub(crate) fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
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

    /// The bounded blocking accept on handle `lid`: std's listener
    /// carries no OS accept timeout, so the non-blocking socket polls
    /// to the configured deadline — the same bound every blocking net
    /// wait keeps, polled rather than waited because that is the only
    /// portable shape.
    pub(crate) fn accept(&mut self, lid: i64) -> io::Result<TcpStream> {
        let timeout = self.timeout;
        let listener = self.listener(lid)?;
        let _ = listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + timeout;
        loop {
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
    finish_response(&response)
}

/// The response's decode, the blocking exchange's and the parked
/// machine's shared finish: the body after the header terminator,
/// refused without the terminator or past the cap.
fn finish_response(seen: &[u8]) -> Result<String, io::Error> {
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
    // the parked twin (the 2026-11-23 ruling): a reactor owns
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
/// readability (the wake performing the accept); otherwise the wait
/// blocks the thread, bounded by the configured net timeout.
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
            NetAttempt::Park => {
                let deadline = std::time::Instant::now() + out.listeners().timeout();
                Ok(NativeResult::Park(crate::sched::WorldWait::accept(
                    lid, deadline,
                )))
            },
        };
    }
    match out.listeners().accept(lid) {
        Ok(stream) => Ok(ok(Value::I64(out.net().register(stream)))),
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
/// the `net_listener` deinit pairs with, `net_close`'s own contract.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn net_listener_close_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(lid) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.listeners().close(lid);
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

    /// The parked connect's raw machine (the 2026-11-23 ruling): a
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
                        NetAttempt::Park => std::thread::sleep(Duration::from_millis(1)),
                        wake @ NetAttempt::Done(_) => {
                            answered = Some(wake);
                            break;
                        },
                    }
                }
                match answered.expect("the loopback connect completes") {
                    NetAttempt::Done(Ok(done)) => {
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
                        NetAttempt::Park => std::thread::sleep(Duration::from_millis(1)),
                        NetAttempt::Done(Err(err)) => {
                            refused = Some(err);
                            break;
                        },
                        NetAttempt::Done(Ok(conn)) => {
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
