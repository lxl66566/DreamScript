//! The die split's end-to-end pins (ruled 2026-11-17): the trap
//! primitive is a builtin native the lowerer routes by name, the `die`
//! spelling is the CLI host's own provision for its entry sessions (a
//! declaration member registered beside the ambient — never ambient or
//! `@std` source), and the halt policy — what the trap prints and which
//! exit code follows — is the host's own, riding the trap report
//! beside the io/net/utf8 halt rows. A compile without the provision
//! (the check driver, a `--no-ambient` run, an embedder's bare VM)
//! keeps the boundary the ambient shed left: `die` is an unknown name
//! at the checker, never a callee that checks clean.

use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output},
};

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-process script-cache root: spawned runs never read or
/// write a real user cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = env::temp_dir().join(format!("ds-cli-die-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// Writes one script under a temp dir and answers its path.
fn script(tag: &str, name: &str, text: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-die-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir creates");
    let file = dir.join(name);
    fs::write(&file, text).expect("the script writes");
    file
}

fn run(args: &[&str]) -> Output {
    ds().args(args).output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Asserts each fragment appears, in order.
fn in_order(haystack: &str, fragments: &[&str]) {
    let mut cursor = 0;
    for fragment in fragments {
        let found = haystack[cursor..]
            .find(fragment)
            .unwrap_or_else(|| panic!("missing `{fragment}` after byte {cursor} in:\n{haystack}"));
        cursor += found + fragment.len();
    }
}

/// The entry halt: the trap carries the message out of the engine (the
/// native prints nothing and decides no exit code), the host's report
/// prints the die row through the trap report, and the run exits
/// failed — stdout stays exactly what ran before the die.
#[test]
fn the_entry_run_halts_with_the_die_row_and_a_failed_exit() {
    let file = script(
        "run",
        "die_run.ds",
        "fn main() -> () {\n  println(\"before\")\n  die(\"boom\")\n  println(\"after\")\n}\n",
    );
    let out = run(&["run", file.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the die run exits failed");
    assert_eq!(text(&out.stdout), "before\n", "nothing runs past the die");
    let err = text(&out.stderr);
    in_order(&err, &["trap: die: boom", "die_run::main"]);
}

/// A die inside a nursery child is a hard trap, not a boundary error:
/// the teardown releases the abandoned nursery and the drive reports
/// the message once, exiting failed.
#[test]
fn a_die_trap_inside_a_nursery_child_tears_the_drive_down() {
    let file = script(
        "nursery",
        "die_nursery.ds",
        "fn main() -> () {\n  println(\"start\")\n  nursery {\n    spawn {\n      die(\"child \
         boom\")\n    }\n  }\n  println(\"unreachable\")\n}\n",
    );
    let out = run(&["run", file.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the trapped drive exits failed");
    assert_eq!(text(&out.stdout), "start\n");
    in_order(&text(&out.stderr), &[
        "trap: die: child boom",
        "die_nursery::main",
    ]);
}

/// The un-hosted spelling refuses at the checker: the check driver and
/// a `--no-ambient` run both serve the bare-embedder view, where `die`
/// is an unknown name — the provision never leaks below the host.
#[test]
fn the_unhosted_spelling_stays_an_unknown_name() {
    let file = script(
        "refusal",
        "die_run.ds",
        "fn main() -> () {\n  die(\"boom\")\n}\n",
    );
    let out = run(&["check", file.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the check exits failed");
    in_order(&text(&out.stderr), &["error[DS2004]: unknown name `die`"]);
    let out = run(&[
        "run",
        "--no-ambient",
        file.to_str().expect("temp path is UTF-8"),
    ]);
    assert_eq!(out.status.code(), Some(1), "the bare run exits failed");
    in_order(&text(&out.stderr), &["error[DS2004]: unknown name `die`"]);
}

/// The provision's name collides like any ambient face: a script
/// redeclaring `die` refuses as a duplicate against the host member,
/// so no shadowing spelling ever routes around the host's declaration.
#[test]
fn a_redeclared_spelling_refuses_against_the_host_member() {
    let file = script(
        "redeclared",
        "die_run.ds",
        "extern fn die[t](msg: string) -> t\n\nfn main() -> () {\n  die(\"boom\")\n}\n",
    );
    let out = run(&["run", file.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the collision exits failed");
    in_order(&text(&out.stderr), &[
        "error[DS2005]: `die` is already defined",
        "$host.ds",
    ]);
}

/// A package root run carries the provision too: the manifest-beside
/// entry member compiles through the same entry session, so its die
/// call lowers and the run answers the same halt policy.
#[test]
fn a_package_root_run_carries_the_provision() {
    let dir = env::temp_dir().join(format!("ds-cli-die-root-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the package dir creates");
    fs::write(dir.join("ds.pkg"), "name = \"app\"\nversion = \"0.1.0\"\n")
        .expect("the manifest writes");
    fs::write(
        dir.join("app.ds"),
        "fn main() -> () {\n  die(\"root boom\")\n  println(\"unreachable\")\n}\n",
    )
    .expect("the member writes");
    let entry = dir.join("app.ds");
    let out = run(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the package run exits failed");
    in_order(&text(&out.stderr), &["trap: die: root boom", "app::main"]);
    fs::remove_dir_all(&dir).expect("the temp tree removes");
}

/// The repl reports a die line through the trap report and rolls the
/// poisoned submission back: the session accepts the next line exactly
/// as before and exits clean.
#[test]
fn a_die_line_reports_and_rolls_back_in_the_repl() {
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
        .write_all(b"die(\"repl boom\")\n40 + 2\n")
        .expect("the input writes");
    let out = child.wait_with_output().expect("the session drains");
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "the session exits cleanly: {stdout}");
    in_order(&stdout, &["trap: die: repl boom", "42"]);
}
