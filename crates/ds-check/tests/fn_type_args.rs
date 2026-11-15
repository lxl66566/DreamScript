//! The fn type as a constructor's bracketed type argument [F2, F9]:
//! `list[fn(int) -> int]()` and `map[string, fn(int) -> int]()` parse,
//! solve, and type the element story — the fn-typed binding takes a named
//! fn value, elements push, and read-back narrows to a callable binding.
//! The element *runtime* story runs for named fn values and
//! non-capturing lambdas; a closure element stays refused at the
//! callable-word seam (a capturing value is no scalar FuncRef).

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub struct list[t]\n",
    "pub struct map[k, v]\n",
    "pub extern fn list_len[t] (xs: list[t]) -> int\n",
    "pub extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "pub extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "pub extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
    "pub extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none\n",
);

/// Type-checks `source` beside the ambient, rendering every diagnostic as
/// one `CODE message` line.
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
fn fn_typed_arguments_construct_and_inhabit_bindings() {
    for source in [
        // the empty constructors carry the fn-typed element/value
        "pub fn inc(x: int) -> int { x + 1 }\nconst fns = list[fn(int) -> int]()\n",
        "pub const m = map[string, fn(int) -> int]()\n",
        // nesting composes through the inner application
        "pub const n = list[list[fn() -> int]]()\n",
        // a named fn value inhabits the fn-typed binding
        "pub fn inc(x: int) -> int { x + 1 }\nconst g: fn(int) -> int = inc\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
}

#[test]
fn fn_typed_elements_push_and_read_back() {
    for source in [
        // a named fn element and a non-capturing lambda element (the
        // lambda/closure lowering boundary is owned by the callable-word
        // seam, not the checker)
        "fn inc(x: int) -> int { x + 1 }\nvar fns = list[fn(int) -> int]()\nfn use() -> int {\n  \
         fns.list_push(inc)\n  fns.list_push((x: int) => x + 1)\n  list_len(fns)\n}\n",
        // read-back: list_get's member carries the fn type, `is` narrows,
        // and the call goes through the narrowed fn-typed binding
        "fn inc(x: int) -> int { x + 1 }\nvar fns = list[fn(int) -> int]()\nfn use() -> int {\n  \
         fns.list_push(inc)\n  const m = fns.list_get(0)\n  if m is fn(int) -> int { m(1) } else \
         { 0 }\n}\n",
        // the map's fn-typed value pushes beside the string key
        "fn inc(x: int) -> int { x + 1 }\nvar m = map[string, fn(int) -> int]()\nfn use() -> int \
         {\n  m.map_set(\"k\", inc)\n  0\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
}

#[test]
fn bracketed_fn_types_refuse_at_their_own_spans() {
    // an unsolvable fn type reports at the fn type's own span
    assert_eq!(check("const bad = list[fn(bogus) -> int]()\n"), [
        "DS2001 the type argument `fn(bogus) -> int` does not solve against the package \
         declarations"
    ],);
    // a value in the brackets stays the cross-spelling error
    assert_eq!(
        check("const x = 1\nconst bad = list[x]()\n")
            .iter()
            .filter(|line| line.contains("does not solve"))
            .count(),
        1,
    );
    // a fn type by itself never constructs: the application outside a call
    // is the same refusal as every other type application
    assert_eq!(check("const bad = list[fn(int) -> int]\n"), [
        "DS2001 `list` is a type; the application `list[...]` constructs only in call position — \
         `list[t]()` [F9, B3]"
    ],);
}
