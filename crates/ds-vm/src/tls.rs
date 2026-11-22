//! The net face's TLS tier (H5.5's ruling of 2026-11-17): https over
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
//! certs this round, an embedding host or test pinning its own roots
//! through [`crate::Vm::set_tls_config`].
//!
//! The blocking discipline is the net ops' own ([`crate::net`]): every
//! wait rides the connection table's bounded socket timeouts, a
//! blocking foreign call pinning its fiber. Both legs park under an
//! installed reactor (linux this round): the plain leg through the
//! staged exchange machine, the rustls leg through the parked twin
//! below — the session is a synchronous state machine, so the
//! transport drives it a step per readiness wake, the session's own
//! wants-write/wants-read naming the interest each step watches.

use std::{
    io::{self, Read, Write},
    sync::{Arc, LazyLock},
};

use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};

use crate::{
    native::{NativeResult, OutputSink, ref_arg},
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
struct CloseTolerant(StreamOwned<ClientConnection, std::net::TcpStream>);

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

/// The parked TLS exchange's machine (the tls round of the 2026-11-23
/// connect ruling): the rustls session over the staged connect's
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
    // the connect stage: the plain machine's own probe
    if !machine.connected {
        match net.connect_wake(machine.conn) {
            net::NetAttempt::Park => {
                return net::FetchWake::Pending {
                    watch: Direction::Write,
                }
            },
            net::NetAttempt::Done(Err(err)) => return net::FetchWake::Done(Err(err)),
            net::NetAttempt::Done(Ok(conn)) => {
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
