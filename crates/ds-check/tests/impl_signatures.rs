//! Impl-member signature agreement (B6): every member of a trait impl
//! must declare what its trait member declares — arity, receiver
//! spelling, passing modes, parameter types, return, and no parameter
//! defaults of its own — because the trait member's registered signature
//! is the contract call sites check against. A divergent member refuses
//! with DS2018 at both sites; a re-declared default (the trait
//! declaration is a defaulted parameter's single authority) refuses
//! with DS2019; an impl-only default (inert, since the registered
//! signature carries no default to stage) refuses with DS2020; and a
//! trait member with neither a bodied impl member nor a trait default
//! body refuses with DS2023 at the impl's own door (call sites check
//! against the registered signature alone, so the missing body would
//! surface only at the dispatch seam).

use ds_check::analyze;
use ds_diag::Files;

fn analyze_source(name: &str, source: &str) -> ds_check::Analyzed {
    let mut files = Files::new();
    let id = files.add_source(name, source);
    let ast = ds_parse::parse(&files, id).expect("parses");
    let asts = [ast];
    analyze(&files, &asts)
}

fn messages(analyzed: &ds_check::Analyzed) -> Vec<String> {
    analyzed
        .checked
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code, d.message))
        .collect()
}

const SHOWS: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
);

#[test]
fn matching_member_signature_stays_clean() {
    let analyzed = analyze_source(
        "clean.ds",
        &format!("{SHOWS}impl shows for int {{\n  fn show(self) -> string {{ \"i\" }}\n}}\n"),
    );
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "a matching member checks clean: {rendered:?}"
    );
}

#[test]
fn divergent_return_refuses_at_both_sites() {
    let analyzed = analyze_source(
        "ret.ds",
        &format!("{SHOWS}impl shows for int {{\n  fn show(self) -> int {{ 1 }}\n}}\n"),
    );
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2018")
        .expect("the divergence reports as DS2018");
    assert!(
        diag.message
            .contains("impl member `show` returns `int`, the trait member declares `string`"),
        "the message names both returns: {}",
        diag.message
    );
    assert_eq!(diag.labels.len(), 2, "the impl member and the trait member");
    assert_eq!(
        diag.labels[1].label, "trait member signature",
        "the secondary points back at the trait member"
    );
}

#[test]
fn divergent_arity_refuses() {
    let analyzed = analyze_source(
        "arity.ds",
        &format!(
            "{SHOWS}impl shows for int {{\n  fn show(self, extra: int) -> string {{ \"i\" }}\n}}\n"
        ),
    );
    let rendered = messages(&analyzed);
    assert!(
        rendered
            .iter()
            .any(|m| m.contains("impl member `show` takes 2 parameters, the trait member takes 1")),
        "the arity divergence reports: {rendered:?}"
    );
}

#[test]
fn divergent_parameter_type_refuses() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "trait shows {\n",
        "  fn show(self, suffix: string) -> string\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self, suffix: int) -> string { \"i\" }\n",
        "}\n",
    );
    let analyzed = analyze_source("param.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.contains(
            "impl member `show`'s parameter `suffix` has type `int`, the trait member declares \
             `string`"
        )),
        "the parameter divergence reports: {rendered:?}"
    );
}

#[test]
fn divergent_receiver_spelling_refuses() {
    let source = concat!(
        "tag int\n",
        "trait shows {\n",
        "  fn show(self) -> int\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(x: int) -> int { 1 }\n",
        "}\n",
    );
    let analyzed = analyze_source("recv.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m
            .contains("impl member `show` must spell its receiver `self` like the trait member")),
        "the receiver divergence reports: {rendered:?}"
    );
}

#[test]
fn divergent_receiver_mode_refuses() {
    let source = concat!(
        "tag int\n",
        "struct counter { n: int }\n",
        "trait steps {\n",
        "  fn step(mut self) -> int\n",
        "}\n",
        "impl steps for counter {\n",
        "  fn step(self) -> int { 0 }\n",
        "}\n",
    );
    let analyzed = analyze_source("mode.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.contains(
            "parameter `self` of impl member `step` passes differently than the trait member \
             declares"
        )),
        "the mode divergence reports: {rendered:?}"
    );
}

#[test]
fn inferred_return_agreeing_with_the_declared_return_stays_clean() {
    let analyzed = analyze_source(
        "inferred_clean.ds",
        &format!("{SHOWS}impl shows for int {{\n  fn show(self) {{ \"i\" }}\n}}\n"),
    );
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "an inferred body matching the declared return checks clean: {rendered:?}"
    );
}

#[test]
fn inferred_return_refuses_when_the_declared_return_disagrees() {
    let analyzed = analyze_source(
        "inferred_ret.ds",
        &format!("{SHOWS}impl shows for int {{\n  fn show(self) {{ 1 }}\n}}\n"),
    );
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2018")
        .expect("the inferred divergence reports as DS2018");
    assert!(
        diag.message.contains(
            "impl member `show`'s body returns `int`, the trait member declares `string`"
        ),
        "the message names the body's own type: {}",
        diag.message
    );
    assert!(
        diag.labels.len() == 2,
        "the body plus the trait member signature"
    );
}

#[test]
fn trait_generic_arguments_substitute_positionally() {
    let source = concat!(
        "tag int\n",
        "tag bool\n",
        "struct box[t] { n: t }\n",
        "trait pairs[t] {\n",
        "  fn same(self, other: t) -> bool\n",
        "}\n",
        "impl[u] pairs[u] for box[u] {\n",
        "  fn same(self, other: u) -> bool { true }\n",
        "}\n",
    );
    let analyzed = analyze_source("substituted.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "the trait's `t` substituted by the reference's `u` agrees: {rendered:?}"
    );
}

#[test]
fn unsubstituted_trait_generic_refuses() {
    let source = concat!(
        "tag int\n",
        "tag bool\n",
        "struct box[t] { n: t }\n",
        "trait pairs[t] {\n",
        "  fn same(self, other: t) -> bool\n",
        "}\n",
        "impl[u] pairs for box[u] {\n",
        "  fn same(self, other: u) -> bool { true }\n",
        "}\n",
    );
    let analyzed = analyze_source("unsub.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.contains(
            "the trait declares 1 generic parameter(s); the impl's trait reference spells 0 \
             argument(s)"
        )),
        "the unspelled reference reports: {rendered:?}"
    );
}

#[test]
fn redeclared_default_refuses_at_both_sites() {
    let source = concat!(
        "tag int\n",
        "trait sized {\n",
        "  fn pad(self, by: int = 1) -> int\n",
        "}\n",
        "impl sized for int {\n",
        "  fn pad(self, by: int = 2) -> int { self + by }\n",
        "}\n",
    );
    let analyzed = analyze_source("redecl.ds", source);
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2019")
        .expect("the re-declared default reports as DS2019");
    assert!(
        diag.message.contains(
            "parameter `by` of impl member `pad` re-declares the default its trait member owns"
        ),
        "the message names the parameter and the authority: {}",
        diag.message
    );
    assert_eq!(
        diag.labels.len(),
        2,
        "the re-declared default and the trait member"
    );
    assert_eq!(
        diag.labels[0].label, "re-declared default",
        "the primary points at the impl's own default"
    );
    assert_eq!(
        diag.labels[1].label, "trait member signature",
        "the secondary points back at the trait member"
    );
}

#[test]
fn omitted_default_in_the_impl_body_stays_clean() {
    let source = concat!(
        "tag int\n",
        "trait sized {\n",
        "  fn pad(self, by: int = 1) -> int\n",
        "}\n",
        "impl sized for int {\n",
        "  fn pad(self, by: int) -> int { self + by }\n",
        "}\n",
    );
    let analyzed = analyze_source("omit.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "an impl member omitting the trait's default checks clean: {rendered:?}"
    );
}

/// The DS2019 asymmetry's other half: an impl member declaring a default
/// its trait member does not have is inert, never checked against the
/// call contract and never staged at a call site, so it refuses with its
/// own code naming the trait member having no default to override.
#[test]
fn impl_only_default_refuses_at_both_sites() {
    let source = concat!(
        "tag int\n",
        "trait sized {\n",
        "  fn pad(self, by: int) -> int\n",
        "}\n",
        "impl sized for int {\n",
        "  fn pad(self, by: int = 2) -> int { self + by }\n",
        "}\n",
    );
    let analyzed = analyze_source("implonly.ds", source);
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2020")
        .expect("the impl-only default reports as DS2020");
    assert!(
        diag.message.contains(
            "parameter `by` of impl member `pad` declares a default its trait member does not \
             have; the trait member has no default to override"
        ),
        "the message names the trait member having no default: {}",
        diag.message
    );
    assert_eq!(
        diag.labels.len(),
        2,
        "the impl-only default and the trait member"
    );
    assert_eq!(
        diag.labels[0].label, "impl-only default",
        "the primary points at the impl's own default"
    );
    assert_eq!(
        diag.labels[1].label, "trait member signature",
        "the secondary points back at the trait member"
    );
}

/// The call site keeps staging nothing for an impl-only default: the
/// dispatch call checks against the trait member's registered signature,
/// so an omitted argument stays a missing-argument refusal beside the
/// declaration's own DS2020 — the written literal never reaches a call.
#[test]
fn a_dispatch_call_stages_nothing_for_an_impl_only_default() {
    let source = concat!(
        "tag int\n",
        "trait sized {\n",
        "  fn pad(self, by: int) -> int\n",
        "}\n",
        "impl sized for int {\n",
        "  fn pad(self, by: int = 2) -> int { self + by }\n",
        "}\n",
        "fn main() -> int { 1.pad() }\n",
    );
    let analyzed = analyze_source("stagepin.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.starts_with("DS2020")),
        "the declaration refuses: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|m| m.starts_with("DS2007") && m.contains("missing argument(s): `by`")),
        "the omitted argument still refuses; the impl's default never stages: {rendered:?}"
    );
}

/// A trait member's signature declares its receiver as its first
/// parameter (F3): dynamic dispatch is receiver-carried, so a trait
/// signature with no `self` has no reachable call form and refuses at
/// its own site (DS2021 narrows to trait members — an impl member
/// without a receiver is the type-keyed static surface instead).
#[test]
fn a_trait_member_without_a_receiver_refuses() {
    let source = concat!(
        "tag int\n",
        "trait shows {\n",
        "  fn show() -> int\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show() -> int { 1 }\n",
        "}\n",
    );
    let analyzed = analyze_source("norecv.ds", source);
    let rendered = messages(&analyzed);
    let norecv: Vec<&String> = rendered
        .iter()
        .filter(|m| m.starts_with("DS2021"))
        .collect();
    assert_eq!(
        norecv.len(),
        1,
        "the trait member alone refuses (its impl member agrees with the refused signature): \
         {rendered:?}"
    );
    assert!(
        norecv[0].contains("member `show` declares its receiver as its first parameter"),
        "the refusal names the receiver seam: {norecv:?}"
    );
}

/// The call site checks through a refused member instead of panicking:
/// the declaration refusals stand alone, and neither the dot form nor
/// the qualified form adds its own diagnostic over them.
#[test]
fn a_zero_parameter_member_call_checks_through_the_refused_signature() {
    let source = concat!(
        "tag int\n",
        "trait shows {\n",
        "  fn show() -> int\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show() -> int { 1 }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const a = 2.show()\n",
        "  const b = shows::show(2)\n",
        "  a + b\n",
        "}\n",
    );
    let analyzed = analyze_source("call.ds", source);
    let rendered = messages(&analyzed);
    assert_eq!(
        rendered.len(),
        1,
        "the trait member's refusal stands alone; the calls add nothing: {rendered:?}"
    );
    assert!(
        rendered.iter().all(|m| m.starts_with("DS2021")),
        "no call-site cascade: {rendered:?}"
    );
}

/// An inherent impl member may omit the receiver (the 2026-11-15
/// ruling): the member is the type-keyed static surface, callable as
/// `<type>.<member>()`, and its declaration checks clean.
#[test]
fn an_inherent_member_without_a_receiver_checks_clean() {
    let source = concat!(
        "tag int\n",
        "struct counter { n: int }\n",
        "impl counter {\n",
        "  fn five() -> int { 5 }\n",
        "}\n",
    );
    let analyzed = analyze_source("inherent.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "the receiverless inherent member is legal: {rendered:?}"
    );
}

#[test]
fn partial_impl_missing_member_refuses_at_the_impl() {
    let analyzed = analyze_source("missing.ds", &format!("{SHOWS}impl shows for int {{\n}}\n"));
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2023")
        .expect("the incomplete impl reports as DS2023");
    assert!(
        diag.message.contains(
            "the impl of `int` implements no `show` member, and the trait member declares no \
             default body"
        ),
        "the message names the missing member and the absent default: {}",
        diag.message
    );
    assert_eq!(diag.labels.len(), 2, "the impl and the trait member");
    assert_eq!(
        diag.labels[1].label, "trait member with no default body",
        "the secondary points back at the trait member"
    );
}

#[test]
fn bodiless_impl_member_refuses() {
    let analyzed = analyze_source(
        "bodiless.ds",
        &format!("{SHOWS}impl shows for int {{\n  fn show(self) -> string\n}}\n"),
    );
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2023")
        .expect("the bodiless impl member reports as DS2023");
    assert!(
        diag.message.contains(
            "impl member `show` declares no body, and its trait member declares no default body"
        ),
        "the message names the bodiless member: {}",
        diag.message
    );
}

#[test]
fn omitted_defaulted_member_stays_clean() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "trait shows {\n",
        "  fn show(self) -> string { \"d\" }\n",
        "}\n",
        "impl shows for int {\n",
        "}\n",
    );
    let analyzed = analyze_source("defaulted.ds", source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "a member whose trait declares a default body may be omitted by the impl: {rendered:?}"
    );
}

/// A member signature naming the trait's own generics checks its
/// arguments against the dispatch's substitution: the receiver's solved
/// instance fixes what the generic holds at the site (the impl's trait
/// reference rewritten against the receiver), so an argument the
/// substitution does not contain refuses — a freely bound generic once
/// accepted any argument, for a spelled impl member and a defaulted one
/// alike.
#[test]
fn member_arguments_check_against_the_substituted_trait_generics() {
    let story = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "trait tells[t: shows] {\n",
        "  fn tell(self, other: t) -> string { other.show() }\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"int!\" }\n",
        "}\n",
    );
    let spelled = analyze_source(
        "spelled.ds",
        &format!(
            "{story}{}",
            concat!(
                "impl tells[int] for string {\n",
                "  fn tell(self, other: t) -> string { \"x\" }\n",
                "}\n",
            )
        ),
    );
    assert!(
        messages(&spelled)
            .iter()
            .any(|m| m.contains("unknown type `t`")),
        "an impl member cannot name the trait's generic itself: {:?}",
        messages(&spelled)
    );
    let defaulted = analyze_source(
        "defaulted.ds",
        &format!(
            "{story}{}",
            concat!(
                "impl[u: shows] tells[u] for list[u] {\n",
                "}\n",
                "fn main() -> int {\n",
                "  [3].tell(\"b\")\n",
                "  0\n",
                "}\n",
            )
        ),
    );
    assert!(
        messages(&defaulted)
            .iter()
            .any(|m| m.contains("has type `string`, not contained in `int`")),
        "the defaulted member's argument checks against the substituted generic: {:?}",
        messages(&defaulted)
    );
    let matching = analyze_source(
        "matching.ds",
        &format!(
            "{story}{}",
            concat!(
                "impl[u: shows] tells[u] for list[u] {\n",
                "}\n",
                "fn main() -> int {\n",
                "  [3].tell(4)\n",
                "  0\n",
                "}\n",
            )
        ),
    );
    assert!(
        messages(&matching).is_empty(),
        "a contained argument checks clean: {:?}",
        messages(&matching)
    );
}
