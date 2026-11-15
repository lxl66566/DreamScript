//! The derive-boundary honesty probes: a derive attribute the ruled set
//! has not implemented (or never admits) refuses at the CHECKER with a
//! structured diagnostic naming it — never a silent drop — so lowering
//! is never reached with an unconsumed attribute (`NotChecked`). A
//! derive attribute reaching lowering unconsumed means the trees were
//! not the analyzer's own, and the guard names that misuse; a file
//! whose items carry no derive lowers as before.

use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_meta::analyze;
use ds_mod::Hash128;
use ds_parse::parse;

const PLAIN: &str = concat!(
    "tag int\n",
    "struct point { x: int, y: int }\n",
    "fn main() -> int { 0 }\n",
);

/// Every refusal diagnostic of one source, rendered.
fn refusals(source: &str) -> Vec<String> {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect()
}

/// The checker refuses a ruled-but-unimplemented family and lowering is
/// blocked by the diagnostics, not by the uop contract.
#[test]
fn an_unimplemented_family_refuses_at_the_checker() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "trait hash {\n",
        "  fn hash(self) -> int\n",
        "}\n",
        "impl hash for int {\n",
        "  fn hash(self) -> int { self }\n",
        "}\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "#[derive(hash, show, to_json)]\n",
        "struct point { x: int, y: int }\n",
        "fn main() -> int { 0 }\n",
    );
    let rendered = refusals(source);
    assert_eq!(
        rendered.len(),
        1,
        "hash and show synthesize; only to_json refuses: {rendered:?}"
    );
    assert!(
        rendered[0].contains("DS2024"),
        "the checklist refusal: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("derive `to_json` is ruled but not implemented yet")),
        "{rendered:?}"
    );
    assert!(rendered.iter().all(|r| {
        r.contains("the ruled set is: hash, show, ord, clone, to_json, from_json, default")
    }));
    let mut files = Files::new();
    let id = files.add_source("derive.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let Err(err) = lower(
        "derive",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    ) else {
        panic!("lowering must not run over a refused package")
    };
    assert!(
        matches!(err, LowerError::NotChecked { .. }),
        "the diagnostics own the refusal: {err}"
    );
}

/// `==` in a derive list is the B9 refusal, whatever else the list
/// carries.
#[test]
fn equality_is_never_derived() {
    let rendered = refusals(concat!(
        "tag int\n",
        "#[derive(==)]\n",
        "struct cell { v: int }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 1, "one B9 refusal: {rendered:?}");
    assert!(rendered[0].contains("DS2027"), "{}", rendered[0]);
    assert!(
        rendered[0].contains("`==` is universal structural equality and is never derived"),
        "{}",
        rendered[0]
    );
}

/// A name outside the ruled set refuses naming the ruled set.
#[test]
fn an_unknown_name_refuses_naming_the_ruled_set() {
    let rendered = refusals(concat!(
        "tag int\n",
        "#[derive(eq, mystery)]\n",
        "struct cell { v: int }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 2, "each unknown name once: {rendered:?}");
    for one in &rendered {
        assert!(one.contains("DS2025"), "{one}");
        assert!(
            one.contains("the ruled set is: hash, show, ord, clone, to_json, from_json, default")
        );
    }
}

/// A function-typed or `~Copyable` part refuses naming the part.
#[test]
fn carved_out_parts_refuse_by_name() {
    let rendered = refusals(concat!(
        "tag int\n",
        "tag string\n",
        "trait Copyable { }\n",
        "#[derive(show)]\n",
        "struct handler { cb: fn(int) -> int, v: int }\n",
        "struct lease_ref: ~Copyable { }\n",
        "#[derive(show)]\n",
        "struct holder { kept: lease_ref }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 2, "the two carved parts: {rendered:?}");
    assert!(rendered[0].contains("DS2026"), "{}", rendered[0]);
    assert!(
        rendered[0].contains("refuses the field `cb`"),
        "the function-typed part is named: {}",
        rendered[0]
    );
    assert!(
        rendered[1].contains("refuses the field `kept`"),
        "the ~Copyable part is named: {}",
        rendered[1]
    );
    assert!(
        rendered[1].contains("declares the `~Copyable` linear layer"),
        "{}",
        rendered[1]
    );
}

/// A derive on a declaration that is not data refuses; an unattributed
/// package keeps lowering untouched.
#[test]
fn non_data_items_and_plain_packages() {
    let rendered = refusals(concat!(
        "tag int\n",
        "#[derive(show)]\n",
        "fn helper(v: int) -> int { v }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    assert!(rendered[0].contains("DS2028"), "{}", rendered[0]);
    assert!(
        rendered[0].contains("a derive applies to a struct, enum, or tag declaration"),
        "{}",
        rendered[0]
    );

    let mut files = Files::new();
    let id = files.add_source("plain.ds", PLAIN);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert_eq!(analyzed.checked.diags, [], "the plain package checks clean");
    lower(
        "plain",
        Hash128::compute(PLAIN.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the unattributed package lowers");
}

/// A generic declaration refuses at the attribute: the synthesized
/// member's body reads its parts through the declaration's own type
/// variables, while the runtime seam lowers one body per impl (the
/// per-instantiation frames are the JIT tier's), so the boundary is
/// named at the derive, never as a late lowering error.
#[test]
fn a_generic_declaration_refuses() {
    let rendered = refusals(concat!(
        "tag int\n",
        "#[derive(show)]\n",
        "struct cell[t] { v: t }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    assert!(rendered[0].contains("DS2029"), "{}", rendered[0]);
    assert!(
        rendered[0].contains("derive `show` refuses a generic declaration"),
        "{}",
        rendered[0]
    );
}

/// The ord family refuses its own carve-outs, each naming the family:
/// a function-typed part, a `~Copyable` part, and a generic declaration
/// at the attribute. Every primitive part carries an ambient impl, so
/// a part with no impl (a user type without one) is the honest
/// boundary: the impl synthesizes and the call fails with the standard
/// missing-impl diagnostic over the part's own span.
#[test]
fn the_ord_family_refuses_its_carve_outs() {
    let rendered = refusals(concat!(
        "tag int\n",
        "trait Copyable { }\n",
        "trait ord {\n",
        "  fn compare(self, other: Self) -> int\n",
        "}\n",
        "impl ord for int {\n",
        "  fn compare(self, other: Self) -> int {\n",
        "    if self < other { -1 } else if self > other { 1 } else { 0 }\n",
        "  }\n",
        "}\n",
        "#[derive(ord)]\n",
        "struct handler { cb: fn(int) -> int, v: int }\n",
        "struct lease: ~Copyable { }\n",
        "#[derive(ord)]\n",
        "struct holder { kept: lease }\n",
        "#[derive(ord)]\n",
        "struct gen[t] { v: t }\n",
        "struct plain { v: int }\n",
        "#[derive(ord)]\n",
        "struct named { v: plain }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 4, "the four ord refusals: {rendered:?}");
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `ord` refuses the field `cb`: a function-typed part")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `ord` refuses the field `kept`")
            && r.contains("this part carries no structural ord")),
        "the primary label names the ord family: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2029")
                && r.contains("derive `ord` refuses a generic declaration")),
        "{rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2013") && r.contains("no instance of `ord` for `plain`")),
        "{rendered:?}"
    );
}

/// The hash family refuses its own carve-outs, each naming the family:
/// a function-typed part, a `~Copyable` part, and a generic declaration
/// at the attribute. Every primitive part carries an ambient impl, so a
/// part with no impl (a user type without one) is the honest boundary:
/// the impl synthesizes and the call fails with the standard
/// missing-impl diagnostic over the part's own span.
#[test]
fn the_hash_family_refuses_its_carve_outs() {
    let rendered = refusals(concat!(
        "tag int\n",
        "trait Copyable { }\n",
        "trait hash {\n",
        "  fn hash(self) -> int\n",
        "}\n",
        "impl hash for int {\n",
        "  fn hash(self) -> int { self }\n",
        "}\n",
        "#[derive(hash)]\n",
        "struct handler { cb: fn(int) -> int, v: int }\n",
        "struct lease: ~Copyable { }\n",
        "#[derive(hash)]\n",
        "struct holder { kept: lease }\n",
        "#[derive(hash)]\n",
        "struct gen[t] { v: t }\n",
        "struct plain { v: int }\n",
        "#[derive(hash)]\n",
        "struct named { v: plain }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 4, "the four hash refusals: {rendered:?}");
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `hash` refuses the field `cb`: a function-typed part")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `hash` refuses the field `kept`")
            && r.contains("this part carries no structural hash")),
        "the primary label names the hash family: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2029")
                && r.contains("derive `hash` refuses a generic declaration")),
        "{rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2013") && r.contains("no instance of `hash` for `plain`")),
        "{rendered:?}"
    );
}

/// The clone family refuses its own carve-outs, each naming the family:
/// a function-typed part, a `~Copyable` part, and a generic declaration
/// at the attribute. Every primitive part carries an ambient-impl shape
/// in the probe, so a part with no impl (a user type without one) is the
/// honest boundary: the impl synthesizes and the call fails with the
/// standard missing-impl diagnostic over the part's own span.
#[test]
fn the_clone_family_refuses_its_carve_outs() {
    let rendered = refusals(concat!(
        "tag int\n",
        "trait Copyable { }\n",
        "trait clone {\n",
        "  fn clone(self) -> Self\n",
        "}\n",
        "impl clone for int {\n",
        "  fn clone(self) -> Self { self }\n",
        "}\n",
        "#[derive(clone)]\n",
        "struct handler { cb: fn(int) -> int, v: int }\n",
        "struct lease: ~Copyable { }\n",
        "#[derive(clone)]\n",
        "struct holder { kept: lease }\n",
        "#[derive(clone)]\n",
        "struct gen[t] { v: t }\n",
        "struct plain { v: int }\n",
        "#[derive(clone)]\n",
        "struct named { v: plain }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 4, "the four clone refusals: {rendered:?}");
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `clone` refuses the field `cb`: a function-typed part")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `clone` refuses the field `kept`")
            && r.contains("this part carries no structural clone")),
        "the primary label names the clone family: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2029")
                && r.contains("derive `clone` refuses a generic declaration")),
        "{rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2013") && r.contains("no instance of `clone` for `plain`")),
        "{rendered:?}"
    );
}

/// A derive attribute that somehow survives into the trees lowering
/// walks is the misuse guard: the checker's expansion owns consumption.
#[test]
fn an_unconsumed_attribute_refuses_at_lowering() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "#[derive(show)]\n",
        "struct point { x: int, y: int }\n",
        "fn main() -> int { 0 }\n",
    );
    let mut files = Files::new();
    let id = files.add_source("misuse.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(analyzed.checked.diags.is_empty(), "the derive synthesizes");
    let Err(err) = lower(
        "misuse",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("lowering over the unexpanded trees must refuse")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary, not a miscompile: {err}"
    );
    assert!(
        err.to_string().contains(
            "the checker's derive expansion owns it; lower over the analyzer's expanded trees"
        ),
        "{err}"
    );
}

// ---- the `default` family's refusal classes -------------------------------

/// The `default` family's honest refusals: a field type the
/// type-expression key reaches but the language answers no construction
/// for (a bare name carrying no `default` member, a one-argument head
/// that is not the empty-construction shape) fails at the synthesized
/// call with the standard diagnostic (the family precedent), and the
/// attribute-level classes — a union naming its un-annotated
/// ambiguity, the shapes spelling no key, a payloaded or empty enum, a
/// generic declaration — refuse naming the part or the declaration.
#[test]
fn the_default_family_refuses_its_boundaries() {
    let missing = concat!(
        "tag int\n",
        "struct point { x: int, y: int }\n",
        "#[derive(default)]\n",
        "struct outer { p: point }\n",
        "fn main() -> int { 0 }\n",
    );
    let rendered = refusals(missing);
    let [one] = rendered.as_slice() else {
        panic!("one missing-member refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2004") && one.contains("type `point` carries no member `default`"),
        "the part's own missing impl reports at the synthesized call: {one}"
    );

    let payloaded = concat!(
        "tag int\n",
        "#[derive(default)]\n",
        "enum keyed {\n",
        "  big(int)\n",
        "  tiny\n",
        "}\n",
    );
    let rendered = refusals(payloaded);
    let [one] = rendered.as_slice() else {
        panic!("one first-case refusal: {rendered:?}")
    };
    assert!(
        one.contains("the family defaults to the first case, which must be a unit case"),
        "a payloaded first case refuses: {one}"
    );

    let unpopulated = concat!("tag int\n", "#[derive(default)]\n", "enum void { }\n",);
    let rendered = refusals(unpopulated);
    let [one] = rendered.as_slice() else {
        panic!("one no-cases refusal: {rendered:?}")
    };
    assert!(
        one.contains("an enum with no cases has no default value"),
        "an empty enum has no default value: {one}"
    );

    // a union field: the family refuses naming the ambiguity rather
    // than inventing a member, pointing at the member annotation
    let union_field = concat!(
        "tag int\n",
        "tag string\n",
        "#[derive(default)]\n",
        "struct picker { v: int | string }\n",
    );
    let rendered = refusals(union_field);
    let [one] = rendered.as_slice() else {
        panic!("one union-field refusal: {rendered:?}")
    };
    assert!(
        one.contains("no union member is canonical; annotate one member with `#[default]`"),
        "a union field names the ambiguity instead of inventing a member: {one}"
    );

    // a two-argument application whose head is not `map`: only the map
    // spelling constructs empty
    let non_map_pair = concat!(
        "tag int\n",
        "tag string\n",
        "struct pair[a, b]\n",
        "#[derive(default)]\n",
        "struct holder { p: pair[int, string] }\n",
    );
    let rendered = refusals(non_map_pair);
    let [one] = rendered.as_slice() else {
        panic!("one two-argument-head refusal: {rendered:?}")
    };
    assert!(
        one.contains(
            "a two-argument application constructs empty only through the `map[k, v]()` spelling"
        ),
        "a non-map two-argument head refuses naming the spelling: {one}"
    );

    // an application of more type arguments than any empty construction
    // spells
    let triple_apply = concat!(
        "tag int\n",
        "struct triple[a, b, c]\n",
        "#[derive(default)]\n",
        "struct holder { t: triple[int, int, int] }\n",
    );
    let rendered = refusals(triple_apply);
    let [one] = rendered.as_slice() else {
        panic!("one arity refusal: {rendered:?}")
    };
    assert!(
        one.contains("the empty construction spells no application of 3 type arguments"),
        "a three-argument application refuses naming the arity: {one}"
    );

    // a type argument the constructor brackets themselves cannot spell
    let optional_arg = concat!(
        "tag int\n",
        "struct list[t]\n",
        "#[derive(default)]\n",
        "struct holder { xs: list[int?] }\n",
    );
    let rendered = refusals(optional_arg);
    let [one] = rendered.as_slice() else {
        panic!("one bracket-argument refusal: {rendered:?}")
    };
    assert!(
        one.contains("the constructor brackets spell no key for this type argument"),
        "an optional inside the brackets refuses naming the argument: {one}"
    );

    // a one-argument head the language answers no empty construction
    // for fails at the synthesized call, the family precedent
    let applied_fields = concat!(
        "tag int\n",
        "struct box[t] { v: t }\n",
        "#[derive(default)]\n",
        "struct holder { b: box[int] }\n",
        "fn main() -> int { 0 }\n",
    );
    let rendered = refusals(applied_fields);
    let [one] = rendered.as_slice() else {
        panic!("one cross-spelling refusal: {rendered:?}")
    };
    assert!(
        one.contains("constructs per its declaration, not by type application"),
        "a fields-carrying head refuses at the synthesized call: {one}"
    );

    let callable = concat!(
        "tag int\n",
        "#[derive(default)]\n",
        "struct hook { f: fn(int) -> int }\n",
    );
    let rendered = refusals(callable);
    let [one] = rendered.as_slice() else {
        panic!("one function-typed refusal: {rendered:?}")
    };
    assert!(
        one.contains("refuses the field `f`"),
        "a function-typed part refuses: {one}"
    );

    let generic = concat!(
        "tag int\n",
        "#[derive(default)]\n",
        "struct box[t] { v: t }\n",
    );
    let rendered = refusals(generic);
    let [one] = rendered.as_slice() else {
        panic!("one generic refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2029") && one.contains("derive `default` refuses a generic declaration"),
        "the one-body-per-impl seam refuses: {one}"
    );
}
