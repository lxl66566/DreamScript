use std::net::TcpStream;

/// The raw OS socket binding of the parked connect: libc's own symbols — `socket`,
/// `fcntl(O_NONBLOCK)`, `connect`, and the completion probe — over no socket library, the epoll
/// driver's OS-own-symbols precedent. The parked connect belongs to the Linux driver; any other
/// host keeps the blocking twin under whatever reactor it installs.
#[cfg(target_os = "linux")]
pub(super) mod raw_sock {
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
pub(super) fn fd_into_stream(fd: std::os::fd::OwnedFd) -> TcpStream {
    use std::os::fd::{FromRawFd as _, IntoRawFd as _};
    // SAFETY: the fd is owned and closed exactly once — adopted by the
    // stream that takes it over
    unsafe { TcpStream::from_raw_fd(fd.into_raw_fd()) }
}

/// The OS-level registration key of one stream not yet owned by a
/// table (the accept-tls machine's accepted stream) — the raw_handle
/// twins' own rendering.
pub(crate) fn stream_handle(stream: &TcpStream) -> Option<crate::reactor::IoHandle> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        u64::try_from(stream.as_raw_fd())
            .ok()
            .map(crate::reactor::IoHandle)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        u64::try_from(stream.as_raw_socket())
            .ok()
            .map(crate::reactor::IoHandle)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}
