//! The typed side tables the lowering pass consumes [M3]: the
//! per-expression type table (flow-refined inside `is` branches [B2]),
//! the binder facts, and the one-pass `analyze` bundle whose `Ty` handles
//! all come from one universe.

use ds_check::{Analyzed, analyze, type_check};
use ds_diag::Files;
use ds_parse::{Ast, Expr, parse};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag p_none\n",
    "struct p_num { value: int }\n",
    "type probe = p_none | p_num\n",
    "fn find(p: probe) -> int | none {\n",
    "  if p is p_num { p.value } else { none }\n",
    "}\n",
);

fn analyze_source() -> (Ast, Analyzed) {
    let mut files = Files::new();
    let id = files.add_source("t.ds", SOURCE);
    let ast = parse(&files, id).expect("parses");
    let analyzed = analyze(&files, std::slice::from_ref(&ast));
    assert_eq!(analyzed.checked.diags, [] as [ds_diag::Diagnostic; 0]);
    (ast, analyzed)
}

/// The `p` receiver of the field access inside the `is` branch records
/// the refined member type, not the declared union.
#[test]
fn expr_types_record_flow_refinement() {
    let (ast, analyzed) = analyze_source();
    let universe = &analyzed.decls.universe;
    let span = field_recv(&ast).expect("the fixture has one field access");
    let val = analyzed
        .checked
        .expr_types
        .get(&span)
        .expect("every expression records its Val");
    assert_eq!(universe.render(val.ty), "p_num", "refined at the use site");
}

/// The binder facts keep the declared type, unrefined.
#[test]
fn binders_keep_declared_types() {
    let (_, analyzed) = analyze_source();
    let universe = &analyzed.decls.universe;
    assert!(
        analyzed
            .checked
            .binders
            .values()
            .any(|f| universe.render(f.ty) == "p_none | p_num"),
        "the `p` parameter records its declared union"
    );
}

/// `analyze` and `type_check` agree, and the bundle's `Ty` handles
/// resolve against the returned declaration tables (one universe).
#[test]
fn analyze_and_type_check_share_one_universe() {
    let (ast, analyzed) = analyze_source();
    let mut files = Files::new();
    let _id = files.add_source("t.ds", SOURCE);
    let alone = type_check(&files, &[ast]);
    assert_eq!(alone.diags.len(), analyzed.checked.diags.len());
    let universe = &analyzed.decls.universe;
    // the one function signature decomposes through the bundle's universe
    let defs: Vec<_> = analyzed.checked.def_types.values().collect();
    assert_eq!(defs.len(), 1, "the fixture defines one function");
    for atom in universe.members(*defs[0]) {
        if let Some((params, ret)) = universe.fn_sig(*atom) {
            assert_eq!(params.len(), 1);
            assert_eq!(universe.render(ret), "none | int");
        }
    }
}

/// A generic-call fixture with its own declarations: the ambient's own
/// `len` shape, called over both receiver kinds in both spellings.
const GENERIC_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn len[t] (xs: t) -> int\n",
    "fn over(s: string, xs: list[int]) -> () {\n",
    "  len(s)\n",
    "  len(xs)\n",
    "  s.len()\n",
    "  ()\n",
    "}\n",
);

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
    fn block(b: &ds_parse::BlockExpr, out: &mut Vec<ds_diag::Span>) {
        for stmt in &b.stmts {
            if let ds_parse::StmtKind::Expr(e) = &stmt.kind {
                expr(e, out);
            }
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
            block(b, &mut out);
        }
    }
    out
}

/// The checker records every generic call's full instantiation at the
/// call's own span [H3], in the declaration's parameter order — the
/// side table the lowering pass mints CALLG evidence words from. The
/// UFCS spelling records through the same entry.
#[test]
fn generic_calls_record_the_solved_instantiation() {
    let mut files = Files::new();
    let id = files.add_source("generic.ds", GENERIC_SOURCE);
    let ast = parse(&files, id).expect("parses");
    let analyzed = analyze(&files, std::slice::from_ref(&ast));
    assert!(
        analyzed.checked.diags.is_empty(),
        "the generic fixture checks clean"
    );
    let universe = &analyzed.decls.universe;
    let mut recorded: Vec<String> = Vec::new();
    for span in call_spans(&ast) {
        if let Some(types) = analyzed.checked.generic_calls.get(&span) {
            let rendered: Vec<String> = types.iter().map(|ty| universe.render(*ty)).collect();
            recorded.push(rendered.join(","));
        }
    }
    // three generic calls, one instantiation word each: string,
    // list[int], and the UFCS spelling's string again
    assert_eq!(recorded, ["string", "list[int]", "string"]);
}

/// A call whose generic parameter cannot solve from any argument reports
/// the inference failure and records no partially-solved entry — no
/// silent `any` fallback rides the side table [B1].
#[test]
fn unsolved_generic_calls_record_nothing() {
    let source = concat!(
        "tag int\n",
        "fn broken[t](flag: bool) -> t { flag }\n",
        "fn call_it() -> () {\n",
        "  broken(true)\n",
        "  ()\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("unsolved.ds", source);
    let ast = parse(&files, id).expect("parses");
    let analyzed = analyze(&files, std::slice::from_ref(&ast));
    assert!(
        !analyzed.checked.diags.is_empty(),
        "the unsolved call reports its inference failure"
    );
    assert!(
        analyzed.checked.generic_calls.is_empty(),
        "no instantiation is recorded for the unsolved call"
    );
}

fn field_recv(ast: &Ast) -> Option<ds_diag::Span> {
    fn expr(e: &Expr, out: &mut Option<ds_diag::Span>) {
        if let Expr::Field { recv, name, .. } = e
            && name.name == "value"
        {
            *out = Some(recv.span());
            return;
        }
        match e {
            Expr::Call { callee, args, .. } => {
                expr(callee, out);
                for arg in args {
                    expr(&arg.value, out);
                }
            },
            Expr::If {
                cond, then, else_, ..
            } => {
                expr(cond, out);
                for stmt in &then.stmts {
                    if let ds_parse::StmtKind::Expr(e) = &stmt.kind {
                        expr(e, out);
                    }
                }
                if let Some(e) = else_ {
                    expr(e, out);
                }
            },
            Expr::Field { recv, .. } | Expr::Paren { inner: recv, .. } => expr(recv, out),
            _ => {},
        }
    }
    let mut out = None;
    for b in &ast.blocks {
        for item in &b.items {
            if let ds_parse::ItemKind::Local(l) = &item.kind
                && let Expr::Lambda {
                    body: lambda_body, ..
                } = &l.init
            {
                let Expr::Block(b) = &**lambda_body else {
                    continue;
                };
                for stmt in &b.stmts {
                    if let ds_parse::StmtKind::Expr(e) = &stmt.kind {
                        expr(e, &mut out);
                    }
                }
            }
        }
    }
    out
}
