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

/// The spans of every call expression inside impl member bodies, in
/// source order.
fn impl_call_spans(ast: &Ast) -> Vec<Span> {
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
        if let ItemKind::Impl(imp) = &item.kind {
            for f in &imp.fns {
                if let Some(body) = &f.body {
                    for stmt in &body.stmts {
                        match &stmt.kind {
                            ds_parse::StmtKind::Expr(e) => expr(e, &mut out),
                            ds_parse::StmtKind::Local(local) => expr(&local.init, &mut out),
                        }
                    }
                }
            }
        }
    }
    out
}

/// The shared generic-inherent story: the shows trait with its int and
/// string instances, plus one generic inherent impl over `list[t]` whose
/// body demands evidence (the element call inside the loop).
const GENERIC_STORY: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"n\" }\n",
    "}\n",
    "impl shows for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl[t: shows] list[t] {\n",
    "  fn describe(self) -> string {\n",
    "    var acc = \"l\"\n",
    "    for x in self {\n",
    "      acc = \"${acc}${x.show()}\"\n",
    "    }\n",
    "    acc\n",
    "  }\n",
    "}\n",
);

/// A generic inherent impl records its declared bounds in the declaration
/// tables: one condition over the declaration's minted variable, keyed by
/// the impl item's span — the record the call sites and the lowering pass
/// read, mirroring the trait form's instance conditions.
#[test]
fn a_generic_inherent_impl_records_its_declared_bounds() {
    let (_ast, analyzed) = analyze_source("bounds_record.ds", GENERIC_STORY);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the story checks clean: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>()
    );
    let impl_site = *analyzed
        .decls
        .inherent_impl_bounds
        .keys()
        .next()
        .expect("the generic inherent impl records its bounds");
    let record = &analyzed.decls.inherent_impl_bounds[&impl_site];
    let [cond] = record.conditions.as_slice() else {
        panic!("one condition per declared bound: {:?}", record.conditions);
    };
    let shows = analyzed
        .package
        .lookup_type("shows")
        .and_then(|def| analyzed.decls.traits.get(&def).copied())
        .expect("the shows trait is declared");
    assert_eq!(cond.trait_id, shows, "the condition names the bound trait");
    // the minted variable bridges to the written parameter name `t`
    let [var] = analyzed.decls.universe.members(cond.ty) else {
        panic!("the condition constrains one variable");
    };
    let rendered = analyzed
        .decls
        .universe
        .atom_decl(*var)
        .expect("the variable atom is declared")
        .name
        .clone();
    assert!(
        rendered.starts_with("$t#"),
        "the minted variable carries the written name: {rendered}"
    );
}

/// A call site entering a generic inherent impl's body records the
/// serving instance per declared bound under the receiver's substitution
/// — the trait form's per-bound record minus the instance word. Two
/// instantiations of one body each name their own serving impl.
#[test]
fn a_call_site_records_the_serving_instances_per_declared_bound() {
    let source = format!(
        "{}\n{}\n",
        GENERIC_STORY,
        concat!(
            "fn main() -> int {\n",
            "  println([1].describe())\n",
            "  println([\"a\"].describe())\n",
            "  0\n",
            "}\n",
        )
    );
    let (_ast, analyzed) = analyze_source("bounds_calls.ds", &source);
    assert!(
        analyzed.checked.diags.is_empty(),
        "both call sites check clean: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>()
    );
    let shows = analyzed
        .package
        .lookup_type("shows")
        .and_then(|def| analyzed.decls.traits.get(&def).copied())
        .expect("the shows trait is declared");
    assert_eq!(
        analyzed.checked.inherent_call_bounds.len(),
        2,
        "one record per entering call site"
    );
    let mut served_heads = Vec::new();
    for bounds in analyzed.checked.inherent_call_bounds.values() {
        let [(_var, trait_id, served)] = bounds.as_slice() else {
            panic!("one bound entry per call site: {bounds:?}");
        };
        assert_eq!(*trait_id, shows, "the bound names the shows trait");
        let &instance = served
            .as_ref()
            .expect("a unique instance serves the element");
        served_heads.push(
            analyzed
                .decls
                .instances
                .head(instance)
                .map(|ty| analyzed.decls.universe.render(ty))
                .expect("the serving instance has a head"),
        );
    }
    served_heads.sort();
    assert_eq!(
        served_heads,
        vec!["int".to_owned(), "string".to_owned()],
        "the two instantiations name their own serving instance heads"
    );
}

/// A declared bound unsatisfied at the call site reports there — the
/// trait form's call-site bound miss mirrored: the receiver accepts the
/// owner, the bound's substituted type has no serving instance, and the
/// diagnostic names the trait over the element type.
#[test]
fn an_unsatified_inherent_bound_reports_at_the_call_site() {
    let source = concat!(
        "tag int\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl[t: shows] list[t] {\n",
        "  fn describe(self) -> string { \"d\" }\n",
        "}\n",
        "fn main() -> string {\n",
        "  const xs = [1]\n",
        "  xs.describe()\n",
        "}\n",
    );
    let (ast, analyzed) = analyze_source("bound_miss.ds", source);
    assert!(
        !analyzed.checked.diags.is_empty(),
        "the unsatisfied bound reports at the call"
    );
    let spans = call_spans(&ast);
    let [call] = spans.as_slice() else {
        panic!("one dot-call site: {} spans", spans.len());
    };
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| {
            d.labels.iter().any(|l| {
                l.span.file == call.file && l.span.start <= call.start && l.span.end >= call.end
            })
        })
        .expect("the diagnostic lands at the call site");
    assert!(
        diag.message.contains("no instance of `shows` for `int`"),
        "the message names the bound over the element type: {}",
        diag.message
    );
}

/// A call into a generic inherent member from inside another generic
/// body checks clean — the enclosing body's bound assumes the trait —
/// but records `None` for the serving instance: no instance names a
/// word the site could stage, so the lowerer keeps the honest
/// transitive-staging refusal.
#[test]
fn an_inherent_call_inside_a_generic_body_records_none() {
    let source = format!(
        "{}{}\n{}\n{}\n",
        GENERIC_STORY,
        "impl[t: shows] list[t] {",
        "  fn wrap(self) -> string { self.describe() }",
        "}",
    );
    let (ast, analyzed) = analyze_source("inner_none.ds", &source);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the in-scope assumption satisfies the bound: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>()
    );
    let inner_spans = impl_call_spans(&ast);
    let [inner] = inner_spans.as_slice() else {
        panic!("one call site inside the impl bodies");
    };
    let bounds = analyzed
        .checked
        .inherent_call_bounds
        .get(inner)
        .expect("the inner site records its bounds");
    let [(_var, _trait, served)] = bounds.as_slice() else {
        panic!("one bound entry: {bounds:?}");
    };
    assert!(
        served.is_none(),
        "no instance serves the caller's own variable: {bounds:?}"
    );
}
