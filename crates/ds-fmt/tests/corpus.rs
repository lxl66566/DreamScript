//! The M1 acceptance gates over the corpus [implementation-plan M1]:
//! `fmt(fmt(x)) == fmt(x)` and re-parsing the formatted output yields the
//! same AST, for every `conformance/*.ds` and every positive fixture of the
//! ds-parse surface.

mod common;

use std::path::PathBuf;

use common::{POSITIVE, fmt_once, shape};
use ds_diag::Files;
use ds_parse::parse;

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
        98,
        "fixture count drifted; keep in sync with ds-parse positives"
    );
    for (i, src) in POSITIVE.iter().enumerate() {
        check(&format!("positive[{i}]"), src);
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
