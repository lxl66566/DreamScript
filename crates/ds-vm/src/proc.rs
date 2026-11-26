//! The process face of the CLI process surface (ruled 2026-11-21, H5):
//! the `proc` capability's ops — string argv, environment-variable
//! read, process spawn, and the deliberate-status `exit(code)` row —
//! served by the platform handler the host installs. Every op is total
//! at the type level: answers ride union spans (`args` is total by
//! contract, `env` and the spawn read answer the checked/fallible
//! spans), and the only trap the face raises is the deliberate one —
//! `exit`'s code riding out like `die`'s message, the host's exit-code
//! policy above the engine.
//!
//! The capture half answers the linear `proc_handle`: `proc_spawn`
//! opens the child over pipes (stdin, stdout, and stderr piped) and
//! registers it in the process table the output sink owns — the
//! connection and listener tables' own lifetime rule — so
//! `proc_write` hands the child input, `proc_read` takes its captured
//! output (the empty string the pipe's orderly end), `proc_read_err`
//! its captured diagnostics the same way, and `proc_wait` answers the
//! exit status idempotently (`proc_try_wait` the non-blocking poll
//! over the same reaped word). The blocking rows park on Linux under
//! an installed reactor (the 2026-11-23 ruling): a child's pipe IS an
//! fd, so `proc_read`/`proc_read_err` watch its readability, and the
//! exit rides a pidfd (`pidfd_open`, readable once the child exits) —
//! the net face's own parked-twin discipline, the wake performing the
//! op once and delivering its answer at the parked site. A kernel
//! without pidfd (or a non-Linux host, or no reactor) keeps the
//! blocking twin: the read rows block the calling fiber exactly
//! `proc_run`'s own wait, and the wait twin polls the child on a
//! bounded sleep loop — a waitpid owns no portable interrupt shape,
//! so the host's stop flag cuts a blocked wait within one interval. `proc_close_in` is the
//! stdin-close seam — the child's end-of-input while the handle stays live — and
//! `proc_close` the raw idempotent-close seam the `proc_handle` deinit
//! pairs with: a close of a still-running child kills and reaps it, so
//! a dropped handle leaks neither a child nor a zombie.

use std::{
    io::{ErrorKind, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ds_abi::SlotType;

use crate::{
    native::{
        NativeResult, OutputSink, PLATFORM_IO_OK, PLATFORM_IO_TIMED_OUT, PLATFORM_NONE, i64_arg,
        io_err, ref_arg,
    },
    object::Heaps,
    sched::ProcPipe,
    value::Value,
    vm::Trap,
};

/// `args() -> list[string]`: the process arguments the host staged on
/// the sink (`ds run`'s `--args` words, verbatim). Total by contract —
/// a host staging nothing answers the empty list.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn args_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let mut elems = Vec::with_capacity(out.argv().len());
    for arg in out.argv() {
        elems.push(heaps.new_str(arg.clone()));
    }
    // each fresh element's birth count becomes the list's edge, the
    // fs_list precedent
    for elem in &elems {
        if let Value::Ref(word) = *elem {
            heaps.release_claim(word);
        }
    }
    Ok(NativeResult::Word(heaps.new_list(elems)))
}

/// `env(name) -> string | none`: the process environment variable's
/// value, the checked ok/none span — an unset variable (or one whose
/// value is not valid Unicode) answers the none member, never a trap.
pub(crate) fn env_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let name = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::env::var(&name) {
        Ok(value) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(value),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
    })
}

/// `proc_run(cmd, args) -> int | io_err`: spawn one child process over
/// the argument list, inheriting the parent's standard streams, and
/// answer its exit status — a spawn failure answers the io failure
/// span, and a child ended without a status code (a signal on Unix)
/// answers -1. Output capture stays out of the face: the child
/// inherits, so its output lands exactly where the parent's would.
pub(crate) fn proc_run_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let cmd = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    let elems = heaps.list_at(ref_arg(args, 1)?)?.0.to_vec();
    let mut child_args = Vec::with_capacity(elems.len());
    for elem in elems {
        let Value::Ref(word) = elem else {
            return Err(Trap::SlotTypeMismatch {
                slot: 1,
                expected: SlotType::Ref,
                actual: elem.slot_type(),
            });
        };
        child_args.push(heaps.str_at(word)?.to_owned());
    }
    let status = std::process::Command::new(&cmd).args(&child_args).status();
    Ok(match status {
        Ok(exit) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::from(exit.code().unwrap_or(-1))),
        },
        Err(err) => io_err(heaps, "proc_run", &cmd, &err),
    })
}

/// The `exit(code)` deliberate-status row: the process status rides the
/// trap out to the drive, the installing host's exit-code policy above
/// it — the engine decides none (the die split's own rule). A
/// negative status traps (ruled 2026-11-22, the trapping-arithmetic
/// precedent: a deliberate failure never reads as clean success), the
/// message riding the Die trap out like the u32 range failure's own
/// note; a wider status wraps to its low byte, POSIX process
/// semantics. Never answers.
pub(crate) fn exit_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let code = i64_arg(args, 0)?;
    if code < 0 {
        return Err(Trap::Die {
            message: format!("exit: {code} is out of range 0..=255"),
        });
    }
    Err(Trap::Exit { code: code & 0xff })
}

/// The per-call byte cap of `proc_read` (a larger request streams
/// across calls, the `net_recv` discipline).
pub const PROC_CAP: usize = 1024 * 1024;

/// The default bound on every parked proc wait — the far backstop: the
/// blocking twins are unbounded (a wait sees any child out), so the
/// parked twin's default keeps every realistic wait unbounded too
/// while the deadline wheel still holds its wake guarantee. A host
/// wanting the net face's tight bound sets it
/// ([`crate::Vm::set_proc_timeout`]), the deadline then answering the
/// timeout span.
pub const PROC_PARK_TIMEOUT: Duration = Duration::from_hours(24);

/// The blocking wait twin's poll interval: a waitpid owns no portable
/// interrupt shape (the parked twin's pidfd is Linux's own), so the
/// twin polls the child non-blockingly on a bounded sleep loop and the
/// host's stop flag cuts a blocked wait within one interval — the
/// accept twin's own poll discipline, the per-wait wakeup latency the
/// cost a reactor-less embedder's blocking fallback pays.
const PROC_WAIT_POLL: Duration = Duration::from_millis(10);

/// The process face's raw OS binding (the 2026-11-23 ruling):
/// `pidfd_open` for the exit interest and `fcntl(O_NONBLOCK)` for the
/// pipes — libc's own symbols over no crate, the epoll driver's
/// OS-own-symbols precedent (`crate::net`'s raw_sock the same
/// discipline). The parked twins are the linux reactor's own this
/// round; a non-linux host keeps the blocking twins under any reactor.
#[cfg(target_os = "linux")]
mod raw_proc {
    use std::{
        ffi::{c_int, c_long},
        io,
        os::fd::{FromRawFd as _, OwnedFd},
    };

    // the kernel ABI constants: the fcntl commands and the
    // non-blocking flag, the pidfd_open syscall number (the generic
    // table every live arch shares, Linux 5.3's own)
    pub const F_GETFL: c_int = 3;
    pub const F_SETFL: c_int = 4;
    pub const O_NONBLOCK: c_int = 0o4000;
    const SYS_PIDFD_OPEN: c_long = 434;

    unsafe extern "C" {
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        fn syscall(num: c_long, ...) -> c_long;
    }

    /// Opens the exit-interest fd of `pid`: readable once the process
    /// exits (a zombie included), epoll-able like any fd, and
    /// close-on-exec by the kernel's own default. An error names a
    /// kernel without pidfd (EINVAL/ENOSYS) or a refused fd — the
    /// caller's blocking twin the answer either way.
    pub fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
        // SAFETY: the plain syscall trap; a negative answer is the OS
        // refusal, a nonnegative one the fd the owned handle adopts
        let fd = unsafe { syscall(SYS_PIDFD_OPEN, c_long::from(pid), 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = c_int::try_from(fd).map_err(|_| io::Error::other("pidfd out of range"))?;
        // SAFETY: the fd is owned from here, closed exactly once
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Flips one pipe fd non-blocking (idempotent): F_GETFL's own set
    /// merged with the flag, so no other disposition is lost.
    pub fn set_nonblocking(fd: c_int) -> io::Result<()> {
        // SAFETY: the plain flag read
        let flags = unsafe { fcntl(fd, F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the plain flag write over the merged set
        if unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// One spawned child of the capture half: the process, the parent's
/// ends of its pipes, and the reaped status once a wait answered
/// (`None` while the child runs).
#[derive(Debug)]
struct ProcEntry {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    status: Option<i64>,
    /// The exit-interest fd of the parked wait twin, opened lazily at
    /// the first parked wait and living with the entry — the pipes'
    /// own lifetime rule, closed at the entry's close.
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
}

/// The process table of the capture half: the piped children
/// `proc_spawn` opens, keyed by the monotonic handle ids it hands out.
/// VM state, not heap state: the table rides the output sink (the
/// connection and listener tables' own lifetime rule), and a close
/// kills and reaps the child, so a dropped VM leaks neither a running
/// child nor a zombie. Handles are monotonic and never reused, so a
/// closed (or never-opened) id names no child and every later native
/// on it answers the io floor "unknown process". `close` is total and
/// idempotent: an unknown handle is a no-op (the deinit pairing's
/// contract, `close_raw`'s own).
#[derive(Debug)]
pub(crate) struct Procs {
    table: Vec<(i64, ProcEntry)>,
    next_id: i64,
    /// Whether the blocking rows park (a reactor was installed on a
    /// Linux host): the read rows answer park requests on a
    /// would-block, the wait row on the child's not-yet-exit. The
    /// pipes own no portable non-blocking shape, so the flag flips on
    /// Linux alone — any other host keeps the blocking twins under
    /// whatever reactor it installs.
    parking: bool,
    /// The parked rows' wait bound ([`PROC_PARK_TIMEOUT`] the
    /// default), the blocking twin's own unbounded wait restated as
    /// the wheel's far backstop.
    timeout: Duration,
    /// The host's blocking-drive stop flag (the 2026-11-24 signal
    /// ruling's non-reactor twin): the blocking wait twin observes it
    /// between its poll rounds and answers the `Interrupted` marker
    /// the caller maps to the shutdown trap. A host that installs
    /// none keeps today's behavior — the flag stands false forever.
    stop: Arc<AtomicBool>,
}

impl Default for Procs {
    fn default() -> Self {
        Self {
            table: Vec::new(),
            next_id: 0,
            parking: false,
            timeout: PROC_PARK_TIMEOUT,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Procs {
    /// Registers `child` (stdin/stdout already taken out of it) and
    /// answers its handle. Used by `proc_spawn`.
    pub(crate) fn register(&mut self, mut child: std::process::Child) -> i64 {
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let id = self.next_id;
        self.next_id = id + 1;
        self.table.push((id, ProcEntry {
            child,
            stdin,
            stdout,
            stderr,
            status: None,
            #[cfg(target_os = "linux")]
            pidfd: None,
        }));
        id
    }

    /// Flips the blocking rows to the parking discipline (a reactor
    /// was installed on a Linux host); any other platform keeps the
    /// blocking twins — the pipes own no portable non-blocking shape.
    pub(crate) fn enable_parking(&mut self) {
        #[cfg(target_os = "linux")]
        {
            self.parking = true;
        }
        #[cfg(not(target_os = "linux"))]
        {
            // the blocking twin stays: no portable pipe interest
            let _ = self;
        }
    }

    /// Whether the blocking rows park (a Linux reactor stands).
    pub(crate) fn parking(&self) -> bool {
        self.parking
    }

    /// The parked rows' wait bound.
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Bounds every parked proc wait (the blocking twins stay
    /// unbounded whatever the bound).
    pub(crate) fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Installs the host's blocking-drive stop flag
    /// ([`crate::Vm::set_shutdown_flag`]).
    pub(crate) fn set_stop_flag(&mut self, stop: Arc<AtomicBool>) {
        self.stop = stop;
    }

    /// The entry of handle `pid`.
    fn entry(&mut self, pid: i64) -> std::io::Result<&mut ProcEntry> {
        self.table
            .iter_mut()
            .find(|(id, _)| *id == pid)
            .map(|(_, entry)| entry)
            .ok_or_else(|| unknown_process(pid))
    }

    /// Writes `data` to handle `pid`'s child on its stdin pipe,
    /// blocking while the pipe buffer is full (the child's own
    /// backpressure).
    pub(crate) fn write(&mut self, pid: i64, data: &[u8]) -> std::io::Result<()> {
        let entry = self.entry(pid)?;
        let Some(stdin) = entry.stdin.as_mut() else {
            return Err(std::io::Error::other(format!(
                "process {pid} stdin already closed"
            )));
        };
        stdin.write_all(data)
    }

    /// Closes handle `pid`'s stdin pipe — the child's end-of-input —
    /// while the handle stays live. Total and idempotent like the
    /// closing deinit whose kill it spares: an unknown handle is a
    /// no-op, and a stdin already closed changes nothing.
    pub(crate) fn close_in(&mut self, pid: i64) {
        if let Some((_, entry)) = self.table.iter_mut().find(|(id, _)| *id == pid) {
            entry.stdin = None;
        }
    }

    /// Reads at most `max` bytes from handle `pid`'s child on its
    /// stdout pipe: one blocking read, so an empty answer means the
    /// child's orderly end (the child exited or closed its stdout).
    pub(crate) fn read(&mut self, pid: i64, max: usize) -> std::io::Result<Vec<u8>> {
        let entry = self.entry(pid)?;
        let Some(stdout) = entry.stdout.as_mut() else {
            return Err(std::io::Error::other(format!(
                "process {pid} stdout already closed"
            )));
        };
        let mut buf = vec![0; max];
        match stdout.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Ok(buf)
            },
            Err(err) => Err(err),
        }
    }

    /// Reads at most `max` bytes from handle `pid`'s child on its
    /// stderr pipe: one blocking read, so an empty answer means the
    /// child's orderly end (the child exited or closed its stderr).
    pub(crate) fn read_err(&mut self, pid: i64, max: usize) -> std::io::Result<Vec<u8>> {
        let entry = self.entry(pid)?;
        let Some(stderr) = entry.stderr.as_mut() else {
            return Err(std::io::Error::other(format!(
                "process {pid} stderr already closed"
            )));
        };
        let mut buf = vec![0; max];
        match stderr.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Ok(buf)
            },
            Err(err) => Err(err),
        }
    }

    /// One non-blocking read of at most `max` bytes from handle
    /// `pid`'s child on `pipe` — the parked twin of
    /// [`Procs::read`]/[`Procs::read_err`]: the pipe flips
    /// non-blocking at the attempt (the flag living on the open file
    /// description, the flip idempotent), the would-block answering
    /// `Park` (the reactor owns the wait over the pipe's own
    /// readability).
    #[cfg(target_os = "linux")]
    pub(crate) fn try_read(
        &mut self,
        pid: i64,
        pipe: ProcPipe,
        max: usize,
    ) -> crate::net::NetAttempt<Vec<u8>> {
        use std::os::fd::AsRawFd as _;
        let entry = match self.entry(pid) {
            Ok(entry) => entry,
            Err(err) => return crate::net::NetAttempt::Done(Err(err)),
        };
        let mut buf = vec![0; max];
        let read = match (pipe, entry.stdout.as_mut(), entry.stderr.as_mut()) {
            (ProcPipe::Out, Some(out), _) => {
                if let Err(err) = raw_proc::set_nonblocking(out.as_raw_fd()) {
                    return crate::net::NetAttempt::Done(Err(err));
                }
                out.read(&mut buf)
            },
            (ProcPipe::Err, _, Some(err_end)) => {
                if let Err(err) = raw_proc::set_nonblocking(err_end.as_raw_fd()) {
                    return crate::net::NetAttempt::Done(Err(err));
                }
                err_end.read(&mut buf)
            },
            (pipe, ..) => {
                let which = match pipe {
                    ProcPipe::Out => "stdout",
                    ProcPipe::Err => "stderr",
                };
                return crate::net::NetAttempt::Done(Err(std::io::Error::other(format!(
                    "process {pid} {which} already closed"
                ))));
            },
        };
        match read {
            Ok(n) => {
                buf.truncate(n);
                crate::net::NetAttempt::Done(Ok(buf))
            },
            Err(err) if err.kind() == ErrorKind::WouldBlock => crate::net::NetAttempt::Park,
            Err(err) => crate::net::NetAttempt::Done(Err(err)),
        }
    }

    /// The staged wait of the parked `wait` twin: the status answers
    /// now (the reaped word or the failure), `Park` once the pidfd
    /// stands (the registration resolves it), `Block` where the kernel
    /// refuses a pidfd (the blocking twin the answer — Linux before
    /// 5.3, or a refused fd).
    #[cfg(target_os = "linux")]
    pub(crate) fn try_wait_park(&mut self, pid: i64) -> ProcWaitAttempt {
        let Some(entry) = self
            .table
            .iter_mut()
            .find(|(id, _)| *id == pid)
            .map(|(_, entry)| entry)
        else {
            return ProcWaitAttempt::Done(Err(unknown_process(pid)));
        };
        if let Some(status) = entry.status {
            return ProcWaitAttempt::Done(Ok(status));
        }
        match entry.child.try_wait() {
            Ok(Some(exit)) => {
                let status = i64::from(exit.code().unwrap_or(-1));
                entry.status = Some(status);
                ProcWaitAttempt::Done(Ok(status))
            },
            // the child runs: the exit interest rides the pidfd, opened
            // lazily here and living with the entry (the pipes' own
            // lifetime rule)
            Ok(None) => {
                if entry.pidfd.is_none() {
                    match raw_proc::pidfd_open(entry.child.id()) {
                        Ok(fd) => entry.pidfd = Some(fd),
                        Err(_) => return ProcWaitAttempt::Block,
                    }
                }
                ProcWaitAttempt::Park
            },
            Err(err) => ProcWaitAttempt::Done(Err(err)),
        }
    }

    /// The read interest's IO handle: the child pipe's own fd (the
    /// registration site of a parked read).
    #[cfg(target_os = "linux")]
    pub(crate) fn raw_pipe_handle(
        &mut self,
        pid: i64,
        pipe: ProcPipe,
    ) -> Option<crate::reactor::IoHandle> {
        let entry = self.entry(pid).ok()?;
        let fd = match pipe {
            ProcPipe::Out => entry.stdout.as_ref().map(std::os::fd::AsRawFd::as_raw_fd),
            ProcPipe::Err => entry.stderr.as_ref().map(std::os::fd::AsRawFd::as_raw_fd),
        }?;
        Some(crate::reactor::IoHandle(u64::try_from(fd).ok()?))
    }

    /// The exit interest's IO handle: the entry's own pidfd, opened by
    /// the parked wait's staging.
    #[cfg(target_os = "linux")]
    pub(crate) fn raw_exit_handle(&mut self, pid: i64) -> Option<crate::reactor::IoHandle> {
        use std::os::fd::AsRawFd as _;
        let entry = self.entry(pid).ok()?;
        let fd = entry.pidfd.as_ref()?;
        Some(crate::reactor::IoHandle(
            u64::try_from(fd.as_raw_fd()).ok()?,
        ))
    }

    /// Waits for handle `pid`'s child and answers its exit status — a
    /// child ended without a status code (a signal on Unix) answering
    /// -1, `proc_run`'s own convention. Idempotent: a later wait
    /// answers the reaped status again without touching the child, so
    /// the deinit's close after a wait stays clean. The blocking twin
    /// polls the child non-blockingly on a bounded sleep loop (a
    /// waitpid owns no portable interrupt shape), the host's stop flag
    /// answering the `Interrupted` marker between rounds — the wait
    /// stays unbounded for a live child exactly `proc_run`'s own.
    pub(crate) fn wait(&mut self, pid: i64) -> std::io::Result<i64> {
        let stopped = Arc::clone(&self.stop);
        let entry = self.entry(pid)?;
        if let Some(status) = entry.status {
            return Ok(status);
        }
        let status = loop {
            match entry.child.try_wait() {
                Ok(Some(exit)) => break i64::from(exit.code().unwrap_or(-1)),
                Ok(None) => {
                    if stopped.load(Ordering::Relaxed) {
                        return Err(std::io::Error::new(ErrorKind::Interrupted, "host shutdown"));
                    }
                    std::thread::sleep(PROC_WAIT_POLL);
                },
                Err(err) => return Err(err),
            }
        };
        entry.status = Some(status);
        Ok(status)
    }

    /// Polls handle `pid`'s child without blocking: the exit status
    /// once the child exited (recorded so a later `wait` answers the
    /// reaped word again without touching the child), `None` while it
    /// runs. An unknown handle and a poll error answer `None` too —
    /// the checked span carries no failure member, so the rare error
    /// folds the way `env` folds its own.
    pub(crate) fn try_wait(&mut self, pid: i64) -> Option<i64> {
        let entry = self.entry(pid).ok()?;
        if let Some(status) = entry.status {
            return Some(status);
        }
        match entry.child.try_wait() {
            Ok(Some(exit)) => {
                let status = i64::from(exit.code().unwrap_or(-1));
                entry.status = Some(status);
                Some(status)
            },
            // a still-running child, and the poll error the checked
            // span cannot spell
            Ok(None) | Err(_) => None,
        }
    }

    /// Closes handle `pid` and removes it from the table; later uses
    /// of the handle answer the io floor. Total and idempotent: an
    /// unknown handle is a no-op (the deinit pairing's contract), and
    /// a close of a still-running child kills and reaps it — a dropped
    /// handle never leaks a child or a zombie.
    pub(crate) fn close(&mut self, pid: i64) {
        let Some((_, entry)) = self.table.iter_mut().find(|(id, _)| *id == pid) else {
            return;
        };
        if entry.status.is_none() {
            let _ = entry.child.kill();
            let _ = entry.child.wait();
        }
        entry.stdin = None;
        entry.stdout = None;
        entry.stderr = None;
        self.table.retain(|(id, _)| *id != pid);
    }
}

/// The "unknown process" error, shared by every handle lookup and the
/// close-wake door's exit-wait answer (the blocking twin's post-close
/// answer).
pub(crate) fn unknown_process(pid: i64) -> std::io::Error {
    std::io::Error::other(format!("unknown process {pid}"))
}

/// The staged wait of the parked `wait` twin's one attempt.
#[cfg(target_os = "linux")]
pub(crate) enum ProcWaitAttempt {
    /// The wait answers now: the reaped status or the lookup failure.
    Done(std::io::Result<i64>),
    /// The pidfd stands: park on its readability.
    Park,
    /// No pidfd (the kernel refuses one): the blocking twin answers.
    Block,
}

/// `proc_spawn(cmd, args) -> proc_handle` (the ambient's total
/// spelling): spawn one child over the argument list with its stdin,
/// stdout, and stderr piped and register it in the process table,
/// answering its handle's word — a spawn failure answers the io
/// failure span, the total spelling's printing halt (`net_listen`'s
/// own pairing). Output capture rides the pipes the handle's
/// write/read rows address; `proc_run` stays inherit-only.
pub(crate) fn proc_spawn_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let cmd = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    let elems = heaps.list_at(ref_arg(args, 1)?)?.0.to_vec();
    let mut child_args = Vec::with_capacity(elems.len());
    for elem in elems {
        let Value::Ref(word) = elem else {
            return Err(Trap::SlotTypeMismatch {
                slot: 1,
                expected: SlotType::Ref,
                actual: elem.slot_type(),
            });
        };
        child_args.push(heaps.str_at(word)?.to_owned());
    }
    let spawned = std::process::Command::new(&cmd)
        .args(&child_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    Ok(match spawned {
        Ok(child) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(out.procs().register(child)),
        },
        Err(err) => io_err(heaps, "proc_spawn", &cmd, &err),
    })
}

/// `proc_write(pid, data) -> unit | io_err`: writes `data` to the
/// child's stdin pipe, blocking while the pipe is full (the child's
/// backpressure). The pipe stays open across calls, so a child
/// reading its stdin to the end sees no end-of-input until
/// [`proc_close_in_fn`] or the handle's closing deinit runs; a write
/// after either answers the "stdin already closed" floor.
pub(crate) fn proc_write_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let pid = i64_arg(args, 0)?;
    let data = heaps.str_at(ref_arg(args, 1)?)?.to_owned();
    Ok(match out.procs().write(pid, data.as_bytes()) {
        // the payload word is the inert unit word, like net_send's
        Ok(()) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(0),
        },
        Err(err) => io_err(heaps, "proc_write", &pid.to_string(), &err),
    })
}

/// The read rows' answer shape, shared by the blocking twins and the
/// parked wakes: a string native answers a string, so the empty string
/// is the pipe's orderly end while mid-code-point truncation and
/// binary output surface as errors, not as silently mangled text.
pub(crate) fn read_answer(heaps: &mut Heaps, op: &str, pid: i64, bytes: Vec<u8>) -> NativeResult {
    match String::from_utf8(bytes) {
        Ok(text) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(text),
        },
        Err(_) => io_err(
            heaps,
            op,
            &pid.to_string(),
            &std::io::Error::other("invalid UTF-8 in the captured bytes"),
        ),
    }
}

/// The parked rows' timeout span, the deadline path's answer: the net
/// face's own bounded-wait discipline over the process face's io
/// member shape.
pub(crate) fn timeout_err(
    heaps: &mut Heaps,
    op: &str,
    target: &str,
    bound: Duration,
) -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_IO_TIMED_OUT,
        payload: heaps.new_str(format!("{op} {target}: timed out after {bound:?}")),
    }
}

/// The read rows' one step over `pipe`, the blocking twin and the
/// parked twin the two halves: one read of at most `max_bytes` bytes
/// (clamped to [`PROC_CAP`]), the parking discipline answering the
/// park request on a would-block where a Linux reactor stands.
fn read_step(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    pid: i64,
    max: i64,
    pipe: ProcPipe,
) -> NativeResult {
    let op = pipe.op_name();
    if max < 1 {
        return io_err(
            heaps,
            op,
            &pid.to_string(),
            &std::io::Error::other("max_bytes must be positive"),
        );
    }
    let max = usize::try_from(max).unwrap_or(PROC_CAP).min(PROC_CAP);
    #[cfg(target_os = "linux")]
    if out.procs().parking() {
        // the parking discipline: one non-blocking attempt, the
        // would-block answered as the park request (the scheduler owns
        // the wait over the pipe's own readability, the process face's
        // bound the deadline)
        return match out.procs().try_read(pid, pipe, max) {
            crate::net::NetAttempt::Done(Ok(bytes)) => read_answer(heaps, op, pid, bytes),
            crate::net::NetAttempt::Done(Err(err)) => io_err(heaps, op, &pid.to_string(), &err),
            crate::net::NetAttempt::Park => {
                let deadline = std::time::Instant::now() + out.procs().timeout();
                NativeResult::Park(crate::sched::WorldWait::proc_read(pid, pipe, max, deadline))
            },
        };
    }
    let bytes = match pipe {
        ProcPipe::Out => out.procs().read(pid, max),
        ProcPipe::Err => out.procs().read_err(pid, max),
    };
    match bytes {
        Ok(bytes) => read_answer(heaps, op, pid, bytes),
        Err(err) => io_err(heaps, op, &pid.to_string(), &err),
    }
}

/// `proc_read(pid, max_bytes) -> string | io_err`: one read of at most
/// `max_bytes` bytes from the child's stdout pipe (clamped to
/// [`PROC_CAP`]); the empty string is the pipe's orderly end. A
/// string native answers a string, so mid-code-point truncation and
/// binary output surface as errors, not as silently mangled text.
pub(crate) fn proc_read_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(read_step(
        heaps,
        out,
        i64_arg(args, 0)?,
        i64_arg(args, 1)?,
        ProcPipe::Out,
    ))
}

/// `proc_wait(pid) -> int | io_err`: waits for the child and answers
/// its exit status — a child ended without a status code answering -1,
/// `proc_run`'s convention — idempotently, so a second wait answers
/// the reaped status again. Under a Linux reactor the wait parks on
/// the child's pidfd (the readable wake reaping the status
/// non-blockingly); a kernel refusing a pidfd, a non-Linux host, or a
/// host with no reactor keeps the blocking twin below — the wait
/// polling the child on a bounded sleep loop, so a flipped stop flag
/// answers the shutdown trap within one interval.
pub(crate) fn proc_wait_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let pid = i64_arg(args, 0)?;
    #[cfg(target_os = "linux")]
    if out.procs().parking() {
        match out.procs().try_wait_park(pid) {
            ProcWaitAttempt::Done(Ok(status)) => {
                return Ok(NativeResult::Tag {
                    tag: PLATFORM_IO_OK,
                    payload: Value::I64(status),
                });
            },
            ProcWaitAttempt::Done(Err(err)) => {
                return Ok(io_err(heaps, "proc_wait", &pid.to_string(), &err));
            },
            ProcWaitAttempt::Park => {
                let deadline = std::time::Instant::now() + out.procs().timeout();
                return Ok(NativeResult::Park(crate::sched::WorldWait::proc_wait(
                    pid, deadline,
                )));
            },
            // the kernel refuses a pidfd: the blocking twin below
            ProcWaitAttempt::Block => {},
        }
    }
    Ok(match out.procs().wait(pid) {
        Ok(status) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(status),
        },
        // the stop flag's marker between poll rounds: the shutdown
        // trap exactly the accept twin's own cut
        Err(err) if err.kind() == ErrorKind::Interrupted => return Err(Trap::Shutdown),
        Err(err) => io_err(heaps, "proc_wait", &pid.to_string(), &err),
    })
}

/// `proc_close_in(pid) -> ()`: closes the child's stdin pipe — the
/// child's end-of-input — while the handle stays live, so a child
/// reading its stdin to the end sees the orderly end without the
/// whole handle closing. Total and idempotent like the close it
/// narrows: an unknown handle is a no-op, and a stdin already closed
/// changes nothing (the write row's own floor names the second close).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn proc_close_in_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(pid) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.procs().close_in(pid);
    }
    // the inert unit word
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `proc_read_err(pid, max_bytes) -> string | io_err`: one read of at
/// most `max_bytes` bytes from the child's stderr pipe (clamped to
/// [`PROC_CAP`]), `proc_read`'s own discipline — the empty string is
/// the pipe's orderly end, and mid-code-point truncation or binary
/// diagnostics surface as errors.
pub(crate) fn proc_read_err_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(read_step(
        heaps,
        out,
        i64_arg(args, 0)?,
        i64_arg(args, 1)?,
        ProcPipe::Err,
    ))
}

/// `proc_try_wait(pid) -> int | none`: the child's exit status under
/// the ok member once it exited — the already-reaped word a later
/// `wait` answers again — and the none member while it runs. The
/// blocking wait stays `proc_wait`'s own; this row never parks the
/// calling fiber. An unknown handle answers none: the checked span
/// carries no failure member, and the linear handle keeps its id
/// live, so no handle a caller holds ever names one.
pub(crate) fn proc_try_wait_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let pid = i64_arg(args, 0)?;
    Ok(match out.procs().try_wait(pid) {
        Some(status) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(status),
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
    })
}

/// `proc_close(pid) -> ()`: the raw idempotent-close seam the
/// `proc_handle` deinit pairs with, `net_close`'s own contract — kills
/// and reaps a still-running child, drops the pipe ends, and removes
/// the handle. Total and idempotent: an unknown handle is a no-op
/// (mirroring `close_raw`, so a deinit firing on an already-closed
/// handle stays clean). The close notes the handle for the wake door:
/// a parked read or wait on it wakes now (item 79).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn proc_close_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(pid) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.procs().close(pid);
        out.note_closed(crate::sched::CloseSeam::Proc, pid);
    }
    // the inert unit word
    Ok(NativeResult::Word(Value::I64(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        claims::ClaimTable,
        fiber::{FIBER_ROOT, Fiber},
        object::Objects,
    };

    /// `args()` answers the sink's staged arguments as a list of
    /// strings, empty when the host staged nothing.
    #[test]
    fn args_answers_the_staged_argv() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        out.set_argv(vec!["one".to_owned(), "two words".to_owned()]);
        let result = args_fn(&mut heaps, &mut out, &[]).expect("args is total");
        let word = result.word().expect("args answers the list word");
        let Value::Ref(word) = word else {
            panic!("args answers the list word: {word:?}");
        };
        let texts: Vec<String> = heaps
            .list_at(word)
            .expect("the list reads back")
            .0
            .to_vec()
            .iter()
            .map(|elem| {
                let Value::Ref(word) = *elem else {
                    panic!("an argv element is a string word: {elem:?}");
                };
                heaps.str_at(word).expect("the element reads").to_owned()
            })
            .collect();
        assert_eq!(texts, ["one", "two words"]);

        let empty = args_fn(&mut heaps, &mut OutputSink::captured(), &[])
            .expect("an unstaged host answers the empty list");
        let Value::Ref(word) = empty.word().expect("the list word") else {
            unreachable!()
        };
        let (elems, _) = heaps.list_at(word).expect("the empty list reads");
        assert!(elems.is_empty(), "no staged arguments, no elements");
    }

    /// `env(name)` answers the variable's value under the ok member and
    /// none for an unset name.
    #[test]
    fn env_reads_the_process_environment() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let name = "DS_PROC_TEST_ENV";
        // test-local like the CLI cache roots: the runner gives each
        // test its own process, and no other unit test reads the name
        unsafe {
            std::env::set_var(name, "staged");
        }
        let set_name = heaps.new_str(name.to_owned());
        let set = env_fn(&mut heaps, &mut out, &[set_name]).expect("env is total");
        let NativeResult::Tag { tag, payload } = set else {
            panic!("env answers the checked span: {set:?}");
        };
        assert_eq!(tag, PLATFORM_IO_OK, "a set variable answers ok");
        let Value::Ref(word) = payload else {
            panic!("the ok payload is the value string: {payload:?}");
        };
        assert_eq!(heaps.str_at(word).expect("the payload reads"), "staged");

        let unset_name = heaps.new_str("DS_PROC_TEST_ENV_UNSET".to_owned());
        let unset = env_fn(&mut heaps, &mut out, &[unset_name]).expect("env is total");
        assert_eq!(
            unset,
            NativeResult::Tag {
                tag: PLATFORM_NONE,
                payload: Value::I64(0)
            },
            "an unset variable answers the none member"
        );
        unsafe {
            std::env::remove_var(name);
        }
    }

    /// `proc_run` answers the child's exit status under the ok member
    /// and the io failure span when the program cannot spawn.
    #[test]
    fn proc_run_answers_the_child_status() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        // the child is this test binary listing itself: spawnable on
        // every platform, exits 0, touches no shared state
        let exe = std::env::current_exe().expect("the test binary's path");
        let argv = {
            let flag = heaps.new_str("--list".to_owned());
            let list = heaps.new_list(vec![flag]);
            if let Value::Ref(word) = flag {
                heaps.release_claim(word);
            }
            list
        };
        let program = heaps.new_str(exe.to_string_lossy().into_owned());
        let result =
            proc_run_fn(&mut heaps, &mut out, &[program, argv]).expect("proc_run is total");
        assert_eq!(
            result,
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            },
            "the listed child exits 0"
        );

        let missing = heaps.new_str("ds-proc-test-no-such-program".to_owned());
        let no_args = heaps.new_list(Vec::new());
        let refused =
            proc_run_fn(&mut heaps, &mut out, &[missing, no_args]).expect("proc_run is total");
        let NativeResult::Tag { tag, .. } = refused else {
            panic!("a spawn failure answers the failure span: {refused:?}");
        };
        assert_eq!(
            tag,
            crate::native::PLATFORM_IO_NOT_FOUND,
            "a missing program answers the not-found io class"
        );
    }

    /// `exit(code)` never answers: the wrapped status rides the trap
    /// out, a negative status trapping like the u32 range failure, the
    /// host's policy above the engine.
    #[test]
    fn exit_traps_negative_and_wraps_wide_statuses() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(3)]),
            Err(Trap::Exit { code: 3 })
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(255)]),
            Err(Trap::Exit { code: 255 })
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(256)]),
            Err(Trap::Exit { code: 0 }),
            "a wide status wraps to its low byte"
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(1000)]),
            Err(Trap::Exit { code: 232 }),
            "a wide status wraps to its low byte"
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(-1)]),
            Err(Trap::Die {
                message: "exit: -1 is out of range 0..=255".to_owned()
            }),
            "a negative status traps, never reading as clean success"
        );
    }

    /// Spawns the test binary's own listing (`--list` exits 0 after
    /// printing its test names to stdout): spawnable on every
    /// platform, touches no shared state, and its output is the
    /// capture the read row answers.
    fn spawn_listed_child(heaps: &mut Heaps, out: &mut OutputSink) -> i64 {
        let exe = std::env::current_exe().expect("the test binary's path");
        let flag = heaps.new_str("--list".to_owned());
        let list = heaps.new_list(vec![flag]);
        if let Value::Ref(word) = flag {
            heaps.release_claim(word);
        }
        let program = heaps.new_str(exe.to_string_lossy().into_owned());
        let spawned = proc_spawn_fn(heaps, out, &[program, list]).expect("proc_spawn is total");
        let NativeResult::Tag { tag, payload } = spawned else {
            panic!("the spawn answers the handle's span: {spawned:?}");
        };
        assert_eq!(tag, PLATFORM_IO_OK, "a spawnable program answers ok");
        let Value::I64(pid) = payload else {
            panic!("the ok payload is the handle's word: {payload:?}");
        };
        pid
    }

    /// The capture rows' round trip: the write reaches the child's
    /// stdin, the read answers its captured output (the listing's own
    /// test names) and then the pipe's orderly end, the wait answers
    /// the exit status idempotently, and the close is the idempotent
    /// seam the deinit pairs with (a later read on the closed handle
    /// answering the unknown-process floor).
    #[test]
    fn the_capture_rows_answer_the_piped_round_trip() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let pid = spawn_listed_child(&mut heaps, &mut out);
        // the listed child reads no stdin, so the write parks in the
        // pipe buffer and answers the unit span
        let data = heaps.new_str("ignored".to_owned());
        assert_eq!(
            proc_write_fn(&mut heaps, &mut out, &[Value::I64(pid), data]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            }),
            "the write answers the unit span"
        );
        // The read loop bounds the captured bytes, never the chunk
        // count: a pipe read under parallel-suite load answers short
        // chunks whenever the child writes slower than the parent
        // reads, so a chunk-count bound trips on scheduling alone
        // while the listing's own content stays bounded.
        let mut captured = String::new();
        loop {
            let read = proc_read_fn(&mut heaps, &mut out, &[Value::I64(pid), Value::I64(4096)])
                .expect("the read is total");
            let NativeResult::Tag { tag, payload } = read else {
                panic!("the read answers the checked span: {read:?}");
            };
            assert_eq!(tag, PLATFORM_IO_OK, "a read answers ok");
            let Value::Ref(word) = payload else {
                panic!("the ok payload is the output string: {payload:?}");
            };
            let chunk = heaps.str_at(word).expect("the chunk reads").to_owned();
            if chunk.is_empty() {
                break;
            }
            captured.push_str(&chunk);
            assert!(
                captured.len() < 4 * 1024 * 1024,
                "the listing never reaches its pipe's end"
            );
        }
        assert!(
            captured.contains("exit_traps_negative_and_wraps_wide_statuses"),
            "the captured output holds the listing's own test name"
        );
        assert_eq!(
            proc_wait_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            }),
            "the listed child exits 0"
        );
        assert_eq!(
            proc_wait_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            }),
            "a second wait answers the reaped status again"
        );
        assert_eq!(
            proc_close_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Word(Value::I64(0))),
            "the close answers the inert unit word"
        );
        assert_eq!(
            proc_close_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Word(Value::I64(0))),
            "a second close stays the idempotent no-op"
        );
        let closed = proc_read_fn(&mut heaps, &mut out, &[Value::I64(pid), Value::I64(8)])
            .expect("the read is total");
        let NativeResult::Tag { tag, payload } = closed else {
            panic!("a closed handle answers the failure span: {closed:?}");
        };
        assert_eq!(tag, crate::native::PLATFORM_IO_OTHER);
        let Value::Ref(word) = payload else {
            panic!("the failure payload is the message: {payload:?}");
        };
        assert_eq!(
            heaps.str_at(word).expect("the message reads"),
            format!("proc_read {pid}: unknown process {pid}")
        );
    }

    /// The capture rows' floors: an unknown handle is the same io
    /// floor everywhere a handle is used (the close of one the
    /// idempotent no-op), and a non-positive read bound names its own
    /// refusal before any pipe is touched.
    #[test]
    fn unknown_capture_handles_answer_their_floors() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let data = heaps.new_str("x".to_owned());
        // the unknown-handle floor: the same class and message every
        // row answers, read off the payload instead of comparing the
        // minted `Ref`s
        let floor = |heaps: &mut Heaps, result: Result<NativeResult, Trap>, op: &str| {
            let result = result.expect("the row is total");
            let NativeResult::Tag { tag, payload } = result else {
                panic!("an unknown handle answers the failure span: {result:?}");
            };
            assert_eq!(tag, crate::native::PLATFORM_IO_OTHER);
            let Value::Ref(word) = payload else {
                panic!("the failure payload is the message: {payload:?}");
            };
            assert_eq!(
                heaps.str_at(word).expect("the message reads"),
                format!("{op} 7: unknown process 7")
            );
        };
        let written = proc_write_fn(&mut heaps, &mut out, &[Value::I64(7), data]);
        floor(&mut heaps, written, "proc_write");
        let read = proc_read_fn(&mut heaps, &mut out, &[Value::I64(7), Value::I64(8)]);
        floor(&mut heaps, read, "proc_read");
        let read_err = proc_read_err_fn(&mut heaps, &mut out, &[Value::I64(7), Value::I64(8)]);
        floor(&mut heaps, read_err, "proc_read_err");
        let waited = proc_wait_fn(&mut heaps, &mut out, &[Value::I64(7)]);
        floor(&mut heaps, waited, "proc_wait");
        assert_eq!(
            proc_try_wait_fn(&mut heaps, &mut out, &[Value::I64(7)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_NONE,
                payload: Value::I64(0)
            }),
            "an unknown handle's poll answers the none member"
        );
        assert_eq!(
            proc_close_fn(&mut heaps, &mut out, &[Value::I64(7)]),
            Ok(NativeResult::Word(Value::I64(0))),
            "an unknown handle's close stays the no-op"
        );
        assert_eq!(
            proc_close_in_fn(&mut heaps, &mut out, &[Value::I64(7)]),
            Ok(NativeResult::Word(Value::I64(0))),
            "an unknown handle's stdin close stays the no-op"
        );
        let pid = spawn_listed_child(&mut heaps, &mut out);
        let bounded = proc_read_fn(&mut heaps, &mut out, &[Value::I64(pid), Value::I64(0)])
            .expect("the read is total");
        let NativeResult::Tag { tag, payload } = bounded else {
            panic!("a non-positive bound answers the failure span: {bounded:?}");
        };
        assert_eq!(tag, crate::native::PLATFORM_IO_OTHER);
        let Value::Ref(word) = payload else {
            panic!("the failure payload is the message: {payload:?}");
        };
        assert_eq!(
            heaps.str_at(word).expect("the message reads"),
            format!("proc_read {pid}: max_bytes must be positive")
        );
        proc_close_fn(&mut heaps, &mut out, &[Value::I64(pid)]).expect("the close is total");
    }

    /// The stdin-close seam: `close_in` answers the unit word, a write
    /// after it answers the "stdin already closed" floor, a second
    /// close changes nothing, and the handle's own close still ends
    /// the child cleanly (the seam keeps the table entry live).
    #[test]
    fn the_stdin_close_seam_is_idempotent_and_spares_the_child() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let pid = spawn_listed_child(&mut heaps, &mut out);
        assert_eq!(
            proc_close_in_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Word(Value::I64(0))),
            "the stdin close answers the inert unit word"
        );
        let data = heaps.new_str("ignored".to_owned());
        let written = proc_write_fn(&mut heaps, &mut out, &[Value::I64(pid), data])
            .expect("the write is total");
        let NativeResult::Tag { tag, payload } = written else {
            panic!("a write after the close answers the failure span: {written:?}");
        };
        assert_eq!(tag, crate::native::PLATFORM_IO_OTHER);
        let Value::Ref(word) = payload else {
            panic!("the failure payload is the message: {payload:?}");
        };
        assert_eq!(
            heaps.str_at(word).expect("the message reads"),
            format!("proc_write {pid}: process {pid} stdin already closed")
        );
        assert_eq!(
            proc_close_in_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Word(Value::I64(0))),
            "a second stdin close stays the no-op"
        );
        assert_eq!(
            proc_wait_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            }),
            "the closed-stdin child still waits clean"
        );
        proc_close_fn(&mut heaps, &mut out, &[Value::I64(pid)]).expect("the close is total");
    }

    /// The stderr read row mirrors the stdout one: a child that wrote
    /// no stderr answers the empty string at its pipe's orderly end
    /// (after the wait reaped it), and a non-positive bound names its
    /// own refusal before any pipe is touched.
    #[test]
    fn the_stderr_read_row_mirrors_the_stdout_one() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let pid = spawn_listed_child(&mut heaps, &mut out);
        proc_wait_fn(&mut heaps, &mut out, &[Value::I64(pid)]).expect("the wait is total");
        let read = proc_read_err_fn(&mut heaps, &mut out, &[Value::I64(pid), Value::I64(4096)])
            .expect("the stderr read is total");
        let NativeResult::Tag { tag, payload } = read else {
            panic!("the stderr read answers the checked span: {read:?}");
        };
        assert_eq!(tag, PLATFORM_IO_OK, "a read answers ok");
        let Value::Ref(word) = payload else {
            panic!("the ok payload is the diagnostics string: {payload:?}");
        };
        assert_eq!(
            heaps.str_at(word).expect("the payload reads"),
            "",
            "the listed child writes no stderr, so the pipe's end answers empty"
        );
        let bounded = proc_read_err_fn(&mut heaps, &mut out, &[Value::I64(pid), Value::I64(0)])
            .expect("the stderr read is total");
        let NativeResult::Tag { tag, payload } = bounded else {
            panic!("a non-positive bound answers the failure span: {bounded:?}");
        };
        assert_eq!(tag, crate::native::PLATFORM_IO_OTHER);
        let Value::Ref(word) = payload else {
            panic!("the failure payload is the message: {payload:?}");
        };
        assert_eq!(
            heaps.str_at(word).expect("the message reads"),
            format!("proc_read_err {pid}: max_bytes must be positive")
        );
        proc_close_fn(&mut heaps, &mut out, &[Value::I64(pid)]).expect("the close is total");
    }

    /// The non-blocking poll: an exited child's status answers under
    /// the ok member (a bounded poll loop, the listing child being
    /// short-lived), a later `wait` answers the same reaped word
    /// without touching the child, and a second poll answers it again.
    #[test]
    fn the_try_wait_row_answers_the_reaped_status_without_blocking() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let pid = spawn_listed_child(&mut heaps, &mut out);
        let mut polled = None;
        for _ in 0..1000 {
            let poll = proc_try_wait_fn(&mut heaps, &mut out, &[Value::I64(pid)])
                .expect("the poll is total");
            let NativeResult::Tag { tag, payload } = poll else {
                panic!("the poll answers the checked span: {poll:?}");
            };
            if tag == PLATFORM_IO_OK {
                let Value::I64(status) = payload else {
                    panic!("the ok payload is the status word: {payload:?}");
                };
                polled = Some(status);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            polled,
            Some(0),
            "the listed child's poll answers its status"
        );
        assert_eq!(
            proc_wait_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            }),
            "a wait after the poll answers the reaped status"
        );
        assert_eq!(
            proc_try_wait_fn(&mut heaps, &mut out, &[Value::I64(pid)]),
            Ok(NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            }),
            "a second poll answers the reaped status again"
        );
        proc_close_fn(&mut heaps, &mut out, &[Value::I64(pid)]).expect("the close is total");
    }

    /// The exit interest's raw syscall shape: a pidfd opens on a live
    /// process (the test's own), the syscall number and ABI the only
    /// moving parts — a kernel without pidfd names itself in the
    /// refusal, the parked wait's blocking fallback the answer there.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_exit_interest_opens_a_pidfd() {
        raw_proc::pidfd_open(std::process::id()).expect("a 5.3+ kernel opens a self pidfd");
    }

    /// A spawn failure answers the io class the OS names: a missing
    /// program the not-found class, the spawn row's own floor.
    #[test]
    fn a_failed_capture_spawn_answers_the_not_found_class() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let program = heaps.new_str("ds-proc-test-no-such-program".to_owned());
        let no_args = heaps.new_list(Vec::new());
        let refused =
            proc_spawn_fn(&mut heaps, &mut out, &[program, no_args]).expect("proc_spawn is total");
        let NativeResult::Tag { tag, payload } = refused else {
            panic!("a spawn failure answers the failure span: {refused:?}");
        };
        assert_eq!(tag, crate::native::PLATFORM_IO_NOT_FOUND);
        let Value::Ref(word) = payload else {
            panic!("the failure payload is the message: {payload:?}");
        };
        let message = heaps.str_at(word).expect("the message reads").to_owned();
        assert!(
            message.starts_with("proc_spawn ds-proc-test-no-such-program:"),
            "the message names the op and the program: {message}"
        );
    }
}
