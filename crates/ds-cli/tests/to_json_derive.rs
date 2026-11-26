//! End-to-end through the `ds` binary over the real ambient: the
//! to_json derive family — `#[derive(to_json)]` on a struct or an enum
//! synthesizes the receiverless encode member whose body builds the
//! json enum value through its own case constructions, the plain
//! `to_json: (json) -> string` writer the composition point — run once
//! through the cold path's served artifacts and once bypassing every
//! cache (`--no-cache`), so the splice and the fresh lowering stay
//! pinned to the same behavior. The rows cover every scalar case
//! construction, the optional member present and absent (absence
//! leaving the member out), the optional list field (the decode
//! mirror's refusal, statement-position here), the nested derive
//! composition, the nested list walk, the enum target's unit, payload,
//! and lone tuple-payload shapes, and the from_json roundtrips over
//! the declarations deriving both families; the empty enum hosts its
//! encode member over the name-solved head (the vacuous body answering
//! the empty document), pinned by the member lowering with the module.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-to-json-derive-{name}-"))
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

/// One row per line: the scalar cell's minified object, the outer
/// target's full and absent-optional rows (the nested derive, the list
/// and nested-list walks, the absent member left out), the optional
/// list field absent and present, the enum's two unit shapes and its
/// payload object, the discriminator's encode rows (two payloaded
/// cases answering their own member keys, the sole tuple payload its
/// bare array, a mixed enum's wrapped cases, the lone tuple-payload
/// case its bare array and its roundtrip), the
/// roundtrip rows (present optional kept, absent optional still
/// absent, a missing required member refused, the lone tuple case's
/// re-encode through the narrowed place and its wrong shape refused), and the
/// width fields' rows (byte/u32 as numbers with their edges, char as
/// its one-character string, bytes as a number array, and the
/// constructed row over field reads of every width).
const GOLDEN: &str = concat!(
    "{\"i\":12,\"f\":2.5,\"b\":true,\"s\":\"arch\"}\n",
    "{\"i\":{\"a\":7},\"kind\":\"t\",\"port\":9,\"xs\":[4,5],\"grid\":[[1,2],[3]]}\n",
    "{\"i\":{\"a\":7},\"kind\":\"t\",\"xs\":[],\"grid\":[]}\n",
    "{}\n",
    "{\"rows\":[1,2]}\n",
    "\"left\"\n",
    "\"right\"\n",
    "{\"times\":3}\n",
    "{\"dot\":{\"x\":2,\"y\":5}}\n",
    "{\"ring\":{\"r\":7}}\n",
    "\"stay\"\n",
    "[3,\"hi\"]\n",
    "\"plain\"\n",
    "{\"paired\":[1,2]}\n",
    "{\"named\":{\"label\":\"q\"}}\n",
    "[3,4]\n",
    "[5,6]\n",
    "parse_err\n",
    "{\"a\":7,\"tag\":\"t\",\"xs\":[1,2],\"port\":3}\n",
    "{\"a\":7,\"tag\":\"t\",\"xs\":[]}\n",
    "parse_err\n",
    "{\"b\":0,\"c\":\"q\",\"u\":0,\"z\":[]}\n",
    "{\"b\":255,\"c\":\"q\",\"u\":4294967295,\"z\":[0,255]}\n",
    "{\"b\":65,\"c\":\"é\",\"u\":4000000000,\"z\":[65,0]}\n",
    "0\n",
);

#[test]
fn the_to_json_derive_answers_through_the_served_ambient() {
    let dir = workdir("served");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("to_json_derive.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// The same family bypassing every cache: the fresh lowering must agree
/// with the served splice byte for byte.
#[test]
fn the_to_json_derive_agrees_fresh() {
    let dir = workdir("fresh");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(fixture("to_json_derive.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}
