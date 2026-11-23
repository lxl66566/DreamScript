//! The net face's resolver helper (item 79's parked DNS twin, ruled
//! 2026-11-24): std's `to_socket_addrs` is a blocking getaddrinfo with
//! no non-blocking shape and no epoll-integrable twin (getaddrinfo_a
//! does not exist on musl), so under an installed reactor the staged
//! connect machine's step 0 runs the resolution on a dedicated helper
//! thread — one worker serving the connection table's rows, its
//! completion posting the eventfd whose readability the reactor
//! watches (the OS's own wake primitive, the epoll driver's
//! precedent). The worker's lifetime is the table's own: the
//! channel's far end drops with the [`crate::net::Connections`] it
//! rides, the worker finishing any in-flight job and exiting, its
//! join the drop tail — the fork-join pool's scope-join discipline,
//! so the thread never outlives the VM. The eventfd lives in the job
//! cell both the row and the worker hold, so a canceled wait's late
//! posting never writes a closed fd: the descriptor drops exactly
//! when the last holder does, and no row ever posts into a dead wake
//! — the interest deregisters with the wait's cancellation whichever
//! door ends it.

use std::{
    ffi::{c_int, c_void},
    io,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};

use crate::reactor::IoHandle;

// the kernel ABI constants: eventfd's exec-closed and non-blocking
// flags — stable ABI values, not headers
const EFD_CLOEXEC: c_int = 0x80000;
const EFD_NONBLOCK: c_int = 0o4000;

unsafe extern "C" {
    fn eventfd(init: u32, flags: c_int) -> c_int;
    #[link_name = "read"]
    fn sys_read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    #[link_name = "write"]
    fn sys_write(fd: c_int, buf: *const c_void, count: usize) -> isize;
}

/// The IP literal `host` names, the staged connect machine's inline
/// path (a literal resolves to itself, so no helper job and no
/// park: the machine starts at its connect stage exactly the
/// blocking twin's own resolution). `None` for a name.
#[must_use]
pub fn ip_literal(host: &str) -> Option<IpAddr> {
    host.parse::<IpAddr>().ok()
}

/// One resolution's shared cell, the row and the worker's own pairing:
/// the worker stores the answer and then writes the eventfd; the wake
/// drains the eventfd and takes the answer. The Arc pairing bounds the
/// eventfd's lifetime to the two holders, so a canceled row's fd stays
/// open until the worker's own post lands — never a write to a closed
/// fd, never a stale fd number the kernel could reuse under the
/// reactor.
#[derive(Debug)]
pub(crate) struct ResolveCell {
    /// The worker's answer: the resolved addresses or the failure's
    /// rendered detail, `None` until the worker posts.
    answer: Mutex<Option<Result<Vec<SocketAddr>, String>>>,
    /// The wake: the eventfd the reactor watches, posted once per
    /// resolution.
    wake: EventFd,
}

impl ResolveCell {
    /// One fresh cell with its unposted eventfd. Fails only if the
    /// kernel refuses the descriptor (the fd ceiling).
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            answer: Mutex::new(None),
            wake: EventFd::new()?,
        })
    }

    /// The registration key the resolve stage's interest watches.
    #[must_use]
    pub(crate) fn handle(&self) -> Option<IoHandle> {
        Some(IoHandle(u64::try_from(self.wake.fd.as_raw_fd()).ok()?))
    }

    /// The readable wake's consume: the eventfd drained, the worker's
    /// answer taken — `None` when the worker has not posted yet (the
    /// wait stands, level-triggered readiness reporting again once it
    /// does).
    pub(crate) fn take(&self) -> Option<Result<Vec<SocketAddr>, String>> {
        self.wake.drain();
        self.answer
            .lock()
            .expect("the worker posts, the wake takes")
            .take()
    }
}

/// The eventfd binding: exec-closed, non-blocking, one 8-byte counter
/// write per completion and one drain per readable wake — libc's own
/// symbols, the raw-socket ruling's OS-own-symbols discipline.
#[derive(Debug)]
struct EventFd {
    fd: OwnedFd,
}

impl EventFd {
    fn new() -> io::Result<Self> {
        // SAFETY: the plain eventfd call; a negative answer is the OS
        // refusal, a nonnegative one the fd the owned handle adopts
        let fd = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the fd is owned from here, closed exactly once
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    /// Posts the completion wake: one 8-byte counter write.
    fn post(&self) {
        let one = 1u64;
        // SAFETY: the plain write call over the stack counter
        let _ = unsafe {
            sys_write(
                self.fd.as_raw_fd(),
                std::ptr::from_ref(&one).cast::<c_void>(),
                8,
            )
        };
    }

    /// Drains the counter: the readable wake's consume, so a drained
    /// eventfd reports nothing until the worker posts again. The
    /// non-blocking read answers EAGAIN on an empty counter — nothing
    /// to take.
    fn drain(&self) {
        let mut sink = [0u8; 8];
        // SAFETY: the plain read call over the stack sink
        let _ = unsafe { sys_read(self.fd.as_raw_fd(), sink.as_mut_ptr().cast::<c_void>(), 8) };
    }
}

/// One queued resolution: the name, its port, and the cell the worker
/// posts into.
struct Job {
    host: String,
    port: u16,
    cell: Arc<ResolveCell>,
}

/// The dedicated helper thread: started by the first resolving connect
/// under the parking discipline, serving the connection table's rows
/// until the table drops its far end of the channel — the worker then
/// finishes any in-flight job, posts it, and exits, its join the drop
/// tail. No job ever touches VM state: the worker resolves and posts,
/// the drive reads.
#[derive(Debug)]
pub(crate) struct Resolver {
    jobs: Option<mpsc::Sender<Job>>,
    worker: Option<JoinHandle<()>>,
}

impl Resolver {
    /// Starts the helper thread. Fails only if the OS refuses the
    /// thread.
    pub(crate) fn start() -> io::Result<Self> {
        let (send, recv) = mpsc::channel::<Job>();
        let worker = std::thread::Builder::new()
            .name("ds-resolver".to_owned())
            .spawn(move || {
                // one job at a time, std's own resolver, the answer
                // posted behind the eventfd write
                while let Ok(job) = recv.recv() {
                    let answer = match (job.host.as_str(), job.port).to_socket_addrs() {
                        Ok(addrs) => {
                            let list = addrs.collect::<Vec<_>>();
                            if list.is_empty() {
                                Err("no address resolved".to_owned())
                            } else {
                                Ok(list)
                            }
                        },
                        Err(err) => Err(err.to_string()),
                    };
                    *job.cell
                        .answer
                        .lock()
                        .expect("the worker posts, the drive takes") = Some(answer);
                    job.cell.wake.post();
                }
            })
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(Self {
            jobs: Some(send),
            worker: Some(worker),
        })
    }

    /// Submits one resolution; the worker posts the cell's answer and
    /// its eventfd. Err only on a worker already gone (the table's
    /// own drop) or a closed channel.
    pub(crate) fn submit(&self, host: &str, port: u16, cell: Arc<ResolveCell>) -> io::Result<()> {
        self.jobs
            .as_ref()
            .ok_or_else(|| io::Error::other("the resolver worker is gone"))?
            .send(Job {
                host: host.to_owned(),
                port,
                cell,
            })
            .map_err(|err| io::Error::other(err.to_string()))
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        // the channel's far end closes first, so the worker's recv
        // errors out once its in-flight job posts; the join is the
        // thread's bounded exit — never a leak past the table's drop
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The worker's teardown: dropping the resolver closes the
    /// channel and joins the thread, the worker exiting once its
    /// in-flight job posts — no thread outlives the table it served
    /// (a stuck worker would hang this join, the timeout the
    /// witness).
    #[test]
    fn the_worker_exits_with_the_resolver_it_serves() {
        let mut resolver = Resolver::start().expect("the helper starts");
        let cell = Arc::new(ResolveCell::new().expect("the cell opens"));
        resolver
            .submit("localhost", 80, Arc::clone(&cell))
            .expect("the job submits");
        // the worker's join handle taken out before the drop: joining
        // it here pins the exit the drop itself would have waited on
        let worker = resolver.worker.take().expect("the worker stands");
        drop(resolver);
        worker.join().expect("the worker exited with its table");
        // the posted answer is readable behind the drained eventfd,
        // the row's own consume
        let mut posted = None;
        for _ in 0..1000 {
            if let Some(answer) = cell.take() {
                posted = Some(answer);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let answer = posted.expect("the in-flight job posted before the exit");
        let addrs = answer.expect("localhost resolves");
        assert!(
            addrs.iter().any(SocketAddr::is_ipv4),
            "localhost carries a v4 address: {addrs:?}"
        );
    }

    /// The cell's shape: a fresh cell holds no answer and answers no
    /// registration key's drain as a post; a posted cell's take
    /// answers exactly once, the empty second take the spurious
    /// wake's standing wait.
    #[test]
    fn a_cell_answers_only_the_posted_take() {
        let cell = ResolveCell::new().expect("the cell opens");
        assert!(cell.handle().is_some(), "the eventfd is a live fd");
        assert!(cell.take().is_none(), "nothing posted, nothing taken");
        let addr: SocketAddr = "127.0.0.1:80".parse().expect("the test address");
        *cell.answer.lock().expect("the worker posts") = Some(Ok(vec![addr]));
        assert_eq!(cell.take(), Some(Ok(vec![addr])));
        assert!(cell.take().is_none(), "the take is once");
    }

    /// The literal probe: dotted and coloned literals answer their own
    /// address, a name `None` — the inline path's own gate.
    #[test]
    fn literals_answer_their_address_and_names_nothing() {
        assert_eq!(
            ip_literal("127.0.0.1"),
            Some("127.0.0.1".parse::<IpAddr>().expect("the literal"))
        );
        assert_eq!(
            ip_literal("::1"),
            Some("::1".parse::<IpAddr>().expect("the literal"))
        );
        assert_eq!(ip_literal("0.0.0.0"), Some(IpAddr::from([0, 0, 0, 0])));
        assert_eq!(ip_literal("localhost"), None);
        assert_eq!(ip_literal("example.invalid"), None);
        assert_eq!(ip_literal(""), None);
    }
}
