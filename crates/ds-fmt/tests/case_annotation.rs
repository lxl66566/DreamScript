//! The union-default member annotation's surface round-trip: an
//! annotated case renders with its `#[default]`, on its own line above
//! the case in the multiline form and inline beside it in the one-line
//! form — the formatter drops nothing the grammar admits.

mod common;

use common::fmt_once;

#[test]
fn the_annotated_case_renders_inline() {
    assert_eq!(
        fmt_once("t.ds", "enum tone { #[default] quiet, loud }\n"),
        "enum tone { #[default] quiet, loud }\n"
    );
}

#[test]
fn the_annotated_case_renders_on_its_own_line() {
    assert_eq!(
        fmt_once(
            "t.ds",
            "enum tone {\n#[default] quiet,\nloud { volume: int },\n}\n"
        ),
        "enum tone {\n  #[default] quiet\n  loud { volume: int }\n}\n"
    );
}
