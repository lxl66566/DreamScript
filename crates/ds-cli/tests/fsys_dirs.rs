//! End-to-end through the `ds` binary over the real ambient: the fsys
//! path round — `mkdir`'s single-level create, `remove`'s one-entry
//! removal, and `fs_size`'s byte count — each failure answering the
//! io_err union the ambient owns, the is-narrowing reading its class.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

/// The binary with a private working directory (the fixture writes its
/// probe files by relative paths) and a private script-cache root.
fn ds_in(dir: &PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(dir);
    command.env("DS_CACHE", dir.join("cache"));
    command
}

/// A fresh empty working directory per call, below the OS temp root.
fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-fsys-dirs-{name}-"))
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

/// The acceptance: the byte count over the written probe, the create
/// and its already_exists branch, the non-empty directory's io_other
/// floor, the file's removal, the missing path's not_found answers,
/// and the emptied directory's sweep — every handled failure printed
/// nothing beyond its own report line.
#[test]
fn the_fsys_path_round_runs_through_the_ambient() {
    let dir = workdir("face");
    let out = run(
        ds_in(&dir).arg("run").arg(fixture("fsys_dirs_face.ds")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "size 5\n",
            "dir made\n",
            "again exists\n",
            "inner made\n",
            "full other\n",
            "file removed\n",
            "missing gone\n",
            "back gone\n",
            "inner removed\n",
            "swept\n",
            "clean false\n",
            "0\n",
        ),
        "the golden stdout drifted"
    );
}
