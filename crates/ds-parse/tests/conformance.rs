//! Conformance smoke test: every `conformance/*.ds` at the repo root parses
//! with zero syntax diagnostics. `unresolved` items live as comments and
//! `reject` items are live code that must parse (they fail at type checking,
//! not here), so a clean whole-file parse is the expectation for every file.

use ds_diag::Files;
use ds_parse::parse;

#[test]
fn conformance_corpus_parses_cleanly() {
    let dir = ds_corpus_path::dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        panic!(
            "conformance corpus missing at {}; it landed on main alongside this crate",
            dir.display()
        );
    };
    let mut checked = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "ds") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            panic!("cannot read {}", path.display());
        };
        assert!(
            source.contains("// items:"),
            "{} lacks the mechanical items header",
            path.display()
        );
        let mut files = Files::new();
        let file = files.add_source(
            path.file_name().unwrap_or_default().to_string_lossy(),
            &source,
        );
        if let Err(diags) = parse(&files, file) {
            for d in &diags {
                let loc = files.location(d.labels[0].span.file, d.labels[0].span.start);
                eprintln!(
                    "{} {}:{}:{}: {}",
                    d.code.as_str(),
                    path.display(),
                    loc.line,
                    loc.col,
                    d.message
                );
            }
            panic!("{} produced syntax diagnostics", path.display());
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "conformance dir exists but holds no .ds files; the suite is vacuous"
    );
}
