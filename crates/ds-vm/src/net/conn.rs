#[cfg(target_os = "linux")]
use std::net::SocketAddr;
use std::{
    io::{self, ErrorKind, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

#[cfg(target_os = "linux")]
use super::raw::{fd_into_stream, raw_sock};
use super::*;

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
    /// The pinned TLS server identity of a host or test
    /// ([`crate::Vm::set_tls_server_config`], the tls tier's server
    /// half); `None` answers the unprovisioned refusal — no default
    /// identity exists, a server certificate being the host's own to
    /// pin.
    tls_server: Option<Arc<rustls::ServerConfig>>,
    /// The accepted connections' rustls sessions, keyed by their
    /// connection handles — a handle with a row here routes its data
    /// natives through the session (the tls records,
    /// [`crate::tls`]'s close-tolerant discipline the same).
    tls_servers: Vec<(i64, rustls::ServerConnection)>,
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
    connecting: Vec<(i64, std::os::fd::OwnedFd, SocketAddr)>,
    /// The parked plain-http exchanges, keyed by their own minted ids
    /// (the staged connect plus the request/response buffers).
    #[cfg(target_os = "linux")]
    fetches: Vec<(i64, Fetch)>,
    /// The parked TLS exchanges, keyed by their own minted ids (the
    /// staged connect plus the rustls session machine of
    /// [`crate::tls`], the transport this table owns).
    #[cfg(target_os = "linux")]
    tls_fetches: Vec<(i64, crate::tls::TlsFetch)>,
    /// The parked DNS twin's pending rows:
    /// one per resolving connect, keyed by the pre-minted ids the
    /// resolved connect stage registers under — the job cell shared
    /// with the resolver helper thread that posts it
    /// ([`crate::resolver`], the machine's step 0).
    #[cfg(target_os = "linux")]
    resolving: Vec<(i64, Arc<crate::resolver::ResolveCell>)>,
    /// The resolver helper thread, started by the first resolving
    /// connect under the parking discipline and joined by this
    /// table's drop — the thread never outlives the VM it serves.
    #[cfg(target_os = "linux")]
    pub(super) resolver: Option<crate::resolver::Resolver>,
    /// The parked accept-tls machines, keyed by the pre-minted handle
    /// ids their completion registers under (the accepted stream and
    /// its rustls session closing with the row — [`crate::tls`]'s
    /// server machine, the transport this table owns).
    #[cfg(target_os = "linux")]
    tls_accepts: Vec<(i64, crate::tls::TlsAccept)>,
}

impl Default for Connections {
    fn default() -> Self {
        Self {
            streams: Vec::new(),
            next_id: 0,
            timeout: NET_TIMEOUT,
            tls: None,
            tls_server: None,
            tls_servers: Vec::new(),
            parking: false,
            #[cfg(target_os = "linux")]
            connecting: Vec::new(),
            #[cfg(target_os = "linux")]
            fetches: Vec::new(),
            #[cfg(target_os = "linux")]
            tls_fetches: Vec::new(),
            #[cfg(target_os = "linux")]
            resolving: Vec::new(),
            #[cfg(target_os = "linux")]
            resolver: None,
            #[cfg(target_os = "linux")]
            tls_accepts: Vec::new(),
        }
    }
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

    /// Pins the TLS server identity ([`crate::tls`]'s server half): the
    /// embedder and test seam ([`crate::Vm::set_tls_server_config`]) —
    /// no default identity exists, the unprovisioned `net_accept_tls`
    /// answering its own refusal.
    pub(crate) fn set_tls_server_config(&mut self, config: Arc<rustls::ServerConfig>) {
        self.tls_server = Some(config);
    }

    /// The pinned TLS server identity, `None` when no host set one.
    #[must_use]
    pub(crate) fn tls_server_config(&self) -> Option<Arc<rustls::ServerConfig>> {
        self.tls_server.clone()
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
    /// A session-backed row (the tls tier's accepted connection) routes
    /// through the rustls session instead — the plaintext drain first,
    /// the ciphertext reads the park.
    pub(crate) fn try_recv(&mut self, conn: i64, max: usize) -> NetAttempt<Vec<u8>> {
        if let Some(at) = self.tls_row(conn) {
            return self.try_recv_tls(conn, at, max);
        }
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
    /// bytes remain. The parked twin of [`Connections::send`]. A
    /// session-backed row (the tls tier's accepted connection) routes
    /// through the rustls session instead — the owed records draining
    /// before any plaintext stages.
    pub(crate) fn try_send(&mut self, conn: i64, data: &mut Vec<u8>) -> NetAttempt<()> {
        if let Some(at) = self.tls_row(conn) {
            return self.try_send_tls(conn, at, data);
        }
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

    /// The session row of a session-backed connection (the tls tier's
    /// accepted connection), its position in the table.
    fn tls_row(&self, conn: i64) -> Option<usize> {
        self.tls_servers.iter().position(|(id, _)| *id == conn)
    }

    /// One non-blocking recv attempt on a session-backed row: the tls
    /// tier's own step ([`crate::tls`]) — the buffered plaintext first
    /// (an answer at once, no park), the ciphertext reads the wait.
    fn try_recv_tls(&mut self, conn: i64, at: usize, max: usize) -> NetAttempt<Vec<u8>> {
        let Some(sa) = self.streams.iter().position(|(id, _)| *id == conn) else {
            return NetAttempt::Done(Err(unknown(conn)));
        };
        let _ = self.streams[sa].1.set_nonblocking(true);
        let (_, sock) = &mut self.streams[sa];
        let (_, session) = &mut self.tls_servers[at];
        crate::tls::server_recv_step(session, sock, max)
    }

    /// One non-blocking send attempt on a session-backed row: the tls
    /// tier's own step — the session's owed records drain before any
    /// plaintext stages, a park carrying the unstaged remainder.
    fn try_send_tls(&mut self, conn: i64, at: usize, data: &mut Vec<u8>) -> NetAttempt<()> {
        let Some(sa) = self.streams.iter().position(|(id, _)| *id == conn) else {
            return NetAttempt::Done(Err(unknown(conn)));
        };
        let _ = self.streams[sa].1.set_nonblocking(true);
        let (_, sock) = &mut self.streams[sa];
        let (_, session) = &mut self.tls_servers[at];
        crate::tls::server_send_step(session, sock, data)
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

    /// Starts one non-blocking connect over the raw OS socket: an IP-literal
    /// host resolves inline, without parking (the machine starting at its
    /// connect stage), a name submits its resolution to the resolver helper
    /// thread (the parked DNS twin) answering the pre-minted row id whose
    /// eventfd readability the wake's resolve stage consumes. [`NetConnectStart::Pending`]
    /// names the resolving or connecting row the park's completion
    /// reads — the id pre-minted, so the wake registers the stream
    /// under the very id the parked caller saw.
    #[cfg(target_os = "linux")]
    pub(crate) fn start_connect(&mut self, host: &str, port: u16) -> NetConnectStart {
        let conn = self.next_id;
        self.next_id = conn + 1;
        if let Some(ip) = crate::resolver::ip_literal(host) {
            let addr = SocketAddr::new(ip, port);
            return self.connect_stage(vec![addr], conn);
        }
        // the resolver helper starts with the first job it serves and
        // joins with this table's drop; the cell it posts shares the
        // row's Arc, so a canceled row's late posting writes a live fd
        if self.resolver.is_none() {
            match crate::resolver::Resolver::start() {
                Ok(helper) => self.resolver = Some(helper),
                Err(err) => return NetConnectStart::Done(Err(err)),
            }
        }
        let cell = match crate::resolver::ResolveCell::new() {
            Ok(cell) => Arc::new(cell),
            Err(err) => return NetConnectStart::Done(Err(err)),
        };
        let submitted = self
            .resolver
            .as_ref()
            .expect("the helper just started")
            .submit(host, port, Arc::clone(&cell));
        if let Err(err) = submitted {
            return NetConnectStart::Done(Err(err));
        }
        self.resolving.push((conn, cell));
        NetConnectStart::Pending(conn)
    }

    /// The staged machine's connect stage: the raw-socket loop over
    /// the resolved addresses — the first that either connects at
    /// once or stands in progress wins, an immediate refusal falling
    /// to the next address with the last failure the report.
    /// `pending` names the id the in-progress row registers under,
    /// pre-minted by the caller so the wake registers under the very
    /// id the parked caller saw.
    #[cfg(target_os = "linux")]
    fn connect_stage(&mut self, addrs: Vec<SocketAddr>, pending: i64) -> NetConnectStart {
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
                    self.connecting.push((pending, fd, addr));
                    return NetConnectStart::Pending(pending);
                },
                // the refusal falls to the next address; the fd's drop
                // closes the attempt
                Err(err) => last = err,
            }
        }
        NetConnectStart::Done(Err(last))
    }

    /// One parked connect's wake (the staged machine's own step): the
    /// resolve stage first — the readable eventfd taking the helper
    /// thread's answer and starting the connect stage over the
    /// resolved addresses — then the completion probe, a second
    /// connect reading the attempt's own state. `Pending` answers the
    /// direction the row's current stage still watches.
    #[cfg(target_os = "linux")]
    pub(crate) fn connect_wake(&mut self, conn: i64) -> ConnectWake {
        // the resolve stage: the eventfd's readable wake
        // takes the worker's answer, a failed lookup answering its
        // own error, the resolved addresses feeding the connect stage
        // under the very id the parked caller saw
        if let Some(at) = self.resolving.iter().position(|(id, _)| *id == conn) {
            let (_, cell) = self.resolving.remove(at);
            let Some(answer) = cell.take() else {
                // the worker has not posted yet (a spurious wake): the
                // row stands, the eventfd still its interest
                self.resolving.push((conn, cell));
                return ConnectWake::Pending {
                    watch: crate::reactor::Direction::Read,
                };
            };
            let addrs = match answer {
                Ok(addrs) => addrs,
                Err(detail) => {
                    return ConnectWake::Done(Err(io::Error::other(detail)));
                },
            };
            return match self.connect_stage(addrs, conn) {
                NetConnectStart::Done(Ok(registered)) => ConnectWake::Done(Ok(registered)),
                NetConnectStart::Done(Err(err)) => ConnectWake::Done(Err(err)),
                // the connect stage stands in progress under the same
                // id: the row's interest moves to the raw fd's
                // writability
                NetConnectStart::Pending(_) => ConnectWake::Pending {
                    watch: crate::reactor::Direction::Write,
                },
            };
        }
        // the connect stage: the completion probe reads the attempt's
        // own state — 0 or EISCONN connected, EALREADY still pending,
        // any other errno the failure the attempt met
        let done = self
            .connecting
            .iter()
            .position(|(id, ..)| *id == conn)
            .map(|at| raw_sock::connect_done(&self.connecting[at].1, &self.connecting[at].2));
        match done {
            // still pending: a spurious wake, the wait standing
            Some(Ok(true)) => ConnectWake::Pending {
                watch: crate::reactor::Direction::Write,
            },
            Some(Ok(false)) => {
                let at = self
                    .connecting
                    .iter()
                    .position(|(id, ..)| *id == conn)
                    .expect("the row the probe just read");
                let (row, fd, _) = self.connecting.remove(at);
                let stream = fd_into_stream(fd);
                self.adopt(row, stream);
                ConnectWake::Done(Ok(row))
            },
            Some(Err(err)) => {
                self.connecting.retain(|(id, ..)| *id != conn);
                ConnectWake::Done(Err(err))
            },
            None => ConnectWake::Done(Err(unknown(conn))),
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

    /// Drops one pending connect's row (either stage — the resolving
    /// job's cell, the raw fd closing with the connecting one), the
    /// parked-IO cancellation's cleanup, total over an unknown id
    /// (the wait's every other end door rides the same release). The
    /// cell's Arc keeps the eventfd open until the helper's own post
    /// lands, so a canceled row's late posting writes a live fd.
    pub(crate) fn cancel_connect(&mut self, conn: i64) {
        #[cfg(target_os = "linux")]
        {
            self.connecting.retain(|(id, ..)| *id != conn);
            self.resolving.retain(|(id, _)| *id != conn);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = conn;
        }
    }

    /// The OS-level handle of one pending connect's current stage, the
    /// park's registration key: the resolving row's eventfd, the
    /// connecting row's raw socket. `None` for an unknown row (and on
    /// the hosts that keep the blocking twin — no row ever stands
    /// there).
    pub(crate) fn raw_connect_handle(&mut self, conn: i64) -> Option<crate::reactor::IoHandle> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            if let Some((_, cell)) = self.resolving.iter().find(|(id, _)| *id == conn) {
                return cell.handle();
            }
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

    /// The interest direction one pending connect row's current stage
    /// watches (the park's initial registration): the resolve stage's eventfd
    /// readability, the connect stage's raw
    /// writability. An unknown row answers the connect's own — the
    /// rows only stand under the linux driver, where the connect
    /// stage is the machine a row cannot be past.
    #[cfg(target_os = "linux")]
    pub(crate) fn connect_interest(&mut self, conn: i64) -> crate::reactor::Direction {
        if self.resolving.iter().any(|(id, _)| *id == conn) {
            crate::reactor::Direction::Read
        } else {
            crate::reactor::Direction::Write
        }
    }

    /// The interest direction one parked plain exchange's current
    /// stage watches: its transport row's own ([`Connections::connect_interest`],
    /// the machine's step 0 the same resolve stage).
    #[cfg(target_os = "linux")]
    pub(crate) fn fetch_interest(&mut self, fetch: i64) -> crate::reactor::Direction {
        self.fetches
            .iter()
            .find(|(id, _)| *id == fetch)
            .map(|(_, machine)| machine.conn)
            .map_or(crate::reactor::Direction::Write, |conn| {
                self.connect_interest(conn)
            })
    }

    /// The interest direction one parked TLS exchange's current stage
    /// watches: its transport row's own, the plain machine's.
    #[cfg(target_os = "linux")]
    pub(crate) fn tls_fetch_interest(&mut self, fetch: i64) -> crate::reactor::Direction {
        self.tls_fetches
            .iter()
            .find(|(id, _)| *id == fetch)
            .map(|(_, machine)| machine.conn)
            .map_or(crate::reactor::Direction::Write, |conn| {
                self.connect_interest(conn)
            })
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
                    ConnectWake::Pending { watch } => {
                        self.fetches.push((fetch, machine));
                        return FetchWake::Pending { watch };
                    },
                    ConnectWake::Done(Err(err)) => return FetchWake::Done(Err(err)),
                    ConnectWake::Done(Ok(conn)) => {
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

    /// Starts one parked TLS exchange: the staged connect plus the
    /// rustls session the caller built over its pinned client, the
    /// request bytes pre-staged into the machine. The connect
    /// discipline is the plain fetch's own; the session advances at
    /// the wakes ([`crate::tls`]'s machine).
    #[cfg(target_os = "linux")]
    pub(crate) fn start_tls_fetch(
        &mut self,
        host: &str,
        port: u16,
        session: rustls::ClientConnection,
        request: Vec<u8>,
    ) -> Result<i64, io::Error> {
        let (conn, connected) = match self.start_connect(host, port) {
            NetConnectStart::Done(Ok(conn)) => (conn, true),
            NetConnectStart::Done(Err(err)) => return Err(err),
            NetConnectStart::Pending(row) => (row, false),
        };
        let fetch = self.next_id;
        self.next_id = fetch + 1;
        self.tls_fetches.push((
            fetch,
            crate::tls::TlsFetch::new(conn, connected, session, request),
        ));
        Ok(fetch)
    }

    /// Advances one parked TLS exchange as far as the readiness takes
    /// it: the connect probe first, then the session's own
    /// wants-read/wants-write steering the machine's steps (the TLS
    /// tier's driving, [`crate::tls`]). A pending step answers the
    /// direction the next wake watches; a finished exchange releases
    /// the transport either way.
    pub(crate) fn tls_fetch_wake(&mut self, fetch: i64) -> FetchWake {
        #[cfg(target_os = "linux")]
        {
            let Some(at) = self.tls_fetches.iter().position(|(id, _)| *id == fetch) else {
                return FetchWake::Done(Err(unknown_fetch(fetch)));
            };
            // the row comes out for the step: the session machine
            // drives the transport through this table's own lookups
            let (_, mut machine) = self.tls_fetches.remove(at);
            let wake = crate::tls::step(&mut machine, self);
            if matches!(wake, FetchWake::Pending { .. }) {
                self.tls_fetches.push((fetch, machine));
            } else if machine.connected {
                self.close(machine.conn);
            } else {
                self.cancel_connect(machine.conn);
            }
            wake
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = fetch;
            FetchWake::Done(Err(io::Error::other(
                "the parked TLS exchange is the linux driver's own",
            )))
        }
    }

    /// Drops one parked TLS exchange (a cancel, a deadline — the row's
    /// transport and session closing with it), total over an unknown id.
    pub(crate) fn cancel_tls_fetch(&mut self, fetch: i64) {
        #[cfg(target_os = "linux")]
        if let Some(at) = self.tls_fetches.iter().position(|(id, _)| *id == fetch) {
            let (_, machine) = self.tls_fetches.remove(at);
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

    /// The OS-level handle of one parked TLS exchange's transport —
    /// the registered stream's fd once connected, the pending connect's
    /// raw fd first — the park's registration key.
    pub(crate) fn raw_tls_fetch_handle(&mut self, fetch: i64) -> Option<crate::reactor::IoHandle> {
        #[cfg(target_os = "linux")]
        {
            let (_, machine) = self.tls_fetches.iter().find(|(id, _)| *id == fetch)?;
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

    /// Starts one parked accept-tls machine: the rustls server session
    /// the caller built over its pinned identity, the accepted stream
    /// when the native's own attempt already took one (the machine
    /// parking straight into its handshake), the listener's
    /// registration key when the accept itself still stands. Answers
    /// the machine row's id — the pre-minted connection id its
    /// completion registers under.
    #[cfg(target_os = "linux")]
    pub(crate) fn start_tls_accept(
        &mut self,
        listener: i64,
        listener_handle: Option<crate::reactor::IoHandle>,
        session: rustls::ServerConnection,
        stream: Option<TcpStream>,
    ) -> i64 {
        // the reactor owns readiness from here: a stream the native's
        // own attempt already took goes non-blocking now, the parked
        // steps' would-block answers the machine's own gates
        if let Some(stream) = stream.as_ref() {
            let _ = stream.set_nonblocking(true);
        }
        let conn = self.next_id;
        self.next_id = conn + 1;
        self.tls_accepts.push((
            conn,
            crate::tls::TlsAccept::new(conn, listener, listener_handle, session, stream),
        ));
        conn
    }

    /// Advances one parked accept-tls machine as far as the readiness
    /// takes it: the accept stage's readable door first, then the
    /// session's own wants steering the handshake's steps ([`crate::tls`]).
    /// A pending step answers the direction the next wake watches; the
    /// machine's end registers the stream with its session under the
    /// pre-minted id, or closes the stream with the failure.
    pub(crate) fn tls_accept_wake(&mut self, accept: i64, listeners: &mut Listeners) -> AcceptWake {
        #[cfg(target_os = "linux")]
        {
            let Some(at) = self.tls_accepts.iter().position(|(id, _)| *id == accept) else {
                return AcceptWake::Done(Err(unknown_accept(accept)));
            };
            // the row comes out for the step: the machine's accept
            // stage probes the listener table
            let (_, mut machine) = self.tls_accepts.remove(at);
            let wake = crate::tls::accept_step(&mut machine, listeners);
            match wake {
                AcceptWake::Pending { .. } => self.tls_accepts.push((accept, machine)),
                AcceptWake::Done(Ok(conn)) => {
                    let stream = machine
                        .take_stream()
                        .expect("the handshake owned the accepted stream");
                    self.adopt_tls(conn, stream, machine.session);
                },
                // the failure closes the accepted stream with the
                // machine's drop
                AcceptWake::Done(Err(_)) => {},
            }
            wake
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (accept, listeners);
            AcceptWake::Done(Err(io::Error::other(
                "the parked accept is the linux driver's own",
            )))
        }
    }

    /// Drops one parked accept-tls machine (a cancel, a deadline — the
    /// accepted stream and session closing with the row), total over
    /// an unknown id.
    pub(crate) fn cancel_tls_accept(&mut self, accept: i64) {
        #[cfg(target_os = "linux")]
        self.tls_accepts.retain(|(id, _)| *id != accept);
        #[cfg(not(target_os = "linux"))]
        {
            let _ = accept;
        }
    }

    /// The current transport's registration key of one parked
    /// accept-tls machine — the listener's fd while the accept stands,
    /// the accepted stream's after — the park's interest.
    pub(crate) fn raw_tls_accept_handle(
        &mut self,
        accept: i64,
    ) -> Option<crate::reactor::IoHandle> {
        #[cfg(target_os = "linux")]
        {
            self.tls_accepts
                .iter()
                .find(|(id, _)| *id == accept)
                .and_then(|(_, machine)| machine.handle())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = accept;
            None
        }
    }

    /// Registers the handshake-completed stream and its session under
    /// the pre-minted id the parked caller saw (the accept machine's
    /// own completion).
    #[cfg(target_os = "linux")]
    fn adopt_tls(&mut self, conn: i64, stream: TcpStream, session: rustls::ServerConnection) {
        if self.parking {
            let _ = stream.set_nonblocking(true);
        }
        self.streams.push((conn, stream));
        self.tls_servers.push((conn, session));
    }

    /// Registers the blocking twin's handshake-completed stream under
    /// a fresh id, the session row alongside — the same face the
    /// parked completion answers.
    pub(crate) fn adopt_blocking_tls(
        &mut self,
        stream: TcpStream,
        session: rustls::ServerConnection,
    ) -> i64 {
        let conn = self.register(stream);
        self.tls_servers.push((conn, session));
        conn
    }

    /// Installs this table's read/write timeouts on `stream` (the
    /// tls server half's blocking handshake included).
    pub(crate) fn bounded(&self, stream: TcpStream) -> io::Result<TcpStream> {
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        Ok(stream)
    }

    /// The stream of handle `conn`.
    pub(crate) fn stream(&mut self, conn: i64) -> io::Result<&mut TcpStream> {
        self.streams
            .iter_mut()
            .find(|(id, _)| *id == conn)
            .map(|(_, stream)| stream)
            .ok_or_else(|| unknown(conn))
    }

    /// Writes `data` on handle `conn`'s socket. A session-backed row
    /// (the tls tier's accepted connection) routes through the rustls
    /// session instead — the records the session seals.
    pub(crate) fn send(&mut self, conn: i64, data: &[u8]) -> io::Result<()> {
        if let Some(at) = self.tls_row(conn) {
            return self.send_tls(conn, at, data);
        }
        let timeout = self.timeout;
        let stream = self.stream(conn)?;
        stream
            .write_all(data)
            .map_err(|err| normalize(err, timeout))
    }

    /// The blocking send on a session-backed row: the tls tier's step
    /// machine driven to its answer, a would-block the socket timeout's
    /// own expiry rendering the timeout span.
    fn send_tls(&mut self, conn: i64, at: usize, data: &[u8]) -> io::Result<()> {
        let timeout = self.timeout;
        let Some(sa) = self.streams.iter().position(|(id, _)| *id == conn) else {
            return Err(unknown(conn));
        };
        let (_, sock) = &mut self.streams[sa];
        let (_, session) = &mut self.tls_servers[at];
        let mut pending = data.to_vec();
        match crate::tls::server_send_step(session, sock, &mut pending) {
            NetAttempt::Done(answer) => answer.map_err(|err| normalize(err, timeout)),
            NetAttempt::Park => Err(io::Error::new(
                ErrorKind::TimedOut,
                format!("timed out after {timeout:?}"),
            )),
        }
    }

    /// Reads at most `max` bytes on handle `conn`'s socket: one read, so
    /// an empty answer means an orderly close. A session-backed row
    /// (the tls tier's accepted connection) routes through the rustls
    /// session instead — the plaintext the session decodes.
    pub(crate) fn recv(&mut self, conn: i64, max: usize) -> io::Result<Vec<u8>> {
        if let Some(at) = self.tls_row(conn) {
            return self.recv_tls(conn, at, max);
        }
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

    /// The blocking recv on a session-backed row: the tls tier's step
    /// machine driven to its answer, a would-block the socket timeout's
    /// own expiry rendering the timeout span.
    fn recv_tls(&mut self, conn: i64, at: usize, max: usize) -> io::Result<Vec<u8>> {
        let timeout = self.timeout;
        let Some(sa) = self.streams.iter().position(|(id, _)| *id == conn) else {
            return Err(unknown(conn));
        };
        let (_, sock) = &mut self.streams[sa];
        let (_, session) = &mut self.tls_servers[at];
        match crate::tls::server_recv_step(session, sock, max) {
            NetAttempt::Done(answer) => answer.map_err(|err| normalize(err, timeout)),
            NetAttempt::Park => Err(io::Error::new(
                ErrorKind::TimedOut,
                format!("timed out after {timeout:?}"),
            )),
        }
    }

    /// Closes handle `conn` and removes it from the table (the session
    /// row of a tls-adopted connection with it); later uses of the
    /// handle answer "unknown connection". Total and idempotent: an
    /// unknown handle is a no-op (the deinit pairing's contract).
    pub(crate) fn close(&mut self, conn: i64) {
        self.tls_servers.retain(|(id, _)| *id != conn);
        self.streams.retain(|(id, _)| *id != conn);
    }
}
