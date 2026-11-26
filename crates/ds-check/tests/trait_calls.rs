//! The solved-dispatch side table of trait-method call sites: every
//! call whose instance the environment statically resolves records the
//! selected member, the trait, the unique instance, the declaring impl
//! item, and the receiver's type, keyed by the call's own span. Sites an
//! in-scope assumption covers record nothing — they are the
//! runtime-resolved evidence form.

use ds_check::{Analyzed, Res, analyze};
use ds_diag::{Files, LabelStyle, Span};
use ds_parse::{Ast, Expr, ItemKind, parse};

fn analyze_source(name: &str, source: &str) -> (Ast, Analyzed) {
    let mut files = Files::new();
    let id = files.add_source(name, source);
    let ast = parse(&files, id).expect("parses");
    let analyzed = analyze(&files, std::slice::from_ref(&ast));
    (ast, analyzed)
}

/// The spans of every call expression inside top-level function bodies,
/// in source order.
fn call_spans(ast: &Ast) -> Vec<Span> {
    fn expr(e: &Expr, out: &mut Vec<Span>) {
        if let Expr::Call {
            span, callee, args, ..
        } = e
        {
            out.push(*span);
            expr(callee, out);
            for arg in args {
                expr(&arg.value, out);
            }
            return;
        }
        if let Expr::Field { recv, .. } = e {
            expr(recv, out);
        }
    }
    let mut out = Vec::new();
    for item in ast.blocks.iter().flat_map(|b| b.items.iter()) {
        if let ItemKind::Local(l) = &item.kind
            && let Expr::Lambda {
                body: lambda_body, ..
            } = &l.init
            && let Expr::Block(b) = &**lambda_body
        {
            for stmt in &b.stmts {
                if let ds_parse::StmtKind::Expr(e) = &stmt.kind {
                    expr(e, &mut out);
                }
            }
        }
    }
    out
}

/// The spans of every impl item, in source order.
fn impl_spans(ast: &Ast) -> Vec<Span> {
    ast.blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .filter(|item| matches!(item.kind, ItemKind::Impl(_)))
        .map(|item| item.span)
        .collect()
}

const DISPATCH_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct box_a { n: int }\n",
    "struct box_b { n: int }\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for box_a {\n",
    "  fn show(self) -> string { \"a\" }\n",
    "}\n",
    "impl showable for box_b {\n",
    "  fn show(self) -> string { \"b\" }\n",
    "}\n",
    "fn calls(a: box_a, b: box_b) -> () {\n",
    "  a.show()\n",
    "  b.show()\n",
    "  showable::show(b)\n",
    "  ()\n",
    "}\n",
);

/// Two impls of one trait record their own solved instance at the call
/// site, and the qualification spelling records the same evidence as the
/// dot-call on the same receiver. The instance ties back to the declaring
/// impl item's span, the identity the dispatch seam reaches the bodies
/// through.
#[test]
fn dot_calls_record_one_solved_instance_per_impl() {
    let (ast, analyzed) = analyze_source("dispatch.ds", DISPATCH_SOURCE);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the dispatch fixture checks clean"
    );
    let spans = call_spans(&ast);
    assert_eq!(spans.len(), 3, "the fixture makes three calls");
    let universe = &analyzed.decls.universe;
    let first = &analyzed.checked.trait_calls[&spans[0]];
    let second = &analyzed.checked.trait_calls[&spans[1]];
    let qualified = &analyzed.checked.trait_calls[&spans[2]];
    assert_eq!(universe.render(first.recv), "box_a");
    assert_eq!(universe.render(second.recv), "box_b");
    assert_ne!(
        first.instance, second.instance,
        "distinct receivers solve distinct instances"
    );
    assert_eq!(
        second.instance, qualified.instance,
        "the qualified spelling records the same evidence"
    );
    assert_eq!(first.trait_id, second.trait_id);
    let impls = impl_spans(&ast);
    assert_eq!(impls.len(), 2);
    assert_eq!(
        first.impl_site,
        Some(impls[0]),
        "the box_a receiver ties to the box_a impl"
    );
    assert_eq!(
        second.impl_site,
        Some(impls[1]),
        "the box_b receiver ties to the box_b impl"
    );
}

const FREE_FIRST_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct box_a { n: int }\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for box_a {\n",
    "  fn show(self) -> string { \"trait\" }\n",
    "}\n",
    "fn show(x: box_a) -> string { \"free\" }\n",
    "fn calls(a: box_a) -> () {\n",
    "  a.show()\n",
    "  ()\n",
    "}\n",
);

/// When a free function and a trait method share the dotted name, the
/// free tier wins and the call site records no trait dispatch.
#[test]
fn free_functions_win_the_dot_position() {
    let (ast, analyzed) = analyze_source("free_first.ds", FREE_FIRST_SOURCE);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the free-first fixture checks clean"
    );
    assert!(
        analyzed.checked.trait_calls.is_empty(),
        "the free function won, so no trait dispatch is recorded"
    );
    // the dot position itself resolved to the free function
    let res = &analyzed.resolutions[0];
    let mut dot = None;
    for item in ast.blocks.iter().flat_map(|b| b.items.iter()) {
        if let ItemKind::Local(l) = &item.kind
            && let Expr::Lambda {
                body: lambda_body, ..
            } = &l.init
            && let Expr::Block(b) = &**lambda_body
            && let Some(ds_parse::StmtKind::Expr(Expr::Call { callee, .. })) =
                b.stmts.first().map(|s| &s.kind)
            && let Expr::Field { name, .. } = &**callee
        {
            dot = res.get(name.span);
        }
    }
    assert!(
        matches!(dot, Some(Res::Dot(ds_check::DotRes::Free { .. }))),
        "the method position resolved through the free-function tier"
    );
}

const AMBIGUITY_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct box_a { n: int }\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "trait pretty {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for box_a {\n",
    "  fn show(self) -> string { \"showable\" }\n",
    "}\n",
    "impl pretty for box_a {\n",
    "  fn show(self) -> string { \"pretty\" }\n",
    "}\n",
    "fn calls(a: box_a) -> () {\n",
    "  a.show()\n",
    "  showable::show(a)\n",
    "  ()\n",
    "}\n",
);

/// Two traits whose members both accept the receiver are true ambiguity —
/// a compile error, never a priority order — and only the qualified
/// spelling records its dispatch.
#[test]
fn two_traits_sharing_the_name_are_true_ambiguity() {
    let (ast, analyzed) = analyze_source("ambiguity.ds", AMBIGUITY_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.iter().any(|m| m.contains("truly ambiguous")),
        "the dot-call reports the ambiguity: {rendered:?}"
    );
    let spans = call_spans(&ast);
    assert_eq!(spans.len(), 2);
    assert!(
        analyzed.checked.trait_calls.contains_key(&spans[1]),
        "the qualified spelling still records its dispatch"
    );
    assert!(
        !analyzed.checked.trait_calls.contains_key(&spans[0]),
        "the ambiguous dot-call records nothing"
    );
}

const UNRECORDED_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct box_a { n: int }\n",
    "struct box_c { n: int }\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for box_a {\n",
    "  fn show(self) -> string { \"a\" }\n",
    "}\n",
    "fn unsatisfied(c: box_c) -> () {\n",
    "  c.show()\n",
    "  ()\n",
    "}\n",
    "fn assumed[t: showable](x: t) -> () {\n",
    "  x.show()\n",
    "  ()\n",
    "}\n",
);

/// A receiver with no instance reports the unimplemented trait, and a
/// site only an in-scope assumption covers records nothing — its
/// dispatch is the runtime-resolved evidence form, not a solved instance.
#[test]
fn unsatisfied_and_assumption_only_sites_record_nothing() {
    let (_ast, analyzed) = analyze_source("unrecorded.ds", UNRECORDED_SOURCE);
    assert!(
        analyzed
            .checked
            .diags
            .iter()
            .any(|d| d.message.contains("no instance of `showable` for `box_c`")),
        "the unsatisfied receiver reports its missing instance"
    );
    assert!(
        analyzed.checked.trait_calls.is_empty(),
        "neither the unsatisfied site nor the assumption-only site records a solved instance"
    );
}

const SAME_PARAM_NAME_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "struct pair[t]\n",
    "trait json {\n",
    "  fn to_json(self) -> string\n",
    "}\n",
    "impl[t: json] json for list[t] {\n",
    "  fn to_json(self) -> string { \"list\" }\n",
    "}\n",
    "impl[t: json] json for pair[t] {\n",
    "  fn to_json(self) -> string { \"pair\" }\n",
    "}\n",
);

/// Two generic impls whose parameters share the name `t` both declare and
/// check: the package universe mints each impl's variable as a uniquely
/// named tag, so the first impl never occupies the second's name.
#[test]
fn same_named_generic_params_across_impls_both_declare() {
    let (_ast, analyzed) = analyze_source("same_param.ds", SAME_PARAM_NAME_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.is_empty(),
        "both generic impls declare and check cleanly: {rendered:?}"
    );
    assert_eq!(
        analyzed.decls.instance_impls.len(),
        2,
        "each impl contributes its instance"
    );
}

const GENERIC_IMPL_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string { \"l\" }\n",
    "}\n",
    "fn calls(xs: list[int]) -> () {\n",
    "  xs.show()\n",
    "  ()\n",
    "}\n",
);

/// A generic impl's head `list[t]` declares an opaque application atom no
/// containment ever matches for a concrete receiver; the call site over
/// `list[int]` checks clean because the head unifies with the receiver and
/// the `shows` bound holds for the substituted `int`. The recorded
/// instance is the declared generic instance — no per-substitution
/// instance is minted; the recorded receiver type carries the
/// substitution.
#[test]
fn generic_impl_call_site_records_the_declared_instance() {
    let (ast, analyzed) = analyze_source("generic_impl.ds", GENERIC_IMPL_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.is_empty(),
        "the generic-impl call site checks clean: {rendered:?}"
    );
    let spans = call_spans(&ast);
    assert_eq!(spans.len(), 1, "the fixture makes one call");
    let universe = &analyzed.decls.universe;
    let solved = &analyzed.checked.trait_calls[&spans[0]];
    assert_eq!(universe.render(solved.recv), "list[int]");
    let impls = impl_spans(&ast);
    assert_eq!(impls.len(), 2);
    assert_eq!(
        solved.impl_site,
        Some(impls[1]),
        "the list impl's own instance serves the call"
    );
    let (head, _) = (
        analyzed.decls.instances.head(solved.instance),
        analyzed.decls.instances.trait_of(solved.instance),
    );
    assert_eq!(
        universe.render(head.unwrap()),
        "list[$t#1]",
        "the solved instance is the declared generic head"
    );
}

const GENERIC_BOUND_MISS_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "trait unrelated {\n",
    "  fn peek(self) -> int\n",
    "}\n",
    "impl [t: unrelated] shows for list[t] {\n",
    "  fn show(self) -> string { \"l\" }\n",
    "}\n",
    "fn calls(xs: list[int]) -> () {\n",
    "  shows::show(xs)\n",
    "  ()\n",
    "}\n",
);

/// A head that matches while its declared bound fails reports the
/// unsatisfied bound, never a bare missing-instance message: the head
/// unified (`t` solved to `int`) and the bound `unrelated` for `int` is
/// the missing proof. Both call spellings carry the detail — the dot
/// call resolves the same nearest miss the qualification does.
#[test]
fn generic_impl_with_unsatisfied_bound_names_the_bound() {
    let (_ast, analyzed) = analyze_source("bound_miss.ds", GENERIC_BOUND_MISS_SOURCE);
    assert!(
        analyzed
            .checked
            .diags
            .iter()
            .any(|d| d.code.as_str() == "DS2013"
                && d.message.contains("no instance of `unrelated` for `int`")),
        "the missing bound is named: {:?}",
        analyzed.checked.diags
    );
    assert!(
        analyzed.checked.trait_calls.is_empty(),
        "an unsatisfied bound records no dispatch"
    );
}

const DOT_BOUND_MISS_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "trait unrelated {\n",
    "  fn peek(self) -> int\n",
    "}\n",
    "impl [t: unrelated] shows for list[t] {\n",
    "  fn show(self) -> string { \"l\" }\n",
    "}\n",
    "fn calls(xs: list[int]) -> () {\n",
    "  xs.show()\n",
    "  ()\n",
    "}\n",
);

/// The dot-call spelling reports the same substituted-bound detail the
/// qualification shows — the receiver-level message alone would hide the
/// root cause.
#[test]
fn dot_call_with_unsatisfied_bound_names_the_bound() {
    let (_ast, analyzed) = analyze_source("dot_bound_miss.ds", DOT_BOUND_MISS_SOURCE);
    let detail = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2013")
        .expect("the dot call reports DS2013");
    assert!(
        detail
            .message
            .contains("no instance of `unrelated` for `int`"),
        "the dot call names the missing bound: {}",
        detail.message
    );
    assert!(
        detail
            .message
            .contains("the `shows` impl's bound is unsatisfied there"),
        "the dot call names the serving impl's bound: {}",
        detail.message
    );
    assert!(
        analyzed.checked.trait_calls.is_empty(),
        "an unsatisfied bound records no dispatch"
    );
}

const SELF_MEMBER_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "  fn twice(self) -> string { self.show() + self.show() }\n",
    "  fn qualified(self) -> string { shows::show(self) }\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "fn calls(x: int) -> () {\n",
    "  x.twice()\n",
    "  ()\n",
    "}\n",
);

/// A trait's own members see `Self` as satisfying their trait: default
/// bodies call sibling members on `self` through both spellings with no
/// instance — the assumption is scoped to the member bodies and sites an
/// assumption covers still record no dispatch.
#[test]
fn trait_members_see_self_as_satisfying_their_trait() {
    let (ast, analyzed) = analyze_source("self_member.ds", SELF_MEMBER_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.is_empty(),
        "the trait's default bodies check clean: {rendered:?}"
    );
    // The member-body calls are covered by the scoped Self-assumption and
    // record nothing (the runtime-resolved evidence form); the concrete
    // receiver's call is the one recorded dispatch.
    assert_eq!(
        analyzed.checked.trait_calls.len(),
        1,
        "exactly the concrete site records a solved instance"
    );
    let spans = call_spans(&ast);
    assert_eq!(spans.len(), 1, "the fixture makes one top-level call");
    assert!(
        analyzed.checked.trait_calls.contains_key(&spans[0]),
        "the concrete receiver's call records its dispatch"
    );
}

const CONCRETE_GENERIC_OVERLAP_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "impl shows for list[int] {\n",
    "  fn show(self) -> string { \"li\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string { \"l\" }\n",
    "}\n",
);

/// A concrete head and a generic head the set algebra reads disjoint are
/// still one overlap: the probe grounds the generic head at `int` and
/// both impls serve `list[int]`, so the later declaration is rejected
/// with the coherence error instead of deferring to the call site.
#[test]
fn concrete_and_generic_heads_overlap_at_declaration() {
    let (_ast, analyzed) = analyze_source("concrete_overlap.ds", CONCRETE_GENERIC_OVERLAP_SOURCE);
    let overlap = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2006")
        .expect("the concrete-generic overlap reports as DS2006");
    assert!(
        overlap
            .message
            .contains("instance head `list[t]` intersects the `list[int]` instance"),
        "the message names both heads: {}",
        overlap.message
    );
    assert!(
        overlap.message.contains("witness value type `list[int]`"),
        "the witness is the shared instantiation: {}",
        overlap.message
    );
}

const GENERIC_ASSUMED_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string { \"l\" }\n",
    "}\n",
    "fn calls[u: shows](ys: list[u]) -> () {\n",
    "  ys.show()\n",
    "  ()\n",
    "}\n",
);

/// A generic impl also serves receivers that are themselves bound
/// variables: the head unifies (`t` binds the caller's variable) and the
/// bound holds through the in-scope assumption. The dispatch target is
/// statically the generic impl, so the site records it — the receiver's
/// element type is the runtime-resolved half, and the lowerer refuses it
/// honestly.
#[test]
fn generic_impl_serves_bound_variable_receivers() {
    let (ast, analyzed) = analyze_source("generic_assumed.ds", GENERIC_ASSUMED_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.is_empty(),
        "the bound-variable receiver checks clean: {rendered:?}"
    );
    let spans = call_spans(&ast);
    assert_eq!(spans.len(), 1);
    assert!(
        analyzed.checked.trait_calls.contains_key(&spans[0]),
        "the statically-known generic impl records its dispatch"
    );
}

const GENERIC_AMBIGUITY_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string { \"one\" }\n",
    "}\n",
    "impl [u: shows] shows for list[u] {\n",
    "  fn show(self) -> string { \"two\" }\n",
    "}\n",
    "fn calls(xs: list[int]) -> () {\n",
    "  xs.show()\n",
    "  ()\n",
    "}\n",
);

/// Two generic impls of one trait with the same head shape whose bounds
/// provably intersect (the atom `int` satisfies both) are a
/// declaration-time overlap: the instantiation probe grounds both heads
/// at `int` and both serve `list[int]`, so the second declaration is
/// rejected with the witness-carrying coherence error — never a
/// deferred call-site ambiguity.
#[test]
fn two_generic_impls_of_one_shape_overlap_at_declaration() {
    let (ast, analyzed) = analyze_source("generic_amb.ds", GENERIC_AMBIGUITY_SOURCE);
    let overlap = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2006")
        .expect("the generic-generic overlap reports as DS2006");
    assert!(
        overlap
            .message
            .contains("instance head `list[u]` intersects the `list[$t#1]` instance"),
        "the message names both heads: {}",
        overlap.message
    );
    assert!(
        overlap.message.contains("witness value type `list[int]`"),
        "the witness is the shared instantiation: {}",
        overlap.message
    );
    assert!(
        !analyzed
            .checked
            .diags
            .iter()
            .any(|d| d.code.as_str() == "DS2010"),
        "the conflict never defers to the call site"
    );
    let impls = impl_spans(&ast);
    assert_eq!(impls.len(), 3, "the monomorphic impl plus both generics");
    let spans = call_spans(&ast);
    let recorded = &analyzed.checked.trait_calls[&spans[0]];
    assert_eq!(
        recorded.impl_site,
        Some(impls[1]),
        "the call site resolves against the surviving generic impl alone"
    );
}

const ORDERED_AMBIGUITY_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string { \"one\" }\n",
    "}\n",
    "impl [u: shows] shows for list[u] {\n",
    "  fn show(self) -> string { \"two\" }\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "fn calls(xs: list[int]) -> () {\n",
    "  xs.show()\n",
    "  ()\n",
    "}\n",
);

/// The declaration probe proves a generic-generic overlap only through an
/// existing atom satisfying both bounds; when the bound-providing impl
/// is declared after both generics, no per-declaration probe can prove
/// the pair. The package-level closure pass re-solves the closed
/// instance environment once declarations settle and reports the
/// overlap at the later declaration — the first-error-only tradeoff.
#[test]
fn ordered_generic_pair_closes_at_package_level() {
    let (ast, analyzed) = analyze_source("ordered_amb.ds", ORDERED_AMBIGUITY_SOURCE);
    let overlap = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2006")
        .expect("the package-level pass reports the hidden pair as DS2006");
    assert!(
        overlap
            .message
            .contains("instance head `list[$u#2]` intersects the `list[$t#1]` instance"),
        "the message names both solved heads: {}",
        overlap.message
    );
    assert!(
        overlap.message.contains("witness value type `list[int]`"),
        "the witness is the shared instantiation: {}",
        overlap.message
    );
    let impls = impl_spans(&ast);
    let primary = overlap
        .labels
        .iter()
        .find(|label| label.style == LabelStyle::Primary)
        .expect("the overlap carries a primary label");
    assert_eq!(
        primary.span, impls[1],
        "the report site is the later declaration of the overlapping pair"
    );
    let spans = call_spans(&ast);
    assert!(
        !analyzed.checked.trait_calls.contains_key(&spans[0]),
        "the call site of an incoherent pair records no dispatch"
    );
    // The body check still walks an incoherent environment, so the call
    // site keeps its follow-on ambiguity; the pair does not depend on it
    // to surface.
    let follow_on = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2010")
        .expect("the follow-on ambiguity still names the incoherent call");
    assert!(
        follow_on
            .message
            .contains("2 impls of `shows` accept receiver `list[int]`"),
        "the ambiguity message keeps naming the trait and receiver: {}",
        follow_on.message
    );
}

const GENERIC_UNION_RECEIVER_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "impl shows for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string { \"l\" }\n",
    "}\n",
    "fn calls(xs: list[int] | list[string]) -> () {\n",
    "  xs.show()\n",
    "  ()\n",
    "}\n",
);

/// A union receiver is served when every member unifies with the head
/// under its own substitution: `list[int]` and `list[string]` both cover
/// under `t`, each proving the bound against its own element.
#[test]
fn generic_impl_serves_union_receivers_member_wise() {
    let (ast, analyzed) = analyze_source("generic_union.ds", GENERIC_UNION_RECEIVER_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.is_empty(),
        "the union receiver checks clean: {rendered:?}"
    );
    let spans = call_spans(&ast);
    assert!(analyzed.checked.trait_calls.contains_key(&spans[0]));
}

const UNION_HEAD_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "trait ordered {\n",
    "  fn compare(self, other: Self) -> int\n",
    "}\n",
    "enum mark { miss, hit(int) }\n",
    "impl showable for mark {\n",
    "  fn show(self) -> string { \"m\" }\n",
    "}\n",
    "impl ordered for mark {\n",
    "  fn compare(self, other: Self) -> int { 0 }\n",
    "}\n",
    "fn order[t: ordered](a: t, b: t) -> int { a.compare(b) }\n",
    "fn calls(u: mark, o: mark) -> () {\n",
    "  u.show()\n",
    "  showable::show(u)\n",
    "  const c1 = u.compare(o)\n",
    "  const c2 = order(u, o)\n",
    "  if u is miss {\n",
    "    const c3 = u.compare(o)\n",
    "  }\n",
    "  ()\n",
    "}\n",
);

/// A union-head impl decomposes into per-branch instances, and a union
/// receiver is served branch-wise by those siblings: one impl item sits
/// behind them, so dispatch records a representative branch and the
/// member's `Self` binds to the written head — a case-narrowed receiver
/// accepts a union `other: Self`, and a free generic fn's bound check
/// passes over the whole union.
#[test]
fn union_receiver_resolves_through_sibling_branches() {
    let (ast, analyzed) = analyze_source("union_head.ds", UNION_HEAD_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.is_empty(),
        "every union-receiver spelling checks clean: {rendered:?}"
    );
    let impls = impl_spans(&ast);
    assert_eq!(impls.len(), 2, "the showable and ordered impls");
    // Four calls record: the union-receiver dot call and its qualified
    // spelling over the showable impl, the union-operand compare, and the
    // `is`-narrowed compare against a union other. The free generic's
    // own body call (the first span collected) records nothing — an
    // in-scope assumption serves it.
    let spans = call_spans(&ast);
    assert!(
        !analyzed.checked.trait_calls.contains_key(&spans[0]),
        "the generic body's call rides its assumption"
    );
    let mut by_site: Vec<_> = analyzed.checked.trait_calls.values().collect();
    by_site.sort_by_key(|call| call.impl_site.map(|span| span.start));
    assert_eq!(
        by_site.len(),
        4,
        "dot, qualified, union-operand compare, narrowed compare"
    );
    let shows = by_site[0];
    assert_eq!(shows.impl_site, Some(impls[0]), "the shared showable item");
    assert_eq!(
        analyzed.decls.universe.render(shows.recv),
        "miss | hit",
        "the recorded receiver is the whole union"
    );
    let written = analyzed
        .decls
        .instances
        .written(shows.instance)
        .expect("the representative names a declared branch");
    assert_eq!(
        analyzed.decls.universe.render(written),
        "miss | hit",
        "the written head survived decomposition"
    );
    let head = analyzed
        .decls
        .instances
        .head(shows.instance)
        .expect("the representative is a branch instance");
    assert!(
        ["miss", "hit"].contains(&analyzed.decls.universe.render(head).as_str()),
        "the representative is one branch, not the union: {}",
        analyzed.decls.universe.render(head)
    );
    for call in &by_site[2..] {
        assert_eq!(call.impl_site, Some(impls[1]), "the compare spellings");
    }
}

const GENERIC_UNION_HEAD_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: showable] showable for int | list[t] {\n",
    "  fn show(self) -> string { \"x\" }\n",
    "}\n",
    "fn calls(n: int, xs: list[string]) -> () {\n",
    "  n.show()\n",
    "  showable::show(xs)\n",
    "  ()\n",
    "}\n",
);

/// A decomposed generic union head solves its own conditions per branch:
/// the `int` branch names no bound variable, so it solves unconditionally
/// and its call site resolves with no staged evidence, while the
/// `list[t]` branch keeps the `t` condition and its call site records
/// the element's serving instance. The impl's requirement set is the
/// union over branches, so a list site instantiating the condition
/// still shows its evidence.
#[test]
fn generic_union_head_solves_conditions_per_branch() {
    let (ast, analyzed) = analyze_source("generic_union_head.ds", GENERIC_UNION_HEAD_SOURCE);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        rendered.is_empty(),
        "both branch call sites resolve: {rendered:?}"
    );
    let universe = &analyzed.decls.universe;
    let spans = call_spans(&ast);
    assert_eq!(spans.len(), 2, "the fixture makes two calls");
    let int_site = &analyzed.checked.trait_calls[&spans[0]];
    assert_eq!(universe.render(int_site.recv), "int");
    assert_eq!(
        universe.render(analyzed.decls.instances.head(int_site.instance).unwrap()),
        "int",
        "the unconditioned branch serves the int receiver"
    );
    assert!(
        int_site.bounds.is_empty(),
        "a branch naming no bound variable stages no evidence"
    );
    let list_site = &analyzed.checked.trait_calls[&spans[1]];
    assert_eq!(universe.render(list_site.recv), "list[string]");
    assert_eq!(
        universe.render(analyzed.decls.instances.head(list_site.instance).unwrap()),
        "list[$t#1]",
        "the conditioned branch serves the list receiver"
    );
    let [(var, bound, served)] = list_site.bounds.as_slice() else {
        panic!("the list site records its own branch's bound");
    };
    assert_eq!(*bound, int_site.trait_id, "the kept condition's trait");
    assert_eq!(universe.atom_decl(*var).unwrap().name, "$t#1");
    let served = served.expect("the string instance serves the element");
    assert_eq!(
        universe.render(analyzed.decls.instances.head(served).unwrap()),
        "string",
        "the substituted element's instance is the staged evidence"
    );
}

const GENERIC_UNION_HEAD_MISS_SOURCE: &str = concat!(
    "tag int\n",
    "tag unit\n",
    "struct list[t]\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl [t: showable] showable for int | list[t] {\n",
    "  fn show(self) -> string { \"x\" }\n",
    "}\n",
    "fn calls(xs: list[unit]) -> () {\n",
    "  xs.show()\n",
    "  ()\n",
    "}\n",
);

/// A branch head whose own condition is unsatisfiable at a call site
/// still refuses with the bound-miss diagnostic naming the substituted
/// element — the per-branch solving never papers over a missing proof.
#[test]
fn generic_union_head_branch_condition_miss_names_the_element() {
    let (_ast, analyzed) = analyze_source("union_head_miss.ds", GENERIC_UNION_HEAD_MISS_SOURCE);
    let detail = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2013")
        .expect("the call site refuses");
    assert!(
        detail
            .message
            .contains("no instance of `showable` for `unit`")
            && detail
                .message
                .contains("the `showable` impl's bound is unsatisfied there"),
        "the refusal names the branch's own unsolved bound: {}",
        detail.message
    );
    assert!(
        analyzed.checked.trait_calls.is_empty(),
        "an unsatisfied branch records no dispatch"
    );
}

const GENERIC_UNION_HEAD_UNION_RECV_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: showable] showable for int | list[t] {\n",
    "  fn show(self) -> string { \"x\" }\n",
    "}\n",
    "fn calls(u: int | list[string]) -> () {\n",
    "  u.show()\n",
    "  ()\n",
    "}\n",
);

/// A union receiver over both branches of one decomposed generic head
/// resolves through the covering group: the int branch is unconditioned,
/// the list branch proves its own condition for the substituted element,
/// and the recorded representative is the first branch with the written
/// head intact.
#[test]
fn generic_union_head_union_receiver_resolves_through_the_covering_group() {
    let (ast, analyzed) = analyze_source(
        "union_head_union_recv.ds",
        GENERIC_UNION_HEAD_UNION_RECV_SOURCE,
    );
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(rendered.is_empty(), "the union site resolves: {rendered:?}");
    let universe = &analyzed.decls.universe;
    let spans = call_spans(&ast);
    let site = &analyzed.checked.trait_calls[&spans[0]];
    assert_eq!(universe.render(site.recv), "int | list[string]");
    assert_eq!(
        universe.render(analyzed.decls.instances.head(site.instance).unwrap()),
        "int",
        "the representative is the unconditioned first branch"
    );
    assert_eq!(
        universe.render(analyzed.decls.instances.written(site.instance).unwrap()),
        "int | list[$t#1]",
        "the written head survived decomposition"
    );
    assert!(
        site.bounds.is_empty(),
        "the representative names no bound the union receiver could solve"
    );
}

const DISTINCT_IMPLS_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "enum mark { miss, hit(int) }\n",
    "impl showable for miss {\n",
    "  fn show(self) -> string { \"m\" }\n",
    "}\n",
    "impl showable for hit {\n",
    "  fn show(self) -> string { \"h\" }\n",
    "}\n",
    "fn calls(u: mark) -> () {\n",
    "  u.show()\n",
    "  ()\n",
    "}\n",
);

/// Members covered by distinct impl items never serve a union receiver:
/// dispatch cannot pick between the two bodies, so the refusal stays the
/// plain missing-instance one, naming the union.
#[test]
fn union_members_covered_by_distinct_impls_still_refuse() {
    let (_ast, analyzed) = analyze_source("distinct_impls.ds", DISTINCT_IMPLS_SOURCE);
    assert!(
        analyzed.checked.diags.len() == 1,
        "one refusal: {:?}",
        analyzed.checked.diags
    );
    let diag = &analyzed.checked.diags[0];
    assert_eq!(diag.code.as_str(), "DS2013");
    assert_eq!(
        diag.message, "no instance of `showable` for `miss | hit`",
        "the refusal names the union, not a member"
    );
    assert!(
        analyzed.checked.trait_calls.is_empty(),
        "no dispatch records"
    );
}

const UNCOVERED_MEMBER_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "enum mark { miss, hit(int), rect { w: int } }\n",
    "impl showable for miss | hit {\n",
    "  fn show(self) -> string { \"m\" }\n",
    "}\n",
    "fn calls(u: mark) -> () {\n",
    "  u.show()\n",
    "  ()\n",
    "}\n",
);

/// A union with an uncovered member still refuses: the sibling group
/// covers `miss` and `hit` but never `rect`, so branch-wise aggregation
/// stays out and the missing-instance diagnostic names the union.
#[test]
fn a_union_with_an_uncovered_member_still_refuses() {
    let (_ast, analyzed) = analyze_source("uncovered_member.ds", UNCOVERED_MEMBER_SOURCE);
    assert!(
        analyzed.checked.diags.len() == 1,
        "one refusal: {:?}",
        analyzed.checked.diags
    );
    let diag = &analyzed.checked.diags[0];
    assert_eq!(diag.code.as_str(), "DS2013");
    assert!(
        diag.message.starts_with("no instance of `showable` for `"),
        "the refusal names the union: {}",
        diag.message
    );
}
