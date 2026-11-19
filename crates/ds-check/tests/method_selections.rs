//! The type-directed member selection of every dot-call [F3]: the callee
//! name occurrence records the one member the checker settled on — a
//! trait member a solved instance or an in-scope assumption answers, an
//! inherent fn whose owner covers the receiver, and the type-keyed tier's
//! member alike — while a truly ambiguous call records nothing, its own
//! diagnostic owning the refusal.

use ds_check::{Analyzed, DotRes, MethodOwner, Res, analyze};
use ds_diag::{Files, Span};
use ds_parse::{Ast, parse};

fn analyze_source(name: &str, source: &str) -> (Ast, Analyzed) {
    let mut files = Files::new();
    let id = files.add_source(name, source);
    let ast = parse(&files, id).expect("parses");
    let analyzed = analyze(&files, std::slice::from_ref(&ast));
    (ast, analyzed)
}

/// The callee name spans of every recorded dot-call use, in source order.
fn dot_callees(analyzed: &Analyzed, text: &str) -> Vec<(Span, String)> {
    analyzed
        .resolutions
        .iter()
        .flat_map(ds_check::Resolutions::uses)
        .filter_map(|(span, res)| match res {
            Res::Dot(DotRes::Methods { .. } | DotRes::Static { .. }) => Some((
                *span,
                text[span.start as usize..span.end as usize].to_owned(),
            )),
            _ => None,
        })
        .collect()
}

const SELECT_SOURCE: &str = concat!(
    "tag int\n",
    "trait sized {\n",
    "  fn size(self) -> int { 0 }\n",
    "}\n",
    "struct box_a { n: int }\n",
    "impl sized for box_a {\n",
    "  fn size(self) -> int { 1 }\n",
    "}\n",
    "impl box_a {\n",
    "  fn norm(self) -> int { 2 }\n",
    "}\n",
    "impl box_a {\n",
    "  fn origin() -> int { 0 }\n",
    "}\n",
    "fn calls(a: box_a) -> () {\n",
    "  a.size()\n",
    "  a.norm()\n",
    "  box_a.origin()\n",
    "  ()\n",
    "}\n",
    "fn generic[t: sized](x: t) -> int {\n",
    "  x.size()\n",
    "}\n",
);

/// Every clean dot-call records its selected member at the callee name's
/// own span, including the assumption-satisfied site the solved-dispatch
/// table does not carry (no instance exists for a bound type variable).
#[test]
fn every_clean_dot_call_records_its_selected_member() {
    let (_ast, analyzed) = analyze_source("select.ds", SELECT_SOURCE);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the selection fixture checks clean"
    );
    let package = &analyzed.package;
    let callees = dot_callees(&analyzed, SELECT_SOURCE);
    assert_eq!(
        callees
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["size", "norm", "origin", "size"],
        "the fixture makes four dot-call callee uses"
    );
    for (span, name) in &callees {
        let &selected = analyzed
            .checked
            .method_selections
            .get(span)
            .unwrap_or_else(|| panic!("the `{name}` callee at {span:?} records no selection"));
        assert_eq!(
            &package.method(selected).name,
            name,
            "the selection names the callee's own member"
        );
    }
    // The four sites select by owner shape: the trait member twice — the
    // instance-solved call site and the assumption-satisfied generic
    // body — then the inherent receiver member, then the type-keyed
    // receiverless member.
    let owners: Vec<String> = callees
        .iter()
        .map(|(span, _)| {
            match &package
                .method(analyzed.checked.method_selections[span])
                .owner
            {
                MethodOwner::Trait(_) => "trait".to_owned(),
                MethodOwner::Inherent(target) => format!("inherent {target}"),
            }
        })
        .collect();
    assert_eq!(
        owners,
        ["trait", "inherent box_a", "inherent box_a", "trait"],
        "{owners:?}"
    );
    // Only the instance-solved site carries a dispatch record: the
    // generic body's assumption answers no instance, so the selection
    // table is the one place its member is named.
    assert_eq!(
        analyzed.checked.trait_calls.len(),
        1,
        "the assumption-satisfied site records no solved dispatch"
    );
    assert_eq!(
        analyzed.checked.method_selections.len(),
        4,
        "every clean dot-call selects exactly one member"
    );
    // The type-keyed call also records the static dispatch record at the
    // call's own span, mirroring the value-receiver inherent tier.
    assert_eq!(analyzed.checked.inherent_calls.len(), 2);
}

const AMBIGUOUS_SOURCE: &str = concat!(
    "tag int\n",
    "trait a {\n",
    "  fn f(self) -> int { 1 }\n",
    "}\n",
    "trait b {\n",
    "  fn f(self) -> int { 2 }\n",
    "}\n",
    "tag unit\n",
    "impl a for unit {\n",
    "  fn f(self) -> int { 1 }\n",
    "}\n",
    "impl b for unit {\n",
    "  fn f(self) -> int { 2 }\n",
    "}\n",
    "fn call(v: unit) -> int {\n",
    "  v.f()\n",
    "}\n",
);

/// A dot-call whose candidate set type-direction cannot settle records no
/// selection: the checker's own ambiguity diagnostic owns the refusal, so
/// no member is named behind the callee occurrence.
#[test]
fn an_ambiguous_dot_call_records_no_selection() {
    let (_ast, analyzed) = analyze_source("ambiguous.ds", AMBIGUOUS_SOURCE);
    assert!(
        analyzed
            .checked
            .diags
            .iter()
            .any(|diag| diag.code.as_str() == "DS2014"),
        "the checker refuses the truly ambiguous call"
    );
    let callees = dot_callees(&analyzed, AMBIGUOUS_SOURCE);
    assert_eq!(callees.len(), 1, "{callees:?}");
    assert!(
        !analyzed
            .checked
            .method_selections
            .contains_key(&callees[0].0),
        "an unsettled call names no member"
    );
}
