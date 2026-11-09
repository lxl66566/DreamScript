//! End-to-end `ds check` through the binary: the conformance corpus with
//! its ambient stub packages exits clean under the stage scoping, and the
//! negative fixture reports its overlap and moved-then-used pair with
//! golden renderings.

use std::{
    env,
    process::{Command, Output},
};

fn ds_check(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ds"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the ds binary builds with the test")
}

/// The M2 acceptance over the corpus: `ds check` over
/// conformance/types_hybrid.ds and conformance/generics_coherent_traits.ds
/// exits 0. Each file is given its valid package — its ambient stdlib
/// stubs per the corpus README (a package is exactly the files given, so
/// the vocabulary arrives as a package member) — and the documented noise
/// (the get_port ordering tension in run items, the intended reject, m6
/// constructs) is printed but never fatal.
#[test]
fn types_hybrid_checks_clean_with_its_ambient_package() {
    let out = ds_check(&[
        "check",
        "tests/fixtures/types_ambient.ds",
        "../../conformance/types_hybrid.ds",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, [] as [u8; 0]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    // The get_port tension prints as noise; nothing else is fatal.
    assert_eq!(stderr.matches("error[").count(), 2);
    assert!(stderr.contains("DS2001"));
}

#[test]
fn generics_checks_clean_with_its_ambient_package() {
    let out = ds_check(&[
        "check",
        "tests/fixtures/generics_ambient.ds",
        "../../conformance/generics_coherent_traits.ds",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, [] as [u8; 0]);
    // The reject item's intended overlap prints as noise, along with the
    // m6-item diagnostics; none is fatal.
    assert!(String::from_utf8(out.stderr).unwrap().contains("DS2006"));
}

/// A valid single-file package that declares every name it uses: nothing
/// to report, in either rendering.
#[test]
fn a_clean_single_file_package_is_silent() {
    for format in ["human", "json"] {
        let out = ds_check(&["check", "--error-format", format, "tests/fixtures/clean.ds"]);
        assert_eq!(out.status.code(), Some(0), "format {format}");
        assert_eq!(out.stdout, [] as [u8; 0], "format {format}");
        assert_eq!(out.stderr, [] as [u8; 0], "format {format}");
    }
}

/// The negative fixture: the intended overlap rejection [B6] and
/// moved-then-used pair [C1] are fatal and render golden in both formats.
#[test]
fn the_negative_fixture_renders_golden_human_diagnostics() {
    let out = ds_check(&["check", "tests/fixtures/moved_overlap.ds"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "\
error[DS2006]: instance head `int | bool` intersects the `int` instance; witness value type `int`
 --> tests/fixtures/moved_overlap.ds:16:14
   |
16 | impl enc for int | bool {
   |              ^^^^^^^^^^ overlapping head
   |
   = note: the earlier instance's head covers `int`; two instances of one trait must be pairwise \
         disjoint [B6]
error[DS3001]: value `c` is no longer usable
 --> tests/fixtures/moved_overlap.ds:26:8
   |
25 |   send(c)
   |        - moved here
   |
26 |   send(c)
   |        ^ used again here after move
   |
   = note: `c` is `~Copyable` and was consumed
"
    );
}

#[test]
fn the_negative_fixture_renders_golden_json_diagnostics() {
    let out = ds_check(&[
        "check",
        "--error-format",
        "json",
        "tests/fixtures/moved_overlap.ds",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, [] as [u8; 0]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "one JSON object per diagnostic");
    assert!(lines[0].starts_with(r#"{"code":"DS2006""#));
    assert!(lines[0].contains(r#""message":"instance head `int | bool` intersects the `int` instance; witness value type `int`""#));
    assert!(lines[0].contains(r#""primary":true"#));
    assert!(lines[1].starts_with(r#"{"code":"DS3001""#));
    assert!(lines[1].contains(r#""label":"moved here","primary":false"#));
    assert!(lines[1].contains(r#""label":"used again here after move","primary":true"#));
    assert!(lines[1].contains(r#""notes":["`c` is `~Copyable` and was consumed"]"#));
    assert!(stderr.ends_with('\n'));
}

/// A missing source is a self-describing failure, not a crash.
#[test]
fn a_missing_source_reports_the_path() {
    let out = ds_check(&["check", "tests/fixtures/absent.ds"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("reading tests/fixtures/absent.ds"));
}
