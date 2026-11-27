//! The nursery result channel's checker face: the block's value is the
//! collected answers' list, a `cancel` arm inside a spawned body never
//! answers (it drops out of the body's own unions), and a spawned body's
//! `mut` window over a binding outliving the nursery block refuses —
//! permanent and drive-independent (DS3008).

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag none\n",
    "pub enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "}\n",
    "pub struct page { url: string }\n",
    "pub struct list[t]\n",
    "pub extern fn list_len[t] (xs: list[t]) -> int\n",
    "pub extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "pub extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "pub extern fn cancel[t] (e: t) -> ()\n",
    "pub extern fn println (msg: string) -> ()\n",
    "pub trait Copyable { }\n",
    "pub struct link: ~Copyable { slot: int }\n",
    "pub extern fn tweak (l: mut link) -> ()\n",
);

/// Type-checks `source` beside the nursery ambient, rendering every
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
fn the_nursery_block_types_as_the_collected_answers_list() {
    // the element type is the union of the spawned bodies' answers, the
    // containment reads like any list value's, and a diverging-only
    // block keeps the unit element (the empty list's inhabitant)
    for source in [
        "pub fn f() -> list[int] {\n  nursery {\n    spawn { 1 }\n    spawn { 2 }\n  }\n}\n",
        "pub fn f() -> int {\n  nursery {\n    spawn { 1 }\n    spawn { \"s\" }\n  \
         }.list_len()\n}\n",
        "pub fn f() -> list[page] {\n  nursery {\n    for i in 0..3 {\n      spawn { page { url: \
         \"u\" } }\n    }\n  }\n}\n",
        "pub fn f() -> list[()] {\n  nursery {\n    spawn { cancel(net_err::timeout) }\n  }\n}\n",
        "pub fn f() -> list[()] {\n  nursery {\n    println(\"no spawns at all\")\n  }\n}\n",
    ] {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
    // a mismatched annotation refuses by containment, not by channel
    let mismatched = check("pub fn f() -> list[int] {\n  nursery {\n    spawn { \"s\" }\n  }\n}\n");
    assert_eq!(mismatched.len(), 1, "one containment refusal");
    assert!(
        mismatched[0].starts_with("DS2001") && mismatched[0].contains("`list[string]`"),
        "the refusal names the collected element union: {}",
        mismatched[0]
    );
}

#[test]
fn cancel_arms_inside_a_spawned_body_never_answer() {
    // the tutorial's fetch shape: the match's cancel arms contribute no
    // unit to the answer union, so the collected list is the pages
    // alone; outside a spawned body the same call keeps its declared
    // signature
    assert_eq!(
        check(concat!(
            "pub fn fetch(u: string) -> page | net_err {\n",
            "  if u == \"x\" {\n",
            "    net_err::timeout\n",
            "  } else {\n",
            "    page { url: u }\n",
            "  }\n",
            "}\n",
            "pub fn fetch_all(urls: list[string]) -> list[page] | net_err {\n",
            "  nursery {\n",
            "    for u in urls {\n",
            "      spawn {\n",
            "        match fetch(u) {\n",
            "          timeout => cancel(net_err::timeout)\n",
            "          refused => cancel(net_err::refused)\n",
            "          p => p\n",
            "        }\n",
            "      }\n",
            "    }\n",
            "  }\n",
            "}\n",
        )),
        Vec::<String>::new(),
    );
    // a spawn body that only cancels never answers: the element union
    // keeps the completing sibling's type alone
    assert_eq!(
        check(concat!(
            "pub fn f() -> list[int] {\n",
            "  nursery {\n",
            "    spawn { cancel(net_err::timeout) }\n",
            "    spawn { 1 }\n",
            "  }\n",
            "}\n",
        )),
        Vec::<String>::new(),
    );
}

#[test]
fn a_spawned_body_rejects_mut_windows_over_outlived_bindings() {
    // once a child can park on a world op the drive interleaves
    // siblings, so a mutation of a binding that survives the join has no
    // sequenced observer — the refusal permanent and drive-independent
    const REJECTED: &str = "DS3008 `xs` outlives this nursery block; a spawned body may never \
                            hold a `mut` window over it [E2]";
    // a function-scope var behind the block
    assert_eq!(
        check(
            "pub fn f() -> int {\n  var xs = [1]\n  nursery {\n    spawn {\n      \
             xs.list_push(2)\n    }\n  }\n  list_len(xs)\n}\n"
        ),
        [REJECTED],
    );
    // the free spelling, over a parameter (parameters outlive the block)
    assert_eq!(
        check(
            "pub fn fill(xs: mut list[int]) -> int {\n  nursery {\n    spawn {\n      \
             list_push(xs, 1)\n    }\n  }\n  list_len(xs)\n}\n"
        ),
        [REJECTED],
    );
    // a top-level var always outlives
    assert_eq!(
        check(
            "pub var xs = [1]\npub fn f() -> int {\n  nursery {\n    spawn {\n      \
             xs.list_push(2)\n    }\n  }\n  list_len(xs)\n}\n"
        ),
        [REJECTED],
    );
    // a mut-mode receiver over a linear binding outliving the block
    assert_eq!(
        check(
            "pub fn f() -> int {\n  var l = link { slot: 1 }\n  nursery {\n    spawn {\n      \
             tweak(l)\n    }\n  }\n  0\n}\n"
        ),
        [
            "DS3008 `l` outlives this nursery block; a spawned body may never hold a `mut` window \
             over it [E2]"
        ],
    );
    // an enclosing block's binding behind an inner spawn
    assert_eq!(
        check(
            "pub fn f() -> int {\n  var xs = [1]\n  if true {\n    nursery {\n      spawn {\n  \
             xs.list_push(2)\n      }\n    }\n  }\n  list_len(xs)\n}\n"
        ),
        [REJECTED],
    );
}

#[test]
fn block_introduced_bindings_keep_their_mut_windows() {
    // bindings the nursery block itself introduced die with it after
    // the join, so a spawned body's window over one stays legal — the
    // block-scoped sharing pattern the channel replaces only outside
    for source in
        [
            "pub fn f() -> int {\n  nursery {\n    var xs = [1]\n    spawn {\n      \
             xs.list_push(2)\n    }\n  }\n  0\n}\n",
            "pub fn f() -> int {\n  nursery {\n    const xs = [1]\n    spawn {\n      var ys = \
             xs\n      ys.list_push(2)\n    }\n  }\n  0\n}\n",
            // the parent's own statement inside the block sequences with the
            // spawns by construction, so it keeps its window
            "pub fn f() -> int {\n  var xs = [1]\n  nursery {\n    xs.list_push(2)\n    spawn { 1 \
             }\n  }\n  list_len(xs)\n}\n",
            // a spawn body's own binding, and a nested nursery's spawn over
            // the inner block's own binding
            "pub fn f() -> int {\n  nursery {\n    spawn {\n      var ys = [1]\n      \
             ys.list_push(2)\n    }\n  }\n  0\n}\n",
            "pub fn f() -> int {\n  nursery {\n    spawn {\n      nursery {\n        var xs = \
             [1]\n        spawn {\n          xs.list_push(2)\n        }\n      }\n    }\n  }\n  \
             0\n}\n",
        ]
    {
        assert_eq!(check(source), Vec::<String>::new(), "{source} checks clean");
    }
    // the outer block's binding DOES outlive the nested block (it dies
    // at the outer exit, past the inner join), so the inner spawn's
    // window over it refuses against the inner block
    assert_eq!(
        check(
            "pub fn f() -> int {\n  nursery {\n    var xs = [1]\n    spawn {\n      \
             nursery {\n        spawn {\n          xs.list_push(2)\n        }\n      }\n    \
             }\n  }\n  0\n}\n"
        ),
        ["DS3008 `xs` outlives this nursery block; a spawned body may never hold a `mut` window over it [E2]"],
    );
}
