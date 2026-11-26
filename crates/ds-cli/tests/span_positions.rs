//! The pc-to-span section's faces end to end through the `ds` binary
//! [H7]: a trap's backtrace renders each kept frame's file, line, and
//! column through the module's span table, and the boundary's
//! unhandled-error report prints the `?` legs' recorded chain beside
//! the message. The run path arms the VM's error-trace switch; the
//! section is the one source channel every report names.

use std::{
    env, fs,
    path::PathBuf,
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
    support::shared_dir("ds-cli-span-cache-")
}

fn run(args: &[&str]) -> Output {
    ds().args(args).output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Writes one manifest-free script under a fresh temp dir and answers
/// the file's path (the name the span table records).
fn script(tag: &str, source: &str) -> PathBuf {
    let dir = support::guarded_dir(&format!("ds-cli-span-{tag}-"));
    let file = dir.join("app.ds");
    fs::write(&file, source).expect("the script writes");
    file
}

/// A trap's backtrace names each frame's position: the trapping `x + 1`
/// renders as file:line:column, not a raw pc.
#[test]
fn a_trap_backtrace_renders_positions() {
    let file = script(
        "trap",
        "fn inner(x: int) -> int {\n    x + 1\n}\n\nfn main() -> int {\n    \
         inner(9223372036854775807)\n}\n",
    );
    let path = file.to_str().expect("the path is UTF-8");
    let output = run(&["run", path]);
    let err = text(&output.stderr);
    assert!(!output.status.success(), "the overflow exits nonzero");
    assert!(err.contains("trap: integer overflow"), "stderr: {err}");
    assert!(
        err.contains(&format!("inner at {path}:2:5")),
        "the trapping frame names its own position: {err}"
    );
}

/// The boundary's chain: the caught `?` leg recorded the failing frame,
/// and the total spelling's halt prints the chain beside the report —
/// the hop named through the span table. The refused http_get keeps the
/// run network-free (the connection refuses locally).
#[test]
fn the_boundary_prints_the_recorded_chain() {
    let file = script(
        "chain",
        "fn probe() -> int | io_err {\n    const text = read_file(\"/nonexistent\")?\n    5\n}\n\n\
         fn main() -> string {\n    const a = probe().err((_: io_err) => 0)\n    const page = \
         http_get(\"http://127.0.0.1:1/x\")\n    page\n}\n",
    );
    let path = file.to_str().expect("the path is UTF-8");
    let output = run(&["run", path]);
    let out = text(&output.stdout);
    let err = text(&output.stderr);
    assert!(!output.status.success(), "the halt exits nonzero");
    assert!(out.contains("net error:"), "the message on stdout: {out}");
    assert!(
        err.contains("error trace (innermost first):"),
        "the chain beside the report: {err}"
    );
    assert!(
        err.contains(&format!("at {path}:2:18 in app::probe")),
        "the hop names the `?` site: {err}"
    );
    assert!(
        err.contains("the run halted on an unhandled net failure"),
        "the boundary line: {err}"
    );
}
