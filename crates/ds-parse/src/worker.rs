//! The grammar runs on a worker thread that owns a dedicated stack segment.
//!
//! pest's recursive descent costs roughly 12-30 KiB per bracket-nesting level
//! (the precedence chain is ~19 rules deep per level), which no caller's stack
//! can be assumed to host, and the conversion pass recurses alongside it. Each
//! caller thread therefore keeps one worker and hands it jobs, so the segment
//! is paid once per caller instead of once per parse; a job carries its own
//! copy of the source because the worker outlives the caller's borrow.

use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc,
    thread,
};

use ds_diag::{Diagnostic, ErrorCode, FileId};

use crate::Ast;

/// Stack size for the grammar thread. The scanner's nesting limits bound
/// every recursion class that reaches the grammar, and the conversion
/// pass that follows the parse has the largest frames: measured on a
/// debug build (rustc 1.102-nightly, x86_64 Linux), one paren or arrow
/// layer of conversion recursion costs ~114 KiB, and one `else if`
/// level ~3 KiB, so the worst admitted source (40 parens + 39 arrows +
/// 499 elses, roughly 11 MiB) fits with close to 3x headroom. Release
/// frames are several times smaller, so the segment is sized by the
/// debug profile, which is the profile every test run uses.
const PARSE_STACK: usize = 32 << 20;

/// One message for the worker thread.
struct Job {
    file: FileId,
    src: String,
    script: bool,
    reply: mpsc::Sender<Result<Ast, Vec<Diagnostic>>>,
}

/// The worker cached for one caller thread; dropping the sender ends its job
/// loop.
struct Worker {
    jobs: mpsc::Sender<Job>,
}

thread_local! {
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Counts spawns per caller thread, which pins the reuse contract.
    static SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Refuses every spawn on this caller thread, pinning the degrade a
    /// host that cannot start threads answers.
    static REFUSE_SPAWNS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What handing one job to this thread's worker did.
enum Handoff {
    Sent,
    /// The cached worker died between jobs: the job rides back and the slot
    /// is emptied, so the caller's retry spawns a fresh thread.
    Dead(Job),
    /// The host cannot start the worker thread, so the job never left and
    /// the caller answers the internal error instead of retrying.
    Refused,
}

/// Runs the grammar and the conversion for one source on this caller thread's
/// worker, answering on the caller's stack.
pub(super) fn run(file: FileId, src: &str, script: bool) -> Result<Ast, Vec<Diagnostic>> {
    let (reply, answer) = mpsc::channel();
    let mut job = Job {
        file,
        src: src.to_owned(),
        script,
        reply,
    };
    loop {
        match hand_off(job) {
            Handoff::Sent => break,
            Handoff::Dead(returned) => job = returned,
            // The host cannot start the worker thread: the same internal
            // answer every other worker failure gives, never a panic on the
            // caller's stack.
            Handoff::Refused => {
                return Err(vec![Diagnostic::error(
                    ErrorCode::UnexpectedToken,
                    "internal parser error: the parse worker could not start",
                )]);
            },
        }
    }
    answer.recv().unwrap_or_else(|_| {
        Err(vec![Diagnostic::error(
            ErrorCode::UnexpectedToken,
            "internal parser error: the parse worker stopped",
        )])
    })
}

/// Hands one job to this thread's worker, spawning a worker when the slot is
/// empty; a dead worker hands the job back for the caller's retry, and a
/// refused spawn answers `Refused`.
fn hand_off(job: Job) -> Handoff {
    WORKER.with_borrow_mut(|slot| {
        if slot.is_none() {
            *slot = spawn();
        }
        let Some(worker) = slot.as_mut() else {
            return Handoff::Refused;
        };
        match worker.jobs.send(job) {
            Ok(()) => Handoff::Sent,
            Err(mpsc::SendError(returned)) => {
                *slot = None;
                Handoff::Dead(returned)
            },
        }
    })
}

/// Serves one job at a time until the owner's sender drops.
fn serve(jobs: mpsc::Receiver<Job>) {
    for job in jobs {
        // A pest panic is a parser bug; answer it as a diagnostic so the
        // worker outlives it.
        let out = catch_unwind(AssertUnwindSafe(|| {
            crate::run_grammar(job.file, &job.src, job.script)
        }))
        .unwrap_or_else(|_| {
            Err(vec![Diagnostic::error(
                ErrorCode::UnexpectedToken,
                "internal parser error: the grammar panicked",
            )])
        });
        let _ = job.reply.send(out);
    }
}

/// `None` when the host cannot start the thread: the caller answers the
/// internal-error diagnostic instead of panicking on its own stack.
fn spawn() -> Option<Worker> {
    #[cfg(test)]
    if REFUSE_SPAWNS.with(std::cell::Cell::get) {
        return None;
    }
    let (jobs, queue) = mpsc::channel();
    // The handle is dropped: the worker waits for jobs only, and the sender
    // drop is what ends it.
    thread::Builder::new()
        .name("ds-parse".to_owned())
        .stack_size(PARSE_STACK)
        .spawn(move || serve(queue))
        .ok()?;
    #[cfg(test)]
    SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
    Some(Worker { jobs })
}

#[cfg(test)]
mod tests {
    use ds_diag::Files;

    use super::*;

    /// The segment belongs to the caller thread: the second parse reuses the
    /// worker the first one spawned.
    #[test]
    fn a_caller_thread_reuses_one_worker() {
        let mut files = Files::new();
        let f = files.add_source("t.ds", "fn f() -> () {}\n");
        run(f, files.source(f), false).expect("parses");
        let spawned = SPAWNS.with(std::cell::Cell::get);
        run(f, files.source(f), false).expect("parses");
        assert_eq!(
            SPAWNS.with(std::cell::Cell::get),
            spawned,
            "the caller thread keeps its worker"
        );
    }

    /// A host that cannot start the worker thread answers the internal
    /// error diagnostic — the same degrade a dead worker's caller gets —
    /// instead of panicking on the caller's stack.
    #[test]
    fn a_refused_spawn_answers_the_internal_error() {
        REFUSE_SPAWNS.with(|refuse| refuse.set(true));
        let mut files = Files::new();
        let f = files.add_source("t.ds", "fn f() -> () {}\n");
        let out = run(f, files.source(f), false);
        assert!(
            matches!(&out, Err(diags)
                if diags.len() == 1 && diags[0].message.contains("could not start")),
            "a refused spawn answers the internal error diagnostic: {out:?}"
        );
    }
}
