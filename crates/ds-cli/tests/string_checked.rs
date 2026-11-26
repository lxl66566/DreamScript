//! End-to-end through the `ds` binary over the real ambient: the checked
//! string reads — `substring`'s `string | none` and `char_at`'s
//! `char | none` — answer their hit and none members through every
//! extraction spelling (match, is-narrowing, the unwrap_or fallback),
//! never clamping and never trapping; and the retired `head` spelling
//! fails as an unresolved name.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-string-checked-{name}-"))
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

/// The probe's golden: the hit sides extract through match, narrowing,
/// and the fallback; the out-of-range sides answer none by either bound
/// and the inverted range; the empty string's char read falls back; and
/// the multi-byte slice cuts between scalars.
const GOLDEN: &str = concat!(
    "he\n", "none\n", "none\n", "none\n", "e\n", "none\n", "x\n", "él\n", "",
);

#[test]
fn the_checked_string_reads_answer_none_through_the_union() {
    let dir = workdir("served");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("string_checked_face.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// The same face bypassing every cache: the fresh lowering must agree
/// with the served splice byte for byte.
#[test]
fn the_checked_string_reads_agree_fresh() {
    let dir = workdir("fresh");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(fixture("string_checked_face.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// The retired `head` spelling names no ambient declaration, so a call
/// fails at the check as an unresolved name (`first` is the one
/// list-front spelling).
#[test]
fn the_retired_head_spelling_fails_as_unresolved() {
    let dir = workdir("retired");
    let src = dir.join("retired_head.ds");
    fs::write(
        &src,
        "fn main() -> () {\n  const xs = [1]\n  println(\"${head(xs)}\")\n}\n",
    )
    .expect("the probe source writes");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("check")
            .arg(&src)
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(
        !out.status.success(),
        "the retired spelling must not check: {out:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("head"),
        "the diagnostic names the retired spelling: {out:?}"
    );
}
