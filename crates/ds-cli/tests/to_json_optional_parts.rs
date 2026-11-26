//! End-to-end through the `ds` binary over the real ambient and the
//! @std json driver: the json derive families' optional-in-part
//! rows — a `t?` list element encoding through the presence narrow
//! over the for-bound element (absence skipping the element) and
//! decoding through the narrow over the per-element binder (presence
//! reading through the element's own reader and pushing, absence
//! contributing nothing to the list, a present json null refusing
//! like any other value no reader takes), a nested
//! optional list element composing the array walk inside the narrow
//! both ways, and an optional payload field encoding through the
//! narrow over the payload binder (absence leaving the member out),
//! every declaration roundtripping decode-side both ways — run once
//! through the cold path's served artifacts and once bypassing every
//! cache (`--no-cache`), so the splice and the fresh lowering stay
//! pinned to the same behavior.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-to-json-optional-parts-{name}-"))
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

/// One row per line: the holes struct's `t?` list element skipping its
/// absent elements and its nested optional list element composing the
/// array walk inside the narrow, the all-absent row answering empty
/// arrays, the holes struct's decode roundtrip (each present element
/// placed, absence contributing nothing, a present json null and each
/// wrong shape — an element, the array member itself, a missing
/// member, a non-object document — answering `parse_err`), the pulse
/// enum's optional payload field encoding present
/// and absent with its roundtrip rows (a present member kept, an
/// absent member still absent, the unit case, and a wrong shape
/// answering `parse_err`), and the beat enum's tuple payload encoding
/// its optional items with its roundtrip rows (a missing position
/// decoding absence, the unit case, and a present json null refusing
/// like any value no reader takes).
const GOLDEN: &str = concat!(
    "{\"xs\":[1,3],\"ys\":[[2],[3,4]]}\n",
    "{\"xs\":[],\"ys\":[]}\n",
    "{\"xs\":[1,3],\"ys\":[[2],[3,4]]}\n",
    "{\"xs\":[],\"ys\":[]}\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "parse_err\n",
    "\"quiet\"\n",
    "{\"at\":7}\n",
    "{}\n",
    "{\"at\":3}\n",
    "{}\n",
    "\"quiet\"\n",
    "parse_err\n",
    "\"rest\"\n",
    "[120,\"x\"]\n",
    "[]\n",
    "[120,\"x\"]\n",
    "[]\n",
    "\"rest\"\n",
    "parse_err\n",
    "",
);

#[test]
fn the_optional_in_part_rows_answer_through_the_served_ambient() {
    let dir = workdir("served");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("to_json_optional_parts.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// The same rows bypassing every cache: the fresh lowering must agree
/// with the served splice byte for byte.
#[test]
fn the_optional_in_part_rows_agree_fresh() {
    let dir = workdir("fresh");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(fixture("to_json_optional_parts.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}
