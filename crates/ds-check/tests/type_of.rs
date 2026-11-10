//! The `type_of` pass over focused sources [B2, B4, B7, B9, E4, F3, F4]:
//! boundary anchoring, monotone refinement with assignment reset, the E4
//! union-error tokens, B7 numeric table, B9 equality, method-candidate
//! selection, and exhaustiveness by set difference.

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

/// Primitive atoms and one struct every case can share.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag bool\n",
    "tag char\n",
    "tag io_err\n",
    "struct config { host: string, port: int }\n",
    "struct list[t]\n",
    "extern fn str[t] (v: t) -> string\n",
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

#[test]
fn boundary_annotation_checks_by_containment() {
    // The written signature anchors the algebra [B2]: the body's final
    // expression must be contained in it.
    clean("fn f(x: int) -> int { x }\n");
    assert_eq!(check("fn f(x: int) -> string { x }\n"), ["DS2001 `lambda \
                                                          body` has type \
                                                          `int`, not \
                                                          contained in \
                                                          `string` [B2]"]);
}

#[test]
fn boundary_parameters_require_annotations() {
    // Inference failure is a compile error; locals infer, boundaries do
    // not [B2, B1]. A top-level `fn` converts onto the const-declaration
    // node, so it carries the lambda's message; a trait member keeps the
    // boundary wording.
    assert_eq!(check("fn f(x) -> int { 1 }\n"), ["DS2002 parameter `x` \
                                                  has no expected type \
                                                  to infer from; \
                                                  annotate it [B2]"]);
    assert_eq!(check("trait t {\n  fn g(x) -> int\n}\n"), [
        "DS2002 parameter `x` needs a type annotation at this boundary [B2]"
    ]);
}

#[test]
fn is_narrows_the_then_branch() {
    clean("fn f(x: int | string) -> int {\n  if x is int { x + 1 } else { 0 }\n}\n");
    // Without the test, the union does not support arithmetic.
    assert_eq!(check("fn f(x: int | string) -> int { x + 1 }\n"), [
        "DS2001 arithmetic needs numeric operands, `int | string` and `int` given [B7]"
    ]);
}

#[test]
fn and_right_operand_sees_the_left_narrowing() {
    // The record's own condition shape: `x is int && x >= 1` [B2].
    clean("fn f(x: int | string) -> bool {\n  if x is int && x >= 1 { true } else { false }\n}\n");
}

#[test]
fn and_chain_else_branch_refines_nothing() {
    // The then branch sees every conjunct's narrowing; the else branch
    // runs on ¬(a∧b) = ¬a∨¬b, which guarantees no single conjunct false,
    // so an `&&` chain narrows nothing there [B2].
    clean(concat!(
        "fn f(x: int | string, y: int | string) -> int {\n",
        "  if x is int && y is string { x + 1 } else { 0 }\n",
        "}\n",
    ));
    // `x` in else stays `int | string`: else also runs when x is int and
    // only y failed the test.
    assert_eq!(
        check(concat!(
            "fn f(x: int | string, y: int | string) -> string {\n",
            "  if x is int && y is string { \"both\" } else { x }\n",
            "}\n",
        )),
        ["DS2001 `lambda body` has type `int | string`, not contained in `string` [B2]"]
    );
    // A single conjunct still inverts soundly: ¬a is exactly the else.
    clean("fn f(x: int | none) -> int {\n  if x is none { 0 } else { x + 1 }\n}\n");
}

#[test]
fn and_chain_of_three_conjuncts_leaves_else_alone() {
    clean(concat!(
        "fn f(x: int | string, y: int | string, z: int | string) -> int {\n",
        "  if x is int && y is int && z is int { x + y + z } else { 0 }\n",
        "}\n",
    ));
    assert_eq!(
        check(concat!(
            "fn f(x: int | string, y: int | string, z: int | string) -> string {\n",
            "  if x is int && y is int && z is int { \"all\" } else { x }\n",
            "}\n",
        )),
        ["DS2001 `lambda body` has type `int | string`, not contained in `string` [B2]"]
    );
}

#[test]
fn forward_referenced_top_level_binding_keeps_the_return_union() {
    // Typing a later-declared top-level binding mid-body [G3] must not
    // clobber the enclosing function's return union: `?` and `return`
    // after the reference still see it.
    clean(concat!(
        "fn uses_try(x: int | io_err | string) -> int | io_err {\n",
        "  later + 1\n",
        "  x?\n",
        "  1\n",
        "}\n",
        "fn uses_return(x: int | string) -> int {\n",
        "  later + 1\n",
        "  if x is string { return 0 }\n",
        "  x + 1\n",
        "}\n",
        "const later = 3\n",
    ));
}

#[test]
fn none_tests_narrow_both_branches() {
    clean("fn f(x: int | none) -> int {\n  if x != none { x + 1 } else { 0 }\n}\n");
    // The inverse: `== none` keeps none in the then branch.
    clean("fn f(x: int | none) -> int {\n  if x == none { 0 } else { x + 1 }\n}\n");
}

#[test]
fn refinement_resets_at_assignment() {
    // `var` is the only rebindable binding; assignment resets refinement
    // to the declared type [B2, C3].
    clean(concat!(
        "fn f(n: int | none) -> int {\n",
        "  var m: int | none = n\n",
        "  if m is none { m = 1 } else { m = 2 }\n",
        "  3\n",
        "}\n",
    ));
    // After the reset the union is back: arithmetic on it is an error.
    assert_eq!(
        check(concat!(
            "fn f(n: int | none) -> int {\n",
            "  var m: int | none = n\n",
            "  if m is none { m = 1 } else { m = 2 }\n",
            "  m + 1\n",
            "}\n",
        ))
        .last()
        .map(String::as_str),
        Some("DS2001 arithmetic needs numeric operands, `none | int` and `int` given [B7]")
    );
}

#[test]
fn match_arms_type_over_the_remainder() {
    // Sequential set semantics: the second arm binds what the first left
    // [F4]; the annotated first arm is a narrowing filter.
    clean(concat!(
        "fn f(x: int | string | bool) -> int {\n",
        "  match x {\n",
        "    s: string => 0,\n",
        "    i: int    => i + 1,\n",
        "    _         => 2,\n",
        "  }\n",
        "}\n",
    ));
}

#[test]
fn match_guards_narrow_the_arm_body() {
    clean(concat!(
        "fn f(x: int | string) -> int {\n",
        "  match x {\n",
        "    n if n is int => n + 1,\n",
        "    _             => 0,\n",
        "  }\n",
        "}\n",
    ));
}

#[test]
fn literal_arm_keeps_the_scrutinee_type() {
    // A literal arm covers no member atom, so it refines nothing: the
    // scrutinee binder keeps its type inside the arm body.
    clean(concat!(
        "fn f(x: int) -> int {\n",
        "  match x {\n",
        "    1 => x + 1,\n",
        "    _ => 0,\n",
        "  }\n",
        "}\n",
    ));
}

#[test]
fn match_arm_pattern_errors_report_once() {
    // The arm's pattern walks once (coverage and binders from the same
    // walk), so a pattern error is not double-reported.
    assert_eq!(
        check(concat!(
            "fn f(x: int | string) -> int {\n",
            "  match x {\n",
            "    1.5 => 0,\n",
            "    _   => 1,\n",
            "  }\n",
            "}\n",
        )),
        ["DS2001 a `float` literal cannot match `int | string`"]
    );
}

#[test]
fn non_exhaustive_match_names_the_missing_set() {
    assert_eq!(
        check(concat!(
            "tag timeout\n",
            "tag refused\n",
            "fn f(x: io_err | timeout | refused) -> int {\n",
            "  match x {\n",
            "    e if e is io_err => 0,\n",
            "  }\n",
            "}\n",
        ))
        .last()
        .map(String::as_str),
        Some(
            "DS2003 match on `io_err | timeout | refused` is not exhaustive: missing: `io_err | \
             timeout | refused`"
        )
    );
    // A guarded arm proves nothing; an unguarded arm covers.
    assert_eq!(
        check(concat!(
            "tag timeout\n",
            "tag refused\n",
            "fn f(x: io_err | timeout | refused) -> int {\n",
            "  match x {\n",
            "    e if e is io_err => 0,\n",
            "    e if e is timeout => 1,\n",
            "    _                 => 2,\n",
            "  }\n",
            "}\n",
        ))
        .last()
        .map(String::as_str),
        None
    );
}

#[test]
fn bare_tag_arms_are_member_tests() {
    // A bare unit-tag arm names a member test [F9]: it credits
    // exhaustiveness, dispatches by value, and binds nothing.
    clean(concat!(
        "tag on\n",
        "tag off\n",
        "fn f(s: on | off) -> string {\n",
        "  match s {\n",
        "    on => \"a\",\n",
        "    off => \"b\",\n",
        "  }\n",
        "}\n",
    ));
    // A missing tag arm names the missing member.
    assert_eq!(
        check(concat!(
            "tag on\n",
            "tag off\n",
            "fn f(s: on | off) -> string {\n",
            "  match s {\n",
            "    on => \"a\",\n",
            "  }\n",
            "}\n",
        ))
        .last()
        .map(String::as_str),
        Some("DS2003 match on `on | off` is not exhaustive: missing: `off`")
    );
    // A tag the scrutinee does not carry is an error, not a binding.
    assert_eq!(
        check(concat!(
            "tag on\n",
            "fn f(x: int | io_err) -> int {\n",
            "  match x {\n",
            "    on => 1,\n",
            "    _   => 2,\n",
            "  }\n",
            "}\n",
        ))
        .last()
        .map(String::as_str),
        Some("DS2001 pattern `on` is not a member of `int | io_err`")
    );
}

#[test]
fn tag_arms_win_over_same_named_locals_and_declarations_still_bind() {
    // The type-namespace entry wins in test positions: the local `on`
    // cannot capture the arm, and the match still exhausts by tags.
    clean(concat!(
        "tag on\n",
        "tag off\n",
        "fn f(s: on | off, on: int) -> string {\n",
        "  match s {\n",
        "    on => \"a\",\n",
        "    off => \"b\",\n",
        "  }\n",
        "}\n",
    ));
    // Declaration positions bind as ever: a local may share a tag's name.
    clean(concat!(
        "tag on\n",
        "fn f() -> int {\n",
        "  const on = 3\n",
        "  on\n",
        "}\n",
    ));
}

#[test]
fn or_patterns_of_unit_tags_form_one_member_set() {
    // Alternatives that are both tag tests bind no names, so the
    // or-pattern is one member set, not an inconsistent-binding error.
    clean(concat!(
        "tag timeout\n",
        "tag refused\n",
        "tag ready\n",
        "fn f(e: timeout | refused | ready) -> int {\n",
        "  match e {\n",
        "    timeout | refused => 1,\n",
        "    ready => 2,\n",
        "  }\n",
        "}\n",
    ));
    // Mixing a tag test with a binder still binds different names.
    assert_eq!(
        check(concat!(
            "tag timeout\n",
            "tag refused\n",
            "fn f(e: timeout | refused) -> int {\n",
            "  match e {\n",
            "    timeout | other => 1,\n",
            "    refused => 2,\n",
            "  }\n",
            "}\n",
        ))
        .first()
        .map(String::as_str),
        Some("DS2001 or-pattern alternatives bind different names [F4]")
    );
}

#[test]
fn try_propagation_checks_containment() {
    // The ok part survives, the error members must be contained [E4]:
    // at an expected position the split runs by the expectation, and
    // every propagated member must be carried by the return union.
    clean("fn f(x: int | io_err) -> string | io_err { str(x?) }\n");
    assert_eq!(
        check(concat!(
            "fn f(x: int | io_err) -> config {\n",
            "  config { host: \"h\", port: x? }\n",
            "}\n",
        ))
        .first()
        .map(String::as_str),
        Some(
            "DS2008 `?` propagates `io_err`, which the return union `config` does not contain [E4]"
        )
    );
}

#[test]
fn tail_try_over_a_contained_union_is_the_return() {
    // E4's canonical shape: the final-expression `?` splits against the
    // return union, so a fully-contained operand yields itself — the
    // expression IS the return [E4].
    clean(concat!(
        "tag parse_err\n",
        "extern fn read_file (path: string) -> string | io_err\n",
        "extern fn parse_config (text: string) -> config | parse_err\n",
        "fn load_config(path: string) -> config | io_err | parse_err {\n",
        "  const text = read_file(path)?\n",
        "  parse_config(text)?\n",
        "}\n",
    ));
    // An operand sharing no member with the return union still leaves
    // nothing.
    assert_eq!(
        check(concat!(
            "extern fn read_file (path: string) -> string | io_err\n",
            "fn f(path: string) -> config {\n",
            "  read_file(path)?\n",
            "}\n",
        ))
        .last()
        .map(String::as_str),
        Some("DS2002 `?` leaves nothing: every member of `string | io_err` propagates [E4]")
    );
    // A member outside the return union cannot propagate through the tail.
    assert_eq!(
        check(concat!(
            "extern fn read_file (path: string) -> string | io_err\n",
            "fn f() -> config | io_err {\n",
            "  read_file(\"x\")?\n",
            "}\n",
        ))
        .last()
        .map(String::as_str),
        Some(
            "DS2008 `?` propagates `string`, which the return union `io_err | config` does not \
             contain [E4]"
        )
    );
}

#[test]
fn mid_block_return_try_over_a_contained_union_is_the_return() {
    // A statement-position `return e?` types its operand against the
    // declared return, exactly like the final-expression shape: a
    // fully-contained union keeps itself instead of reporting an empty
    // success part [E4].
    clean(concat!(
        "extern fn read_file (path: string) -> config | io_err\n",
        "fn load_config(path: string) -> config | io_err {\n",
        "  if path == \"x\" {\n",
        "    return read_file(path)?\n",
        "  }\n",
        "  read_file(path)?\n",
        "}\n",
    ));
    // A member outside the return union still cannot propagate.
    assert_eq!(
        check(concat!(
            "extern fn read_file (path: string) -> string | io_err\n",
            "fn f(path: string) -> config | io_err {\n",
            "  if path == \"x\" {\n",
            "    return read_file(path)?\n",
            "  }\n",
            "  read_file(path)?\n",
            "}\n",
        ))
        .first()
        .map(String::as_str),
        Some(
            "DS2008 `?` propagates `string`, which the return union `io_err | config` does not \
             contain [E4]"
        )
    );
}

#[test]
fn branch_final_try_splits_against_the_expected_return() {
    // The final-expression anchor threads through if and match branch
    // finals, so a fully-contained union keeps itself in every branch
    // [E4].
    clean(concat!(
        "extern fn read_file (path: string) -> config | io_err\n",
        "fn load(path: string) -> config | io_err {\n",
        "  if path == \"x\" {\n",
        "    read_file(path)?\n",
        "  } else {\n",
        "    read_file(path)?\n",
        "  }\n",
        "}\n",
    ));
    clean(concat!(
        "tag on\n",
        "tag off\n",
        "extern fn read_file (path: string) -> config | io_err\n",
        "fn load(g: on | off, path: string) -> config | io_err {\n",
        "  match g {\n",
        "    on => read_file(path)?,\n",
        "    off => read_file(path)?,\n",
        "  }\n",
        "}\n",
    ));
    // A member outside the return union still cannot propagate through a
    // branch final.
    assert_eq!(
        check(concat!(
            "extern fn read_file (path: string) -> string | io_err\n",
            "fn f(path: string) -> config | io_err {\n",
            "  if path == \"x\" {\n",
            "    read_file(path)?\n",
            "  } else {\n",
            "    read_file(path)?\n",
            "  }\n",
            "}\n",
        ))
        .first()
        .map(String::as_str),
        Some(
            "DS2008 `?` propagates `string`, which the return union `io_err | config` does not \
             contain [E4]"
        )
    );
}

#[test]
fn for_over_a_range_binds_the_element_and_gates_endpoints() {
    // The range's element type is the endpoints' shared integer width;
    // the body's binder carries it [F7].
    clean(concat!(
        "fn f() -> int {\n",
        "  var total: int = 0\n",
        "  for i in 1..5 {\n",
        "    total += i\n",
        "  }\n",
        "  total\n",
        "}\n",
    ));
    clean(concat!(
        "fn f(x: int, n: int) -> bool {\n",
        "  for i in x..n { }\n",
        "  x in 0..n\n",
        "}\n",
    ));
    // Float endpoints are rejected naming the conversion discipline [B7].
    assert_eq!(
        check("fn f() -> () { for i in 1.0..5.0 { } }\n")
            .first()
            .map(String::as_str),
        Some(
            "DS2001 range endpoints are integers, `float` and `float` given; conversions are \
             explicit [B7]"
        )
    );
    // Mixed integer widths name the conversion, like arithmetic [B7].
    assert_eq!(
        check(concat!(
            "tag u32\n",
            "fn f(a: u32, b: int) -> () {\n",
            "  for i in a..b { }\n",
            "}\n",
        ))
        .first()
        .map(String::as_str),
        Some(
            "DS2001 mixed integer widths `u32` and `int`; conversions are explicit [B7]: `u32(x)`"
        )
    );
    // Non-numeric endpoints never iterate.
    assert_eq!(
        check("fn f(s: string) -> () { for i in s..5 { } }\n")
            .first()
            .map(String::as_str),
        Some(
            "DS2001 range endpoints are integers, `string` and `int` given; conversions are \
             explicit [B7]"
        )
    );
}

#[test]
fn range_membership_checks_against_the_element_type() {
    // `x in a..b` is bool-typed against the range's element type [F7].
    clean("fn f(x: int) -> bool { x in 1..=5 }\n");
    assert_eq!(
        check("fn f(x: float) -> bool { x in 1..5 }\n")
            .first()
            .map(String::as_str),
        Some(
            "DS2001 range membership needs the range's integer type, `float` given; conversions \
             are explicit [B7]: `int(x)`"
        )
    );
    // A range itself is not a storable value: only `for`-in and `in`
    // consume it [F7].
    assert_eq!(
        check("fn f() -> () { const r = 1..5 }\n")
            .first()
            .map(String::as_str),
        Some("DS2002 a range is not a storable value; ranges are `for`-in and `in` sugar [F7]")
    );
}

#[test]
fn err_handler_is_difference_typed() {
    // (config | io_err) - io_err == config, so the `==` against config is
    // well-typed [E4, B9].
    clean(concat!(
        "extern fn load () -> config | io_err\n",
        "extern fn default_config () -> config\n",
        "const cfg = load().err((e) => {\n",
        "  default_config()\n",
        "})\n",
        "const same = cfg == default_config()\n",
    ));
    // A handler whose parameter is disjoint from the operand is an error.
    assert_eq!(
        check(concat!(
            "extern fn load () -> config | io_err\n",
            "extern fn default_config () -> config\n",
            "const cfg = load().err((e: int) => { default_config() })\n",
        ))
        .last()
        .map(String::as_str),
        Some("DS2001 the handler's `err` handles `int`, which `io_err | config` does not carry")
    );
}

#[test]
fn numeric_table_int_float_split() {
    clean("fn f(a: int, b: int) -> int { a +% b }\n");
    clean("fn f(a: float, b: float) -> float { a + b }\n");
    // Wrapping operators are integer-only [B7].
    assert_eq!(check("fn f(a: float, b: float) -> float { a +% b }\n"), [
        "DS2001 wrapping operators are integer-only; floats are IEEE [B7]"
    ]);
    // Mixed int/float arithmetic suggests the explicit conversion [B7].
    let diags = check("fn f(a: int, b: float) -> float { a + b }\n");
    assert_eq!(diags.as_slice(), ["DS2001 mixed numeric operands `int` \
                                   and `float`; conversions are explicit \
                                   [B7]"]);
    // Suffixes name their atoms: unsuffixed is int, `f32` needs the atom.
    clean("fn f() -> int { 1 + 2 }\n");
    assert_eq!(check("fn f() -> float { 1.5f32 }\n"), ["DS2002 `float \
                                                        literal suffix` \
                                                        needs a declared \
                                                        `f32` type; \
                                                        inference never \
                                                        falls back [B1, \
                                                        B7]"]);
}

#[test]
fn byte_literals_type_through_a_declared_byte_atom() {
    // The record pins `b'x'` as a single byte [F6] but names no byte
    // type, so the literal types through the declared-atom convention
    // like any width suffix: inference never falls back [B7], and a
    // declared `byte` is a distinct atom disjoint from `int` [B1].
    assert_eq!(check("fn f() -> () { const b: int = b'x' }\n"), [
        "DS2002 `byte literal` needs a declared `byte` type; inference never falls back [B1, B7]"
    ]);
    clean("tag byte\nfn f() -> () { const b: byte = b'x' }\n");
    assert_eq!(check("tag byte\nfn f() -> () { const n: int = b'x' }\n"), [
        "DS2001 `annotated binding` has type `byte`, not contained in `int` [B2]"
    ]);
}

#[test]
fn structural_equality_carve_outs() {
    clean("fn f(a: int, b: int) -> bool { a == b }\n");
    clean("const same = config { host: \"h\", port: 1 } == config { host: \"h\", port: 1 }\n");
    // Disjoint operands make `==` constant: rejected [B9].
    assert_eq!(check("fn f(a: int, b: string) -> bool { a == b }\n"), [
        "DS2001 `int` and `string` share no member; `==` would be constant [B9]"
    ]);
    // Function comparison is rejected statically [B9 carve-out 1].
    assert_eq!(check("fn f(g: fn(int) -> int) -> bool { g == g }\n"), [
        "DS2001 function comparison is rejected [B9 carve-out 1]"
    ]);
}

#[test]
fn interpolation_holes_convert_printable_primitives() {
    // F6: a hole is a string context; the printable primitives convert
    // through str(e) implicitly, in both hole spellings
    clean("const s = \"count: ${1}\"\n");
    clean("const s = \"${1.5} ${true} ${'x'}\"\n");
    clean("fn f(x: int, s: string) -> string { \"v=${x} w=$s\" }\n");
    clean("const s = \"${str(1)}\"\n");
}

#[test]
fn interpolation_rejects_unprintable_and_union_holes() {
    // a struct operand waits for the M6 show derive
    assert_eq!(
        check(concat!(
            "const c = config { host: \"h\", port: 1 }\n",
            "const s = \"${c}\"\n",
        )),
        [
            "DS2001 interpolation hole has type `config`; the printable primitives (bool, char, \
             float, int, string) convert implicitly [F6]"
        ]
    );
    // a union operand extracts first
    assert_eq!(check("fn f(x: int | io_err) -> string { \"${x}\" }\n"), [
        "DS2001 interpolation hole has type `int | io_err`; the printable primitives (bool, char, \
         float, int, string) convert implicitly [F6]"
    ]);
}

#[test]
fn bare_ident_hole_resolves_and_reports() {
    clean("fn f(x: int) -> string { \"v=$x\" }\n");
    assert_eq!(check("const s = \"v=$missing\"\n"), [
        "DS2004 unknown name `missing`",
        "DS2002 `missing` does not resolve to a value"
    ]);
}

#[test]
fn method_selection_through_instances_and_assumptions() {
    // A concrete receiver resolves through the instance environment [B6].
    clean(concat!(
        "trait show {\n",
        "  fn text(self) -> string\n",
        "}\n",
        "impl show for int {\n",
        "  fn text(self) -> string { str(self) }\n",
        "}\n",
        "fn f(x: int) -> string { x.text() }\n",
    ));
    // No instance: an unsatisfied constraint, not any fallback.
    assert_eq!(
        check(concat!(
            "trait show {\n",
            "  fn text(self) -> string\n",
            "}\n",
            "fn f(x: int) -> string { x.text() }\n",
        ))
        .last()
        .map(String::as_str),
        Some("DS2013 no instance of `show` for `int`")
    );
    // A generic bound carries the assumption into the body [B6].
    clean(concat!(
        "trait show {\n",
        "  fn text(self) -> string\n",
        "}\n",
        "fn shown[t: show](x: t) -> string { x.text() }\n",
    ));
    // Two traits, both satisfied: true ambiguity [F3].
    assert_eq!(
        check(concat!(
            "trait show {\n",
            "  fn text(self) -> string\n",
            "}\n",
            "trait render {\n",
            "  fn text(self) -> string\n",
            "}\n",
            "impl show for int { }\n",
            "impl render for int { }\n",
            "fn f(x: int) -> string { x.text() }\n",
        ))
        .last()
        .map(String::as_str),
        Some(
            "DS2014 dot-call `text` is truly ambiguous between `show` and `render`; qualify with \
             `Trait::text` [F3]"
        )
    );
}

#[test]
fn tuples_and_struct_literals_check_shapes() {
    clean("const kv: (string, int) = (\"host\", 8080)\n");
    clean("const c = config { host: \"h\", port: 1 }\n");
    // Missing fields and wrong field types are named.
    assert_eq!(check("const c = config { host: \"h\" }\n"), [
        "DS2001 `config` literal misses field `port`"
    ]);
    assert_eq!(check("const c = config { host: \"h\", port: \"x\" }\n"), [
        "DS2001 `field value` has type `string`, not contained in `int` [B2]"
    ]);
    // Punning reads the field name as a value use.
    clean("fn f(host: string) -> config {\n  config { host, port: 1 }\n}\n");
}

#[test]
fn receivers_bind_self_and_annotate_self_type() {
    // Record 6.1: the receiver is `self` (borrow) or `mut self` (exclusive
    // window), exempt from annotation and typed `Self`; other parameters
    // and the return may spell `Self`.
    clean(concat!(
        "trait add {\n",
        "  fn add(self, other: Self) -> Self\n",
        "}\n",
        "impl add for int {\n",
        "  fn add(self, other: Self) -> Self { self + other }\n",
        "}\n",
    ));
    clean(concat!(
        "trait bump {\n",
        "  fn bump(mut self) -> int\n",
        "}\n",
        "impl bump for int {\n",
        "  fn bump(mut self) -> int { self + 1 }\n",
        "}\n",
    ));
    // Inherent impls see `Self` too, and the receiver binds their target.
    clean(concat!(
        "impl int {\n",
        "  fn zero(self) -> Self { self - self }\n",
        "}\n",
    ));
    // The receiver's type is the implementing type: a field access
    // through it checks.
    clean(concat!(
        "impl config {\n",
        "  fn host(self) -> string { self.host }\n",
        "}\n",
    ));
    // A member referencing `Self` outside the member scopes does not
    // resolve: `Self` binds inside trait/impl declarations only (the
    // resolve rejection plus the parameter's inference fallback).
    assert_eq!(check("fn f(x: Self) -> int { 1 }\n").as_slice(), [
        "DS2004 unknown type `Self`",
        "DS2002 parameter `x` has no expected type to infer from; annotate it [B2]",
    ]);
}

#[test]
fn self_as_a_type_name_is_retired() {
    // One diagnostic names the replacement; the annotation fallbacks stay
    // silent under it.
    assert_eq!(check("fn f(x: self) -> int { 1 }\n").as_slice(), [
        "DS2015 `self` is retired as a type name; the receiver type is spelled `Self`"
    ]);
    assert_eq!(check("trait t {\n  fn f(self) -> self\n}\n").as_slice(), [
        "DS2015 `self` is retired as a type name; the receiver type is spelled `Self`"
    ]);
}

#[test]
fn the_annotated_receiver_is_the_old_spelling() {
    // The pre-adoption `(self: self, ...)` shape reports the receiver
    // ruling once per member, not per annotation site.
    assert_eq!(
        check(concat!("trait t {\n", "  fn f(self: self) -> int\n", "}\n",)).as_slice(),
        [
            "DS2016 the receiver is spelled `self` or `mut self` with no annotation; it is typed \
             `Self`"
        ]
    );
    assert_eq!(
        check(concat!(
            "trait t {\n",
            "  fn f(self) -> int\n",
            "}\n",
            "impl t for int {\n",
            "  fn f(self: int) -> int { 1 }\n",
            "}\n",
        ))
        .as_slice(),
        [
            "DS2016 the receiver is spelled `self` or `mut self` with no annotation; it is typed \
             `Self`"
        ]
    );
}

#[test]
fn field_access_needs_one_struct() {
    clean("fn f(c: config) -> int { c.port }\n");
    assert_eq!(check("fn f(c: config | int) -> int { c.port }\n"), [
        "DS2001 field access needs one struct, `int | config` given; narrow with `is` first [B2]"
    ]);
}

#[test]
fn difference_types_erase_to_the_solved_shape() {
    // `io_err - io_err` erases entirely [B1]: the annotation solves to
    // `empty`, and nothing inhabits it — no any fallback.
    assert_eq!(check("const x: io_err - io_err = io_err\n"), [
        "DS2001 `annotated binding` has type `io_err`, not contained in `empty` [B2]"
    ]);
}

#[test]
fn optional_sugar_is_union_none_everywhere() {
    clean("fn f(x: int?) -> int | none { x }\n");
    clean("fn g(x: int | none) -> int? { x }\n");
}

#[test]
fn a_binding_tail_checks_and_the_block_value_is_unit() {
    // The final statement of a block checks even when it is a binding
    // (script entry files fold into exactly this shape), and the block's
    // value is then unit: a non-unit boundary still fails containment.
    clean("fn f() -> int { const x = 1\n x\n}\n");
    clean("fn f() -> () { const x = 1\n}\n");
    let diags = check("fn f() -> int { const x = 1\n}\n");
    assert!(
        diags
            .iter()
            .any(|d| d.starts_with("DS2001") && d.contains("`lambda body`")),
        "the unit block fails the int boundary: {diags:?}"
    );
    let diags = check("fn f() -> () { const x = 1 + \"s\"\n}\n");
    assert!(
        diags
            .iter()
            .any(|d| d.starts_with("DS2001") && d.contains("numeric")),
        "the binding tail itself is checked: {diags:?}"
    );
}

/// A destructuring declaration binds exactly one tuple [B3]: the element
/// binders take their element types, a wrong arity is the tuple-pattern
/// error, and a union-typed initializer is rejected outright — picking
/// one member's types would bind without a coverage proof [F4].
#[test]
fn destructuring_declarations_bind_one_tuple_or_reject() {
    clean("fn f(t: (int, string)) -> int { const (a, b) = t\n a }\n");
    clean("fn f(t: (int, string)) -> int { const (a, _) = t\n a }\n");
    // wrong arity: no member of the initializer's type matches
    assert_eq!(
        check("fn f(t: (int, string)) -> int { const (a, b, c) = t\n 0 }\n"),
        ["DS2001 tuple pattern of 3 element(s) matches no member of `(int, string)` [B3]"]
    );
    // a union initializer would bind one member's types unproven
    assert_eq!(
        check(concat!(
            "fn pick() -> (int, int) | none { (1, 2) }\n",
            "fn f() -> int { const u = pick()\n const (a, b) = u\n a }\n",
        )),
        [
            "DS2001 a destructuring declaration over `none | (int, int)` needs one tuple of 2 \
             element(s); narrow the union with a match first [B3, F4]"
        ]
    );
}

/// A match arm's tuple pattern types and binds each element [B3, F4].
#[test]
fn match_arm_tuple_patterns_type_each_element() {
    clean("fn f(t: (int, string)) -> int { match t { (a, b) => a, } }\n");
    clean("fn f(t: (int, string)) -> string { match t { (_, b) => b, } }\n");
    // the element binder carries its own element type
    assert_eq!(
        check("fn f(t: (int, string)) -> string { match t { (a, b) => a, } }\n"),
        ["DS2001 `lambda body` has type `int`, not contained in `string` [B2]"]
    );
}

/// Tuple construction is bidirectional against an expected tuple type:
/// each element receives its expected type, so the expected-consuming
/// forms infer inside a tuple the way they do in a return position.
#[test]
fn tuple_construction_propagates_expected_element_types() {
    clean("fn f() -> (list[int], int) { ([], 3) }\n");
    // the expected element reaches the empty literal; without a
    // matching element expectation the empty literal still demands one
    assert_eq!(check("fn f() -> (int, int) { ([], 3) }\n"), ["DS2002 the \
                                                              empty list \
                                                              literal needs \
                                                              an expected \
                                                              `list[t]`; \
                                                              write `list[t]()` \
                                                              for the explicit \
                                                              empty list \
                                                              [B3]"]);
}

/// Generic ambient externs check their arguments [B2]: the structural
/// binder stays silent on a constructor mismatch, so the solved
/// parameter has to contain the argument or the mismatch reports.
#[test]
fn generic_calls_check_arguments() {
    // The R6 probe pair: a mismatched constructor binds nothing and
    // reports against the written parameter spelling.
    assert_eq!(
        check(concat!(
            "extern fn list_len[t] (xs: list[t]) -> int
",
            "fn f() -> int { list_len(\"words\") }
",
        )),
        ["DS2001 `argument` has type `string`, not contained in `list[t]` [B2]"]
    );
    assert_eq!(
        check(concat!(
            "struct map[k, v]
",
            "extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()
",
            "fn f(mut m: map[string, int]) -> () { map_set(m, 8080, 1) }
",
        )),
        ["DS2001 `argument` has type `int`, not contained in `string` [B2]"]
    );
    // A bound variable carries into the next argument's check: `t` is
    // `string` by the time the second argument arrives.
    assert_eq!(
        check(concat!(
            "extern fn cons[t] (x: t, xs: list[t]) -> list[t]
",
            "fn f() -> list[string] { cons(\"x\", 1) }
",
        )),
        ["DS2001 `argument` has type `int`, not contained in `list[string]` [B2]"]
    );
    // The honest spelling checks clean.
    clean(concat!(
        "extern fn list_len[t] (xs: list[t]) -> int
",
        "fn f(xs: list[string]) -> int { list_len(xs) }
",
    ));
}

/// Nested generic applications construct [F9]: `list[list[string]]()` and
/// the map spellings apply their own bracketed type arguments.
#[test]
fn nested_generic_applications_construct() {
    clean(concat!(
        "fn f() -> list[list[string]] {
",
        "  list[list[string]]()
",
        "}
",
    ));
    clean(concat!(
        "struct map[k, v]
",
        "fn f() -> map[string, list[int]] {
",
        "  map[string, list[int]]()
",
        "}
",
    ));
    clean(concat!(
        "struct map[k, v]
",
        "fn f() -> list[map[string, int]] {
",
        "  list[map[string, int]]()
",
        "}
",
    ));
    // A value in the brackets stays the cross-spelling error.
    assert_eq!(
        check(
            "fn f() -> list[int] { list[1]() }
"
        ),
        ["DS2001 the bracketed argument of `list[...]` is a type, not a value [F9]"]
    );
}

#[test]
fn trailing_block_arguments_feed_the_final_lambda_parameter() {
    // the block after a call is the callee's final zero-argument lambda
    // [F7]: it fills the open parameter slot and types against the
    // fn-typed parameter, identical to the parenthesized lambda
    clean(concat!(
        "fn call(x: int, f: fn() -> int) -> int { x + f() }\n",
        "fn main() -> int { call(5) { 42 } }\n",
    ));
    clean(concat!(
        "fn call(x: int, f: fn() -> int) -> int { x + f() }\n",
        "fn main() -> int { call(5, () => 42) }\n",
    ));
    assert_eq!(
        check(concat!(
            "fn one(x: int) -> int { x }\n",
            "fn main() -> int { one(5) { 42 } }\n",
        )),
        ["DS2007 the trailing lambda has no parameter to feed"]
    );
}

#[test]
fn generic_calls_enforce_their_parameter_bounds() {
    // The solved instantiation carries each parameter's declared trait
    // bound [B6]: an argument type the instance environment (or an
    // in-scope assumption) covers passes, anything else reports the
    // unsatisfied constraint once. Bound-free generic externs stay
    // clean.
    clean(concat!(
        "trait show {\n",
        "  fn text(self) -> string\n",
        "}\n",
        "impl show for int {\n",
        "  fn text(self) -> string { str(self) }\n",
        "}\n",
        "extern fn list_len[t] (xs: list[t]) -> int\n",
        "fn shown[t: show](x: t) -> string { x.text() }\n",
        "fn calls(n: int, xs: list[int]) -> () {\n",
        "  shown(n)\n",
        "  list_len(xs)\n",
        "  ()\n",
        "}\n",
    ));
    assert_eq!(
        check(concat!(
            "trait show {\n",
            "  fn text(self) -> string\n",
            "}\n",
            "impl show for int {\n",
            "  fn text(self) -> string { str(self) }\n",
            "}\n",
            "extern fn list_len[t] (xs: list[t]) -> int\n",
            "fn shown[t: show](x: t) -> string { x.text() }\n",
            "fn calls(n: int, s: string, xs: list[int]) -> () {\n",
            "  shown(n)\n",
            "  shown(s)\n",
            "  list_len(xs)\n",
            "  ()\n",
            "}\n",
        )),
        ["DS2013 no instance of `show` for `string`"]
    );
    // A caller-side generic bound propagates as an assumption: the inner
    // call needs no instance of its own.
    clean(concat!(
        "trait show {\n",
        "  fn text(self) -> string\n",
        "}\n",
        "fn shown[t: show](x: t) -> string { x.text() }\n",
        "fn relay[t: show](x: t) -> string { shown(x) }\n",
    ));
    // An errored argument solves to the empty type; the bound check
    // skips the cascade instead of piling a constraint failure onto it.
    let diags = check(concat!(
        "trait show {\n",
        "  fn text(self) -> string\n",
        "}\n",
        "fn shown[t: show](x: t) -> string { x.text() }\n",
        "fn calls() -> () {\n",
        "  shown(missing)\n",
        "  ()\n",
        "}\n",
    ));
    assert_eq!(diags.len(), 2, "only the unknown-name pair: {diags:?}");
    assert!(
        diags.iter().all(|d| !d.contains("no instance of")),
        "the cascade skips the bound check: {diags:?}"
    );
}

#[test]
fn dot_call_on_a_value_binding_reports_not_callable() {
    // The dot's free tier searches the value namespace [F9], so the
    // callee can be a plain const or var: a call against it is a
    // call-shape error with the receiver and arguments walked, never a
    // silently empty type.
    assert_eq!(
        check(concat!("const n = 5\n", "fn f(x: int) -> int { x.n() }\n",)),
        ["DS2002 `n` is a value binding, not a function"]
    );
}

#[test]
fn indirect_calls_check_the_argument_count() {
    // The fn-typed-value path matches the direct path's arity
    // discipline: extras report at the first extra argument and still
    // walk (inner errors surface), missing arguments report at the call.
    assert_eq!(
        check(concat!(
            "fn id(x: int) -> int { x }\n",
            "const f = id\n",
            "fn g() -> int { f(1, 2) }\n",
        )),
        ["DS2007 more arguments than the function type takes"]
    );
    assert_eq!(
        check(concat!(
            "fn id(x: int) -> int { x }\n",
            "const f = id\n",
            "fn g() -> int { f() }\n",
        )),
        ["DS2007 missing argument(s): the function type takes 1"]
    );
    // The dropped argument still walks: its own unknown name surfaces
    // alongside the arity error (the resolution pair plus the walk's
    // not-a-value fallback).
    let diags = check(concat!(
        "fn id(x: int) -> int { x }\n",
        "const f = id\n",
        "fn g() -> int { f(1, missing) }\n",
    ));
    assert_eq!(
        diags.len(),
        3,
        "arity plus the walked extra's pair: {diags:?}"
    );
    assert!(
        diags.iter().any(|d| d.contains("unknown name")),
        "the extra argument walked for inner errors: {diags:?}"
    );
}

#[test]
fn duplicate_inherent_method_keeps_the_first_signature() {
    // Two inherent impls defining one method name: the duplicate reports
    // at collection (both spans, first registration kept), and the call
    // checks against the first impl's signature — never the second's
    // silently overwriting slot.
    let diags = check(concat!(
        "tag t\n",
        "impl t { fn m(self) -> int { 1 } }\n",
        "impl t { fn m(self) -> int { 2 } }\n",
        "fn use(x: t) -> int { x.m() }\n",
    ));
    assert_eq!(diags.len(), 1, "the duplicate alone: {diags:?}");
    assert_eq!(diags[0], "DS2005 method `m` is already defined");
    // With the duplicate only, the call still checks: swapping the second
    // impl's return to string must not leak into the call's typing.
    let diags = check(concat!(
        "tag t\n",
        "impl t { fn m(self) -> int { 1 } }\n",
        "impl t { fn m(self) -> string { \"s\" } }\n",
        "fn use(x: t) -> int { x.m() + 1 }\n",
    ));
    assert_eq!(
        diags.len(),
        1,
        "no type cascade off the duplicate: {diags:?}"
    );
    assert_eq!(diags[0], "DS2005 method `m` is already defined");
}

#[test]
fn top_level_destructuring_patterns_type_their_binders() {
    // A top-level tuple/struct-pun pattern declares package defs whose
    // types memoize like any single-name binding [G3] — uses far from the
    // declaration see the element types, not "no initializer".
    clean(concat!(
        "const (n, s) = (1, \"x\")\n",
        "fn use() -> int { n }\n",
    ));
    clean(concat!(
        "struct pair { a: int, b: string }\n",
        "extern fn make_pair () -> pair\n",
        "const pair { a, b } = make_pair()\n",
        "fn use() -> int { a }\n",
    ));
    // The binders carry their element types: `s` is a string used as an
    // int, reported at the use site.
    assert_eq!(
        check(concat!(
            "struct pair { a: int, b: string }\n",
            "extern fn make_pair () -> pair\n",
            "const pair { a, b } = make_pair()\n",
            "fn use() -> int { b }\n",
        )),
        ["DS2001 `lambda body` has type `string`, not contained in `int` [B2]"]
    );
}
