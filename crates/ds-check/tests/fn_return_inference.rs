//! The first-order return unification: a fn-typed parameter
//! whose written return carries an unsolved variable takes the lambda
//! argument's inferred answer and binds the written return's variables
//! by first-order pattern match — an application strips to its
//! arguments, a variable binds directly. A variable no argument fixes
//! names DS2002; a shape mismatch is the containment refusal a named fn
//! gets; a generic fn value passed bare keeps its own-variable refusal.

use ds_check::{analyze, type_check};
use ds_diag::Files;
use ds_parse::{Ast, Expr, parse};

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub struct list[t]\n",
    "pub extern fn str[t] (v: t) -> string\n",
    "pub extern fn flat_map[t, r] (xs: list[t], f: fn(t) -> list[r]) -> list[r]\n",
    "pub extern fn pick[t, r] (xs: list[t], f: fn(t) -> (r, int)) -> (r, int)\n",
    "pub extern fn app[t, r] (f: fn(t) -> r) -> (t, r)\n",
    "pub extern fn opt_head[t, r] (xs: list[t], f: fn(t) -> r | none) -> list[r]\n",
);

/// Type-checks `source` beside the ambient, rendering every diagnostic as
/// one `CODE message` line.
fn check(source: &str) -> Vec<String> {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let src = files.add_source("probe.ds", source);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, src).expect("the probe parses"),
    ];
    type_check(&files, &asts)
        .diags
        .iter()
        .map(|diag| format!("{} {}", diag.code.as_str(), diag.message))
        .collect()
}

fn clean(source: &str) {
    let diags = check(source);
    assert!(
        diags.is_empty(),
        "expected clean, got:\n{}",
        diags.join("\n")
    );
}

#[test]
fn lambda_and_named_arguments_infer_the_written_return() {
    for source in [
        // the lambda literal's body infers its own answer: the written
        // `list[r]` binds r from `list[string]`
        "const ys = flat_map([\"a\", \"b\"], (w) => [w])\n",
        // the block-bodied spelling answers the same
        "const ys = flat_map([\"a\"], (w) => {\n  const parts = [w]\n  parts\n})\n",
        // a named monomorphic fn argument solves through the same match
        "fn explode(w: string) -> list[string] { [w] }\nconst ys = flat_map([\"a\"], explode)\n",
        // a tuple return binds its variable in place
        "const picked = pick([1], (w) => (str(w), w))\n",
        // inside an enclosing generic the lambda answers the scope's
        // own variable, which is a runtime-evidenced fact, never a hole
        "fn outer[u](xs: list[u]) -> list[u] { flat_map(xs, (w) => [w]) }\n",
    ] {
        clean(source);
    }
}

#[test]
fn a_shape_mismatch_is_the_containment_refusal_for_lambda_and_named_fn() {
    // the lambda answering a tuple under a written `list[r]` fails the
    // first-order match, and the settled check renders the written
    // spelling — the identical line a named fn of the same shape gets
    let lambda = check("const ys = flat_map([\"a\"], (w) => ((1, 2)))\n");
    let named = check(concat!(
        "fn pair_up(w: string) -> (int, int) { (1, 2) }\n",
        "const ys = flat_map([\"a\"], pair_up)\n",
    ));
    assert_eq!(lambda, named, "the lambda and the named fn agree");
    assert_eq!(lambda, ["DS2001 `argument` has type `fn(string) -> (int, \
                         int)`, not contained in `fn(t) -> list[r]` [B2]"
        .to_owned()]);
}

#[test]
fn an_unfixed_variable_names_the_inference_failure() {
    // only the fn-typed parameter mentions `t`, so the call's answer
    // binds `r` alone
    assert_eq!(check("const apped = app((n) => [\"a\"])\n"), [
        "DS2002 cannot infer `t` from this call's arguments [B2]".to_owned()
    ]);
    // `none` is contained in `r | none`, so the match leaves r unsolved
    assert_eq!(check("const heads = opt_head([\"a\"], (w) => none)\n"), [
        "DS2002 cannot infer `r` from this call's arguments [B2]".to_owned()
    ]);
}

#[test]
fn a_generic_fn_value_keeps_the_own_variable_refusal() {
    // a generic named fn passed bare carries its minted variable into
    // the containment refusal; the number is minted, so the pin reads
    // the shape, not the digits
    let bare = check(concat!(
        "fn dup[t](v: t) -> list[t] { [v] }\n",
        "const ys = flat_map([\"a\"], dup)\n",
    ));
    assert_eq!(bare.len(), 1);
    assert!(
        bare[0].starts_with("DS2001 `argument` has type `fn($t#")
            && bare[0].ends_with("`, not contained in `fn(t) -> list[r]` [B2]"),
        "{bare:?}"
    );
    // the same value through a const keeps the refusal
    let via_const = check(concat!(
        "fn dup[t](v: t) -> list[t] { [v] }\n",
        "const g = dup\n",
        "const ys = flat_map([\"a\"], g)\n",
    ));
    assert_eq!(via_const, bare, "the const spelling refuses identically");
}

/// The generic call's solved instantiation records the matched return
/// beside the call — the side table the lowering pass mints
/// CALLG evidence words from.
#[test]
fn the_solved_instantiation_records_the_matched_return() {
    let source = concat!(
        "fn over(xs: list[string]) -> list[string] {\n",
        "  flat_map(xs, (w) => [w])\n",
        "}\n",
    );
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let src = files.add_source("probe.ds", source);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, src).expect("the probe parses"),
    ];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean"
    );
    let universe = &analyzed.decls.universe;
    let recorded: Vec<String> = call_spans(&asts[1])
        .into_iter()
        .filter_map(|span| analyzed.checked.generic_calls.get(&span))
        .map(|types| {
            types
                .iter()
                .map(|ty| universe.render(*ty))
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();
    assert_eq!(recorded, ["string,string"]);
}

/// Every call span in a top-level `fn` body, in walk order.
fn call_spans(ast: &Ast) -> Vec<ds_diag::Span> {
    fn expr(e: &Expr, out: &mut Vec<ds_diag::Span>) {
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
        if let ds_parse::ItemKind::Local(l) = &item.kind
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

/// Inside an enclosing generic, the lambda's answer names the scope's
/// variable and the instantiation records it, not a fresh hole.
#[test]
fn an_enclosing_generics_variable_records_through_the_match() {
    let source = concat!(
        "fn outer[u](xs: list[u]) -> list[u] {\n",
        "  flat_map(xs, (w) => [w])\n",
        "}\n",
    );
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let src = files.add_source("probe.ds", source);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, src).expect("the probe parses"),
    ];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the enclosing-generic call checks clean"
    );
    let universe = &analyzed.decls.universe;
    let recorded: Vec<String> = call_spans(&asts[1])
        .into_iter()
        .filter_map(|span| analyzed.checked.generic_calls.get(&span))
        .map(|types| {
            types
                .iter()
                .map(|ty| universe.render(*ty))
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();
    assert_eq!(recorded.len(), 1, "the flat_map call records once");
    let solved: Vec<&str> = recorded[0].split(',').collect();
    assert_eq!(solved.len(), 2, "flat_map takes two generics: {recorded:?}");
    assert!(
        solved.iter().all(|slot| slot.starts_with("$u#")),
        "the record holds the scope's variable in both slots, got {recorded:?}"
    );
}
