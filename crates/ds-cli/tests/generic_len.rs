//! End-to-end generic calls through the `ds` binary: the
//! ambient `len[t]` runs in source mode over both receiver shapes with
//! distinct outputs, and the `ds build` round trip reproduces the source
//! run's output byte for byte.

use std::{env, fs, path::PathBuf, process::Command};

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
    support::shared_dir("ds-cli-test-cache-")
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/generic_len.ds")
}

fn run_ok(command: &mut Command) -> String {
    let out = command.output().expect("the binary runs");
    assert!(out.status.success(), "the run failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The acceptance: `len` over a string counts chars and over a list
/// counts elements, in both call spellings — the unit entry's trailing
/// `0` line is the script-mode convention.
#[test]
fn len_prints_distinct_outputs_per_receiver_shape() {
    assert_eq!(
        run_ok(ds().arg("run").arg(fixture())),
        "string 11 11\nlist 3 3\n"
    );
}

/// The compiled round trip reproduces the source run exactly.
#[test]
fn built_module_reproduces_the_source_run() {
    let module = env::temp_dir().join(format!("ds_generic_len_probe-{}.dsmod", std::process::id()));
    let _ = fs::remove_file(&module);
    let out = ds()
        .arg("build")
        .arg(fixture())
        .arg("-o")
        .arg(&module)
        .output()
        .expect("the binary builds");
    assert!(out.status.success(), "the build failed: {out:?}");
    assert_eq!(
        run_ok(ds().arg("run").arg(&module)),
        "string 11 11\nlist 3 3\n"
    );
    let _ = fs::remove_file(&module);
}
