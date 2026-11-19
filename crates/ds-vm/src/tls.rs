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
//! blocking foreign call pinning its fiber.

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
