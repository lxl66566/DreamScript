//! The `ds test` runner through the binary: discovery in ascending
//! order, `--list` without running, the pinned per-test lines and the
//! trailing summary, the failing tests' captured output under its
//! fence (a passing test's output stays swallowed), the exit
//! discipline (0 all-pass, 1 any failure), and the bare `no tests`
//! answer on a root without the directory.

use std::{env, path::PathBuf, process::Command};

mod support;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: spawned runs never read or
/// write a real user cache directory, and the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-test-cmd-cache-")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn out(command: &mut Command) -> String {
    let produced = command.output().expect("the binary runs");
    String::from_utf8_lossy(&produced.stdout).into_owned()
}

#[test]
fn list_prints_the_discovered_tests_in_ascending_order() {
    let stdout = out(ds().arg("test").arg(fixture("test_pkg")).arg("--list"));
    assert_eq!(
        stdout,
        "tests/fail_exit.ds\ntests/fail_trap.ds\ntests/pass.ds\n"
    );
}

#[test]
fn a_root_without_tests_answers_bare_no_tests_clean() {
    let mut command = ds();
    command.arg("test").arg(fixture("test_pkg_notests"));
    let produced = command.output().expect("the binary runs");
    assert!(produced.status.success(), "no tests is a clean answer");
    assert_eq!(
        String::from_utf8_lossy(&produced.stdout),
        "no tests\n",
        "one line, nothing else"
    );
}

#[test]
fn the_runner_pins_the_lines_the_fence_and_the_summary() {
    let mut command = ds();
    command.arg("test").arg(fixture("test_pkg"));
    let produced = command.output().expect("the binary runs");
    assert_eq!(
        produced.status.code(),
        Some(1),
        "any failure exits 1 (a trap and a deliberate exit(2) failed)"
    );
    let stdout = String::from_utf8_lossy(&produced.stdout).into_owned();
    // ascending order, one line per test, the wall time trailing
    assert!(
        stdout.contains("test tests/fail_exit.ds ... FAILED ("),
        "{stdout}"
    );
    assert!(
        stdout.contains("test tests/fail_trap.ds ... FAILED ("),
        "{stdout}"
    );
    assert!(stdout.contains("test tests/pass.ds ... ok ("), "{stdout}");
    // the failing runs' captured output shows under its fence, the
    // passing run's output stays swallowed
    assert!(stdout.contains("---- tests/fail_trap.ds ----"), "{stdout}");
    assert!(stdout.contains("trap: die: boom"), "{stdout}");
    assert!(stdout.contains("---- tests/fail_exit.ds ----"), "{stdout}");
    assert!(!stdout.contains("unreached"), "{stdout}");
    // the trailing summary closes the run
    assert!(
        stdout.contains("test result: failed. 1 passed; 2 failed; 3 total"),
        "{stdout}"
    );
    // the fence and the summary follow every per-test line
    let last_line = stdout.find("test tests/pass.ds ... ok").expect("pass line");
    let fence = stdout
        .find("---- tests/fail_trap.ds ----")
        .expect("the fence");
    let summary = stdout.find("test result:").expect("the summary");
    assert!(last_line < fence && fence < summary, "{stdout}");
}

#[test]
fn an_all_pass_root_exits_clean_and_swallows_the_output() {
    let mut command = ds();
    command.arg("test").arg(fixture("test_pkg_pass"));
    let produced = command.output().expect("the binary runs");
    assert!(produced.status.success(), "all-pass exits 0");
    let stdout = String::from_utf8_lossy(&produced.stdout).into_owned();
    assert!(stdout.contains("test tests/pass.ds ... ok ("), "{stdout}");
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed; 1 total"),
        "{stdout}"
    );
    // a passing test's stdout never reaches the runner's output
    assert!(!stdout.contains("hidden"), "{stdout}");
}
