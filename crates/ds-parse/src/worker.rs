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

/// Stack size for the grammar thread. The scanner's nesting limit bounds the
/// bracket depth that reaches the grammar, so this holds the deepest admitted
/// source with room for the conversion pass.
const PARSE_STACK: usize = 8 << 20;

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
        let sent = WORKER.with_borrow_mut(|slot| slot.get_or_insert_with(spawn).jobs.send(job));
        match sent {
            Ok(()) => break,
            // A worker that died outside a job's guard is replaced; the next
            // job goes to a fresh thread.
            Err(mpsc::SendError(returned)) => {
                job = returned;
                WORKER.with_borrow_mut(|slot| *slot = Some(spawn()));
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

fn spawn() -> Worker {
    let (jobs, queue) = mpsc::channel();
    thread::Builder::new()
        .name("ds-parse".to_owned())
        .stack_size(PARSE_STACK)
        .spawn(move || serve(queue))
        .expect("spawning the parse worker cannot fail");
    // The handle is dropped: the worker waits for jobs only, and the sender
    // drop is what ends it.
    #[cfg(test)]
    SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
    Worker { jobs }
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
}
