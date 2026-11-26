//! The `ds dev --hot` loop through the `ds` binary: one session pins
//! the hot-swap semantics end to end — a yielding entry parks between
//! iterations, a changed file recompiles and swaps, and the parked
//! frame's next fully-qualified call resolves through the swapped
//! slot, so the dev run's stdout stays byte-identical to a fresh `ds
//! run` of the updated source; a refused reload keeps the installed
//! version running; a completed entry restarts on the next change. A
//! staged lambda whose reorder preserves its content (body plus capture
//! set hashing equal) re-keys onto the moved index — the drive keeps
//! running the same behavior — while an edit riding the move refuses.

use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{Manifest, Package};

/// One expected line's ceiling: the fresh ambient compile of the first
/// round and each reload round's recompile run inside it — a package dev
/// session's reload recompile included — so the bound carries headroom
/// for a machine under a parallel suite's compile load.
const WAIT: Duration = Duration::from_secs(90);

/// Writes the source atomically (a sibling temp file renamed over the
/// target), so a dev child's window re-read never lands inside a direct
/// write's truncate-to-write gap under load.
fn swap_source(
    path: impl AsRef<std::path::Path>,
    contents: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    let path = path.as_ref();
    let mut swap = path.as_os_str().to_os_string();
    swap.push("~");
    let swap = std::path::PathBuf::from(swap);
    std::fs::write(&swap, contents)?;
    std::fs::rename(&swap, path)
}

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command
}

/// The dev loop child with both pipes draining through reader threads.
struct Dev {
    child: Child,
    stdout: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    /// Every program stdout line received, in order: the byte-identity
    /// compare reads the whole history, not the unconsumed tail.
    log: std::cell::RefCell<Vec<String>>,
}

impl Dev {
    fn spawn(path: &std::path::Path, poll_ms: u64) -> Self {
        let mut command = ds();
        command.arg("dev").arg(path);
        Self::launch(command, poll_ms)
    }

    /// The dev child over a command the test already shaped (a package
    /// root's store env, an entry's staged arguments): the watch flags
    /// ride along as with every spawn.
    fn launch(mut command: Command, poll_ms: u64) -> Self {
        let mut child = command
            .arg("--hot")
            .arg("--poll-ms")
            .arg(poll_ms.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the dev loop spawns");
        let stdout = reader(child.stdout.take().expect("stdout pipes"));
        let stderr = reader(child.stderr.take().expect("stderr pipes"));
        Self {
            child,
            stdout,
            stderr,
            log: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The session log folded with the first `skip` lines dropped and
    /// the trailing answer echo dropped after them: the watch door
    /// prints the entry's answer after the program's own output, and
    /// the default `ds run` face answers silent, so the differentials
    /// compare the program prints alone. A trailing echo is the one-word
    /// unit answer; a non-unit echo never spells `0` and stays.
    fn without_answer_echo(&self, skip: usize) -> String {
        let logged = self.log.borrow();
        let lines: &[String] = match logged.last() {
            Some(line) if line == "0" => &logged[..logged.len() - 1],
            _ => &logged[..],
        };
        let mut text = String::new();
        for line in &lines[skip.min(lines.len())..] {
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    /// The next program stdout line, failing the test on the timeout.
    fn line(&self) -> String {
        let line = self.stdout.recv_timeout(WAIT).unwrap_or_else(|_| {
            let mut err = String::new();
            while let Ok(line) = self.stderr.try_recv() {
                err.push_str(&line);
                err.push('\n');
            }
            panic!(
                "no stdout line within {WAIT:?}; stdout so far:\n{}\nstderr so far:\n{err}",
                self.collected()
            )
        });
        self.log.borrow_mut().push(line.clone());
        line
    }

    /// The next dev stderr line, failing the test on the timeout.
    fn err(&self) -> String {
        self.stderr
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("no stderr line within {WAIT:?}"))
    }

    /// Everything the reader threads collected so far, in order.
    fn collected(&self) -> String {
        let mut text = String::new();
        while let Ok(line) = self.stdout.try_recv() {
            text.push_str(&line);
            text.push('\n');
        }
        text
    }

    fn kill(&mut self) {
        self.child.kill().expect("the dev loop dies");
        self.child.wait().expect("the dev loop reaps");
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        // a failed assertion between spawn and kill never leaks the child
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One pipe line channel: a detached reader thread, so the assertions
/// never block on a quiet pipe.
fn reader(pipe: impl std::io::Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        return;
                    }
                },
                Err(_) => return,
            }
        }
    });
    rx
}

/// The dev-loop program body: `work` is the only difference between the
/// versions, so a fresh run of the updated source prints exactly what
/// the swapped run prints — the parked frame's next `work` call
/// resolves through the current slot. The entry parks at the while
/// loop's back edge through the host's suspension request; no language
/// spelling exists.
fn program(work: &str, bound: usize) -> String {
    let mut text = String::new();
    text.push_str("fn main() {\n");
    text.push_str("  var i = 0\n");
    text.push_str("  while i < ");
    text.push_str(&bound.to_string());
    text.push_str(" {\n");
    text.push_str("    println(\"${work(i)}\")\n");
    text.push_str("    i = i + 1\n");
    text.push_str("  }\n");
    text.push_str("}\n\n");
    text.push_str("fn work(n: int) -> int {\n");
    text.push_str("  ");
    text.push_str(work);
    text.push_str("\n}\n");
    text
}

#[test]
fn a_hot_swap_retires_the_old_frame_at_its_next_qualified_call() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let v1 = program("return n * 2", 3);
    let v2 = program("return n * 10", 3);
    swap_source(&prog, &v1).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 100);
    assert_eq!(dev.line(), "0", "the first round runs v1's work");
    swap_source(&prog, &v2).expect("the swap writes");
    assert_eq!(dev.line(), "10", "the parked frame's next call runs v2");
    assert_eq!(dev.line(), "20", "the rest of the run is v2's code");
    let answer = dev.line();
    assert_eq!(answer, "0", "the completed entry answers the unit word");
    dev.kill();

    // the differential: the swapped run printed exactly what a fresh
    // `ds run` of the updated source prints, byte for byte
    let fresh_source = dir.path().join("fresh.ds");
    swap_source(&fresh_source, &v2).expect("the fresh source writes");
    let fresh = ds()
        .arg("run")
        .arg("--print-result")
        .arg(&fresh_source)
        .output()
        .expect("the fresh run completes");
    assert!(fresh.status.success(), "{:?}", fresh.stderr);
    assert_eq!(
        dev.without_answer_echo(0),
        String::from_utf8_lossy(&fresh.stdout).into_owned(),
        "the hot-swapped run is byte-identical to the fresh run"
    );
}

#[test]
fn a_refused_reload_keeps_the_installed_version_running() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let v1 = program("return n", 4);
    let v2 = program("return n * 10", 4);
    swap_source(&prog, v1).expect("the source writes");
    // the wide poll keeps the round boundaries apart: each write lands
    // well inside one tick window, so the iteration counts stay fixed
    let mut dev = Dev::spawn(&prog, 500);
    assert_eq!(dev.line(), "0", "the first round runs v1's work");
    // a broken file refuses the reload, but the parked frame re-enters:
    // the installed version keeps running
    swap_source(&prog, "fn broken {\n").expect("the broken source writes");
    assert_eq!(
        dev.line(),
        "1",
        "the refused round resumes the entry on the installed version"
    );
    let diagnostic = dev.err();
    assert!(
        diagnostic.contains("DS"),
        "the refused round reports the diagnostic: {diagnostic}"
    );
    swap_source(&prog, v2).expect("the swap writes");
    assert_eq!(dev.line(), "20", "the next round swaps to v2");
    assert_eq!(dev.line(), "30");
    dev.kill();
}

#[test]
fn a_completed_entry_restarts_on_the_next_change() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    swap_source(&prog, "fn main() {\n  println(\"boot\")\n}\n").expect("the source writes");
    let mut dev = Dev::spawn(&prog, 100);
    assert_eq!(dev.line(), "boot");
    assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
    swap_source(&prog, "fn main() {\n  println(\"boot2\")\n}\n").expect("the change writes");
    assert_eq!(dev.line(), "boot2", "the next change restarts the entry");
    assert_eq!(dev.line(), "0");
    dev.kill();
}

/// An editor's atomic replace reaches the session like an in-place
/// edit: the swap file renames over the source (the inode moves), and
/// the next round reloads the replaced bytes — the Linux signal's
/// directory-level watch behind the same merge window the poll twin's
/// read composes, one behavior either way.
#[test]
fn an_atomic_replace_hot_swaps_like_an_edit() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    swap_source(&prog, program("return n * 2", 3)).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 100);
    assert_eq!(dev.line(), "0", "the first round runs v1's work");
    let swap = dir.path().join("prog.ds~");
    swap_source(&swap, program("return n * 10", 3)).expect("the swap source writes");
    std::fs::rename(&swap, &prog).expect("the atomic replace lands");
    assert_eq!(dev.line(), "10", "the replaced bytes reload mid-run");
    assert_eq!(dev.line(), "20", "the rest of the run is the replaced code");
    let answer = dev.line();
    assert_eq!(answer, "0", "the completed entry answers the unit word");
    dev.kill();
}

/// The dev-loop program staging a callable word across parks: `inc` is a
/// fn-typed value held in the parked frame, so every reload round runs
/// the re-check over its staged word. The lambda's table name embeds its
/// table index (`main$N`), so a reload that reorders the table renames
/// it — the name alone never survives a move, and the content judge
/// decides: an identical body and capture set re-keys onto the moved
/// index, an edit riding the move refuses.
fn staged_program(body: &str, bound: usize) -> String {
    let mut text = String::new();
    text.push_str("fn main() {\n");
    text.push_str("  const inc = (x: int) => ");
    text.push_str(body);
    text.push('\n');
    text.push_str("  var i = 0\n");
    text.push_str("  while i < ");
    text.push_str(&bound.to_string());
    text.push_str(" {\n");
    text.push_str("    println(\"${inc(i)}\")\n");
    text.push_str("    i = i + 1\n");
    text.push_str("  }\n");
    text.push_str("}\n");
    text
}

/// The reordering prefix every staged-lambda fixture prepends: a fresh
/// `pad` ahead of the entry shifts the whole table, so the staged
/// lambda's position-derived name moves with it.
const PAD: &str = "fn pad(n: int) -> int {\n  return 999\n}\n";

/// A reload whose reorder carries a body edit refuses: the fresh `pad`
/// ahead of the entry shifts the table so the staged lambda's
/// position-derived name moves, and the body changes in the same reload
/// (`x * 2` to `x * 10`) — neither the name nor the content identity
/// survived, so the structured diagnostic names the staged value and the
/// vanished function, the installed version keeps running, and the next
/// change that restores the table's order installs again — a refused
/// swap over a silent misroute.
#[test]
fn a_reordering_reload_refuses_over_the_staged_word_and_recovers() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let v1 = staged_program("x * 2", 4);
    // the reordering source: a fresh `pad` ahead of the entry shifts the
    // whole table AND the body edit rides the move (a content-preserving
    // shift re-keys instead)
    let v2 = format!("{PAD}{}", staged_program("x * 10", 4));
    let v3 = staged_program("x * 10", 4);
    swap_source(&prog, &v1).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 500);
    assert_eq!(dev.line(), "0", "the first round runs v1's inc");
    swap_source(&prog, &v2).expect("the reordering swap writes");
    assert_eq!(
        dev.line(),
        "2",
        "the refused reordering keeps the installed version running (1 * 2)"
    );
    let diagnostic = dev.err();
    assert!(
        diagnostic.contains("StagedFnMisroute"),
        "the refused swap reports the structured refusal: {diagnostic}"
    );
    assert!(
        diagnostic.contains("FrameSlot"),
        "the refusal names the staged value's site: {diagnostic}"
    );
    swap_source(&prog, &v3).expect("the order-restoring swap writes");
    assert_eq!(
        dev.line(),
        "20",
        "the next change restores the table's order, the swap installs, and the staged word runs \
         the new body mid-loop (2 * 10)"
    );
    assert_eq!(dev.line(), "30", "the loop finishes on v3's body (3 * 10)");
    assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
    dev.kill();
}

/// A reload that drops the staged word's function (the lambda leaves the
/// source) refuses the swap with a structured diagnostic and keeps the
/// installed version running.
#[test]
fn a_reload_dropping_the_staged_function_refuses_and_keeps_running() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let v1 = staged_program("x * 2", 4);
    let v2 = "fn main() {\n  var i = 0\n  while i < 4 {\n    println(\"${i * 10}\")\n    i = i + \
              1\n  }\n}\n"
        .to_owned();
    swap_source(&prog, &v1).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 500);
    assert_eq!(dev.line(), "0", "the first round runs v1's inc");
    swap_source(&prog, &v2).expect("the dropping swap writes");
    assert_eq!(
        dev.line(),
        "2",
        "the refused swap keeps the installed version running (1 * 2)"
    );
    let diagnostic = dev.err();
    assert!(
        diagnostic.contains("StagedFnMisroute"),
        "the refused swap reports the structured refusal: {diagnostic}"
    );
    assert_eq!(
        dev.line(),
        "4",
        "the installed version keeps running (2 * 2)"
    );
    dev.kill();
}

/// A reorder that preserves the staged lambda's content re-keys and the
/// drive keeps running byte-identical: the fresh `pad` shifts the table
/// so the lambda's position-derived name moves, but its body and capture
/// set are unchanged — the content identity survived the move, the swap
/// installs with no refusal, and the whole dev session's stdout is
/// byte-identical to a fresh `ds run` of the reordered source (identical
/// content means identical behavior; the re-key only preserves identity).
#[test]
fn a_content_preserving_reorder_rekeys_the_staged_lambda() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let v1 = staged_program("x * 2", 4);
    let v2 = format!("{PAD}{}", staged_program("x * 2", 4));
    swap_source(&prog, &v1).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 500);
    assert_eq!(dev.line(), "0", "the first round runs v1's inc");
    swap_source(&prog, &v2).expect("the content-preserving reorder writes");
    assert_eq!(
        dev.line(),
        "2",
        "the re-keyed staged word keeps running x * 2"
    );
    assert_eq!(dev.line(), "4");
    assert_eq!(dev.line(), "6");
    assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
    dev.kill();

    // the differential: the swapped run printed exactly what a fresh
    // `ds run` of the reordered source prints, byte for byte
    let fresh_source = dir.path().join("fresh.ds");
    swap_source(&fresh_source, &v2).expect("the fresh source writes");
    let fresh = ds()
        .arg("run")
        .arg("--print-result")
        .arg(&fresh_source)
        .output()
        .expect("the fresh run completes");
    assert!(fresh.status.success(), "{:?}", fresh.stderr);
    assert_eq!(
        dev.without_answer_echo(0),
        String::from_utf8_lossy(&fresh.stdout).into_owned(),
        "the re-keyed run is byte-identical to the fresh run"
    );
}

/// The content-preserving reorder really installed: the follow-up edit
/// at the lambda's moved position lands, proving the reordered version
/// is the installed one (a refused reorder would have left v1 current,
/// and the follow-up — a moved name whose body also changed — would
/// refuse in turn, printing the old body's answer).
#[test]
fn a_rekeyed_reorder_takes_so_the_follow_up_edit_lands() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let v1 = staged_program("x * 2", 8);
    let v2 = format!("{PAD}{}", staged_program("x * 2", 8));
    let v3 = format!("{PAD}{}", staged_program("x * 10", 8));
    swap_source(&prog, &v1).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 500);
    assert_eq!(dev.line(), "0", "the first round runs v1's inc");
    swap_source(&prog, &v2).expect("the content-preserving reorder writes");
    assert_eq!(dev.line(), "2");
    assert_eq!(dev.line(), "4");
    assert_eq!(dev.line(), "6");
    // the follow-up keeps the lambda at its moved position (main$2) and
    // edits the body: it installs only over the reordered version, and
    // the next round answers 4 * 10 — 8 (4 * 2 on the kept v1) is the
    // refusal world the re-key rules out
    swap_source(&prog, &v3).expect("the follow-up edit writes");
    assert_eq!(
        dev.line(),
        "40",
        "the follow-up lands on the reordered version and runs x * 10"
    );
    assert_eq!(dev.line(), "50", "the loop continues on v3's body");
    dev.kill();
}

/// The dev-loop program whose entry traps mid-drive holding a
/// deinit-carrying value: `work` binds its local `res` before the
/// division by zero strikes, so the trapped drive's kept frames carry
/// the claim — and the release after the report fires it exactly once
/// (the drain door the REPL's rollback drives), the session keeps
/// watching, and the fixed reload restarts the entry fresh.
#[test]
fn a_trapped_entry_releases_and_the_fixed_reload_restarts_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let header =
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n\n";
    let body = |work: &str| {
        format!(
            "{header}fn work(n: int) -> int {{\n  const r = res {{ id: n }}\n  return \
             {work}\n}}\n\nfn main() {{\n  var i = 0\n  while i < 3 {{\n    \
             println(\"${{work(i)}}\")\n    i = i + 1\n  }}\n}}\n"
        )
    };
    let v1 = body("10 / (n - 1)");
    let v2 = body("n * 2");
    swap_source(&prog, &v1).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 100);
    assert_eq!(dev.line(), "dying 0", "the first iteration's own exit drop");
    assert_eq!(dev.line(), "-10", "work(0) answers 10 / -1");
    // the second iteration traps mid-drive: the report lands on stderr,
    // and the release fires the trapped frames' deinit exactly once —
    // never at a frame pop, the frame never popped
    let report = dev.err();
    assert!(
        report.contains("division by zero"),
        "the trapped drive reports: {report}"
    );
    assert_eq!(
        dev.line(),
        "dying 1",
        "the release fires the trapped entry's kept value once"
    );
    // the session keeps watching: the fixed reload restarts the entry
    // fresh, and the whole restart prints what a fresh run would
    swap_source(&prog, &v2).expect("the fix writes");
    for expected in ["dying 0", "0", "dying 1", "2", "dying 2", "4", "0"] {
        assert_eq!(dev.line(), expected);
    }
    dev.kill();
}

/// The dev-loop program whose entry traps inside a nursery block between
/// the spawn and the join: the spawned child is never entered, so the
/// release after the report fires the owner's kept value exactly once and
/// releases the never-entered child cold — no child line ever prints for
/// the trapping round (the release never enters a child), the child's
/// pool slot and the nursery's slot return, and the fixed reload's
/// restart prints exactly what a fresh run of the fixed source prints.
#[test]
fn a_trapped_entrys_abandoned_nursery_releases_with_the_entry() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let header =
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n\n";
    let body = |boom: &str| {
        format!(
            "{header}fn boom(n: int) -> int {{\n  return {boom}\n}}\n\nfn work(n: int) -> int \
             {{\n  const r = res {{ id: n }}\n  var out = 0\n  nursery {{\n    spawn {{ \
             println(\"child ${{n}}\") }}\n    out = boom(n)\n  }}\n  return out\n}}\n\nfn main() \
             -> () {{\n  var i = 0\n  while i < 3 {{\n    println(\"${{work(i)}}\")\n    i = i + \
             1\n  }}\n}}\n"
        )
    };
    let v1 = body("10 / (n - 1)");
    let v2 = body("n * 2");
    swap_source(&prog, &v1).expect("the source writes");
    let mut dev = Dev::spawn(&prog, 100);
    // the first round lands: the block joins its child, the value dies at
    // the exit, work answers 10 / -1
    assert_eq!(
        dev.line(),
        "child 0",
        "the first round's join drives the child"
    );
    assert_eq!(dev.line(), "dying 0", "the landed round's own exit drop");
    assert_eq!(dev.line(), "-10");
    // the second round traps between the spawn and the join: the report
    // lands on stderr, the release fires the kept frames' deinit exactly
    // once, and the never-entered child releases cold — its line never
    // prints
    let report = dev.err();
    assert!(
        report.contains("division by zero"),
        "the trapped drive reports: {report}"
    );
    assert_eq!(
        dev.line(),
        "dying 1",
        "the release fires the trapped owner's kept value once"
    );
    // the session keeps watching: the fixed reload restarts the entry
    // fresh, every round now joining its child, and the whole restart
    // prints what a fresh run would
    swap_source(&prog, &v2).expect("the fix writes");
    for expected in [
        "child 0", "dying 0", "0", "child 1", "dying 1", "2", "child 2", "dying 2", "4", "0",
    ] {
        assert_eq!(dev.line(), expected);
    }
    dev.kill();

    // the differential: the restart's output is byte-identical to a fresh
    // `ds run` of the fixed source (the release's own "dying 1" line is
    // the dev door's, not a round's)
    let fresh = ds()
        .arg("run")
        .arg("--print-result")
        .arg(&prog)
        .output()
        .expect("the fresh run completes");
    assert!(fresh.status.success(), "{:?}", fresh.stderr);
    let tail = dev.without_answer_echo(4);
    assert_eq!(
        tail,
        String::from_utf8_lossy(&fresh.stdout).into_owned(),
        "the reloaded restart prints what the fresh run prints"
    );
}

/// An empty source is a load like any other: the single-shot round
/// reports its own diagnostic (no lowerable main entry, exactly a
/// whitespace-only file) instead of silently exiting.
#[test]
fn an_empty_source_reports_its_own_diagnostic() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let empty = dir.path().join("empty.ds");
    swap_source(&empty, "").expect("the empty source writes");
    let out = ds()
        .arg("dev")
        .arg(&empty)
        .output()
        .expect("the dev round completes");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, [] as [u8; 0]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("dev: no lowerable `main` entry in the package"),
        "stderr: {stderr}"
    );
}

/// The process arguments reach the entry through `args()` on every
/// version: the first round runs the argv the session staged, and after
/// the entry completes, a changed round restarts it reading the same
/// argv — the reload's restart prints exactly what a fresh `ds run` of
/// the updated source prints.
#[test]
fn an_argv_entry_survives_a_reload_reading_the_process_arguments() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("prog.ds");
    let v1 = "fn main() -> int {\n  const n = args().get(0).ok((w: string) => w).err((_: none) => \
              \"0\").parse_int().err((_: parse_err) => 0)\n  println(\"got ${n}\")\n  return n * \
              2\n}\n";
    let v2 = "fn main() -> int {\n  const n = args().get(0).ok((w: string) => w).err((_: none) => \
              \"0\").parse_int().err((_: parse_err) => 0)\n  println(\"got ${n}\")\n  return n * \
              10\n}\n";
    swap_source(&prog, v1).expect("the source writes");
    let mut command = ds();
    command.arg("dev").arg(&prog).arg("--args").arg("21");
    let mut dev = Dev::launch(command, 100);
    assert_eq!(dev.line(), "got 21", "the first round reads --args");
    assert_eq!(dev.line(), "42", "the completed entry answers n * 2");
    swap_source(&prog, v2).expect("the change writes");
    assert_eq!(
        dev.line(),
        "got 21",
        "the restarted entry reads the same process arguments"
    );
    assert_eq!(dev.line(), "210", "the new version answers n * 10");
    dev.kill();

    // the differential: the restart's output is byte-identical to a
    // fresh `ds run --args` of the updated source
    let fresh = ds()
        .arg("run")
        .arg("--print-result")
        .arg(&prog)
        .arg("--args")
        .arg("21")
        .output()
        .expect("the fresh run completes");
    assert!(fresh.status.success(), "{:?}", fresh.stderr);
    let tail = dev.without_answer_echo(2);
    assert_eq!(
        tail,
        String::from_utf8_lossy(&fresh.stdout).into_owned(),
        "the reloaded restart prints what the fresh run prints"
    );
}

/// The dep package the package-root session links: one member whose
/// `twice` the root's `work` calls through the resolved graph.
const LIB_MEMBER: &str = "pub fn twice(n: int) -> int {\n  return n * 2\n}\n";

fn lib_manifest() -> String {
    "name = \"@lib\"\nversion = \"1.2.0\"\n".to_owned()
}

fn root_manifest(lib: Hash128) -> String {
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"1.2.0\", hash = \
         \"blake3:{lib}\" }}\n"
    )
}

/// The root program the package-root session drives: `work` is the
/// same-module helper whose fully-qualified call redirects through the
/// swapped slot, `twice` the linked dep the walk installs, and `main` a
/// zero-parameter entry reading its `n` off the process arguments
/// (`args()`), the same argv on every version.
fn root_program(work: &str, main_tail: bool) -> String {
    let mut text = String::new();
    text.push_str("import @lib/helper (twice)\n\n");
    text.push_str("fn work(n: int) -> int {\n  ");
    text.push_str(work);
    text.push_str("\n}\n\n");
    text.push_str("fn main() -> int {\n");
    text.push_str(
        "  const n = args().get(0).ok((w: string) => w).err((_: none) => \
         \"0\").parse_int().err((_: parse_err) => 0)\n",
    );
    if main_tail {
        text.push_str("  println(\"${work(n)}\")\n  return n\n");
    } else {
        text.push_str(
            "  var i = 0\n  while i < 3 {\n    println(\"${work(n)}\")\n    i = i + 1\n  }\n  \
             return 0\n",
        );
    }
    text.push_str("}\n");
    text
}

/// The package-root dev session: the watched pair is the entry member
/// and the manifest beside it, the linked dep serving from the test
/// store through the deps-first walk. A member edit swaps mid-run (the
/// parked frame's next `work` call resolves through the current slot),
/// a refused round keeps the installed version running, and the
/// completed entry's restart reads the same argv on the new version.
#[test]
fn a_package_root_dev_session_hot_swaps_on_a_member_edit() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let cache = dir.path().join("store");
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).expect("the lib dir creates");
    swap_source(lib.join("ds.pkg"), lib_manifest()).expect("the manifest writes");
    swap_source(lib.join("helper.ds"), LIB_MEMBER).expect("the member writes");
    let lib_id = {
        let manifest =
            Manifest::parse(&lib_manifest(), FileId::new(0)).expect("fixture manifest parses");
        let bundle = Package::new(&manifest, [(
            "helper.ds".to_owned(),
            LIB_MEMBER.as_bytes().to_vec(),
        )])
        .expect("fixture bundle builds");
        bundle.content_hash()
    };
    let mut save = ds();
    save.env("DS_CACHE", &cache).env_remove("DS_REGISTRY");
    let save = save.arg("save").arg(&lib).output().expect("ds save runs");
    assert!(
        save.status.success(),
        "ds save exits 0 (stderr: {})",
        String::from_utf8_lossy(&save.stderr)
    );

    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).expect("the root dir creates");
    swap_source(root.join("ds.pkg"), root_manifest(lib_id)).expect("the manifest writes");
    let entry = root.join("app.ds");
    let v1 = root_program("return twice(n)", false);
    let v2 = root_program("return twice(n) + 1", false);
    let v3 = root_program("return twice(n)", true);
    swap_source(&entry, &v1).expect("the source writes");

    let mut command = ds();
    command
        .env("DS_CACHE", &cache)
        .env_remove("DS_REGISTRY")
        .arg("dev")
        .arg(&entry)
        .arg("--args")
        .arg("21");
    let mut dev = Dev::launch(command, 500);
    assert_eq!(
        dev.line(),
        "42",
        "the first round links @lib and runs work(21)"
    );
    // a broken member refuses the reload; the installed version keeps
    // running through the walk's already-installed deps
    swap_source(&entry, "fn broken {\n").expect("the broken source writes");
    assert_eq!(
        dev.line(),
        "42",
        "the refused round resumes the entry on the installed version"
    );
    let diagnostic = dev.err();
    assert!(
        diagnostic.contains("DS"),
        "the refused round reports the diagnostic: {diagnostic}"
    );
    // the member edit swaps: the parked frame's next `work` call runs the
    // new body over the same linked `twice`
    swap_source(&entry, &v2).expect("the swap writes");
    assert_eq!(
        dev.line(),
        "43",
        "the parked frame's next call runs v2's work"
    );
    assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
    // the completed entry restarts on the next change, reading argv
    swap_source(&entry, &v3).expect("the restart writes");
    assert_eq!(
        dev.line(),
        "42",
        "the restart reads the same argv and runs v3's work"
    );
    assert_eq!(dev.line(), "21", "the restarted entry answers n");
    dev.kill();
}

/// The package-root watch sees a sibling member's edit: the watched
/// bytes compose every member below the root, so an edit to a helper
/// member (a `pub` member the entry calls across files) reloads
/// exactly like the entry's own and the parked frame's next
/// cross-member call answers the new body through the swapped slot. A
/// broken sibling refuses the round under the sibling's own member
/// path, the installed version keeping running.
#[test]
fn a_package_root_sibling_edit_hot_reloads() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).expect("the root dir creates");
    swap_source(root.join("ds.pkg"), "name = \"app\"\nversion = \"0.1.0\"\n")
        .expect("the manifest writes");
    let work_member = |body: &str| format!("pub fn work(n: int) -> int {{\n  return {body}\n}}\n");
    swap_source(root.join("work.ds"), work_member("n * 2")).expect("the sibling member writes");
    let entry = root.join("app.ds");
    swap_source(
        &entry,
        "fn main() -> int {\n  const n = args().get(0).ok((w: string) => w).err((_: none) => \
         \"0\").parse_int().err((_: parse_err) => 0)\n  var i = 0\n  while i < 10 {\n    \
         println(\"${work(n)}\")\n    i = i + 1\n  }\n  return 0\n}\n",
    )
    .expect("the source writes");

    let mut command = ds();
    command
        .env("DS_CACHE", dir.path().join("cache"))
        .env_remove("DS_REGISTRY")
        .arg("dev")
        .arg(&entry)
        .arg("--args")
        .arg("21");
    let mut dev = Dev::launch(command, 500);
    assert_eq!(
        dev.line(),
        "42",
        "the first round composes the sibling and runs work(21)"
    );
    // the sibling edit swaps: the parked frame's next `work` call runs
    // the new body out of the sibling member
    swap_source(root.join("work.ds"), work_member("n * 3")).expect("the sibling edit writes");
    assert_eq!(
        dev.line(),
        "63",
        "the parked frame's next call runs the sibling's new body"
    );
    // a broken sibling refuses the round naming the sibling member;
    // the installed version keeps running
    swap_source(root.join("work.ds"), "fn broken {\n").expect("the broken sibling writes");
    assert_eq!(
        dev.line(),
        "63",
        "the refused round resumes the entry on the installed version"
    );
    let mut found = String::new();
    for _ in 0..4 {
        found = dev.err();
        if found.contains("--> work.ds") {
            break;
        }
    }
    assert!(
        found.contains("--> work.ds"),
        "the refused round names the sibling member: {found}"
    );
    dev.kill();
}

/// One dep member body at a parameterized edit: the multi-package watch
/// repins `twice`'s body while keeping the name and table position, so
/// the moved dep's swap keeps every live cross-module site (the swap
/// re-check's name judge).
fn lib_member(body: &str) -> String {
    format!("pub fn twice(n: int) -> int {{\n  return {body}\n}}\n")
}

/// The repin round's dep save: writes the lib manifest and member,
/// installs the bundle into the test store through `ds save`, and
/// answers the bundle identity the root's manifest pins (the dep's
/// member identity — what the VM dep list records — is the graph's own
/// business).
fn save_lib(
    dir: &std::path::Path,
    cache: &std::path::Path,
    manifest: &str,
    member: &str,
) -> Hash128 {
    swap_source(dir.join("ds.pkg"), manifest).expect("the manifest writes");
    swap_source(dir.join("helper.ds"), member).expect("the member writes");
    let parsed = Manifest::parse(manifest, FileId::new(0)).expect("fixture manifest parses");
    let bundle = Package::new(&parsed, [(
        "helper.ds".to_owned(),
        member.as_bytes().to_vec(),
    )])
    .expect("fixture bundle builds");
    let identity = bundle.content_hash();
    let mut save = ds();
    save.env("DS_CACHE", cache).env_remove("DS_REGISTRY");
    let save = save.arg("save").arg(dir).output().expect("ds save runs");
    assert!(
        save.status.success(),
        "ds save exits 0 (stderr: {})",
        String::from_utf8_lossy(&save.stderr)
    );
    identity
}

/// The root manifest pinning one dep content: `ver` is the dep's own
/// version at the pinned identity.
fn pin_manifest(ver: &str, lib: Hash128) -> String {
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"{ver}\", hash = \
         \"blake3:{lib}\" }}\n"
    )
}

/// The entry the multi-package sessions drive: the loop calls the linked
/// `twice` directly, so every parked round's next call crosses the dep's
/// slot and a dep swap lands mid-drive.
fn dep_program(bound: usize) -> String {
    format!(
        "import @lib/helper (twice)\n\nfn main() -> int {{\n  var i = 0\n  while i < {bound} {{\n    \
         println(\"${{twice(i)}}\")\n    i = i + 1\n  }}\n  return 0\n}}\n"
    )
}

/// One multi-package session's tree, the temp dir's lifetime included:
/// the lib dir with v1 saved and the root dir pinning it.
struct MultiPackage {
    /// The tree's owner, kept alive for the session's paths (`_`-named:
    /// its drop is the load-bearing part, never a read).
    _dir: tempfile::TempDir,
    cache: std::path::PathBuf,
    lib: std::path::PathBuf,
    root: std::path::PathBuf,
    entry: std::path::PathBuf,
}

/// Builds the tree and saves the dep's first content, answering the
/// scaffolding with the temp dir kept alive for the session.
fn multi_package(v1_member: &str) -> MultiPackage {
    let dir = tempfile::tempdir().expect("a temp dir");
    let cache = dir.path().join("store");
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).expect("the lib dir creates");
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).expect("the root dir creates");
    let entry = root.join("app.ds");
    let v1 = save_lib(&lib, &cache, &lib_manifest(), v1_member);
    swap_source(root.join("ds.pkg"), pin_manifest("1.2.0", v1)).expect("the manifest writes");
    MultiPackage {
        _dir: dir,
        cache,
        lib,
        root,
        entry,
    }
}

/// A manifest edit pinning new dep content moves the linked dep's
/// identity, and the round recomputes the graph honestly: the moved
/// dep hot-swaps under its own name through the same two-slot semantics
/// the root rides (the member edit keeps `twice`'s name and table
/// position, so the swap re-checks pass), and the parked frame's next
/// cross-module call resolves through the swapped slot — the drive
/// keeps running on the new dep code without a restart, and the
/// post-swap tail is byte-identical to a fresh `ds run` of the repinned
/// tree.
#[test]
fn a_repinned_dep_swaps_alongside_the_root() {
    let fix = multi_package(&lib_member("n * 2"));
    swap_source(&fix.entry, dep_program(6)).expect("the source writes");
    let mut command = ds();
    command
        .env("DS_CACHE", &fix.cache)
        .env_remove("DS_REGISTRY")
        .arg("dev")
        .arg(&fix.entry);
    let mut dev = Dev::launch(command, 500);
    assert_eq!(
        dev.line(),
        "0",
        "the first round links @lib and runs twice(0)"
    );
    assert_eq!(
        dev.line(),
        "2",
        "the parked frame's next call runs v1's twice"
    );
    // the dep edit ships a version bump and a repin: `ds save` installs
    // the new content and the root manifest pins it
    let v2 = save_lib(
        &fix.lib,
        &fix.cache,
        "name = \"@lib\"\nversion = \"1.3.0\"\n",
        &lib_member("n * 10"),
    );
    swap_source(fix.root.join("ds.pkg"), pin_manifest("1.3.0", v2)).expect("the repin writes");
    assert_eq!(
        dev.line(),
        "20",
        "the moved dep swaps and the parked frame's next call runs the new body"
    );
    assert_eq!(dev.line(), "30");
    assert_eq!(dev.line(), "40");
    assert_eq!(dev.line(), "50");
    assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
    dev.kill();

    // the differential: the post-swap tail is byte-identical to a fresh
    // `ds run` of the repinned tree
    let fresh = ds()
        .env("DS_CACHE", &fix.cache)
        .env_remove("DS_REGISTRY")
        .arg("run")
        .arg("--print-result")
        .arg(&fix.entry)
        .output()
        .expect("the fresh run completes");
    assert!(fresh.status.success(), "{:?}", fresh.stderr);
    let tail = dev.without_answer_echo(2);
    let fresh_tail = String::from_utf8_lossy(&fresh.stdout)
        .lines()
        .skip(2)
        .fold(String::new(), |text, line| text + line + "\n");
    assert_eq!(
        tail, fresh_tail,
        "the swapped drive's tail prints what the fresh run prints"
    );
}

/// A moved dep whose incoming table drops the name a live frame's call
/// site targets refuses the swap: the root member and the dep rename
/// land in one round (the consumer updated to the renamed API, so the
/// root's compile succeeds against the new surface), but the parked
/// frame still runs the old root code whose cross-module site names the
/// old `twice` — the swap re-check refuses (`PoolRefMisroute`), the
/// installed versions keep running, and the next repin that restores
/// the name lands the swap through the kept name (the re-check's own
/// judge), so the drive picks the new dep code up at its next call.
#[test]
fn a_dep_swap_refusing_over_a_live_call_site_keeps_running() {
    let fix = multi_package(&lib_member("n * 2"));
    swap_source(&fix.entry, dep_program(6)).expect("the source writes");
    let mut command = ds();
    command
        .env("DS_CACHE", &fix.cache)
        .env_remove("DS_REGISTRY")
        .arg("dev")
        .arg(&fix.entry);
    let mut dev = Dev::launch(command, 500);
    assert_eq!(
        dev.line(),
        "0",
        "the first round links @lib and runs twice(0)"
    );
    assert_eq!(
        dev.line(),
        "2",
        "the parked frame's next call runs v1's twice"
    );
    // the rename round: the dep renames `twice` away and the root
    // consumer follows, so the round compiles clean against the new
    // surface — the refusal is the swap re-check's alone, naming the
    // live old frame's site
    let v2 = save_lib(
        &fix.lib,
        &fix.cache,
        "name = \"@lib\"\nversion = \"1.3.0\"\n",
        "pub fn double(n: int) -> int {\n  return n * 10\n}\n",
    );
    swap_source(fix.root.join("ds.pkg"), pin_manifest("1.3.0", v2)).expect("the repin writes");
    swap_source(
        &fix.entry,
        "import @lib/helper (double)\n\nfn main() -> int {\n  var i = 0\n  while i < 6 {\n    \
         println(\"${double(i)}\")\n    i = i + 1\n  }\n  return 0\n}\n",
    )
    .expect("the consumer update writes");
    assert_eq!(
        dev.line(),
        "4",
        "the refused swap keeps the installed versions running (2 * 2)"
    );
    let diagnostic = dev.err();
    assert!(
        diagnostic.contains("swapping the moved dep `@lib` refused"),
        "the refusal names the moved dep: {diagnostic}"
    );
    assert!(
        diagnostic.contains("PoolRefMisroute"),
        "the refusal is the swap re-check's structured one: {diagnostic}"
    );
    // the recovery repin restores the old name and the root member with
    // it: the live site's kept name passes the re-check, the swap lands,
    // and the parked frame's next call runs the new body through the
    // swapped slot (i resumes at 3)
    let v3 = save_lib(
        &fix.lib,
        &fix.cache,
        "name = \"@lib\"\nversion = \"1.4.0\"\n",
        &lib_member("n * 10"),
    );
    swap_source(fix.root.join("ds.pkg"), pin_manifest("1.4.0", v3)).expect("the recovery writes");
    swap_source(&fix.entry, dep_program(6)).expect("the member restores");
    assert_eq!(
        dev.line(),
        "30",
        "the restored name lands the swap and the parked frame runs the new body"
    );
    assert_eq!(dev.line(), "40");
    assert_eq!(dev.line(), "50");
    assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
    dev.kill();
}

/// A directory-shaped dep's source edit drives the same reload round an
/// entry edit does: the watch set grows over the resolved path dep's
/// directory (the tree discipline inside it), the watched bytes compose
/// the dep's members and manifest beside the root's own, and the
/// re-resolution reads the dep from source — its identity moved, so the
/// moved dep hot-swaps under its own name through the two-slot
/// semantics with no repin, the manifest bytes never changing.
#[test]
fn a_path_dep_source_edit_hot_reloads_without_a_repin() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let cache = dir.path().join("store");
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).expect("the lib dir creates");
    swap_source(lib.join("ds.pkg"), "name = \"@lib\"\nversion = \"1.2.0\"\n")
        .expect("the manifest writes");
    swap_source(lib.join("helper.ds"), lib_member("n * 2")).expect("the member writes");
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).expect("the root dir creates");
    swap_source(
        root.join("ds.pkg"),
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.@lib]\npath = \"../lib\"\n",
    )
    .expect("the manifest writes");
    let entry = root.join("app.ds");
    swap_source(&entry, dep_program(6)).expect("the source writes");
    let mut command = ds();
    command
        .env("DS_CACHE", &cache)
        .env_remove("DS_REGISTRY")
        .arg("dev")
        .arg(&entry);
    let mut dev = Dev::launch(command, 500);
    assert_eq!(
        dev.line(),
        "0",
        "the first round links @lib from source and runs twice(0)"
    );
    assert_eq!(
        dev.line(),
        "2",
        "the parked frame's next call runs the source-read twice"
    );
    // the dep source edit alone: no ds save, no manifest repin — the
    // directory is the source of truth the round re-reads
    swap_source(lib.join("helper.ds"), lib_member("n * 10")).expect("the dep edit writes");
    assert_eq!(
        dev.line(),
        "20",
        "the moved dep swaps and the parked frame's next call runs the new body"
    );
    assert_eq!(dev.line(), "30");
    assert_eq!(dev.line(), "40");
    assert_eq!(dev.line(), "50");
    assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
    dev.kill();

    // and the same tree's plain run links the edited dep from source
    // with no repin: the fresh run prints the new body throughout
    let fresh = ds()
        .env("DS_CACHE", &cache)
        .env_remove("DS_REGISTRY")
        .arg("run")
        .arg(&entry)
        .output()
        .expect("the fresh run completes");
    assert!(fresh.status.success(), "{:?}", fresh.stderr);
    assert_eq!(
        String::from_utf8_lossy(&fresh.stdout).into_owned(),
        "0\n10\n20\n30\n40\n50\n",
        "the fresh run serves the edited dep source"
    );
}

/// A world-parked entry wakes at a watch window's pump: the entry's
/// accept parks (the dev session's VM installs the platform's reactor,
/// the `ds run` precedent) and the drive hands the window back, the
/// watch keeps its cadence beside the parked op — a refused reload
/// round reports while the accept stands — and the peer's late
/// connection wakes the entry, its answer crossing exactly the echo
/// round under `ds run`. The shape is the linux driver's own; a host
/// without one keeps the blocking accept twin.
#[cfg(target_os = "linux")]
#[test]
fn a_parked_accept_wakes_at_the_watch_windows_pump() {
    use std::net::TcpStream;

    /// A port with nothing bound on it: bind, note, drop.
    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        listener.local_addr().expect("bound address").port()
    }

    let dir = tempfile::tempdir().expect("a temp dir");
    let prog = dir.path().join("parked_accept.ds");
    for _ in 0..8 {
        let port = free_port();
        let source = format!(
            "fn main() {{\n  const l = net_listen(\"127.0.0.1\", {port})\n  \
             println(\"listening\")\n  const c = accept(l)\n  println(\"accepted\")\n  0\n}}\n"
        );
        swap_source(&prog, &source).expect("the source writes");
        let mut dev = Dev::spawn(&prog, 50);
        let listening = dev.line();
        if listening != "listening" {
            // a stolen port halts the failed bind before the park: re-pick
            assert!(
                listening.contains("net error:"),
                "the first round prints or halts: {listening}"
            );
            dev.kill();
            continue;
        }
        // the entry parks on the accept; the watch keeps its cadence — a
        // refused reload round reports while the accept stands
        swap_source(&prog, "fn broken {\n").expect("the broken source writes");
        let diagnostic = dev.err();
        assert!(
            diagnostic.contains("DS"),
            "the refused round reports beside the parked accept: {diagnostic}"
        );
        let peer = TcpStream::connect(("127.0.0.1", port)).expect("the peer dials");
        assert_eq!(dev.line(), "accepted", "the window's pump wakes the accept");
        assert_eq!(dev.line(), "0", "the completed entry answers the unit word");
        // the accepted connection owns its deinit at scope exit: a clean
        // close the dropped session proves
        drop(peer);
        dev.kill();
        return;
    }
    panic!("no free loopback port answered the bind");
}
