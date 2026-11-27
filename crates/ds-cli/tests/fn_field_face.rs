//! End-to-end through the `ds` binary: the fn-typed field doors the
//! DSI shape-phase mint carries. A fn-typed struct field and a
//! fn-typed case payload mint fn-signature atoms in the shape phase,
//! the run path's checked-prefix encode walks every one of them, and
//! the serve replays them — the doors run clean with and without a
//! call through the field.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-fn-field-face-{name}-"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Runs the command, removes the working directory, answers the process
/// output.
fn run(command: &mut Command, dir: &PathBuf) -> Output {
    let out = command.output().expect("the binary runs");
    let _ = fs::remove_dir_all(dir);
    out
}

fn ds_in(dir: &PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.env("DS_CACHE", dir.join("cache"));
    command
}

/// The fn-typed field doors run clean: the never-called field, the
/// dot-call, the indirect value read, and the fn-typed case payload.
#[test]
fn the_fn_typed_field_doors_run() {
    let dir = workdir("doors");
    let out = run(
        ds_in(&dir).arg("run").arg(fixture("fn_field_face.ds")),
        &dir,
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "dot 20 indirect 2 payload 21\n",
        "the fn-typed field doors' golden stdout drifted"
    );
}
