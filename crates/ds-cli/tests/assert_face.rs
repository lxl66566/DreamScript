//! The @std assert family end to end through the `ds` binary: the
//! bool-typed `assert` and the per-width equality pair abort through
//! the ambient `panic` (the deliberate-bug spelling's own member face),
//! the differing-equality message rendering the operands into the trap
//! line, and the passing calls answering in statement position. The
//! refused generic spelling pins its classes: the abort shape's
//! unsolved union at the check, and the equality compare's missing
//! runtime word kind at lowering — every door a generic body needs —
//! so the concrete per-width members are the landed surface.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

mod support;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: spawned runs never read or
/// write a real user cache directory.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-assert-cache-")
}

fn run(args: &[&str]) -> Output {
    ds().args(args).output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_str()
        .expect("fixture path is UTF-8")
        .to_owned()
}

/// Writes one manifest-free script under a fresh temp dir and answers
/// the file's path.
fn script(tag: &str, source: &str) -> PathBuf {
    let dir = support::guarded_dir(&format!("ds-cli-assert-{tag}-"));
    let file = dir.join("app.ds");
    fs::write(&file, source).expect("the script writes");
    file
}

/// The affirmative face: every member answers on its satisfied arm and
/// the run exits clean.
#[test]
fn the_assert_family_answers_on_the_satisfied_arms() {
    let out = run(&["run", &fixture("assert_face.ds")]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the face run exits clean: {}",
        text(&out.stderr)
    );
    assert_eq!(text(&out.stdout), "face asserted\n0\n", "the face answers");
}

/// A false condition aborts through the ambient panic: the message
/// rides the Die trap line and the backtrace names the caller.
#[test]
fn a_false_assert_aborts_through_the_panic() {
    let entry = script(
        "false",
        "fn main() -> int {\n  assert(1 == 2, \"arithmetic broke\")\n  0\n}\n",
    );
    let out = run(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the abort exits failed");
    assert!(text(&out.stdout).is_empty(), "stdout stays clean");
    let err = text(&out.stderr);
    assert!(err.contains("trap: die: arithmetic broke"), "stderr: {err}");
    assert!(
        err.contains("app::main"),
        "the backtrace names the caller: {err}"
    );
}

/// A differing equality renders the operands into the trap line, the
/// message carrying the left and right words.
#[test]
fn a_differing_equality_renders_the_operands_into_the_trap_line() {
    let entry = script(
        "diff",
        "fn main() -> int {\n  assert_eq_int(2, 3, \"ints\")\n  0\n}\n",
    );
    let out = run(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the abort exits failed");
    let err = text(&out.stderr);
    assert!(
        err.contains("trap: die: ints left 2 right 3"),
        "the rendered message rides the trap: {err}"
    );
}

/// The refused generic spelling: the natural `assert_eq_any` body
/// refuses at the check first — the panic return's type variable never
/// solves inside the if arm, so the arm's value unions it into a
/// `() | t` that does not fit the unit answer.
#[test]
fn the_generic_equality_spelling_refuses_at_the_abort_shape() {
    let out = run(&["run", &fixture("assert_generic_refusal.ds")]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the generic spelling refuses: {}",
        text(&out.stderr)
    );
    let err = text(&out.stderr);
    assert!(
        err.contains("not contained in `()`"),
        "the refusal names the unsolved union: {err}"
    );
}

/// The generic equality compare underneath: the checker admits `==`
/// over a type variable and the per-shape instantiation lowers it —
/// the site's solved one-word shape plans an instance whose compare
/// dispatches through the builtin instance the call stages, so the
/// concrete per-width landing answers through its own body. The abort
/// arm keeps the abort shape.
#[test]
fn the_generic_equality_compare_runs_through_its_instance() {
    let out = run(&["run", &fixture("assert_generic_equality.ds")]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the generic compare lowers through its instance: {}",
        text(&out.stderr)
    );
    let err = text(&out.stderr);
    assert!(err.is_empty(), "the instance compare traps nothing: {err}");
}
