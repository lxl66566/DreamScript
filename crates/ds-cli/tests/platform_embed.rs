//! End-to-end through the `ds` binary: the embed platform provider
//! — the run path installs the console-only `embed` handler beside the
//! default `std`, so a source declaring `platform "embed"` runs over
//! the console face, while a source whose declaration claims a world op
//! beyond it refuses at install with the unprovided-op error.

use std::{env, path::PathBuf, process::Command};

use ds_vm::platform::OP_FILE_SIZE;

mod support;

fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-test-cache-")
}

/// Runs the built binary and flattens the outcome.
fn ds_run(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The console face serves both print and println through the installed
/// embed handler, on the compile path and again on a warm second run.
#[test]
fn the_embed_platform_serves_the_console_face() {
    for pass in ["cold", "warm"] {
        let (ok, out, err) = ds_run(&["run", "--no-ambient", "tests/fixtures/embed_console.ds"]);
        assert!(ok, "the {pass} run succeeds (stderr: {err})");
        assert_eq!(out, "hello from embed\n", "the {pass} run prints");
    }
}

/// A source whose declaration claims a world op the embed handler does
/// not serve passes the checker (usage inside the claim) and refuses at
/// install — the structured unprovided-op error, the designed refusal.
#[test]
fn an_embed_source_refuses_world_ops_at_install() {
    let (ok, out, err) = ds_run(&["run", "--no-ambient", "tests/fixtures/embed_fs_refused.ds"]);
    assert!(!ok, "the fsys call site under embed exits nonzero");
    assert_eq!(out, "", "nothing runs before the install refuses");
    assert!(
        err.contains(&format!(
            "calls world op {OP_FILE_SIZE} (`fsys`), which the platform `embed` does not provide"
        )),
        "the install names the unprovided capability: {err}"
    );
}
