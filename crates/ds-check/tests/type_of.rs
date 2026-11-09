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
    "tag io_err\n",
    "struct config { host: string, port: int }\n",
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
    clean("const f = (x: int) -> int { x }\n");
    assert_eq!(check("const f = (x: int) -> string { x }\n"), [
        "DS2001 `lambda body` has type `int`, not contained in `string` [B2]"
    ]);
}

#[test]
fn boundary_parameters_require_annotations() {
    // Inference failure is a compile error; locals infer, boundaries do
    // not [B2, B1].
    assert_eq!(check("const f = (x) -> int { 1 }\n"), ["DS2002 parameter \
                                                        `x` has no \
                                                        expected type to \
                                                        infer from; \
                                                        annotate it [B2]"]);
    // A named function's parameters are boundaries too.
    assert_eq!(check("fn g(x) -> int { 1 }\n"), ["DS2002 parameter `x` \
                                                  needs a type \
                                                  annotation at this \
                                                  boundary [B2]"]);
}

#[test]
fn is_narrows_the_then_branch() {
    clean("const f = (x: int | string) -> int {\n  if x is int { x + 1 } else { 0 }\n}\n");
    // Without the test, the union does not support arithmetic.
    assert_eq!(check("const f = (x: int | string) -> int { x + 1 }\n"), [
        "DS2001 arithmetic needs numeric operands, `int | string` and `int` given [B7]"
    ]);
}

#[test]
fn and_right_operand_sees_the_left_narrowing() {
    // The record's own condition shape: `x is int && x >= 1` [B2].
    clean(
        "const f = (x: int | string) -> bool {\n  if x is int && x >= 1 { true } else { false \
         }\n}\n",
    );
}

#[test]
fn none_tests_narrow_both_branches() {
    clean("const f = (x: int | none) -> int {\n  if x != none { x + 1 } else { 0 }\n}\n");
    // The inverse: `== none` keeps none in the then branch.
    clean("const f = (x: int | none) -> int {\n  if x == none { 0 } else { x + 1 }\n}\n");
}

#[test]
fn refinement_resets_at_assignment() {
    // `var` is the only rebindable binding; assignment resets refinement
    // to the declared type [B2, C3].
    clean(concat!(
        "const f = (n: int | none) -> int {\n",
        "  var m: int | none = n\n",
        "  if m is none { m = 1 } else { m = 2 }\n",
        "  3\n",
        "}\n",
    ));
    // After the reset the union is back: arithmetic on it is an error.
    assert_eq!(
        check(concat!(
            "const f = (n: int | none) -> int {\n",
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
        "const f = (x: int | string | bool) -> int {\n",
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
        "const f = (x: int | string) -> int {\n",
        "  match x {\n",
        "    n if n is int => n + 1,\n",
        "    _             => 0,\n",
        "  }\n",
        "}\n",
    ));
}

#[test]
fn non_exhaustive_match_names_the_missing_set() {
    assert_eq!(
        check(concat!(
            "tag timeout\n",
            "tag refused\n",
            "const f = (x: io_err | timeout | refused) -> int {\n",
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
            "const f = (x: io_err | timeout | refused) -> int {\n",
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
fn or_return_extracts_none_from_optionals() {
    // `none` is always the extractable member [B4]; the early exit's value
    // must be contained in the return union [E4].
    clean(
        "tag parse_err\nconst f = (x: int | none) -> int | parse_err {\n  x or return \
         parse_err\n}\n",
    );
    assert_eq!(check("const f = (x: string) -> int { x or return 0 }\n"), [
        "DS2002 `or return` extracts nothing from `string` against the return union `int` [E4]"
    ]);
}

#[test]
fn try_propagation_checks_containment() {
    // The ok part survives, the error members must be contained [E4]:
    // at an expected position the split runs by the expectation, and
    // every propagated member must be carried by the return union.
    clean("const f = (x: int | io_err) -> string | io_err { str(x?) }\n");
    assert_eq!(
        check(concat!(
            "const f = (x: int | io_err) -> config {\n",
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
    clean("const f = (a: int, b: int) -> int { a +% b }\n");
    clean("const f = (a: float, b: float) -> float { a + b }\n");
    // Wrapping operators are integer-only [B7].
    assert_eq!(
        check("const f = (a: float, b: float) -> float { a +% b }\n"),
        ["DS2001 wrapping operators are integer-only; floats are IEEE [B7]"]
    );
    // Mixed int/float arithmetic suggests the explicit conversion [B7].
    let diags = check("const f = (a: int, b: float) -> float { a + b }\n");
    assert_eq!(diags.as_slice(), ["DS2001 mixed numeric operands `int` \
                                   and `float`; conversions are explicit \
                                   [B7]"]);
    // Suffixes name their atoms: unsuffixed is int, `f32` needs the atom.
    clean("const f = () -> int { 1 + 2 }\n");
    assert_eq!(check("const f = () -> float { 1.5f32 }\n"), [
        "DS2002 `float literal suffix` needs a declared `f32` type; inference never falls back \
         [B1, B7]"
    ]);
}

#[test]
fn structural_equality_carve_outs() {
    clean("const f = (a: int, b: int) -> bool { a == b }\n");
    clean("const same = config { host: \"h\", port: 1 } == config { host: \"h\", port: 1 }\n");
    // Disjoint operands make `==` constant: rejected [B9].
    assert_eq!(
        check("const f = (a: int, b: string) -> bool { a == b }\n"),
        ["DS2001 `int` and `string` share no member; `==` would be constant [B9]"]
    );
    // Function comparison is rejected statically [B9 carve-out 1].
    assert_eq!(
        check("const f = (g: fn(int) -> int) -> bool { g == g }\n"),
        ["DS2001 function comparison is rejected [B9 carve-out 1]"]
    );
}

#[test]
fn interpolation_holes_are_string_operands() {
    clean("const s = \"count: ${str(1)}\"\n");
    assert_eq!(check("const s = \"count: ${1}\"\n"), [
        "DS2001 interpolation hole has type `int`; call `str()` at the boundary [F6]"
    ]);
}

#[test]
fn method_selection_through_instances_and_assumptions() {
    // A concrete receiver resolves through the instance environment [B6].
    clean(concat!(
        "trait show {\n",
        "  const text = (self: self) -> string\n",
        "}\n",
        "impl show for int {\n",
        "  const text = (self: self) -> string { str(self) }\n",
        "}\n",
        "const f = (x: int) -> string { x.text() }\n",
    ));
    // No instance: an unsatisfied constraint, not any fallback.
    assert_eq!(
        check(concat!(
            "trait show {\n",
            "  const text = (self: self) -> string\n",
            "}\n",
            "const f = (x: int) -> string { x.text() }\n",
        ))
        .last()
        .map(String::as_str),
        Some("DS2013 no instance of `show` for `int`")
    );
    // A generic bound carries the assumption into the body [B6].
    clean(concat!(
        "trait show {\n",
        "  const text = (self: self) -> string\n",
        "}\n",
        "const shown = [t: show] (x: t) -> string { x.text() }\n",
    ));
    // Two traits, both satisfied: true ambiguity [F3].
    assert_eq!(
        check(concat!(
            "trait show {\n",
            "  const text = (self: self) -> string\n",
            "}\n",
            "trait render {\n",
            "  const text = (self: self) -> string\n",
            "}\n",
            "impl show for int { }\n",
            "impl render for int { }\n",
            "const f = (x: int) -> string { x.text() }\n",
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
    clean("const f = (host: string) -> config {\n  config { host, port: 1 }\n}\n");
}

#[test]
fn field_access_needs_one_struct() {
    clean("const f = (c: config) -> int { c.port }\n");
    assert_eq!(check("const f = (c: config | int) -> int { c.port }\n"), [
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
    clean("const f = (x: int?) -> int | none { x }\n");
    clean("const g = (x: int | none) -> int? { x }\n");
}
