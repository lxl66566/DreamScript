//! The REPL through the `ds` binary over piped stdin: one session binds a
//! linear resource, uses it in a later line, observes its deinit, drops
//! and resets it; one session pins the expression print (value, inferred
//! type, effect row); one session pins rejection and recovery (a syntax
//! error, an unknown name, a trap — each leaving the session usable) and
//! the `:` command refusals; one session runs a `#[derive(show)]`
//! declaration and calls the derived member — the check queries run over
//! the derive-expanded trees, so the call clears the check grain and the
//! run lowers through the analyzed query's own expanded trees.

use std::{env, process::Command};

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
fn session_binds_uses_drops_and_resets_a_linear_resource() {
    // The deinit fires at every run's scope exit while the binding lives
    // (twice: the binding line, the use line), never again after :drop,
    // and the dropped name is gone until :reset clears the broken text.
    let (out, ok) = session(
        "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}\nconst \
         r = res { id: 7 }\nr.id + 1\n:drop r\nr.id + 1\n:reset\n1 + 1\n",
    );
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[
        "DreamScript repl",
        ">>> ... ... ... res declared",
        ">>> dying 7\nr: res\n~ pure",
        ">>> dying 7\n8\n: int\n~ trap",
        ">>> dropped r",
        "error[DS2004]: unknown name `r`",
        ">>> session reset",
        ">>> 2\n: int\n~ trap",
    ]);
    assert_eq!(
        out.matches("dying 7").count(),
        2,
        "the deinit fires once per run while the binding lives, never after the drop: {out}"
    );
}

#[test]
fn expression_prints_value_type_and_effect_row() {
    // The tail expression's value, its inferred type, and its own effect
    // row (the per-span table): int arithmetic traps on overflow (B7), a
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
    // unexpanded is the misuse the M3 contract refuses.
    let (out, ok) = session(concat!(
        "#[derive(show)] struct point { x: int, y: int }\n",
        "const p = point { x: 1, y: 2 }\n",
    ));
    assert!(ok, "the session exits cleanly: {out}");
    in_order(&out, &[">>> point declared", ">>> p: point\n~ pure"]);
}
