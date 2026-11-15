//! The derive slice's conformance gate, pinned beside the engine: the
//! ruled refusal classes and the clean run items over
//! `conformance/derives.ds`, through the meta layer's expanding
//! analyze entry (the raw trees enter here).

use ds_check::corpus::stages;
use ds_corpus_path::resolve as corpus_path;
use ds_diag::{Diagnostic, Files, Span};
use ds_meta::analyze;
use ds_parse::{Ast, Item, parse};

/// Ambient for the derive slice: the show, ord, hash, and clone traits
/// the derives target beside the primitives' own impls (ord's int
/// directly, string, char, and byte through their compare natives;
/// hash's int mixing its own word; clone's int answering itself), the
/// compare and hash externs those impls call, and the Copyable marker
/// (the file's `res` names the marker in its supers).
const DERIVES_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag char\n",
    "pub tag byte\n",
    "pub trait Copyable { }\n",
    "pub trait show {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "pub trait ord {\n",
    "  fn compare(self, other: Self) -> int\n",
    "}\n",
    "impl ord for int {\n",
    "  fn compare(self, other: Self) -> int {\n",
    "    if self < other { -1 } else if self > other { 1 } else { 0 }\n",
    "  }\n",
    "}\n",
    "impl ord for string {\n",
    "  fn compare(self, other: Self) -> int { str_compare(self, other) }\n",
    "}\n",
    "impl ord for char {\n",
    "  fn compare(self, other: Self) -> int { char_compare(self, other) }\n",
    "}\n",
    "impl ord for byte {\n",
    "  fn compare(self, other: Self) -> int { byte_compare(self, other) }\n",
    "}\n",
    "pub trait hash {\n",
    "  fn hash(self) -> int\n",
    "}\n",
    "impl hash for int {\n",
    "  fn hash(self) -> int { self }\n",
    "}\n",
    "pub trait clone {\n",
    "  fn clone(self) -> Self\n",
    "}\n",
    "impl clone for int {\n",
    "  fn clone(self) -> Self { self }\n",
    "}\n",
    "pub extern fn str[t] (v: t) -> string\n",
    "pub extern fn str_compare (a: string, b: string) -> int\n",
    "pub extern fn char_compare (a: char, b: char) -> int\n",
    "pub extern fn byte_compare (a: byte, b: byte) -> int\n",
    "pub extern fn println (s: string) -> ()\n",
);

#[test]
fn derives_rejects_carry_the_ruled_refusal_classes() {
    let rejects = gate("../../conformance/derives.ds", DERIVES_AMBIENT);
    let expected: &[(&str, &str, &str)] = &[
        (
            "eq-cell",
            "DS2027",
            "`==` is universal structural equality and is never derived [B9]",
        ),
        (
            "mystery-cell",
            "DS2025",
            "`mystery` is not a derive name [G1]",
        ),
        (
            "to-json-cell",
            "DS2024",
            "derive `to_json` is ruled but not implemented yet [G1]",
        ),
        (
            "cb-cell",
            "DS2026",
            "derive `show` refuses the field `cb`: a function-typed part compares and shows as no \
             structural value [G1, B9]",
        ),
        (
            "holder",
            "DS2026",
            "derive `show` refuses the field `kept`: `res` declares the `~Copyable` linear layer \
             [G1, B9]",
        ),
        (
            "not-data",
            "DS2028",
            "a derive applies to a struct, enum, or tag declaration [G1]",
        ),
        (
            "gen-cell",
            "DS2029",
            "derive `show` refuses a generic declaration [G1]",
        ),
    ];
    assert_eq!(rejects.len(), expected.len(), "one reject per class");
    for ((id, _what, diag), (want_id, code, message)) in rejects.iter().zip(expected) {
        assert_eq!(id, want_id, "the reject items align with the header");
        assert_eq!(diag.code.as_str(), *code, "{id}");
        assert_eq!(diag.message, *message, "{id}");
    }
}

#[test]
fn derives_run_items_check_clean_with_synthesized_impls() {
    // The show, ord, hash, and clone derives synthesize their impls and
    // every run item checks clean beside the ambient's own traits;
    // `res` (the linear declaration the holder reject names)
    // typechecks on its own.
    let path = ds_corpus_path::resolve("../../conformance/derives.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", DERIVES_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = analyze(&files, &asts).checked;
    let items = items_of(&asts[1]);
    for (item, stage) in items.iter().zip(stages.iter()) {
        if stage.stage != "run" && stage.stage != "typecheck" {
            continue;
        }
        let item_diags: Vec<&Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        assert!(
            item_diags.is_empty(),
            "item `{}` must check clean, got:\n{}",
            stage.id,
            item_diags
                .iter()
                .map(|d| format!("{} {}", d.code.as_str(), d.message))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// One item's positions: the declaration spans a corpus item owns.
fn items_of(ast: &Ast) -> Vec<&Item> {
    ast.blocks.iter().flat_map(|b| b.items.iter()).collect()
}

/// Whether one span sits inside one item.
fn within(span: Span, item: &Item) -> bool {
    span.file == item.span.file && span.start >= item.span.start && span.end <= item.span.end
}

/// Runs the check over one corpus file plus ambient and returns every
/// `reject` item with the diagnostic whose label falls inside it.
fn gate(corpus: &str, ambient: &str) -> Vec<(String, String, Diagnostic)> {
    let path = corpus_path(corpus);
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", ambient);
    let cor = files.add_source("corpus.ds", &source);
    let amb_ast = parse(&files, amb).unwrap_or_else(|d| panic!("{corpus} ambient: {d:?}"));
    let cor_ast = parse(&files, cor).unwrap_or_else(|d| panic!("{corpus} corpus: {d:?}"));
    let checked = analyze(&files, &[amb_ast, cor_ast.clone()]).checked;

    let items = items_of(&cor_ast);
    assert_eq!(
        items.len(),
        stages.len(),
        "{corpus}: header items and parsed items disagree"
    );
    let mut out = Vec::new();
    for (item, stage) in items.iter().zip(stages.iter()) {
        let item_diags: Vec<&Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        if stage.stage == "typecheck" {
            assert!(
                item_diags.is_empty(),
                "{}: item `{}` must typecheck, got:\n{}",
                corpus,
                stage.id,
                item_diags
                    .iter()
                    .map(|d| format!("{} {}", d.code.as_str(), d.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        } else if let Some(what) = stage.stage.strip_prefix("reject ") {
            assert_eq!(
                item_diags.len(),
                1,
                "{}: item `{}` expects exactly one rejection, got {:?}",
                corpus,
                stage.id,
                what
            );
            out.push((stage.id.clone(), what.to_owned(), item_diags[0].clone()));
        }
    }
    out
}
