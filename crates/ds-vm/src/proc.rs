//! The process face of the CLI process surface (ruled 2026-11-21, H5):
//! the `proc` capability's ops — string argv, environment-variable
//! read, process spawn, and the `exit(code)` host member's seam —
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
//! over the same reaped word). The rows block the calling fiber
//! exactly like `proc_run`'s own wait: no portable reactor interest
//! shape exists for a child's pipe or exit, so the parked twins stay
//! the io-scheduler round's own. `proc_close_in` is the stdin-close
//! seam — the child's end-of-input while the handle stays live — and
//! `proc_close` the raw idempotent-close seam the `proc_handle` deinit
//! pairs with: a close of a still-running child kills and reaps it, so
//! a dropped handle leaks neither a child nor a zombie.

use std::io::{Read, Write};

use ds_abi::SlotType;

use crate::{
    native::{NativeResult, OutputSink, PLATFORM_IO_OK, PLATFORM_NONE, i64_arg, io_err, ref_arg},
    object::Heaps,
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

/// The `exit(code)` host member's seam: the deliberate process status
/// rides the trap out to the drive, the CLI host's exit-code policy
/// above it — the engine decides none (the die split's own rule). A
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
#[derive(Debug, Default)]
pub(crate) struct Procs {
    procs: Vec<(i64, ProcEntry)>,
    next_id: i64,
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
        self.procs.push((id, ProcEntry {
            child,
            stdin,
            stdout,
            stderr,
            status: None,
        }));
        id
    }

    /// The entry of handle `pid`.
    fn entry(&mut self, pid: i64) -> std::io::Result<&mut ProcEntry> {
        self.procs
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
        if let Some((_, entry)) = self.procs.iter_mut().find(|(id, _)| *id == pid) {
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

    /// Waits for handle `pid`'s child and answers its exit status — a
    /// child ended without a status code (a signal on Unix) answering
    /// -1, `proc_run`'s own convention. Idempotent: a later wait
    /// answers the reaped status again without touching the child, so
    /// the deinit's close after a wait stays clean.
    pub(crate) fn wait(&mut self, pid: i64) -> std::io::Result<i64> {
        let entry = self.entry(pid)?;
        if let Some(status) = entry.status {
            return Ok(status);
        }
        let status = i64::from(entry.child.wait()?.code().unwrap_or(-1));
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
        let Some((_, entry)) = self.procs.iter_mut().find(|(id, _)| *id == pid) else {
            return;
        };
        if entry.status.is_none() {
            let _ = entry.child.kill();
            let _ = entry.child.wait();
        }
        entry.stdin = None;
        entry.stdout = None;
        entry.stderr = None;
        self.procs.retain(|(id, _)| *id != pid);
    }
}

/// The "unknown process" error, shared by every handle lookup.
fn unknown_process(pid: i64) -> std::io::Error {
    std::io::Error::other(format!("unknown process {pid}"))
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
    let pid = i64_arg(args, 0)?;
    let max = i64_arg(args, 1)?;
    if max < 1 {
        return Ok(io_err(
            heaps,
            "proc_read",
            &pid.to_string(),
            &std::io::Error::other("max_bytes must be positive"),
        ));
    }
    let max = usize::try_from(max).unwrap_or(PROC_CAP).min(PROC_CAP);
    let bytes = match out.procs().read(pid, max) {
        Ok(bytes) => bytes,
        Err(err) => return Ok(io_err(heaps, "proc_read", &pid.to_string(), &err)),
    };
    Ok(match String::from_utf8(bytes) {
        Ok(text) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(text),
        },
        Err(_) => io_err(
            heaps,
            "proc_read",
            &pid.to_string(),
            &std::io::Error::other("invalid UTF-8 in the captured bytes"),
        ),
    })
}

/// `proc_wait(pid) -> int | io_err`: waits for the child and answers
/// its exit status — a child ended without a status code answering -1,
/// `proc_run`'s convention — idempotently, so a second wait answers
/// the reaped status again. The wait blocks the calling fiber exactly
/// like `proc_run`'s own; no portable reactor interest shape exists
/// for a child's exit.
pub(crate) fn proc_wait_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let pid = i64_arg(args, 0)?;
    Ok(match out.procs().wait(pid) {
        Ok(status) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(status),
        },
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
    let pid = i64_arg(args, 0)?;
    let max = i64_arg(args, 1)?;
    if max < 1 {
        return Ok(io_err(
            heaps,
            "proc_read_err",
            &pid.to_string(),
            &std::io::Error::other("max_bytes must be positive"),
        ));
    }
    let max = usize::try_from(max).unwrap_or(PROC_CAP).min(PROC_CAP);
    let bytes = match out.procs().read_err(pid, max) {
        Ok(bytes) => bytes,
        Err(err) => return Ok(io_err(heaps, "proc_read_err", &pid.to_string(), &err)),
    };
    Ok(match String::from_utf8(bytes) {
        Ok(text) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(text),
        },
        Err(_) => io_err(
            heaps,
            "proc_read_err",
            &pid.to_string(),
            &std::io::Error::other("invalid UTF-8 in the captured bytes"),
        ),
    })
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
/// handle stays clean).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn proc_close_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(pid) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.procs().close(pid);
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
            std::thread::sleep(std::time::Duration::from_millis(10));
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
