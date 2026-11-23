//! End-to-end through the `ds` binary over the real ambient: the
//! from_json derive family — `#[derive(from_json)]` on a struct or an
//! enum synthesizes the receiverless decode member whose body walks the
//! landed typed readers — run once through the cold path's served
//! artifacts and once bypassing every cache (`--no-cache`), so the
//! splice and the fresh lowering stay pinned to the same behavior. The
//! rows cover every reader kind happy, each wrong shape answering
//! `parse_err`, the optional member absent, present, and wrong-shaped,
//! the optional list field's absence, presence, and wrong shapes (the
//! statement-position member row, to_json's mirror), the nested derive
//! composition both ways, and the enum target's string and object
//! shapes.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-from-json-derive-{name}-"))
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

/// One row per line: the scalar cell's happy read and each reader
/// kind's wrong shape, the outer target's happy walk (nested derive,
/// optional, list, nested list) with its failure classes, the wide
/// target's optional-member composition, the slack target's optional
/// list field (present, absent, wrong shape, wrong element), the
/// scalars target's width readers (byte/u32 as numbers with their
/// range edges, char as a one-character string, bytes as a number
/// array, every wrong shape or out-of-range value answering
/// parse_err), and the enum target's shapes — the ruled string/object
/// walk plus the discriminator ruling's rows (2026-11-21): two
/// payloaded cases decoding from their own member keys, the sole tuple
/// payload from its bare array, and a mixed enum's wrapped payloaded
/// cases, each wrong key or wrong shape answering parse_err — plus
/// the empty enum's unconditional parse_err rows (no member case can
/// match, so every document refuses).
const GOLDEN: &str = concat!(
    "cell 12 2.5 true arch\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "outer 7 t true 2 2\n",
    "outer 7 t true 0 0\n",
    "outer 7 t false 0 0\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "wide true true\n",
    "wide false false\n",
    "parse_err\n",
    "parse_err\n",
    "{\"xs\":[4,5]}\n",
    "{}\n",
    "parse_err\n",
    "parse_err\n",
    "{\"b\":7,\"c\":\"q\",\"u\":4000000000,\"z\":[1,2,255]}\n",
    "{\"b\":255,\"c\":\"é\",\"u\":4294967295,\"z\":[0,255]}\n",
    "{\"b\":0,\"c\":\"q\",\"u\":0,\"z\":[]}\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "left\n",
    "right\n",
    "spin 3\n",
    "parse_err\n",
    "parse_err\n",
    "{\"dot\":{\"x\":2,\"y\":5}}\n",
    "{\"ring\":{\"r\":7}}\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "\"stay\"\n",
    "[3,\"hi\"]\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "\"plain\"\n",
    "{\"paired\":[1,2]}\n",
    "{\"named\":{\"label\":\"q\"}}\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "0\n",
);

#[test]
fn the_from_json_derive_answers_through_the_served_ambient() {
    let dir = workdir("served");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("from_json_derive.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// The same family bypassing every cache: the fresh lowering must agree
/// with the served splice byte for byte.
#[test]
fn the_from_json_derive_agrees_fresh() {
    let dir = workdir("fresh");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(fixture("from_json_derive.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}
