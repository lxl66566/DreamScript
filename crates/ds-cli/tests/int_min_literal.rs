//! The integer-literal i64 range through the `ds` binary: i64::MIN is
//! writable as one negated literal (the sign folds into the literal's
//! range check, the exact min magnitude lowering as the min constant),
//! prints its full digits through interpolation, and a wider negation
//! refuses at the check with the literal named — never at lowering.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-int-min-{name}-"))
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

/// The probe's golden: the min literal's interpolated digits, the
/// computed-min equality, and main's int answer.
const GOLDEN: &str = concat!("-9223372036854775808\n", "min\n", "");

#[test]
fn int_min_prints_through_interpolation_and_equals_the_computed_min() {
    let dir = workdir("run");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(fixture("int_min_literal.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// A negation past the min magnitude refuses at the check with the
/// literal named, through the run path as through every other.
#[test]
fn a_negation_past_the_min_magnitude_refuses_at_the_check() {
    let dir = workdir("refuse");
    let src = dir.join("past_min.ds");
    fs::write(
        &src,
        "fn main() -> int {\n  const x = -99999999999999999999\n  0\n}\n",
    )
    .expect("the probe source writes");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(&src)
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(
        !out.status.success(),
        "the past-min literal must not run: {out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("DS1003")
            && stderr.contains("integer literal `99999999999999999999` negated does not fit"),
        "the refusal names the literal and the range: {stderr}"
    );
}
