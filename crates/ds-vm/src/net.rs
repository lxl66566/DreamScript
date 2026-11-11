//! The TCP net natives: the network half of the platform surface.
//!
//! Five natives in the std platform table (indices 20-24, the table
//! contract in `native`'s module docs): `net_connect`, `net_send`,
//! `net_recv`, `net_close`, and `http_get`. They answer with the uniform
//! two-word tag+payload span [BL-2] — the shared success marker on word 0,
//! the payload on word 1 — and failures carry the [`PLATFORM_NET_ERR`]
//! tag with the message `Ref` as payload, shaped `"op target: detail"`
//! like the file natives' `io_err` messages.
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
//! stays open until the VM drops.
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
//! # Caps
//!
//! One `net_recv` reads at most `max_bytes` bytes, clamped to the 1 MiB
//! [`NET_CAP`] per call (a larger request streams across calls), and
//! `http_get` reads at most 1 MiB of response, answering `net_err` past
//! the cap.
//!
//! # The M7 platform tier decision: no TLS
//!
//! `http_get` speaks HTTP/1.0 over a plain `std::net::TcpStream` — no
//! async runtime, no TLS. An `https://` URL answers `net_err` "https
//! unsupported at this platform tier (no TLS)"; `http://` is the only
//! scheme. The response body (after the `\r\n\r\n` header terminator) is
//! the success payload; the status line is not inspected, and only
//! close-delimited HTTP/1.0 responses decode (no chunked transfer).

use std::{
    io::{self, ErrorKind, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

use crate::{
    native::{NativeResult, OutputSink, PLATFORM_IO_OK, PLATFORM_NET_ERR, i64_arg, ref_arg},
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
}

impl Default for Connections {
    fn default() -> Self {
        Self {
            streams: Vec::new(),
            next_id: 0,
            timeout: NET_TIMEOUT,
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
        let id = self.next_id;
        self.next_id = id + 1;
        self.streams.push((id, stream));
        id
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
    /// the handle answer "unknown connection".
    pub(crate) fn close(&mut self, conn: i64) -> io::Result<()> {
        let Some(at) = self.streams.iter().position(|(id, _)| *id == conn) else {
            return Err(unknown(conn));
        };
        // dropping the stream closes the socket
        self.streams.remove(at);
        Ok(())
    }
}

/// The "unknown connection" error, shared by every handle lookup.
fn unknown(conn: i64) -> io::Error {
    io::Error::other(format!("unknown connection {conn}"))
}

/// Renders a timed-out wait as its own detail instead of the OS's
/// "resource temporarily unavailable", so the `net_err` message says
/// what happened.
fn normalize(err: io::Error, timeout: Duration) -> io::Error {
    match err.kind() {
        // read/write timeouts surface as EAGAIN on Unix, EWOULDBLOCK on
        // Windows; connect timeouts as ETIMEDOUT
        ErrorKind::TimedOut | ErrorKind::WouldBlock => {
            io::Error::other(format!("timed out after {timeout:?}"))
        },
        _ => err,
    }
}

/// The `net_err` span: the message `Ref` as the payload word.
fn net_err(heaps: &mut Heaps, op: &str, target: &str, detail: &str) -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_NET_ERR,
        payload: heaps.new_str(format!("{op} {target}: {detail}")),
    }
}

/// The `ok` span: the success value as the payload word.
fn ok(payload: Value) -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload,
    }
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
    match out.net().send(conn, data.as_bytes()) {
        // the payload word is the inert unit word, like write_file's
        Ok(()) => Ok(ok(Value::I64(0))),
        Err(err) => Ok(net_err(
            heaps,
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
            "net_recv",
            &conn.to_string(),
            "max_bytes must be positive",
        ));
    }
    let bytes = match out
        .net()
        .recv(conn, usize::try_from(max).unwrap_or(NET_CAP).min(NET_CAP))
    {
        Ok(bytes) => bytes,
        Err(err) => {
            return Ok(net_err(
                heaps,
                "net_recv",
                &conn.to_string(),
                &err.to_string(),
            ))
        },
    };
    // a string native answers a string: mid-code-point truncation and
    // binary protocols surface as errors, not as silently mangled text
    match String::from_utf8(bytes) {
        Ok(text) => Ok(ok(heaps.new_str(text))),
        Err(_) => Ok(net_err(
            heaps,
            "net_recv",
            &conn.to_string(),
            "invalid UTF-8 in the received bytes",
        )),
    }
}

/// `net_close(conn: int) -> unit | net_err`: closes the socket and
/// invalidates the handle.
pub(crate) fn net_close_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let conn = i64_arg(args, 0)?;
    match out.net().close(conn) {
        Ok(()) => Ok(ok(Value::I64(0))),
        Err(err) => Ok(net_err(
            heaps,
            "net_close",
            &conn.to_string(),
            &err.to_string(),
        )),
    }
}

/// `http_get(url: string) -> string | net_err`: a plain-HTTP/1.0 GET. See
/// the module docs for the tier decision (no TLS, body-only payload,
/// close-delimited responses).
pub(crate) fn http_get_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let url = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    let detail = http_get(out, &url);
    match detail {
        Ok(body) => Ok(ok(heaps.new_str(body))),
        Err(detail) => Ok(net_err(heaps, "http_get", &url, &detail)),
    }
}

/// The `http_get` body: parse the URL, run the one request/response
/// exchange, answer the response body, or the `net_err` detail.
fn http_get(out: &mut OutputSink, url: &str) -> Result<String, String> {
    let Some(rest) = url.strip_prefix("http://").map(str::to_owned) else {
        if url.starts_with("https://") {
            return Err("https unsupported at this platform tier (no TLS)".into());
        }
        return Err("unsupported URL scheme (http:// only)".into());
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (rest[..at].to_owned(), rest[at..].to_owned()),
        None => (rest, "/".to_owned()),
    };
    let host = authority.split(':').next().unwrap_or_default().to_owned();
    let port = match authority.split_once(':') {
        // IPv6-literal authorities are out of scope at this tier
        Some((_, port)) => port
            .parse::<u16>()
            .map_err(|_| "invalid port in the URL".to_owned())?,
        None => HTTP_PORT,
    };
    let net = out.net();
    let timeout = net.timeout();
    let mut stream = net.open(&host, port).map_err(|err| err.to_string())?;
    let request = format!("GET {path} HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|err| normalize(err, timeout).to_string())?;
    // read to the server's close-delimited end, or past the cap
    let mut response = Vec::new();
    loop {
        let mut chunk = [0; HTTP_CHUNK];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&chunk[..n]);
                if response.len() > NET_CAP {
                    return Err("response exceeds the 1 MiB cap".into());
                }
            },
            Err(err) => return Err(normalize(err, timeout).to_string()),
        }
    }
    let Some(at) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err("malformed response: no header terminator".into());
    };
    String::from_utf8(response[at + 4..].to_owned())
        .map_err(|_| "invalid UTF-8 in the response body".into())
}

#[cfg(test)]
mod tests {
    use ds_abi::SlotType;

    use super::*;
    use crate::{fiber::FIBER_ROOT, object::Objects, value::RefId};

    /// The timeout discipline: a never-answering socket yields the
    /// timeout detail, not the OS's EAGAIN rendering.
    #[test]
    fn timeouts_render_as_timed_out() {
        let err = normalize(
            io::Error::from(ErrorKind::WouldBlock),
            Duration::from_millis(250),
        );
        assert_eq!(err.to_string(), "timed out after 250ms");
    }

    /// An unknown handle is the same error everywhere a handle is used.
    #[test]
    fn unknown_handles_answer_unknown_connection() {
        let mut conns = Connections::default();
        let err = conns.close(7).expect_err("handle 7 was never opened");
        assert_eq!(err.to_string(), "unknown connection 7");
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
