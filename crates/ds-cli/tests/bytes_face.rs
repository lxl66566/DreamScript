//! The bytes representation face end to end through the `ds` binary
//! (the 2026-11-18 ruling): `show` renders the payload as a byte list
//! over the ambient's ds-source impl, the derive families compose over
//! a bytes part (the ambient's show/ord/hash impls answering the
//! synthesized dispatches — before the ruling each refused at the
//! missing member), and the UTF-8 bridges take the string's own bytes
//! each way, the decode answering the taxonomy's `string | utf8_err`
//! union through match extraction.

use std::{
    env, fs,
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
            let dir = env::temp_dir().join(format!("ds-cli-test-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(command: &mut Command) -> Output {
    command.output().expect("the binary runs")
}

/// The acceptance: the byte-list rendering (decimal per byte, empty
/// printing `bytes[]`), the derive compositions over a bytes part, and
/// the bridges — the unit entry's trailing `0` line is the script-mode
/// convention.
#[test]
fn the_bytes_face_runs_over_the_ambient() {
    let out = run(ds().arg("run").arg(fixture("bytes_face.ds")));
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "bytes[97, 98]\n",
            "bytes[]\n",
            "bytes[49, 50, 195]\n",
            "wrap { b: bytes[97, 98] }\n",
            "wrap { b: bytes[97, 98] }\n",
            "-1\n",
            "1\n",
            "0\n",
            "true\n",
            "false\n",
            "6\n",
            "héllo\n",
            "0\n",
        )
    );
}

/// The built round trip reproduces the source run exactly.
#[test]
fn built_module_reproduces_the_source_run() {
    let module = env::temp_dir().join(format!("ds_bytes_face_probe-{}.dsmod", std::process::id()));
    let _ = fs::remove_file(&module);
    let build = run(ds()
        .arg("build")
        .arg(fixture("bytes_face.ds"))
        .arg("-o")
        .arg(&module));
    assert!(build.status.success(), "the build failed: {build:?}");
    let out = run(ds().arg("run").arg(&module));
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "bytes[97, 98]\n",
            "bytes[]\n",
            "bytes[49, 50, 195]\n",
            "wrap { b: bytes[97, 98] }\n",
            "wrap { b: bytes[97, 98] }\n",
            "-1\n",
            "1\n",
            "0\n",
            "true\n",
            "false\n",
            "6\n",
            "héllo\n",
            "0\n",
        )
    );
    let _ = fs::remove_file(&module);
}
