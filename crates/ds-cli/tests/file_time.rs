//! End-to-end through the `ds` binary over the real ambient: the
//! platform file/time face — `open_file`/`read_exact` over the ambient's
//! package-owned `file_handle` struct (each helper call's scope end fires the
//! deinit and closes the handle, so the read loop pins that nothing
//! leaks), the `time` op's clock word, the ambient's own json
//! reader/writer pair — and the linear discipline's negative pin (a moved file binding used
//! afterwards refuses at the affine pass).

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

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
    let dir = env::temp_dir().join(format!("ds-cli-file-time-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the working directory creates");
    dir
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Runs the command, removes the working directory, answers the
/// process output.
fn run(command: &mut Command, dir: &PathBuf) -> Output {
    let out = command.output().expect("the binary runs");
    let _ = fs::remove_dir_all(dir);
    out
}

/// The acceptance: the reads print their lengths, the bytes members ride
/// the same reads, the scoped-open loop completes without exhausting
/// the process's descriptors, the clock answers a sane ordered pair, and
/// parse_json minifies.
#[test]
fn the_file_time_face_runs_through_the_ambient() {
    let dir = workdir("face");
    let out = run(
        ds_in(&dir).arg("run").arg(fixture("file_time_face.ds")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "head 44\n",
            "bytes 4 112\n",
            "total 8000\n",
            "clock true\n",
            "{\"a\":[1,true,null]}\n",
            "0\n",
        ),
        "the golden stdout drifted"
    );
}

/// The linear discipline: a file binding consumed by a `move` window
/// and used afterwards refuses at the affine pass — the handle's
/// ownership is a compile-time fact, never a runtime close error.
#[test]
fn a_moved_file_binding_used_afterwards_refuses() {
    let dir = workdir("moved");
    let out = run(
        ds_in(&dir).arg("run").arg(fixture("file_move_refusal.ds")),
        &dir,
    );
    assert!(
        !out.status.success(),
        "the move-then-use must refuse: {out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("error[DS3001]"),
        "the refusal is the affine moved-again diagnostic, was: {stderr}"
    );
    assert!(
        stderr.contains("value `f` is no longer usable"),
        "the diagnostic names the binding, was: {stderr}"
    );
}
