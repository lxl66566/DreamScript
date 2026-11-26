//! The `map[k, v]()` constructor surface and `map_set`'s mut-window
//! exclusivity: builder-call construction through `var`
//! receivers, the const-receiver rejection in both call spellings, and
//! the checked reads over the hit and miss members.

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub struct map[k, v]\n",
    "pub extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
    "pub extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none\n",
    "pub extern fn map_len[k, v] (m: map[k, v]) -> int\n",
);

/// Type-checks `source` beside the map ambient, rendering every
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
fn map_set_builds_through_var_receivers() {
    // the builder-call interim: map_set fills a var receiver in both
    // spellings, and the checked read splits over the hit and miss
    for source in [
        "fn f() -> int {\n  var m = map[string, string]()\n  map_set(m, \"host\", \
         \"localhost\")\n  map_len(m)\n}\n",
        "pub fn f() -> int {\n  var m = map[int, string]()\n  m.map_set(1, \"one\")\n  \
         map_len(m)\n}\n",
        "pub fn f(m: mut map[string, string]) -> () {\n  map_set(m, \"host\", \"localhost\")\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
}

#[test]
fn map_set_rejects_const_receivers() {
    // a const binding may alias its shared heap word, so the mut window
    // rejects it in both spellings
    const REJECTED: &str =
        "DS3006 `m` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]";
    assert_eq!(
        check(
            "fn f() -> int {\n  const m = map[string, string]()\n  map_set(m, \"host\", \
             \"localhost\")\n  map_len(m)\n}\n"
        ),
        [REJECTED],
    );
    assert_eq!(
        check(
            "fn f() -> int {\n  const m = map[string, string]()\n  m.map_set(\"host\", \
             \"localhost\")\n  map_len(m)\n}\n"
        ),
        [REJECTED],
    );
    // the default borrow-mode parameter rejects too: a borrow is as
    // immutable as a const binding
    assert_eq!(
        check(
            "fn g(m: map[string, string]) -> int {\n  map_set(m, \"host\", \"localhost\")\n  \
             map_len(m)\n}\n"
        ),
        [REJECTED],
    );
}

#[test]
fn map_reads_stay_borrowed() {
    // reads take no window: a const receiver reads clean
    assert_eq!(
        check(
            "fn f() -> string | none {\n  const m = map[string, string]()\n  map_get(m, \
             \"host\")\n}\n"
        ),
        Vec::<String>::new(),
    );
}
