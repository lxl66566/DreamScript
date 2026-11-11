//! The M1 acceptance gates over the corpus:
//! `fmt(fmt(x)) == fmt(x)` and re-parsing the formatted output yields the
//! same AST, for every `conformance/*.ds` and every positive fixture of the
//! ds-parse surface.

mod common;

use std::path::PathBuf;

use common::{POSITIVE, fmt_once, shape};
use ds_diag::Files;
use ds_fmt::format_source;
use ds_parse::{parse, parse_script};

/// Runs both gates over one source, with failure messages that name the case.
fn check(name: &str, src: &str) {
    let out = fmt_once(name, src);
    // The formatted output must itself parse (a formatter that emits
    // unparseable text is wrong even before idempotence).
    let mut files = Files::new();
    let file = files.add_source(name, &out);
    if let Err(diags) = parse(&files, file) {
        panic!(
            "{name}: formatted output does not parse: {} (labels {})\n--- output:\n{out}",
            diags[0].message,
            diags[0].labels.len()
        );
    }
    // Idempotence.
    let out2 = fmt_once(name, &out);
    assert_eq!(
        out, out2,
        "{name} not idempotent\n--- first:\n{out}\n--- second:\n{out2}"
    );
    // AST preservation: see common/mod.rs for exactly what is compared.
    let (a, b) = (shape(name, src), shape(name, &out));
    assert_eq!(
        a,
        b,
        "{name}: re-parsing the formatted output changed the AST near: {}\n--- output:\n{out}",
        first_diff(&a, &b)
    );
}

#[test]
fn conformance_corpus_round_trips() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance");
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("conformance corpus missing at {}: {e}", dir.display()));
    let mut checked = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "ds") {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        check(&name, &source);
        checked += 1;
    }
    assert!(
        checked >= 6,
        "expected the six conformance files, saw {checked}"
    );
}

#[test]
fn positive_fixtures_round_trip() {
    assert_eq!(
        POSITIVE.len(),
        100,
        "fixture count drifted; keep in sync with ds-parse positives"
    );
    for (i, src) in POSITIVE.iter().enumerate() {
        check(&format!("positive[{i}]"), src);
    }
}

/// Script-mode entry files: the synthesized `main` block carries a
/// zero-width span (the parse-side alias guard), which the inline probe must
/// not trust. The formatted output spells the entry as a written
/// `fn main() -> () { ... }` — a pure-declaration file — so the gates are:
/// re-parse in library mode, idempotence, and the synthesized body keeping
/// its statement-per-line shape.
#[test]
fn script_entry_files_round_trip() {
    let fixtures: &[&str] = &[
        // Multi-statement body: never inline, whatever the span says.
        "const a = 1\nconst b = 2\nprintln(a + b)\n",
        // One multi-line statement: the body's own extent decides.
        "if ready {\n  run(a,\n    b)\n}\n",
        // One one-line statement stays inline.
        "println(\"hi\")\n",
        // Declarations beside the fold stay their own items.
        "extern fn println (msg: string) -> ()\nconst n = 1\nprintln(n)\n",
    ];
    for (i, src) in fixtures.iter().enumerate() {
        let name = format!("script-entry[{i}]");
        let mut files = Files::new();
        let file = files.add_source(&name, src);
        let ast = parse_script(&files, file)
            .unwrap_or_else(|d| panic!("{name} should parse as the entry: {}", d[0].message));
        let out = format_source(&files, file, &ast);
        let mut re_files = Files::new();
        let re = re_files.add_source(&name, &out);
        let re_ast = parse(&re_files, re).unwrap_or_else(|d| {
            panic!(
                "{name}: formatted output does not parse: {}\n--- output:\n{out}",
                d[0].message
            )
        });
        let out2 = format_source(&re_files, re, &re_ast);
        assert_eq!(
            out, out2,
            "{name} not idempotent\n--- first:\n{out}\n--- second:\n{out2}"
        );
    }
    // The multi-statement entry opens up: one statement per line.
    let mut files = Files::new();
    let file = files.add_source("multi.ds", "const a = 1\nconst b = 2\nprintln(a + b)\n");
    let ast = parse_script(&files, file).expect("parses as the entry");
    let out = format_source(&files, file, &ast);
    assert_eq!(
        out,
        "fn main() -> () {\n  const a = 1\n  const b = 2\n  println(a + b)\n}\n"
    );
}

/// `##` separators between the statements that fold into the synthesized
/// `main` survive formatting: the fold leaves the statement-carrying blocks
/// empty and appends `main` to the last one, so the separators sit in the
/// inter-block gap windows like any library separator — provided the
/// window end does not trust the synthesized span (which joins the file's
/// first..last statements and starts before the carrying block). The
/// comment-before-separator shape once inverted that window into a panic.
/// Gates for every fixture: the physical separator lines survive verbatim,
/// the output re-parses in library mode, and formatting is idempotent.
#[test]
fn script_entry_separators_survive_the_fold() {
    let fixtures: &[(&str, &str)] = &[
        // The plain drop: one separator between the folded statements.
        (
            "const a = 1\n## section\nprintln(a)\n",
            "## section\nfn main() -> () {\n  const a = 1\n\n  println(a)\n}\n",
        ),
        // The former panic: a comment between the last folded statement of a
        // block and the separator rides the empty block's trailing trivia.
        (
            "const a = 1\n// mid\n## section\nprintln(a)\n",
            "// mid\n## section\nfn main() -> () {\n  const a = 1\n\n  println(a)\n}\n",
        ),
        // Consecutive folded groups: both separators stack before `main`,
        // each still on its own physical line.
        (
            "const a = 1\n## s1\nprintln(a)\n## s2\nprintln(b)\n",
            "## s1\n## s2\nfn main() -> () {\n  const a = 1\n\n  println(a)\n\n  println(b)\n}\n",
        ),
        // Declarations between the statement groups stay their own items;
        // the separators keep separating them from `main`.
        (
            "const a = 1\n## helpers\nfn helper() -> () {}\n## run\nprintln(a)\n",
            "## helpers\nfn helper() -> () {}\n## run\nfn main() -> () {\n  const a = 1\n\n  \
             println(a)\n}\n",
        ),
        // A separator behind the last statement survives ahead of `main`:
        // the fold fills the empty block the trailing separator leaves, so
        // the separator lands in the window before that block.
        (
            "println(a)\n## tail\n",
            "## tail\nfn main() -> () { println(a) }\n",
        ),
    ];
    for (i, (src, expected)) in fixtures.iter().enumerate() {
        let name = format!("script-entry-sep[{i}]");
        let mut files = Files::new();
        let file = files.add_source(&name, src);
        let ast = parse_script(&files, file)
            .unwrap_or_else(|d| panic!("{name} should parse as the entry: {}", d[0].message));
        let out = format_source(&files, file, &ast);
        assert_eq!(
            out, *expected,
            "{name} separator lost or moved\n--- src:\n{src}"
        );
        let mut re_files = Files::new();
        let re = re_files.add_source(&name, &out);
        let re_ast = parse(&re_files, re).unwrap_or_else(|d| {
            panic!(
                "{name}: formatted output does not parse: {}\n--- output:\n{out}",
                d[0].message
            )
        });
        let out2 = format_source(&re_files, re, &re_ast);
        assert_eq!(
            out, out2,
            "{name} not idempotent\n--- first:\n{out}\n--- second:\n{out2}"
        );
    }
}

/// A short window around the first byte where two shape dumps diverge.
fn first_diff(a: &str, b: &str) -> String {
    let at = a
        .bytes()
        .zip(b.bytes())
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let window = |s: &str| s[at.saturating_sub(60)..(at + 90).min(s.len())].to_owned();
    format!(
        "...\n  src:  {}\n  fmt:  {}",
        window(a).escape_debug(),
        window(b).escape_debug()
    )
}
