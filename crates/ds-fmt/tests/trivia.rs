//! Trivia edge suite: comment attachment, blank-line collapsing, doc-comment
//! travel, `///|` separator recovery, and the nothing-is-dropped guarantee.

mod common;

use common::fmt_once;

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
    assert_eq!(
        fmt_once("t.ds", "tag a\n///|\ntag b\n"),
        "tag a\n///|\ntag b\n"
    );
    // Text and column preserved.
    assert_eq!(
        fmt_once("t.ds", "tag a\n  ///| section two\ntag b\n"),
        "tag a\n  ///| section two\ntag b\n"
    );
    // Blank lines around a separator collapse to one each side.
    assert_eq!(
        fmt_once("t.ds", "tag a\n\n\n///| s\n\n\ntag b\n"),
        "tag a\n\n///| s\n\ntag b\n"
    );
    // A comment in the gap belongs to the following block's content.
    assert_eq!(
        fmt_once("t.ds", "tag a\n///|\n// lead\ntag b\n"),
        "tag a\n///|\n// lead\ntag b\n"
    );
}

#[test]
fn separator_only_and_empty_blocks() {
    assert_eq!(fmt_once("t.ds", "///|\n"), "///|\n");
    assert_eq!(
        fmt_once("t.ds", "///|\n///|\ntag a\n"),
        "///|\n///|\ntag a\n"
    );
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

#[test]
fn nothing_dropped_over_the_corpus() {
    // Every `//` and `/*` byte of the source must survive formatting: the
    // formatter may move comments, never lose them.
    for src in [
        "tag a\n// one\n\n\n/* two */\n///|\n// three\ntag b\n// eof\n",
        "fn f() {\n// a\nconst x = 1 // b\n// c\n}\n// after\n",
    ] {
        let out = fmt_once("t.ds", src);
        assert_eq!(out.matches("//").count(), src.matches("//").count());
        assert_eq!(out.matches("/*").count(), src.matches("/*").count());
    }
}
