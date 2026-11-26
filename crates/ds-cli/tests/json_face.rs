//! End-to-end through the `ds` binary over the real ambient: the
//! ambient's own json face — the ds-source reader over every value
//! shape, the writer's minified round trip, and the invalid documents
//! answering `parse_err` — run once through the cold path's served
//! artifacts and once bypassing every cache (`--no-cache`), so the
//! splice and the fresh lowering stay pinned to the same behavior.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-json-face-{name}-"))
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

/// One read-write round trip per line: the literals, the number split
/// (int-faithful, floats keeping a float spelling, the exponent
/// spelling read back as a float), every string escape class (the two
/// escaped specials, the `\u00XX` control spellings, and the plain
/// `\uXXXX` read), arrays and objects at every nesting depth with
/// whitespace tolerance, and the refusals (truncated words, trailing
/// content, and malformed structure). The decode rows pin the typed
/// readers (a wrong shape answering parse_err, the number split kept
/// on the read side), the width readers (byte and u32 as numbers with
/// their range edges printed back through the encode helpers, char as
/// a one-character string, bytes as a number array), and the member
/// reader over present, missing, and
/// non-object documents; the target rows pin the per-target from_json
/// walks a struct and an enum target spell over the readers, with the
/// missing-member, wrong-shape, wrong-member-type, and unknown-case
/// failures answering parse_err.
const GOLDEN: &str = concat!(
    "null\n",
    "true\n",
    "false\n",
    "12\n",
    "2.5\n",
    "1000.0\n",
    "-7\n",
    "parse_err\n",
    "\"hello\"\n",
    "\"a\\u000ab\\\"c\\\\d\"\n",
    "\"A\"\n",
    "[]\n",
    "[1,2]\n",
    "[[true],[null]]\n",
    "{}\n",
    "{\"k\":1}\n",
    "{\"a\":[1,2.5,true,null],\"b\":\"x\"}\n",
    "[1,2]\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "int: 12\n",
    "string: x\n",
    "float: 2.5\n",
    "parse_err\n",
    "bool: true\n",
    "array: 2\n",
    "object: 1\n",
    "byte: 200\n",
    "parse_err\n",
    "u32: 4000000000\n",
    "parse_err\n",
    "char: q\n",
    "parse_err\n",
    "bytes: [1,2,255]\n",
    "parse_err\n",
    "k: 1\n",
    "none\n",
    "none\n",
    "spot home 1 2\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "left\n",
    "spin 3\n",
    "parse_err\n",
    "",
);

#[test]
fn the_json_face_round_trips_through_the_served_ambient() {
    let dir = workdir("served");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("json_face.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// The same face bypassing every cache: the fresh lowering must agree
/// with the served splice byte for byte.
#[test]
fn the_json_face_agrees_fresh() {
    let dir = workdir("fresh");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(fixture("json_face.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}
