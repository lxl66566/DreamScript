//! The solved-dispatch side table of trait-method call sites [B6]: every
//! call whose instance the environment statically resolves records the
//! selected member, the trait, the unique instance, the declaring impl
//! item, and the receiver's type, keyed by the call's own span. Sites an
//! in-scope assumption covers record nothing — they are the
//! runtime-resolved evidence form.

use ds_check::{Analyzed, Res, analyze};
use ds_diag::{Files, Span};
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
        if let ItemKind::Local(l) = &item.kind {
            if let Expr::Lambda {
                body: lambda_body, ..
            } = &l.init
            {
                if let Expr::Block(b) = &**lambda_body {
                    for stmt in &b.stmts {
                        if let ds_parse::StmtKind::Expr(e) = &stmt.kind {
                            expr(e, &mut out);
                        }
                    }
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
/// free tier wins and the call site records no trait dispatch [F3].
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
        if let ItemKind::Local(l) = &item.kind {
            if let Expr::Lambda {
                body: lambda_body, ..
            } = &l.init
            {
                if let Expr::Block(b) = &**lambda_body {
                    if let Some(ds_parse::StmtKind::Expr(Expr::Call { callee, .. })) =
                        b.stmts.first().map(|s| &s.kind)
                    {
                        if let Expr::Field { name, .. } = &**callee {
                            dot = res.get(name.span);
                        }
                    }
                }
            }
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
/// spelling records its dispatch [F3, B6].
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
