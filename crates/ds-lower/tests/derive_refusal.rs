//! The derive-boundary honesty probes: a derive attribute the ruled set
//! has not implemented (or never admits) refuses at the CHECKER with a
//! structured diagnostic naming it — never a silent drop — so lowering
//! is never reached with an unconsumed attribute (`NotChecked`). A
//! derive attribute reaching lowering unconsumed means the trees were
//! not the analyzer's own, and the guard names that misuse; a file
//! whose items carry no derive lowers as before.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
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

/// A part-less struct refuses the clone derive at the attribute: the
/// member's `Self` return carries no word, so it would lower to a
/// zero-width RETSPAN, which the ABI refuses by design (a plain fn
/// returning the same value shares the hole, a pre-existing boundary).
#[test]
fn a_partless_struct_refuses_the_clone_derive() {
    let rendered = refusals(concat!(
        "tag int\n",
        "trait clone {\n",
        "  fn clone(self) -> Self\n",
        "}\n",
        "impl clone for int {\n",
        "  fn clone(self) -> Self { self }\n",
        "}\n",
        "#[derive(clone)]\n",
        "struct unit { }\n",
        "#[derive(clone)]\n",
        "struct point { x: int }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(
        rendered.len(),
        1,
        "only the part-less struct refuses: {rendered:?}"
    );
    assert!(rendered[0].contains("DS2030"), "{}", rendered[0]);
    assert!(
        rendered[0].contains("derive `clone` refuses a part-less struct"),
        "{}",
        rendered[0]
    );
    assert!(
        rendered[0].contains("zero-width RETSPAN"),
        "the refusal names the ABI boundary: {}",
        rendered[0]
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
