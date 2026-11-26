//! The @std open error box end to end through the `ds` binary: the
//! explicit conversion entering from a concrete taxonomy member (the
//! path face's `io_err`, the json face's `parse_err`, the decode
//! bridge's `utf8_err`), `?`'s containment routing the box member out
//! to the boundary fn's return union, and the payload and message
//! extracting over the narrowed box. The containment check itself
//! stays unchanged: a `?` whose error side the return union does not
//! carry refuses — the no-implicit-widening rule.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

mod support;

/// The binary with a private working directory (the fixture writes its
/// probe file by a relative path) and a private script-cache root.
fn ds_in(dir: &PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(dir);
    command.env("DS_CACHE", dir.join("cache"));
    command
}

/// A fresh empty working directory per call, below the OS temp root.
fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-error-box-{name}-"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Runs the command and flattens the outcome.
fn run(command: &mut Command) -> Output {
    command.output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The acceptance run: the conversion entering from the three concrete
/// members through `map_err` handlers, `?` routing the box member into
/// each boundary fn's own return union, the success side passing
/// through untouched, and the boundary rendering the class and the
/// message off the narrowed box — the program exits 0 with the box
/// handled, never halted.
#[test]
fn the_error_box_face_runs_through_the_ambient() {
    let dir = workdir("face");
    let out = run(ds_in(&dir).arg("run").arg(fixture("error_box_face.ds")));
    let _ = fs::remove_dir_all(&dir);
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        text(&out.stdout),
        concat!(
            "key=value\n",
            "missing (config unread)\n",
            "{\"a\":1}\n",
            "malformed at 2 (config malformed)\n",
            "ok\n",
            "bad bytes (name not utf-8)\n",
            "field config unread\n",
            "payload not_found\n",
            "0\n",
        ),
        "the box face's golden stdout drifted"
    );
}

/// The containment check is unchanged: a `?` whose operand carries
/// `io_err` against a return union naming only `box_err` refuses — no
/// implicit widening valve opens, the conversion is the only way in.
#[test]
fn the_propagation_refuses_outside_the_return_union() {
    const REFUSAL: &str = concat!(
        "fn load(path: string) -> string | box_err {\n",
        "  read_file(path)?\n",
        "}\n",
    );
    let dir = workdir("refusal");
    let source = dir.join("error_box_refusal.ds");
    fs::write(&source, REFUSAL).expect("the refusing script writes");
    let out = run(ds_in(&dir).arg("check").arg(&source));
    let _ = fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "the unchecked containment refuses: {out:?}"
    );
    let rendered = text(&out.stderr);
    assert!(
        rendered.contains("DS2008"),
        "the refusal names the propagation class: {rendered}"
    );
    assert!(
        rendered.contains("which the return union `string | box_err` does not contain"),
        "the refusal names both unions: {rendered}"
    );
}
