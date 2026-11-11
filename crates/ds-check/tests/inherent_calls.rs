//! The solved-dispatch side table of inherent-impl dot-calls (F3): every
//! call whose owner covers the receiver records the selected method at
//! the call's own span, and the declaration tables carry the
//! method-to-impl-item tie the lowerer dispatches through. Inherent
//! members on union heads stay unreachable per the narrowing boundary:
//! an owner naming no declared atom accepts no receiver.

use ds_check::{Analyzed, MethodOwner, analyze};
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
        if let ItemKind::Local(l) = &item.kind
            && let Expr::Lambda { body, .. } = &l.init
            && let Expr::Block(b) = &**body
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

/// The span of the file's one inherent impl item.
fn inherent_impl_span(ast: &Ast) -> Span {
    for item in ast.blocks.iter().flat_map(|b| b.items.iter()) {
        if let ItemKind::Impl(imp) = &item.kind
            && imp.trait_ref.is_none()
        {
            return item.span;
        }
    }
    panic!("the source carries one inherent impl item");
}

/// A solved inherent dot-call records its selected method at the call's
/// own span, the method keys the impl item tie, and no trait dispatch
/// records at the same site.
#[test]
fn an_inherent_dot_call_records_the_selected_method() {
    let source = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter { fn peek(self) -> int { self.ticks } }\n",
        "fn main() -> int {\n",
        "  const c = counter { ticks: 7 }\n",
        "  c.peek()\n",
        "}\n",
    );
    let (ast, analyzed) = analyze_source("record.ds", source);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>()
    );
    let spans = call_spans(&ast);
    let [call] = spans.as_slice() else {
        panic!("one dot-call site: {spans:?}");
    };
    let &method = analyzed
        .checked
        .inherent_calls
        .get(call)
        .expect("the solved inherent call records its method");
    let decl = analyzed.package.method(method);
    assert_eq!(decl.name, "peek");
    assert!(
        matches!(&decl.owner, MethodOwner::Inherent(owner) if owner == "counter"),
        "the selected method belongs to the counter impl: {:?}",
        decl.owner
    );
    let impl_site = analyzed
        .decls
        .inherent_impls
        .get(&method)
        .expect("the method keys its impl item");
    assert_eq!(*impl_site, inherent_impl_span(&ast));
    assert!(
        !analyzed.checked.trait_calls.contains_key(call),
        "no trait dispatch records at an inherent site"
    );
}

/// An inherent impl over a union head registers but accepts no receiver:
/// the owner names no declared atom, so the dot-call reports the
/// receiver-level refusal and nothing records.
#[test]
fn a_union_head_impl_accepts_no_receiver() {
    let source = concat!(
        "tag int\n",
        "tag ready\n",
        "type outcome = ready | int\n",
        "impl outcome { fn done(self) -> int { 1 } }\n",
        "fn main() -> int {\n",
        "  const o = ready\n",
        "  o.done()\n",
        "}\n",
    );
    let (ast, analyzed) = analyze_source("union_head.ds", source);
    assert!(
        !analyzed.checked.diags.is_empty(),
        "the union-head dot-call reports at the dot"
    );
    let rendered = analyzed
        .checked
        .diags
        .iter()
        .map(|d| d.code.to_string())
        .collect::<Vec<_>>();
    assert!(
        rendered.iter().any(|code| code.contains("DS2013")),
        "the refusal is the no-accepting-owner class: {rendered:?}"
    );
    let spans = call_spans(&ast);
    assert!(
        spans
            .iter()
            .all(|span| !analyzed.checked.inherent_calls.contains_key(span)),
        "an unaccepted receiver records nothing"
    );
}
