//! The process face's capture half end to end through the `ds` binary:
//! a piped `proc_spawn` answers the linear `proc_handle`, `write` feeds
//! the child's stdin, `read_out` takes its captured stdout (the empty
//! string the pipe's orderly end), `wait` answers the exit status
//! idempotently, and the closing deinit kills and reaps a still-running
//! child — a program dropping the handle without a wait still exits
//! clean and promptly. A failed spawn keeps the total spelling, so it
//! halts on the io error line like a failed bind halts on the net
//! error line. Every child is the ds binary itself running a local
//! script: portable everywhere the tests run, no external commands.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Instant,
};

/// One private cache root per test binary, so the runs never touch a
/// real user cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir =
                env::temp_dir().join(format!("ds-cli-proc-capture-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// Writes one source program into the test's private source directory.
fn source_file(tag: &str, text: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-proc-capture-src-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the source directory creates");
    let path = dir.join(format!("{tag}.ds"));
    fs::write(&path, text).expect("the source writes");
    path
}

/// The ds binary's path, forward-slipped so a ds string literal holds
/// it on every platform.
fn ds_path() -> String {
    PathBuf::from(env!("CARGO_BIN_EXE_ds"))
        .to_string_lossy()
        .replace('\\', "/")
}

/// The path spelling a ds string literal holds.
fn ds_str(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Runs `ds run <source>` under the private cache.
fn ds_run(source: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
        .arg("run")
        .arg(source)
        .output()
        .expect("the ds binary runs")
}

#[test]
fn the_capture_rows_answer_output_and_status() {
    // the child prints one line and answers the deliberate status 7
    let child = source_file(
        "child_status",
        "fn main() -> () {\n  println(\"child speaks\")\n  exit(7)\n  \
         println(\"unreachable\")\n}\n",
    );
    let source = source_file(
        "capture",
        &format!(
            "fn main() -> int {{\n  const p = proc_spawn(\"{ds}\", [\"run\", \"{child}\"])\n  \
             var captured = \"\"\n  var done = false\n  while !done {{\n    const text = \
             read_out(p, 4096)\n    match text {{\n      not_found => println(\"not_found\")\n      \
             permission_denied => println(\"permission_denied\")\n      already_exists => \
             println(\"already_exists\")\n      timed_out => println(\"timed_out\")\n      \
             unexpected_eof => println(\"unexpected_eof\")\n      io_other {{ msg }} => \
             println(\"other: ${{msg}}\")\n      out => {{\n        if out == \"\" {{\n          done = \
             true\n        }} else {{\n          captured = captured + out\n        }}\n      }}\n    \
             }}\n  }}\n  const status = wait(p).err((_: io_err) => -1)\n  const again = \
             wait(p).err((_: io_err) => -1)\n  println(\"captured=${{captured}}\")\n  \
             println(\"status=${{status}} again=${{again}}\")\n  0\n}}\n",
            ds = ds_path(),
            child = ds_str(&child),
        ),
    );
    let out = ds_run(&source);
    assert!(
        out.status.success(),
        "status {:?} stdout {} stderr {:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("captured=child speaks\n"),
        "the child's stdout rides the pipe: {stdout:?}"
    );
    assert!(
        stdout.contains("status=7 again=7"),
        "the wait answers the child's status idempotently: {stdout:?}"
    );
}

#[test]
fn the_write_row_feeds_the_childs_stdin() {
    // the child answers one line of its own stdin back on stdout
    let child = source_file(
        "child_reads",
        "fn main() -> () {\n  const line = read_line().ok((l: string) => l).err((_: none) => \
         \"<eof>\")\n  println(\"got: ${line}\")\n}\n",
    );
    let source = source_file(
        "feeds",
        &format!(
                "fn main() -> int {{\n  const p = proc_spawn(\"{ds}\", [\"run\", \"{child}\"])\n  \
                 write(p, \"ping\\n\")\n  println(\"wrote\")\n  const text = read_out(p, 4096)\n  \
                 match text {{\n    not_found => println(\"not_found\")\n    permission_denied => \
                 println(\"permission_denied\")\n    already_exists => \
                 println(\"already_exists\")\n    timed_out => println(\"timed_out\")\n    \
                 unexpected_eof => println(\"unexpected_eof\")\n    io_other {{ msg }} => \
                 println(\"other: ${{msg}}\")\n    out => println(\"read=${{out}}\")\n  }}\n  \
                 const status = wait(p).err((_: io_err) => -1)\n  \
                 println(\"status=${{status}}\")\n  0\n}}\n",
                ds = ds_path(),
                child = ds_str(&child),
            ),
    );
    let out = ds_run(&source);
    assert!(
        out.status.success(),
        "status {:?} stdout {} stderr {:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("read=got: ping\n"),
        "the write reaches the child's stdin and the read takes its answer: {stdout:?}"
    );
    assert!(
        stdout.contains("status=0"),
        "the child exits clean: {stdout:?}"
    );
}

#[test]
fn a_failed_capture_spawn_halts_on_the_total_spellings_error_line() {
    let source = source_file(
        "failed_spawn",
        "fn main() -> int {\n  const p = proc_spawn(\"ds-proc-capture-no-such-program\", [])\n  \
         println(\"unreachable\")\n  0\n}\n",
    );
    let out = ds_run(&source);
    assert!(
        !out.status.success(),
        "a failed spawn never reads as clean success: status {:?}",
        out.status.code()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("io error: proc_spawn ds-proc-capture-no-such-program:"),
        "the total spelling's failure halts on the io error line: {stdout:?}"
    );
}

#[test]
fn the_closing_deinit_kills_a_still_running_child() {
    // the child sleeps far past the parent's whole run, so only the
    // handle's closing deinit can end it — the parent drops the handle
    // without a wait and still exits clean and promptly
    let child = source_file(
        "child_sleeps",
        "fn main() -> () {\n  sleep(60000000000)\n}\n",
    );
    let source = source_file(
        "drops",
        &format!(
            "fn main() -> int {{\n  const p = proc_spawn(\"{ds}\", [\"run\", \"{child}\"])\n  \
             println(\"spawned\")\n  0\n}}\n",
            ds = ds_path(),
            child = ds_str(&child),
        ),
    );
    let started = Instant::now();
    let out = ds_run(&source);
    let elapsed = started.elapsed();
    assert!(
        out.status.success(),
        "a dropped handle's deinit keeps the exit clean: status {:?} stdout {} stderr {:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "spawned\n0\n");
    // generous against parallel load: the child sleeps a minute, the
    // run ends when the deinit kills it
    assert!(
        elapsed.as_secs() < 30,
        "the deinit reaps the child instead of waiting it out: {elapsed:?}"
    );
}
