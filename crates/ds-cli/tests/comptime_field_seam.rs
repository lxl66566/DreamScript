//! End-to-end through the `ds` binary over the real ambient: the
//! comptime field seam — `for comptime f in fields(t)` unrolls over a
//! written struct's fields at check time, the record binder's `name`
//! and `tag` members string constants each copy and the keyed access
//! `x.[f.name]` a fixed field offset the copy resolves — run once
//! through the cold path's served artifacts and once bypassing every
//! cache (`--no-cache`), so the splice and the fresh lowering agree.
//! The walker rows pin `fields(t)` over the fn's own generic parameter:
//! the once-check marks the loop and each concrete entering site's
//! expansion walk unrolls it under its own shape, the per-copy records
//! keyed by the walked fn's definition and solved arguments — the
//! per-shape sums, the composed generic caller sharing one instance,
//! the name and tag rows, a written loop nested inside a per-instance
//! copy, and per-instance loops nested in each other. The refusal pins
//! hold the seam's doors: a written struct alone walks (an enum's
//! cases and a generic head refuse, an instance over a non-struct
//! shape refusing at its expansion), the record binder is no runtime
//! value and projects only its `name` and `tag` members (its `type`
//! member a comptime fact), the keyed access keys only a record's
//! name, member and generic dispatch plus lambda values inside a copy
//! queue with the walker round (their records are per-copy facts), a
//! per-instance loop over a generic impl member's own variable lowers
//! only per shape (the ONE body refuses), and the loop lives only in
//! runtime code over a resolvable projection (a comptime fn and a
//! non-`fields` iterable refuse).

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
/// walker rows: the per-instance sums over both shapes (1+2+3, 7+8),
/// the composed caller's shared instance (3*2), one shape's name and
/// tag rows, the written loop nested inside a per-instance copy (3
/// fields x the pair's sum 3), and the per-instance loops nested in
/// each other (2 copies x the pair's sum 3). The trailing `0` is the
/// run's own answer line, as every golden pins.
const GOLDEN: &str = concat!(
    "12\n", "27\n", "abc\n", "9\n", "6\n", "6\n", "nu\n", "2\n", "6\n", "15\n", "6\n", "a,b,\n",
    "a=int b=int c=int \n", "9\n", "6\n", "6\n", "0\n",
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
    println(f.type)
  }
}
",
        "its `type` member is a comptime fact",
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
        "enum turn { left, right }

struct pair { l: int, r: int }

fn per_sum[t] (p: t) -> int {
  var total = 0
  for comptime f in fields(t) {
    total += p.[f.name]
  }
  total
}

fn main() -> () {
  println(per_sum(left).show())
}
",
        "an enum's cases, a builtin",
    ),
    (
        "struct pair { l: int, r: int }

fn walk[t] (x: t) -> int {
  for comptime f in fields(t) {
    println(f.name)
  }
  1
}

fn main() -> () {
  println(\"0\")
}
",
        "",
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

/// The lowering doors: the checker accepts each source, and the lowering
/// must refuse it — a generic impl member's ONE body lowers over its
/// variables, and a per-instance loop over the impl's own type variable
/// projects only per concrete entering shape, none existing here.
const RUN_PINS: &[(&str, &str)] = &[(
    "struct box2[t] { item: t }

trait shows { fn show(self) -> string }

impl [t: shows] shows for box2[t] {
  fn show(self) -> string {
    var total = 0
    for comptime f in fields(t) {
      total += 1
    }
    total.show()
  }
}

fn main() -> () {
  println(\"0\")
}
",
    "only a concrete entering shape projects the loop's fields",
)];

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
    for (index, (source, expected)) in RUN_PINS.iter().enumerate() {
        let path = dir.join(format!("run-pin{index}.ds"));
        fs::write(&path, source).expect("the pin fixture writes");
        let out = Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("check")
            .arg(&path)
            .env("DS_CACHE", &cache)
            .output()
            .expect("the binary runs");
        assert!(
            out.status.success(),
            "run pin {index}: the door source checks clean (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(&path)
            .env("DS_CACHE", &cache)
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "run pin {index}: the lowering refuses (stderr: {stderr})"
        );
        assert!(
            stderr.contains(expected),
            "run pin {index}: the refusal names its door (stderr: {stderr})"
        );
    }
    let _ = fs::remove_dir_all(dir);
}
