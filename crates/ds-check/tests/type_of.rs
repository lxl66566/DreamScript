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
    assert_eq!(check("trait t {\n  fn g(self, x) -> int\n}\n"), [
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
    // a struct operand without a `show` impl refuses naming the remedy
    assert_eq!(
        check(concat!(
            "const c = config { host: \"h\", port: 1 }\n",
            "const s = \"${c}\"\n",
        )),
        [
            "DS2001 interpolation hole has type `config`; the printable primitives (bool, char, \
             float, int, string) convert implicitly, every other type carries `show` [F6]"
        ]
    );
    // a union operand extracts first (or carries a whole-union impl)
    assert_eq!(check("fn f(x: int | io_err) -> string { \"${x}\" }\n"), [
        "DS2001 interpolation hole has type `int | io_err`; the printable primitives (bool, char, \
         float, int, string) convert implicitly, every other type carries `show` [F6]"
    ]);
}

/// The refusal's help note names what to do (F6): derive or implement
/// `show` for a single-member type, extract or impl for a union — the
/// stale "arrives with M6" promise is gone.
#[test]
fn interpolation_refusal_notes_name_the_remedy() {
    let note_of = |source: &str| -> Vec<String> {
        let combined = format!("{PRELUDE}{source}");
        let mut files = Files::new();
        let file = files.add_source("t.ds", &combined);
        let ast = parse(&files, file).unwrap_or_else(|diags| panic!("should parse: {diags:?}"));
        type_check(&files, &[ast])
            .diags
            .iter()
            .flat_map(|d| d.notes.clone())
            .collect()
    };
    assert_eq!(
        note_of(concat!(
            "const c = config { host: \"h\", port: 1 }\n",
            "const s = \"${c}\"\n",
        )),
        ["derive `show` (`#[derive(show)]`) or implement `show` for this type"]
    );
    assert_eq!(note_of("fn f(x: int | io_err) -> string { \"${x}\" }\n"), [
        "extract a member first [B4, E4], or implement `show` over the whole union"
    ]);
}

/// A hole whose operand carries `show` checks clean (F6): the derived
/// impl, the hand-written trait impl, the inherent member, a
/// whole-union head, and a bound-variable receiver (the assumption's
/// runtime-evidence dispatch) all answer; a boundless type variable
/// keeps the standard refusal.
#[test]
fn interpolation_accepts_show_carrying_operands() {
    clean(concat!(
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "struct cell { v: int }\n",
        "impl show for cell {\n",
        "  fn show(self) -> string { \"cell\" }\n",
        "}\n",
        "const c = cell { v: 1 }\n",
        "const s = \"$c and ${c}\"\n",
    ));
    clean(concat!(
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "struct cell { v: int }\n",
        "impl cell {\n",
        "  fn show(self) -> string { \"cell\" }\n",
        "}\n",
        "const c = cell { v: 1 }\n",
        "const s = \"$c\"\n",
    ));
    clean(concat!(
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl show for int | io_err {\n",
        "  fn show(self) -> string { \"u\" }\n",
        "}\n",
        "fn f(x: int | io_err) -> string { \"${x}\" }\n",
    ));
    // the assumption-answered form checks clean: the hole records the
    // member and the lowering resolves the dispatch through the frame's
    // runtime-evidence slot
    clean(concat!(
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "fn f[t: show](v: t) -> string { \"$v\" }\n",
    ));
    // a boundless type variable names no assumption and no instance
    // serves it: the standard F6 refusal, the variable rendering under
    // its minted internal name
    let boundless = check(concat!(
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "fn f[t](v: t) -> string { \"$v\" }\n",
    ));
    assert_eq!(boundless.len(), 1);
    assert!(
        boundless[0].starts_with("DS2001 interpolation hole has type `$t#")
            && boundless[0].ends_with(
                "the printable primitives (bool, char, float, int, string) convert implicitly, \
                 every other type carries `show` [F6]"
            ),
        "the boundless variable hole keeps the standard refusal: {boundless:?}"
    );
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

/// A lambda literal's own arity matches the expected fn type's exactly:
/// the call ABI stages the declared parameter count, so a lambda taking
/// fewer (or more) parameters than the fn type would trap at the arity
/// validation the moment it is called — the mismatch refuses at the
/// literal's check over every expectation source (the direct call, the
/// generic call's solved fn-typed parameter, and the trailing block's
/// sugar-fed slot), with the arity refusal as the single root cause.
#[test]
fn lambda_literals_match_the_expected_fn_type_arity() {
    // The direct call: fewer and more parameters than `fn(int) -> int`.
    assert_eq!(
        check(concat!(
            "fn take(f: fn(int) -> int) -> int { f(1) }\n",
            "fn main() -> int { take(() => 3) }\n",
        )),
        ["DS2007 the lambda takes 0 parameters, the expected fn type takes 1"]
    );
    assert_eq!(
        check(concat!(
            "fn take(f: fn(int) -> int) -> int { f(1) }\n",
            "fn main() -> int { take((x, y) => 3) }\n",
        )),
        ["DS2007 the lambda takes 2 parameters, the expected fn type takes 1"]
    );
    // The generic call's fn-typed parameter (the seam the few-params
    // acceptance left open: both spellings checked clean and trapped at
    // the hosted per-element arity validation).
    assert_eq!(
        check(concat!(
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "fn main() -> int {\n",
            "  const ys = map([1, 2], () => 3)\n",
            "  0\n",
            "}\n",
        )),
        ["DS2007 the lambda takes 0 parameters, the expected fn type takes 1"]
    );
    assert_eq!(
        check(concat!(
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "fn main() -> int {\n",
            "  const ys = map([1, 2]) { 3 }\n",
            "  0\n",
            "}\n",
        )),
        ["DS2007 the lambda takes 0 parameters, the expected fn type takes 1"]
    );
    // An annotated binding keeps its own refusal: the lambda types with
    // no fn-typed expectation and the boundary containment catches the
    // distinct fn atoms.
    assert_eq!(
        check(concat!(
            "fn main() -> () {\n",
            "  const g: fn(int) -> int = () => 3\n",
            "}\n",
        )),
        [
            "DS2001 `annotated binding` has type `fn() -> int`, not contained in `fn(int) -> int` \
             [B2]"
        ]
    );
    // The arity-matched spellings stay clean, block and written both.
    clean(concat!(
        "fn take(f: fn(int) -> int) -> int { f(1) }\n",
        "fn main() -> int { take((x: int) => x * 2) }\n",
    ));
    clean(concat!(
        "fn take() -> int { 1 }\n",
        "fn main() -> int { take() }\n",
    ));
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
fn same_name_inherent_methods_select_by_owner() {
    // Two owner types share one method name: each dot-call binds its own
    // owner's signature — distinct returns prove no cross-binding — a
    // generic owner accepts its applications, and a receiver the owner
    // does not cover reports at the dot [F3].
    clean(concat!(
        "struct counter { ticks: int }\n",
        "struct timer { laps: int }\n",
        "impl counter { fn peek(self) -> int { 1 } }\n",
        "impl timer { fn peek(self) -> int { 2 } }\n",
        "fn use(c: counter, t: timer) -> int { c.peek() + t.peek() }\n",
    ));
    // timer.peek returns string: the sum reports the arithmetic meeting,
    // proving t.peek() bound timer's signature and c.peek() counter's.
    assert_eq!(
        check(concat!(
            "struct counter { ticks: int }\n",
            "struct timer { laps: int }\n",
            "impl counter { fn peek(self) -> int { 1 } }\n",
            "impl timer { fn peek(self) -> string { \"t\" } }\n",
            "fn use(c: counter, t: timer) -> int { c.peek() + t.peek() }\n",
        )),
        ["DS2001 arithmetic needs numeric operands, `int` and `string` given [B7]"]
    );
    // A generic impl owns the bare constructor; applications of it accept.
    clean(concat!(
        "struct box[t]\n",
        "impl[t] box[t] { fn size(self) -> int { 1 } }\n",
        "fn use(b: box[int]) -> int { b.size() }\n",
    ));
    // The receiver must be contained in the owner: a wrong type and an
    // unnarrowed union both report at the dot.
    assert_eq!(
        check(concat!(
            "struct counter { ticks: int }\n",
            "impl counter { fn peek(self) -> int { 1 } }\n",
            "fn wrong(x: int) -> int { x.peek() }\n",
        )),
        ["DS2013 no method `peek` accepts receiver `int`"]
    );
    let union = check(concat!(
        "struct counter { ticks: int }\n",
        "impl counter { fn peek(self) -> int { 1 } }\n",
        "fn use(c: int | counter) -> int { c.peek() }\n",
    ));
    assert_eq!(
        union.len(),
        1,
        "the unnarrowed union reports once: {union:?}"
    );
    assert!(
        union[0].contains("no method `peek` accepts receiver"),
        "{union:?}"
    );
    // Narrowing restores the call.
    clean(concat!(
        "struct counter { ticks: int }\n",
        "impl counter { fn peek(self) -> int { 1 } }\n",
        "fn use(c: int | counter) -> int {\n",
        "  if c is counter { c.peek() } else { 0 }\n",
        "}\n",
    ));
}

#[test]
fn mut_self_member_calls_share_the_const_receiver_gate() {
    // The D2/C3 exclusivity gate is per mut window, not per call
    // spelling: a `mut self` member — trait or inherent — rejects a
    // `const` receiver exactly like the free-function spelling, while a
    // `var` receiver and a fresh value pass.
    assert_eq!(
        check(concat!(
            "struct point { x: int, y: int }\n",
            "trait bump { fn bump(mut self) -> int }\n",
            "impl bump for point {\n",
            "  fn bump(mut self) -> int { 1 }\n",
            "}\n",
            "fn f() -> int {\n",
            "  const p = point { x: 1, y: 2 }\n",
            "  p.bump()\n",
            "}\n",
        )),
        ["DS3006 `p` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]"]
    );
    assert_eq!(
        check(concat!(
            "struct point { x: int, y: int }\n",
            "impl point { fn bump(mut self) -> int { 1 } }\n",
            "fn f() -> int {\n",
            "  const p = point { x: 1, y: 2 }\n",
            "  p.bump()\n",
            "}\n",
        )),
        ["DS3006 `p` is a `const` binding; a `mut` window needs a `var` receiver [D2, C3]"]
    );
    clean(concat!(
        "struct point { x: int, y: int }\n",
        "trait bump { fn bump(mut self) -> int }\n",
        "impl bump for point {\n",
        "  fn bump(mut self) -> int { 1 }\n",
        "}\n",
        "fn f() -> int {\n",
        "  var p = point { x: 1, y: 2 }\n",
        "  p.bump()\n",
        "}\n",
    ));
    clean(concat!(
        "struct point { x: int, y: int }\n",
        "trait bump { fn bump(mut self) -> int }\n",
        "impl bump for point {\n",
        "  fn bump(mut self) -> int { 1 }\n",
        "}\n",
        "fn f() -> int { point { x: 1, y: 2 }.bump() }\n",
    ));
}

#[test]
fn only_mut_parameters_are_reassignable() {
    // C3 has exactly two re-assignable carriers: `var` bindings and `mut`
    // parameter windows. `move` transfers once and `comptime` is a
    // compile-time value, so assignments to them reject like a borrow
    // parameter's — in fn and lambda position alike.
    let expected = ["DS2001 assignment needs a `var` binding; `const` bindings are immutable [C3]"];
    assert_eq!(
        check(concat!(
            "struct point { x: int, y: int }\n",
            "fn bump(move p: point) -> point {\n",
            "  p = point { x: 1, y: 2 }\n",
            "  p\n",
            "}\n",
        )),
        expected
    );
    assert_eq!(
        check(concat!(
            "struct point { x: int, y: int }\n",
            "fn bump(comptime p: int) -> int {\n",
            "  p = 2\n",
            "  p\n",
            "}\n",
        )),
        expected
    );
    assert_eq!(
        check(concat!(
            "struct point { x: int, y: int }\n",
            "fn bump() -> int {\n",
            "  const f = (move p: point) -> int {\n",
            "    p = point { x: 1, y: 2 }\n",
            "    p.x\n",
            "  }\n",
            "  f(point { x: 0, y: 0 })\n",
            "}\n",
        )),
        expected
    );
    clean(concat!(
        "struct point { x: int, y: int }\n",
        "fn bump(mut p: point) -> point {\n",
        "  p = point { x: 1, y: 2 }\n",
        "  p\n",
        "}\n",
    ));
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

/// The inference-hole boundary of `?` [E4]: the split needs a known
/// return union, so a lambda whose return is inferred — no annotation, or
/// a bare type-variable expectation from a generic callee's `r` — reports
/// honestly instead of binding the whole union; the declared-return
/// spelling keeps the split. Deferring through a placeholder variable
/// would mistype mid-body uses (the binding would carry the error
/// members), so the rejection is the principled M2 answer.
#[test]
fn question_marks_need_a_known_return_union() {
    // Declared returns split cleanly, in value and argument position.
    clean(concat!(
        "extern fn read (p: string) -> string | io_err\n",
        "fn g() -> string | io_err {\n",
        "  const h = (p: string) -> string | io_err { read(p)? }\n",
        "  h(\"x\")\n",
        "}\n",
    ));
    clean(concat!(
        "extern fn read (p: string) -> string | io_err\n",
        "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
        "fn g(xs: list[string]) -> list[string | io_err] {\n",
        "  map(xs, (w: string) -> string | io_err { read(w)? })\n",
        "}\n",
    ));
    // An inferred return cannot split: the report names the missing
    // return union (never "outside a function" — a function encloses).
    let stray = "DS2008 `?` needs an enclosing return union to propagate into [E4]";
    assert_eq!(
        check(concat!(
            "extern fn read (p: string) -> string | io_err\n",
            "fn g() -> string | io_err {\n",
            "  const h = (p: string) => read(p)?\n",
            "  h(\"x\")\n",
            "}\n",
        )),
        [stray]
    );
    // The bare type-variable expectation (a generic callee's `r`) is the
    // same hole: the expectation is not a union to split against — and the
    // missing union is the ONLY diagnostic: the empty `r` wrapping into
    // `list[empty]` at the boundary consumed the poisoned call without
    // re-reporting.
    assert_eq!(
        check(concat!(
            "extern fn read (p: string) -> string | io_err\n",
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "fn g(xs: list[string]) -> list[string] {\n",
            "  map(xs, (w: string) => read(w)?)\n",
            "}\n",
        )),
        [stray]
    );
}

/// The expression-error epoch: once an expression reports, its enclosing
/// positions consume the poisoned value without re-reporting — the root
/// cause stands alone. Sibling positions keep their own errors.
#[test]
fn root_expression_errors_do_not_cascade_upward() {
    // The DS2008 inside the lambda body must not re-surface as a
    // `list[empty]` containment at the function boundary.
    assert_eq!(
        check(concat!(
            "extern fn read (p: string) -> string | io_err\n",
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "fn g(xs: list[string]) -> list[string] {\n",
            "  map(xs, (w: string) => read(w)?)\n",
            "}\n",
        )),
        ["DS2008 `?` needs an enclosing return union to propagate into [E4]"]
    );
    // An errored initializer reports once; the empty-type fallback at the
    // binding is not a second error.
    assert_eq!(
        check(concat!(
            "fn f(x: int | string) -> int {\n",
            "  const y = 1 + \"s\"\n",
            "  y\n",
            "}\n",
        )),
        ["DS2001 arithmetic needs numeric operands, `int` and `string` given [B7]"]
    );
    // The wrapper shapes: a poisoned operand silences the operator's own
    // complaint, and the field/method/call reports over it.
    assert_eq!(check("fn f(x: int) -> bool { 1 + \"s\" == x }\n"), [
        "DS2001 arithmetic needs numeric operands, `int` and `string` given [B7]"
    ]);
    assert_eq!(check("fn f(c: config) -> int { (1 + \"s\").port }\n"), [
        "DS2001 arithmetic needs numeric operands, `int` and `string` given [B7]"
    ]);
}

#[test]
fn sibling_positions_keep_their_own_errors() {
    // One bad sibling reports its root; the good sibling's mismatch is an
    // independent error and still reports.
    assert_eq!(
        check(concat!(
            "extern fn two (a: int, b: int) -> int\n",
            "fn f() -> int {\n",
            "  two(1 + \"s\", \"t\")\n",
            "}\n",
        )),
        [
            "DS2001 arithmetic needs numeric operands, `int` and `string` given [B7]",
            "DS2001 `argument` has type `string`, not contained in `int` [B2]"
        ]
    );
    // Two bad siblings each report their own root.
    assert_eq!(
        check(concat!(
            "extern fn two (a: int, b: int) -> int\n",
            "fn f() -> int {\n",
            "  two(1 + \"s\", 2 + \"u\")\n",
            "}\n",
        )),
        [
            "DS2001 arithmetic needs numeric operands, `int` and `string` given [B7]",
            "DS2001 arithmetic needs numeric operands, `int` and `string` given [B7]"
        ]
    );
}

/// A provably-disjoint `is` test computes a constant false: in value or
/// assert position it is a compile error (mirroring the constant `==`
/// carve-out), while condition positions refine legitimately — a disjoint
/// test narrows the kept branch to empty, which is sound dead code, and a
/// match arm covering nothing is the exhaustiveness story.
#[test]
fn always_false_is_tests_report_only_in_value_position() {
    // Value position: the test can never hold.
    assert_eq!(
        check(concat!(
            "fn f(x: string) -> bool {\n",
            "  const b = x is io_err\n",
            "  b\n",
            "}\n",
        )),
        ["DS2001 the `is` test against `io_err` is always false over `string` [B2]"]
    );
    // The operand's flow-refined current type is what the test meets: in
    // the else branch `x` is narrowed away from int, so `is int` there is
    // still honest refinement (condition), but its value use reports.
    assert_eq!(
        check(concat!(
            "fn f(x: int | string) -> bool {\n",
            "  if x is int { true } else { x is io_err }\n",
            "}\n",
        )),
        ["DS2001 the `is` test against `io_err` is always false over `string` [B2]"]
    );
    // Condition positions stay quiet: if, while, and match guards.
    clean("fn f(x: string) -> int {\n  if x is io_err { 1 } else { 0 }\n}\n");
    clean(concat!(
        "fn f(x: string) -> int {\n",
        "  var n = 0\n",
        "  while x is io_err { n = n + 1 }\n",
        "  n\n",
        "}\n",
    ));
    clean(concat!(
        "fn f(x: string) -> int {\n",
        "  match x { s if s is io_err => 0, _ => 1 }\n",
        "}\n",
    ));
    // A test that CAN hold never reports, in any position.
    clean("fn f(x: int | string) -> bool { x is int }\n");
    clean("fn f(x: int | string) -> int {\n  if x is int { 1 } else { 0 }\n}\n");
}

/// The universe predeclares `none` as its distinguished unit atom [B4]; a
/// `tag none` (or a unit enum case `none`) restates that atom instead of
/// minting a second — every written `none`, in type and value position,
/// keeps solving. Before the alias the refused mint silently orphaned the
/// declaration's def and `int | none` / `is none` stopped solving.
#[test]
fn none_restatement_maps_onto_the_predeclared_atom() {
    clean(concat!(
        "tag none\n",
        "fn f(x: int | none) -> int {\n",
        "  if x is none { 0 } else { x }\n",
        "}\n",
        "const a = none\n",
        "fn g() -> none { a }\n",
    ));
    clean(concat!(
        "enum opt { none, some }\n",
        "fn f(x: int | none) -> int {\n",
        "  if x is none { 0 } else { x }\n",
        "}\n",
        "const a = none\n",
        "fn g() -> none { a }\n",
    ));
    // Optional sugar still meets the predeclared member.
    clean("tag none\nfn f(x: int?) -> int | none { x }\n");
}

/// The script-entry fold's synthesized `main` checks through a lambda node
/// the user never wrote; its boundary label names the script entry, not a
/// lambda (a written fn keeps the lambda-body label since every fn
/// converts onto the const-of-lambda node).
#[test]
fn script_entry_value_tail_names_the_script_main_body() {
    let mut files = Files::new();
    let file = files.add_source("t.ds", &format!("{PRELUDE}1\n\"tail\"\n"));
    let ast = ds_parse::parse_script(&files, file)
        .unwrap_or_else(|diags| panic!("should parse: {diags:?}"));
    let diags = type_check(&files, &[ast])
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code.as_str(), d.message))
        .collect::<Vec<_>>();
    assert_eq!(diags, [
        "DS2001 `script main body` has type `string`, not contained in `()` [B2]"
    ]);
}

/// The loop depth is frame-scoped: a lambda or spawned body inherits no
/// enclosing loop, so a loop exit lexically inside one reports the
/// crossed boundary instead of waiting for the lowering refusal.
#[test]
fn loop_exits_never_cross_a_lambda_or_spawned_body_frame() {
    let boundary = "DS2002 loop exits never cross a lambda or spawned-body frame boundary";
    assert_eq!(
        check(concat!(
            "fn call(f: fn() -> ()) -> () { f() }\n",
            "fn main() -> () {\n",
            "  var i = 0\n",
            "  while i < 3 {\n",
            "    call(() => { break })\n",
            "    i += 1\n",
            "  }\n",
            "}\n",
        )),
        [boundary]
    );
    assert_eq!(
        check(concat!(
            "fn call(f: fn() -> ()) -> () { f() }\n",
            "fn main() -> () {\n",
            "  var i = 0\n",
            "  while i < 3 {\n",
            "    call(() => { continue })\n",
            "    i += 1\n",
            "  }\n",
            "}\n",
        )),
        [boundary]
    );
    assert_eq!(
        check(concat!(
            "fn main() -> () {\n",
            "  var i = 0\n",
            "  while i < 3 {\n",
            "    nursery {\n",
            "      spawn { break }\n",
            "    }\n",
            "    i += 1\n",
            "  }\n",
            "}\n",
        )),
        [boundary]
    );
}

/// A loop lexically inside a lambda body still owns that body's exits,
/// and a nursery body lowers inline in the enclosing frame, so its
/// exits keep addressing the enclosing loop.
#[test]
fn frame_scoped_loops_own_their_exits() {
    clean(concat!(
        "fn call(f: fn() -> ()) -> () { f() }\n",
        "fn main() -> () {\n",
        "  call(() => {\n",
        "    var j = 0\n",
        "    while j < 3 {\n",
        "      break\n",
        "      j += 1\n",
        "    }\n",
        "  })\n",
        "}\n",
    ));
    clean(concat!(
        "fn main() -> () {\n",
        "  var i = 0\n",
        "  while i < 3 {\n",
        "    nursery {\n",
        "      i += 1\n",
        "      break\n",
        "    }\n",
        "  }\n",
        "}\n",
    ));
}

/// A top-level initializer is its own frame even when its lazy fill is
/// triggered from inside a loop walk in another body.
#[test]
fn a_top_level_initializer_is_its_own_frame() {
    assert_eq!(check("const k = break\n"), [
        "DS2002 loop exit outside a loop"
    ]);
}

/// A value-level arm subtracts nothing from the remainder [B2
/// value-match remainder]: an arm carrying a literal test may fail at
/// run, so a later member arm checks over the full scrutinee type (it
/// may be dead, never wrong), and so do two tested struct arms over a
/// single-member scrutinee.
#[test]
fn a_value_tested_arm_subtracts_nothing_from_the_remainder() {
    clean(concat!(
        "enum shape {\n",
        "  dot\n",
        "  at(int)\n",
        "}\n",
        "fn f(s: shape) -> int {\n",
        "  match s {\n",
        "    at(1) => 1,\n",
        "    at(n) => n,\n",
        "    dot => 2,\n",
        "  }\n",
        "  0\n",
        "}\n",
    ));
    clean(concat!(
        "struct pt { x: int, y: int }\n",
        "fn f(p: pt) -> int {\n",
        "  match p {\n",
        "    pt { x: 1, y: _ } => 1,\n",
        "    pt { x: _, y: 2 } => 2,\n",
        "    other => 3,\n",
        "  }\n",
        "  0\n",
        "}\n",
    ));
}

/// A literal-`true` while whose body cannot break keeps the body's own
/// diverge flag: the loop never completes normally, so a `return` tail
/// inside it leaves through the return — the function body's tail check
/// once failed the whole loop as a `()` value.
#[test]
fn while_true_return_diverges() {
    clean("fn go() -> int {\n  while true { return 1 }\n}\n");
}

/// The same loop with a `break` inside keeps its unit value reachable:
/// the break path exits the loop normally, so the missing int tail still
/// refuses. A `for` body's break binds the `for`, never an enclosing
/// while-true, so the for variant stays diverging.
#[test]
fn while_true_break_keeps_the_unit_tail() {
    let diags = check("fn go() -> int {\n  while true { break }\n}\n");
    assert!(
        diags.iter().any(|m| m.starts_with("DS2001")),
        "the break path keeps the loop's unit value: {diags:?}"
    );
    let diags =
        check("fn go() -> int {\n  while true {\n    if true { break }\n    return 1\n  }\n}\n");
    assert!(
        diags.iter().any(|m| m.starts_with("DS2001")),
        "a conditional break keeps the loop's unit value: {diags:?}"
    );
    clean("fn go() -> int {\n  while true {\n    for i in 0..3 { break }\n    return 1\n  }\n}\n");
}

/// The for loop and a non-`true` condition never diverge: zero iterations
/// (or a false condition) complete the loop with its unit value, so an
/// early `return` inside the body cannot stand in for the fn's own tail.
#[test]
fn for_and_guarded_while_keep_the_unit_tail() {
    let diags = check("fn go() -> int {\n  for i in 0..10 { return i }\n}\n");
    assert!(
        diags.iter().any(|m| m.starts_with("DS2001")),
        "the for body's return never diverges the for: {diags:?}"
    );
    let diags = check("fn go() -> int {\n  while false { return 1 }\n}\n");
    assert!(
        diags.iter().any(|m| m.starts_with("DS2001")),
        "a false condition completes the loop with unit: {diags:?}"
    );
}

/// A `return` in a value-consumed position refuses at the checker (the
/// early exit is a statement [F2]): a block whose value feeds a binding,
/// an argument, or a join arm has no answer for the consuming slot, so
/// the seam never reaches the lowerer's dead-answer path. The statement
/// spellings — including a narrow inside a value block's head — stay
/// clean.
#[test]
fn value_position_returns_refuse_with_the_statement_ruling() {
    const MSG: &str = "DS2039 `return` is a statement; this position consumes the block's value, \
                       so the exit has no answer to feed it";
    // the if-arm const initializer that once checked clean and trapped
    assert_eq!(
        check(
            "fn f(n: int) -> int | io_err {\n  const x = if n > 0 { n } else { return io_err }\n  \
             x\n}\n"
        ),
        [MSG]
    );
    assert_eq!(
        check(
            "fn f(n: int) -> int | io_err {\n  const x = if n > 0 { return io_err } else { 1 }\n  \
             x\n}\n"
        ),
        [MSG]
    );
    // a value match arm, a bare block initializer, and a direct value slot
    assert_eq!(
        check(
            "fn f(n: int) -> int | io_err {\n  const x = match n { 0 => return io_err, _ => 1 }\n  x\n}\n"
        ),
        [MSG]
    );
    assert_eq!(
        check(
            "fn f(n: int) -> int | io_err {\n  const x = { if n > 0 { n } else { return io_err } \
             }\n  x\n}\n"
        ),
        [MSG]
    );
    assert_eq!(
        check("fn f(n: int) -> int | io_err {\n  const x = return io_err\n  x\n}\n"),
        [MSG]
    );
    // statement spellings stay clean: the bare statement, the body tail,
    // the loop body, and the narrow inside a value block's head (the
    // decode route the from_json synthesis rides)
    clean("fn f(n: int) -> int | io_err {\n  if n < 0 { return io_err }\n  n\n}\n");
    clean("fn f(n: int) -> int | io_err { return io_err }\n");
    clean("fn f(n: int) -> int | io_err {\n  while true { return io_err }\n  0\n}\n");
    clean(
        "fn pick(n: int) -> int | io_err { n }\nfn f(n: int) -> int | io_err {\n  const o = if n > 0 {\n    1\n  } else {\n    const r = pick(n)\n    if r is io_err { return io_err }\n    r\n  }\n  o\n}\n",
    );
}

#[test]
fn integer_literals_fold_the_leading_sign_into_the_i64_range() {
    // i64::MIN is writable: `-` directly over the exact min magnitude
    // folds the sign into the literal's range check, in decimal and
    // prefixed radices alike.
    clean("fn f() -> int { -9223372036854775808 }\n");
    clean("fn f() -> int { -0x8000000000000000 }\n");
    clean("fn f() -> int { -9223372036854775807 }\n");
    // The bare magnitude and anything wider refuse at the source level,
    // not at lowering.
    assert_eq!(check("fn f() -> int { 9223372036854775808 }\n"), [
        "DS1003 integer literal `9223372036854775808` does not fit the i64 range"
    ]);
    assert_eq!(check("fn f() -> int { -99999999999999999999 }\n"), [
        "DS1003 integer literal `99999999999999999999` negated does not fit the i64 range"
    ]);
    // A parenthesized magnitude keeps the bare-literal refusal: only the
    // direct negation folds.
    assert_eq!(check("fn f() -> int { -(9223372036854775808) }\n"), [
        "DS1003 integer literal `9223372036854775808` does not fit the i64 range"
    ]);
}

#[test]
fn written_type_applications_validate_arity_and_constructor_kind() {
    // A generic head applied with the wrong count names the declaration
    // and both counts, at a parameter annotation, a struct field, and an
    // `is` test alike (the prelude's `list` declares one parameter).
    assert_eq!(
        check("struct pair[t] { a: t, b: t }\nfn f(x: pair[int, string]) -> int { 0 }\n"),
        ["DS2040 `pair` declares 1 type parameter(s); the application spells 2"]
    );
    assert_eq!(check("struct s { f: list[int, string] }\n"), [
        "DS2040 `list` declares 1 type parameter(s); the application spells 2"
    ]);
    assert_eq!(
        check("fn f(x: int) -> int { if x is list[int, string] { 0 } else { 1 } }\n"),
        ["DS2040 `list` declares 1 type parameter(s); the application spells 2"]
    );
    // A non-generic head declares zero parameters; a tag never takes
    // type arguments at all.
    assert_eq!(check("fn f(x: config[int]) -> int { 0 }\n"), [
        "DS2040 `config` declares 0 type parameter(s); the application spells 1"
    ]);
    assert_eq!(check("fn f(x: int[string]) -> int { 0 }\n"), [
        "DS2040 `int` is a tag and takes no type arguments"
    ]);
    // The positive spellings stay clean: the matching counts, the map's
    // two, and a generic alias expanded at its use.
    clean("fn f(x: list[int]) -> int { 0 }\n");
    clean("struct map[k, v]\nfn f(m: map[string, int]) -> int { 0 }\n");
    clean("type rw[t] = list[t]\nfn f() -> rw[int] { list[int]() }\n");
}
