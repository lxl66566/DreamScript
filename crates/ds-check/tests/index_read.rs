//! The value-position subscript read: the receiver routes over the
//! ambient collections with the assignment place's own resolution — the
//! probe checks against the collection's parameter type (the list's
//! `int` index, the map's key) and the read answers the element — while
//! the checked `get` members keep their `t | none` spelling.

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub struct list[t]\n",
    "pub extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "pub struct map[k, v]\n",
    "pub extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
    "impl[t] list[t] {\n",
    "  fn get (self, i: int) -> t | none\n",
    "}\n",
    "impl[k, v] map[k, v] {\n",
    "  fn get (self, key: k) -> v | none\n",
    "}\n",
    "pub trait Copyable { }\n",
);

/// Type-checks `source` beside the collection ambient, rendering every
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
fn reads_type_over_the_collections_with_the_place_probe() {
    for source in [
        "pub const xs = [1, 2]\npub fn f() -> int {\n  xs[0]\n}\n",
        "pub fn f(xs: list[string]) -> string {\n  xs[0]\n}\n",
        // the probe expression checks against the collection's parameter
        // type, so a computed index composes
        "pub fn f(xs: list[int], i: int) -> int {\n  xs[i + 1]\n}\n",
        // a map read's probe is the key, the answer the value
        "pub fn f(m: map[string, int]) -> int {\n  m[\"a\"]\n}\n",
        // a nested read descends two collection applications
        "pub fn f(grid: list[list[int]]) -> int {\n  grid[0][1]\n}\n",
        // a read over a `mut list` receiver reads the same resolution
        "pub fn f(xs: mut list[int]) -> int {\n  xs[0]\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
}

#[test]
fn the_probe_reports_against_the_collection_parameter() {
    // a list's probe is the int index
    assert_eq!(
        check("pub fn f(xs: list[int]) -> int {\n  xs[\"a\"]\n}\n"),
        ["DS2001 `subscript probe` has type `string`, not contained in `int` [B2]"]
    );
    // a map's probe is its key
    assert_eq!(
        check("pub fn f(m: map[string, int]) -> int {\n  m[1]\n}\n"),
        ["DS2001 `subscript probe` has type `int`, not contained in `string` [B2]"]
    );
}

#[test]
fn a_non_collection_receiver_refuses_the_read() {
    assert_eq!(check("pub fn f(s: string) -> int {\n  s[0]\n}\n"), [
        "DS2001 a subscript read rides the ambient collections; `string` is not one"
    ]);
    // a union receiver has no single collection to route over
    assert_eq!(
        check("pub fn f(xs: list[int] | map[string, int]) -> int {\n  xs[0]\n}\n"),
        [
            "DS2001 a subscript read rides the ambient collections; `list[int] | map[string, \
             int]` is not one"
        ]
    );
}

#[test]
fn the_checked_get_members_keep_their_spelling() {
    // the read answers the element; `get` answers the union
    for source in [
        "pub const xs = [1, 2]\npub fn f() -> int | none {\n  xs.get(0)\n}\n",
        "pub fn f(m: map[string, int]) -> int | none {\n  m.get(\"a\")\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
    // the read answers the bare element, so it contains into the plain
    // element return, while `get`'s union does not
    assert_eq!(
        check("pub const xs = [1, 2]\npub fn f() -> int {\n  xs.get(0)\n}\n"),
        ["DS2001 `lambda body` has type `none | int`, not contained in `int` [B2]"]
    );
    assert_eq!(
        check("pub const xs = [1, 2]\npub fn f() -> int | none {\n  xs[0]\n}\n"),
        Vec::<String>::new(),
        "the read's bare element contains into the wider union"
    );
}

#[test]
fn the_place_store_keeps_its_own_resolution() {
    // the assignment spelling still routes the store path (a `var`
    // root, the `set` member's semantics), untouched by the read arm
    for source in [
        "pub fn f() -> int {\n  var xs = [1, 2]\n  xs[0] = 9\n  xs[0]\n}\n",
        "pub fn f() -> int {\n  var xs = [1, 2]\n  xs[0] += 9\n  xs[0]\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
    // a const root keeps the mut-window refusal on the store spelling
    assert!(
        check("pub fn f() -> int {\n  const xs = [1, 2]\n  xs[0] = 9\n  xs[0]\n}\n")
            .iter()
            .any(|diag| diag.contains("DS3006"))
    );
}
