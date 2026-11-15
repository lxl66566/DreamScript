//! The G4 visibility tiers and import surface over one package:
//! private is the defining file's alone, `pub(pkg)` opens the package,
//! `pub` opens dependents (the `.dsi` gate), an `@`-sigil import's
//! selections validate against the exported surface, a bare-path import
//! stays declarative, a condition rides its import as a source-internal
//! spelling until the target model lands, and a visibility modifier on
//! an item that names no package-level definition refuses honestly.

use ds_check::{Vis, resolve_package};
use ds_diag::{ErrorCode, Files};
use ds_parse::parse;

/// Parses the sources into one package and resolves them, returning the
/// collection and resolution diagnostics sorted by code. The `int` atom
/// rides an exported prelude member so no test depends on the real
/// ambient.
fn diags(sources: &[(&str, &str)]) -> Vec<ds_diag::Diagnostic> {
    let mut files = Files::new();
    let mut asts = Vec::new();
    let prelude = files.add_source("prelude.ds", "pub tag int\n");
    let ast = parse(&files, prelude).expect("the prelude parses");
    asts.push(ast);
    for (name, src) in sources {
        let file = files.add_source(*name, src);
        let ast =
            parse(&files, file).unwrap_or_else(|diags| panic!("{name} should parse: {diags:?}"));
        asts.push(ast);
    }
    let (_, _, mut all) = resolve_package(&asts);
    all.sort_by_key(|diag| diag.code.as_str());
    all
}

/// One library file carrying one name per tier, consumed from a second
/// file of the same package.
fn two_file_library() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "lib.ds",
            concat!(
                "fn hidden() -> int { 1 }\n",
                "pub(pkg) fn internal() -> int { 2 }\n",
                "pub fn open() -> int { 3 }\n",
                "fn uses_own() -> int { hidden() }\n",
            ),
        ),
        ("main.ds", "fn main() -> int { internal() + open() }\n"),
    ]
}

/// `pub(pkg)` and `pub` names answer a cross-file use; private stays
/// its defining file's alone, the same-file use still resolving.
#[test]
fn pub_pkg_and_pub_answer_a_cross_file_use() {
    let all = diags(&two_file_library());
    assert!(all.is_empty(), "the package-visible tiers answer: {all:?}");
}

/// A private definition used from another file refuses with the
/// two-span private diagnostic, the in-file use staying clean.
#[test]
fn a_private_use_across_files_refuses() {
    let sources = vec![
        ("lib.ds", "fn hidden() -> int { 1 }\n"),
        ("main.ds", "fn main() -> int { hidden() }\n"),
    ];
    let all = diags(&sources);
    assert_eq!(all.len(), 1, "exactly the private refusal: {all:?}");
    assert_eq!(all[0].code, ErrorCode::PrivateUse);
}

/// The refusal is per use site, so two cross-file uses of one private
/// name report twice while the defining file's own uses stay clean.
#[test]
fn each_cross_file_private_use_reports_once() {
    let sources = vec![
        ("lib.ds", "fn hidden() -> int { 1 }\n"),
        (
            "main.ds",
            concat!(
                "fn a() -> int { hidden() }\n",
                "fn b() -> int { hidden() }\n",
            ),
        ),
    ];
    let all = diags(&sources);
    assert_eq!(all.len(), 2, "one refusal per use: {all:?}");
    assert!(
        all.iter().all(|diag| diag.code == ErrorCode::PrivateUse),
        "both refusals name the private tier"
    );
}

/// The private tier covers every namespace kind: a private type is its
/// file's alone, and an enum's cases carry the enum's own tier.
#[test]
fn the_private_tier_covers_types_and_enum_cases() {
    let sources = vec![
        (
            "lib.ds",
            concat!(
                "struct rec { n: int }\n",
                "enum tone { quiet, loud }\n",
                "pub enum open_tone { open_quiet }\n",
            ),
        ),
        (
            "main.ds",
            concat!(
                "fn m(r: rec) -> int { r.n }\n",
                "fn q() -> tone { loud }\n",
                "fn o(t: open_tone) -> int { 2 }\n",
            ),
        ),
    ];
    let all = diags(&sources);
    // rec and tone are private, the used case inherits the enum's tier, open_tone's case rides the
    // pub tier
    assert_eq!(all.len(), 3, "rec, tone, and its case refuse: {all:?}");
    assert!(
        all.iter().all(|diag| diag.code == ErrorCode::PrivateUse),
        "every refusal names the private tier"
    );
}

/// An `@`-sigil import's selection validates against the exported
/// surface: a `pub` name selects clean, a `pub(pkg)` or private name
/// refuses as not exported, and a name no member declares refuses as
/// missing. The compile unit's merged namespace serves the `@std`
/// member this round.
#[test]
fn import_selections_validate_against_the_exported_surface() {
    let sources = vec![
        (
            "lib.ds",
            concat!(
                "pub fn reader() -> int { 1 }\n",
                "pub(pkg) fn internal() -> int { 2 }\n",
                "fn hidden() -> int { 3 }\n",
            ),
        ),
        (
            "main.ds",
            concat!(
                "import @std/csv (reader)\n",
                "import @std/csv (internal)\n",
                "import @std/csv (hidden)\n",
                "import @std/csv (absent)\n",
            ),
        ),
    ];
    let all = diags(&sources);
    assert!(
        all.len() == 3,
        "the non-exported and absent selections refuse: {all:?}"
    );
    assert_eq!(all[0].code, ErrorCode::ImportNotExported);
    assert_eq!(all[1].code, ErrorCode::ImportNotExported);
    assert_eq!(all[2].code, ErrorCode::ImportNameMissing);
}

/// An import without a selection list validates nothing: the module
/// alias is a declarative spelling the link walk records.
#[test]
fn a_selectionless_import_stays_declarative() {
    let sources = vec![
        ("lib.ds", "pub fn reader() -> int { 1 }\n"),
        (
            "main.ds",
            "import @std/csv\nimport acme/pipeline/schema (row)\n",
        ),
    ];
    assert!(diags(&sources).is_empty(), "no selection, no validation");
}

/// A bare-path import, and a sigiled path naming a package other than
/// `@std`, are declarative in the checker: their members sit outside the
/// compile unit, so the dep-list records them and the link walk
/// validates the link.
#[test]
fn an_out_of_unit_import_never_refuses_in_the_checker() {
    let sources = vec![(
        "main.ds",
        concat!(
            "import acme/pipeline/schema (row, parse_row)\n",
            "import acme/io as pio\n",
            "import @lib/helper (twice)\n",
        ),
    )];
    assert!(diags(&sources).is_empty(), "the link walk owns the refusal");
}

/// A condition rides its import as a source-internal spelling until the
/// compile-time target model lands: the checker neither evaluates nor
/// refuses it, and an `@std` import's selections still validate under it.
#[test]
fn a_conditions_spelling_stays_inert_this_round() {
    let sources = vec![
        ("lib.ds", "pub fn reader() -> int { 1 }\n"),
        (
            "main.ds",
            "import acme/pipeline/fs if os == \"windows\"\nimport @std/csv (reader) if os == \
             \"windows\"\n",
        ),
    ];
    assert!(
        diags(&sources).is_empty(),
        "the condition neither binds nor refuses"
    );
}

/// A visibility modifier on an import, impl, or platform declaration
/// names no package-level definition: the re-export and the impl member
/// tiers land with cross-package linking, so the modifier refuses
/// here.
#[test]
fn a_modifier_on_a_nameless_item_refuses() {
    let sources = vec![(
        "main.ds",
        concat!(
            "pub import @std/csv\n",
            "pub(pkg) import @std/csv\n",
            "pub impl int { fn bump(self) -> int { 0 } }\n",
            "pub platform \"std\" provides <console>\n",
        ),
    )];
    let all = diags(&sources);
    assert_eq!(all.len(), 4, "every nameless modifier refuses: {all:?}");
    assert!(
        all.iter().all(|diag| diag.code == ErrorCode::VisNoTarget),
        "the refusals name the empty target"
    );
}

/// The collected tiers answer their slot: private, `pub(pkg)`, and
/// `pub`, with `pub` alone exporting (the `.dsi` gate).
#[test]
fn the_collected_tiers_answer_their_slot() {
    let mut files = Files::new();
    let prelude = files.add_source("prelude.ds", "pub tag int\n");
    let prelude_ast = parse(&files, prelude).expect("the prelude parses");
    let file = files.add_source(
        "lib.ds",
        "fn a() -> int { 0 }\npub(pkg) fn b() -> int { 0 }\npub fn c() -> int { 0 }\n",
    );
    let ast = parse(&files, file).expect("parses");
    let (package, _, diags) = resolve_package(&[prelude_ast, ast]);
    assert_eq!(diags, [] as [ds_diag::Diagnostic; 0]);
    for (name, vis) in [("a", Vis::Private), ("b", Vis::Pkg), ("c", Vis::Pub)] {
        let def = package
            .lookup_value(name)
            .unwrap_or_else(|| panic!("`{name}` collects"));
        assert_eq!(package.def(def).vis, vis, "`{name}`'s tier");
        assert_eq!(
            package.def(def).vis.exported(),
            vis == Vis::Pub,
            "`{name}`'s gate"
        );
    }
}
