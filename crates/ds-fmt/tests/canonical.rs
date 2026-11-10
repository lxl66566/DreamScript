//! Canonical-form suite: the zero-option rulings of the record (addendum 6.1)
//! as observable behavior — square-bracket generics, no angle form, no
//! trailing commas, 2-space indent, no wrapping, one trailing newline, string
//! literals verbatim.

mod common;

use common::fmt_once;

#[test]
fn angle_generics_never_emitted() {
    assert_eq!(
        fmt_once("t.ds", "struct box[t] { item: list<int> }\n"),
        "struct box[t] { item: list[int] }\n"
    );
    assert_eq!(
        fmt_once("t.ds", "impl stack<int> { fn top(self) -> t? { self }\n}\n"),
        "impl stack[int] {\n  fn top(self) -> t? { self }\n}\n"
    );
}

#[test]
fn case_type_refs_keep_parens() {
    // `bad_number(string)` is a case-type reference, not generic application;
    // the parens must survive (the AST alone cannot tell them apart).
    assert_eq!(
        fmt_once("t.ds", "type e = badnumber(string) | missing\n"),
        "type e = badnumber(string) | missing\n"
    );
}

#[test]
fn trailing_commas_not_emitted() {
    assert_eq!(
        fmt_once("t.ds", "fn f(a: int,\n  b: int,\n) -> int { g(a, b,) }\n"),
        "fn f(a: int, b: int) -> int { g(a, b) }\n"
    );
    assert_eq!(
        fmt_once("t.ds", "const t = (1, 2,)\n"),
        "const t = (1, 2)\n"
    );
    assert_eq!(
        fmt_once("t.ds", "fn f() { const (a, b,) = pair\n a\n}\n"),
        "fn f() {\n  const (a, b) = pair\n  a\n}\n"
    );
}

#[test]
fn one_element_tuple_keeps_its_comma() {
    // The comma alone distinguishes the tuple from a paren.
    assert_eq!(fmt_once("t.ds", "const t = (1,)\n"), "const t = (1,)\n");
    assert_eq!(fmt_once("t.ds", "const p = (1)\n"), "const p = (1)\n");
}

#[test]
fn two_space_indent() {
    assert_eq!(
        fmt_once(
            "t.ds",
            "fn f() {\nif a {\nnursery {\ng(1)\n}\n} else {\nh(2)\n}\n}\n"
        ),
        "fn f() {\n  if a {\n    nursery {\n      g(1)\n    }\n  } else {\n    h(2)\n  }\n}\n"
    );
    // `.`-chain continuations sit at base indent + 2.
    assert_eq!(
        fmt_once(
            "t.ds",
            "fn f() { const t = us\n  .filter((u) => u.a >= 1)\n  .map((u) => u.a)\n t\n}\n"
        ),
        "fn f() {\n  const t = us\n    .filter((u) => u.a >= 1)\n    .map((u) => u.a)\n  t\n}\n"
    );
}

#[test]
fn no_wrapping_long_lines_stay_long() {
    let long = format!("const s = \"{}\" + g(a, b, c)\n", "x".repeat(120));
    let out = fmt_once("t.ds", &long);
    assert_eq!(out, long);
}

#[test]
fn exactly_one_trailing_newline() {
    assert_eq!(fmt_once("t.ds", "tag a\n"), "tag a\n");
    assert_eq!(fmt_once("t.ds", "tag a\n\n\n"), "tag a\n");
    assert_eq!(fmt_once("t.ds", ""), "");
    assert_eq!(fmt_once("t.ds", "  \n\n\t\n"), "");
}

#[test]
fn string_literals_verbatim() {
    // Escapes, raw strings, and hash escalation re-render byte-for-byte.
    assert_eq!(
        fmt_once("t.ds", "const s = \"a $b ${x + 1} \\n \\u{1F600} \\\"\"\n"),
        "const s = \"a $b ${x + 1} \\n \\u{1F600} \\\"\"\n"
    );
    assert_eq!(
        fmt_once("t.ds", "const s = r#\"has \"quotes\" inside\"#\n"),
        "const s = r#\"has \"quotes\" inside\"#\n"
    );
    assert_eq!(
        fmt_once("t.ds", "const s = r###\"deeper \"## hash\"###\n"),
        "const s = r###\"deeper \"## hash\"###\n"
    );
}

#[test]
fn numeric_separators_are_canonicalized_away() {
    // The AST stores separator-free digit text; the canonical form is the
    // compact spelling (B7: separators are input laxity).
    assert_eq!(
        fmt_once("t.ds", "const a = 1_000_000i64\n"),
        "const a = 1000000i64\n"
    );
    assert_eq!(
        fmt_once("t.ds", "const d = 0b1010_1010u8\n"),
        "const d = 0b10101010u8\n"
    );
}

#[test]
fn inline_containers_stay_inline_multiline_open_up() {
    assert_eq!(
        fmt_once("t.ds", "struct p { x: int, y: int }\n"),
        "struct p { x: int, y: int }\n"
    );
    assert_eq!(
        fmt_once("t.ds", "struct p {\nx: int,\ny: int,\n}\n"),
        "struct p {\n  x: int\n  y: int\n}\n"
    );
    // Match arms: expression bodies keep the comma the grammar requires.
    assert_eq!(
        fmt_once("t.ds", "fn f() {\nmatch x { a => 1, _ => 2, }\n}\n"),
        "fn f() {\n  match x { a => 1, _ => 2 }\n}\n"
    );
}

#[test]
fn converted_fns_render_as_fn() {
    // A top-level `fn` converts onto the const-declaration node (one
    // binding of one lambda); the formatter renders that shape back as
    // the fn spelling — the one named-function form [F2].
    assert_eq!(
        fmt_once("t.ds", "fn  speak ( self ) ->  string { \"\" }\n"),
        "fn speak(self) -> string { \"\" }\n"
    );
    assert_eq!(
        fmt_once(
            "t.ds",
            "fn push[t: ~Copyable] ( s: stack[t] ) -> stack[t] {\n  s\n}\n"
        ),
        "fn push[t: ~Copyable](s: stack[t]) -> stack[t] {\n  s\n}\n"
    );
}

#[test]
fn visibility_and_attrs() {
    assert_eq!(
        fmt_once(
            "t.ds",
            "#[derive(hash, show)]\nstruct point { x: int, y: int }\n"
        ),
        "#[derive(hash, show)]\nstruct point { x: int, y: int }\n"
    );
    assert_eq!(
        fmt_once("t.ds", "pub(pkg)  fn   g ( x : int ) ->  int { x }\n"),
        "pub(pkg) fn g(x: int) -> int { x }\n"
    );
}
