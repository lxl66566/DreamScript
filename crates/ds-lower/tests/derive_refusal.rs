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

/// A generic struct passes the attribute: `impl_item` synthesizes the
/// bounded generic impl and the file checks clean (the parts dispatch
/// through the bound's evidence), so the refusal the old attribute
/// gate owned is the runtime seam's — lowering refuses the member's
/// receiver frame, the same door the hand-written mirrors below pin.
/// A written bound on the declaration derives the same way: the
/// synthesis intersects it with the family's trait and the checker
/// reads every arm of the intersection.
#[test]
fn a_generic_struct_derives_and_meets_the_runtime_door() {
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "#[derive(show)]\n",
        "struct pair[t] { a: t, b: t }\n",
        "fn main() -> int { 0 }\n",
    ));
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "#[derive(show)]\n",
        "struct box[t: show] { v: t }\n",
        "fn main() -> int { 0 }\n",
    ));
}

/// The ord family refuses its own carve-outs, each naming the family:
/// a function-typed part and a `~Copyable` part. Every primitive part
/// carries an ambient impl, so a part with no impl (a user type
/// without one) is the honest boundary: the impl synthesizes and the
/// call fails with the standard missing-impl diagnostic over the
/// part's own span. A generic struct carries no carve-out: the parts
/// dispatch through the added bound and the runtime seam owns the
/// refusal.
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
    assert_eq!(rendered.len(), 3, "the three ord refusals: {rendered:?}");
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
            .any(|r| r.contains("DS2013") && r.contains("no instance of `ord` for `plain`")),
        "{rendered:?}"
    );
}

/// The hash family refuses its own carve-outs, each naming the family:
/// a function-typed part and a `~Copyable` part. Every primitive part
/// carries an ambient impl, so a part with no impl (a user type
/// without one) is the honest boundary: the impl synthesizes and the
/// call fails with the standard missing-impl diagnostic over the
/// part's own span.
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
    assert_eq!(rendered.len(), 3, "the three hash refusals: {rendered:?}");
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
            .any(|r| r.contains("DS2013") && r.contains("no instance of `hash` for `plain`")),
        "{rendered:?}"
    );
}

/// The clone family refuses its own carve-outs, each naming the family:
/// a function-typed part and a `~Copyable` part. Every primitive part
/// carries an ambient-impl shape in the probe, so a part with no impl
/// (a user type without one) is the honest boundary: the impl
/// synthesizes and the call fails with the standard missing-impl
/// diagnostic over the part's own span.
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
    assert_eq!(rendered.len(), 3, "the three clone refusals: {rendered:?}");
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
            .any(|r| r.contains("DS2013") && r.contains("no instance of `clone` for `plain`")),
        "{rendered:?}"
    );
}

/// The generic-enum cause, per family: each of the seven refuses a
/// generic enum at the attribute — the synthesized member's match arms
/// and constructions read the cases over the declaration's own type
/// variables, and a case over a generic head resolves no atom in any
/// spelling (the synthesized unit-case arms fail too, where a
/// hand-written impl passes), so the pattern seam is the door, ahead
/// of any runtime question and before any trait or json face needs to
/// resolve. One refusal per family names its own family.
#[test]
fn the_generic_enum_cause_refuses_per_family() {
    let rendered = refusals(concat!(
        "tag int\n",
        "#[derive(show, ord, hash, clone, default, from_json, to_json)]\n",
        "enum keyed[t] {\n",
        "  tiny,\n",
        "  big(t),\n",
        "}\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 7, "one refusal per family: {rendered:?}");
    for family in [
        "show",
        "ord",
        "hash",
        "clone",
        "default",
        "from_json",
        "to_json",
    ] {
        assert!(
            rendered.iter().any(|r| r.contains("DS2029")
                && r.contains(&format!("derive `{family}` refuses a generic enum"))),
            "the {family} family names its own refusal: {rendered:?}"
        );
    }
}

/// One lift probe through the whole pipeline: the source checks clean
/// and lowering refuses, asserting the refusal text names the M3
/// contract — the boundary the lifted families' products meet.
fn lift_mirror_refuses_at_lowering(source: &str) {
    let mut files = Files::new();
    let id = files.add_source("mirror.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the mirror checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "mirror",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    ) else {
        panic!("the mirror's product must refuse under the M3 contract")
    };
    assert!(
        err.to_string().contains("outside the M3 runtime contract"),
        "the refusal names the runtime value seam, never the derive: {err}"
    );
}

/// The `show` family's lift door, pinned as the hand-written mirror of
/// exactly what the admitted derive synthesizes (the bounded generic
/// impl `impl_item` builds; the real-derive pins above ride the same
/// door): a fields-carrying generic struct's
/// impl checks clean — the receiver `self: pair[t]` is a solved head and
/// the parts dispatch through the bound's runtime evidence — while
/// lowering refuses at the member's receiver frame, because a concrete
/// application of a user generic struct has no runtime value: the shape
/// pass admits only heap-reference applications (one Ref word), so the
/// ONE body the seam lowers per impl would need the receiver laid out
/// per instantiation at the entering site — the JIT specialization
/// tier's frame, not the interpreter's.
#[test]
fn the_lifted_show_product_refuses_at_the_receiver_frame() {
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "struct pair[t] { a: t, b: t }\n",
        "impl[t: show] show for pair[t] {\n",
        "  fn show(self) -> string {\n",
        "    \"pair { a: ${self.a.show()}, b: ${self.b.show()} }\"\n",
        "  }\n",
        "}\n",
        "fn main() -> int { 0 }\n",
    ));
    // the enum shape meets the same receiver frame: the member's
    // `self: flag[t]` spans the application's words whatever the body
    // reads (the arms here touch no part)
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "enum flag[t] {\n",
        "  on,\n",
        "  off,\n",
        "}\n",
        "impl[t: show] show for flag[t] {\n",
        "  fn show(self) -> string {\n",
        "    match self {\n",
        "      on => \"on\"\n",
        "      off => \"off\"\n",
        "    }\n",
        "  }\n",
        "}\n",
        "fn main() -> int { 0 }\n",
    ));
}

/// The admitted families' real products meet the same runtime door:
/// each family over a generic struct checks clean and lowering
/// refuses — the receiver frame for the trait families (the parts
/// dispatch through the added bound), the construction for `default`
/// (a receiverless member building the application value from
/// concrete parts), the union member for `from_json` (the member's
/// answer spans the application plus `parse_err`).
#[test]
fn the_admitted_generic_families_meet_their_runtime_doors() {
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "trait ord {\n",
        "  fn compare(self, other: Self) -> int\n",
        "}\n",
        "#[derive(ord)]\n",
        "struct pair[t] { a: t, b: t }\n",
        "fn main() -> int { 0 }\n",
    ));
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "trait hash {\n",
        "  fn hash(self) -> int\n",
        "}\n",
        "#[derive(hash)]\n",
        "struct pair[t] { a: t, b: t }\n",
        "fn main() -> int { 0 }\n",
    ));
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "trait clone {\n",
        "  fn clone(self) -> Self\n",
        "}\n",
        "#[derive(clone)]\n",
        "struct pair[t] { a: t, b: t }\n",
        "fn main() -> int { 0 }\n",
    ));
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "impl int {\n",
        "  fn default() -> int { 0 }\n",
        "}\n",
        "#[derive(default)]\n",
        "struct box[t] { n: int }\n",
        "fn main() -> int { 0 }\n",
    ));
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "tag string\n",
        "struct map[k, v]\n",
        "tag parse_err { pos: int }\n",
        "enum json { jnull, jint { v: int } }\n",
        "impl json {\n",
        "  fn default() -> json { jnull }\n",
        "}\n",
        "trait decoder {\n",
        "  fn dec_object(self) -> map[string, json] | parse_err\n",
        "  fn dec_member(self, key: string) -> json | none\n",
        "  fn dec_int(self) -> int | parse_err\n",
        "}\n",
        "impl decoder for json {\n",
        "  fn dec_object(self) -> map[string, json] | parse_err { parse_err(pos: 0) }\n",
        "  fn dec_member(self, key: string) -> json | none { none }\n",
        "  fn dec_int(self) -> int | parse_err { 0 }\n",
        "}\n",
        "#[derive(from_json)]\n",
        "struct gen[t] { n: int }\n",
        "fn main() -> int { 0 }\n",
    ));
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "tag string\n",
        "struct map[k, v]\n",
        "enum json { jnull, jint { v: int } }\n",
        "impl[k, v] map[k, v] {\n",
        "  fn set (mut self, key: k, value: v) -> map[k, v]\n",
        "}\n",
        "trait encoder {\n",
        "  fn enc_int (self, value: int) -> Self\n",
        "  fn enc_object (self, fields: map[string, Self]) -> Self\n",
        "}\n",
        "impl json {\n",
        "  fn default() -> json { jnull }\n",
        "}\n",
        "impl encoder for json {\n",
        "  fn enc_int (self, value: int) -> json { jint(v: value) }\n",
        "  fn enc_object (self, fields: map[string, json]) -> json { jnull }\n",
        "}\n",
        "#[derive(to_json)]\n",
        "struct gen[t] { n: int }\n",
        "fn main() -> int { 0 }\n",
    ));
}

/// The type-keyed families' variable-part doors, the trait families'
/// contrast: no default value of a type variable exists (the key
/// refuses the bare variable and a variable inside a tuple, while a
/// `t?` or `list[t]` field constructs empty and derives clean), and
/// no json member sits behind a variable (the members are type-keyed,
/// never bound, so any part mentioning a variable refuses) — while
/// the trait families dispatch through the added bound and the same
/// `v: t` field checks clean.
#[test]
fn the_type_keyed_families_refuse_a_variable_part() {
    let rendered = refusals(concat!(
        "tag int\n",
        "#[derive(default)]\n",
        "struct gen[t] { v: t }\n",
        "fn main() -> int { 0 }\n",
    ));
    let [one] = rendered.as_slice() else {
        panic!("one default variable-part refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2026")
            && one.contains(
                "derive `default` refuses the field `v`: a type variable carries no default value"
            ),
        "{one}"
    );

    let rendered = refusals(concat!(
        "tag int\n",
        "#[derive(default)]\n",
        "struct gen[t] { p: (t, int) }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains("refuses the field `p`: a type variable carries no default value")),
        "a variable inside a tuple reads its default: {rendered:?}"
    );

    let rendered = refusals(concat!(
        "tag int\n",
        "struct list[t]\n",
        "#[derive(default)]\n",
        "struct gen[t] { xs: list[t], v: t? }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert!(
        rendered.is_empty(),
        "the empty constructions derive clean: {rendered:?}"
    );

    for family in ["from_json", "to_json"] {
        let source = format!(
            "tag int\nstruct list[t]\n#[derive({family})]\nstruct gen[t] {{ v: t, xs: list[t] \
             }}\nfn main() -> int {{ 0 }}\n"
        );
        let rendered = refusals(&source);
        assert_eq!(
            rendered.len(),
            1,
            "one {family} variable-part refusal: {rendered:?}"
        );
        assert!(
            rendered[0].contains("DS2026")
                && rendered[0].contains(&format!(
                    "derive `{family}` refuses the field `v`: a type variable carries no json \
                     member (the members are type-keyed, never bound)"
                )),
            "{}",
            rendered[0]
        );
    }
}

/// The `default` and `clone` families' lift door, the receiverless
/// mirror: a member constructing the application value from concrete
/// parts — no type variable anywhere in the body — still refuses at
/// lowering's shape pass, at the construction itself. A concrete
/// application of a user generic struct has no runtime representation
/// whatever the body reads, so even the families whose synthesized
/// bodies never dispatch through the variable wait on the same
/// per-instantiation frame seam. The checker's own construction door
/// rides beside it: a concretely annotated literal still refuses, since
/// the struct-literal arm reads in-scope type parameters alone and the
/// expected type never reaches it.
#[test]
fn the_lifted_default_product_refuses_at_the_construction() {
    lift_mirror_refuses_at_lowering(concat!(
        "tag int\n",
        "struct box[t] { n: int }\n",
        "impl[t] box[t] {\n",
        "  fn default() -> box[t] {\n",
        "    box { n: 0 }\n",
        "  }\n",
        "}\n",
        "fn main() -> int { 0 }\n",
    ));
    let rendered = refusals(concat!(
        "tag int\n",
        "struct pair[t] { a: t, b: t }\n",
        "fn main() -> int {\n",
        "  const p: pair[int] = pair { a: 1, b: 2 }\n",
        "  0\n",
        "}\n",
    ));
    assert!(
        rendered.iter().any(|r| r
            .contains("constructing generic `pair` needs an expected type or in-scope parameters")),
        "the annotated literal names the checker's own construction door: {rendered:?}"
    );
}

/// The enum families' second door, ahead of any runtime question: a
/// payloaded case pattern over a generic head resolves no atom in any
/// spelling (the hand-written impl here, a generic free fn the same),
/// while the unit-case arms above resolve — so a lifted derive over a
/// generic enum whose cases carry parts would synthesize arms the
/// checker refuses before the receiver frame is ever reached. The
/// enum-body walk (`arm` in the derive engine) needs this resolution
/// seam named and fixed first.
#[test]
fn a_payloaded_case_pattern_over_a_generic_head_refuses_at_checking() {
    let rendered = refusals(concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "enum keyed[t] {\n",
        "  tiny,\n",
        "  big(t),\n",
        "}\n",
        "impl[t: show] show for keyed[t] {\n",
        "  fn show(self) -> string {\n",
        "    match self {\n",
        "      tiny => \"tiny\"\n",
        "      big(p0) => \"big(${p0.show()})\"\n",
        "    }\n",
        "  }\n",
        "}\n",
        "fn main() -> int { 0 }\n",
    ));
    assert!(
        rendered
            .iter()
            .any(|r| r.contains("DS2004") && r.contains("unknown pattern `big`")),
        "the payloaded case resolves no atom over the generic head: {rendered:?}"
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
/// the shapes spelling no key, a payloaded or empty enum, a type
/// variable (no default value of a variable exists) — refuse naming
/// the part or the declaration.
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
        panic!("one variable-part refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2026")
            && one.contains(
                "derive `default` refuses the field `v`: a type variable carries no default value"
            ),
        "no default value of a variable exists: {one}"
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
/// `~Copyable` part (the shared part refusal), a type-variable part
/// (the json members are type-keyed, never bound), a tag (no members
/// to walk), and a tuple field (a shape spelling no read over the
/// typed readers). The attribute-level refusals need no json face: the
/// family refuses before any name resolves. The optional list field — once the
/// family's own refusal — now expands clean: the decode walk mirrors
/// to_json's statement-position member row, the presence narrow's
/// catchall arm carrying the array walk. So do the tuple payload and
/// several payloaded cases (the 2026-11-21 discriminator ruling), so
/// their rows live in the json-faced e2e suites alone; an empty enum
/// hosts its member over the name-solved head
/// (`the_json_families_host_an_empty_enum`).
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
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 5, "the five refusals: {rendered:?}");
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
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains(
                "derive `from_json` refuses the field `v`: a type variable carries no json member"
            )),
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

/// A zero-case enum's name solves to its type (the empty set), so the
/// json families host their members over the name-solved head:
/// `from_json` answers `parse_err` unconditionally (no member case
/// can match, so the body never binds the shared fail seed) and
/// `to_json` is vacuous over the impossible receiver, answering the
/// driver's empty document. Both impl heads check and the members
/// carry solvable signatures without the ambient.
#[test]
fn the_json_families_host_an_empty_enum() {
    // from_json: the body constructs one parse_err, the member's
    // signature solving `Self | parse_err` down to parse_err alone.
    let mut files = Files::new();
    let id = files.add_source(
        "from_json_empty.ds",
        concat!(
            "tag int\n",
            "tag json\n",
            "tag parse_err { pos: int }\n",
            "#[derive(from_json)]\n",
            "enum never { }\n",
            "fn main() -> int { 0 }\n",
        ),
    );
    let asts = [parse(&files, id).expect("the probe parses")];
    let (expanded, diags) = ds_meta::expand_derives(&asts);
    assert!(diags.is_empty(), "the empty enum hosts: {diags:?}");
    let grown = expanded[0]
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .count();
    assert_eq!(
        grown, 6,
        "the consumed derive appends its one impl beside the five written items"
    );
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the name-solved head checks: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| human(d, &files))
            .collect::<Vec<_>>()
    );
    // to_json: the vacuous body reads the driver's empty document
    // (`json.default()`, the driver face's convention), so the probe
    // declares the document type's own default member.
    let mut files = Files::new();
    let id = files.add_source(
        "to_json_empty.ds",
        concat!(
            "tag int\n",
            "enum json { jnull }\n",
            "impl json { fn default() -> json { jnull } }\n",
            "#[derive(to_json)]\n",
            "enum never { }\n",
            "fn main() -> int { 0 }\n",
        ),
    );
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the vacuous encode member checks: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| human(d, &files))
            .collect::<Vec<_>>()
    );
}

/// A zero-case enum hosts the trait-impl families over its name-solved
/// head — the instance core's canonical empty spelling, mirroring the
/// json families' resolution: show, hash, ord, and clone each declare a
/// vacuous instance (the head disjoint from every other head, the
/// synthesized bodies never touching the impossible receiver — show,
/// hash, ord answer constants; clone hands the receiver back), so the
/// four derive, check, and lower, while `default` keeps its own
/// attribute refusal (an empty enum has no default value). A call site
/// naming a value of the empty type stays honestly refused: no instance
/// is selected for a query no value inhabits.
#[test]
fn the_trait_families_host_an_empty_enum() {
    // The four families host; only `default` refuses, at the attribute.
    let rendered = refusals(concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "trait hash {\n",
        "  fn hash(self) -> int\n",
        "}\n",
        "trait ord {\n",
        "  fn compare(self, other: Self) -> int\n",
        "}\n",
        "trait clone {\n",
        "  fn clone(self) -> Self\n",
        "}\n",
        "#[derive(show, hash, ord, clone, default)]\n",
        "enum never { }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 1, "only default refuses: {rendered:?}");
    assert!(
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains(
                "derive `default` refuses the enum `never`: an enum with no cases has no default \
                 value"
            )),
        "{rendered:?}"
    );

    // One vacuous instance per family, each head the empty set, checking
    // and lowering clean.
    let hosted = concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "trait hash {\n",
        "  fn hash(self) -> int\n",
        "}\n",
        "trait ord {\n",
        "  fn compare(self, other: Self) -> int\n",
        "}\n",
        "trait clone {\n",
        "  fn clone(self) -> Self\n",
        "}\n",
        "#[derive(show, hash, ord, clone)]\n",
        "enum never { }\n",
        "fn main() -> int { 0 }\n",
    );
    let mut files = Files::new();
    let id = files.add_source("empty_enum_host.ds", hosted);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the name-solved heads check: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| human(d, &files))
            .collect::<Vec<_>>()
    );
    assert_eq!(analyzed.decls.instances.len(), 4, "one instance per family");
    for index in 0..4_u32 {
        let instance = ds_check::InstanceId::from_raw(index);
        let head = analyzed.decls.instances.head(instance).unwrap();
        assert_eq!(analyzed.decls.universe.render(head), "empty");
    }
    lower(
        "empty_enum_host",
        Hash128::compute(hosted.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the hosted impls lower");

    // A call site over the empty type names no dispatchable value: the
    // missing-instance diagnostic answers, the empty set rendered as
    // itself.
    let rendered = refusals(concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "#[derive(show)]\n",
        "enum never { }\n",
        "fn f(v: never) -> string { v.show() }\n",
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    assert!(
        rendered[0].contains("DS2013") && rendered[0].contains("no instance of `show` for `empty`"),
        "{}",
        rendered[0]
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
        "trait decoder {\n",
        "  fn dec_object (self) -> map[string, Self] | parse_err\n",
        "  fn dec_array (self) -> list[Self] | parse_err\n",
        "  fn dec_member (self, key: string) -> Self | none\n",
        "  fn dec_string (self) -> string | parse_err\n",
        "  fn dec_int (self) -> int | parse_err\n",
        "}\n",
        "impl json {\n",
        "  fn default() -> json { json_null }\n",
        "}\n",
        "impl decoder for json {\n",
        "  fn dec_object (self) -> map[string, json] | parse_err { json_as_object(self) }\n",
        "  fn dec_array (self) -> list[json] | parse_err { json_as_array(self) }\n",
        "  fn dec_member (self, key: string) -> json | none { json_member(self, key) }\n",
        "  fn dec_string (self) -> string | parse_err { json_as_string(self) }\n",
        "  fn dec_int (self) -> int | parse_err { json_as_int(self) }\n",
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

/// The encode walk's own frame ceiling, the decode pin's mirror: a
/// wide target (forty int fields and one nested-derive field) synthesizes
/// a `to_json` body whose frame needs 259 slots — three past the uop
/// format's 256-slot cap — so lowering refuses with the standard
/// frame-cap diagnostic while the checker stays clean. The rows stage
/// through the map's `set` member (the collection face's native seam)
/// and the nested member dispatch stages the quad field's whole
/// four-word span, the crossing alloc that pins the count; the json
/// face rides the episode-local vocabulary (signature stubs over the
/// object-map seam), so the probe measures the walk's width
/// hermetically.
#[test]
fn an_encode_walk_past_the_frame_cap_refuses_at_lowering() {
    let fields = (0..40)
        .map(|i| format!("f{i}: int"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct map[k, v]\n",
        "enum json {\n",
        "  json_null\n",
        "  json_int { value: int }\n",
        "  json_object { fields: map[string, json] }\n",
        "}\n",
        "extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
        "impl[k, v] map[k, v] {\n",
        "  fn set (mut self, key: k, value: v) -> map[k, v]\n",
        "}\n",
        "trait encoder {\n",
        "  fn enc_int (self, value: int) -> Self\n",
        "  fn enc_object (self, fields: map[string, Self]) -> Self\n",
        "}\n",
        "impl json {\n",
        "  fn default() -> json { json_null }\n",
        "}\n",
        "impl encoder for json {\n",
        "  fn enc_int (self, value: int) -> json { json_int(value: value) }\n",
        "  fn enc_object (self, fields: map[string, json]) -> json { json_object(fields: fields) \
         }\n",
        "}\n",
        "#[derive(to_json)]\n",
        "struct quad { a: int, b: int, c: int, d: int }\n",
        "#[derive(to_json)]\n",
    )
    .to_owned()
        + "struct wide {"
        + &fields
        + ", g: quad }\n"
        + "fn main() -> int { 0 }\n";
    let mut files = Files::new();
    let id = files.add_source("wide_encode.ds", &source);
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
        "wide_encode",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    ) else {
        panic!("the encode walk past the frame cap must refuse at lowering")
    };
    let LowerError::FrameTooWide { name, need } = err else {
        panic!("the standard frame-cap diagnostic owns the refusal: {err}")
    };
    assert!(
        name.contains("to_json"),
        "the refusing frame is the synthesized encode member: {name}"
    );
    assert_eq!(need, 259, "the measured walk's slot count");
}

/// The to_json family refuses from_json's mirror classes at the
/// attribute, each naming the family and the refusing shape: a
/// function-typed or `~Copyable` part (the shared part refusal), a
/// type-variable part (the json members are type-keyed, never bound),
/// a tag (no members to walk), and a tuple field (a shape spelling no
/// json write). The attribute-level refusals need no json face: the
/// family refuses before any name resolves. The
/// optional list field — the decode mirror's refusal — expands clean:
/// the encode walk's member rows are statement-position, so the
/// presence narrow carries the array walk the value-position else
/// could not (the checked pin rides the conformance gate's json-faced
/// ambient). A tuple case (lone or beside siblings) and several
/// payloaded cases encode under the 2026-11-21 discriminator ruling,
/// so their rows live in the json-faced e2e suites alone; an empty
/// enum hosts its member over the name-solved head
/// (`the_json_families_host_an_empty_enum`).
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
        "fn main() -> int { 0 }\n",
    ));
    assert_eq!(rendered.len(), 5, "the five refusals: {rendered:?}");
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
        rendered.iter().any(|r| r.contains("DS2026")
            && r.contains(
                "derive `to_json` refuses the field `v`: a type variable carries no json member"
            )),
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
