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

fn field_recv(ast: &Ast) -> Option<ds_diag::Span> {
    fn expr(e: &Expr, out: &mut Option<ds_diag::Span>) {
        if let Expr::Field { recv, name, .. } = e {
            if name.name == "value" {
                *out = Some(recv.span());
                return;
            }
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
            if let ds_parse::ItemKind::Local(l) = &item.kind {
                if let Expr::Lambda {
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
    }
    out
}
