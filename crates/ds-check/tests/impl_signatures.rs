//! Impl-member signature agreement (B6): every member of a trait impl
//! must declare what its trait member declares — arity, receiver
//! spelling, passing modes, parameter types, return — because the trait
//! member's registered signature is the contract call sites check
//! against. A divergent member refuses with DS2018 at both sites.

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
