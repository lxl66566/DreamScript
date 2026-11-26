//! End-to-end through the `ds` binary over the real ambient: the
//! comptime field seam — `for comptime f in fields(t)` unrolls over a
//! written struct's fields at check time, the record binder's `name`
//! member a string constant each copy and the keyed access `x.[f.name]`
//! a fixed field offset the copy resolves — run once through the cold
//! path's served artifacts and once bypassing every cache
//! (`--no-cache`), so the splice and the fresh lowering agree. The
//! refusal pins hold the seam's doors: a written struct alone walks
//! (an enum's cases and a generic head refuse), the record binder is
//! no runtime value and projects only its `name` member, the keyed
//! access keys only a record's name, member and generic dispatch plus
//! lambda values inside a copy queue with the walker round (their
//! records are per-copy facts), and the loop lives only in runtime
//! code over a resolvable projection (a comptime fn and a non-`fields`
//! iterable refuse).

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-comptime-field-{name}-"))
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

/// One row per line: the keyed reads' sum (3+4+5), the keyed writes'
/// all-nines readback, the name constants concatenated in field order,
/// the nested loops' 3x3 count, the generic fn's two instances each
/// counting its own three copies, and the `name`-keyed struct's fields
/// read by their own constants (the text row, the width row). The
/// trailing `0` is the run's own answer line, as every golden pins.
const GOLDEN: &str = concat!(
    "12\n", "27\n", "abc\n", "9\n", "6\n", "6\n", "nu\n", "2\n", "0\n",
);

#[test]
fn the_comptime_field_seam_answers_through_the_served_ambient() {
    let dir = workdir("served");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("comptime_field_seam.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// The same seam bypassing every cache: the fresh lowering must agree
/// with the served splice byte for byte.
#[test]
fn the_comptime_field_seam_agrees_fresh() {
    let dir = workdir("fresh");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg("--no-cache")
            .arg(fixture("comptime_field_seam.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}

/// One refusal pin: the fixture source and the diagnostic fragment its
/// one refusal must carry.
const PINS: &[(&str, &str)] = &[
    (
        "struct pair { l: int, r: int }
enum turn { left, right }

fn main() -> () {
  for comptime f in fields(turn) {
    println(f.name)
  }
}
",
        "an enum's cases carry no field offsets",
    ),
    (
        "struct box[t] { item: t }

fn main() -> () {
  for comptime f in fields(box) {
    println(f.name)
  }
}
",
        "a generic head's fields are per-application facts",
    ),
    (
        "struct pair { l: int, r: int }

fn main() -> () {
  for comptime f in fields(pair) {
    println(f)
  }
}
",
        "the comptime record is no runtime value",
    ),
    (
        "struct pair { l: int, r: int }

fn main() -> () {
  var p = pair { l: 1, r: 2 }
  for comptime f in fields(pair) {
    p.[f] = 1
  }
  println(p.l.show())
}
",
        "not a comptime field name",
    ),
    (
        "struct pair { l: int, r: int }

fn main() -> () {
  for comptime f in fields(pair) {
    println(f.tag)
  }
}
",
        "queue with the walker round",
    ),
    (
        "struct pair { l: int, r: int }

fn main() -> () {
  var p = pair { l: 1, r: 2 }
  for comptime f in fields(pair) {
    println(p.[f.name].show())
  }
}
",
        "a member or generic dispatch inside a `for comptime` copy",
    ),
    (
        "struct pair { l: int, r: int }

fn main() -> () {
  for comptime f in fields(pair) {
    const g = (x: int) => x
    println(f.name)
  }
}
",
        "its mint record is a per-copy fact",
    ),
    (
        "struct pair { l: int, r: int }

fn walk[t] (x: t) -> int {
  for comptime f in fields(t) {
    println(f.name)
  }
  1
}
",
        "unknown name",
    ),
    (
        "struct pair { l: int, r: int }

fn main() -> () {
  var total = 0
  for comptime f in total {
    total += 1
  }
  println(total.show())
}
",
        "not a `fields` projection",
    ),
    (
        "struct pair { l: int, r: int }

fn schema(comptime t: type) -> string {
  for comptime f in fields(t) {
    println(f.name)
  }
  \"\"
}
",
        "already evaluates at compile time",
    ),
    (
        "struct pair { l: int, r: int }

fn main() -> () {
  var p = pair { l: 1, r: 2 }
  for comptime f in fields(pair) {
    p.[f.name] = 1
  }
  println(p.l.show())
}
",
        "",
    ),
];

#[test]
fn the_seam_refusals_hold_their_doors_and_the_keyed_write_passes_clean() {
    let dir = workdir("pins");
    let cache = dir.join("cache");
    fs::create_dir_all(&cache).expect("the cache dir writes");
    for (index, (source, expected)) in PINS.iter().enumerate() {
        let path = dir.join(format!("pin{index}.ds"));
        fs::write(&path, source).expect("the pin fixture writes");
        let out = Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("check")
            .arg(&path)
            .env("DS_CACHE", &cache)
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let errors = stderr.matches("error[").count();
        if expected.is_empty() {
            assert!(
                out.status.success(),
                "pin {index}: the clean fixture checks clean (stderr: {stderr})"
            );
            assert_eq!(errors, 0, "pin {index}: no diagnostic expected: {stderr}");
        } else {
            assert!(
                !out.status.success(),
                "pin {index}: the refusal fires (stderr: {stderr})"
            );
            assert_eq!(
                errors, 1,
                "pin {index}: one diagnostic, the copies share their spans: {stderr}"
            );
            assert!(
                stderr.contains(expected),
                "pin {index}: the refusal names its seam (stderr: {stderr})"
            );
        }
    }
    let _ = fs::remove_dir_all(dir);
}
