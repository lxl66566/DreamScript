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

// ---- the type-keyed static surface (the 2026-11-15 ruling) --------------

/// A receiverless inherent member is callable through its type name: the
/// resolution pass records the type-keyed candidates at the method
/// position, the call records the selected method in the inherent table
/// at the call's own span (the same record a solved receiver dot-call
/// writes), and no trait dispatch records at the same site.
#[test]
fn a_receiverless_member_calls_through_its_type_name() {
    let source = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter {\n",
        "  fn make() -> counter { counter { ticks: 0 } }\n",
        "}\n",
        "fn main() -> int {\n",
        "  counter.make().ticks\n",
        "}\n",
    );
    let (ast, analyzed) = analyze_source("static.ds", source);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the type-keyed call checks clean: {:?}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>()
    );
    let spans = call_spans(&ast);
    let [call] = spans.as_slice() else {
        panic!("one static call site: {spans:?}");
    };
    let method = *analyzed
        .checked
        .inherent_calls
        .get(call)
        .expect("the static call records its member at the call span");
    assert!(
        matches!(
            analyzed.package.method(method).owner,
            MethodOwner::Inherent(ref owner) if owner == "counter"
        ),
        "the record names the type's own member"
    );
    assert!(
        !analyzed.checked.trait_calls.contains_key(call),
        "no trait dispatch rides the static call"
    );
    assert!(
        analyzed
            .checked
            .inherent_call_bounds
            .get(call)
            .is_none_or(Vec::is_empty),
        "a monomorphic static call stages no evidence words"
    );
}

/// The two call forms are disjoint: a value receiver never reaches a
/// receiverless member (the type-keyed surface owns it), and a bare
/// type name never reaches a receiver-taking member.
#[test]
fn the_two_member_call_forms_are_disjoint() {
    let value_receiver = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter {\n",
        "  fn make() -> counter { counter { ticks: 0 } }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const c = counter { ticks: 1 }\n",
        "  c.make()\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("value_recv.ds", value_receiver);
    let rendered = render(&analyzed);
    assert!(
        rendered.contains("DS2013") && rendered.contains("no method `make` accepts receiver"),
        "a value receiver does not reach the type-keyed member: {rendered}"
    );

    let type_receiver = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter {\n",
        "  fn peek(self) -> int { self.ticks }\n",
        "}\n",
        "fn main() -> int {\n",
        "  counter.peek()\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("type_recv.ds", type_receiver);
    let rendered = render(&analyzed);
    assert!(
        rendered.contains("member `peek` of `counter` takes a receiver"),
        "the bare type name does not reach a receiver member: {rendered}"
    );
}

/// The namespace split decides the receiver's tier (F9): a value named
/// like the type shadows the type-keyed call at the receiver position
/// (locals first, value defs next, the type namespace last), and a
/// free function with the member's name never applies behind a type
/// name.
#[test]
fn a_value_shadowing_the_type_keeps_the_value_tier() {
    let source = concat!(
        "tag int\n",
        "struct point { x: int }\n",
        "impl point {\n",
        "  fn make() -> point { point { x: 0 } }\n",
        "}\n",
        "fn make() -> int { 7 }\n",
        "fn main() -> int {\n",
        "  const p = point.make()\n",
        "  p.x\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("shadow.ds", source);
    let rendered = render(&analyzed);
    assert!(
        rendered.is_empty(),
        "the free `make` never shadows the type-keyed call: {rendered}"
    );

    let local_shadow = concat!(
        "tag int\n",
        "struct point { x: int }\n",
        "impl point {\n",
        "  fn make() -> point { point { x: 0 } }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const point = point { x: 3 }\n",
        "  point.make()\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("local_shadow.ds", local_shadow);
    let rendered = render(&analyzed);
    assert!(
        rendered.contains("DS2013") && rendered.contains("no method `make` accepts receiver"),
        "a local binding shadows the type name, keeping the value tier: {rendered}"
    );
}

/// The missing member behind a type name reports at the method position
/// naming the type, and a generic impl's receiverless member refuses
/// the bare-name call (the spelling carries no type arguments) while
/// the instantiated receiver carries them and substitutes the impl's
/// variables into the member's written signature.
#[test]
fn the_static_surface_refusals_report_their_seams() {
    let missing = concat!(
        "tag int\n",
        "struct point { x: int }\n",
        "fn main() -> int {\n",
        "  point.missing()\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("missing.ds", missing);
    let rendered = render(&analyzed);
    assert!(
        rendered.contains("type `point` carries no member `missing`"),
        "the missing member names its type: {rendered}"
    );

    let generic = concat!(
        "tag int\n",
        "struct list[t]\n",
        "trait copy { }\n",
        "impl[t: copy] list[t] {\n",
        "  fn make() -> list[t] { list[t]() }\n",
        "}\n",
        "fn main() -> int {\n",
        "  list.make()\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("generic.ds", generic);
    let rendered = render(&analyzed);
    assert!(
        rendered.contains(
            "member `make` of `list` lives in a generic impl; the type-keyed call needs the \
             applied receiver `list[...]`, which the bare type name cannot spell"
        ),
        "the bare-name call cannot spell a generic impl's type arguments: {rendered}"
    );
}

/// The instantiated type-keyed call over a generic impl's receiverless
/// member: the applied receiver carries the impl's type arguments, the
/// head's own impl claims the member, and the written signature reads
/// through the site's substitution — `make`'s `list[t]` answers
/// `list[int]` at `list[int].make()`, proven by the mismatch a wrong
/// annotation reports.
#[test]
fn an_instantiated_receiver_calls_a_generic_impls_receiverless_member() {
    let clean = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "trait copy { }\n",
        "impl copy for int { }\n",
        "impl copy for string { }\n",
        "impl[t: copy] list[t] {\n",
        "  fn make() -> list[t] { list[t]() }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const xs: list[int] = list[int].make()\n",
        "  const ys = list[string].make()\n",
        "  0\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("instantiated.ds", clean);
    let rendered = render(&analyzed);
    assert!(
        rendered.is_empty(),
        "the instantiated calls check clean with substituted signatures: {rendered}"
    );

    let mistyped = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "trait copy { }\n",
        "impl copy for int { }\n",
        "impl copy for string { }\n",
        "impl[t: copy] list[t] {\n",
        "  fn make() -> list[t] { list[t]() }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const xs = list[int].make()\n",
        "  const wrong: string = xs\n",
        "  0\n",
        "}\n",
    );
    let (_, analyzed) = analyze_source("mistyped.ds", mistyped);
    let rendered = render(&analyzed);
    assert!(
        rendered.contains("has type `list[int]`, not contained in `string`"),
        "the call's answer type carries the site's substitution: {rendered}"
    );
}

/// The rendered diagnostic strings of one analysis, one per line.
fn render(analyzed: &Analyzed) -> String {
    analyzed
        .checked
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code.as_str(), d.message))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A tag name behind the dot keeps its constructor-value reading when the
/// type's own impl carries no member with that name: the trait impl still
/// dispatches over the tag's value, and the type-keyed tier claims the
/// spelling only for a member of the receiver's own impl. A same-name
/// overlap resolves the other way — the type's own receiverless member
/// owns the bare-name receiver, the value form feeding the trait.
#[test]
fn a_tag_name_keeps_its_value_reading_behind_the_dot() {
    let plain = concat!(
        "tag string\n",
        "tag io_err\n",
        "trait narrated { fn show(self) -> string }\n",
        "impl narrated for io_err { fn show(self) -> string { \"tagwish\" } }\n",
        "fn main() -> string { io_err.show() }\n",
    );
    let (ast, analyzed) = analyze_source("plain", plain);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the trait dispatch over the tag value: {}",
        render(&analyzed)
    );
    let spans = call_spans(&ast);
    let [call] = spans.as_slice() else {
        panic!("one call site")
    };
    assert!(
        !analyzed.checked.inherent_calls.contains_key(call),
        "a tag value receiver dispatches the trait, never the static tier"
    );
    assert!(
        analyzed.checked.trait_calls.contains_key(call),
        "the trait impl owns the tag value behind the dot"
    );

    let overlap = concat!(
        "tag string\n",
        "tag io_err\n",
        "trait narrated { fn show(self) -> string }\n",
        "impl narrated for io_err { fn show(self) -> string { \"valuewish\" } }\n",
        "impl io_err { fn show() -> string { \"staticwish\" } }\n",
        "fn main() -> string { io_err.show() }\n",
    );
    let (ast, analyzed) = analyze_source("overlap", overlap);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the overlap probe checks clean: {}",
        render(&analyzed)
    );
    let spans = call_spans(&ast);
    let [call] = spans.as_slice() else {
        panic!("one call site")
    };
    let method = *analyzed
        .checked
        .inherent_calls
        .get(call)
        .expect("the bare-name call records its static pick");
    assert!(
        matches!(
            analyzed.package.method(method).owner,
            MethodOwner::Inherent(ref owner) if owner == "io_err"
        ),
        "the type's own receiverless member owns the bare-name receiver"
    );
}
