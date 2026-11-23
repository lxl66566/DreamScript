//! The reactor behind the net/time faces (the 2026-11-21 scheduler
//! ruling): readiness over IO handles, installed through the capability
//! seam as VM state — an embedder that installs nothing keeps the
//! blocking natives, so blocking semantics stay a host choice and never
//! a language fact. The trait names the platform shapes (epoll on
//! Linux, kqueue and IOCP the other two named drivers; io_uring a later
//! swap behind the same trait), and the epoll driver is the first
//! implementation, over the OS's own C symbols with no reactor crate
//! between the binding and the system — nothing unvetted enters the
//! dependency policy's queue (the loader-binding precedent). Timers are
//! NOT the reactor's: the scheduler owns a deadline wheel in user space
//! and hands `poll` the distance to the nearest deadline as its
//! timeout, so every port implements the same timer discipline over
//! the one readiness primitive.

use std::time::Duration;

/// One watched IO handle: an OS-level resource identity (a file
/// descriptor on Unix, a socket handle on Windows), opaque to the
/// scheduler. Portability over std's split raw-handle types: a driver
/// converts once at its edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IoHandle(pub u64);

/// The reserved token of the host's shutdown interest (the
/// 2026-11-24 signal ruling): a host registers its own shutdown door
/// — Linux's signalfd — as one more reactor interest under this token,
/// and the drive reads its wake as the shutdown event, never a wait.
/// Minted wait tokens count up from zero, so the reservation can never
/// collide.
pub const SHUTDOWN_TOKEN: u64 = u64::MAX;

/// The readiness direction one interest watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The handle can be read without blocking.
    Read,
    /// The handle can be written without blocking.
    Write,
}

/// One readiness event the reactor answered: the registration's token
/// and the directions reported ready.
#[derive(Clone, Copy, Debug)]
pub struct TokenEvent {
    /// The token the interest registered under.
    pub token: u64,
    /// Readiness was reported for reading.
    pub read: bool,
    /// Readiness was reported for writing.
    pub write: bool,
    /// The handle's peer is gone (EPOLLHUP) or the handle errored
    /// (EPOLLERR): a read interest answers now — the pipe's orderly
    /// end or the error word — so a read-direction wake must take it.
    /// The kernel reports a pipe whose write end closed with an empty
    /// buffer as HUP alone, no IN bit.
    pub hup: bool,
}

/// A driver's refusal, rendered: the OS detail, never a panic — a
/// failed registration or poll surfaces to the program through its own
/// error channel (the net error span) instead of killing the drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactorError(pub String);

/// Readiness over IO handles: register an interest under a token,
/// drop it, and wait for events up to `timeout` (no timeout = block
/// until an event). Level-triggered semantics: a reported readiness
/// persists until consumed, so a spurious wake is answered by the
/// scheduler simply polling again — no event is ever lost to a missed
/// edge.
pub trait Reactor {
    /// Watches `handle` for `dir` readiness, reporting under `token`.
    /// A re-registration of the same handle replaces its interest.
    fn register(
        &mut self,
        handle: IoHandle,
        dir: Direction,
        token: u64,
    ) -> Result<(), ReactorError>;

    /// Drops every interest watched on `handle`. Total: an unknown
    /// handle is a no-op, so a cancellation racing a close stays clean.
    fn deregister(&mut self, handle: IoHandle) -> Result<(), ReactorError>;

    /// Waits for readiness up to `timeout`, appending events to
    /// `events` (cleared first): the one blocking call the
    /// single-thread executor makes when no fiber is runnable.
    fn poll(
        &mut self,
        timeout: Option<Duration>,
        events: &mut Vec<TokenEvent>,
    ) -> Result<(), ReactorError>;
}

/// The platform's default driver, `None` where none is implemented:
/// the epoll driver on Linux, nothing elsewhere yet (kqueue and IOCP
/// are the named next drivers; a host with no driver keeps the
/// blocking natives — installing no reactor is always a coherent
/// choice, never an error).
#[must_use]
pub fn platform_default() -> Option<Box<dyn Reactor>> {
    #[cfg(target_os = "linux")]
    {
        Epoll::new()
            .ok()
            .map(|epoll| Box::new(epoll) as Box<dyn Reactor>)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The epoll driver (Linux): one epoll set, level-triggered interests
/// keyed by token in the event's user data word, over the OS's own
/// `epoll_create1`/`epoll_ctl`/`epoll_wait` symbols.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Epoll {
    epfd: i32,
}

#[cfg(target_os = "linux")]
mod epoll_imp {
    //! The OS binding: three symbols off the platform libc, the same
    //! shape the ffi loader's binding takes — no reactor crate, no raw
    //! asm, no new dependency.
    pub use std::ffi::c_int;

    // the kernel ABI constants: ops and event bits, stable since 2.6
    pub const EPOLL_CTL_ADD: c_int = 1;
    pub const EPOLL_CTL_DEL: c_int = 2;
    pub const EPOLL_CTL_MOD: c_int = 3;
    pub const EPOLLIN: u32 = 0x001;
    pub const EPOLLOUT: u32 = 0x004;
    pub const EPOLLERR: u32 = 0x008;
    pub const EPOLLHUP: u32 = 0x010;
    pub const EPOLL_CLOEXEC: c_int = 0x80000;

    /// The kernel's epoll_event is `__attribute__((packed))`: 12 bytes,
    /// the u64 data word at offset 4. A natural-alignment repr(C)
    /// `{u32, u64}` puts its data at offset 8 and strides 16 bytes, so
    /// the kernel's packed writes and the reader's aligned reads
    /// disagree about where the token lives: a single-event batch
    /// reads its token from bytes that happen to hold it, but every
    /// earlier event of a multi-event batch has the NEXT event's
    /// kernel-written bits land in its data word's high half — a token
    /// the wait table never minted, its wake silently dropped. The
    /// three-u32 spelling keeps the 12-byte stride with no packed
    /// field references: the data word assembles from its halves.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct EpollEvent {
        pub events: u32,
        pub data_lo: u32,
        pub data_hi: u32,
    }

    impl EpollEvent {
        // the halves are the layout itself: packing a u64 token into the
        // kernel's offset-4 word splits it at 32 bits by construction
        #[allow(clippy::cast_possible_truncation)]
        pub fn new(events: u32, data: u64) -> Self {
            Self {
                events,
                data_lo: data as u32,
                data_hi: (data >> 32) as u32,
            }
        }

        pub fn data(&self) -> u64 {
            u64::from(self.data_lo) | (u64::from(self.data_hi) << 32)
        }
    }

    // the ABI contract this file stands on: one packed event, 12 bytes
    const _: () = assert!(size_of::<EpollEvent>() == 12);

    unsafe extern "C" {
        pub fn epoll_create1(flags: c_int) -> c_int;
        pub fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut EpollEvent) -> c_int;
        pub fn epoll_wait(
            epfd: c_int,
            events: *mut EpollEvent,
            maxevents: c_int,
            timeout: c_int,
        ) -> c_int;
    }
}

#[cfg(target_os = "linux")]
impl Epoll {
    /// One fresh epoll set. Fails only if the kernel refuses the
    /// descriptor (the fd ceiling).
    pub fn new() -> Result<Self, ReactorError> {
        // SAFETY: the epoll ABI's plain call; the returned fd is the
        // set's own handle, closed on drop.
        let epfd = unsafe { epoll_imp::epoll_create1(epoll_imp::EPOLL_CLOEXEC) };
        if epfd < 0 {
            return Err(ReactorError(format!(
                "epoll_create1: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self { epfd })
    }

    /// The ctl helper over one interest change.
    fn ctl(
        &self,
        op: epoll_imp::c_int,
        handle: IoHandle,
        events: u32,
        token: u64,
    ) -> Result<(), ReactorError> {
        let fd = i32::try_from(handle.0)
            .map_err(|_| ReactorError(format!("handle {} is no fd", handle.0)))?;
        let mut event = epoll_imp::EpollEvent::new(events, token);
        // SAFETY: the plain ctl call; `event` outlives the call.
        if unsafe { epoll_imp::epoll_ctl(self.epfd, op, fd, &raw mut event) } < 0 {
            return Err(ReactorError(format!(
                "epoll_ctl: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Reactor for Epoll {
    fn register(
        &mut self,
        handle: IoHandle,
        dir: Direction,
        token: u64,
    ) -> Result<(), ReactorError> {
        let bits = match dir {
            Direction::Read => epoll_imp::EPOLLIN,
            Direction::Write => epoll_imp::EPOLLOUT,
        };
        // ADD over an existing interest answers EEXIST; a replacing
        // registration (the scheduler re-arming one fd after a
        // direction change) rides MOD, so try ADD then MOD
        if self
            .ctl(epoll_imp::EPOLL_CTL_ADD, handle, bits, token)
            .is_err()
        {
            self.ctl(epoll_imp::EPOLL_CTL_MOD, handle, bits, token)?;
        }
        Ok(())
    }

    fn deregister(&mut self, handle: IoHandle) -> Result<(), ReactorError> {
        // DEL with a null event: an unknown fd answers ENOENT, the
        // total no-op the cancellation races want
        let fd = i32::try_from(handle.0)
            .map_err(|_| ReactorError(format!("handle {} is no fd", handle.0)))?;
        // SAFETY: the plain ctl call with no event payload.
        if unsafe {
            epoll_imp::epoll_ctl(
                self.epfd,
                epoll_imp::EPOLL_CTL_DEL,
                fd,
                std::ptr::null_mut(),
            )
        } < 0
        {
            let err = std::io::Error::last_os_error();
            if !matches!(
                err.raw_os_error(),
                Some(2 | 9) // ENOENT: never registered; EBADF: already closed
            ) {
                return Err(ReactorError(format!("epoll_ctl: {err}")));
            }
        }
        Ok(())
    }

    fn poll(
        &mut self,
        timeout: Option<Duration>,
        events: &mut Vec<TokenEvent>,
    ) -> Result<(), ReactorError> {
        events.clear();
        let mut raw = [epoll_imp::EpollEvent::new(0, 0); 64];
        let millis = match timeout {
            // the deadline wheel hands finite distances; epoll's own
            // rounding of a sub-millisecond remainder up to 1ms is the
            // finer wait available through this symbol
            Some(wait) => i32::try_from(wait.as_millis().min(i32::MAX as u128 - 1) + 1)
                .unwrap_or(i32::MAX - 1),
            None => -1,
        };
        // SAFETY: the plain wait call; `raw` holds the answers.
        // maxevents names the 64-slot batch's own length, a fixed small
        // bound the wait cannot exceed
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let n = unsafe {
            epoll_imp::epoll_wait(
                self.epfd,
                raw.as_mut_ptr(),
                raw.len() as epoll_imp::c_int,
                millis,
            )
        };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            // EINTR: no event arrived, the caller polls again — an
            // empty answer, never a refusal
            if err.raw_os_error() == Some(4) {
                return Ok(());
            }
            return Err(ReactorError(format!("epoll_wait: {err}")));
        }
        // a negative count was refused above, so the widening keeps
        // every remaining answer
        #[allow(clippy::cast_sign_loss)]
        for event in &raw[..n as usize] {
            events.push(TokenEvent {
                token: event.data(),
                read: event.events & epoll_imp::EPOLLIN != 0,
                write: event.events & epoll_imp::EPOLLOUT != 0,
                hup: event.events & (epoll_imp::EPOLLHUP | epoll_imp::EPOLLERR) != 0,
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for Epoll {
    fn drop(&mut self) {
        // SAFETY: the set's own fd, adopted by the owned handle that
        // closes it on drop — exactly once, never twice.
        {
            use std::os::fd::FromRawFd;
            drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(self.epfd) });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        os::fd::AsRawFd,
    };

    use super::*;

    /// A connected socket pair over a loopback listener: the local
    /// test's transport.
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let addr = listener.local_addr().expect("local address");
        let dialer = TcpStream::connect(addr).expect("dialer connects");
        let (acceptor, _) = listener.accept().expect("acceptor accepted");
        (dialer, acceptor)
    }

    fn handle(stream: &TcpStream) -> IoHandle {
        IoHandle(u64::try_from(stream.as_raw_fd()).expect("fd fits u64"))
    }

    /// A pipe whose writer closed with its buffer drained reports the
    /// hangup: the kernel spells that pipe's end HUP alone — no IN bit
    /// — so the decode must surface it as `hup`, a read wake answering
    /// the pipe's orderly end over it (the parked proc read's lost
    /// wake over a child's exit).
    #[test]
    fn a_drained_pipe_reports_its_hangup() {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the child spawns");
        let mut out = child.stdout.take().expect("the pipe's read end");
        child.wait().expect("the child exits");
        let pipe = IoHandle(u64::try_from(out.as_raw_fd()).expect("fd fits u64"));
        let mut reactor = Epoll::new().expect("epoll set");
        reactor
            .register(pipe, Direction::Read, 13)
            .expect("register");
        let mut events = Vec::new();
        reactor
            .poll(Some(Duration::from_millis(200)), &mut events)
            .expect("poll");
        assert_eq!(events.len(), 1, "the closed pipe reports");
        assert_eq!(events[0].token, 13);
        assert!(events[0].hup, "the drained pipe's end spells HUP");
        let mut buf = [0; 8];
        assert_eq!(out.read(&mut buf).expect("the hup pipe reads"), 0);
        drop(reactor);
        drop(out);
    }

    /// Readiness follows the data: a registered reader sits quiet
    /// until the peer writes, then reports, then goes quiet again once
    /// consumed.
    #[test]
    fn readiness_follows_the_data() {
        let (mut dialer, mut acceptor) = pair();
        acceptor.set_nonblocking(true).expect("nonblocking");
        let mut reactor = Epoll::new().expect("epoll set");
        reactor
            .register(handle(&acceptor), Direction::Read, 7)
            .expect("register");
        let mut events = Vec::new();
        // quiet: a zero timeout polls without blocking and answers
        // nothing
        reactor
            .poll(Some(Duration::from_millis(0)), &mut events)
            .expect("poll");
        assert!(events.is_empty(), "no data yet, no event");
        dialer.write_all(b"x").expect("peer writes");
        dialer.flush().expect("flush");
        // level-triggered: the event persists across polls until read
        for _ in 0..2 {
            reactor
                .poll(Some(Duration::from_millis(50)), &mut events)
                .expect("poll");
            assert_eq!(events.len(), 1, "the readable event stands");
            assert_eq!(events[0].token, 7);
            assert!(events[0].read);
        }
        let mut buf = [0; 8];
        assert_eq!(acceptor.read(&mut buf).expect("read"), 1);
        reactor
            .poll(Some(Duration::from_millis(0)), &mut events)
            .expect("poll");
        assert!(events.is_empty(), "consumed, quiet again");
        reactor.deregister(handle(&acceptor)).expect("deregister");
    }

    /// Write readiness: a fresh socket's send buffer has room, so a
    /// write interest reports immediately; deregistration is total.
    #[test]
    fn write_readiness_and_total_deregistration() {
        let (dialer, acceptor) = pair();
        dialer.set_nonblocking(true).expect("nonblocking");
        let mut reactor = Epoll::new().expect("epoll set");
        reactor
            .register(handle(&dialer), Direction::Write, 9)
            .expect("register");
        let mut events = Vec::new();
        reactor
            .poll(Some(Duration::from_millis(0)), &mut events)
            .expect("poll");
        assert_eq!(events.len(), 1, "the buffer has room");
        assert!(events[0].write);
        reactor.deregister(handle(&dialer)).expect("deregister");
        // an unknown handle's deregistration is the no-op
        reactor.deregister(handle(&dialer)).expect("total");
        reactor
            .poll(Some(Duration::from_millis(0)), &mut events)
            .expect("poll");
        assert!(events.is_empty(), "no interest stands");
        drop(acceptor);
    }

    /// A multi-event batch reports every token exactly: the kernel's
    /// packed epoll_event writes 12 bytes per event, so an aligned
    /// reader's earlier tokens once absorbed the next event's bits as
    /// their high half — a token the wait table never minted, its wake
    /// silently dropped (the parked-IO wake loss under load, fixed
    /// 2026-11-23). Two simultaneously-readable fds pin the contract.
    #[test]
    fn a_multi_event_batch_reports_every_token_exactly() {
        let (mut dialer_a, acceptor_a) = pair();
        let (mut dialer_b, acceptor_b) = pair();
        acceptor_a.set_nonblocking(true).expect("nonblocking");
        acceptor_b.set_nonblocking(true).expect("nonblocking");
        let mut reactor = Epoll::new().expect("epoll set");
        reactor
            .register(handle(&acceptor_a), Direction::Read, 11)
            .expect("register a");
        reactor
            .register(handle(&acceptor_b), Direction::Read, 0x7000_0002)
            .expect("register b");
        dialer_a.write_all(b"a").expect("a writes");
        dialer_b.write_all(b"b").expect("b writes");
        let _ = dialer_a.flush();
        let _ = dialer_b.flush();
        let mut events = Vec::new();
        reactor
            .poll(Some(Duration::from_millis(500)), &mut events)
            .expect("poll");
        assert_eq!(events.len(), 2, "both fds report in the one batch");
        let tokens: Vec<u64> = events.iter().map(|event| event.token).collect();
        // the high-bit token makes any bit-spill pollution loud: the
        // old layout folded the other event's EPOLLIN into token 11
        assert!(
            tokens.contains(&11) && tokens.contains(&0x7000_0002),
            "exact tokens, got {tokens:?}"
        );
        for event in &events {
            assert!(event.read);
        }
    }

    /// The blocking wait: a poll with no timeout answers the event the
    /// peer's write delivers from another thread.
    #[test]
    fn a_blocking_poll_answers_the_peer() {
        let (mut dialer, acceptor) = pair();
        acceptor.set_nonblocking(true).expect("nonblocking");
        let mut reactor = Epoll::new().expect("epoll set");
        reactor
            .register(handle(&acceptor), Direction::Read, 3)
            .expect("register");
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            dialer.write_all(b"late").expect("peer writes late");
            dialer
        });
        let mut events = Vec::new();
        reactor.poll(None, &mut events).expect("poll");
        assert_eq!(events.len(), 1, "the late write woke the wait");
        assert_eq!(events[0].token, 3);
        writer.join().expect("writer lands").flush().ok();
    }

    /// The platform default exists on Linux (the tier-1 host) and is
    /// the epoll driver.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_platform_default_is_epoll_on_linux() {
        let mut reactor = platform_default().expect("linux has a driver");
        // a registration round trip proves the box is live
        let (dialer, acceptor) = pair();
        acceptor.set_nonblocking(true).expect("nonblocking");
        reactor
            .register(handle(&acceptor), Direction::Read, 1)
            .expect("register through the box");
        drop(dialer);
        drop(acceptor);
    }
}
