//! The @std string-separator and numeric-face surfaces end to end
//! through `ds run`: `join`/`split` take a string separator beside the
//! char (the checked union parameter re-spans every argument into the
//! separator's whole staging span, an empty string splitting per
//! char), and `abs` joins the int width on the float face's spelling —
//! the solved argument type picks the numeric instance. What the
//! fixtures pin:
//!
//! - split slices per the separator's text, per the char, and per char under the empty string; a
//!   union-typed separator variable calls like a direct literal.
//! - join renders the neighbors around either separator spelling.
//! - int abs answers the magnitude (the `i64::MIN` trapping with the arithmetic family's overflow),
//!   float abs the IEEE absolute value, both off the one `abs` spelling.
//! - a separator outside `char | string` refuses at the check; a non-numeric `abs` argument accepts
//!   at the check and refuses at the call's lowering, naming the numeric instances.

use std::{env, fs, path::PathBuf, process::Command};

mod support;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: spawned runs never read or
/// write a real user cache directory, and the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-stdface-cache-")
}

/// Writes `source` to a scratch file and runs it, answering (status,
/// stdout, stderr).
fn run_source(source: &str) -> (std::process::ExitStatus, String, String) {
    let dir = support::guarded_dir("ds-cli-stdface-src-");
    let path = dir.join("stdface_main.ds");
    fs::write(&path, source).expect("scratch source writes");
    let out = ds().arg("run").arg(&path).output().expect("ds run spawns");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn show_list(xs: &str) -> String {
    format!(
        r#"fn show_list (xs: list[string]) -> () {{
  var out = ""
  var i = 0
  while i < len(xs) {{
    if i > 0 {{ out = "${{out}}|" }}
    out = "${{out}}${{xs.get(i).unwrap_or("")}}"
    i += 1
  }}
  println(out)
}}

{xs}
"#
    )
}

#[test]
fn split_takes_the_string_separator_beside_the_char() {
    let (status, stdout, stderr) = run_source(&show_list(
        r#"fn main() -> () {
  show_list(split("a,b;c", ","))
  show_list(split("a,b;c", ','))
  show_list(split("do.or.re", ""))
  var sep = ","
  show_list(split("x,y", sep))
}
"#,
    ));
    assert!(status.success(), "the split spellings run clean: {stderr}");
    assert_eq!(
        stdout, "a|b;c\na|b;c\nd|o|.|o|r|.|r|e\nx|y\n",
        "the string, char, empty (per-char), and union-variable separators answer alike"
    );
}

#[test]
fn join_takes_the_string_separator_beside_the_char() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  println(join(["a", "b"], "-"))
  println(join(["a", "b"], '-'))
  var sep = "--"
  println(join(["a", "b", "c"], sep))
}
"#,
    );
    assert!(status.success(), "the join spellings run clean: {stderr}");
    assert_eq!(stdout, "a-b\na-b\na--b--c\n");
}

#[test]
fn a_separator_outside_the_union_refuses_at_the_check() {
    let (status, _stdout, stderr) = run_source(
        r#"fn main() -> () {
  println(str(split("a,b", 5)))
}
"#,
    );
    assert!(!status.success(), "an int separator refuses");
    assert!(
        stderr.contains("not contained in `string | char`"),
        "the containment refusal names the separator union: {stderr}"
    );
}

#[test]
fn abs_joins_the_int_width_on_the_float_face() {
    let (status, stdout, stderr) = run_source(
        r"fn main() -> () {
  println(str(abs(-5)))
  println(str(abs(0 - 9)))
  println(str(abs(0.0 - 2.5)))
  println(str(abs(3.5)))
}
",
    );
    assert!(status.success(), "the abs spellings run clean: {stderr}");
    assert_eq!(stdout, "5\n9\n2.5\n3.5\n");
}

#[test]
fn the_int_magnitude_traps_at_the_words_floor() {
    let (status, _stdout, stderr) = run_source(
        r"fn main() -> () {
  println(str(abs(-9223372036854775807 - 1)))
}
",
    );
    assert!(!status.success(), "the i64::MIN magnitude stops the run");
    assert!(
        stderr.contains("integer overflow"),
        "the trap fires: {stderr}"
    );
}

#[test]
fn the_format_family_answers_over_the_builtin_faces() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  println(str_fixed(3.14159, 2))
  println(str_fixed(1.0 / 3.0, 4))
  println(str_fixed(2.0, 0))
  println(hex(255))
  println(hex(0))
  println(hex(-26))
  println(pad_left("7", 3, '0'))
  println(pad_left("abc", 2, '0'))
  println(pad_right("7", 3, '.'))
}
"#,
    );
    assert!(status.success(), "the format family runs clean: {stderr}");
    assert_eq!(
        stdout, "3.14\n0.3333\n2\nff\n0\n-1a\n007\nabc\n7..\n",
        "the fixed-point width, the hex digits, and the two alignments answer"
    );
}

#[test]
fn a_non_numeric_abs_argument_refuses_at_the_lowering() {
    let (status, _stdout, stderr) = run_source(
        r#"fn main() -> () {
  println(str(abs("s")))
}
"#,
    );
    assert!(!status.success(), "a string argument refuses at the call");
    assert!(
        stderr.contains("DS4009"),
        "the lowering refusal names the numeric instances: {stderr}"
    );
}
