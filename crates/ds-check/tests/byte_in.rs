//! The byte atom and the range-wired `in` honesty: `b'x'` literals type
//! through the `byte` atom — the stdlib ambient declares it, and a
//! self-contained package may declare its own — byte sits outside the int
//! conversion family (arithmetic, ordering, and range membership refuse),
//! and a non-range `in` right side is refused at the checker (`in` is
//! range-only; list membership spells the `xs.contains(x)` member call).

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

/// A self-contained package's atoms: `byte` is the package's own
/// declaration, the shape a package without the stdlib ambient uses.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "tag byte\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
);

/// Checks one source against the prelude, returning `CODE message` lines.
fn check(source: &str) -> Vec<String> {
    let combined = format!("{PRELUDE}{source}");
    let mut files = Files::new();
    let file = files.add_source("t.ds", &combined);
    let ast = parse(&files, file).unwrap_or_else(|diags| panic!("should parse: {diags:?}"));
    type_check(&files, &[ast])
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code.as_str(), d.message))
        .collect()
}

fn clean(source: &str) {
    let diags = check(source);
    assert!(
        diags.is_empty(),
        "expected clean, got:\n{}",
        diags.join("\n")
    );
}

/// Checks one source against the real stdlib ambient alone: the ambient
/// is a package member registered before the user's sources, so no user
/// declaration may repeat its names in the same namespace.
fn check_ambient(source: &str) -> Vec<String> {
    let mut files = Files::new();
    let amb = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let file = files.add_source("t.ds", source);
    let asts = [
        parse(&files, amb).unwrap_or_else(|d| panic!("ambient parses: {d:?}")),
        parse(&files, file).unwrap_or_else(|d| panic!("should parse: {d:?}")),
    ];
    type_check(&files, &asts)
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code.as_str(), d.message))
        .collect()
}

#[test]
fn byte_literals_type_through_a_declared_byte_atom() {
    clean("const sep = b'x'\n");
    clean("fn f() -> byte { b'x' }\n");
    clean("fn f(b: byte) -> byte { b }\n");
}

#[test]
fn a_byte_literal_without_a_byte_atom_names_it() {
    // Inference never falls back: the atom must be declared, by
    // the ambient or by the package itself.
    let mut files = Files::new();
    let file = files.add_source("t.ds", "const sep = b'x'\n");
    let ast = parse(&files, file).unwrap();
    let diags = type_check(&files, &[ast])
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code.as_str(), d.message))
        .collect::<Vec<_>>();
    assert_eq!(diags, [
        "DS2002 `byte literal` needs a declared `byte` type; inference never falls back [B1, B7]"
    ]);
}

#[test]
fn the_ambient_declares_byte() {
    let diags = check_ambient("const sep = b'\\n'\nfn f() -> byte { sep }\n");
    assert!(
        diags.is_empty(),
        "expected clean, got:\n{}",
        diags.join("\n")
    );
    // A user redeclaration of the ambient's label in the type namespace
    // is the cross-file tag shadow: a distinct atom, its own
    // file's uses reading it first, the refusal only within one file.
    let diags = check_ambient("tag byte\n");
    assert!(
        diags.is_empty(),
        "the shadow declares clean, got:\n{}",
        diags.join("\n")
    );
    let diags = check_ambient("tag byte\ntag byte\n");
    assert_eq!(
        diags.first().map(String::as_str),
        Some("DS2005 `byte` is already defined")
    );
}

#[test]
fn byte_sits_outside_the_int_conversion_family() {
    // No implicit widening exists: every operator family refuses a
    // byte operand instead of converting it. The first diagnostic is the
    // refusal; the empty-typed result may cascade one inference note.
    assert_eq!(
        check("fn f(b: byte) -> () { const x = b + 1 }\n")
            .first()
            .map(String::as_str),
        Some("DS2001 arithmetic needs numeric operands, `byte` and `int` given [B7]")
    );
    assert_eq!(
        check("fn f(b: byte) -> () { const x = b + b'x' }\n")
            .first()
            .map(String::as_str),
        Some("DS2001 arithmetic needs numeric operands, `byte` and `byte` given [B7]")
    );
    assert_eq!(
        check("fn f(b: byte) -> bool { b < 1 }\n")
            .first()
            .map(String::as_str),
        Some(
            "DS2001 ordering needs one numeric type on both sides (`byte` vs `int`); `ord` \
             derives arrive at M6 [B9, F5]"
        )
    );
    assert_eq!(
        check("fn f(b: byte) -> bool { b in 0..255 }\n")
            .first()
            .map(String::as_str),
        Some(
            "DS2001 range membership needs the range's integer type, `byte` given; conversions \
             are explicit [B7]: `int(x)`"
        )
    );
}

#[test]
fn non_list_in_right_sides_refuse_at_the_checker() {
    const MSG: &str =
        "DS2017 `in` takes a bare range (`x in a..b`); list membership is `xs.contains(x)`";
    assert_eq!(
        check("fn f(m: map[string, int]) -> bool { \"k\" in m }\n"),
        [MSG]
    );
    assert_eq!(check("fn f(s: string) -> bool { \"x\" in s }\n"), [MSG]);
    assert_eq!(check("fn f(n: int) -> bool { 1 in n }\n"), [MSG]);
}

/// List membership rides the member call spelling: `in` over a list
/// refuses like every other non-range right side, and the member call
/// checks clean against the real ambient's member row.
#[test]
fn list_membership_refuses_and_contains_checks_clean() {
    const MSG: &str =
        "DS2017 `in` takes a bare range (`x in a..b`); list membership is `xs.contains(x)`";
    assert_eq!(check("fn f(xs: list[int]) -> bool { 1 in xs }\n"), [MSG]);
    assert_eq!(check("fn f(ws: list[string]) -> bool { \"k\" in ws }\n"), [
        MSG
    ]);
    assert_eq!(
        check("fn f(xs: list[int], n: int) -> bool { n in xs && n > 0 }\n"),
        [MSG]
    );
    assert_eq!(check("fn f(ws: list[string]) -> bool { 1 in ws }\n"), [MSG]);
    let diags = check_ambient("fn f(xs: list[int]) -> bool { xs.contains(1) }\n");
    assert!(
        diags.is_empty(),
        "expected clean, got:\n{}",
        diags.join("\n")
    );
}

#[test]
fn range_in_membership_stays_clean() {
    clean("fn f(x: int, n: int) -> bool { x in 1..n }\n");
    clean("fn f(x: int) -> bool { x in 1..=5 }\n");
    clean("fn f(x: int, n: int) -> bool { x in (1..n) }\n");
    clean("fn f(x: int, n: int) -> bool { x in 1..n && x > 0 }\n");
}
