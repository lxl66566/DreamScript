//! The list-literal and `list[t]()` constructor surface [B3, F7, F9]:
//! bidirectional element typing against an expected `list[t]`, the
//! mixed-element rejection, the empty literal's expected-type requirement,
//! the constructor's spelling rules, and the cross-spelling rejections.

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub struct page { url: string }\n",
    "pub struct list[t]\n",
    "pub extern fn list_len[t] (xs: list[t]) -> int\n",
    "pub extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "pub extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "pub trait Copyable { }\n",
    "pub struct link: ~Copyable { slot: int }\n",
    "pub extern fn tweak (l: mut link) -> ()\n",
);

/// Type-checks `source` beside the list ambient, rendering every
/// diagnostic as one `CODE message` line.
fn check(source: &str) -> Vec<String> {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let src = files.add_source("probe.ds", source);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, src).expect("the probe parses"),
    ];
    let checked = type_check(&files, &asts);
    checked
        .diags
        .iter()
        .map(|diag| format!("{} {}", diag.code.as_str(), diag.message))
        .collect()
}

#[test]
fn literals_infer_elements_and_type_against_returns() {
    // inference without an expected type, containment against the declared
    // return, and the bidirectional read over the built list
    for source in [
        "pub fn f() -> list[int] {\n  [1, 2, 3]\n}\n",
        "pub fn f() -> list[string] {\n  [\"a\", \"b\"]\n}\n",
        "pub const xs = [1, 2]\nfn f() -> int {\n  list_len(xs)\n}\n",
        "pub fn f() -> list[int] {\n  []\n}\n",
        "pub const xs = list[int]()\nfn f() -> int {\n  list_len(xs)\n}\n",
        "pub fn f() -> int {\n  list_len(list[page]())\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
}

#[test]
fn mixed_elements_report_at_the_offending_item() {
    // with an expected list[t]: the element misses the expected type
    assert_eq!(check("fn f() -> list[int] {\n  [1, \"a\"]\n}\n"), [
        "DS2001 `list element` has type `string`, not contained in `int` [B2]"
    ],);
    // without one: the first item fixes the type, the later item reports
    assert_eq!(check("const bad = [1, \"a\"]\n"), ["DS2001 list literal \
                                                    mixes element types: \
                                                    item 2 has type \
                                                    `string`, the first \
                                                    item `int` [B2]"],);
}

#[test]
fn the_empty_literal_needs_its_element_evidence() {
    // one diagnostic, no empty-initializer cascade
    assert_eq!(check("const bad = []\n"), ["DS2002 the empty list \
                                            literal needs an expected \
                                            `list[t]`; write `list[t]()` \
                                            for the explicit empty list \
                                            [B3]"],);
}

#[test]
fn the_constructor_takes_no_arguments() {
    assert_eq!(check("const bad = list[int](1, 2)\n"), ["DS2001 the \
                                                         empty-list \
                                                         constructor \
                                                         takes no arguments; \
                                                         build with a \
                                                         list literal \
                                                         `[...]` [B3]"],);
}

#[test]
fn cross_spellings_reject_per_the_declaration() {
    // every type besides the list constructs per its own declaration
    assert_eq!(check("const bad = page[int]()\n"), ["DS2001 `page` \
                                                     constructs per its \
                                                     declaration, not by \
                                                     type application \
                                                     [B3]"],);
    // a value in the type-argument position
    assert_eq!(
        check("const x = 1\nconst bad = list[x]()\n")
            .iter()
            .filter(|line| line.contains("does not solve"))
            .count(),
        1,
    );
    // a type application outside a call constructs nothing
    assert_eq!(check("const bad = list[int]\n"), ["DS2001 `list` is a \
                                                   type; the application \
                                                   `list[...]` \
                                                   constructs only in \
                                                   call position — \
                                                   `list[t]()` [F9, B3]"],);
}

#[test]
fn list_push_rejects_const_receivers() {
    // the mut window is exclusive by spelling [D2]: a const binding may
    // alias its shared heap word, so both call spellings reject it
    const REJECTED: &str =
        "DS3006 `xs` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]";
    assert_eq!(
        check("fn f() -> int {\n  const xs = [1]\n  xs.list_push(2)\n  list_len(xs)\n}\n"),
        [REJECTED],
    );
    assert_eq!(
        check("fn f() -> int {\n  const xs = [1]\n  list_push(xs, 2)\n  list_len(xs)\n}\n"),
        [REJECTED],
    );
    // a top-level const binding rejects the same way
    assert_eq!(
        check("const xs = [1]\nfn f() -> int {\n  xs.list_push(2)\n  list_len(xs)\n}\n"),
        [REJECTED],
    );
}

#[test]
fn var_and_fresh_receivers_type_the_window() {
    // a var binding and a mut-window parameter carry the spelling's
    // exclusivity; a fresh rvalue is uniquely owned by construction [D2]
    for source in [
        "pub fn f() -> int {\n  var xs = [1]\n  xs.list_push(2)\n  list_len(xs)\n}\n",
        "pub fn f() -> int {\n  var xs = [1]\n  list_push(xs, 2)\n  list_len(xs)\n}\n",
        "pub fn fill(xs: mut list[int]) -> () {\n  xs.list_push(1)\n}\n",
        "pub fn f() -> () {\n  list_push(list[page](), page { url: \"u\" })\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
}

#[test]
fn linear_const_receivers_keep_their_linearity_proof() {
    // a `~Copyable` receiver cannot alias (copying is rejected), so a
    // const binding still feeds the mut window [D2, C1]
    assert_eq!(
        check("const l = link { slot: 1 }\nfn f() -> () {\n  tweak(l)\n}\n"),
        Vec::<String>::new(),
    );
    assert_eq!(
        check("fn f() -> () {\n  const l = link { slot: 1 }\n  tweak(l)\n}\n"),
        Vec::<String>::new(),
    );
}
