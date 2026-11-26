//! The CLI process surface end to end through the `ds` binary: the
//! program reads the verbatim string arguments
//! through `args()`, reads the environment through `env(name)`, writes
//! stderr through `eprint`/`eprintln`, reads stdin through `read_line`,
//! spawns children through `proc_run`, and the host owns the exit code
//! — clean completion 0, a `die` trap 1, an explicit `exit(code)` its
//! deliberate status (a negative code trapping to 1 with the standard
//! trap note, a code past 255 wrapping to its low byte).

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

mod support;

/// One private cache root per test binary, so the runs never touch a
/// real user cache directory.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-process-cache-")
}

/// Writes one source program beside the test's other fixtures.
fn source_file(tag: &str, text: &str) -> PathBuf {
    let dir = support::guarded_dir("ds-cli-process-src-");
    let path = dir.join(format!("{tag}.ds"));
    fs::write(&path, text).expect("the source writes");
    path
}

/// Runs `ds run <source>` with the given extra arguments.
fn ds_run(source: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command
        .env("DS_CACHE", test_cache())
        .arg("run")
        .arg(source)
        .args(args);
    command.output().expect("the ds binary runs")
}

/// Runs `ds run <source>` with piped stdin content.
fn ds_run_stdin(source: &Path, stdin_text: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
        .arg("run")
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the ds binary spawns");
    child
        .stdin
        .as_mut()
        .expect("stdin pipes")
        .write_all(stdin_text.as_bytes())
        .expect("the input writes");
    child.wait_with_output().expect("the run completes")
}

#[test]
fn args_reaches_the_program_verbatim() {
    let source = source_file(
        "argv",
        "fn main() -> () {\n  const argv = args()\n  println(\"argc=${len(argv)}\")\n  var i = \
         0\n  while i < len(argv) {\n    println(\"${argv.get(i).ok((w: string) => w).err((_: \
         none) => \"?\")}\")\n    i = i + 1\n  }\n}\n",
    );
    // no arguments: the empty list
    let out = ds_run(&source, &[]);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "argc=0\n0\n");

    // strings stay strings: spaces, empties, and non-numeric words ride
    // verbatim, the shape `--args` could never carry as i64 words
    let out = ds_run(&source, &["--args", "hello world", "", "x7"]);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "argc=3\nhello world\n\nx7\n0\n"
    );
}

#[test]
fn env_reads_the_process_environment() {
    let source = source_file(
        "env",
        "fn main() -> () {\n  println(\"${env(\"DS_E2E_PRESENT\").ok((v: string) => v).err((_: \
         none) => \"<none>\")}\")\n  println(\"${env(\"DS_E2E_ABSENT\").ok((v: string) => \
         v).err((_: none) => \"<none>\")}\")\n}\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
        .env("DS_E2E_PRESENT", "here")
        .env_remove("DS_E2E_ABSENT")
        .arg("run")
        .arg(&source)
        .output()
        .expect("the ds binary runs");
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "here\n<none>\n0\n");
}

#[test]
fn eprint_writes_stderr_leaving_stdout_clean() {
    let source = source_file(
        "stderr",
        "fn main() -> () {\n  eprintln(\"oops\")\n  eprint(\"tail\\n\")\n  println(\"fine\")\n}\n",
    );
    let out = ds_run(&source, &[]);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fine\n0\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "oops\ntail\n");
}

#[test]
fn read_line_reads_stdin_until_eof() {
    let source = source_file(
        "stdin",
        "fn main() -> () {\n  println(\"${read_line().ok((l: string) => l).err((_: none) => \
         \"<eof>\")}\")\n  println(\"${read_line().ok((l: string) => l).err((_: none) => \
         \"<eof>\")}\")\n  println(\"${read_line().ok((l: string) => l).err((_: none) => \
         \"<eof>\")}\")\n}\n",
    );
    let out = ds_run_stdin(&source, "first\nsecond\n");
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "first\nsecond\n<eof>\n0\n"
    );
}

#[test]
fn proc_run_answers_the_childs_exit_status() {
    // the child is the ds binary itself running a script, portable
    // everywhere the tests run
    let child = source_file(
        "child",
        "fn main() -> () {\n  println(\"child speaks\")\n}\n",
    );
    let ds = PathBuf::from(env!("CARGO_BIN_EXE_ds"))
        .to_string_lossy()
        .replace('\\', "/");
    let child_path = child.to_string_lossy().replace('\\', "/");
    let source = source_file(
        "parent",
        &format!(
            "fn main() -> () {{\n  const status = proc_run(\"{ds}\", [\"run\", \
             \"{child_path}\"]).err((_: io_err) => -1)\n  println(\"child=${{status}}\")\n}}\n"
        ),
    );
    let out = ds_run(&source, &[]);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("child speaks\n"),
        "the child's stdout inherits: {stdout}"
    );
    assert!(stdout.contains("child=0\n"), "the status answers: {stdout}");
}

#[test]
fn the_host_owns_the_exit_code() {
    // clean completion exits 0
    let clean = source_file("clean", "fn main() -> () {\n  println(\"done\")\n}\n");
    let out = ds_run(&clean, &[]);
    assert_eq!(out.status.code(), Some(0), "a clean completion exits 0");

    // a die trap prints its message and exits 1
    // the trailing statement keeps the generic `die` call out of tail
    // position (a sole generic-extern call cannot unify its answer)
    let dies = source_file(
        "dies",
        "fn main() -> () {\n  die(\"boom\")\n  println(\"unreachable\")\n}\n",
    );
    let out = ds_run(&dies, &[]);
    assert_eq!(out.status.code(), Some(1), "a die trap exits 1");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("die: boom"),
        "the die message reports: {:?}",
        out.stderr
    );

    // exit(code) answers the deliberate status with no report, even
    // from inside a helper the entry calls
    let exits = source_file(
        "exits",
        "fn bail(code: int) -> () {\n  exit(code)\n  println(\"unreachable\")\n}\nfn main() -> () \
         {\n  println(\"before\")\n  bail(3)\n  println(\"after\")\n}\n",
    );
    let out = ds_run(&exits, &[]);
    assert_eq!(out.status.code(), Some(3), "exit(3) answers 3");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "before\n",
        "nothing past the exit runs"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "the deliberate status prints no report"
    );

    // exit(0) is a clean early completion
    let zero = source_file(
        "zero",
        "fn main() -> () {\n  exit(0)\n  println(\"unreachable\")\n}\n",
    );
    let out = ds_run(&zero, &[]);
    assert_eq!(out.status.code(), Some(0), "exit(0) answers 0");

    // a wide status wraps to its low byte, POSIX process semantics
    let wide = source_file(
        "wide",
        "fn main() -> () {\n  exit(1000)\n  println(\"unreachable\")\n}\n",
    );
    let out = ds_run(&wide, &[]);
    assert_eq!(out.status.code(), Some(232), "exit(1000) wraps to 232");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "the wrapped status prints no report"
    );

    // a negative status traps: the run answers 1 with the standard
    // trap note (the trapping-arithmetic discipline — a deliberate
    // failure never reads as clean success)
    let negative = source_file(
        "negative",
        "fn main() -> () {\n  exit(-1)\n  println(\"unreachable\")\n}\n",
    );
    let out = ds_run(&negative, &[]);
    assert_eq!(out.status.code(), Some(1), "exit(-1) traps to 1");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("die: exit: -1 is out of range 0..=255"),
        "the trap note reports: {:?}",
        out.stderr
    );

    // a trap without die still exits 1
    let traps = source_file("traps", "fn main() -> int {\n  return 1 /% 0\n}\n");
    let out = ds_run(&traps, &[]);
    assert_eq!(out.status.code(), Some(1), "a trap exits 1");
}

/// A never-returning call sits in a block's tail and in the script
/// fold's synthesized main once the answers answer `never`: the
/// deliberate status reaches the process from either seam, and the
/// statements before the tail ran.
#[test]
fn a_tailing_exit_answers_at_both_seams() {
    // the script fold: a top-level `exit` ends the synthesized main's
    // body, the deliberate status the run's own answer
    let script = source_file("script", "println(\"bye\")\nexit(0)\n");
    let out = ds_run(&script, &[]);
    assert_eq!(out.status.code(), Some(0), "the deliberate status lands");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "bye\n",
        "the prefix ran"
    );

    // the arm union: an `exit` inside an if arm leaves the arm's exit
    // standing in for the join, the status riding out the same way
    let armed = source_file("armed", "if 1 > 0 {\n  exit(3)\n}\nprintln(\"never\")\n");
    let out = ds_run(&armed, &[]);
    assert_eq!(out.status.code(), Some(3), "the armed exit lands");
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "the continuation never ran: {:?}",
        out.stdout
    );
}

/// The `exit` spelling is an ambient declaration beside the proc face:
/// the check driver resolves it with no host provision (which
/// capability serves the module is an install concern the check never
/// sees), while a `--no-ambient` run keeps the name unknown. The
/// capability refusal itself rides the install — an embedder whose
/// platform omits `proc` refuses the module naming the capability (the
/// entry session's own pin) — and the CLI's run always serves the
/// capability, so the deliberate status works wherever the ambient
/// does.
#[test]
fn exit_checks_from_the_ambient_and_refuses_without_it() {
    // the trailing statement keeps the generic call out of tail
    // position (a sole generic-extern call cannot unify its answer)
    let source = source_file(
        "exit_refusal",
        "fn main() -> () {\n  exit(3)\n  println(\"unreachable\")\n}\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
        .arg("check")
        .arg(&source)
        .output()
        .expect("the ds binary runs");
    assert!(out.status.success(), "the ambient spelling checks: {out:?}");
    assert_eq!(out.stderr, b"", "the check is silent");

    let out = ds_run(&source, &["--no-ambient"]);
    assert_eq!(out.status.code(), Some(1), "the bare run exits failed");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error[DS2004]: unknown name `exit`"),
        "the bare run refuses the un-ambiented spelling: {err}"
    );
}

/// The spelling's name collides like any ambient face: a script
/// redeclaring `exit` refuses as a duplicate against the ambient's
/// declaration, so no shadowing spelling routes around the proc
/// capability's op row.
#[test]
fn a_redeclared_exit_refuses_against_the_ambient_member() {
    let source = source_file(
        "exit_redeclared",
        "extern fn exit[t](code: int) -> t\n\nfn main() -> () {\n  exit(3)\n}\n",
    );
    let out = ds_run(&source, &[]);
    assert_eq!(out.status.code(), Some(1), "the collision exits failed");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error[DS2005]: `exit` is already defined") && err.contains("ambient.ds"),
        "the redeclaration names the ambient member: {err}"
    );
}

/// An `exit` inside a nursery child is a hard trap, not a boundary
/// error: the deliberate status tears the drive down like `die`, the
/// host answering the child's code with no report printed.
#[test]
fn an_exit_inside_a_nursery_child_answers_its_status() {
    let source = source_file(
        "exit_nursery",
        "fn main() -> () {\n  println(\"start\")\n  nursery {\n    spawn {\n               \
         exit(7)\n    }\n  }\n  println(\"unreachable\")\n}\n",
    );
    let out = ds_run(&source, &[]);
    assert_eq!(out.status.code(), Some(7), "the child's deliberate status");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "start\n",
        "nothing past the torn-down nursery runs"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "the deliberate status prints no report"
    );
}
