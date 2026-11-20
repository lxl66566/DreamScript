//! Trivia edge suite: comment attachment, blank-line collapsing, doc-comment
//! travel, `##` separator recovery, the nothing-is-dropped guarantee, and
//! the verbatim ranges an EOL fold must skip.

mod common;

use common::fmt_once;
use ds_diag::Files;
use ds_fmt::{format_source, format_source_tracked};
use ds_parse::parse;

#[test]
fn line_comment_attaches_to_following_item() {
    assert_eq!(
        fmt_once("t.ds", "// about a\ntag a\n// about b\ntag b\n"),
        "// about a\ntag a\n// about b\ntag b\n"
    );
}

#[test]
fn same_line_comment_trails_its_item() {
    assert_eq!(
        fmt_once("t.ds", "tag a // done\ntag b\n"),
        "tag a // done\ntag b\n"
    );
}

#[test]
fn comment_run_keeps_order_before_item() {
    assert_eq!(
        fmt_once("t.ds", "// one\n// two\n/* three */\ntag a\n"),
        "// one\n// two\n/* three */\ntag a\n"
    );
}

#[test]
fn blank_line_runs_collapse_to_one() {
    assert_eq!(
        fmt_once(
            "t.ds",
            &"tag a\n\n\n\n\n<tag>\ntag b\n".replace("<tag>", "// gap")
        ),
        "tag a\n\n// gap\ntag b\n"
    );
    assert_eq!(
        fmt_once("t.ds", "tag a\n\n\nconst b = 1\n\n\n\nconst c = 2\n"),
        "tag a\n\nconst b = 1\n\nconst c = 2\n"
    );
}

#[test]
fn no_blank_between_adjacent_lines() {
    assert_eq!(fmt_once("t.ds", "tag a\ntag b\n"), "tag a\ntag b\n");
    // A trailing comment line then next item: adjacent, no blank injected.
    assert_eq!(
        fmt_once("t.ds", "tag a // x\ntag b\n"),
        "tag a // x\ntag b\n"
    );
}

#[test]
fn doc_comments_travel_with_their_item() {
    let src = "/// doc for a\n// and a note\ntag a\ntag b\n";
    assert_eq!(
        fmt_once("t.ds", src),
        "/// doc for a\n// and a note\ntag a\ntag b\n"
    );
    // Attr sits between doc and item: attrs render above the decl, doc above
    // the attrs' position is preserved as leading trivia order.
    let src = "/// doc\n#[derive(show)]\ntag a\n";
    assert_eq!(fmt_once("t.ds", src), "/// doc\n#[derive(show)]\ntag a\n");
}

#[test]
fn doc_comments_stay_byte_stable_attached_or_loose() {
    // The doc slot is attachment data only: attached runs and runs a blank
    // line cut loose both print exactly where they stood.
    let src = "/// parses one row\nfn parse(s: string) -> int { 0 }\n/// loose\n\nfn other() -> \
               int { 1 }\n";
    assert_eq!(fmt_once("t.ds", src), src);
}

#[test]
fn member_and_field_docs_print_where_they_stood() {
    // The member and field doc slots are the placement input at member
    // grain: an attached `///` run prints immediately above its member at
    // the member's indent, never migrating to the item's trailing line.
    assert_eq!(
        fmt_once(
            "t.ds",
            "trait animal {\n/// speaks\nfn speak(self) -> string\n}\n"
        ),
        "trait animal {\n  /// speaks\n  fn speak(self) -> string\n}\n"
    );
    assert_eq!(
        fmt_once(
            "t.ds",
            "struct point {\n/// x coord\nx: int\n/// y\ny: int\n}\n"
        ),
        "struct point {\n  /// x coord\n  x: int\n  /// y\n  y: int\n}\n"
    );
    // An impl member fn carries the same slot.
    assert_eq!(
        fmt_once(
            "t.ds",
            "impl animal for spot {\n/// the impl\nfn speak(self) -> string { \"\" }\n}\n"
        ),
        "impl animal for spot {\n  /// the impl\n  fn speak(self) -> string { \"\" }\n}\n"
    );
    // An enum-case payload field is the nested case: the doc sits at the
    // field's own indent inside the case's body.
    assert_eq!(
        fmt_once("t.ds", "enum shape {\npoint {\n/// radius\nr: int\n}\n}\n"),
        "enum shape {\n  point {\n    /// radius\n    r: int\n  }\n}\n"
    );
    // A multi-line doc block keeps its whole run above the member.
    assert_eq!(
        fmt_once(
            "t.ds",
            "trait t {\n/// one\n/// two\nfn f(self) -> int\n}\n"
        ),
        "trait t {\n  /// one\n  /// two\n  fn f(self) -> int\n}\n"
    );
    // `////` is an ordinary comment: a separator line above a definition
    // survives formatting like any leading trivia.
    assert_eq!(
        fmt_once("t.ds", "//// section\ntag a\n"),
        "//// section\ntag a\n"
    );
}

#[test]
fn only_attached_member_docs_print_in_place() {
    // The placement input is the attached run alone: a plain interior
    // comment, a doc a blank line cut loose, and the comments no slot
    // owns (associated types, deinit members, case names) keep the v1
    // interior-comment migration to the item's trailing line.
    assert_eq!(
        fmt_once(
            "t.ds",
            "struct point {\n/// x\nx: int\n// note\ny: int\n}\n"
        ),
        "struct point {\n  /// x\n  x: int\n  y: int\n} // note\n"
    );
    assert_eq!(
        fmt_once("t.ds", "struct point {\n/// loose\n\nx: int\n}\n"),
        "struct point {\n  x: int\n} /// loose\n"
    );
    // A same-line comment after the closing brace still trails the item,
    // past the member docs that printed inside.
    assert_eq!(
        fmt_once("t.ds", "struct point {\n/// x\nx: int\n} // tail\n"),
        "struct point {\n  /// x\n  x: int\n} // tail\n"
    );
    // Members without a slot keep the migration for their docs.
    assert_eq!(
        fmt_once("t.ds", "struct s {\n/// no slot\ndeinit { 0\n}\n}\n"),
        "struct s {\n  deinit {\n    0\n  }\n} /// no slot\n"
    );
}

#[test]
fn member_doc_placement_round_trips_idempotently() {
    // The in-place placement is stable: formatting the formatted output
    // re-attaches each doc run to the member it now precedes and prints
    // it back at the same spot, migrated comments included.
    let src = "//// helpers\ntrait animal {\n/// speaks\n/// loudly\nfn speak(self) -> string\n// \
               stray\n}\nstruct point {\n/// x\nx: int\n}\nenum shape {\npoint {\n/// radius\nr: \
               int\n}\n}\n";
    let once = fmt_once("t.ds", src);
    assert_eq!(fmt_once("t.ds", &once), once);
    assert_eq!(
        once,
        "//// helpers\ntrait animal {\n  /// speaks\n  /// loudly\n  fn speak(self) -> string\n} \
         // stray\nstruct point {\n  /// x\n  x: int\n}\nenum shape {\n  point {\n    /// \
         radius\n    r: int\n  }\n}\n"
    );
}

#[test]
fn eof_comments_survive() {
    assert_eq!(
        fmt_once("t.ds", "tag a\n// the end\n"),
        "tag a\n// the end\n"
    );
    assert_eq!(
        fmt_once("t.ds", "tag a\n\n\n// the end\n"),
        "tag a\n\n// the end\n"
    );
    // Comment-only file.
    assert_eq!(fmt_once("t.ds", "// nothing else\n"), "// nothing else\n");
}

#[test]
fn comments_before_closing_brace_survive() {
    assert_eq!(
        fmt_once("t.ds", "fn f() {\nconst x = 1\n// done with f\n}\n"),
        "fn f() {\n  const x = 1\n  // done with f\n}\n"
    );
    // Statement comments inside a block: leading attaches to the statement.
    assert_eq!(
        fmt_once("t.ds", "fn f() {\n// about x\nconst x = 1\nx\n}\n"),
        "fn f() {\n  // about x\n  const x = 1\n  x\n}\n"
    );
}

#[test]
fn trailing_comment_on_last_statement() {
    assert_eq!(
        fmt_once("t.ds", "fn f() {\nconst x = 1 // done\n}\n"),
        "fn f() {\n  const x = 1 // done\n}\n"
    );
}

#[test]
fn deep_trivia_migrates_but_never_disappears() {
    // A comment inside a declaration (between header and body) has no deeper
    // attachment point: it migrates to the item's trailing position (v1
    // limit, documented in the crate docs). The body keeps its source-driven
    // layout (it spanned lines, so it opens up).
    assert_eq!(
        fmt_once("t.ds", "fn f() /* mid */ { x\n}\n"),
        "fn f() {\n  x\n} /* mid */\n"
    );
    // Same-line deep trivia lands inline behind the item.
    assert_eq!(
        fmt_once("t.ds", "const x = 1 /* unit */\n"),
        "const x = 1 /* unit */\n"
    );
}

#[test]
fn block_comments_on_their_own_line() {
    assert_eq!(
        fmt_once("t.ds", "/* a */\n/* b */\ntag a\n"),
        "/* a */\n/* b */\ntag a\n"
    );
}

#[test]
fn separators_recovered_verbatim() {
    assert_eq!(fmt_once("t.ds", "tag a\n##\ntag b\n"), "tag a\n##\ntag b\n");
    // Separator text after the marker is preserved.
    assert_eq!(
        fmt_once("t.ds", "tag a\n## section two\ntag b\n"),
        "tag a\n## section two\ntag b\n"
    );
    // Blank lines around a separator collapse to one each side.
    assert_eq!(
        fmt_once("t.ds", "tag a\n\n\n## s\n\n\ntag b\n"),
        "tag a\n\n## s\n\ntag b\n"
    );
    // A comment in the gap belongs to the following block's content.
    assert_eq!(
        fmt_once("t.ds", "tag a\n##\n// lead\ntag b\n"),
        "tag a\n##\n// lead\ntag b\n"
    );
}

#[test]
fn separator_only_and_empty_blocks() {
    assert_eq!(fmt_once("t.ds", "##\n"), "##\n");
    assert_eq!(fmt_once("t.ds", "##\n##\ntag a\n"), "##\n##\ntag a\n");
    assert_eq!(fmt_once("t.ds", ""), "");
    assert_eq!(fmt_once("t.ds", "  \n\n\t\n"), "");
}

#[test]
fn crlf_input_formats_to_lf() {
    assert_eq!(
        fmt_once("t.ds", "const a = 1\r\nconst b = 2\r\n"),
        "const a = 1\nconst b = 2\n"
    );
}

/// Verbatim line endings are program data: a raw string's bare-LF line
/// survives inside a CRLF-dominant source, and the tracked ranges cover
/// exactly the literals and comments, so an EOL fold can leave them
/// byte-identical.
#[test]
fn verbatim_line_endings_and_ranges_survive_formatting() {
    let mut files = Files::new();
    let src = "const a = 1\r\n// note\r\nconst s = r\"one\ntwo\"\r\n";
    let file = files.add_source("t.ds", src);
    let ast = parse(&files, file).expect("parses");
    let formatted = format_source_tracked(&files, file, &ast);
    assert_eq!(
        formatted.text,
        "const a = 1\n// note\nconst s = r\"one\ntwo\"\n"
    );
    let verbatim: Vec<&str> = formatted
        .verbatim
        .iter()
        .map(|&(s, e)| &formatted.text[s..e])
        .collect();
    assert_eq!(verbatim, vec!["// note", "r\"one\ntwo\""]);
    // The mixed output is a fixed point of the formatter itself.
    let mut re_files = Files::new();
    let re = re_files.add_source("t.ds", &formatted.text);
    let re_ast = parse(&re_files, re).expect("the output parses");
    assert_eq!(format_source(&re_files, re, &re_ast), formatted.text);
}

#[test]
fn nothing_dropped_over_the_corpus() {
    // Every `//` and `/*` byte of the source must survive formatting: the
    // formatter may move comments, never lose them.
    for src in [
        "tag a\n// one\n\n\n/* two */\n##\n// three\ntag b\n// eof\n",
        "fn f() {\n// a\nconst x = 1 // b\n// c\n}\n// after\n",
    ] {
        let out = fmt_once("t.ds", src);
        assert_eq!(out.matches("//").count(), src.matches("//").count());
        assert_eq!(out.matches("/*").count(), src.matches("/*").count());
    }
}
