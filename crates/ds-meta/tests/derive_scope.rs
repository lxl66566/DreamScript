//! The derive-expanding analyze entries over raw trees: the expansion
//! strip-and-synthesize seam the scope query depends on, pinned where
//! the engine lives.

use ds_check::{Package, ScopeKind, scope_frames, visible_at};
use ds_diag::Files;
use ds_meta::expand_derives;
use ds_parse::parse;

/// A derived declaration's own positions answer its declaration scope
/// only: the synthesized impl frames mint their spans inside the
/// declaration, so none of their binders answers there, and same-named
/// package value declarations stay unshadowed.
#[test]
fn derived_declaration_positions_answer_no_synthesized_binders() {
    let source = concat!(
        "const h = 1\n",
        "const seed = 2\n",
        "#[derive(hash, show)] struct Point { x: int, y: int }\n",
    );
    let mut files = Files::new();
    let mut asts = Vec::new();
    for (name, src) in [("a.ds", "tag int\n"), ("m.ds", source)] {
        let file = files.add_source(name, src);
        let ast =
            parse(&files, file).unwrap_or_else(|diags| panic!("{name} should parse: {diags:?}"));
        asts.push(ast);
    }
    let (asts, diags) = expand_derives(&asts);
    assert!(diags.is_empty(), "the derives expand: {diags:?}");
    let mut package = Package::new();
    for ast in &asts {
        let diags = package.add_file(ast);
        assert!(diags.is_empty(), "unexpected collect diags: {diags:?}");
    }
    let frames = scope_frames(&asts[1], &package);
    // Every position of the derived declaration — the attribute, the
    // fields, the braces — answers the declaration scope only.
    let start = at(source, "#[derive", 0, 0);
    let end = at(source, "}", 0, 1);
    for offset in start..=end {
        let entries = visible_at(&frames, &package, asts[0].file, offset);
        let locals: Vec<_> = entries
            .iter()
            .filter(|entry| matches!(entry.kind, ScopeKind::Let | ScopeKind::Param))
            .collect();
        assert!(locals.is_empty(), "at {offset}: {locals:?}");
        let kind = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("at {offset}: `{name}` answers: {:?}", names(&entries)))
                .kind
        };
        // The minted binders (self, h, seed, ...) suppress nothing: the
        // same-named package value declarations answer.
        assert_eq!(kind("h"), ScopeKind::Const);
        assert_eq!(kind("seed"), ScopeKind::Const);
        assert_eq!(kind("Point"), ScopeKind::Struct);
    }
}

/// The position helper: the nth occurrence of one needle, advanced.
fn at(source: &str, needle: &str, nth: usize, advance: u32) -> u32 {
    let mut from = 0;
    for _ in 0..nth {
        let found = source[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("`{needle}` occurs {nth} times"));
        from += found + needle.len();
    }
    let found = source[from..]
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` occurs more than {nth} times"));
    u32::try_from(from + found).expect("offsets fit u32") + advance
}

/// The names of one answer, for panic messages.
fn names(entries: &[ds_check::ScopeEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}
