//! The F9.5 phase-1 re-key: tag atom identity is its declaring
//! declaration plus label, tag names left the value namespace, and the
//! directed positions resolve a bare label against the expected union's
//! members. The ambient/user pair drives the cross-file shadow a single
//! file cannot spell.

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag not_found\n",
    "pub tag permission_denied\n",
    "pub tag already_exists\n",
    "pub tag timed_out\n",
    "pub tag unexpected_eof\n",
    "pub tag io_other { msg: string }\n",
    "pub type io_err = not_found | permission_denied | already_exists | timed_out | \
     unexpected_eof | io_other\n",
    "pub tag timeout\n",
    "pub tag refused\n",
    "pub tag addr_in_use\n",
    "pub tag reset\n",
    "pub tag net_other { msg: string }\n",
    "pub type net_err = timeout | refused | addr_in_use | reset | net_other\n",
);

/// Checks the user file against the ambient stub and returns its
/// diagnostics in walk order.
fn check_user(user: &str) -> Vec<String> {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let cor = files.add_source("user.ds", user);
    let amb_ast = parse(&files, amb).expect("the ambient stub parses");
    let cor_ast = parse(&files, cor).expect("the user file parses");
    let checked = type_check(&files, &[amb_ast, cor_ast]);
    checked
        .diags
        .iter()
        .map(|d| {
            let file = d.labels.iter().find(|l| l.span.file == cor).is_some();
            format!(
                "{} {} {}",
                if file { "user" } else { "ambient" },
                d.code.as_str(),
                d.message
            )
        })
        .collect()
}

#[test]
fn a_fn_shares_the_ambient_tag_label_and_the_call_means_the_fn() {
    let diags = check_user(concat!(
        "fn timeout(n: int) -> int { n }\n",
        "const called: int = timeout(4)\n",
    ));
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_shadowing_declaration_is_a_distinct_atom() {
    // The user file shadows `refused`; its own alias body reads its own
    // atom (the directed use resolves against the one expected member),
    // while the ambient's alias expanded from the shadowing file keeps
    // the ambient's atom.
    let diags = check_user(concat!(
        "tag refused\n",
        "type my_err = refused | io_err\n",
        "const mine: my_err = refused\n",
        "const theirs: net_err = refused\n",
    ));
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn two_same_label_members_of_one_union_refuse_ambiguous() {
    let diags = check_user(concat!(
        "tag refused\n",
        "type my_err = refused | io_err\n",
        "type both = net_err | my_err\n",
        "const ambiguous: both = refused\n",
    ));
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert!(diags[0].contains("DS2046"), "{diags:?}");
    assert!(diags[0].contains("user"), "{diags:?}");
}

#[test]
fn a_declared_label_outside_the_expected_union_refuses_unknown_member() {
    // annotated binding, match arm, and `is` test at value position
    let diags = check_user(concat!(
        "const bad: io_err = refused\n",
        "fn m(e: io_err) -> string { match e { refused => \"x\", rest => \"y\" } }\n",
        "const test: bool = not_found is addr_in_use\n",
    ));
    assert_eq!(diags.len(), 3, "{diags:?}");
    for diag in &diags {
        assert!(diag.contains("DS2045"), "{diag}");
    }
}

#[test]
fn a_same_file_redeclaration_keeps_the_refusal() {
    let diags = check_user("tag refused\ntag refused\n");
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert!(diags[0].contains("DS2005"), "{diags:?}");
}

#[test]
fn the_condition_is_test_still_refines_over_a_disjoint_tag() {
    // condition position: the disjoint label narrows legitimately, no
    // unknown-member refusal fires
    let diags = check_user(concat!(
        "fn f(e: io_err) -> () { if e is refused { () } else { () } }\n",
    ));
    assert!(diags.is_empty(), "{diags:?}");
}

/// The qualified `type::label` escape hatch: each head carries exactly
/// one member of the shared label, so the qualified spellings resolve
/// clean where the bare label of the union of both would refuse
/// ambiguous.
#[test]
fn the_qualified_labels_pick_their_own_declarations() {
    let diags = check_user(concat!(
        "tag refused\n",
        "type my_err = refused | io_err\n",
        "type both = net_err | my_err\n",
        "const mine: my_err = my_err::refused\n",
        "const theirs: net_err = net_err::refused\n",
        "const from_both: both = net_err::refused\n",
    ));
    assert!(diags.is_empty(), "{diags:?}");
}

/// A head whose union carries two same-label members keeps the
/// ambiguity refusal under the qualified spelling too, naming both
/// declarations.
#[test]
fn a_qualified_label_over_an_ambiguous_head_refuses() {
    let diags = check_user(concat!(
        "tag refused\n",
        "type my_err = refused | io_err\n",
        "type both = net_err | my_err\n",
        "const bad: both = both::refused\n",
    ));
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert!(diags[0].contains("DS2046"), "{diags:?}");
    assert!(diags[0].contains("several members"), "{diags:?}");
}

/// The qualified spelling over a head the label is no member of refuses
/// unknown-member naming the head, in the value, test, and pattern
/// spellings.
#[test]
fn a_qualified_label_outside_the_head_refuses_unknown_member() {
    let diags = check_user(concat!(
        "const v: io_err = io_err::refused\n",
        "const t: bool = not_found is io_err::refused\n",
        "fn m(e: io_err) -> string { match e { io_err::refused => \"x\", rest => \"y\" } }\n",
    ));
    assert_eq!(diags.len(), 3, "{diags:?}");
    for diag in &diags {
        assert!(diag.contains("DS2045"), "{diag}");
    }
}

/// The qualified label never re-directs by an expected type: a head's
/// own member answers even where a same-label member of the expected
/// union exists.
#[test]
fn the_qualified_label_is_never_redirected_by_the_expected_type() {
    let diags = check_user(concat!(
        "tag refused\n",
        "type my_err = refused | io_err\n",
        "const theirs: net_err = my_err::refused\n",
    ));
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert!(diags[0].contains("not contained"), "{diags:?}");
}

/// The payloaded qualified spellings construct and destructure through
/// the same case doors as the bare ones.
#[test]
fn the_payloaded_qualified_label_constructs_and_matches() {
    let diags = check_user(concat!(
        "tag note { msg: string }\n",
        "type notes = note | io_err\n",
        "const n: notes = notes::note(\"x\")\n",
        "fn m(v: notes) -> string {\n",
        "  match v {\n",
        "    notes::note { msg } => msg\n",
        "    rest => \"other\"\n",
        "  }\n",
        "}\n",
    ));
    assert!(diags.is_empty(), "{diags:?}");
}
