//! The script-entry fold: the entry file's top-level statements compile
//! into one synthesized `main` binding while the declarations beside them
//! stay declarations. Library mode keeps the pure-declaration rule and
//! rejects the same statements with a structured diagnostic; a written
//! `main` colliding with the synthesis is its own error.

use ds_diag::{ErrorCode, Files, Location};
use ds_parse::{Ast, Expr, ItemKind, Pattern, StmtKind, Type, parse, parse_script};

/// Parses one source as the script entry, or renders the failure.
fn script(src: &str) -> Ast {
    let mut files = Files::new();
    let file = files.add_source("t.ds", src);
    parse_script(&files, file).unwrap_or_else(|diags| {
        let rendered: Vec<String> = diags
            .iter()
            .map(|d| {
                let label = &d.labels[0];
                let loc = files.location(label.span.file, label.span.start);
                format!(
                    "  {} {}:{}:{} — {} ({})",
                    d.code.as_str(),
                    files.name(label.span.file),
                    loc.line,
                    loc.col,
                    d.message,
                    label.label
                )
            })
            .collect();
        panic!(
            "should parse as the entry:\n{}\n--- source:\n{src}",
            rendered.join("\n")
        )
    })
}

/// One source's first diagnostic: its code, label positions, label count.
fn err(src: &str, entry: bool) -> (ErrorCode, Vec<Location>, usize) {
    let mut files = Files::new();
    let file = files.add_source("t.ds", src);
    let diags = match entry {
        true => parse_script(&files, file).expect_err("should fail"),
        false => parse(&files, file).expect_err("should fail"),
    };
    let first = &diags[0];
    (
        first.code,
        first
            .labels
            .iter()
            .map(|l| files.location(l.span.file, l.span.start))
            .collect(),
        first.labels.len(),
    )
}

/// The synthesized `main` binding of one script: its lambda's parameters,
/// return annotation, and body statements.
fn synth_main(ast: &Ast) -> (&[ds_parse::Param], Option<&Type>, &[ds_parse::Stmt]) {
    let main = ast
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .find_map(|item| match &item.kind {
            ItemKind::Local(local)
                if matches!(&local.pat, Pattern::Binding { name, .. } if name.name == "main") =>
            {
                Some(local)
            },
            _ => None,
        })
        .expect("the fold synthesizes a main binding");
    let Expr::Lambda {
        params, ret, body, ..
    } = &main.init
    else {
        panic!("the synthesized main binds a lambda");
    };
    let Expr::Block(block) = body.as_ref() else {
        panic!("the lambda body is a block");
    };
    (params.as_slice(), ret.as_ref(), block.stmts.as_slice())
}

#[test]
fn library_mode_rejects_a_top_level_statement() {
    // The pure-declaration rule holds on every non-entry path; the entry
    // exception is a mode choice, so the same text parses as the entry.
    let (code, at, labels) = err("answer\n", false);
    assert_eq!(code, ErrorCode::TopLevelStatement);
    assert_eq!(labels, 1);
    assert_eq!((at[0].line, at[0].col), (1, 1));
    assert!(script("answer\n").blocks.len() == 1);
}

#[test]
fn the_fold_collects_statements_in_source_order() {
    // Lets and expression statements fold across separator blocks, in
    // source order; declarations stay items of their blocks.
    let ast = script(concat!(
        "extern fn println (msg: string) -> ()\n",
        "const n = 1\n",
        "println(\"two\")\n",
        "## section\n",
        "const m = n\n",
    ));
    let items: Vec<&str> = ast
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .filter_map(|item| match &item.kind {
            ItemKind::ExternFn(f) => Some(f.name.name.as_str()),
            ItemKind::Local(local) => match &local.pat {
                Pattern::Binding { name, .. } => Some(name.name.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(items, ["println", "main"], "declarations plus the fold");
    let (params, ret, stmts) = synth_main(&ast);
    assert!(params.is_empty(), "the entry takes no parameters");
    let Type::Unit(_) = ret.expect("the synthesized return is unit") else {
        panic!("the return annotation is unit");
    };
    let kinds: Vec<&str> = stmts
        .iter()
        .map(|stmt| match stmt.kind {
            StmtKind::Local(_) => "let",
            StmtKind::Expr(_) => "expr",
        })
        .collect();
    assert_eq!(kinds, ["let", "expr", "let"], "source order across blocks");
}

#[test]
fn a_written_main_collides_with_the_fold() {
    // The synthesis owns the `main` name; a written one is an error
    // pointing at the first statement, in either spelling.
    for (src, line) in [
        ("fn main() -> () { 0 }\nprintln(\"stmt\")\n", 2),
        ("const n = 1\nfn main() -> () { n }\n", 1),
    ] {
        let (code, at, labels) = err(src, true);
        assert_eq!(code, ErrorCode::ScriptMainConflict);
        assert_eq!(labels, 1);
        assert_eq!(
            at[0].line, line,
            "the label names the first statement: {src}"
        );
    }
}

#[test]
fn visibility_marks_a_declaration_not_a_statement() {
    // `pub` on a binding keeps it a package declaration even under script
    // mode; only the plain expression folds.
    let ast = script("pub const n = 1\nprintln(\"x\")\n");
    let items: Vec<&str> = ast
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .filter_map(|item| match &item.kind {
            ItemKind::Local(local) => match &local.pat {
                Pattern::Binding { name, .. } => Some(name.name.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(items, ["n", "main"]);
    let (_, _, stmts) = synth_main(&ast);
    assert_eq!(stmts.len(), 1, "the pub binding never folds");
}

#[test]
fn a_file_without_statements_folds_nothing() {
    // A pure-declaration entry file keeps its own `main`; the fold is inert.
    let ast = script("fn main() -> () { 0 }\n");
    let (_, _, stmts) = synth_main(&ast);
    assert_eq!(stmts.len(), 1, "the written main keeps its own body");
    assert_eq!(
        ast.blocks.iter().flat_map(|b| b.items.iter()).count(),
        1,
        "no synthesis happened"
    );
}
