//! The @std extraction family's local typing: `unwrap_or`'s eager
//! fallback meets the extracted type, `unwrap_or_else`'s zero-parameter
//! lambda checks against it, `ok_or` unions the new member in for `?` to
//! propagate, and `map_err` mirrors the `.err`/`.ok` handler rules —
//! plus the negatives: a receiver without the `none` member, a fallback
//! width meeting, wrong-arity and non-lambda spellings, and the disjoint
//! map_err handler.

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag missing\n",
    "pub tag bad_port\n",
    "pub extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "pub extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
    "pub extern fn unwrap_or_else[t] (x: t?, f: fn() -> t) -> t\n",
    "pub extern fn ok_or[t, e] (x: t?, err: e) -> t | e\n",
    "pub extern fn map_err[t, e, e2] (x: t | e, f: fn(e) -> e2) -> t | e2\n",
    "pub struct list[t]\n",
    "pub fn zero() -> int {\n",
    "  0\n",
    "}\n",
);

/// Type-checks `source` beside the family ambient, rendering every
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
fn unwrap_or_types_t_none_to_t_and_checks_eagerly() {
    // the extracted type is the receiver minus none, the fallback meets
    // it, and the plain-name spelling carries the same rule
    for source in [
        "pub const x = list_get([1, 2], 0)\nfn f() -> int {\n  x.unwrap_or(0)\n}\n",
        "fn f(present: bool) -> int {\n  const x = if present {\n    1\n  } else {\n    none\n  \
         }\n  x.unwrap_or(2)\n}\n",
        "pub fn f(x: int?) -> int {\n  unwrap_or(x, 3)\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
}

#[test]
fn a_receiver_without_the_none_member_is_an_error_naming_it() {
    // the task's sharp negative: `unwrap_or` over a plain `t`
    assert_eq!(check("fn f(x: int) -> int {\n  x.unwrap_or(0)\n}\n"), [
        "DS2001 `unwrap_or` extracts a `t | none` receiver; `int` carries no `none` member [E4]",
    ]);
    // the same rule over a string receiver
    assert_eq!(
        check("fn f(s: string) -> string {\n  s.unwrap_or(\"x\")\n}\n"),
        [
            "DS2001 `unwrap_or` extracts a `t | none` receiver; `string` carries no `none` member \
             [E4]",
        ]
    );
    // ok_or names the receiver too
    assert_eq!(
        check("fn f(x: bool) -> bool | missing {\n  x.ok_or(missing)\n}\n"),
        ["DS2001 `ok_or` extracts a `t | none` receiver; `bool` carries no `none` member [E4]",]
    );
}

#[test]
fn the_fallback_must_meet_the_extracted_type() {
    // the width meeting is diagnosed, not assumed through inference
    assert_eq!(
        check("fn f(x: int?) -> int {\n  x.unwrap_or(\"s\")\n}\n"),
        ["DS2001 the fallback `string` does not meet the extracted type `int` [E4]",]
    );
    // the lazy spelling checks the lambda's result against the same type
    assert_eq!(
        check("fn f(x: int?) -> int {\n  x.unwrap_or_else(() => \"s\")\n}\n"),
        ["DS2001 the fallback lambda's `string` does not meet the extracted type `int` [E4]",]
    );
}

#[test]
fn the_lazy_fallback_takes_a_zero_parameter_lambda() {
    // a named function reference and a parameter-taking lambda are both
    // refused: only a literal lambda inlines at the call site
    assert_eq!(
        check("fn f(x: int?) -> int {\n  x.unwrap_or_else(zero)\n}\n"),
        ["DS2001 `unwrap_or_else` takes a `fn() -> t` lambda that inlines at the call site"]
    );
    assert_eq!(
        check("fn f(x: int?) -> int {\n  x.unwrap_or_else(() => 1)\n}\n"),
        Vec::<String>::new()
    );
}

#[test]
fn ok_or_unions_the_new_member_for_the_propagation() {
    // the produced union is exactly t plus the error's members, and `?`
    // composes with it against the declared return
    for source in [
        "pub fn f(x: int?) -> int | missing {\n  x.ok_or(missing)?\n}\n",
        "pub fn f(x: int?) -> int {\n  x.ok_or(missing).err((_: missing) => 0)\n}\n",
        "pub fn f(x: int?) -> int | missing | bad_port {\n  x.ok_or(missing)?\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
    // a return that does not carry the new member is the containment
    // error of the `?`, not of ok_or itself
    assert_eq!(check("fn f(x: int?) -> int {\n  x.ok_or(missing)?\n}\n"), [
        "DS2008 `?` propagates `missing`, which the return union `int` does not contain [E4]"
    ]);
}

#[test]
fn map_err_follows_the_handler_rules() {
    // the annotated parameter handles, the remainder flows, and the
    // handler's result unions in
    for source in [
        "pub fn f(x: int | missing) -> int | bad_port {\n  x.map_err((e: missing) => \
         bad_port)\n}\n",
        "fn f(x: int | missing) -> int {\n  x.map_err((e: missing) => bad_port).err((_: bad_port) \
         => 0)\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
    // a handler disjoint from the receiver is the `.err`-style error
    assert_eq!(
        check(
            "fn f(x: int | missing) -> int | bad_port {\n  x.map_err((e: bad_port) => \
             bad_port)\n}\n"
        ),
        ["DS2001 `map_err` handles `bad_port`, which `int | missing` does not carry"]
    );
    // the arity and lambda spellings
    assert_eq!(
        check("fn f(x: int | missing) -> int {\n  x.map_err(0)\n}\n"),
        ["DS2001 `map_err` takes a one-parameter lambda `(e) => ...`"]
    );
}
