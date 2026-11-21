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

/// A refused derive argument keeps the attribute unconsumed, so
/// lowering is blocked by the checker's diagnostics, not by the uop
/// contract: the refused family rides `==` (the B9 refusal every list
/// carries whatever else it names).
#[test]
fn a_refused_family_blocks_lowering_through_the_diagnostics() {
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
        "#[derive(hash, show, ==)]\n",
        "struct point { x: int, y: int }\n",
        "fn main() -> int { 0 }\n",
    );
    let rendered = refusals(source);
    assert_eq!(
        rendered.len(),
        1,
        "hash and show synthesize; only `==` refuses: {rendered:?}"
    );
    assert!(
        rendered[0].contains("DS2027"),
        "the B9 refusal: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("`==` is universal structural equality and is never derived")),
        "{rendered:?}"
    );
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
/// type-expression key reaches but the head answers no member for (a
/// bare name or an application whose head carries no `default` member)
/// fails at the synthesized call with the standard diagnostic (the
/// family precedent), and the attribute-level classes — a union naming
/// its un-annotated ambiguity, a union inside the constructor brackets,
/// the shapes spelling no key, a payloaded or empty enum, a generic
/// declaration — refuse naming the part or the declaration.
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

    // a two-argument application whose head is not `map` dispatches the
    // applied type's own `default` member (the instantiated receiverless
    // call); an impl-less head fails at the synthesized call with the
    // standard missing-member diagnostic, the family precedent
    let non_map_pair = concat!(
        "tag int\n",
        "tag string\n",
        "struct pair[a, b] { x: a, y: b }\n",
        "#[derive(default)]\n",
        "struct holder { p: pair[int, string] }\n",
    );
    let rendered = refusals(non_map_pair);
    let [one] = rendered.as_slice() else {
        panic!("one missing-member refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2004") && one.contains("type `pair` carries no member `default`"),
        "a non-map two-argument head dispatches its own member, the missing one reporting: {one}"
    );

    // an application of three or more type arguments dispatches the
    // same way, whatever the arity
    let triple_apply = concat!(
        "tag int\n",
        "struct triple[a, b, c] { x: a, y: b, z: c }\n",
        "#[derive(default)]\n",
        "struct holder { t: triple[int, int, int] }\n",
    );
    let rendered = refusals(triple_apply);
    let [one] = rendered.as_slice() else {
        panic!("one missing-member refusal: {rendered:?}")
    };
    assert!(
        one.contains("type `triple` carries no member `default`"),
        "a three-argument application dispatches its own member: {one}"
    );

    // a type argument the constructor brackets themselves cannot spell
    // rides the fn-type carrier: the empty construction never builds an
    // element, so an optional element needs no default of its own and
    // the field checks clean
    let optional_arg = concat!(
        "tag int\n",
        "struct list[t]\n",
        "#[derive(default)]\n",
        "struct holder { xs: list[int?] }\n",
        "fn main() -> int { 0 }\n",
    );
    let rendered = refusals(optional_arg);
    assert!(
        rendered.is_empty(),
        "an optional inside the brackets constructs empty: {rendered:?}"
    );

    // a union inside the brackets keeps the argument refusal — no
    // clause lifts it
    let union_arg = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "#[derive(default)]\n",
        "struct holder { xs: list[int | string] }\n",
    );
    let rendered = refusals(union_arg);
    let [one] = rendered.as_slice() else {
        panic!("one bracket-argument refusal: {rendered:?}")
    };
    assert!(
        one.contains("the constructor brackets spell no key for this type argument"),
        "a union inside the brackets refuses naming the argument: {one}"
    );

    // a one-argument head the language answers no empty construction
    // for dispatches the same way, the family precedent
    let applied_fields = concat!(
        "tag int\n",
        "struct box[t] { v: t }\n",
        "#[derive(default)]\n",
        "struct holder { b: box[int] }\n",
        "fn main() -> int { 0 }\n",
    );
    let rendered = refusals(applied_fields);
    let [one] = rendered.as_slice() else {
        panic!("one missing-member refusal: {rendered:?}")
    };
    assert!(
        one.contains("type `box` carries no member `default`"),
        "a fields-carrying head dispatches its own member, the missing one reporting: {one}"
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

/// The compound-field lift's acceptance: a field typed as an
/// application the collection spellings do not serve — two arguments
/// over a non-`map` head, three arguments, whatever the arity —
/// defaults through the applied type's own `default` member (the
/// instantiated receiverless call), so a head whose generic impl
/// carries the member synthesizes and CHECKS clean. Lowering that file
/// refuses inside the impl's own body: constructing a generic
/// struct's value spans the per-instantiation frames the runtime seam
/// keeps out of the interpreter (the pre-existing M3 boundary, the
/// same one a hand-written construction of the same value meets), so
/// the derive's own product is honest while the body's seam waits on
/// the JIT tier.
#[test]
fn an_impl_carrying_application_field_checks_through_the_instantiated_call() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag bool\n",
        "struct pair[t, u]\n",
        "struct triple[t, u, v]\n",
        "impl[t, u] pair[t, u] {\n",
        "  fn default() -> pair[t, u] { pair { } }\n",
        "}\n",
        "impl[t, u, v] triple[t, u, v] {\n",
        "  fn default() -> triple[t, u, v] { triple { } }\n",
        "}\n",
        "#[derive(default)]\n",
        "struct holder { p: pair[int, string], t: triple[int, string, bool] }\n",
        "fn main() -> int { 0 }\n",
    );
    let rendered = refusals(source);
    assert!(
        rendered.is_empty(),
        "both application fields synthesize the instantiated calls and check clean: {rendered:?}"
    );
    let mut files = Files::new();
    let id = files.add_source("applied.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let Err(err) = lower(
        "applied",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    ) else {
        panic!("the impl bodies' own construction refuses under the M3 contract")
    };
    assert!(
        err.to_string().contains("outside the M3 runtime contract"),
        "the refusal names the body's runtime seam, never the derive: {err}"
    );
}

/// The from_json family refuses its own carve-outs at the attribute,
/// each naming the family and the refusing shape: a function-typed or
/// `~Copyable` part (the shared part refusal), a generic declaration
/// (the runtime seam's one-member-body boundary), a tag (no members to
/// walk), a tuple field (a shape spelling no read over the typed
/// readers), and the one enum shape the walk cannot name (an empty
/// enum). The attribute-level refusals need no json face: the family
/// refuses before any name resolves. The optional list field — once the
/// family's own refusal — now expands clean: the decode walk mirrors
/// to_json's statement-position member row, the presence narrow's
/// catchall arm carrying the array walk. So do the tuple payload and
/// several payloaded cases (the 2026-11-21 discriminator ruling), so
/// their rows live in the json-faced e2e suites alone.
#[test]
fn the_from_json_family_refuses_its_carve_outs() {
    let rendered = refusals(concat!(
        "tag int\n",
        "trait Copyable { }\n",
        "struct list[t]\n",
        "struct wrap { v: int }\n",
        "#[derive(from_json)]\n",
        "struct handler { cb: fn(int) -> int, v: int }\n",
        "struct lease: ~Copyable { }\n",
        "#[derive(from_json)]\n",
        "struct holder { kept: lease }\n",
        "#[derive(from_json)]\n",
        "struct gen[t] { v: t }\n",
        "#[derive(from_json)]\n",
        "tag tone\n",
        "#[derive(from_json)]\n",
        "struct pair { both: (int, int) }\n",
        "#[derive(from_json)]\n",
        "enum never { }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 6, "the six refusals: {rendered:?}");
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `from_json` refuses the field `cb`: a function-typed part")),
        "{rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2026")
                && r.contains("derive `from_json` refuses the field `kept`")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2029")
            && r.contains("derive `from_json` refuses a generic declaration")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("refuses the declaration `tone`: a tag declares no members")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("refuses the field `both`: this shape spells no json read")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("refuses the enum `never`: an enum with no cases")),
        "{rendered:?}"
    );
    // The one clean derive synthesizes its impl beside the declaration;
    // `wrap` (spelled before the refusing rows) keeps the attribute
    // consumed and the plain struct after them is not attributed.
    let mut files = Files::new();
    let id = files.add_source(
        "from_json.ds",
        concat!(
            "tag int\n",
            "#[derive(from_json)]\n",
            "struct wrap { v: int }\n",
            "fn main() -> int { 0 }\n",
        ),
    );
    let asts = [parse(&files, id).expect("the probe parses")];
    let (expanded, diags) = ds_meta::expand_derives(&asts);
    assert!(
        diags.is_empty(),
        "the scalar walk refuses nothing: {diags:?}"
    );
    let grown = expanded[0]
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .count();
    assert_eq!(grown, 4, "the consumed derive appends its one impl");
    // The optional list field derives clean: the expansion appends its
    // impl beside the declaration, the statement-position member row
    // carrying the array walk the value-position else never could.
    let mut files = Files::new();
    let id = files.add_source(
        "from_json_soft.ds",
        concat!(
            "tag int\n",
            "struct list[t]\n",
            "#[derive(from_json)]\n",
            "struct soft { rows: list[int]? }\n",
            "fn main() -> int { 0 }\n",
        ),
    );
    let asts = [parse(&files, id).expect("the probe parses")];
    let (expanded, diags) = ds_meta::expand_derives(&asts);
    assert!(
        diags.is_empty(),
        "the optional list walk refuses nothing: {diags:?}"
    );
    let grown = expanded[0]
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .count();
    assert_eq!(
        grown, 5,
        "the consumed derive appends its one impl beside the four written items"
    );
}

/// The decode walk's own frame ceiling: a wide target (a nested-derive
/// member, a string, an optional member, two lists, two nested lists,
/// and an optional list) synthesizes a `from_json` body whose frame
/// needs 259 slots — three past the uop format's 256-slot cap — so
/// lowering refuses with the standard frame-cap diagnostic while the
/// checker stays clean (the width is a lowering fact, never a check
/// error). The json face rides the episode-local vocabulary (bodied
/// readers over builtin externs), so the probe measures the walk's
/// width hermetically.
#[test]
fn a_decode_walk_past_the_frame_cap_refuses_at_lowering() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag parse_err { pos: int }\n",
        "struct list[t]\n",
        "struct map[k, v]\n",
        "enum json {\n",
        "  json_null\n",
        "  json_int { value: int }\n",
        "  json_string { value: string }\n",
        "  json_array { items: list[json] }\n",
        "  json_object { fields: map[string, json] }\n",
        "}\n",
        "extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none\n",
        "extern fn list_len[t] (xs: list[t]) -> int\n",
        "pub extern fn len[t] (xs: t) -> int\n",
        "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
        "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
        "extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
        "impl[t] list[t] {\n",
        "  fn get (self, i: int) -> t | none\n",
        "  fn push (mut self, x: t) -> list[t]\n",
        "}\n",
        "impl[k, v] map[k, v] {\n",
        "  fn get (self, key: k) -> v | none\n",
        "  fn set (mut self, key: k, value: v) -> map[k, v]\n",
        "}\n",
        "fn json_member (v: json, key: string) -> json | none {\n",
        "  match v {\n",
        "    json_object { fields } => {\n",
        "      const m = map_get(fields, key)\n",
        "      m\n",
        "    },\n",
        "    _ => none,\n",
        "  }\n",
        "}\n",
        "fn json_as_int (v: json) -> int | parse_err {\n",
        "  match v {\n",
        "    json_int { value } => value,\n",
        "    _ => parse_err(pos: 0),\n",
        "  }\n",
        "}\n",
        "fn json_as_string (v: json) -> string | parse_err {\n",
        "  match v {\n",
        "    json_string { value } => value,\n",
        "    _ => parse_err(pos: 0),\n",
        "  }\n",
        "}\n",
        "fn json_as_array (v: json) -> list[json] | parse_err {\n",
        "  match v {\n",
        "    json_array { items } => items,\n",
        "    _ => parse_err(pos: 0),\n",
        "  }\n",
        "}\n",
        "fn json_as_object (v: json) -> map[string, json] | parse_err {\n",
        "  match v {\n",
        "    json_object { fields } => fields,\n",
        "    _ => parse_err(pos: 0),\n",
        "  }\n",
        "}\n",
        "#[derive(from_json)]\n",
        "struct inner { a: int }\n",
        "#[derive(from_json)]\n",
        "struct outer { i: inner, kind: string, port: int?, xs: list[int], grid: list[list[int]], \
         cols: list[list[int]], rows: list[int]? }\n",
        "fn main() -> int { 0 }\n",
    );
    let mut files = Files::new();
    let id = files.add_source("wide_decode.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the wide walk checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "wide_decode",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    ) else {
        panic!("the decode walk past the frame cap must refuse at lowering")
    };
    let LowerError::FrameTooWide { name, need } = err else {
        panic!("the standard frame-cap diagnostic owns the refusal: {err}")
    };
    assert!(
        name.contains("from_json"),
        "the refusing frame is the synthesized decode member: {name}"
    );
    assert_eq!(need, 259, "the measured walk's slot count");
}

/// The to_json family refuses from_json's mirror classes at the
/// attribute, each naming the family and the refusing shape: a
/// function-typed or `~Copyable` part (the shared part refusal), a
/// generic declaration (the runtime seam's one-member-body boundary),
/// a tag (no members to walk), a tuple field (a shape spelling no json
/// write), the enum shapes the walk cannot name (an empty enum). The
/// attribute-level refusals need no json face: the family refuses
/// before any name resolves. The
/// optional list field — the decode mirror's refusal — expands clean:
/// the encode walk's member rows are statement-position, so the
/// presence narrow carries the array walk the value-position else
/// could not (the checked pin rides the conformance gate's json-faced
/// ambient). A tuple case (lone or beside siblings) and several
/// payloaded cases encode under the 2026-11-21 discriminator ruling,
/// so their rows live in the json-faced e2e suites alone.
#[test]
fn the_to_json_family_refuses_its_carve_outs() {
    let rendered = refusals(concat!(
        "tag int\n",
        "trait Copyable { }\n",
        "struct list[t]\n",
        "struct wrap { v: int }\n",
        "#[derive(to_json)]\n",
        "struct handler { cb: fn(int) -> int, v: int }\n",
        "struct lease: ~Copyable { }\n",
        "#[derive(to_json)]\n",
        "struct holder { kept: lease }\n",
        "#[derive(to_json)]\n",
        "struct gen[t] { v: t }\n",
        "#[derive(to_json)]\n",
        "tag tone\n",
        "#[derive(to_json)]\n",
        "struct pair { both: (int, int) }\n",
        "#[derive(to_json)]\n",
        "enum never { }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 6, "the six refusals: {rendered:?}");
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("derive `to_json` refuses the field `cb`: a function-typed part")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(
            |r| r.contains("DS2026") && r.contains("derive `to_json` refuses the field `kept`")
        ),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2029")
            && r.contains("derive `to_json` refuses a generic declaration")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("refuses the declaration `tone`: a tag declares no members")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("refuses the field `both`: this shape spells no json write")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("refuses the enum `never`: an enum with no cases")),
        "{rendered:?}"
    );
    // The optional list field derives clean: the expansion appends its
    // impl beside the declaration, the statement-position member rows
    // carrying the array walk the decode mirror refuses.
    let mut files = Files::new();
    let id = files.add_source(
        "to_json.ds",
        concat!(
            "tag int\n",
            "struct list[t]\n",
            "#[derive(to_json)]\n",
            "struct soft { rows: list[int]? }\n",
            "fn main() -> int { 0 }\n",
        ),
    );
    let asts = [parse(&files, id).expect("the probe parses")];
    let (expanded, diags) = ds_meta::expand_derives(&asts);
    assert!(
        diags.is_empty(),
        "the optional list walk refuses nothing: {diags:?}"
    );
    let grown = expanded[0]
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .count();
    assert_eq!(
        grown, 5,
        "the consumed derive appends its one impl beside the four written items"
    );
}
