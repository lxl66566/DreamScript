//! The REPL through the `ds` binary over piped stdin: one session binds a
//! linear resource, uses it in a later line, observes its deinit, drops
//! and resets it; one session pins the expression print (value, inferred
//! type, effect row); one session pins rejection and recovery (a syntax
//! error, an unknown name, a trap — each leaving the session usable) and
//! the `:` command refusals; one session runs a `#[derive(show)]`
//! declaration and calls the derived member — the check queries run over
//! the derive-expanded trees, so the call clears the check grain and the
//! run lowers through the analyzed query's own expanded trees; the trap
//! sessions pin the rollback's drain (a trapped line's own deinit
//! bindings fire exactly once, a parked binding moved into the trapped
//! call survives) and `:drop`'s declaration half (text-level only, the
//! next round's check surfacing a still-referenced name); one session
//! pins the rollback's line-scoped nursery teardown (a trapping line's
//! abandoned nursery dies with the rollback, so its never-entered
//! child never runs and the next install clears the recycled
//! slots).

use std::{env, process::Command};

mod support;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command
}

/// Runs one piped session and returns stdout plus the exit status.
fn session(input: &str) -> (String, bool) {
    use std::io::Write;
    let mut child = ds()
        .arg("repl")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the binary spawns");
    child
        .stdin
        .as_mut()
        .expect("stdin pipes")
        .write_all(input.as_bytes())
        .expect("the input writes");
    let out = child.wait_with_output().expect("the session drains");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// Asserts each fragment appears, in order, in the output.
fn in_order(output: &str, fragments: &[&str]) {
    let mut cursor = 0;
    for fragment in fragments {
        let found = output[cursor..]
            .find(fragment)
            .unwrap_or_else(|| panic!("missing `{fragment}` after byte {cursor} in:\n{output}"));
        cursor += found + fragment.len();
    }
}

#[test]
fn session_binds_drops_and_resets_a_linear_resource() {
    // The resumed frame: the binding lives in the parked frame (no deinit
    // at the binding line — the scope never exits between submissions),
    // `:drop` fires the deinit exactly once at the drop, the dropped name
    // is gone, and `:reset` clears the session. A use line keeps its
    // reference, so the drop of a used binding refuses (the later test).
    let (out, ok) = session(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\nconst \
         r = res { id: 7 }\n:drop r\nr\n:reset\n1 + 1\n",
    );
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        "DreamScript repl",
        ">>> ... ... ... res declared",
        ">>> r: res\n~ pure",
        ">>> dying 7\ndropped r",
        "error[DS2004]: unknown name `r`",
        ">>> session reset",
        ">>> 2\n: int\n~ trap",
    ]);
    assert_eq!(
        out.matches("dying 7").count(),
        1,
        "the deinit fires exactly once, at the drop — never at the binding line, never again \
         after: {out}"
    );
}

#[test]
fn side_effects_run_once_across_submissions() {
    // The pin of the resumed frame: a print in submission 1 prints once
    // across submissions 2..n — prior statements never re-run — and each
    // later submission's own print fires exactly its own once.
    let (out, ok) = session(concat!(
        "println(\"first\")\n",
        "1 + 1\n",
        "const x = 5\n",
        "println(\"second\")\n",
        "x + 1\n",
        "2 + 2\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    assert_eq!(
        out.matches("first").count(),
        1,
        "one print, one firing: {out}"
    );
    assert_eq!(
        out.matches("second").count(),
        1,
        "one print, one firing: {out}"
    );
    in_order(&out, &[
        "first", "2\n: int", "x: int", "second", "6\n: int", "4\n: int",
    ]);
}

#[test]
fn the_session_end_runs_the_surviving_deinits() {
    // The scope exit at end of input: the surviving binding's deinit
    // fires there, the dropped binding's fired exactly once at its drop,
    // and neither fires twice.
    let (out, ok) = session(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"bye ${self.id}\") }\n}\n",
        "const a = res { id: 1 }\n",
        "const b = res { id: 2 }\n",
        ":drop a\n",
        "2 + 2\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        "a: res",
        "b: res",
        "bye 1\ndropped a",
        "4\n: int",
        "bye 2",
    ]);
    assert_eq!(
        out.matches("bye 1").count(),
        1,
        "the drop fires once: {out}"
    );
    assert_eq!(
        out.matches("bye 2").count(),
        1,
        "the scope exit fires once: {out}"
    );
}

#[test]
fn a_trap_leaves_the_live_frame_consistent() {
    // A trapping line never commits and the frame does not advance: the
    // prior binding still reads, the trapped line is gone from the
    // session, and the corrected resubmission runs.
    let (out, ok) = session(concat!("const x = 10\n", "1 / 0\n", "x + 5\n", "6 / 2\n",));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        "x: int",
        "trap: division by zero",
        ">>> 15\n: int",
        ">>> 3\n: int",
    ]);
}

/// The trapped line's own deinit-carrying bindings die their one death at
/// the rollback's drain: a temp the failed statement established and a
/// callee local kept by the interrupted frame both fire their hooks
/// exactly once when the trap releases them (the report prints first),
/// while the parked binding survives untouched and fires only at the
/// session end — nothing fires twice.
#[test]
fn a_trapped_lines_own_deinits_fire_once_at_the_rollback() {
    let (out, ok) = session(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n",
        "fn inner() -> int {\nconst u = res { id: 3 }\n1 / 0\n}\n",
        "const r = res { id: 1 }\n",
        "{\nconst t = res { id: 2 }\n1 / 0\n}\n",
        "inner()\n",
        "r.id\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        ">>> r: res",
        "trap: division by zero",
        "dying 2",
        "trap: division by zero",
        "dying 3",
        ">>> 1\n: int",
    ]);
    assert_eq!(
        out.matches("dying 1").count(),
        1,
        "the parked binding fires exactly once, at the session end: {out}"
    );
    assert_eq!(
        out.matches("dying 2").count(),
        1,
        "the trapped temp fires exactly once: {out}"
    );
    assert_eq!(
        out.matches("dying 3").count(),
        1,
        "the trapped callee local fires exactly once: {out}"
    );
}

/// A value the parked frame still holds through its binding survives a
/// trapped consuming call: the rollback releases the hand-off count
/// without firing the hook (the value's real death — a later read's
/// line, the session end, or a `:drop` — fires it exactly once).
#[test]
fn a_parked_binding_moved_into_a_trapped_call_survives() {
    let (out, ok) = session(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n",
        "fn boom(x: res) -> int {\nx.id / 0\n}\n",
        "const r = res { id: 5 }\n",
        "boom(r)\n",
        "r.id\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        "boom: fn(res) -> int",
        ">>> r: res",
        "trap: division by zero",
        ">>> 5\n: int",
        "dying 5",
    ]);
    assert_eq!(
        out.matches("dying 5").count(),
        1,
        "the moved binding fires exactly once, at the session end: {out}"
    );
}

/// A trapped line's abandoned nursery tears down with the rollback: the
/// never-entered child's line never prints (nothing drives it), the
/// trapped block's own binding fires its deinit exactly once at each
/// rollback's drain, and the slots recycle — the retried trap reopens
/// its nursery over the torn slot and tears it down again, and the
/// next round's healthy nursery block joins its child exactly as a
/// fresh line would. The machinery pin (the slot states and the
/// recycling) lives in ds-vm's trapped_nursery_release suite.
#[test]
fn a_trapped_lines_abandoned_nursery_tears_down_at_the_rollback() {
    let (out, ok) = session(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n",
        "const r = res { id: 1 }\n",
        "{\nconst t = res { id: 2 }\nnursery {\nspawn { println(\"never\") }\n1 / 0\n}\n}\n",
        "{\nconst t = res { id: 3 }\nnursery {\nspawn { println(\"never\") }\n1 / 0\n}\n}\n",
        "nursery {\nspawn { println(\"ran\") }\n}\n",
        "r.id\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        "r: res",
        "trap: division by zero",
        "dying 2",
        "trap: division by zero",
        "dying 3",
        "ran",
        "1\n: int",
        "dying 1",
    ]);
    assert_eq!(
        out.matches("never").count(),
        0,
        "the never-entered children's lines never run: {out}"
    );
    assert_eq!(
        out.matches("dying 2").count(),
        1,
        "the first trapped block's binding fires exactly once, at its rollback: {out}"
    );
    assert_eq!(
        out.matches("dying 3").count(),
        1,
        "the retried trap's binding fires exactly once, at its own rollback: {out}"
    );
    assert_eq!(
        out.matches("dying 1").count(),
        1,
        "the parked binding fires exactly once, at the session end: {out}"
    );
}

/// `:drop` of a declaration is text-level only: the block leaves the
/// session at once (no frame words exist for it to release) and the next
/// round's own check surfaces any name a surviving line still
/// references — a binding's drop refuses up front instead (the earlier
/// test).
#[test]
fn dropping_a_declaration_surfaces_still_referenced_names_at_the_next_check() {
    let (out, ok) = session(concat!(
        "fn twice(x: int) -> int { x * 2 }\n",
        "const a = twice(21)\n",
        ":drop twice\n",
        "1 + 1\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        ">>> twice: fn(int) -> int",
        ">>> a: int",
        ">>> dropped twice",
        "error[DS2004]: unknown name `twice`",
    ]);
}

#[test]
fn a_referenced_binding_refuses_its_drop_and_the_session_continues() {
    // A name other lines reference cannot die textually — the referencing
    // line would poison every later submission's check — so the drop
    // names it and stays; an alias's drop renames only (the value lives
    // with its owner, which still reads afterwards).
    let (out, ok) = session(concat!(
        "const a = 21\n",
        "const b = a\n",
        ":drop a\n",
        ":drop b\n",
        "a + 1\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        ">>> a: int",
        ">>> b: int",
        "cannot drop a: `const b = a` still references it",
        "b holds no live words of its own (a constant or an alias)\ndropped b",
        ">>> 22\n: int",
    ]);
}

#[test]
fn expression_prints_value_type_and_effect_row() {
    // The tail expression's value, its inferred type, and its own effect
    // row (the per-span table): int arithmetic traps on overflow, a
    // println line is unit-typed and world-effecting — and the second
    // line's row stays the tail's own `world`, not the session row
    // `trap | world` the earlier statement widens `main` to.
    let (out, ok) = session("1 + 2\nprintln(\"hi\")\n");
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[">>> 3\n: int\n~ trap", ">>> hi\n: ()\n~ world"]);
}

#[test]
fn errors_reject_the_line_and_the_session_recovers() {
    // A syntax error, an unknown name, and a trap each print through the
    // ordinary diagnostic pipeline (or the trap reporter), keep the line
    // out of the session, and the next line runs clean. Unknown `:` words
    // and `:drop` misspellings refuse with a usage line.
    let (out, ok) = session(
        "const broken = \n1 + 1\nnope + 1\n2 + 2\n1 / 0\n3 + 3\n:wat\n:drop ghost\n:drop\n",
    );
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        "error[DS1001]",
        ">>> 2\n: int",
        "error[DS2004]: unknown name `nope`",
        ">>> 4\n: int",
        "trap: division by zero",
        ">>> 6\n: int",
        "unknown command :wat (commands: :quit, :drop <name>, :reset)",
        "no binding ghost in the session",
        "usage: :drop <name>",
    ]);
}

#[test]
fn a_derived_members_call_checks_and_runs_in_a_session() {
    // The member's call clears the expansion-aware check grain and runs
    // through the synthesized impl body, printing the construction
    // spelling of the value.
    let (out, ok) = session(concat!(
        "#[derive(show)] struct point { x: int, y: int }\n",
        "const p = point { x: 1, y: 2 }\n",
        "println(p.show())\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        ">>> point declared",
        ">>> p: point\n~ pure",
        ">>> point { x: 1, y: 2 }\n: ()\n~ world",
    ]);
}

#[test]
fn a_derived_declaration_lowers_through_the_analyzed_query() {
    // A `#[derive(show)]` declaration joins the session as a declaration
    // block and the line still runs: the run path lowers the analyzed
    // bundle's own expanded trees, where the consumed attribute became
    // an ordinary impl item — an attribute reaching the lowering pass
    // unexpanded is the misuse lowering refuses.
    let (out, ok) = session(concat!(
        "#[derive(show)] struct point { x: int, y: int }\n",
        "const p = point { x: 1, y: 2 }\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[">>> point declared", ">>> p: point\n~ pure"]);
}

/// A multi-word answer prints its whole span, one line per word exactly
/// like `ds run` — never a head-word truncation.
#[test]
fn a_multi_word_answer_prints_its_whole_span() {
    let (out, ok) = session("(1, true)\n");
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[">>> 1\ntrue\n: (int, bool)"]);
}

/// `:drop` renames only the binding-name token: a name that is a
/// substring of the `const`/`var` head (`n` in `const`, `a` in `var`)
/// used to capture the keyword itself and poison every later line.
#[test]
fn drop_renames_the_binding_name_token_only() {
    let (out, ok) = session(concat!(
        "const n = 5\n",
        "var a = 1\n",
        ":drop n\n",
        ":drop a\n",
        "1 + 1\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        ">>> n: int",
        ">>> a: int",
        "dropped n",
        "dropped a",
        ">>> 2\n: int",
    ]);
}

/// Runs one program text through `ds run` and answers its stdout: the
/// differential twin of a REPL session's own prints.
fn run_stdout(source: &str) -> String {
    let dir = support::guarded_dir("ds-repl-diff-");
    let path = dir.join("twin.ds");
    std::fs::write(&path, source).expect("the twin source writes");
    let output = ds()
        .arg("run")
        .arg(path.as_os_str())
        .env("DS_CACHE", dir.join("cache").as_os_str())
        .output()
        .expect("the ds binary runs");
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the twin runs (stderr: {})",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_file(&path).expect("the twin source removes");
    out
}

/// The program's own print lines (every line the program printed,
/// stripped of the REPL's prompt chrome — the first `>>> ` and any
/// chained `... ` continuations): the differential surface.
fn program_lines(out: &str, stems: &[&str]) -> Vec<String> {
    out.lines()
        .map(|line| {
            let mut line = line;
            while let Some(stripped) = line.strip_prefix(">>> ").or(line.strip_prefix("... ")) {
                line = stripped;
            }
            line
        })
        .filter(|line| stems.iter().any(|stem| line.starts_with(stem)))
        .map(str::to_owned)
        .collect()
}

/// A session holding a deinit-carrying binding accepts its next
/// declaration round: the growing file's deinit entries append right
/// after their own items, so the parked row's drop sites keep naming
/// them at stable indices and the install passes — the program's prints
/// staying byte-identical to a fresh `ds run` of the same text (the
/// twin scopes the binding in a block, the ordinary-mode spelling of
/// the session's own end-scoped death).
#[test]
fn a_deinit_binding_session_accepts_its_next_declaration_round() {
    let (out, ok) = session(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"bye ${self.id}\") }\n}\n",
        "const r = res { id: 1 }\n",
        "fn f(x: int) -> int { x * 2 }\n",
        "println(\"forty:${f(21)}\")\n",
        "println(\"rid:${r.id}\")\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    assert!(
        !out.contains("installing the session module failed"),
        "the declaration round installs over the parked frame: {out}"
    );
    in_order(&out, &[
        ">>> ... ... ... res declared",
        ">>> r: res",
        ">>> f: fn(int) -> int",
        ">>> forty:42",
        ">>> rid:1",
        "bye 1",
    ]);
    let twin = run_stdout(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"bye ${self.id}\") }\n}\n",
        "fn f(x: int) -> int { x * 2 }\n",
        // the twin hardcodes the field read's answer: an ordinary-mode
        // field read of an affine struct is the consuming hand-off, so
        // the session's end-scoped death needs the value unread past
        // its print (the block's scope end standing in for the session
        // end, the `0` tail keeping the println off main's native tail)
        "fn main() {\n{\nconst r = res { id: 1 \
         }\nprintln(\"forty:${f(21)}\")\nprintln(\"rid:1\")\n}\n0\n}\n",
    ));
    assert_eq!(
        program_lines(&out, &["forty:", "rid:", "bye "]),
        program_lines(&twin, &["forty:", "rid:", "bye "]),
        "the session's own prints stay byte-identical to the fresh run"
    );
}

/// A `var` re-assigned in a round fires the replaced value's deinit
/// exactly once at each death and the frame's final content at the
/// session end — the same fired set a fresh `ds run` produces through
/// the ordinary exit drops: the store's re-registration re-establishes
/// the establishment band the old content's drop spent.
#[test]
fn a_reassigned_vars_replaced_value_fires_its_deinit_like_a_fresh_run() {
    let (out, ok) = session(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n",
        "var x = res { id: 1 }\n",
        "x = res { id: 2 }\n",
        "x = res { id: 3 }\n",
        "println(\"keep:${x.id}\")\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    assert_eq!(
        out.matches("dying 1").count(),
        1,
        "the first fires once: {out}"
    );
    assert_eq!(
        out.matches("dying 2").count(),
        1,
        "the second fires once: {out}"
    );
    assert_eq!(
        out.matches("dying 3").count(),
        1,
        "the third fires once: {out}"
    );
    in_order(&out, &["dying 1", "dying 2", "keep:3", "dying 3"]);
    let twin = run_stdout(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n",
        // the twin hardcodes the read's answer and tails with `0`: the
        // session's keep print must not ride main's native tail (which
        // abandons the exit drops) and its read of the live binding is
        // the pinned mode's borrow, not the ordinary hand-off
        "fn main() {\nvar x = res { id: 1 }\nx = res { id: 2 }\nx = res { id: 3 \
         }\nprintln(\"keep:3\")\n0\n}\n",
    ));
    assert_eq!(
        program_lines(&out, &["dying ", "keep:"]),
        program_lines(&twin, &["dying ", "keep:"]),
        "the session's fired set matches the fresh run byte for byte"
    );
}

/// A `var` re-assigned inside a loop (the rollover shape: the carried
/// claim survives the back-edge join, each iteration's store firing the
/// content it replaces) fires every content's deinit exactly once, the
/// final one at the session end — matching a fresh `ds run` through the
/// ordinary exit drops.
#[test]
fn a_loop_rollovers_replaced_values_fire_like_a_fresh_run() {
    let (out, ok) = session(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n",
        "var x = res { id: 1 }\n",
        "var i = 0\n",
        "while i < 2 {\nx = res { id: i + 10 }\ni = i + 1\n}\n",
        "x.id\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &["dying 1", "dying 10", "11\n: int", "dying 11"]);
    assert_eq!(
        out.matches("dying ").count(),
        3,
        "each content fires once: {out}"
    );
    let twin = run_stdout(concat!(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\n",
        "fn main() {\nvar x = res { id: 1 }\nvar i = 0\nwhile i < 2 {\nx = res { id: i + 10 }\ni \
         = i + 1\n}\n0\n}\n",
    ));
    assert_eq!(
        program_lines(&out, &["dying "]),
        program_lines(&twin, &["dying "]),
        "the session's fired set matches the fresh run byte for byte"
    );
}

/// One line event off a piped child's stdout: a detached reader thread
/// per pipe, so the assertions never block on a quiet pipe.
fn line_channel(pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufRead::lines(std::io::BufReader::new(pipe)) {
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

/// A parked submission wakes at the pump between the host's reads: the
/// submission's sleep parks its frame (the session's VM installs the
/// platform's reactor, the `ds run` precedent), the prompt returns and
/// stays silent while the host blocks on stdin — the deadline fires
/// with nothing pumping — and the next line's pass pumps the seam
/// first, the expired deadline waking the parked fiber exactly `ds
/// run`'s executor would: the submission's remaining prints land ahead
/// of the line just read. The shape is the linux driver's own; a host
/// without one keeps the blocking sleep twin, the submission never
/// parking.
#[cfg(target_os = "linux")]
#[test]
fn a_parked_sleep_submission_wakes_at_the_next_reads_pump() {
    use std::{io::Write as _, process::Stdio, sync::mpsc::Receiver, thread, time::Duration};
    let mut child = ds()
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary spawns");
    let mut stdin = child.stdin.take().expect("stdin pipes");
    let stdout = child.stdout.take().expect("stdout pipes");
    let lines: Receiver<String> = line_channel(stdout);
    let mut seen: Vec<String> = Vec::new();
    let mut next_line = |stem: &str| {
        let line = lines
            .recv_timeout(Duration::from_secs(90))
            .unwrap_or_else(|_| panic!("no `{stem}` line within 90s"));
        seen.push(line.clone());
        line
    };
    // the sleeping submission: one print before the park, one after
    stdin
        .write_all(b"{\nprintln(\"before\")\nsleep(200000000)\nprintln(\"after\")\n}\n")
        .expect("the submission writes");
    stdin.flush().expect("the submission flushes");
    let banner = next_line("banner");
    assert!(banner.contains("DreamScript repl"), "the banner: {banner}");
    assert!(
        next_line("before").contains("before"),
        "the submission prints before the park"
    );
    // the sleep's 200ms deadline fires while the host blocks on stdin:
    // no pump runs, so the parked submission's tail cannot print
    thread::sleep(Duration::from_millis(600));
    assert!(
        lines.try_recv().is_err(),
        "the parked submission stays silent while the host blocks on stdin"
    );
    // the read's pass pumps the seam first: the expired deadline wakes
    // the submission, its tail landing ahead of the line just read
    stdin.write_all(b"1 + 1\n").expect("the line writes");
    stdin.flush().expect("the line flushes");
    let woken = next_line("after");
    assert!(
        woken.contains("after"),
        "the pump wakes the submission: {woken}"
    );
    // the landing's report (the unit type, the effect row) prints ahead
    // of the line the read just answered
    let unit = next_line(": ()");
    assert!(
        unit.contains(": ()"),
        "the landing reports its story: {unit}"
    );
    let row = next_line("~");
    assert!(
        row.starts_with('~'),
        "the landing reports its effect row: {row}"
    );
    let answer = next_line("2");
    assert!(
        answer.contains('2'),
        "the new line runs after the landing: {answer}"
    );
    drop(stdin);
    let status = child.wait().expect("the session drains");
    assert!(status.success(), "the session exits cleanly");
    assert_eq!(
        seen.iter().filter(|line| line.contains("after")).count(),
        1,
        "the parked submission's tail prints exactly once: {seen:?}"
    );
}
