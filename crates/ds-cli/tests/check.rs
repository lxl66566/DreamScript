//! End-to-end `ds check` through the binary: the conformance corpus checks
//! clean as-is under the stdlib ambient (the M3 checked-declaration layer),
//! the plain fixtures stay self-contained under `--no-ambient`, and the
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

/// One corpus file's documented noise: the file checks with the ambient
/// (exit 0), stdout stays empty, and stderr prints exactly the staged
/// leftovers the corpus README's stage vocabulary owns — the printed
/// count is the pin; every diagnostic must name an item staged away from
/// `typecheck`, or the command would have failed.
fn assert_corpus_clean(file: &str, noise: usize) {
    let path = format!("../../conformance/{file}");
    let out = ds_check(&["check", &path]);
    assert_eq!(out.status.code(), Some(0), "{file}");
    assert_eq!(out.stdout, [] as [u8; 0], "{file}");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(
        stderr.matches("error[").count(),
        noise,
        "{file}: staged leftovers changed: {stderr}"
    );
}

/// The M3 acceptance: the corpus files check clean as-is, the stdlib
/// ambient arriving as the package's first member. The documented noise
/// per file, all confined to items the header stages away from
/// `typecheck`:
///
/// * types_hybrid: the `config` type is the file's own declaration, so `default_config` stays out
///   of the ambient and the row-typed `read_file`/`parse_json` leave the run items' `?`-propagation
///   tensions (get_port, load, cfg, same).
/// * generics: the reject item's intended overlap plus the m6 items (comptime `fields`/`type_name`,
///   the derive engine behind `json for point`).
/// * syntax_ts_heart: run items only — `read_config`/`default_config` absence and the
///   type-constructor call form of `bad_number`/ `missing_field`.
/// * linearity_modes_rc: run items only — `open`/`read_exact` absence (the `file` type is the
///   file's own) and `u32`'s distinct width [B7].
/// * effects_nursery: run items only — `urls` is an ambient value the M5 run milestone owns, `push`
///   is the generics file's own stack API, and `list[t]()` construction plus ranges arrive with the
///   collection M3 work.
#[test]
fn the_corpus_checks_clean_under_the_stdlib_ambient() {
    assert_corpus_clean("types_hybrid.ds", 12);
    assert_corpus_clean("generics_coherent_traits.ds", 8);
    assert_corpus_clean("syntax_ts_heart.ds", 12);
    assert_corpus_clean("linearity_modes_rc.ds", 10);
    assert_corpus_clean("effects_nursery.ds", 9);
}

/// The intended rejection prints as noise under the ambient and pins its
/// golden rendering: the overlap diagnostic the corpus header expects.
#[test]
fn the_generics_reject_item_prints_its_overlap() {
    let out = ds_check(&["check", "../../conformance/generics_coherent_traits.ds"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8(out.stderr).unwrap().contains(
        "error[DS2006]: instance head `int | bool` intersects the `int` instance; witness value \
         type `int`"
    ));
}

/// Without the ambient the corpus's stdlib names do not resolve: the
/// ambient is what makes the corpus check clean.
#[test]
fn without_the_ambient_the_corpus_names_stay_unresolved() {
    let out = ds_check(&["check", "--no-ambient", "../../conformance/types_hybrid.ds"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("unknown name `read_file`")
    );
}

/// A self-contained package under the ambient collides with the shared
/// namespace [G3]: the file's own `tag int` redefines the ambient's.
#[test]
fn a_package_redeclaring_an_ambient_name_reports_the_duplicate() {
    let out = ds_check(&["check", "tests/fixtures/clean.ds"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("`int` is already defined")
    );
}

/// A valid single-file package that declares every name it uses: nothing
/// to report, in either rendering.
#[test]
fn a_clean_single_file_package_is_silent() {
    for format in ["human", "json"] {
        let out = ds_check(&[
            "check",
            "--no-ambient",
            "--error-format",
            format,
            "tests/fixtures/clean.ds",
        ]);
        assert_eq!(out.status.code(), Some(0), "format {format}");
        assert_eq!(out.stdout, [] as [u8; 0], "format {format}");
        assert_eq!(out.stderr, [] as [u8; 0], "format {format}");
    }
}

/// The negative fixture: the intended overlap rejection [B6] and
/// moved-then-used pair [C1] are fatal and render golden in both formats.
#[test]
fn the_negative_fixture_renders_golden_human_diagnostics() {
    let out = ds_check(&["check", "--no-ambient", "tests/fixtures/moved_overlap.ds"]);
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
        "--no-ambient",
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
